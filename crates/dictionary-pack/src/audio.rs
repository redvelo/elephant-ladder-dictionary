use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::pack::{
    Admission, bytes32, lock, nonnegative, open_database, positive, read_bounded, safe_asset_name,
    validate_asset_bytes, validate_asset_size,
};
use crate::{
    AUDIO_CHUNK_APPLICATION_ID, AUDIO_CHUNK_SCHEMA, AUDIO_ENCODER_PROFILE,
    AUDIO_INDEX_APPLICATION_ID, AUDIO_INDEX_SCHEMA, AudioRevisionInputs, ExpectedPack,
    FORMAT_VERSION, PackAsset, PackError, PackId, PackLimits, PackRevision, Sha256Hex,
};

/// The fixed name of the format-v1 audio collection manifest.
pub const AUDIO_MANIFEST_FILE: &str = "audio-manifest-v1.json";
const RECORDING_INPUT_DOMAIN: &[u8] = b"ELDICT-AUDIO-RECORDING-INPUTS-V1";

/// Deterministic release manifest emitted beside the audio `SQLite` assets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AudioManifest {
    pub manifest_version: u32,
    pub format_version: u32,
    pub pack_id: String,
    pub pack_revision: Sha256Hex,
    pub assets: Vec<PackAsset>,
}

/// The canonical index asset name of an audio collection revision.
#[must_use]
pub fn audio_index_file_name(pack_id: &PackId, revision: &PackRevision) -> String {
    format!("{}-{}-index.eldict", pack_id.as_str(), revision.to_hex())
}

/// The canonical, content-addressed name of an audio chunk asset. Identical chunks
/// keep identical names across collection revisions.
#[must_use]
pub fn audio_chunk_file_name(pack_id: &PackId, ordinal: u64, sha256: &[u8; 32]) -> String {
    format!(
        "{}-chunk-{ordinal:03}-{}.eldict",
        pack_id.as_str(),
        hex::encode(&sha256[..8])
    )
}

