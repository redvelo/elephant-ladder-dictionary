use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use elephant_ladder_dictionary_pack::{
    COMPRESSION_PROFILE, CorpusIdentity, DATA_SCHEMA, EntryId, FORMAT_VERSION, FramedDigest,
    INDEX_SCHEMA, MAX_ASSETS, PACK_MANIFEST_FILE, PackAsset, PackError, PackManifest, PackRevision,
    ROUTING_POLICY, SELECTED_STREAM_DIGEST_DOMAIN, SHARD_RECORD_DIGEST_DOMAIN, Sha256Hex,
    UNICODE_PROFILE, lookup_keys,
};
use rusqlite::{Connection, Transaction, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::source::{JsonlSourceReader, SourceIngestionError, SourceRecord};

const ZSTD_LEVEL: i32 = 6;
const LOOKUP_KEY_BATCH_ROWS: usize = 262_144;
pub(crate) const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;
/// Below GitHub's 2 GiB release asset limit.
pub(crate) const MAX_ASSET_BYTES: u64 = 1_900_000_000;

/// Facts returned after a pack directory has been published.
#[derive(Clone, Debug)]
pub struct BuildResult {
    pub output_directory: PathBuf,
    pub pack_revision: PackRevision,
    pub manifest: PackManifest,
    /// Physical JSONL records read, selected or not.
    pub source_record_count: u64,
    pub source_bytes: u64,
    pub compressed_payload_bytes: u64,
    pub lookup_row_count: u64,
}

/// A deterministic pack construction failure.
#[derive(Debug, Error)]
pub enum BuildError {
    #[error("invalid build input: {0}")]
    Invalid(String),
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
    #[error("no records matched language code `{0}`")]
    NoSelectedRecords(String),
    #[error("uncompressed source SHA-256 mismatch: expected {expected}, found {actual}")]
    SourceDigestMismatch { expected: String, actual: String },
    #[error("output directory already exists: {0}")]
    OutputExists(PathBuf),
    #[error("pack build limit exceeded: {0}")]
    BuildLimit(String),
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
/// Returns a precise input, source, storage, compression, or publication error.
#[allow(clippy::too_many_lines)]
pub fn build_pack<R: Read>(
    identity: &CorpusIdentity,
    source: R,
    output_directory: impl AsRef<Path>,
) -> Result<BuildResult, BuildError> {
    validate_identity(identity)?;
    let pack_id = &identity.pack_id;
    let revision = identity.revision();
    let output_directory = output_directory.as_ref();
    let mut staging = StagingDirectory::create(output_directory)?;

    let index_file_name = format!("{pack_id}-{revision}-index.eldict");
    let index_path = staging.path().join(&index_file_name);
    let mut index = Connection::open(elephant_ladder_dictionary_pack::sqlite_path(&index_path))?;
    index.execute_batch(INDEX_SCHEMA)?;
    configure_build_connection(&index)?;
    let transaction = index.transaction()?;
    transaction.execute_batch("PRAGMA defer_foreign_keys = ON;")?;

    let mut source_digest_reader = DigestReader::new(source);
    let mut selected_digest = FramedDigest::new(SELECTED_STREAM_DIGEST_DOMAIN);
    let mut selected_count = 0_u64;
    let mut selected_bytes = 0_u64;
    let mut shard: Option<ShardWriter> = None;
    let mut shard_assets = Vec::new();
    let mut compressed_payload_bytes = 0_u64;
    let mut compressor =
        zstd::bulk::Compressor::new(ZSTD_LEVEL).map_err(|source| BuildError::Compression {
            selected_ordinal: 0,
            source,
        })?;
    let mut lookup_keys = LookupKeyBuffer::default();

    let source_record_count = {
        let mut records = JsonlSourceReader::new(
            &mut source_digest_reader,
            &identity.corpus_language,
            MAX_LINE_BYTES,
        );
        for record in records.by_ref() {
            let record = record?;
            let selected_ordinal = selected_count;
            selected_bytes += record.source_bytes.len() as u64;
            frame_record(&mut selected_digest, selected_ordinal, &record);

            let record_sha256: [u8; 32] = Sha256::digest(&record.source_bytes).into();
            let entry_id = EntryId::derive(&revision, record.line_number, &record_sha256);
            let compressed = compressor
                .compress(&record.source_bytes)
                .map_err(|source| BuildError::Compression {
                    selected_ordinal,
                    source,
                })?;
            let compressed_size = compressed.len() as u64;

            if shard.as_ref().is_some_and(|current| {
                current.record_count > 0
                    && current.compressed_payload_bytes + compressed_size
                        > identity.target_shard_payload_bytes
            }) && let Some(current) = shard.take()
            {
                let completed = finish_shard(current, &transaction)?;
                compressed_payload_bytes += completed.compressed_payload_bytes;
                shard_assets.push(completed.asset);
            }
            let current = if let Some(current) = shard.as_mut() {
                current
            } else {
                // One manifest slot is reserved for the index.
                if shard_assets.len() + 2 > MAX_ASSETS {
                    return Err(BuildError::BuildLimit(format!(
                        "pack would exceed the {MAX_ASSETS}-asset limit"
                    )));
                }
                shard.insert(ShardWriter::create(
                    staging.path(),
                    identity,
                    &revision,
                    shard_assets.len() as u64,
                    selected_ordinal,
                )?)
            };
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
                &identity.corpus_language,
                &mut lookup_keys,
            )?;
            selected_count += 1;
        }
        records.source_record_count()
    };

    let source_bytes = source_digest_reader.bytes_read;
    let actual_source_sha256: [u8; 32] = source_digest_reader.digest.finalize().into();
    if &actual_source_sha256 != identity.uncompressed_source_sha256.as_bytes() {
        return Err(BuildError::SourceDigestMismatch {
            expected: identity.uncompressed_source_sha256.to_hex(),
            actual: hex::encode(actual_source_sha256),
        });
    }
    let Some(shard) = shard else {
        return Err(BuildError::NoSelectedRecords(
            identity.corpus_language.clone(),
        ));
    };
    let completed = finish_shard(shard, &transaction)?;
    compressed_payload_bytes += completed.compressed_payload_bytes;
    shard_assets.push(completed.asset);

    let selected_record_digest = Sha256Hex::from_bytes(selected_digest.finish());
    lookup_keys.flush(&transaction)?;
    transaction.execute_batch(
        "INSERT INTO lookup_keys (
             match_class, key_utf8, selected_ordinal, authored_key_ordinal
         )
         SELECT match_class, key_utf8, selected_ordinal, authored_key_ordinal
         FROM pending_lookup_keys
         ORDER BY match_class, key_utf8, selected_ordinal, authored_key_ordinal;
         DROP TABLE pending_lookup_keys;",
    )?;
    insert_pack_metadata(
        &transaction,
        identity,
        &revision,
        selected_count,
        selected_bytes,
        &selected_record_digest,
    )?;
    let lookup_row_count =
        transaction.query_row("SELECT count(*) FROM lookup_keys", [], |row| {
            row.get::<_, u64>(0)
        })?;
    transaction.commit()?;
    close_connection(index)?;

    let mut assets = vec![asset_for("index", &index_file_name, &index_path)?];
    assets.extend(shard_assets);
    let manifest = PackManifest {
        manifest_version: 1,
        format_version: FORMAT_VERSION,
        pack_id: pack_id.clone(),
        pack_revision: revision,
        selected_record_count: selected_count,
        selected_record_bytes: selected_bytes,
        selected_record_digest,
        assets,
    };
    write_json(&staging.path().join(PACK_MANIFEST_FILE), &manifest)?;
    sync_directory(staging.path())?;
    staging.publish(output_directory)?;

    Ok(BuildResult {
        output_directory: output_directory.to_owned(),
        pack_revision: revision,
        manifest,
        source_record_count,
        source_bytes,
        compressed_payload_bytes,
        lookup_row_count,
    })
}

