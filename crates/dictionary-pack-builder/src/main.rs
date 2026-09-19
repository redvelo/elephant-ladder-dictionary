use std::env;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use elephant_ladder_dictionary_pack::{
    AUDIO_MANIFEST_FILE, AudioCollection, AudioManifest, DATA_APPLICATION_ID, DictionaryPack,
    ExpectedPack, FORMAT_VERSION, INDEX_APPLICATION_ID, PackError, PackLimits, PackRevision,
    Sha256Hex, UNICODE_PROFILE,
};
use elephant_ladder_dictionary_pack_builder::{
    AcquireOptions, AcquisitionState, AudioBuildOptions, BuildManifest, BuildOptions,
    CatalogConfig, EditionConfig, MAX_RECORDING_SECONDS, ReferenceSet, SourceSnapshot, acquire,
    assemble_catalog, build_audio_collection, build_manifest, build_pack, generate_signing_key,
    read_signing_key, sign_catalog, transcode_to_opus, utc_now, validate_snapshot,
};
use sha2::{Digest, Sha256};

const PROGRESS_INTERVAL_BYTES: u64 = 64 * 1024 * 1024;

struct ProgressReader<R> {
    inner: R,
    bytes_read: Arc<AtomicU64>,
    next_report: u64,
    started_at: Instant,
    eof_reported: bool,
}

impl<R> ProgressReader<R> {
    fn new(inner: R, bytes_read: Arc<AtomicU64>, started_at: Instant) -> Self {
        Self {
            inner,
            bytes_read,
            next_report: PROGRESS_INTERVAL_BYTES,
            started_at,
            eof_reported: false,
        }
    }
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let bytes_read = self.bytes_read.load(Ordering::Relaxed);
        let bytes_until_report = self.next_report.saturating_sub(bytes_read);
        let read_limit = usize::try_from(bytes_until_report)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let count = self.inner.read(&mut buffer[..read_limit])?;
        let total = bytes_read + count as u64;
        self.bytes_read.store(total, Ordering::Relaxed);
        if count == 0 && !self.eof_reported {
            eprintln!(
                "build progress: input complete; input_bytes={total} elapsed_seconds={:.1}; finalizing pack",
                self.started_at.elapsed().as_secs_f64()
            );
            self.eof_reported = true;
        }
        if total == self.next_report {
            let elapsed = self.started_at.elapsed().as_secs_f64();
            let mebibytes = u32::try_from(total / (1024 * 1024)).unwrap_or(u32::MAX);
            eprintln!(
                "build progress: input_bytes={total} elapsed_seconds={elapsed:.1} rate_mib_s={:.2}",
                f64::from(mebibytes) / elapsed.max(f64::EPSILON)
            );
            self.next_report += PROGRESS_INTERVAL_BYTES;
        }
        Ok(count)
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut arguments = env::args_os();
    let program = arguments
        .next()
        .unwrap_or_else(|| "elephant-dictionary-pack".into());
    let Some(command) = arguments.next() else {
        return Err(usage(&program));
    };
    if command == "format-version" {
        if arguments.next().is_some() {
            return Err(usage(&program));
        }
        print_format_version();
        return Ok(());
    }
    if command == "validate" {
        let Some(flag) = arguments.next() else {
            return Err(usage(&program));
        };
        let Some(directory) = arguments.next() else {
            return Err(usage(&program));
        };
        if flag != "--pack" || arguments.next().is_some() {
            return Err(usage(&program));
        }
        return run_validate(PathBuf::from(directory));
    }
    if command == "audio" {
        let arguments = arguments.collect::<Vec<_>>();
        return run_audio(&program, &arguments);
    }
    if command == "source" {
        let arguments = arguments.collect::<Vec<_>>();
        return run_source(&program, &arguments);
    }
    if command == "catalog" {
        let arguments = arguments.collect::<Vec<_>>();
        return run_catalog(&program, &arguments);
    }
    if command != "build" {
        return Err(usage(&program));
    }

