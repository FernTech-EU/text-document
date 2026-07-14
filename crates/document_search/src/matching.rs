//! The one definition of "a match".
//!
//! Everything that asks "does this text contain that query" goes through here: the
//! in-document find, find-and-replace, and (through the public API) a host app's own
//! project-wide search. Two implementations would drift, and the way a writer meets that
//! drift is "the editor found it but the search panel didn't".
//!
//! ## The rule this module exists to enforce
//!
//! **An offset computed in folded text is not valid in the original text.** Folding can
//! change a string's *length* — `'İ'.to_lowercase()` is two chars, `ß` folds to `ss`, `é`
//! folds to `e` — so a position found in a folded haystack lands somewhere else entirely
//! when applied to the source.
//!
//! That was not hypothetical. The literal search path used to lowercase the haystack,
//! compute char offsets *in the lowercased copy*, and hand them back to a caller that
//! applied them to the original. On `"İİİ ipsum dolor"`, `find_all("ipsum")` reported
//! position 7 instead of 4, and `replace_text` — running with the default, case-insensitive
//! options — turned the prose into `"İİİ ipsLOREMlor"`. It did not merely highlight the
//! wrong word; it *destroyed* the writer's text, and it would do so in any manuscript
//! containing a Turkish capital İ.
//!
//! So folding here always produces an **index map** alongside the folded string, and matches
//! are always reported in ORIGINAL char offsets.
//!
//! ## And a match must not begin or end inside a letter
//!
//! Because one source char can fold to several ([`Folded`]), a substring of the folded text
//! is not necessarily a substring of anything the writer typed. `Straße` folds to
//! `strasse`, so a naive scan finds `s` in the middle of the `ß`. There is no source range
//! to report for half a letter, and replacing it would produce mojibake. Every match is
//! therefore checked against the index map and **rejected unless it begins and ends on a
//! source-char boundary** — see [`Folded::to_source_match`].

use std::cmp::Ordering;
use std::sync::OnceLock;

use unicode_segmentation::UnicodeSegmentation;

use crate::folding::{self};

pub use crate::folding::{FoldLocale, FoldSpec};

/// How to match.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct MatchOptions {
    /// `false` (the default) folds case — which is why the index map is not optional.
    pub case_sensitive: bool,
    /// `false` (the default) folds diacritics, ligatures and Arabic orthography, so that
    /// `aurelien` finds `Aurélien` and `احمد` finds `أَحْمَد`.
    pub diacritic_sensitive: bool,
    /// Only match when the occurrence begins and ends on a word boundary.
    pub whole_word: bool,
    /// The language of the text being searched — **per scene**, not per search. One pass
    /// can fold a French chapter and a Turkish one under different rules; the user's
    /// toggles above stay global, and the language only decides what folding *means*.
    pub locale: FoldLocale,
}

impl MatchOptions {
    /// The subset of these options that decides how text **folds**.
    ///
    /// `whole_word` is deliberately *not* part of it: it decides which matches survive, not
    /// how a single character folds. Keeping that distinction in the types is what lets
    /// [`FoldedText`] answer both kinds of query from one fold — so a host app caching the
    /// fold of a whole manuscript does not throw it away when the writer ticks a checkbox.
    pub fn fold_spec(&self) -> FoldSpec {
        FoldSpec {
            case_sensitive: self.case_sensitive,
            diacritic_sensitive: self.diacritic_sensitive,
            locale: self.locale,
        }
    }
}

/// One occurrence, in **char offsets into the original haystack** — never into the folded
/// copy.
///
/// The span covers every source char the match consumed, *including ones the fold elided*: a
/// query `cafe` matching a decomposed `café` reports 5 chars, not 4, so that replacing it
/// cannot leave an orphaned combining accent floating on the replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub char_start: usize,
    pub char_len: usize,
}

/// Every occurrence of `needle` in `haystack`, in original char offsets.
///
/// Folds the haystack, searches it, and throws the fold away. For a **single** search that is
/// exactly right. For a search box — where the same corpus is searched again on every
/// keystroke — see [`FoldedText`], which keeps the fold.
pub fn find_all(haystack: &str, needle: &str, options: &MatchOptions) -> Vec<Match> {
    FoldedText::new(haystack, &options.fold_spec()).find_all(needle, options.whole_word)
}