fn validate_identity(identity: &CorpusIdentity) -> Result<(), BuildError> {
    for (name, value) in [
        ("corpus_language", identity.corpus_language.as_str()),
        ("wiktionary_edition", identity.wiktionary_edition.as_str()),
        (
            "wiktionary_dump_date",
            identity.wiktionary_dump_date.as_str(),
        ),
        (
            "kaikki_extraction_date",
            identity.kaikki_extraction_date.as_str(),
        ),
        ("source_url", identity.source_url.as_str()),
        (
            "wiktextract_revision",
            identity.wiktextract_revision.as_str(),
        ),
        (
            "wikitextprocessor_revision",
            identity.wikitextprocessor_revision.as_str(),
        ),
        ("builder_revision", identity.builder_revision.as_str()),
        ("minimum_app_version", identity.minimum_app_version.as_str()),
    ] {
        if value.is_empty() {
            return Err(BuildError::Invalid(format!("`{name}` must not be empty")));
        }
    }
    for (name, json) in [
        ("source_manifest", &identity.source_manifest),
        ("license_manifest", &identity.license_manifest),
    ] {
        if let Err(error) = serde_json::from_str::<serde_json::Value>(json) {
            return Err(BuildError::Invalid(format!(
                "`{name}` is not JSON: {error}"
            )));
        }
    }
    if identity.target_shard_payload_bytes == 0 {
        return Err(BuildError::Invalid(
            "`target_shard_payload_bytes` must be greater than zero".to_owned(),
        ));
    }
    Ok(())
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

    let keys = lookup_keys(&record.word);
    for (class, key) in [(1, keys.authored), (2, keys.nfc), (3, keys.folded)] {
        lookup_buffer.push(transaction, selected_ordinal, 0, class, key)?;
    }
    let mut seen: [HashSet<Vec<u8>>; 3] = std::array::from_fn(|_| HashSet::new());
    for (form_index, form) in record.routing_forms.iter().enumerate() {
        let keys = lookup_keys(form);
        for ((class, key), seen) in [(4_u8, keys.authored), (5, keys.nfc), (6, keys.folded)]
            .into_iter()
            .zip(&mut seen)
        {
            if seen.insert(key.clone()) {
                lookup_buffer.push(transaction, selected_ordinal, form_index as u64, class, key)?;
            }
        }
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
        key: Vec<u8>,
    ) -> Result<(), BuildError> {
        self.rows.push(PendingLookupKey {
            class,
            key,
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

fn insert_pack_metadata(
    transaction: &Transaction<'_>,
    identity: &CorpusIdentity,
    revision: &PackRevision,
    selected_count: u64,
    selected_bytes: u64,
    selected_digest: &Sha256Hex,
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
         ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, NULL, ?25)",
        params![
            FORMAT_VERSION,
            identity.pack_id.as_str(),
            revision.as_bytes().as_slice(),
            &identity.corpus_language,
            &identity.wiktionary_edition,
            &identity.wiktionary_dump_date,
            &identity.kaikki_extraction_date,
            &identity.source_url,
            identity.compressed_source_sha256.as_bytes().as_slice(),
            identity.uncompressed_source_sha256.as_bytes().as_slice(),
            &identity.wiktextract_revision,
            &identity.wikitextprocessor_revision,
            &identity.builder_revision,
            UNICODE_PROFILE,
            COMPRESSION_PROFILE,
            ROUTING_POLICY,
            sql_i64(
                "target shard payload bytes",
                identity.target_shard_payload_bytes
            )?,
            sql_i64("selected record count", selected_count)?,
            sql_i64("selected record bytes", selected_bytes)?,
            selected_digest.as_bytes().as_slice(),
            identity.source_manifest_sha256().as_bytes().as_slice(),
            &identity.source_manifest,
            identity.license_manifest_sha256().as_bytes().as_slice(),
            &identity.license_manifest,
            &identity.minimum_app_version,
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
        identity: &CorpusIdentity,
        revision: &PackRevision,
        ordinal: u64,
        first_selected_ordinal: u64,
    ) -> Result<Self, BuildError> {
        let file_name = format!("{}-{revision}-data-{ordinal:03}.eldict", identity.pack_id);
        let path = directory.join(&file_name);
        let connection = Connection::open(elephant_ladder_dictionary_pack::sqlite_path(&path))?;
        connection.execute_batch(DATA_SCHEMA)?;
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
            pack_id: identity.pack_id.to_string(),
            pack_revision: *revision,
            corpus_language: identity.corpus_language.clone(),
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
                record.source_bytes.len(),
                compressed.len(),
                record_sha256.as_slice(),
                compressed,
            ])?;
        frame_record(&mut self.record_digest, selected_ordinal, record);
        self.record_count += 1;
        self.last_selected_ordinal = selected_ordinal;
        self.compressed_payload_bytes += compressed.len() as u64;
        self.uncompressed_record_bytes += record.source_bytes.len() as u64;
        Ok(())
    }
}

struct CompletedShard {
    asset: PackAsset,
    compressed_payload_bytes: u64,
}

fn finish_shard(shard: ShardWriter, index: &Transaction<'_>) -> Result<CompletedShard, BuildError> {
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
}

impl<R: Read> Read for DigestReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.digest.update(&buffer[..read]);
        self.bytes_read += read as u64;
        Ok(read)
    }
}

