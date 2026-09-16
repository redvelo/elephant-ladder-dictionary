use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use rusqlite::{Connection, params};
use serde::Serialize;
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::audio_state::{
    AcquisitionState, Phase, author_from_html, original_path, qualify_license,
};
use crate::builder::{
    StagingDirectory, asset_for, close_connection, io_error, sql_i64, sync_directory,
};
use crate::{AudioEditionConfig, BuildError, transcode_to_opus};
use elephant_ladder_dictionary_pack::{
    AUDIO_CHUNK_SCHEMA, AUDIO_ENCODER_PROFILE, AUDIO_INDEX_SCHEMA, AUDIO_MANIFEST_FILE,
    AudioManifest, AudioRevisionInputs, FORMAT_VERSION, PackAsset, PackId, PackRevision,
    RecordingFacts, RecordingLicense, RecordingStatus, Sha256Hex, audio_chunk_file_name,
    audio_chunk_for, audio_index_file_name, frame_recording_facts, recording_input_digest,
};

/// Longest accepted source recording.
pub const MAX_RECORDING_SECONDS: u64 = 60;
const MAX_CHUNK_BYTES: u64 = 1_900_000_000;

/// Inputs for one audio collection build.
#[derive(Clone, Debug)]
pub struct AudioBuildOptions {
    pub builder_revision: String,
    pub minimum_app_version: String,
    pub workers: usize,
}

/// Summary of a built audio collection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AudioBuildResult {
    pub pack_id: String,
    pub pack_revision: Sha256Hex,
    pub recordings: u64,
    pub available: u64,
    pub unqualified: u64,
    pub missing: u64,
    pub failed: u64,
    pub blob_bytes: u64,
}

struct Built {
    facts: RecordingFacts,
    audio: Option<(Vec<u8>, [u8; 32], u64)>,
}

/// Builds an audio collection from a completed acquisition.
///
/// # Errors
///
/// Returns an error when the acquisition is incomplete, the output exists, or any
/// asset cannot be written.
pub fn build_audio_collection(
    state: &AcquisitionState,
    audio: &AudioEditionConfig,
    options: &AudioBuildOptions,
    output: &Path,
) -> Result<AudioBuildResult, BuildError> {
    let pack_id = PackId::from_str(&audio.pack_id)?;
    if audio.chunk_count == 0 {
        return Err(BuildError::InvalidManifest(
            "audio chunk count must be positive".to_owned(),
        ));
    }
    let (corpus_pack_id, corpus_revision, corpus_language, _, completed_at) = state.metadata()?;
    let acquired_at = completed_at
        .ok_or_else(|| BuildError::InvalidManifest("acquisition is not complete".to_owned()))?;
    let corpus_pack_id = PackId::from_str(&corpus_pack_id)?;
    let corpus_revision: Sha256Hex = corpus_revision
        .parse()
        .map_err(|message: &str| BuildError::InvalidManifest(message.to_owned()))?;

    let files = state.files()?;
    let built = transcode_all(state.directory(), &files, options.workers.max(1))?;

    let mut digest = recording_input_digest();
    for recording in &built {
        frame_recording_facts(&mut digest, &recording.facts);
    }
    let recording_input_digest: [u8; 32] = digest.finalize().into();
    let revision = PackRevision::derive_audio(AudioRevisionInputs {
        pack_id: &pack_id,
        corpus_language: &corpus_language,
        source_corpus_pack_id: &corpus_pack_id,
        source_corpus_revision: &PackRevision::from_bytes(*corpus_revision.as_bytes()),
        acquired_at: &acquired_at,
        encoder_profile: AUDIO_ENCODER_PROFILE,
        builder_revision: &options.builder_revision,
        chunk_count: audio.chunk_count,
        recording_input_digest: &recording_input_digest,
        minimum_app_version: &options.minimum_app_version,
    });

    let mut staging = StagingDirectory::create(output)?;
    let (mut assets, routes) = write_chunks(staging.path(), &pack_id, &built, audio.chunk_count)?;

    let index_name = audio_index_file_name(&pack_id, &revision);
    let index_path = staging.path().join(&index_name);
    write_index(
        &index_path,
        &IndexInputs {
            pack_id: &pack_id,
            revision: &revision,
            corpus_language: &corpus_language,
            corpus_pack_id: &corpus_pack_id,
            corpus_revision: &corpus_revision,
            acquired_at: &acquired_at,
            builder_revision: &options.builder_revision,
            chunk_count: audio.chunk_count,
            recording_input_digest: &recording_input_digest,
            minimum_app_version: &options.minimum_app_version,
        },
        &built,
        &routes,
    )?;
    assets.insert(0, asset_for("index", &index_name, &index_path)?);

    let manifest = AudioManifest {
        manifest_version: 1,
        format_version: FORMAT_VERSION,
        pack_id: pack_id.as_str().to_owned(),
        pack_revision: Sha256Hex::from_bytes(*revision.as_bytes()),
        assets,
    };
    let manifest_path = staging.path().join(AUDIO_MANIFEST_FILE);
    let mut bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| BuildError::InvalidManifest(error.to_string()))?;
    bytes.push(b'\n');
    fs::write(&manifest_path, bytes).map_err(|source| io_error("write", &manifest_path, source))?;
    sync_directory(staging.path())?;
    staging.publish(output)?;

    let count = |status| {
        built
            .iter()
            .filter(|recording| recording.facts.status == status)
            .count() as u64
    };
    Ok(AudioBuildResult {
        pack_id: pack_id.as_str().to_owned(),
        pack_revision: Sha256Hex::from_bytes(*revision.as_bytes()),
        recordings: built.len() as u64,
        available: count(RecordingStatus::Available),
        unqualified: count(RecordingStatus::Unqualified),
        missing: count(RecordingStatus::Missing),
        failed: count(RecordingStatus::Failed),
        blob_bytes: routes.iter().map(|route| route.blob_bytes).sum(),
    })
}

