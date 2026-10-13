mod common;

use std::fs;
use std::io::Cursor;

use elephant_ladder_dictionary_pack::{CorpusIdentity, lookup_keys};
use elephant_ladder_dictionary_pack_builder::{
    BuildError, BuildResult, SourceIngestionError, build_pack,
};
use rusqlite::Connection;

fn database(result: &BuildResult, role: &str) -> Connection {
    let asset = result
        .manifest
        .assets
        .iter()
        .find(|asset| asset.role == role)
        .unwrap();
    Connection::open(result.output_directory.join(&asset.file_name)).unwrap()
}

#[test]
fn reused_compressor_matches_one_shot_output_and_round_trips() {
    let lines: [&[u8]; 3] = [
        br#"{ "lang_code":"en", "word":"cafe\u0301", "unknown" : [1,2] }"#,
        br#"{"lang_code":"en","word":"small"}"#,
        br#"{"lang_code":"en","word":"repeated","value":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
    ];
    let mut source = Vec::new();
    for line in lines {
        source.extend_from_slice(line);
        source.extend_from_slice(b"\r\n");
    }
    let (_temporary, result) = common::build_result(&source, 1_000_000);
    let data = database(&result, "data");
    let mut statement = data
        .prepare("SELECT source_json_zstd FROM records ORDER BY selected_ordinal")
        .unwrap();
    let compressed = statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for (actual, line) in compressed.iter().zip(lines) {
        assert_eq!(actual, &zstd::bulk::compress(line, 6).unwrap());
        assert_eq!(zstd::decode_all(actual.as_slice()).unwrap(), line);
    }
}

