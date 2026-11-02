// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! A plain Left or Right does not stop on the positions a table's anchor takes in the text.
//!
//! In "abcdef" followed by a table, the end of the paragraph is 6, the anchor is 7, the
//! boundary after it is 8, and the first cell starts at 9. A caret on 7 or 8 reads as standing
//! in the first cell, as it does at 9, so Right from 6 took three presses to move from the
//! paragraph into the table, two of them without the caret going anywhere, and a screen
//! reader met two empty stops on the way. The move now goes from 6 to 9 in one press and
//! back in one, and a move that keeps the anchor still stops on the anchor, so that a
//! selection can take a table whole.

use proptest::prelude::*;
use text_document::{MoveMode, MoveOperation, TextDocument};

const TABLE_AFTER_PARAGRAPH: &str = "abcdef\n\n| a1 | b1 |\n| c1 | d1 |\n\nAfter.\n";

fn loaded(djot: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_sync(djot).unwrap();
    doc
}

fn position_in(doc: &TextDocument, needle: &str) -> usize {
    let text = doc.to_addressable_text().unwrap();
    text[..text.find(needle).expect("the text is in the document")]
        .chars()
        .count()
}

/// The caret at `from` after `operation` repeated `n` times without keeping the anchor,
/// and whether it moved.
fn plain(doc: &TextDocument, from: usize, operation: MoveOperation, n: usize) -> (usize, bool) {
    let cursor = doc.cursor_at(from);
    assert_eq!(cursor.position(), from, "the caret is placed where asked");
    let moved = cursor.move_position(operation, MoveMode::MoveAnchor, n);
    (cursor.position(), moved)
}

struct Places {
    /// The end of the paragraph before the table.
    paragraph_end: usize,
    /// The table's anchor, then the boundary after it.
    anchor: usize,
    after_anchor: usize,
    /// The start of the first cell.
    cell: usize,
}

fn places(doc: &TextDocument) -> Places {
    let cell = position_in(doc, "a1");
    let paragraph_end = position_in(doc, "abcdef") + "abcdef".len();
    assert_eq!(
        cell,
        paragraph_end + 3,
        "the layout the tests are written for"
    );
    Places {
        paragraph_end,
        anchor: paragraph_end + 1,
        after_anchor: paragraph_end + 2,
        cell,
    }
}

#[test]
fn right_goes_from_the_paragraph_to_the_first_cell_in_one_step() {
    let doc = loaded(TABLE_AFTER_PARAGRAPH);
    let p = places(&doc);
    for operation in [MoveOperation::Right, MoveOperation::NextCharacter] {
        assert_eq!(plain(&doc, p.paragraph_end, operation, 1), (p.cell, true));
        // The step over the table counts once: the second step is the first cell's first
        // character.
        assert_eq!(
            plain(&doc, p.paragraph_end, operation, 2),
            (p.cell + 1, true)
        );
    }
}

#[test]
fn left_goes_from_the_first_cell_to_the_paragraph_in_one_step() {
    let doc = loaded(TABLE_AFTER_PARAGRAPH);
    let p = places(&doc);
    for operation in [MoveOperation::Left, MoveOperation::PreviousCharacter] {
        assert_eq!(plain(&doc, p.cell, operation, 1), (p.paragraph_end, true));
        assert_eq!(
            plain(&doc, p.cell, operation, 2),
            (p.paragraph_end - 1, true)
        );
    }
}

#[test]
fn right_then_left_is_the_start() {
    let doc = loaded(TABLE_AFTER_PARAGRAPH);
    let p = places(&doc);
    let cursor = doc.cursor_at(p.paragraph_end);
    cursor.move_position(MoveOperation::Right, MoveMode::MoveAnchor, 1);
    cursor.move_position(MoveOperation::Left, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), p.paragraph_end);
}

/// A caret on the anchor (placed there with `set_position`, or left by a Shift move back into
/// the table from after it) stands in the first cell already: Right takes it on from there, and Left to the paragraph before.
#[test]
fn a_caret_on_the_anchor_moves_on_from_the_first_cell() {
    let doc = loaded(TABLE_AFTER_PARAGRAPH);
    let p = places(&doc);
    for on_the_table_edge in [p.anchor, p.after_anchor] {
        let (right, moved) = plain(&doc, on_the_table_edge, MoveOperation::Right, 1);
        assert!(moved);
        assert_eq!(right, p.cell + 1, "from {on_the_table_edge}");
        assert_ne!(
            right, p.cell,
            "a press that stays in the first cell is a dead key"
        );
        assert_eq!(
            plain(&doc, on_the_table_edge, MoveOperation::Left, 1),
            (p.paragraph_end, true),
            "from {on_the_table_edge}"
        );
    }
}

