use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use elephant_ladder_dictionary_pack::{
    AUDIO_MANIFEST_FILE, AudioCollection, DictionaryPack, ExpectedPack, PackLimits, PackRevision,
    RecordingStatus, Sha256Hex, opus_duration_ms,
};
use elephant_ladder_dictionary_pack_builder::{
    AcquireOptions, AcquisitionState, AudioBuildOptions, AudioEditionConfig, BuildManifest,
    BuildOptions, ReferenceSet, acquire, build_audio_collection, build_pack,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/audio")
}

fn digest(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from_bytes(Sha256::digest(bytes).into())
}

fn build_corpus(directory: &Path) -> DictionaryPack {
    let source = concat!(
        "{\"lang_code\":\"en\",\"word\":\"cubi\",\"sounds\":[{\"audio\":\"LL-Q150_(fra)-Antochkat-cubi.wav\"}]}\n",
        "{\"lang_code\":\"en\",\"word\":\"abate\",\"sounds\":[{\"audio\":\"En-us-abate-old.oga\"},",
        "{\"audio\":\"En-us-modicum.mp3\"}]}\n",
        "{\"lang_code\":\"en\",\"word\":\"locative\",\"sounds\":[{\"audio\":\"En-in-locative.opus\"},",
        "{\"audio\":\"not a file\"}]}\n",
        "{\"lang_code\":\"en\",\"word\":\"gone\",\"sounds\":[{\"audio\":\"En-us-gone.ogg\"},",
        "{\"audio\":\"En-us-restricted.ogg\"},{\"audio\":\"En-us-corrupt.ogg\"}]}\n",
    );
    let source_manifest = json!({"provider":"audio-fixture"});
    let license_manifest = json!({"licenses":[{"spdx":"CC0-1.0"}]});
    let manifest = BuildManifest {
        pack_id: "wiktionary-en-en".to_owned(),
        corpus_language: "en".to_owned(),
        wiktionary_edition: "enwiktionary".to_owned(),
        wiktionary_dump_date: "2026-09-02".to_owned(),
        kaikki_extraction_date: "2026-09-09".to_owned(),
        source_url: "https://example.invalid/source.jsonl.gz".to_owned(),
        compressed_source_sha256: digest(b"compressed"),
        uncompressed_source_sha256: digest(source.as_bytes()),
        wiktextract_revision: "wiktextract".to_owned(),
        wikitextprocessor_revision: "wikitextprocessor".to_owned(),
        builder_revision: "builder".to_owned(),
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
        directory,
        BuildOptions::default(),
    )
    .unwrap();
    DictionaryPack::open(directory, PackLimits::default()).unwrap()
}

struct FakeCommons {
    url: String,
    rate_limited: Arc<AtomicUsize>,
}

type Pages = BTreeMap<String, Value>;
type Files = BTreeMap<String, Vec<u8>>;

fn fixture_pages(provenance: &Value, base: &str) -> (Pages, Files) {
    let mut pages = BTreeMap::new();
    let mut files = BTreeMap::new();
    for recording in provenance["recordings"].as_array().unwrap() {
        let local = recording["file"].as_str().unwrap();
        let bytes = fs::read(fixtures().join(local)).unwrap();
        let title = recording["commons_title"].as_str().unwrap().to_owned();
        let metadata = recording["extmetadata"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), json!({"value": value})))
            .collect::<serde_json::Map<_, _>>();
        pages.insert(
            title.clone(),
            json!({
                "title": title,
                "imageinfo": [{
                    "sha1": recording["sha1"],
                    "timestamp": recording["timestamp"],
                    "size": bytes.len(),
                    "mime": recording["mime"],
                    "descriptionurl": recording["description_url"],
                    "url": format!("{base}/files/{local}?utm_source=test"),
                    "extmetadata": metadata,
                }]
            }),
        );
        files.insert(format!("/files/{local}"), bytes);
    }
    let restricted = files["/files/En-us-abate.oga"].clone();
    pages.insert(
        "File:En-us-restricted.ogg".to_owned(),
        json!({"title": "File:En-us-restricted.ogg", "imageinfo": [{
            "sha1": hex::encode(sha1::Sha1::digest(&restricted)),
            "timestamp": "2020-01-01T00:00:00Z", "size": restricted.len(), "mime": "application/ogg",
            "descriptionurl": "https://commons.wikimedia.org/wiki/File:En-us-restricted.ogg",
            "url": format!("{base}/files/restricted.ogg"),
            "extmetadata": {"LicenseShortName": {"value": "CC BY-NC 4.0"}}
        }]}),
    );
    files.insert("/files/restricted.ogg".to_owned(), restricted);
    pages.insert(
        "File:En-us-corrupt.ogg".to_owned(),
        json!({"title": "File:En-us-corrupt.ogg", "imageinfo": [{
            "sha1": "0000000000000000000000000000000000000000",
            "timestamp": "2020-01-01T00:00:00Z", "size": 3, "mime": "application/ogg",
            "descriptionurl": "https://commons.wikimedia.org/wiki/File:En-us-corrupt.ogg",
            "url": format!("{base}/files/corrupt.ogg"),
            "extmetadata": {"LicenseShortName": {"value": "CC0"}}
        }]}),
    );
    files.insert("/files/corrupt.ogg".to_owned(), b"bad".to_vec());
    (pages, files)
}

