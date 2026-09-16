PRAGMA application_id = 0x454c4149;
PRAGMA user_version = 1;
PRAGMA page_size = 4096;
PRAGMA encoding = 'UTF-8';
PRAGMA foreign_keys = ON;

CREATE TABLE collection_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_version INTEGER NOT NULL CHECK (format_version = 1),
    pack_id TEXT NOT NULL CHECK (length(pack_id) BETWEEN 1 AND 64),
    pack_revision BLOB NOT NULL CHECK (length(pack_revision) = 32),
    corpus_language TEXT NOT NULL CHECK (length(corpus_language) > 0),
    source_corpus_pack_id TEXT NOT NULL CHECK (length(source_corpus_pack_id) > 0),
    source_corpus_revision BLOB NOT NULL CHECK (length(source_corpus_revision) = 32),
    acquired_at TEXT NOT NULL CHECK (length(acquired_at) > 0),
    encoder_profile TEXT NOT NULL,
    builder_revision TEXT NOT NULL CHECK (length(builder_revision) > 0),
    chunk_count INTEGER NOT NULL CHECK (chunk_count > 0),
    recording_count INTEGER NOT NULL CHECK (recording_count >= 0),
    available_count INTEGER NOT NULL CHECK (available_count BETWEEN 0 AND recording_count),
    recording_input_digest BLOB NOT NULL CHECK (length(recording_input_digest) = 32),
    minimum_app_version TEXT NOT NULL CHECK (length(minimum_app_version) > 0)
) STRICT;

CREATE TABLE chunks (
    chunk_ordinal INTEGER PRIMARY KEY CHECK (chunk_ordinal >= 0),
    file_name TEXT NOT NULL UNIQUE,
    file_sha256 BLOB NOT NULL CHECK (length(file_sha256) = 32),
    file_size_bytes INTEGER NOT NULL CHECK (file_size_bytes > 0),
    blob_count INTEGER NOT NULL CHECK (blob_count >= 0),
    blob_bytes INTEGER NOT NULL CHECK (blob_bytes >= 0)
) STRICT;

CREATE TABLE licenses (
    license_ordinal INTEGER PRIMARY KEY CHECK (license_ordinal >= 0),
    short_name TEXT NOT NULL CHECK (length(short_name) > 0),
    identifier TEXT,
    url TEXT,
    UNIQUE (short_name, identifier, url)
) STRICT;

CREATE TABLE recordings (
    file_name TEXT PRIMARY KEY CHECK (length(file_name) > 0),
    status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 4),
    reason TEXT,
    reference_count INTEGER NOT NULL CHECK (reference_count > 0),
    source_title TEXT,
    source_sha1 BLOB CHECK (source_sha1 IS NULL OR length(source_sha1) = 20),
    source_timestamp TEXT,
    source_media_type TEXT,
    source_size_bytes INTEGER CHECK (source_size_bytes IS NULL OR source_size_bytes >= 0),
    description_url TEXT,
    license_ordinal INTEGER REFERENCES licenses (license_ordinal),
    author_text TEXT,
    author_urls_json TEXT CHECK (author_urls_json IS NULL OR json_valid(author_urls_json)),
    attribution_required INTEGER CHECK (attribution_required IS NULL OR attribution_required IN (0, 1)),
    opus_sha256 BLOB CHECK (opus_sha256 IS NULL OR length(opus_sha256) = 32),
    opus_size_bytes INTEGER CHECK (opus_size_bytes IS NULL OR opus_size_bytes > 0),
    duration_ms INTEGER CHECK (duration_ms IS NULL OR duration_ms > 0),
    chunk_ordinal INTEGER REFERENCES chunks (chunk_ordinal),
    CHECK ((status = 1) = (reason IS NULL)),
    CHECK ((status = 1) = (opus_sha256 IS NOT NULL)),
    CHECK ((opus_sha256 IS NULL) = (opus_size_bytes IS NULL)),
    CHECK ((opus_sha256 IS NULL) = (duration_ms IS NULL)),
    CHECK ((opus_sha256 IS NULL) = (chunk_ordinal IS NULL)),
    CHECK (status <> 1 OR (source_sha1 IS NOT NULL AND license_ordinal IS NOT NULL AND description_url IS NOT NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX recordings_blob_route ON recordings (chunk_ordinal, opus_sha256);
