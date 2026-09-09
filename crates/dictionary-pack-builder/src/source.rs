use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read};
use std::str;

use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};
use thiserror::Error;

/// A selected, validated source record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRecord {
    /// One-based line number in the uncompressed source.
    pub line_number: u64,
    /// Zero-based byte offset of the line in the uncompressed source.
    pub byte_offset: u64,
    /// Exact source bytes, excluding the terminal LF or CRLF delimiter.
    pub source_bytes: Vec<u8>,
    /// The selected record's authored headword.
    pub word: String,
    /// Qualified `forms[].form` values in source order.
    pub routing_forms: Vec<String>,
    /// Parsed selected object reused by crate-internal consumers.
    pub(crate) object: Map<String, Value>,
}

/// A source JSONL input error.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SourceIngestionError {
    /// Reading the decompressed source failed.
    #[error("failed to read source line {line_number} at byte offset {byte_offset}: {source}")]
    Io {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The source position cannot be represented by the public offset type.
    #[error("source position overflow at line {line_number}, byte offset {byte_offset}")]
    PositionOverflow {
        /// One-based source line number.
        line_number: u64,
        /// Last representable zero-based byte offset.
        byte_offset: u64,
    },
    /// A delimiter-free line exceeded the configured byte bound.
    #[error(
        "source line {line_number} at byte offset {byte_offset} exceeds the {max_bytes}-byte limit"
    )]
    LineTooLong {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
        /// Configured maximum delimiter-free line length.
        max_bytes: usize,
    },
    /// A delimiter-free source line was empty.
    #[error("source line {line_number} at byte offset {byte_offset} is empty")]
    EmptyLine {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
    },
    /// A source line was not UTF-8.
    #[error(
        "source line {line_number} at byte offset {byte_offset} is not UTF-8 (valid through byte {valid_up_to})"
    )]
    InvalidUtf8 {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
        /// First byte index not known to be valid UTF-8.
        valid_up_to: usize,
    },
    /// A source line was not valid JSON.
    #[error("source line {line_number} at byte offset {byte_offset} is malformed JSON: {source}")]
    MalformedJson {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
        /// JSON parser error.
        #[source]
        source: serde_json::Error,
    },
    /// A source line's top-level JSON value was not an object.
    #[error("source line {line_number} at byte offset {byte_offset} is not a JSON object")]
    NonObject {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
    },
    /// A present `lang_code` field was not a string.
    #[error(
        "source line {line_number} at byte offset {byte_offset} has {actual} `lang_code`; expected a string"
    )]
    InvalidLangCode {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
        /// JSON type found in the field.
        actual: &'static str,
    },
    /// A selected record had no `word` field.
    #[error("source line {line_number} at byte offset {byte_offset} has no `word`")]
    MissingWord {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
    },
    /// A selected record's `word` field was not a string.
    #[error(
        "source line {line_number} at byte offset {byte_offset} has {actual} `word`; expected a string"
    )]
    InvalidWord {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
        /// JSON type found in the field.
        actual: &'static str,
    },
    /// A selected record's headword was empty.
    #[error("source line {line_number} at byte offset {byte_offset} has an empty `word`")]
    EmptyWord {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
    },
    /// A selected record's `forms` field was not an array.
    #[error(
        "source line {line_number} at byte offset {byte_offset} has {actual} `forms`; expected an array"
    )]
    InvalidForms {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
        /// JSON type found in the field.
        actual: &'static str,
    },
    /// An element of `forms` was not an object.
    #[error(
        "source line {line_number} at byte offset {byte_offset} has {actual} `forms[{index}]`; expected an object"
    )]
    InvalidFormEntry {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
        /// Zero-based index in `forms`.
        index: usize,
        /// JSON type found at the index.
        actual: &'static str,
    },
    /// A `forms` element had no `form` field.
    #[error("source line {line_number} at byte offset {byte_offset} has no `forms[{index}].form`")]
    MissingForm {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
        /// Zero-based index in `forms`.
        index: usize,
    },
    /// A `forms[].form` field was not a string.
    #[error(
        "source line {line_number} at byte offset {byte_offset} has {actual} `forms[{index}].form`; expected a string"
    )]
    InvalidForm {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
        /// Zero-based index in `forms`.
        index: usize,
        /// JSON type found in the field.
        actual: &'static str,
    },
    /// A selected record exceeded the routing-form count bound.
    #[error(
        "source line {line_number} at byte offset {byte_offset} exceeds the {max_forms}-routing-form limit"
    )]
    TooManyRoutingForms {
        /// One-based source line number.
        line_number: u64,
        /// Zero-based uncompressed source byte offset.
        byte_offset: u64,
        /// Configured maximum number of retained routing forms.
        max_forms: usize,
    },
}