/// Serves the Commons API and originals for the checked-in fixture recordings.
fn fake_commons() -> FakeCommons {
    let provenance: Value = serde_json::from_slice(
        &fs::read(fixtures().join("commons-fixtures.provenance.json")).unwrap(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (pages, files) = fixture_pages(&provenance, &base);

    let rate_limited = Arc::new(AtomicUsize::new(0));
    let limited = Arc::clone(&rate_limited);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let path = request_line.split(' ').nth(1).unwrap_or("/").to_owned();
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header == "\r\n" || header.is_empty() {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let (status, bytes) = if path.starts_with("/w/api.php") {
                if limited.fetch_add(1, Ordering::SeqCst) == 0 {
                    ("429 Too Many Requests", Vec::new())
                } else {
                    ("200 OK", api_response(&body, &pages))
                }
            } else {
                let path = path.split('?').next().unwrap();
                files
                    .get(path)
                    .map_or(("404 Not Found", Vec::new()), |bytes| {
                        ("200 OK", bytes.clone())
                    })
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nRetry-After: 0\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = stream.write_all(&bytes);
        }
    });
    FakeCommons {
        url: format!("{base}/w/api.php"),
        rate_limited,
    }
}

fn api_response(body: &[u8], pages: &BTreeMap<String, Value>) -> Vec<u8> {
    let form = String::from_utf8(body.to_vec()).unwrap();
    let titles = form
        .split('&')
        .find_map(|pair| pair.strip_prefix("titles="))
        .map(percent_decode)
        .unwrap_or_default();
    let mut redirects = Vec::new();
    let mut result = Vec::new();
    for title in titles.split('|') {
        let target = if title == "File:En-us-abate-old.oga" {
            redirects.push(json!({"from": title, "to": "File:En-us-abate.oga"}));
            "File:En-us-abate.oga"
        } else {
            title
        };
        result.push(
            pages
                .get(target)
                .cloned()
                .unwrap_or_else(|| json!({"title": target, "missing": true})),
        );
    }
    serde_json::to_vec(&json!({"query": {"redirects": redirects, "pages": result}})).unwrap()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => output.push(b' '),
            b'%' => {
                output.push(u8::from_str_radix(&value[index + 1..index + 3], 16).unwrap());
                index += 2;
            }
            byte => output.push(byte),
        }
        index += 1;
    }
    String::from_utf8(output).unwrap()
}

fn expected(directory: &Path) -> ExpectedPack {
    let manifest_bytes = fs::read(directory.join(AUDIO_MANIFEST_FILE)).unwrap();
    let manifest: Value = serde_json::from_slice(&manifest_bytes).unwrap();
    ExpectedPack {
        pack_id: manifest["pack_id"].as_str().unwrap().parse().unwrap(),
        pack_revision: PackRevision::from_bytes(
            *manifest["pack_revision"]
                .as_str()
                .unwrap()
                .parse::<Sha256Hex>()
                .unwrap()
                .as_bytes(),
        ),
        manifest_sha256: digest(&manifest_bytes),
    }
}

fn chunk_names(directory: &Path) -> Vec<String> {
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join(AUDIO_MANIFEST_FILE)).unwrap()).unwrap();
    manifest["assets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|asset| asset["role"] == "chunk")
        .map(|asset| asset["file_name"].as_str().unwrap().to_owned())
        .collect()
}

