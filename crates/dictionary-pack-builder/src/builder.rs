use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use elephant_ladder_dictionary_pack::{
    COMPRESSION_PROFILE, EntryId, FORMAT_VERSION, PACK_MANIFEST_FILE, PackError, PackRevision,
    ROUTING_POLICY, SELECTED_STREAM_DIGEST_DOMAIN, SHARD_RECORD_DIGEST_DOMAIN, UNICODE_PROFILE,
    initialize_data_database, initialize_index_database, lookup_keys,
};
use rusqlite::{Connection, Transaction, params};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::manifest::{BuildManifest, PackAsset, PackManifest, Sha256Hex};
use crate::report::{
    BUILD_REPORT_FILE, BuildReport, CompressionRatio, LargestSelectedRecord, ReportAsset,
    SelectedReport, SourceReport, SourceShapeObservation, TranslationCoverage,
};
use crate::{JsonlSourceReader, SourceIngestionError, SourceRecord};

const ZSTD_LEVEL: i32 = 6;
const LOOKUP_KEY_BATCH_ROWS: usize = 262_144;

/// Resource limits applied while ingesting an otherwise pinned source.
#[derive(Clone, Copy, Debug)]
pub struct BuildOptions {
    pub max_line_bytes: usize,
    pub max_routing_forms: usize,
    pub max_assets: usize,
    pub max_asset_bytes: u64,
    pub max_shape_depth: usize,
    pub max_shape_path_bytes: usize,
    pub max_shape_observations: usize,
    pub max_shape_nodes: u64,
    pub max_largest_records: usize,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            max_line_bytes: 64 * 1024 * 1024,
            max_routing_forms: 4_096,
            max_assets: 256,
            max_asset_bytes: 1_900_000_000,
            max_shape_depth: 64,
            max_shape_path_bytes: 4_096,
            max_shape_observations: 100_000,
            max_shape_nodes: 1_000_000_000,
            max_largest_records: 20,
        }
    }
}

/// Facts returned after a pack directory has been published.
#[derive(Clone, Debug)]
pub struct BuildResult {
    pub output_directory: PathBuf,
    pub pack_revision: PackRevision,
    pub manifest: PackManifest,
    pub report: BuildReport,
}