/// Streaming reader for decompressed Wiktextract JSONL.
///
/// The reader buffers at most one line up to `max_line_bytes + 1` bytes. Oversized
/// lines are drained before an error is returned, so iteration can continue.
pub struct JsonlSourceReader<R> {
    reader: BufReader<R>,
    selected_lang_code: String,
    max_line_bytes: usize,
    max_routing_forms: usize,
    next_line_number: u64,
    next_byte_offset: u64,
    finished: bool,
    source_record_count: u64,
}

impl<R: Read> JsonlSourceReader<R> {
    /// Creates a bounded source reader.
    pub fn new(
        reader: R,
        selected_lang_code: impl Into<String>,
        max_line_bytes: usize,
        max_routing_forms: usize,
    ) -> Self {
        Self {
            reader: BufReader::new(reader),
            selected_lang_code: selected_lang_code.into(),
            max_line_bytes,
            max_routing_forms,
            next_line_number: 1,
            next_byte_offset: 0,
            finished: false,
            source_record_count: 0,
        }
    }

    /// Returns the number of physical JSONL records read so far.
    #[must_use]
    pub const fn source_record_count(&self) -> u64 {
        self.source_record_count
    }

    fn read_line(&mut self) -> Result<Option<(Vec<u8>, bool, u64)>, io::Error> {
        let mut line = Vec::new();
        let mut discarded = false;
        let mut physical_length = 0_u64;

        loop {
            let buffer = self.reader.fill_buf()?;
            if buffer.is_empty() {
                return if physical_length == 0 {
                    Ok(None)
                } else {
                    Ok(Some((line, discarded, physical_length)))
                };
            }

            let lf_index = buffer.iter().position(|byte| *byte == b'\n');
            let consumed = lf_index.map_or(buffer.len(), |index| index + 1);
            let content_length = lf_index.unwrap_or(consumed);
            for &byte in &buffer[..content_length] {
                if line.len() <= self.max_line_bytes {
                    line.push(byte);
                } else {
                    discarded = true;
                }
            }
            physical_length =
                physical_length.saturating_add(u64::try_from(consumed).unwrap_or(u64::MAX));
            self.reader.consume(consumed);

            if lf_index.is_some() {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some((line, discarded, physical_length)));
            }
        }
    }

    fn process_line(
        &self,
        source_bytes: Vec<u8>,
        line_number: u64,
        byte_offset: u64,
    ) -> Result<Option<SourceRecord>, SourceIngestionError> {
        if source_bytes.is_empty() {
            return Err(SourceIngestionError::EmptyLine {
                line_number,
                byte_offset,
            });
        }
        let text =
            str::from_utf8(&source_bytes).map_err(|error| SourceIngestionError::InvalidUtf8 {
                line_number,
                byte_offset,
                valid_up_to: error.valid_up_to(),
            })?;
        if !text.trim_start().starts_with('{') {
            return Err(SourceIngestionError::NonObject {
                line_number,
                byte_offset,
            });
        }
        let routing: RoutingObject<'_> =
            serde_json::from_str(text).map_err(|source| SourceIngestionError::MalformedJson {
                line_number,
                byte_offset,
                source,
            })?;

        let Some(lang_code) = routing.lang_code else {
            return Ok(None);
        };
        let RoutingString::String(lang_code) = lang_code else {
            return Err(SourceIngestionError::InvalidLangCode {
                line_number,
                byte_offset,
                actual: lang_code.json_type(),
            });
        };
        if lang_code != self.selected_lang_code {
            return Ok(None);
        }

        let TopLevelObject(object) =
            serde_json::from_str(text).map_err(|source| SourceIngestionError::MalformedJson {
                line_number,
                byte_offset,
                source,
            })?;
        let word = required_word(&object, line_number, byte_offset)?;
        let routing_forms =
            routing_forms(&object, line_number, byte_offset, self.max_routing_forms)?;
        Ok(Some(SourceRecord {
            line_number,
            byte_offset,
            source_bytes,
            word,
            routing_forms,
            object,
        }))
    }
}

