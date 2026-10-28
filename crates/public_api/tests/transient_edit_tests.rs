// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Transient edits: what an input method's composition is shown with, and that no undo
//! history records.
//!
//! What has to hold:
//!
//! * a composition shown and replaced at every update, then committed, is one undo step,
//!   apart from the typing before and after it; a cancelled one is nothing at all;
//! * transient edits push nothing, keep the redo stack, and leave the coalescing of typing
//!   as it was;
//! * they are announced as edits are, so views redraw them, and never as text that arrived
//!   or as a move of the history;
//! * a revert gives back the recorded document exactly, paragraphs and block identities
//!   included, and moves every cursor back;
//! * every recorded edit, undo, redo and load reverts them first, and acts where its
//!   cursor or its positions stand in the recorded text.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use proptest::prelude::*;
use text_document::{
    DocumentError, DocumentEvent, FindOptions, HighlightContext, InsertionOrigin, MoveMode,
    ReplaceOptions, ReplaceRange, SyntaxHighlighter, TextDocument, TextFormat, TransientRefusal,
};

fn doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

fn text(doc: &TextDocument) -> String {
    doc.to_plain_text().unwrap()
}

fn block_texts(doc: &TextDocument) -> Vec<String> {
    doc.blocks().iter().map(|block| block.text()).collect()
}

/// Typed a character at a time, as keystrokes are, so the typing coalesces.
fn type_text(cursor: &text_document::TextCursor, typed: &str) {
    for ch in typed.chars() {
        cursor.insert_text(&ch.to_string()).unwrap();
    }
}

fn select(cursor: &text_document::TextCursor, start: usize, end: usize) {
    cursor.set_position(start, MoveMode::MoveAnchor);
    cursor.set_position(end, MoveMode::KeepAnchor);
}

/// Each state of a composition shown in turn, replacing the one before, as an input method
/// updates its preedit.
fn compose(doc: &TextDocument, cursor: &text_document::TextCursor, states: &[&str]) {
    for state in states {
        if let Some(range) = doc.transient_range() {
            select(cursor, range.start, range.end);
        }
        cursor.insert_text_transient(state).unwrap();
    }
}

/// How many steps the undo and the redo stacks hold, counted by stepping through them and
/// back. Only with no transient edits outstanding: an undo reverts them.
fn history_depth(doc: &TextDocument) -> (usize, usize) {
    assert!(!doc.has_transient_edits());
    let mut undos = 0;
    while doc.can_undo() {
        doc.undo().unwrap();
        undos += 1;
    }
    for _ in 0..undos {
        doc.redo().unwrap();
    }
    let mut redos = 0;
    while doc.can_redo() {
        doc.redo().unwrap();
        redos += 1;
    }
    for _ in 0..redos {
        doc.undo().unwrap();
    }
    (undos, redos)
}

// ── A composition ───────────────────────────────────────────────

#[test]
fn a_committed_composition_is_one_step_after_the_typing() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");

    compose(&doc, &cursor, &["n", "ni", "nih", "nihao"]);
    assert_eq!(text(&doc), "abcnihao");
    assert_eq!(doc.transient_range(), Some(3..8));
    assert_eq!(cursor.position(), 8);

    cursor.commit_transient_text("你好").unwrap();
    assert_eq!(text(&doc), "abc你好");
    assert_eq!(cursor.position(), 5);
    assert!(!doc.has_transient_edits());
    assert_eq!(doc.transient_range(), None);

    doc.undo().unwrap();
    assert_eq!(text(&doc), "abc", "the typed text is intact");
    doc.undo().unwrap();
    assert_eq!(text(&doc), "");
    assert!(!doc.can_undo());

    doc.redo().unwrap();
    assert_eq!(text(&doc), "abc");
    doc.redo().unwrap();
    assert_eq!(text(&doc), "abc你好");
    assert!(!doc.can_redo());
}

#[test]
fn a_cancelled_composition_records_nothing() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    assert_eq!(history_depth(&doc), (1, 0));
    // Stepping through the history moved the cursor: back to the end of the typing.
    cursor.set_position(3, MoveMode::MoveAnchor);

    compose(&doc, &cursor, &["n", "ni", "nihao"]);
    assert!(doc.revert_transient_edits());
    assert_eq!(text(&doc), "abc");
    assert_eq!(cursor.position(), 3);
    assert!(!doc.has_transient_edits());
    assert_eq!(history_depth(&doc), (1, 0));

    assert!(
        !doc.revert_transient_edits(),
        "nothing is left to revert a second time"
    );
}

#[test]
fn a_composition_in_the_middle_of_a_paragraph() {
    let doc = doc("Hello world");
    let cursor = doc.cursor_at(5);
    compose(&doc, &cursor, &["わ", "わた", "わたし"]);
    assert_eq!(text(&doc), "Helloわたし world");
    assert_eq!(doc.transient_range(), Some(5..8));

    cursor.commit_transient_text("私").unwrap();
    assert_eq!(text(&doc), "Hello私 world");
    assert_eq!(cursor.position(), 6);

    doc.undo().unwrap();
    assert_eq!(text(&doc), "Hello world");
    assert!(!doc.can_undo());
    doc.redo().unwrap();
    assert_eq!(text(&doc), "Hello私 world");
}

#[test]
fn a_composition_in_a_later_paragraph() {
    // Positions count each paragraph separator as one: "Second" ends at 12.
    let doc = doc("First\nSecond\nThird");
    let cursor = doc.cursor_at(12);
    compose(&doc, &cursor, &["g", "ga", "gan"]);
    assert_eq!(block_texts(&doc), ["First", "Secondgan", "Third"]);
    assert_eq!(doc.transient_range(), Some(12..15));

    cursor.commit_transient_text("感").unwrap();
    assert_eq!(block_texts(&doc), ["First", "Second感", "Third"]);

    doc.undo().unwrap();
    assert_eq!(block_texts(&doc), ["First", "Second", "Third"]);
    doc.redo().unwrap();
    assert_eq!(block_texts(&doc), ["First", "Second感", "Third"]);
}

#[test]
fn a_composition_over_a_selection_replaces_it_only_when_committed() {
    let doc = doc("one two three");
    let cursor = doc.cursor();
    select(&cursor, 4, 7);
    cursor.insert_text_transient("t").unwrap();
    assert_eq!(text(&doc), "one t three");
    compose(&doc, &cursor, &["ta", "tao"]);
    assert_eq!(text(&doc), "one tao three");

    // Cancelled: the selected word comes back.
    assert!(doc.revert_transient_edits());
    assert_eq!(text(&doc), "one two three");
    assert!(!doc.can_undo());

    // Committed: the word replaced in one step.
    select(&cursor, 4, 7);
    compose(&doc, &cursor, &["t", "tao"]);
    cursor.commit_transient_text("套").unwrap();
    assert_eq!(text(&doc), "one 套 three");
    doc.undo().unwrap();
    assert_eq!(text(&doc), "one two three");
    assert!(!doc.can_undo());
}