fn transcode_all(
    acquisition: &Path,
    files: &[crate::audio_state::FileState],
    workers: usize,
) -> Result<Vec<Built>, BuildError> {
    for file in files {
        if matches!(file.phase, Phase::Pending | Phase::Resolved) {
            return Err(BuildError::InvalidManifest(format!(
                "`{}` is still {}",
                file.file_name,
                file.phase.as_str()
            )));
        }
    }
    let next = Mutex::new(0_usize);
    let results = Mutex::new(Vec::with_capacity(files.len()));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = {
                        let mut next = next.lock().expect("work queue lock");
                        let index = *next;
                        *next += 1;
                        index
                    };
                    let Some(file) = files.get(index) else {
                        return;
                    };
                    let built = build_one(acquisition, file);
                    results.lock().expect("result lock").push((index, built));
                }
            });
        }
    });
    let mut results = results.into_inner().expect("result lock");
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, built)| built).collect()
}

fn build_one(
    acquisition: &Path,
    file: &crate::audio_state::FileState,
) -> Result<Built, BuildError> {
    let facts = file.facts.as_ref();
    let mut recording = base_facts(file);
    match file.phase {
        Phase::Missing => {
            recording.status = RecordingStatus::Missing;
            recording.reason = Some(
                file.reason
                    .clone()
                    .unwrap_or_else(|| "not on Commons".to_owned()),
            );
            return Ok(Built {
                facts: recording,
                audio: None,
            });
        }
        Phase::Failed => {
            recording.reason = Some(
                file.reason
                    .clone()
                    .unwrap_or_else(|| "acquisition failed".to_owned()),
            );
            return Ok(Built {
                facts: recording,
                audio: None,
            });
        }
        Phase::Pending | Phase::Resolved | Phase::Downloaded => {}
    }
    let facts = facts.ok_or_else(|| {
        BuildError::InvalidManifest(format!("`{}` was downloaded without facts", file.file_name))
    })?;
    let license: RecordingLicense = match qualify_license(facts) {
        Ok(license) => license,
        Err(reason) => {
            recording.status = RecordingStatus::Unqualified;
            recording.license =
                facts
                    .license_short_name
                    .clone()
                    .map(|short_name| RecordingLicense {
                        short_name,
                        identifier: facts.license_identifier.clone(),
                        url: facts.license_url.clone(),
                    });
            recording.reason = Some(reason);
            return Ok(Built {
                facts: recording,
                audio: None,
            });
        }
    };
    recording.license = Some(license);
    let path = original_path(acquisition, &facts.sha1);
    let source = fs::read(&path).map_err(|source| io_error("read original", &path, source))?;
    if hex::encode(Sha1::digest(&source)) != facts.sha1 {
        recording.reason = Some("stored original no longer matches its Commons SHA-1".to_owned());
        return Ok(Built {
            facts: recording,
            audio: None,
        });
    }
    let serial = u32::from_be_bytes(
        recording
            .source_sha1
            .map_or([0; 4], |sha1| [sha1[0], sha1[1], sha1[2], sha1[3]]),
    );
    match transcode_to_opus(&source, serial, MAX_RECORDING_SECONDS) {
        Ok(transcoded) => {
            recording.status = RecordingStatus::Available;
            recording.reason = None;
            let sha256: [u8; 32] = Sha256::digest(&transcoded.bytes).into();
            Ok(Built {
                facts: recording,
                audio: Some((transcoded.bytes, sha256, transcoded.duration_ms)),
            })
        }
        Err(error) => {
            recording.reason = Some(error.to_string());
            Ok(Built {
                facts: recording,
                audio: None,
            })
        }
    }
}

