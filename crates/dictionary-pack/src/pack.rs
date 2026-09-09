use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OpenFlags, params};
use sha2::{Digest, Sha256};

use crate::semantic::{RoutingRecord, SemanticBounds};
use crate::{
    COMPRESSION_PROFILE, DATA_APPLICATION_ID, DATA_SCHEMA, EntryId, EntryReference, EntrySummary,
    FORMAT_VERSION, INDEX_APPLICATION_ID, INDEX_SCHEMA, MatchClass, PACK_MANIFEST_FILE, PackError,
    PackId, PackManifest, PackRevision, PackRevisionInputs, ROUTING_POLICY,
    SELECTED_STREAM_DIGEST_DOMAIN, SHARD_RECORD_DIGEST_DOMAIN, UNICODE_PROFILE, lookup_keys,
};

/// Runtime bounds applied while admitting a pack and projecting records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackLimits {
    pub max_manifest_bytes: u64,
    pub max_assets: usize,
    pub max_asset_bytes: u64,
    pub max_query_bytes: usize,
    pub max_lookup_results: usize,
    pub max_compressed_record_bytes: u64,
    pub max_uncompressed_record_bytes: u64,
    pub max_routing_forms: usize,
    pub max_text_bytes: usize,
    pub max_tags: usize,
    pub max_pronunciations: usize,
    pub max_senses: usize,
    pub max_glosses_per_sense: usize,
    pub max_translations_per_sense: usize,
}

impl Default for PackLimits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 1024 * 1024,
            max_assets: 256,
            max_asset_bytes: 4 * 1024 * 1024 * 1024,
            max_query_bytes: 4096,
            max_lookup_results: 100,
            max_compressed_record_bytes: 16 * 1024 * 1024,
            max_uncompressed_record_bytes: 64 * 1024 * 1024,
            max_routing_forms: 4096,
            max_text_bytes: 16 * 1024,
            max_tags: 256,
            max_pronunciations: 256,
            max_senses: 1024,
            max_glosses_per_sense: 256,
            max_translations_per_sense: 256,
        }
    }
}

impl PackLimits {
    fn validate(self) -> Result<Self, PackError> {
        let valid = self.max_manifest_bytes > 0
            && self.max_assets > 0
            && self.max_asset_bytes > 0
            && self.max_query_bytes > 0
            && self.max_lookup_results > 0
            && self.max_compressed_record_bytes > 0
            && self.max_uncompressed_record_bytes > 0
            && self.max_routing_forms > 0
            && self.max_text_bytes > 0
            && self.max_tags > 0
            && self.max_pronunciations > 0
            && self.max_senses > 0
            && self.max_glosses_per_sense > 0
            && self.max_translations_per_sense > 0;
        if valid {
            Ok(self)
        } else {
            Err(PackError::Limit(
                "all configured pack limits must be greater than zero".to_owned(),
            ))
        }
    }

    const fn semantic(self) -> SemanticBounds {
        SemanticBounds {
            text_bytes: self.max_text_bytes,
            tags: self.max_tags,
            pronunciations: self.max_pronunciations,
            senses: self.max_senses,
            glosses_per_sense: self.max_glosses_per_sense,
            translations_per_sense: self.max_translations_per_sense,
        }
    }
}

/// Per-call exact lookup controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LookupOptions {
    pub limit: usize,
}

impl Default for LookupOptions {
    fn default() -> Self {
        Self { limit: 20 }
    }
}

/// One exact match and its bounded typed semantic projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LookupMatch {
    pub entry: EntrySummary,
}

/// Results from the first non-empty exact-match stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LookupOutcome {
    pub matches: Vec<LookupMatch>,
    pub truncated: bool,
}

/// Facts authenticated by a complete traversal of a pack's selected corpus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidationReport {
    pub authenticated_record_count: u64,
    pub uncompressed_record_bytes: u64,
    pub lookup_key_row_count: u64,
}

impl LookupOutcome {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

struct DataShard {
    ordinal: u64,
    route: ShardRoute,
    connection: Mutex<Connection>,
}

/// An admitted immutable dictionary pack with synchronous exact lookup.
pub struct DictionaryPack {
    directory: PathBuf,
    pack_id: PackId,
    revision: PackRevision,
    corpus_language: String,
    manifest: PackManifest,
    limits: PackLimits,
    index: Mutex<Connection>,
    shards: Vec<DataShard>,
}

impl DictionaryPack {
    /// Opens and validates the immutable envelope and relational structure of a
    /// format-v1 pack directory.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed manifests, unsafe assets, byte mismatches,
    /// invalid databases, or inconsistent redundant metadata.
    pub fn open(directory: impl AsRef<Path>, limits: PackLimits) -> Result<Self, PackError> {
        let limits = limits.validate()?;
        let directory = directory.as_ref().to_owned();
        let manifest_path = directory.join(PACK_MANIFEST_FILE);
        let manifest_bytes = read_bounded(&manifest_path, limits.max_manifest_bytes)?;
        let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|error| PackError::Malformed(format!("invalid pack manifest: {error}")))?;
        validate_manifest(&manifest, limits)?;
        let pack_id = PackId::from_str(&manifest.pack_id)?;
        let revision = PackRevision::from_bytes(*manifest.pack_revision.as_bytes());
        let revision_hex = revision.to_hex();
        let expected_index_name = format!("{}-{revision_hex}-index.eldict", pack_id.as_str());

        let mut index_path = None;
        let mut data_paths = BTreeMap::new();
        for asset in &manifest.assets {
            let path = directory.join(&asset.file_name);
            validate_asset_bytes(&path, asset.size_bytes, asset.sha256.as_bytes())?;
            match asset.role.as_str() {
                "index" => {
                    if asset.file_name != expected_index_name {
                        return Err(PackError::Malformed(
                            "index asset does not use its canonical identity-derived name"
                                .to_owned(),
                        ));
                    }
                    index_path = Some(path);
                }
                "data" => {
                    data_paths.insert(asset.file_name.clone(), path);
                }
                _ => unreachable!("manifest roles were validated"),
            }
        }

        let index_path = index_path.ok_or_else(|| {
            PackError::Malformed("manifest does not declare an index asset".to_owned())
        })?;
        let index = open_database(&index_path, INDEX_APPLICATION_ID, INDEX_SCHEMA)?;
        let metadata = read_index_metadata(&index)?;
        cross_check_pack_metadata(&manifest, &pack_id, &revision, &metadata)?;
        validate_index_relations(&index, &metadata)?;

