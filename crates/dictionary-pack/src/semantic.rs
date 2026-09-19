use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{EntryId, PackError, PackId, PackRevision};

/// The first non-empty exact-match stage used for an entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
#[repr(u8)]
pub enum MatchClass {
    AuthoredHeadword = 1,
    NfcHeadword = 2,
    FoldedHeadword = 3,
    AuthoredForm = 4,
    NfcForm = 5,
    FoldedForm = 6,
}

/// Stable identity and source order of a returned entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryReference {
    pub pack_id: PackId,
    pub pack_revision: PackRevision,
    pub entry_id: EntryId,
    pub selected_ordinal: u64,
}

/// A window of an authored list. Items start at `offset` within `total` authored or
/// target-filtered items.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounded<T> {
    pub offset: usize,
    pub total: usize,
    pub items: Vec<T>,
}

impl<T> Bounded<T> {
    const fn empty() -> Self {
        Self {
            offset: 0,
            total: 0,
            items: Vec::new(),
        }
    }

    /// Authored items exist outside this window.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        self.offset > 0 || self.offset + self.items.len() < self.total
    }
}

/// Item counts and text bounds for one projection.
///
/// A zero count reports a list's total without projecting its items.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionOptions {
    pub text_bytes: usize,
    /// Items in string lists inside one value, such as tags or glosses.
    pub strings: usize,
    pub etymology: usize,
    pub pronunciations: usize,
    pub hyphenations: usize,
    pub forms: usize,
    pub senses: usize,
    pub examples_per_sense: usize,
    /// Translations at entry level and within each sense.
    pub translations: usize,
    /// Items in each relation group at entry level and within each sense.
    pub relations: usize,
    pub descendants: usize,
    pub notes: usize,
    pub categories: usize,
}

impl ProjectionOptions {
    /// Bounded core facts for lookup results.
    #[must_use]
    pub const fn summary() -> Self {
        Self {
            text_bytes: 4096,
            strings: 32,
            etymology: 0,
            pronunciations: 8,
            hyphenations: 0,
            forms: 0,
            senses: 16,
            examples_per_sense: 0,
            translations: 16,
            relations: 0,
            descendants: 0,
            notes: 0,
            categories: 0,
        }
    }

    /// Every known section within generous bounds.
    #[must_use]
    pub const fn detail() -> Self {
        Self {
            text_bytes: 16 * 1024,
            strings: 256,
            etymology: 16,
            pronunciations: 64,
            hyphenations: 16,
            forms: 256,
            senses: 256,
            examples_per_sense: 16,
            translations: 256,
            relations: 256,
            descendants: 256,
            notes: 64,
            categories: 256,
        }
    }

    /// No bounds; used by exhaustive qualification.
    #[must_use]
    pub const fn exhaustive() -> Self {
        Self {
            text_bytes: usize::MAX,
            strings: usize::MAX,
            etymology: usize::MAX,
            pronunciations: usize::MAX,
            hyphenations: usize::MAX,
            forms: usize::MAX,
            senses: usize::MAX,
            examples_per_sense: usize::MAX,
            translations: usize::MAX,
            relations: usize::MAX,
            descendants: usize::MAX,
            notes: usize::MAX,
            categories: usize::MAX,
        }
    }
}

impl Default for ProjectionOptions {
    fn default() -> Self {
        Self::summary()
    }
}

/// An authored ruby annotation over base text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ruby {
    pub base: String,
    pub text: String,
}

/// An authored emphasis range in Unicode scalar values, end exclusive. Ranges refer
/// to the complete authored text and may exceed a bounded projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmphasisRange {
    pub start: u64,
    pub end: u64,
}

/// An authored reference to a lemma or alternative entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LemmaReference {
    pub word: String,
    pub extra: Option<String>,
    pub romanization: Option<String>,
    pub tags: Vec<String>,
    pub truncated: bool,
}

/// Authored pronunciation, context, and audio-reference facts from one sound object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pronunciation {
    pub ipa: Option<String>,
    pub enpr: Option<String>,
    pub zh_pronunciation: Option<String>,
    pub hangeul: Option<String>,
    pub homophone: Option<String>,
    pub romanization: Option<String>,
    pub form: Option<String>,
    pub rhymes: Option<String>,
    pub text: Option<String>,
    pub note: Option<String>,
    pub other: Option<String>,
    pub sense: Option<String>,
    pub alternative: Option<String>,
    pub not_same_pronunciation: Option<bool>,
    pub audio: Option<AudioReference>,
    pub audio_ipa: Option<String>,
    pub wav_url: Option<String>,
    pub ogg_url: Option<String>,
    pub oga_url: Option<String>,
    pub mp3_url: Option<String>,
    pub opus_url: Option<String>,
    pub flac_url: Option<String>,
    pub homophones: Vec<String>,
    pub hyphenation: Vec<String>,
    pub tags: Vec<String>,
    pub raw_tags: Vec<String>,
    pub topics: Vec<String>,
    /// At least one string or string-array value was bounded.
    pub truncated: bool,
}

