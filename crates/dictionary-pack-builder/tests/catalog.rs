mod common;

use std::fs;
use std::process::Command;

use elephant_ladder_dictionary_pack::{
    CATALOG_FILE, CatalogError, CatalogPack, DictionaryPack, PACK_MANIFEST_FILE, parse_catalog,
};
use elephant_ladder_dictionary_pack_builder::{CatalogConfig, CorpusConfig, assemble_catalog};
use serde_json::{Value, json};
use tempfile::TempDir;

const SOURCE: &[u8] = b"{\"lang_code\":\"en\",\"word\":\"one\"}\n";

fn config() -> CatalogConfig {
    CatalogConfig {
        catalog_revision: "2026-09-16.1".to_owned(),
        generated_at: "2026-09-16T00:00:00Z".to_owned(),
        release_base_url: "https://github.com/example/packs/releases/download/2026-09-16.1/"
            .to_owned(),
        corpora: vec![CorpusConfig {
            directory: "pack".into(),
            monolingual: true,
            bilingual_targets: vec!["fr".to_owned(), "ja".to_owned()],
        }],
        audio: Vec::new(),
    }
}

fn assembled_release() -> TempDir {
    let temporary = tempfile::tempdir().unwrap();
    common::build_into(&temporary.path().join("pack"), "en", SOURCE, 1_000_000);
    assemble_catalog(
        &config(),
        temporary.path(),
        &temporary.path().join("release"),
    )
    .unwrap();
    temporary
}

#[test]
fn assembled_catalog_parses_and_anchors_pack_admission() {
    let temporary = assembled_release();
    let release = temporary.path().join("release");
    let catalog = parse_catalog(&fs::read(release.join(CATALOG_FILE)).unwrap()).unwrap();

    let [CatalogPack::Corpus(corpus)] = catalog.packs.as_slice() else {
        panic!("expected one corpus");
    };
    assert_eq!(corpus.corpus_language, "en");
    assert_eq!(corpus.licenses, ["CC-BY-SA-4.0", "GFDL-1.3-only"]);
    assert_eq!(corpus.bilingual_targets, ["fr", "ja"]);
    let manifest_asset = corpus
        .release
        .assets
        .iter()
        .find(|asset| asset.file_name == PACK_MANIFEST_FILE)
        .unwrap();
    assert_eq!(
        manifest_asset.url,
        format!(
            "https://github.com/example/packs/releases/download/2026-09-16.1/{}-{}-{PACK_MANIFEST_FILE}",
            corpus.release.pack_id, corpus.release.pack_revision
        )
    );
    for asset in &corpus.release.assets {
        let name = asset.url.rsplit('/').next().unwrap();
        let bytes = fs::read(release.join(name)).unwrap();
        assert_eq!(bytes.len() as u64, asset.size_bytes);
        assert_eq!(common::digest(&bytes), asset.sha256);
    }

    let expected = catalog.packs[0].expected_pack();
    DictionaryPack::verify(temporary.path().join("pack"), &expected).unwrap();
}

#[test]
fn oversized_catalogs_are_rejected() {
    assert_eq!(
        parse_catalog(&vec![b' '; 4 * 1024 * 1024 + 1]),
        Err(CatalogError::TooLarge)
    );
}

#[test]
fn parsed_catalogs_must_satisfy_the_format() {
    let temporary = assembled_release();
    let catalog: Value = serde_json::from_slice(
        &fs::read(temporary.path().join("release").join(CATALOG_FILE)).unwrap(),
    )
    .unwrap();
    let mutations: [fn(&mut Value); 11] = [
        |catalog| catalog["unexpected"] = json!(true),
        |catalog| catalog["catalog_version"] = json!(2),
        |catalog| catalog["packs"][0]["surprise"] = json!(true),
        |catalog| catalog["packs"][0]["release"]["surprise"] = json!(true),
        |catalog| catalog["packs"][0]["release"]["pack_id"] = json!("Not Safe"),
        |catalog| catalog["packs"][0]["pack_type"] = json!("stardict"),
        |catalog| catalog["packs"][0]["bilingual_targets"] = json!(["fr", "fr"]),
        |catalog| catalog["packs"][0]["release"]["installed_bytes"] = json!(1),
        |catalog| catalog["packs"][0]["release"]["assets"][0]["file_name"] = json!("../escape"),
        |catalog| {
            let assets = catalog["packs"][0]["release"]["assets"]
                .as_array_mut()
                .unwrap();
            assets.retain(|asset| asset["file_name"] != PACK_MANIFEST_FILE);
        },
        |catalog| {
            let pack = catalog["packs"][0].clone();
            catalog["packs"].as_array_mut().unwrap().push(pack);
        },
    ];
    for mutate in mutations {
        let mut mutated = catalog.clone();
        mutate(&mut mutated);
        let bytes = serde_json::to_vec(&mutated).unwrap();
        assert!(
            matches!(parse_catalog(&bytes), Err(CatalogError::Invalid(_))),
            "accepted {mutated}"
        );
    }
}

#[test]
fn catalog_cli_assembles_a_release() {
    let temporary = tempfile::tempdir().unwrap();
    common::build_into(&temporary.path().join("pack"), "en", SOURCE, 1_000_000);
    let config = json!({
        "catalog_revision": "cli-1",
        "generated_at": "2026-09-16T00:00:00Z",
        "release_base_url": "https://example.invalid/releases",
        "corpora": [{"directory": "pack", "monolingual": true, "bilingual_targets": []}],
    });
    fs::write(
        temporary.path().join("catalog.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_elephant-dictionary-pack"))
        .current_dir(temporary.path())
        .args([
            "catalog",
            "assemble",
            "--config",
            "catalog.json",
            "--output",
            "release",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "catalog_revision=cli-1\npacks=1\n"
    );

    let catalog =
        parse_catalog(&fs::read(temporary.path().join("release").join(CATALOG_FILE)).unwrap())
            .unwrap();
    assert_eq!(catalog.catalog_revision, "cli-1");
}