        let routes = read_shard_routes(&index)?;
        let shards = open_shards(
            &index,
            &metadata,
            &routes,
            data_paths,
            &manifest,
            &pack_id,
            &revision_hex,
        )?;

        Ok(Self {
            directory,
            pack_id,
            revision,
            corpus_language: metadata.corpus_language,
            manifest,
            limits,
            index: Mutex::new(index),
            shards,
        })
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub fn pack_id(&self) -> &PackId {
        &self.pack_id
    }

    #[must_use]
    pub const fn pack_revision(&self) -> &PackRevision {
        &self.revision
    }

    #[must_use]
    pub fn corpus_language(&self) -> &str {
        &self.corpus_language
    }

    #[must_use]
    pub const fn manifest(&self) -> &PackManifest {
        &self.manifest
    }

    /// Exhaustively authenticates and semantically validates every selected record
    /// and lookup route.
    ///
    /// This explicit qualification operation scans the complete corpus. Ordinary
    /// [`Self::open`] remains limited to envelope and relational admission checks.
    ///
    /// # Errors
    ///
    /// Returns an error when any record, known semantic structure, identity, route,
    /// lookup key, digest, or declared total disagrees with the authenticated source
    /// bytes.
    pub fn validate_all(&self) -> Result<ValidationReport, PackError> {
        let index = lock(&self.index)?;
        let metadata = read_index_metadata(&index)?;
        let mut entries = index.prepare(
            "SELECT entry_id, selected_ordinal, source_line_number, source_byte_offset, \
             shard_ordinal, authored_headword, language_code, record_sha256 \
             FROM entries ORDER BY selected_ordinal",
        )?;
        let mut lookup = index.prepare(
            "SELECT match_class, key_utf8, selected_ordinal, authored_key_ordinal \
             FROM lookup_keys \
             ORDER BY selected_ordinal, match_class, key_utf8, authored_key_ordinal",
        )?;
        let rows = entries.query_map([], index_entry_from_row)?;
        let mut lookup_rows = lookup.query([])?;
        let mut validation = ValidationState::new(self.shards.len());
        for row in rows {
            let entry = row?;
            validation.authenticate_entry(self, &entry, &mut lookup_rows)?;
        }
        if lookup_rows.next()?.is_some() {
            return Err(PackError::Corrupt(
                "lookup key rows were not covered exactly once".to_owned(),
            ));
        }
        validation.finish(self, &metadata, &index)
    }

    /// Performs the six-stage exact lookup and stops at the first non-empty stage.
    ///
    /// # Errors
    ///
    /// Returns an error when options exceed pack limits or selected storage fails
    /// record authentication.
    pub fn lookup(&self, query: &str, options: LookupOptions) -> Result<LookupOutcome, PackError> {
        self.lookup_for_language(query, options, None)
    }

    pub(crate) fn lookup_for_language(
        &self,
        query: &str,
        options: LookupOptions,
        target_language: Option<&str>,
    ) -> Result<LookupOutcome, PackError> {
        if query.is_empty() {
            return Ok(LookupOutcome {
                matches: Vec::new(),
                truncated: false,
            });
        }
        if query.len() > self.limits.max_query_bytes {
            return Err(PackError::Limit(
                "lookup query exceeds byte limit".to_owned(),
            ));
        }
        if options.limit == 0 || options.limit > self.limits.max_lookup_results {
            return Err(PackError::Limit(format!(
                "lookup limit must be between 1 and {}",
                self.limits.max_lookup_results
            )));
        }

        let keys = lookup_keys(query);
        let stages = [
            (MatchClass::AuthoredHeadword, keys.authored.as_slice()),
            (MatchClass::NfcHeadword, keys.nfc.as_slice()),
            (MatchClass::FoldedHeadword, keys.folded.as_slice()),
            (MatchClass::AuthoredForm, keys.authored.as_slice()),
            (MatchClass::NfcForm, keys.nfc.as_slice()),
            (MatchClass::FoldedForm, keys.folded.as_slice()),
        ];
        for (class, key) in stages {
            let rows = self.lookup_stage(class, key, options.limit)?;
            if rows.is_empty() {
                continue;
            }
            let truncated = rows.len() > options.limit;
            let mut matches = Vec::with_capacity(rows.len().min(options.limit));
            for row in rows.into_iter().take(options.limit) {
                let entry = self.load_record(&row, class, key, target_language)?;
                matches.push(LookupMatch { entry });
            }
            return Ok(LookupOutcome { matches, truncated });
        }
        Ok(LookupOutcome {
            matches: Vec::new(),
            truncated: false,
        })
    }