/// A pronunciation recording identified by its normalized Wikimedia Commons file name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioReference {
    /// The authored `audio` value.
    pub authored: String,
    /// The Commons file name with underscores as spaces and an uppercase first
    /// character, or `None` when the authored value is not a plausible file name.
    pub file_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hyphenation {
    pub parts: Vec<String>,
    pub sense: Option<String>,
    pub tags: Vec<String>,
    pub raw_tags: Vec<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Form {
    pub form: String,
    pub romanization: Option<String>,
    pub ipa: Vec<String>,
    pub hiragana: Option<String>,
    pub article: Option<String>,
    pub literal_meaning: Option<String>,
    pub source: Option<String>,
    pub sense: Option<String>,
    pub sense_index: Option<String>,
    pub pronouns: Vec<String>,
    pub ruby: Vec<Ruby>,
    pub tags: Vec<String>,
    pub raw_tags: Vec<String>,
    pub topics: Vec<String>,
    pub truncated: bool,
}

/// An authored translation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Translation {
    /// May be absent when the authored translation is represented by a note.
    pub word: Option<String>,
    pub language_name: Option<String>,
    pub language_code: Option<String>,
    pub alt: Option<String>,
    /// The authored `translation`, or deprecated `english`, clarification.
    pub translation: Option<String>,
    pub note: Option<String>,
    pub sense: Option<String>,
    pub sense_index: Option<String>,
    pub taxonomic: Option<String>,
    pub romanization: Option<String>,
    pub traditional_writing: Option<String>,
    pub other: Option<String>,
    pub source: Option<String>,
    pub uncertain: Option<bool>,
    pub ruby: Vec<Ruby>,
    pub tags: Vec<String>,
    pub raw_tags: Vec<String>,
    pub topics: Vec<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Example {
    pub text: Option<String>,
    pub text_emphasis: Vec<EmphasisRange>,
    /// The authored `translation`, or deprecated `english`.
    pub translation: Option<String>,
    pub translation_emphasis: Vec<EmphasisRange>,
    pub romanization: Option<String>,
    pub romanization_emphasis: Vec<EmphasisRange>,
    pub literal_meaning: Option<String>,
    pub reference: Option<String>,
    pub note: Option<String>,
    pub example_type: Option<String>,
    pub ruby: Vec<Ruby>,
    pub tags: Vec<String>,
    pub raw_tags: Vec<String>,
    pub truncated: bool,
}

/// The authored relation list a [`Relation`] belongs to.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelationKind {
    Synonyms,
    Antonyms,
    Hypernyms,
    Hyponyms,
    Holonyms,
    Meronyms,
    Troponyms,
    Paronyms,
    CoordinateTerms,
    Instances,
    Derived,
    Related,
    Compounds,
    Abbreviations,
    Contractions,
    Proverbs,
    Idioms,
    Expressions,
    Collocations,
    Phrases,
    Cognates,
    Morphologies,
    Anagrams,
    Various,
}

impl RelationKind {
    pub const ALL: [Self; 24] = [
        Self::Synonyms,
        Self::Antonyms,
        Self::Hypernyms,
        Self::Hyponyms,
        Self::Holonyms,
        Self::Meronyms,
        Self::Troponyms,
        Self::Paronyms,
        Self::CoordinateTerms,
        Self::Instances,
        Self::Derived,
        Self::Related,
        Self::Compounds,
        Self::Abbreviations,
        Self::Contractions,
        Self::Proverbs,
        Self::Idioms,
        Self::Expressions,
        Self::Collocations,
        Self::Phrases,
        Self::Cognates,
        Self::Morphologies,
        Self::Anagrams,
        Self::Various,
    ];

    /// The Wiktextract field holding this relation list.
    #[must_use]
    pub const fn source_field(self) -> &'static str {
        match self {
            Self::Synonyms => "synonyms",
            Self::Antonyms => "antonyms",
            Self::Hypernyms => "hypernyms",
            Self::Hyponyms => "hyponyms",
            Self::Holonyms => "holonyms",
            Self::Meronyms => "meronyms",
            Self::Troponyms => "troponyms",
            Self::Paronyms => "paronyms",
            Self::CoordinateTerms => "coordinate_terms",
            Self::Instances => "instances",
            Self::Derived => "derived",
            Self::Related => "related",
            Self::Compounds => "compounds",
            Self::Abbreviations => "abbreviations",
            Self::Contractions => "contraction",
            Self::Proverbs => "proverbs",
            Self::Idioms => "idioms",
            Self::Expressions => "expressions",
            Self::Collocations => "collocations",
            Self::Phrases => "phrases",
            Self::Cognates => "cognates",
            Self::Morphologies => "morphologies",
            Self::Anagrams => "anagrams",
            Self::Various => "various",
        }
    }
}

/// One authored related term.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Relation {
    pub word: Option<String>,
    pub alt: Option<String>,
    pub translation: Option<String>,
    pub extra: Option<String>,
    pub qualifier: Option<String>,
    pub romanization: Option<String>,
    pub sense: Option<String>,
    pub sense_index: Option<String>,
    pub note: Option<String>,
    pub alternative_spelling: Option<String>,
    pub literal_meaning: Option<String>,
    pub taxonomic: Option<String>,
    pub source: Option<String>,
    pub language_name: Option<String>,
    pub language_code: Option<String>,
    pub ruby: Vec<Ruby>,
    pub tags: Vec<String>,
    pub raw_tags: Vec<String>,
    pub topics: Vec<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationGroup {
    pub kind: RelationKind,
    pub relations: Bounded<Relation>,
}

/// One node of an authored descendant tree in depth-first order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Descendant {
    /// Zero for top-level descendants.
    pub depth: u32,
    pub word: Option<String>,
    pub language_name: Option<String>,
    pub language_code: Option<String>,
    pub romanization: Option<String>,
    pub sense: Option<String>,
    pub sense_index: Option<String>,
    pub ruby: Vec<Ruby>,
    pub tags: Vec<String>,
    pub raw_tags: Vec<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Classifier {
    pub classifier: String,
    pub tags: Vec<String>,
    pub raw_tags: Vec<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sense {
    pub glosses: Vec<String>,
    pub raw_glosses: Vec<String>,
    pub qualifier: Option<String>,
    pub taxonomic: Option<String>,
    pub sense_index: Option<String>,
    pub notes: Vec<String>,
    pub tags: Vec<String>,
    pub raw_tags: Vec<String>,
    pub topics: Vec<String>,
    pub categories: Vec<String>,
    pub sense_ids: Vec<String>,
    pub wikidata: Vec<String>,
    pub wikipedia: Vec<String>,
    pub form_of: Vec<LemmaReference>,
    pub alt_of: Vec<LemmaReference>,
    pub compound_of: Vec<LemmaReference>,
    pub classifiers: Vec<Classifier>,
    pub examples: Bounded<Example>,
    pub translations: Bounded<Translation>,
    /// Only authored non-empty relation lists.
    pub relations: Vec<RelationGroup>,
    pub truncated: bool,
}

/// A bounded semantic projection of one retained source record. It contains no HTML
/// or generic JSON.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub reference: EntryReference,
    pub headword: String,
    pub language_name: Option<String>,
    pub language_code: String,
    pub part_of_speech: Option<String>,
    pub part_of_speech_title: Option<String>,
    pub literal_meaning: Option<String>,
    pub etymology_number: Option<String>,
    pub tags: Vec<String>,
    pub raw_tags: Vec<String>,
    pub wikidata: Vec<String>,
    pub wikipedia: Vec<String>,
    pub form_of: Vec<LemmaReference>,
    pub alt_of: Vec<LemmaReference>,
    pub classifiers: Vec<Classifier>,
    /// `etymology_text` or `etymology_texts`.
    pub etymology: Bounded<String>,
    pub pronunciations: Bounded<Pronunciation>,
    pub hyphenations: Bounded<Hyphenation>,
    pub forms: Bounded<Form>,
    pub senses: Bounded<Sense>,
    /// Entry-level translations, filtered to the target language for bilingual views.
    pub translations: Bounded<Translation>,
    /// Target-filtered translations in every sense, including senses outside the
    /// projected window.
    pub sense_translation_total: usize,
    /// Only authored non-empty relation lists.
    pub relations: Vec<RelationGroup>,
    pub descendants: Bounded<Descendant>,
    pub notes: Bounded<String>,
    pub categories: Bounded<String>,
    /// At least one entry-level scalar or string list was bounded.
    pub truncated: bool,
}

