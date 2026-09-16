use std::fs;
use std::io::Cursor;
use std::process::Command;
use std::sync::Arc;

use elephant_ladder_dictionary_pack::{
    BilingualLookupOutcome, BilingualView, DictionaryPack, DictionaryService, DictionarySnapshot,
    DictionaryView, ExpectedPack, LookupOptions, MatchClass, MonolingualView, PACK_MANIFEST_FILE,
    PackError, PackLimits, ProjectionOptions,
};
use elephant_ladder_dictionary_pack_builder::{BuildManifest, BuildOptions, Sha256Hex, build_pack};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn digest(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from_bytes(Sha256::digest(bytes).into())
}

fn build(source: &[u8], shard_target: u64) -> (TempDir, Arc<DictionaryPack>) {
    let source_manifest = json!({"provider":"runtime-fixture"});
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
        target_shard_payload_bytes: shard_target,
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
        Cursor::new(source),
        &output,
        BuildOptions::default(),
    )
    .unwrap();
    let pack = Arc::new(DictionaryPack::open(output, PackLimits::default()).unwrap());
    (temporary, pack)
}

#[test]
fn exact_lookup_stops_at_first_stage_and_preserves_order_and_truncation() {
    let source = concat!(
        "{\"lang_code\":\"en\",\"word\":\"Straße\"}\n",
        "{\"lang_code\":\"en\",\"word\":\"strasse\"}\n",
        "{\"lang_code\":\"en\",\"word\":\"strasse\"}\n",
    );
    let (_temporary, pack) = build(source.as_bytes(), 1);

    let exact = pack
        .lookup(
            "strasse",
            LookupOptions {
                limit: 1,
                ..LookupOptions::default()
            },
        )
        .unwrap();
    assert_eq!(exact.matches.len(), 1);
    assert!(exact.truncated);
    assert_eq!(exact.matches[0].entry.headword, "strasse");
    assert_eq!(exact.matches[0].match_class, MatchClass::AuthoredHeadword);
    assert_eq!(exact.matches[0].entry.reference.selected_ordinal, 1);

    let folded = pack
        .lookup(
            "STRASSE",
            LookupOptions {
                limit: 10,
                ..LookupOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        folded
            .matches
            .iter()
            .map(|matched| matched.entry.reference.selected_ordinal)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert!(
        folded
            .matches
            .iter()
            .all(|matched| matched.match_class == MatchClass::FoldedHeadword)
    );
}

#[test]
fn form_lookup_routes_across_shards_and_returns_typed_semantics() {
    let source = concat!(
        "{\"lang_code\":\"en\",\"word\":\"one\"}\n",
        "{\"lang_code\":\"en\",\"word\":\"run\",\"forms\":[{\"form\":\"Ran\"}],",
        "\"sounds\":[{\"ipa\":\"ɹʌn\"}],\"senses\":[{\"glosses\":[\"move quickly\"],",
        "\"translations\":[{\"word\":\"courir\",\"lang_code\":\"fr\"}]}]}\n",
    );
    let (_temporary, pack) = build(source.as_bytes(), 1);
    let outcome = pack.lookup("ran", LookupOptions::default()).unwrap();
    let entry = &outcome.matches[0].entry;
    assert_eq!(outcome.matches[0].match_class, MatchClass::FoldedForm);
    assert_eq!(entry.headword, "run");
    assert_eq!(entry.pronunciations.items[0].ipa.as_deref(), Some("ɹʌn"));
    assert_eq!(entry.senses.items[0].glosses, ["move quickly"]);
    assert_eq!(
        entry.senses.items[0].translations.items[0].word.as_deref(),
        Some("courir")
    );
}

#[test]
fn exhaustive_validation_reports_authenticated_totals() {
    let source = concat!(
        "{\"lang_code\":\"en\",\"word\":\"one\"}\n",
        "{\"lang_code\":\"en\",\"word\":\"run\",\"forms\":[{\"form\":\"Ran\"},{\"form\":\"ran\"}]}\n",
    );
    let (_temporary, pack) = build(source.as_bytes(), 1);

    let report = pack.validate_all().unwrap();
    assert_eq!(report.authenticated_record_count, 2);
    assert_eq!(report.uncompressed_record_bytes, source.len() as u64 - 2);
    assert_eq!(report.lookup_key_row_count, 11);

    let output = Command::new(env!("CARGO_BIN_EXE_elephant-dictionary-pack"))
        .args(["validate", "--pack"])
        .arg(pack.directory())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "authenticated_record_count=2\nuncompressed_record_bytes={}\nlookup_key_row_count=11\n",
            source.len() - 2
        )
    );
}

