use std::{fmt, str::FromStr};

use sha2::{Digest, Sha256};

use crate::{PackError, UNICODE_PROFILE};

const PACK_REVISION_DOMAIN: &[u8] = b"ELDICT-PACK-REVISION-V1";
const AUDIO_REVISION_DOMAIN: &[u8] = b"ELDICT-AUDIO-REVISION-V1";
const ENTRY_ID_DOMAIN: &[u8] = b"ELDICT-ENTRY-ID-V1";
const SNAPSHOT_REVISION_DOMAIN: &[u8] = b"ELDICT-SNAPSHOT-V1";
const MAX_PACK_ID_BYTES: usize = 64;

/// A stable, path-safe corpus identity independent of a pack revision.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackId(String);

impl PackId {
    /// Returns the validated identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for PackId {
    type Err = PackError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid_length = !value.is_empty() && value.len() <= MAX_PACK_ID_BYTES;
        let valid_edges = value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            && value
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric);
        let valid_characters = value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        let valid_separators = !value.as_bytes().windows(2).any(|pair| pair == b"--");

        if valid_length && valid_edges && valid_characters && valid_separators {
            Ok(Self(value.to_owned()))
        } else {
            Err(PackError::InvalidPackId(value.to_owned()))
        }
    }
}

/// Inputs that define one immutable physical corpus revision.
#[derive(Clone, Copy, Debug)]
pub struct PackRevisionInputs<'a> {
    pub pack_id: &'a PackId,
    pub corpus_language: &'a str,
    pub wiktionary_edition: &'a str,
    pub wiktionary_dump_date: &'a str,
    pub kaikki_extraction_date: &'a str,
    pub source_url: &'a str,
    pub compressed_source_sha256: &'a [u8; 32],
    pub uncompressed_source_sha256: &'a [u8; 32],
    pub wiktextract_revision: &'a str,
    pub wikitextprocessor_revision: &'a str,
    pub builder_revision: &'a str,
    pub compression_profile: &'a str,
    pub routing_policy: &'a str,
    pub target_shard_payload_bytes: u64,
    pub source_manifest_sha256: &'a [u8; 32],
    pub license_manifest_sha256: &'a [u8; 32],
    pub compatible_audio_collection: Option<&'a str>,
    pub minimum_app_version: &'a str,
}

/// Inputs that define one immutable audio collection revision.
#[derive(Clone, Copy, Debug)]
pub struct AudioRevisionInputs<'a> {
    pub pack_id: &'a PackId,
    pub corpus_language: &'a str,
    pub source_corpus_pack_id: &'a PackId,
    pub source_corpus_revision: &'a PackRevision,
    pub acquired_at: &'a str,
    pub encoder_profile: &'a str,
    pub builder_revision: &'a str,
    pub chunk_count: u64,
    /// Digest of every recording's acquired facts, in file name order.
    pub recording_input_digest: &'a [u8; 32],
    pub minimum_app_version: &'a str,
}

/// The digest identity of one immutable pack revision.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackRevision([u8; 32]);

impl PackRevision {
    /// Derives a revision from every format input that can affect pack behavior.
    #[must_use]
    pub fn derive(inputs: PackRevisionInputs<'_>) -> Self {
        let mut digest = FramedDigest::new(PACK_REVISION_DOMAIN);
        digest.field(&crate::FORMAT_VERSION.to_be_bytes());
        digest.field(inputs.pack_id.as_str().as_bytes());
        digest.field(inputs.corpus_language.as_bytes());
        digest.field(inputs.wiktionary_edition.as_bytes());
        digest.field(inputs.wiktionary_dump_date.as_bytes());
        digest.field(inputs.kaikki_extraction_date.as_bytes());
        digest.field(inputs.source_url.as_bytes());
        digest.field(inputs.compressed_source_sha256);
        digest.field(inputs.uncompressed_source_sha256);
        digest.field(inputs.wiktextract_revision.as_bytes());
        digest.field(inputs.wikitextprocessor_revision.as_bytes());
        digest.field(inputs.builder_revision.as_bytes());
        digest.field(UNICODE_PROFILE.as_bytes());
        digest.field(inputs.compression_profile.as_bytes());
        digest.field(inputs.routing_policy.as_bytes());
        digest.field(&inputs.target_shard_payload_bytes.to_be_bytes());
        digest.field(inputs.source_manifest_sha256);
        digest.field(inputs.license_manifest_sha256);
        digest.field(inputs.compatible_audio_collection.unwrap_or("").as_bytes());
        digest.field(inputs.minimum_app_version.as_bytes());
        Self(digest.finish())
    }

    /// Derives an audio collection revision from every input that affects its bytes.
    #[must_use]
    pub fn derive_audio(inputs: AudioRevisionInputs<'_>) -> Self {
        let mut digest = FramedDigest::new(AUDIO_REVISION_DOMAIN);
        digest.field(&crate::FORMAT_VERSION.to_be_bytes());
        digest.field(inputs.pack_id.as_str().as_bytes());
        digest.field(inputs.corpus_language.as_bytes());
        digest.field(inputs.source_corpus_pack_id.as_str().as_bytes());
        digest.field(inputs.source_corpus_revision.as_bytes());
        digest.field(inputs.acquired_at.as_bytes());
        digest.field(inputs.encoder_profile.as_bytes());
        digest.field(inputs.builder_revision.as_bytes());
        digest.field(&inputs.chunk_count.to_be_bytes());
        digest.field(inputs.recording_input_digest);
        digest.field(inputs.minimum_app_version.as_bytes());
        Self(digest.finish())
    }

