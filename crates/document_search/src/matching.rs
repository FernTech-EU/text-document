//! The one definition of "a match".
//!
//! Everything that asks "does this text contain that query" goes through here: the
//! in-document find, find-and-replace, and (through the public API) a host app's own
//! project-wide search. Two implementations would drift, and the way a writer meets that
//! drift is "the editor found it but the search panel didn't".
//!
//! ## The rule this module exists to enforce
//!
//! **An offset computed in folded text is not valid in the original text.** Case-folding
//! can change a string's *length* — `'İ'.to_lowercase()` is two chars — so a position
//! found in a lowercased haystack lands somewhere else entirely when applied to the
//! source.
//!
//! That was not hypothetical. The literal search path used to lowercase the haystack,
//! compute char offsets *in the lowercased copy*, and hand them back to a caller that
//! applied them to the original. On `"İİİ ipsum dolor"`, `find_all("ipsum")` reported
//! position 7 instead of 4, and `replace_text` — running with the default,
//! case-insensitive options — turned the prose into `"İİİ ipsLOREMlor"`. It did not
//! merely highlight the wrong word; it *destroyed* the writer's text, and it would do so
//! in any manuscript containing a Turkish capital İ.
//!
//! So folding here always produces an **index map** alongside the folded string, and
//! matches are always reported in ORIGINAL char offsets.

use std::cmp::Ordering;

use unicode_segmentation::UnicodeSegmentation;

/// How to match. (Diacritic folding and per-language tailoring join this later; the
/// shape is deliberately a struct so adding one is not a signature break at every call
/// site.)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatchOptions {
    /// `false` (the default) folds case — which is why the offset map above is not
    /// optional.
    pub case_sensitive: bool,
    /// Only match when the occurrence begins and ends on a word boundary.
    pub whole_word: bool,
}

/// One occurrence, in **char offsets into the original haystack** — never into the
/// folded copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub char_start: usize,
    pub char_len: usize,
}

/// Every occurrence of `needle` in `haystack`, in original char offsets.
pub fn find_all(haystack: &str, needle: &str, options: &MatchOptions) -> Vec<Match> {
    if needle.is_empty() || haystack.is_empty() {
        return Vec::new();
    }
    let folded_needle = fold(needle, options.case_sensitive);
    if folded_needle.is_empty() {
        return Vec::new();
    }

    let folded = Folded::new(haystack, options.case_sensitive);
    let hits = folded.matches(&folded_needle);
    if !options.whole_word {
        return hits;
    }

    // Boundaries are computed on the ORIGINAL text, so they compose directly with the
    // original offsets the matcher reports — no second index map needed.
    let boundaries = word_boundaries(haystack);
    hits.into_iter()
        .filter(|m| {
            is_boundary(&boundaries, m.char_start)
                && is_boundary(&boundaries, m.char_start + m.char_len)
        })
        .collect()
}

/// Fold a string for matching. Kept as one function so a query and a haystack can never
/// be folded by different rules.
fn fold(text: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        text.to_string()
    } else {
        text.to_lowercase()
    }
}

/// A folded haystack, plus the map back to where each folded char came from.
struct Folded {
    text: String,
    /// `origin[i]` is the ORIGINAL char index that produced folded char `i`. One extra
    /// trailing entry (the original char count) so an *end* offset maps too.
    origin: Vec<u32>,
    /// Byte offset of each folded char, so a byte match can be turned into a folded char
    /// index without building a byte-indexed map of the whole haystack.
    byte_of_char: Vec<u32>,
}

impl Folded {
    fn new(text: &str, case_sensitive: bool) -> Self {
        let mut folded = String::with_capacity(text.len());
        let mut origin = Vec::with_capacity(text.len());
        let mut byte_of_char = Vec::with_capacity(text.len());

        for (char_idx, ch) in text.chars().enumerate() {
            let char_idx = char_idx as u32;
            if case_sensitive {
                byte_of_char.push(folded.len() as u32);
                origin.push(char_idx);
                folded.push(ch);
            } else {
                // One source char may fold to several — each of them maps back to the
                // single char it came from. This is the whole point.
                for lowered in ch.to_lowercase() {
                    byte_of_char.push(folded.len() as u32);
                    origin.push(char_idx);
                    folded.push(lowered);
                }
            }
        }
        // Sentinels, so a match ending at the very end of the text still maps.
        byte_of_char.push(folded.len() as u32);
        origin.push(text.chars().count() as u32);

        Self {
            text: folded,
            origin,
            byte_of_char,
        }
    }

