use std::io::Cursor;
use std::sync::Arc;

use elephant_ladder_dictionary_pack::{
    BilingualView, DictionaryPack, DictionarySnapshot, DictionaryView, EmphasisRange,
    EntryReference, LookupOptions, MonolingualView, PackError, PackLimits, ProjectionOptions,
    RelationKind, Ruby, Section, SectionPage, ViewOutcome,
};
use elephant_ladder_dictionary_pack_builder::{BuildManifest, BuildOptions, Sha256Hex, build_pack};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn digest(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from_bytes(Sha256::digest(bytes).into())
}

fn build(language: &str, source: &str) -> (TempDir, Arc<DictionaryPack>) {
    let source_manifest = json!({"provider":"projection-fixture"});
    let license_manifest = json!({"license":"fixture"});
    let manifest = BuildManifest {
        pack_id: format!("wiktionary-{language}-{language}"),
        corpus_language: language.to_owned(),
        wiktionary_edition: language.to_owned(),
        wiktionary_dump_date: "2026-09-02".to_owned(),
        kaikki_extraction_date: "2026-09-06".to_owned(),
        source_url: "https://example.invalid/source.jsonl.gz".to_owned(),
        compressed_source_sha256: digest(b"compressed fixture"),
        uncompressed_source_sha256: digest(source.as_bytes()),
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
    let output = temporary.path().join("pack");
    build_pack(
        &manifest,
        Cursor::new(source.as_bytes()),
        &output,
        BuildOptions::default(),
    )
    .unwrap();
    let pack = Arc::new(DictionaryPack::open(output, PackLimits::default()).unwrap());
    (temporary, pack)
}

fn english_record() -> String {
    let record = json!({
        "word": "run",
        "lang": "English",
        "lang_code": "en",
        "pos": "verb",
        "etymology_number": 2,
        "etymology_text": "From Middle English rinnen.",
        "wikipedia": ["Running"],
        "sounds": [
            {"ipa": "/ɹʌn/", "tags": ["US"], "audio": "En-us-run.ogg", "topics": ["sports"]},
        ],
        "hyphenations": [{"parts": ["run"]}],
        "forms": [
            {"form": "runs", "tags": ["present", "singular", "third-person"]},
            {"form": "ran", "tags": ["past"], "ipa": "/ɹæn/"},
            {"form": "running", "tags": ["participle", "present"]},
        ],
        "senses": [
            {
                "glosses": ["To move swiftly."],
                "senseid": ["en:move swiftly"],
                "qualifier": "intransitive",
                "examples": [
                    {"text": "I run daily.", "bold_text_offsets": [[2, 5]],
                     "type": "example", "translation": "Je cours."},
                    {"text": "They ran home.", "english": "Ils sont rentrés."},
                ],
                "synonyms": [{"word": "sprint", "tags": ["informal"]}],
                "translations": [
                    {"word": "courir", "lang": "French", "lang_code": "fr"},
                    {"word": "correr", "lang": "Spanish", "lang_code": "es"},
                ],
            },
            {
                "glosses": ["To manage."],
                "form_of": [{"word": "operate", "extra": "a business"}],
            },
        ],
        "translations": [
            {"word": "rennen", "lang_code": "de", "sense": "move swiftly"},
        ],
        "derived": [{"word": "runner"}, {"word": "rerun"}, {"word": "outrun"}],
        "descendants": [
            {"word": "ranu", "lang_code": "xx", "descendants": [
                {"word": "ranuk", "lang_code": "yy"},
            ]},
            {"word": "rɛn", "lang_code": "zz"},
        ],
        "categories": ["English verbs", "English irregular verbs"],
        "future_field": {"retained": true},
    });
    format!("{record}\n")
}

fn japanese_record() -> String {
    let record = json!({
        "word": "走る",
        "lang": "日本語",
        "lang_code": "ja",
        "pos": "verb",
        "pos_title": "動詞",
        "etymology_texts": ["和語。", "古語「はしる」。"],
        "forms": [
            {"form": "はしる", "tags": ["hiragana"], "roman": "hashiru",
             "ruby": [["走", "はし"]]},
        ],
        "senses": [
            {
                "glosses": ["速く移動する。"],
                "sense_index": "1",
                "notes": ["口語"],
                "examples": [
                    {"text": "駅まで走る。", "ruby": [["駅", "えき"], ["走", "はし"]],
                     "bold_text_offsets": [[3, 5]]},
                ],
            },
        ],
        "translations": [
            {"word": "run", "lang_code": "en", "sense_index": 1},
        ],
        "cognates": [{"word": "주행", "roman": "juhaeng"}],
    });
    format!("{record}\n")
}

#[test]
fn detail_projection_exposes_every_known_english_section() {
    let (_temporary, pack) = build("en", &english_record());
    pack.validate_all().unwrap();
    let summary = pack.lookup("run", LookupOptions::default()).unwrap();
    let summary = &summary.matches[0].entry;
    assert_eq!(summary.forms.total, 3);
    assert!(summary.forms.items.is_empty());
    assert_eq!(summary.senses.items[0].examples.total, 2);
    assert!(summary.senses.items[0].examples.items.is_empty());
    assert_eq!(summary.relations.len(), 1);
    assert!(summary.relations[0].relations.items.is_empty());
    assert_eq!(summary.descendants.total, 3);
    assert_eq!(summary.categories.total, 2);
    assert_eq!(summary.etymology.total, 1);

    let entry = pack
        .entry(&summary.reference, ProjectionOptions::detail())
        .unwrap();
    assert_eq!(entry.etymology_number.as_deref(), Some("2"));
    assert_eq!(entry.etymology.items, ["From Middle English rinnen."]);
    assert_eq!(entry.wikipedia, ["Running"]);
    let pronunciation = &entry.pronunciations.items[0];
    assert_eq!(pronunciation.topics, ["sports"]);
    assert_eq!(
        pronunciation.audio.as_ref().unwrap().file_name.as_deref(),
        Some("En-us-run.ogg")
    );
    assert_eq!(entry.hyphenations.items[0].parts, ["run"]);
    assert_eq!(entry.forms.items[1].form, "ran");
    assert_eq!(entry.forms.items[1].ipa, ["/ɹæn/"]);

    let sense = &entry.senses.items[0];
    assert_eq!(sense.qualifier.as_deref(), Some("intransitive"));
    assert_eq!(sense.sense_ids, ["en:move swiftly"]);
    assert_eq!(
        sense.examples.items[0].text_emphasis,
        [EmphasisRange { start: 2, end: 5 }]
    );
    assert_eq!(
        sense.examples.items[0].example_type.as_deref(),
        Some("example")
    );
    assert_eq!(
        sense.examples.items[1].translation.as_deref(),
        Some("Ils sont rentrés.")
    );
    assert_eq!(sense.relations[0].kind, RelationKind::Synonyms);
    assert_eq!(
        sense.relations[0].relations.items[0].word.as_deref(),
        Some("sprint")
    );
    assert_eq!(sense.translations.total, 2);
    assert_eq!(entry.sense_translation_total, 2);
    assert_eq!(entry.senses.items[1].form_of[0].word, "operate");
    assert_eq!(
        entry.senses.items[1].form_of[0].extra.as_deref(),
        Some("a business")
    );

    assert_eq!(entry.relations[0].kind, RelationKind::Derived);
    assert_eq!(entry.relations[0].relations.total, 3);
    assert_eq!(
        entry
            .descendants
            .items
            .iter()
            .map(|descendant| (descendant.depth, descendant.word.as_deref()))
            .collect::<Vec<_>>(),
        [(0, Some("ranu")), (1, Some("ranuk")), (0, Some("rɛn"))]
    );
    assert_eq!(entry.categories.items.len(), 2);
}

#[test]
fn section_pages_window_lists_by_stable_offset() {
    let (_temporary, pack) = build("en", &english_record());
    let reference = pack
        .lookup("run", LookupOptions::default())
        .unwrap()
        .matches[0]
        .entry
        .reference
        .clone();
    let options = ProjectionOptions::detail();

    let SectionPage::Forms(forms) = pack
        .section_page(&reference, Section::Forms, 1, 1, options)
        .unwrap()
    else {
        panic!("expected forms");
    };
    assert_eq!((forms.offset, forms.total), (1, 3));
    assert_eq!(forms.items[0].form, "ran");
    assert!(forms.is_partial());

    let SectionPage::Relations(derived) = pack
        .section_page(
            &reference,
            Section::Relations(RelationKind::Derived),
            2,
            10,
            options,
        )
        .unwrap()
    else {
        panic!("expected relations");
    };
    assert_eq!(derived.items[0].word.as_deref(), Some("outrun"));

    let SectionPage::Descendants(descendants) = pack
        .section_page(&reference, Section::Descendants, 1, 1, options)
        .unwrap()
    else {
        panic!("expected descendants");
    };
    assert_eq!(descendants.total, 3);
    assert_eq!(
        (
            descendants.items[0].depth,
            descendants.items[0].word.as_deref()
        ),
        (1, Some("ranuk"))
    );

    let SectionPage::Examples(examples) = pack
        .section_page(
            &reference,
            Section::SenseExamples { sense: 0 },
            1,
            5,
            options,
        )
        .unwrap()
    else {
        panic!("expected examples");
    };
    assert_eq!(examples.items[0].text.as_deref(), Some("They ran home."));
    assert_eq!(
        pack.section_page(
            &reference,
            Section::SenseExamples { sense: 9 },
            0,
            5,
            options
        )
        .unwrap(),
        SectionPage::NoSuchSense
    );

    let SectionPage::Etymology(beyond) = pack
        .section_page(&reference, Section::Etymology, 5, 5, options)
        .unwrap()
    else {
        panic!("expected etymology");
    };
    assert_eq!((beyond.total, beyond.items.len()), (1, 0));

    assert!(matches!(
        pack.section_page(&reference, Section::Forms, 0, 0, options),
        Err(PackError::Limit(_))
    ));
    assert!(matches!(
        pack.section_page(&reference, Section::Forms, 0, 257, options),
        Err(PackError::Limit(_))
    ));
}

#[test]
fn entry_reads_reject_references_from_other_packs_or_positions() {
    let (_first_temporary, first) = build("en", &english_record());
    let (_second_temporary, second) = build("ja", &japanese_record());
    let reference = first
        .lookup("run", LookupOptions::default())
        .unwrap()
        .matches[0]
        .entry
        .reference
        .clone();
    assert!(matches!(
        second.entry(&reference, ProjectionOptions::detail()),
        Err(PackError::UnknownEntry)
    ));
    let moved = EntryReference {
        selected_ordinal: 7,
        ..reference.clone()
    };
    assert!(matches!(
        first.entry(&moved, ProjectionOptions::detail()),
        Err(PackError::UnknownEntry)
    ));
    let unknown = EntryReference {
        entry_id: elephant_ladder_dictionary_pack::EntryId::from_bytes([9; 32]),
        ..reference
    };
    assert!(matches!(
        first.entry(&unknown, ProjectionOptions::detail()),
        Err(PackError::UnknownEntry)
    ));
}

#[test]
fn bilingual_entry_reads_filter_translations_to_the_target() {
    let (_temporary, pack) = build("en", &english_record());
    let view = BilingualView::new("english-french", pack.clone(), "fr").unwrap();
    let reference = pack
        .lookup("run", LookupOptions::default())
        .unwrap()
        .matches[0]
        .entry
        .reference
        .clone();
    let entry = view.entry(&reference, ProjectionOptions::detail()).unwrap();
    assert_eq!(entry.translations.total, 0);
    assert_eq!(entry.sense_translation_total, 1);
    assert_eq!(
        entry.senses.items[0].translations.items[0].word.as_deref(),
        Some("courir")
    );
    let SectionPage::Translations(page) = view
        .section_page(
            &reference,
            Section::SenseTranslations { sense: 0 },
            0,
            10,
            ProjectionOptions::detail(),
        )
        .unwrap()
    else {
        panic!("expected translations");
    };
    assert_eq!(page.total, 1);
}

#[test]
fn japanese_shapes_keep_ruby_text_indexes_and_etymology_lists() {
    let (_temporary, pack) = build("ja", &japanese_record());
    pack.validate_all().unwrap();
    let reference = pack
        .lookup("はしる", LookupOptions::default())
        .unwrap()
        .matches[0]
        .entry
        .reference
        .clone();
    let entry = pack.entry(&reference, ProjectionOptions::detail()).unwrap();
    assert_eq!(entry.part_of_speech_title.as_deref(), Some("動詞"));
    assert_eq!(entry.etymology.total, 2);
    assert_eq!(
        entry.forms.items[0].romanization.as_deref(),
        Some("hashiru")
    );
    assert_eq!(
        entry.forms.items[0].ruby,
        [Ruby {
            base: "走".to_owned(),
            text: "はし".to_owned()
        }]
    );
    let sense = &entry.senses.items[0];
    assert_eq!(sense.sense_index.as_deref(), Some("1"));
    assert_eq!(sense.notes, ["口語"]);
    assert_eq!(sense.examples.items[0].ruby.len(), 2);
    assert_eq!(
        entry.translations.items[0].sense_index.as_deref(),
        Some("1")
    );
    assert_eq!(entry.relations[0].kind, RelationKind::Cognates);
    assert_eq!(
        entry.relations[0].relations.items[0]
            .romanization
            .as_deref(),
        Some("juhaeng")
    );
}

#[test]
fn projection_rejects_malformed_extended_shapes() {
    for (index, malformed) in [
        "\"etymology_text\":[]",
        "\"etymology_texts\":\"text\"",
        "\"etymology_number\":-1",
        "\"forms\":[{\"form\":\"x\",\"ruby\":[[\"only\"]]}]",
        "\"senses\":[{\"examples\":[{\"bold_text_offsets\":[[5,2]]}]}]",
        "\"senses\":[{\"form_of\":[{\"extra\":\"no word\"}]}]",
        "\"senses\":[{\"sense_index\":true}]",
        "\"derived\":[1]",
        "\"descendants\":[{\"descendants\":{}}]",
        "\"sounds\":[{\"not_same_pronunciation\":\"yes\"}]",
    ]
    .into_iter()
    .enumerate()
    {
        let word = format!("malformed{index}");
        let source = format!("{{\"lang_code\":\"en\",\"word\":\"{word}\",{malformed}}}\n");
        let (_temporary, pack) = build("en", &source);
        assert!(
            matches!(pack.validate_all(), Err(PackError::Corrupt(_))),
            "accepted {malformed}"
        );
    }
}

#[test]
fn snapshot_lookup_orders_views_by_selection_language_and_isolates_failures() {
    let (_english_temporary, english) = build("en", &english_record());
    let (_japanese_temporary, japanese) = build("ja", &japanese_record());
    let snapshot = DictionarySnapshot::new(vec![
        DictionaryView::Monolingual(MonolingualView::new("en", english.clone()).unwrap()),
        DictionaryView::Bilingual(BilingualView::new("en-fr", english.clone(), "fr").unwrap()),
        DictionaryView::Bilingual(BilingualView::new("en-it", english, "it").unwrap()),
        DictionaryView::Monolingual(MonolingualView::new("ja", japanese).unwrap()),
    ])
    .unwrap();

    let order = |language| {
        snapshot
            .lookup("run", language, LookupOptions::default())
            .iter()
            .map(|lookup| lookup.view_id.to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(order(None), ["en", "en-fr", "en-it", "ja"]);
    assert_eq!(order(Some("ja-Jpan")), ["ja", "en", "en-fr", "en-it"]);
    assert_eq!(order(Some("EN_gb")), ["en", "en-fr", "en-it", "ja"]);
    assert_eq!(order(Some("")), ["en", "en-fr", "en-it", "ja"]);

    let outcomes = snapshot.lookup("run", None, LookupOptions::default());
    assert!(matches!(outcomes[0].outcome, ViewOutcome::Completed(_)));
    assert!(matches!(outcomes[1].outcome, ViewOutcome::Completed(_)));
    assert!(matches!(outcomes[2].outcome, ViewOutcome::NoTranslation(_)));
    assert!(matches!(outcomes[3].outcome, ViewOutcome::NoEntry));

    let too_many = LookupOptions {
        limit: 1000,
        ..LookupOptions::default()
    };
    assert!(
        snapshot
            .lookup("run", None, too_many)
            .iter()
            .all(|lookup| matches!(lookup.outcome, ViewOutcome::Failed(PackError::Limit(_))))
    );

    let reference = match &outcomes[1].outcome {
        ViewOutcome::Completed(outcome) => outcome.matches[0].entry.reference.clone(),
        _ => unreachable!(),
    };
    let entry = snapshot
        .view("en-fr")
        .unwrap()
        .entry(&reference, ProjectionOptions::detail())
        .unwrap();
    assert_eq!(entry.sense_translation_total, 1);
    assert!(matches!(
        snapshot
            .view("ja")
            .unwrap()
            .entry(&reference, ProjectionOptions::detail()),
        Err(PackError::UnknownEntry)
    ));
}
