use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use elephant_ladder_dictionary_pack::{
    BilingualLookupOutcome, BilingualView, DictionaryPack, LookupOptions, MatchClass, PackLimits,
    ProjectionOptions, RelationKind,
};
use elephant_ladder_dictionary_pack_builder::{BuildManifest, BuildOptions, Sha256Hex, build_pack};
use serde_json::Value;
use sha2::{Digest, Sha256};

const FIXTURE_SHA256: &str = "94b649cad4f25aaaefdeeb5443c966c93f3eedc9380e24bc7e65b8b99e71fc2d";
const RECORD_SHA256: [&str; 4] = [
    "270f9dfd3fc81da37daaafbd77b41789e564befeb44138fbc438df62c32eba1a",
    "11df3370b42ebc46bd09d709455048fd3ec69418fa4d27c7623ef91f4a4c59ed",
    "9a06dc011059b9ffb622b937256306c64b1574dc8dfb1be1dd2737c14f4cfa0e",
    "e32bfb41cfedb94bdcb18eb1f18cede88733a300afe72b031416ebe2acaa0e36",
];

fn fixture_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/dictionaries")
        .join(file)
}

fn digest(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from_bytes(Sha256::digest(bytes).into())
}

fn text<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing provenance string at {pointer}"))
}

fn verify_fixture(fixture: &[u8], provenance: &Value) {
    assert_eq!(fixture.len(), 96_425);
    assert_eq!(digest(fixture).to_hex(), FIXTURE_SHA256);
    assert!(fixture.ends_with(b"\n"));
    assert!(!fixture[..fixture.len() - 1].contains(&b'\r'));
    let records = fixture[..fixture.len() - 1]
        .split(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 4);
    for (record, expected) in records.iter().zip(RECORD_SHA256) {
        assert_eq!(digest(record).to_hex(), expected);
    }

    assert_eq!(provenance["schema_version"], 1);
    assert_eq!(
        text(provenance, "/fixture/file"),
        "frwiktionary-20260901.jsonl"
    );
    assert_eq!(text(provenance, "/fixture/sha256"), FIXTURE_SHA256);
    assert_eq!(provenance["fixture"]["size_bytes"], 96_425);
    assert_eq!(provenance["fixture"]["line_count"], 4);
    assert_eq!(
        text(provenance, "/source/wiktionary_edition"),
        "frwiktionary"
    );
    assert_eq!(text(provenance, "/source/selected_entry_language"), "fr");
    assert_eq!(text(provenance, "/source/dump_date"), "2026-09-01");
    assert_eq!(
        text(provenance, "/source/raw_extraction/url"),
        "https://kaikki.org/dictionary/downloads/fr/fr-extract.jsonl.gz"
    );
    assert_eq!(
        text(provenance, "/retrieved_and_verified_date"),
        "2026-09-09"
    );
    for (index, expected) in RECORD_SHA256.iter().enumerate() {
        assert_eq!(
            text(
                provenance,
                &format!("/records/{index}/record_sha256_excluding_lf")
            ),
            *expected
        );
    }
}

fn fixture_manifest(fixture: &[u8], provenance: &Value) -> BuildManifest {
    let source_manifest = provenance.clone();
    let license_manifest = provenance["licensing"].clone();
    BuildManifest {
        pack_id: "wiktionary-fr-fr".to_owned(),
        corpus_language: "fr".to_owned(),
        wiktionary_edition: "frwiktionary".to_owned(),
        wiktionary_dump_date: "2026-09-01".to_owned(),
        kaikki_extraction_date: "2026-09-08".to_owned(),
        source_url: text(provenance, "/source/raw_extraction/url").to_owned(),
        compressed_source_sha256: text(provenance, "/source/raw_extraction/compressed_sha256")
            .parse()
            .unwrap(),
        uncompressed_source_sha256: digest(fixture),
        wiktextract_revision: text(provenance, "/source/wiktextract_commit").to_owned(),
        wikitextprocessor_revision: text(provenance, "/source/wikitextprocessor_commit").to_owned(),
        builder_revision: "frwiktionary-fixture-integration-v1".to_owned(),
        compression_profile: "zstd-v1-level-6".to_owned(),
        routing_policy: "routing-v1".to_owned(),
        target_shard_payload_bytes: 1_000_000,
        source_manifest_sha256: digest(&serde_json::to_vec(&source_manifest).unwrap()),
        source_manifest,
        license_manifest_sha256: digest(&serde_json::to_vec(&license_manifest).unwrap()),
        license_manifest,
        compatible_audio_collection: None,
        minimum_app_version: "0.1.0".to_owned(),
    }
}

