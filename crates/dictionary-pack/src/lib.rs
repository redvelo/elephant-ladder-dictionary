//! Immutable offline dictionary pack format and read-time primitives.

mod audio;
mod catalog;
mod error;
mod format;
mod identity;
mod manifest;
mod pack;
mod semantic;
mod service;
mod unicode;

pub use audio::{
    AUDIO_MANIFEST_FILE, AudioCollection, AudioManifest, AudioMetadata, AudioValidationReport,
    Recording, RecordingAudio, RecordingFacts, RecordingLicense, RecordingStatus,
    audio_chunk_file_name, audio_chunk_for, audio_index_file_name, frame_recording_facts,
    opus_duration_ms, recording_input_digest,
};
pub use catalog::{
    AudioPack, CATALOG_FILE, CATALOG_SIGNATURE_FILE, Catalog, CatalogAsset, CatalogError,
    CatalogPack, CorpusPack, MAX_CATALOG_BYTES, PackRelease, RecordingCounts, encode_signature,
    parse_verifying_key, validate_catalog, verify_catalog,
};
pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
pub use error::PackError;
pub use format::{
    AUDIO_CHUNK_APPLICATION_ID, AUDIO_CHUNK_SCHEMA, AUDIO_ENCODER_PROFILE,
    AUDIO_INDEX_APPLICATION_ID, AUDIO_INDEX_SCHEMA, AUDIO_MEDIA_TYPE, COMPRESSION_PROFILE,
    DATA_APPLICATION_ID, DATA_SCHEMA, FORMAT_VERSION, INDEX_APPLICATION_ID, INDEX_SCHEMA,
    ROUTING_POLICY, SELECTED_STREAM_DIGEST_DOMAIN, SHARD_RECORD_DIGEST_DOMAIN,
    initialize_data_database, initialize_index_database,
};
pub use identity::{
    AudioRevisionInputs, EntryId, PackId, PackRevision, PackRevisionInputs, SnapshotRevision,
    SnapshotViewIdentity, ViewType,
};
pub use manifest::{AssetManifest, PACK_MANIFEST_FILE, PackAsset, PackManifest, Sha256Hex};
pub use pack::{
    AudioReferences, DictionaryPack, ExpectedPack, LookupMatch, LookupOptions, LookupOutcome,
    PackLimits, PackMetadata, ValidationReport,
};
pub use semantic::{
    AudioReference, Bounded, Classifier, Descendant, EmphasisRange, Entry, EntryReference, Example,
    Form, Hyphenation, LemmaReference, MatchClass, ProjectionOptions, Pronunciation, Relation,
    RelationGroup, RelationKind, Ruby, Section, SectionPage, Sense, Translation, commons_file_name,
};
pub use service::{
    BilingualLookupOutcome, BilingualView, DictionaryService, DictionarySnapshot, DictionaryView,
    MonolingualView, SnapshotError, ViewLookup, ViewOutcome,
};
pub use unicode::{LookupKeys, UNICODE_PROFILE, folded_key, lookup_keys, nfc_key};
