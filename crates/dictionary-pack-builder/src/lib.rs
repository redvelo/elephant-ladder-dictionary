//! Maintenance-time construction and qualification of dictionary packs.

mod audio_acquire;
mod audio_build;
mod audio_state;
mod builder;
mod catalog;
mod snapshot;
mod source;
#[cfg(feature = "transcode")]
mod transcode;

pub use audio_acquire::{
    AcquireOptions, AcquireReport, MAX_DOWNLOAD_CONCURRENCY, acquire, utc_now,
};
pub use audio_build::{
    AudioBuildOptions, AudioBuildResult, MAX_RECORDING_SECONDS, build_audio_collection,
};
pub use audio_state::{
    AcquisitionMetadata, AcquisitionState, CommonsFacts, FileState, Phase, ReferenceSet,
};
pub use builder::{BuildError, BuildResult, build_pack};
pub use catalog::{CatalogBuildError, CatalogConfig, CorpusConfig, assemble_catalog};
pub use snapshot::{
    AudioEditionConfig, CapturedOrigin, EditionConfig, EditionOrigin, MAX_MIRROR_PART_BYTES,
    MirrorAsset, SnapshotError, SourceDigest, SourceMirror, SourceSnapshot, UncompressedSource,
    corpus_identity, validate_snapshot,
};
pub use source::SourceIngestionError;
#[cfg(feature = "transcode")]
pub use transcode::{TranscodeError, Transcoded, transcode_to_opus};
