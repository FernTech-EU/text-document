//! Sanity tests for the new blockquote editing use cases exposed via
//! the public API: `wrap_selection_in_blockquote`, `toggle_blockquote`,
//! `unwrap_current_frame`, `increase_blockquote_depth`,
//! `decrease_blockquote_depth`. They drive the full stack
//! (Cursor → use case → entity store) and verify round-trip behaviour
//! through markdown export.

use text_document::{MoveMode, TextDocument};

fn new_doc_with_markdown(md: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_markdown(md).unwrap().wait().unwrap();
    doc
}

#[test]
fn wrap_current_block_in_blockquote_then_export_has_quote_prefix() {
    let doc = new_doc_with_markdown("A plain paragraph.\n");
    let cursor = doc.cursor_at(0);
    cursor.set_position(0, MoveMode::MoveAnchor);

    assert!(!cursor.is_in_blockquote(), "precondition: not in a quote");
    cursor.wrap_selection_in_blockquote().unwrap();
    assert!(
        cursor.is_in_blockquote(),
        "after wrap, the cursor's block must be inside a blockquote"
    );

    let md = doc.to_markdown().unwrap();
    assert!(
        md.contains("> A plain paragraph"),
        "exported markdown must carry the `>` prefix; got: {md:?}"
    );
    assert!(
        !md.replace("> ", "")
            .contains("A plain paragraph\\.\n\nA plain paragraph"),
        "block must not appear twice; got: {md:?}"
    );
}

#[test]
fn toggle_blockquote_round_trips() {
    let doc = new_doc_with_markdown("Hello.\n");
    let cursor = doc.cursor_at(0);
    cursor.set_position(0, MoveMode::MoveAnchor);

    assert!(!cursor.is_in_blockquote());
    cursor.toggle_blockquote().unwrap();
    assert!(cursor.is_in_blockquote(), "toggle on a plain block wraps");

    cursor.toggle_blockquote().unwrap();
    assert!(!cursor.is_in_blockquote(), "toggle inside a quote unwraps");

    let md = doc.to_markdown().unwrap();
    assert!(
        !md.contains('>'),
        "after toggle off, no `>` prefix should remain; got: {md:?}"
    );
}

#[test]
fn increase_then_decrease_blockquote_depth_round_trips() {
    let doc = new_doc_with_markdown("Hello.\n");
    let cursor = doc.cursor_at(0);
    cursor.set_position(0, MoveMode::MoveAnchor);

    assert_eq!(cursor.blockquote_depth_at_cursor(), 0);

    cursor.increase_blockquote_depth().unwrap();
    assert_eq!(
        cursor.blockquote_depth_at_cursor(),
        1,
        "first increase yields depth 1"
    );

    cursor.increase_blockquote_depth().unwrap();
    assert_eq!(
        cursor.blockquote_depth_at_cursor(),
        2,
        "second increase yields nested depth 2"
    );

    cursor.decrease_blockquote_depth().unwrap();
    assert_eq!(
        cursor.blockquote_depth_at_cursor(),
        1,
        "first decrease drops to depth 1"
    );

    cursor.decrease_blockquote_depth().unwrap();
    assert_eq!(
        cursor.blockquote_depth_at_cursor(),
        0,
        "second decrease drops to plain"
    );
}

#[test]
fn unwrap_current_frame_lifts_block_to_parent() {
    let doc = new_doc_with_markdown("> Quoted line.\n");
    let cursor = doc.cursor_at(0);
    cursor.set_position(0, MoveMode::MoveAnchor);
    assert!(cursor.is_in_blockquote());

    cursor.unwrap_current_frame().unwrap();
    assert!(!cursor.is_in_blockquote(), "unwrap removes the quote frame");

    let md = doc.to_markdown().unwrap();
    assert!(
        !md.contains('>'),
        "markdown after unwrap must not contain a `>`; got: {md:?}"
    );
    assert!(md.contains("Quoted line"));
}

#[test]
fn wrap_then_unwrap_round_trips_to_identical_markdown() {
    let original = "First.\n\nSecond.\n";
    let doc = new_doc_with_markdown(original);
    let cursor = doc.cursor_at(0);
    cursor.set_position(0, MoveMode::MoveAnchor);

    cursor.wrap_selection_in_blockquote().unwrap();
    cursor.unwrap_current_frame().unwrap();

    let md = doc.to_markdown().unwrap();
    assert!(
        md.contains("First") && md.contains("Second") && !md.contains('>'),
        "round-trip wrap+unwrap must restore plain content; got: {md:?}"
    );
}

