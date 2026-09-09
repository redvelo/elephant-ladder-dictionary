//! Immutable offline dictionary pack format and read-time primitives.

mod error;
mod format;
mod identity;
mod manifest;
mod pack;
mod semantic;
mod service;
mod unicode;

pub use error::PackError;
pub use format::{
    COMPRESSION_PROFILE, DATA_APPLICATION_ID, DATA_SCHEMA, FORMAT_VERSION, INDEX_APPLICATION_ID,
    INDEX_SCHEMA, ROUTING_POLICY, SELECTED_STREAM_DIGEST_DOMAIN, SHARD_RECORD_DIGEST_DOMAIN,
    initialize_data_database, initialize_index_database,
};
pub use identity::{
    EntryId, PackId, PackRevision, PackRevisionInputs, SnapshotRevision, SnapshotViewIdentity,
    ViewType,
};
pub use manifest::{AssetManifest, PACK_MANIFEST_FILE, PackAsset, PackManifest, Sha256Hex};
pub use pack::{
    DictionaryPack, LookupMatch, LookupOptions, LookupOutcome, PackLimits, ValidationReport,
};
pub use semantic::{
    EntryReference, EntrySummary, MatchClass, ProgressiveSection, PronunciationSummary,
    SectionKind, SenseSummary, TranslationSummary,
};
pub use service::{
    BilingualLookupOutcome, BilingualView, DictionaryService, DictionarySnapshot, DictionaryView,
    MonolingualView, SnapshotError,
};
pub use unicode::{LookupKeys, UNICODE_PROFILE, folded_key, lookup_keys, nfc_key};
