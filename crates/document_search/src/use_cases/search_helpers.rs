use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::{Result, anyhow};
use common::database::Store;
use common::database::rope_helpers::block_content_via_store;
use common::entities::Block;
use regex::{Regex, RegexBuilder};

use crate::matching::{self, MatchOptions};

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

/// Byte offset of every char, plus a trailing sentinel — so a regex's byte match can be
/// turned into a char offset by binary search.
///
/// This replaces a `byte -> char` map that was allocated with one `usize` **per byte of
/// text** (8 bytes per byte: sixteen megabytes for a two-megabyte novel, rebuilt on every
/// search). This one is 4 bytes per *char* and is only built on the regex path.
fn char_start_bytes(text: &str) -> Vec<u32> {
    let mut out: Vec<u32> = text.char_indices().map(|(b, _)| b as u32).collect();
    out.push(text.len() as u32);
    out
}

fn byte_to_char(char_starts: &[u32], byte: usize) -> Option<usize> {
    char_starts.binary_search(&(byte as u32)).ok()
}

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
/// shared with the public API so a host app's project-wide search and this in-document
/// find can never disagree about whole-word rules or case folding.
pub fn find_all_matches(
    full_text: &str,
    query: &str,
    case_sensitive: bool,
    whole_word: bool,
    use_regex: bool,
) -> Result<Vec<(usize, usize)>> {
    if query.is_empty() {
        return Ok(Vec::new());
    }

    if !use_regex {
        let options = MatchOptions {
            case_sensitive,
            whole_word,
        };
        return Ok(matching::find_all(full_text, query, &options)
            .into_iter()
            .map(|m| (m.char_start, m.char_len))
            .collect());
    }

    let re = compiled_regex(query, case_sensitive)?;
    let char_starts = char_start_bytes(full_text);
    let boundaries = whole_word.then(|| matching::word_boundaries(full_text));

    let mut results = Vec::new();
    for mat in re.find_iter(full_text) {
        // A regex can match at a position that is not a char start only if the pattern
        // matched inside a multi-byte char, which `regex` does not do — but the map is a
        // lookup, not an assumption, so a miss is skipped rather than panicking.
        let (Some(char_start), Some(char_end)) = (
            byte_to_char(&char_starts, mat.start()),
            byte_to_char(&char_starts, mat.end()),
        ) else {
            continue;
        };

        if let Some(boundaries) = &boundaries
            && !(matching::is_boundary(boundaries, char_start)
                && matching::is_boundary(boundaries, char_end))
        {
            continue;
        }
        results.push((char_start, char_end - char_start));
    }

    Ok(results)
}
