use std::collections::BTreeSet;
use std::sync::Arc;

use arc_swap::ArcSwap;
use thiserror::Error;

use crate::{
    DictionaryPack, Entry, EntryReference, LookupOptions, LookupOutcome, PackError,
    ProjectionOptions, Section, SectionPage, SnapshotRevision, SnapshotViewIdentity, ViewType,
};

/// One enabled monolingual pack view.
#[derive(Clone)]
pub struct MonolingualView {
    view_id: String,
    pack: Arc<DictionaryPack>,
}

impl MonolingualView {
    /// Creates a named monolingual view.
    ///
    /// # Errors
    ///
    /// Returns an error when the view identifier is empty.
    pub fn new(
        view_id: impl Into<String>,
        pack: Arc<DictionaryPack>,
    ) -> Result<Self, SnapshotError> {
        let view_id = view_id.into();
        validate_view_id(&view_id)?;
        Ok(Self { view_id, pack })
    }

    #[must_use]
    pub fn view_id(&self) -> &str {
        &self.view_id
    }

    #[must_use]
    pub fn pack(&self) -> &Arc<DictionaryPack> {
        &self.pack
    }

    /// Performs an exact lookup in this view.
    ///
    /// # Errors
    ///
    /// Returns pack limit, storage, or record-authentication errors.
    pub fn lookup(&self, query: &str, options: LookupOptions) -> Result<LookupOutcome, PackError> {
        self.pack.lookup(query, options)
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
        self.pack.entry(reference, projection)
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
        self.pack
            .section_page(reference, section, offset, limit, projection)
    }
}

/// One enabled pack view restricted to authored translations in a target language.
#[derive(Clone)]
pub struct BilingualView {
    view_id: String,
    pack: Arc<DictionaryPack>,
    target_language: String,
}

impl BilingualView {
    /// Creates a named bilingual view with an exact target-language code filter.
    ///
    /// # Errors
    ///
    /// Returns an error when either identifier is empty.
    pub fn new(
        view_id: impl Into<String>,
        pack: Arc<DictionaryPack>,
        target_language: impl Into<String>,
    ) -> Result<Self, SnapshotError> {
        let view_id = view_id.into();
        let target_language = target_language.into();
        validate_view_id(&view_id)?;
        if target_language.is_empty() {
            return Err(SnapshotError::InvalidView(
                "target language must not be empty".to_owned(),
            ));
        }
        Ok(Self {
            view_id,
            pack,
            target_language,
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
    pub fn target_language(&self) -> &str {
        &self.target_language
    }

    /// Performs an exact lookup and retains only target-language translations.
    ///
    /// # Errors
    ///
    /// Returns pack limit, storage, or record-authentication errors.
    pub fn lookup(
        &self,
        query: &str,
        options: LookupOptions,
    ) -> Result<BilingualLookupOutcome, PackError> {
        let outcome = self
            .pack
            .lookup_for_language(query, options, Some(&self.target_language))?;
        if outcome.is_empty() {
            return Ok(BilingualLookupOutcome::NoEntry);
        }
        let has_translation = outcome.matches.iter().any(|matched| {
            matched.entry.translations.total > 0 || matched.entry.sense_translation_total > 0
        });
        if has_translation {
            Ok(BilingualLookupOutcome::Translations(outcome))
        } else {
            Ok(BilingualLookupOutcome::NoTranslation(outcome))
        }
    }

    /// Reads one entry with translations filtered to the target language.
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
            .entry_for_language(reference, projection, Some(&self.target_language))
    }

    /// Reads one section page with translations filtered to the target language.
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
        self.pack.section_page_for_language(
            reference,
            section,
            offset,
            limit,
            projection,
            Some(&self.target_language),
        )
    }
}

/// A bilingual lookup distinguishes absent entries from absent target translations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BilingualLookupOutcome {
    NoEntry,
    NoTranslation(LookupOutcome),
    Translations(LookupOutcome),
}

/// One ordered enabled dictionary view.
#[derive(Clone)]
pub enum DictionaryView {
    Monolingual(MonolingualView),
    Bilingual(BilingualView),
}

impl DictionaryView {
    #[must_use]
    pub fn view_id(&self) -> &str {
        match self {
            Self::Monolingual(view) => view.view_id(),
            Self::Bilingual(view) => view.view_id(),
        }
    }

    #[must_use]
    pub fn pack(&self) -> &Arc<DictionaryPack> {
        match self {
            Self::Monolingual(view) => view.pack(),
            Self::Bilingual(view) => view.pack(),
        }
    }

    /// The target language of a bilingual view.
    #[must_use]
    pub fn target_language(&self) -> Option<&str> {
        match self {
            Self::Monolingual(_) => None,
            Self::Bilingual(view) => Some(view.target_language()),
        }
    }

