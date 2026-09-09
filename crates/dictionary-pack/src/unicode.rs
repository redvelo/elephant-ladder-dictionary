use std::borrow::Cow;

use icu_casemap::CaseMapper;
use icu_normalizer::ComposingNormalizerBorrowed;

/// The exact implementation profile included in every pack revision.
pub const UNICODE_PROFILE: &str = "icu4x-2.3.0-nfc-full-default-case-fold-nfc";

fn normalize_nfc(value: &str) -> Cow<'_, str> {
    ComposingNormalizerBorrowed::new_nfc().normalize(value)
}

fn fold_nfc(normalized: &str) -> String {
    match CaseMapper::new().fold_string(normalized) {
        Cow::Borrowed(folded) => normalize_nfc(folded).into_owned(),
        Cow::Owned(folded) => match normalize_nfc(&folded) {
            Cow::Borrowed(_) => folded,
            Cow::Owned(normalized) => normalized,
        },
    }
}

/// Authored and derived binary lookup keys for one source value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LookupKeys {
    pub authored: Vec<u8>,
    pub nfc: Vec<u8>,
    pub folded: Vec<u8>,
}

/// Returns the canonical NFC representation without compatibility normalization.
#[must_use]
pub fn nfc_key(value: &str) -> String {
    normalize_nfc(value).into_owned()
}

/// Returns NFC, full locale-independent default case folding, then NFC.
#[must_use]
pub fn folded_key(value: &str) -> String {
    fold_nfc(&normalize_nfc(value))
}

/// Derives the three headword or form keys stored with binary `SQLite` semantics.
#[must_use]
pub fn lookup_keys(value: &str) -> LookupKeys {
    let normalized = normalize_nfc(value);
    let folded = fold_nfc(&normalized).into_bytes();

    LookupKeys {
        authored: value.as_bytes().to_vec(),
        nfc: normalized.into_owned().into_bytes(),
        folded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfc_matches_canonically_equivalent_accents() {
        assert_eq!(nfc_key("cafe\u{301}"), "café");
    }

    #[test]
    fn full_case_folding_handles_expansion_and_final_sigma() {
        assert_eq!(folded_key("Straße"), "strasse");
        assert_eq!(folded_key("ΟΣ"), folded_key("ος"));
    }

    #[test]
    fn combined_lookup_matches_standalone_keys_for_representative_unicode() {
        let cases = [
            ("Café", "Café", "café"),
            ("Cafe\u{301}", "Café", "café"),
            ("Straße", "Straße", "strasse"),
            ("ΟΣ ος", "ΟΣ ος", "οσ οσ"),
            ("\u{1e96}", "\u{1e96}", "\u{1e96}"),
            ("\u{390}", "\u{390}", "\u{390}"),
        ];

        for (authored, expected_nfc, expected_folded) in cases {
            let keys = lookup_keys(authored);

            assert_eq!(keys.authored, authored.as_bytes());
            assert_eq!(keys.nfc, expected_nfc.as_bytes());
            assert_eq!(keys.folded, expected_folded.as_bytes());
            assert_eq!(keys.nfc, nfc_key(authored).into_bytes());
            assert_eq!(keys.folded, folded_key(authored).into_bytes());
        }
    }

    #[test]
    fn matching_does_not_apply_compatibility_or_whitespace_rules() {
        assert_ne!(nfc_key("ﬁ"), nfc_key("fi"));
        assert_ne!(folded_key("two  words"), folded_key("two words"));
        assert_ne!(folded_key("résumé"), folded_key("resume"));
    }

    #[test]
    fn matching_is_not_turkic_locale_specific() {
        assert_eq!(folded_key("I"), "i");
        assert_ne!(folded_key("I"), "ı");
    }
}
