use serde_json::{Map, Value};

use crate::{EntryId, PackError, PackId, PackRevision};

/// The first non-empty exact-match stage used for an entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

/// Authored pronunciation, context, and audio-reference facts from one sound object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PronunciationSummary {
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
    pub audio: Option<String>,
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
    /// At least one string or string-array value in this sound was bounded.
    pub truncated: bool,
}

/// An authored translation suitable for typed bilingual filtering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranslationSummary {
    /// May be absent when the authored translation is represented by a note.
    pub word: Option<String>,
    pub language_name: Option<String>,
    pub language_code: Option<String>,
    pub alt: Option<String>,
    pub english: Option<String>,
    pub note: Option<String>,
    pub sense: Option<String>,
    pub taxonomic: Option<String>,
    pub romanization: Option<String>,
    pub tags: Vec<String>,
    pub raw_tags: Vec<String>,
    /// At least one string or string-array value in this translation was bounded.
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SenseSummary {
    pub glosses: Vec<String>,
    pub raw_glosses: Vec<String>,
    pub tags: Vec<String>,
    pub translations: Vec<TranslationSummary>,
    /// At least one projected string or nested array in this sense was bounded.
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SectionKind {
    Pronunciations,
    Senses,
    Translations,
}

/// Counts make bounded, progressively loadable projections explicit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgressiveSection {
    pub kind: SectionKind,
    pub available: usize,
    pub included: usize,
    pub truncated: bool,
}

