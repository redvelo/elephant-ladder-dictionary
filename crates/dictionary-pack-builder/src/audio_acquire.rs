use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::BuildError;
use crate::audio_state::{AcquisitionState, CommonsFacts, Phase, original_path};
use crate::builder::io_error;

/// The most concurrent media downloads the Wikimedia robot policy permits.
pub const MAX_DOWNLOAD_CONCURRENCY: usize = 2;

/// Controls for one acquisition run.
#[derive(Clone, Debug)]
pub struct AcquireOptions {
    pub api_url: String,
    /// A Wikimedia User-Agent policy identifier naming the bot and a contact.
    pub user_agent: String,
    /// Capped at [`MAX_DOWNLOAD_CONCURRENCY`].
    pub download_workers: usize,
    /// Total media download speed across workers.
    pub download_bytes_per_second: u64,
    pub max_original_bytes: u64,
    /// Failed files are retried until they have failed this many times.
    pub max_attempts: u64,
    /// Attempts per request after network errors or server errors.
    pub request_attempts: u32,
    /// Base delay for exponential backoff after network errors.
    pub backoff: Duration,
    /// Pause after a rate limit that gives no `Retry-After`.
    pub rate_limit_pause: Duration,
    /// Pause of every request after a server error.
    pub server_error_pause: Duration,
    /// Consecutive rate limits after which the run stops.
    pub max_consecutive_rate_limits: u32,
    /// One slice of the work, as `(ordinal, total)` with `ordinal` in `1..=total`.
    ///
    /// Slices are disjoint and derived from the file name, so several machines can run
    /// against copies of the same state without coordinating. Originals are content
    /// addressed, so their directories merge by copying.
    pub shard: Option<(u32, u32)>,
}

impl AcquireOptions {
    /// Wikimedia Commons with the robot policy's media limits: two concurrent
    /// downloads, 25 Mbps, and a 15 minute pause after server errors.
    #[must_use]
    pub fn commons(contact: &str) -> Self {
        Self {
            api_url: "https://commons.wikimedia.org/w/api.php".to_owned(),
            user_agent: format!(
                "ElephantLadderDictionaryBot/{} ({contact}) ureq/3",
                env!("CARGO_PKG_VERSION")
            ),
            download_workers: MAX_DOWNLOAD_CONCURRENCY,
            download_bytes_per_second: 25_000_000 / 8,
            max_original_bytes: 20 * 1024 * 1024,
            max_attempts: 3,
            request_attempts: 6,
            backoff: Duration::from_secs(5),
            rate_limit_pause: Duration::from_secs(60),
            server_error_pause: Duration::from_secs(15 * 60),
            max_consecutive_rate_limits: 10,
            shard: None,
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
    let throttle = Throttle::new(options);

    let mut cursor: Option<String> = None;
    loop {
        let names = state.names_in_phase_after(Phase::Pending, cursor.as_deref(), API_BATCH)?;
        let Some(last) = names.last().cloned() else {
            break;
        };
        let mine = retain_shard(names, options.shard);
        if !mine.is_empty() {
            resolve_batch(state, &agent, options, &throttle, &mine)?;
        }
        // A sharded run leaves other shards' names in place, so it walks past them.
        cursor = options.shard.map(|_| last);
    }

    let mut cursor: Option<String> = None;
    loop {
        let names =
            state.names_in_phase_after(Phase::Resolved, cursor.as_deref(), DOWNLOAD_BATCH)?;
        let Some(last) = names.last().cloned() else {
            break;
        };
        let mine = retain_shard(names, options.shard);
        if !mine.is_empty() {
            download_batch(state, &agent, options, &throttle, mine)?;
        }
        cursor = options.shard.map(|_| last);
    }

    let completed = state.complete_if_settled(&utc_now())?;
    let phases = state
        .phase_counts()?
        .into_iter()
        .map(|(phase, count)| (phase.as_str(), count))
        .collect();
    Ok(AcquireReport { phases, completed })
}

/// Keeps the names belonging to this run's shard.
fn retain_shard(names: Vec<String>, shard: Option<(u32, u32)>) -> Vec<String> {
    let Some((ordinal, total)) = shard else {
        return names;
    };
    names
        .into_iter()
        .filter(|name| shard_of(name, total) == ordinal)
        .collect()
}

/// The shard a file name belongs to, from a stable hash so every machine agrees.
fn shard_of(name: &str, total: u32) -> u32 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    u32::try_from(hash % u64::from(total)).unwrap_or(0) + 1
}

fn resolve_batch(
    state: &AcquisitionState,
    agent: &ureq::Agent,
    options: &AcquireOptions,
    throttle: &Throttle,
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
    let response: Value = request(throttle, options, || {
        let mut response = agent
            .post(&options.api_url)
            .send_form(form)
            .map_err(|error| Attempt::Transient(error.to_string()))?;
        check_status(&response)?;
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_API_BYTES)
            .read_to_vec()
            .map_err(|error| Attempt::Transient(error.to_string()))?;
        let value: Value = serde_json::from_slice(&body)
            .map_err(|error| Attempt::Fatal(format!("invalid API JSON: {error}")))?;
        match value.pointer("/error/code").and_then(Value::as_str) {
            Some("maxlag") => Err(Attempt::RateLimited(retry_after(response.headers()))),
            Some(code) => Err(Attempt::Fatal(format!("API error `{code}`"))),
            None => Ok((value, 0)),
        }
    })
    .map_err(|error| BuildError::InvalidManifest(format!("Commons API query stopped: {error}")))?;

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
    Stopped(String),
}