struct RoutingObject<'a> {
    lang_code: Option<RoutingString<'a>>,
}

impl<'de> Deserialize<'de> for RoutingObject<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = RoutingObject<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object with unique top-level fields")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut fields = HashSet::new();
                let mut lang_code = None;
                while let Some(FieldName(key)) = access.next_key()? {
                    let is_lang_code = key == "lang_code";
                    if fields.contains(key.as_ref()) {
                        return Err(de::Error::custom(format!(
                            "duplicate top-level field `{key}`"
                        )));
                    }
                    fields.insert(key);
                    if is_lang_code {
                        lang_code = Some(access.next_value()?);
                    } else {
                        access.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(RoutingObject { lang_code })
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

struct FieldName<'a>(Cow<'a, str>);

impl<'de> Deserialize<'de> for FieldName<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldNameVisitor;

        impl<'de> Visitor<'de> for FieldNameVisitor {
            type Value = FieldName<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object field name")
            }

            fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Self::Value, E> {
                Ok(FieldName(Cow::Borrowed(value)))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(FieldName(Cow::Owned(value.to_owned())))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(FieldName(Cow::Owned(value)))
            }
        }

        deserializer.deserialize_string(FieldNameVisitor)
    }
}

enum RoutingString<'a> {
    String(Cow<'a, str>),
    Other(&'static str),
}

impl RoutingString<'_> {
    const fn json_type(&self) -> &'static str {
        match self {
            Self::String(_) => "string",
            Self::Other(json_type) => json_type,
        }
    }
}

impl<'de> Deserialize<'de> for RoutingString<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StringVisitor;

        impl<'de> Visitor<'de> for StringVisitor {
            type Value = RoutingString<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("any JSON value")
            }

            fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Self::Value, E> {
                Ok(RoutingString::String(Cow::Borrowed(value)))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(RoutingString::String(Cow::Owned(value.to_owned())))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(RoutingString::String(Cow::Owned(value)))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(RoutingString::Other("boolean"))
            }

            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(RoutingString::Other("number"))
            }

            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(RoutingString::Other("number"))
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(RoutingString::Other("number"))
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RoutingString::Other("null"))
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RoutingString::Other("null"))
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(RoutingString::Other("array"))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(RoutingString::Other("object"))
            }
        }

        deserializer.deserialize_any(StringVisitor)
    }
}

struct TopLevelObject(Map<String, Value>);

impl<'de> Deserialize<'de> for TopLevelObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = TopLevelObject;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object with unique top-level fields")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut object = Map::new();
                while let Some((key, value)) = access.next_entry::<String, Value>()? {
                    if object.insert(key.clone(), value).is_some() {
                        return Err(de::Error::custom(format!(
                            "duplicate top-level field `{key}`"
                        )));
                    }
                }
                Ok(TopLevelObject(object))
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