    fn lookup_stage(
        &self,
        class: MatchClass,
        key: &[u8],
        limit: usize,
    ) -> Result<Vec<IndexEntry>, PackError> {
        let index = lock(&self.index)?;
        let sql_limit = i64::try_from(limit.saturating_add(1))
            .map_err(|_| PackError::Limit("lookup limit is too large".to_owned()))?;
        let mut statement = index.prepare(
            "SELECT e.entry_id, e.selected_ordinal, e.source_line_number, e.source_byte_offset, \
             e.shard_ordinal, e.authored_headword, e.language_code, e.record_sha256 \
              FROM lookup_keys AS k JOIN entries AS e ON e.selected_ordinal = k.selected_ordinal \
             WHERE k.match_class = ?1 AND k.key_utf8 = ?2 \
             ORDER BY e.selected_ordinal, e.entry_id LIMIT ?3",
        )?;
        let rows = statement.query_map(params![class as u8, key, sql_limit], |row| {
            Ok(IndexEntry {
                entry_id: row.get(0)?,
                selected_ordinal: row.get(1)?,
                source_line_number: row.get(2)?,
                source_byte_offset: row.get(3)?,
                shard_ordinal: row.get(4)?,
                authored_headword: row.get(5)?,
                language_code: row.get(6)?,
                record_sha256: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(PackError::from)
    }

    fn load_record(
        &self,
        index: &IndexEntry,
        class: MatchClass,
        lookup_key: &[u8],
        target_language: Option<&str>,
    ) -> Result<EntrySummary, PackError> {
        let authenticated = self.authenticate_record(index)?;
        let selected_ordinal = authenticated.selected_ordinal;
        if !routing_matches(&authenticated.routing, class, lookup_key) {
            return Err(PackError::Corrupt(format!(
                "record {selected_ordinal} does not authenticate its lookup route"
            )));
        }
        self.project_record(&authenticated, class, target_language)
    }

    fn project_record(
        &self,
        authenticated: &AuthenticatedRecord,
        class: MatchClass,
        target_language: Option<&str>,
    ) -> Result<EntrySummary, PackError> {
        authenticated.routing.project(
            EntryReference {
                pack_id: self.pack_id.clone(),
                pack_revision: self.revision,
                entry_id: authenticated.entry_id,
                selected_ordinal: authenticated.selected_ordinal,
            },
            class,
            self.limits.semantic(),
            target_language,
        )
    }

    fn authenticate_record(&self, index: &IndexEntry) -> Result<AuthenticatedRecord, PackError> {
        let entry_id = bytes32("index entry_id", &index.entry_id)?;
        let record_hash = bytes32("index record_sha256", &index.record_sha256)?;
        let selected_ordinal = nonnegative("selected ordinal", index.selected_ordinal)?;
        let source_line_number = positive("source line number", index.source_line_number)?;
        let source_byte_offset = nonnegative("source byte offset", index.source_byte_offset)?;
        let shard_ordinal = nonnegative("shard ordinal", index.shard_ordinal)?;
        let shard_index = usize::try_from(shard_ordinal)
            .map_err(|_| PackError::Corrupt("shard ordinal is out of range".to_owned()))?;
        let shard = self.shards.get(shard_index).ok_or_else(|| {
            PackError::Corrupt(format!("entry routes to missing shard {shard_ordinal}"))
        })?;
        if shard.ordinal != shard_ordinal {
            return Err(PackError::Corrupt(
                "entry shard route is inconsistent".to_owned(),
            ));
        }
        let connection = lock(&shard.connection)?;
        let stored = connection.query_row(
            "SELECT entry_id, source_line_number, source_byte_offset, authored_headword, \
             language_code, uncompressed_size_bytes, compressed_size_bytes, record_sha256, \
             source_json_zstd FROM records WHERE selected_ordinal = ?1",
            [index.selected_ordinal],
            stored_record_from_row,
        )?;
        if stored.entry_id != index.entry_id
            || stored.source_line_number != index.source_line_number
            || stored.source_byte_offset != index.source_byte_offset
            || stored.authored_headword != index.authored_headword
            || stored.language_code != index.language_code
            || stored.record_sha256 != index.record_sha256
        {
            return Err(PackError::Corrupt(format!(
                "index and shard routing facts disagree for ordinal {selected_ordinal}"
            )));
        }
        let bytes = self.decompress_record(&stored, selected_ordinal)?;
        let output_size = usize::try_from(stored.uncompressed_size)
            .map_err(|_| PackError::Limit("record output size is too large".to_owned()))?;
        if bytes.len() != output_size || Sha256::digest(&bytes).as_slice() != record_hash {
            return Err(PackError::Corrupt(format!(
                "record {selected_ordinal} size or SHA-256 mismatch"
            )));
        }
        let derived = EntryId::derive(&self.revision, source_line_number, &record_hash);
        if derived.as_bytes() != &entry_id {
            return Err(PackError::Corrupt(format!(
                "record {selected_ordinal} entry identity mismatch"
            )));
        }
        let routing = RoutingRecord::parse(&bytes, self.limits.max_routing_forms)?;
        if routing.word != index.authored_headword || routing.lang_code != index.language_code {
            return Err(PackError::Corrupt(format!(
                "record {selected_ordinal} authored routing mismatch"
            )));
        }
        Ok(AuthenticatedRecord {
            entry_id: derived,
            selected_ordinal,
            source_line_number,
            source_byte_offset,
            shard_ordinal,
            bytes,
            routing,
        })
    }

    fn decompress_record(
        &self,
        stored: &StoredRecord,
        selected_ordinal: u64,
    ) -> Result<Vec<u8>, PackError> {
        let compressed_size = positive("compressed record size", stored.compressed_size)?;
        let uncompressed_size = positive("uncompressed record size", stored.uncompressed_size)?;
        if compressed_size != stored.compressed.len() as u64 {
            return Err(PackError::Corrupt(format!(
                "record {selected_ordinal} compressed size mismatch"
            )));
        }
        if compressed_size > self.limits.max_compressed_record_bytes
            || uncompressed_size > self.limits.max_uncompressed_record_bytes
        {
            return Err(PackError::Limit(format!(
                "record {selected_ordinal} exceeds configured compression bounds"
            )));
        }
        let output_size = usize::try_from(uncompressed_size)
            .map_err(|_| PackError::Limit("record output size is too large".to_owned()))?;
        zstd::bulk::decompress(&stored.compressed, output_size).map_err(|error| {
            PackError::Corrupt(format!(
                "record {selected_ordinal} cannot be decompressed: {error}"
            ))
        })
    }
}

struct IndexEntry {
    entry_id: Vec<u8>,
    selected_ordinal: i64,
    source_line_number: i64,
    source_byte_offset: i64,
    shard_ordinal: i64,
    authored_headword: String,
    language_code: String,
    record_sha256: Vec<u8>,
}

struct StoredRecord {
    entry_id: Vec<u8>,
    source_line_number: i64,
    source_byte_offset: i64,
    authored_headword: String,
    language_code: String,
    uncompressed_size: i64,
    compressed_size: i64,
    record_sha256: Vec<u8>,
    compressed: Vec<u8>,
}

struct AuthenticatedRecord {
    entry_id: EntryId,
    selected_ordinal: u64,
    source_line_number: u64,
    source_byte_offset: u64,
    shard_ordinal: u64,
    bytes: Vec<u8>,
    routing: RoutingRecord,
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
struct LookupRow {
    match_class: i64,
    key: Vec<u8>,
    selected_ordinal: i64,
    authored_ordinal: i64,
}

struct ValidationState {
    selected_digest: FramedDigest,
    shard_digests: Vec<FramedDigest>,
    record_count: u64,
    record_bytes: u64,
    lookup_count: u64,
}

impl ValidationState {
    fn new(shard_count: usize) -> Self {
        Self {
            selected_digest: FramedDigest::new(SELECTED_STREAM_DIGEST_DOMAIN),
            shard_digests: (0..shard_count)
                .map(|_| FramedDigest::new(SHARD_RECORD_DIGEST_DOMAIN))
                .collect(),
            record_count: 0,
            record_bytes: 0,
            lookup_count: 0,
        }
    }

    fn authenticate_entry(
        &mut self,
        pack: &DictionaryPack,
        entry: &IndexEntry,
        lookup: &mut rusqlite::Rows<'_>,
    ) -> Result<(), PackError> {
        let ordinal = nonnegative("selected ordinal", entry.selected_ordinal)?;
        if ordinal != self.record_count {
            return Err(PackError::Corrupt(
                "selected records are not in contiguous ordinal order".to_owned(),
            ));
        }
        let authenticated = pack.authenticate_record(entry)?;
        pack.project_record(&authenticated, MatchClass::AuthoredHeadword, None)
            .map_err(|error| contextualize_record_error(ordinal, error))?;
        frame_record(
            &mut self.selected_digest,
            ordinal,
            authenticated.source_line_number,
            authenticated.source_byte_offset,
            &authenticated.bytes,
        )?;
        let shard_index = usize::try_from(authenticated.shard_ordinal)
            .map_err(|_| PackError::Corrupt("shard ordinal is out of range".to_owned()))?;
        let shard_digest = self.shard_digests.get_mut(shard_index).ok_or_else(|| {
            PackError::Corrupt(format!(
                "record {ordinal} routes to missing shard {}",
                authenticated.shard_ordinal
            ))
        })?;
        frame_record(
            shard_digest,
            ordinal,
            authenticated.source_line_number,
            authenticated.source_byte_offset,
            &authenticated.bytes,
        )?;
        self.authenticate_lookup_rows(entry, &authenticated, lookup)?;
        self.record_bytes = self
            .record_bytes
            .checked_add(
                u64::try_from(authenticated.bytes.len())
                    .map_err(|_| PackError::Corrupt("record byte count overflow".to_owned()))?,
            )
            .ok_or_else(|| PackError::Corrupt("record byte count overflow".to_owned()))?;
        self.record_count = self
            .record_count
            .checked_add(1)
            .ok_or_else(|| PackError::Corrupt("record count overflow".to_owned()))?;
        Ok(())
    }

    fn authenticate_lookup_rows(
        &mut self,
        entry: &IndexEntry,
        authenticated: &AuthenticatedRecord,
        lookup: &mut rusqlite::Rows<'_>,
    ) -> Result<(), PackError> {
        let expected = expected_lookup_rows(&authenticated.routing, entry.selected_ordinal)?;
        for expected_row in &expected {
            let actual_row = lookup.next()?.ok_or_else(|| {
                PackError::Corrupt(format!(
                    "lookup keys disagree with record {}",
                    authenticated.selected_ordinal
                ))
            })?;
            let actual = lookup_row_from_row(actual_row)?;
            if &actual != expected_row {
                return Err(PackError::Corrupt(format!(
                    "lookup keys disagree with record {}",
                    authenticated.selected_ordinal
                )));
            }
        }
        self.lookup_count = self
            .lookup_count
            .checked_add(
                u64::try_from(expected.len())
                    .map_err(|_| PackError::Corrupt("lookup key count overflow".to_owned()))?,
            )
            .ok_or_else(|| PackError::Corrupt("lookup key count overflow".to_owned()))?;
        Ok(())
    }

    fn finish(
        self,
        pack: &DictionaryPack,
        metadata: &IndexMetadata,
        index: &Connection,
    ) -> Result<ValidationReport, PackError> {
        if self.record_count != metadata.record_count
            || self.record_bytes != metadata.record_bytes
            || self.selected_digest.finish() != metadata.record_digest
        {
            return Err(PackError::Corrupt(
                "selected record stream digest or totals mismatch".to_owned(),
            ));
        }
        for (shard, digest) in pack.shards.iter().zip(self.shard_digests) {
            if digest.finish() != shard.route.record_digest {
                return Err(PackError::Corrupt(format!(
                    "record digest mismatch for shard {}",
                    shard.ordinal
                )));
            }
        }
        let stored_lookup_count: i64 =
            index.query_row("SELECT count(*) FROM lookup_keys", [], |row| row.get(0))?;
        if nonnegative("stored lookup key count", stored_lookup_count)? != self.lookup_count {
            return Err(PackError::Corrupt(
                "lookup key rows were not covered exactly once".to_owned(),
            ));
        }
        Ok(ValidationReport {
            authenticated_record_count: self.record_count,
            uncompressed_record_bytes: self.record_bytes,
            lookup_key_row_count: self.lookup_count,
        })
    }
}

struct IndexMetadata {
    pack_id: String,
    revision: [u8; 32],
    corpus_language: String,
    wiktionary_edition: String,
    wiktionary_dump_date: String,
    kaikki_extraction_date: String,
    source_url: String,
    compressed_source_sha256: [u8; 32],
    uncompressed_source_sha256: [u8; 32],
    wiktextract_revision: String,
    wikitextprocessor_revision: String,
    builder_revision: String,
    unicode_profile: String,
    compression_profile: String,
    routing_policy: String,
    target_shard_payload_bytes: u64,
    record_count: u64,
    record_bytes: u64,
    record_digest: [u8; 32],
    source_manifest_sha256: [u8; 32],
    source_manifest_json: String,
    license_manifest_sha256: [u8; 32],
    license_manifest_json: String,
    compatible_audio_collection: Option<String>,
    minimum_app_version: String,
}

#[derive(Clone)]
struct ShardRoute {
    ordinal: u64,
    file_name: String,
    file_hash: [u8; 32],
    file_size: u64,
    record_count: u64,
    first: u64,
    last: u64,
    compressed_bytes: u64,
    uncompressed_bytes: u64,
    record_digest: [u8; 32],
}

fn validate_manifest(manifest: &PackManifest, limits: PackLimits) -> Result<(), PackError> {
    if manifest.manifest_version != 1 || manifest.format_version != FORMAT_VERSION {
        return Err(PackError::Malformed(
            "unsupported manifest or pack format version".to_owned(),
        ));
    }
    if manifest.selected_record_count == 0 || manifest.selected_record_bytes == 0 {
        return Err(PackError::Malformed(
            "manifest record totals must be greater than zero".to_owned(),
        ));
    }
    if manifest.assets.len() > limits.max_assets {
        return Err(PackError::Limit(
            "manifest declares too many assets".to_owned(),
        ));
    }
    let mut names = BTreeSet::new();
    let mut indexes = 0;
    let mut data = 0;
    for asset in &manifest.assets {
        safe_asset_name(&asset.file_name)?;
        if !names.insert(asset.file_name.as_str()) {
            return Err(PackError::Malformed(format!(
                "duplicate asset name `{}`",
                asset.file_name
            )));
        }
        if asset.size_bytes == 0 || asset.size_bytes > limits.max_asset_bytes {
            return Err(PackError::Limit(format!(
                "asset `{}` has an invalid declared size",
                asset.file_name
            )));
        }
        match asset.role.as_str() {
            "index" => indexes += 1,
            "data" => data += 1,
            role => {
                return Err(PackError::Malformed(format!(
                    "unsupported asset role `{role}`"
                )));
            }
        }
    }
    if indexes != 1 || data == 0 {
        return Err(PackError::Malformed(
            "manifest must declare exactly one index and at least one data asset".to_owned(),
        ));
    }
    Ok(())
}

fn safe_asset_name(name: &str) -> Result<(), PackError> {
    let mut components = Path::new(name).components();
    let safe = !name.is_empty()
        && !name.contains('\\')
        && matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none();
    if safe {
        Ok(())
    } else {
        Err(PackError::Malformed(format!(
            "unsafe relative asset name `{name}`"
        )))
    }
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, PackError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
        return Err(PackError::Limit(format!(
            "file `{}` exceeds its admission bound",
            path.display()
        )));
    }
    fs::read(path).map_err(|source| io_error(path, source))
}

fn validate_asset_bytes(path: &Path, size: u64, expected: &[u8; 32]) -> Result<(), PackError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != size {
        return Err(PackError::Corrupt(format!(
            "asset size mismatch for `{}`",
            path.display()
        )));
    }
    let mut file = File::open(path).map_err(|source| io_error(path, source))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| io_error(path, source))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    if digest.finalize().as_slice() != expected {
        return Err(PackError::Corrupt(format!(
            "asset SHA-256 mismatch for `{}`",
            path.display()
        )));
    }
    Ok(())
}

fn open_database(
    path: &Path,
    application_id: u32,
    expected_schema: &str,
) -> Result<Connection, PackError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF;")?;
    let query_only: i64 = connection.query_row("PRAGMA query_only", [], |row| row.get(0))?;
    let actual_application_id: u32 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let page_size: u32 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let encoding: String = connection.query_row("PRAGMA encoding", [], |row| row.get(0))?;
    if query_only != 1
        || actual_application_id != application_id
        || version != FORMAT_VERSION
        || page_size != 4096
        || encoding != "UTF-8"
    {
        return Err(PackError::Malformed(format!(
            "database identity mismatch for `{}`",
            path.display()
        )));
    }
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(PackError::Corrupt(format!(
            "SQLite integrity check failed for `{}`: {integrity}",
            path.display()
        )));
    }
    let actual_objects = schema_objects(&connection)?;
    let expected_connection = Connection::open_in_memory()?;
    expected_connection.execute_batch(expected_schema)?;
    let expected = schema_objects(&expected_connection)?;
    if actual_objects != expected {
        return Err(PackError::Malformed(format!(
            "database schema object set mismatch for `{}`",
            path.display()
        )));
    }
    Ok(connection)
}

fn schema_objects(
    connection: &Connection,
) -> Result<BTreeSet<(String, String, String, String)>, PackError> {
    let mut statement = connection.prepare(
        "SELECT type, name, tbl_name, sql FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })?;
    rows.collect::<Result<_, _>>().map_err(PackError::from)
}

fn read_index_metadata(connection: &Connection) -> Result<IndexMetadata, PackError> {
    let count: i64 =
        connection.query_row("SELECT count(*) FROM pack_metadata", [], |row| row.get(0))?;
    if count != 1 {
        return Err(PackError::Corrupt(
            "index must contain one metadata row".to_owned(),
        ));
    }
    let raw = connection.query_row(
        "SELECT pack_id, pack_revision, corpus_language, wiktionary_edition, \
         wiktionary_dump_date, kaikki_extraction_date, source_url, compressed_source_sha256, \
         uncompressed_source_sha256, wiktextract_revision, wikitextprocessor_revision, \
         builder_revision, unicode_profile, compression_profile, routing_policy, \
         target_shard_payload_bytes, selected_record_count, selected_record_bytes, \
         selected_record_digest, source_manifest_sha256, source_manifest_json, \
         license_manifest_sha256, license_manifest_json, compatible_audio_collection, \
         minimum_app_version \
         FROM pack_metadata WHERE singleton = 1",
        [],
        |row| {
            Ok(RawIndexMetadata {
                pack_id: row.get(0)?,
                revision: row.get(1)?,
                corpus_language: row.get(2)?,
                wiktionary_edition: row.get(3)?,
                wiktionary_dump_date: row.get(4)?,
                kaikki_extraction_date: row.get(5)?,
                source_url: row.get(6)?,
                compressed_source_sha256: row.get(7)?,
                uncompressed_source_sha256: row.get(8)?,
                wiktextract_revision: row.get(9)?,
                wikitextprocessor_revision: row.get(10)?,
                builder_revision: row.get(11)?,
                unicode_profile: row.get(12)?,
                compression_profile: row.get(13)?,
                routing_policy: row.get(14)?,
                target_shard_payload_bytes: row.get(15)?,
                record_count: row.get(16)?,
                record_bytes: row.get(17)?,
                record_digest: row.get(18)?,
                source_manifest_sha256: row.get(19)?,
                source_manifest_json: row.get(20)?,
                license_manifest_sha256: row.get(21)?,
                license_manifest_json: row.get(22)?,
                compatible_audio_collection: row.get(23)?,
                minimum_app_version: row.get(24)?,
            })
        },
    )?;
    Ok(IndexMetadata {
        pack_id: raw.pack_id,
        revision: bytes32("pack revision", &raw.revision)?,
        corpus_language: raw.corpus_language,
        wiktionary_edition: raw.wiktionary_edition,
        wiktionary_dump_date: raw.wiktionary_dump_date,
        kaikki_extraction_date: raw.kaikki_extraction_date,
        source_url: raw.source_url,
        compressed_source_sha256: bytes32(
            "compressed source SHA-256",
            &raw.compressed_source_sha256,
        )?,
        uncompressed_source_sha256: bytes32(
            "uncompressed source SHA-256",
            &raw.uncompressed_source_sha256,
        )?,
        wiktextract_revision: raw.wiktextract_revision,
        wikitextprocessor_revision: raw.wikitextprocessor_revision,
        builder_revision: raw.builder_revision,
        unicode_profile: raw.unicode_profile,
        compression_profile: raw.compression_profile,
        routing_policy: raw.routing_policy,
        target_shard_payload_bytes: positive(
            "target shard payload bytes",
            raw.target_shard_payload_bytes,
        )?,
        record_count: positive("selected record count", raw.record_count)?,
        record_bytes: positive("selected record bytes", raw.record_bytes)?,
        record_digest: bytes32("selected record digest", &raw.record_digest)?,
        source_manifest_sha256: bytes32("source manifest SHA-256", &raw.source_manifest_sha256)?,
        source_manifest_json: raw.source_manifest_json,
        license_manifest_sha256: bytes32("license manifest SHA-256", &raw.license_manifest_sha256)?,
        license_manifest_json: raw.license_manifest_json,
        compatible_audio_collection: raw.compatible_audio_collection,
        minimum_app_version: raw.minimum_app_version,
    })
}

struct RawIndexMetadata {
    pack_id: String,
    revision: Vec<u8>,
    corpus_language: String,
    wiktionary_edition: String,
    wiktionary_dump_date: String,
    kaikki_extraction_date: String,
    source_url: String,
    compressed_source_sha256: Vec<u8>,
    uncompressed_source_sha256: Vec<u8>,
    wiktextract_revision: String,
    wikitextprocessor_revision: String,
    builder_revision: String,
    unicode_profile: String,
    compression_profile: String,
    routing_policy: String,
    target_shard_payload_bytes: i64,
    record_count: i64,
    record_bytes: i64,
    record_digest: Vec<u8>,
    source_manifest_sha256: Vec<u8>,
    source_manifest_json: String,
    license_manifest_sha256: Vec<u8>,
    license_manifest_json: String,
    compatible_audio_collection: Option<String>,
    minimum_app_version: String,
}

fn cross_check_pack_metadata(
    manifest: &PackManifest,
    pack_id: &PackId,
    revision: &PackRevision,
    metadata: &IndexMetadata,
) -> Result<(), PackError> {
    authenticate_manifest_json(
        "source manifest",
        &metadata.source_manifest_json,
        &metadata.source_manifest_sha256,
    )?;
    authenticate_manifest_json(
        "license manifest",
        &metadata.license_manifest_json,
        &metadata.license_manifest_sha256,
    )?;
    let derived_revision = PackRevision::derive(PackRevisionInputs {
        pack_id,
        corpus_language: &metadata.corpus_language,
        wiktionary_edition: &metadata.wiktionary_edition,
        wiktionary_dump_date: &metadata.wiktionary_dump_date,
        kaikki_extraction_date: &metadata.kaikki_extraction_date,
        source_url: &metadata.source_url,
        compressed_source_sha256: &metadata.compressed_source_sha256,
        uncompressed_source_sha256: &metadata.uncompressed_source_sha256,
        wiktextract_revision: &metadata.wiktextract_revision,
        wikitextprocessor_revision: &metadata.wikitextprocessor_revision,
        builder_revision: &metadata.builder_revision,
        compression_profile: &metadata.compression_profile,
        routing_policy: &metadata.routing_policy,
        target_shard_payload_bytes: metadata.target_shard_payload_bytes,
        source_manifest_sha256: &metadata.source_manifest_sha256,
        license_manifest_sha256: &metadata.license_manifest_sha256,
        compatible_audio_collection: metadata.compatible_audio_collection.as_deref(),
        minimum_app_version: &metadata.minimum_app_version,
    });
    if metadata.pack_id != pack_id.as_str()
        || &metadata.revision != revision.as_bytes()
        || derived_revision != *revision
        || metadata.unicode_profile != UNICODE_PROFILE
        || metadata.compression_profile != COMPRESSION_PROFILE
        || metadata.routing_policy != ROUTING_POLICY
        || metadata.record_count != manifest.selected_record_count
        || metadata.record_bytes != manifest.selected_record_bytes
        || metadata.record_digest != *manifest.selected_record_digest.as_bytes()
        || metadata.corpus_language.is_empty()
        || metadata.minimum_app_version.is_empty()
    {
        return Err(PackError::Corrupt(
            "manifest and index metadata disagree".to_owned(),
        ));
    }
    Ok(())
}

fn authenticate_manifest_json(
    name: &str,
    json: &str,
    expected: &[u8; 32],
) -> Result<(), PackError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| PackError::Corrupt(format!("stored {name} is invalid JSON: {error}")))?;
    let canonical = serde_json::to_string(&value).map_err(|error| {
        PackError::Corrupt(format!("stored {name} cannot be serialized: {error}"))
    })?;
    if Sha256::digest(canonical.as_bytes()).as_slice() != expected {
        return Err(PackError::Corrupt(format!(
            "stored {name} SHA-256 mismatch"
        )));
    }
    Ok(())
}