fn base_facts(file: &crate::audio_state::FileState) -> RecordingFacts {
    let facts = file.facts.as_ref();
    let mut recording = RecordingFacts {
        file_name: file.file_name.clone(),
        status: RecordingStatus::Failed,
        reason: file.reason.clone(),
        reference_count: file.reference_count,
        source_title: facts.map(|facts| facts.title.clone()),
        source_sha1: facts.and_then(|facts| decode_sha1(&facts.sha1)),
        source_timestamp: facts.map(|facts| facts.timestamp.clone()),
        source_media_type: facts.map(|facts| facts.media_type.clone()),
        source_size_bytes: facts.map(|facts| facts.size_bytes),
        description_url: facts.map(|facts| facts.description_url.clone()),
        license: None,
        author_text: None,
        author_urls: Vec::new(),
        attribution_required: facts.and_then(|facts| facts.attribution_required),
    };
    if let Some(html) = facts.and_then(|facts| facts.artist_html.as_deref()) {
        let (text, urls) = author_from_html(html);
        recording.author_text = Some(text).filter(|text| !text.is_empty());
        recording.author_urls = urls;
    }
    recording
}

fn decode_sha1(text: &str) -> Option<[u8; 20]> {
    let mut bytes = [0; 20];
    hex::decode_to_slice(text, &mut bytes).ok().map(|()| bytes)
}

struct ChunkRoute {
    ordinal: u64,
    file_name: String,
    sha256: [u8; 32],
    size: u64,
    blob_count: u64,
    blob_bytes: u64,
}

fn write_chunks(
    directory: &Path,
    pack_id: &PackId,
    built: &[Built],
    chunk_count: u64,
) -> Result<(Vec<PackAsset>, Vec<ChunkRoute>), BuildError> {
    let mut assets = Vec::new();
    let mut routes = Vec::new();
    for ordinal in 0..chunk_count {
        let blobs = built
            .iter()
            .filter_map(|recording| {
                recording
                    .audio
                    .as_ref()
                    .filter(|_| audio_chunk_for(&recording.facts.file_name, chunk_count) == ordinal)
                    .map(|(bytes, sha256, _)| (*sha256, bytes.as_slice()))
            })
            .collect::<BTreeMap<_, _>>();
        let (asset, route) = write_chunk(directory, pack_id, ordinal, &blobs)?;
        assets.push(asset);
        routes.push(route);
    }
    Ok((assets, routes))
}

