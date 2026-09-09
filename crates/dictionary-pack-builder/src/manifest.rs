use std::str::FromStr;

pub use elephant_ladder_dictionary_pack::{AssetManifest, PackAsset, PackManifest, Sha256Hex};
use elephant_ladder_dictionary_pack::{PackId, PackRevision, PackRevisionInputs};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Fully pinned inputs for one deterministic pack build.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildManifest {
    pub pack_id: String,
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
    pub compression_profile: String,
    pub routing_policy: String,
    pub target_shard_payload_bytes: u64,
    pub source_manifest_sha256: Sha256Hex,
    pub source_manifest: Value,
    pub license_manifest_sha256: Sha256Hex,
    pub license_manifest: Value,
    pub compatible_audio_collection: Option<String>,
    pub minimum_app_version: String,
}

impl BuildManifest {
    pub(crate) fn pack_id(&self) -> Result<PackId, elephant_ladder_dictionary_pack::PackError> {
        PackId::from_str(&self.pack_id)
    }

    pub(crate) fn revision(&self, pack_id: &PackId) -> PackRevision {
        PackRevision::derive(PackRevisionInputs {
            pack_id,
            corpus_language: &self.corpus_language,
            wiktionary_edition: &self.wiktionary_edition,
            wiktionary_dump_date: &self.wiktionary_dump_date,
            kaikki_extraction_date: &self.kaikki_extraction_date,
            source_url: &self.source_url,
            compressed_source_sha256: self.compressed_source_sha256.as_bytes(),
            uncompressed_source_sha256: self.uncompressed_source_sha256.as_bytes(),
            wiktextract_revision: &self.wiktextract_revision,
            wikitextprocessor_revision: &self.wikitextprocessor_revision,
            builder_revision: &self.builder_revision,
            compression_profile: &self.compression_profile,
            routing_policy: &self.routing_policy,
            target_shard_payload_bytes: self.target_shard_payload_bytes,
            source_manifest_sha256: self.source_manifest_sha256.as_bytes(),
            license_manifest_sha256: self.license_manifest_sha256.as_bytes(),
            compatible_audio_collection: self.compatible_audio_collection.as_deref(),
            minimum_app_version: &self.minimum_app_version,
        })
    }
}