fn assert_recordings(collection: &AudioCollection) {
    let wav = collection
        .recording("LL-Q150 (fra)-Antochkat-cubi.wav")
        .unwrap()
        .unwrap();
    assert_eq!(wav.facts.status, RecordingStatus::Available);
    assert_eq!(
        wav.facts.author_text.as_deref(),
        Some("Speaker: Antochkat\nRecorder: Antochkat")
    );
    assert_eq!(wav.facts.license.as_ref().unwrap().short_name, "CC0");
    let bytes = collection
        .read("LL-Q150 (fra)-Antochkat-cubi.wav")
        .unwrap()
        .unwrap();
    assert_eq!(
        opus_duration_ms(&bytes).unwrap(),
        wav.audio.unwrap().duration_ms
    );
    assert!((600..=650).contains(&wav.audio.unwrap().duration_ms));

    let redirected = collection
        .recording("En-us-abate-old.oga")
        .unwrap()
        .unwrap();
    assert_eq!(redirected.facts.status, RecordingStatus::Available);
    assert_eq!(
        redirected.facts.source_title.as_deref(),
        Some("File:En-us-abate.oga")
    );
    for (name, status) in [
        ("En-us-modicum.mp3", RecordingStatus::Available),
        ("En-in-locative.opus", RecordingStatus::Available),
        ("En-us-gone.ogg", RecordingStatus::Missing),
        ("En-us-restricted.ogg", RecordingStatus::Unqualified),
        ("En-us-corrupt.ogg", RecordingStatus::Failed),
    ] {
        let recording = collection.recording(name).unwrap().unwrap();
        assert_eq!(recording.facts.status, status, "{name}");
        assert_eq!(
            recording.audio.is_some(),
            status == RecordingStatus::Available
        );
        assert_eq!(
            recording.facts.reason.is_some(),
            status != RecordingStatus::Available
        );
        assert_eq!(
            collection.read(name).unwrap().is_some(),
            status == RecordingStatus::Available
        );
    }
    assert!(collection.recording("Unreferenced.ogg").unwrap().is_none());
}

#[test]
fn acquisition_build_and_runtime_cover_every_recording_status() {
    let temporary = tempfile::tempdir().unwrap();
    let corpus = build_corpus(&temporary.path().join("corpus"));
    let references = corpus.audio_references().unwrap();
    assert_eq!(references.files.len(), 7);
    assert_eq!(references.invalid.get("not a file"), Some(&1));
    assert!(
        references
            .files
            .contains_key("LL-Q150 (fra)-Antochkat-cubi.wav")
    );
    let reference_set = ReferenceSet {
        schema_version: 1,
        corpus_pack_id: corpus.pack_id().as_str().to_owned(),
        corpus_revision: Sha256Hex::from_bytes(*corpus.pack_revision().as_bytes()),
        corpus_language: "en".to_owned(),
        files: references.files.into_iter().collect(),
        invalid: references.invalid.into_iter().collect(),
    };

    let commons = fake_commons();
    let options = AcquireOptions {
        api_url: commons.url.clone(),
        download_workers: 2,
        backoff: Duration::from_millis(1),
        ..AcquireOptions::commons("https://example.invalid/test")
    };
    let state_directory = temporary.path().join("acquisition");
    let state =
        AcquisitionState::open_or_create(&state_directory, &reference_set, "2026-09-16T00:00:00Z")
            .unwrap();
    let report = acquire(&state, &options).unwrap();
    assert!(report.completed);
    assert!(commons.rate_limited.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        report.phases,
        BTreeMap::from([("downloaded", 5), ("failed", 1), ("missing", 1)])
    );
    drop(state);
    let resumed =
        AcquisitionState::open_or_create(&state_directory, &reference_set, "ignored").unwrap();
    assert!(acquire(&resumed, &options).unwrap().completed);

    let audio = AudioEditionConfig {
        pack_id: "wiktionary-en-audio".to_owned(),
        chunk_count: 4,
    };
    let build_options = AudioBuildOptions {
        builder_revision: "builder-a".to_owned(),
        minimum_app_version: "0.1.0".to_owned(),
        workers: 3,
    };
    let first = temporary.path().join("collection-a");
    let second = temporary.path().join("collection-b");
    let result = build_audio_collection(&resumed, &audio, &build_options, &first).unwrap();
    build_audio_collection(&resumed, &audio, &build_options, &second).unwrap();
    assert_eq!(
        (
            result.recordings,
            result.available,
            result.unqualified,
            result.missing,
            result.failed
        ),
        (7, 4, 1, 1, 1)
    );
    for entry in fs::read_dir(&first).unwrap() {
        let name = entry.unwrap().file_name();
        assert_eq!(
            fs::read(first.join(&name)).unwrap(),
            fs::read(second.join(&name)).unwrap(),
            "{name:?} is not deterministic"
        );
    }

    let collection =
        AudioCollection::verify(&first, &expected(&first), PackLimits::default()).unwrap();
    let validation = collection.validate_all().unwrap();
    assert_eq!(
        (validation.recording_count, validation.available_count),
        (7, 4)
    );

    assert_recordings(&collection);

    let rebuilt = temporary.path().join("collection-c");
    build_audio_collection(
        &resumed,
        &audio,
        &AudioBuildOptions {
            builder_revision: "builder-b".to_owned(),
            ..build_options
        },
        &rebuilt,
    )
    .unwrap();
    assert_ne!(
        expected(&rebuilt).pack_revision,
        expected(&first).pack_revision
    );
    assert_eq!(chunk_names(&rebuilt), chunk_names(&first));
    AudioCollection::open_installed(&rebuilt, &expected(&rebuilt), PackLimits::default()).unwrap();
}