fn write_chunk(
    directory: &Path,
    pack_id: &PackId,
    ordinal: u64,
    blobs: &BTreeMap<[u8; 32], &[u8]>,
) -> Result<(PackAsset, ChunkRoute), BuildError> {
    let temporary = directory.join(format!("chunk-{ordinal}.tmp"));
    let connection = Connection::open(&temporary)?;
    connection.execute_batch(AUDIO_CHUNK_SCHEMA)?;
    connection.execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")?;
    let blob_count = blobs.len() as u64;
    let blob_bytes = blobs.values().map(|bytes| bytes.len() as u64).sum();
    connection.execute(
        "INSERT INTO chunk_metadata (singleton, format_version, pack_id, chunk_ordinal, blob_count) \
         VALUES (1, ?1, ?2, ?3, ?4)",
        params![FORMAT_VERSION, pack_id.as_str(), sql_i64("chunk", ordinal)?, sql_i64("blobs", blob_count)?],
    )?;
    connection.execute_batch("BEGIN")?;
    {
        let mut insert = connection.prepare("INSERT INTO blobs (sha256, bytes) VALUES (?1, ?2)")?;
        for (sha256, bytes) in blobs {
            insert.execute(params![sha256.as_slice(), bytes])?;
        }
    }
    connection.execute_batch("COMMIT; VACUUM;")?;
    close_connection(connection)?;
    let staged = asset_for("chunk", "", &temporary)?;
    if staged.size_bytes > MAX_CHUNK_BYTES {
        return Err(BuildError::BuildLimit(format!(
            "audio chunk {ordinal} is {} bytes; raise the chunk count",
            staged.size_bytes
        )));
    }
    let sha256 = *staged.sha256.as_bytes();
    let file_name = audio_chunk_file_name(pack_id, ordinal, &sha256);
    let path = directory.join(&file_name);
    fs::rename(&temporary, &path).map_err(|source| io_error("rename chunk", &path, source))?;
    Ok((
        PackAsset {
            role: "chunk".to_owned(),
            file_name: file_name.clone(),
            sha256: staged.sha256,
            size_bytes: staged.size_bytes,
        },
        ChunkRoute {
            ordinal,
            file_name,
            sha256,
            size: staged.size_bytes,
            blob_count,
            blob_bytes,
        },
    ))
}

type LicenseKey = (String, Option<String>, Option<String>);

fn insert_recordings(
    connection: &Connection,
    built: &[Built],
    licenses: &[LicenseKey],
    chunk_count: u64,
) -> Result<(), BuildError> {
    let mut insert = connection.prepare(
        "INSERT INTO recordings (file_name, status, reason, reference_count, source_title, \
         source_sha1, source_timestamp, source_media_type, source_size_bytes, description_url, \
         license_ordinal, author_text, author_urls_json, attribution_required, opus_sha256, \
         opus_size_bytes, duration_ms, chunk_ordinal) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
    )?;
    for recording in built {
        let facts = &recording.facts;
        let license_ordinal = facts
            .license
            .as_ref()
            .map(|license| {
                let key = (
                    license.short_name.clone(),
                    license.identifier.clone(),
                    license.url.clone(),
                );
                licenses
                    .binary_search(&key)
                    .ok()
                    .and_then(|ordinal| i64::try_from(ordinal).ok())
                    .ok_or_else(|| {
                        BuildError::InvalidManifest("license was not indexed".to_owned())
                    })
            })
            .transpose()?;
        let author_urls = if facts.author_urls.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&facts.author_urls)
                    .map_err(|error| BuildError::InvalidManifest(error.to_string()))?,
            )
        };
        let audio = recording.audio.as_ref();
        insert.execute(params![
            facts.file_name,
            facts.status as u8,
            facts.reason,
            sql_i64("references", facts.reference_count)?,
            facts.source_title,
            facts.source_sha1.as_ref().map(<[u8; 20]>::as_slice),
            facts.source_timestamp,
            facts.source_media_type,
            facts
                .source_size_bytes
                .map(|size| sql_i64("source size", size))
                .transpose()?,
            facts.description_url,
            license_ordinal,
            facts.author_text,
            author_urls,
            facts.attribution_required,
            audio.map(|(_, sha256, _)| sha256.as_slice()),
            audio
                .map(|(bytes, _, _)| sql_i64("opus size", bytes.len() as u64))
                .transpose()?,
            audio
                .map(|(_, _, duration)| sql_i64("duration", *duration))
                .transpose()?,
            audio
                .map(|_| sql_i64("chunk", audio_chunk_for(&facts.file_name, chunk_count)))
                .transpose()?,
        ])?;
    }
    Ok(())
}