/// A move keeping the anchor is not skipped: Shift+Right from the paragraph's end still
/// meets the table, which the table trap takes whole (to its far edge), where a plain Right
/// goes into the first cell.
#[test]
fn a_move_keeping_the_anchor_is_not_skipped() {
    let doc = loaded(TABLE_AFTER_PARAGRAPH);
    let p = places(&doc);
    let cursor = doc.cursor_at(p.paragraph_end);
    cursor.move_position(MoveOperation::Right, MoveMode::KeepAnchor, 1);
    assert_ne!(
        cursor.position(),
        p.cell,
        "Shift+Right must not go to the first cell"
    );
    assert!(
        cursor.position() > p.cell,
        "the selection takes the table whole: {}",
        cursor.position()
    );
    assert!(cursor.selection().to_plain_text().contains("d1"));

    // Backward: Shift+Left from the first cell's start steps onto the table's edge, where a
    // plain Left goes on to the end of the paragraph.
    let cursor = doc.cursor_at(p.cell);
    cursor.move_position(MoveOperation::Left, MoveMode::KeepAnchor, 1);
    assert_ne!(
        cursor.position(),
        p.paragraph_end,
        "Shift+Left must not skip the table's edge"
    );
}

/// A step of zero moves nothing, even from the anchor.
#[test]
fn zero_steps_from_the_anchor_move_nothing() {
    let doc = loaded(TABLE_AFTER_PARAGRAPH);
    let p = places(&doc);
    assert_eq!(
        plain(&doc, p.anchor, MoveOperation::Right, 0),
        (p.anchor, false)
    );
}

#[test]
fn typing_after_the_step_lands_in_the_first_cell() {
    let doc = loaded(TABLE_AFTER_PARAGRAPH);
    let p = places(&doc);
    let cursor = doc.cursor_at(p.paragraph_end);
    cursor.move_position(MoveOperation::Right, MoveMode::MoveAnchor, 1);
    cursor.insert_text("X").unwrap();
    assert!(doc.to_plain_text().unwrap().contains("Xa1"));
    assert!(doc.to_plain_text().unwrap().contains("abcdef"));
}

/// The deletions are not moves: they keep the range they had at the table's edge, which
/// removes nothing of the table. (They call the boundary helpers directly; this pins that the
/// skip stays out of them.)
#[test]
fn a_deletion_at_a_tables_edge_leaves_the_table_whole() {
    let doc = loaded(TABLE_AFTER_PARAGRAPH);
    let p = places(&doc);
    let before = doc.to_djot().unwrap();
    assert_eq!(
        before,
        "abcdef\n\n| a1 | b1 |\n|---|---|\n| c1 | d1 |\n\nAfter."
    );
    doc.cursor_at(p.paragraph_end).delete_char().unwrap();
    assert_eq!(
        doc.to_djot().unwrap(),
        before,
        "Delete at the paragraph's end"
    );
    doc.cursor_at(p.cell).delete_previous_char().unwrap();
    assert_eq!(
        doc.to_djot().unwrap(),
        before,
        "Backspace at the first cell's start"
    );
}

#[test]
fn a_table_that_opens_the_document_has_nothing_before_it() {
    let doc = loaded("| a1 | b1 |\n| c1 | d1 |\n\nAfter.\n");
    let cell = position_in(&doc, "a1");
    // Left from the first cell's start stays.
    assert_eq!(plain(&doc, cell, MoveOperation::Left, 1), (cell, false));
    assert_eq!(plain(&doc, cell, MoveOperation::Left, 3), (cell, false));
    // The caret at the document start is on the anchor, in the first cell: Right goes on.
    let (right, moved) = plain(&doc, 0, MoveOperation::Right, 1);
    assert!(moved);
    assert_eq!(right, cell + 1);
    // Left from the boundary after the anchor has nothing before it either.
    assert_eq!(plain(&doc, 1, MoveOperation::Left, 1), (1, false));
}

