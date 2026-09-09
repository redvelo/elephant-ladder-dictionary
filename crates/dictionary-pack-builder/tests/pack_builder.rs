use std::fs;
use std::io::Cursor;

use elephant_ladder_dictionary_pack::lookup_keys;
use elephant_ladder_dictionary_pack_builder::{
    BUILD_REPORT_FILE, BuildError, BuildManifest, BuildOptions, BuildReport, Sha256Hex,
    SourceIngestionError, build_pack,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn digest(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from_bytes(Sha256::digest(bytes).into())
}

fn file_digest(path: &std::path::Path) -> Sha256Hex {
    digest(&fs::read(path).unwrap())
}

fn manifest(source: &[u8], target_shard_payload_bytes: u64) -> BuildManifest {
    let source_manifest = json!({"provider":"fixture","version":1});
    let license_manifest = json!({"license":"CC BY-SA 4.0"});
    BuildManifest {
        pack_id: "wiktionary-en-en".to_owned(),
        corpus_language: "en".to_owned(),
        wiktionary_edition: "en".to_owned(),
        wiktionary_dump_date: "2026-09-02".to_owned(),
        kaikki_extraction_date: "2026-09-06".to_owned(),
        source_url: "https://example.invalid/source.jsonl.gz".to_owned(),
        compressed_source_sha256: digest(b"pinned compressed source"),
        uncompressed_source_sha256: digest(source),
        wiktextract_revision: "wiktextract-revision".to_owned(),
        wikitextprocessor_revision: "wikitextprocessor-revision".to_owned(),
        builder_revision: "builder-v1".to_owned(),
        compression_profile: "zstd-v1-level-6".to_owned(),
        routing_policy: "routing-v1".to_owned(),
        target_shard_payload_bytes,
        source_manifest_sha256: digest(&serde_json::to_vec(&source_manifest).unwrap()),
        source_manifest,
        license_manifest_sha256: digest(&serde_json::to_vec(&license_manifest).unwrap()),
        license_manifest,
        compatible_audio_collection: None,
        minimum_app_version: "0.1.0".to_owned(),
    }
}

fn build(
    source: &[u8],
    target: u64,
) -> (
    TempDir,
    elephant_ladder_dictionary_pack_builder::BuildResult,
) {
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("pack");
    let result = build_pack(
        &manifest(source, target),
        Cursor::new(source),
        &output,
        BuildOptions {
            max_line_bytes: 4_096,
            max_routing_forms: 32,
            ..BuildOptions::default()
        },
    )
    .unwrap();
    (temporary, result)
}

fn database(
    result: &elephant_ladder_dictionary_pack_builder::BuildResult,
    role: &str,
) -> Connection {
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
    let (_temporary, result) = build(&source, 1_000_000);
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
        "{\"form\":\"CAFÉ\"},{\"form\":\"CAFÉ\"},{\"form\":\"CAFÉ\"}]}\n"
    );
    let (_temporary, result) = build(source.as_bytes(), 1_000_000);
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
    assert_eq!(count, 1);
    assert_eq!(first_ordinal, 0);
}

#[test]
fn duplicate_source_records_remain_distinct_entries() {
    let line = "{\"lang_code\":\"en\",\"word\":\"same\"}";
    let source = format!("{line}\n{line}\n");
    let (_temporary, result) = build(source.as_bytes(), 1_000_000);
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
    let (_temporary, result) = build(source.as_bytes(), 1);
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
        let (stored_hash, stored_size): (Vec<u8>, i64) = index
            .query_row(
                "SELECT file_sha256, file_size_bytes FROM data_shards WHERE file_name = ?1",
                [&asset.file_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored_hash, file_digest(&path).as_bytes());
        assert_eq!(
            stored_size,
            i64::try_from(fs::metadata(path).unwrap().len()).unwrap()
        );
    }
}