/// A haystack folded **once**, ready to be searched many times.
///
/// ## Why this exists
///
/// A project-wide search box re-searches the *same* prose on every keystroke, and folding it
/// is not free. Measured over a 300k-word manuscript: the fold costs about the same as
/// *parsing* the prose in the first place, and roughly twice what scanning it does — and it
/// was being rebuilt from scratch for every character the writer typed. A host app holds one
/// of these per scene and pays that cost once.
///
/// ## What it is keyed by, and what it is not
///
/// It is built from a [`FoldSpec`] — case, diacritics, language — because those are what
/// decide how a character folds. **`whole_word` is not one of them**: it decides which matches
/// survive, not how the text folds. So it is a parameter of [`find_all`](Self::find_all), not
/// of the fold, and one `FoldedText` answers both kinds of query. That is not a nicety: it
/// means ticking "whole word" does not throw away a manuscript's worth of folding.
///
/// The word-boundary table is built **lazily**, on the first whole-word query, because it is a
/// second full pass over the text and most searches never ask for it.
pub struct FoldedText {
    /// The text as the writer typed it. Retained because [`Match`] offsets address it, and a
    /// caller that has cached this has usually thrown its own copy away — the alternative is
    /// every caller keeping a parallel copy and hoping the two stay in step.
    source: String,
    folded: Folded,
    spec: FoldSpec,
    boundaries: OnceLock<Vec<u32>>,
}

impl FoldedText {
    /// Fold `haystack` under `spec`. (`MatchOptions::fold_spec` gets you one.)
    pub fn new(haystack: &str, spec: &FoldSpec) -> Self {
        Self {
            source: haystack.to_string(),
            folded: Folded::new(haystack, spec),
            spec: *spec,
            boundaries: OnceLock::new(),
        }
    }

    /// The original text — what the offsets below address, and what a snippet is cut from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Every occurrence of `needle`, in **original** char offsets.
    ///
    /// `whole_word` is decided here rather than at fold time; see the type's docs.
    pub fn find_all(&self, needle: &str, whole_word: bool) -> Vec<Match> {
        if needle.is_empty() || self.folded.text.is_empty() {
            return Vec::new();
        }
        // The needle is folded by the same function the haystack was, so the two can never be
        // folded by different rules.
        let folded_needle = fold_query(needle, &self.spec);
        if folded_needle.is_empty() {
            return Vec::new();
        }
        if !whole_word {
            return self.folded.matches(&folded_needle);
        }

        // Boundaries are computed on the ORIGINAL text, so they compose directly with the
        // original offsets the matcher reports — no second index map needed.
        let boundaries = self
            .boundaries
            .get_or_init(|| word_boundaries(&self.source));
        let arabic = starts_with_arabic(&folded_needle);

        self.folded
            .scan(&folded_needle)
            .filter(|(folded_start, _, m)| {
                let ends_on_a_word = is_boundary(boundaries, m.char_start + m.char_len);
                let starts_on_a_word = is_boundary(boundaries, m.char_start)
                    || (arabic && after_arabic_proclitic(&self.folded, boundaries, *folded_start));
                starts_on_a_word && ends_on_a_word
            })
            .map(|(_, _, m)| m)
            .collect()
    }

    /// Roughly how many bytes of heap this holds.
    ///
    /// A host app caching one per scene has to bound the total, and the honest number is not
    /// obvious: the index maps are four bytes per *folded char*, so together they outweigh the
    /// text itself several times over.
    pub fn heap_size(&self) -> usize {
        self.source.capacity()
            + self.folded.text.capacity()
            + self.folded.origin.capacity() * 4
            + self.folded.byte_of_char.capacity() * 4
            + self.boundaries.get().map_or(0, |b| b.capacity() * 4)
    }
}

/// Fold a query. Same function as the haystack's fold, so the two can never be folded by
/// different rules — but a query has no index map, because nothing maps back into it.
fn fold_query(needle: &str, spec: &FoldSpec) -> String {
    let mut out = String::with_capacity(needle.len());
    let mut chars = needle.chars().peekable();
    while let Some(c) = chars.next() {
        let consumed = folding::fold_char(c, chars.peek().copied(), spec, &mut |g| out.push(g));
        if consumed == 2 {
            chars.next();
        }
    }
    out
}

