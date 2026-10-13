use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OpenFlags, params};
use sha2::{Digest, Sha256};

use crate::semantic::RoutingRecord;
use crate::{
    CorpusIdentity, DATA_APPLICATION_ID, DATA_SCHEMA, Entry, EntryId, EntryReference,
    FORMAT_VERSION, FramedDigest, INDEX_APPLICATION_ID, INDEX_SCHEMA, MatchClass,
    PACK_MANIFEST_FILE, PackError, PackId, PackManifest, PackRevision, ProjectionOptions,
    SELECTED_STREAM_DIGEST_DOMAIN, SHARD_RECORD_DIGEST_DOMAIN, Section, SectionPage, Sha256Hex,
    lookup_keys,
};

pub(crate) const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// The most assets a pack manifest may declare.
pub const MAX_ASSETS: usize = 256;
pub(crate) const MAX_ASSET_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// The longest lookup query, in bytes.
pub const MAX_QUERY_BYTES: usize = 4096;
/// The most matches one lookup returns.
pub const MAX_LOOKUP_RESULTS: usize = 100;
const MAX_COMPRESSED_RECORD_BYTES: u64 = 16 * 1024 * 1024;
const MAX_UNCOMPRESSED_RECORD_BYTES: u64 = 64 * 1024 * 1024;
/// The most `forms[].form` values one record may route lookups through.
pub const MAX_ROUTING_FORMS: usize = 4096;
/// The most items one section page returns.
pub const MAX_SECTION_PAGE_ITEMS: usize = 256;

/// Per-call exact lookup controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LookupOptions {
    pub limit: usize,
    pub projection: ProjectionOptions,
}

impl Default for LookupOptions {
    fn default() -> Self {
        Self {
            limit: 20,
            projection: ProjectionOptions::summary(),
        }
    }
}

/// One exact match and its bounded typed semantic projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LookupMatch {
    pub match_class: MatchClass,
    pub entry: Entry,
}

/// Results from the first non-empty exact-match stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LookupOutcome {
    pub matches: Vec<LookupMatch>,
    /// More rows matched the returning stage than `limit` allowed.
    pub truncated: bool,
}

/// Every audio file referenced by a pack's records.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AudioReferences {
    /// Normalized Commons file names with the number of referencing sounds.
    pub files: BTreeMap<String, u64>,
    /// Authored audio values that are not plausible Commons file names.
    pub invalid: BTreeMap<String, u64>,
}

/// Facts authenticated by a complete traversal of a pack's selected corpus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidationReport {
    pub authenticated_record_count: u64,
    pub uncompressed_record_bytes: u64,
    pub lookup_key_row_count: u64,
}

/// Pack identity obtained through a trusted channel, such as a verified catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedPack {
    pub pack_id: PackId,
    pub pack_revision: PackRevision,
    /// SHA-256 of the exact manifest file bytes.
    pub manifest_sha256: Sha256Hex,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Admission {
    /// Hashes every asset byte and checks complete relational structure.
    Full,
    /// Trusts previously verified asset bytes and checks identity and structure cheaply.
    Installed,
}

struct DataShard {
    route: ShardRoute,
    connection: Mutex<Connection>,
}

struct IndexMetadata {
    identity: CorpusIdentity,
    record_count: u64,
    record_bytes: u64,
    record_digest: Sha256Hex,
}

/// An admitted immutable dictionary pack with synchronous exact lookup.
pub struct DictionaryPack {
    directory: PathBuf,
    revision: PackRevision,
    metadata: IndexMetadata,
    manifest: PackManifest,
    index: Mutex<Connection>,
    shards: Vec<DataShard>,
}

impl DictionaryPack {
    /// Opens and validates the immutable envelope and relational structure of a
    /// format-v1 pack directory. Trust is relative to the pack's own manifest.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed manifests, unsafe assets, byte mismatches,
    /// invalid databases, or inconsistent redundant metadata.
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, PackError> {
        Self::admit(directory.as_ref(), None, Admission::Full)
    }