/// One pageable list of an entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
    Etymology,
    Pronunciations,
    Hyphenations,
    Forms,
    Senses,
    Translations,
    Relations(RelationKind),
    Descendants,
    Notes,
    Categories,
    SenseExamples { sense: usize },
    SenseTranslations { sense: usize },
    SenseRelations { sense: usize, kind: RelationKind },
}

/// One page of a section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SectionPage {
    Etymology(Bounded<String>),
    Pronunciations(Bounded<Pronunciation>),
    Hyphenations(Bounded<Hyphenation>),
    Forms(Bounded<Form>),
    Senses(Bounded<Sense>),
    Translations(Bounded<Translation>),
    Relations(Bounded<Relation>),
    Descendants(Bounded<Descendant>),
    Notes(Bounded<String>),
    Categories(Bounded<String>),
    Examples(Bounded<Example>),
    /// The requested sense does not exist.
    NoSuchSense,
}

pub(crate) struct RoutingRecord {
    pub word: String,
    pub lang_code: String,
    pub forms: Vec<String>,
    object: Map<String, Value>,
}

impl RoutingRecord {
    pub(crate) fn parse(bytes: &[u8], max_forms: usize) -> Result<Self, PackError> {
        let Value::Object(object) = serde_json::from_slice(bytes).map_err(|error| {
            PackError::Corrupt(format!("stored record is invalid JSON: {error}"))
        })?
        else {
            return Err(PackError::Corrupt(
                "stored record is not a JSON object".to_owned(),
            ));
        };
        let word = required_string(&object, "word", "record")?.to_owned();
        let lang_code = required_string(&object, "lang_code", "record")?.to_owned();
        let mut forms = Vec::new();
        if let Some(value) = object.get("forms") {
            let values = array(value, "record `forms`")?;
            for (index, value) in values.iter().enumerate() {
                let form = value_object(value, &format!("record `forms[{index}]`"))?;
                let form = required_string(form, "form", &format!("record `forms[{index}]`"))?;
                if !form.is_empty() && form != "-" {
                    if forms.len() == max_forms {
                        return Err(PackError::Limit(
                            "record routing forms exceed limit".to_owned(),
                        ));
                    }
                    forms.push(form.to_owned());
                }
            }
        }
        Ok(Self {
            word,
            lang_code,
            forms,
            object,
        })
    }

    pub(crate) fn project(
        &self,
        reference: EntryReference,
        options: ProjectionOptions,
        target_language: Option<&str>,
    ) -> Result<Entry, PackError> {
        let projector = Projector {
            options,
            target_language,
        };
        let object = &self.object;
        let mut fields = Fields::new(object, "record", options);
        let (headword, headword_truncated) = bounded(&self.word, options.text_bytes);
        let (language_code, language_code_truncated) = bounded(&self.lang_code, options.text_bytes);
        let language_name = fields.text("lang")?;
        let part_of_speech = fields.text("pos")?;
        let part_of_speech_title = fields.text("pos_title")?;
        let literal_meaning = fields.text("literal_meaning")?;
        let etymology_number = fields.index("etymology_number")?;
        let tags = fields.texts("tags")?;
        let raw_tags = fields.texts("raw_tags")?;
        let wikidata = fields.texts("wikidata")?;
        let wikipedia = fields.texts("wikipedia")?;
        let entry_truncated = fields.truncated || headword_truncated || language_code_truncated;

        let senses_value = object.get("senses");
        let sense_translation_total = match senses_value {
            Some(value) => {
                let senses = array(value, "record `senses`")?;
                let mut total = 0;
                for (index, sense) in senses.iter().enumerate() {
                    let context = format!("record `senses[{index}]`");
                    let sense = value_object(sense, &context)?;
                    total += projector.translation_count(
                        sense.get("translations"),
                        &format!("{context} `translations`"),
                    )?;
                }
                total
            }
            None => 0,
        };

        Ok(Entry {
            reference,
            headword,
            language_name,
            language_code,
            part_of_speech,
            part_of_speech_title,
            literal_meaning,
            etymology_number,
            tags,
            raw_tags,
            wikidata,
            wikipedia,
            form_of: projector.lemmas(object.get("form_of"), "record `form_of`")?,
            alt_of: projector.lemmas(object.get("alt_of"), "record `alt_of`")?,
            classifiers: projector
                .classifiers(object.get("classifiers"), "record `classifiers`")?,
            etymology: projector.etymology(object, 0, options.etymology)?,
            pronunciations: projector.pronunciations(object, 0, options.pronunciations)?,
            hyphenations: projector.hyphenations(object, 0, options.hyphenations)?,
            forms: projector.forms(object, 0, options.forms)?,
            senses: projector.senses(senses_value, 0, options.senses)?,
            translations: projector.translations(
                object.get("translations"),
                "record `translations`",
                0,
                options.translations,
            )?,
            sense_translation_total,
            relations: projector.relation_groups(object, "record", options.relations)?,
            descendants: projector.descendants(
                object.get("descendants"),
                "record `descendants`",
                0,
                options.descendants,
            )?,
            notes: projector.strings(object.get("notes"), "record `notes`", 0, options.notes)?,
            categories: projector.strings(
                object.get("categories"),
                "record `categories`",
                0,
                options.categories,
            )?,
            truncated: entry_truncated,
        })
    }

