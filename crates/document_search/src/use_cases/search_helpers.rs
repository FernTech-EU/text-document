use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::{Result, anyhow};
use common::database::Store;
use common::database::rope_helpers::block_content_via_store;
use common::entities::Block;
use regex::{Regex, RegexBuilder};

use crate::matching::{self, FoldLocale, MatchOptions};

/// Build the full document text from the given blocks by reading content
/// from the global rope via `block_offsets`.
///
/// This is the *fallback* path used only when the rope's char space does
/// not match document flow (a sub-frame inserted with a parent — its
/// blocks aren't mirrored to the rope). The primary search path reads the
/// whole rope directly via `rope_full_text_if_flow_matches`, which already
/// covers table-cell content (cells are mirrored inline into the rope in
/// document order). Blocks not registered in the offset index contribute
/// the empty string — see `block_content_via_store`.
///
/// The returned string has blocks joined by single `\n` separators —
/// matching the position semantics that `document_position` is computed
/// against.
///
/// Blocks must be sorted by `document_position` (caller's
/// responsibility).
pub fn build_full_text_via_store(blocks: &[Block], store: &Store) -> String {
    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&block_content_via_store(block, store));
    }
    out
}

/// The text each match actually covers, sliced from the text the search ran on.
///
/// Done here, and returned to the caller, so that **nobody slices it themselves**. Two
/// reasons, and both have already bitten this crate:
///
/// - With folding on, the query is not the matched text. `cafe` matches `café`; `strasse`
///   matches `straße`. A caller that echoed the query back would show the writer a word that
///   is not in their book.
/// - The only other whole-document string a caller can reach is `to_plain_text`, which does
///   not share this offset space — it carries no `U+FFFC` anchor for an embedded table, so
///   slicing it with these offsets is wrong by two chars per preceding table.
///
/// The chars are collected **once** for the whole match list, not re-walked per match, which
/// would be quadratic on a document where the query occurs often — exactly the document
/// where it matters.
pub fn matched_texts(full_text: &str, matches: &[(usize, usize)]) -> Vec<String> {
    if matches.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = full_text.chars().collect();
    matches
        .iter()
        .map(|&(position, length)| {
            let start = position.min(chars.len());
            let end = (position + length).min(chars.len());
            chars[start..end].iter().collect()
        })
        .collect()
}

/// Every DTO that describes a *search* carries the same four fields, and they must reach the
/// matcher together. Written as conversions rather than as four positional arguments,
/// because a `find_all_matches(text, query, true, false, false, "")` call site is a bug
/// waiting for someone to transpose two bools — and one of those bools decides whether a
/// rename touches half a manuscript.
macro_rules! match_options_from {
    ($dto:ty) => {
        impl From<&$dto> for MatchOptions {
            fn from(dto: &$dto) -> Self {
                MatchOptions {
                    case_sensitive: dto.case_sensitive,
                    diacritic_sensitive: dto.diacritic_sensitive,
                    whole_word: dto.whole_word,
                    locale: FoldLocale::from_tag(&dto.language),
                }
            }
        }
    };
}

match_options_from!(crate::FindTextDto);
match_options_from!(crate::FindAllDto);
match_options_from!(crate::ReplaceTextDto);

thread_local! {
    /// Compiled regexes, keyed by `(pattern, case_sensitive)`.
    ///
    /// The regex was recompiled on **every call** — including once per keystroke of a
    /// search-as-you-type box, where compilation dwarfs the scan it precedes.
    static REGEX_CACHE: RefCell<HashMap<(String, bool), Regex>> = RefCell::new(HashMap::new());
}

/// How many compiled regexes to keep. A search box produces one entry per prefix of what
/// is typed, so this is bounded rather than unbounded; the cache is cleared wholesale on
/// overflow because the working set is "the pattern being typed right now", not an LRU.
const REGEX_CACHE_CAP: usize = 32;

