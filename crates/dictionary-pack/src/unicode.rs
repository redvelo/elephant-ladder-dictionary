use std::borrow::Cow;

use icu_casemap::CaseMapper;
use icu_normalizer::ComposingNormalizerBorrowed;

/// The exact implementation profile included in every pack revision.
pub const UNICODE_PROFILE: &str = "icu4x-2.3.0-nfc-full-default-case-fold-nfc";

/// Authored and derived binary lookup keys for one source value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LookupKeys {
    pub authored: Vec<u8>,
    /// Canonical NFC without compatibility normalization.
    pub nfc: Vec<u8>,
    /// NFC, full locale-independent default case folding, then NFC.
    pub folded: Vec<u8>,
}

/// Derives the three headword or form keys stored with binary `SQLite` semantics.
#[must_use]
pub fn lookup_keys(value: &str) -> LookupKeys {
    let normalized = ComposingNormalizerBorrowed::new_nfc().normalize(value);
    let folded = match CaseMapper::new().fold_string(&normalized) {
        Cow::Borrowed(folded) => ComposingNormalizerBorrowed::new_nfc()
            .normalize(folded)
            .into_owned(),
        Cow::Owned(folded) => match ComposingNormalizerBorrowed::new_nfc().normalize(&folded) {
            Cow::Borrowed(_) => folded,
            Cow::Owned(normalized) => normalized,
        },
    };
    LookupKeys {
        authored: value.as_bytes().to_vec(),
        nfc: normalized.into_owned().into_bytes(),
        folded: folded.into_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nfc(value: &str) -> String {
        String::from_utf8(lookup_keys(value).nfc).unwrap()
    }

    fn folded(value: &str) -> String {
        String::from_utf8(lookup_keys(value).folded).unwrap()
    }

    #[test]
    fn keys_cover_representative_unicode() {
        for (authored, expected_nfc, expected_folded) in [
            ("Café", "Café", "café"),
            ("Cafe\u{301}", "Café", "café"),
            ("Straße", "Straße", "strasse"),
            ("ΟΣ ος", "ΟΣ ος", "οσ οσ"),
            ("\u{1e96}", "\u{1e96}", "\u{1e96}"),
            ("\u{390}", "\u{390}", "\u{390}"),
        ] {
            let keys = lookup_keys(authored);
            assert_eq!(keys.authored, authored.as_bytes());
            assert_eq!(keys.nfc, expected_nfc.as_bytes());
            assert_eq!(keys.folded, expected_folded.as_bytes());
        }
        assert_eq!(folded("ΟΣ"), folded("ος"));
    }

    #[test]
    fn matching_does_not_apply_compatibility_whitespace_or_locale_rules() {
        assert_ne!(nfc("ﬁ"), nfc("fi"));
        assert_ne!(folded("two  words"), folded("two words"));
        assert_ne!(folded("résumé"), folded("resume"));
        assert_eq!(folded("I"), "i");
        assert_ne!(folded("I"), "ı");
    }
}