/// The chunk that holds a recording, stable for a given file name and chunk count.
///
/// # Panics
///
/// Panics when `chunk_count` is zero.
#[must_use]
pub fn audio_chunk_for(file_name: &str, chunk_count: u64) -> u64 {
    assert!(chunk_count > 0, "chunk count must be positive");
    let digest = Sha256::digest(file_name.as_bytes());
    u64::from_be_bytes(digest[..8].try_into().expect("digest prefix is 8 bytes")) % chunk_count
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RecordingStatus {
    Available = 1,
    /// The license is outside the allowlist or the file carries restrictions.
    Unqualified = 2,
    /// Commons has no such file.
    Missing = 3,
    /// Acquisition, verification, decoding, or encoding failed.
    Failed = 4,
}

impl RecordingStatus {
    fn from_code(code: i64) -> Result<Self, PackError> {
        match code {
            1 => Ok(Self::Available),
            2 => Ok(Self::Unqualified),
            3 => Ok(Self::Missing),
            4 => Ok(Self::Failed),
            _ => Err(PackError::Corrupt(format!(
                "unknown recording status {code}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingLicense {
    pub short_name: String,
    pub identifier: Option<String>,
    pub url: Option<String>,
}

/// The acquired facts of one referenced recording, independent of its encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingFacts {
    /// Normalized Commons file name.
    pub file_name: String,
    pub status: RecordingStatus,
    /// Present exactly when the recording is not available.
    pub reason: Option<String>,
    pub reference_count: u64,
    pub source_title: Option<String>,
    pub source_sha1: Option<[u8; 20]>,
    pub source_timestamp: Option<String>,
    pub source_media_type: Option<String>,
    pub source_size_bytes: Option<u64>,
    pub description_url: Option<String>,
    pub license: Option<RecordingLicense>,
    pub author_text: Option<String>,
    pub author_urls: Vec<String>,
    pub attribution_required: Option<bool>,
}

/// The encoded audio of an available recording.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingAudio {
    pub sha256: [u8; 32],
    pub size_bytes: u64,
    pub duration_ms: u64,
    pub chunk_ordinal: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recording {
    pub facts: RecordingFacts,
    pub audio: Option<RecordingAudio>,
}

/// Frames one recording's acquired facts into a collection input digest.
pub fn frame_recording_facts(digest: &mut Sha256, facts: &RecordingFacts) {
    fn field(digest: &mut Sha256, bytes: &[u8]) {
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    fn optional(digest: &mut Sha256, value: Option<&[u8]>) {
        match value {
            Some(bytes) => {
                digest.update([1]);
                field(digest, bytes);
            }
            None => digest.update([0]),
        }
    }
    field(digest, facts.file_name.as_bytes());
    field(digest, &[facts.status as u8]);
    optional(digest, facts.reason.as_deref().map(str::as_bytes));
    field(digest, &facts.reference_count.to_be_bytes());
    optional(digest, facts.source_title.as_deref().map(str::as_bytes));
    optional(digest, facts.source_sha1.as_ref().map(<[u8; 20]>::as_slice));
    optional(digest, facts.source_timestamp.as_deref().map(str::as_bytes));
    optional(
        digest,
        facts.source_media_type.as_deref().map(str::as_bytes),
    );
    optional(
        digest,
        facts
            .source_size_bytes
            .map(u64::to_be_bytes)
            .as_ref()
            .map(<[u8; 8]>::as_slice),
    );
    optional(digest, facts.description_url.as_deref().map(str::as_bytes));
    match &facts.license {
        Some(license) => {
            digest.update([1]);
            field(digest, license.short_name.as_bytes());
            optional(digest, license.identifier.as_deref().map(str::as_bytes));
            optional(digest, license.url.as_deref().map(str::as_bytes));
        }
        None => digest.update([0]),
    }
    optional(digest, facts.author_text.as_deref().map(str::as_bytes));
    field(digest, &(facts.author_urls.len() as u64).to_be_bytes());
    for url in &facts.author_urls {
        field(digest, url.as_bytes());
    }
    optional(
        digest,
        facts
            .attribution_required
            .map(|required| [u8::from(required)])
            .as_ref()
            .map(<[u8; 1]>::as_slice),
    );
}

/// A new recording input digest.
#[must_use]
pub fn recording_input_digest() -> Sha256 {
    let mut digest = Sha256::new();
    digest.update((RECORDING_INPUT_DOMAIN.len() as u64).to_be_bytes());
    digest.update(RECORDING_INPUT_DOMAIN);
    digest
}

/// Authenticated identity facts from an audio collection index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioMetadata {
    pub pack_id: String,
    pub revision: [u8; 32],
    pub corpus_language: String,
    pub source_corpus_pack_id: String,
    pub source_corpus_revision: [u8; 32],
    pub acquired_at: String,
    pub encoder_profile: String,
    pub builder_revision: String,
    pub chunk_count: u64,
    pub recording_count: u64,
    pub available_count: u64,
    pub recording_input_digest: [u8; 32],
    pub minimum_app_version: String,
}

#[derive(Clone)]
struct ChunkRoute {
    ordinal: u64,
    file_name: String,
    file_sha256: [u8; 32],
    file_size: u64,
    blob_count: u64,
    blob_bytes: u64,
}

struct Chunk {
    route: ChunkRoute,
    connection: Mutex<Connection>,
}

/// Facts authenticated by a complete traversal of an audio collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioValidationReport {
    pub recording_count: u64,
    pub available_count: u64,
    pub blob_count: u64,
    pub blob_bytes: u64,
}

/// An admitted immutable audio collection.
pub struct AudioCollection {
    directory: PathBuf,
    metadata: AudioMetadata,
    manifest: AudioManifest,
    limits: PackLimits,
    index: Mutex<Connection>,
    chunks: Vec<Chunk>,
}

impl AudioCollection {
    /// Performs complete admission of a collection that must match a trusted identity.
    ///
    /// # Errors
    ///
    /// Returns an error for any manifest, asset, database, identity, or relational
    /// inconsistency.
    pub fn verify(
        directory: impl AsRef<Path>,
        expected: &ExpectedPack,
        limits: PackLimits,
    ) -> Result<Self, PackError> {
        Self::admit(directory.as_ref(), expected, Admission::Full, limits)
    }

    /// Opens a collection previously admitted by [`Self::verify`] without hashing
    /// asset bytes, running integrity checks, or scanning tables. Every recording
    /// read remains authenticated against its stored digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest, sizes, identities, or schemas disagree.
    pub fn open_installed(
        directory: impl AsRef<Path>,
        expected: &ExpectedPack,
        limits: PackLimits,
    ) -> Result<Self, PackError> {
        Self::admit(directory.as_ref(), expected, Admission::Installed, limits)
    }

    fn admit(
        directory: &Path,
        expected: &ExpectedPack,
        admission: Admission,
        limits: PackLimits,
    ) -> Result<Self, PackError> {
        let limits = limits.validate()?;
        let (manifest, pack_id, revision) = read_audio_manifest(directory, expected, limits)?;

        let index_name = audio_index_file_name(&pack_id, &revision);
        let mut index_path = None;
        let mut chunk_assets = BTreeMap::new();
        for asset in &manifest.assets {
            safe_asset_name(&asset.file_name)?;
            if asset.size_bytes == 0 || asset.size_bytes > limits.max_asset_bytes {
                return Err(PackError::Limit(format!(
                    "asset `{}` has an invalid declared size",
                    asset.file_name
                )));
            }
            let path = directory.join(&asset.file_name);
            match admission {
                Admission::Full => {
                    validate_asset_bytes(&path, asset.size_bytes, asset.sha256.as_bytes())?;
                }
                Admission::Installed => validate_asset_size(&path, asset.size_bytes)?,
            }
            match asset.role.as_str() {
                "index" if asset.file_name == index_name && index_path.is_none() => {
                    index_path = Some(path);
                }
                "chunk" => {
                    if chunk_assets
                        .insert(asset.file_name.clone(), (path, asset.clone()))
                        .is_some()
                    {
                        return Err(PackError::Malformed(format!(
                            "duplicate chunk asset `{}`",
                            asset.file_name
                        )));
                    }
                }
                _ => {
                    return Err(PackError::Malformed(format!(
                        "unexpected audio asset `{}` with role `{}`",
                        asset.file_name, asset.role
                    )));
                }
            }
        }
        let index_path = index_path.ok_or_else(|| {
            PackError::Malformed("audio manifest does not declare its index".to_owned())
        })?;
        let index = open_database(
            &index_path,
            AUDIO_INDEX_APPLICATION_ID,
            AUDIO_INDEX_SCHEMA,
            admission,
        )?;
        let metadata = read_metadata(&index)?;
        check_audio_identity(&pack_id, &revision, &metadata)?;

        let routes = read_chunk_routes(&index)?;
        if u64::try_from(routes.len()).ok() != Some(metadata.chunk_count)
            || routes.len() != chunk_assets.len()
        {
            return Err(PackError::Corrupt(
                "audio chunk routes do not match the manifest".to_owned(),
            ));
        }
        let mut chunks = Vec::with_capacity(routes.len());
        for (position, route) in routes.into_iter().enumerate() {
            chunks.push(open_chunk(
                &pack_id,
                position,
                route,
                &mut chunk_assets,
                admission,
            )?);
        }
        if admission == Admission::Full {
            validate_recording_relations(&index, &metadata)?;
        }
        Ok(Self {
            directory: directory.to_owned(),
            metadata,
            manifest,
            limits,
            index: Mutex::new(index),
            chunks,
        })
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub const fn metadata(&self) -> &AudioMetadata {
        &self.metadata
    }

    #[must_use]
    pub const fn manifest(&self) -> &AudioManifest {
        &self.manifest
    }

    /// Returns a referenced recording by normalized Commons file name.
    ///
    /// # Errors
    ///
    /// Returns storage or consistency errors.
    pub fn recording(&self, file_name: &str) -> Result<Option<Recording>, PackError> {
        let index = lock(&self.index)?;
        let mut statement = index.prepare_cached(RECORDING_SELECT)?;
        let row = statement.query_row([file_name], raw_recording).optional()?;
        drop(statement);
        row.map(|raw| recording_from_raw(&index, raw)).transpose()
    }

    /// Reads the encoded audio of an available recording, verified against its digest.
    ///
    /// Returns `None` when the recording is not referenced or not available.
    ///
    /// # Errors
    ///
    /// Returns an error when stored bytes exceed limits or fail authentication.
    pub fn read(&self, file_name: &str) -> Result<Option<Vec<u8>>, PackError> {
        let Some(Recording {
            audio: Some(audio), ..
        }) = self.recording(file_name)?
        else {
            return Ok(None);
        };
        self.read_blob(&audio).map(Some)
    }

    fn read_blob(&self, audio: &RecordingAudio) -> Result<Vec<u8>, PackError> {
        if audio.size_bytes > self.limits.max_recording_bytes {
            return Err(PackError::Limit(
                "recording exceeds the audio byte limit".to_owned(),
            ));
        }
        let chunk = usize::try_from(audio.chunk_ordinal)
            .ok()
            .and_then(|ordinal| self.chunks.get(ordinal))
            .ok_or_else(|| PackError::Corrupt("recording routes to a missing chunk".to_owned()))?;
        let connection = lock(&chunk.connection)?;
        let bytes: Vec<u8> = connection
            .query_row(
                "SELECT bytes FROM blobs WHERE sha256 = ?1",
                [audio.sha256.as_slice()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| PackError::Corrupt("recording blob is missing".to_owned()))?;
        if bytes.len() as u64 != audio.size_bytes
            || Sha256::digest(&bytes).as_slice() != audio.sha256
        {
            return Err(PackError::Corrupt(
                "recording blob digest mismatch".to_owned(),
            ));
        }
        Ok(bytes)
    }

    /// Exhaustively authenticates every recording and blob.
    ///
    /// # Errors
    ///
    /// Returns an error when any recording, blob, stream, or total disagrees.
    pub fn validate_all(&self) -> Result<AudioValidationReport, PackError> {
        let index = lock(&self.index)?;
        let mut statement = index.prepare(&format!("{RECORDING_SELECT_ALL} ORDER BY file_name"))?;
        let raws = statement
            .query_map([], raw_recording)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut digest = recording_input_digest();
        let mut available = 0;
        let mut blobs = BTreeMap::new();
        for raw in raws {
            let recording = recording_from_raw(&index, raw)?;
            frame_recording_facts(&mut digest, &recording.facts);
            if let Some(audio) = recording.audio {
                available += 1;
                if audio.chunk_ordinal
                    != audio_chunk_for(&recording.facts.file_name, self.metadata.chunk_count)
                {
                    return Err(PackError::Corrupt(format!(
                        "recording `{}` is not in its assigned chunk",
                        recording.facts.file_name
                    )));
                }
                let bytes = self.read_blob(&audio)?;
                let duration = opus_duration_ms(&bytes)?;
                if duration != audio.duration_ms {
                    return Err(PackError::Corrupt(format!(
                        "recording `{}` duration disagrees with its stream",
                        recording.facts.file_name
                    )));
                }
                blobs.insert((audio.chunk_ordinal, audio.sha256), audio.size_bytes);
            }
        }
        let digest: [u8; 32] = digest.finalize().into();
        if digest != self.metadata.recording_input_digest
            || available != self.metadata.available_count
        {
            return Err(PackError::Corrupt(
                "recording input digest or available count mismatch".to_owned(),
            ));
        }
        let mut blob_bytes = 0;
        for chunk in &self.chunks {
            let (count, bytes) = blobs
                .iter()
                .filter(|((ordinal, _), _)| *ordinal == chunk.route.ordinal)
                .fold((0_u64, 0_u64), |(count, bytes), (_, size)| {
                    (count + 1, bytes + size)
                });
            if count != chunk.route.blob_count || bytes != chunk.route.blob_bytes {
                return Err(PackError::Corrupt(format!(
                    "audio chunk {} contains unreferenced or missing blobs",
                    chunk.route.ordinal
                )));
            }
            blob_bytes += bytes;
        }
        Ok(AudioValidationReport {
            recording_count: self.metadata.recording_count,
            available_count: available,
            blob_count: blobs.len() as u64,
            blob_bytes,
        })
    }
}

/// Returns the playback duration of an Ogg Opus stream in milliseconds, after
/// checking page framing, `OpusHead`, and `OpusTags`.
///
/// # Errors
///
/// Returns [`PackError::Corrupt`] for malformed framing or headers.
pub fn opus_duration_ms(bytes: &[u8]) -> Result<u64, PackError> {
    let corrupt = |message: &str| PackError::Corrupt(format!("invalid Ogg Opus stream: {message}"));
    let mut offset = 0;
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut pending = Vec::new();
    let mut last_granule = None;
    let mut serial = None;
    let mut ended = false;
    while offset < bytes.len() {
        if ended {
            return Err(corrupt("data after end of stream"));
        }
        let header = bytes
            .get(offset..offset + 27)
            .ok_or_else(|| corrupt("truncated page"))?;
        if &header[..4] != b"OggS" || header[4] != 0 {
            return Err(corrupt("bad page capture"));
        }
        let flags = header[5];
        let granule = header[6..14]
            .iter()
            .rev()
            .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte));
        let page_serial = header[14..18]
            .iter()
            .rev()
            .fold(0_u32, |value, byte| (value << 8) | u32::from(*byte));
        if *serial.get_or_insert(page_serial) != page_serial {
            return Err(corrupt("multiple logical streams"));
        }
        let segments = usize::from(header[26]);
        let table = bytes
            .get(offset + 27..offset + 27 + segments)
            .ok_or_else(|| corrupt("truncated segment table"))?;
        let mut position = offset + 27 + segments;
        let mut completed = false;
        for &lacing in table {
            let end = position + usize::from(lacing);
            pending.extend_from_slice(
                bytes
                    .get(position..end)
                    .ok_or_else(|| corrupt("truncated segment"))?,
            );
            position = end;
            if lacing < 255 {
                packets.push(std::mem::take(&mut pending));
                completed = true;
            }
        }
        if completed && granule != u64::MAX {
            last_granule = Some(granule);
        }
        ended = flags & 0x04 != 0;
        offset = position;
    }
    if !ended || !pending.is_empty() || packets.len() < 3 {
        return Err(corrupt("incomplete stream"));
    }
    let head = &packets[0];
    if head.len() < 19 || &head[..8] != b"OpusHead" || head[8] != 1 || head[9] != 1 {
        return Err(corrupt("expected a mono version-1 OpusHead"));
    }
    if !packets[1].starts_with(b"OpusTags") {
        return Err(corrupt("missing OpusTags"));
    }
    let pre_skip = u64::from(u16::from_le_bytes([head[10], head[11]]));
    let samples = last_granule
        .and_then(|granule| granule.checked_sub(pre_skip))
        .filter(|samples| *samples > 0)
        .ok_or_else(|| corrupt("no audio samples"))?;
    Ok(samples.div_ceil(48))
}

const RECORDING_SELECT_ALL: &str = "SELECT file_name, status, reason, reference_count, source_title, \
     source_sha1, source_timestamp, source_media_type, source_size_bytes, description_url, \
     license_ordinal, author_text, author_urls_json, attribution_required, opus_sha256, \
     opus_size_bytes, duration_ms, chunk_ordinal FROM recordings";
const RECORDING_SELECT: &str = "SELECT file_name, status, reason, reference_count, source_title, \
     source_sha1, source_timestamp, source_media_type, source_size_bytes, description_url, \
     license_ordinal, author_text, author_urls_json, attribution_required, opus_sha256, \
     opus_size_bytes, duration_ms, chunk_ordinal FROM recordings WHERE file_name = ?1";

type RawRecording = (
    String,
    i64,
    Option<String>,
    i64,
    Option<String>,
    Option<Vec<u8>>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<Vec<u8>>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn raw_recording(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRecording> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
    ))
}

fn recording_from_raw(index: &Connection, raw: RawRecording) -> Result<Recording, PackError> {
    let license = match raw.10 {
        Some(ordinal) => Some(index.query_row(
            "SELECT short_name, identifier, url FROM licenses WHERE license_ordinal = ?1",
            [ordinal],
            |row| {
                Ok(RecordingLicense {
                    short_name: row.get(0)?,
                    identifier: row.get(1)?,
                    url: row.get(2)?,
                })
            },
        )?),
        None => None,
    };
    let author_urls = match raw.12 {
        Some(json) => serde_json::from_str::<Vec<String>>(&json)
            .map_err(|error| PackError::Corrupt(format!("invalid author URLs: {error}")))?,
        None => Vec::new(),
    };
    let audio = match (raw.14, raw.15, raw.16, raw.17) {
        (Some(sha256), Some(size), Some(duration), Some(chunk)) => Some(RecordingAudio {
            sha256: bytes32("recording blob digest", &sha256)?,
            size_bytes: positive("recording size", size)?,
            duration_ms: positive("recording duration", duration)?,
            chunk_ordinal: nonnegative("recording chunk", chunk)?,
        }),
        (None, None, None, None) => None,
        _ => {
            return Err(PackError::Corrupt(
                "recording audio facts are partial".to_owned(),
            ));
        }
    };
    Ok(Recording {
        facts: RecordingFacts {
            file_name: raw.0,
            status: RecordingStatus::from_code(raw.1)?,
            reason: raw.2,
            reference_count: positive("reference count", raw.3)?,
            source_title: raw.4,
            source_sha1: raw
                .5
                .map(|bytes| {
                    <[u8; 20]>::try_from(bytes.as_slice())
                        .map_err(|_| PackError::Corrupt("source SHA-1 is not 20 bytes".to_owned()))
                })
                .transpose()?,
            source_timestamp: raw.6,
            source_media_type: raw.7,
            source_size_bytes: raw
                .8
                .map(|size| nonnegative("source size", size))
                .transpose()?,
            description_url: raw.9,
            license,
            author_text: raw.11,
            author_urls,
            attribution_required: raw.13.map(|value| value == 1),
        },
        audio,
    })
}

fn read_audio_manifest(
    directory: &Path,
    expected: &ExpectedPack,
    limits: PackLimits,
) -> Result<(AudioManifest, PackId, PackRevision), PackError> {
    let manifest_bytes = read_bounded(
        &directory.join(AUDIO_MANIFEST_FILE),
        limits.max_manifest_bytes,
    )?;
    if Sha256::digest(&manifest_bytes).as_slice() != expected.manifest_sha256.as_bytes() {
        return Err(PackError::Corrupt(
            "audio manifest SHA-256 differs from the expected pack".to_owned(),
        ));
    }
    let manifest: AudioManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| PackError::Malformed(format!("invalid audio manifest: {error}")))?;
    let pack_id = PackId::from_str(&manifest.pack_id)?;
    let revision = PackRevision::from_bytes(*manifest.pack_revision.as_bytes());
    if manifest.manifest_version != 1 || manifest.format_version != FORMAT_VERSION {
        return Err(PackError::Malformed(
            "unsupported audio manifest or format version".to_owned(),
        ));
    }
    if expected.pack_id != pack_id || expected.pack_revision != revision {
        return Err(PackError::Corrupt(
            "audio collection identity differs from the expected pack".to_owned(),
        ));
    }
    if manifest.assets.len() > limits.max_assets {
        return Err(PackError::Limit(
            "audio manifest declares too many assets".to_owned(),
        ));
    }
    Ok((manifest, pack_id, revision))
}

fn open_chunk(
    pack_id: &PackId,
    position: usize,
    route: ChunkRoute,
    chunk_assets: &mut BTreeMap<String, (PathBuf, PackAsset)>,
    admission: Admission,
) -> Result<Chunk, PackError> {
    let expected_name = audio_chunk_file_name(pack_id, route.ordinal, &route.file_sha256);
    let (path, asset) = chunk_assets.remove(&route.file_name).ok_or_else(|| {
        PackError::Corrupt(format!(
            "index routes undeclared chunk `{}`",
            route.file_name
        ))
    })?;
    if route.ordinal != position as u64
        || route.file_name != expected_name
        || route.file_size != asset.size_bytes
        || route.file_sha256 != *asset.sha256.as_bytes()
    {
        return Err(PackError::Corrupt(format!(
            "audio chunk route {} disagrees with its asset",
            route.ordinal
        )));
    }
    let connection = open_database(
        &path,
        AUDIO_CHUNK_APPLICATION_ID,
        AUDIO_CHUNK_SCHEMA,
        admission,
    )?;
    let (chunk_pack_id, chunk_ordinal, blob_count): (String, i64, i64) = connection.query_row(
        "SELECT pack_id, chunk_ordinal, blob_count FROM chunk_metadata \
             WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if chunk_pack_id != pack_id.as_str()
        || nonnegative("chunk ordinal", chunk_ordinal)? != route.ordinal
        || nonnegative("chunk blob count", blob_count)? != route.blob_count
    {
        return Err(PackError::Corrupt(format!(
            "audio chunk {} metadata disagrees with its route",
            route.ordinal
        )));
    }
    if admission == Admission::Full {
        let (count, bytes): (i64, Option<i64>) = connection.query_row(
            "SELECT count(*), sum(length(bytes)) FROM blobs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if nonnegative("blob count", count)? != route.blob_count
            || nonnegative("blob bytes", bytes.unwrap_or(0))? != route.blob_bytes
        {
            return Err(PackError::Corrupt(format!(
                "audio chunk {} totals disagree with its route",
                route.ordinal
            )));
        }
    }
    Ok(Chunk {
        route,
        connection: Mutex::new(connection),
    })
}

fn check_audio_identity(
    pack_id: &PackId,
    revision: &PackRevision,
    metadata: &AudioMetadata,
) -> Result<(), PackError> {
    let derived = PackRevision::derive_audio(AudioRevisionInputs {
        pack_id,
        corpus_language: &metadata.corpus_language,
        source_corpus_pack_id: &PackId::from_str(&metadata.source_corpus_pack_id)?,
        source_corpus_revision: &PackRevision::from_bytes(metadata.source_corpus_revision),
        acquired_at: &metadata.acquired_at,
        encoder_profile: &metadata.encoder_profile,
        builder_revision: &metadata.builder_revision,
        chunk_count: metadata.chunk_count,
        recording_input_digest: &metadata.recording_input_digest,
        minimum_app_version: &metadata.minimum_app_version,
    });
    if metadata.pack_id != pack_id.as_str()
        || &metadata.revision != revision.as_bytes()
        || derived != *revision
        || metadata.encoder_profile != AUDIO_ENCODER_PROFILE
    {
        return Err(PackError::Corrupt(
            "audio manifest and index metadata disagree".to_owned(),
        ));
    }
    Ok(())
}

fn read_metadata(index: &Connection) -> Result<AudioMetadata, PackError> {
    let count: i64 = index.query_row("SELECT count(*) FROM collection_metadata", [], |row| {
        row.get(0)
    })?;
    if count != 1 {
        return Err(PackError::Corrupt(
            "audio index must contain one metadata row".to_owned(),
        ));
    }
    let raw = index.query_row(
        "SELECT pack_id, pack_revision, corpus_language, source_corpus_pack_id, \
         source_corpus_revision, acquired_at, encoder_profile, builder_revision, chunk_count, \
         recording_count, available_count, recording_input_digest, minimum_app_version \
         FROM collection_metadata WHERE singleton = 1",
        [],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, Vec<u8>>(11)?,
                row.get::<_, String>(12)?,
            ))
        },
    )?;
    Ok(AudioMetadata {
        pack_id: raw.0,
        revision: bytes32("audio revision", &raw.1)?,
        corpus_language: raw.2,
        source_corpus_pack_id: raw.3,
        source_corpus_revision: bytes32("source corpus revision", &raw.4)?,
        acquired_at: raw.5,
        encoder_profile: raw.6,
        builder_revision: raw.7,
        chunk_count: positive("chunk count", raw.8)?,
        recording_count: nonnegative("recording count", raw.9)?,
        available_count: nonnegative("available count", raw.10)?,
        recording_input_digest: bytes32("recording input digest", &raw.11)?,
        minimum_app_version: raw.12,
    })
}

fn read_chunk_routes(index: &Connection) -> Result<Vec<ChunkRoute>, PackError> {
    let mut statement = index.prepare(
        "SELECT chunk_ordinal, file_name, file_sha256, file_size_bytes, blob_count, blob_bytes \
         FROM chunks ORDER BY chunk_ordinal",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    rows.map(|row| {
        let row = row?;
        Ok(ChunkRoute {
            ordinal: nonnegative("chunk ordinal", row.0)?,
            file_name: row.1,
            file_sha256: bytes32("chunk SHA-256", &row.2)?,
            file_size: positive("chunk size", row.3)?,
            blob_count: nonnegative("chunk blob count", row.4)?,
            blob_bytes: nonnegative("chunk blob bytes", row.5)?,
        })
    })
    .collect()
}

fn validate_recording_relations(
    index: &Connection,
    metadata: &AudioMetadata,
) -> Result<(), PackError> {
    let foreign_keys: i64 =
        index.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    let (recordings, available): (i64, i64) = index.query_row(
        "SELECT count(*), coalesce(sum(status = 1), 0) FROM recordings",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if foreign_keys != 0
        || nonnegative("recording count", recordings)? != metadata.recording_count
        || nonnegative("available count", available)? != metadata.available_count
    {
        return Err(PackError::Corrupt(
            "audio recording relations or totals are inconsistent".to_owned(),
        ));
    }
    Ok(())
}