    /// Authored audio values of every sound, in source order.
    pub(crate) fn audio_values(&self) -> Result<Vec<&str>, PackError> {
        let Some(value) = self.object.get("sounds") else {
            return Ok(Vec::new());
        };
        let mut values = Vec::new();
        for (index, sound) in array(value, "record `sounds`")?.iter().enumerate() {
            let context = item_context("record `sounds`", index);
            match value_object(sound, &context)?.get("audio") {
                Some(Value::String(audio)) => values.push(audio.as_str()),
                Some(_) => {
                    return Err(PackError::Corrupt(format!(
                        "{context} `audio` is not a string"
                    )));
                }
                None => {}
            }
        }
        Ok(values)
    }

    pub(crate) fn section(
        &self,
        section: Section,
        offset: usize,
        limit: usize,
        options: ProjectionOptions,
        target_language: Option<&str>,
    ) -> Result<SectionPage, PackError> {
        let projector = Projector {
            options,
            target_language,
        };
        let object = &self.object;
        Ok(match section {
            Section::Etymology => {
                SectionPage::Etymology(projector.etymology(object, offset, limit)?)
            }
            Section::Pronunciations => {
                SectionPage::Pronunciations(projector.pronunciations(object, offset, limit)?)
            }
            Section::Hyphenations => {
                SectionPage::Hyphenations(projector.hyphenations(object, offset, limit)?)
            }
            Section::Forms => SectionPage::Forms(projector.forms(object, offset, limit)?),
            Section::Senses => {
                SectionPage::Senses(projector.senses(object.get("senses"), offset, limit)?)
            }
            Section::Translations => SectionPage::Translations(projector.translations(
                object.get("translations"),
                "record `translations`",
                offset,
                limit,
            )?),
            Section::Relations(kind) => SectionPage::Relations(projector.relations(
                object.get(kind.source_field()),
                &format!("record `{}`", kind.source_field()),
                offset,
                limit,
            )?),
            Section::Descendants => SectionPage::Descendants(projector.descendants(
                object.get("descendants"),
                "record `descendants`",
                offset,
                limit,
            )?),
            Section::Notes => SectionPage::Notes(projector.strings(
                object.get("notes"),
                "record `notes`",
                offset,
                limit,
            )?),
            Section::Categories => SectionPage::Categories(projector.strings(
                object.get("categories"),
                "record `categories`",
                offset,
                limit,
            )?),
            Section::SenseExamples { sense } => match sense_object(object, sense)? {
                Some((sense, context)) => SectionPage::Examples(projector.examples(
                    sense.get("examples"),
                    &format!("{context} `examples`"),
                    offset,
                    limit,
                )?),
                None => SectionPage::NoSuchSense,
            },
            Section::SenseTranslations { sense } => match sense_object(object, sense)? {
                Some((sense, context)) => SectionPage::Translations(projector.translations(
                    sense.get("translations"),
                    &format!("{context} `translations`"),
                    offset,
                    limit,
                )?),
                None => SectionPage::NoSuchSense,
            },
            Section::SenseRelations { sense, kind } => match sense_object(object, sense)? {
                Some((sense, context)) => SectionPage::Relations(projector.relations(
                    sense.get(kind.source_field()),
                    &format!("{context} `{}`", kind.source_field()),
                    offset,
                    limit,
                )?),
                None => SectionPage::NoSuchSense,
            },
        })
    }
}

type SenseObject<'a> = (&'a Map<String, Value>, String);

fn sense_object(
    object: &Map<String, Value>,
    index: usize,
) -> Result<Option<SenseObject<'_>>, PackError> {
    let Some(value) = object.get("senses") else {
        return Ok(None);
    };
    let senses = array(value, "record `senses`")?;
    let Some(sense) = senses.get(index) else {
        return Ok(None);
    };
    let context = format!("record `senses[{index}]`");
    Ok(Some((value_object(sense, &context)?, context)))
}

#[derive(Clone, Copy)]
struct Projector<'a> {
    options: ProjectionOptions,
    target_language: Option<&'a str>,
}

