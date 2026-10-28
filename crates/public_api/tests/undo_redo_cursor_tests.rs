// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Where an undo and a redo leave the cursor that made the edit.
//!
//! A redo leaves it where the original edit left it: after a redone insertion its end,
//! after a redone deletion the deletion point, after a redone format change the selection
//! the change was made on. Wherever the cursor went in between, as QTextDocument, TextEdit
//! and Word do. Typing `abc def`, undoing and redoing, then typing `Y` gives `abc defY`.
//!
//! An undo leaves it as QTextDocument does: where the text an undone insertion put in was,
//! and after the text an undone deletion took away.

use text_document::{ListStyle, MoveMode, TextDocument, TextFormat};

fn doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

fn text(doc: &TextDocument) -> String {
    doc.to_plain_text().unwrap()
}

/// Type `typed` a character at a time, as a keyboard does.
fn type_text(cursor: &text_document::TextCursor, typed: &str) {
    for c in typed.chars() {
        cursor.insert_text(&c.to_string()).unwrap();
    }
}

fn select(cursor: &text_document::TextCursor, anchor: usize, position: usize) {
    cursor.set_position(anchor, MoveMode::MoveAnchor);
    cursor.set_position(position, MoveMode::KeepAnchor);
}

fn caret(cursor: &text_document::TextCursor) -> (usize, usize) {
    (cursor.anchor(), cursor.position())
}

// ── Redo ────────────────────────────────────────────────────────

#[test]
fn a_redone_insertion_leaves_the_caret_after_it() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc def");
    doc.undo().unwrap();
    doc.redo().unwrap();
    assert_eq!(text(&doc), "abc def");
    assert_eq!(caret(&cursor), (7, 7));

    cursor.insert_text("Y").unwrap();
    assert_eq!(text(&doc), "abc defY");
}