    /// Performs complete admission of a pack that must match a trusted identity.
    /// Use this once before a pack becomes installed.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::open`], and [`PackError::Corrupt`] when the
    /// manifest bytes, pack identifier, or revision differ from `expected`.
    pub fn verify(directory: impl AsRef<Path>, expected: &ExpectedPack) -> Result<Self, PackError> {
        Self::admit(directory.as_ref(), Some(expected), Admission::Full)
    }

    /// Opens a pack previously admitted by [`Self::verify`] without hashing asset
    /// bytes or running `SQLite` integrity checks. The manifest digest, asset sizes,
    /// database identities, exact schemas, and metadata identity are still checked,
    /// and every record served is authenticated against its stored digest.
    ///
    /// # Errors
    ///
    /// Returns an error when any of those checks disagree with `expected`.
    pub fn open_installed(
        directory: impl AsRef<Path>,
        expected: &ExpectedPack,
    ) -> Result<Self, PackError> {
        Self::admit(directory.as_ref(), Some(expected), Admission::Installed)
    }

    fn admit(
        directory: &Path,
        expected: Option<&ExpectedPack>,
        admission: Admission,
    ) -> Result<Self, PackError> {
        let directory = directory.to_owned();
        let manifest_bytes = read_bounded(&directory.join(PACK_MANIFEST_FILE), MAX_MANIFEST_BYTES)?;
        if let Some(expected) = expected
            && Sha256::digest(&manifest_bytes).as_slice() != expected.manifest_sha256.as_bytes()
        {
            return Err(PackError::Corrupt(
                "pack manifest SHA-256 differs from the expected pack".to_owned(),
            ));
        }
        let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|error| PackError::Malformed(format!("invalid pack manifest: {error}")))?;
        validate_manifest(&manifest)?;
        let revision = manifest.pack_revision;
        if let Some(expected) = expected
            && (expected.pack_id != manifest.pack_id || expected.pack_revision != revision)
        {
            return Err(PackError::Corrupt(
                "pack identity differs from the expected pack".to_owned(),
            ));
        }
        let expected_index_name = format!("{}-{revision}-index.eldict", manifest.pack_id);