#[test]
fn bilingual_views_distinguish_missing_entries_and_translations() {
    let source = concat!(
        "{\"lang_code\":\"en\",\"word\":\"hello\",\"translations\":[",
        "{\"word\":\"salut\",\"lang_code\":\"fr\"},",
        "{\"word\":\"hallo\",\"lang_code\":\"de\"}],\"senses\":[{\"translations\":[",
        "{\"word\":\"hallo\",\"lang_code\":\"de\"}]},{\"translations\":[",
        "{\"word\":\"bonjour\",\"lang_code\":\"fr\"}]}]}\n",
        "{\"lang_code\":\"en\",\"word\":\"alone\",\"senses\":[{\"glosses\":[\"solo\"]}]}\n",
    );
    let (_temporary, pack) = build(source.as_bytes(), 1_000_000);
    let view = BilingualView::new("english-french", pack, "fr").unwrap();
    let options = LookupOptions {
        projection: ProjectionOptions {
            senses: 1,
            ..ProjectionOptions::summary()
        },
        ..LookupOptions::default()
    };

    let BilingualLookupOutcome::Translations(translated) = view.lookup("hello", options).unwrap()
    else {
        panic!("expected target translation");
    };
    let entry = &translated.matches[0].entry;
    assert_eq!(entry.translations.items[0].word.as_deref(), Some("salut"));
    assert_eq!(entry.translations.total, 1);
    assert_eq!(entry.senses.total, 2);
    assert_eq!(entry.senses.items.len(), 1);
    assert_eq!(entry.senses.items[0].translations.total, 0);
    assert_eq!(entry.sense_translation_total, 1);
    assert!(matches!(
        view.lookup("alone", LookupOptions::default()).unwrap(),
        BilingualLookupOutcome::NoTranslation(_)
    ));
    assert_eq!(
        view.lookup("absent", LookupOptions::default()).unwrap(),
        BilingualLookupOutcome::NoEntry
    );
}

