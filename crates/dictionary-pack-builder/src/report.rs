use serde::{Deserialize, Serialize};

use crate::Sha256Hex;

/// The fixed name of deterministic builder qualification evidence.
pub const BUILD_REPORT_FILE: &str = "build-report-v1.json";

/// Deterministic qualification evidence emitted by a successful build.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildReport {
    pub schema_version: u32,
    pub pack_id: String,
    pub pack_revision: Sha256Hex,
    pub source: SourceReport,
    pub selected: SelectedReport,
    pub lookup_row_count: u64,
    pub index_asset: ReportAsset,
    pub data_assets: Vec<ReportAsset>,
    pub shard_count: u64,
    pub compressed_payload_bytes: u64,
    pub compression_ratio: CompressionRatio,
    pub largest_selected_records: Vec<LargestSelectedRecord>,
    pub translation_coverage: TranslationCoverage,
    pub source_shape_observations: Vec<SourceShapeObservation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReport {
    pub record_count: u64,
    pub size_bytes: u64,
    pub sha256: Sha256Hex,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedReport {
    pub record_count: u64,
    pub size_bytes: u64,
    pub digest: Sha256Hex,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReportAsset {
    pub file_name: String,
    pub size_bytes: u64,
    pub sha256: Sha256Hex,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompressionRatio {
    pub uncompressed_bytes: u64,
    pub compressed_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LargestSelectedRecord {
    pub selected_ordinal: u64,
    pub source_line: u64,
    pub size_bytes: u64,
    pub sha256: Sha256Hex,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationCoverage {
    pub selected_records_with_top_level_translation_array: u64,
    pub top_level_translation_items: u64,
    pub sense_objects: u64,
    pub sense_objects_with_translation_array: u64,
    pub sense_translation_items: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceShapeObservation {
    pub json_path: String,
    pub value_shape: String,
    pub occurrence_count: u64,
    pub first_source_line: u64,
}

impl From<&elephant_ladder_dictionary_pack::PackAsset> for ReportAsset {
    fn from(asset: &elephant_ladder_dictionary_pack::PackAsset) -> Self {
        Self {
            file_name: asset.file_name.clone(),
            size_bytes: asset.size_bytes,
            sha256: asset.sha256,
        }
    }
}
