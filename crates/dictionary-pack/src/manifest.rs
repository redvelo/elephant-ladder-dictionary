use std::{fmt, str::FromStr};

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The fixed name of the format-v1 output manifest.
pub const PACK_MANIFEST_FILE: &str = "pack-manifest-v1.json";

/// A SHA-256 value serialized as exactly 64 lowercase hexadecimal characters.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Sha256Hex([u8; 32]);

impl Sha256Hex {
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

impl fmt::Display for Sha256Hex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

impl FromStr for Sha256Hex {
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
        hex::decode_to_slice(value, &mut bytes).map_err(|_| "invalid SHA-256 hexadecimal value")?;
        Ok(Self(bytes))
    }
}

impl Serialize for Sha256Hex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(self.0))
    }
}

impl<'de> Deserialize<'de> for Sha256Hex {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HexVisitor;

        impl Visitor<'_> for HexVisitor {
            type Value = Sha256Hex;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("exactly 64 lowercase hexadecimal SHA-256 characters")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                value
                    .parse()
                    .map_err(|message: &'static str| E::custom(message))
            }
        }

        deserializer.deserialize_str(HexVisitor)
    }
}

/// One immutable asset in `pack-manifest-v1.json`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackAsset {
    pub role: String,
    pub file_name: String,
    pub sha256: Sha256Hex,
    pub size_bytes: u64,
}

/// Deterministic release manifest emitted beside the `SQLite` assets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub manifest_version: u32,
    pub format_version: u32,
    pub pack_id: String,
    pub pack_revision: Sha256Hex,
    pub selected_record_count: u64,
    pub selected_record_bytes: u64,
    pub selected_record_digest: Sha256Hex,
    pub assets: Vec<PackAsset>,
}

pub type AssetManifest = PackManifest;