// ── Korean Hanja conversion ─────────────────────────────────────

#[test]
fn a_hanja_conversion_is_one_step_back_to_the_syllable() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc한");
    doc.break_undo_merge();

    // The input method takes the committed syllable out and shows its candidate.
    select(&cursor, 3, 4);
    assert_eq!(cursor.remove_selected_text_transient().unwrap(), "한");
    assert_eq!(text(&doc), "abc");
    assert_eq!(doc.transient_range(), Some(3..3));
    cursor.insert_text_transient("韓").unwrap();
    assert_eq!(text(&doc), "abc韓");
    assert_eq!(doc.transient_range(), Some(3..4));

    cursor.commit_transient_text("韓").unwrap();
    assert_eq!(text(&doc), "abc韓");
    doc.undo().unwrap();
    assert_eq!(text(&doc), "abc한", "one undo gives the syllable back");
    doc.undo().unwrap();
    assert_eq!(text(&doc), "");
}

#[test]
fn a_hanja_candidate_is_read_as_the_syllable_it_shows_in_place_of() {
    // The syllable taken out, the candidate shown, then a second candidate over the first.
    for updates in [&["\u{97D3}"][..], &["\u{97D3}", "\u{6C49}"][..]] {
        let doc = doc("ab\u{D55C}cd");
        let ime = doc.cursor();
        select(&ime, 2, 3);
        ime.remove_selected_text_transient().unwrap();
        compose(&doc, &ime, updates);
        assert_eq!(doc.transient_range(), Some(2..3));

        // A replacement read over the candidate replaces the syllable.
        doc.cursor().replace(2, 3, "X", Default::default()).unwrap();
        assert_eq!(text(&doc), "abXcd");
    }

    let doc = doc("ab\u{D55C}cd");
    let ime = doc.cursor();
    select(&ime, 2, 3);
    ime.remove_selected_text_transient().unwrap();
    compose(&doc, &ime, &["\u{97D3}"]);
    let replaced = doc
        .replace_ranges(
            &[ReplaceRange {
                position: 2,
                length: 1,
                replacement: "X".into(),
            }],
            &ReplaceOptions::default(),
        )
        .unwrap();
    assert_eq!(replaced, 1);
    assert_eq!(text(&doc), "abXcd");
}

#[test]
fn a_cancelled_hanja_conversion_gives_the_syllable_back_untouched() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "한");
    select(&cursor, 0, 1);
    cursor.remove_selected_text_transient().unwrap();
    cursor.insert_text_transient("韓").unwrap();

    assert!(doc.revert_transient_edits());
    assert_eq!(text(&doc), "한");
    assert_eq!(history_depth(&doc), (1, 0));
}

#[test]
fn a_converted_syllable_keeps_the_format_it_wore_whole() {
    let doc = doc("");
    let cursor = doc.cursor();
    cursor.insert_text("abc").unwrap();
    let bold = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    cursor.insert_formatted_text("한", &bold).unwrap();
    doc.break_undo_merge();

    select(&cursor, 3, 4);
    cursor.remove_selected_text_transient().unwrap();
    cursor.insert_text_transient("韓").unwrap();
    cursor.commit_transient_text("韓").unwrap();

    let probe = doc.cursor_at(4);
    probe.set_position(3, MoveMode::KeepAnchor);
    assert_eq!(probe.selected_text().unwrap(), "韓");
    // `char_format` reads the character after the caret.
    assert_eq!(
        doc.cursor_at(3).char_format().unwrap().font_bold,
        Some(true)
    );
}

#[test]
fn an_empty_commit_records_only_what_was_taken_out() {
    // After a transient insertion: nothing.
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    compose(&doc, &cursor, &["n"]);
    cursor.commit_transient_text("").unwrap();
    assert_eq!(text(&doc), "abc");
    assert_eq!(history_depth(&doc), (1, 0));

    // After a transient removal: the removal, as one step.
    cursor.set_position(3, MoveMode::MoveAnchor);
    type_text(&cursor, "한");
    doc.break_undo_merge();
    select(&cursor, 3, 4);
    cursor.remove_selected_text_transient().unwrap();
    cursor.commit_transient_text("").unwrap();
    assert_eq!(text(&doc), "abc");
    doc.undo().unwrap();
    assert_eq!(text(&doc), "abc한");
}

#[test]
fn an_empty_commit_of_an_insertion_is_a_cancel_and_closes_no_step() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    compose(&doc, &cursor, &["n", "ni"]);
    cursor.commit_transient_text("").unwrap();
    type_text(&cursor, "d");
    assert_eq!(text(&doc), "abcd");
    doc.undo().unwrap();
    assert_eq!(
        text(&doc),
        "",
        "the typing on either side is still one step"
    );
}

#[test]
fn a_commit_with_nothing_outstanding_is_an_insertion_of_its_own() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    cursor.commit_transient_text("x").unwrap();
    assert_eq!(text(&doc), "abcx");
    doc.undo().unwrap();
    assert_eq!(text(&doc), "abc");
}

// ── History and coalescing ──────────────────────────────────────

#[test]
fn transient_edits_leave_both_stacks_as_they_were() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    doc.break_undo_merge();
    type_text(&cursor, "def");
    doc.undo().unwrap();
    assert_eq!(text(&doc), "abc");
    assert_eq!(history_depth(&doc), (1, 1));

    compose(&doc, &cursor, &["x", "xy"]);
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.insert_text_transient("y").unwrap();
    select(&cursor, 1, 2);
    cursor.remove_selected_text_transient().unwrap();
    assert!(doc.can_undo());
    assert!(
        doc.can_redo(),
        "a transient edit does not clear the redo stack"
    );

    doc.revert_transient_edits();
    assert_eq!(text(&doc), "abc");
    assert_eq!(history_depth(&doc), (1, 1));
    doc.redo().unwrap();
    assert_eq!(text(&doc), "abcdef");
}

#[test]
fn typing_around_a_cancelled_composition_coalesces_as_without_it() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    compose(&doc, &cursor, &["n", "ni"]);
    doc.revert_transient_edits();
    type_text(&cursor, "d");
    assert_eq!(text(&doc), "abcd");

    doc.undo().unwrap();
    assert_eq!(
        text(&doc),
        "",
        "the typing before and after the composition is the one step it would have been"
    );
}

#[test]
fn a_recorded_edit_coalesces_over_an_outstanding_composition_as_without_it() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    compose(&doc, &cursor, &["n", "nihao"]);
    // Typed while the composition shows: it is reverted, and the character goes in where
    // the cursor stands in the recorded text, coalescing with the typing before.
    cursor.insert_text("d").unwrap();
    assert_eq!(text(&doc), "abcd");
    assert!(!doc.has_transient_edits());

    doc.undo().unwrap();
    assert_eq!(text(&doc), "");
}

