mod common;

use std::io::Cursor;

use elephant_ladder_dictionary_pack::{DictionaryPack, LookupOptions};
use elephant_ladder_dictionary_pack_builder::{
    EditionConfig, MAX_MIRROR_PART_BYTES, SourceSnapshot, build_pack, corpus_identity,
    validate_snapshot,
};
use serde_json::{Value, json};

const SOURCE: &[u8] = b"{\"lang_code\":\"fr\",\"word\":\"livre\"}\n";

fn edition() -> EditionConfig {
    serde_json::from_slice(&std::fs::read(common::fr_edition_path()).unwrap()).unwrap()
}

fn snapshot_json() -> Value {
    let mut value = common::fr_snapshot_json(SOURCE, MAX_MIRROR_PART_BYTES + 10);
    let sha = value["compressed"]["sha256"].clone();
    value["mirror"]["parts"] = json!([
        {"file_name": "frwiktionary-20260901-f5150fd28b8d.jsonl.gz.part000",
         "size_bytes": MAX_MIRROR_PART_BYTES, "sha256": sha},
        {"file_name": "frwiktionary-20260901-f5150fd28b8d.jsonl.gz.part001",
         "size_bytes": 10, "sha256": sha}
    ]);
    value
}

fn snapshot(value: Value) -> SourceSnapshot {
    serde_json::from_value(value).unwrap()
}

#[test]
fn snapshot_identities_are_deterministic_and_build() {
    let edition = edition();
    let snapshot = snapshot(snapshot_json());
    validate_snapshot(&edition, &snapshot).unwrap();
    let identity = corpus_identity(&edition, &snapshot, "builder").unwrap();
    assert_eq!(
        identity,
        corpus_identity(&edition, &snapshot, "builder").unwrap()
    );
    assert_eq!(identity.kaikki_extraction_date, "2026-09-15");
    assert_eq!(identity.wiktextract_revision, "d6fca27");
    let source_manifest: Value = serde_json::from_str(&identity.source_manifest).unwrap();
    assert_eq!(
        source_manifest["snapshot"]["mirror"]["release_tag"],
        "source-frwiktionary-20260901-f5150fd28b8d"
    );
    assert!(corpus_identity(&edition, &snapshot, "").is_err());

    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("pack");
    build_pack(&identity, Cursor::new(SOURCE), &output).unwrap();
    let pack = DictionaryPack::open(&output).unwrap();
    assert_eq!(pack.identity(), &identity);
    assert_eq!(
        pack.lookup("livre", None, LookupOptions::default())
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
