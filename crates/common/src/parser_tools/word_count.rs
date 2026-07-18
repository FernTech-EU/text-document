//! Pure word/character counting over `&str` — no document, no store, no threads.
//!
//! Mirrors [`djot_to_plain_text`]
//! and the search matcher's shape: a cheap primitive over `&str` with the *policy*
//! (which counting method) passed in as a parameter, so a host app can count a manuscript
//! of thousands of scenes without importing each one into a document.
//!
//! Language is **not** a parameter: UAX #29 word segmentation is not locale-tailored the
//! way case-folding is, and the CJK rule ([`CountMethod::CjkHybrid`]) is script-detected
//! per character. A caller that wants "count this Chinese scene per character" selects
//! `CjkHybrid`; it does not pass a language tag.

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use crate::parser_tools::content_parser::djot_to_plain_text;
use crate::parser_tools::djot_options::DjotImportOptions;

/// How words are delimited. Characters are always counted the same way (Unicode scalar
/// values), independent of this choice — see [`WordCharCounts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CountMethod {
    /// `str::split_whitespace` — fast, parity mode (e.g. matching another tool's count).
    /// Miscounts scripts that are not space-delimited (CJK) and is crude around
    /// punctuation, but it is exactly what many word processors report.
    WhitespaceSplit,
    /// UAX #29 word segmentation via `unicode_words` — the sound general-purpose default:
    /// apostrophes glue (`"Elena's"` is one word), hyphens split, punctuation-only runs
    /// are excluded. Still undercounts CJK (a run of ideographs may segment as one word).
    #[default]
    UnicodeWords,
    /// UAX #29 for alphabetic scripts, but every Han / Hiragana / Katakana character counts
    /// as one word — the East-Asian convention, where a "word count" of non-space-delimited
    /// prose approximates a character count. Applied per character (not per detected script
    /// run) so it is correct whether the segmenter split a run per-character (Han) or glued
    /// it (Katakana, UAX #29 rule WB13). Korean (Hangul) is space-delimited and stays on the
    /// [`UnicodeWords`](CountMethod::UnicodeWords) rule.
    CjkHybrid,
}

/// The three counts a caller might display. All three are always computed — "characters
/// with spaces" vs "without" is a display choice, not a separate counting mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WordCharCounts {
    pub words: usize,
    pub chars_with_spaces: usize,
    pub chars_without_spaces: usize,
}

/// Count words and characters in already-extracted plain text.
pub fn count(text: &str, method: CountMethod) -> WordCharCounts {
    let chars_with_spaces = text.chars().count();
    let chars_without_spaces = text.chars().filter(|c| !c.is_whitespace()).count();
    let words = match method {
        CountMethod::WhitespaceSplit => text.split_whitespace().count(),
        CountMethod::UnicodeWords => text.unicode_words().count(),
        CountMethod::CjkHybrid => {
            // Each Han/Kana character is one word; every UAX #29 word that contains no
            // Han/Kana is one word. A mixed "Hello 世界" → "Hello" (1) + 世 + 界 = 3.
            let cjk = text.chars().filter(|&c| is_han_or_kana(c)).count();
            let non_cjk_words = text
                .unicode_words()
                .filter(|w| !w.chars().any(is_han_or_kana))
                .count();
            cjk + non_cjk_words
        }
    };
    WordCharCounts {
        words,
        chars_with_spaces,
        chars_without_spaces,
    }
}

/// Extract prose from Djot source, then count. The two-step (strip then count) is kept
/// deliberately simple: `djot_to_plain_text` carries a pinned contract (table-anchor
/// sentinel, single-`\n` block joins) and fusing the parse-and-count into one AST walk is
/// the easiest way to accidentally create a third, silently-diverging "what is the text".
pub fn count_djot(djot: &str, method: CountMethod) -> WordCharCounts {
    count(
        &djot_to_plain_text(djot, &DjotImportOptions::default()),
        method,
    )
}

