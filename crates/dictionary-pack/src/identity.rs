use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};

use crate::{COMPRESSION_PROFILE, FORMAT_VERSION, PackError, ROUTING_POLICY, UNICODE_PROFILE};

const PACK_REVISION_DOMAIN: &[u8] = b"ELDICT-PACK-REVISION-V1";
const ENTRY_ID_DOMAIN: &[u8] = b"ELDICT-ENTRY-ID-V1";
const MAX_PACK_ID_BYTES: usize = 64;

/// A stable, path-safe pack identity independent of a pack revision.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackId(String);

impl PackId {
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

impl Serialize for PackId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PackId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

macro_rules! digest_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name([u8; 32]);

        impl $name {
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            #[must_use]
            pub fn to_hex(self) -> String {
                hex::encode(self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}({})", stringify!($name), hex::encode(self.0))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&hex::encode(self.0))
            }
        }

        impl FromStr for $name {
            type Err = &'static str;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                if value.len() != 64
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err("expected exactly 64 lowercase hexadecimal SHA-256 characters");
                }
                let mut bytes = [0; 32];
                hex::decode_to_slice(value, &mut bytes)
                    .map_err(|_| "invalid SHA-256 hexadecimal value")?;
                Ok(Self(bytes))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&hex::encode(self.0))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(de::Error::custom)
            }
        }
    };
}

digest_type! {
    /// A SHA-256 value serialized as exactly 64 lowercase hexadecimal characters.
    Sha256Hex
}

digest_type! {
    /// The digest identity of one immutable pack revision.
    PackRevision
}

digest_type! {
    /// A stable record identity scoped to one pack revision.
    EntryId
}

digest_type! {
    /// The digest of an immutable ordered view configuration.
    SnapshotRevision
}

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
}

/// The pinned inputs that define one immutable corpus pack revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorpusIdentity {
    pub pack_id: PackId,
    pub corpus_language: String,
    pub wiktionary_edition: String,
    pub wiktionary_dump_date: String,
    pub kaikki_extraction_date: String,
    pub source_url: String,
    pub compressed_source_sha256: Sha256Hex,
    pub uncompressed_source_sha256: Sha256Hex,
    pub wiktextract_revision: String,
    pub wikitextprocessor_revision: String,
    pub builder_revision: String,
    pub target_shard_payload_bytes: u64,
    /// Compact JSON describing the source extraction.
    pub source_manifest: String,
    /// Compact JSON describing the corpus licenses.
    pub license_manifest: String,
    pub minimum_app_version: String,
}

impl CorpusIdentity {
    /// Derives the revision from every input that can affect pack behavior, including
    /// the format version and the Unicode, compression, and routing profiles.
    #[must_use]
    pub fn revision(&self) -> PackRevision {
        let mut digest = FramedDigest::new(PACK_REVISION_DOMAIN);
        digest.field(&FORMAT_VERSION.to_be_bytes());
        digest.field(self.pack_id.as_str().as_bytes());
        digest.field(self.corpus_language.as_bytes());
        digest.field(self.wiktionary_edition.as_bytes());
        digest.field(self.wiktionary_dump_date.as_bytes());
        digest.field(self.kaikki_extraction_date.as_bytes());
        digest.field(self.source_url.as_bytes());
        digest.field(self.compressed_source_sha256.as_bytes());
        digest.field(self.uncompressed_source_sha256.as_bytes());
        digest.field(self.wiktextract_revision.as_bytes());
        digest.field(self.wikitextprocessor_revision.as_bytes());
        digest.field(self.builder_revision.as_bytes());
        digest.field(UNICODE_PROFILE.as_bytes());
        digest.field(COMPRESSION_PROFILE.as_bytes());
        digest.field(ROUTING_POLICY.as_bytes());
        digest.field(&self.target_shard_payload_bytes.to_be_bytes());
        digest.field(self.source_manifest_sha256().as_bytes());
        digest.field(self.license_manifest_sha256().as_bytes());
        digest.field(b"");
        digest.field(self.minimum_app_version.as_bytes());
        PackRevision(digest.finish())
    }

    #[must_use]
    pub fn source_manifest_sha256(&self) -> Sha256Hex {
        Sha256Hex(Sha256::digest(self.source_manifest.as_bytes()).into())
    }

    #[must_use]
    pub fn license_manifest_sha256(&self) -> Sha256Hex {
        Sha256Hex(Sha256::digest(self.license_manifest.as_bytes()).into())
    }
}

/// A SHA-256 over length-prefixed fields, started with a domain separator.
pub struct FramedDigest(Sha256);

impl FramedDigest {
    #[must_use]
    pub fn new(domain: &[u8]) -> Self {
        let mut digest = Self(Sha256::new());
        digest.field(domain);
        digest
    }

    pub fn field(&mut self, bytes: &[u8]) {
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
    }

    /// Frames presence, then the field when present.
    pub fn optional(&mut self, bytes: Option<&[u8]>) {
        match bytes {
            Some(bytes) => {
                self.0.update([1]);
                self.field(bytes);
            }
            None => self.0.update([0]),
        }
    }

    #[must_use]
    pub fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(compressed: [u8; 32]) -> CorpusIdentity {
        CorpusIdentity {
            pack_id: "wiktionary-en-en".parse().unwrap(),
            corpus_language: "en".to_owned(),
            wiktionary_edition: "enwiktionary".to_owned(),
            wiktionary_dump_date: "2026-09-02".to_owned(),
            kaikki_extraction_date: "2026-09-06".to_owned(),
            source_url: "https://kaikki.org/dictionary/raw-wiktextract-data.jsonl.gz".to_owned(),
            compressed_source_sha256: Sha256Hex::from_bytes(compressed),
            uncompressed_source_sha256: Sha256Hex::from_bytes([2; 32]),
            wiktextract_revision: "ccec6f1".to_owned(),
            wikitextprocessor_revision: "4deed51".to_owned(),
            builder_revision: "builder-v1".to_owned(),
            target_shard_payload_bytes: 1_610_612_736,
            source_manifest: "{\"provider\":\"test\"}".to_owned(),
            license_manifest: "{\"licenses\":[]}".to_owned(),
            minimum_app_version: "0.1.0".to_owned(),
        }
    }

    #[test]
    fn pack_ids_are_conservative_and_path_safe() {
        assert!(PackId::from_str("wiktionary-en-en").is_ok());
        for invalid in ["", "-english", "english-", "English", "en_us", "en--us"] {
            assert!(PackId::from_str(invalid).is_err(), "accepted {invalid}");
            assert!(serde_json::from_str::<PackId>(&format!("\"{invalid}\"")).is_err());
        }
    }

    #[test]
    fn every_pack_input_is_revision_significant() {
        let first = identity([1; 32]).revision();
        let second = identity([9; 32]).revision();
        assert_ne!(first, second);
        assert_eq!(first.to_hex().len(), 64);
        assert_eq!(first.to_hex().parse::<PackRevision>().unwrap(), first);
    }

    #[test]
    fn digests_round_trip_through_lowercase_hex_only() {
        let hex = "0".repeat(63) + "a";
        assert_eq!(hex.parse::<Sha256Hex>().unwrap().to_hex(), hex);
        assert!(hex.to_uppercase().parse::<Sha256Hex>().is_err());
        assert!("00".parse::<Sha256Hex>().is_err());
        assert_eq!(
            serde_json::to_string(&hex.parse::<EntryId>().unwrap()).unwrap(),
            format!("\"{hex}\"")
        );
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
}