/// Hashes a finished asset file and checks it against the release asset limit.
pub(crate) fn asset_for(role: &str, file_name: &str, path: &Path) -> Result<PackAsset, BuildError> {
    let mut file = File::open(path).map_err(|source| io_error("open for hashing", path, source))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size_bytes = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| io_error("hash asset", path, source))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        size_bytes += read as u64;
    }
    // `FlushFileBuffers` needs a writable handle, which this read-only one is not on
    // Windows; asset durability there is covered by the fixture-only builder role.
    if !cfg!(windows) {
        file.sync_all()
            .map_err(|source| io_error("sync asset", path, source))?;
    }
    if size_bytes > MAX_ASSET_BYTES {
        return Err(BuildError::BuildLimit(format!(
            "asset `{file_name}` is {size_bytes} bytes; maximum is {MAX_ASSET_BYTES}"
        )));
    }
    Ok(PackAsset {
        role: role.to_owned(),
        file_name: file_name.to_owned(),
        sha256: Sha256Hex::from_bytes(digest.finalize().into()),
        size_bytes,
    })
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), BuildError> {
    // Windows cannot open a directory as a `File`; directory durability is a Unix
    // build-host concern, and the builder runs on Windows only to build test fixtures.
    if cfg!(windows) {
        return Ok(());
    }
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("sync directory", path, source))
}