#[test]
fn installed_collections_reject_identity_changes_and_tampered_recordings() {
    let temporary = tempfile::tempdir().unwrap();
    let corpus = build_corpus(&temporary.path().join("corpus"));
    let references = corpus.audio_references().unwrap();
    let reference_set = ReferenceSet {
        schema_version: 1,
        corpus_pack_id: corpus.pack_id().as_str().to_owned(),
        corpus_revision: Sha256Hex::from_bytes(*corpus.pack_revision().as_bytes()),
        corpus_language: "en".to_owned(),
        files: references.files.into_iter().collect(),
        invalid: Vec::new(),
    };
    let commons = fake_commons();
    let state = AcquisitionState::open_or_create(
        &temporary.path().join("acquisition"),
        &reference_set,
        "2026-09-16T00:00:00Z",
    )
    .unwrap();
    acquire(
        &state,
        &AcquireOptions {
            api_url: commons.url,
            backoff: Duration::from_millis(1),
            ..AcquireOptions::commons("https://example.invalid/test")
        },
    )
    .unwrap();
    let directory = temporary.path().join("collection");
    build_audio_collection(
        &state,
        &AudioEditionConfig {
            pack_id: "wiktionary-en-audio".to_owned(),
            chunk_count: 1,
        },
        &AudioBuildOptions {
            builder_revision: "builder".to_owned(),
            minimum_app_version: "0.1.0".to_owned(),
            workers: 1,
        },
        &directory,
    )
    .unwrap();
    let expected = expected(&directory);

    let wrong = ExpectedPack {
        pack_revision: PackRevision::from_bytes([1; 32]),
        ..expected.clone()
    };
    assert!(AudioCollection::open_installed(&directory, &wrong, PackLimits::default()).is_err());

    let chunk = directory.join(&chunk_names(&directory)[0]);
    let connection = rusqlite::Connection::open(&chunk).unwrap();
    let (sha, mut bytes): (Vec<u8>, Vec<u8>) = connection
        .query_row("SELECT sha256, bytes FROM blobs LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x55;
    connection
        .execute(
            "UPDATE blobs SET bytes = ?1 WHERE sha256 = ?2",
            rusqlite::params![bytes, sha],
        )
        .unwrap();
    connection.close().unwrap();

    assert!(AudioCollection::verify(&directory, &expected, PackLimits::default()).is_err());
    let installed =
        AudioCollection::open_installed(&directory, &expected, PackLimits::default()).unwrap();
    assert!(installed.validate_all().is_err());
}

#[test]
fn persistent_rate_limits_stop_the_run_and_leave_files_resumable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/w/api.php", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&requests);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = write!(
                stream,
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    let temporary = tempfile::tempdir().unwrap();
    let references = ReferenceSet {
        schema_version: 1,
        corpus_pack_id: "wiktionary-en-en".to_owned(),
        corpus_revision: digest(b"corpus"),
        corpus_language: "en".to_owned(),
        files: vec![("En-us-run.ogg".to_owned(), 1)],
        invalid: Vec::new(),
    };
    let state = AcquisitionState::open_or_create(
        &temporary.path().join("acquisition"),
        &references,
        "2026-09-16T00:00:00Z",
    )
    .unwrap();
    let error = acquire(
        &state,
        &AcquireOptions {
            api_url: url,
            max_consecutive_rate_limits: 3,
            backoff: Duration::from_millis(1),
            ..AcquireOptions::commons("https://example.invalid/test")
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("rate limited 4 consecutive times"),
        "{error}"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 4);
    assert_eq!(
        state
            .names_in_phase(elephant_ladder_dictionary_pack_builder::Phase::Pending, 10)
            .unwrap(),
        ["En-us-run.ogg"]
    );
}

#[test]
fn commons_options_follow_the_wikimedia_robot_policy() {
    let options = AcquireOptions::commons("https://github.com/redvelo/elephant-ladder-dictionary");
    assert!(
        options
            .user_agent
            .starts_with("ElephantLadderDictionaryBot/")
    );
    assert!(
        options
            .user_agent
            .contains("(https://github.com/redvelo/elephant-ladder-dictionary) ureq/")
    );
    assert_eq!(options.download_workers, 2);
    assert_eq!(options.download_bytes_per_second * 8, 25_000_000);
    assert_eq!(options.server_error_pause, Duration::from_secs(900));
}
