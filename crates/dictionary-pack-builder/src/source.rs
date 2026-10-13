use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read};
use std::str;

use elephant_ladder_dictionary_pack::MAX_ROUTING_FORMS;
use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};
use thiserror::Error;

/// A selected, validated source record.
#[derive(Debug)]
pub(crate) struct SourceRecord {
    /// One-based line number in the uncompressed source.
    pub line_number: u64,
    /// Zero-based byte offset of the line in the uncompressed source.
    pub byte_offset: u64,
    /// Exact source bytes, excluding the terminal LF or CRLF delimiter.
    pub source_bytes: Vec<u8>,
    pub word: String,
    /// Qualified `forms[].form` values in source order.
    pub routing_forms: Vec<String>,
}

/// A source JSONL input error, located by one-based line and zero-based byte offset.
#[derive(Debug, Error)]
pub enum SourceIngestionError {
    #[error("failed to read source line {line_number} at byte offset {byte_offset}: {source}")]
    Io {
        line_number: u64,
        byte_offset: u64,
        #[source]
        source: io::Error,
    },
    #[error(
        "source line {line_number} at byte offset {byte_offset} exceeds the {max_bytes}-byte limit"
    )]
    LineTooLong {
        line_number: u64,
        byte_offset: u64,
        max_bytes: usize,
    },
    #[error("source line {line_number} at byte offset {byte_offset} is malformed JSON: {source}")]
    MalformedJson {
        line_number: u64,
        byte_offset: u64,
        #[source]
        source: serde_json::Error,
    },
    #[error("source line {line_number} at byte offset {byte_offset} {reason}")]
    Invalid {
        line_number: u64,
        byte_offset: u64,
        reason: String,
    },
}

/// Streaming reader for decompressed Wiktextract JSONL that yields the records of
/// one language.
///
/// The reader buffers at most one line up to `max_line_bytes + 1` bytes. Oversized
/// lines are drained before an error is returned, so iteration can continue.
pub(crate) struct JsonlSourceReader<R> {
    reader: BufReader<R>,
    selected_lang_code: String,
    max_line_bytes: usize,
    next_line_number: u64,
    next_byte_offset: u64,
    finished: bool,
    source_record_count: u64,
}

impl<R: Read> JsonlSourceReader<R> {
    pub(crate) fn new(reader: R, selected_lang_code: &str, max_line_bytes: usize) -> Self {
        Self {
            reader: BufReader::new(reader),
            selected_lang_code: selected_lang_code.to_owned(),
            max_line_bytes,
            next_line_number: 1,
            next_byte_offset: 0,
            finished: false,
            source_record_count: 0,
        }
    }

    /// The number of physical JSONL records read so far.
    pub(crate) const fn source_record_count(&self) -> u64 {
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
            physical_length += consumed as u64;
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
        let invalid = |reason: String| SourceIngestionError::Invalid {
            line_number,
            byte_offset,
            reason,
        };
        let malformed = |source| SourceIngestionError::MalformedJson {
            line_number,
            byte_offset,
            source,
        };
        if source_bytes.is_empty() {
            return Err(invalid("is empty".to_owned()));
        }
        let text = str::from_utf8(&source_bytes).map_err(|error| {
            invalid(format!(
                "is not UTF-8 (valid through byte {})",
                error.valid_up_to()
            ))
        })?;
        if !text.trim_start().starts_with('{') {
            return Err(invalid("is not a JSON object".to_owned()));
        }
        let routing: RoutingObject<'_> = serde_json::from_str(text).map_err(malformed)?;
        let Some(lang_code) = routing.lang_code else {
            return Ok(None);
        };
        let RoutingString::String(lang_code) = lang_code else {
            return Err(invalid(format!(
                "has {} `lang_code`; expected a string",
                lang_code.json_type()
            )));
        };
        if lang_code != self.selected_lang_code {
            return Ok(None);
        }

        let TopLevelObject(object) = serde_json::from_str(text).map_err(malformed)?;
        let word = match object.get("word") {
            Some(Value::String(word)) if !word.is_empty() => word.clone(),
            Some(Value::String(_)) => return Err(invalid("has an empty `word`".to_owned())),
            Some(other) => {
                return Err(invalid(format!(
                    "has {} `word`; expected a string",
                    json_type(other)
                )));
            }
            None => return Err(invalid("has no `word`".to_owned())),
        };
        let routing_forms = routing_forms(&object).map_err(invalid)?;
        Ok(Some(SourceRecord {
            line_number,
            byte_offset,
            source_bytes,
            word,
            routing_forms,
        }))
    }
}