#[test]
fn indexes_all_form_classes_and_keeps_first_duplicate_form() {
    let source = concat!(
        "{\"lang_code\":\"en\",\"word\":\"Straße\",\"forms\":[",
        "{\"form\":\"CAFÉ\"},{\"form\":\"CAFÉ\"},{\"form\":\"CAFÉ\"}]}\n"
    );
    let (_temporary, result) = common::build_result(source.as_bytes(), 1_000_000);
    let index = database(&result, "index");

    let classes: Vec<i64> = index
        .prepare("SELECT DISTINCT match_class FROM lookup_keys ORDER BY match_class")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(classes, [1, 2, 3, 4, 5, 6]);

    let folded = lookup_keys("CAFÉ").folded;
    let (count, first_ordinal): (i64, i64) = index
        .query_row(
            "SELECT count(*), min(authored_key_ordinal) FROM lookup_keys \
             WHERE match_class = 6 AND key_utf8 = ?1",
            [folded],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((count, first_ordinal), (1, 0));
}

#[test]
fn duplicate_source_records_remain_distinct_entries() {
    let line = "{\"lang_code\":\"en\",\"word\":\"same\"}";
    let source = format!("{line}\n{line}\n");
    let (_temporary, result) = common::build_result(source.as_bytes(), 1_000_000);
    let index = database(&result, "index");
    let entries: Vec<(Vec<u8>, i64, Vec<u8>)> = index
        .prepare(
            "SELECT entry_id, source_line_number, record_sha256 FROM entries ORDER BY selected_ordinal",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert_ne!(entries[0].0, entries[1].0);
    assert_eq!(entries[0].2, entries[1].2);
    assert_eq!((entries[0].1, entries[1].1), (1, 2));
}

#[test]
fn never_splits_records_and_forces_multiple_shards() {
    let source = concat!(
        "{\"lang_code\":\"en\",\"word\":\"one\",\"x\":\"aaaaaaaaaaaaaaaa\"}\n",
        "{\"lang_code\":\"en\",\"word\":\"two\",\"x\":\"bbbbbbbbbbbbbbbb\"}\n",
        "{\"lang_code\":\"en\",\"word\":\"three\",\"x\":\"cccccccccccccccc\"}\n"
    );
    let (_temporary, result) = common::build_result(source.as_bytes(), 1);
    let data_assets = result
        .manifest
        .assets
        .iter()
        .filter(|asset| asset.role == "data")
        .collect::<Vec<_>>();
    assert_eq!(data_assets.len(), 3);
    let index = database(&result, "index");
    for asset in data_assets {
        let path = result.output_directory.join(&asset.file_name);
        let database = Connection::open(&path).unwrap();
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM records", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let (stored_hash, stored_size): (Vec<u8>, u64) = index
            .query_row(
                "SELECT file_sha256, file_size_bytes FROM data_shards WHERE file_name = ?1",
                [&asset.file_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(stored_hash, common::digest(&bytes).as_bytes());
        assert_eq!(stored_size, bytes.len() as u64);
    }
}

#[test]
fn repeated_builds_have_the_same_logical_identity_and_facts() {
    let source = concat!(
        "{\"lang_code\":\"fr\",\"word\":\"ignore\"}\n",
        "{\"lang_code\":\"en\",\"word\":\"one\"}\n",
        "{\"lang_code\":\"en\",\"word\":\"two\"}\n"
    );
    let (_first_temp, first) = common::build_result(source.as_bytes(), 64);
    let (_second_temp, second) = common::build_result(source.as_bytes(), 64);
    assert_eq!(first.pack_revision, second.pack_revision);
    assert_eq!(first.manifest, second.manifest);
    assert_eq!(first.source_record_count, 3);
    assert_eq!(first.source_bytes, source.len() as u64);
    assert_eq!(first.manifest.selected_record_count, 2);
    assert_eq!(first.manifest.assets.len(), 3);

    let logical_rows = |result: &BuildResult| {
        database(result, "index")
            .prepare("SELECT selected_ordinal, source_line_number, shard_ordinal, authored_headword FROM entries ORDER BY selected_ordinal")
            .unwrap()
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(logical_rows(&first), logical_rows(&second));

    let index = database(&first, "index");
    let lookup_count: u64 = index
        .query_row("SELECT count(*) FROM lookup_keys", [], |row| row.get(0))
        .unwrap();
    let compressed: u64 = index
        .query_row(
            "SELECT sum(compressed_payload_bytes) FROM data_shards",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(first.lookup_row_count, lookup_count);
    assert_eq!(first.compressed_payload_bytes, compressed);
}

#[test]
fn source_failures_leave_no_output_directory() {
    let source = b"{not json}\n";
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("pack");
    let error = build_pack(
        &common::identity("en", source, 1_000),
        Cursor::new(source),
        &output,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        BuildError::Source(SourceIngestionError::MalformedJson { .. })
    ));
    assert!(!output.exists());
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 0);
}

#[test]
fn publishing_never_replaces_an_existing_output() {
    let source = b"{\"lang_code\":\"en\",\"word\":\"one\"}\n";
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("pack");
    fs::create_dir(&output).unwrap();
    fs::write(output.join("sentinel"), b"keep").unwrap();
    let error = build_pack(
        &common::identity("en", source, 1_000),
        Cursor::new(source),
        &output,
    )
    .unwrap_err();
    assert!(matches!(error, BuildError::OutputExists(path) if path == output));
    assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"keep");
}

#[test]
fn identities_must_be_complete() {
    let source = b"{\"lang_code\":\"en\",\"word\":\"one\"}\n";
    let temporary = tempfile::tempdir().unwrap();
    let mutations: [fn(&mut CorpusIdentity); 4] = [
        |identity| identity.builder_revision.clear(),
        |identity| identity.target_shard_payload_bytes = 0,
        |identity| identity.license_manifest = "{".to_owned(),
        |identity| identity.uncompressed_source_sha256 = common::digest(b"other"),
    ];
    for mutate in mutations {
        let mut identity = common::identity("en", source, 1_000);
        mutate(&mut identity);
        let error = build_pack(
            &identity,
            Cursor::new(source),
            temporary.path().join("pack"),
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                BuildError::Invalid(_) | BuildError::SourceDigestMismatch { .. }
            ),
            "{error}"
        );
    }
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 0);
}