impl<R: Read> Iterator for JsonlSourceReader<R> {
    type Item = Result<SourceRecord, SourceIngestionError>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.finished {
            let line_number = self.next_line_number;
            let byte_offset = self.next_byte_offset;
            let read = match self.read_line() {
                Ok(Some(read)) => read,
                Ok(None) => {
                    self.finished = true;
                    return None;
                }
                Err(source) => {
                    self.finished = true;
                    return Some(Err(SourceIngestionError::Io {
                        line_number,
                        byte_offset,
                        source,
                    }));
                }
            };
            let (source_bytes, discarded, physical_length) = read;
            let Some(next_byte_offset) = byte_offset.checked_add(physical_length) else {
                self.finished = true;
                return Some(Err(SourceIngestionError::PositionOverflow {
                    line_number,
                    byte_offset,
                }));
            };
            let Some(next_line_number) = line_number.checked_add(1) else {
                self.finished = true;
                return Some(Err(SourceIngestionError::PositionOverflow {
                    line_number,
                    byte_offset,
                }));
            };
            self.next_byte_offset = next_byte_offset;
            self.next_line_number = next_line_number;
            self.source_record_count = self.source_record_count.saturating_add(1);

            if discarded || source_bytes.len() > self.max_line_bytes {
                return Some(Err(SourceIngestionError::LineTooLong {
                    line_number,
                    byte_offset,
                    max_bytes: self.max_line_bytes,
                }));
            }
            match self.process_line(source_bytes, line_number, byte_offset) {
                Ok(Some(record)) => return Some(Ok(record)),
                Ok(None) => {}
                Err(error) => return Some(Err(error)),
            }
        }
        None
    }
}

fn required_word(
    object: &Map<String, Value>,
    line_number: u64,
    byte_offset: u64,
) -> Result<String, SourceIngestionError> {
    let word = object
        .get("word")
        .ok_or(SourceIngestionError::MissingWord {
            line_number,
            byte_offset,
        })?;
    let Value::String(word) = word else {
        return Err(SourceIngestionError::InvalidWord {
            line_number,
            byte_offset,
            actual: json_type(word),
        });
    };
    if word.is_empty() {
        return Err(SourceIngestionError::EmptyWord {
            line_number,
            byte_offset,
        });
    }
    Ok(word.clone())
}

fn routing_forms(
    object: &Map<String, Value>,
    line_number: u64,
    byte_offset: u64,
    max_forms: usize,
) -> Result<Vec<String>, SourceIngestionError> {
    let Some(forms) = object.get("forms") else {
        return Ok(Vec::new());
    };
    let Value::Array(forms) = forms else {
        return Err(SourceIngestionError::InvalidForms {
            line_number,
            byte_offset,
            actual: json_type(forms),
        });
    };

    let mut routing_forms = Vec::new();
    for (index, form) in forms.iter().enumerate() {
        let Value::Object(form) = form else {
            return Err(SourceIngestionError::InvalidFormEntry {
                line_number,
                byte_offset,
                index,
                actual: json_type(form),
            });
        };
        let value = form.get("form").ok_or(SourceIngestionError::MissingForm {
            line_number,
            byte_offset,
            index,
        })?;
        let Value::String(value) = value else {
            return Err(SourceIngestionError::InvalidForm {
                line_number,
                byte_offset,
                index,
                actual: json_type(value),
            });
        };
        if value.is_empty() || value == "-" {
            continue;
        }
        if routing_forms.len() == max_forms {
            return Err(SourceIngestionError::TooManyRoutingForms {
                line_number,
                byte_offset,
                max_forms,
            });
        }
        routing_forms.push(value.clone());
    }
    Ok(routing_forms)
}

const fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{JsonlSourceReader, SourceIngestionError};

    const LIMIT: usize = 1_024;

    fn reader(input: &[u8]) -> JsonlSourceReader<Cursor<&[u8]>> {
        JsonlSourceReader::new(Cursor::new(input), "en", LIMIT, 4)
    }

    #[test]
    fn preserves_bytes_and_tracks_lines_and_offsets_for_all_endings() {
        let input = concat!(
            "{\"lang_code\":\"en\",\"word\":\"one\"}\r\n",
            "{\"lang_code\":\"fr\",\"word\":\"deux\"}\n",
            "{\"lang_code\":\"en\",\"word\":\"三\"}"
        );
        let records = reader(input.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .expect("valid source");

        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0].source_bytes,
            br#"{"lang_code":"en","word":"one"}"#
        );
        assert_eq!(records[0].line_number, 1);
        assert_eq!(records[0].byte_offset, 0);
        assert_eq!(
            records[1].source_bytes,
            "{\"lang_code\":\"en\",\"word\":\"三\"}".as_bytes()
        );
        assert_eq!(records[1].line_number, 3);
        assert_eq!(
            records[1].byte_offset,
            u64::try_from(
                input
                    .find("{\"lang_code\":\"en\",\"word\":\"三\"}")
                    .expect("third line")
            )
            .expect("offset fits")
        );
        assert_eq!(records[1].word, "三");
    }

    #[test]
    fn rejects_lf_and_crlf_empty_lines() {
        for input in [b"\n".as_slice(), b"\r\n".as_slice()] {
            assert!(matches!(
                reader(input).next().expect("error"),
                Err(SourceIngestionError::EmptyLine {
                    line_number: 1,
                    byte_offset: 0
                })
            ));
        }
    }

    #[test]
    fn rejects_oversized_lines_without_losing_the_next_line() {
        let input =
            b"12345678901234567890123456789012345678901\n{\"lang_code\":\"en\",\"word\":\"ok\"}\n";
        let mut records = JsonlSourceReader::new(Cursor::new(input), "en", 40, 4);
        assert!(matches!(
            records.next().expect("size error"),
            Err(SourceIngestionError::LineTooLong { max_bytes: 40, .. })
        ));
        let record = records.next().expect("record").expect("valid record");
        assert_eq!(record.line_number, 2);
        assert_eq!(record.byte_offset, 42);
    }

    #[test]
    fn accepts_exact_bound_before_crlf() {
        let input = b"{}\r\n";
        let mut records = JsonlSourceReader::new(Cursor::new(input), "en", 2, 4);
        assert!(records.next().is_none());
    }

    #[test]
    fn preserves_a_final_carriage_return_without_lf() {
        let input = b"{\"lang_code\":\"en\",\"word\":\"x\"}\r";
        let record = reader(input).next().expect("record").expect("valid record");
        assert_eq!(record.source_bytes, input);
    }

    #[test]
    fn rejects_invalid_utf8_json_and_non_objects() {
        assert!(matches!(
            reader(b"{\"lang_code\":\"en\",\"word\":\"\xff\"}\n")
                .next()
                .expect("UTF-8 error"),
            Err(SourceIngestionError::InvalidUtf8 { .. })
        ));
        assert!(matches!(
            reader(b"{not json}\n").next().expect("JSON error"),
            Err(SourceIngestionError::MalformedJson { .. })
        ));
        assert!(matches!(
            reader(b"[]\n").next().expect("object error"),
            Err(SourceIngestionError::NonObject { .. })
        ));
    }

    #[test]
    fn rejects_duplicate_top_level_fields() {
        let error = reader(
            br#"{"lang_code":"en","word":"one","word":"two"}
"#,
        )
        .next()
        .unwrap()
        .unwrap_err();
        assert!(matches!(error, SourceIngestionError::MalformedJson { .. }));
        assert!(
            error
                .to_string()
                .contains("duplicate top-level field `word`")
        );
    }

    #[test]
    fn rejects_duplicate_unknown_fields_on_unselected_records() {
        let error = reader(br#"{"unknown":{"large":[1,2,3]},"lang_code":"fr","unknown":null}"#)
            .next()
            .expect("duplicate error")
            .expect_err("duplicate must be rejected");
        assert!(matches!(error, SourceIngestionError::MalformedJson { .. }));
        assert!(
            error
                .to_string()
                .contains("duplicate top-level field `unknown`")
        );
    }

    #[test]
    fn skips_redirects_and_other_languages_but_rejects_mistyped_language() {
        let input = concat!(
            "{\"redirect\":\"target\"}\n",
            "{\"lang_code\":\"fr\",\"word\":false}\n",
            "{\"lang_code\":7,\"word\":\"bad\"}\n"
        );
        let error = reader(input.as_bytes()).next().expect("language error");
        assert!(matches!(
            error,
            Err(SourceIngestionError::InvalidLangCode {
                line_number: 3,
                byte_offset: _,
                actual: "number"
            })
        ));
    }

    #[test]
    fn requires_a_non_empty_string_word_only_for_selected_records() {
        for (input, expected) in [
            (r#"{"lang_code":"en"}"#, "missing"),
            (r#"{"lang_code":"en","word":null}"#, "invalid"),
            (r#"{"lang_code":"en","word":""}"#, "empty"),
        ] {
            let error = reader(input.as_bytes()).next().expect("word error");
            assert!(
                matches!(
                    (&error, expected),
                    (Err(SourceIngestionError::MissingWord { .. }), "missing")
                        | (Err(SourceIngestionError::InvalidWord { .. }), "invalid")
                        | (Err(SourceIngestionError::EmptyWord { .. }), "empty")
                ),
                "unexpected result: {error:?}"
            );
        }
    }

    #[test]
    fn extracts_unicode_routing_forms_and_excludes_placeholders() {
        let input = concat!(
            "{\"lang_code\":\"en\",\"word\":\"café\",",
            "\"forms\":[{\"form\":\"cafés\",\"tags\":[\"plural\"]},",
            "{\"form\":\"\"},{\"form\":\"-\"},{\"form\":\"咖啡\"}],",
            "\"unknown\":true}\n"
        );
        let record = reader(input.as_bytes())
            .next()
            .expect("record")
            .expect("valid record");
        assert_eq!(record.word, "café");
        assert_eq!(record.routing_forms, ["cafés", "咖啡"]);
        assert_eq!(record.source_bytes, input.trim_end().as_bytes());
    }

    #[test]
    fn selects_when_word_and_forms_precede_language() {
        let input = br#"{"forms":[{"form":"before"}],"word":"ordered","lang_code":"en"}"#;
        let record = reader(input).next().expect("record").expect("valid record");
        assert_eq!(record.word, "ordered");
        assert_eq!(record.routing_forms, ["before"]);
    }

    #[test]
    fn skips_large_nested_values_on_unselected_records() {
        let values = (0..2_000)
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        let input = format!(
            "{{\"word\":false,\"nested\":[{}],\"lang_code\":\"fr\"}}\n\
             {{\"lang_code\":\"en\",\"word\":\"selected\"}}\n",
            values.join(",")
        );
        let mut records = JsonlSourceReader::new(Cursor::new(input.as_bytes()), "en", 16_384, 4);
        let record = records
            .next()
            .expect("selected record")
            .expect("valid source");
        assert_eq!(record.line_number, 2);
        assert_eq!(record.word, "selected");
    }

    #[test]
    fn retains_selected_object_for_crate_internal_consumers() {
        let input = br#"{"word":"kept","lang_code":"en","senses":[{"glosses":["fact"]}]}"#;
        let record = reader(input).next().expect("record").expect("valid record");
        assert_eq!(record.object.get("word"), Some(&serde_json::json!("kept")));
        assert_eq!(
            record.object.get("senses"),
            Some(&serde_json::json!([{"glosses": ["fact"]}]))
        );
    }

    #[test]
    fn rejects_incompatible_forms_shapes() {
        let cases = [
            (r#""bad""#, "forms"),
            (r#"["bad"]"#, "entry"),
            (r"[{}]", "missing"),
            (r#"[{"form":3}]"#, "form"),
        ];
        for (forms, expected) in cases {
            let input = format!(r#"{{"lang_code":"en","word":"x","forms":{forms}}}"#);
            let error = reader(input.as_bytes()).next().expect("forms error");
            assert!(
                matches!(
                    (&error, expected),
                    (Err(SourceIngestionError::InvalidForms { .. }), "forms")
                        | (Err(SourceIngestionError::InvalidFormEntry { .. }), "entry")
                        | (Err(SourceIngestionError::MissingForm { .. }), "missing")
                        | (Err(SourceIngestionError::InvalidForm { .. }), "form")
                ),
                "unexpected result: {error:?}"
            );
        }
    }

    #[test]
    fn bounds_retained_routing_forms() {
        let input =
            br#"{"lang_code":"en","word":"x","forms":[{"form":"a"},{"form":""},{"form":"b"}]}"#;
        let error = JsonlSourceReader::new(Cursor::new(input), "en", LIMIT, 1)
            .next()
            .expect("forms error");
        assert!(matches!(
            error,
            Err(SourceIngestionError::TooManyRoutingForms { max_forms: 1, .. })
        ));
    }
}