/// Han ideographs (incl. common extensions & compatibility) and Japanese kana. Excludes
/// Hangul: Korean is space-delimited and counts by the UAX #29 word rule.
fn is_han_or_kana(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF     // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF   // CJK Unified Ideographs
        | 0xF900..=0xFAFF   // CJK Compatibility Ideographs
        | 0x20000..=0x3FFFF // Supplementary + Tertiary Ideographic Planes (all CJK Ext B–I)
        | 0x3040..=0x309F   // Hiragana
        | 0x30A0..=0x30FF   // Katakana
        | 0x31F0..=0x31FF   // Katakana Phonetic Extensions
        | 0xFF66..=0xFF9D   // Halfwidth Katakana
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_whitespace_only() {
        for m in [
            CountMethod::WhitespaceSplit,
            CountMethod::UnicodeWords,
            CountMethod::CjkHybrid,
        ] {
            assert_eq!(count("", m).words, 0);
            assert_eq!(count("   \n\t ", m).words, 0);
        }
        let c = count("   \n\t ", CountMethod::UnicodeWords);
        assert_eq!(c.chars_with_spaces, 6);
        assert_eq!(c.chars_without_spaces, 0);
    }

    #[test]
    fn punctuation_only_is_zero_words_for_unicode_but_not_whitespace() {
        // `unicode_words` drops punctuation-only runs; `split_whitespace` counts the token.
        assert_eq!(count("-- ... !!", CountMethod::UnicodeWords).words, 0);
        assert_eq!(count("-- ... !!", CountMethod::WhitespaceSplit).words, 3);
    }

    #[test]
    fn apostrophes_glue_and_hyphens_split_under_unicode() {
        assert_eq!(count("Elena's", CountMethod::UnicodeWords).words, 1);
        // "Jean-Luc" is two UAX #29 words (the hyphen is a boundary).
        assert_eq!(count("Jean-Luc", CountMethod::UnicodeWords).words, 2);
        // Whitespace split sees each as one token.
        assert_eq!(count("Elena's", CountMethod::WhitespaceSplit).words, 1);
        assert_eq!(count("Jean-Luc", CountMethod::WhitespaceSplit).words, 1);
    }

    #[test]
    fn char_counts_ignore_method() {
        let text = "one two";
        for m in [
            CountMethod::WhitespaceSplit,
            CountMethod::UnicodeWords,
            CountMethod::CjkHybrid,
        ] {
            let c = count(text, m);
            assert_eq!(c.chars_with_spaces, 7);
            assert_eq!(c.chars_without_spaces, 6);
        }
    }

    #[test]
    fn cjk_hybrid_counts_han_per_character() {
        // Four Han ideographs → four words under CjkHybrid.
        assert_eq!(count("春眠不覺", CountMethod::CjkHybrid).words, 4);
        // Under plain Unicode words the run may be one (or few) segments — always ≤ 4.
        assert!(count("春眠不覺", CountMethod::UnicodeWords).words <= 4);
    }

    #[test]
    fn cjk_hybrid_counts_katakana_run_per_character() {
        // A glued Katakana run (UAX #29 WB13) is still counted per character.
        assert_eq!(count("カタカナ", CountMethod::CjkHybrid).words, 4);
    }

    #[test]
    fn cjk_hybrid_mixes_latin_words_and_cjk_chars() {
        // "Hello" (1 word) + 世 + 界 (2 chars) = 3.
        assert_eq!(count("Hello 世界", CountMethod::CjkHybrid).words, 3);
    }

    #[test]
    fn cjk_hybrid_hiragana_per_character_hangul_by_word() {
        assert_eq!(count("ひらがな", CountMethod::CjkHybrid).words, 4);
        // Hangul is space-delimited: two space-separated Korean words stay two words.
        assert_eq!(count("한국어 낱말", CountMethod::CjkHybrid).words, 2);
    }

    #[test]
    fn count_djot_matches_count_over_extracted_plain_text() {
        let djot = "# Title\n\nA *bold* word and some prose.";
        let plain = djot_to_plain_text(djot, &DjotImportOptions::default());
        for m in [
            CountMethod::WhitespaceSplit,
            CountMethod::UnicodeWords,
            CountMethod::CjkHybrid,
        ] {
            assert_eq!(count_djot(djot, m), count(&plain, m));
        }
    }
}