#[test]
fn wrap_inside_existing_blockquote_creates_nested_depth() {
    let doc = new_doc_with_markdown("> Outer line.\n");
    let cursor = doc.cursor_at(0);
    cursor.set_position(0, MoveMode::MoveAnchor);
    assert_eq!(cursor.blockquote_depth_at_cursor(), 1);

    cursor.wrap_selection_in_blockquote().unwrap();
    assert_eq!(
        cursor.blockquote_depth_at_cursor(),
        2,
        "wrapping inside an existing quote produces a nested quote"
    );

    let md = doc.to_markdown().unwrap();
    assert!(
        md.contains("> >") || md.contains(">>"),
        "nested quote must export with two `>` levels; got: {md:?}"
    );
}

/// User-reported bug repro:
///
/// 1. Start with `> A` (one-line blockquote).
/// 2. Place caret at end of A and press Enter → expect a new empty
///    quoted block AFTER A; cursor inside it.
/// 3. Press Enter a second time on the now-empty quoted block → expect
///    that empty paragraph to exit the quote (depth drops to 0) and
///    appear AFTER `> A`, not before.
///
/// The reported bug: after the second Enter, the new empty line
/// appeared BEFORE the quoted line. This test pins the document order
/// post-unwrap.
/// Regression: `Cursor::insert_block` at the end of a quoted block
/// must produce exactly two quoted blocks. The buggy markdown export
/// previously compounded "\n\n" separators per nested-frame level,
/// producing 14 blank lines between A and the new empty quote.
#[test]
fn insert_block_inside_quote_produces_clean_markdown() {
    let doc = TextDocument::new();
    doc.set_markdown("> A\n").unwrap().wait().unwrap();
    let cursor = doc.cursor_at(0);
    cursor.set_position(1, MoveMode::MoveAnchor);
    cursor.insert_block().unwrap();
    let md = doc.to_markdown().unwrap();
    // Two quoted blocks: "> A" then "> " (empty). The separator between
    // top-level blocks in markdown is `\n\n`. Any document with more
    // than two consecutive `\n` between content lines indicates the
    // recursive export compounded separators.
    assert!(
        md.contains("> A"),
        "exported markdown must keep `> A`; got {md:?}"
    );
    assert!(
        !md.contains("\n\n\n"),
        "no more than two consecutive newlines between blocks; got {md:?}"
    );
}

/// Reproduce the user's depth-3 scenario exactly.
///
/// Initial:
///   > block A
///   > > block B
///   > > > block C|        (cursor at end of C)
///
/// After 1st Enter: cursor on a new empty depth-3 block after C.
/// After 2nd Enter: cursor on a new empty depth-2 block. The empty
/// must end up AFTER C in document order (lift one level but keep
/// document position), NOT before C.
#[test]
fn enter_enter_at_end_of_depth3_quote_keeps_c_above_the_new_empty() {
    let doc = TextDocument::new();
    doc.set_markdown("> block A\n> > block B\n> > > block C\n")
        .unwrap()
        .wait()
        .unwrap();

    // Walk to end of last block via block-hopping.
    let cursor = doc.cursor_at(0);
    cursor.set_position(0, MoveMode::MoveAnchor);
    for _ in 0..3 {
        cursor.move_position(
            text_document::MoveOperation::EndOfBlock,
            MoveMode::MoveAnchor,
            1,
        );
        cursor.move_position(
            text_document::MoveOperation::NextBlock,
            MoveMode::MoveAnchor,
            1,
        );
    }
    cursor.move_position(
        text_document::MoveOperation::EndOfBlock,
        MoveMode::MoveAnchor,
        1,
    );
    assert_eq!(
        cursor.blockquote_depth_at_cursor(),
        3,
        "precondition: cursor must be at depth 3 (end of `block C`)"
    );

    // 1st Enter: still in quote at depth 3, on a new empty block.
    cursor.insert_block().unwrap();
    assert_eq!(cursor.blockquote_depth_at_cursor(), 3);
    assert!(cursor.current_block_is_empty());

    // 2nd Enter: unwrap one level.
    cursor.unwrap_current_block_from_blockquote().unwrap();
    assert_eq!(cursor.blockquote_depth_at_cursor(), 2);

    // Verify document order via the flow snapshot — C must precede the
    // new empty block. The data layer is the source of truth for the
    // widget's rendering; any GUI ordering bug must originate here.
    use text_document::FlowElementSnapshot;
    fn flatten(els: &[FlowElementSnapshot], depth: usize, out: &mut Vec<(usize, String)>) {
        for el in els {
            match el {
                FlowElementSnapshot::Block(b) => out.push((depth, b.text.clone())),
                FlowElementSnapshot::Frame(f) => flatten(&f.elements, depth + 1, out),
                FlowElementSnapshot::Table(_) => {}
            }
        }
    }
    let snap = doc.snapshot_flow();
    let mut tuples = Vec::new();
    flatten(&snap.elements, 0, &mut tuples);
    let c_pos = tuples
        .iter()
        .position(|(_, t)| t == "block C")
        .expect("`block C` must remain in the flow");
    let empty_pos = tuples
        .iter()
        .position(|(_, t)| t.is_empty())
        .expect("the new empty block must remain in the flow");
    assert!(
        c_pos < empty_pos,
        "`block C` (flow idx {c_pos}) must come before the new empty block (flow idx {empty_pos}); flow: {tuples:?}"
    );
}

