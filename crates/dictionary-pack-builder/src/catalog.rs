use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use elephant_ladder_dictionary_pack::{
    AUDIO_MANIFEST_FILE, AudioCollection, AudioManifest, AudioPack, CATALOG_FILE, Catalog,
    CatalogAsset, CatalogError, CatalogPack, CorpusPack, DictionaryPack, ExpectedPack,
    PACK_MANIFEST_FILE, PackAsset, PackError, PackId, PackRelease, PackRevision, Sha256Hex,
    validate_catalog,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::builder::write_json;

/// Release inputs for `catalog assemble`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfig {
    pub catalog_revision: String,
    pub generated_at: String,
    /// The `https` URL under which every release asset is published.
    pub release_base_url: String,
    pub corpora: Vec<CorpusConfig>,
    /// Built audio collection directories, relative to the configuration file.
    #[serde(default)]
    pub audio: Vec<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CorpusConfig {
    /// A built pack directory, relative to the configuration file.
    pub directory: PathBuf,
    pub monolingual: bool,
    pub bilingual_targets: Vec<String>,
}

#[derive(Debug, Error)]
pub enum CatalogBuildError {
    #[error(transparent)]
    Pack(#[from] PackError),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error("invalid catalog input: {0}")]
    Invalid(String),
    #[error("filesystem operation `{operation}` failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl From<crate::BuildError> for CatalogBuildError {
    fn from(error: crate::BuildError) -> Self {
        match error {
            crate::BuildError::Io {
                operation,
                path,
                source,
            } => Self::Io {
                operation,
                path,
                source,
            },
            other => Self::Invalid(other.to_string()),
        }
    }
}

/// Verifies every configured pack and writes a release directory containing the
/// catalog and every release asset under its unique release name. Manifests are
/// published as `<pack-id>-<revision>-<manifest file>` so names stay unique.
///
/// Assets are hard-linked when possible and copied otherwise.
///
/// # Errors
///
/// Returns an error when the output exists, a pack fails verification, or the
/// resulting catalog is invalid.
pub fn assemble_catalog(
    config: &CatalogConfig,
    config_directory: &Path,
    output: &Path,
) -> Result<Catalog, CatalogBuildError> {
    let base_url = config.release_base_url.trim_end_matches('/');
    if !base_url.starts_with("https://") {
        return Err(CatalogBuildError::Invalid(
            "release_base_url must be an https URL".to_owned(),
        ));
    }
    fs::create_dir(output).map_err(|source| io_error("create_dir", output, source))?;
    let mut packs = Vec::with_capacity(config.corpora.len() + config.audio.len());
    for corpus in &config.corpora {
        let directory = config_directory.join(&corpus.directory);
        let pack = DictionaryPack::open(&directory)?;
        let identity = pack.identity();
        let manifest = pack.manifest();
        let assets = publish(
            &directory,
            PACK_MANIFEST_FILE,
            &manifest.pack_id,
            &manifest.pack_revision,
            &manifest.assets,
            base_url,
            output,
        )?;
        packs.push(CatalogPack::Corpus(CorpusPack {
            release: release(
                &manifest.pack_id,
                manifest.pack_revision,
                &identity.minimum_app_version,
                assets,
            ),
            corpus_language: identity.corpus_language.clone(),
            wiktionary_edition: identity.wiktionary_edition.clone(),
            wiktionary_dump_date: identity.wiktionary_dump_date.clone(),
            monolingual: corpus.monolingual,
            bilingual_targets: corpus.bilingual_targets.clone(),
            licenses: license_identifiers(&identity.license_manifest)?,
        }));
    }
    for audio in &config.audio {
        let directory = config_directory.join(audio);
        let (collection, manifest) = open_audio(&directory)?;
        let metadata = collection.metadata();
        let assets = publish(
            &directory,
            AUDIO_MANIFEST_FILE,
            &manifest.pack_id,
            &manifest.pack_revision,
            &manifest.assets,
            base_url,
            output,
        )?;
        packs.push(CatalogPack::Audio(AudioPack {
            release: release(
                &manifest.pack_id,
                manifest.pack_revision,
                &metadata.minimum_app_version,
                assets,
            ),
            corpus_language: metadata.corpus_language.clone(),
            recordings: collection.recording_counts()?,
            delivery: metadata.delivery,
        }));
    }
    let catalog = Catalog {
        catalog_version: 1,
        catalog_revision: config.catalog_revision.clone(),
        generated_at: config.generated_at.clone(),
        packs,
    };
    validate_catalog(&catalog)?;
    write_json(&output.join(CATALOG_FILE), &catalog)?;
    Ok(catalog)
}

fn release(
    pack_id: &PackId,
    pack_revision: PackRevision,
    minimum_app_version: &str,
    assets: Vec<CatalogAsset>,
) -> PackRelease {
    PackRelease {
        pack_id: pack_id.clone(),
        pack_revision,
        minimum_app_version: minimum_app_version.to_owned(),
        installed_bytes: assets.iter().map(|asset| asset.size_bytes).sum(),
        assets,
    }
}

/// Fully verifies an audio collection against the manifest it was built with.
fn open_audio(directory: &Path) -> Result<(AudioCollection, AudioManifest), CatalogBuildError> {
    let path = directory.join(AUDIO_MANIFEST_FILE);
    let bytes = fs::read(&path).map_err(|source| io_error("read", &path, source))?;
    let manifest: AudioManifest = serde_json::from_slice(&bytes)
        .map_err(|error| CatalogBuildError::Invalid(format!("invalid audio manifest: {error}")))?;
    let expected = ExpectedPack {
        pack_id: manifest.pack_id.clone(),
        pack_revision: manifest.pack_revision,
        manifest_sha256: Sha256Hex::from_bytes(Sha256::digest(&bytes).into()),
    };
    let collection = AudioCollection::verify(directory, &expected)?;
    collection.validate_all()?;
    Ok((collection, manifest))
}

/// Links one pack's files into the release and returns its catalog assets: the manifest
/// under a release-unique name, then every asset under its own name.
fn publish(
    directory: &Path,
    manifest_file: &'static str,
    pack_id: &PackId,
    pack_revision: &PackRevision,
    assets: &[PackAsset],
    base_url: &str,
    output: &Path,
) -> Result<Vec<CatalogAsset>, CatalogBuildError> {
    let manifest_path = directory.join(manifest_file);
    let manifest_bytes =
        fs::read(&manifest_path).map_err(|source| io_error("read", &manifest_path, source))?;
    let mut files = vec![(
        manifest_file.to_owned(),
        format!("{pack_id}-{pack_revision}-{manifest_file}"),
        manifest_bytes.len() as u64,
        Sha256Hex::from_bytes(Sha256::digest(&manifest_bytes).into()),
    )];
    files.extend(assets.iter().map(|asset| {
        (
            asset.file_name.clone(),
            asset.file_name.clone(),
            asset.size_bytes,
            asset.sha256,
        )
    }));
    let mut catalog_assets = Vec::with_capacity(files.len());
    for (file_name, release_name, size_bytes, sha256) in files {
        let source = directory.join(&file_name);
        let target = output.join(&release_name);
        if fs::hard_link(&source, &target).is_err() {
            fs::copy(&source, &target).map_err(|source| io_error("copy", &target, source))?;
        }
        catalog_assets.push(CatalogAsset {
            url: format!("{base_url}/{release_name}"),
            file_name,
            size_bytes,
            sha256,
        });
    }
    Ok(catalog_assets)
}

fn license_identifiers(license_manifest: &str) -> Result<Vec<String>, CatalogBuildError> {
    let manifest: serde_json::Value = serde_json::from_str(license_manifest)
        .map_err(|error| CatalogBuildError::Invalid(error.to_string()))?;
    manifest["licenses"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|license| {
            license["spdx"].as_str().map(str::to_owned).ok_or_else(|| {
                CatalogBuildError::Invalid("license manifest entry has no `spdx` string".to_owned())
            })
        })
        .collect()
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> CatalogBuildError {
    CatalogBuildError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}
