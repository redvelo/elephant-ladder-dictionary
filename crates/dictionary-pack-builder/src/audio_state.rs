use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::BuildError;
use elephant_ladder_dictionary_pack::{RecordingLicense, Sha256Hex};

const STATE_FILE: &str = "acquisition.sqlite";
const ORIGINALS_DIRECTORY: &str = "originals";
const STATE_SCHEMA: &str = "
PRAGMA application_id = 0x454c4151;
PRAGMA user_version = 1;
CREATE TABLE acquisition_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    corpus_pack_id TEXT NOT NULL,
    corpus_revision TEXT NOT NULL,
    corpus_language TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT
) STRICT;
CREATE TABLE files (
    file_name TEXT PRIMARY KEY,
    reference_count INTEGER NOT NULL CHECK (reference_count > 0),
    phase TEXT NOT NULL CHECK (phase IN ('pending', 'resolved', 'downloaded', 'missing', 'failed')),
    reason TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    title TEXT,
    sha1 TEXT,
    timestamp TEXT,
    media_type TEXT,
    size_bytes INTEGER,
    description_url TEXT,
    original_url TEXT,
    license_short_name TEXT,
    license_identifier TEXT,
    license_url TEXT,
    artist_html TEXT,
    attribution_required INTEGER,
    restrictions TEXT
) STRICT, WITHOUT ROWID;
CREATE INDEX files_phase ON files (phase, file_name);
";

/// The corpus references an acquisition collects, as written by `audio references`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSet {
    pub schema_version: u32,
    pub corpus_pack_id: String,
    pub corpus_revision: Sha256Hex,
    pub corpus_language: String,
    /// Normalized Commons file names with referencing sound counts, sorted by name.
    pub files: Vec<(String, u64)>,
    /// Authored values that are not plausible file names.
    pub invalid: Vec<(String, u64)>,
}

/// Acquisition progress of one referenced file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Pending,
    Resolved,
    Downloaded,
    Missing,
    Failed,
}

impl Phase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resolved => "resolved",
            Self::Downloaded => "downloaded",
            Self::Missing => "missing",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, BuildError> {
        Ok(match value {
            "pending" => Self::Pending,
            "resolved" => Self::Resolved,
            "downloaded" => Self::Downloaded,
            "missing" => Self::Missing,
            "failed" => Self::Failed,
            other => {
                return Err(BuildError::InvalidManifest(format!(
                    "unknown acquisition phase `{other}`"
                )));
            }
        })
    }
}

/// Commons facts about one file.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommonsFacts {
    pub title: String,
    pub sha1: String,
    pub timestamp: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub description_url: String,
    pub original_url: String,
    pub license_short_name: Option<String>,
    pub license_identifier: Option<String>,
    pub license_url: Option<String>,
    pub artist_html: Option<String>,
    pub attribution_required: Option<bool>,
    pub restrictions: Option<String>,
}

/// The persisted state of one referenced file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileState {
    pub file_name: String,
    pub reference_count: u64,
    pub phase: Phase,
    pub reason: Option<String>,
    pub attempts: u64,
    pub facts: Option<CommonsFacts>,
}

/// A resumable acquisition directory.
pub struct AcquisitionState {
    directory: PathBuf,
    connection: Connection,
}

impl AcquisitionState {
    /// Opens an acquisition directory, creating it from `references` when absent.
    ///
    /// # Errors
    ///
    /// Returns an error when an existing state belongs to different references.
    pub fn open_or_create(
        directory: &Path,
        references: &ReferenceSet,
        started_at: &str,
    ) -> Result<Self, BuildError> {
        let path = directory.join(STATE_FILE);
        let exists = path.exists();
        std::fs::create_dir_all(directory.join(ORIGINALS_DIRECTORY)).map_err(|source| {
            crate::builder::io_error("create acquisition directory", directory, source)
        })?;
        let mut connection = Connection::open(&path)?;
        connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        if exists {
            let state = Self {
                directory: directory.to_owned(),
                connection,
            };
            let (pack, revision, count): (String, String, i64) = state.connection.query_row(
                "SELECT corpus_pack_id, corpus_revision, (SELECT count(*) FROM files) \
                 FROM acquisition_metadata",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if pack != references.corpus_pack_id
                || revision != references.corpus_revision.to_hex()
                || usize::try_from(count).ok() != Some(references.files.len())
            {
                return Err(BuildError::InvalidManifest(
                    "acquisition state belongs to different references".to_owned(),
                ));
            }
            return Ok(state);
        }
        connection.execute_batch(STATE_SCHEMA)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO acquisition_metadata \
             (singleton, corpus_pack_id, corpus_revision, corpus_language, started_at) \
             VALUES (1, ?1, ?2, ?3, ?4)",
            params![
                references.corpus_pack_id,
                references.corpus_revision.to_hex(),
                references.corpus_language,
                started_at
            ],
        )?;
        {
            let mut insert = transaction.prepare(
                "INSERT INTO files (file_name, reference_count, phase) VALUES (?1, ?2, 'pending')",
            )?;
            for (file_name, count) in &references.files {
                insert.execute(params![
                    file_name,
                    crate::builder::sql_i64("reference count", *count)?
                ])?;
            }
        }
        transaction.commit()?;
        Ok(Self {
            directory: directory.to_owned(),
            connection,
        })
    }