    let mut manifest = None;
    let mut input = None;
    let mut output = None;
    while let Some(flag) = arguments.next() {
        let value = arguments.next().ok_or_else(|| {
            format!(
                "missing value for `{}`\n{}",
                flag.to_string_lossy(),
                usage(&program)
            )
        })?;
        match flag.to_str() {
            Some("--manifest") if manifest.is_none() => manifest = Some(PathBuf::from(value)),
            Some("--input") if input.is_none() => input = Some(PathBuf::from(value)),
            Some("--output") if output.is_none() => output = Some(PathBuf::from(value)),
            _ => return Err(usage(&program)),
        }
    }
    let manifest_path = manifest.ok_or_else(|| usage(&program))?;
    let input_path = input.ok_or_else(|| usage(&program))?;
    let output_path = output.ok_or_else(|| usage(&program))?;
    let manifest_file = File::open(&manifest_path)
        .map_err(|error| format!("cannot open {}: {error}", manifest_path.display()))?;
    let manifest: BuildManifest = serde_json::from_reader(manifest_file).map_err(|error| {
        format!(
            "invalid build manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let stdin = io::stdin();
    let source: Box<dyn Read> = if input_path == OsStr::new("-") {
        Box::new(stdin.lock())
    } else {
        Box::new(
            File::open(&input_path)
                .map_err(|error| format!("cannot open {}: {error}", input_path.display()))?,
        )
    };
    let started_at = Instant::now();
    let bytes_read = Arc::new(AtomicU64::new(0));
    eprintln!("build started: output={}", output_path.display());
    let source = ProgressReader::new(source, Arc::clone(&bytes_read), started_at);
    let result = build_pack(&manifest, source, &output_path, BuildOptions::default())
        .map_err(|error| error.to_string())?;
    eprintln!(
        "build completed: input_bytes={} elapsed_seconds={:.1}",
        bytes_read.load(Ordering::Relaxed),
        started_at.elapsed().as_secs_f64()
    );
    println!("pack_revision={}", result.pack_revision);
    println!("output={}", result.output_directory.display());
    Ok(())
}

fn run_validate(directory: PathBuf) -> Result<(), String> {
    let started_at = Instant::now();
    eprintln!("validation started: pack={}", directory.display());
    let pack = DictionaryPack::open(directory, PackLimits::default())
        .map_err(|error| error.to_string())?;
    let report = pack.validate_all().map_err(|error| error.to_string())?;
    eprintln!(
        "validation completed: elapsed_seconds={:.1}",
        started_at.elapsed().as_secs_f64()
    );
    println!(
        "authenticated_record_count={}",
        report.authenticated_record_count
    );
    println!(
        "uncompressed_record_bytes={}",
        report.uncompressed_record_bytes
    );
    println!("lookup_key_row_count={}", report.lookup_key_row_count);
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let file = File::open(path).map_err(|error| format!("cannot open {path}: {error}"))?;
    serde_json::from_reader(file).map_err(|error| format!("invalid JSON in {path}: {error}"))
}

fn run_audio(program: &OsStr, arguments: &[std::ffi::OsString]) -> Result<(), String> {
    let arguments = arguments
        .iter()
        .map(|argument| argument.to_str().ok_or_else(|| usage(program)))
        .collect::<Result<Vec<_>, _>>()?;
    match arguments.as_slice() {
        ["references", "--pack", pack, "--output", output] => audio_references(pack, output),
        [
            "acquire",
            "--references",
            references,
            "--state",
            state,
            "--contact",
            contact,
            shard @ ..,
        ] => {
            let shard = match shard {
                [] => None,
                ["--shard", value] => Some(parse_shard(value)?),
                _ => return Err(usage(program)),
            };
            run_acquire(references, state, contact, shard)
        }
        [
            "build",
            "--state",
            state,
            "--edition",
            edition,
            "--builder-revision",
            revision,
            "--output",
            output,
        ] => {
            let edition: EditionConfig = read_json(edition)?;
            let audio = edition
                .audio
                .ok_or_else(|| "edition has no audio configuration".to_owned())?;
            let state =
                AcquisitionState::open(Path::new(state)).map_err(|error| error.to_string())?;
            let options = AudioBuildOptions {
                builder_revision: (*revision).to_owned(),
                minimum_app_version: edition.minimum_app_version,
                workers: std::thread::available_parallelism().map_or(1, usize::from),
            };
            let result = build_audio_collection(&state, &audio, &options, Path::new(output))
                .map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        ["validate", "--collection", collection] => {
            let directory = Path::new(collection);
            let manifest_bytes = std::fs::read(directory.join(AUDIO_MANIFEST_FILE))
                .map_err(|error| format!("cannot read audio manifest: {error}"))?;
            let manifest: AudioManifest = serde_json::from_slice(&manifest_bytes)
                .map_err(|error| format!("invalid audio manifest: {error}"))?;
            let expected = ExpectedPack {
                pack_id: manifest
                    .pack_id
                    .parse()
                    .map_err(|error: PackError| error.to_string())?,
                pack_revision: PackRevision::from_bytes(*manifest.pack_revision.as_bytes()),
                manifest_sha256: Sha256Hex::from_bytes(Sha256::digest(&manifest_bytes).into()),
            };
            let collection = AudioCollection::verify(directory, &expected, PackLimits::default())
                .map_err(|error| error.to_string())?;
            let report = collection
                .validate_all()
                .map_err(|error| error.to_string())?;
            println!("recording_count={}", report.recording_count);
            println!("available_count={}", report.available_count);
            println!("blob_count={}", report.blob_count);
            println!("blob_bytes={}", report.blob_bytes);
            Ok(())
        }
        ["transcode", "--input", input, "--output", output] => {
            let source =
                std::fs::read(input).map_err(|error| format!("cannot read {input}: {error}"))?;
            let transcoded = transcode_to_opus(&source, 1, MAX_RECORDING_SECONDS)
                .map_err(|error| error.to_string())?;
            std::fs::write(output, &transcoded.bytes)
                .map_err(|error| format!("cannot write {output}: {error}"))?;
            println!("bytes={}", transcoded.bytes.len());
            println!("duration_ms={}", transcoded.duration_ms);
            Ok(())
        }
        _ => Err(usage(program)),
    }
}

fn audio_references(pack: &str, output: &str) -> Result<(), String> {
    let pack =
        DictionaryPack::open(pack, PackLimits::default()).map_err(|error| error.to_string())?;
    let references = pack.audio_references().map_err(|error| error.to_string())?;
    let set = ReferenceSet {
        schema_version: 1,
        corpus_pack_id: pack.pack_id().as_str().to_owned(),
        corpus_revision: Sha256Hex::from_bytes(*pack.pack_revision().as_bytes()),
        corpus_language: pack.corpus_language().to_owned(),
        files: references.files.into_iter().collect(),
        invalid: references.invalid.into_iter().collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&set).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    std::fs::write(output, bytes).map_err(|error| format!("cannot write {output}: {error}"))?;
    println!("files={}", set.files.len());
    println!("invalid={}", set.invalid.len());
    Ok(())
}

fn run_source(program: &OsStr, arguments: &[std::ffi::OsString]) -> Result<(), String> {
    let arguments = arguments
        .iter()
        .map(|argument| argument.to_str().ok_or_else(|| usage(program)))
        .collect::<Result<Vec<_>, _>>()?;
    match arguments.as_slice() {
        ["validate", "--edition", edition, "--snapshot", snapshot] => {
            let edition: EditionConfig = read_json(edition)?;
            let snapshot: SourceSnapshot = read_json(snapshot)?;
            validate_snapshot(&edition, &snapshot).map_err(|error| error.to_string())
        }
        [
            "manifest",
            "--edition",
            edition,
            "--snapshot",
            snapshot,
            "--builder-revision",
            revision,
        ] => {
            let edition: EditionConfig = read_json(edition)?;
            let snapshot: SourceSnapshot = read_json(snapshot)?;
            let manifest =
                build_manifest(&edition, &snapshot, revision).map_err(|error| error.to_string())?;
            let json =
                serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
            println!("{json}");
            Ok(())
        }
        _ => Err(usage(program)),
    }
}

fn run_acquire(
    references: &str,
    state: &str,
    contact: &str,
    shard: Option<(u32, u32)>,
) -> Result<(), String> {
    let references: ReferenceSet = read_json(references)?;
    let state = AcquisitionState::open_or_create(Path::new(state), &references, &utc_now())
        .map_err(|error| error.to_string())?;
    let options = AcquireOptions {
        shard,
        ..AcquireOptions::commons(contact)
    };
    let report = acquire(&state, &options).map_err(|error| error.to_string())?;
    for (phase, count) in report.phases {
        println!("{phase}={count}");
    }
    println!("completed={}", report.completed);
    Ok(())
}

/// Parses `ordinal/total`, as in `2/3` for the second of three machines.
fn parse_shard(value: &str) -> Result<(u32, u32), String> {
    let invalid = || "shard must be ORDINAL/TOTAL, such as 2/3".to_owned();
    let (ordinal, total) = value.split_once('/').ok_or_else(invalid)?;
    let ordinal: u32 = ordinal.parse().map_err(|_| invalid())?;
    let total: u32 = total.parse().map_err(|_| invalid())?;
    if ordinal == 0 || total == 0 || ordinal > total {
        return Err(invalid());
    }
    Ok((ordinal, total))
}

fn run_catalog(program: &OsStr, arguments: &[std::ffi::OsString]) -> Result<(), String> {
    let arguments = arguments
        .iter()
        .map(|argument| argument.to_str().ok_or_else(|| usage(program)))
        .collect::<Result<Vec<_>, _>>()?;
    match arguments.as_slice() {
        ["keygen", "--output", path] => {
            let key = generate_signing_key(Path::new(path)).map_err(|error| error.to_string())?;
            println!("public_key={}", hex::encode(key.verifying_key().to_bytes()));
            Ok(())
        }
        ["assemble", "--config", config, "--output", output] => {
            let config_path = Path::new(config);
            let file = File::open(config_path)
                .map_err(|error| format!("cannot open {config}: {error}"))?;
            let config: CatalogConfig = serde_json::from_reader(file)
                .map_err(|error| format!("invalid catalog config {config}: {error}"))?;
            let catalog = assemble_catalog(
                &config,
                config_path.parent().unwrap_or_else(|| Path::new(".")),
                Path::new(output),
            )
            .map_err(|error| error.to_string())?;
            println!("catalog_revision={}", catalog.catalog_revision);
            println!("packs={}", catalog.packs.len());
            Ok(())
        }
        ["sign", "--release", release, "--key", key] => {
            let key = read_signing_key(Path::new(key)).map_err(|error| error.to_string())?;
            sign_catalog(Path::new(release), &key).map_err(|error| error.to_string())?;
            println!("public_key={}", hex::encode(key.verifying_key().to_bytes()));
            Ok(())
        }
        _ => Err(usage(program)),
    }
}

fn print_format_version() {
    println!("format_version={FORMAT_VERSION}");
    println!("index_application_id={INDEX_APPLICATION_ID:#010x}");
    println!("data_application_id={DATA_APPLICATION_ID:#010x}");
    println!("unicode_profile={UNICODE_PROFILE}");
}

fn usage(program: &std::ffi::OsStr) -> String {
    format!(
        "usage: {program} format-version\n       {program} build --manifest MANIFEST.json --input SOURCE.jsonl|- --output DIRECTORY\n       {program} validate --pack DIRECTORY\n       {program} audio references --pack CORPUS_DIRECTORY --output REFERENCES.json\n       {program} audio acquire --references REFERENCES.json --state DIRECTORY --contact URL_OR_EMAIL [--shard ORDINAL/TOTAL]\n       {program} audio build --state DIRECTORY --edition EDITION.json --builder-revision REVISION --output COLLECTION_DIRECTORY\n       {program} audio validate --collection COLLECTION_DIRECTORY\n       {program} audio transcode --input SOURCE --output RECORDING.opus\n       {program} source validate --edition EDITION.json --snapshot SNAPSHOT.json\n       {program} source manifest --edition EDITION.json --snapshot SNAPSHOT.json --builder-revision REVISION\n       {program} catalog keygen --output SIGNING_KEY\n       {program} catalog assemble --config CATALOG_CONFIG.json --output RELEASE_DIRECTORY\n       {program} catalog sign --release RELEASE_DIRECTORY --key SIGNING_KEY\n\n`--input -` reads decompressed JSONL from standard input.",
        program = program.to_string_lossy(),
    )
}
