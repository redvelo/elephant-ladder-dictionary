use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

use elephant_ladder_dictionary_pack::{DictionaryPack, PackLimits};
use elephant_ladder_dictionary_pack_builder::{BuildManifest, Sha256Hex};
use serde_json::json;
use sha2::{Digest, Sha256};

fn digest(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from_bytes(Sha256::digest(bytes).into())
}

#[test]
fn build_reads_decompressed_jsonl_from_stdin() {
    let source = b"{\"lang_code\":\"en\",\"word\":\"streamed\"}\n";
    let source_manifest = json!({"provider":"stdin-fixture"});
    let license_manifest = json!({"license":"fixture"});
    let manifest = BuildManifest {
        pack_id: "wiktionary-en-en".to_owned(),
        corpus_language: "en".to_owned(),
        wiktionary_edition: "en".to_owned(),
        wiktionary_dump_date: "2026-09-02".to_owned(),
        kaikki_extraction_date: "2026-09-06".to_owned(),
        source_url: "https://example.invalid/source.jsonl.gz".to_owned(),
        compressed_source_sha256: digest(b"compressed fixture"),
        uncompressed_source_sha256: digest(source),
        wiktextract_revision: "wiktextract-revision".to_owned(),
        wikitextprocessor_revision: "wikitextprocessor-revision".to_owned(),
        builder_revision: "builder-v1".to_owned(),
        compression_profile: "zstd-v1-level-6".to_owned(),
        routing_policy: "routing-v1".to_owned(),
        target_shard_payload_bytes: 1_000_000,
        source_manifest_sha256: digest(&serde_json::to_vec(&source_manifest).unwrap()),
        source_manifest,
        license_manifest_sha256: digest(&serde_json::to_vec(&license_manifest).unwrap()),
        license_manifest,
        compatible_audio_collection: None,
        minimum_app_version: "0.1.0".to_owned(),
    };
    let temporary = tempfile::tempdir().unwrap();
    let manifest_path = temporary.path().join("manifest.json");
    let output_path = temporary.path().join("pack");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_elephant-dictionary-pack"))
        .args([
            "build",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--input",
            "-",
            "--output",
            output_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(source).unwrap();
    let result = child.wait_with_output().unwrap();

    assert!(
        result.status.success(),
        "builder failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("pack_revision="));
    DictionaryPack::open(output_path, PackLimits::default()).unwrap();
}