fn validate_index_relations(
    connection: &Connection,
    metadata: &IndexMetadata,
) -> Result<(), PackError> {
    let foreign_keys: i64 =
        connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    let (count, first, last): (i64, Option<i64>, Option<i64>) = connection.query_row(
        "SELECT count(*), min(selected_ordinal), max(selected_ordinal) FROM entries",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let expected_last = metadata.record_count.checked_sub(1);
    if foreign_keys != 0
        || nonnegative("entry count", count)? != metadata.record_count
        || first != Some(0)
        || last.and_then(|value| u64::try_from(value).ok()) != expected_last
    {
        return Err(PackError::Corrupt(
            "index entry relations or record totals are inconsistent".to_owned(),
        ));
    }
    Ok(())
}

fn read_shard_routes(connection: &Connection) -> Result<Vec<ShardRoute>, PackError> {
    let mut statement = connection.prepare(
        "SELECT shard_ordinal, file_name, file_sha256, file_size_bytes, record_count, \
         first_selected_ordinal, last_selected_ordinal, compressed_payload_bytes, \
         uncompressed_record_bytes, shard_record_digest FROM data_shards ORDER BY shard_ordinal",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, Vec<u8>>(9)?,
        ))
    })?;
    rows.map(|row| {
        let row = row?;
        Ok(ShardRoute {
            ordinal: nonnegative("shard ordinal", row.0)?,
            file_name: row.1,
            file_hash: bytes32("shard file SHA-256", &row.2)?,
            file_size: positive("shard file size", row.3)?,
            record_count: positive("shard record count", row.4)?,
            first: nonnegative("first selected ordinal", row.5)?,
            last: nonnegative("last selected ordinal", row.6)?,
            compressed_bytes: positive("compressed shard bytes", row.7)?,
            uncompressed_bytes: positive("uncompressed shard bytes", row.8)?,
            record_digest: bytes32("shard record digest", &row.9)?,
        })
    })
    .collect()
}

