//! Property-based / fuzz tests for the public API.
//!
//! Uses `proptest` to generate random inputs and verify invariants hold.
//!
//! Note on document semantics: `character_count()` counts characters *within*
//! blocks, NOT block separators (newlines). So `to_plain_text()` may contain
//! `\n` separators that are not counted. Similarly, `\r` is normalized to `\n`
//! by the document engine.

use proptest::prelude::*;
use text_document::{FindOptions, MoveMode, MoveOperation, SelectionType, TextDocument};

// ── Helpers ─────────────────────────────────────────────────────

fn new_doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

/// Count characters the same way the document does:
/// sum of per-block character counts (excludes block separators).
fn doc_char_count(text: &str) -> usize {
    text.split('\n').map(|line| line.chars().count()).sum()
}

// ── Strategies ──────────────────────────────────────────────────

/// Arbitrary printable text without \r or NUL (the document normalizes \r to \n).
fn arb_text() -> impl Strategy<Value = String> {
    "[^\x00\r]{0,500}"
}

/// Arbitrary multi-line text (no \r).
fn arb_multiline_text() -> impl Strategy<Value = String> {
    prop::collection::vec("[^\x00\r\n]{0,80}", 1..10).prop_map(|lines| lines.join("\n"))
}

/// Arbitrary move operation.
fn arb_move_op() -> impl Strategy<Value = MoveOperation> {
    prop_oneof![
        Just(MoveOperation::Start),
        Just(MoveOperation::End),
        Just(MoveOperation::NextCharacter),
        Just(MoveOperation::PreviousCharacter),
        Just(MoveOperation::NextWord),
        Just(MoveOperation::PreviousWord),
        Just(MoveOperation::NextBlock),
        Just(MoveOperation::PreviousBlock),
        Just(MoveOperation::StartOfBlock),
        Just(MoveOperation::EndOfBlock),
        Just(MoveOperation::StartOfLine),
        Just(MoveOperation::EndOfLine),
        Just(MoveOperation::StartOfWord),
        Just(MoveOperation::EndOfWord),
        Just(MoveOperation::Up),
        Just(MoveOperation::Down),
        Just(MoveOperation::Left),
        Just(MoveOperation::Right),
        Just(MoveOperation::WordLeft),
        Just(MoveOperation::WordRight),
        Just(MoveOperation::NoMove),
    ]
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: set_plain_text -> to_plain_text roundtrip
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn plain_text_roundtrip(text in arb_text()) {
        let doc = new_doc(&text);
        let result = doc.to_plain_text().unwrap();
        prop_assert_eq!(result, text);
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: character_count == sum of per-block char counts
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn character_count_matches_block_content(text in arb_text()) {
        let doc = new_doc(&text);
        let char_count = doc.character_count();
        let expected = doc_char_count(&text);
        prop_assert_eq!(char_count, expected,
            "character_count() = {} but expected {} for {:?}",
            char_count, expected, text);
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: block_count == number of lines
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn block_count_matches_newlines(text in arb_multiline_text()) {
        let doc = new_doc(&text);
        let expected_blocks = text.split('\n').count();
        let actual_blocks = doc.block_count();
        prop_assert_eq!(actual_blocks, expected_blocks,
            "block_count() = {} but text has {} lines", actual_blocks, expected_blocks);
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: insert then undo restores original text
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn insert_undo_restores_text(
        base in arb_text(),
        insert in "[a-zA-Z0-9 ]{1,50}",
        pos_frac in 0.0f64..=1.0,
    ) {
        let doc = new_doc(&base);
        let char_count = doc.character_count();
        let pos = (pos_frac * char_count as f64).floor() as usize;
        let pos = pos.min(char_count);

        let cursor = doc.cursor_at(pos);
        cursor.insert_text(&insert).unwrap();

        // Verify insert took effect
        let after_insert = doc.to_plain_text().unwrap();
        prop_assert!(after_insert.contains(&insert));

        // Undo should restore
        doc.undo().unwrap();
        let after_undo = doc.to_plain_text().unwrap();
        prop_assert_eq!(&after_undo, &base);
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: cursor position always in [0, character_count]
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn cursor_position_always_in_bounds(
        text in arb_multiline_text(),
        ops in prop::collection::vec(arb_move_op(), 1..20),
    ) {
        let doc = new_doc(&text);
        let cursor = doc.cursor();

        for op in &ops {
            cursor.move_position(*op, MoveMode::MoveAnchor, 1);
            let pos = cursor.position();
            let stats = doc.stats();
            // Max position includes block separators: character_count + (block_count - 1)
            let max_pos = stats.character_count + stats.block_count.saturating_sub(1);
            prop_assert!(pos <= max_pos,
                "cursor position {} exceeds max_position {} after {:?}",
                pos, max_pos, op);
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: set_position clamps to document length
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn set_position_always_clamps(
        text in arb_text(),
        pos in 0usize..10000,
    ) {
        let doc = new_doc(&text);
        let cursor = doc.cursor();
        cursor.set_position(pos, MoveMode::MoveAnchor);
        let actual = cursor.position();
        let stats = doc.stats();
        // Max position includes block separators: character_count + (block_count - 1)
        let max_pos = stats.character_count + stats.block_count.saturating_sub(1);
        prop_assert!(actual <= max_pos,
            "set_position({}) resulted in position {} > max_position {}",
            pos, actual, max_pos);
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: selection_start <= selection_end
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn selection_start_le_end(
        text in arb_multiline_text(),
        sel in prop_oneof![
            Just(SelectionType::WordUnderCursor),
            Just(SelectionType::LineUnderCursor),
            Just(SelectionType::BlockUnderCursor),
            Just(SelectionType::Document),
        ],
        pos_frac in 0.0f64..=1.0,
    ) {
        let doc = new_doc(&text);
        let char_count = doc.character_count();
        if char_count == 0 {
            return Ok(());
        }
        let pos = ((pos_frac * char_count as f64).floor() as usize).min(char_count);
        let cursor = doc.cursor_at(pos);
        cursor.select(sel);

        if cursor.has_selection() {
            prop_assert!(cursor.selection_start() <= cursor.selection_end(),
                "selection_start {} > selection_end {}",
                cursor.selection_start(), cursor.selection_end());
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: find_all results are sorted and within bounds
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn find_all_results_sorted_and_in_bounds(
        text in "[a-c ]{10,100}",
        query in "[a-c]{1,3}",
    ) {
        let doc = new_doc(&text);
        let opts = FindOptions::default();
        let matches = doc.find_all(&query, &opts).unwrap();

        // Results should be sorted by position
        for i in 1..matches.len() {
            prop_assert!(matches[i].position >= matches[i - 1].position,
                "unsorted matches: position {} after {}",
                matches[i].position, matches[i - 1].position);
        }

        // All positions should be within document bounds
        let len = doc.character_count();
        for m in &matches {
            prop_assert!(m.position + m.length <= len,
                "match at {} len {} exceeds document length {}",
                m.position, m.length, len);
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: stats are consistent
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn stats_consistency(text in arb_multiline_text()) {
        let doc = new_doc(&text);
        let stats = doc.stats();

        prop_assert_eq!(stats.character_count, doc.character_count());
        prop_assert_eq!(stats.block_count, doc.block_count());
        prop_assert!(stats.frame_count >= 1, "should have at least one frame");

        let expected_empty = doc_char_count(&text) == 0;
        prop_assert_eq!(doc.is_empty(), expected_empty);
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: stats.character_count matches to_plain_text().chars().count()
// minus block separators — a non-tautological cross-check of the
// cached document character_count against actual content.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn stats_char_count_matches_plain_text(text in arb_multiline_text()) {
        let doc = new_doc(&text);
        let stats = doc.stats();
        let plain = doc.to_plain_text().unwrap();

        // to_plain_text joins blocks with '\n'; character_count does not
        // include those separators. With N blocks there are N-1 separators.
        let plain_chars = plain.chars().count();
        let sep_count = stats.block_count.saturating_sub(1);
        let content_chars = plain_chars - sep_count;

        prop_assert_eq!(stats.character_count, content_chars,
            "stats.character_count ({}) != plain_text char count minus {} separators ({})",
            stats.character_count, sep_count, content_chars);
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: random insert/delete edits maintain character_count consistency
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(Debug, Clone)]
enum EditOp {
    Insert(String),
    DeleteChar,
    DeletePrevChar,
}

fn arb_edit_op() -> impl Strategy<Value = EditOp> {
    prop_oneof![
        "[a-zA-Z0-9 ]{1,20}".prop_map(EditOp::Insert),
        Just(EditOp::DeleteChar),
        Just(EditOp::DeletePrevChar),
    ]
}

proptest! {
    #[test]
    fn random_edits_maintain_consistency(
        initial in "[a-zA-Z ]{0,100}",
        ops in prop::collection::vec(
            (arb_edit_op(), 0.0f64..=1.0),
            1..15,
        ),
    ) {
        let doc = new_doc(&initial);

        for (op, pos_frac) in &ops {
            let char_count = doc.character_count();
            let pos = ((*pos_frac * char_count as f64).floor() as usize).min(char_count);
            let cursor = doc.cursor_at(pos);

            match op {
                EditOp::Insert(text) => { cursor.insert_text(text).unwrap(); }
                EditOp::DeleteChar => {
                    // Skip delete on empty doc (known backend edge case: returns -1)
                    if char_count == 0 { continue; }
                    cursor.delete_char().unwrap();
                }
                EditOp::DeletePrevChar => {
                    if char_count == 0 { continue; }
                    cursor.delete_previous_char().unwrap();
                }
            }

            // Invariant: character_count matches actual text content length
            let text = doc.to_plain_text().unwrap();
            let actual_chars = doc_char_count(&text);
            prop_assert_eq!(doc.character_count(), actual_chars,
                "character_count mismatch after {:?} at pos {}", op, pos);

            // Invariant: block_count >= 1
            prop_assert!(doc.block_count() >= 1,
                "block_count should be >= 1, got {}", doc.block_count());
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: multiple undos fully restore initial state
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn multiple_undos_restore_state(
        initial in "[a-zA-Z ]{1,50}",
        inserts in prop::collection::vec("[a-z]{1,10}", 1..6),
    ) {
        let doc = new_doc(&initial);
        let num_inserts = inserts.len();

        for text in &inserts {
            let pos = doc.character_count();
            let cursor = doc.cursor_at(pos);
            cursor.insert_text(text).unwrap();
        }

        // Undo all inserts (merging may reduce the number of undo steps)
        let mut undo_count = 0;
        while doc.can_undo() {
            doc.undo().unwrap();
            undo_count += 1;
        }
        prop_assert!(undo_count >= 1);
        prop_assert!(undo_count <= num_inserts);

        let restored = doc.to_plain_text().unwrap();
        prop_assert_eq!(&restored, &initial);

        // Redo all
        for _ in 0..undo_count {
            prop_assert!(doc.can_redo());
            doc.redo().unwrap();
        }

        // Should not be able to redo further
        prop_assert!(!doc.can_redo());
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: replace_all count matches find_all count
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn replace_all_count_matches_find_all(
        text in "[a-c ]{10,100}",
        query in "[a-c]{1,2}",
    ) {
        let doc = new_doc(&text);
        let opts = FindOptions::default();
        let find_count = doc.find_all(&query, &opts).unwrap().len();

        // Create a fresh doc for replace (since replace mutates)
        let doc2 = new_doc(&text);
        let replace_count = doc2.replace_text(&query, "X", true, &text_document::ReplaceOptions::new(opts.clone())).unwrap();

        prop_assert_eq!(replace_count, find_count,
            "replace_all count {} != find_all count {} for query '{}'",
            replace_count, find_count, query);
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: fragment round-trip preserves text
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn fragment_roundtrip_preserves_text(text in "[a-zA-Z0-9 ]{1,100}") {
        let doc = new_doc(&text);
        let frag = text_document::DocumentFragment::from_document(&doc).unwrap();
        let frag_text = frag.to_plain_text();
        prop_assert_eq!(&frag_text, &text);

        // Insert into a new doc and verify
        let doc2 = TextDocument::new();
        let cursor = doc2.cursor();
        cursor.insert_fragment(&frag).unwrap();
        let result = doc2.to_plain_text().unwrap();
        prop_assert!(result.contains(&text),
            "fragment insertion should contain original text");
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: unicode text roundtrip and cursor navigation
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Strategy generating text with CJK, Cyrillic, Arabic, accented, and emoji characters.
fn arb_unicode_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            "[a-zA-Z ]{1,10}",                         // Latin
            "[àáâãäåæçèéêëìíîïðñòóôõöùúûüýþÿ]{1,5}",   // accented
            "[абвгдежзийклмнопрстуфхцчшщъыьэюя]{1,5}", // Cyrillic
            "[日本語中文한국어]{1,5}",                 // CJK
            "[🌍🎉🔥💯🚀✨]{1,3}",                     // emoji
        ],
        1..6,
    )
    .prop_map(|parts| parts.join(""))
}

proptest! {
    #[test]
    fn unicode_roundtrip(text in arb_unicode_text()) {
        let doc = new_doc(&text);
        let result = doc.to_plain_text().unwrap();
        prop_assert_eq!(&result, &text);
    }
}

proptest! {
    #[test]
    fn unicode_character_count_correct(text in arb_unicode_text()) {
        let doc = new_doc(&text);
        let expected = doc_char_count(&text);
        prop_assert_eq!(doc.character_count(), expected,
            "character_count mismatch for unicode text");
    }
}

proptest! {
    #[test]
    fn unicode_cursor_navigation_stays_in_bounds(
        text in arb_unicode_text(),
        ops in prop::collection::vec(arb_move_op(), 1..15),
    ) {
        let doc = new_doc(&text);
        let cursor = doc.cursor();
        for op in &ops {
            cursor.move_position(*op, MoveMode::MoveAnchor, 1);
            let pos = cursor.position();
            let len = doc.character_count();
            prop_assert!(pos <= len,
                "unicode cursor position {} exceeds character_count {} after {:?}",
                pos, len, op);
        }
    }
}

proptest! {
    #[test]
    fn unicode_insert_undo_restores(
        base in arb_unicode_text(),
        insert in "[あいうえお]{1,5}",
        pos_frac in 0.0f64..=1.0,
    ) {
        let doc = new_doc(&base);
        let char_count = doc.character_count();
        let pos = (pos_frac * char_count as f64).floor() as usize;
        let pos = pos.min(char_count);

        let cursor = doc.cursor_at(pos);
        cursor.insert_text(&insert).unwrap();
        doc.undo().unwrap();
        let after_undo = doc.to_plain_text().unwrap();
        prop_assert_eq!(&after_undo, &base);
    }
}

proptest! {
    #[test]
    fn unicode_find_matches_are_valid(
        text in arb_unicode_text(),
    ) {
        let doc = new_doc(&text);
        // Search for a substring from the text itself (first 1-3 chars)
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() { return Ok(()); }
        let end = chars.len().min(3);
        let query: String = chars[..end].iter().collect();
        let opts = FindOptions::default();
        let result = doc.find(&query, 0, &opts).unwrap();
        if let Some(m) = result {
            let len = doc.character_count();
            prop_assert!(m.position + m.length <= len,
                "unicode find match out of bounds");
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Property: edit block grouping — single undo reverses all ops
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

proptest! {
    #[test]
    fn edit_block_groups_into_single_undo(
        initial in "[a-zA-Z ]{1,50}",
        inserts in prop::collection::vec("[a-z]{1,5}", 2..6),
    ) {
        let doc = new_doc(&initial);
        // set_plain_text does not push an undo step, so the only
        // undoable action is the edit block we're about to create.
        prop_assert!(!doc.can_undo(),
            "baseline undo stack should be empty after set_plain_text");

        let cursor = doc.cursor_at(doc.character_count());

        cursor.begin_edit_block();
        for text in &inserts {
            cursor.insert_text(text).unwrap();
        }
        cursor.end_edit_block();

        prop_assert!(doc.can_undo(), "edit block should be undoable");

        // Single undo should revert all inserts
        doc.undo().unwrap();
        let restored = doc.to_plain_text().unwrap();
        prop_assert_eq!(&restored, &initial);
        // ...and the stack must be empty — i.e. the N inserts really
        // collapsed into exactly one undo step.
        prop_assert!(!doc.can_undo(),
            "edit block must collapse to a single undo step");
    }
}