/// A folded haystack, plus the map back to where each folded char came from.
///
/// Shared with the regex path (`search_helpers`), which folds diacritics the same way and
/// needs the same map back — and the same guard against a match that starts inside a letter.
pub(crate) struct Folded {
    text: String,
    /// `origin[i]` is the ORIGINAL char index that produced folded char `i`. One extra
    /// trailing entry (the original char count) so an *end* offset maps too.
    ///
    /// The map is **not** a bijection, in either direction, and both asymmetries matter:
    /// one source char can produce several folded chars (`ß` → `ss`), and one source char
    /// can produce **none** (a dropped combining mark, an elided tatweel).
    origin: Vec<u32>,
    /// Byte offset of each folded char, so a byte match can be turned into a folded char
    /// index without building a byte-indexed map of the whole haystack.
    byte_of_char: Vec<u32>,
}

impl Folded {
    /// Fold a haystack under the same rules a query is folded by — the options are the
    /// *match* options, so the two can never diverge.
    pub(crate) fn new_for(text: &str, options: &MatchOptions) -> Self {
        Self::new(text, &options.fold_spec())
    }

    /// The folded text a scan runs against.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    fn new(text: &str, spec: &FoldSpec) -> Self {
        let mut folded = String::with_capacity(text.len());
        let mut origin: Vec<u32> = Vec::with_capacity(text.len());
        let mut byte_of_char: Vec<u32> = Vec::with_capacity(text.len());

        let mut chars = text.chars().enumerate().peekable();
        let mut source_len = 0usize;
        while let Some((i, c)) = chars.next() {
            let next = chars.peek().map(|&(_, c)| c);
            let consumed = folding::fold_char(c, next, spec, &mut |g| {
                byte_of_char.push(folded.len() as u32);
                origin.push(i as u32);
                folded.push(g);
            });
            if consumed == 2 {
                chars.next();
            }
            source_len = i + consumed;
        }
        // Sentinels, so a match ending at the very end of the text still maps.
        byte_of_char.push(folded.len() as u32);
        origin.push(source_len as u32);

        Self {
            text: folded,
            origin,
            byte_of_char,
        }
    }

    /// Folded **char** index of a folded **byte** offset.
    ///
    /// Binary search over the per-char byte offsets, rather than a byte-indexed map
    /// allocated for the whole haystack (8 bytes per *byte* of text — megabytes on a novel,
    /// rebuilt on every keystroke).
    pub(crate) fn char_of_byte(&self, byte: usize) -> Option<usize> {
        self.byte_of_char.binary_search(&(byte as u32)).ok()
    }

    /// Map a span of the folded text back to a span of the source — or `None` if it does
    /// not line up with source chars.
    ///
    /// **This is the guard.** A folded span that starts or ends *strictly inside* the
    /// expansion of one source char names no source range at all: the `s` a scan finds in
    /// the middle of a folded `ß` is half a letter. Reporting it would highlight a
    /// character that is not there, and *replacing* it would splice into the middle of one.
    pub(crate) fn to_source_match(&self, folded_start: usize, folded_end: usize) -> Option<Match> {
        let from = *self.origin.get(folded_start)?;
        let to = *self.origin.get(folded_end)?;

        // Begins inside a source char's expansion: the preceding folded char came from the
        // same source char.
        if folded_start > 0 && self.origin[folded_start - 1] == from {
            return None;
        }
        // Ends inside one: the folded char just past the match came from the same source
        // char as the last one inside it. The trailing sentinel makes this safe at the very
        // end of the text.
        if folded_end > 0 && self.origin[folded_end - 1] == to {
            return None;
        }

        Some(Match {
            char_start: from as usize,
            char_len: to.saturating_sub(from) as usize,
        })
    }

    fn matches(&self, folded_needle: &str) -> Vec<Match> {
        self.scan(folded_needle).map(|(_, _, m)| m).collect()
    }

    /// `match_indices` is the two-way algorithm — the literal scan it replaced was an
    /// O(n·m) char-window byte-slice compare, re-sliced on every character.
    fn scan<'a>(
        &'a self,
        folded_needle: &'a str,
    ) -> impl Iterator<Item = (usize, usize, Match)> + 'a {
        let needle_chars = folded_needle.chars().count();
        self.text
            .match_indices(folded_needle)
            .filter_map(move |(byte, _)| {
                let folded_start = self.char_of_byte(byte)?;
                let folded_end = folded_start + needle_chars;
                let m = self.to_source_match(folded_start, folded_end)?;
                Some((folded_start, folded_end, m))
            })
    }
}