fn compiled_regex(pattern: &str, case_sensitive: bool) -> Result<Regex> {
    let key = (pattern.to_string(), case_sensitive);
    REGEX_CACHE.with(|cache| {
        if let Some(re) = cache.borrow().get(&key) {
            return Ok(re.clone());
        }
        let re = RegexBuilder::new(pattern)
            .case_insensitive(!case_sensitive)
            .size_limit(1 << 20) // 1 MB compiled size limit
            .dfa_size_limit(1 << 20)
            .build()
            .map_err(|e| anyhow!("Invalid regex pattern: {}", e))?;
        let mut cache = cache.borrow_mut();
        if cache.len() >= REGEX_CACHE_CAP {
            cache.clear();
        }
        cache.insert(key, re.clone());
        Ok(re)
    })
}

/// Find all occurrences of the query in the text, respecting search options.
/// All positions are in char indices (not byte offsets).
/// Returns a vec of `(char_position, char_length)` for each match.
///
/// The literal path delegates to [`crate::matching`] — the one definition of "a match",
/// shared with the public API so a host app's project-wide search and this in-document find
/// can never disagree about whole-word rules or folding.
///
/// ## The regex path
///
/// The pattern runs against the **folded** haystack, with the same index map back to the
/// source that the literal path uses — so `diacritic_sensitive` is honoured here too rather
/// than being an option that silently means nothing on half the calls that take it.
///
/// *Case* is the exception: it stays with the regex engine (`(?i)`), searching the
/// unfolded-for-case text. Fold the case away and a pattern like `[A-Z]` would match
/// nothing, because there would be no uppercase left to match — the author of a regex is
/// addressing the text as written.
pub fn find_all_matches(
    full_text: &str,
    query: &str,
    options: &MatchOptions,
    use_regex: bool,
) -> Result<Vec<(usize, usize)>> {
    if query.is_empty() {
        return Ok(Vec::new());
    }

    if !use_regex {
        return Ok(matching::find_all(full_text, query, options)
            .into_iter()
            .map(|m| (m.char_start, m.char_len))
            .collect());
    }

    let re = compiled_regex(query, options.case_sensitive)?;
    let folded = matching::Folded::new_for(
        full_text,
        &MatchOptions {
            // The regex engine owns case; this fold owns diacritics. Each mechanism does
            // exactly one job, and neither undoes the other.
            case_sensitive: true,
            ..*options
        },
    );
    let boundaries = options
        .whole_word
        .then(|| matching::word_boundaries(full_text));

    let mut results = Vec::new();
    for mat in re.find_iter(folded.text()) {
        // A regex can match **nothing** — `a*`, `x?`, `\b` all match the empty string at every
        // position. The literal path cannot produce one (an empty needle returns early), so
        // nothing downstream expects one: `replace_text` would take a zero-length range as an
        // *insertion* and splice the replacement in at every character of the document, having
        // matched nothing at all. Refuse it here, where the concept of an empty match exists.
        if mat.start() == mat.end() {
            continue;
        }
        // A regex can match at a position that is not a char start only if the pattern
        // matched inside a multi-byte char, which `regex` does not do — but the map is a
        // lookup, not an assumption, so a miss is skipped rather than panicking.
        let (Some(folded_start), Some(folded_end)) = (
            folded.char_of_byte(mat.start()),
            folded.char_of_byte(mat.end()),
        ) else {
            continue;
        };
        // Same guard as the literal path: a span that begins or ends inside one source
        // char's expansion names half a letter, and there is no source range to report.
        let Some(m) = folded.to_source_match(folded_start, folded_end) else {
            continue;
        };

        if let Some(boundaries) = &boundaries
            && !(matching::is_boundary(boundaries, m.char_start)
                && matching::is_boundary(boundaries, m.char_start + m.char_len))
        {
            continue;
        }
        results.push((m.char_start, m.char_len));
    }

    Ok(results)
}
