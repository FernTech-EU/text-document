//! `TextCursor::text_before` — a bounded, O(log n) read of the plain text immediately
//! preceding the cursor, straight from the store's rope index. The existing
//! `document_inspection_commands::get_text_at_position` path (what `selected_text` uses)
//! walks every block in the document regardless of how small the requested window is —
//! these tests pin the fast path's output against that same ground truth (via
//! `TextDocument::to_plain_text`, sliced by hand) rather than trusting the block-boundary
//! arithmetic by inspection alone.

use text_document::{MoveMode, TextDocument};

fn new_doc_with_text(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

/// The ground truth: everything strictly before `position` in the full plain text, tail
/// clamped to `max_len` characters. `to_plain_text()` already renders block boundaries as
/// `'\n'` (see `insert_block_creates_new_paragraph` in `cursor_editing_tests.rs`), so this
/// is directly comparable to `text_before`'s own documented `'\n'`-boundary contract.
fn expected_text_before(doc: &TextDocument, position: usize, max_len: usize) -> String {
    let full = doc.to_plain_text().unwrap();
    let prefix: String = full.chars().take(position).collect();
    let total = prefix.chars().count();
    let skip = total.saturating_sub(max_len);
    prefix.chars().skip(skip).collect()
}

#[test]
fn empty_at_document_start() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor_at(0);
    assert_eq!(cursor.text_before(10).unwrap(), "");
}

#[test]
fn max_len_zero_returns_empty_even_mid_document() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor_at(5);
    assert_eq!(cursor.text_before(0).unwrap(), "");
}

#[test]
fn exact_window_within_one_block() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor_at(11);
    assert_eq!(cursor.text_before(5).unwrap(), "world");
}

#[test]
fn window_larger_than_available_text_clamps_to_document_start() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(5);
    assert_eq!(cursor.text_before(1000).unwrap(), "Hello");
}

#[test]
fn window_exactly_at_document_start_boundary() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor_at(5);
    // Exactly the whole prefix, no more, no less.
    assert_eq!(cursor.text_before(5).unwrap(), "Hello");
}

#[test]
fn crosses_a_single_block_boundary_with_newline_separator() {
    let doc = new_doc_with_text("HelloWorld");
    let cursor_split = doc.cursor_at(5);
    cursor_split.insert_block().unwrap();
    assert_eq!(doc.block_count(), 2);
    assert_eq!(doc.to_plain_text().unwrap(), "Hello\nWorld");

    // Cursor at the very end (after "World", position 11 in "Hello\nWorld").
    let cursor = doc.cursor_at(11);
    assert_eq!(cursor.text_before(11).unwrap(), "Hello\nWorld");
    assert_eq!(cursor.text_before(6).unwrap(), "\nWorld");
    assert_eq!(cursor.text_before(1).unwrap(), "d");
}

#[test]
fn reading_right_at_the_second_blocks_own_start_excludes_it_entirely() {
    let doc = new_doc_with_text("HelloWorld");
    let cursor_split = doc.cursor_at(5);
    cursor_split.insert_block().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello\nWorld");

    // Position 6 is exactly the first char of "World" (index 0 within block 2).
    // Reading backward from here must include the separator but NONE of "World".
    let cursor = doc.cursor_at(6);
    assert_eq!(cursor.text_before(10).unwrap(), "Hello\n");
}

#[test]
fn reading_right_at_the_separator_itself_excludes_the_separator() {
    let doc = new_doc_with_text("HelloWorld");
    let cursor_split = doc.cursor_at(5);
    cursor_split.insert_block().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello\nWorld");

    // Position 5 is the separator's own absolute slot — reading backward FROM it must
    // stop just before it, i.e. yield only "Hello" content, no trailing '\n'.
    let cursor = doc.cursor_at(5);
    assert_eq!(cursor.text_before(10).unwrap(), "Hello");
}

#[test]
fn crosses_multiple_block_boundaries() {
    let doc = new_doc_with_text("OneTwoThree");
    doc.cursor_at(3).insert_block().unwrap(); // "One\nTwoThree"
    doc.cursor_at(7).insert_block().unwrap(); // "One\nTwo\nThree"
    assert_eq!(doc.block_count(), 3);
    assert_eq!(doc.to_plain_text().unwrap(), "One\nTwo\nThree");

    let cursor = doc.cursor_at(13);
    assert_eq!(cursor.text_before(13).unwrap(), "One\nTwo\nThree");
    assert_eq!(cursor.text_before(6).unwrap(), "\nThree");
    assert_eq!(cursor.text_before(4).unwrap(), "hree");
}

/// Exhaustive differential check: for a real multi-block, multi-length document, every
/// `(position, max_len)` combination must match a plain slice of the full text. This is the
/// strongest guard against an off-by-one in the block-boundary arithmetic — hand-picked
/// examples above cover the cases that motivated the design, this covers everything else.
#[test]
fn matches_full_plain_text_slice_across_every_position_and_window() {
    let doc = new_doc_with_text("AliceBobCarolDaveEve");
    // Uneven block lengths on purpose, so boundary arithmetic can't accidentally rely on
    // every block being the same size.
    doc.cursor_at(5).insert_block().unwrap(); // "Alice\nBobCarolDaveEve"
    doc.cursor_at(9).insert_block().unwrap(); // "Alice\nBob\nCarolDaveEve"
    doc.cursor_at(15).insert_block().unwrap(); // "Alice\nBob\nCarol\nDaveEve"
    doc.cursor_at(20).insert_block().unwrap(); // "Alice\nBob\nCarol\nDave\nEve"
    assert_eq!(doc.block_count(), 5);
    let full = doc.to_plain_text().unwrap();
    let total = full.chars().count();
    assert_eq!(full, "Alice\nBob\nCarol\nDave\nEve");

    for position in 0..=total {
        let cursor = doc.cursor_at(position);
        for max_len in [0usize, 1, 2, 3, 5, 8, total, total + 5] {
            let got = cursor.text_before(max_len).unwrap();
            let want = expected_text_before(&doc, position, max_len);
            assert_eq!(
                got, want,
                "position={position} max_len={max_len}: got {got:?}, want {want:?}"
            );
        }
    }
}

#[test]
fn uses_the_primary_position_not_the_anchor() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(5, MoveMode::KeepAnchor);
    assert!(cursor.has_selection());
    // position() is 5 (the live end of the selection); text_before must read relative to
    // that, not the anchor at 0.
    assert_eq!(cursor.text_before(5).unwrap(), "Hello");
}