impl Projector<'_> {
    fn window<T>(
        value: Option<&Value>,
        context: &str,
        offset: usize,
        limit: usize,
        mut item: impl FnMut(&Value, &str) -> Result<T, PackError>,
    ) -> Result<Bounded<T>, PackError> {
        let Some(value) = value else {
            return Ok(Bounded::empty());
        };
        let authored = array(value, context)?;
        let mut items = Vec::new();
        for (index, value) in authored.iter().enumerate().skip(offset).take(limit) {
            items.push(item(value, &item_context(context, index))?);
        }
        Ok(Bounded {
            offset,
            total: authored.len(),
            items,
        })
    }

    fn strings(
        &self,
        value: Option<&Value>,
        context: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Bounded<String>, PackError> {
        let text_bytes = self.options.text_bytes;
        Self::window(value, context, offset, limit, |value, context| {
            let Value::String(value) = value else {
                return Err(PackError::Corrupt(format!("{context} is not a string")));
            };
            Ok(bounded(value, text_bytes).0)
        })
    }

    fn etymology(
        &self,
        object: &Map<String, Value>,
        offset: usize,
        limit: usize,
    ) -> Result<Bounded<String>, PackError> {
        match object.get("etymology_text") {
            Some(Value::String(text)) => {
                let items = if offset == 0 && limit > 0 {
                    vec![bounded(text, self.options.text_bytes).0]
                } else {
                    Vec::new()
                };
                Ok(Bounded {
                    offset,
                    total: 1,
                    items,
                })
            }
            Some(_) => Err(PackError::Corrupt(
                "record `etymology_text` is not a string".to_owned(),
            )),
            None => self.strings(
                object.get("etymology_texts"),
                "record `etymology_texts`",
                offset,
                limit,
            ),
        }
    }

    fn pronunciations(
        &self,
        object: &Map<String, Value>,
        offset: usize,
        limit: usize,
    ) -> Result<Bounded<Pronunciation>, PackError> {
        Self::window(
            object.get("sounds"),
            "record `sounds`",
            offset,
            limit,
            |value, context| {
                let mut fields = Fields::new(value_object(value, context)?, context, self.options);
                let authored_audio = fields.text("audio")?;
                Ok(Pronunciation {
                    ipa: fields.text("ipa")?,
                    enpr: fields.text("enpr")?,
                    zh_pronunciation: match fields.text("zh-pron")? {
                        Some(current) => Some(current),
                        None => fields.text("zh_pron")?,
                    },
                    hangeul: fields.text("hangeul")?,
                    homophone: fields.text("homophone")?,
                    romanization: fields.text("roman")?,
                    form: fields.text("form")?,
                    rhymes: fields.text("rhymes")?,
                    text: fields.text("text")?,
                    note: fields.text("note")?,
                    other: fields.text("other")?,
                    sense: fields.text("sense")?,
                    alternative: fields.text("alternative")?,
                    not_same_pronunciation: fields.flag("not_same_pronunciation")?,
                    audio: authored_audio.map(|authored| AudioReference {
                        file_name: commons_file_name(&authored),
                        authored,
                    }),
                    audio_ipa: fields.text("audio-ipa")?,
                    wav_url: fields.text("wav_url")?,
                    ogg_url: fields.text("ogg_url")?,
                    oga_url: fields.text("oga_url")?,
                    mp3_url: fields.text("mp3_url")?,
                    opus_url: fields.text("opus_url")?,
                    flac_url: fields.text("flac_url")?,
                    homophones: fields.texts("homophones")?,
                    hyphenation: fields.texts("hyphenation")?,
                    tags: fields.texts("tags")?,
                    raw_tags: fields.texts("raw_tags")?,
                    topics: fields.texts("topics")?,
                    truncated: fields.truncated,
                })
            },
        )
    }

    fn hyphenations(
        &self,
        object: &Map<String, Value>,
        offset: usize,
        limit: usize,
    ) -> Result<Bounded<Hyphenation>, PackError> {
        Self::window(
            object.get("hyphenations"),
            "record `hyphenations`",
            offset,
            limit,
            |value, context| {
                let mut fields = Fields::new(value_object(value, context)?, context, self.options);
                Ok(Hyphenation {
                    parts: fields.texts("parts")?,
                    sense: fields.text("sense")?,
                    tags: fields.texts("tags")?,
                    raw_tags: fields.texts("raw_tags")?,
                    truncated: fields.truncated,
                })
            },
        )
    }

    fn forms(
        &self,
        object: &Map<String, Value>,
        offset: usize,
        limit: usize,
    ) -> Result<Bounded<Form>, PackError> {
        Self::window(
            object.get("forms"),
            "record `forms`",
            offset,
            limit,
            |value, context| {
                let mut fields = Fields::new(value_object(value, context)?, context, self.options);
                let form = fields.text("form")?.unwrap_or_default();
                let mut ipa = fields.text("ipa")?.into_iter().collect::<Vec<_>>();
                ipa.extend(fields.texts("ipas")?);
                Ok(Form {
                    form,
                    romanization: fields.text("roman")?,
                    ipa,
                    hiragana: fields.text("hiragana")?,
                    article: fields.text("article")?,
                    literal_meaning: fields.text("literal_meaning")?,
                    source: fields.text("source")?,
                    sense: fields.text("sense")?,
                    sense_index: fields.index("sense_index")?,
                    pronouns: fields.texts("pronouns")?,
                    ruby: fields.ruby("ruby")?,
                    tags: fields.texts("tags")?,
                    raw_tags: fields.texts("raw_tags")?,
                    topics: fields.texts("topics")?,
                    truncated: fields.truncated,
                })
            },
        )
    }

    fn senses(
        &self,
        value: Option<&Value>,
        offset: usize,
        limit: usize,
    ) -> Result<Bounded<Sense>, PackError> {
        Self::window(value, "record `senses`", offset, limit, |value, context| {
            let sense = value_object(value, context)?;
            let mut fields = Fields::new(sense, context, self.options);
            let mut notes = fields.text("note")?.into_iter().collect::<Vec<_>>();
            notes.extend(fields.texts("notes")?);
            Ok(Sense {
                glosses: fields.texts("glosses")?,
                raw_glosses: fields.texts("raw_glosses")?,
                qualifier: fields.text("qualifier")?,
                taxonomic: fields.text("taxonomic")?,
                sense_index: fields.index("sense_index")?,
                notes,
                tags: fields.texts("tags")?,
                raw_tags: fields.texts("raw_tags")?,
                topics: fields.texts("topics")?,
                categories: fields.texts("categories")?,
                sense_ids: fields.texts("senseid")?,
                wikidata: fields.texts("wikidata")?,
                wikipedia: fields.texts("wikipedia")?,
                truncated: fields.truncated,
                form_of: self.lemmas(sense.get("form_of"), &format!("{context} `form_of`"))?,
                alt_of: self.lemmas(sense.get("alt_of"), &format!("{context} `alt_of`"))?,
                compound_of: self.lemmas(
                    sense.get("compound_of"),
                    &format!("{context} `compound_of`"),
                )?,
                classifiers: self.classifiers(
                    sense.get("classifiers"),
                    &format!("{context} `classifiers`"),
                )?,
                examples: self.examples(
                    sense.get("examples"),
                    &format!("{context} `examples`"),
                    0,
                    self.options.examples_per_sense,
                )?,
                translations: self.translations(
                    sense.get("translations"),
                    &format!("{context} `translations`"),
                    0,
                    self.options.translations,
                )?,
                relations: self.relation_groups(sense, context, self.options.relations)?,
            })
        })
    }

    fn examples(
        &self,
        value: Option<&Value>,
        context: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Bounded<Example>, PackError> {
        Self::window(value, context, offset, limit, |value, context| {
            let mut fields = Fields::new(value_object(value, context)?, context, self.options);
            let translation = match fields.text("translation")? {
                Some(translation) => Some(translation),
                None => fields.text("english")?,
            };
            Ok(Example {
                text: fields.text("text")?,
                text_emphasis: fields.emphasis("bold_text_offsets")?,
                translation,
                translation_emphasis: fields.emphasis("bold_translation_offsets")?,
                romanization: fields.text("roman")?,
                romanization_emphasis: fields.emphasis("bold_roman_offsets")?,
                literal_meaning: fields.text("literal_meaning")?,
                reference: fields.text("ref")?,
                note: fields.text("note")?,
                example_type: fields.text("type")?,
                ruby: fields.ruby("ruby")?,
                tags: fields.texts("tags")?,
                raw_tags: fields.texts("raw_tags")?,
                truncated: fields.truncated,
            })
        })
    }

    fn translation_count(&self, value: Option<&Value>, context: &str) -> Result<usize, PackError> {
        let Some(value) = value else {
            return Ok(0);
        };
        let authored = array(value, context)?;
        let mut total = 0;
        for (index, value) in authored.iter().enumerate() {
            let object = value_object(value, &item_context(context, index))?;
            if self.translation_matches(object, &item_context(context, index))? {
                total += 1;
            }
        }
        Ok(total)
    }

    fn translation_matches(
        &self,
        object: &Map<String, Value>,
        context: &str,
    ) -> Result<bool, PackError> {
        let Some(target) = self.target_language else {
            return Ok(true);
        };
        let code = match object.get("lang_code") {
            Some(Value::String(code)) => Some(code.as_str()),
            Some(_) => {
                return Err(PackError::Corrupt(format!(
                    "{context} `lang_code` is not a string"
                )));
            }
            None => match object.get("code") {
                Some(Value::String(code)) => Some(code.as_str()),
                Some(_) => {
                    return Err(PackError::Corrupt(format!(
                        "{context} `code` is not a string"
                    )));
                }
                None => None,
            },
        };
        Ok(code == Some(target))
    }

    fn translations(
        &self,
        value: Option<&Value>,
        context: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Bounded<Translation>, PackError> {
        let Some(value) = value else {
            return Ok(Bounded::empty());
        };
        let authored = array(value, context)?;
        let mut total = 0;
        let mut items = Vec::new();
        for (index, value) in authored.iter().enumerate() {
            let item_context = item_context(context, index);
            let object = value_object(value, &item_context)?;
            if !self.translation_matches(object, &item_context)? {
                continue;
            }
            if total >= offset && items.len() < limit {
                items.push(self.translation(object, &item_context)?);
            }
            total += 1;
        }
        Ok(Bounded {
            offset,
            total,
            items,
        })
    }

    fn translation(
        &self,
        object: &Map<String, Value>,
        context: &str,
    ) -> Result<Translation, PackError> {
        let mut fields = Fields::new(object, context, self.options);
        let word = fields.text("word")?;
        let note = fields.text("note")?;
        if word.is_none() && note.is_none() {
            return Err(PackError::Corrupt(format!(
                "{context} has neither a `word` nor a `note` field"
            )));
        }
        let language_code = match fields.text("lang_code")? {
            Some(code) => {
                fields.text("code")?;
                Some(code)
            }
            None => fields.text("code")?,
        };
        let translation = match fields.text("translation")? {
            Some(translation) => Some(translation),
            None => fields.text("english")?,
        };
        Ok(Translation {
            word,
            language_name: fields.text("lang")?,
            language_code,
            alt: fields.text("alt")?,
            translation,
            note,
            sense: fields.text("sense")?,
            sense_index: fields.index("sense_index")?,
            taxonomic: fields.text("taxonomic")?,
            romanization: fields.text("roman")?,
            traditional_writing: fields.text("traditional_writing")?,
            other: fields.text("other")?,
            source: fields.text("source")?,
            uncertain: fields.flag("uncertain")?,
            ruby: fields.ruby("ruby")?,
            tags: fields.texts("tags")?,
            raw_tags: fields.texts("raw_tags")?,
            topics: fields.texts("topics")?,
            truncated: fields.truncated,
        })
    }

    fn relation_groups(
        &self,
        object: &Map<String, Value>,
        context: &str,
        limit: usize,
    ) -> Result<Vec<RelationGroup>, PackError> {
        let mut groups = Vec::new();
        for kind in RelationKind::ALL {
            let relations = self.relations(
                object.get(kind.source_field()),
                &format!("{context} `{}`", kind.source_field()),
                0,
                limit,
            )?;
            if relations.total > 0 {
                groups.push(RelationGroup { kind, relations });
            }
        }
        Ok(groups)
    }

    fn relations(
        &self,
        value: Option<&Value>,
        context: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Bounded<Relation>, PackError> {
        Self::window(value, context, offset, limit, |value, context| {
            let mut fields = Fields::new(value_object(value, context)?, context, self.options);
            let translation = match fields.text("translation")? {
                Some(translation) => Some(translation),
                None => fields.text("english")?,
            };
            Ok(Relation {
                word: fields.text("word")?,
                alt: fields.text("alt")?,
                translation,
                extra: fields.text("extra")?,
                qualifier: fields.text("qualifier")?,
                romanization: fields.text("roman")?,
                sense: fields.text("sense")?,
                sense_index: fields.index("sense_index")?,
                note: fields.text("note")?,
                alternative_spelling: fields.text("alternative_spelling")?,
                literal_meaning: fields.text("literal_meaning")?,
                taxonomic: fields.text("taxonomic")?,
                source: fields.text("source")?,
                language_name: fields.text("lang")?,
                language_code: fields.text("lang_code")?,
                ruby: fields.ruby("ruby")?,
                tags: fields.texts("tags")?,
                raw_tags: fields.texts("raw_tags")?,
                topics: fields.texts("topics")?,
                truncated: fields.truncated,
            })
        })
    }

    fn descendants(
        &self,
        value: Option<&Value>,
        context: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Bounded<Descendant>, PackError> {
        let mut page = Bounded {
            offset,
            total: 0,
            items: Vec::new(),
        };
        if let Some(value) = value {
            self.descendant_nodes(value, context, 0, limit, &mut page)?;
        }
        Ok(page)
    }

    fn descendant_nodes(
        &self,
        value: &Value,
        context: &str,
        depth: u32,
        limit: usize,
        page: &mut Bounded<Descendant>,
    ) -> Result<(), PackError> {
        for (index, value) in array(value, context)?.iter().enumerate() {
            let context = item_context(context, index);
            let object = value_object(value, &context)?;
            if page.total >= page.offset && page.items.len() < limit {
                let mut fields = Fields::new(object, &context, self.options);
                page.items.push(Descendant {
                    depth,
                    word: fields.text("word")?,
                    language_name: fields.text("lang")?,
                    language_code: fields.text("lang_code")?,
                    romanization: fields.text("roman")?,
                    sense: fields.text("sense")?,
                    sense_index: fields.index("sense_index")?,
                    ruby: fields.ruby("ruby")?,
                    tags: fields.texts("tags")?,
                    raw_tags: fields.texts("raw_tags")?,
                    truncated: fields.truncated,
                });
            }
            page.total += 1;
            if let Some(children) = object.get("descendants") {
                self.descendant_nodes(
                    children,
                    &format!("{context} `descendants`"),
                    depth + 1,
                    limit,
                    page,
                )?;
            }
        }
        Ok(())
    }

    fn lemmas(
        &self,
        value: Option<&Value>,
        context: &str,
    ) -> Result<Vec<LemmaReference>, PackError> {
        Ok(
            Self::window(value, context, 0, self.options.strings, |value, context| {
                let object = value_object(value, context)?;
                let mut fields = Fields::new(object, context, self.options);
                let word = required_string(object, "word", context)?;
                let (word, word_truncated) = bounded(word, self.options.text_bytes);
                Ok(LemmaReference {
                    word,
                    extra: fields.text("extra")?,
                    romanization: fields.text("roman")?,
                    tags: fields.texts("tags")?,
                    truncated: fields.truncated || word_truncated,
                })
            })?
            .items,
        )
    }

    fn classifiers(
        &self,
        value: Option<&Value>,
        context: &str,
    ) -> Result<Vec<Classifier>, PackError> {
        Ok(
            Self::window(value, context, 0, self.options.strings, |value, context| {
                let mut fields = Fields::new(value_object(value, context)?, context, self.options);
                Ok(Classifier {
                    classifier: fields.text("classifier")?.unwrap_or_default(),
                    tags: fields.texts("tags")?,
                    raw_tags: fields.texts("raw_tags")?,
                    truncated: fields.truncated,
                })
            })?
            .items,
        )
    }
}

/// Typed, bounded reads of known fields from one source object.
struct Fields<'a> {
    object: &'a Map<String, Value>,
    context: &'a str,
    options: ProjectionOptions,
    truncated: bool,
}

impl<'a> Fields<'a> {
    const fn new(
        object: &'a Map<String, Value>,
        context: &'a str,
        options: ProjectionOptions,
    ) -> Self {
        Self {
            object,
            context,
            options,
            truncated: false,
        }
    }

    fn text(&mut self, field: &str) -> Result<Option<String>, PackError> {
        let (value, truncated) =
            optional_string(self.object, field, self.context, self.options.text_bytes)?;
        self.truncated |= truncated;
        Ok(value)
    }

    fn texts(&mut self, field: &str) -> Result<Vec<String>, PackError> {
        let (values, truncated) = string_array(
            self.object.get(field),
            &format!("{} `{field}`", self.context),
            self.options.strings,
            self.options.text_bytes,
        )?;
        self.truncated |= truncated;
        Ok(values)
    }

    fn flag(&self, field: &str) -> Result<Option<bool>, PackError> {
        match self.object.get(field) {
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err(PackError::Corrupt(format!(
                "{} `{field}` is not a boolean",
                self.context
            ))),
            None => Ok(None),
        }
    }

    /// Editions author sense and etymology indexes as integers or strings.
    fn index(&mut self, field: &str) -> Result<Option<String>, PackError> {
        match self.object.get(field) {
            Some(Value::Number(number)) if number.is_u64() => Ok(Some(number.to_string())),
            Some(Value::String(_)) => self.text(field),
            Some(_) => Err(PackError::Corrupt(format!(
                "{} `{field}` is not an index",
                self.context
            ))),
            None => Ok(None),
        }
    }

    fn ruby(&mut self, field: &str) -> Result<Vec<Ruby>, PackError> {
        let Some(value) = self.object.get(field) else {
            return Ok(Vec::new());
        };
        let context = format!("{} `{field}`", self.context);
        let authored = array(value, &context)?;
        self.truncated |= authored.len() > self.options.strings;
        let mut ruby = Vec::new();
        for (index, pair) in authored.iter().enumerate() {
            let pair = array(pair, &item_context(&context, index))?;
            let [Value::String(base), Value::String(text)] = pair.as_slice() else {
                return Err(PackError::Corrupt(format!(
                    "{} is not a base and text pair",
                    item_context(&context, index)
                )));
            };
            if ruby.len() < self.options.strings {
                let (base, base_truncated) = bounded(base, self.options.text_bytes);
                let (text, text_truncated) = bounded(text, self.options.text_bytes);
                self.truncated |= base_truncated || text_truncated;
                ruby.push(Ruby { base, text });
            }
        }
        Ok(ruby)
    }

    fn emphasis(&mut self, field: &str) -> Result<Vec<EmphasisRange>, PackError> {
        let Some(value) = self.object.get(field) else {
            return Ok(Vec::new());
        };
        let context = format!("{} `{field}`", self.context);
        let authored = array(value, &context)?;
        self.truncated |= authored.len() > self.options.strings;
        let mut ranges = Vec::new();
        for (index, range) in authored.iter().enumerate() {
            let range = array(range, &item_context(&context, index))?;
            let (Some(start), Some(end)) = (
                range.first().and_then(Value::as_u64),
                range.get(1).and_then(Value::as_u64),
            ) else {
                return Err(PackError::Corrupt(format!(
                    "{} is not a start and end pair",
                    item_context(&context, index)
                )));
            };
            if range.len() != 2 || start > end {
                return Err(PackError::Corrupt(format!(
                    "{} is not an ordered start and end pair",
                    item_context(&context, index)
                )));
            }
            if ranges.len() < self.options.strings {
                ranges.push(EmphasisRange { start, end });
            }
        }
        Ok(ranges)
    }
}

/// Normalizes an authored Commons file name the way `MediaWiki` titles do, or returns
/// `None` when the value is not a plausible file name.
#[must_use]
pub fn commons_file_name(authored: &str) -> Option<String> {
    let decoded = percent_decoded(authored);
    let trimmed = decoded
        .trim()
        .trim_start_matches("File:")
        .trim_start_matches("file:");
    let normalized = trimmed.replace('_', " ");
    let normalized = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    let plausible = !normalized.is_empty()
        && normalized.len() <= 255
        && normalized.contains('.')
        && !normalized.chars().any(|character| {
            matches!(
                character,
                '/' | '\\' | '#' | '<' | '>' | '[' | ']' | '|' | '{' | '}'
            ) || character.is_control()
        });
    if !plausible {
        return None;
    }
    let mut characters = normalized.chars();
    let first = characters.next()?;
    Some(first.to_uppercase().chain(characters).collect())
}

/// Decodes `%XX` sequences the way `MediaWiki` decodes them in titles.
///
/// Authored `audio` values are sometimes percent encoded, and the encoded form names no
/// file on Commons. A sequence that is not two hexadecimal digits, or that decodes to
/// something that is not UTF-8, is left exactly as authored.
fn percent_decoded(value: &str) -> String {
    if !value.contains('%') {
        return value.to_owned();
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = (index + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[index + 1..index + 3]).ok())
            .flatten()
            .filter(|_| bytes[index] == b'%')
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        if let Some(byte) = hex {
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).unwrap_or_else(|_| value.to_owned())
}

/// Places a list index inside a trailing quoted field name.
fn item_context(context: &str, index: usize) -> String {
    match context.strip_suffix('`') {
        Some(field) => format!("{field}[{index}]`"),
        None => format!("{context}[{index}]"),
    }
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    context: &str,
) -> Result<&'a str, PackError> {
    match object.get(field) {
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(PackError::Corrupt(format!(
            "{context} `{field}` is not a string"
        ))),
        None => Err(PackError::Corrupt(format!(
            "{context} has no `{field}` field"
        ))),
    }
}

fn optional_string(
    object: &Map<String, Value>,
    field: &str,
    context: &str,
    max: usize,
) -> Result<(Option<String>, bool), PackError> {
    match object.get(field) {
        Some(Value::String(value)) => {
            let (value, truncated) = bounded(value, max);
            Ok((Some(value), truncated))
        }
        Some(_) => Err(PackError::Corrupt(format!(
            "{context} `{field}` is not a string"
        ))),
        None => Ok((None, false)),
    }
}

fn bounded(value: &str, max: usize) -> (String, bool) {
    if value.len() <= max {
        return (value.to_owned(), false);
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn array<'a>(value: &'a Value, context: &str) -> Result<&'a Vec<Value>, PackError> {
    value
        .as_array()
        .ok_or_else(|| PackError::Corrupt(format!("{context} is not an array")))
}

fn value_object<'a>(value: &'a Value, context: &str) -> Result<&'a Map<String, Value>, PackError> {
    value
        .as_object()
        .ok_or_else(|| PackError::Corrupt(format!("{context} is not an object")))
}

