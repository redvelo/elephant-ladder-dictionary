//! Immutable offline dictionary pack format and read-time primitives.

mod audio;
mod catalog;
mod error;
mod format;
mod identity;
mod manifest;
mod pack;
mod semantic;
mod unicode;
mod view;

pub use audio::{
    AUDIO_MANIFEST_FILE, AudioCollection, AudioDelivery, AudioManifest, AudioMetadata,
    AudioValidationReport, Recording, RecordingAudio, RecordingFacts, RecordingLicense,
    RecordingStatus, audio_chunk_file_name, audio_chunk_for, audio_index_file_name,
    opus_duration_ms, recording_input_digest,
};
pub use catalog::{
    AudioPack, CATALOG_FILE, Catalog, CatalogAsset, CatalogError, CatalogPack, CorpusPack,
    MAX_CATALOG_BYTES, PackRelease, RecordingCounts, parse_catalog, validate_catalog,
};
pub use error::PackError;
pub use format::{
    AUDIO_CHUNK_APPLICATION_ID, AUDIO_CHUNK_SCHEMA, AUDIO_ENCODER_PROFILE,
    AUDIO_INDEX_APPLICATION_ID, AUDIO_INDEX_SCHEMA, AUDIO_MEDIA_TYPE, COMPRESSION_PROFILE,
    DATA_APPLICATION_ID, DATA_SCHEMA, FORMAT_VERSION, INDEX_APPLICATION_ID, INDEX_SCHEMA,
    ROUTING_POLICY, SELECTED_STREAM_DIGEST_DOMAIN, SHARD_RECORD_DIGEST_DOMAIN,
};
pub use identity::{
    CorpusIdentity, EntryId, FramedDigest, PackId, PackRevision, Sha256Hex, SnapshotRevision,
};
pub use manifest::{PACK_MANIFEST_FILE, PackAsset, PackManifest};
pub use pack::{
    AudioReferences, DictionaryPack, ExpectedPack, LookupMatch, LookupOptions, LookupOutcome,
    MAX_ASSETS, MAX_LOOKUP_RESULTS, MAX_QUERY_BYTES, MAX_ROUTING_FORMS, MAX_SECTION_PAGE_ITEMS,
    ValidationReport,
};
pub use semantic::{
    Bounded, Classifier, Descendant, EmphasisRange, Entry, EntryReference, Example, Form,
    Hyphenation, LemmaReference, MatchClass, ProjectionOptions, Pronunciation, Relation,
    RelationGroup, RelationKind, Ruby, Section, SectionPage, Sense, Translation,
    commons_description_url, commons_file_name, commons_title,
};
pub use unicode::{LookupKeys, UNICODE_PROFILE, lookup_keys};
pub use view::{DictionarySnapshot, DictionaryView, SnapshotError, ViewLookup, ViewOutcome};