#[test]
fn a_commit_closes_its_step_on_both_sides() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    compose(&doc, &cursor, &["n", "nihao"]);
    cursor.commit_transient_text("你好").unwrap();
    type_text(&cursor, "de");
    assert_eq!(text(&doc), "abc你好de");

    doc.undo().unwrap();
    assert_eq!(text(&doc), "abc你好");
    doc.undo().unwrap();
    assert_eq!(text(&doc), "abc");
    doc.undo().unwrap();
    assert_eq!(text(&doc), "");
}

#[test]
fn undo_during_a_composition_reverts_it_and_then_undoes() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    doc.break_undo_merge();
    type_text(&cursor, "def");
    compose(&doc, &cursor, &["n", "ni"]);

    doc.undo().unwrap();
    assert!(!doc.has_transient_edits());
    assert_eq!(text(&doc), "abc");
    doc.redo().unwrap();
    assert_eq!(text(&doc), "abcdef");
}

#[test]
fn redo_during_a_composition_reverts_it_and_then_redoes() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    doc.break_undo_merge();
    type_text(&cursor, "def");
    doc.undo().unwrap();
    compose(&doc, &cursor, &["n"]);

    doc.redo().unwrap();
    assert!(!doc.has_transient_edits());
    assert_eq!(text(&doc), "abcdef");
}

// ── Events ──────────────────────────────────────────────────────

fn contents_changes(events: &[DocumentEvent]) -> Vec<(usize, usize, usize)> {
    events
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
        .collect()
}

#[test]
fn transient_edits_are_announced_as_edits_and_never_as_history() {
    let doc = doc("abc");
    doc.set_modified(false);
    doc.poll_events();
    let revision = doc.content_revision();
    let cursor = doc.cursor_at(3);

    compose(&doc, &cursor, &["n", "ni"]);
    let events = doc.poll_events();
    assert_eq!(contents_changes(&events), [(3, 0, 1), (3, 1, 2)]);
    assert!(
        !events.iter().any(|event| matches!(
            event,
            DocumentEvent::TextInserted { .. }
                | DocumentEvent::UndoRedoChanged { .. }
                | DocumentEvent::ModificationChanged(_)
        )),
        "nothing has arrived and the history has not moved: {events:?}"
    );
    assert!(doc.content_revision() > revision);
    assert!(!doc.is_modified());

    doc.revert_transient_edits();
    let events = doc.poll_events();
    assert_eq!(contents_changes(&events), [(3, 2, 0)]);
    assert!(!events.iter().any(|event| matches!(
        event,
        DocumentEvent::TextInserted { .. } | DocumentEvent::UndoRedoChanged { .. }
    )));
    assert!(!doc.is_modified());
}

#[test]
fn the_commit_is_the_one_arrival_of_the_text() {
    let doc = doc("abc");
    doc.poll_events();
    let cursor = doc.cursor_at(3);
    compose(&doc, &cursor, &["n", "nihao"]);
    cursor
        .commit_transient_text_with_origin("你好", InsertionOrigin::Composed)
        .unwrap();

    let arrivals: Vec<_> = doc
        .poll_events()
        .into_iter()
        .filter_map(|event| match event {
            DocumentEvent::TextInserted {
                position,
                chars_inserted,
                origin,
            } => Some((position, chars_inserted, origin)),
            _ => None,
        })
        .collect();
    assert_eq!(arrivals, [(3, 2, InsertionOrigin::Composed)]);
    assert!(doc.is_modified());
}

#[test]
fn callbacks_hear_transient_edits_too() {
    let doc = doc("abc");
    let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&heard);
    let _subscription = doc.on_change(move |event| sink.lock().unwrap().push(event));
    let cursor = doc.cursor_at(3);
    cursor.insert_text_transient("n").unwrap();
    doc.revert_transient_edits();
    assert_eq!(
        contents_changes(&heard.lock().unwrap()),
        [(3, 0, 1), (3, 1, 0)]
    );
}

// ── The revert ──────────────────────────────────────────────────

#[test]
fn a_commit_over_a_span_across_paragraphs_is_one_step() {
    let doc = doc("one\ntwo\nthree");
    let cursor = doc.cursor();
    select(&cursor, 2, 9);
    cursor.remove_selected_text_transient().unwrap();
    cursor.insert_text_transient("X").unwrap();
    assert_eq!(block_texts(&doc), ["onXhree"]);

    cursor.commit_transient_text("Y").unwrap();
    assert_eq!(block_texts(&doc), ["onYhree"]);
    doc.undo().unwrap();
    assert_eq!(block_texts(&doc), ["one", "two", "three"]);
    assert!(!doc.can_undo());
}

#[test]
fn a_transient_removal_across_paragraphs_is_reverted_exactly() {
    let doc = doc("one\ntwo\nthree");
    let ids: Vec<usize> = doc.blocks().iter().map(|block| block.id()).collect();
    doc.poll_events();

    let cursor = doc.cursor();
    select(&cursor, 2, 9);
    assert_eq!(
        cursor.remove_selected_text_transient().unwrap(),
        "e\ntwo\nt"
    );
    assert_eq!(block_texts(&doc), ["onhree"]);
    assert!(
        doc.poll_events()
            .contains(&DocumentEvent::BlockCountChanged(1))
    );

    doc.revert_transient_edits();
    assert_eq!(block_texts(&doc), ["one", "two", "three"]);
    let back: Vec<usize> = doc.blocks().iter().map(|block| block.id()).collect();
    assert_eq!(back, ids, "the very blocks the history refers to");
    assert!(
        doc.poll_events()
            .contains(&DocumentEvent::BlockCountChanged(3))
    );
    assert!(!doc.can_undo());
}

#[test]
fn a_revert_moves_every_cursor_back() {
    let doc = doc("abc def ghi");
    let cursor = doc.cursor_at(3);
    let before = doc.cursor_at(1);
    let after = doc.cursor_at(10);
    compose(&doc, &cursor, &["X", "XY"]);
    assert_eq!((before.position(), after.position()), (1, 12));
    let inside = doc.cursor_at(4);

    doc.revert_transient_edits();
    assert_eq!(cursor.position(), 3);
    assert_eq!(before.position(), 1);
    assert_eq!(after.position(), 10);
    assert_eq!(inside.position(), 3);
}

#[test]
fn a_formatted_composition_is_shown_formatted_and_reverted() {
    let doc = doc("abc");
    let cursor = doc.cursor_at(3);
    let underlined = TextFormat {
        font_underline: Some(true),
        ..Default::default()
    };
    cursor
        .insert_formatted_text_transient("ni", &underlined)
        .unwrap();
    assert_eq!(text(&doc), "abcni");
    // `char_format` reads the character after the caret.
    assert_eq!(
        doc.cursor_at(3).char_format().unwrap().font_underline,
        Some(true)
    );
    assert!(!doc.can_undo());

    doc.revert_transient_edits();
    assert_eq!(text(&doc), "abc");
    assert!(!doc.can_undo());
}