fn download_batch(
    state: &AcquisitionState,
    agent: &ureq::Agent,
    options: &AcquireOptions,
    throttle: &Throttle,
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
    let stopped = AtomicBool::new(false);
    let (sender, receiver) = mpsc::channel();
    let directory = state.directory().to_owned();
    thread::scope(|scope| -> Result<(), BuildError> {
        for _ in 0..options.download_workers.clamp(1, MAX_DOWNLOAD_CONCURRENCY) {
            let sender = sender.clone();
            let (jobs, directory, stopped) = (&jobs, &directory, &stopped);
            scope.spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    let Some((name, facts)) =
                        jobs.lock().ok().and_then(|mut jobs| jobs.pop_front())
                    else {
                        return;
                    };
                    let outcome = download_one(agent, options, throttle, directory, &facts);
                    if matches!(outcome, Outcome::Stopped(_)) {
                        stopped.store(true, Ordering::Release);
                    }
                    if sender.send((name, outcome)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(sender);
        let mut stop = None;
        for (name, outcome) in receiver {
            match outcome {
                Outcome::Downloaded => state.set_phase(&name, Phase::Downloaded, None)?,
                Outcome::Failed(reason) => state.set_phase(&name, Phase::Failed, Some(&reason))?,
                Outcome::Stopped(reason) => stop = Some(reason),
            }
        }
        match stop {
            Some(reason) => Err(BuildError::InvalidManifest(format!(
                "downloads stopped: {reason}"
            ))),
            None => Ok(()),
        }
    })
}

fn download_one(
    agent: &ureq::Agent,
    options: &AcquireOptions,
    throttle: &Throttle,
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
    let bytes = match request(throttle, options, || {
        let mut response = agent
            .get(&facts.original_url)
            .call()
            .map_err(|error| Attempt::Transient(error.to_string()))?;
        check_status(&response)?;
        let bytes = response
            .body_mut()
            .with_config()
            .limit(options.max_original_bytes)
            .read_to_vec()
            .map_err(|error| Attempt::Transient(error.to_string()))?;
        let size = bytes.len() as u64;
        Ok((bytes, size))
    }) {
        Ok(bytes) => bytes,
        Err(RequestError::Stopped(reason)) => return Outcome::Stopped(reason),
        Err(RequestError::Failed(reason)) => return Outcome::Failed(reason),
    };
    if hex::encode(Sha1::digest(&bytes)) != facts.sha1 {
        // A complete transfer whose digest still disagrees means Commons' own metadata
        // does not describe the bytes it serves, and retrying will never fix it.
        return Outcome::Failed(if bytes.len() as u64 == facts.size_bytes {
            format!(
                "Commons served {} bytes whose SHA-1 does not match its own metadata",
                bytes.len()
            )
        } else {
            format!(
                "download does not match the Commons SHA-1 ({} of {} bytes)",
                bytes.len(),
                facts.size_bytes
            )
        });
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
    /// A network error; retried with backoff.
    Transient(String),
    /// HTTP 429 or API `maxlag`; every request pauses.
    RateLimited(Option<Duration>),
    /// HTTP 5xx; every request pauses for the server error pause.
    ServerError(u16),
    Fatal(String),
}

enum RequestError {
    /// The server keeps rate limiting; the whole run must stop.
    Stopped(String),
    Failed(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped(reason) | Self::Failed(reason) => formatter.write_str(reason),
        }
    }
}

fn check_status(response: &ureq::http::Response<ureq::Body>) -> Result<(), Attempt> {
    match response.status().as_u16() {
        200 => Ok(()),
        429 => Err(Attempt::RateLimited(retry_after(response.headers()))),
        status @ 500..=599 => Err(Attempt::ServerError(status)),
        status => Err(Attempt::Fatal(format!("HTTP {status}"))),
    }
}

/// Pacing shared by every request of a run: rate limits and server errors pause
/// all requests, and completed downloads pace later ones to the byte rate.
struct Throttle {
    resume_at: Mutex<Instant>,
    consecutive_rate_limits: Mutex<u32>,
    bytes_per_second: u64,
    rate_limit_pause: Duration,
    max_consecutive_rate_limits: u32,
}

impl Throttle {
    fn new(options: &AcquireOptions) -> Self {
        Self {
            resume_at: Mutex::new(Instant::now()),
            consecutive_rate_limits: Mutex::new(0),
            bytes_per_second: options.download_bytes_per_second,
            rate_limit_pause: options.rate_limit_pause,
            max_consecutive_rate_limits: options.max_consecutive_rate_limits,
        }
    }

    fn wait(&self) {
        loop {
            let resume_at = *self
                .resume_at
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            if now >= resume_at {
                return;
            }
            thread::sleep(resume_at - now);
        }
    }

    fn pause(&self, duration: Duration) {
        let mut resume_at = self
            .resume_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *resume_at = (*resume_at).max(Instant::now() + duration);
    }

    fn rate_limited(&self, retry_after: Option<Duration>) -> Result<(), RequestError> {
        let mut count = self
            .consecutive_rate_limits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *count += 1;
        if *count > self.max_consecutive_rate_limits {
            return Err(RequestError::Stopped(format!(
                "rate limited {} consecutive times",
                *count
            )));
        }
        drop(count);
        self.pause(retry_after.unwrap_or(self.rate_limit_pause));
        Ok(())
    }

    fn completed(&self, bytes: u64) {
        *self
            .consecutive_rate_limits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = 0;
        if self.bytes_per_second > 0 && bytes > 0 {
            let delay =
                Duration::from_nanos(bytes.saturating_mul(1_000_000_000) / self.bytes_per_second);
            let mut resume_at = self
                .resume_at
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *resume_at = (*resume_at).max(Instant::now()) + delay;
        }
    }
}

/// Runs one request under the shared throttle. The action returns its value and the
/// bytes it transferred.
fn request<T>(
    throttle: &Throttle,
    options: &AcquireOptions,
    mut action: impl FnMut() -> Result<(T, u64), Attempt>,
) -> Result<T, RequestError> {
    let mut failures = 0;
    loop {
        throttle.wait();
        let reason = match action() {
            Ok((value, bytes)) => {
                throttle.completed(bytes);
                return Ok(value);
            }
            Err(Attempt::Fatal(reason)) => return Err(RequestError::Failed(reason)),
            Err(Attempt::RateLimited(retry_after)) => {
                throttle.rate_limited(retry_after)?;
                continue;
            }
            Err(Attempt::ServerError(status)) => {
                throttle.pause(options.server_error_pause);
                format!("HTTP {status}")
            }
            Err(Attempt::Transient(reason)) => {
                thread::sleep(options.backoff.saturating_mul(1 << failures.min(6)));
                reason
            }
        };
        failures += 1;
        if failures >= options.request_attempts {
            return Err(RequestError::Failed(format!(
                "failed after {failures} attempts: {reason}"
            )));
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

#[cfg(test)]
mod tests {
    use super::{retain_shard, shard_of};

    fn names() -> Vec<String> {
        (0..500)
            .map(|index| format!("LL-Q150 (fra)-Speaker-word{index}.wav"))
            .collect()
    }

    #[test]
    fn shards_are_disjoint_and_cover_every_name() {
        let names = names();
        let total = 3;
        let mut seen = Vec::new();
        for ordinal in 1..=total {
            let mine = retain_shard(names.clone(), Some((ordinal, total)));
            assert!(!mine.is_empty(), "shard {ordinal} of {total} has no work");
            seen.extend(mine);
        }
        seen.sort();
        let mut expected = names;
        expected.sort();
        // Every machine downloads a different slice, and together they leave nothing behind.
        assert_eq!(seen, expected);
    }

    #[test]
    fn a_name_always_belongs_to_the_same_shard() {
        let name = "LL-Q150 (fra)-Speaker-cubi.wav";
        assert_eq!(shard_of(name, 4), shard_of(name, 4));
        assert!((1..=4).contains(&shard_of(name, 4)));
        assert_eq!(shard_of(name, 1), 1);
    }

    #[test]
    fn an_unsharded_run_keeps_every_name() {
        assert_eq!(retain_shard(names(), None).len(), 500);
    }
}
