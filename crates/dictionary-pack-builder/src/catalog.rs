use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use elephant_ladder_dictionary_pack::{
    AUDIO_MANIFEST_FILE, AudioCollection, AudioManifest, AudioPack, CATALOG_FILE, Catalog,
    CatalogAsset, CatalogError, CatalogPack, CorpusPack, DictionaryPack, ExpectedPack,
    PACK_MANIFEST_FILE, PackAsset, PackError, PackLimits, PackRelease, PackRevision, Sha256Hex,
    validate_catalog,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Release inputs for `catalog assemble`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfig {
    pub catalog_revision: String,
    pub generated_at: String,
    /// Base URL under which every release asset is published.
    pub release_base_url: String,
    pub corpora: Vec<CorpusConfig>,
    #[serde(default)]
    pub audio: Vec<AudioConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CorpusConfig {
    /// A built pack directory, relative to the configuration file.
    pub directory: PathBuf,
    pub monolingual: bool,
    pub bilingual_targets: Vec<String>,
    /// Set when an earlier release already published this pack, so the catalog points at
    /// those assets instead of publishing them again.
    #[serde(default)]
    pub release_base_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AudioConfig {
    /// A built audio collection directory, relative to the configuration file.
    pub directory: PathBuf,
    /// Set when an earlier release already published this collection.
    #[serde(default)]
    pub release_base_url: Option<String>,
}

#[derive(Debug, Error)]
#[non_exhaustive]
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

/// The release asset name of a pack manifest, which is unique within a release.
#[must_use]
pub fn release_manifest_name(
    pack_id: &str,
    pack_revision: &Sha256Hex,
    manifest_file: &str,
) -> String {
    format!("{pack_id}-{pack_revision}-{manifest_file}")
}

/// Verifies every configured pack and writes a release directory containing the
/// unsigned catalog and every release asset under its unique release name.
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
    let base_url = release_base(&config.release_base_url)?;
    fs::create_dir(output).map_err(|source| io_error("create_dir", output, source))?;
    let mut packs = Vec::with_capacity(config.corpora.len() + config.audio.len());
    for corpus in &config.corpora {
        let directory = config_directory.join(&corpus.directory);
        let pack = DictionaryPack::open(&directory, PackLimits::default())?;
        let metadata = pack.metadata();
        let manifest = pack.manifest();
        let assets = publish(&Publication {
            directory: &directory,
            manifest_file: PACK_MANIFEST_FILE,
            pack_id: &manifest.pack_id,
            pack_revision: &manifest.pack_revision,
            assets: &manifest.assets,
            base_url: corpus.release_base_url.as_deref().unwrap_or(base_url),
            output: corpus.release_base_url.is_none().then_some(output),
        })?;
        packs.push(CatalogPack::Corpus(CorpusPack {
            release: release(
                &manifest.pack_id,
                manifest.pack_revision,
                &metadata.minimum_app_version,
                assets,
            ),
            corpus_language: metadata.corpus_language.clone(),
            wiktionary_edition: metadata.wiktionary_edition.clone(),
            wiktionary_dump_date: metadata.wiktionary_dump_date.clone(),
            monolingual: corpus.monolingual,
            bilingual_targets: corpus.bilingual_targets.clone(),
            licenses: license_identifiers(&metadata.license_manifest_json)?,
        }));
    }
    for audio in &config.audio {
        let directory = config_directory.join(&audio.directory);
        let (collection, manifest) = open_audio(&directory)?;
        let metadata = collection.metadata();
        let assets = publish(&Publication {
            directory: &directory,
            manifest_file: AUDIO_MANIFEST_FILE,
            pack_id: &manifest.pack_id,
            pack_revision: &manifest.pack_revision,
            assets: &manifest.assets,
            base_url: audio.release_base_url.as_deref().unwrap_or(base_url),
            output: audio.release_base_url.is_none().then_some(output),
        })?;
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
    let mut bytes = serde_json::to_vec_pretty(&catalog)
        .map_err(|error| CatalogBuildError::Invalid(error.to_string()))?;
    bytes.push(b'\n');
    write_new(&output.join(CATALOG_FILE), &bytes, 0o644)?;
    Ok(catalog)
}

fn release_base(url: &str) -> Result<&str, CatalogBuildError> {
    let url = url.trim_end_matches('/');
    if url.starts_with("https://") {
        Ok(url)
    } else {
        Err(CatalogBuildError::Invalid(
            "release_base_url must be an https URL".to_owned(),
        ))
    }
}

fn release(
    pack_id: &str,
    pack_revision: Sha256Hex,
    minimum_app_version: &str,
    assets: Vec<CatalogAsset>,
) -> PackRelease {
    PackRelease {
        pack_id: pack_id.to_owned(),
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
        pack_id: manifest.pack_id.parse()?,
        pack_revision: PackRevision::from_bytes(*manifest.pack_revision.as_bytes()),
        manifest_sha256: Sha256Hex::from_bytes(Sha256::digest(&bytes).into()),
    };
    let collection = AudioCollection::verify(directory, &expected, PackLimits::default())?;
    collection.validate_all()?;
    Ok((collection, manifest))
}

struct Publication<'a> {
    directory: &'a Path,
    manifest_file: &'static str,
    pack_id: &'a str,
    pack_revision: &'a Sha256Hex,
    assets: &'a [PackAsset],
    base_url: &'a str,
    /// Where to place the assets, or `None` when an earlier release already holds them.
    output: Option<&'a Path>,
}

/// The catalog assets of one pack: its manifest under a release-unique name, then its files.
fn publish(publication: &Publication<'_>) -> Result<Vec<CatalogAsset>, CatalogBuildError> {
    let base_url = release_base(publication.base_url)?;
    let manifest_path = publication.directory.join(publication.manifest_file);
    let manifest_bytes =
        fs::read(&manifest_path).map_err(|source| io_error("read", &manifest_path, source))?;
    let mut files = vec![(
        publication.manifest_file.to_owned(),
        release_manifest_name(
            publication.pack_id,
            publication.pack_revision,
            publication.manifest_file,
        ),
        manifest_bytes.len() as u64,
        Sha256Hex::from_bytes(Sha256::digest(&manifest_bytes).into()),
    )];
    files.extend(publication.assets.iter().map(|asset| {
        (
            asset.file_name.clone(),
            asset.file_name.clone(),
            asset.size_bytes,
            asset.sha256,
        )
    }));
    let mut assets = Vec::with_capacity(files.len());
    for (file_name, release_name, size_bytes, sha256) in files {
        if let Some(output) = publication.output {
            link_or_copy(
                &publication.directory.join(&file_name),
                &output.join(&release_name),
            )?;
        }
        assets.push(CatalogAsset {
            url: format!("{base_url}/{release_name}"),
            file_name,
            size_bytes,
            sha256,
        });
    }
    Ok(assets)
}

fn license_identifiers(license_manifest_json: &str) -> Result<Vec<String>, CatalogBuildError> {
    let manifest: serde_json::Value = serde_json::from_str(license_manifest_json)
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

fn link_or_copy(source: &Path, target: &Path) -> Result<(), CatalogBuildError> {
    if fs::hard_link(source, target).is_ok() {
        return Ok(());
    }
    fs::copy(source, target)
        .map(|_| ())
        .map_err(|error| io_error("copy", target, error))
}

fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<(), CatalogBuildError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = options
        .open(path)
        .map_err(|source| io_error("create", path, source))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error("write", path, source))
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> CatalogBuildError {
    CatalogBuildError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}
