use std::collections::BTreeSet;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::AudioDelivery;
use crate::{
    AUDIO_MANIFEST_FILE, ExpectedPack, PACK_MANIFEST_FILE, PackId, PackRevision, Sha256Hex,
};

/// The release asset name of a version-1 catalog.
pub const CATALOG_FILE: &str = "catalog-v1.json";
pub const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
const MAX_CATALOG_PACKS: usize = 256;
const MAX_PACK_ASSETS: usize = 256;
const MAX_CATALOG_TEXT_BYTES: usize = 2048;

/// A description of immutable pack revisions available for installation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub catalog_version: u32,
    pub catalog_revision: String,
    pub generated_at: String,
    pub packs: Vec<CatalogPack>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "pack_type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CatalogPack {
    Corpus(CorpusPack),
    Audio(AudioPack),
}

impl CatalogPack {
    #[must_use]
    pub const fn release(&self) -> &PackRelease {
        match self {
            Self::Corpus(pack) => &pack.release,
            Self::Audio(pack) => &pack.release,
        }
    }

    /// The manifest file every installed pack of this type contains.
    #[must_use]
    pub const fn manifest_file(&self) -> &'static str {
        match self {
            Self::Corpus(_) => PACK_MANIFEST_FILE,
            Self::Audio(_) => AUDIO_MANIFEST_FILE,
        }
    }

    /// The identity a downloaded pack directory must match.
    ///
    /// # Panics
    ///
    /// Never for a pack returned by [`verify_catalog`], which validates the pack
    /// identifier and manifest asset.
    #[must_use]
    pub fn expected_pack(&self) -> ExpectedPack {
        let release = self.release();
        ExpectedPack {
            pack_id: PackId::from_str(&release.pack_id)
                .expect("catalog pack identifier was validated"),
            pack_revision: PackRevision::from_bytes(*release.pack_revision.as_bytes()),
            manifest_sha256: release
                .assets
                .iter()
                .find(|asset| asset.file_name == self.manifest_file())
                .expect("catalog manifest asset was validated")
                .sha256,
        }
    }
}