#[test]
fn translation_and_pronunciation_metadata_is_retained() {
    let source = concat!(
        "{\"lang_code\":\"en\",\"word\":\"metadata\",\"translations\":[{",
        "\"lang\":\"French\",\"code\":\"fr\",\"alt\":\"alternative\",",
        "\"english\":\"clarification\",\"note\":\"note only\",\"sense\":\"meaning\",",
        "\"taxonomic\":\"Taxon species\",\"roman\":\"romanized\",",
        "\"tags\":[\"formal\"],\"raw_tags\":[\"raw formal\"]}],",
        "\"sounds\":[{\"ipa\":\"/meta/\",\"enpr\":\"mĕt′ə\",",
        "\"zh-pron\":\"zh fact\",\"hangeul\":\"한글\",\"homophone\":\"metta\",",
        "\"roman\":\"roman sound\",\"form\":\"sound form\",",
        "\"rhymes\":\"-eta\",\"text\":\"Audio (US)\",\"audio\":\"Metadata.ogg\",",
        "\"note\":\"pronunciation note\",\"other\":\"other notation\",",
        "\"audio-ipa\":\"[meta]\",\"wav_url\":\"https://example.invalid/a.wav\",",
        "\"ogg_url\":\"https://example.invalid/a.ogg\",",
        "\"oga_url\":\"https://example.invalid/a.oga\",",
        "\"mp3_url\":\"https://example.invalid/a.mp3\",",
        "\"opus_url\":\"https://example.invalid/a.opus\",",
        "\"flac_url\":\"https://example.invalid/a.flac\",",
        "\"homophones\":[\"meta\"],\"hyphenation\":[\"met-a\"],",
        "\"tags\":[\"US\"],\"raw_tags\":[\"General American\"],",
        "\"future_field\":{\"ignored\":true}}]}\n",
    );
    let (_temporary, pack) = build(source.as_bytes(), 1_000_000);
    let outcome = pack.lookup("metadata", LookupOptions::default()).unwrap();
    let entry = &outcome.matches[0].entry;
    let translation = &entry.translations.items[0];
    assert_eq!(translation.word, None);
    assert_eq!(translation.language_name.as_deref(), Some("French"));
    assert_eq!(translation.language_code.as_deref(), Some("fr"));
    assert_eq!(translation.alt.as_deref(), Some("alternative"));
    assert_eq!(translation.translation.as_deref(), Some("clarification"));
    assert_eq!(translation.note.as_deref(), Some("note only"));
    assert_eq!(translation.sense.as_deref(), Some("meaning"));
    assert_eq!(translation.taxonomic.as_deref(), Some("Taxon species"));
    assert_eq!(translation.romanization.as_deref(), Some("romanized"));
    assert_eq!(translation.tags, ["formal"]);
    assert_eq!(translation.raw_tags, ["raw formal"]);

    let pronunciation = &entry.pronunciations.items[0];
    assert_eq!(pronunciation.ipa.as_deref(), Some("/meta/"));
    assert_eq!(pronunciation.enpr.as_deref(), Some("mĕt′ə"));
    assert_eq!(pronunciation.zh_pronunciation.as_deref(), Some("zh fact"));
    assert_eq!(pronunciation.hangeul.as_deref(), Some("한글"));
    assert_eq!(pronunciation.homophone.as_deref(), Some("metta"));
    assert_eq!(pronunciation.romanization.as_deref(), Some("roman sound"));
    assert_eq!(pronunciation.form.as_deref(), Some("sound form"));
    assert_eq!(pronunciation.rhymes.as_deref(), Some("-eta"));
    assert_eq!(pronunciation.text.as_deref(), Some("Audio (US)"));
    assert_eq!(pronunciation.note.as_deref(), Some("pronunciation note"));
    assert_eq!(pronunciation.other.as_deref(), Some("other notation"));
    assert_eq!(
        pronunciation
            .audio
            .as_ref()
            .and_then(|audio| audio.file_name.as_deref()),
        Some("Metadata.ogg")
    );
    assert_eq!(pronunciation.audio_ipa.as_deref(), Some("[meta]"));
    assert_eq!(
        pronunciation.wav_url.as_deref(),
        Some("https://example.invalid/a.wav")
    );
    assert_eq!(
        pronunciation.ogg_url.as_deref(),
        Some("https://example.invalid/a.ogg")
    );
    assert_eq!(
        pronunciation.oga_url.as_deref(),
        Some("https://example.invalid/a.oga")
    );
    assert_eq!(
        pronunciation.mp3_url.as_deref(),
        Some("https://example.invalid/a.mp3")
    );
    assert_eq!(
        pronunciation.opus_url.as_deref(),
        Some("https://example.invalid/a.opus")
    );
    assert_eq!(
        pronunciation.flac_url.as_deref(),
        Some("https://example.invalid/a.flac")
    );
    assert_eq!(pronunciation.homophones, ["meta"]);
    assert_eq!(pronunciation.hyphenation, ["met-a"]);
    assert_eq!(pronunciation.tags, ["US"]);
    assert_eq!(pronunciation.raw_tags, ["General American"]);
}

