use serde::{Deserialize, Serialize};

use crate::{PackId, PackRevision, Sha256Hex};

/// The fixed name of the format-v1 corpus pack manifest.
pub const PACK_MANIFEST_FILE: &str = "pack-manifest-v1.json";

/// One immutable asset in a pack manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackAsset {
    pub role: String,
    pub file_name: String,
    pub sha256: Sha256Hex,
    pub size_bytes: u64,
}

/// Deterministic release manifest emitted beside the corpus `SQLite` assets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub manifest_version: u32,
    pub format_version: u32,
    pub pack_id: PackId,
    pub pack_revision: PackRevision,
    pub selected_record_count: u64,
    pub selected_record_bytes: u64,
    pub selected_record_digest: Sha256Hex,
    pub assets: Vec<PackAsset>,
}