/// Identity and transport facts shared by every pack type.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackRelease {
    pub pack_id: String,
    pub pack_revision: Sha256Hex,
    pub minimum_app_version: String,
    /// Sum of every asset size, which is also the installed size.
    pub installed_bytes: u64,
    pub assets: Vec<CatalogAsset>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogAsset {
    /// The file name inside the installed pack directory.
    pub file_name: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: Sha256Hex,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusPack {
    pub release: PackRelease,
    pub corpus_language: String,
    pub wiktionary_edition: String,
    pub wiktionary_dump_date: String,
    pub monolingual: bool,
    /// Qualified bilingual target languages.
    pub bilingual_targets: Vec<String>,
    /// SPDX identifiers from the pack license manifest.
    pub licenses: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AudioPack {
    pub release: PackRelease,
    pub corpus_language: String,
    pub recordings: RecordingCounts,
    /// Whether the collection ships its audio or fetches it from the source on demand.
    pub delivery: AudioDelivery,
}

/// Referenced recordings of an audio collection by status.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingCounts {
    pub available: u64,
    pub unqualified: u64,
    pub missing: u64,
    pub failed: u64,
}

#[derive(Debug, Error, Eq, PartialEq)]
#[non_exhaustive]
pub enum CatalogError {
    #[error("catalog exceeds {MAX_CATALOG_BYTES} bytes")]
    TooLarge,
    #[error("catalog is invalid: {0}")]
    Invalid(String),
}

/// Parses and validates exact catalog bytes.
///
/// # Errors
///
/// Returns an error when the catalog is too large or violates its format.
pub fn parse_catalog(catalog: &[u8]) -> Result<Catalog, CatalogError> {
    if catalog.len() > MAX_CATALOG_BYTES {
        return Err(CatalogError::TooLarge);
    }
    let catalog: Catalog = serde_json::from_slice(catalog)
        .map_err(|error| CatalogError::Invalid(error.to_string()))?;
    validate_catalog(&catalog)?;
    Ok(catalog)
}

/// Checks catalog invariants that serde cannot express.
///
/// # Errors
///
/// Returns [`CatalogError::Invalid`] describing the first violation.
pub fn validate_catalog(catalog: &Catalog) -> Result<(), CatalogError> {
    let invalid = |message: String| Err(CatalogError::Invalid(message));
    if catalog.catalog_version != 1 {
        return invalid("unsupported catalog version".to_owned());
    }
    text("catalog_revision", &catalog.catalog_revision)?;
    text("generated_at", &catalog.generated_at)?;
    if catalog.packs.len() > MAX_CATALOG_PACKS {
        return invalid("catalog declares too many packs".to_owned());
    }
    let mut identities = BTreeSet::new();
    for pack in &catalog.packs {
        let release = pack.release();
        validate_release(release, pack.manifest_file())?;
        if !identities.insert((release.pack_id.as_str(), release.pack_revision)) {
            return invalid(format!(
                "duplicate pack revision {}/{}",
                release.pack_id, release.pack_revision
            ));
        }
        match pack {
            CatalogPack::Corpus(corpus) => {
                language("corpus_language", &corpus.corpus_language)?;
                text("wiktionary_edition", &corpus.wiktionary_edition)?;
                text("wiktionary_dump_date", &corpus.wiktionary_dump_date)?;
                let mut targets = BTreeSet::new();
                for target in &corpus.bilingual_targets {
                    language("bilingual target", target)?;
                    if target == &corpus.corpus_language || !targets.insert(target) {
                        return invalid(format!("invalid bilingual target `{target}`"));
                    }
                }
                if !corpus.monolingual && corpus.bilingual_targets.is_empty() {
                    return invalid(format!("corpus `{}` provides no view", release.pack_id));
                }
                if corpus.licenses.is_empty() {
                    return invalid(format!("corpus `{}` declares no license", release.pack_id));
                }
                for license in &corpus.licenses {
                    text("license", license)?;
                }
            }
            CatalogPack::Audio(audio) => {
                language("corpus_language", &audio.corpus_language)?;
            }
        }
    }
    Ok(())
}

fn validate_release(release: &PackRelease, manifest_file: &str) -> Result<(), CatalogError> {
    PackId::from_str(&release.pack_id).map_err(|error| CatalogError::Invalid(error.to_string()))?;
    text("minimum_app_version", &release.minimum_app_version)?;
    if release.assets.is_empty() || release.assets.len() > MAX_PACK_ASSETS {
        return Err(CatalogError::Invalid(format!(
            "pack `{}` declares an invalid asset count",
            release.pack_id
        )));
    }
    let mut names = BTreeSet::new();
    let mut total = 0_u64;
    for asset in &release.assets {
        let safe_name = !asset.file_name.is_empty()
            && asset.file_name.len() <= 255
            && asset
                .file_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            && !asset.file_name.starts_with('.');
        if !safe_name || !names.insert(asset.file_name.as_str()) {
            return Err(CatalogError::Invalid(format!(
                "pack `{}` declares an unsafe or duplicate asset name",
                release.pack_id
            )));
        }
        let absolute = asset.url.starts_with("https://") || asset.url.starts_with("http://");
        if !absolute || asset.url.len() > MAX_CATALOG_TEXT_BYTES || asset.size_bytes == 0 {
            return Err(CatalogError::Invalid(format!(
                "asset `{}` declares an invalid URL or size",
                asset.file_name
            )));
        }
        total = total.checked_add(asset.size_bytes).ok_or_else(|| {
            CatalogError::Invalid(format!("pack `{}` size overflows", release.pack_id))
        })?;
    }
    if !names.contains(manifest_file) {
        return Err(CatalogError::Invalid(format!(
            "pack `{}` does not declare `{manifest_file}`",
            release.pack_id
        )));
    }
    if total != release.installed_bytes {
        return Err(CatalogError::Invalid(format!(
            "pack `{}` installed size differs from its assets",
            release.pack_id
        )));
    }
    Ok(())
}

fn text(name: &str, value: &str) -> Result<(), CatalogError> {
    if value.is_empty()
        || value.len() > MAX_CATALOG_TEXT_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(CatalogError::Invalid(format!("invalid {name}")));
    }
    Ok(())
}

fn language(name: &str, value: &str) -> Result<(), CatalogError> {
    let valid = !value.is_empty()
        && value.len() <= 35
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(CatalogError::Invalid(format!("invalid {name} `{value}`")))
    }
}
