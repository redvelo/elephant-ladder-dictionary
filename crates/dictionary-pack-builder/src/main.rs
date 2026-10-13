use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use elephant_ladder_dictionary_pack::{
    AUDIO_MANIFEST_FILE, AudioCollection, AudioDelivery, AudioManifest, DictionaryPack,
    ExpectedPack, Sha256Hex,
};
use elephant_ladder_dictionary_pack_builder::{
    AcquireOptions, AcquisitionState, AudioBuildOptions, CatalogConfig, EditionConfig,
    ReferenceSet, SourceSnapshot, acquire, assemble_catalog, build_audio_collection, build_pack,
    corpus_identity, utc_now, validate_snapshot,
};
#[cfg(feature = "transcode")]
use elephant_ladder_dictionary_pack_builder::{MAX_RECORDING_SECONDS, transcode_to_opus};
use sha2::{Digest, Sha256};

const PROGRESS_INTERVAL_BYTES: u64 = 64 * 1024 * 1024;

/// Reports input progress on standard error while a multi-gigabyte build runs.
struct ProgressReader<R> {
    inner: R,
    bytes_read: Arc<AtomicU64>,
    next_report: u64,
    started_at: Instant,
    eof_reported: bool,
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
    let mut arguments = std::env::args_os();
    let program = arguments
        .next()
        .unwrap_or_else(|| "elephant-dictionary-pack".into());
    let arguments = arguments
        .map(|argument| argument.into_string().map_err(|_| usage(&program)))
        .collect::<Result<Vec<_>, _>>()?;
    let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    match arguments.as_slice() {
        [
            "build",
            "--edition",
            edition,
            "--snapshot",
            snapshot,
            "--builder-revision",
            revision,
            "--input",
            input,
            "--output",
            output,
        ] => run_build(edition, snapshot, revision, input, output),
        ["validate", "--pack", directory] => run_validate(Path::new(directory)),
        [
            "source",
            "validate",
            "--edition",
            edition,
            "--snapshot",
            snapshot,
        ] => {
            let edition: EditionConfig = read_json(edition)?;
            let snapshot: SourceSnapshot = read_json(snapshot)?;
            validate_snapshot(&edition, &snapshot).map_err(|error| error.to_string())
        }
        ["audio", rest @ ..] => run_audio(&program, rest),
        [
            "catalog",
            "assemble",
            "--config",
            config,
            "--output",
            output,
        ] => {
            let config_path = Path::new(config);
            let config: CatalogConfig = read_json(config)?;
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
        _ => Err(usage(&program)),
    }
}

fn run_build(
    edition: &str,
    snapshot: &str,
    revision: &str,
    input: &str,
    output: &str,
) -> Result<(), String> {
    let edition: EditionConfig = read_json(edition)?;
    let snapshot: SourceSnapshot = read_json(snapshot)?;
    let identity =
        corpus_identity(&edition, &snapshot, revision).map_err(|error| error.to_string())?;
    let stdin = io::stdin();
    let source: Box<dyn Read> = if input == "-" {
        Box::new(stdin.lock())
    } else {
        Box::new(File::open(input).map_err(|error| format!("cannot open {input}: {error}"))?)
    };
    let started_at = Instant::now();
    let bytes_read = Arc::new(AtomicU64::new(0));
    eprintln!("build started: output={output}");
    let source = ProgressReader {
        inner: source,
        bytes_read: Arc::clone(&bytes_read),
        next_report: PROGRESS_INTERVAL_BYTES,
        started_at,
        eof_reported: false,
    };
    let result = build_pack(&identity, source, output).map_err(|error| error.to_string())?;
    eprintln!(
        "build completed: input_bytes={} elapsed_seconds={:.1}",
        bytes_read.load(Ordering::Relaxed),
        started_at.elapsed().as_secs_f64()
    );
    println!("pack_revision={}", result.pack_revision);
    println!("output={}", result.output_directory.display());
    println!("source_records={}", result.source_record_count);
    println!("source_bytes={}", result.source_bytes);
    println!("selected_records={}", result.manifest.selected_record_count);
    println!("selected_bytes={}", result.manifest.selected_record_bytes);
    println!("compressed_bytes={}", result.compressed_payload_bytes);
    println!("lookup_rows={}", result.lookup_row_count);
    Ok(())
}

fn run_validate(directory: &Path) -> Result<(), String> {
    let started_at = Instant::now();
    eprintln!("validation started: pack={}", directory.display());
    let pack = DictionaryPack::open(directory).map_err(|error| error.to_string())?;
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

fn run_audio(program: &OsStr, arguments: &[&str]) -> Result<(), String> {
    match arguments {
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
            delivery @ ..,
        ] => {
            let delivery = match delivery {
                [] => AudioDelivery::Bundled,
                ["--index-only"] => AudioDelivery::Remote,
                _ => return Err(usage(program)),
            };
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
                delivery,
            };
            let result = build_audio_collection(&state, &audio, &options, Path::new(output))
                .map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        ["validate", "--collection", collection] => audio_validate(Path::new(collection)),
        #[cfg(feature = "transcode")]
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
        #[cfg(not(feature = "transcode"))]
        ["build" | "transcode", ..] => {
            Err("this build cannot encode audio; rebuild with the transcode feature".to_owned())
        }
        _ => Err(usage(program)),
    }
}

fn audio_references(pack: &str, output: &str) -> Result<(), String> {
    let pack = DictionaryPack::open(pack).map_err(|error| error.to_string())?;
    let references = pack.audio_references().map_err(|error| error.to_string())?;
    let set = ReferenceSet {
        schema_version: 1,
        corpus_pack_id: pack.pack_id().clone(),
        corpus_revision: *pack.pack_revision(),
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

fn audio_validate(directory: &Path) -> Result<(), String> {
    let manifest_bytes = std::fs::read(directory.join(AUDIO_MANIFEST_FILE))
        .map_err(|error| format!("cannot read audio manifest: {error}"))?;
    let manifest: AudioManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("invalid audio manifest: {error}"))?;
    let expected = ExpectedPack {
        pack_id: manifest.pack_id,
        pack_revision: manifest.pack_revision,
        manifest_sha256: Sha256Hex::from_bytes(Sha256::digest(&manifest_bytes).into()),
    };
    let collection =
        AudioCollection::verify(directory, &expected).map_err(|error| error.to_string())?;
    let report = collection
        .validate_all()
        .map_err(|error| error.to_string())?;
    println!("recording_count={}", report.recording_count);
    println!("available_count={}", report.available_count);
    println!("blob_count={}", report.blob_count);
    println!("blob_bytes={}", report.blob_bytes);
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

fn usage(program: &OsStr) -> String {
    let program = PathBuf::from(program);
    let program = program.file_name().map_or_else(
        || OsString::from("elephant-dictionary-pack"),
        OsStr::to_os_string,
    );
    format!(
        "usage: {program} build --edition EDITION.json --snapshot SNAPSHOT.json --builder-revision REVISION --input SOURCE.jsonl|- --output DIRECTORY\n       {program} validate --pack DIRECTORY\n       {program} source validate --edition EDITION.json --snapshot SNAPSHOT.json\n       {program} audio references --pack CORPUS_DIRECTORY --output REFERENCES.json\n       {program} audio acquire --references REFERENCES.json --state DIRECTORY --contact URL_OR_EMAIL [--shard ORDINAL/TOTAL]\n       {program} audio build --state DIRECTORY --edition EDITION.json --builder-revision REVISION --output COLLECTION_DIRECTORY [--index-only]\n       {program} audio validate --collection COLLECTION_DIRECTORY\n       {program} audio transcode --input SOURCE --output RECORDING.opus\n       {program} catalog assemble --config CATALOG_CONFIG.json --output RELEASE_DIRECTORY\n\n`--input -` reads decompressed JSONL from standard input.",
        program = program.to_string_lossy(),
    )
}
