use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Write;
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::BuildError;
use crate::audio_state::{AcquisitionState, CommonsFacts, Phase, original_path};
use crate::builder::io_error;

/// Controls for one acquisition run.
#[derive(Clone, Debug)]
pub struct AcquireOptions {
    pub api_url: String,
    /// Identifies the project and a contact address, as Wikimedia requires.
    pub user_agent: String,
    pub download_workers: usize,
    pub max_original_bytes: u64,
    /// Failed files are retried until they have failed this many times.
    pub max_attempts: u64,
    /// Attempts per HTTP request before a run stops or a file fails.
    pub request_attempts: u32,
    /// Base delay for exponential backoff when no `Retry-After` is given.
    pub backoff: Duration,
}

impl AcquireOptions {
    #[must_use]
    pub fn commons(user_agent: String) -> Self {
        Self {
            api_url: "https://commons.wikimedia.org/w/api.php".to_owned(),
            user_agent,
            download_workers: 4,
            max_original_bytes: 20 * 1024 * 1024,
            max_attempts: 3,
            request_attempts: 6,
            backoff: Duration::from_secs(5),
        }
    }
}

/// File counts by phase after a run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquireReport {
    pub phases: BTreeMap<&'static str, u64>,
    pub completed: bool,
}

const API_BATCH: usize = 50;
const DOWNLOAD_BATCH: usize = 512;
const MAX_API_BYTES: u64 = 16 * 1024 * 1024;

/// Resolves and downloads every unsettled file, resuming from persisted state.
///
/// # Errors
///
/// Returns an error when the Commons API stays unavailable or state cannot be
/// written. Progress already recorded is kept.
pub fn acquire(
    state: &AcquisitionState,
    options: &AcquireOptions,
) -> Result<AcquireReport, BuildError> {
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(options.user_agent.as_str())
        .timeout_global(Some(Duration::from_secs(120)))
        .max_redirects(5)
        .build()
        .new_agent();
    state.retry_failed(options.max_attempts)?;

    loop {
        let names = state.names_in_phase(Phase::Pending, API_BATCH)?;
        if names.is_empty() {
            break;
        }
        resolve_batch(state, &agent, options, &names)?;
    }

    loop {
        let names = state.names_in_phase(Phase::Resolved, DOWNLOAD_BATCH)?;
        if names.is_empty() {
            break;
        }
        download_batch(state, &agent, options, names)?;
    }

    let completed = state.complete_if_settled(&utc_now())?;
    let phases = state
        .phase_counts()?
        .into_iter()
        .map(|(phase, count)| (phase.as_str(), count))
        .collect();
    Ok(AcquireReport { phases, completed })
}