    fn matches(&self, folded_needle: &str) -> Vec<Match> {
        let needle_chars = folded_needle.chars().count();
        // `match_indices` is the two-way algorithm — the literal scan it replaced was an
        // O(n*m) char-window byte-slice compare, re-sliced on every character.
        self.text
            .match_indices(folded_needle)
            .filter_map(|(byte, _)| {
                // Byte -> folded char index by binary search over the per-char byte
                // offsets, rather than a byte-indexed map allocated for the whole
                // haystack (8 bytes per *byte* of text — megabytes on a novel).
                let start = self.byte_of_char.binary_search(&(byte as u32)).ok()?;
                let end = start + needle_chars;
                let from = *self.origin.get(start)?;
                let to = *self.origin.get(end)?;
                Some(Match {
                    char_start: from as usize,
                    char_len: to.saturating_sub(from) as usize,
                })
            })
            .collect()
    }
}

/// Apostrophes that sit *inside* a word under UAX#29 but that a writer means as a word
/// edge: ASCII `'`, the typographic `’`, and the modifier letter `ʼ`.
fn is_apostrophe(ch: char) -> bool {
    matches!(ch, '\u{0027}' | '\u{2019}' | '\u{02BC}')
}

/// Char indices that are word boundaries, sorted and deduplicated.
///
/// # The apostrophe
///
/// UAX#29 glues an apostrophe *into* a word: `Elena's` is one word, `d'Aurélien` is one
/// word. So a whole-word search for `Elena` silently missed `Elena's`, and `Aurélien`
/// missed `d'Aurélien`. In a *search* that is an annoyance; in **Replace All** it is a
/// half-renamed manuscript — and a missing match is invisible, where a spurious one is
/// merely skippable.
///
/// One rule fixes English possessives and Romance/Celtic/Dutch/Greek elision at once:
/// **an apostrophe is a boundary on both sides.** The accepted cost is that whole-word
/// `don` now also matches inside `don't`. That is a visible false positive, and a
/// visible false positive beats an invisible false negative that corrupts prose.
///
/// Deliberately *not* generalised to "any non-letter is a boundary": the hyphen already
/// breaks under UAX#29 (`Jean-Luc` needs nothing), and Catalan `col·lecció` correctly
/// stays one word because U+00B7 is `MidLetter` — a blanket rule would *introduce* a
/// Catalan bug to fix an English one.
pub(crate) fn word_boundaries(text: &str) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    out.push(0);

    // One pass. The previous version called `text[..byte_start].chars().count()` for
    // every word — quadratic in the length of the text, on a document that can be a
    // whole novel.
    let mut words = text.unicode_word_indices().peekable();
    let mut char_idx: u32 = 0;

    for (byte_idx, ch) in text.char_indices() {
        while let Some(&(word_byte, word)) = words.peek() {
            match word_byte.cmp(&byte_idx) {
                Ordering::Less => {
                    words.next();
                }
                Ordering::Equal => {
                    out.push(char_idx);
                    out.push(char_idx + word.chars().count() as u32);
                    words.next();
                }
                Ordering::Greater => break,
            }
        }
        if is_apostrophe(ch) {
            out.push(char_idx);
            out.push(char_idx + 1);
        }
        char_idx += 1;
    }
    out.push(char_idx);

    out.sort_unstable();
    out.dedup();
    out
}

