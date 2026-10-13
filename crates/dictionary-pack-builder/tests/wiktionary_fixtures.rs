//! Pinned real Wiktextract records, verified against their provenance, built, and
//! exercised through lookup.

mod common;

use std::fs;
use std::io::Cursor;
use std::sync::Arc;

use elephant_ladder_dictionary_pack::{
    CorpusIdentity, DictionaryPack, DictionaryView, LookupOptions, MatchClass, ProjectionOptions,
    RelationKind, Section, SectionPage, ViewOutcome,
};
use elephant_ladder_dictionary_pack_builder::build_pack;
use serde_json::Value;
use tempfile::TempDir;

fn text<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing provenance string at {pointer}"))
}

/// Only the French provenance records extractor revisions.
fn optional_text(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or("fixture")
        .to_owned()
}

/// Verifies a fixture against its provenance, builds it, and exhaustively validates it.
fn build_fixture(stem: &str, language: &str) -> (TempDir, Arc<DictionaryPack>) {
    let directory = common::fixtures().join("dictionaries");
    let fixture = fs::read(directory.join(format!("{stem}.jsonl"))).unwrap();
    let provenance: Value = serde_json::from_slice(
        &fs::read(directory.join(format!("{stem}.provenance.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(
        common::digest(&fixture).to_hex(),
        text(&provenance, "/fixture/sha256")
    );
    assert_eq!(
        provenance["fixture"]["size_bytes"].as_u64(),
        Some(fixture.len() as u64)
    );
    assert!(fixture.ends_with(b"\n"));
    let records = fixture[..fixture.len() - 1]
        .split(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    let expected = provenance["records"].as_array().unwrap();
    assert_eq!(records.len(), expected.len());
    assert_eq!(
        provenance["fixture"]["line_count"].as_u64(),
        Some(records.len() as u64)
    );
    for (record, expected) in records.iter().zip(expected) {
        assert_eq!(
            common::digest(record).to_hex(),
            text(expected, "/record_sha256_excluding_lf")
        );
    }

    let identity = CorpusIdentity {
        pack_id: format!("wiktionary-{language}-{language}").parse().unwrap(),
        corpus_language: language.to_owned(),
        wiktionary_edition: text(&provenance, "/source/wiktionary_edition").to_owned(),
        wiktionary_dump_date: text(&provenance, "/source/dump_date").to_owned(),
        kaikki_extraction_date: "fixture".to_owned(),
        source_url: text(&provenance, "/source/raw_extraction/url").to_owned(),
        compressed_source_sha256: text(&provenance, "/source/raw_extraction/compressed_sha256")
            .parse()
            .unwrap(),
        uncompressed_source_sha256: common::digest(&fixture),
        wiktextract_revision: optional_text(&provenance, "/source/wiktextract_commit"),
        wikitextprocessor_revision: optional_text(&provenance, "/source/wikitextprocessor_commit"),
        builder_revision: format!("{stem}-fixture-integration-v1"),
        target_shard_payload_bytes: 1_000_000,
        source_manifest: provenance.to_string(),
        license_manifest: provenance["licensing"].to_string(),
        minimum_app_version: "0.1.0".to_owned(),
    };
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("pack");
    build_pack(&identity, Cursor::new(&fixture), &output).unwrap();
    let pack = Arc::new(DictionaryPack::open(output).unwrap());
    let report = pack.validate_all().unwrap();
    assert_eq!(report.authenticated_record_count, records.len() as u64);
    (temporary, pack)
}

#[test]
fn french_fixture_covers_forms_relations_and_bilingual_translations() {
    let (_temporary, pack) = build_fixture("frwiktionary-20260901", "fr");

    let elephant = pack
        .lookup("éléphant", None, LookupOptions::default())
        .unwrap();
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
    let entry = pack
        .entry(
            &elephant.matches[0].entry.reference,
            None,
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

    let ladder = pack
        .lookup("échelle", None, LookupOptions::default())
        .unwrap();
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
    assert_eq!(
        ladder.matches[1].entry.senses.items[0].form_of[0].word,
        "écheler"
    );

    for (form, selected_ordinal) in [("échelles", 1), ("j’échelle", 2), ("livres numériques", 3)]
    {
        let result = pack.lookup(form, None, LookupOptions::default()).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(
            result.matches[0].entry.reference.selected_ordinal,
            selected_ordinal
        );
        assert_eq!(result.matches[0].match_class, MatchClass::AuthoredForm);
    }

    let bilingual = DictionaryView::new("french-english", pack, Some("en")).unwrap();
    let ViewOutcome::Completed(translated) =
        bilingual.lookup("livre numérique", LookupOptions::default())
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
fn english_fixture_covers_bilingual_targets_large_entries_and_lemmas() {
    let (_temporary, pack) = build_fixture("enwiktionary-20260902", "en");

    for target in ["fr", "es", "de", "it", "ja", "zh"] {
        let view = DictionaryView::new(format!("en-{target}"), pack.clone(), Some(target)).unwrap();
        assert!(
            matches!(
                view.lookup("elephant", LookupOptions::default()),
                ViewOutcome::Completed(_)
            ),
            "no {target} translation of elephant"
        );
    }

    let verb = pack.lookup("run", None, LookupOptions::default()).unwrap();
    let summary = &verb.matches[0].entry;
    assert_eq!(summary.part_of_speech.as_deref(), Some("verb"));
    assert_eq!(summary.senses.total, 64);
    assert_eq!(summary.senses.items.len(), 16);
    let SectionPage::Senses(later) = pack
        .section_page(
            &summary.reference,
            None,
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

    let past = pack.lookup("ran", None, LookupOptions::default()).unwrap();
    assert_eq!(past.matches[0].match_class, MatchClass::AuthoredHeadword);
    let lemma = past.matches[0]
        .entry
        .senses
        .items
        .iter()
        .flat_map(|sense| &sense.form_of)
        .next()
        .unwrap();
    assert_eq!(lemma.word, "run");

    let ebook = pack
        .lookup("e-book", None, LookupOptions::default())
        .unwrap();
    let audio = ebook.matches[0]
        .entry
        .pronunciations
        .items
        .iter()
        .find_map(|pronunciation| pronunciation.audio.as_deref())
        .unwrap();
    assert_eq!(audio, "LL-Q1860 (eng)-Vealhurl-e-book.wav");
}

#[test]
fn japanese_fixture_covers_kana_kanji_forms_and_lemma_links() {
    let (_temporary, pack) = build_fixture("jawiktionary-20260901", "ja");

    let cat = pack.lookup("猫", None, LookupOptions::default()).unwrap();
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

    let kanji = pack.lookup("走る", None, LookupOptions::default()).unwrap();
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

    let verb = pack
        .lookup("はしる", None, LookupOptions::default())
        .unwrap();
    let entry = pack
        .entry(
            &verb.matches[0].entry.reference,
            None,
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

    let simplified = pack
        .lookup("电子书", None, LookupOptions::default())
        .unwrap();
    assert_eq!(simplified.matches[0].entry.headword, "電子書");
    assert!(
        simplified.matches[0]
            .entry
            .pronunciations
            .items
            .iter()
            .any(|pronunciation| pronunciation.zh_pronunciation.is_some())
    );

    let cat = pack.lookup("貓", None, LookupOptions::default()).unwrap();
    let summary = &cat.matches[0].entry;
    assert_eq!(summary.pronunciations.total, 138);
    assert_eq!(summary.pronunciations.items.len(), 8);
    let entry = pack
        .entry(&summary.reference, None, ProjectionOptions::detail())
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
            None,
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

    let variant = pack.lookup("猫", None, LookupOptions::default()).unwrap();
    assert_eq!(variant.matches[0].entry.headword, "猫");

    let ladder = pack.lookup("梯子", None, LookupOptions::default()).unwrap();
    let ladder = pack
        .entry(
            &ladder.matches[0].entry.reference,
            None,
            ProjectionOptions::detail(),
        )
        .unwrap();
    assert!(ladder.descendants.total > 0);

    let view = DictionaryView::new("zh", pack, None).unwrap();
    assert!(matches!(
        view.lookup("不存在的詞", LookupOptions::default()),
        ViewOutcome::NoEntry
    ));
}
