use rusqlite::Connection;

use crate::PackError;

/// The only pack format version supported by this crate.
pub const FORMAT_VERSION: u32 = 1;
/// `SQLite` application ID `ELDI` for a dictionary index database.
pub const INDEX_APPLICATION_ID: u32 = 0x454c_4449;
/// `SQLite` application ID `ELDD` for a dictionary data-shard database.
pub const DATA_APPLICATION_ID: u32 = 0x454c_4444;
/// Canonical schema used to create a format-v1 index database.
pub const INDEX_SCHEMA: &str = include_str!("schema/index-v1.sql");
/// Canonical schema used to create a format-v1 data-shard database.
pub const DATA_SCHEMA: &str = include_str!("schema/data-v1.sql");
/// Compression algorithm and settings required by format-v1 packs.
pub const COMPRESSION_PROFILE: &str = "zstd-v1-level-6";
/// Source fields admitted as lookup aliases by format-v1 packs.
pub const ROUTING_POLICY: &str = "routing-v1";
/// Domain separator for the digest of all selected records.
pub const SELECTED_STREAM_DIGEST_DOMAIN: &[u8] = b"ELDICT-SELECTED-STREAM-V1";
/// Domain separator for the digest of records assigned to one shard.
pub const SHARD_RECORD_DIGEST_DOMAIN: &[u8] = b"ELDICT-SHARD-RECORDS-V1";

/// Initializes a new index database with the exact format-v1 schema.
///
/// The caller must provide an empty database and finalize it before treating it as
/// an immutable pack asset.
///
/// # Errors
///
/// Returns [`PackError::Database`] when `SQLite` cannot apply the schema, including
/// when the connection already contains conflicting objects.
pub fn initialize_index_database(connection: &Connection) -> Result<(), PackError> {
    connection.execute_batch(INDEX_SCHEMA)?;
    Ok(())
}

/// Initializes a new data-shard database with the exact format-v1 schema.
///
/// The caller must provide an empty database and finalize it before treating it as
/// an immutable pack asset.
///
/// # Errors
///
/// Returns [`PackError::Database`] when `SQLite` cannot apply the schema, including
/// when the connection already contains conflicting objects.
pub fn initialize_data_database(connection: &Connection) -> Result<(), PackError> {
    connection.execute_batch(DATA_SCHEMA)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use rusqlite::Connection;

    use super::*;

    fn object_names(connection: &Connection) -> BTreeSet<(String, String)> {
        let mut statement = connection
            .prepare(
                "SELECT type, name FROM sqlite_schema
                 WHERE name NOT LIKE 'sqlite_%'
                 ORDER BY type, name",
            )
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn index_schema_has_exact_identity_and_objects() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_index_database(&connection).unwrap();

        assert_eq!(
            connection
                .query_row("PRAGMA application_id", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            INDEX_APPLICATION_ID
        );
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            FORMAT_VERSION
        );
        assert_eq!(
            object_names(&connection),
            BTreeSet::from([
                ("index".to_owned(), "entries_shard_route".to_owned()),
                ("table".to_owned(), "data_shards".to_owned()),
                ("table".to_owned(), "entries".to_owned()),
                ("table".to_owned(), "lookup_keys".to_owned()),
                ("table".to_owned(), "pack_metadata".to_owned()),
                ("table".to_owned(), "source_shape_observations".to_owned()),
            ])
        );
    }

    #[test]
    fn data_schema_has_exact_identity_and_objects() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_data_database(&connection).unwrap();

        assert_eq!(
            connection
                .query_row("PRAGMA application_id", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            DATA_APPLICATION_ID
        );
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            FORMAT_VERSION
        );
        assert_eq!(
            object_names(&connection),
            BTreeSet::from([
                ("table".to_owned(), "records".to_owned()),
                ("table".to_owned(), "shard_metadata".to_owned()),
            ])
        );
    }

    #[test]
    fn schemas_reject_duplicate_initialization() {
        let index = Connection::open_in_memory().unwrap();
        initialize_index_database(&index).unwrap();
        assert!(initialize_index_database(&index).is_err());

        let data = Connection::open_in_memory().unwrap();
        initialize_data_database(&data).unwrap();
        assert!(initialize_data_database(&data).is_err());
    }
}