fn open_shards(
    index: &Connection,
    metadata: &IndexMetadata,
    routes: &[ShardRoute],
    mut data_paths: BTreeMap<String, PathBuf>,
    manifest: &PackManifest,
    pack_id: &PackId,
    revision_hex: &str,
) -> Result<Vec<DataShard>, PackError> {
    if routes.len() != data_paths.len() {
        return Err(PackError::Corrupt(
            "manifest and index declare different data-shard counts".to_owned(),
        ));
    }
    validate_shard_routes(index, metadata, routes)?;
    let mut shards = Vec::with_capacity(routes.len());
    for (position, route) in routes.iter().enumerate() {
        let expected_ordinal = u64::try_from(position)
            .map_err(|_| PackError::Corrupt("too many shard routes".to_owned()))?;
        if route.ordinal != expected_ordinal {
            return Err(PackError::Corrupt(
                "data-shard ordinals are not contiguous from zero".to_owned(),
            ));
        }
        let expected_name = format!(
            "{}-{revision_hex}-data-{expected_ordinal:03}.eldict",
            pack_id.as_str()
        );
        if route.file_name != expected_name {
            return Err(PackError::Malformed(format!(
                "data shard {} does not use its canonical identity-derived name",
                route.ordinal
            )));
        }
        let path = data_paths.remove(&route.file_name).ok_or_else(|| {
            PackError::Corrupt(format!(
                "index routes undeclared data asset `{}`",
                route.file_name
            ))
        })?;
        let asset = manifest
            .assets
            .iter()
            .find(|asset| asset.file_name == route.file_name)
            .ok_or_else(|| {
                PackError::Corrupt(format!("manifest lost routed asset `{}`", route.file_name))
            })?;
        if route.file_size != asset.size_bytes || route.file_hash != *asset.sha256.as_bytes() {
            return Err(PackError::Corrupt(format!(
                "index asset facts disagree for `{}`",
                route.file_name
            )));
        }
        let connection = open_database(&path, DATA_APPLICATION_ID, DATA_SCHEMA)?;
        validate_shard(&connection, metadata, route)?;
        shards.push(DataShard {
            ordinal: route.ordinal,
            route: route.clone(),
            connection: Mutex::new(connection),
        });
    }
    Ok(shards)
}

