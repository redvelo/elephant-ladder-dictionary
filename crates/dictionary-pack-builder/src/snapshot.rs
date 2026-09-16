use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{BuildManifest, Sha256Hex};
use elephant_ladder_dictionary_pack::{COMPRESSION_PROFILE, PackId, ROUTING_POLICY};

/// Stable, hand-maintained facts about one Wiktionary edition corpus.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditionConfig {
    pub schema_version: u32,
    pub wiktionary_edition: String,
    pub corpus_language: String,
    pub pack_id: String,
    pub origin: EditionOrigin,
    pub target_shard_payload_bytes: u64,
    pub minimum_app_version: String,
    pub license_manifest: Value,
    pub audio: Option<AudioEditionConfig>,
}

/// Stable facts about an edition's audio collection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AudioEditionConfig {
    pub pack_id: String,
    /// Fixed so recordings keep their chunk across collection revisions.
    pub chunk_count: u64,
}

/// Where Kaikki publishes the mutable latest extraction of an edition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditionOrigin {
    pub source_url: String,
    pub info_url: String,
}

/// An immutable record of one captured and mirrored source archive.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSnapshot {
    pub schema_version: u32,
    pub wiktionary_edition: String,
    pub captured_at: String,
    pub origin: CapturedOrigin,
    pub dump_date: String,
    pub extraction_date: String,
    pub wiktextract_revision: String,
    pub wikitextprocessor_revision: String,
    pub compressed: SourceDigest,
    pub uncompressed: UncompressedSource,
    pub mirror: SourceMirror,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedOrigin {
    pub source_url: String,
    pub info_url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// The information page sentence the dates and revisions were read from.
    pub info_statement: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDigest {
    pub size_bytes: u64,
    pub sha256: Sha256Hex,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UncompressedSource {
    pub size_bytes: u64,
    pub sha256: Sha256Hex,
    pub line_count: u64,
}

/// Release assets that reproduce the captured archive when concatenated in order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMirror {
    pub repository: String,
    pub release_tag: String,
    pub parts: Vec<MirrorAsset>,
    pub info_page: MirrorAsset,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorAsset {
    pub file_name: String,
    pub size_bytes: u64,
    pub sha256: Sha256Hex,
}

impl MirrorAsset {
    #[must_use]
    pub fn url(&self, mirror: &SourceMirror) -> String {
        format!(
            "https://github.com/{}/releases/download/{}/{}",
            mirror.repository, mirror.release_tag, self.file_name
        )
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("invalid source snapshot: {0}")]
pub struct SnapshotError(String);

/// The largest mirrored part, below GitHub's 2 GiB release asset limit.
pub const MAX_MIRROR_PART_BYTES: u64 = 1024 * 1024 * 1024;

/// Checks a snapshot against its edition and its own internal consistency.
///
/// # Errors
///
/// Returns the first inconsistency found.
pub fn validate_snapshot(
    edition: &EditionConfig,
    snapshot: &SourceSnapshot,
) -> Result<(), SnapshotError> {
    let fail = |message: &str| Err(SnapshotError(message.to_owned()));
    if edition.schema_version != 1 || snapshot.schema_version != 1 {
        return fail("unsupported schema version");
    }
    PackId::from_str(&edition.pack_id).map_err(|error| SnapshotError(error.to_string()))?;
    if snapshot.wiktionary_edition != edition.wiktionary_edition
        || snapshot.origin.source_url != edition.origin.source_url
        || snapshot.origin.info_url != edition.origin.info_url
    {
        return fail("snapshot does not belong to this edition configuration");
    }
    for (name, value) in [
        ("captured_at", &snapshot.captured_at),
        ("dump_date", &snapshot.dump_date),
        ("extraction_date", &snapshot.extraction_date),
    ] {
        if !is_iso_date_prefix(value) {
            return Err(SnapshotError(format!("`{name}` is not an ISO date")));
        }
    }
    if snapshot.extraction_date < snapshot.dump_date {
        return fail("extraction predates its dump");
    }
    if snapshot.wiktextract_revision.is_empty() || snapshot.wikitextprocessor_revision.is_empty() {
        return fail("extraction revisions must not be empty");
    }

    let mirror = &snapshot.mirror;
    let valid_repository = mirror.repository.split('/').count() == 2
        && mirror
            .repository
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'));
    let expected_tag = format!(
        "source-{}-{}-{}",
        snapshot.wiktionary_edition,
        snapshot.dump_date.replace('-', ""),
        &snapshot.compressed.sha256.to_hex()[..12]
    );
    if !valid_repository || mirror.release_tag != expected_tag {
        return Err(SnapshotError(format!(
            "mirror must be an owner/name repository with release tag `{expected_tag}`"
        )));
    }
    let mut names = std::collections::BTreeSet::new();
    let assets = mirror
        .parts
        .iter()
        .chain(std::iter::once(&mirror.info_page));
    for asset in assets {
        let safe = !asset.file_name.is_empty()
            && !asset.file_name.starts_with('.')
            && asset
                .file_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        if !safe || !names.insert(asset.file_name.as_str()) || asset.size_bytes == 0 {
            return fail("mirror asset names must be safe, unique, and non-empty");
        }
    }
    if mirror.parts.is_empty()
        || mirror
            .parts
            .iter()
            .any(|part| part.size_bytes > MAX_MIRROR_PART_BYTES)
        || mirror
            .parts
            .iter()
            .try_fold(0_u64, |total, part| total.checked_add(part.size_bytes))
            != Some(snapshot.compressed.size_bytes)
    {
        return fail("mirror parts must be bounded and sum to the compressed source size");
    }
    Ok(())
}

/// Derives the fully pinned build manifest for a snapshot.
///
/// # Errors
///
/// Returns an error when the snapshot is invalid for the edition.
pub fn build_manifest(
    edition: &EditionConfig,
    snapshot: &SourceSnapshot,
    builder_revision: &str,
) -> Result<BuildManifest, SnapshotError> {
    validate_snapshot(edition, snapshot)?;
    if builder_revision.is_empty() {
        return Err(SnapshotError(
            "builder revision must not be empty".to_owned(),
        ));
    }
    let source_manifest = json!({
        "schema_version": 2,
        "provider": "Kaikki.org",
        "wiktionary_edition": snapshot.wiktionary_edition,
        "selected_entry_language": edition.corpus_language,
        "snapshot": snapshot,
    });
    Ok(BuildManifest {
        pack_id: edition.pack_id.clone(),
        corpus_language: edition.corpus_language.clone(),
        wiktionary_edition: edition.wiktionary_edition.clone(),
        wiktionary_dump_date: snapshot.dump_date.clone(),
        kaikki_extraction_date: snapshot.extraction_date.clone(),
        source_url: snapshot.origin.source_url.clone(),
        compressed_source_sha256: snapshot.compressed.sha256,
        uncompressed_source_sha256: snapshot.uncompressed.sha256,
        wiktextract_revision: snapshot.wiktextract_revision.clone(),
        wikitextprocessor_revision: snapshot.wikitextprocessor_revision.clone(),
        builder_revision: builder_revision.to_owned(),
        compression_profile: COMPRESSION_PROFILE.to_owned(),
        routing_policy: ROUTING_POLICY.to_owned(),
        target_shard_payload_bytes: edition.target_shard_payload_bytes,
        source_manifest_sha256: json_digest(&source_manifest),
        source_manifest,
        license_manifest_sha256: json_digest(&edition.license_manifest),
        license_manifest: edition.license_manifest.clone(),
        compatible_audio_collection: None,
        minimum_app_version: edition.minimum_app_version.clone(),
    })
}

fn json_digest(value: &Value) -> Sha256Hex {
    let json = serde_json::to_string(value).expect("JSON values serialize");
    Sha256Hex::from_bytes(Sha256::digest(json.as_bytes()).into())
}

fn is_iso_date_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 10
        && bytes[..10].iter().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}