    /// Performs an exact lookup in this view.
    #[must_use]
    pub fn lookup(&self, query: &str, options: LookupOptions) -> ViewOutcome {
        match self {
            Self::Monolingual(view) => match view.lookup(query, options) {
                Ok(outcome) if outcome.is_empty() => ViewOutcome::NoEntry,
                Ok(outcome) => ViewOutcome::Completed(outcome),
                Err(error) => ViewOutcome::Failed(error),
            },
            Self::Bilingual(view) => match view.lookup(query, options) {
                Ok(BilingualLookupOutcome::NoEntry) => ViewOutcome::NoEntry,
                Ok(BilingualLookupOutcome::NoTranslation(outcome)) => {
                    ViewOutcome::NoTranslation(outcome)
                }
                Ok(BilingualLookupOutcome::Translations(outcome)) => {
                    ViewOutcome::Completed(outcome)
                }
                Err(error) => ViewOutcome::Failed(error),
            },
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
        match self {
            Self::Monolingual(view) => view.entry(reference, projection),
            Self::Bilingual(view) => view.entry(reference, projection),
        }
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
        match self {
            Self::Monolingual(view) => {
                view.section_page(reference, section, offset, limit, projection)
            }
            Self::Bilingual(view) => {
                view.section_page(reference, section, offset, limit, projection)
            }
        }
    }

    fn identity(&self) -> SnapshotViewIdentity<'_> {
        match self {
            Self::Monolingual(view) => SnapshotViewIdentity {
                view_id: view.view_id(),
                pack_id: view.pack.pack_id(),
                pack_revision: view.pack.pack_revision(),
                view_type: ViewType::Monolingual,
                target_language: None,
            },
            Self::Bilingual(view) => SnapshotViewIdentity {
                view_id: view.view_id(),
                pack_id: view.pack.pack_id(),
                pack_revision: view.pack.pack_revision(),
                view_type: ViewType::Bilingual,
                target_language: Some(view.target_language()),
            },
        }
    }
}

/// The outcome of one view in a snapshot lookup.
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

/// An immutable, ordered leaseable dictionary configuration.
pub struct DictionarySnapshot {
    revision: SnapshotRevision,
    views: Vec<DictionaryView>,
}

impl DictionarySnapshot {
    /// Creates a deterministic snapshot and rejects duplicate view identifiers.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate or invalid view identifiers.
    pub fn new(views: Vec<DictionaryView>) -> Result<Self, SnapshotError> {
        let mut identifiers = BTreeSet::new();
        for view in &views {
            validate_view_id(view.view_id())?;
            if !identifiers.insert(view.view_id()) {
                return Err(SnapshotError::DuplicateView(view.view_id().to_owned()));
            }
        }
        let identities = views
            .iter()
            .map(DictionaryView::identity)
            .collect::<Vec<_>>();
        let revision = SnapshotRevision::derive(&identities);
        Ok(Self { revision, views })
    }

    #[must_use]
    pub const fn revision(&self) -> &SnapshotRevision {
        &self.revision
    }

    #[must_use]
    pub fn views(&self) -> &[DictionaryView] {
        &self.views
    }

    #[must_use]
    pub fn view(&self, view_id: &str) -> Option<&DictionaryView> {
        self.views.iter().find(|view| view.view_id() == view_id)
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

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<DictionaryPack>();
    assert_send_sync::<DictionarySnapshot>();
    assert_send_sync::<DictionaryService>();
};

/// Atomic owner of the current immutable dictionary snapshot.
pub struct DictionaryService {
    current: ArcSwap<DictionarySnapshot>,
}

impl DictionaryService {
    #[must_use]
    pub fn new(initial: Arc<DictionarySnapshot>) -> Self {
        Self {
            current: ArcSwap::from(initial),
        }
    }

    /// Leases the current snapshot; replacement cannot affect an existing lease.
    #[must_use]
    pub fn lease(&self) -> Arc<DictionarySnapshot> {
        self.current.load_full()
    }

    /// Atomically replaces the current snapshot and returns the previous snapshot.
    pub fn replace(&self, replacement: Arc<DictionarySnapshot>) -> Arc<DictionarySnapshot> {
        self.current.swap(replacement)
    }
}

/// Snapshot construction failures.
#[derive(Debug, Error, Eq, PartialEq)]
#[non_exhaustive]
pub enum SnapshotError {
    #[error("invalid dictionary view: {0}")]
    InvalidView(String),
    #[error("duplicate dictionary view identifier: {0}")]
    DuplicateView(String),
}

fn validate_view_id(view_id: &str) -> Result<(), SnapshotError> {
    if view_id.is_empty() {
        Err(SnapshotError::InvalidView(
            "view identifier must not be empty".to_owned(),
        ))
    } else {
        Ok(())
    }
}