        let mut index_path = None;
        let mut data_paths = BTreeMap::new();
        for asset in &manifest.assets {
            let path = directory.join(&asset.file_name);
            match admission {
                Admission::Full => {
                    validate_asset_bytes(&path, asset.size_bytes, asset.sha256.as_bytes())?;
                }
                Admission::Installed => validate_asset_size(&path, asset.size_bytes)?,
            }
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
        let index = open_database(&index_path, INDEX_APPLICATION_ID, INDEX_SCHEMA, admission)?;
        let metadata = read_index_metadata(&index)?;
        cross_check_pack_metadata(&manifest, &metadata)?;
        if admission == Admission::Full {
            validate_index_relations(&index, &metadata)?;
        }

        let routes = read_shard_routes(&index)?;
        let shards = open_shards(&index, &metadata, &routes, data_paths, &manifest, admission)?;

        Ok(Self {
            directory,
            revision,
            metadata,
            manifest,
            index: Mutex::new(index),
            shards,
        })
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub const fn pack_id(&self) -> &PackId {
        &self.manifest.pack_id
    }

    #[must_use]
    pub const fn pack_revision(&self) -> &PackRevision {
        &self.revision
    }

    #[must_use]
    pub fn corpus_language(&self) -> &str {
        &self.metadata.identity.corpus_language
    }

    /// The authenticated pinned inputs this pack was built from.
    #[must_use]
    pub const fn identity(&self) -> &CorpusIdentity {
        &self.metadata.identity
    }

    #[must_use]
    pub const fn manifest(&self) -> &PackManifest {
        &self.manifest
    }

    /// Exhaustively authenticates and semantically validates every selected record
    /// and lookup route. Ordinary admission remains limited to envelope and
    /// relational checks.
    ///
    /// # Errors
    ///
    /// Returns an error when any record, known semantic structure, identity, route,
    /// lookup key, digest, or declared total disagrees with the authenticated source
    /// bytes.
    pub fn validate_all(&self) -> Result<ValidationReport, PackError> {
        let index = lock(&self.index)?;
        let mut entries =
            index.prepare(&format!("{INDEX_ENTRY_COLUMNS} ORDER BY selected_ordinal"))?;
        let mut lookup = index.prepare(
            "SELECT match_class, key_utf8, selected_ordinal, authored_key_ordinal \
             FROM lookup_keys \
             ORDER BY selected_ordinal, match_class, key_utf8, authored_key_ordinal",
        )?;
        let rows = entries.query_map([], index_entry_from_row)?;
        let mut lookup_rows = lookup.query([])?;
        let mut validation = ValidationState::new(self.shards.len());
        for row in rows {
            validation.authenticate_entry(self, &row?, &mut lookup_rows)?;
        }
        if lookup_rows.next()?.is_some() {
            return Err(PackError::Corrupt(
                "lookup key rows were not covered exactly once".to_owned(),
            ));
        }
        validation.finish(self, &index)
    }

    /// Scans every authenticated record and collects its audio references.
    ///
    /// # Errors
    ///
    /// Returns an error when any record fails authentication or has a malformed
    /// `sounds` structure.
    pub fn audio_references(&self) -> Result<AudioReferences, PackError> {
        let entries = {
            let index = lock(&self.index)?;
            let mut statement =
                index.prepare(&format!("{INDEX_ENTRY_COLUMNS} ORDER BY selected_ordinal"))?;
            statement
                .query_map([], index_entry_from_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut references = AudioReferences::default();
        for entry in &entries {
            let record = self.authenticate_record(entry)?;
            for authored in record.routing.audio_values()? {
                let target = match crate::commons_file_name(authored) {
                    Some(file_name) => references.files.entry(file_name),
                    None => references.invalid.entry(authored.to_owned()),
                };
                *target.or_insert(0) += 1;
            }
        }
        Ok(references)
    }

    /// Performs the six-stage exact lookup and stops at the first non-empty stage.
    /// A target language restricts projected translations to that exact code.
    ///
    /// # Errors
    ///
    /// Returns an error when options exceed pack limits or selected storage fails
    /// record authentication.
    pub fn lookup(
        &self,
        query: &str,
        target_language: Option<&str>,
        options: LookupOptions,
    ) -> Result<LookupOutcome, PackError> {
        if query.is_empty() {
            return Ok(LookupOutcome {
                matches: Vec::new(),
                truncated: false,
            });
        }
        if query.len() > MAX_QUERY_BYTES {
            return Err(PackError::Limit(
                "lookup query exceeds byte limit".to_owned(),
            ));
        }
        if options.limit == 0 || options.limit > MAX_LOOKUP_RESULTS {
            return Err(PackError::Limit(format!(
                "lookup limit must be between 1 and {MAX_LOOKUP_RESULTS}"
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
                let authenticated = self.authenticate_record(&row)?;
                if !routing_matches(&authenticated.routing, class, key) {
                    return Err(PackError::Corrupt(format!(
                        "record {} does not authenticate its lookup route",
                        authenticated.selected_ordinal
                    )));
                }
                matches.push(LookupMatch {
                    match_class: class,
                    entry: self.project_record(
                        &authenticated,
                        options.projection,
                        target_language,
                    )?,
                });
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
        let mut statement = index.prepare(
            "SELECT e.entry_id, e.selected_ordinal, e.source_line_number, e.source_byte_offset, \
             e.shard_ordinal, e.authored_headword, e.language_code, e.record_sha256 \
              FROM lookup_keys AS k JOIN entries AS e ON e.selected_ordinal = k.selected_ordinal \
             WHERE k.match_class = ?1 AND k.key_utf8 = ?2 \
             ORDER BY e.selected_ordinal, e.entry_id LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![class as u8, key, (limit + 1) as u64],
            index_entry_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(PackError::from)
    }

    /// Reads and projects one entry of this pack by reference.
    ///
    /// # Errors
    ///
    /// Returns [`PackError::UnknownEntry`] when the reference belongs to another pack
    /// or revision or names no entry, and record-authentication errors otherwise.
    pub fn entry(
        &self,
        reference: &EntryReference,
        target_language: Option<&str>,
        projection: ProjectionOptions,
    ) -> Result<Entry, PackError> {
        let authenticated = self.authenticate_reference(reference)?;
        self.project_record(&authenticated, projection, target_language)
    }

    /// Reads one page of one section of an entry.
    ///
    /// # Errors
    ///
    /// Returns [`PackError::Limit`] when `limit` is zero or exceeds
    /// [`MAX_SECTION_PAGE_ITEMS`], and the errors of [`Self::entry`].
    pub fn section_page(
        &self,
        reference: &EntryReference,
        target_language: Option<&str>,
        section: Section,
        offset: usize,
        limit: usize,
        projection: ProjectionOptions,
    ) -> Result<SectionPage, PackError> {
        if limit == 0 || limit > MAX_SECTION_PAGE_ITEMS {
            return Err(PackError::Limit(format!(
                "section page limit must be between 1 and {MAX_SECTION_PAGE_ITEMS}"
            )));
        }
        let authenticated = self.authenticate_reference(reference)?;
        authenticated
            .routing
            .section(section, offset, limit, projection, target_language)
    }

    fn authenticate_reference(
        &self,
        reference: &EntryReference,
    ) -> Result<AuthenticatedRecord, PackError> {
        if reference.pack_id != self.manifest.pack_id || reference.pack_revision != self.revision {
            return Err(PackError::UnknownEntry);
        }
        let row = {
            let index = lock(&self.index)?;
            let mut statement =
                index.prepare_cached(&format!("{INDEX_ENTRY_COLUMNS} WHERE entry_id = ?1"))?;
            let mut rows = statement.query_map(
                [reference.entry_id.as_bytes().as_slice()],
                index_entry_from_row,
            )?;
            rows.next().transpose()?
        };
        let row = row.ok_or(PackError::UnknownEntry)?;
        let authenticated = self.authenticate_record(&row)?;
        if authenticated.selected_ordinal != reference.selected_ordinal {
            return Err(PackError::UnknownEntry);
        }
        Ok(authenticated)
    }

    fn project_record(
        &self,
        authenticated: &AuthenticatedRecord,
        projection: ProjectionOptions,
        target_language: Option<&str>,
    ) -> Result<Entry, PackError> {
        if projection.text_bytes == 0 {
            return Err(PackError::Limit(
                "projection text bound must be greater than zero".to_owned(),
            ));
        }
        authenticated.routing.project(
            EntryReference {
                pack_id: self.manifest.pack_id.clone(),
                pack_revision: self.revision,
                entry_id: authenticated.entry_id,
                selected_ordinal: authenticated.selected_ordinal,
            },
            projection,
            target_language,
        )
    }

    fn authenticate_record(&self, index: &IndexEntry) -> Result<AuthenticatedRecord, PackError> {
        let entry_id = bytes32("index entry_id", &index.entry_id)?;
        let record_hash = bytes32("index record_sha256", &index.record_sha256)?;
        let selected_ordinal = index.selected_ordinal;
        let shard = usize::try_from(index.shard_ordinal)
            .ok()
            .and_then(|ordinal| self.shards.get(ordinal))
            .ok_or_else(|| {
                PackError::Corrupt(format!(
                    "entry routes to missing shard {}",
                    index.shard_ordinal
                ))
            })?;
        let connection = lock(&shard.connection)?;
        let stored = connection.query_row(
            "SELECT entry_id, source_line_number, source_byte_offset, authored_headword, \
             language_code, uncompressed_size_bytes, compressed_size_bytes, record_sha256, \
             source_json_zstd FROM records WHERE selected_ordinal = ?1",
            [selected_ordinal],
            |row| {
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
            },
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
        let bytes = decompress_record(&stored, selected_ordinal)?;
        if bytes.len() as u64 != stored.uncompressed_size
            || Sha256::digest(&bytes).as_slice() != record_hash
        {
            return Err(PackError::Corrupt(format!(
                "record {selected_ordinal} size or SHA-256 mismatch"
            )));
        }
        let derived = EntryId::derive(&self.revision, index.source_line_number, &record_hash);
        if derived.as_bytes() != &entry_id {
            return Err(PackError::Corrupt(format!(
                "record {selected_ordinal} entry identity mismatch"
            )));
        }
        let routing = RoutingRecord::parse(&bytes)?;
        if routing.word != index.authored_headword || routing.lang_code != index.language_code {
            return Err(PackError::Corrupt(format!(
                "record {selected_ordinal} authored routing mismatch"
            )));
        }
        Ok(AuthenticatedRecord {
            entry_id: derived,
            selected_ordinal,
            source_line_number: index.source_line_number,
            source_byte_offset: index.source_byte_offset,
            shard_ordinal: index.shard_ordinal,
            bytes,
            routing,
        })
    }
}

fn decompress_record(stored: &StoredRecord, selected_ordinal: u64) -> Result<Vec<u8>, PackError> {
    if stored.compressed_size == 0
        || stored.uncompressed_size == 0
        || stored.compressed_size != stored.compressed.len() as u64
    {
        return Err(PackError::Corrupt(format!(
            "record {selected_ordinal} compressed size mismatch"
        )));
    }
    if stored.compressed_size > MAX_COMPRESSED_RECORD_BYTES
        || stored.uncompressed_size > MAX_UNCOMPRESSED_RECORD_BYTES
    {
        return Err(PackError::Limit(format!(
            "record {selected_ordinal} exceeds the record size limits"
        )));
    }
    let output_size = usize::try_from(stored.uncompressed_size)
        .map_err(|_| PackError::Limit("record output size is too large".to_owned()))?;
    zstd::bulk::decompress(&stored.compressed, output_size).map_err(|error| {
        PackError::Corrupt(format!(
            "record {selected_ordinal} cannot be decompressed: {error}"
        ))
    })
}

const INDEX_ENTRY_COLUMNS: &str = "SELECT entry_id, selected_ordinal, source_line_number, \
     source_byte_offset, shard_ordinal, authored_headword, language_code, record_sha256 \
     FROM entries";

struct IndexEntry {
    entry_id: Vec<u8>,
    selected_ordinal: u64,
    source_line_number: u64,
    source_byte_offset: u64,
    shard_ordinal: u64,
    authored_headword: String,
    language_code: String,
    record_sha256: Vec<u8>,
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

struct StoredRecord {
    entry_id: Vec<u8>,
    source_line_number: u64,
    source_byte_offset: u64,
    authored_headword: String,
    language_code: String,
    uncompressed_size: u64,
    compressed_size: u64,
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
    match_class: u8,
    key: Vec<u8>,
    selected_ordinal: u64,
    authored_ordinal: u64,
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
        let ordinal = entry.selected_ordinal;
        if ordinal != self.record_count {
            return Err(PackError::Corrupt(
                "selected records are not in contiguous ordinal order".to_owned(),
            ));
        }
        let authenticated = pack.authenticate_record(entry)?;
        pack.project_record(&authenticated, ProjectionOptions::exhaustive(), None)
            .map_err(|error| match error {
                PackError::Corrupt(message) => PackError::Corrupt(format!(
                    "record {ordinal} semantic validation failed: {message}"
                )),
                other => other,
            })?;
        frame_record(&mut self.selected_digest, &authenticated);
        let shard_digest = usize::try_from(authenticated.shard_ordinal)
            .ok()
            .and_then(|shard| self.shard_digests.get_mut(shard))
            .ok_or_else(|| {
                PackError::Corrupt(format!(
                    "record {ordinal} routes to missing shard {}",
                    authenticated.shard_ordinal
                ))
            })?;
        frame_record(shard_digest, &authenticated);
        let expected = expected_lookup_rows(&authenticated.routing, ordinal);
        for expected_row in &expected {
            let actual = lookup
                .next()?
                .map(|row| {
                    Ok::<_, rusqlite::Error>(LookupRow {
                        match_class: row.get(0)?,
                        key: row.get(1)?,
                        selected_ordinal: row.get(2)?,
                        authored_ordinal: row.get(3)?,
                    })
                })
                .transpose()?;
            if actual.as_ref() != Some(expected_row) {
                return Err(PackError::Corrupt(format!(
                    "lookup keys disagree with record {ordinal}"
                )));
            }
        }
        self.lookup_count += expected.len() as u64;
        self.record_bytes += authenticated.bytes.len() as u64;
        self.record_count += 1;
        Ok(())
    }

    fn finish(
        self,
        pack: &DictionaryPack,
        index: &Connection,
    ) -> Result<ValidationReport, PackError> {
        let metadata = &pack.metadata;
        if self.record_count != metadata.record_count
            || self.record_bytes != metadata.record_bytes
            || self.selected_digest.finish() != *metadata.record_digest.as_bytes()
        {
            return Err(PackError::Corrupt(
                "selected record stream digest or totals mismatch".to_owned(),
            ));
        }
        for (shard, digest) in pack.shards.iter().zip(self.shard_digests) {
            if digest.finish() != shard.route.record_digest {
                return Err(PackError::Corrupt(format!(
                    "record digest mismatch for shard {}",
                    shard.route.ordinal
                )));
            }
        }
        let stored_lookup_count: u64 =
            index.query_row("SELECT count(*) FROM lookup_keys", [], |row| row.get(0))?;
        if stored_lookup_count != self.lookup_count {
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

fn validate_manifest(manifest: &PackManifest) -> Result<(), PackError> {
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
    if manifest.assets.len() > MAX_ASSETS {
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
        if asset.size_bytes == 0 || asset.size_bytes > MAX_ASSET_BYTES {
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

pub(crate) fn safe_asset_name(name: &str) -> Result<(), PackError> {
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

pub(crate) fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, PackError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
        return Err(PackError::Limit(format!(
            "file `{}` exceeds its admission bound",
            path.display()
        )));
    }
    fs::read(path).map_err(|source| io_error(path, source))
}

pub(crate) fn validate_asset_size(path: &Path, size: u64) -> Result<(), PackError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != size {
        return Err(PackError::Corrupt(format!(
            "asset size mismatch for `{}`",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn validate_asset_bytes(
    path: &Path,
    size: u64,
    expected: &[u8; 32],
) -> Result<(), PackError> {
    validate_asset_size(path, size)?;
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

pub(crate) fn open_database(
    path: &Path,
    application_id: u32,
    expected_schema: &str,
    admission: Admission,
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
    if admission == Admission::Full {
        let integrity: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(PackError::Corrupt(format!(
                "SQLite integrity check failed for `{}`: {integrity}",
                path.display()
            )));
        }
    }
    let expected_connection = Connection::open_in_memory()?;
    expected_connection.execute_batch(expected_schema)?;
    if schema_objects(&connection)? != schema_objects(&expected_connection)? {
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
    connection
        .query_row(
            "SELECT pack_id, corpus_language, wiktionary_edition, wiktionary_dump_date, \
         kaikki_extraction_date, source_url, compressed_source_sha256, \
         uncompressed_source_sha256, wiktextract_revision, wikitextprocessor_revision, \
         builder_revision, target_shard_payload_bytes, source_manifest_json, \
         license_manifest_json, minimum_app_version, selected_record_count, \
         selected_record_bytes, selected_record_digest \
         FROM pack_metadata WHERE singleton = 1",
            [],
            |row| {
                Ok(IndexMetadata {
                    identity: CorpusIdentity {
                        pack_id: row.get(0)?,
                        corpus_language: row.get(1)?,
                        wiktionary_edition: row.get(2)?,
                        wiktionary_dump_date: row.get(3)?,
                        kaikki_extraction_date: row.get(4)?,
                        source_url: row.get(5)?,
                        compressed_source_sha256: digest_column(row, 6)?,
                        uncompressed_source_sha256: digest_column(row, 7)?,
                        wiktextract_revision: row.get(8)?,
                        wikitextprocessor_revision: row.get(9)?,
                        builder_revision: row.get(10)?,
                        target_shard_payload_bytes: row.get(11)?,
                        source_manifest: row.get(12)?,
                        license_manifest: row.get(13)?,
                        minimum_app_version: row.get(14)?,
                    },
                    record_count: row.get(15)?,
                    record_bytes: row.get(16)?,
                    record_digest: digest_column(row, 17)?,
                })
            },
        )
        .map_err(PackError::from)
}

fn digest_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Sha256Hex> {
    let bytes: Vec<u8> = row.get(index)?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map(Sha256Hex::from_bytes)
        .map_err(|_| {
            rusqlite::Error::InvalidColumnType(
                index,
                "digest".to_owned(),
                rusqlite::types::Type::Blob,
            )
        })
}

impl rusqlite::types::FromSql for PackId {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|error| rusqlite::types::FromSqlError::Other(Box::new(error)))
    }
}

fn cross_check_pack_metadata(
    manifest: &PackManifest,
    metadata: &IndexMetadata,
) -> Result<(), PackError> {
    let identity = &metadata.identity;
    if identity.pack_id != manifest.pack_id
        || identity.revision() != manifest.pack_revision
        || metadata.record_count != manifest.selected_record_count
        || metadata.record_bytes != manifest.selected_record_bytes
        || metadata.record_digest != manifest.selected_record_digest
        || identity.corpus_language.is_empty()
        || identity.minimum_app_version.is_empty()
    {
        return Err(PackError::Corrupt(
            "manifest and index metadata disagree".to_owned(),
        ));
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
    let (count, first, last): (u64, Option<u64>, Option<u64>) = connection.query_row(
        "SELECT count(*), min(selected_ordinal), max(selected_ordinal) FROM entries",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if foreign_keys != 0
        || count != metadata.record_count
        || first != Some(0)
        || last != metadata.record_count.checked_sub(1)
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
        Ok(ShardRoute {
            ordinal: row.get(0)?,
            file_name: row.get(1)?,
            file_hash: *digest_column(row, 2)?.as_bytes(),
            file_size: row.get(3)?,
            record_count: row.get(4)?,
            first: row.get(5)?,
            last: row.get(6)?,
            compressed_bytes: row.get(7)?,
            uncompressed_bytes: row.get(8)?,
            record_digest: *digest_column(row, 9)?.as_bytes(),
        })
    })?;
    rows.collect::<Result<_, _>>().map_err(PackError::from)
}

fn open_shards(
    index: &Connection,
    metadata: &IndexMetadata,
    routes: &[ShardRoute],
    mut data_paths: BTreeMap<String, PathBuf>,
    manifest: &PackManifest,
    admission: Admission,
) -> Result<Vec<DataShard>, PackError> {
    if routes.len() != data_paths.len() {
        return Err(PackError::Corrupt(
            "manifest and index declare different data-shard counts".to_owned(),
        ));
    }
    if admission == Admission::Full {
        validate_shard_routes(index, metadata, routes)?;
    }
    let mut shards = Vec::with_capacity(routes.len());
    for (position, route) in routes.iter().enumerate() {
        if route.ordinal != position as u64 {
            return Err(PackError::Corrupt(
                "data-shard ordinals are not contiguous from zero".to_owned(),
            ));
        }
        let expected_name = format!(
            "{}-{}-data-{position:03}.eldict",
            manifest.pack_id, manifest.pack_revision
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
        let connection = open_database(&path, DATA_APPLICATION_ID, DATA_SCHEMA, admission)?;
        validate_shard(&connection, manifest, metadata, route, admission)?;
        shards.push(DataShard {
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
        let (count, first, last, wrong_language): (u64, Option<u64>, Option<u64>, i64) = connection
            .query_row(
                "SELECT count(*), min(selected_ordinal), max(selected_ordinal), \
                 coalesce(sum(language_code != ?2), 0) FROM entries WHERE shard_ordinal = ?1",
                params![route.ordinal, &metadata.identity.corpus_language],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        let route_matches = route.first == next_ordinal
            && count == route.record_count
            && first == Some(route.first)
            && last == Some(route.last)
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
        total_count += route.record_count;
        total_bytes += route.uncompressed_bytes;
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
    manifest: &PackManifest,
    metadata: &IndexMetadata,
    route: &ShardRoute,
    admission: Admission,
) -> Result<(), PackError> {
    let count: i64 =
        connection.query_row("SELECT count(*) FROM shard_metadata", [], |row| row.get(0))?;
    if count != 1 {
        return Err(PackError::Corrupt(
            "shard must contain one metadata row".to_owned(),
        ));
    }
    let shard: (PackId, Vec<u8>, u64, String, u64, u64, u64, Vec<u8>) = connection.query_row(
        "SELECT pack_id, pack_revision, shard_ordinal, corpus_language, record_count, \
         first_selected_ordinal, last_selected_ordinal, shard_record_digest \
         FROM shard_metadata WHERE singleton = 1",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        },
    )?;
    let metadata_consistent = shard.0 == manifest.pack_id
        && shard.1 == manifest.pack_revision.as_bytes()
        && shard.2 == route.ordinal
        && shard.3 == metadata.identity.corpus_language
        && shard.4 == route.record_count
        && shard.5 == route.first
        && shard.6 == route.last
        && shard.7 == route.record_digest
        && route.record_count > 0
        && route
            .last
            .checked_sub(route.first)
            .and_then(|value| value.checked_add(1))
            == Some(route.record_count);
    let consistent = metadata_consistent
        && (admission == Admission::Installed || shard_records_match_route(connection, route)?);
    if !consistent {
        return Err(PackError::Corrupt(format!(
            "data-shard metadata mismatch for ordinal {}",
            route.ordinal
        )));
    }
    Ok(())
}

fn shard_records_match_route(
    connection: &Connection,
    route: &ShardRoute,
) -> Result<bool, PackError> {
    let aggregate: (u64, Option<u64>, Option<u64>, Option<u64>, Option<u64>) = connection
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
    Ok(aggregate.0 == route.record_count
        && aggregate.1 == Some(route.first)
        && aggregate.2 == Some(route.last)
        && aggregate.3 == Some(route.compressed_bytes)
        && aggregate.4 == Some(route.uncompressed_bytes))
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

fn expected_lookup_rows(record: &RoutingRecord, selected_ordinal: u64) -> Vec<LookupRow> {
    let mut rows = Vec::new();
    let headword = lookup_keys(&record.word);
    for (class, key) in [
        (1, headword.authored),
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
        let keys = lookup_keys(form);
        for (class, key) in [(4, keys.authored), (5, keys.nfc), (6, keys.folded)] {
            if seen.insert((class, key.clone())) {
                rows.push(LookupRow {
                    match_class: class,
                    key,
                    selected_ordinal,
                    authored_ordinal: form_index as u64,
                });
            }
        }
    }
    rows.sort();
    rows
}

fn frame_record(digest: &mut FramedDigest, record: &AuthenticatedRecord) {
    digest.field(&record.selected_ordinal.to_be_bytes());
    digest.field(&record.source_line_number.to_be_bytes());
    digest.field(&record.source_byte_offset.to_be_bytes());
    digest.field(&record.bytes);
}

pub(crate) fn bytes32(name: &str, bytes: &[u8]) -> Result<[u8; 32], PackError> {
    bytes
        .try_into()
        .map_err(|_| PackError::Corrupt(format!("{name} is not 32 bytes")))
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, PackError> {
    mutex
        .lock()
        .map_err(|_| PackError::Corrupt("database lock was poisoned".to_owned()))
}

pub(crate) fn io_error(path: &Path, source: io::Error) -> PackError {
    PackError::Io {
        path: path.to_owned(),
        source,
    }
}
