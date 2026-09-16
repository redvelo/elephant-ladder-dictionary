PRAGMA application_id = 0x454c4143;
PRAGMA user_version = 1;
PRAGMA page_size = 4096;
PRAGMA encoding = 'UTF-8';

CREATE TABLE chunk_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_version INTEGER NOT NULL CHECK (format_version = 1),
    pack_id TEXT NOT NULL CHECK (length(pack_id) BETWEEN 1 AND 64),
    chunk_ordinal INTEGER NOT NULL CHECK (chunk_ordinal >= 0),
    blob_count INTEGER NOT NULL CHECK (blob_count >= 0)
) STRICT;

CREATE TABLE blobs (
    sha256 BLOB PRIMARY KEY CHECK (length(sha256) = 32),
    bytes BLOB NOT NULL CHECK (length(bytes) > 0)
) STRICT, WITHOUT ROWID;