struct IndexInputs<'a> {
    pack_id: &'a PackId,
    revision: &'a PackRevision,
    corpus_language: &'a str,
    corpus_pack_id: &'a PackId,
    corpus_revision: &'a Sha256Hex,
    acquired_at: &'a str,
    builder_revision: &'a str,
    chunk_count: u64,
    recording_input_digest: &'a [u8; 32],
    minimum_app_version: &'a str,
}

fn write_index(
    path: &Path,
    inputs: &IndexInputs<'_>,
    built: &[Built],
    routes: &[ChunkRoute],
) -> Result<(), BuildError> {
    let connection = Connection::open(path)?;
    connection.execute_batch(AUDIO_INDEX_SCHEMA)?;
    connection.execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF; BEGIN;")?;
    let available = built
        .iter()
        .filter(|recording| recording.audio.is_some())
        .count() as u64;
    connection.execute(
        "INSERT INTO collection_metadata (singleton, format_version, pack_id, pack_revision, \
         corpus_language, source_corpus_pack_id, source_corpus_revision, acquired_at, \
         encoder_profile, builder_revision, chunk_count, recording_count, available_count, \
         recording_input_digest, minimum_app_version) \
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            FORMAT_VERSION,
            inputs.pack_id.as_str(),
            inputs.revision.as_bytes().as_slice(),
            inputs.corpus_language,
            inputs.corpus_pack_id.as_str(),
            inputs.corpus_revision.as_bytes().as_slice(),
            inputs.acquired_at,
            AUDIO_ENCODER_PROFILE,
            inputs.builder_revision,
            sql_i64("chunk count", inputs.chunk_count)?,
            sql_i64("recordings", built.len() as u64)?,
            sql_i64("available", available)?,
            inputs.recording_input_digest.as_slice(),
            inputs.minimum_app_version,
        ],
    )?;
    for route in routes {
        connection.execute(
            "INSERT INTO chunks (chunk_ordinal, file_name, file_sha256, file_size_bytes, \
             blob_count, blob_bytes) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                sql_i64("chunk", route.ordinal)?,
                route.file_name,
                route.sha256.as_slice(),
                sql_i64("chunk size", route.size)?,
                sql_i64("blob count", route.blob_count)?,
                sql_i64("blob bytes", route.blob_bytes)?,
            ],
        )?;
    }
    let licenses = built
        .iter()
        .filter_map(|recording| recording.facts.license.as_ref())
        .map(|license| {
            (
                license.short_name.clone(),
                license.identifier.clone(),
                license.url.clone(),
            )
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    for (ordinal, (short_name, identifier, url)) in licenses.iter().enumerate() {
        connection.execute(
            "INSERT INTO licenses (license_ordinal, short_name, identifier, url) VALUES (?1, ?2, ?3, ?4)",
            params![sql_i64("license", ordinal as u64)?, short_name, identifier, url],
        )?;
    }
    insert_recordings(&connection, built, &licenses, inputs.chunk_count)?;
    connection.execute_batch("COMMIT; VACUUM;")?;
    close_connection(connection)
}
