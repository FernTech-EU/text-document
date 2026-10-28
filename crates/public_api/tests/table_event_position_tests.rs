// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! The figures of a `ContentsChanged` are in the document's own positions, tables included.
//!
//! A consumer keeping a journal of changes, or offsets into the document, shifts them by
//! `(position, chars_removed, chars_added)`. Those positions are the ones a cursor,
//! `text_at` and `to_addressable_text` use, where a table is its `U+FFFC` anchor and the
//! separator after it. So every event must be a splice of that text: the text before the
//! edit, with `chars_removed` characters at `position` replaced by the `chars_added`
//! characters at `position` in the text after it, is the text after it.
//!
//! The edits that report through a diff of the blocks (undo, redo, the table edits) once
//! joined the blocks without the anchors: inserting or removing a table reported less than
//! it changed, or nothing, and an undo after a table moved the cursors after it by two
//! characters too many.

use text_document::{DocumentEvent, TextCursor, TextDocument};

fn addressable(doc: &TextDocument) -> Vec<char> {
    doc.to_addressable_text().unwrap().chars().collect()
}

/// Run `edit`, and check every `ContentsChanged` it announced is a splice of the
/// addressable text from before it to after it. Returns how many there were.
fn splices(doc: &TextDocument, what: &str, edit: impl FnOnce()) -> usize {
    doc.poll_events();
    let before = addressable(doc);
    edit();
    let after = addressable(doc);
    let events = doc.poll_events();
    let changes: Vec<(usize, usize, usize)> = events
        .iter()
        .filter_map(|event| match event {
            DocumentEvent::ContentsChanged {
                position,
                chars_removed,
                chars_added,
                ..
            } => Some((*position, *chars_removed, *chars_added)),
            _ => None,
        })
        .collect();
    assert!(
        !changes.is_empty(),
        "{what}: no ContentsChanged in {events:?}"
    );
    for &(position, removed, added) in &changes {
        let fits = position + removed <= before.len() && position + added <= after.len();
        assert!(
            fits && before[..position] == after[..position]
                && before[position + removed..] == after[position + added..],
            "{what}: ({position}, {removed}, {added}) is not a splice from {:?} to {:?}",
            before.iter().collect::<String>(),
            after.iter().collect::<String>(),
        );
    }
    changes.len()
}

/// `before`, a two-by-two table, `after`, with a cursor at the end of `after`.
fn doc_with_table() -> (TextDocument, usize, TextCursor) {
    let doc = TextDocument::new();
    doc.set_plain_text("before\nafter").unwrap();
    let table_id = doc.cursor_at(7).insert_table(2, 2).unwrap().id();
    let end = doc.cursor_at(addressable(&doc).len());
    (doc, table_id, end)
}

/// Where a cell of the table `doc_with_table` inserted starts: its first cell is at 9,
/// after `before\n` and the table's anchor and separator.
fn cell_position(doc: &TextDocument, table_id: usize, row: usize, column: usize) -> usize {
    let table = doc.cursor_at(9).current_table().expect("the table");
    assert_eq!(table.id(), table_id);
    table.cell(row, column).unwrap().blocks()[0].position()
}

#[test]
fn inserting_a_table_reports_its_anchor_and_cells_and_moves_the_text_after_it() {
    let doc = TextDocument::new();
    doc.set_plain_text("before\nafter").unwrap();
    let end = doc.cursor_at(12);
    doc.poll_events();
    doc.cursor_at(7).insert_table(2, 2).unwrap();
    let changes: Vec<DocumentEvent> = doc
        .poll_events()
        .into_iter()
        .filter(|event| matches!(event, DocumentEvent::ContentsChanged { .. }))
        .collect();
    // `\u{FFFC}\n` and four empty cells, the last one's separator before `after`.
    assert_eq!(
        changes,
        vec![DocumentEvent::ContentsChanged {
            position: 7,
            chars_removed: 0,
            chars_added: 6,
            blocks_affected: 4,
        }]
    );
    assert_eq!(doc.text_at(end.position() - 5, 5).unwrap(), "after");
}

#[test]
fn edits_in_a_cell_their_undo_and_redo_are_splices_of_the_addressable_text() {
    let (doc, table_id, end) = doc_with_table();
    let cell = cell_position(&doc, table_id, 1, 0);
    // A cursor at the cell's start, which an undo of the typing there must not move.
    let at_cell = doc.cursor_at(cell);

    let typing = doc.cursor_at(cell);
    splices(&doc, "typing in a cell", || {
        typing.insert_text("XY").unwrap()
    });
    drop(typing);
    assert_eq!(doc.text_at(cell, 2).unwrap(), "XY");

    splices(&doc, "undo", || doc.undo().unwrap());
    assert_eq!(
        at_cell.position(),
        cell,
        "the undo moved a cursor before it"
    );
    assert_eq!(doc.text_at(end.position() - 5, 5).unwrap(), "after");

    splices(&doc, "redo", || doc.redo().unwrap());
    assert_eq!(doc.text_at(cell, 2).unwrap(), "XY");
    assert_eq!(doc.text_at(end.position() - 5, 5).unwrap(), "after");
}