/// A deterministic pack construction failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BuildError {
    #[error("invalid build manifest: {0}")]
    InvalidManifest(String),
    #[error(transparent)]
    Pack(#[from] PackError),
    #[error(transparent)]
    Source(#[from] SourceIngestionError),
    #[error("filesystem operation `{operation}` failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("SQLite operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("Zstandard compression failed for selected record {selected_ordinal}: {source}")]
    Compression {
        selected_ordinal: u64,
        #[source]
        source: io::Error,
    },
    #[error("{name} exceeds SQLite's signed 64-bit integer range: {value}")]
    IntegerRange { name: &'static str, value: u64 },
    #[error("no records matched language code `{0}`")]
    NoSelectedRecords(String),
    #[error("uncompressed source SHA-256 mismatch: expected {expected}, found {actual}")]
    SourceDigestMismatch { expected: String, actual: String },
    #[error("output directory already exists: {0}")]
    OutputExists(PathBuf),
    #[error("pack build limit exceeded: {0}")]
    BuildLimit(String),
    #[error("cannot inspect selected source line {line_number}: {source}")]
    ObservationJson {
        line_number: u64,
        #[source]
        source: serde_json::Error,
    },
}

/// Builds and atomically publishes one immutable pack from decompressed JSONL.
///
/// Selected-stream and shard-record digests frame the domain separator first. Each
/// record then contributes, in order, four fields: selected ordinal as u64 BE,
/// one-based source line as u64 BE, source byte offset as u64 BE, and exact
/// delimiter-free source bytes. Every field is prefixed by its u64 BE byte length.
///
/// # Errors
///
/// Returns a precise manifest, source, storage, compression, or publication error.
#[allow(clippy::too_many_lines)]
pub fn build_pack<R: Read>(
    manifest: &BuildManifest,
    source: R,
    output_directory: impl AsRef<Path>,
    options: BuildOptions,
) -> Result<BuildResult, BuildError> {
    validate_manifest(manifest, options)?;
    let pack_id = manifest.pack_id()?;
    let revision = manifest.revision(&pack_id);
    let output_directory = output_directory.as_ref();
    let mut staging = StagingDirectory::create(output_directory)?;

    let source_manifest_json = canonical_manifest_json(
        "source_manifest",
        &manifest.source_manifest,
        manifest.source_manifest_sha256,
    )?;
    let license_manifest_json = canonical_manifest_json(
        "license_manifest",
        &manifest.license_manifest,
        manifest.license_manifest_sha256,
    )?;

    let revision_hex = revision.to_hex();
    let index_file_name = format!("{}-{revision_hex}-index.eldict", pack_id.as_str());
    let index_path = staging.path().join(&index_file_name);
    let mut index = open_index(&index_path)?;
    let transaction = index.transaction()?;
    transaction.execute_batch("PRAGMA defer_foreign_keys = ON;")?;

    let mut source_digest_reader = DigestReader::new(source);
    let mut selected_digest = FramedDigest::new(SELECTED_STREAM_DIGEST_DOMAIN);
    let mut selected_count = 0_u64;
    let mut selected_bytes = 0_u64;
    let mut shard = None;
    let mut shard_assets = Vec::new();
    let mut compressed_payload_bytes = 0_u64;
    let mut observations = ShapeObservations::default();
    let mut translation_coverage = TranslationCoverage::default();
    let mut largest_records = Vec::new();
    let mut compressor = None;
    let mut lookup_keys = LookupKeyBuffer::default();

    let source_record_count = {
        let mut records = JsonlSourceReader::new(
            &mut source_digest_reader,
            &manifest.corpus_language,
            options.max_line_bytes,
            options.max_routing_forms,
        );
        for record in records.by_ref() {
            let record = record?;
            let selected_ordinal = selected_count;
            let record_size =
                u64::try_from(record.source_bytes.len()).map_err(|_| BuildError::IntegerRange {
                    name: "record byte length",
                    value: u64::MAX,
                })?;
            selected_bytes =
                selected_bytes
                    .checked_add(record_size)
                    .ok_or(BuildError::IntegerRange {
                        name: "selected record bytes",
                        value: u64::MAX,
                    })?;
            frame_record(&mut selected_digest, selected_ordinal, &record);

            let record_sha256: [u8; 32] = Sha256::digest(&record.source_bytes).into();
            observe_record(
                &record,
                &mut observations,
                &mut translation_coverage,
                options,
            )?;
            retain_largest_record(
                &mut largest_records,
                LargestSelectedRecord {
                    selected_ordinal,
                    source_line: record.line_number,
                    size_bytes: record_size,
                    sha256: Sha256Hex::from_bytes(record_sha256),
                },
                options.max_largest_records,
            );
            let entry_id = EntryId::derive(&revision, record.line_number, &record_sha256);
            let compressor = match &mut compressor {
                Some(compressor) => compressor,
                None => compressor.insert(zstd::bulk::Compressor::new(ZSTD_LEVEL).map_err(
                    |source| BuildError::Compression {
                        selected_ordinal,
                        source,
                    },
                )?),
            };
            let compressed = compressor
                .compress(&record.source_bytes)
                .map_err(|source| BuildError::Compression {
                    selected_ordinal,
                    source,
                })?;
            let compressed_size =
                u64::try_from(compressed.len()).map_err(|_| BuildError::IntegerRange {
                    name: "compressed record byte length",
                    value: u64::MAX,
                })?;

            if shard.as_ref().is_some_and(|current: &ShardWriter| {
                current.record_count > 0
                    && current
                        .compressed_payload_bytes
                        .checked_add(compressed_size)
                        .is_none_or(|size| size > manifest.target_shard_payload_bytes)
            }) && let Some(current) = shard.take()
            {
                let completed = finish_shard(current, &transaction, options.max_asset_bytes)?;
                checked_increment(
                    &mut compressed_payload_bytes,
                    completed.compressed_payload_bytes,
                    "compressed pack payload bytes",
                )?;
                shard_assets.push(completed.asset);
            }
            if shard.is_none() {
                // Reserve one manifest slot for the index in addition to this shard.
                if shard_assets.len().saturating_add(2) > options.max_assets {
                    return Err(BuildError::BuildLimit(format!(
                        "pack would exceed the {}-asset limit",
                        options.max_assets
                    )));
                }
                let ordinal =
                    u64::try_from(shard_assets.len()).map_err(|_| BuildError::IntegerRange {
                        name: "shard ordinal",
                        value: u64::MAX,
                    })?;
                shard = Some(ShardWriter::create(
                    staging.path(),
                    pack_id.as_str(),
                    &revision,
                    ordinal,
                    selected_ordinal,
                    &manifest.corpus_language,
                )?);
            }
            let current = shard.as_mut().ok_or_else(|| {
                BuildError::InvalidManifest("internal shard allocation failure".to_owned())
            })?;
            current.insert(
                selected_ordinal,
                &entry_id,
                &record,
                &record_sha256,
                &compressed,
            )?;
            insert_index_record(
                &transaction,
                current.ordinal,
                selected_ordinal,
                &entry_id,
                &record,
                &record_sha256,
                &manifest.corpus_language,
                &mut lookup_keys,
            )?;
            selected_count = selected_count
                .checked_add(1)
                .ok_or(BuildError::IntegerRange {
                    name: "selected record count",
                    value: u64::MAX,
                })?;
        }
        records.source_record_count()
    };

    let source_size_bytes = source_digest_reader.bytes_read();
    let actual_source_sha256 = source_digest_reader.finish();
    if &actual_source_sha256 != manifest.uncompressed_source_sha256.as_bytes() {
        return Err(BuildError::SourceDigestMismatch {
            expected: manifest.uncompressed_source_sha256.to_hex(),
            actual: hex::encode(actual_source_sha256),
        });
    }
    if selected_count == 0 {
        return Err(BuildError::NoSelectedRecords(
            manifest.corpus_language.clone(),
        ));
    }
    let completed = finish_shard(
        shard.ok_or_else(|| {
            BuildError::InvalidManifest("internal shard allocation failure".to_owned())
        })?,
        &transaction,
        options.max_asset_bytes,
    )?;
    checked_increment(
        &mut compressed_payload_bytes,
        completed.compressed_payload_bytes,
        "compressed pack payload bytes",
    )?;
    shard_assets.push(completed.asset);

    let selected_record_digest = selected_digest.finish();
    lookup_keys.flush(&transaction)?;
    materialize_lookup_keys(&transaction)?;
    insert_shape_observations(&transaction, &observations)?;
    insert_pack_metadata(
        &transaction,
        manifest,
        &revision,
        selected_count,
        selected_bytes,
        &selected_record_digest,
        &source_manifest_json,
        &license_manifest_json,
    )?;
    let lookup_row_count =
        transaction.query_row("SELECT count(*) FROM lookup_keys", [], |row| {
            row.get::<_, u64>(0)
        })?;
    transaction.commit()?;
    close_connection(index)?;

    let index_asset = asset_for("index", &index_file_name, &index_path)?;
    ensure_asset_bound(&index_asset, options.max_asset_bytes)?;
    let shard_count = u64::try_from(shard_assets.len()).map_err(|_| BuildError::IntegerRange {
        name: "shard count",
        value: u64::MAX,
    })?;
    let mut assets = Vec::with_capacity(shard_assets.len() + 1);
    assets.push(index_asset);
    assets.extend(shard_assets);
    let pack_manifest = PackManifest {
        manifest_version: 1,
        format_version: FORMAT_VERSION,
        pack_id: pack_id.to_string(),
        pack_revision: Sha256Hex::from_bytes(*revision.as_bytes()),
        selected_record_count: selected_count,
        selected_record_bytes: selected_bytes,
        selected_record_digest: Sha256Hex::from_bytes(selected_record_digest),
        assets,
    };
    write_pack_manifest(staging.path(), &pack_manifest)?;
    let report = BuildReport {
        schema_version: 1,
        pack_id: pack_id.to_string(),
        pack_revision: Sha256Hex::from_bytes(*revision.as_bytes()),
        source: SourceReport {
            record_count: source_record_count,
            size_bytes: source_size_bytes,
            sha256: Sha256Hex::from_bytes(actual_source_sha256),
        },
        selected: SelectedReport {
            record_count: selected_count,
            size_bytes: selected_bytes,
            digest: Sha256Hex::from_bytes(selected_record_digest),
        },
        lookup_row_count,
        index_asset: ReportAsset::from(&pack_manifest.assets[0]),
        data_assets: pack_manifest.assets[1..]
            .iter()
            .map(ReportAsset::from)
            .collect(),
        shard_count,
        compressed_payload_bytes,
        compression_ratio: CompressionRatio {
            uncompressed_bytes: selected_bytes,
            compressed_bytes: compressed_payload_bytes,
        },
        largest_selected_records: largest_records,
        translation_coverage,
        source_shape_observations: observations.into_report(),
    };
    write_build_report(staging.path(), &report)?;
    sync_directory(staging.path())?;
    staging.publish(output_directory)?;

    Ok(BuildResult {
        output_directory: output_directory.to_owned(),
        pack_revision: revision,
        manifest: pack_manifest,
        report,
    })
}

fn validate_manifest(manifest: &BuildManifest, options: BuildOptions) -> Result<(), BuildError> {
    for (name, value) in [
        ("corpus_language", manifest.corpus_language.as_str()),
        ("wiktionary_edition", manifest.wiktionary_edition.as_str()),
        (
            "wiktionary_dump_date",
            manifest.wiktionary_dump_date.as_str(),
        ),
        (
            "kaikki_extraction_date",
            manifest.kaikki_extraction_date.as_str(),
        ),
        ("source_url", manifest.source_url.as_str()),
        (
            "wiktextract_revision",
            manifest.wiktextract_revision.as_str(),
        ),
        (
            "wikitextprocessor_revision",
            manifest.wikitextprocessor_revision.as_str(),
        ),
        ("builder_revision", manifest.builder_revision.as_str()),
        ("routing_policy", manifest.routing_policy.as_str()),
        ("minimum_app_version", manifest.minimum_app_version.as_str()),
    ] {
        if value.is_empty() {
            return Err(BuildError::InvalidManifest(format!(
                "`{name}` must not be empty"
            )));
        }
    }
    if manifest.compression_profile != COMPRESSION_PROFILE {
        return Err(BuildError::InvalidManifest(format!(
            "unsupported compression_profile `{}`; expected `{COMPRESSION_PROFILE}`",
            manifest.compression_profile
        )));
    }
    if manifest.routing_policy != ROUTING_POLICY {
        return Err(BuildError::InvalidManifest(format!(
            "unsupported routing_policy `{}`; expected `{ROUTING_POLICY}`",
            manifest.routing_policy
        )));
    }
    if manifest.target_shard_payload_bytes == 0 {
        return Err(BuildError::InvalidManifest(
            "`target_shard_payload_bytes` must be greater than zero".to_owned(),
        ));
    }
    if options.max_line_bytes == 0
        || options.max_routing_forms == 0
        || options.max_assets < 2
        || options.max_asset_bytes == 0
        || options.max_shape_depth == 0
        || options.max_shape_path_bytes == 0
        || options.max_shape_observations == 0
        || options.max_shape_nodes == 0
        || options.max_largest_records == 0
    {
        return Err(BuildError::InvalidManifest(
            "builder limits must be greater than zero".to_owned(),
        ));
    }
    Ok(())
}

fn canonical_manifest_json(
    name: &'static str,
    value: &serde_json::Value,
    expected: Sha256Hex,
) -> Result<String, BuildError> {
    let json = serde_json::to_string(value).map_err(|error| {
        BuildError::InvalidManifest(format!("cannot serialize `{name}`: {error}"))
    })?;
    let actual: [u8; 32] = Sha256::digest(json.as_bytes()).into();
    if &actual != expected.as_bytes() {
        return Err(BuildError::InvalidManifest(format!(
            "`{name}` SHA-256 mismatch: expected {}, found {}",
            expected.to_hex(),
            hex::encode(actual)
        )));
    }
    Ok(json)
}

fn open_index(path: &Path) -> Result<Connection, BuildError> {
    let connection = Connection::open(path)?;
    initialize_index_database(&connection)?;
    configure_build_connection(&connection)?;
    Ok(connection)
}

fn configure_build_connection(connection: &Connection) -> Result<(), BuildError> {
    connection.execute_batch(
        "PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         PRAGMA temp_store = FILE;
         PRAGMA cache_size = -524288;
         CREATE TEMP TABLE pending_lookup_keys (
             match_class INTEGER NOT NULL,
             key_utf8 BLOB NOT NULL,
             selected_ordinal INTEGER NOT NULL,
             authored_key_ordinal INTEGER NOT NULL
         );",
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_index_record(
    transaction: &Transaction<'_>,
    shard_ordinal: u64,
    selected_ordinal: u64,
    entry_id: &EntryId,
    record: &SourceRecord,
    record_sha256: &[u8; 32],
    language_code: &str,
    lookup_buffer: &mut LookupKeyBuffer,
) -> Result<(), BuildError> {
    transaction.prepare_cached(
        "INSERT INTO entries (entry_id, selected_ordinal, source_line_number, source_byte_offset, \
         shard_ordinal, authored_headword, language_code, record_sha256) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?.execute(params![
            entry_id.as_bytes().as_slice(),
            sql_i64("selected ordinal", selected_ordinal)?,
            sql_i64("source line number", record.line_number)?,
            sql_i64("source byte offset", record.byte_offset)?,
            sql_i64("shard ordinal", shard_ordinal)?,
            &record.word,
            language_code,
            record_sha256.as_slice(),
        ])?;

    insert_lookup_value(
        transaction,
        lookup_buffer,
        selected_ordinal,
        0,
        &record.word,
        1,
    )?;
    let capacity = record.routing_forms.len();
    let mut seen: [HashSet<Vec<u8>>; 3] = std::array::from_fn(|_| HashSet::with_capacity(capacity));
    for (form_index, form) in record.routing_forms.iter().enumerate() {
        let ordinal = u64::try_from(form_index).map_err(|_| BuildError::IntegerRange {
            name: "authored form ordinal",
            value: u64::MAX,
        })?;
        let keys = lookup_keys(form);
        for ((class, key), seen) in [(4_u8, keys.authored), (5, keys.nfc), (6, keys.folded)]
            .into_iter()
            .zip(&mut seen)
        {
            if !seen.contains(key.as_slice()) {
                lookup_buffer.push(transaction, selected_ordinal, ordinal, class, &key)?;
                seen.insert(key);
            }
        }
    }
    Ok(())
}

fn insert_lookup_value(
    transaction: &Transaction<'_>,
    lookup_buffer: &mut LookupKeyBuffer,
    selected_ordinal: u64,
    authored_ordinal: u64,
    value: &str,
    first_class: u8,
) -> Result<(), BuildError> {
    let keys = lookup_keys(value);
    for (class, key) in [first_class, first_class + 1, first_class + 2]
        .into_iter()
        .zip([keys.authored, keys.nfc, keys.folded])
    {
        lookup_buffer.push(transaction, selected_ordinal, authored_ordinal, class, &key)?;
    }
    Ok(())
}

#[derive(Default)]
struct LookupKeyBuffer {
    rows: Vec<PendingLookupKey>,
}

struct PendingLookupKey {
    class: u8,
    key: Vec<u8>,
    selected_ordinal: u64,
    authored_ordinal: u64,
}

impl LookupKeyBuffer {
    fn push(
        &mut self,
        transaction: &Transaction<'_>,
        selected_ordinal: u64,
        authored_ordinal: u64,
        class: u8,
        key: &[u8],
    ) -> Result<(), BuildError> {
        self.rows.push(PendingLookupKey {
            class,
            key: key.to_vec(),
            selected_ordinal,
            authored_ordinal,
        });
        if self.rows.len() == LOOKUP_KEY_BATCH_ROWS {
            self.flush(transaction)?;
        }
        Ok(())
    }

    fn flush(&mut self, transaction: &Transaction<'_>) -> Result<(), BuildError> {
        let mut statement = transaction.prepare_cached(
            "INSERT INTO pending_lookup_keys (match_class, key_utf8, selected_ordinal, \
         authored_key_ordinal) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for row in self.rows.drain(..) {
            statement.execute(params![
                row.class,
                row.key,
                sql_i64("selected ordinal", row.selected_ordinal)?,
                sql_i64("authored key ordinal", row.authored_ordinal)?,
            ])?;
        }
        Ok(())
    }
}

fn materialize_lookup_keys(transaction: &Transaction<'_>) -> Result<(), BuildError> {
    transaction.execute_batch(
        "INSERT INTO lookup_keys (
             match_class, key_utf8, selected_ordinal, authored_key_ordinal
         )
         SELECT match_class, key_utf8, selected_ordinal, authored_key_ordinal
         FROM pending_lookup_keys
         ORDER BY match_class, key_utf8, selected_ordinal, authored_key_ordinal;
         DROP TABLE pending_lookup_keys;",
    )?;
    Ok(())
}

#[derive(Default)]
struct ShapeObservations {
    rows: BTreeMap<String, BTreeMap<&'static str, (u64, u64)>>,
    distinct_count: usize,
    visited_nodes: u64,
}

impl ShapeObservations {
    fn observe(
        &mut self,
        path: &str,
        shape: &'static str,
        source_line: u64,
        options: BuildOptions,
    ) -> Result<(), BuildError> {
        if path.len() > options.max_shape_path_bytes {
            return Err(BuildError::BuildLimit(format!(
                "normalized JSON path on source line {source_line} is {} bytes; maximum is {}",
                path.len(),
                options.max_shape_path_bytes
            )));
        }
        self.visited_nodes = self.visited_nodes.checked_add(1).ok_or_else(|| {
            BuildError::BuildLimit("source-shape traversal count overflowed".to_owned())
        })?;
        if self.visited_nodes > options.max_shape_nodes {
            return Err(BuildError::BuildLimit(format!(
                "source-shape traversal exceeds the {}-node limit",
                options.max_shape_nodes
            )));
        }
        if let Some((count, _)) = self
            .rows
            .get_mut(path)
            .and_then(|shapes| shapes.get_mut(shape))
        {
            *count = count.checked_add(1).ok_or_else(|| {
                BuildError::BuildLimit("source-shape occurrence count overflowed".to_owned())
            })?;
        } else {
            if self.distinct_count == options.max_shape_observations {
                return Err(BuildError::BuildLimit(format!(
                    "source shape exceeds the {}-distinct-observation limit",
                    options.max_shape_observations
                )));
            }
            self.rows
                .entry(path.to_owned())
                .or_default()
                .insert(shape, (1, source_line));
            self.distinct_count += 1;
        }
        Ok(())
    }

    fn into_report(self) -> Vec<SourceShapeObservation> {
        self.rows
            .into_iter()
            .flat_map(|(json_path, shapes)| {
                shapes.into_iter().map(
                    move |(value_shape, (occurrence_count, first_source_line))| {
                        SourceShapeObservation {
                            json_path: json_path.clone(),
                            value_shape: value_shape.to_owned(),
                            occurrence_count,
                            first_source_line,
                        }
                    },
                )
            })
            .collect()
    }
}

fn observe_record(
    record: &SourceRecord,
    observations: &mut ShapeObservations,
    translation_coverage: &mut TranslationCoverage,
    options: BuildOptions,
) -> Result<(), BuildError> {
    let mut path = "$".to_owned();
    observations.observe(&path, "object", record.line_number, options)?;
    observe_object_children(
        &record.object,
        &mut path,
        0,
        record.line_number,
        observations,
        options,
    )?;
    observe_translations(&record.object, translation_coverage)
}

fn observe_value(
    value: &serde_json::Value,
    path: &mut String,
    depth: usize,
    source_line: u64,
    observations: &mut ShapeObservations,
    options: BuildOptions,
) -> Result<(), BuildError> {
    if depth > options.max_shape_depth {
        return Err(BuildError::BuildLimit(format!(
            "source JSON on line {source_line} exceeds the {}-level shape depth limit",
            options.max_shape_depth
        )));
    }
    observations.observe(path, value_shape(value), source_line, options)?;
    match value {
        serde_json::Value::Array(values) => {
            let parent_length = path.len();
            path.push_str("[]");
            for child in values {
                observe_value(child, path, depth + 1, source_line, observations, options)?;
            }
            path.truncate(parent_length);
        }
        serde_json::Value::Object(fields) => {
            observe_object_children(fields, path, depth, source_line, observations, options)?;
        }
        _ => {}
    }
    Ok(())
}

fn observe_object_children(
    fields: &serde_json::Map<String, serde_json::Value>,
    path: &mut String,
    depth: usize,
    source_line: u64,
    observations: &mut ShapeObservations,
    options: BuildOptions,
) -> Result<(), BuildError> {
    for (field, child) in fields {
        let quoted =
            serde_json::to_string(field).map_err(|source| BuildError::ObservationJson {
                line_number: source_line,
                source,
            })?;
        let parent_length = path.len();
        path.push('[');
        path.push_str(&quoted);
        path.push(']');
        observe_value(child, path, depth + 1, source_line, observations, options)?;
        path.truncate(parent_length);
    }
    Ok(())
}

fn value_shape(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(number) if number.is_i64() || number.is_u64() => "number:integer",
        serde_json::Value::Number(_) => "number:non-integer",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn observe_translations(
    object: &serde_json::Map<String, serde_json::Value>,
    coverage: &mut TranslationCoverage,
) -> Result<(), BuildError> {
    if let Some(translations) = object
        .get("translations")
        .and_then(serde_json::Value::as_array)
    {
        checked_increment(
            &mut coverage.selected_records_with_top_level_translation_array,
            1,
            "records with top-level translation arrays",
        )?;
        checked_increment(
            &mut coverage.top_level_translation_items,
            u64::try_from(translations.len()).map_err(|_| BuildError::IntegerRange {
                name: "top-level translation items",
                value: u64::MAX,
            })?,
            "top-level translation items",
        )?;
    }
    let Some(senses) = object.get("senses").and_then(serde_json::Value::as_array) else {
        return Ok(());
    };
    for sense in senses.iter().filter_map(serde_json::Value::as_object) {
        checked_increment(&mut coverage.sense_objects, 1, "sense objects")?;
        if let Some(translations) = sense
            .get("translations")
            .and_then(serde_json::Value::as_array)
        {
            checked_increment(
                &mut coverage.sense_objects_with_translation_array,
                1,
                "sense objects with translation arrays",
            )?;
            checked_increment(
                &mut coverage.sense_translation_items,
                u64::try_from(translations.len()).map_err(|_| BuildError::IntegerRange {
                    name: "sense translation items",
                    value: u64::MAX,
                })?,
                "sense translation items",
            )?;
        }
    }
    Ok(())
}

fn checked_increment(value: &mut u64, amount: u64, name: &'static str) -> Result<(), BuildError> {
    *value = value.checked_add(amount).ok_or(BuildError::IntegerRange {
        name,
        value: u64::MAX,
    })?;
    Ok(())
}

fn retain_largest_record(
    records: &mut Vec<LargestSelectedRecord>,
    record: LargestSelectedRecord,
    maximum: usize,
) {
    records.push(record);
    records.sort_by(|left, right| {
        right
            .size_bytes
            .cmp(&left.size_bytes)
            .then_with(|| left.selected_ordinal.cmp(&right.selected_ordinal))
            .then_with(|| left.sha256.cmp(&right.sha256))
    });
    records.truncate(maximum);
}

fn insert_shape_observations(
    transaction: &Transaction<'_>,
    observations: &ShapeObservations,
) -> Result<(), BuildError> {
    let mut statement = transaction.prepare(
        "INSERT INTO source_shape_observations \
         (json_path, value_shape, occurrence_count, first_source_line) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (path, shapes) in &observations.rows {
        for (shape, (count, first_line)) in shapes {
            statement.execute(params![
                path,
                shape,
                sql_i64("shape occurrence count", *count)?,
                sql_i64("shape first source line", *first_line)?,
            ])?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_pack_metadata(
    transaction: &Transaction<'_>,
    manifest: &BuildManifest,
    revision: &PackRevision,
    selected_count: u64,
    selected_bytes: u64,
    selected_digest: &[u8; 32],
    source_manifest_json: &str,
    license_manifest_json: &str,
) -> Result<(), BuildError> {
    transaction.execute(
        "INSERT INTO pack_metadata (singleton, format_version, pack_id, pack_revision, \
         corpus_language, wiktionary_edition, wiktionary_dump_date, kaikki_extraction_date, \
         source_url, compressed_source_sha256, uncompressed_source_sha256, wiktextract_revision, \
         wikitextprocessor_revision, builder_revision, unicode_profile, compression_profile, \
         routing_policy, target_shard_payload_bytes, selected_record_count, selected_record_bytes, \
         selected_record_digest, source_manifest_sha256, source_manifest_json, \
         license_manifest_sha256, license_manifest_json, compatible_audio_collection, \
         minimum_app_version) \
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
         ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
        params![
            FORMAT_VERSION,
            &manifest.pack_id,
            revision.as_bytes().as_slice(),
            &manifest.corpus_language,
            &manifest.wiktionary_edition,
            &manifest.wiktionary_dump_date,
            &manifest.kaikki_extraction_date,
            &manifest.source_url,
            manifest.compressed_source_sha256.as_bytes().as_slice(),
            manifest.uncompressed_source_sha256.as_bytes().as_slice(),
            &manifest.wiktextract_revision,
            &manifest.wikitextprocessor_revision,
            &manifest.builder_revision,
            UNICODE_PROFILE,
            &manifest.compression_profile,
            &manifest.routing_policy,
            sql_i64(
                "target shard payload bytes",
                manifest.target_shard_payload_bytes
            )?,
            sql_i64("selected record count", selected_count)?,
            sql_i64("selected record bytes", selected_bytes)?,
            selected_digest.as_slice(),
            manifest.source_manifest_sha256.as_bytes().as_slice(),
            source_manifest_json,
            manifest.license_manifest_sha256.as_bytes().as_slice(),
            license_manifest_json,
            manifest.compatible_audio_collection.as_deref(),
            &manifest.minimum_app_version,
        ],
    )?;
    Ok(())
}

struct ShardWriter {
    connection: Connection,
    path: PathBuf,
    file_name: String,
    ordinal: u64,
    first_selected_ordinal: u64,
    last_selected_ordinal: u64,
    record_count: u64,
    compressed_payload_bytes: u64,
    uncompressed_record_bytes: u64,
    record_digest: FramedDigest,
    pack_id: String,
    pack_revision: PackRevision,
    corpus_language: String,
}

impl ShardWriter {
    fn create(
        directory: &Path,
        pack_id: &str,
        revision: &PackRevision,
        ordinal: u64,
        first_selected_ordinal: u64,
        corpus_language: &str,
    ) -> Result<Self, BuildError> {
        let file_name = format!("{pack_id}-{}-data-{ordinal:03}.eldict", revision.to_hex());
        let path = directory.join(&file_name);
        let connection = Connection::open(&path)?;
        initialize_data_database(&connection)?;
        configure_build_connection(&connection)?;
        connection.execute_batch("BEGIN IMMEDIATE")?;
        Ok(Self {
            connection,
            path,
            file_name,
            ordinal,
            first_selected_ordinal,
            last_selected_ordinal: first_selected_ordinal,
            record_count: 0,
            compressed_payload_bytes: 0,
            uncompressed_record_bytes: 0,
            record_digest: FramedDigest::new(SHARD_RECORD_DIGEST_DOMAIN),
            pack_id: pack_id.to_owned(),
            pack_revision: *revision,
            corpus_language: corpus_language.to_owned(),
        })
    }

    fn insert(
        &mut self,
        selected_ordinal: u64,
        entry_id: &EntryId,
        record: &SourceRecord,
        record_sha256: &[u8; 32],
        compressed: &[u8],
    ) -> Result<(), BuildError> {
        let uncompressed_size =
            u64::try_from(record.source_bytes.len()).map_err(|_| BuildError::IntegerRange {
                name: "uncompressed record byte length",
                value: u64::MAX,
            })?;
        let compressed_size =
            u64::try_from(compressed.len()).map_err(|_| BuildError::IntegerRange {
                name: "compressed record byte length",
                value: u64::MAX,
            })?;
        self.connection
            .prepare_cached(
                "INSERT INTO records (selected_ordinal, entry_id, source_line_number, \
             source_byte_offset, authored_headword, language_code, uncompressed_size_bytes, \
             compressed_size_bytes, record_sha256, source_json_zstd) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?
            .execute(params![
                sql_i64("selected ordinal", selected_ordinal)?,
                entry_id.as_bytes().as_slice(),
                sql_i64("source line number", record.line_number)?,
                sql_i64("source byte offset", record.byte_offset)?,
                &record.word,
                &self.corpus_language,
                sql_i64("uncompressed record byte length", uncompressed_size)?,
                sql_i64("compressed record byte length", compressed_size)?,
                record_sha256.as_slice(),
                compressed,
            ])?;
        frame_record(&mut self.record_digest, selected_ordinal, record);
        self.record_count = self
            .record_count
            .checked_add(1)
            .ok_or(BuildError::IntegerRange {
                name: "shard record count",
                value: u64::MAX,
            })?;
        self.last_selected_ordinal = selected_ordinal;
        self.compressed_payload_bytes = self
            .compressed_payload_bytes
            .checked_add(compressed_size)
            .ok_or(BuildError::IntegerRange {
                name: "compressed shard payload bytes",
                value: u64::MAX,
            })?;
        self.uncompressed_record_bytes = self
            .uncompressed_record_bytes
            .checked_add(uncompressed_size)
            .ok_or(BuildError::IntegerRange {
                name: "uncompressed shard record bytes",
                value: u64::MAX,
            })?;
        Ok(())
    }
}

struct CompletedShard {
    asset: PackAsset,
    compressed_payload_bytes: u64,
}

fn finish_shard(
    shard: ShardWriter,
    index: &Transaction<'_>,
    max_asset_bytes: u64,
) -> Result<CompletedShard, BuildError> {
    let compressed_payload_bytes = shard.compressed_payload_bytes;
    let digest = shard.record_digest.finish();
    shard.connection.execute(
        "INSERT INTO shard_metadata (singleton, format_version, pack_id, pack_revision, \
         shard_ordinal, corpus_language, record_count, first_selected_ordinal, \
         last_selected_ordinal, shard_record_digest) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            FORMAT_VERSION,
            &shard.pack_id,
            shard.pack_revision.as_bytes().as_slice(),
            sql_i64("shard ordinal", shard.ordinal)?,
            &shard.corpus_language,
            sql_i64("shard record count", shard.record_count)?,
            sql_i64("first selected ordinal", shard.first_selected_ordinal)?,
            sql_i64("last selected ordinal", shard.last_selected_ordinal)?,
            digest.as_slice(),
        ],
    )?;
    shard.connection.execute_batch("COMMIT")?;
    close_connection(shard.connection)?;
    let asset = asset_for("data", &shard.file_name, &shard.path)?;
    ensure_asset_bound(&asset, max_asset_bytes)?;
    index
        .prepare_cached(
            "INSERT INTO data_shards (shard_ordinal, file_name, file_sha256, file_size_bytes, \
             record_count, first_selected_ordinal, last_selected_ordinal, compressed_payload_bytes, \
             uncompressed_record_bytes, shard_record_digest) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?
        .execute(params![
            sql_i64("shard ordinal", shard.ordinal)?,
            &shard.file_name,
            asset.sha256.as_bytes().as_slice(),
            sql_i64("shard file size", asset.size_bytes)?,
            sql_i64("shard record count", shard.record_count)?,
            sql_i64("first selected ordinal", shard.first_selected_ordinal)?,
            sql_i64("last selected ordinal", shard.last_selected_ordinal)?,
            sql_i64(
                "compressed shard payload bytes",
                shard.compressed_payload_bytes
            )?,
            sql_i64(
                "uncompressed shard record bytes",
                shard.uncompressed_record_bytes
            )?,
            digest.as_slice(),
        ])?;
    Ok(CompletedShard {
        asset,
        compressed_payload_bytes,
    })
}

fn frame_record(digest: &mut FramedDigest, selected_ordinal: u64, record: &SourceRecord) {
    digest.field(&selected_ordinal.to_be_bytes());
    digest.field(&record.line_number.to_be_bytes());
    digest.field(&record.byte_offset.to_be_bytes());
    digest.field(&record.source_bytes);
}

struct FramedDigest(Sha256);

impl FramedDigest {
    fn new(domain: &[u8]) -> Self {
        let mut digest = Self(Sha256::new());
        digest.field(domain);
        digest
    }

    fn field(&mut self, bytes: &[u8]) {
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

struct DigestReader<R> {
    inner: R,
    digest: Sha256,
    bytes_read: u64,
}

impl<R> DigestReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
            bytes_read: 0,
        }
    }

    const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    fn finish(self) -> [u8; 32] {
        self.digest.finalize().into()
    }
}

impl<R: Read> Read for DigestReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.digest.update(&buffer[..read]);
        self.bytes_read = self
            .bytes_read
            .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        Ok(read)
    }
}

fn asset_for(role: &str, file_name: &str, path: &Path) -> Result<PackAsset, BuildError> {
    let size_bytes = fs::metadata(path)
        .map_err(|source| io_error("read metadata", path, source))?
        .len();
    let mut file = File::open(path).map_err(|source| io_error("open for hashing", path, source))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| io_error("hash asset", path, source))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    file.sync_all()
        .map_err(|source| io_error("sync asset", path, source))?;
    Ok(PackAsset {
        role: role.to_owned(),
        file_name: file_name.to_owned(),
        sha256: Sha256Hex::from_bytes(digest.finalize().into()),
        size_bytes,
    })
}

fn ensure_asset_bound(asset: &PackAsset, maximum: u64) -> Result<(), BuildError> {
    if asset.size_bytes > maximum {
        Err(BuildError::BuildLimit(format!(
            "asset `{}` is {} bytes; maximum is {maximum}",
            asset.file_name, asset.size_bytes
        )))
    } else {
        Ok(())
    }
}

fn sync_directory(path: &Path) -> Result<(), BuildError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("sync directory", path, source))
}

fn write_pack_manifest(directory: &Path, manifest: &PackManifest) -> Result<(), BuildError> {
    let path = directory.join(PACK_MANIFEST_FILE);
    let mut bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| BuildError::InvalidManifest(error.to_string()))?;
    bytes.push(b'\n');
    let mut file = File::create(&path).map_err(|source| io_error("create", &path, source))?;
    file.write_all(&bytes)
        .map_err(|source| io_error("write", &path, source))?;
    file.sync_all()
        .map_err(|source| io_error("sync", &path, source))
}

