//! Maintenance-time construction and qualification of dictionary packs.

mod builder;
mod manifest;
mod report;
mod source;

pub use builder::{BuildError, BuildOptions, BuildResult, build_pack};
pub use manifest::{AssetManifest, BuildManifest, PackAsset, PackManifest, Sha256Hex};
pub use report::{
    BUILD_REPORT_FILE, BuildReport, CompressionRatio, LargestSelectedRecord, ReportAsset,
    SelectedReport, SourceReport, SourceShapeObservation, TranslationCoverage,
};
pub use source::{JsonlSourceReader, SourceIngestionError, SourceRecord};
