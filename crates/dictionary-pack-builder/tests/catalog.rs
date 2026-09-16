use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::process::Command;

use elephant_ladder_dictionary_pack::{
    CATALOG_FILE, CATALOG_SIGNATURE_FILE, CatalogError, CatalogPack, DictionaryPack,
    PACK_MANIFEST_FILE, PackLimits, SigningKey, parse_verifying_key, verify_catalog,
};
use elephant_ladder_dictionary_pack_builder::{
    BuildManifest, BuildOptions, CatalogConfig, CorpusConfig, Sha256Hex, assemble_catalog,
    build_pack, generate_signing_key, release_manifest_name, sign_catalog,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn digest(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from_bytes(Sha256::digest(bytes).into())
}

fn build_pack_directory(root: &Path) {
    let source = b"{\"lang_code\":\"en\",\"word\":\"one\"}\n";
    let source_manifest = json!({"provider":"catalog-fixture"});
    let license_manifest = json!({"licenses":[{"spdx":"CC-BY-SA-4.0"},{"spdx":"GFDL-1.3-only"}]});
    let manifest = BuildManifest {
        pack_id: "wiktionary-en-en".to_owned(),
        corpus_language: "en".to_owned(),
        wiktionary_edition: "enwiktionary".to_owned(),
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
    build_pack(
        &manifest,
        Cursor::new(source),
        root.join("pack"),
        BuildOptions::default(),
    )
    .unwrap();
}

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
    }
}

fn signed_release() -> (TempDir, SigningKey) {
    let temporary = tempfile::tempdir().unwrap();
    build_pack_directory(temporary.path());
    assemble_catalog(
        &config(),
        temporary.path(),
        &temporary.path().join("release"),
    )
    .unwrap();
    let key = generate_signing_key(&temporary.path().join("signing.key")).unwrap();
    sign_catalog(&temporary.path().join("release"), &key).unwrap();
    (temporary, key)
}

#[test]
fn assembled_catalog_verifies_and_anchors_pack_admission() {
    let (temporary, key) = signed_release();
    let release = temporary.path().join("release");
    let catalog_bytes = fs::read(release.join(CATALOG_FILE)).unwrap();
    let signature = fs::read(release.join(CATALOG_SIGNATURE_FILE)).unwrap();
    let catalog = verify_catalog(&catalog_bytes, &signature, &[key.verifying_key()]).unwrap();

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
    let release_name =
        release_manifest_name(&corpus.release.pack_id, &corpus.release.pack_revision);
    assert_eq!(
        manifest_asset.url,
        format!("https://github.com/example/packs/releases/download/2026-09-16.1/{release_name}")
    );
    for asset in &corpus.release.assets {
        let name = asset.url.rsplit('/').next().unwrap();
        let bytes = fs::read(release.join(name)).unwrap();
        assert_eq!(bytes.len() as u64, asset.size_bytes);
        assert_eq!(digest(&bytes), asset.sha256);
    }

    let expected = catalog.packs[0].expected_pack();
    DictionaryPack::verify(
        temporary.path().join("pack"),
        &expected,
        PackLimits::default(),
    )
    .unwrap();
}

#[test]
fn catalog_verification_rejects_tampering_untrusted_keys_and_bad_signatures() {
    let (temporary, key) = signed_release();
    let release = temporary.path().join("release");
    let catalog_bytes = fs::read(release.join(CATALOG_FILE)).unwrap();
    let signature = fs::read(release.join(CATALOG_SIGNATURE_FILE)).unwrap();
    let trusted = [key.verifying_key()];

    let mut tampered = catalog_bytes.clone();
    let position = tampered.iter().position(|byte| *byte == b'1').unwrap();
    tampered[position] = b'2';
    assert_eq!(
        verify_catalog(&tampered, &signature, &trusted),
        Err(CatalogError::UntrustedSignature)
    );

    let other = SigningKey::from_bytes(&[7; 32]);
    assert_eq!(
        verify_catalog(&catalog_bytes, &signature, &[other.verifying_key()]),
        Err(CatalogError::UntrustedSignature)
    );
    assert_eq!(
        verify_catalog(&catalog_bytes, &signature, &[]),
        Err(CatalogError::UntrustedSignature)
    );
    for malformed in [
        &signature[..10],
        signature.to_ascii_uppercase().as_slice(),
        b"",
        &[signature.as_slice(), b"\n"].concat(),
    ] {
        assert_eq!(
            verify_catalog(&catalog_bytes, malformed, &trusted),
            Err(CatalogError::MalformedSignature)
        );
    }
    assert_eq!(
        verify_catalog(&vec![b' '; 4 * 1024 * 1024 + 1], &signature, &trusted),
        Err(CatalogError::TooLarge)
    );
}

#[test]
fn signed_catalogs_must_still_satisfy_the_format() {
    use ed25519_dalek::Signer;
    use elephant_ladder_dictionary_pack::encode_signature;

    let (temporary, _) = signed_release();
    let catalog: Value = serde_json::from_slice(
        &fs::read(temporary.path().join("release").join(CATALOG_FILE)).unwrap(),
    )
    .unwrap();
    let key = SigningKey::from_bytes(&[3; 32]);
    let mutations: [fn(&mut Value); 10] = [
        |catalog| catalog["unexpected"] = json!(true),
        |catalog| catalog["catalog_version"] = json!(2),
        |catalog| catalog["packs"][0]["surprise"] = json!(true),
        |catalog| catalog["packs"][0]["release"]["surprise"] = json!(true),
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
        let signature = encode_signature(&key.sign(&bytes));
        assert!(
            matches!(
                verify_catalog(&bytes, signature.as_bytes(), &[key.verifying_key()]),
                Err(CatalogError::Invalid(_))
            ),
            "accepted {mutated}"
        );
    }
}

#[test]
fn catalog_cli_generates_keys_assembles_and_signs() {
    let temporary = tempfile::tempdir().unwrap();
    build_pack_directory(temporary.path());
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
    let tool = env!("CARGO_BIN_EXE_elephant-dictionary-pack");
    let run = |arguments: &[&str]| {
        let output = Command::new(tool)
            .current_dir(temporary.path())
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };

    let keygen = run(&["catalog", "keygen", "--output", "signing.key"]);
    let public_key = keygen
        .trim()
        .strip_prefix("public_key=")
        .unwrap()
        .to_owned();
    run(&[
        "catalog",
        "assemble",
        "--config",
        "catalog.json",
        "--output",
        "release",
    ]);
    let signed = run(&[
        "catalog",
        "sign",
        "--release",
        "release",
        "--key",
        "signing.key",
    ]);
    assert_eq!(signed.trim(), format!("public_key={public_key}"));

    let release = temporary.path().join("release");
    verify_catalog(
        &fs::read(release.join(CATALOG_FILE)).unwrap(),
        &fs::read(release.join(CATALOG_SIGNATURE_FILE)).unwrap(),
        &[parse_verifying_key(&public_key).unwrap()],
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(temporary.path().join("signing.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }
    let again = Command::new(tool)
        .current_dir(temporary.path())
        .args(["catalog", "keygen", "--output", "signing.key"])
        .output()
        .unwrap();
    assert!(!again.status.success());
}
