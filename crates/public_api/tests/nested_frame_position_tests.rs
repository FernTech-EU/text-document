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
    assert_backspace_lands_in(
        MD,
        "Nested blockquote at depth 2",
        "Nestd blockquote at depth 2",
    );
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
    assert_backspace_lands_in(
        MD_WITH_TABLE,
        "Trailing paragraph at the end",
        "Traiing paragraph at the end",
    );
}

#[test]
fn backspace_in_trailing_paragraph_no_table() {
    assert_backspace_lands_in(
        MD,
        "Trailing paragraph at the end",
        "Traiing paragraph at the end",
    );
}

/// A1 regression: a Backspace whose char range crosses a blockquote
/// boundary used to update only the root frame's `child_order`, leaving
/// the sub-frame with a dangling entry. The follow-up operation would
/// then act on a stale block id. Sequence: place the caret at flow
/// position 0 of the first quoted paragraph and Backspace — that merges
/// the quoted block into the preceding non-quoted paragraph and removes
/// the quoted block from the entity store. After the merge, perform a
/// second Backspace at the same caret to assert that subsequent edits
/// stay consistent — under the old code the second op would observe a
/// corrupt `child_order` and either panic or land in the wrong block.
#[test]
fn cross_frame_backspace_then_followup_stays_consistent() {
    let doc = TextDocument::new();
    doc.set_markdown(MD).unwrap().wait().unwrap();

    let quote_pos = flow_position_of(&doc, "A single-level blockquote");
    let before = doc.to_plain_text().unwrap();

    let cursor = doc.cursor_at(quote_pos);
    cursor.set_position(quote_pos, MoveMode::MoveAnchor);
    cursor.delete_previous_char().unwrap();

    // A follow-up Backspace at the new cursor position must succeed and
    // remove exactly one more character — proving the structural state
    // after the cross-frame merge is internally consistent.
    let pos_after_first = cursor.position();
    cursor.set_position(pos_after_first, MoveMode::MoveAnchor);
    cursor.delete_previous_char().unwrap();

    let after = doc.to_plain_text().unwrap();
    assert_eq!(
        after.chars().count(),
        before.chars().count() - 2,
        "two cross-frame backspaces must remove exactly two characters total"
    );
}

/// A2 regression: deleting all the content of a nested (depth-2)
/// blockquote in one sweep used to leave the depth-2 sub-frame in the
/// entity store with `blocks=[]` because the auto-prune walked only the
/// root frame's `child_order`. The recursive prune walks all depths.
/// We exercise this by selecting from before the depth-2 quote to after
/// it and deleting, then checking the document still round-trips and a
/// subsequent operation does not crash.
#[test]
fn delete_spanning_depth2_quote_prunes_all_levels() {
    let doc = TextDocument::new();
    doc.set_markdown(MD).unwrap().wait().unwrap();

    // Select from the start of the depth-2 quoted paragraph all the way
    // to its end; this removes every block inside it.
    let quote_pos = flow_position_of(&doc, "Nested blockquote at depth 2");
    let end_pos = quote_pos + "Nested blockquote at depth 2. Inline formatting works inside.".len();

    let cursor = doc.cursor_at(quote_pos);
    cursor.set_position(quote_pos, MoveMode::MoveAnchor);
    cursor.set_position(end_pos, MoveMode::KeepAnchor);
    cursor.remove_selected_text().unwrap();

    // A follow-up Backspace should land somewhere reasonable and not
    // panic, even though the depth-2 (and possibly depth-3) frames were
    // emptied.
    let after_pos = cursor.position();
    cursor.set_position(after_pos, MoveMode::MoveAnchor);
    cursor.delete_previous_char().unwrap();

    let plain = doc.to_plain_text().unwrap();
    assert!(
        !plain.contains("Nested blockquote at depth 2."),
        "the depth-2 quote text must be gone after the delete"
    );
}

/// A3 regression: inserting a sub-frame via `Cursor::insert_frame()`
/// used to skip rope mirroring for the new block, disabling the rope
/// fast path for ALL subsequent cursor ops. The fix mirrors the new
/// block to the rope at the correct byte position. Verify by inserting
/// a sub-frame and then exercising a backspace that requires the fast
/// path to land in the right block.
#[test]
fn insert_frame_inside_document_keeps_fast_path_alive() {
    let doc = TextDocument::new();
    doc.set_markdown(MD).unwrap().wait().unwrap();

    // Insert a sub-frame somewhere inside the document (e.g. in the
    // middle of "A normal paragraph.").
    let para_pos = flow_position_of(&doc, "A normal paragraph");
    let cursor = doc.cursor_at(para_pos + 5);
    cursor.set_position(para_pos + 5, MoveMode::MoveAnchor);
    cursor.insert_frame().unwrap();

    // The trailing paragraph still has to be reachable by a backspace
    // that lands in it — that requires the rope fast path to be active.
    assert_backspace_lands_in(
        MD,
        "Trailing paragraph at the end",
        "Traiing paragraph at the end",
    );
    // (The above re-imports MD into a fresh doc; we also check our
    // mutated doc stays usable by performing a backspace there.)
    let trail = flow_position_of(&doc, "Trailing paragraph at the end");
    let cursor = doc.cursor_at(trail + 5);
    cursor.set_position(trail + 5, MoveMode::MoveAnchor);
    cursor.delete_previous_char().unwrap();
    let plain = doc.to_plain_text().unwrap();
    assert!(
        plain.contains("Traiing paragraph at the end"),
        "after insert_frame in a nested context, backspace must still land in the right block; got: {plain:?}"
    );
}
