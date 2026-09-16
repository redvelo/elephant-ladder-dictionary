use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use elephant_ladder_dictionary_pack::{
    BilingualView, DictionaryPack, DictionaryView, LookupOptions, MatchClass, MonolingualView,
    PackLimits, ProjectionOptions, RelationKind, Section, SectionPage, ViewOutcome,
};
use elephant_ladder_dictionary_pack_builder::{BuildManifest, BuildOptions, Sha256Hex, build_pack};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tempfile::TempDir;

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

/// Verifies a fixture against its provenance, builds it, and exhaustively validates it.
fn build_fixture(stem: &str, language: &str) -> (TempDir, Arc<DictionaryPack>) {
    let fixture = fs::read(fixture_path(&format!("{stem}.jsonl"))).unwrap();
    let provenance: Value = serde_json::from_slice(
        &fs::read(fixture_path(&format!("{stem}.provenance.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(
        digest(&fixture).to_hex(),
        text(&provenance, "/fixture/sha256")
    );
    assert_eq!(
        provenance["fixture"]["size_bytes"].as_u64(),
        Some(fixture.len() as u64)
    );
    let records = fixture[..fixture.len() - 1]
        .split(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    let expected = provenance["records"].as_array().unwrap();
    assert_eq!(records.len(), expected.len());
    for (record, expected) in records.iter().zip(expected) {
        assert_eq!(
            digest(record).to_hex(),
            text(expected, "/record_sha256_excluding_lf")
        );
    }

    let source_manifest = provenance.clone();
    let license_manifest = provenance["licensing"].clone();
    let manifest = BuildManifest {
        pack_id: format!("wiktionary-{language}-{language}"),
        corpus_language: language.to_owned(),
        wiktionary_edition: text(&provenance, "/source/wiktionary_edition").to_owned(),
        wiktionary_dump_date: text(&provenance, "/source/dump_date").to_owned(),
        kaikki_extraction_date: "fixture".to_owned(),
        source_url: text(&provenance, "/source/raw_extraction/url").to_owned(),
        compressed_source_sha256: text(&provenance, "/source/raw_extraction/compressed_sha256")
            .parse()
            .unwrap(),
        uncompressed_source_sha256: digest(&fixture),
        wiktextract_revision: "fixture".to_owned(),
        wikitextprocessor_revision: "fixture".to_owned(),
        builder_revision: format!("{stem}-fixture-integration-v1"),
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
    let output = temporary.path().join("pack");
    build_pack(
        &manifest,
        Cursor::new(&fixture),
        &output,
        BuildOptions::default(),
    )
    .unwrap();
    let pack = Arc::new(DictionaryPack::open(output, PackLimits::default()).unwrap());
    let report = pack.validate_all().unwrap();
    assert_eq!(report.authenticated_record_count, records.len() as u64);
    (temporary, pack)
}

#[test]
fn english_fixture_covers_bilingual_targets_large_entries_and_lemmas() {
    let (_temporary, pack) = build_fixture("enwiktionary-20260902", "en");

    for target in ["fr", "es", "de", "it", "ja", "zh"] {
        let view = DictionaryView::Bilingual(
            BilingualView::new(format!("en-{target}"), pack.clone(), target).unwrap(),
        );
        assert!(
            matches!(
                view.lookup("elephant", LookupOptions::default()),
                ViewOutcome::Completed(_)
            ),
            "no {target} translation of elephant"
        );
    }

    let run = pack.lookup("run", LookupOptions::default()).unwrap();
    let summary = &run.matches[0].entry;
    assert_eq!(summary.part_of_speech.as_deref(), Some("verb"));
    assert_eq!(summary.senses.total, 64);
    assert_eq!(summary.senses.items.len(), 16);
    let SectionPage::Senses(later) = pack
        .section_page(
            &summary.reference,
            Section::Senses,
            60,
            16,
            ProjectionOptions::detail(),
        )
        .unwrap()
    else {
        panic!("expected senses");
    };
    assert_eq!((later.offset, later.items.len()), (60, 4));

    let ran = pack.lookup("ran", LookupOptions::default()).unwrap();
    assert_eq!(ran.matches[0].match_class, MatchClass::AuthoredHeadword);
    let lemma = ran.matches[0]
        .entry
        .senses
        .items
        .iter()
        .flat_map(|sense| &sense.form_of)
        .next()
        .unwrap();
    assert_eq!(lemma.word, "run");

    let ebook = pack.lookup("e-book", LookupOptions::default()).unwrap();
    let audio = ebook.matches[0]
        .entry
        .pronunciations
        .items
        .iter()
        .find_map(|pronunciation| pronunciation.audio.as_ref())
        .unwrap();
    assert_eq!(
        audio.file_name.as_deref(),
        Some("LL-Q1860 (eng)-Vealhurl-e-book.wav")
    );
}

#[test]
fn japanese_fixture_covers_kana_kanji_forms_and_lemma_links() {
    let (_temporary, pack) = build_fixture("jawiktionary-20260901", "ja");

    let cat = pack.lookup("猫", LookupOptions::default()).unwrap();
    assert_eq!(cat.matches[0].entry.headword, "ねこ");
    assert_eq!(cat.matches[0].match_class, MatchClass::AuthoredForm);
    assert!(
        cat.matches[0]
            .entry
            .pronunciations
            .items
            .iter()
            .any(|pronunciation| pronunciation.audio.is_some())
    );

    let kanji = pack.lookup("走る", LookupOptions::default()).unwrap();
    let headwords = kanji
        .matches
        .iter()
        .map(|matched| matched.entry.headword.as_str())
        .collect::<Vec<_>>();
    assert_eq!(headwords, ["走る"]);
    assert_eq!(
        kanji.matches[0].entry.senses.items[0].form_of[0].word,
        "はしる"
    );

    let verb = pack.lookup("はしる", LookupOptions::default()).unwrap();
    let entry = pack
        .entry(
            &verb.matches[0].entry.reference,
            ProjectionOptions::detail(),
        )
        .unwrap();
    let example = entry
        .senses
        .items
        .iter()
        .flat_map(|sense| &sense.examples.items)
        .next()
        .unwrap();
    assert!(!example.text_emphasis.is_empty());
    assert!(
        entry
            .relations
            .iter()
            .any(|group| group.kind == RelationKind::Synonyms)
    );
}

#[test]
fn chinese_fixture_covers_variants_legacy_pronunciations_and_large_entries() {
    let (_temporary, pack) = build_fixture("zhwiktionary-20260901", "zh");

    let simplified = pack.lookup("电子书", LookupOptions::default()).unwrap();
    assert_eq!(simplified.matches[0].entry.headword, "電子書");
    assert!(
        simplified.matches[0]
            .entry
            .pronunciations
            .items
            .iter()
            .any(|pronunciation| pronunciation.zh_pronunciation.is_some())
    );

    let cat = pack.lookup("貓", LookupOptions::default()).unwrap();
    let summary = &cat.matches[0].entry;
    assert_eq!(summary.pronunciations.total, 138);
    assert_eq!(summary.pronunciations.items.len(), 8);
    let entry = pack
        .entry(&summary.reference, ProjectionOptions::detail())
        .unwrap();
    let synonyms = entry
        .relations
        .iter()
        .find(|group| group.kind == RelationKind::Synonyms)
        .unwrap();
    assert_eq!(synonyms.relations.total, 558);
    assert_eq!(synonyms.relations.items.len(), 256);
    let SectionPage::Relations(rest) = pack
        .section_page(
            &summary.reference,
            Section::Relations(RelationKind::Synonyms),
            512,
            256,
            ProjectionOptions::detail(),
        )
        .unwrap()
    else {
        panic!("expected synonyms");
    };
    assert_eq!(rest.items.len(), 46);

    let variant = pack.lookup("猫", LookupOptions::default()).unwrap();
    assert_eq!(variant.matches[0].entry.headword, "猫");

    let ladder = pack.lookup("梯子", LookupOptions::default()).unwrap();
    let ladder = pack
        .entry(
            &ladder.matches[0].entry.reference,
            ProjectionOptions::detail(),
        )
        .unwrap();
    assert!(ladder.descendants.total > 0);

    let view = DictionaryView::Monolingual(MonolingualView::new("zh", pack).unwrap());
    assert!(matches!(
        view.lookup("不存在的詞", LookupOptions::default()),
        ViewOutcome::NoEntry
    ));
}