/// Writes pretty JSON with a trailing line feed and syncs it.
pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<(), BuildError> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|error| BuildError::Invalid(error.to_string()))?;
    bytes.push(b'\n');
    let mut file = File::create(path).map_err(|source| io_error("create", path, source))?;
    io::Write::write_all(&mut file, &bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error("write", path, source))
}

pub(crate) fn close_connection(connection: Connection) -> Result<(), BuildError> {
    connection
        .close()
        .map_err(|(_, error)| BuildError::Database(error))
}

pub(crate) fn sql_i64(name: &'static str, value: u64) -> Result<i64, BuildError> {
    i64::try_from(value).map_err(|_| {
        BuildError::Invalid(format!("{name} {value} exceeds the SQLite integer range"))
    })
}

pub(crate) fn io_error(operation: &'static str, path: &Path, source: io::Error) -> BuildError {
    BuildError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

/// A sibling directory that becomes the output by one atomic rename, and is removed
/// when the build fails first.
pub(crate) struct StagingDirectory {
    path: PathBuf,
    published: bool,
}

impl StagingDirectory {
    pub(crate) fn create(output: &Path) -> Result<Self, BuildError> {
        if output.exists() {
            return Err(BuildError::OutputExists(output.to_owned()));
        }
        let parent = output.parent().unwrap_or_else(|| Path::new("."));
        let name = output
            .file_name()
            .ok_or_else(|| BuildError::Invalid("output path must name a directory".to_owned()))?;
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
        Err(BuildError::Invalid(
            "could not allocate a staging directory".to_owned(),
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn publish(&mut self, output: &Path) -> Result<(), BuildError> {
        #[cfg(unix)]
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
        // Windows has no no-replace rename; the builder runs there only to build test
        // fixtures, where no publisher races another.
        #[cfg(not(unix))]
        {
            if output.exists() {
                return Err(BuildError::OutputExists(output.to_owned()));
            }
            fs::rename(&self.path, output)
                .map_err(|source| io_error("publish staging directory", output, source))?;
        }
        self.published = true;
        sync_directory(output.parent().unwrap_or_else(|| Path::new(".")))
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