    /// Opens an existing acquisition directory.
    ///
    /// # Errors
    ///
    /// Returns an error when no state exists.
    pub fn open(directory: &Path) -> Result<Self, BuildError> {
        let path = directory.join(STATE_FILE);
        if !path.is_file() {
            return Err(BuildError::InvalidManifest(format!(
                "no acquisition state in {}",
                directory.display()
            )));
        }
        Ok(Self {
            directory: directory.to_owned(),
            connection: Connection::open(path)?,
        })
    }

    /// Where a verified original with `sha1` is stored.
    #[must_use]
    pub fn original_path(&self, sha1: &str) -> PathBuf {
        original_path(&self.directory, sha1)
    }

    /// Corpus identity and acquisition times.
    ///
    /// # Errors
    ///
    /// Returns storage errors.
    pub fn metadata(&self) -> Result<(String, String, String, String, Option<String>), BuildError> {
        Ok(self.connection.query_row(
            "SELECT corpus_pack_id, corpus_revision, corpus_language, started_at, completed_at \
             FROM acquisition_metadata",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?)
    }

    /// File names in `phase`, in name order, at most `limit`.
    ///
    /// # Errors
    ///
    /// Returns storage errors.
    pub fn names_in_phase(&self, phase: Phase, limit: usize) -> Result<Vec<String>, BuildError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT file_name FROM files WHERE phase = ?1 ORDER BY file_name LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![phase.as_str(), i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| row.get(0),
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Counts files per phase.
    ///
    /// # Errors
    ///
    /// Returns storage errors.
    pub fn phase_counts(&self) -> Result<Vec<(Phase, u64)>, BuildError> {
        let mut statement = self
            .connection
            .prepare("SELECT phase, count(*) FROM files GROUP BY phase ORDER BY phase")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.map(|row| {
            let (phase, count) = row?;
            Ok((Phase::parse(&phase)?, u64::try_from(count).unwrap_or(0)))
        })
        .collect()
    }

    /// Records resolved Commons facts for a file.
    ///
    /// # Errors
    ///
    /// Returns storage errors.
    pub fn resolve(&self, file_name: &str, facts: &CommonsFacts) -> Result<(), BuildError> {
        self.connection.execute(
            "UPDATE files SET phase = 'resolved', reason = NULL, title = ?2, sha1 = ?3, \
             timestamp = ?4, media_type = ?5, size_bytes = ?6, description_url = ?7, \
             original_url = ?8, license_short_name = ?9, license_identifier = ?10, \
             license_url = ?11, artist_html = ?12, attribution_required = ?13, \
             restrictions = ?14 WHERE file_name = ?1",
            params![
                file_name,
                facts.title,
                facts.sha1,
                facts.timestamp,
                facts.media_type,
                crate::builder::sql_i64("size", facts.size_bytes)?,
                facts.description_url,
                facts.original_url,
                facts.license_short_name,
                facts.license_identifier,
                facts.license_url,
                facts.artist_html,
                facts.attribution_required,
                facts.restrictions,
            ],
        )?;
        Ok(())
    }

    /// Moves a file to `phase` with an optional reason, counting failed attempts.
    ///
    /// # Errors
    ///
    /// Returns storage errors.
    pub fn set_phase(
        &self,
        file_name: &str,
        phase: Phase,
        reason: Option<&str>,
    ) -> Result<(), BuildError> {
        self.connection.execute(
            "UPDATE files SET phase = ?2, reason = ?3, \
             attempts = attempts + (CASE WHEN ?2 = 'failed' THEN 1 ELSE 0 END) \
             WHERE file_name = ?1",
            params![file_name, phase.as_str(), reason],
        )?;
        Ok(())
    }

    /// Returns failed files with fewer than `max_attempts` attempts to their prior phase.
    ///
    /// # Errors
    ///
    /// Returns storage errors.
    pub fn retry_failed(&self, max_attempts: u64) -> Result<u64, BuildError> {
        let changed = self.connection.execute(
            "UPDATE files SET phase = CASE WHEN sha1 IS NULL THEN 'pending' ELSE 'resolved' END, \
             reason = NULL WHERE phase = 'failed' AND attempts < ?1",
            [crate::builder::sql_i64("attempts", max_attempts)?],
        )?;
        Ok(changed as u64)
    }

    /// Marks acquisition complete when no file remains pending or resolved.
    ///
    /// # Errors
    ///
    /// Returns storage errors.
    pub fn complete_if_settled(&self, completed_at: &str) -> Result<bool, BuildError> {
        let unsettled: i64 = self.connection.query_row(
            "SELECT count(*) FROM files WHERE phase IN ('pending', 'resolved')",
            [],
            |row| row.get(0),
        )?;
        if unsettled == 0 {
            self.connection.execute(
                "UPDATE acquisition_metadata SET completed_at = coalesce(completed_at, ?1)",
                [completed_at],
            )?;
        }
        Ok(unsettled == 0)
    }

    /// Reads one file's state.
    ///
    /// # Errors
    ///
    /// Returns storage errors.
    pub fn file(&self, file_name: &str) -> Result<Option<FileState>, BuildError> {
        let mut statement = self
            .connection
            .prepare_cached(&format!("{FILE_SELECT} WHERE file_name = ?1"))?;
        let raw = statement.query_row([file_name], raw_file).optional()?;
        raw.map(file_from_raw).transpose()
    }

    /// Reads every file's state in name order.
    ///
    /// # Errors
    ///
    /// Returns storage errors.
    pub fn files(&self) -> Result<Vec<FileState>, BuildError> {
        let mut statement = self
            .connection
            .prepare(&format!("{FILE_SELECT} ORDER BY file_name"))?;
        let rows = statement.query_map([], raw_file)?;
        rows.map(|row| file_from_raw(row?)).collect()
    }
}

const FILE_SELECT: &str = "SELECT file_name, reference_count, phase, reason, attempts, title, sha1, \
     timestamp, media_type, size_bytes, description_url, original_url, license_short_name, \
     license_identifier, license_url, artist_html, attribution_required, restrictions FROM files";

type RawFile = (
    String,
    i64,
    String,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<bool>,
    Option<String>,
);

fn raw_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawFile> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
    ))
}