#[test]
fn repeated_builds_have_the_same_logical_identity() {
    let source = concat!(
        "{\"lang_code\":\"fr\",\"word\":\"ignore\"}\n",
        "{\"lang_code\":\"en\",\"word\":\"one\"}\n",
        "{\"lang_code\":\"en\",\"word\":\"two\"}\n"
    );
    let (_first_temp, first) = build(source.as_bytes(), 64);
    let (_second_temp, second) = build(source.as_bytes(), 64);
    assert_eq!(first.pack_revision, second.pack_revision);
    assert_eq!(
        first.manifest.selected_record_digest,
        second.manifest.selected_record_digest
    );
    assert_eq!(
        first
            .manifest
            .assets
            .iter()
            .map(|asset| (&asset.role, &asset.file_name))
            .collect::<Vec<_>>(),
        second
            .manifest
            .assets
            .iter()
            .map(|asset| (&asset.role, &asset.file_name))
            .collect::<Vec<_>>()
    );

    let logical_rows = |result: &elephant_ladder_dictionary_pack_builder::BuildResult| {
        database(result, "index")
            .prepare("SELECT selected_ordinal, source_line_number, shard_ordinal, authored_headword FROM entries ORDER BY selected_ordinal")
            .unwrap()
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(logical_rows(&first), logical_rows(&second));
    assert_eq!(first.report, second.report);
    assert_eq!(
        fs::read(first.output_directory.join(BUILD_REPORT_FILE)).unwrap(),
        fs::read(second.output_directory.join(BUILD_REPORT_FILE)).unwrap()
    );
}

#[test]
fn records_normalized_shapes_counts_and_translation_coverage() {
    let source = concat!(
        "{\"lang_code\":\"fr\",\"word\":\"ignored\",\"foreign\":true}\n",
        "{\"lang_code\":\"en\",\"word\":\"one\",\"x\":[1,2.5,null,true],",
        "\"senses\":[{\"translations\":[{},{}]},false],",
        "\"translations\":[{\"word\":\"uno\"}]}\n",
        "{\"lang_code\":\"en\",\"word\":\"two\",\"x\":{\"dynamic-value\":false},",
        "\"senses\":[{\"translations\":null}],\"translations\":\"unknown\"}\n"
    );
    let (_temporary, result) = build(source.as_bytes(), 1_000_000);
    let index = database(&result, "index");
    let rows: Vec<(String, String, u64, u64)> = index
        .prepare(
            "SELECT json_path, value_shape, occurrence_count, first_source_line \
             FROM source_shape_observations ORDER BY json_path, value_shape",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let report_rows = result
        .report
        .source_shape_observations
        .iter()
        .map(|row| {
            (
                row.json_path.clone(),
                row.value_shape.clone(),
                row.occurrence_count,
                row.first_source_line,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(rows, report_rows);
    assert!(rows.contains(&("$".to_owned(), "object".to_owned(), 2, 2)));
    assert!(rows.contains(&("$[\"x\"][]".to_owned(), "number:integer".to_owned(), 1, 2)));
    assert!(rows.contains(&(
        "$[\"x\"][]".to_owned(),
        "number:non-integer".to_owned(),
        1,
        2
    )));
    assert!(rows.contains(&(
        "$[\"x\"][\"dynamic-value\"]".to_owned(),
        "boolean".to_owned(),
        1,
        3
    )));
    assert!(!rows.iter().any(|row| row.0.contains("ignored")));
    assert_eq!(
        result.report.translation_coverage,
        elephant_ladder_dictionary_pack_builder::TranslationCoverage {
            selected_records_with_top_level_translation_array: 1,
            top_level_translation_items: 1,
            sense_objects: 2,
            sense_objects_with_translation_array: 1,
            sense_translation_items: 2,
        }
    );
}

#[test]
fn report_contains_exact_build_and_asset_facts() {
    let source = concat!(
        "{\"lang_code\":\"fr\",\"word\":\"ignore\"}\n",
        "{\"lang_code\":\"en\",\"word\":\"longer-word\"}\n",
        "{\"lang_code\":\"en\",\"word\":\"x\"}\n"
    );
    let (_temporary, result) = build(source.as_bytes(), 1);
    let report_path = result.output_directory.join(BUILD_REPORT_FILE);
    let report_bytes = fs::read(&report_path).unwrap();
    let report: BuildReport = serde_json::from_slice(&report_bytes).unwrap();
    assert_eq!(report, result.report);
    assert_eq!(report.source.record_count, 3);
    assert_eq!(
        report.source.size_bytes,
        u64::try_from(source.len()).unwrap()
    );
    assert_eq!(report.source.sha256, digest(source.as_bytes()));
    assert_eq!(report.selected.record_count, 2);
    assert_eq!(
        report.selected.digest,
        result.manifest.selected_record_digest
    );
    assert_eq!(report.shard_count, 2);
    assert_eq!(report.data_assets.len(), 2);
    assert_eq!(
        report.compression_ratio.uncompressed_bytes,
        report.selected.size_bytes
    );
    assert_eq!(
        report.compression_ratio.compressed_bytes,
        report.compressed_payload_bytes
    );
    assert_eq!(report.largest_selected_records[0].selected_ordinal, 0);
    assert_eq!(report.index_asset.sha256, result.manifest.assets[0].sha256);
    for asset in std::iter::once(&report.index_asset).chain(&report.data_assets) {
        let path = result.output_directory.join(&asset.file_name);
        assert_eq!(asset.size_bytes, fs::metadata(&path).unwrap().len());
        assert_eq!(asset.sha256, file_digest(&path));
    }
    let index = database(&result, "index");
    let lookup_count: u64 = index
        .query_row("SELECT count(*) FROM lookup_keys", [], |row| row.get(0))
        .unwrap();
    let compressed_count: u64 = index
        .query_row(
            "SELECT sum(compressed_payload_bytes) FROM data_shards",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(report.lookup_row_count, lookup_count);
    assert_eq!(report.compressed_payload_bytes, compressed_count);
    assert!(
        !result
            .manifest
            .assets
            .iter()
            .any(|asset| asset.file_name == BUILD_REPORT_FILE)
    );

    let mut value: Value = serde_json::from_slice(&report_bytes).unwrap();
    value["unexpected"] = Value::Bool(true);
    assert!(serde_json::from_value::<BuildReport>(value).is_err());
}

#[test]
fn shape_limits_fail_explicitly_and_clean_staging() {
    let cases = [
        (
            b"{\"lang_code\":\"en\",\"word\":\"x\",\"a\":{\"b\":1}}\n".as_slice(),
            BuildOptions {
                max_shape_depth: 1,
                ..BuildOptions::default()
            },
            "depth",
        ),
        (
            b"{\"lang_code\":\"en\",\"word\":\"x\",\"a\":1}\n".as_slice(),
            BuildOptions {
                max_shape_observations: 2,
                ..BuildOptions::default()
            },
            "distinct-observation",
        ),
        (
            b"{\"lang_code\":\"en\",\"word\":\"x\",\"a\":[1,2]}\n".as_slice(),
            BuildOptions {
                max_shape_nodes: 3,
                ..BuildOptions::default()
            },
            "traversal",
        ),
        (
            b"{\"lang_code\":\"en\",\"word\":\"x\",\"long-field\":1}\n".as_slice(),
            BuildOptions {
                max_shape_path_bytes: 8,
                ..BuildOptions::default()
            },
            "path",
        ),
    ];
    for (source, options, expected) in cases {
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("pack");
        let error = build_pack(
            &manifest(source, 1_000),
            Cursor::new(source),
            &output,
            options,
        )
        .unwrap_err();
        assert!(matches!(error, BuildError::BuildLimit(_)));
        assert!(error.to_string().contains(expected), "{error}");
        assert!(!output.join(BUILD_REPORT_FILE).exists());
        assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 0);
    }
}

#[test]
fn source_failures_leave_no_output_directory() {
    let source = b"{not json}\n";
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("pack");
    let error = build_pack(
        &manifest(source, 1_000),
        Cursor::new(source),
        &output,
        BuildOptions::default(),
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
        &manifest(source, 1_000),
        Cursor::new(source),
        &output,
        BuildOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(error, BuildError::OutputExists(path) if path == output));
    assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"keep");
}

#[test]
fn strict_manifest_rejects_unknown_fields_and_bad_hash_width() {
    let base = serde_json::to_value(manifest(b"x", 1)).unwrap();
    let mut unknown = base.clone();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("unexpected".to_owned(), Value::Bool(true));
    assert!(serde_json::from_value::<BuildManifest>(unknown).is_err());

    let mut bad_hash = base;
    bad_hash.as_object_mut().unwrap().insert(
        "uncompressed_source_sha256".to_owned(),
        Value::String("00".to_owned()),
    );
    assert!(serde_json::from_value::<BuildManifest>(bad_hash).is_err());
}