#[test]
fn lookup_rejects_malformed_known_semantic_shapes() {
    let malformed_fields = [
        "\"lang\":[]",
        "\"tags\":{}",
        "\"tags\":[1]",
        "\"sounds\":{}",
        "\"sounds\":[1]",
        "\"sounds\":[{\"ipa\":[]}]",
        "\"sounds\":[{\"zh-pron\":1}]",
        "\"sounds\":[{\"homophones\":\"not-an-array\"}]",
        "\"senses\":{}",
        "\"senses\":[1]",
        "\"senses\":[{\"glosses\":\"not-an-array\"}]",
        "\"senses\":[{\"raw_glosses\":[false]}]",
        "\"translations\":{}",
        "\"translations\":[1]",
        "\"translations\":[{\"word\":1,\"note\":\"context\"}]",
        "\"translations\":[{\"word\":\"mot\",\"tags\":[null]}]",
        "\"translations\":[{\"word\":\"mot\",\"lang_code\":\"fr\",\"code\":1}]",
        "\"senses\":[{\"translations\":[{\"word\":\"mot\",\"raw_tags\":1}]}]",
    ];
    for (index, malformed) in malformed_fields.into_iter().enumerate() {
        let source =
            format!("{{\"lang_code\":\"en\",\"word\":\"malformed{index}\",{malformed}}}\n");
        let (_temporary, pack) = build(source.as_bytes(), 1_000_000);
        let error = pack
            .lookup(&format!("malformed{index}"), LookupOptions::default())
            .unwrap_err();
        let PackError::Corrupt(message) = error else {
            panic!("expected corrupt semantic shape, got {error:?}");
        };
        assert!(message.contains("record"), "unhelpful context: {message}");
    }
}

#[test]
fn exhaustive_validation_rejects_malformed_semantics_accepted_at_admission() {
    let source =
        b"{\"lang_code\":\"en\",\"word\":\"malformed\",\"senses\":[{\"glosses\":false}]}\n";
    let (_temporary, pack) = build(source, 1_000_000);

    let error = pack.validate_all().unwrap_err();
    assert!(matches!(
        error,
        PackError::Corrupt(message)
            if message.contains("record 0 semantic validation failed")
                && message.contains("record `senses[0]` `glosses`")
    ));
}

#[test]
fn semantic_projection_reports_every_kind_of_bounded_loss() {
    let source = concat!(
        "{\"lang_code\":\"en\",\"word\":\"lengthy\",\"lang\":\"English\",",
        "\"pos\":\"noun-long\",\"tags\":[\"first-long\",\"second\"],",
        "\"sounds\":[{\"ipa\":\"12345\",\"homophones\":[\"abcde\",\"second\"],",
        "\"hyphenation\":[\"abcde\",\"second\"],\"tags\":[\"abcde\",\"second\"],",
        "\"raw_tags\":[\"abcde\",\"second\"]},{\"ipa\":\"other\"}],",
        "\"translations\":[{\"word\":\"12345\",\"lang_code\":\"francais\",",
        "\"tags\":[\"abcde\",\"second\"],\"raw_tags\":[\"abcde\",\"second\"]},",
        "{\"word\":\"other\",\"lang_code\":\"fr\"}],",
        "\"senses\":[{\"glosses\":[\"abcde\",\"second\"],",
        "\"raw_glosses\":[\"abcde\",\"second\"],\"tags\":[\"abcde\",\"second\"],",
        "\"translations\":[{\"word\":\"abcde\"},{\"word\":\"second\"}]},",
        "{\"glosses\":[\"omitted\"],\"translations\":[{\"word\":\"third\"}]}]}\n",
    );
    let (_temporary, pack) = build(source.as_bytes(), 1_000_000);
    let options = LookupOptions {
        projection: ProjectionOptions {
            text_bytes: 4,
            strings: 1,
            pronunciations: 1,
            senses: 1,
            translations: 1,
            ..ProjectionOptions::summary()
        },
        ..LookupOptions::default()
    };
    let outcome = pack.lookup("lengthy", options).unwrap();
    let entry = &outcome.matches[0].entry;
    assert_eq!(entry.headword, "leng");
    assert_eq!(entry.language_code, "en");
    assert!(entry.truncated);
    assert_eq!(entry.tags, ["firs"]);

    assert_eq!(
        (entry.pronunciations.total, entry.pronunciations.items.len()),
        (2, 1)
    );
    assert!(entry.pronunciations.is_partial());
    let pronunciation = &entry.pronunciations.items[0];
    assert!(pronunciation.truncated);
    assert_eq!(pronunciation.ipa.as_deref(), Some("1234"));
    assert_eq!(pronunciation.homophones, ["abcd"]);
    assert_eq!(pronunciation.hyphenation, ["abcd"]);
    assert_eq!(pronunciation.tags, ["abcd"]);
    assert_eq!(pronunciation.raw_tags, ["abcd"]);

    assert_eq!(
        (entry.translations.total, entry.translations.items.len()),
        (2, 1)
    );
    let translation = &entry.translations.items[0];
    assert!(translation.truncated);
    assert_eq!(translation.word.as_deref(), Some("1234"));
    assert_eq!(translation.tags, ["abcd"]);
    assert_eq!(translation.raw_tags, ["abcd"]);

    assert_eq!((entry.senses.total, entry.senses.items.len()), (2, 1));
    assert_eq!(entry.sense_translation_total, 3);
    let sense = &entry.senses.items[0];
    assert!(sense.truncated);
    assert_eq!(sense.glosses, ["abcd"]);
    assert_eq!(sense.raw_glosses, ["abcd"]);
    assert_eq!(sense.tags, ["abcd"]);
    assert_eq!(
        (sense.translations.total, sense.translations.items.len()),
        (2, 1)
    );
    assert!(sense.translations.items[0].truncated);
    assert_eq!(sense.translations.items[0].word.as_deref(), Some("abcd"));
}