#[test]
fn a_commit_inside_an_edit_block_joins_the_block() {
    let doc = doc("");
    let cursor = doc.cursor();
    cursor.begin_edit_block();
    cursor.insert_text("abc").unwrap();
    compose(&doc, &cursor, &["n", "ni"]);
    cursor.commit_transient_text("你").unwrap();
    cursor.end_edit_block();
    assert_eq!(text(&doc), "abc你");

    doc.undo().unwrap();
    assert_eq!(text(&doc), "");
}

#[test]
fn the_history_operations_leave_a_composition_outstanding() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    compose(&doc, &cursor, &["n"]);
    doc.break_undo_merge();
    doc.clear_undo_redo();
    assert!(doc.has_transient_edits());
    assert_eq!(text(&doc), "abcn");
    assert!(!doc.can_undo());

    doc.revert_transient_edits();
    assert_eq!(text(&doc), "abc");
    assert!(!doc.can_undo());
}

#[test]
fn a_revert_counts_the_blocks_it_touches() {
    let blocks_affected = |events: Vec<DocumentEvent>| -> Vec<usize> {
        events
            .into_iter()
            .filter_map(|event| match event {
                DocumentEvent::ContentsChanged {
                    blocks_affected, ..
                } => Some(blocks_affected),
                _ => None,
            })
            .collect()
    };
    // The last two characters of "Second" (10..12): one block, though the next starts at 13.
    let doc = doc("First\nSecond\nThird");
    let cursor = doc.cursor();
    select(&cursor, 10, 12);
    cursor.remove_selected_text_transient().unwrap();
    doc.poll_events();
    doc.revert_transient_edits();
    assert_eq!(blocks_affected(doc.poll_events()), [1]);

    // From "First" into "Third": all three.
    select(&cursor, 2, 15);
    cursor.remove_selected_text_transient().unwrap();
    doc.poll_events();
    doc.revert_transient_edits();
    assert_eq!(blocks_affected(doc.poll_events()), [3]);
}

#[test]
fn a_recorded_edit_through_a_clone_of_the_document_reverts_the_composition() {
    let doc = doc("abc");
    let view = doc.clone();
    doc.cursor_at(3).insert_text_transient("ni").unwrap();
    assert!(view.has_transient_edits(), "one document, one composition");

    view.cursor_at(0).insert_text(">").unwrap();
    assert!(!doc.has_transient_edits());
    assert_eq!(text(&doc), ">abc");
}

// ── Recorded writes over a composition ──────────────────────────

#[test]
fn a_callback_writing_on_the_revert_writes_after_the_write_it_made_way_for() {
    let doc = doc("");
    let cursor = doc.cursor();
    type_text(&cursor, "abc");
    doc.break_undo_merge();
    type_text(&cursor, "def");
    compose(&doc, &cursor, &["n", "ni"]);

    // Heard first is the revert's announcement. A callback writing then must not write
    // before the undo the revert made way for: the undo would take back its edit instead
    // of the writer's last step.
    let writer = doc.cursor_at(0);
    let once = Arc::new(AtomicBool::new(false));
    let fired = Arc::clone(&once);
    let _subscription = doc.on_change(move |event| {
        if matches!(event, DocumentEvent::ContentsChanged { .. })
            && !fired.swap(true, Ordering::SeqCst)
        {
            writer.set_position(0, MoveMode::MoveAnchor);
            writer.insert_text("!").unwrap();
        }
    });
    doc.undo().unwrap();
    assert!(once.load(Ordering::SeqCst));
    assert_eq!(
        text(&doc),
        "!abc",
        "the undo took back \"def\", then \"!\" went in"
    );
    doc.undo().unwrap();
    assert_eq!(text(&doc), "abc");
}

#[test]
fn a_write_that_does_nothing_still_announces_the_revert() {
    let doc = doc("abc");
    let cursor = doc.cursor_at(3);
    compose(&doc, &cursor, &["n", "ni"]);
    let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&heard);
    let _subscription = doc.on_change(move |event| sink.lock().unwrap().push(event));

    // No selection: nothing to remove, but the composition was reverted first.
    assert_eq!(cursor.remove_selected_text().unwrap(), "");
    assert!(!doc.has_transient_edits());
    assert_eq!(contents_changes(&heard.lock().unwrap()), [(3, 2, 0)]);
}

#[test]
fn a_panic_inside_a_transient_edit_keeps_the_document_and_its_store_in_step() {
    /// Panics the first time it sees a `!`, as a buggy highlighter might.
    struct PanicsOnce(AtomicBool);
    impl SyntaxHighlighter for PanicsOnce {
        fn highlight_block(&self, text: &str, _ctx: &mut HighlightContext) {
            if text.contains('!') && !self.0.swap(true, Ordering::SeqCst) {
                panic!("a highlighter bug");
            }
        }
    }

    let doc = doc("abc");
    doc.set_syntax_highlighter(Some(Arc::new(PanicsOnce(AtomicBool::new(false)))));
    let store = doc.rope_store_for_test();
    let cursor = doc.cursor_at(3);
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cursor.insert_text_transient("!")
    }));
    assert!(caught.is_err());

    // The highlighter runs after the change is made, so the change stays, as a transient
    // edit the document and its store both hold, and a revert takes it away.
    assert!(doc.has_transient_edits());
    assert!(store.has_transient_edits());
    assert_eq!(text(&doc), "abc!");
    assert!(doc.revert_transient_edits());
    assert_eq!(text(&doc), "abc");
    assert!(!store.has_transient_edits());
    assert!(!doc.can_undo());
}

#[test]
fn explicit_positions_are_read_with_the_composition_and_mapped_back() {
    let doc = doc("hello world");
    let cursor = doc.cursor_at(5);
    compose(&doc, &cursor, &["XYZ"]);
    assert_eq!(text(&doc), "helloXYZ world");

    // "world", where it stands with the composition shown.
    let other = doc.cursor();
    other.replace(9, 14, "earth", Default::default()).unwrap();
    assert_eq!(text(&doc), "hello earth");

    compose(&doc, &cursor, &["XYZ"]);
    let start = text(&doc).find("earth").unwrap();
    let replaced = doc
        .replace_ranges(
            &[ReplaceRange {
                position: start,
                length: 5,
                replacement: "moon".into(),
            }],
            &ReplaceOptions::default(),
        )
        .unwrap();
    assert_eq!(replaced, 1);
    assert_eq!(text(&doc), "hello moon");
}

#[test]
fn a_range_that_held_only_transient_text_names_nothing() {
    let doc = doc("hello world");
    let cursor = doc.cursor_at(5);
    compose(&doc, &cursor, &["XYZ"]);
    let refused = doc
        .cursor()
        .replace(5, 8, "!", Default::default())
        .unwrap_err();
    assert!(matches!(
        refused,
        DocumentError::TransientEdits(TransientRefusal::RangeOfTransientTextOnly)
    ));
    assert_eq!(text(&doc), "hello world");
    assert!(!doc.has_transient_edits());
    assert!(!doc.can_undo());

    compose(&doc, &cursor, &["XYZ"]);
    let replaced = doc
        .replace_ranges(
            &[
                ReplaceRange {
                    position: 5,
                    length: 3,
                    replacement: "!".into(),
                },
                ReplaceRange {
                    position: 0,
                    length: 5,
                    replacement: "howdy".into(),
                },
            ],
            &ReplaceOptions::default(),
        )
        .unwrap();
    assert_eq!(replaced, 1);
    assert_eq!(text(&doc), "howdy world");
}