    /// Creates a revision from an already validated digest.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the binary digest used in databases and manifests.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the lowercase hexadecimal representation.
    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl From<[u8; 32]> for PackRevision {
    fn from(value: [u8; 32]) -> Self {
        Self(value)
    }
}

impl fmt::Display for PackRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

/// A stable record identity scoped to one pack revision.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntryId([u8; 32]);

impl EntryId {
    /// Derives an entry identity from its source position and exact record bytes.
    #[must_use]
    pub fn derive(
        pack_revision: &PackRevision,
        source_line_number: u64,
        record_sha256: &[u8; 32],
    ) -> Self {
        let mut digest = FramedDigest::new(ENTRY_ID_DOMAIN);
        digest.field(pack_revision.as_bytes());
        digest.field(&source_line_number.to_be_bytes());
        digest.field(record_sha256);
        Self(digest.finish())
    }

    /// Returns the binary digest used in databases and entry references.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Creates an entry identifier from validated pack storage bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the lowercase hexadecimal representation.
    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

/// The projection represented by an enabled dictionary view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewType {
    Monolingual,
    Bilingual,
}

/// Identity facts for one ordered enabled view.
#[derive(Clone, Copy, Debug)]
pub struct SnapshotViewIdentity<'a> {
    pub view_id: &'a str,
    pub pack_id: &'a PackId,
    pub pack_revision: &'a PackRevision,
    pub view_type: ViewType,
    pub target_language: Option<&'a str>,
}

/// The digest of an immutable ordered view configuration.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SnapshotRevision([u8; 32]);

impl SnapshotRevision {
    /// Derives a revision while preserving view order.
    #[must_use]
    pub fn derive(views: &[SnapshotViewIdentity<'_>]) -> Self {
        let mut digest = FramedDigest::new(SNAPSHOT_REVISION_DOMAIN);
        digest.field(&(views.len() as u64).to_be_bytes());
        for view in views {
            digest.field(view.view_id.as_bytes());
            digest.field(view.pack_id.as_str().as_bytes());
            digest.field(view.pack_revision.as_bytes());
            digest.field(match view.view_type {
                ViewType::Monolingual => b"monolingual",
                ViewType::Bilingual => b"bilingual",
            });
            digest.field(view.target_language.unwrap_or("").as_bytes());
        }
        Self(digest.finish())
    }

    /// Returns the binary snapshot digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the lowercase hexadecimal representation.
    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

struct FramedDigest(Sha256);

impl FramedDigest {
    fn new(domain: &[u8]) -> Self {
        let mut value = Self(Sha256::new());
        value.field(domain);
        value
    }

    fn field(&mut self, value: &[u8]) {
        self.0.update((value.len() as u64).to_be_bytes());
        self.0.update(value);
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn pack_inputs<'a>(pack_id: &'a PackId, source: &'a [u8; 32]) -> PackRevisionInputs<'a> {
        PackRevisionInputs {
            pack_id,
            corpus_language: "en",
            wiktionary_edition: "en",
            wiktionary_dump_date: "2026-09-02",
            kaikki_extraction_date: "2026-09-06",
            source_url: "https://kaikki.org/dictionary/raw-wiktextract-data.jsonl.gz",
            compressed_source_sha256: source,
            uncompressed_source_sha256: &[2; 32],
            wiktextract_revision: "ccec6f1",
            wikitextprocessor_revision: "4deed51",
            builder_revision: "builder-v1",
            compression_profile: "zstd-v1-level-6",
            routing_policy: "routing-v1",
            target_shard_payload_bytes: 1_610_612_736,
            source_manifest_sha256: &[3; 32],
            license_manifest_sha256: &[4; 32],
            compatible_audio_collection: None,
            minimum_app_version: "0.1.0",
        }
    }

    #[test]
    fn pack_ids_are_conservative_and_path_safe() {
        assert!(PackId::from_str("wiktionary-en-en").is_ok());
        for invalid in ["", "-english", "english-", "English", "en_us", "en--us"] {
            assert!(PackId::from_str(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn every_pack_input_is_revision_significant() {
        let pack_id = PackId::from_str("wiktionary-en-en").unwrap();
        let first = PackRevision::derive(pack_inputs(&pack_id, &[1; 32]));
        let second = PackRevision::derive(pack_inputs(&pack_id, &[9; 32]));
        assert_ne!(first, second);
        assert_eq!(first.to_hex().len(), 64);
    }

    #[test]
    fn entry_identity_preserves_duplicate_source_positions() {
        let revision = PackRevision::from_bytes([1; 32]);
        let record = [2; 32];
        assert_ne!(
            EntryId::derive(&revision, 10, &record),
            EntryId::derive(&revision, 11, &record)
        );
    }

    #[test]
    fn snapshot_identity_preserves_order_and_view_semantics() {
        let pack_id = PackId::from_str("wiktionary-en-en").unwrap();
        let revision = PackRevision::from_bytes([1; 32]);
        let mono = SnapshotViewIdentity {
            view_id: "english",
            pack_id: &pack_id,
            pack_revision: &revision,
            view_type: ViewType::Monolingual,
            target_language: None,
        };
        let bilingual = SnapshotViewIdentity {
            view_id: "english-to-fr",
            pack_id: &pack_id,
            pack_revision: &revision,
            view_type: ViewType::Bilingual,
            target_language: Some("fr"),
        };
        assert_ne!(
            SnapshotRevision::derive(&[mono, bilingual]),
            SnapshotRevision::derive(&[bilingual, mono])
        );
    }
}