fn write_build_report(directory: &Path, report: &BuildReport) -> Result<(), BuildError> {
    let path = directory.join(BUILD_REPORT_FILE);
    let mut bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| BuildError::InvalidManifest(error.to_string()))?;
    bytes.push(b'\n');
    let mut file = File::create(&path).map_err(|source| io_error("create", &path, source))?;
    file.write_all(&bytes)
        .map_err(|source| io_error("write", &path, source))?;
    file.sync_all()
        .map_err(|source| io_error("sync", &path, source))
}

fn close_connection(connection: Connection) -> Result<(), BuildError> {
    connection
        .close()
        .map_err(|(_, error)| BuildError::Database(error))
}

fn sql_i64(name: &'static str, value: u64) -> Result<i64, BuildError> {
    i64::try_from(value).map_err(|_| BuildError::IntegerRange { name, value })
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> BuildError {
    BuildError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

struct StagingDirectory {
    path: PathBuf,
    published: bool,
}

impl StagingDirectory {
    fn create(output: &Path) -> Result<Self, BuildError> {
        if output.exists() {
            return Err(BuildError::OutputExists(output.to_owned()));
        }
        let parent = output.parent().unwrap_or_else(|| Path::new("."));
        let name = output.file_name().ok_or_else(|| {
            BuildError::InvalidManifest("output path must name a directory".to_owned())
        })?;
        for attempt in 0..100_u32 {
            let path = parent.join(format!(
                ".{}.staging-{}-{attempt}",
                name.to_string_lossy(),
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        published: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(io_error("create staging directory", &path, source)),
            }
        }
        Err(BuildError::InvalidManifest(
            "could not allocate a staging directory".to_owned(),
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn publish(&mut self, output: &Path) -> Result<(), BuildError> {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &self.path,
            rustix::fs::CWD,
            output,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|error| {
            let source = io::Error::from_raw_os_error(error.raw_os_error());
            if source.kind() == io::ErrorKind::AlreadyExists {
                BuildError::OutputExists(output.to_owned())
            } else {
                io_error("publish staging directory", output, source)
            }
        })?;
        self.published = true;
        sync_directory(output.parent().unwrap_or_else(|| Path::new(".")))?;
        Ok(())
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