fn validate_shard_routes(
    connection: &Connection,
    metadata: &IndexMetadata,
    routes: &[ShardRoute],
) -> Result<(), PackError> {
    let mut next_ordinal = 0_u64;
    let mut total_count = 0_u64;
    let mut total_bytes = 0_u64;
    for route in routes {
        let (count, first, last, wrong_language): (i64, Option<i64>, Option<i64>, i64) = connection
            .query_row(
                "SELECT count(*), min(selected_ordinal), max(selected_ordinal), \
                 coalesce(sum(language_code != ?2), 0) FROM entries WHERE shard_ordinal = ?1",
                params![
                    i64::try_from(route.ordinal).map_err(|_| PackError::Corrupt(
                        "shard ordinal exceeds SQLite integer range".to_owned()
                    ))?,
                    &metadata.corpus_language
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        let route_matches = route.first == next_ordinal
            && nonnegative("index shard entry count", count)? == route.record_count
            && first.and_then(|value| u64::try_from(value).ok()) == Some(route.first)
            && last.and_then(|value| u64::try_from(value).ok()) == Some(route.last)
            && wrong_language == 0;
        if !route_matches {
            return Err(PackError::Corrupt(format!(
                "index entry routes disagree for shard {}",
                route.ordinal
            )));
        }
        next_ordinal = route
            .last
            .checked_add(1)
            .ok_or_else(|| PackError::Corrupt("shard ordinal range overflow".to_owned()))?;
        total_count = total_count
            .checked_add(route.record_count)
            .ok_or_else(|| PackError::Corrupt("shard record count overflow".to_owned()))?;
        total_bytes = total_bytes
            .checked_add(route.uncompressed_bytes)
            .ok_or_else(|| PackError::Corrupt("shard record byte count overflow".to_owned()))?;
    }
    if total_count != metadata.record_count
        || next_ordinal != metadata.record_count
        || total_bytes != metadata.record_bytes
    {
        return Err(PackError::Corrupt(
            "data-shard routes do not cover the selected record stream".to_owned(),
        ));
    }
    Ok(())
}

fn validate_shard(
    connection: &Connection,
    metadata: &IndexMetadata,
    route: &ShardRoute,
) -> Result<(), PackError> {
    let count: i64 =
        connection.query_row("SELECT count(*) FROM shard_metadata", [], |row| row.get(0))?;
    if count != 1 {
        return Err(PackError::Corrupt(
            "shard must contain one metadata row".to_owned(),
        ));
    }
    let shard = connection.query_row(
        "SELECT pack_id, pack_revision, shard_ordinal, corpus_language, record_count, \
         first_selected_ordinal, last_selected_ordinal, shard_record_digest \
         FROM shard_metadata WHERE singleton = 1",
        [],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Vec<u8>>(7)?,
            ))
        },
    )?;
    let aggregate: (i64, Option<i64>, Option<i64>, Option<i64>, Option<i64>) = connection
        .query_row(
            "SELECT count(*), min(selected_ordinal), max(selected_ordinal), \
         sum(compressed_size_bytes), sum(uncompressed_size_bytes) FROM records",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
    let consistent = shard.0 == metadata.pack_id
        && bytes32("shard pack revision", &shard.1)? == metadata.revision
        && nonnegative("shard metadata ordinal", shard.2)? == route.ordinal
        && shard.3 == metadata.corpus_language
        && positive("shard metadata count", shard.4)? == route.record_count
        && nonnegative("shard metadata first ordinal", shard.5)? == route.first
        && nonnegative("shard metadata last ordinal", shard.6)? == route.last
        && bytes32("shard metadata digest", &shard.7)? == route.record_digest
        && nonnegative("shard row count", aggregate.0)? == route.record_count
        && aggregate.1.and_then(|value| u64::try_from(value).ok()) == Some(route.first)
        && aggregate.2.and_then(|value| u64::try_from(value).ok()) == Some(route.last)
        && aggregate.3.and_then(|value| u64::try_from(value).ok()) == Some(route.compressed_bytes)
        && aggregate.4.and_then(|value| u64::try_from(value).ok())
            == Some(route.uncompressed_bytes)
        && route
            .last
            .checked_sub(route.first)
            .and_then(|value| value.checked_add(1))
            == Some(route.record_count);
    if !consistent {
        return Err(PackError::Corrupt(format!(
            "data-shard metadata mismatch for ordinal {}",
            route.ordinal
        )));
    }
    Ok(())
}

