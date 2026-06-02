use std::collections::HashSet;

use anyhow::{Result, anyhow};
use common::database::Store;
use common::database::rope_helpers::block_content_via_store;
use common::entities::Block;
use regex::RegexBuilder;
use unicode_segmentation::UnicodeSegmentation;

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

/// Build a mapping from byte offset to char index for a string.
/// `byte_to_char[byte_offset] = char_index`
/// The vec has len = `text.len() + 1` (inclusive of the end position).
pub fn build_byte_to_char_map(text: &str) -> Vec<usize> {
    let mut map = vec![0usize; text.len() + 1];
    let mut char_idx = 0;
    for (byte_idx, _) in text.char_indices() {
        map[byte_idx] = char_idx;
        char_idx += 1;
    }
    map[text.len()] = char_idx;
    map
}

/// Pre-compute the set of char indices that are Unicode word boundaries.
///
/// A word boundary is a char index where a word starts or ends according
/// to `unicode_word_indices()`. Index 0 and `chars_len` are always
/// boundaries. Looking up a boundary is O(1) via `HashSet::contains`.
pub fn build_word_boundary_set(text: &str) -> HashSet<usize> {
    let chars_len = text.chars().count();
    let mut set = HashSet::new();
    set.insert(0);
    set.insert(chars_len);
    for (byte_start, word) in text.unicode_word_indices() {
        let word_char_start = text[..byte_start].chars().count();
        let word_char_end = word_char_start + word.chars().count();
        set.insert(word_char_start);
        set.insert(word_char_end);
    }
    set
}

/// Find all occurrences of the query in the text, respecting search options.
/// All positions are in char indices (not byte offsets).
/// Returns a vec of `(char_position, char_length)` for each match.
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

    // Pre-compute word boundaries once if needed, instead of O(n) per match.
    let word_boundaries = if whole_word {
        Some(build_word_boundary_set(full_text))
    } else {
        None
    };

    let mut results = Vec::new();

    if use_regex {
        let re = RegexBuilder::new(query)
            .case_insensitive(!case_sensitive)
            .size_limit(1 << 20) // 1 MB compiled size limit
            .dfa_size_limit(1 << 20)
            .build()
            .map_err(|e| anyhow!("Invalid regex pattern: {}", e))?;

        let char_offsets = build_byte_to_char_map(full_text);

        for mat in re.find_iter(full_text) {
            let char_start = char_offsets[mat.start()];
            let char_end = char_offsets[mat.end()];
            let char_len = char_end - char_start;

            if let Some(ref wb) = word_boundaries {
                if wb.contains(&char_start) && wb.contains(&char_end) {
                    results.push((char_start, char_len));
                }
            } else {
                results.push((char_start, char_len));
            }
        }
    } else {
        // Literal search using lowercased Strings instead of Vec<char>.
        let (search_text, search_query) = if case_sensitive {
            (full_text.to_string(), query.to_string())
        } else {
            (full_text.to_lowercase(), query.to_lowercase())
        };

        // Build a char-index → byte-offset mapping for the search text
        let char_indices: Vec<usize> = search_text.char_indices().map(|(i, _)| i).collect();
        let query_char_len = search_query.chars().count();

        if query_char_len == 0 || char_indices.len() < query_char_len {
            return Ok(results);
        }

        let mut char_pos = 0;
        while char_pos + query_char_len <= char_indices.len() {
            let byte_start = char_indices[char_pos];
            let byte_end = if char_pos + query_char_len < char_indices.len() {
                char_indices[char_pos + query_char_len]
            } else {
                search_text.len()
            };

            if search_text[byte_start..byte_end] == search_query[..] {
                if let Some(ref wb) = word_boundaries {
                    if wb.contains(&char_pos) && wb.contains(&(char_pos + query_char_len)) {
                        results.push((char_pos, query_char_len));
                    }
                } else {
                    results.push((char_pos, query_char_len));
                }
            }
            char_pos += 1;
        }
    }

    Ok(results)
}