fn routing_forms(object: &Map<String, Value>) -> Result<Vec<String>, String> {
    let Some(forms) = object.get("forms") else {
        return Ok(Vec::new());
    };
    let Value::Array(forms) = forms else {
        return Err(format!(
            "has {} `forms`; expected an array",
            json_type(forms)
        ));
    };
    let mut routing_forms = Vec::new();
    for (index, form) in forms.iter().enumerate() {
        let Value::Object(form) = form else {
            return Err(format!(
                "has {} `forms[{index}]`; expected an object",
                json_type(form)
            ));
        };
        let value = match form.get("form") {
            Some(Value::String(value)) => value,
            Some(other) => {
                return Err(format!(
                    "has {} `forms[{index}].form`; expected a string",
                    json_type(other)
                ));
            }
            None => return Err(format!("has no `forms[{index}].form`")),
        };
        if value.is_empty() || value == "-" {
            continue;
        }
        if routing_forms.len() == MAX_ROUTING_FORMS {
            return Err(format!(
                "exceeds the {MAX_ROUTING_FORMS}-routing-form limit"
            ));
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

impl<R: Read> Iterator for JsonlSourceReader<R> {
    type Item = Result<SourceRecord, SourceIngestionError>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.finished {
            let line_number = self.next_line_number;
            let byte_offset = self.next_byte_offset;
            let (source_bytes, discarded, physical_length) = match self.read_line() {
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
            self.next_byte_offset += physical_length;
            self.next_line_number += 1;
            self.source_record_count += 1;

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

/// Only the `lang_code` of a line, read without materializing unselected records.
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

/// A selected line as an object, rejecting duplicate top-level fields.
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{JsonlSourceReader, MAX_ROUTING_FORMS, SourceIngestionError};

    fn reader(input: &[u8]) -> JsonlSourceReader<Cursor<&[u8]>> {
        JsonlSourceReader::new(Cursor::new(input), "en", 1_024)
    }

    fn reason(error: SourceIngestionError) -> String {
        match error {
            SourceIngestionError::Invalid { reason, .. } => reason,
            other => panic!("expected an invalid record, got {other}"),
        }
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
            .unwrap();

        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0].source_bytes,
            br#"{"lang_code":"en","word":"one"}"#
        );
        assert_eq!((records[0].line_number, records[0].byte_offset), (1, 0));
        assert_eq!(
            records[1].source_bytes,
            "{\"lang_code\":\"en\",\"word\":\"三\"}".as_bytes()
        );
        assert_eq!(records[1].line_number, 3);
        assert_eq!(
            records[1].byte_offset,
            input
                .find("{\"lang_code\":\"en\",\"word\":\"三\"}")
                .unwrap() as u64
        );
        assert_eq!(records[1].word, "三");
    }

    #[test]
    fn rejects_empty_lines_and_preserves_a_final_carriage_return() {
        for input in [b"\n".as_slice(), b"\r\n".as_slice()] {
            assert_eq!(
                reason(reader(input).next().unwrap().unwrap_err()),
                "is empty"
            );
        }
        let input = b"{\"lang_code\":\"en\",\"word\":\"x\"}\r";
        assert_eq!(reader(input).next().unwrap().unwrap().source_bytes, input);
    }

    #[test]
    fn rejects_oversized_lines_without_losing_the_next_line() {
        let input =
            b"12345678901234567890123456789012345678901\n{\"lang_code\":\"en\",\"word\":\"ok\"}\n";
        let mut records = JsonlSourceReader::new(Cursor::new(input), "en", 40);
        assert!(matches!(
            records.next().unwrap(),
            Err(SourceIngestionError::LineTooLong { max_bytes: 40, .. })
        ));
        let record = records.next().unwrap().unwrap();
        assert_eq!((record.line_number, record.byte_offset), (2, 42));
        assert!(
            JsonlSourceReader::new(Cursor::new(b"{}\r\n"), "en", 2)
                .next()
                .is_none()
        );
    }

    #[test]
    fn rejects_invalid_utf8_json_non_objects_and_duplicate_fields() {
        assert!(
            reason(
                reader(b"{\"lang_code\":\"en\",\"word\":\"\xff\"}\n")
                    .next()
                    .unwrap()
                    .unwrap_err()
            )
            .starts_with("is not UTF-8")
        );
        assert!(matches!(
            reader(b"{not json}\n").next().unwrap(),
            Err(SourceIngestionError::MalformedJson { .. })
        ));
        assert_eq!(
            reason(reader(b"[]\n").next().unwrap().unwrap_err()),
            "is not a JSON object"
        );
        for input in [
            br#"{"lang_code":"en","word":"one","word":"two"}"#.as_slice(),
            br#"{"unknown":{"large":[1,2,3]},"lang_code":"fr","unknown":null}"#.as_slice(),
        ] {
            let error = reader(input).next().unwrap().unwrap_err();
            assert!(matches!(error, SourceIngestionError::MalformedJson { .. }));
            assert!(error.to_string().contains("duplicate top-level field"));
        }
    }

    #[test]
    fn skips_other_languages_and_requires_a_string_language_and_word() {
        let input = concat!(
            "{\"redirect\":\"target\"}\n",
            "{\"lang_code\":\"fr\",\"word\":false}\n",
            "{\"lang_code\":7,\"word\":\"bad\"}\n"
        );
        assert_eq!(
            reason(reader(input.as_bytes()).next().unwrap().unwrap_err()),
            "has number `lang_code`; expected a string"
        );
        for (input, expected) in [
            (r#"{"lang_code":"en"}"#, "has no `word`"),
            (
                r#"{"lang_code":"en","word":null}"#,
                "has null `word`; expected a string",
            ),
            (r#"{"lang_code":"en","word":""}"#, "has an empty `word`"),
        ] {
            assert_eq!(
                reason(reader(input.as_bytes()).next().unwrap().unwrap_err()),
                expected
            );
        }
    }

    #[test]
    fn extracts_unicode_routing_forms_in_any_field_order() {
        let input = concat!(
            "{\"forms\":[{\"form\":\"cafés\",\"tags\":[\"plural\"]},",
            "{\"form\":\"\"},{\"form\":\"-\"},{\"form\":\"咖啡\"}],",
            "\"word\":\"café\",\"unknown\":true,\"lang_code\":\"en\"}\n"
        );
        let record = reader(input.as_bytes()).next().unwrap().unwrap();
        assert_eq!(record.word, "café");
        assert_eq!(record.routing_forms, ["cafés", "咖啡"]);
        assert_eq!(record.source_bytes, input.trim_end().as_bytes());
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
        let record = JsonlSourceReader::new(Cursor::new(input.as_bytes()), "en", 16_384)
            .next()
            .unwrap()
            .unwrap();
        assert_eq!((record.line_number, record.word.as_str()), (2, "selected"));
    }

    #[test]
    fn rejects_incompatible_forms_shapes_and_bounds_routing_forms() {
        for (forms, expected) in [
            (r#""bad""#, "has string `forms`; expected an array"),
            (r#"["bad"]"#, "has string `forms[0]`; expected an object"),
            (r"[{}]", "has no `forms[0].form`"),
            (
                r#"[{"form":3}]"#,
                "has number `forms[0].form`; expected a string",
            ),
        ] {
            let input = format!(r#"{{"lang_code":"en","word":"x","forms":{forms}}}"#);
            assert_eq!(
                reason(reader(input.as_bytes()).next().unwrap().unwrap_err()),
                expected
            );
        }
        let forms = (0..=MAX_ROUTING_FORMS)
            .map(|index| format!("{{\"form\":\"f{index}\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        let input = format!(r#"{{"lang_code":"en","word":"x","forms":[{forms}]}}"#);
        let error = JsonlSourceReader::new(Cursor::new(input.as_bytes()), "en", 1 << 20)
            .next()
            .unwrap()
            .unwrap_err();
        assert!(reason(error).contains("routing-form limit"));
    }
}
