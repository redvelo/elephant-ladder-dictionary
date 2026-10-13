#![allow(dead_code)]

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use elephant_ladder_dictionary_pack::{CorpusIdentity, DictionaryPack, Sha256Hex};
use elephant_ladder_dictionary_pack_builder::{BuildResult, build_pack};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

pub fn digest(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from_bytes(Sha256::digest(bytes).into())
}

pub fn identity(language: &str, source: &[u8], target_shard_payload_bytes: u64) -> CorpusIdentity {
    CorpusIdentity {
        pack_id: format!("wiktionary-{language}-{language}").parse().unwrap(),
        corpus_language: language.to_owned(),
        wiktionary_edition: format!("{language}wiktionary"),
        wiktionary_dump_date: "2026-09-02".to_owned(),
        kaikki_extraction_date: "2026-09-06".to_owned(),
        source_url: "https://example.invalid/source.jsonl.gz".to_owned(),
        compressed_source_sha256: digest(b"compressed fixture"),
        uncompressed_source_sha256: digest(source),
        wiktextract_revision: "wiktextract-revision".to_owned(),
        wikitextprocessor_revision: "wikitextprocessor-revision".to_owned(),
        builder_revision: "builder-v1".to_owned(),
        target_shard_payload_bytes,
        source_manifest: json!({"provider": "fixture", "version": 1}).to_string(),
        license_manifest:
            json!({"licenses": [{"spdx": "CC-BY-SA-4.0"}, {"spdx": "GFDL-1.3-only"}]}).to_string(),
        minimum_app_version: "0.1.0".to_owned(),
    }
}

pub fn build_into(
    output: &Path,
    language: &str,
    source: &[u8],
    target_shard_payload_bytes: u64,
) -> BuildResult {
    build_pack(
        &identity(language, source, target_shard_payload_bytes),
        Cursor::new(source),
        output,
    )
    .unwrap()
}

pub fn build_result(source: &[u8], target_shard_payload_bytes: u64) -> (TempDir, BuildResult) {
    let temporary = tempfile::tempdir().unwrap();
    let result = build_into(
        &temporary.path().join("pack"),
        "en",
        source,
        target_shard_payload_bytes,
    );
    (temporary, result)
}

pub fn build(
    language: &str,
    source: &[u8],
    target_shard_payload_bytes: u64,
) -> (TempDir, Arc<DictionaryPack>) {
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("pack");
    build_into(&output, language, source, target_shard_payload_bytes);
    (temporary, Arc::new(DictionaryPack::open(output).unwrap()))
}

pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

pub fn fr_edition_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../editions/frwiktionary.json")
}

/// A snapshot of the French edition whose uncompressed facts describe `source`.
pub fn fr_snapshot_json(source: &[u8], compressed_size: u64) -> Value {
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
        "compressed": {"size_bytes": compressed_size, "sha256": compressed_sha},
        "uncompressed": {
            "size_bytes": source.len(),
            "sha256": digest(source).to_hex(),
            "line_count": source.split(|byte| *byte == b'\n').count() - 1
        },
        "mirror": {
            "repository": "redvelo/elephant-ladder-dictionary",
            "release_tag": "source-frwiktionary-20260901-f5150fd28b8d",
            "parts": [
                {"file_name": "frwiktionary-20260901-f5150fd28b8d.jsonl.gz.part000",
                 "size_bytes": compressed_size, "sha256": compressed_sha}
            ],
            "info_page": {"file_name": "frwiktionary-20260901-f5150fd28b8d.rawdata.html",
                          "size_bytes": 15279, "sha256": compressed_sha}
        }
    })
}
