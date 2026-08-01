//! Sentence granularity on the cursor: `SelectionType::SentenceUnderCursor` and the four
//! sentence `MoveOperation`s, plus the per-cursor `content_locale` that tailors them.

use text_document::{MoveMode, MoveOperation, SelectionType, TextDocument};

fn doc_with(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

const THREE: &str = "One is first. Two is second. Three is third.";
//                   0            13            27            43

// ── selection ──

#[test]
fn select_sentence_under_cursor_takes_the_whole_sentence() {
    let doc = doc_with(THREE);
    let cursor = doc.cursor_at(16); // inside "Two is second."
    cursor.select(SelectionType::SentenceUnderCursor);
    assert_eq!(cursor.selected_text().unwrap(), "Two is second.");
}

/// The selection stops at the terminator, not at the space before the next sentence — a
/// trailing space in the selection would be pasted along with it.
#[test]
fn the_selection_excludes_the_space_after_the_terminator() {
    let doc = doc_with(THREE);
    let cursor = doc.cursor_at(2);
    cursor.select(SelectionType::SentenceUnderCursor);
    let text = cursor.selected_text().unwrap();
    assert_eq!(text, "One is first.");
    assert!(!text.ends_with(' '));
}

/// A block with nothing to select leaves the cursor alone rather than collapsing it somewhere
/// surprising — the same contract `WordUnderCursor` has off a word.
#[test]
fn selecting_in_an_empty_block_is_a_no_op() {
    let doc = doc_with("Text.\n\nMore.");
    let blank = "Text.\n".chars().count();
    let cursor = doc.cursor_at(blank);
    cursor.select(SelectionType::SentenceUnderCursor);
    assert!(!cursor.has_selection());
    assert_eq!(cursor.position(), blank);
}

// ── the content locale ──

#[test]
fn the_content_locale_tailors_the_selection() {
    let doc = doc_with("Mr. Smith went home.");
    let cursor = doc.cursor_at(6);

    // Untailored: UAX #29 ends a sentence at the abbreviation's period.
    assert_eq!(cursor.content_locale(), None, "untailored by default");
    cursor.select(SelectionType::SentenceUnderCursor);
    assert_eq!(cursor.selected_text().unwrap(), "Smith went home.");

    // English knows the title.
    let cursor = doc.cursor_at(6);
    cursor.set_content_locale(Some("en-US"));
    assert_eq!(cursor.content_locale().as_deref(), Some("en-US"));
    cursor.select(SelectionType::SentenceUnderCursor);
    assert_eq!(cursor.selected_text().unwrap(), "Mr. Smith went home.");
}

/// A clone reads the same text, so it must read it in the same language — otherwise a cloned
/// cursor would silently segment differently from the one it came from.
#[test]
fn a_cloned_cursor_inherits_the_content_locale() {
    let doc = doc_with("M. Dupont est parti.");
    let cursor = doc.cursor_at(5);
    cursor.set_content_locale(Some("fr-FR"));

    let clone = cursor.clone();
    assert_eq!(clone.content_locale().as_deref(), Some("fr-FR"));
    clone.select(SelectionType::SentenceUnderCursor);
    assert_eq!(clone.selected_text().unwrap(), "M. Dupont est parti.");
}

// ── movement ──

#[test]
fn start_and_end_of_sentence_reach_its_edges() {
    let doc = doc_with(THREE);

    let cursor = doc.cursor_at(20);
    cursor.move_position(MoveOperation::StartOfSentence, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 14, "the start of \"Two is second.\"");

    let cursor = doc.cursor_at(20);
    cursor.move_position(MoveOperation::EndOfSentence, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 28, "just past its terminator");
}

/// Repeating the operation must keep making progress rather than stalling on the edge it has
/// already reached.
#[test]
fn repeating_start_of_sentence_walks_backwards() {
    let doc = doc_with(THREE);
    let cursor = doc.cursor_at(30); // inside the third sentence

    cursor.move_position(MoveOperation::StartOfSentence, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 29);
    cursor.move_position(MoveOperation::StartOfSentence, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 14);
    cursor.move_position(MoveOperation::StartOfSentence, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 0);
    // At the very start it stays put instead of running off the front.
    cursor.move_position(MoveOperation::StartOfSentence, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 0);
}

#[test]
fn next_and_previous_sentence_step_between_them() {
    let doc = doc_with(THREE);
    let cursor = doc.cursor_at(0);

    cursor.move_position(MoveOperation::NextSentence, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 14);
    cursor.move_position(MoveOperation::NextSentence, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 29);

    cursor.move_position(MoveOperation::PreviousSentence, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 14);
    cursor.move_position(MoveOperation::PreviousSentence, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 0);
}

/// The repeat count is the same `n` every other movement takes.
#[test]
fn the_repeat_count_applies() {
    let doc = doc_with(THREE);
    let cursor = doc.cursor_at(0);
    cursor.move_position(MoveOperation::NextSentence, MoveMode::MoveAnchor, 2);
    assert_eq!(cursor.position(), 29);
}

/// `KeepAnchor` extends a selection, exactly as it does for word and block movement.
#[test]
fn keep_anchor_extends_the_selection_by_sentences() {
    let doc = doc_with(THREE);
    let cursor = doc.cursor_at(0);
    cursor.move_position(MoveOperation::EndOfSentence, MoveMode::KeepAnchor, 1);
    assert!(cursor.has_selection());
    assert_eq!(cursor.selected_text().unwrap(), "One is first.");
}

/// Movement must not run past the end of the document, whatever the repeat count.
#[test]
fn movement_stops_at_the_document_edges() {
    let doc = doc_with(THREE);
    let total = doc.character_count();

    let cursor = doc.cursor_at(0);
    cursor.move_position(MoveOperation::NextSentence, MoveMode::MoveAnchor, 99);
    assert!(cursor.position() <= total, "ran past the end");

    let cursor = doc.cursor_at(total);
    cursor.move_position(MoveOperation::EndOfSentence, MoveMode::MoveAnchor, 99);
    assert!(cursor.position() <= total, "ran past the end");
}

/// Movement is block-scoped like the query behind it, so it never steps over a paragraph break
/// into a sentence the writer did not mean to reach.
#[test]
fn movement_respects_block_boundaries() {
    let doc = doc_with("First block.\nSecond block.");
    let cursor = doc.cursor_at(2);
    cursor.move_position(MoveOperation::EndOfSentence, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 12, "stops at the end of its own block");
}