#[test]
fn a_redone_insertion_brings_back_a_caret_moved_since_the_undo() {
    let doc = doc("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();
    doc.undo().unwrap();
    cursor.set_position(0, MoveMode::MoveAnchor);
    doc.redo().unwrap();
    assert_eq!(text(&doc), "Hello world");
    assert_eq!(caret(&cursor), (11, 11));
}

#[test]
fn a_redone_paragraph_break_leaves_the_caret_in_the_new_paragraph() {
    let doc = doc("abcdef");
    let cursor = doc.cursor_at(3);
    cursor.insert_block().unwrap();
    assert_eq!(caret(&cursor), (4, 4));
    doc.undo().unwrap();
    doc.redo().unwrap();
    assert_eq!(text(&doc), "abc\ndef");
    assert_eq!(caret(&cursor), (4, 4));
}

#[test]
fn a_redone_forward_deletion_leaves_the_caret_at_the_deletion_point() {
    let doc = doc("abc def");
    let cursor = doc.cursor_at(4);
    cursor.delete_char().unwrap();
    assert_eq!(text(&doc), "abc ef");
    doc.undo().unwrap();
    cursor.set_position(0, MoveMode::MoveAnchor);
    doc.redo().unwrap();
    assert_eq!(text(&doc), "abc ef");
    assert_eq!(caret(&cursor), (4, 4));
}

#[test]
fn a_redone_backward_deletion_leaves_the_caret_at_the_deletion_point() {
    let doc = doc("abc def");
    let cursor = doc.cursor_at(7);
    cursor.delete_previous_char().unwrap();
    assert_eq!(text(&doc), "abc de");
    doc.undo().unwrap();
    cursor.set_position(0, MoveMode::MoveAnchor);
    doc.redo().unwrap();
    assert_eq!(text(&doc), "abc de");
    assert_eq!(caret(&cursor), (6, 6));
}

#[test]
fn a_redone_removal_of_a_selection_leaves_the_caret_where_it_was() {
    let doc = doc("abc def ghi");
    let cursor = doc.cursor();
    select(&cursor, 4, 8);
    cursor.remove_selected_text().unwrap();
    assert_eq!(text(&doc), "abc ghi");
    doc.undo().unwrap();
    cursor.set_position(11, MoveMode::MoveAnchor);
    doc.redo().unwrap();
    assert_eq!(text(&doc), "abc ghi");
    assert_eq!(caret(&cursor), (4, 4));
}

#[test]
fn a_redone_paste_of_several_paragraphs_leaves_the_caret_after_it() {
    let doc = doc("abc");
    let cursor = doc.cursor_at(3);
    cursor.insert_html("<p>one</p><p>two</p>").unwrap();
    let after_paste = caret(&cursor);
    let pasted = text(&doc);
    assert!(pasted.contains("two"), "{pasted:?}");
    doc.undo().unwrap();
    assert_eq!(text(&doc), "abc");
    doc.redo().unwrap();
    assert_eq!(text(&doc), pasted);
    assert_eq!(caret(&cursor), after_paste);
}

#[test]
fn a_redone_format_change_gives_back_its_selection() {
    let doc = doc("abc def");
    let cursor = doc.cursor();
    select(&cursor, 4, 7);
    cursor
        .merge_char_format(&TextFormat {
            font_bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    doc.undo().unwrap();
    cursor.set_position(1, MoveMode::MoveAnchor);
    doc.redo().unwrap();
    assert_eq!(caret(&cursor), (4, 7));
    assert_eq!(
        doc.cursor_at(5).char_format().unwrap().font_bold,
        Some(true)
    );
}

#[test]
fn a_redone_list_leaves_the_caret_where_it_was() {
    let doc = doc("one\ntwo");
    let cursor = doc.cursor_at(6);
    cursor.create_list(ListStyle::Decimal).unwrap();
    let after = caret(&cursor);
    doc.undo().unwrap();
    cursor.set_position(0, MoveMode::MoveAnchor);
    doc.redo().unwrap();
    assert!(cursor.current_list().is_some());
    assert_eq!(caret(&cursor), after);
}

#[test]
fn a_redone_table_leaves_the_caret_where_the_insertion_left_it() {
    let doc = doc("abc def");
    let cursor = doc.cursor_at(3);
    cursor.insert_table(2, 2).unwrap();
    let after = caret(&cursor);
    let in_table = cursor.current_table().is_some();
    doc.undo().unwrap();
    cursor.set_position(7, MoveMode::MoveAnchor);
    doc.redo().unwrap();
    assert_eq!(caret(&cursor), after);
    assert_eq!(cursor.current_table().is_some(), in_table);
}

#[test]
fn a_redone_edit_block_leaves_the_caret_where_its_last_edit_did() {
    let doc = doc("");
    let cursor = doc.cursor();
    cursor.begin_edit_block();
    cursor.insert_text("one").unwrap();
    cursor.insert_block().unwrap();
    cursor.insert_text("two").unwrap();
    cursor.end_edit_block();
    doc.undo().unwrap();
    assert_eq!(text(&doc), "");
    doc.redo().unwrap();
    assert_eq!(text(&doc), "one\ntwo");
    assert_eq!(caret(&cursor), (7, 7));
}

#[test]
fn an_edit_that_records_nothing_does_not_move_where_a_redo_puts_the_caret() {
    let doc = TextDocument::new();
    doc.set_markdown("before\n\n| a | b |\n|---|---|\n| c | d |\n\nafter")
        .unwrap()
        .wait()
        .unwrap();
    let cursor = doc.cursor_at(doc.character_count());
    type_text(&cursor, "xyz");
    let typed = caret(&cursor);
    // A Delete at the end of the paragraph before a table joins nothing and removes
    // nothing, and records nothing: the typing is still what an undo takes back, and what
    // a redo restores the caret of.
    cursor.set_position(6, MoveMode::MoveAnchor);
    let unchanged = text(&doc);
    cursor.delete_char().unwrap();
    assert_eq!(text(&doc), unchanged);
    doc.undo().unwrap();
    assert!(!text(&doc).contains("xyz"));
    doc.redo().unwrap();
    assert_eq!(caret(&cursor), typed);
}

#[test]
fn a_redone_composition_leaves_the_caret_after_the_committed_text() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    for state in ["n", "ni", "nihao"] {
        if let Some(range) = doc.transient_range() {
            select(&cursor, range.start, range.end);
        }
        cursor.insert_text_transient(state).unwrap();
    }
    cursor.commit_transient_text("你好").unwrap();
    assert_eq!(caret(&cursor), (5, 5));
    doc.undo().unwrap();
    assert_eq!(text(&doc), "abc");
    doc.redo().unwrap();
    assert_eq!(text(&doc), "abc你好");
    assert_eq!(caret(&cursor), (5, 5));
}

#[test]
fn a_redo_with_a_composition_outstanding_reverts_it_first() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc def");
    doc.undo().unwrap();
    cursor.insert_text_transient("ka").unwrap();
    doc.redo().unwrap();
    assert!(!doc.has_transient_edits());
    assert_eq!(text(&doc), "abc def");
    assert_eq!(caret(&cursor), (7, 7));
}

#[test]
fn a_redo_moves_only_the_cursor_that_made_the_edit() {
    let doc = doc("abc def");
    let cursor = doc.cursor_at(7);
    let other = doc.cursor_at(2);
    cursor.insert_text("!").unwrap();
    doc.undo().unwrap();
    doc.redo().unwrap();
    assert_eq!(caret(&cursor), (8, 8));
    assert_eq!(caret(&other), (2, 2));
}

#[test]
fn a_redo_after_the_editing_cursor_is_gone_moves_no_other_cursor() {
    let doc = doc("abc");
    let other = doc.cursor_at(1);
    doc.cursor_at(3).insert_text("def").unwrap();
    doc.undo().unwrap();
    doc.redo().unwrap();
    assert_eq!(text(&doc), "abcdef");
    assert_eq!(caret(&other), (1, 1));
}

// ── Undo ────────────────────────────────────────────────────────

#[test]
fn an_undone_insertion_leaves_the_caret_where_the_text_was() {
    let doc = doc("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();
    doc.undo().unwrap();
    assert_eq!(text(&doc), "Hello");
    assert_eq!(caret(&cursor), (5, 5));
}

#[test]
fn an_undone_backward_deletion_leaves_the_caret_after_the_restored_text() {
    let doc = doc("abc def");
    let cursor = doc.cursor_at(7);
    cursor.delete_previous_char().unwrap();
    cursor.delete_previous_char().unwrap();
    cursor.delete_previous_char().unwrap();
    assert_eq!(text(&doc), "abc ");
    while doc.can_undo() {
        doc.undo().unwrap();
    }
    assert_eq!(text(&doc), "abc def");
    assert_eq!(caret(&cursor), (7, 7));

    cursor.insert_text("Y").unwrap();
    assert_eq!(text(&doc), "abc defY");
}

#[test]
fn an_undone_forward_deletion_leaves_the_caret_after_the_restored_text() {
    let doc = doc("aab");
    let cursor = doc.cursor_at(0);
    cursor.delete_char().unwrap();
    assert_eq!(text(&doc), "ab");
    doc.undo().unwrap();
    assert_eq!(text(&doc), "aab");
    assert_eq!(caret(&cursor), (1, 1));
}

#[test]
fn an_undone_removal_of_a_selection_leaves_the_caret_after_the_restored_text() {
    let doc = doc("abc def ghi");
    let cursor = doc.cursor();
    select(&cursor, 4, 8);
    cursor.remove_selected_text().unwrap();
    doc.undo().unwrap();
    assert_eq!(text(&doc), "abc def ghi");
    assert_eq!(caret(&cursor), (8, 8));
}

#[test]
fn an_undo_brings_the_caret_back_to_the_edit() {
    let doc = doc("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();
    cursor.set_position(0, MoveMode::MoveAnchor);
    doc.undo().unwrap();
    assert_eq!(caret(&cursor), (5, 5));
}

#[test]
fn an_undone_format_change_keeps_the_selection() {
    let doc = doc("abc def");
    let cursor = doc.cursor();
    select(&cursor, 4, 7);
    cursor
        .merge_char_format(&TextFormat {
            font_bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    doc.undo().unwrap();
    assert_eq!(caret(&cursor), (4, 7));
}