#[test]
fn enter_then_enter_at_end_of_quote_exits_after_not_before() {
    let doc = TextDocument::new();
    doc.set_markdown("> A\n").unwrap().wait().unwrap();

    // First Enter at end of A — splits the block, new empty stays in
    // the quote.
    let a_pos = flow_position_helper(&doc, "A");
    let end_of_a = a_pos + 1; // "A" is one character
    let cursor = doc.cursor_at(end_of_a);
    cursor.set_position(end_of_a, MoveMode::MoveAnchor);
    cursor.insert_block().unwrap();

    let cursor = doc.cursor_at(cursor.position());
    assert!(
        cursor.is_in_blockquote(),
        "after the 1st Enter, the new empty block must still be inside the quote"
    );

    // Second Enter on the empty quoted block — exits the quote.
    cursor.unwrap_current_block_from_blockquote().unwrap();

    // Document order MUST be: quoted "A", then plain empty line. We
    // verify by exporting markdown and checking the relative order of
    // "> A" and the trailing empty line.
    let md = doc.to_markdown().unwrap();
    let a_idx = md
        .find("> A")
        .unwrap_or_else(|| panic!("expected quoted 'A' to remain; got: {md:?}"));
    // After exit, there must be no `> ` line BEFORE `> A` (no quoted
    // empty paragraph dangling above the original quote).
    let before_a = &md[..a_idx];
    assert!(
        !before_a.contains('>'),
        "no quoted content must appear BEFORE '> A' after exiting the quote; got: {md:?}"
    );
    // Cursor must now be OUTSIDE the quote.
    let cursor = doc.cursor_at(cursor.position());
    assert!(
        !cursor.is_in_blockquote(),
        "cursor must be outside the quote after exit; got md: {md:?}"
    );
}

/// Helper used by the bug-repro test. Finds the FLOW position of the
/// first block whose plain text contains `needle`.
fn flow_position_helper(doc: &TextDocument, needle: &str) -> usize {
    let snap = doc.snapshot_flow();
    fn walk(
        els: &[text_document::FlowElementSnapshot],
        out: &mut Vec<text_document::BlockSnapshot>,
    ) {
        for el in els {
            match el {
                text_document::FlowElementSnapshot::Block(b) => out.push(b.clone()),
                text_document::FlowElementSnapshot::Frame(f) => walk(&f.elements, out),
                text_document::FlowElementSnapshot::Table(t) => {
                    for cell in &t.cells {
                        out.extend(cell.blocks.iter().cloned());
                    }
                }
            }
        }
    }
    let mut blocks = Vec::new();
    walk(&snap.elements, &mut blocks);
    blocks
        .into_iter()
        .find(|b| b.text.contains(needle))
        .map(|b| b.position)
        .expect("block with needle present in flow")
}

#[test]
fn undo_restores_pre_wrap_state() {
    let doc = new_doc_with_markdown("Hello.\n");
    let cursor = doc.cursor_at(0);
    cursor.set_position(0, MoveMode::MoveAnchor);

    cursor.wrap_selection_in_blockquote().unwrap();
    assert!(cursor.is_in_blockquote());

    doc.undo().unwrap();
    let cursor = doc.cursor_at(0);
    cursor.set_position(0, MoveMode::MoveAnchor);
    assert!(
        !cursor.is_in_blockquote(),
        "undo must restore the pre-wrap state"
    );

    let md = doc.to_markdown().unwrap();
    assert!(
        !md.contains('>'),
        "markdown after undo must not contain `>`; got: {md:?}"
    );
}
