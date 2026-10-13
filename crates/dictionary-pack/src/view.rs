use std::collections::BTreeSet;
use std::sync::Arc;

use thiserror::Error;

use crate::{
    DictionaryPack, Entry, EntryReference, FramedDigest, LookupOptions, LookupOutcome, PackError,
    ProjectionOptions, Section, SectionPage, SnapshotRevision,
};

const SNAPSHOT_REVISION_DOMAIN: &[u8] = b"ELDICT-SNAPSHOT-V1";

/// One enabled pack view: the whole pack, or its translations into one target language.
#[derive(Clone)]
pub struct DictionaryView {
    view_id: String,
    pack: Arc<DictionaryPack>,
    target_language: Option<String>,
}

impl DictionaryView {
    /// Creates a named view. A target language restricts translations to that exact
    /// language code and makes lookups distinguish absent translations from absent
    /// entries.
    ///
    /// # Errors
    ///
    /// Returns an error when the view identifier or target language is empty.
    pub fn new(
        view_id: impl Into<String>,
        pack: Arc<DictionaryPack>,
        target_language: Option<&str>,
    ) -> Result<Self, SnapshotError> {
        let view_id = view_id.into();
        if view_id.is_empty() {
            return Err(SnapshotError::InvalidView(
                "view identifier must not be empty".to_owned(),
            ));
        }
        if target_language == Some("") {
            return Err(SnapshotError::InvalidView(
                "target language must not be empty".to_owned(),
            ));
        }
        Ok(Self {
            view_id,
            pack,
            target_language: target_language.map(str::to_owned),
        })
    }

    #[must_use]
    pub fn view_id(&self) -> &str {
        &self.view_id
    }

    #[must_use]
    pub fn pack(&self) -> &Arc<DictionaryPack> {
        &self.pack
    }

    #[must_use]
    pub fn target_language(&self) -> Option<&str> {
        self.target_language.as_deref()
    }

    /// Performs an exact lookup in this view.
    #[must_use]
    pub fn lookup(&self, query: &str, options: LookupOptions) -> ViewOutcome {
        let outcome = match self.pack.lookup(query, self.target_language(), options) {
            Ok(outcome) => outcome,
            Err(error) => return ViewOutcome::Failed(error),
        };
        if outcome.matches.is_empty() {
            return ViewOutcome::NoEntry;
        }
        let translated = self.target_language.is_none()
            || outcome.matches.iter().any(|matched| {
                matched.entry.translations.total > 0 || matched.entry.sense_translation_total > 0
            });
        if translated {
            ViewOutcome::Completed(outcome)
        } else {
            ViewOutcome::NoTranslation(outcome)
        }
    }

    /// Reads one entry of this view.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`DictionaryPack::entry`].
    pub fn entry(
        &self,
        reference: &EntryReference,
        projection: ProjectionOptions,
    ) -> Result<Entry, PackError> {
        self.pack
            .entry(reference, self.target_language(), projection)
    }

    /// Reads one section page of one entry of this view.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`DictionaryPack::section_page`].
    pub fn section_page(
        &self,
        reference: &EntryReference,
        section: Section,
        offset: usize,
        limit: usize,
        projection: ProjectionOptions,
    ) -> Result<SectionPage, PackError> {
        self.pack.section_page(
            reference,
            self.target_language(),
            section,
            offset,
            limit,
            projection,
        )
    }
}

/// The outcome of one view in a lookup.
#[derive(Debug)]
pub enum ViewOutcome {
    Completed(LookupOutcome),
    NoEntry,
    /// Entries matched, but none has a translation into the view's target language.
    NoTranslation(LookupOutcome),
    Failed(PackError),
}

/// One view and its lookup outcome.
#[derive(Debug)]
pub struct ViewLookup<'a> {
    pub view_id: &'a str,
    pub outcome: ViewOutcome,
}

/// An immutable, ordered dictionary configuration.
pub struct DictionarySnapshot {
    revision: SnapshotRevision,
    views: Vec<DictionaryView>,
}

impl DictionarySnapshot {
    /// Creates a deterministic snapshot and rejects duplicate view identifiers.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate view identifiers.
    pub fn new(views: Vec<DictionaryView>) -> Result<Self, SnapshotError> {
        let mut identifiers = BTreeSet::new();
        let mut digest = FramedDigest::new(SNAPSHOT_REVISION_DOMAIN);
        digest.field(&(views.len() as u64).to_be_bytes());
        for view in &views {
            if !identifiers.insert(view.view_id()) {
                return Err(SnapshotError::DuplicateView(view.view_id().to_owned()));
            }
            digest.field(view.view_id.as_bytes());
            digest.field(view.pack.pack_id().as_str().as_bytes());
            digest.field(view.pack.pack_revision().as_bytes());
            digest.field(match view.target_language {
                None => b"monolingual",
                Some(_) => b"bilingual",
            });
            digest.field(view.target_language().unwrap_or("").as_bytes());
        }
        Ok(Self {
            revision: SnapshotRevision::from_bytes(digest.finish()),
            views,
        })
    }

    #[must_use]
    pub const fn revision(&self) -> &SnapshotRevision {
        &self.revision
    }

    #[must_use]
    pub fn views(&self) -> &[DictionaryView] {
        &self.views
    }

    /// Looks up `query` in every view.
    ///
    /// Views whose corpus language shares the primary subtag of `language` come
    /// first; configured order is kept within each group. A failed view does not
    /// affect other views.
    #[must_use]
    pub fn lookup(
        &self,
        query: &str,
        language: Option<&str>,
        options: LookupOptions,
    ) -> Vec<ViewLookup<'_>> {
        let hint = language.map(primary_subtag).filter(|hint| !hint.is_empty());
        let matches_hint = |view: &&DictionaryView| {
            hint.as_deref()
                .is_some_and(|hint| primary_subtag(view.pack().corpus_language()) == hint)
        };
        let (preferred, others): (Vec<_>, Vec<_>) = self.views.iter().partition(matches_hint);
        preferred
            .into_iter()
            .chain(others)
            .map(|view| ViewLookup {
                view_id: view.view_id(),
                outcome: view.lookup(query, options),
            })
            .collect()
    }
}

fn primary_subtag(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Snapshot construction failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum SnapshotError {
    #[error("invalid dictionary view: {0}")]
    InvalidView(String),
    #[error("duplicate dictionary view identifier: {0}")]
    DuplicateView(String),
}