#[test]
fn every_recorded_write_reverts_transient_edits_first() {
    type Write = fn(&TextDocument, &text_document::TextCursor) -> Result<(), DocumentError>;
    // Each write runs over "abc! def" with "XY" shown transiently after the "!" (the caret
    // there, at 6), and must act on the recorded text with the caret back at 4. `Some` is
    // the plain text it must leave, where the write's effect on it is plain.
    let writes: Vec<(&str, Write, Option<&str>)> = vec![
        ("insert_text", |_, c| c.insert_text("?"), Some("abc!? def")),
        (
            "insert_formatted_text",
            |_, c| c.insert_formatted_text("?", &TextFormat::default()),
            Some("abc!? def"),
        ),
        (
            "insert_line_break",
            |_, c| c.insert_line_break(),
            Some("abc!\u{2028} def"),
        ),
        ("insert_block", |_, c| c.insert_block(), Some("abc!\n def")),
        ("insert_html", |_, c| c.insert_html("<b>?</b>"), None),
        ("insert_markdown", |_, c| c.insert_markdown("*?*"), None),
        ("insert_djot", |_, c| c.insert_djot("_?_"), None),
        (
            "insert_fragment",
            |_, c| c.insert_fragment(&text_document::DocumentFragment::from_plain_text("?")),
            Some("abc!? def"),
        ),
        ("insert_image", |_, c| c.insert_image("img", "", 1, 1), None),
        ("insert_frame", |_, c| c.insert_frame(), None),
        (
            "insert_table",
            |_, c| c.insert_table(1, 1).map(|_| ()),
            None,
        ),
        ("delete_char", |_, c| c.delete_char(), Some("abc!def")),
        (
            "delete_previous_char",
            |_, c| c.delete_previous_char(),
            Some("abc def"),
        ),
        (
            "remove_selected_text",
            |_, c| {
                c.set_position(0, MoveMode::KeepAnchor);
                c.remove_selected_text().map(|_| ())
            },
            None,
        ),
        (
            "replace",
            |_, c| c.replace(0, 3, "xyz", Default::default()),
            Some("xyz! def"),
        ),
        (
            "set_char_format",
            |_, c| {
                c.set_position(0, MoveMode::KeepAnchor);
                c.set_char_format(&TextFormat {
                    font_bold: Some(true),
                    ..Default::default()
                })
            },
            Some("abc! def"),
        ),
        (
            "merge_char_format",
            |_, c| {
                c.set_position(0, MoveMode::KeepAnchor);
                c.merge_char_format(&TextFormat {
                    font_italic: Some(true),
                    ..Default::default()
                })
            },
            Some("abc! def"),
        ),
        (
            "set_block_format",
            |_, c| {
                c.set_block_format(&text_document::BlockFormat {
                    heading_level: Some(1),
                    ..Default::default()
                })
            },
            Some("abc! def"),
        ),
        (
            "create_list",
            |_, c| c.create_list(text_document::ListStyle::Disc),
            Some("abc! def"),
        ),
        (
            "insert_list",
            |_, c| c.insert_list(text_document::ListStyle::Decimal),
            None,
        ),
        (
            "toggle_blockquote",
            |_, c| c.toggle_blockquote(),
            Some("abc! def"),
        ),
        ("undo", |d, _| d.undo(), Some("abc def")),
        ("redo", |d, _| d.redo(), Some("abc! def")),
        (
            "set_plain_text",
            |d, _| d.set_plain_text("new"),
            Some("new"),
        ),
        ("clear", |d, _| d.clear(), Some("")),
        (
            "set_djot_sync",
            |d, _| d.set_djot_sync("new").map(|_| ()),
            Some("new"),
        ),
        (
            "set_djot",
            |d, _| d.set_djot("new")?.wait().map(|_| ()),
            Some("new"),
        ),
        (
            "set_markdown",
            |d, _| d.set_markdown("new")?.wait().map(|_| ()),
            Some("new"),
        ),
        (
            "set_html",
            |d, _| d.set_html("<p>new</p>")?.wait().map(|_| ()),
            Some("new"),
        ),
        (
            "replace_text",
            |d, _| {
                d.replace_text("abc", "xyz", true, &ReplaceOptions::default())
                    .map(|_| ())
            },
            Some("xyz! def"),
        ),
        (
            "replace_ranges",
            |d, _| {
                d.replace_ranges(
                    &[ReplaceRange {
                        position: 0,
                        length: 3,
                        replacement: "xyz".into(),
                    }],
                    &ReplaceOptions::default(),
                )
                .map(|_| ())
            },
            Some("xyz! def"),
        ),
        (
            "find_and_replace",
            |d, _| {
                d.find_and_replace(
                    "abc",
                    &ReplaceOptions::new(FindOptions::default()),
                    |_, _| Some("xyz".into()),
                )
                .map(|_| ())
            },
            Some("xyz! def"),
        ),
        (
            "add_resource",
            |d, _| d.add_resource(text_document::ResourceType::Image, "img", "image/png", &[0]),
            Some("abc! def"),
        ),
        ("set_title", |d, _| d.set_title("Title"), Some("abc! def")),
        (
            "set_text_direction",
            |d, _| d.set_text_direction(text_document::TextDirection::RightToLeft),
            Some("abc! def"),
        ),
        (
            "set_default_wrap_mode",
            |d, _| d.set_default_wrap_mode(text_document::WrapMode::WordWrap),
            Some("abc! def"),
        ),
        (
            "set_default_language",
            |d, _| d.set_default_language("fr"),
            Some("abc! def"),
        ),
        (
            "append_line",
            |d, _| d.append_line("more").map(|_| ()),
            Some("abc! def\nmore"),
        ),
        (
            "append_lines",
            |d, _| d.append_lines(["more", "again"]).map(|_| ()),
            Some("abc! def\nmore\nagain"),
        ),
        (
            "truncate_front",
            |d, _| {
                d.append_line("more")?;
                d.truncate_front(1).map(|_| ())
            },
            Some("more"),
        ),
    ];
    for (name, write, expected) in writes {
        let doc = doc("abc def");
        let cursor = doc.cursor_at(3);
        type_text(&cursor, "!");
        if name == "redo" {
            doc.undo().unwrap();
            cursor.set_position(3, MoveMode::MoveAnchor);
            type_text(&cursor, "");
        }
        compose(&doc, &cursor, &["X", "XY"]);
        write(&doc, &cursor).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!doc.has_transient_edits(), "{name} left the composition");
        assert!(
            !text(&doc).contains("XY"),
            "{name} wrote over the composition: {:?}",
            text(&doc)
        );
        if let Some(expected) = expected {
            assert_eq!(text(&doc), expected, "{name}");
        }
    }
}