/// A table that ends the text has nothing after it: Right from the end of its last cell
/// stays, and the step before the end is a step.
#[test]
fn a_table_at_the_end_of_the_text_keeps_its_end() {
    let doc = loaded("abcdef\n\n| a1 | b1 |\n");
    let end = position_in(&doc, "b1") + 2;
    assert_eq!(plain(&doc, end, MoveOperation::Right, 1), (end, false));
    assert_eq!(plain(&doc, end - 1, MoveOperation::Right, 1), (end, true));
    assert_eq!(plain(&doc, end, MoveOperation::Right, 3), (end, false));
}

/// The same with footnote bodies after the table: no view shows them, and a move from the
/// main text stops at its end.
#[test]
fn a_table_before_footnote_bodies_keeps_the_end_of_the_main_text() {
    let doc = loaded("x[^n]\n\n| a1 |\n\n[^n]: the note\n");
    let end = position_in(&doc, "a1") + 2;
    assert_eq!(plain(&doc, end, MoveOperation::Right, 1), (end, false));
    assert_eq!(plain(&doc, end - 1, MoveOperation::Right, 1), (end, true));
}

/// A table after a list item is crossed in one step each way, and the item's list is not
/// disturbed.
#[test]
fn a_table_after_a_list_item_is_crossed_in_one_step_each_way() {
    let doc = loaded("- item\n\n  | a1 | b1 |\n\n  more\n\n- two\n");
    let item_end = position_in(&doc, "item") + 4;
    let cell = position_in(&doc, "a1");
    assert_eq!(plain(&doc, item_end, MoveOperation::Right, 1), (cell, true));
    assert_eq!(plain(&doc, cell, MoveOperation::Left, 1), (item_end, true));
    assert!(doc.block_by_number(0).unwrap().list().is_some());
}

/// A table in a quotation is crossed as one at the top level is.
#[test]
fn a_table_in_a_quotation_is_crossed_in_one_step_each_way() {
    let doc = loaded(
        "> abcdef
>
> | a1 | b1 |

After.
",
    );
    let paragraph_end = position_in(&doc, "abcdef") + 6;
    let cell = position_in(&doc, "a1");
    assert_eq!(
        plain(&doc, paragraph_end, MoveOperation::Right, 1),
        (cell, true)
    );
    assert_eq!(
        plain(&doc, cell, MoveOperation::Left, 1),
        (paragraph_end, true)
    );
}

#[test]
fn an_empty_first_cell_is_a_step() {
    let doc = loaded("abcdef\n\n| | b1 |\n\n");
    let paragraph_end = position_in(&doc, "abcdef") + 6;
    let (right, moved) = plain(&doc, paragraph_end, MoveOperation::Right, 1);
    assert!(moved);
    assert_eq!(
        right,
        paragraph_end + 3,
        "the step lands in the empty first cell, not on the boundary after the anchor"
    );
    assert_eq!(
        plain(&doc, right, MoveOperation::Left, 1),
        (paragraph_end, true)
    );
}

/// Down from the paragraph before a table goes into its first cell, as Right does.
#[test]
fn down_from_the_paragraph_goes_into_the_first_cell() {
    let doc = loaded(TABLE_AFTER_PARAGRAPH);
    let p = places(&doc);
    for operation in [MoveOperation::Down, MoveOperation::NextBlock] {
        assert_eq!(plain(&doc, p.paragraph_end, operation, 1), (p.cell, true));
    }
}