/// Apostrophes that sit *inside* a word under UAX#29 but that a writer means as a word
/// edge: ASCII `'`, the typographic `’`, and the modifier letter `ʼ`.
fn is_apostrophe(ch: char) -> bool {
    matches!(ch, '\u{0027}' | '\u{2019}' | '\u{02BC}')
}

/// The Arabic script blocks: the main block, the supplement, and Arabic Extended-A. Used
/// only to decide whether the proclitic rule below is worth consulting at all.
fn is_arabic(ch: char) -> bool {
    matches!(ch, '\u{0600}'..='\u{06FF}' | '\u{0750}'..='\u{077F}' | '\u{08A0}'..='\u{08FF}')
}

fn starts_with_arabic(folded_needle: &str) -> bool {
    folded_needle.chars().next().is_some_and(is_arabic)
}

/// Arabic proclitics that a whole-word search must see through.
///
/// Arabic fuses its definite article to the noun with **no separator** — `الكتاب` is
/// "the book" written as one orthographic word — so no boundary rule, in any spec, can find
/// `كتاب` inside it. That needs a table, and this is it: the article `ال`, and the article
/// with the conjunctions and prepositions that attach in front of it.
///
/// Deliberately **not** the bare single-letter proclitics (`ب ل ك و ف`), nor the Hebrew
/// prefixes: those letters are frequently a word's own first root letter, so admitting them
/// would make whole-word search match inside unrelated words far more often than it would
/// help. Written in the *folded* form, since that is what they are compared against — the
/// fold has already turned `ٱل` into `ال` and removed any harakat between the letters.
///
/// **Scope, stated out loud:** morphological analysis (Arabic or Hebrew stemming, German
/// decompounding) is out of scope for a *literal* search. Replace on a stemmed match is
/// undefined — you would rewrite a different surface string from the one on screen.
const ARABIC_PROCLITICS: &[&str] = &["ال", "وال", "بال", "فال", "كال", "لل"];