#[test]
fn table_edits_revert_transient_edits_first() {
    type Write = fn(&text_document::TextCursor, usize) -> Result<(), DocumentError>;
    let writes: Vec<(&str, Write)> = vec![
        ("insert_table_row", |c, t| c.insert_table_row(t, 0)),
        ("insert_table_column", |c, t| c.insert_table_column(t, 0)),
        ("remove_table_row", |c, t| c.remove_table_row(t, 0)),
        ("remove_table_column", |c, t| c.remove_table_column(t, 0)),
        ("merge_table_cells", |c, t| {
            c.merge_table_cells(t, 0, 0, 0, 1)
        }),
        ("remove_table", |c, t| c.remove_table(t)),
    ];
    for (name, write) in writes {
        let doc = doc("ab");
        let table = doc.cursor_at(2).insert_table(2, 2).unwrap().id();
        doc.clear_undo_redo();
        let cursor = doc.cursor_at(1);
        compose(&doc, &cursor, &["X", "XY"]);
        write(&cursor, table).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!doc.has_transient_edits(), "{name} left the composition");
        assert!(!text(&doc).contains("XY"), "{name}: {:?}", text(&doc));
        assert!(doc.can_undo(), "{name} recorded nothing");
    }
}

#[test]
fn a_selection_of_what_followed_a_transient_removal_holds_only_it() {
    // "hello" with "ll" removed transiently reads "heo": its "o", live 2..3, is the
    // recorded "o" at 4..5, and a recorded edit over it replaces that one character.
    let removed = || {
        let doc = doc("hello");
        let cursor = doc.cursor();
        select(&cursor, 2, 4);
        cursor.remove_selected_text_transient().unwrap();
        assert_eq!(text(&doc), "heo");
        (doc, cursor)
    };

    let (doc, cursor) = removed();
    select(&cursor, 2, 3);
    cursor.insert_text("X").unwrap();
    assert_eq!(text(&doc), "hellX");

    let (doc, cursor) = removed();
    cursor.replace(2, 3, "X", Default::default()).unwrap();
    assert_eq!(text(&doc), "hellX");

    let (doc, _) = removed();
    doc.replace_ranges(
        &[ReplaceRange {
            position: 2,
            length: 1,
            replacement: "X".into(),
        }],
        &ReplaceOptions::default(),
    )
    .unwrap();
    assert_eq!(text(&doc), "hellX");

    // And "he", live 0..2, is "he".
    let (doc, cursor) = removed();
    select(&cursor, 0, 2);
    cursor.insert_text("Y").unwrap();
    assert_eq!(text(&doc), "Yllo");
}

#[test]
fn a_cursor_only_the_transient_edits_moved_goes_back_where_it_stood() {
    let doc = doc("hello world");
    let at_removal = doc.cursor_at(2);
    let before_o = doc.cursor_at(4);
    let world = doc.cursor();
    select(&world, 11, 6);

    let ime = doc.cursor();
    select(&ime, 2, 4);
    ime.remove_selected_text_transient().unwrap();
    ime.insert_text_transient("LL").unwrap();
    assert_eq!(text(&doc), "heLLo world");
    assert_eq!((at_removal.position(), before_o.position()), (2, 2));

    doc.revert_transient_edits();
    assert_eq!(at_removal.position(), 2);
    assert_eq!(before_o.position(), 4);
    assert_eq!((world.anchor(), world.position()), (11, 6));
}

#[test]
fn a_cursor_only_the_transient_edits_moved_keeps_the_cells_it_selected() {
    let doc = doc("ab");
    let table = doc.cursor_at(2).insert_table(2, 2).unwrap();
    doc.clear_undo_redo();
    let cells = doc.cursor();
    cells.select_table_cell(table.id(), 1, 1);
    assert_eq!(cells.selected_cells().len(), 1);
    let ime = doc.cursor_at(1);
    compose(&doc, &ime, &["n", "ni"]);
    doc.revert_transient_edits();
    assert_eq!(
        cells.selected_cells().len(),
        1,
        "the cell selection is back"
    );
    assert_eq!(
        cells.selection_kind(),
        text_document::SelectionKind::Cells(text_document::CellRange {
            table_id: table.id(),
            start_row: 1,
            start_col: 1,
            end_row: 1,
            end_col: 1,
        })
    );
}

#[test]
fn a_cursor_moved_during_a_composition_is_mapped_back_with_its_selection() {
    let doc = doc("hello world");
    let ime = doc.cursor();
    select(&ime, 2, 4);
    ime.remove_selected_text_transient().unwrap();
    // A selection made meanwhile, of "o wor" in "heo world".
    let later = doc.cursor();
    select(&later, 2, 7);

    doc.revert_transient_edits();
    let selected = later.selected_text().unwrap();
    assert_eq!(
        selected, "o wor",
        "the recorded text it held, and only that"
    );
}

#[test]
fn a_revert_gives_back_formats_lists_and_images_exactly() {
    let doc = TextDocument::new();
    doc.set_djot_sync("Some *bold* and _italic_ text.\n\n- one\n- two\n\n> quoted")
        .unwrap();
    let cursor = doc.cursor_at(2);
    cursor.insert_image("img", "", 4, 4).unwrap();
    doc.clear_undo_redo();
    let before = doc.to_djot().unwrap();
    let html = doc.to_html().unwrap();

    let ime = doc.cursor();
    select(&ime, 3, doc.character_count());
    ime.remove_selected_text_transient().unwrap();
    ime.insert_text_transient("composition").unwrap();
    assert!(text(&doc).contains("composition"));
    assert!(
        doc.to_djot().unwrap().contains("composition"),
        "an export sees the transient text"
    );

    doc.revert_transient_edits();
    assert_eq!(doc.to_djot().unwrap(), before);
    assert_eq!(doc.to_html().unwrap(), html);
    assert!(!doc.can_undo());
}

#[test]
fn a_panic_after_a_change_that_announced_nothing_announces_it() {
    struct PanicsOnce(AtomicBool);
    impl SyntaxHighlighter for PanicsOnce {
        fn highlight_block(&self, text: &str, _ctx: &mut HighlightContext) {
            if text.contains('!') && !self.0.swap(true, Ordering::SeqCst) {
                panic!("a highlighter bug");
            }
        }
    }
    let doc = doc("abc");
    doc.set_syntax_highlighter(Some(Arc::new(PanicsOnce(AtomicBool::new(false)))));
    doc.poll_events();
    let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _subscription = doc.on_change({
        let heard = Arc::clone(&heard);
        move |event| heard.lock().unwrap().push(event)
    });
    let cursor = doc.cursor_at(3);
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cursor.insert_text_transient("!")
    }));
    assert!(caught.is_err());
    assert_eq!(contents_changes(&doc.poll_events()), [(3, 0, 1)]);
    // A callback hears it at once, with nothing else done to the document since.
    assert_eq!(contents_changes(&heard.lock().unwrap()), [(3, 0, 1)]);
}

