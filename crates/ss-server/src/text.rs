//! Name rules shared by display, channel, role, group-DM, and webhook names.
//!
//! A name is trimmed, NFC-normalized, and must then be 1 to `max` Unicode
//! scalar values with no hidden characters: controls, format characters
//! (bidi marks, overrides, isolates, tags), line and paragraph separators,
//! private-use and unassigned code points, and every default-ignorable code
//! point (zero-width spaces, fillers, variation selectors). Those are how one
//! name is made to render like another, or to reorder the text around it.
//! The zero-width joiner and U+FE0E/U+FE0F stay allowed because emoji need
//! them. Message bodies are stored as sent.

use icu_normalizer::ComposingNormalizerBorrowed;
use icu_properties::{
    CodePointMapData, CodePointSetData,
    props::{DefaultIgnorableCodePoint, GeneralCategory},
};

/// Longest channel topic, in Unicode scalar values.
pub const MAX_TOPIC: usize = 1024;

/// Why [`name`] refused a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// Empty after trimming, or longer than the cap.
    #[error("name has the wrong length")]
    Length,
    /// Contains a control or invisible formatting character.
    #[error("name contains a hidden or control character")]
    Hidden,
}

/// The stored form of `raw`: trimmed and NFC. See the module docs.
pub fn name(raw: &str, max: usize) -> Result<String, NameError> {
    let normalized = nfc(raw.trim());
    let len = normalized.chars().count();
    if len == 0 || len > max {
        return Err(NameError::Length);
    }
    if normalized.chars().any(hidden) {
        return Err(NameError::Hidden);
    }
    Ok(normalized)
}

/// `text` in Unicode Normalization Form C.
#[must_use]
pub fn nfc(text: &str) -> String {
    ComposingNormalizerBorrowed::new_nfc()
        .normalize(text)
        .into_owned()
}

/// Characters that render as nothing, or reorder or restyle their
/// neighbours, decided by Unicode property rather than a hand-kept list:
/// general categories Cc (control), Cf (format), Zl and Zp (line and
/// paragraph separators), Co (private use, whose glyph is whatever a font
/// says) and Cn (unassigned, which a later Unicode version may make a format
/// character), plus every `Default_Ignorable_Code_Point`. The zero-width
/// joiner and the text/emoji presentation selectors U+FE0E and U+FE0F are
/// the only exceptions, because emoji sequences need them.
fn hidden(ch: char) -> bool {
    if matches!(ch, '\u{200d}' | '\u{fe0e}' | '\u{fe0f}') {
        return false;
    }
    let category = CodePointMapData::<GeneralCategory>::new().get(ch);
    matches!(
        category,
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
    ) || CodePointSetData::new::<DefaultIgnorableCodePoint>().contains(ch)
}

#[cfg(test)]
mod tests {
    use super::{NameError, name};

    #[test]
    fn names_are_trimmed_and_composed() {
        assert_eq!(name("  general  ", 64).unwrap(), "general");
        // "e" + COMBINING ACUTE ACCENT becomes one scalar value.
        let composed = name("cafe\u{301}", 64).unwrap();
        assert_eq!(composed, "caf\u{e9}");
        assert_eq!(composed, name("caf\u{e9}", 64).unwrap());
        // The cap counts the composed form.
        assert!(name(&"e\u{301}".repeat(4), 4).is_ok());
    }

    #[test]
    fn empty_long_and_hidden_names_are_rejected() {
        assert_eq!(name("   ", 64), Err(NameError::Length));
        assert_eq!(name(&"a".repeat(65), 64), Err(NameError::Length));
        for bad in [
            "ad\u{202e}nimda",
            "zero\u{200b}width",
            "non\u{200c}joiner",
            "mark\u{200f}",
            "bom\u{feff}",
            "tab\there",
            "line\nbreak",
            "iso\u{2066}late",
            "filler\u{3164}",
        ] {
            assert_eq!(name(bad, 64), Err(NameError::Hidden), "{bad:?}");
        }
        // Outside the old hand-written list: Mongolian vowel separator,
        // combining grapheme joiner, isolates, tag characters, other
        // variation selectors, separators, private use, unassigned.
        for bad in [
            "a\u{180e}b",
            "a\u{034f}b",
            "\u{2066}admin\u{2069}",
            "\u{2068}x\u{2069}",
            "tag\u{e0001}",
            "tag\u{e0041}\u{e007f}",
            "vs\u{fe00}",
            "vs\u{e0100}",
            "line\u{2028}sep",
            "para\u{2029}sep",
            "soft\u{ad}hyphen",
            "private\u{e000}",
            "unassigned\u{378}",
            "interlinear\u{fff9}",
            "\u{1d173}music",
        ] {
            assert_eq!(name(bad, 64), Err(NameError::Hidden), "{bad:?}");
        }
        assert!(name("Ünïcödé ✓ 名前", 64).is_ok());
        // Emoji keep their joiners and presentation selectors.
        for emoji in [
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
            "\u{2764}\u{fe0f}",
            "\u{2764}\u{fe0e}",
            "1\u{fe0f}\u{20e3}",
            "\u{1f44b}\u{1f3fd}",
            "\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}",
            "\u{1f1f3}\u{1f1f4}",
        ] {
            let named = format!("Ada {emoji}");
            assert_eq!(name(&named, 64).as_deref(), Ok(named.as_str()), "{emoji:?}");
        }
    }
}
