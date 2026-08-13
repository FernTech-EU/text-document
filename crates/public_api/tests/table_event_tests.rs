//! **A structural table edit is a content change, and has to say so.**
//!
//! Inserting a row into a table adds blocks. Every consumer that keeps offsets
//! into the document — a comment anchor, a spell session, a layout cache keyed
//! on `content_revision` — needs to hear about that, and for a long time none of
//! them did: the seven table primitives bumped `BlockCountChanged` and stopped
//! there.
//!
//! The failure was quiet in exactly the way that matters. Nothing errored,
//! nothing looked wrong on screen, and a consumer caching by revision simply
//! kept serving the layout it had.
//!
//! ## What these tests pin, and what they deliberately do not
//!
//! Each table primitive now reports its change through
//! `emit_content_change_events`, the **same** block diff undo and redo use. That
//! is the claim: a table edit is as visible as an undo, and reports through one
//! audited mechanism rather than seven hand-written descriptions of an edit.
//!
//! ⚠ **The delta is not asserted against `to_plain_text`.** It does not
//! reconcile against it, and it does not for undo and redo either: the diff is
//! computed over blocks joined by newlines, which is a different string from the
//! one `to_plain_text` renders for a document containing a table. That predates
//! this file and is left exactly as found. A test claiming the two agree would
//! be asserting something about the whole API that nobody has established, and
//! quietly changing which coordinate space these events speak in would be far
//! worse than the silence this fixes.

use text_document::{DocumentEvent, TextDocument};

/// A document holding a paragraph, a table, and a paragraph after it, with the
/// setup events drained.
///
/// The trailing paragraph is the point: it is what has offsets to lose when the
/// table above it changes, and a fixture with nothing after the table would pass
/// while the bug was fully present.
fn doc_with_table(rows: usize, columns: usize) -> (TextDocument, usize) {
    let doc = TextDocument::new();
    doc.set_plain_text("before").unwrap();

    let cursor = doc.cursor_at(doc.character_count());
    let table = cursor.insert_table(rows, columns).unwrap();
    let table_id = table.id();

    let cursor = doc.cursor_at(doc.character_count());
    cursor.insert_text("after").unwrap();

    doc.poll_events();
    (doc, table_id)
}

/// The single `ContentsChanged` in a batch, or a failure naming what did arrive.
///
/// Exactly one, not at least one: a structural edit that reported itself twice
/// would have every consumer shift its offsets twice.
fn one_contents_changed(events: &[DocumentEvent], what: &str) -> (usize, usize, usize, usize) {
    let found: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            DocumentEvent::ContentsChanged {
                position,
                chars_removed,
                chars_added,
                blocks_affected,
            } => Some((*position, *chars_removed, *chars_added, *blocks_affected)),
            _ => None,
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{what} should report exactly one content change; events were {events:?}"
    );
    found[0]
}

/// Drive one table edit and assert it reported itself.
fn reports_a_change(what: &str, edit: impl FnOnce(&TextDocument, usize)) {
    let (doc, table_id) = doc_with_table(3, 3);
    let before_revision = doc.content_revision();

    edit(&doc, table_id);

    let events = doc.poll_events();
    let (_, _, _, blocks_affected) = one_contents_changed(&events, what);
    assert_ne!(
        doc.content_revision(),
        before_revision,
        "{what}: the revision must move, or a cache keyed on it serves stale layout"
    );
    // Blocks, not characters. Merging two **empty** cells removes a block and
    // changes no characters at all, so a character assertion here would be
    // asserting something untrue about a perfectly correct edit. The
    // directional character claims live in the insert/remove test below, where
    // they mean something.
    assert!(
        blocks_affected > 0,
        "{what}: reported no affected blocks, which cannot be true of a structural edit"
    );
}

/// **The bug, one case per primitive.** Each of these was silent, and the
/// cursor-relative wrappers (`insert_row_above`, `remove_current_table` and the
/// rest) inherit the fix because they delegate to exactly these.
#[test]
fn every_structural_table_edit_reports_a_content_change() {
    reports_a_change("inserting a row", |doc, id| {
        doc.cursor_at(0).insert_table_row(id, 1).unwrap()
    });
    reports_a_change("inserting a column", |doc, id| {
        doc.cursor_at(0).insert_table_column(id, 1).unwrap()
    });
    reports_a_change("removing a row", |doc, id| {
        doc.cursor_at(0).remove_table_row(id, 1).unwrap()
    });
    reports_a_change("removing a column", |doc, id| {
        doc.cursor_at(0).remove_table_column(id, 1).unwrap()
    });
    reports_a_change("merging cells", |doc, id| {
        doc.cursor_at(0).merge_table_cells(id, 0, 0, 0, 1).unwrap()
    });
    reports_a_change("removing the table", |doc, id| {
        doc.cursor_at(0).remove_table(id).unwrap()
    });
}

/// A row insert and a row removal report opposite deltas, which is what a
/// consumer shifting offsets by them depends on.
#[test]
fn inserting_and_removing_report_opposite_deltas() {
    let (doc, table_id) = doc_with_table(3, 3);

    doc.cursor_at(0).insert_table_row(table_id, 1).unwrap();
    let (_, removed_on_insert, added_on_insert, _) =
        one_contents_changed(&doc.poll_events(), "inserting a row");
    assert!(
        added_on_insert > 0 && removed_on_insert == 0,
        "an insert only adds"
    );

    doc.cursor_at(0).remove_table_row(table_id, 1).unwrap();
    let (_, removed_on_remove, added_on_remove, _) =
        one_contents_changed(&doc.poll_events(), "removing a row");
    assert!(
        removed_on_remove > 0 && added_on_remove == 0,
        "a removal only removes"
    );
}

/// **A table edit and its undo agree**, which is the real content of the fix:
/// both now speak through the same block diff, so a consumer that already
/// handles undo correctly handles a table edit correctly for free.
#[test]
fn a_table_edit_and_its_undo_report_through_the_same_mechanism() {
    let (doc, table_id) = doc_with_table(3, 3);

    doc.cursor_at(0).insert_table_row(table_id, 1).unwrap();
    let (insert_pos, _, insert_added, insert_blocks) =
        one_contents_changed(&doc.poll_events(), "inserting a row");

    doc.undo().unwrap();
    let (undo_pos, undo_removed, _, undo_blocks) =
        one_contents_changed(&doc.poll_events(), "undoing the insert");

    assert_eq!(undo_pos, insert_pos, "the same place");
    assert_eq!(undo_removed, insert_added, "the same amount, the other way");
    assert_eq!(undo_blocks, insert_blocks, "the same blocks");
}

/// A revision that moves is only useful if it moves **once** per edit. Two
/// bumps would have a consumer relayout twice for one row.
#[test]
fn the_revision_moves_exactly_once_per_edit() {
    let (doc, table_id) = doc_with_table(3, 3);
    let before = doc.content_revision();

    doc.cursor_at(0).insert_table_row(table_id, 1).unwrap();

    assert_eq!(
        doc.content_revision(),
        before.wrapping_add(1),
        "one edit, one revision"
    );
}

/// Formatting is not content, and never was: this one already behaved, and is
/// here so that a later change cannot make everything shout.
#[test]
fn setting_a_table_format_reports_no_content_change() {
    let (doc, table_id) = doc_with_table(2, 2);

    let format = text_document::TableFormat::default();
    doc.cursor_at(0)
        .set_table_format(table_id, &format)
        .unwrap();

    assert!(
        !doc.poll_events()
            .iter()
            .any(|e| matches!(e, DocumentEvent::ContentsChanged { .. })),
        "setting a format is not a content change"
    );
}