#[test]
fn a_panic_after_paragraphs_were_merged_announces_the_block_count() {
    struct PanicsOnMerged(AtomicBool);
    impl SyntaxHighlighter for PanicsOnMerged {
        fn highlight_block(&self, text: &str, _ctx: &mut HighlightContext) {
            if text == "Fircond" && !self.0.swap(true, Ordering::SeqCst) {
                panic!("a highlighter bug");
            }
        }
    }
    let doc = doc("First\nSecond");
    doc.set_syntax_highlighter(Some(Arc::new(PanicsOnMerged(AtomicBool::new(false)))));
    doc.poll_events();
    let cursor = doc.cursor();
    select(&cursor, 3, 8);
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cursor.remove_selected_text_transient()
    }));
    assert!(caught.is_err());
    assert_eq!(block_texts(&doc), ["Fircond"]);
    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, DocumentEvent::BlockCountChanged(1))),
        "{events:?}"
    );
    doc.revert_transient_edits();
    assert_eq!(block_texts(&doc), ["First", "Second"]);
}

#[test]
fn a_write_reaching_the_store_another_way_is_refused() {
    let doc = doc("abc");
    let store = doc.rope_store_for_test();
    assert!(store.is_write_allowed());

    doc.cursor_at(3).insert_text_transient("n").unwrap();
    assert!(store.has_transient_edits());
    assert!(!store.is_write_allowed());
    let refused = store.check_write_allowed().unwrap_err();
    assert!(
        refused
            .downcast_ref::<common::database::rope_store::TransientEditsOutstanding>()
            .is_some()
    );

    doc.revert_transient_edits();
    assert!(!store.has_transient_edits());
    assert!(store.is_write_allowed());
}

// ── What is refused ─────────────────────────────────────────────

#[test]
fn a_scattered_layer_reverts_but_does_not_commit() {
    let doc = doc("abc def");
    let cursor = doc.cursor_at(0);
    cursor.insert_text_transient("X").unwrap();
    cursor.set_position(8, MoveMode::MoveAnchor);
    cursor.insert_text_transient("Y").unwrap();
    assert_eq!(text(&doc), "Xabc defY");

    let refused = cursor.commit_transient_text("Z").unwrap_err();
    assert!(matches!(
        refused,
        DocumentError::TransientEdits(TransientRefusal::CommitInSeveralPlaces)
    ));
    assert!(
        doc.has_transient_edits(),
        "the refusal leaves them as they were"
    );
    assert_eq!(text(&doc), "Xabc defY");

    doc.revert_transient_edits();
    assert_eq!(text(&doc), "abc def");
    assert!(!doc.can_undo());
}

#[test]
fn a_transient_edit_across_table_cells_is_refused() {
    let doc = doc("ab");
    let table = doc.cursor_at(2).insert_table(1, 2).unwrap();
    let first = table.cell(0, 0).unwrap().blocks()[0].position();
    let second = table.cell(0, 1).unwrap().blocks()[0].position();
    doc.cursor_at(first).insert_text("left").unwrap();
    let second = second + 4;
    doc.cursor_at(second).insert_text("right").unwrap();
    doc.clear_undo_redo();
    let before = text(&doc);

    let cursor = doc.cursor();
    select(&cursor, first + 2, second + 2);
    let refused = cursor.insert_text_transient("X").unwrap_err();
    assert!(matches!(
        refused,
        DocumentError::TransientEdits(TransientRefusal::CrossesTableCells)
    ));
    assert_eq!(text(&doc), before);
    assert!(!doc.has_transient_edits());
}

#[test]
fn a_transient_edit_is_refused_while_a_load_in_place_writes() {
    let paragraphs: Vec<String> = (0..4000)
        .map(|n| format!("Paragraph {n} of the loaded text."))
        .collect();
    let loaded = paragraphs.join("\n\n");
    let doc = doc("abc");
    // The load runs on a thread of its own: a transient edit made right after it starts
    // finds it still writing, almost always. One that finds it done is made as usual.
    let mut refusals = 0;
    for _ in 0..10 {
        let load = doc.set_djot(&loaded).unwrap();
        match doc.cursor_at(0).insert_text_transient("n") {
            Err(DocumentError::TransientEdits(TransientRefusal::LoadInPlace)) => {
                refusals += 1;
                assert!(!doc.has_transient_edits());
            }
            Err(other) => panic!("refused as {other:?}"),
            Ok(()) => {
                assert!(load.is_done());
                doc.revert_transient_edits();
            }
        }
        load.wait().unwrap();
        assert!(!doc.has_transient_edits());
        assert!(text(&doc).starts_with("Paragraph 0 of the loaded text."));
    }
    assert!(refusals > 0, "no transient edit found a load still writing");
}

#[test]
fn the_stores_refusal_is_a_transient_edits_error() {
    use common::database::rope_store::TransientEditsOutstanding;

    let plain = anyhow::Error::new(TransientEditsOutstanding);
    assert!(matches!(
        DocumentError::from(plain),
        DocumentError::TransientEdits(TransientRefusal::LayerOutstanding)
    ));
    let wrapped = anyhow::Error::new(TransientEditsOutstanding).context("inserting text");
    let error = DocumentError::from(wrapped);
    assert!(matches!(
        error,
        DocumentError::TransientEdits(TransientRefusal::LayerOutstanding)
    ));
    assert!(
        error
            .to_string()
            .contains("transient edits are outstanding")
    );
    assert_eq!(
        error.to_string(),
        TransientRefusal::LayerOutstanding.to_string()
    );
    assert!(matches!(
        DocumentError::from(anyhow::anyhow!("something else")),
        DocumentError::Internal(_)
    ));
}

#[test]
fn a_transient_write_refused_through_anyhow_is_a_write_in_progress() {
    use common::database::rope_store::TransientWriteRefused;

    let refused =
        anyhow::Error::new(TransientWriteRefused::TransientWriteInProgress).context("composing");
    assert!(matches!(
        DocumentError::from(refused),
        DocumentError::TransientEdits(TransientRefusal::WriteInProgress)
    ));
}

#[test]
fn a_recorded_edit_racing_a_transient_edit_on_another_thread_is_refused_as_one() {
    // A recorded edit reverts the transient edits, then makes its write: a transient edit
    // another thread makes between the two leaves the store refusing the write.
    let doc = Arc::new(doc("abc"));
    let rounds = 300;
    let composer = {
        let doc = Arc::clone(&doc);
        std::thread::spawn(move || {
            let cursor = doc.cursor_at(0);
            for _ in 0..rounds {
                let _ = cursor.insert_text_transient("x");
                doc.revert_transient_edits();
            }
        })
    };
    let typist = doc.cursor_at(0);
    let mut typed = 0;
    for _ in 0..rounds {
        match typist.insert_text("a") {
            Ok(()) => typed += 1,
            Err(DocumentError::TransientEdits(TransientRefusal::LayerOutstanding)) => {}
            Err(other) => panic!("refused as {other:?}"),
        }
    }
    composer.join().unwrap();
    doc.revert_transient_edits();
    let text = text(&doc);
    assert_eq!(text.matches('a').count(), typed + 1, "{text:?}");
    assert_eq!(text.matches('x').count(), 0, "{text:?}");
}

