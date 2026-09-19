//! Maintenance-time construction and qualification of dictionary packs.

mod audio_acquire;
mod audio_build;
mod audio_state;
mod builder;
mod catalog;
mod manifest;
mod report;
mod snapshot;
mod source;
mod transcode;

pub use audio_acquire::{
    AcquireOptions, AcquireReport, MAX_DOWNLOAD_CONCURRENCY, acquire, utc_now,
};
pub use audio_build::{
    AudioBuildOptions, AudioBuildResult, MAX_RECORDING_SECONDS, build_audio_collection,
};
pub use audio_state::{
    AcquisitionState, CommonsFacts, FileState, Phase, ReferenceSet, author_from_html,
    qualify_license,
};
pub use builder::{BuildError, BuildOptions, BuildResult, build_pack};
pub use catalog::{
    CatalogBuildError, CatalogConfig, CorpusConfig, assemble_catalog, generate_signing_key,
    read_signing_key, release_manifest_name, sign_catalog,
};
pub use manifest::{AssetManifest, BuildManifest, PackAsset, PackManifest, Sha256Hex};
pub use report::{
    BUILD_REPORT_FILE, BuildReport, CompressionRatio, LargestSelectedRecord, ReportAsset,
    SelectedReport, SourceReport, SourceShapeObservation, TranslationCoverage,
};
pub use snapshot::{
    AudioEditionConfig, CapturedOrigin, EditionConfig, EditionOrigin, MAX_MIRROR_PART_BYTES,
    MirrorAsset, SnapshotError, SourceDigest, SourceMirror, SourceSnapshot, UncompressedSource,
    build_manifest, validate_snapshot,
};
pub use source::{JsonlSourceReader, SourceIngestionError, SourceRecord};
pub use transcode::{TranscodeError, Transcoded, transcode_to_opus};