/// A bounded semantic projection. It intentionally contains no HTML or generic JSON.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntrySummary {
    pub reference: EntryReference,
    pub match_class: MatchClass,
    pub headword: String,
    pub language_name: Option<String>,
    pub language_code: String,
    pub part_of_speech: Option<String>,
    pub tags: Vec<String>,
    pub pronunciations: Vec<PronunciationSummary>,
    /// Non-disambiguated translations authored on the entry.
    pub translations: Vec<TranslationSummary>,
    pub senses: Vec<SenseSummary>,
    pub sections: Vec<ProgressiveSection>,
    /// At least one entry-level scalar or tag was bounded.
    pub truncated: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct SemanticBounds {
    pub text_bytes: usize,
    pub tags: usize,
    pub pronunciations: usize,
    pub senses: usize,
    pub glosses_per_sense: usize,
    pub translations_per_sense: usize,
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
        match_class: MatchClass,
        bounds: SemanticBounds,
        target_language: Option<&str>,
    ) -> Result<EntrySummary, PackError> {
        let (tags, tags_truncated) = string_array(
            self.object.get("tags"),
            "record `tags`",
            bounds.tags,
            bounds.text_bytes,
        )?;
        let (pronunciations, pronunciation_total) = pronunciations(&self.object, bounds)?;
        let (translations, top_translation_total) = translations(
            self.object.get("translations"),
            "record `translations`",
            bounds,
            target_language,
        )?;
        let top_translation_included = translations.len();
        let (senses, sense_total, sense_translation_total, sense_translation_included) =
            senses(&self.object, bounds, target_language)?;
        let (headword, headword_truncated) = bounded(&self.word, bounds.text_bytes);
        let (language_code, language_code_truncated) = bounded(&self.lang_code, bounds.text_bytes);
        let (language_name, language_name_truncated) =
            optional_string(&self.object, "lang", "record", bounds.text_bytes)?;
        let (part_of_speech, part_of_speech_truncated) =
            optional_string(&self.object, "pos", "record", bounds.text_bytes)?;
        let translation_total = top_translation_total + sense_translation_total;
        let translation_included = top_translation_included + sense_translation_included;
        Ok(EntrySummary {
            reference,
            match_class,
            headword,
            language_name,
            language_code,
            part_of_speech,
            tags,
            pronunciations,
            translations,
            senses,
            sections: vec![
                section(
                    SectionKind::Pronunciations,
                    pronunciation_total,
                    bounds.pronunciations,
                ),
                section(SectionKind::Senses, sense_total, bounds.senses),
                ProgressiveSection {
                    kind: SectionKind::Translations,
                    available: translation_total,
                    included: translation_included,
                    truncated: translation_included < translation_total,
                },
            ],
            truncated: tags_truncated
                || headword_truncated
                || language_code_truncated
                || language_name_truncated
                || part_of_speech_truncated,
        })
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
                "{context}[{index}] is not a string"
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

fn projected_string(
    object: &Map<String, Value>,
    field: &str,
    context: &str,
    max: usize,
    truncated: &mut bool,
) -> Result<Option<String>, PackError> {
    let (value, was_truncated) = optional_string(object, field, context, max)?;
    *truncated |= was_truncated;
    Ok(value)
}

fn projected_array(
    value: Option<&Value>,
    context: &str,
    max_count: usize,
    max_bytes: usize,
    truncated: &mut bool,
) -> Result<Vec<String>, PackError> {
    let (value, was_truncated) = string_array(value, context, max_count, max_bytes)?;
    *truncated |= was_truncated;
    Ok(value)
}

fn pronunciations(
    object: &Map<String, Value>,
    bounds: SemanticBounds,
) -> Result<(Vec<PronunciationSummary>, usize), PackError> {
    let Some(value) = object.get("sounds") else {
        return Ok((Vec::new(), 0));
    };
    let authored = array(value, "record `sounds`")?;
    let mut projected = Vec::with_capacity(authored.len().min(bounds.pronunciations));
    for (index, value) in authored.iter().enumerate() {
        let context = format!("record `sounds[{index}]`");
        let sound = value_object(value, &context)?;
        let pronunciation = pronunciation(sound, &context, bounds)?;
        if projected.len() < bounds.pronunciations {
            projected.push(pronunciation);
        }
    }
    Ok((projected, authored.len()))
}

fn pronunciation(
    value: &Map<String, Value>,
    context: &str,
    bounds: SemanticBounds,
) -> Result<PronunciationSummary, PackError> {
    let mut truncated = false;
    macro_rules! scalar {
        ($field:literal) => {
            projected_string(value, $field, context, bounds.text_bytes, &mut truncated)?
        };
    }
    let ipa = scalar!("ipa");
    let enpr = scalar!("enpr");
    let legacy_zh_pronunciation = scalar!("zh_pron");
    let current_zh_pronunciation = scalar!("zh-pron");
    let hangeul = scalar!("hangeul");
    let homophone = scalar!("homophone");
    let romanization = scalar!("roman");
    let form = scalar!("form");
    let rhymes = scalar!("rhymes");
    let text = scalar!("text");
    let note = scalar!("note");
    let other = scalar!("other");
    let audio = scalar!("audio");
    let audio_ipa = scalar!("audio-ipa");
    let wav_url = scalar!("wav_url");
    let vorbis_url = scalar!("ogg_url");
    let oga_container_url = scalar!("oga_url");
    let mp3_url = scalar!("mp3_url");
    let opus_url = scalar!("opus_url");
    let flac_url = scalar!("flac_url");
    let homophones = projected_array(
        value.get("homophones"),
        &format!("{context} `homophones`"),
        bounds.tags,
        bounds.text_bytes,
        &mut truncated,
    )?;
    let hyphenation = projected_array(
        value.get("hyphenation"),
        &format!("{context} `hyphenation`"),
        bounds.tags,
        bounds.text_bytes,
        &mut truncated,
    )?;
    let tags = projected_array(
        value.get("tags"),
        &format!("{context} `tags`"),
        bounds.tags,
        bounds.text_bytes,
        &mut truncated,
    )?;
    let raw_tags = projected_array(
        value.get("raw_tags"),
        &format!("{context} `raw_tags`"),
        bounds.tags,
        bounds.text_bytes,
        &mut truncated,
    )?;
    Ok(PronunciationSummary {
        ipa,
        enpr,
        zh_pronunciation: current_zh_pronunciation.or(legacy_zh_pronunciation),
        hangeul,
        homophone,
        romanization,
        form,
        rhymes,
        text,
        note,
        other,
        audio,
        audio_ipa,
        wav_url,
        ogg_url: vorbis_url,
        oga_url: oga_container_url,
        mp3_url,
        opus_url,
        flac_url,
        homophones,
        hyphenation,
        tags,
        raw_tags,
        truncated,
    })
}

fn senses(
    object: &Map<String, Value>,
    bounds: SemanticBounds,
    target_language: Option<&str>,
) -> Result<(Vec<SenseSummary>, usize, usize, usize), PackError> {
    let Some(value) = object.get("senses") else {
        return Ok((Vec::new(), 0, 0, 0));
    };
    let authored = array(value, "record `senses`")?;
    let mut translation_total = 0;
    let mut translation_included = 0;
    let mut projected = Vec::with_capacity(authored.len().min(bounds.senses));
    for (index, value) in authored.iter().enumerate() {
        let context = format!("record `senses[{index}]`");
        let sense = value_object(value, &context)?;
        let (translations, total) = translations(
            sense.get("translations"),
            &format!("{context} `translations`"),
            bounds,
            target_language,
        )?;
        translation_total += total;
        let (glosses, glosses_truncated) = string_array(
            sense.get("glosses"),
            &format!("{context} `glosses`"),
            bounds.glosses_per_sense,
            bounds.text_bytes,
        )?;
        let (raw_glosses, raw_glosses_truncated) = string_array(
            sense.get("raw_glosses"),
            &format!("{context} `raw_glosses`"),
            bounds.glosses_per_sense,
            bounds.text_bytes,
        )?;
        let (tags, tags_truncated) = string_array(
            sense.get("tags"),
            &format!("{context} `tags`"),
            bounds.tags,
            bounds.text_bytes,
        )?;
        if projected.len() < bounds.senses {
            translation_included += translations.len();
            projected.push(SenseSummary {
                glosses,
                raw_glosses,
                tags,
                truncated: glosses_truncated
                    || raw_glosses_truncated
                    || tags_truncated
                    || translations.iter().any(|translation| translation.truncated)
                    || translations.len() < total,
                translations,
            });
        }
    }
    Ok((
        projected,
        authored.len(),
        translation_total,
        translation_included,
    ))
}

fn translations(
    value: Option<&Value>,
    context: &str,
    bounds: SemanticBounds,
    target_language: Option<&str>,
) -> Result<(Vec<TranslationSummary>, usize), PackError> {
    let Some(value) = value else {
        return Ok((Vec::new(), 0));
    };
    let authored = array(value, context)?;
    let mut projected = Vec::new();
    let mut matching = 0;
    for (index, value) in authored.iter().enumerate() {
        let item_context = format!("{context}[{index}]");
        let authored = value_object(value, &item_context)?;
        let translation = translation(authored, &item_context, bounds)?;
        let authored_language_code = authored
            .get("lang_code")
            .or_else(|| authored.get("code"))
            .and_then(Value::as_str);
        if target_language.is_none_or(|target| authored_language_code == Some(target)) {
            matching += 1;
            if projected.len() < bounds.translations_per_sense {
                projected.push(translation);
            }
        }
    }
    Ok((projected, matching))
}

fn translation(
    value: &Map<String, Value>,
    context: &str,
    bounds: SemanticBounds,
) -> Result<TranslationSummary, PackError> {
    let mut truncated = false;
    let word = projected_string(value, "word", context, bounds.text_bytes, &mut truncated)?;
    let language_name =
        projected_string(value, "lang", context, bounds.text_bytes, &mut truncated)?;
    let lang_code = projected_string(
        value,
        "lang_code",
        context,
        bounds.text_bytes,
        &mut truncated,
    )?;
    let code = projected_string(value, "code", context, bounds.text_bytes, &mut truncated)?;
    let alt = projected_string(value, "alt", context, bounds.text_bytes, &mut truncated)?;
    let english = projected_string(value, "english", context, bounds.text_bytes, &mut truncated)?;
    let note = projected_string(value, "note", context, bounds.text_bytes, &mut truncated)?;
    let sense = projected_string(value, "sense", context, bounds.text_bytes, &mut truncated)?;
    let taxonomic = projected_string(
        value,
        "taxonomic",
        context,
        bounds.text_bytes,
        &mut truncated,
    )?;
    let romanization =
        projected_string(value, "roman", context, bounds.text_bytes, &mut truncated)?;
    let tags = projected_array(
        value.get("tags"),
        &format!("{context} `tags`"),
        bounds.tags,
        bounds.text_bytes,
        &mut truncated,
    )?;
    let raw_tags = projected_array(
        value.get("raw_tags"),
        &format!("{context} `raw_tags`"),
        bounds.tags,
        bounds.text_bytes,
        &mut truncated,
    )?;
    if word.is_none() && note.is_none() {
        return Err(PackError::Corrupt(format!(
            "{context} has neither a `word` nor a `note` field"
        )));
    }
    Ok(TranslationSummary {
        word,
        language_name,
        language_code: lang_code.or(code),
        alt,
        english,
        note,
        sense,
        taxonomic,
        romanization,
        tags,
        raw_tags,
        truncated,
    })
}

const fn section(kind: SectionKind, available: usize, maximum: usize) -> ProgressiveSection {
    let included = if available < maximum {
        available
    } else {
        maximum
    };
    ProgressiveSection {
        kind,
        available,
        included,
        truncated: included < available,
    }
}