/// A sorted `Vec<u32>` + binary search, not a `HashSet<usize>`: half the memory, and
/// contiguous rather than pointer-chasing on every candidate match.
pub(crate) fn is_boundary(boundaries: &[u32], char_idx: usize) -> bool {
    boundaries.binary_search(&(char_idx as u32)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(case_sensitive: bool, whole_word: bool) -> MatchOptions {
        MatchOptions {
            case_sensitive,
            whole_word,
        }
    }
    /// The matched text, sliced out of the ORIGINAL by the reported offsets. If the
    /// offsets are wrong, this is wrong — which is the entire failure mode.
    fn matched(haystack: &str, m: Match) -> String {
        haystack
            .chars()
            .skip(m.char_start)
            .take(m.char_len)
            .collect()
    }

    /// The bug this module exists for.
    ///
    /// `İ` lowercases to TWO chars, so offsets found in the lowercased haystack drift.
    /// `find_all` used to report 7 instead of 4 here, and the replace built on it turned
    /// `"İİİ ipsum dolor"` into `"İİİ ipsLOREMlor"` — silent corruption of a writer's
    /// prose, under the DEFAULT (case-insensitive) options.
    #[test]
    fn an_offset_from_folded_text_is_reported_in_the_source() {
        let text = "İİİ ipsum dolor";
        assert!(
            text.to_lowercase().chars().count() > text.chars().count(),
            "this fixture must actually grow when lowercased, or it proves nothing"
        );

        let hits = find_all(text, "ipsum", &opts(false, false));
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].char_start, 4,
            "the offset must be valid in the SOURCE"
        );
        assert_eq!(matched(text, hits[0]), "ipsum");
    }

    /// Whole-word `Elena` must find `Elena's`. UAX#29 glues the apostrophe into the
    /// word, so this used to miss — and a missed match in Replace All is a half-renamed
    /// manuscript.
    #[test]
    fn whole_word_finds_an_english_possessive() {
        let text = "Elena's coat was Elena's alone.";
        let hits = find_all(text, "Elena", &opts(false, true));
        assert_eq!(hits.len(), 2, "both possessives must be found");
        for h in &hits {
            assert_eq!(matched(text, *h), "Elena");
        }
    }

    /// …and the same one rule covers Romance elision: `d'Aurélien` contains `Aurélien`.
    #[test]
    fn whole_word_finds_a_word_after_an_elision() {
        for text in [
            "la promesse d'Aurélien",
            "la promesse d\u{2019}Aurélien", // typographic apostrophe
        ] {
            let hits = find_all(text, "Aurélien", &opts(false, true));
            assert_eq!(hits.len(), 1, "elided {text:?} must still match");
            assert_eq!(matched(text, hits[0]), "Aurélien");
        }
    }

    /// The accepted cost of the apostrophe rule, stated out loud: `don` matches inside
    /// `don't`. A visible false positive the writer can skip beats an invisible false
    /// negative that silently half-renames their book.
    #[test]
    fn the_apostrophe_rule_admits_a_known_false_positive() {
        let hits = find_all("I don't know", "don", &opts(false, true));
        assert_eq!(
            hits.len(),
            1,
            "documented trade-off: `don` matches the `don` of `don't`"
        );
    }

    /// Whole-word must still refuse a real substring.
    #[test]
    fn whole_word_still_rejects_a_substring() {
        assert!(
            find_all("le marbre", "arbre", &opts(false, true)).is_empty(),
            "`arbre` must not match inside `marbre`"
        );
        assert_eq!(find_all("un arbre", "arbre", &opts(false, true)).len(), 1);
    }

    /// Catalan `col·lecció` is ONE word (U+00B7 is deliberately MidLetter). A blanket
    /// "any non-letter is a boundary" rule would have broken this to fix the apostrophe.
    #[test]
    fn a_catalan_middle_dot_does_not_split_a_word() {
        assert!(
            find_all("una col·lecció", "lecció", &opts(false, true)).is_empty(),
            "the middle dot must not become a word boundary"
        );
    }

    #[test]
    fn case_sensitivity_is_honoured() {
        assert!(find_all("Ipsum", "ipsum", &opts(true, false)).is_empty());
        assert_eq!(find_all("Ipsum", "Ipsum", &opts(true, false)).len(), 1);
        assert_eq!(find_all("Ipsum", "ipsum", &opts(false, false)).len(), 1);
    }

    #[test]
    fn overlapping_and_repeated_occurrences() {
        let text = "aaaa";
        let hits = find_all(text, "aa", &opts(false, false));
        // `match_indices` reports non-overlapping matches, which is what a replace needs.
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].char_start, 0);
        assert_eq!(hits[1].char_start, 2);
    }

    #[test]
    fn an_empty_query_or_haystack_matches_nothing() {
        assert!(find_all("anything", "", &opts(false, false)).is_empty());
        assert!(find_all("", "anything", &opts(false, false)).is_empty());
    }

    /// Multi-byte text: offsets are CHARS, and must survive a fold that changes length.
    #[test]
    fn offsets_are_chars_and_survive_multibyte_text() {
        let text = "café au lait, CAFÉ noir";
        let hits = find_all(text, "café", &opts(false, false));
        assert_eq!(hits.len(), 2);
        assert_eq!(matched(text, hits[0]), "café");
        assert_eq!(
            matched(text, hits[1]),
            "CAFÉ",
            "the second is the uppercase one"
        );
    }
}