fn resolve_batch(
    state: &AcquisitionState,
    agent: &ureq::Agent,
    options: &AcquireOptions,
    names: &[String],
) -> Result<(), BuildError> {
    let titles = names
        .iter()
        .map(|name| format!("File:{name}"))
        .collect::<Vec<_>>()
        .join("|");
    let form = [
        ("action", "query"),
        ("format", "json"),
        ("formatversion", "2"),
        ("maxlag", "5"),
        ("redirects", "1"),
        ("prop", "imageinfo"),
        ("iiprop", "url|sha1|timestamp|size|mime|extmetadata"),
        (
            "iiextmetadatafilter",
            "LicenseShortName|License|LicenseUrl|Artist|AttributionRequired|Restrictions",
        ),
        ("titles", titles.as_str()),
    ];
    let response: Value = with_retries(options, "Commons API query", |attempt| {
        let mut response = agent
            .post(&options.api_url)
            .send_form(form)
            .map_err(|error| Attempt::Retry(error.to_string(), None))?;
        let retry_after = retry_after(response.headers());
        let status = response.status().as_u16();
        if matches!(status, 429 | 500 | 502 | 503 | 504) {
            return Err(Attempt::Retry(format!("HTTP {status}"), retry_after));
        }
        if status != 200 {
            return Err(Attempt::Fatal(format!("HTTP {status}")));
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_API_BYTES)
            .read_to_vec()
            .map_err(|error| Attempt::Retry(error.to_string(), None))?;
        let value: Value = serde_json::from_slice(&body)
            .map_err(|error| Attempt::Fatal(format!("invalid API JSON: {error}")))?;
        if let Some(code) = value.pointer("/error/code").and_then(Value::as_str) {
            return if code == "maxlag" {
                Err(Attempt::Retry(
                    format!("maxlag on attempt {attempt}"),
                    retry_after,
                ))
            } else {
                Err(Attempt::Fatal(format!("API error `{code}`")))
            };
        }
        Ok(value)
    })?;

    let mut aliases = BTreeMap::new();
    for key in ["normalized", "redirects"] {
        for alias in response["query"][key].as_array().into_iter().flatten() {
            if let (Some(from), Some(to)) = (alias["from"].as_str(), alias["to"].as_str()) {
                aliases.insert(from.to_owned(), to.to_owned());
            }
        }
    }
    let pages = response["query"]["pages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|page| page["title"].as_str().map(|title| (title.to_owned(), page)))
        .collect::<BTreeMap<_, _>>();
    for name in names {
        let mut title = format!("File:{name}");
        for _ in 0..4 {
            match aliases.get(&title) {
                Some(next) => title.clone_from(next),
                None => break,
            }
        }
        match pages.get(&title).and_then(|page| page_facts(&title, page)) {
            Some(facts) => state.resolve(name, &facts)?,
            None => state.set_phase(name, Phase::Missing, Some("Commons has no such file"))?,
        }
    }
    Ok(())
}

fn page_facts(title: &str, page: &Value) -> Option<CommonsFacts> {
    if page.get("missing").is_some() || page.get("invalid").is_some() {
        return None;
    }
    let info = page["imageinfo"].as_array()?.first()?;
    let metadata = |key: &str| {
        info["extmetadata"][key]["value"]
            .as_str()
            .map(str::to_owned)
    };
    Some(CommonsFacts {
        title: title.to_owned(),
        sha1: info["sha1"].as_str()?.to_owned(),
        timestamp: info["timestamp"].as_str()?.to_owned(),
        media_type: info["mime"].as_str()?.to_owned(),
        size_bytes: info["size"].as_u64()?,
        description_url: info["descriptionurl"].as_str()?.to_owned(),
        original_url: info["url"].as_str()?.split('?').next()?.to_owned(),
        license_short_name: metadata("LicenseShortName"),
        license_identifier: metadata("License"),
        license_url: metadata("LicenseUrl"),
        artist_html: metadata("Artist"),
        attribution_required: metadata("AttributionRequired").map(|value| value == "true"),
        restrictions: metadata("Restrictions").filter(|value| !value.trim().is_empty()),
    })
}

enum Outcome {
    Downloaded,
    Failed(String),
}

fn download_batch(
    state: &AcquisitionState,
    agent: &ureq::Agent,
    options: &AcquireOptions,
    names: Vec<String>,
) -> Result<(), BuildError> {
    let mut jobs = VecDeque::new();
    for name in names {
        let facts = state
            .file(&name)?
            .and_then(|file| file.facts)
            .ok_or_else(|| {
                BuildError::InvalidManifest(format!("`{name}` has no resolved facts"))
            })?;
        jobs.push_back((name, facts));
    }
    let jobs = Mutex::new(jobs);
    let (sender, receiver) = mpsc::channel();
    let directory = state.directory().to_owned();
    thread::scope(|scope| -> Result<(), BuildError> {
        for _ in 0..options.download_workers.max(1) {
            let sender = sender.clone();
            let (jobs, directory) = (&jobs, &directory);
            scope.spawn(move || {
                while let Some((name, facts)) =
                    jobs.lock().ok().and_then(|mut jobs| jobs.pop_front())
                {
                    let outcome = download_one(agent, options, directory, &facts);
                    if sender.send((name, outcome)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(sender);
        for (name, outcome) in receiver {
            match outcome {
                Outcome::Downloaded => state.set_phase(&name, Phase::Downloaded, None)?,
                Outcome::Failed(reason) => state.set_phase(&name, Phase::Failed, Some(&reason))?,
            }
        }
        Ok(())
    })
}

fn download_one(
    agent: &ureq::Agent,
    options: &AcquireOptions,
    directory: &std::path::Path,
    facts: &CommonsFacts,
) -> Outcome {
    let path = original_path(directory, &facts.sha1);
    if fs::read(&path).is_ok_and(|bytes| hex::encode(Sha1::digest(&bytes)) == facts.sha1) {
        return Outcome::Downloaded;
    }
    if facts.size_bytes > options.max_original_bytes {
        return Outcome::Failed(format!(
            "original exceeds {} bytes",
            options.max_original_bytes
        ));
    }
    let bytes = match with_retries(options, "download", |_| {
        let mut response = agent
            .get(&facts.original_url)
            .call()
            .map_err(|error| Attempt::Retry(error.to_string(), None))?;
        let status = response.status().as_u16();
        if matches!(status, 429 | 500 | 502 | 503 | 504) {
            return Err(Attempt::Retry(
                format!("HTTP {status}"),
                retry_after(response.headers()),
            ));
        }
        if status != 200 {
            return Err(Attempt::Fatal(format!("HTTP {status}")));
        }
        response
            .body_mut()
            .with_config()
            .limit(options.max_original_bytes)
            .read_to_vec()
            .map_err(|error| Attempt::Retry(error.to_string(), None))
    }) {
        Ok(bytes) => bytes,
        Err(error) => return Outcome::Failed(error.to_string()),
    };
    if hex::encode(Sha1::digest(&bytes)) != facts.sha1 {
        return Outcome::Failed("download does not match the Commons SHA-1".to_owned());
    }
    match store(&path, &bytes) {
        Ok(()) => Outcome::Downloaded,
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

fn store(path: &std::path::Path, bytes: &[u8]) -> Result<(), BuildError> {
    let parent = path.parent().unwrap_or(path);
    fs::create_dir_all(parent)
        .map_err(|source| io_error("create originals directory", parent, source))?;
    let temporary = path.with_extension("part");
    let mut file =
        fs::File::create(&temporary).map_err(|source| io_error("create", &temporary, source))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error("write", &temporary, source))?;
    fs::rename(&temporary, path).map_err(|source| io_error("rename", path, source))
}

enum Attempt {
    Retry(String, Option<Duration>),
    Fatal(String),
}

fn with_retries<T>(
    options: &AcquireOptions,
    operation: &str,
    mut action: impl FnMut(u32) -> Result<T, Attempt>,
) -> Result<T, BuildError> {
    let mut attempt = 1;
    loop {
        match action(attempt) {
            Ok(value) => return Ok(value),
            Err(Attempt::Fatal(message)) => {
                return Err(BuildError::InvalidManifest(format!(
                    "{operation} failed: {message}"
                )));
            }
            Err(Attempt::Retry(message, retry_after)) => {
                if attempt >= options.request_attempts {
                    return Err(BuildError::InvalidManifest(format!(
                        "{operation} failed after {attempt} attempts: {message}"
                    )));
                }
                let backoff = options.backoff.saturating_mul(1 << (attempt - 1).min(6));
                thread::sleep(retry_after.unwrap_or(backoff).min(Duration::from_secs(600)));
                attempt += 1;
            }
        }
    }
}

fn retry_after(headers: &ureq::http::HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
        .map(Duration::from_secs)
}

/// The current UTC time as an RFC 3339 timestamp with second precision.
#[must_use]
pub fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let remainder = seconds % 86_400;
    // Howard Hinnant's civil-from-days algorithm.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        remainder / 3_600,
        remainder % 3_600 / 60,
        remainder % 60
    )
}
