use std::io::Cursor;
use std::path::PathBuf;

use elephant_ladder_dictionary_pack::{DictionaryPack, LookupOptions, PackLimits};
use elephant_ladder_dictionary_pack_builder::{
    BuildOptions, EditionConfig, MAX_MIRROR_PART_BYTES, SourceSnapshot, build_manifest, build_pack,
    validate_snapshot,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SOURCE: &[u8] = b"{\"lang_code\":\"fr\",\"word\":\"livre\"}\n";

fn edition() -> EditionConfig {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../editions/frwiktionary.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn snapshot_json() -> Value {
    let compressed_sha = "f5150fd28b8d160d6377f951597700ebf9ca7b86be9964413099980de4e3c650";
    json!({
        "schema_version": 1,
        "wiktionary_edition": "frwiktionary",
        "captured_at": "2026-09-16T12:09:00Z",
        "origin": {
            "source_url": "https://kaikki.org/frwiktionary/raw-wiktextract-data.jsonl.gz",
            "info_url": "https://kaikki.org/frwiktionary/rawdata.html",
            "etag": "\"6aa8f64f-2ad47c51\"",
            "last_modified": "Tue, 15 Sep 2026 07:39:59 GMT",
            "info_statement": "extracted on 2026-09-15 from the frwiktionary dump dated 2026-09-01 using wiktextract ( d6fca27 and 65e1673 )"
        },
        "dump_date": "2026-09-01",
        "extraction_date": "2026-09-15",
        "wiktextract_revision": "d6fca27",
        "wikitextprocessor_revision": "65e1673",
        "compressed": {"size_bytes": MAX_MIRROR_PART_BYTES + 10, "sha256": compressed_sha},
        "uncompressed": {
            "size_bytes": SOURCE.len(),
            "sha256": hex::encode(Sha256::digest(SOURCE)),
            "line_count": 1
        },
        "mirror": {
            "repository": "redvelo/elephant-ladder-dictionary-packs",
            "release_tag": "source-frwiktionary-20260901-f5150fd28b8d",
            "parts": [
                {"file_name": "frwiktionary-20260901-f5150fd28b8d.jsonl.gz.part000",
                 "size_bytes": MAX_MIRROR_PART_BYTES, "sha256": compressed_sha},
                {"file_name": "frwiktionary-20260901-f5150fd28b8d.jsonl.gz.part001",
                 "size_bytes": 10, "sha256": compressed_sha}
            ],
            "info_page": {"file_name": "frwiktionary-20260901-f5150fd28b8d.rawdata.html",
                          "size_bytes": 15279, "sha256": compressed_sha}
        }
    })
}

fn snapshot(value: Value) -> SourceSnapshot {
    serde_json::from_value(value).unwrap()
}

#[test]
fn snapshot_manifests_are_deterministic_and_build() {
    let edition = edition();
    let snapshot = snapshot(snapshot_json());
    validate_snapshot(&edition, &snapshot).unwrap();
    let manifest = build_manifest(&edition, &snapshot, "builder").unwrap();
    assert_eq!(
        serde_json::to_vec(&manifest).unwrap(),
        serde_json::to_vec(&build_manifest(&edition, &snapshot, "builder").unwrap()).unwrap()
    );
    assert_eq!(manifest.kaikki_extraction_date, "2026-09-15");
    assert_eq!(manifest.wiktextract_revision, "d6fca27");
    assert_eq!(
        manifest.source_manifest["snapshot"]["mirror"]["release_tag"],
        "source-frwiktionary-20260901-f5150fd28b8d"
    );

    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("pack");
    build_pack(
        &manifest,
        Cursor::new(SOURCE),
        &output,
        BuildOptions::default(),
    )
    .unwrap();
    let pack = DictionaryPack::open(&output, PackLimits::default()).unwrap();
    assert_eq!(pack.metadata().kaikki_extraction_date, "2026-09-15");
    assert_eq!(
        pack.lookup("livre", LookupOptions::default())
            .unwrap()
            .matches
            .len(),
        1
    );
}

#[test]
fn inconsistent_snapshots_are_rejected() {
    let edition = edition();
    let mutations: [fn(&mut Value); 9] = [
        |value| value["wiktionary_edition"] = json!("enwiktionary"),
        |value| {
            value["origin"]["source_url"] =
                json!("https://kaikki.org/dictionary/downloads/fr/fr-extract.jsonl.gz");
        },
        |value| value["extraction_date"] = json!("2026-08-31"),
        |value| value["dump_date"] = json!("September"),
        |value| value["wiktextract_revision"] = json!(""),
        |value| value["mirror"]["release_tag"] = json!("latest"),
        |value| value["mirror"]["repository"] = json!("not-a-repository"),
        |value| value["mirror"]["parts"][1]["size_bytes"] = json!(11),
        |value| {
            value["mirror"]["parts"][0]["size_bytes"] = json!(MAX_MIRROR_PART_BYTES + 1);
            value["mirror"]["parts"][1]["size_bytes"] = json!(9);
        },
    ];
    for mutate in mutations {
        let mut value = snapshot_json();
        mutate(&mut value);
        assert!(
            validate_snapshot(&edition, &snapshot(value.clone())).is_err(),
            "accepted {value}"
        );
    }

    let mut duplicate = snapshot_json();
    duplicate["mirror"]["parts"][1]["file_name"] =
        duplicate["mirror"]["info_page"]["file_name"].clone();
    assert!(validate_snapshot(&edition, &snapshot(duplicate)).is_err());

    let mut unknown = snapshot_json();
    unknown["mirror"]["url"] = json!("https://example.invalid/");
    assert!(serde_json::from_value::<SourceSnapshot>(unknown).is_err());
}