/// Is this occurrence preceded, within the same orthographic word, by one of the proclitics
/// above — with the proclitic itself sitting at a real word boundary?
fn after_arabic_proclitic(folded: &Folded, boundaries: &[u32], folded_start: usize) -> bool {
    ARABIC_PROCLITICS.iter().any(|proclitic| {
        let len = proclitic.chars().count();
        let Some(prefix_start) = folded_start.checked_sub(len) else {
            return false;
        };
        let from = folded.byte_of_char[prefix_start] as usize;
        let to = folded.byte_of_char[folded_start] as usize;
        if &folded.text[from..to] != *proclitic {
            return false;
        }
        // The article must itself start a word: `كتاب` inside `الكتاب` is the article plus
        // the noun; the same letters in the middle of some other word are not.
        is_boundary(boundaries, folded.origin[prefix_start] as usize)
    })
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
/// `don` now also matches inside `don't`. That is a visible false positive, and a visible
/// false positive beats an invisible false negative that corrupts prose.
///
/// Deliberately *not* generalised to "any non-letter is a boundary": the hyphen already
/// breaks under UAX#29 (`Jean-Luc` needs nothing), and Catalan `col·lecció` correctly stays
/// one word because U+00B7 is `MidLetter` — a blanket rule would *introduce* a Catalan bug
/// to fix an English one.
pub(crate) fn word_boundaries(text: &str) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    out.push(0);

    // One pass. The previous version called `text[..byte_start].chars().count()` for every
    // word — quadratic in the length of the text, on a document that can be a whole novel.
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

/// Dress `replacement` in the case of the text it is replacing.
///
/// A reviewed rename hits `Aurélien`, `AURÉLIEN` and `aurélien` in the same manuscript, and
/// the writer means one rename, not three. Three shapes are recognised — all-caps, initial
/// capital, and anything else (left alone).
///
/// Takes the **locale**, and must: in Turkish the uppercase of `i` is `İ`, not `I`. A
/// case-preserver blind to that would rewrite Turkish prose into a different word — the
/// same class of silent corruption as folding the dotless `ı` onto `i`.
pub fn preserve_case(matched: &str, replacement: &str, locale: FoldLocale) -> String {
    let mut cased = matched.chars().filter(|c| c.is_alphabetic()).peekable();
    if cased.peek().is_none() {
        return replacement.to_string();
    }

    let all_upper = matched
        .chars()
        .filter(|c| c.is_alphabetic())
        .all(char::is_uppercase);
    if all_upper {
        let mut out = String::with_capacity(replacement.len());
        for c in replacement.chars() {
            folding::to_upper(c, locale, &mut out);
        }
        return out;
    }

    let leads_upper = matched
        .chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(char::is_uppercase);
    if leads_upper {
        let mut out = String::with_capacity(replacement.len());
        let mut chars = replacement.chars();
        if let Some(first) = chars.next() {
            folding::to_upper(first, locale, &mut out);
        }
        out.extend(chars);
        return out;
    }

    replacement.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(case_sensitive: bool, whole_word: bool) -> MatchOptions {
        MatchOptions {
            case_sensitive,
            whole_word,
            ..MatchOptions::default()
        }
    }

    /// The matched text, sliced out of the ORIGINAL by the reported offsets. If the offsets
    /// are wrong, this is wrong — which is the entire failure mode.
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
    /// `"İİİ ipsum dolor"` into `"İİİ ipsLOREMlor"` — silent corruption of a writer's prose,
    /// under the DEFAULT (case-insensitive) options.
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

    /// The guard. `Straße` folds to `strasse`, so a scan finds `s` in the middle of the `ß`
    /// — a position that names half a letter. There is no source range to report for it, and
    /// splicing there would produce mojibake.
    #[test]
    fn a_query_cannot_match_half_of_a_folded_letter() {
        let text = "Straße";
        let hits = find_all(text, "s", &opts(false, false));
        assert_eq!(
            hits.len(),
            1,
            "only the leading S — never half of the ß, which folds to two chars"
        );
        assert_eq!(hits[0].char_start, 0);
        assert_eq!(matched(text, hits[0]), "S");

        // …but a query that covers the WHOLE letter is a real match, and reports the letter.
        let hits = find_all(text, "ss", &opts(false, false));
        assert_eq!(hits.len(), 1);
        assert_eq!(matched(text, hits[0]), "ß");

        let hits = find_all(text, "strasse", &opts(false, false));
        assert_eq!(hits.len(), 1);
        assert_eq!(
            matched(text, hits[0]),
            "Straße",
            "the whole word, in the source"
        );
    }

    /// A match must swallow the source chars the fold elided, or a replace leaves an
    /// orphaned combining accent stranded on the replacement text.
    #[test]
    fn a_match_covers_the_combining_marks_the_fold_dropped() {
        let decomposed = "cafe\u{0301} noir"; // NFD: the é is two chars
        let hits = find_all(decomposed, "cafe", &opts(false, false));
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].char_len, 5,
            "four letters plus the combining acute that folded away — a 4-char span would \
             leave the accent behind, floating on whatever replaced the word"
        );
        // The source is NFD, so the matched text is too: `c a f e ◌́`, which *is* "café".
        assert_eq!(matched(decomposed, hits[0]), "cafe\u{0301}");
    }

    /// The point of the whole fold: plain ASCII finds accented prose, and the reported span
    /// is the accented text.
    #[test]
    fn a_plain_query_finds_accented_prose() {
        let text = "la promesse d'Aurélien, et le café";
        let hits = find_all(text, "aurelien", &opts(false, false));
        assert_eq!(hits.len(), 1);
        assert_eq!(matched(text, hits[0]), "Aurélien");

        let hits = find_all(text, "cafe", &opts(false, false));
        assert_eq!(hits.len(), 1);
        assert_eq!(matched(text, hits[0]), "café");
    }

    /// …and the toggle turns it off.
    #[test]
    fn diacritic_sensitive_refuses_the_plain_query() {
        let strict = MatchOptions {
            diacritic_sensitive: true,
            ..MatchOptions::default()
        };
        assert!(find_all("le café", "cafe", &strict).is_empty());
        assert_eq!(find_all("le café", "café", &strict).len(), 1);
        assert_eq!(
            find_all("le CAFÉ", "café", &strict).len(),
            1,
            "case still folds"
        );
    }

    /// Whole-word `Elena` must find `Elena's`. UAX#29 glues the apostrophe into the word, so
    /// this used to miss — and a missed match in Replace All is a half-renamed manuscript.
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

    /// Arabic fuses the article to the noun with no separator, so **no** boundary rule can
    /// see the seam. Whole-word `كتاب` must still find `الكتاب`.
    #[test]
    fn whole_word_sees_through_an_arabic_proclitic() {
        for text in ["قرأت الكتاب", "قرأت وَالْكِتَاب", "قرأت ٱلكتاب"]
        {
            let hits = find_all(text, "كتاب", &opts(false, true));
            assert_eq!(
                hits.len(),
                1,
                "the article must not hide the noun in {text:?}"
            );
        }
        // …and with no proclitic at all it is simply a word.
        assert_eq!(find_all("قرأت كتاب", "كتاب", &opts(false, true)).len(), 1);
    }

    /// The proclitic rule is a *prefix* rule, not "these letters anywhere". The article must
    /// itself begin a word, or `كتاب` would match inside words that merely happen to contain
    /// those letters.
    #[test]
    fn the_proclitic_rule_does_not_admit_a_bare_substring() {
        // `الكتابية` — the noun is followed by more letters, so the END is not a boundary.
        assert!(
            find_all("الكتابية", "كتاب", &opts(false, true)).is_empty(),
            "the occurrence must still END on a word boundary"
        );
        // Without whole_word, it is a plain substring match and does occur.
        assert_eq!(find_all("الكتابية", "كتاب", &opts(false, false)).len(), 1);
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

    /// Catalan `col·lecció` is ONE word (U+00B7 is deliberately MidLetter). A blanket "any
    /// non-letter is a boundary" rule would have broken this to fix the apostrophe.
    #[test]
    fn a_catalan_middle_dot_does_not_split_a_word() {
        assert!(
            find_all("una col·lecció", "lecció", &opts(false, true)).is_empty(),
            "the middle dot must not become a word boundary"
        );
        // …and the whole word is found, middle dot and accents folded alike.
        assert_eq!(
            find_all("una col·lecció", "col·leccio", &opts(false, true)).len(),
            1
        );
    }

    /// Turkish, end to end: the scene's language changes what the same query finds.
    #[test]
    fn a_turkish_scene_keeps_the_two_letters_apart() {
        let turkish = MatchOptions {
            locale: FoldLocale::Turkic,
            ..MatchOptions::default()
        };
        assert_eq!(find_all("KISA bir yol", "kısa", &turkish).len(), 1);
        assert!(
            find_all("KISA bir yol", "kisa", &turkish).is_empty(),
            "in a Turkish scene the dotted i is a different letter"
        );
        // The same prose in an untailored scene folds them together.
        assert_eq!(
            find_all("KISA bir yol", "kisa", &MatchOptions::default()).len(),
            1
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
        // A query that folds away to nothing (a lone combining mark) matches nothing
        // either — rather than matching everywhere.
        assert!(find_all("café", "\u{0301}", &opts(false, false)).is_empty());
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

    #[test]
    fn preserve_case_recognises_the_three_shapes() {
        assert_eq!(
            preserve_case("Aurélien", "aurélian", FoldLocale::Root),
            "Aurélian"
        );
        assert_eq!(
            preserve_case("AURÉLIEN", "aurélian", FoldLocale::Root),
            "AURÉLIAN"
        );
        assert_eq!(
            preserve_case("aurélien", "aurélian", FoldLocale::Root),
            "aurélian"
        );
        // Nothing cased to copy: leave the replacement exactly as given.
        assert_eq!(preserve_case("123", "abc", FoldLocale::Root), "abc");
    }

    /// The Turkish trap: the uppercase of `i` is `İ`. An untailored case-preserver would
    /// write `I`, which is the uppercase of a *different letter*.
    #[test]
    fn preserve_case_uppercases_turkish_correctly() {
        assert_eq!(preserve_case("KISA", "ilk", FoldLocale::Turkic), "İLK");
        assert_eq!(preserve_case("KISA", "ilk", FoldLocale::Root), "ILK");
        assert_eq!(preserve_case("Kısa", "ilk", FoldLocale::Turkic), "İlk");
    }
}