#[test]
fn snapshot_identity_is_ordered_and_existing_leases_survive_replacement() {
    let source = b"{\"lang_code\":\"en\",\"word\":\"one\"}\n";
    let (_temporary, pack) = build(source, 1_000_000);
    let mono = DictionaryView::Monolingual(MonolingualView::new("mono", pack.clone()).unwrap());
    let bilingual =
        DictionaryView::Bilingual(BilingualView::new("bilingual", pack.clone(), "fr").unwrap());
    let first = Arc::new(DictionarySnapshot::new(vec![mono.clone(), bilingual.clone()]).unwrap());
    let second = Arc::new(DictionarySnapshot::new(vec![bilingual, mono]).unwrap());
    assert_ne!(first.revision(), second.revision());

    let service = DictionaryService::new(first.clone());
    let lease = service.lease();
    let previous = service.replace(second.clone());
    assert!(Arc::ptr_eq(&previous, &first));
    assert!(Arc::ptr_eq(&lease, &first));
    assert!(Arc::ptr_eq(&service.lease(), &second));
}

#[test]
fn admission_rejects_unsafe_manifest_names_and_asset_tampering() {
    let source = b"{\"lang_code\":\"en\",\"word\":\"one\"}\n";
    let (temporary, pack) = build(source, 1_000_000);
    let directory = pack.directory().to_owned();
    drop(pack);

    let manifest_path = directory.join(PACK_MANIFEST_FILE);
    let original = fs::read(&manifest_path).unwrap();
    let mut manifest: Value = serde_json::from_slice(&original).unwrap();
    manifest["assets"][0]["file_name"] = Value::String("../index.eldict".to_owned());
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(matches!(
        DictionaryPack::open(&directory, PackLimits::default()),
        Err(PackError::Malformed(_))
    ));

    fs::write(&manifest_path, original).unwrap();
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let data_name = manifest["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|asset| asset["role"] == "data")
        .unwrap()["file_name"]
        .as_str()
        .unwrap();
    let data_path = directory.join(data_name);
    let mut bytes = fs::read(&data_path).unwrap();
    bytes[0] ^= 1;
    fs::write(data_path, bytes).unwrap();
    assert!(matches!(
        DictionaryPack::open(&directory, PackLimits::default()),
        Err(PackError::Corrupt(_))
    ));
    drop(temporary);
}

#[test]
fn admission_reauthenticates_stored_manifest_json() {
    let source = b"{\"lang_code\":\"en\",\"word\":\"one\"}\n";
    for (column, tampered_json, expected_message) in [
        (
            "source_manifest_json",
            "{\"provider\":\"tampered\"}",
            "source manifest SHA-256 mismatch",
        ),
        (
            "license_manifest_json",
            "{\"license\":\"tampered\"}",
            "license manifest SHA-256 mismatch",
        ),
    ] {
        let (_temporary, pack) = build(source, 1_000_000);
        let directory = pack.directory().to_owned();
        drop(pack);

        let manifest_path = directory.join(PACK_MANIFEST_FILE);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let index_name = manifest["assets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|asset| asset["role"] == "index")
            .unwrap()["file_name"]
            .as_str()
            .unwrap()
            .to_owned();
        let index_path = directory.join(&index_name);
        let connection = rusqlite::Connection::open(&index_path).unwrap();
        connection
            .execute(
                &format!("UPDATE pack_metadata SET {column} = ?1 WHERE singleton = 1"),
                [tampered_json],
            )
            .unwrap();
        connection.close().unwrap();

        let index_bytes = fs::read(&index_path).unwrap();
        let index_asset = manifest["assets"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|asset| asset["file_name"] == index_name)
            .unwrap();
        index_asset["size_bytes"] = Value::from(index_bytes.len() as u64);
        index_asset["sha256"] = Value::String(hex::encode(Sha256::digest(&index_bytes)));
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

        assert!(matches!(
            DictionaryPack::open(&directory, PackLimits::default()),
            Err(PackError::Corrupt(message)) if message.contains(expected_message)
        ));
    }
}

#[test]
fn exhaustive_validation_rejects_a_missing_lookup_key_accepted_at_admission() {
    let source = b"{\"lang_code\":\"en\",\"word\":\"one\"}\n";
    let (temporary, pack) = build(source, 1_000_000);
    let directory = pack.directory().to_owned();
    drop(pack);

    let manifest_path = directory.join(PACK_MANIFEST_FILE);
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let index_name = manifest["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|asset| asset["role"] == "index")
        .unwrap()["file_name"]
        .as_str()
        .unwrap()
        .to_owned();
    let index_path = directory.join(&index_name);
    let connection = rusqlite::Connection::open(&index_path).unwrap();
    connection
        .execute(
            "DELETE FROM lookup_keys WHERE match_class = 2 AND selected_ordinal = 0",
            [],
        )
        .unwrap();
    connection.close().unwrap();

    let index_bytes = fs::read(&index_path).unwrap();
    let index_hash = hex::encode(Sha256::digest(&index_bytes));
    let index_asset = manifest["assets"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|asset| asset["file_name"] == index_name)
        .unwrap();
    index_asset["size_bytes"] = Value::from(index_bytes.len() as u64);
    index_asset["sha256"] = Value::String(index_hash);
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    let admitted = DictionaryPack::open(&directory, PackLimits::default()).unwrap();
    assert!(matches!(
        admitted.validate_all(),
        Err(PackError::Corrupt(message)) if message.contains("lookup keys disagree")
    ));
    drop(temporary);
}

#[test]
fn lookup_enforces_the_declared_decompression_bound() {
    let source = b"{\"lang_code\":\"en\",\"word\":\"bounded\",\"gloss\":\"payload\"}\n";
    let (_temporary, initial) = build(source, 1_000_000);
    let directory = initial.directory().to_owned();
    drop(initial);
    let pack = DictionaryPack::open(
        directory,
        PackLimits {
            max_uncompressed_record_bytes: 8,
            ..PackLimits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        pack.lookup("bounded", LookupOptions::default()),
        Err(PackError::Limit(_))
    ));
}

fn expected_pack(pack: &DictionaryPack) -> ExpectedPack {
    let manifest = fs::read(pack.directory().join(PACK_MANIFEST_FILE)).unwrap();
    ExpectedPack {
        pack_id: pack.pack_id().clone(),
        pack_revision: *pack.pack_revision(),
        manifest_sha256: digest(&manifest),
    }
}

fn data_asset_path(directory: &std::path::Path) -> std::path::PathBuf {
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join(PACK_MANIFEST_FILE)).unwrap()).unwrap();
    let name = manifest["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|asset| asset["role"] == "data")
        .unwrap()["file_name"]
        .as_str()
        .unwrap()
        .to_owned();
    directory.join(name)
}

#[test]
fn trusted_admission_requires_the_expected_manifest_and_identity() {
    let source = b"{\"lang_code\":\"en\",\"word\":\"one\"}\n";
    let (_temporary, pack) = build(source, 1_000_000);
    let directory = pack.directory().to_owned();
    let expected = expected_pack(&pack);
    drop(pack);

    let verified = DictionaryPack::verify(&directory, &expected, PackLimits::default()).unwrap();
    assert_eq!(
        verified
            .lookup("one", LookupOptions::default())
            .unwrap()
            .matches
            .len(),
        1
    );
    drop(verified);
    let installed =
        DictionaryPack::open_installed(&directory, &expected, PackLimits::default()).unwrap();
    assert_eq!(
        installed
            .lookup("one", LookupOptions::default())
            .unwrap()
            .matches
            .len(),
        1
    );
    drop(installed);

    let wrong_manifest = ExpectedPack {
        manifest_sha256: digest(b"another manifest"),
        ..expected.clone()
    };
    let wrong_revision = ExpectedPack {
        pack_revision: [7; 32].into(),
        ..expected.clone()
    };
    let wrong_pack = ExpectedPack {
        pack_id: "wiktionary-fr-fr".parse().unwrap(),
        ..expected
    };
    for expected in [wrong_manifest, wrong_revision, wrong_pack] {
        assert!(matches!(
            DictionaryPack::verify(&directory, &expected, PackLimits::default()),
            Err(PackError::Corrupt(_))
        ));
        assert!(matches!(
            DictionaryPack::open_installed(&directory, &expected, PackLimits::default()),
            Err(PackError::Corrupt(_))
        ));
    }
}

#[test]
fn installed_admission_skips_byte_hashing_but_served_records_stay_authenticated() {
    let source = b"{\"lang_code\":\"en\",\"word\":\"authenticated\"}\n";
    let (_temporary, pack) = build(source, 1_000_000);
    let directory = pack.directory().to_owned();
    let expected = expected_pack(&pack);
    drop(pack);

    let data_path = data_asset_path(&directory);
    let original_size = fs::metadata(&data_path).unwrap().len();
    let connection = rusqlite::Connection::open(&data_path).unwrap();
    let mut compressed: Vec<u8> = connection
        .query_row(
            "SELECT source_json_zstd FROM records WHERE selected_ordinal = 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let last = compressed.len() - 1;
    compressed[last] ^= 0x20;
    connection
        .execute(
            "UPDATE records SET source_json_zstd = ?1 WHERE selected_ordinal = 0",
            [compressed],
        )
        .unwrap();
    connection.close().unwrap();
    assert_eq!(fs::metadata(&data_path).unwrap().len(), original_size);

    assert!(matches!(
        DictionaryPack::verify(&directory, &expected, PackLimits::default()),
        Err(PackError::Corrupt(message)) if message.contains("SHA-256 mismatch")
    ));
    let installed =
        DictionaryPack::open_installed(&directory, &expected, PackLimits::default()).unwrap();
    assert!(matches!(
        installed.lookup("authenticated", LookupOptions::default()),
        Err(PackError::Corrupt(_))
    ));
}

#[test]
fn installed_admission_rejects_asset_size_and_schema_changes() {
    let source = b"{\"lang_code\":\"en\",\"word\":\"one\"}\n";
    let (_temporary, pack) = build(source, 1_000_000);
    let directory = pack.directory().to_owned();
    let expected = expected_pack(&pack);
    drop(pack);

    let data_path = data_asset_path(&directory);
    let original = fs::read(&data_path).unwrap();
    let mut extended = original.clone();
    extended.extend_from_slice(&[0; 4096]);
    fs::write(&data_path, &extended).unwrap();
    assert!(matches!(
        DictionaryPack::open_installed(&directory, &expected, PackLimits::default()),
        Err(PackError::Corrupt(message)) if message.contains("size mismatch")
    ));

    fs::write(&data_path, &original).unwrap();
    let connection = rusqlite::Connection::open(&data_path).unwrap();
    connection
        .execute_batch("DROP TABLE shard_metadata; CREATE TABLE shard_metadata (x INTEGER);")
        .unwrap();
    connection.close().unwrap();
    let mut resized = fs::read(&data_path).unwrap();
    resized.resize(original.len(), 0);
    fs::write(&data_path, resized).unwrap();
    assert!(DictionaryPack::open_installed(&directory, &expected, PackLimits::default()).is_err());
}