fn file_from_raw(raw: RawFile) -> Result<FileState, BuildError> {
    let facts = match (raw.5, raw.6) {
        (Some(title), Some(sha1)) => Some(CommonsFacts {
            title,
            sha1,
            timestamp: raw.7.unwrap_or_default(),
            media_type: raw.8.unwrap_or_default(),
            size_bytes: raw.9.and_then(|size| u64::try_from(size).ok()).unwrap_or(0),
            description_url: raw.10.unwrap_or_default(),
            original_url: raw.11.unwrap_or_default(),
            license_short_name: raw.12,
            license_identifier: raw.13,
            license_url: raw.14,
            artist_html: raw.15,
            attribution_required: raw.16,
            restrictions: raw.17,
        }),
        _ => None,
    };
    Ok(FileState {
        file_name: raw.0,
        reference_count: u64::try_from(raw.1).unwrap_or(0),
        phase: Phase::parse(&raw.2)?,
        reason: raw.3,
        attempts: u64::try_from(raw.4).unwrap_or(0),
        facts,
    })
}

/// Where a verified original with `sha1` is stored in an acquisition directory.
#[must_use]
pub fn original_path(directory: &Path, sha1: &str) -> PathBuf {
    directory
        .join(ORIGINALS_DIRECTORY)
        .join(&sha1[..2.min(sha1.len())])
        .join(sha1)
}

