PRAGMA application_id = 0x454c4449;
PRAGMA user_version = 1;
PRAGMA page_size = 4096;
PRAGMA encoding = 'UTF-8';
PRAGMA foreign_keys = ON;

CREATE TABLE pack_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_version INTEGER NOT NULL CHECK (format_version = 1),
    pack_id TEXT NOT NULL CHECK (length(pack_id) BETWEEN 1 AND 64),
    pack_revision BLOB NOT NULL CHECK (length(pack_revision) = 32),
    corpus_language TEXT NOT NULL,
    wiktionary_edition TEXT NOT NULL,
    wiktionary_dump_date TEXT NOT NULL,
    kaikki_extraction_date TEXT NOT NULL,
    source_url TEXT NOT NULL,
    compressed_source_sha256 BLOB NOT NULL CHECK (length(compressed_source_sha256) = 32),
    uncompressed_source_sha256 BLOB NOT NULL CHECK (length(uncompressed_source_sha256) = 32),
    wiktextract_revision TEXT NOT NULL,
    wikitextprocessor_revision TEXT NOT NULL,
    builder_revision TEXT NOT NULL,
    unicode_profile TEXT NOT NULL,
    compression_profile TEXT NOT NULL,
    routing_policy TEXT NOT NULL,
    target_shard_payload_bytes INTEGER NOT NULL CHECK (target_shard_payload_bytes > 0),
    selected_record_count INTEGER NOT NULL CHECK (selected_record_count > 0),
    selected_record_bytes INTEGER NOT NULL CHECK (selected_record_bytes > 0),
    selected_record_digest BLOB NOT NULL CHECK (length(selected_record_digest) = 32),
    source_manifest_sha256 BLOB NOT NULL CHECK (length(source_manifest_sha256) = 32),
    source_manifest_json TEXT NOT NULL CHECK (json_valid(source_manifest_json)),
    license_manifest_sha256 BLOB NOT NULL CHECK (length(license_manifest_sha256) = 32),
    license_manifest_json TEXT NOT NULL CHECK (json_valid(license_manifest_json)),
    compatible_audio_collection TEXT,
    minimum_app_version TEXT NOT NULL
) STRICT;

CREATE TABLE data_shards (
    shard_ordinal INTEGER PRIMARY KEY CHECK (shard_ordinal >= 0),
    file_name TEXT NOT NULL UNIQUE,
    file_sha256 BLOB NOT NULL CHECK (length(file_sha256) = 32),
    file_size_bytes INTEGER NOT NULL CHECK (file_size_bytes > 0),
    record_count INTEGER NOT NULL CHECK (record_count > 0),
    first_selected_ordinal INTEGER NOT NULL CHECK (first_selected_ordinal >= 0),
    last_selected_ordinal INTEGER NOT NULL CHECK (last_selected_ordinal >= first_selected_ordinal),
    compressed_payload_bytes INTEGER NOT NULL CHECK (compressed_payload_bytes > 0),
    uncompressed_record_bytes INTEGER NOT NULL CHECK (uncompressed_record_bytes > 0),
    shard_record_digest BLOB NOT NULL CHECK (length(shard_record_digest) = 32)
) STRICT;

CREATE TABLE entries (
    entry_id BLOB PRIMARY KEY CHECK (length(entry_id) = 32),
    selected_ordinal INTEGER NOT NULL UNIQUE CHECK (selected_ordinal >= 0),
    source_line_number INTEGER NOT NULL CHECK (source_line_number >= 1),
    source_byte_offset INTEGER NOT NULL CHECK (source_byte_offset >= 0),
    shard_ordinal INTEGER NOT NULL REFERENCES data_shards (shard_ordinal),
    authored_headword TEXT NOT NULL CHECK (length(authored_headword) > 0),
    language_code TEXT NOT NULL CHECK (length(language_code) > 0),
    record_sha256 BLOB NOT NULL CHECK (length(record_sha256) = 32)
) STRICT;

CREATE INDEX entries_shard_route ON entries (shard_ordinal, selected_ordinal);

CREATE TABLE lookup_keys (
    match_class INTEGER NOT NULL CHECK (match_class BETWEEN 1 AND 6),
    key_utf8 BLOB NOT NULL CHECK (length(key_utf8) > 0),
    selected_ordinal INTEGER NOT NULL REFERENCES entries (selected_ordinal) CHECK (selected_ordinal >= 0),
    authored_key_ordinal INTEGER NOT NULL CHECK (authored_key_ordinal >= 0),
    PRIMARY KEY (match_class, key_utf8, selected_ordinal, authored_key_ordinal)
) STRICT, WITHOUT ROWID;

CREATE TABLE source_shape_observations (
    json_path TEXT NOT NULL,
    value_shape TEXT NOT NULL,
    occurrence_count INTEGER NOT NULL CHECK (occurrence_count > 0),
    first_source_line INTEGER NOT NULL CHECK (first_source_line >= 1),
    PRIMARY KEY (json_path, value_shape)
) STRICT, WITHOUT ROWID;