fn verify_detail_semantics(pack: &DictionaryPack) {
    let report = pack.validate_all().unwrap();
    assert_eq!(report.authenticated_record_count, 4);

    let elephant = pack.lookup("éléphant", LookupOptions::default()).unwrap();
    let entry = pack
        .entry(
            &elephant.matches[0].entry.reference,
            ProjectionOptions::detail(),
        )
        .unwrap();
    assert_eq!(entry.etymology.total, 1);
    assert_eq!(entry.senses.total, 4);
    assert_eq!(entry.translations.total, 183);
    let example = &entry.senses.items[0].examples.items[0];
    assert_eq!(example.text_emphasis[0].start, 153);
    assert!(example.reference.is_some());
    let derived = entry
        .relations
        .iter()
        .find(|group| group.kind == RelationKind::Derived)
        .unwrap();
    assert_eq!(derived.relations.total, 33);

    let verb = pack.lookup("échelle", LookupOptions::default()).unwrap();
    let verb = &verb.matches[1].entry;
    assert_eq!(verb.senses.items[0].form_of[0].word, "écheler");
}

fn verify_lookup_semantics(pack: Arc<DictionaryPack>) {
    let elephant = pack.lookup("éléphant", LookupOptions::default()).unwrap();
    assert_eq!(elephant.matches.len(), 1);
    assert_eq!(
        elephant.matches[0].match_class,
        MatchClass::AuthoredHeadword
    );
    assert_eq!(
        elephant.matches[0].entry.part_of_speech.as_deref(),
        Some("noun")
    );
    assert_eq!(
        elephant.matches[0].entry.pronunciations.items[0]
            .ipa
            .as_deref(),
        Some("\\e.le.fɑ̃\\")
    );

    let ladder = pack.lookup("échelle", LookupOptions::default()).unwrap();
    assert_eq!(
        ladder
            .matches
            .iter()
            .map(|matched| (
                matched.entry.reference.selected_ordinal,
                matched.entry.part_of_speech.as_deref()
            ))
            .collect::<Vec<_>>(),
        [(1, Some("noun")), (2, Some("verb"))]
    );

    for (form, selected_ordinal) in [("échelles", 1), ("j’échelle", 2), ("livres numériques", 3)]
    {
        let result = pack.lookup(form, LookupOptions::default()).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(
            result.matches[0].entry.reference.selected_ordinal,
            selected_ordinal
        );
        assert_eq!(result.matches[0].match_class, MatchClass::AuthoredForm);
    }

    let bilingual = BilingualView::new("french-english", pack, "en").unwrap();
    let BilingualLookupOutcome::Translations(translated) = bilingual
        .lookup("livre numérique", LookupOptions::default())
        .unwrap()
    else {
        panic!("expected English translations");
    };
    assert!(
        translated.matches[0]
            .entry
            .senses
            .items
            .iter()
            .all(|sense| sense.translations.items.is_empty())
    );
    assert_eq!(
        translated.matches[0]
            .entry
            .translations
            .items
            .iter()
            .map(|translation| translation.word.as_deref())
            .collect::<Vec<_>>(),
        [Some("e-book"), Some("electronic book")]
    );
}

#[test]
fn pinned_frwiktionary_fixture_builds_and_exercises_real_lookup_semantics() {
    let fixture = fs::read(fixture_path("frwiktionary-20260901.jsonl")).unwrap();
    let provenance_bytes = fs::read(fixture_path("frwiktionary-20260901.provenance.json")).unwrap();
    let provenance: Value = serde_json::from_slice(&provenance_bytes).unwrap();
    verify_fixture(&fixture, &provenance);

    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("pack");
    build_pack(
        &fixture_manifest(&fixture, &provenance),
        Cursor::new(&fixture),
        &output,
        BuildOptions::default(),
    )
    .unwrap();
    let pack = Arc::new(DictionaryPack::open(output, PackLimits::default()).unwrap());
    verify_detail_semantics(&pack);
    verify_lookup_semantics(pack);
}
