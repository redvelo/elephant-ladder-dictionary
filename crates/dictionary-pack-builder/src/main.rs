use std::env;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use elephant_ladder_dictionary_pack::{
    DATA_APPLICATION_ID, DictionaryPack, FORMAT_VERSION, INDEX_APPLICATION_ID, PackLimits,
    UNICODE_PROFILE,
};
use elephant_ladder_dictionary_pack_builder::{BuildManifest, BuildOptions, build_pack};

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
        let directory = PathBuf::from(directory);
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
        return Ok(());
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

fn print_format_version() {
    println!("format_version={FORMAT_VERSION}");
    println!("index_application_id={INDEX_APPLICATION_ID:#010x}");
    println!("data_application_id={DATA_APPLICATION_ID:#010x}");
    println!("unicode_profile={UNICODE_PROFILE}");
}

fn usage(program: &std::ffi::OsStr) -> String {
    format!(
        "usage: {} format-version\n       {} build --manifest MANIFEST.json --input SOURCE.jsonl|- --output DIRECTORY\n       {} validate --pack DIRECTORY\n\n`--input -` reads decompressed JSONL from standard input.",
        program.to_string_lossy(),
        program.to_string_lossy(),
        program.to_string_lossy()
    )
}