impl AcquisitionState {
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

/// Whether a Commons license is redistributable under launch rules, and why not.
///
/// # Errors
///
/// Returns the reason a license is unqualified.
pub fn qualify_license(facts: &CommonsFacts) -> Result<RecordingLicense, String> {
    if facts
        .restrictions
        .as_deref()
        .is_some_and(|restrictions| !restrictions.trim().is_empty())
    {
        return Err(format!(
            "Commons restrictions need review: {}",
            facts.restrictions.as_deref().unwrap_or_default()
        ));
    }
    let Some(short_name) = facts.license_short_name.as_deref() else {
        return Err("Commons publishes no license".to_owned());
    };
    if !is_allowed_license(short_name) {
        return Err(format!("license `{short_name}` is not on the allowlist"));
    }
    Ok(RecordingLicense {
        short_name: short_name.to_owned(),
        identifier: facts.license_identifier.clone(),
        url: facts.license_url.clone(),
    })
}

fn is_allowed_license(short_name: &str) -> bool {
    if matches!(short_name, "CC0" | "Public domain" | "GFDL") {
        return true;
    }
    let Some(rest) = short_name
        .strip_prefix("CC BY-SA ")
        .or_else(|| short_name.strip_prefix("CC BY "))
    else {
        return short_name.starts_with("GFDL ");
    };
    let mut parts = rest.split(' ');
    let version = parts.next().unwrap_or_default();
    let port = parts.next();
    let valid_version = matches!(version, "1.0" | "2.0" | "2.5" | "3.0" | "4.0");
    let valid_port =
        port.is_none_or(|port| port.len() == 2 && port.bytes().all(|b| b.is_ascii_lowercase()));
    valid_version && valid_port && parts.next().is_none()
}

/// Converts the Commons `Artist` HTML into plain text and linked page URLs.
#[must_use]
pub fn author_from_html(html_text: &str) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut urls = Vec::new();
    let mut rest = html_text;
    while let Some(start) = rest.find('<') {
        text.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[start + 1..start + end];
        let name = tag
            .trim_start_matches('/')
            .split(|character: char| character.is_whitespace() || character == '/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(name.as_str(), "br" | "li" | "p" | "div" | "tr") {
            text.push('\n');
        } else if name == "a"
            && let Some(href) = attribute(tag, "href")
        {
            let href = decode_entities(&href);
            let url = if href.starts_with("//") {
                format!("https:{href}")
            } else {
                href
            };
            if (url.starts_with("https://") || url.starts_with("http://")) && !urls.contains(&url) {
                urls.push(url);
            }
        }
        rest = &rest[start + end + 1..];
    }
    text.push_str(rest);
    let decoded = decode_entities(&text);
    let lines = decoded
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    (lines.join("\n"), urls)
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let position = lower.find(&format!("{name}="))?;
    let value = &tag[position + name.len() + 1..];
    let quote = value.chars().next()?;
    if quote == '"' || quote == '\'' {
        value[1..].find(quote).map(|end| value[1..=end].to_owned())
    } else {
        Some(value.split_whitespace().next()?.to_owned())
    }
}

fn decode_entities(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        output.push_str(&rest[..start]);
        let candidate = &rest[start..];
        let Some(end) = candidate.find(';').filter(|end| *end <= 10) else {
            output.push('&');
            rest = &candidate[1..];
            continue;
        };
        let entity = &candidate[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|decimal| decimal.parse().ok())
                })
                .and_then(char::from_u32),
        };
        if let Some(character) = decoded {
            output.push(character);
            rest = &candidate[end + 1..];
        } else {
            output.push('&');
            rest = &candidate[1..];
        }
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn licenses_follow_the_launch_allowlist() {
        for allowed in [
            "CC0",
            "Public domain",
            "GFDL",
            "GFDL 1.2",
            "CC BY-SA 4.0",
            "CC BY-SA 3.0 us",
            "CC BY 2.0 fr",
            "CC BY 2.5",
        ] {
            assert!(is_allowed_license(allowed), "{allowed}");
        }
        for rejected in [
            "CC BY-NC 4.0",
            "CC BY-SA 4.0 international",
            "Fair use",
            "CC BY-ND 3.0",
            "",
        ] {
            assert!(!is_allowed_license(rejected), "{rejected}");
        }
    }

    #[test]
    fn restrictions_and_missing_licenses_are_unqualified() {
        let mut facts = CommonsFacts {
            license_short_name: Some("CC0".to_owned()),
            ..CommonsFacts::default()
        };
        assert!(qualify_license(&facts).is_ok());
        facts.restrictions = Some("personality".to_owned());
        assert!(
            qualify_license(&facts)
                .unwrap_err()
                .contains("restrictions")
        );
        facts.restrictions = None;
        facts.license_short_name = None;
        assert!(qualify_license(&facts).is_err());
    }

    #[test]
    fn commons_artist_html_becomes_text_and_links() {
        let (text, urls) = author_from_html(
            "<ul><li>Speaker: <a href=\"//lingualibre.org/wiki/Q1510058\" class=\"extiw\">Antochkat</a></li>\
             <li>Recorder: <a href=\"//commons.wikimedia.org/wiki/User:Antochkat\">Antochkat</a></li></ul>",
        );
        assert_eq!(text, "Speaker: Antochkat\nRecorder: Antochkat");
        assert_eq!(
            urls,
            [
                "https://lingualibre.org/wiki/Q1510058",
                "https://commons.wikimedia.org/wiki/User:Antochkat"
            ]
        );
        assert_eq!(
            author_from_html("Marta Carbone, Association Shtooka &amp; friends&#x21;").0,
            "Marta Carbone, Association Shtooka & friends!"
        );
    }
}
