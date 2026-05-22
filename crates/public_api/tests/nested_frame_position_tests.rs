//! Regression tests for character-position consistency inside nested frames
//! (blockquotes) and around tables.
//!
//! Background: the editor positions its caret using the FLOW snapshot's
//! `position` field (document order). Editing (`delete_previous_char`,
//! `cursor_at`) must resolve that exact position. These previously diverged
//! once a document contained a table — a table disabled the rope fast path for
//! the whole document, and the slow fallback numbered blocks in a different
//! order than the flow snapshot. The visible bug: in the rich-text editor,
//! backspace inside a depth-2 blockquote (sample doc had a table too) deleted
//! a character from an unrelated block instead.
//!
//! The fix makes the rope the single position space for both the flow snapshot
//! and the editing path, including with tables present (cells are mirrored
//! inline; the table itself is a 1-char anchor sentinel).

use text_document::MoveMode;
use text_document::TextDocument;
use text_document::{BlockSnapshot, FlowElementSnapshot};

/// Nested blockquotes to depth 3, with content before and after.
const MD: &str = "# Title\n\nA normal paragraph.\n\n> A single-level blockquote.\n>\n> > Nested blockquote at depth 2. Inline formatting works inside.\n> >\n> > > A third-level nested blockquote, for good measure.\n\n## After\n\nTrailing paragraph at the end.\n";

/// Same document, but with a GFM table after the blockquotes. A table used to
/// disable the rope fast path for the whole document; this mirrors the real
/// rich-text-editor sample where backspace broke from the depth-2 blockquote on.
const MD_WITH_TABLE: &str = "# Title\n\nA normal paragraph.\n\n> A single-level blockquote.\n>\n> > Nested blockquote at depth 2. Inline formatting works inside.\n> >\n> > > A third-level nested blockquote, for good measure.\n\n## A table\n\n| A | B |\n|---|---|\n| c | d |\n\n## After\n\nTrailing paragraph at the end.\n";

/// Collect every block snapshot depth-first in document order — the same walk
/// the layout engine does, so `position` is the FLOW position the editor's
/// caret / hit-test uses.
fn collect_blocks(els: &[FlowElementSnapshot], out: &mut Vec<BlockSnapshot>) {
    for el in els {
        match el {
            FlowElementSnapshot::Block(b) => out.push(b.clone()),
            FlowElementSnapshot::Frame(f) => collect_blocks(&f.elements, out),
            FlowElementSnapshot::Table(t) => {
                for cell in &t.cells {
                    out.extend(cell.blocks.iter().cloned());
                }
            }
        }
    }
}

/// The FLOW position of the first block whose text contains `needle` — i.e.
/// what the editor would have computed from a click / caret in that block.
fn flow_position_of(doc: &TextDocument, needle: &str) -> usize {
    let snap = doc.snapshot_flow();
    let mut blocks = Vec::new();
    collect_blocks(&snap.elements, &mut blocks);
    blocks
        .into_iter()
        .find(|b| b.text.contains(needle))
        .map(|b| b.position)
        .expect("block with needle present in flow")
}

/// Backspace at a caret 5 chars into the named block, positioning by FLOW
/// position (as the editor does), and assert the edit lands in that block.
/// caret = flow_pos + 5 sits before index 5, so backspace removes index 4.
fn assert_backspace_lands_in(md: &str, needle: &str, expected_after_fragment: &str) {
    let doc = TextDocument::new();
    doc.set_markdown(md).unwrap().wait().unwrap();

    let flow_pos = flow_position_of(&doc, needle);
    let caret = flow_pos + 5;
    let before_len = doc.to_plain_text().unwrap().chars().count();

    let cursor = doc.cursor_at(caret);
    cursor.set_position(caret, MoveMode::MoveAnchor);
    cursor.delete_previous_char().unwrap();

    let after = doc.to_plain_text().unwrap();
    assert_eq!(
        after.chars().count(),
        before_len - 1,
        "backspace at flow pos {caret} must remove exactly one char"
    );
    assert!(
        after.contains(expected_after_fragment),
        "backspace at flow pos {caret} must edit the '{needle}' block, not another \
         block (expected fragment {expected_after_fragment:?}; got: {after:?})"
    );
}

#[test]
fn backspace_in_depth2_blockquote() {
    // "Nested" + caret@5 → delete index 4 ('e') → "Nestd".
    assert_backspace_lands_in(MD, "Nested blockquote at depth 2", "Nestd blockquote at depth 2");
}

#[test]
fn backspace_in_depth2_blockquote_with_table_present() {
    // The reported bug: a table in the doc must not break backspace in the
    // depth-2 blockquote (which sits before the table in document order).
    assert_backspace_lands_in(
        MD_WITH_TABLE,
        "Nested blockquote at depth 2",
        "Nestd blockquote at depth 2",
    );
}

#[test]
fn backspace_after_table_with_blockquotes_present() {
    // "Trailing paragraph" sits AFTER the table; its flow position is past the
    // table's inline cells + the anchor sentinel. The edit must still land in
    // it. "Trailing" + caret@5 → delete index 4 ('l') → "Traiing".
    assert_backspace_lands_in(MD_WITH_TABLE, "Trailing paragraph at the end", "Traiing paragraph at the end");
}

#[test]
fn backspace_in_trailing_paragraph_no_table() {
    assert_backspace_lands_in(MD, "Trailing paragraph at the end", "Traiing paragraph at the end");
}