#[test]
fn a_transient_edit_that_changes_nothing_leaves_nothing_outstanding() {
    let doc = doc("abc");
    let cursor = doc.cursor_at(1);
    cursor.insert_text_transient("").unwrap();
    assert_eq!(cursor.remove_selected_text_transient().unwrap(), "");
    assert!(!doc.has_transient_edits());
    assert_eq!(doc.transient_range(), None);
}

// ── Properties ──────────────────────────────────────────────────

/// One transient edit: an insertion of `text`, or a removal, over the selection between two
/// positions given as fractions of the document's length.
#[derive(Debug, Clone)]
enum Transient {
    Insert(f64, f64, String),
    Remove(f64, f64),
}

fn arb_transient() -> impl Strategy<Value = Transient> {
    prop_oneof![
        (0.0f64..=1.0, 0.0f64..=1.0, "[a-z한韓 ]{0,4}")
            .prop_map(|(a, b, text)| Transient::Insert(a, b, text)),
        (0.0f64..=1.0, 0.0f64..=1.0).prop_map(|(a, b)| Transient::Remove(a, b)),
    ]
}

fn end_of(doc: &TextDocument) -> usize {
    doc.character_count() + doc.block_count() - 1
}

fn at(doc: &TextDocument, fraction: f64) -> usize {
    ((fraction * end_of(doc) as f64).floor() as usize).min(end_of(doc))
}

/// `text` with `inserted` put in at character `at`.
fn spliced(text: &str, at: usize, removed: usize, inserted: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out: String = chars[..at].iter().collect();
    out.push_str(inserted);
    out.extend(&chars[at + removed..]);
    out
}

proptest! {
    /// Whatever transient edits were made, a revert gives back the recorded document: its
    /// paragraphs, their identities, its history, and every cursor nothing else moved,
    /// exactly where it stood.
    #[test]
    fn a_revert_gives_back_the_recorded_document(
        initial in "[a-z \n]{0,40}",
        typed in "[a-z ]{0,6}",
        riders in prop::collection::vec((0.0f64..=1.0, 0.0f64..=1.0), 0..4),
        edits in prop::collection::vec(arb_transient(), 1..8),
    ) {
        let doc = doc(&initial);
        let cursor = doc.cursor_at(end_of(&doc));
        type_text(&cursor, &typed);
        let texts = block_texts(&doc);
        let ids: Vec<usize> = doc.blocks().iter().map(|block| block.id()).collect();
        let depth = history_depth(&doc);
        let riders: Vec<_> = riders
            .iter()
            .map(|(a, b)| {
                let rider = doc.cursor();
                select(&rider, at(&doc, *a), at(&doc, *b));
                let stood = (rider.position(), rider.anchor());
                (rider, stood)
            })
            .collect();

        for edit in &edits {
            match edit {
                Transient::Insert(a, b, inserted) => {
                    select(&cursor, at(&doc, *a), at(&doc, *b));
                    cursor.insert_text_transient(inserted).unwrap();
                }
                Transient::Remove(a, b) => {
                    select(&cursor, at(&doc, *a), at(&doc, *b));
                    cursor.remove_selected_text_transient().unwrap();
                }
            }
            prop_assert!(doc.can_undo() == (depth.0 > 0));
        }

        doc.revert_transient_edits();
        prop_assert!(!doc.has_transient_edits());
        prop_assert_eq!(block_texts(&doc), texts);
        let back: Vec<usize> = doc.blocks().iter().map(|block| block.id()).collect();
        prop_assert_eq!(back, ids);
        for (rider, stood) in &riders {
            prop_assert_eq!((rider.position(), rider.anchor()), *stood);
        }
        prop_assert!(cursor.position() <= end_of(&doc));
        prop_assert_eq!(history_depth(&doc), depth);
    }

    /// A composition updated any number of times and committed is one undo step of its own:
    /// the committed text where the composition was, the rest untouched, and one undo back
    /// to the typing before it, which a second undo takes back.
    #[test]
    fn a_committed_composition_is_one_step_of_its_own(
        initial in "[a-z \n]{0,30}",
        fraction in 0.0f64..=1.0,
        states in prop::collection::vec("[a-z]{1,5}", 1..6),
        committed in "[一-龥]{0,3}",
    ) {
        let doc = doc(&initial);
        let cursor = doc.cursor_at(at(&doc, fraction));
        let typed_at = cursor.position();
        type_text(&cursor, "pq");
        let typed = text(&doc);
        let start = cursor.position();
        let states: Vec<&str> = states.iter().map(String::as_str).collect();
        compose(&doc, &cursor, &states);
        let last = states.last().unwrap();
        prop_assert_eq!(text(&doc), spliced(&typed, start, 0, last));

        cursor.commit_transient_text(&committed).unwrap();
        prop_assert_eq!(text(&doc), spliced(&typed, start, 0, &committed));
        prop_assert_eq!(cursor.position(), start + committed.chars().count());
        if !committed.is_empty() {
            doc.undo().unwrap();
            prop_assert_eq!(text(&doc), typed.clone());
        }
        prop_assert!(doc.can_undo(), "the typing is still there to undo");
        doc.undo().unwrap();
        prop_assert_eq!(text(&doc), spliced(&typed, typed_at, 2, ""));
        prop_assert!(!doc.can_undo());
    }

    /// After a transient removal, a range read in the document as it shows is the recorded
    /// text it holds: a recorded replacement of it replaces that text, and only that.
    #[test]
    fn a_range_read_over_a_transient_removal_holds_the_same_text(
        initial in "[a-z ]{1,30}",
        removal in (0.0f64..=1.0, 0.0f64..=1.0),
        range in (0.0f64..=1.0, 0.0f64..=1.0),
    ) {
        let doc = doc(&initial);
        let ime = doc.cursor();
        let (a, b) = (at(&doc, removal.0), at(&doc, removal.1));
        let (a, b) = (a.min(b), a.max(b));
        select(&ime, a, b);
        ime.remove_selected_text_transient().unwrap();
        let live = text(&doc);
        let (s, e) = (at(&doc, range.0), at(&doc, range.1));
        let (s, e) = (s.min(e), s.max(e));
        prop_assume!(s < e && (e <= a || s >= a));
        // The model: before the removal point a position is itself, from it on it is past
        // the removed text.
        let recorded = |p: usize| if p < a || (p == a && p == e) { p } else { p + (b - a) };
        let (rs, re) = (recorded(s), recorded(e));
        let held: String = live.chars().skip(s).take(e - s).collect();
        let original: String = initial.chars().skip(rs).take(re - rs).collect();
        prop_assert_eq!(&held, &original);

        doc.cursor().replace(s, e, "X", Default::default()).unwrap();
        prop_assert_eq!(text(&doc), spliced(&initial, rs, re - rs, "X"));
    }
}
