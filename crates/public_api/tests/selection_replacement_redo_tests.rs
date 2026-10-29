// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! A redo of typing that replaced a selection gives back what was typed, in place of the
//! selection, as one step.
//!
//! Typing over a selection replaces it, and the characters typed after it join the same
//! undo step, as typing does. Select `ab`, type `x` then `e`: an undo gives `ab` back, and
//! the redo gives `xe`, not `abxe`, with the caret after it.

use text_document::{MoveMode, TextCursor, TextDocument};

fn doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

fn text(doc: &TextDocument) -> String {
    doc.to_plain_text().unwrap()
}

/// Type `typed` a character at a time, as a keyboard does.
fn type_text(cursor: &TextCursor, typed: &str) {
    for c in typed.chars() {
        cursor.insert_text(&c.to_string()).unwrap();
    }
}

fn select(cursor: &TextCursor, anchor: usize, position: usize) {
    cursor.set_position(anchor, MoveMode::MoveAnchor);
    cursor.set_position(position, MoveMode::KeepAnchor);
}

fn caret(cursor: &TextCursor) -> (usize, usize) {
    (cursor.anchor(), cursor.position())
}

#[test]
fn a_redo_of_typing_over_a_selection_gives_back_the_typed_text_only() {
    let doc = doc("ab");
    let cursor = doc.cursor();
    select(&cursor, 0, 2);
    type_text(&cursor, "xe");
    assert_eq!(text(&doc), "xe");

    doc.undo().unwrap();
    assert_eq!(text(&doc), "ab");
    assert!(!doc.can_undo(), "the typing is one step");

    doc.redo().unwrap();
    assert_eq!(text(&doc), "xe");
    assert!(!doc.can_redo(), "the typing is one step");
    assert_eq!(caret(&cursor), (2, 2));
}

#[test]
fn a_redo_of_typing_over_a_backward_selection_gives_back_the_typed_text_only() {
    let doc = doc("ab");
    let cursor = doc.cursor();
    select(&cursor, 2, 0);
    type_text(&cursor, "xe");
    assert_eq!(text(&doc), "xe");

    doc.undo().unwrap();
    assert_eq!(text(&doc), "ab");
    doc.redo().unwrap();
    assert_eq!(text(&doc), "xe");
    assert_eq!(caret(&cursor), (2, 2));
}

#[test]
fn a_redo_of_typing_over_a_selection_inside_a_paragraph_keeps_the_text_around_it() {
    let doc = doc("0ab1");
    let cursor = doc.cursor();
    select(&cursor, 1, 3);
    type_text(&cursor, "xyz");
    assert_eq!(text(&doc), "0xyz1");

    doc.undo().unwrap();
    assert_eq!(text(&doc), "0ab1");
    doc.redo().unwrap();
    assert_eq!(text(&doc), "0xyz1");
    assert_eq!(caret(&cursor), (4, 4));

    cursor.insert_text("!").unwrap();
    assert_eq!(text(&doc), "0xyz!1");
}

#[test]
fn undo_and_redo_of_typing_over_a_selection_cycle_without_drifting() {
    let doc = doc("ab cd");
    let cursor = doc.cursor();
    select(&cursor, 0, 2);
    type_text(&cursor, "xe");
    assert_eq!(text(&doc), "xe cd");

    for _ in 0..3 {
        doc.undo().unwrap();
        assert_eq!(text(&doc), "ab cd");
        doc.redo().unwrap();
        assert_eq!(text(&doc), "xe cd");
        assert_eq!(caret(&cursor), (2, 2));
    }
}

#[test]
fn a_plain_text_paste_over_a_selection_then_typing_redoes_as_done() {
    let doc = doc("ab");
    let cursor = doc.cursor();
    select(&cursor, 0, 2);
    // A plain-text paste goes in as typed text does, and the typing after it joins it.
    cursor.insert_text("pasted").unwrap();
    type_text(&cursor, "e");
    assert_eq!(text(&doc), "pastede");

    while doc.can_undo() {
        doc.undo().unwrap();
    }
    assert_eq!(text(&doc), "ab");
    while doc.can_redo() {
        doc.redo().unwrap();
    }
    assert_eq!(text(&doc), "pastede");
    assert_eq!(caret(&cursor), (7, 7));
}

#[test]
fn a_rich_paste_over_a_selection_then_typing_redoes_as_done() {
    let doc = doc("ab");
    let cursor = doc.cursor();
    select(&cursor, 0, 2);
    cursor.insert_html("<b>pasted</b>").unwrap();
    type_text(&cursor, "e");
    assert_eq!(text(&doc), "pastede");

    while doc.can_undo() {
        doc.undo().unwrap();
    }
    assert_eq!(text(&doc), "ab");
    while doc.can_redo() {
        doc.redo().unwrap();
    }
    assert_eq!(text(&doc), "pastede");
    assert_eq!(caret(&cursor), (7, 7));
}

#[test]
fn a_redo_of_typing_over_a_selection_across_two_paragraphs_gives_back_the_typed_text_only() {
    let doc = doc("ab\ncd");
    let cursor = doc.cursor();
    select(&cursor, 1, 4);
    type_text(&cursor, "xe");
    assert_eq!(text(&doc), "axed");

    for _ in 0..2 {
        while doc.can_undo() {
            doc.undo().unwrap();
        }
        assert_eq!(text(&doc), "ab\ncd");
        while doc.can_redo() {
            doc.redo().unwrap();
        }
        assert_eq!(text(&doc), "axed");
        assert_eq!(caret(&cursor), (3, 3));
    }
}

#[test]
fn a_composition_committed_over_a_selection_then_typing_redoes_as_done() {
    let doc = doc("ab");
    let cursor = doc.cursor();
    select(&cursor, 0, 2);
    for state in ["h", "hi"] {
        if let Some(range) = doc.transient_range() {
            select(&cursor, range.start, range.end);
        }
        cursor.insert_text_transient(state).unwrap();
    }
    cursor.commit_transient_text("火").unwrap();
    type_text(&cursor, "e");
    assert_eq!(text(&doc), "火e");

    for _ in 0..2 {
        while doc.can_undo() {
            doc.undo().unwrap();
        }
        assert_eq!(text(&doc), "ab");
        while doc.can_redo() {
            doc.redo().unwrap();
        }
        assert_eq!(text(&doc), "火e");
        assert_eq!(caret(&cursor), (2, 2));
    }
}
