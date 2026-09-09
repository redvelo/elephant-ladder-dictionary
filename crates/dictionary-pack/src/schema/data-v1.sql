PRAGMA application_id = 0x454c4444;
PRAGMA user_version = 1;
PRAGMA page_size = 4096;
PRAGMA encoding = 'UTF-8';

CREATE TABLE shard_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_version INTEGER NOT NULL CHECK (format_version = 1),
    pack_id TEXT NOT NULL CHECK (length(pack_id) BETWEEN 1 AND 64),
    pack_revision BLOB NOT NULL CHECK (length(pack_revision) = 32),
    shard_ordinal INTEGER NOT NULL CHECK (shard_ordinal >= 0),
    corpus_language TEXT NOT NULL,
    record_count INTEGER NOT NULL CHECK (record_count > 0),
    first_selected_ordinal INTEGER NOT NULL CHECK (first_selected_ordinal >= 0),
    last_selected_ordinal INTEGER NOT NULL CHECK (last_selected_ordinal >= first_selected_ordinal),
    shard_record_digest BLOB NOT NULL CHECK (length(shard_record_digest) = 32)
) STRICT;

CREATE TABLE records (
    selected_ordinal INTEGER PRIMARY KEY CHECK (selected_ordinal >= 0),
    entry_id BLOB NOT NULL UNIQUE CHECK (length(entry_id) = 32),
    source_line_number INTEGER NOT NULL CHECK (source_line_number >= 1),
    source_byte_offset INTEGER NOT NULL CHECK (source_byte_offset >= 0),
    authored_headword TEXT NOT NULL CHECK (length(authored_headword) > 0),
    language_code TEXT NOT NULL CHECK (length(language_code) > 0),
    uncompressed_size_bytes INTEGER NOT NULL CHECK (uncompressed_size_bytes > 0),
    compressed_size_bytes INTEGER NOT NULL CHECK (compressed_size_bytes > 0),
    record_sha256 BLOB NOT NULL CHECK (length(record_sha256) = 32),
    source_json_zstd BLOB NOT NULL,
    CHECK (length(source_json_zstd) = compressed_size_bytes)
) STRICT;