/// The other operations are as they were: they never stopped on the anchor (or still do),
/// with their values written out so that a change of any is seen.
#[test]
fn the_other_moves_are_as_they_were() {
    let doc = loaded(TABLE_AFTER_PARAGRAPH);
    let p = places(&doc);
    let a1_end = p.cell + 2;
    // Up and the previous block already backed off the anchor: to the first paragraph.
    assert_eq!(plain(&doc, p.cell, MoveOperation::Up, 1), (0, true));
    assert_eq!(
        plain(&doc, p.cell, MoveOperation::PreviousBlock, 1),
        (0, true)
    );
    // Words: Ctrl+Right from the paragraph's end lands at the end of the first cell's word,
    // Ctrl+Left from the first cell at the start of the paragraph's.
    assert_eq!(
        plain(&doc, p.paragraph_end, MoveOperation::NextWord, 1),
        (a1_end, true)
    );
    assert_eq!(
        plain(&doc, p.cell, MoveOperation::PreviousWord, 1),
        (0, true)
    );
    assert_eq!(plain(&doc, p.cell, MoveOperation::WordLeft, 1), (0, true));
    // A sentence back from the first cell.
    assert_eq!(
        plain(&doc, p.cell, MoveOperation::PreviousSentence, 1),
        (0, true)
    );
    // Home and End of a block, from a caret on the anchor: the first cell's edges.
    for on_the_anchor in [p.anchor, p.after_anchor] {
        assert_eq!(
            plain(&doc, on_the_anchor, MoveOperation::StartOfBlock, 1),
            (p.cell, true)
        );
    }
    assert_eq!(
        plain(&doc, p.anchor, MoveOperation::EndOfBlock, 1),
        (a1_end, true)
    );
    // The document's two ends.
    assert_eq!(plain(&doc, p.cell, MoveOperation::Start, 1), (0, true));
}

/// In a document that opens with a table, `Start` and a word to the left from the first cell
/// still land on the table's anchor, position 0, where the caret reads as in the first cell
/// without having moved. Not changed by the skip; written down so that a change is seen.
#[test]
fn start_and_word_left_in_a_document_opening_with_a_table_land_on_the_anchor() {
    let doc = loaded("| a1 | b1 |\n\nAfter.\n");
    let cell = position_in(&doc, "a1");
    assert_eq!(plain(&doc, cell, MoveOperation::Start, 1), (0, true));
    assert_eq!(plain(&doc, cell, MoveOperation::WordLeft, 1), (0, true));
}

/// Over a document with a table, Right then Left (and Left then Right) from any position
/// that is not on an anchor is the identity, and no plain Right or Left ends on one.
/// This is the helper of the property below: whether `position` is on an anchor.
fn on_an_anchor(doc: &TextDocument, position: usize) -> bool {
    let cursor = doc.cursor_at(position);
    // A caret on an anchor reads as in the first cell, so its table is the cell's table
    // but its position is before the cell's first block.
    cursor.position() == position
        && cursor.current_table_cell().is_some_and(|cell| {
            let first = cell
                .table
                .cell(0, 0)
                .and_then(|c| c.blocks().first().map(|b| b.position()));
            first.is_some_and(|f| {
                f > position && f - position <= 2 && cell.row == 0 && cell.column == 0
            })
        })
}

proptest! {
    #[test]
    fn plain_moves_across_tables_round_trip_and_never_stop_on_an_anchor(
        shape in 0usize..6,
        step in 0.0f64..=1.0,
    ) {
        let djot = [
            TABLE_AFTER_PARAGRAPH,
            "| a1 | b1 |\n\nx\n\n| z9 |\n\nend\n",
            "| a1 |\n\n| z9 |\n\nend\n",
            "abc\n\n| | b1 |\n| c1 | d1 |\n\ntext\n",
            "- item\n\n  | a1 | b1 |\n\n  more\n\n- two\n",
            "x[^n]\n\n| a1 |\n\n[^n]: the note\n",
        ][shape];
        let doc = loaded(djot);
        let max = {
            let end = doc.cursor();
            end.move_position(MoveOperation::End, MoveMode::MoveAnchor, 1);
            end.position()
        };
        let start = ((step * max as f64).floor() as usize).min(max);
        prop_assume!(!on_an_anchor(&doc, start));
        let cursor = doc.cursor_at(start);
        let begin = cursor.position();
        prop_assume!(!on_an_anchor(&doc, begin));
        if cursor.move_position(MoveOperation::Right, MoveMode::MoveAnchor, 1) {
            let there = cursor.position();
            prop_assert!(!on_an_anchor(&doc, there), "Right ended on an anchor at {}", there);
            cursor.move_position(MoveOperation::Left, MoveMode::MoveAnchor, 1);
            prop_assert_eq!(cursor.position(), begin, "Right then Left");
        }
        let cursor = doc.cursor_at(begin);
        if cursor.move_position(MoveOperation::Left, MoveMode::MoveAnchor, 1) {
            prop_assert!(!on_an_anchor(&doc, cursor.position()), "Left ended on an anchor");
            cursor.move_position(MoveOperation::Right, MoveMode::MoveAnchor, 1);
            prop_assert_eq!(cursor.position(), begin, "Left then Right");
        }
    }
}