fn routing_matches(record: &RoutingRecord, class: MatchClass, key: &[u8]) -> bool {
    let mut values: Box<dyn Iterator<Item = &str> + '_> = match class {
        MatchClass::AuthoredHeadword | MatchClass::NfcHeadword | MatchClass::FoldedHeadword => {
            Box::new(std::iter::once(record.word.as_str()))
        }
        MatchClass::AuthoredForm | MatchClass::NfcForm | MatchClass::FoldedForm => {
            Box::new(record.forms.iter().map(String::as_str))
        }
    };
    values.any(|value| {
        let keys = lookup_keys(value);
        match class {
            MatchClass::AuthoredHeadword | MatchClass::AuthoredForm => keys.authored == key,
            MatchClass::NfcHeadword | MatchClass::NfcForm => keys.nfc == key,
            MatchClass::FoldedHeadword | MatchClass::FoldedForm => keys.folded == key,
        }
    })
}

fn index_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IndexEntry> {
    Ok(IndexEntry {
        entry_id: row.get(0)?,
        selected_ordinal: row.get(1)?,
        source_line_number: row.get(2)?,
        source_byte_offset: row.get(3)?,
        shard_ordinal: row.get(4)?,
        authored_headword: row.get(5)?,
        language_code: row.get(6)?,
        record_sha256: row.get(7)?,
    })
}