fn string_array(
    value: Option<&Value>,
    context: &str,
    max_count: usize,
    max_bytes: usize,
) -> Result<(Vec<String>, bool), PackError> {
    let Some(value) = value else {
        return Ok((Vec::new(), false));
    };
    let values = array(value, context)?;
    let mut projected = Vec::with_capacity(values.len().min(max_count));
    let mut truncated = values.len() > max_count;
    for (index, value) in values.iter().enumerate() {
        let Value::String(value) = value else {
            return Err(PackError::Corrupt(format!(
                "{} is not a string",
                item_context(context, index)
            )));
        };
        if projected.len() < max_count {
            let (value, value_truncated) = bounded(value, max_bytes);
            projected.push(value);
            truncated |= value_truncated;
        }
    }
    Ok((projected, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commons_file_names_follow_mediawiki_title_normalization() {
        assert_eq!(
            commons_file_name("En-us-run.ogg").as_deref(),
            Some("En-us-run.ogg")
        );
        assert_eq!(
            commons_file_name("LL-Q150_(fra)-Lepticed7-éléphant.wav").as_deref(),
            Some("LL-Q150 (fra)-Lepticed7-éléphant.wav")
        );
        assert_eq!(
            commons_file_name("File:fr-échelle.ogg").as_deref(),
            Some("Fr-échelle.ogg")
        );
        assert_eq!(
            commons_file_name("LL-Q150 (fra)-Eihel-caravans%C3%A9rail.wav").as_deref(),
            Some("LL-Q150 (fra)-Eihel-caravansérail.wav"),
            "percent encoded values name no file on Commons until they are decoded"
        );
        assert_eq!(
            commons_file_name("LL-Q150 (fra)-Axel toualy-ville h%C3%B4te.wav").as_deref(),
            Some("LL-Q150 (fra)-Axel toualy-ville hôte.wav")
        );
        assert_eq!(
            commons_file_name("Fr-100%25 sure.ogg").as_deref(),
            Some("Fr-100% sure.ogg")
        );
        // A stray percent is authored text, not an escape.
        assert_eq!(
            commons_file_name("Fr-50% off.ogg").as_deref(),
            Some("Fr-50% off.ogg")
        );
        assert_eq!(
            commons_file_name("Fr-%FF%FE.ogg").as_deref(),
            Some("Fr-%FF%FE.ogg"),
            "a sequence that is not UTF-8 stays as authored"
        );
        assert_eq!(commons_file_name("no extension"), None);
        assert_eq!(commons_file_name("../escape.ogg"), None);
    }
}