#[test]
fn removing_a_table_its_undo_and_redo_are_splices_of_the_addressable_text() {
    let (doc, table_id, end) = doc_with_table();
    let cell = cell_position(&doc, table_id, 0, 1);
    doc.cursor_at(cell).insert_text("XY").unwrap();

    splices(&doc, "remove_table", || {
        doc.cursor_at(0).remove_table(table_id).unwrap()
    });
    assert_eq!(doc.to_addressable_text().unwrap(), "before\nafter");
    assert_eq!(end.position(), 12);

    splices(&doc, "undo of remove_table", || doc.undo().unwrap());
    assert_eq!(doc.text_at(end.position() - 5, 5).unwrap(), "after");
    splices(&doc, "redo of remove_table", || doc.redo().unwrap());
    assert_eq!(end.position(), 12);
    splices(&doc, "undo again", || doc.undo().unwrap());
    splices(&doc, "undo of the typing", || doc.undo().unwrap());
    splices(&doc, "undo of insert_table", || doc.undo().unwrap());
    assert_eq!(doc.to_addressable_text().unwrap(), "before\nafter");
    assert_eq!(end.position(), 12);
    splices(&doc, "redo of insert_table", || doc.redo().unwrap());
    assert_eq!(doc.text_at(end.position() - 5, 5).unwrap(), "after");
}

#[test]
fn structural_table_edits_and_text_after_a_table_are_splices_of_the_addressable_text() {
    let (doc, table_id, end) = doc_with_table();
    splices(&doc, "insert_table_row", || {
        doc.cursor_at(0).insert_table_row(table_id, 1).unwrap()
    });
    splices(&doc, "insert_table_column", || {
        doc.cursor_at(0).insert_table_column(table_id, 0).unwrap()
    });
    splices(&doc, "undo of the column", || doc.undo().unwrap());
    splices(&doc, "undo of the row", || doc.undo().unwrap());
    assert_eq!(doc.text_at(end.position() - 5, 5).unwrap(), "after");

    let after_start = end.position() - 5;
    splices(&doc, "typing after the table", || {
        doc.cursor_at(after_start).insert_text("so ").unwrap()
    });
    splices(&doc, "undo of the typing", || doc.undo().unwrap());
    assert_eq!(doc.text_at(end.position() - 5, 5).unwrap(), "after");
    splices(&doc, "redo of the typing", || doc.redo().unwrap());
    assert_eq!(doc.text_at(after_start, 8).unwrap(), "so after");
}

#[test]
fn inserting_a_frame_reports_its_paragraph() {
    let doc = TextDocument::new();
    doc.set_plain_text("before\nafter").unwrap();
    let end = doc.cursor_at(12);
    splices(&doc, "insert_frame", || {
        doc.cursor_at(6).insert_frame().unwrap()
    });
    assert_eq!(doc.text_at(end.position() - 5, 5).unwrap(), "after");
}

#[test]
fn a_table_inserted_inside_a_paragraph_is_reported_as_what_it_put_in() {
    let doc = TextDocument::new();
    doc.set_plain_text("hello").unwrap();
    doc.poll_events();
    doc.cursor_at(3).insert_table(1, 1).unwrap();
    let changes: Vec<(usize, usize, usize)> = doc
        .poll_events()
        .into_iter()
        .filter_map(|event| match event {
            DocumentEvent::ContentsChanged {
                position,
                chars_removed,
                chars_added,
                ..
            } => Some((position, chars_removed, chars_added)),
            _ => None,
        })
        .collect();
    // The table goes after the paragraph, which it leaves whole: its separator, the
    // anchor and the anchor's separator, and its one empty cell.
    assert_eq!(doc.to_addressable_text().unwrap(), "hello\n\u{FFFC}\n");
    assert_eq!(changes, vec![(5, 0, 3)], "not the whole paragraph");
}

#[test]
fn text_an_undo_brings_back_with_a_table_is_reported_where_it_is() {
    let (doc, table_id, _end) = doc_with_table();
    let cell = cell_position(&doc, table_id, 0, 0);
    doc.cursor_at(cell).insert_text("XY").unwrap();
    doc.cursor_at(0).remove_table(table_id).unwrap();
    doc.poll_events();
    doc.undo().unwrap();
    let inserted: Vec<(usize, usize)> = doc
        .poll_events()
        .into_iter()
        .filter_map(|event| match event {
            DocumentEvent::TextInserted {
                position,
                chars_inserted,
                ..
            } => Some((position, chars_inserted)),
            _ => None,
        })
        .collect();
    assert_eq!(doc.text_at(cell, 2).unwrap(), "XY");
    assert_eq!(inserted, vec![(cell, 2)], "not the table's anchor");
}