fn stored_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRecord> {
    Ok(StoredRecord {
        entry_id: row.get(0)?,
        source_line_number: row.get(1)?,
        source_byte_offset: row.get(2)?,
        authored_headword: row.get(3)?,
        language_code: row.get(4)?,
        uncompressed_size: row.get(5)?,
        compressed_size: row.get(6)?,
        record_sha256: row.get(7)?,
        compressed: row.get(8)?,
    })
}

fn lookup_row_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LookupRow> {
    Ok(LookupRow {
        match_class: row.get(0)?,
        key: row.get(1)?,
        selected_ordinal: row.get(2)?,
        authored_ordinal: row.get(3)?,
    })
}

fn expected_lookup_rows(
    record: &RoutingRecord,
    selected_ordinal: i64,
) -> Result<Vec<LookupRow>, PackError> {
    let mut rows = Vec::new();
    let headword = lookup_keys(&record.word);
    for (class, key) in [
        (1_i64, headword.authored),
        (2, headword.nfc),
        (3, headword.folded),
    ] {
        rows.push(LookupRow {
            match_class: class,
            key,
            selected_ordinal,
            authored_ordinal: 0,
        });
    }
    let mut seen = BTreeSet::new();
    for (form_index, form) in record.forms.iter().enumerate() {
        let authored_ordinal = i64::try_from(form_index)
            .map_err(|_| PackError::Corrupt("authored form ordinal overflow".to_owned()))?;
        let keys = lookup_keys(form);
        for (class, key) in [(4_i64, keys.authored), (5, keys.nfc), (6, keys.folded)] {
            if seen.insert((class, key.clone())) {
                rows.push(LookupRow {
                    match_class: class,
                    key,
                    selected_ordinal,
                    authored_ordinal,
                });
            }
        }
    }
    rows.sort();
    Ok(rows)
}

fn frame_record(
    digest: &mut FramedDigest,
    selected_ordinal: u64,
    source_line_number: u64,
    source_byte_offset: u64,
    bytes: &[u8],
) -> Result<(), PackError> {
    digest.field(&selected_ordinal.to_be_bytes())?;
    digest.field(&source_line_number.to_be_bytes())?;
    digest.field(&source_byte_offset.to_be_bytes())?;
    digest.field(bytes)
}

struct FramedDigest(Sha256);

impl FramedDigest {
    fn new(domain: &[u8]) -> Self {
        let mut digest = Self(Sha256::new());
        digest
            .field(domain)
            .expect("format digest domain length fits in u64");
        digest
    }

    fn field(&mut self, bytes: &[u8]) -> Result<(), PackError> {
        let length = u64::try_from(bytes.len())
            .map_err(|_| PackError::Corrupt("digest field length overflow".to_owned()))?;
        self.0.update(length.to_be_bytes());
        self.0.update(bytes);
        Ok(())
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

fn bytes32(name: &str, bytes: &[u8]) -> Result<[u8; 32], PackError> {
    bytes
        .try_into()
        .map_err(|_| PackError::Corrupt(format!("{name} is not 32 bytes")))
}

fn nonnegative(name: &str, value: i64) -> Result<u64, PackError> {
    u64::try_from(value).map_err(|_| PackError::Corrupt(format!("{name} is negative")))
}

fn positive(name: &str, value: i64) -> Result<u64, PackError> {
    let value = nonnegative(name, value)?;
    if value == 0 {
        Err(PackError::Corrupt(format!("{name} is zero")))
    } else {
        Ok(value)
    }
}

fn contextualize_record_error(selected_ordinal: u64, error: PackError) -> PackError {
    match error {
        PackError::Corrupt(message) => PackError::Corrupt(format!(
            "record {selected_ordinal} semantic validation failed: {message}"
        )),
        other => other,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, PackError> {
    mutex
        .lock()
        .map_err(|_| PackError::Corrupt("database lock was poisoned".to_owned()))
}

fn io_error(path: &Path, source: io::Error) -> PackError {
    PackError::Io {
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_manifest_authentication_rejects_invalid_json() {
        assert!(matches!(
            authenticate_manifest_json("source manifest", "{", &[0; 32]),
            Err(PackError::Corrupt(message)) if message.contains("invalid JSON")
        ));
    }
}
