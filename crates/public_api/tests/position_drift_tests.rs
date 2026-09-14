//! Positions read by a use case must be the positions the caller addressed.
//!
//! `insert_text_uc` and `delete_text_uc` stop shifting every later block's stored
//! `Block.document_position` once the rope is the document's position space, so that
//! field drifts by the characters typed (or removed) since the last time something
//! refreshed it. A use case that compares a caller's position with the stored field is
//! then reading a number that stopped being true at the writer's first keystroke.
//!
//! The cut at a paragraph's end is the report that found it: the fragment carried the
//! paragraph break and the head of the next paragraph away, while the deletion — which
//! walks the rope — took only the selection. Every other test here is the same defect
//! on another surface that addresses a range.

use text_document::{
    BlockFormat, ListStyle, MoveMode, ReplaceOptions, ReplaceRange, TextDocument, TextFormat,
};

const PROSE: &str = "First sentence. Second sentence.\nNext paragraph here.\nThird one.";

/// The document after two characters were typed at its very start, one keystroke each,
/// with no deletion since: exactly the state in which the stored positions of every
/// later block lag the rope by two.
///
/// Layout afterwards: `abFirst sentence. Second sentence.` is 0..34, the break sits at
/// 34, `Next paragraph here.` is 35..55, the break at 55, `Third one.` is 56..66.
fn after_typing_upstream() -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(PROSE).unwrap();
    let typer = doc.cursor_at(0);
    typer.insert_text("a").unwrap();
    typer.insert_text("b").unwrap();
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "abFirst sentence. Second sentence.\nNext paragraph here.\nThird one."
    );
    doc
}

/// The document after two characters were removed from the start of its first paragraph
/// with Backspace, one keystroke each: a deletion refreshes the stored positions on entry
/// and then skips shifting the blocks after it, so every later block now sits one *past*
/// the rope.
///
/// Layout afterwards is that of [`PROSE`]: `First sentence. Second sentence.` is 0..32,
/// the break at 32, `Next paragraph here.` is 33..53, the break at 53, `Third one.` is
/// 54..64.
fn after_deleting_upstream() -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(&format!("ab{PROSE}")).unwrap();
    let eraser = doc.cursor_at(2);
    eraser.delete_previous_char().unwrap();
    eraser.delete_previous_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), PROSE);
    doc
}

#[test]
fn a_cut_at_a_paragraph_end_takes_only_the_selection_after_typing_upstream() {
    let doc = after_typing_upstream();
    let cursor = doc.cursor_at(18);
    cursor.set_position(34, MoveMode::KeepAnchor);
    assert_eq!(cursor.selected_text().unwrap(), "Second sentence.");

    let fragment = cursor.selection();
    assert_eq!(
        fragment.to_plain_text(),
        "Second sentence.",
        "the fragment must not carry the paragraph break and the next paragraph's head"
    );

    cursor.remove_selected_text().unwrap();
    cursor.set_position(8, MoveMode::MoveAnchor);
    cursor.insert_fragment(&fragment).unwrap();
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "abFirst Second sentence.sentence. \nNext paragraph here.\nThird one."
    );
}

#[test]
fn a_cut_from_a_later_paragraph_keeps_its_opening_after_typing_upstream() {
    let doc = TextDocument::new();
    doc.set_plain_text(PROSE).unwrap();
    let typer = doc.cursor_at(33);
    typer.insert_text("a").unwrap();
    typer.insert_text("b").unwrap();
    // `Third one.` now sits at 56..66.
    let cursor = doc.cursor_at(62);
    cursor.set_position(66, MoveMode::KeepAnchor);
    assert_eq!(cursor.selected_text().unwrap(), "one.");
    assert_eq!(cursor.selection().to_plain_text(), "one.");
}

#[test]
fn a_selection_reads_the_same_after_deleting_upstream() {
    let doc = after_deleting_upstream();
    let cursor = doc.cursor_at(33);
    cursor.set_position(37, MoveMode::KeepAnchor);
    assert_eq!(cursor.selected_text().unwrap(), "Next");
    assert_eq!(cursor.selection().to_plain_text(), "Next");
}

#[test]
fn a_selection_spanning_paragraphs_reads_the_same_after_typing_upstream() {
    let doc = after_typing_upstream();
    let cursor = doc.cursor_at(25);
    cursor.set_position(39, MoveMode::KeepAnchor);
    assert_eq!(cursor.selected_text().unwrap(), "sentence.\nNext");
    assert_eq!(cursor.selection().to_plain_text(), "sentence.\nNext");
}

#[test]
fn bold_lands_on_the_selected_word_after_typing_upstream() {
    let doc = after_typing_upstream();
    let cursor = doc.cursor_at(35);
    cursor.set_position(39, MoveMode::KeepAnchor);
    assert_eq!(cursor.selected_text().unwrap(), "Next");
    cursor
        .merge_char_format(&TextFormat {
            font_bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    let html = doc.to_html().unwrap();
    assert!(
        html.contains("<p><strong>Next</strong> paragraph here.</p>"),
        "bold must cover the selected word and nothing else: {html}"
    );
}

#[test]
fn a_replaced_format_lands_on_the_selected_word_after_typing_upstream() {
    let doc = after_typing_upstream();
    let cursor = doc.cursor_at(35);
    cursor.set_position(39, MoveMode::KeepAnchor);
    cursor
        .set_char_format(&TextFormat {
            font_italic: Some(true),
            ..Default::default()
        })
        .unwrap();
    let html = doc.to_html().unwrap();
    assert!(
        html.contains("<p><em>Next</em> paragraph here.</p>"),
        "italic must cover the selected word and nothing else: {html}"
    );
}

#[test]
fn bold_lands_on_the_selected_word_after_deleting_upstream() {
    let doc = after_deleting_upstream();
    let cursor = doc.cursor_at(33);
    cursor.set_position(37, MoveMode::KeepAnchor);
    assert_eq!(cursor.selected_text().unwrap(), "Next");
    cursor
        .merge_char_format(&TextFormat {
            font_bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    let html = doc.to_html().unwrap();
    assert!(
        html.contains("<p><strong>Next</strong> paragraph here.</p>"),
        "bold must cover the selected word and nothing else: {html}"
    );
}

#[test]
fn a_heading_lands_on_the_paragraph_after_typing_upstream() {
    let doc = after_typing_upstream();
    let cursor = doc.cursor_at(37);
    cursor
        .set_block_format(&BlockFormat {
            heading_level: Some(1),
            ..Default::default()
        })
        .unwrap();
    let html = doc.to_html().unwrap();
    assert!(
        html.contains("<h1>Next paragraph here.</h1>") && !html.contains("<h1>Third"),
        "the heading must be the paragraph under the caret: {html}"
    );
}

#[test]
fn a_heading_lands_on_the_paragraph_after_deleting_upstream() {
    let doc = after_deleting_upstream();
    // The caret at the very start of the third paragraph: one character before where the
    // stored positions say that paragraph begins.
    let cursor = doc.cursor_at(54);
    cursor
        .set_block_format(&BlockFormat {
            heading_level: Some(1),
            ..Default::default()
        })
        .unwrap();
    let html = doc.to_html().unwrap();
    assert!(
        html.contains("<h1>Third one.</h1>") && !html.contains("<h1>Next"),
        "the heading must be the paragraph under the caret: {html}"
    );
}

#[test]
fn replace_text_lands_on_the_match_after_typing_upstream() {
    let doc = after_typing_upstream();
    let count = doc
        .replace_text("Next", "NEXT", true, &ReplaceOptions::default())
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "abFirst sentence. Second sentence.\nNEXT paragraph here.\nThird one."
    );
}

#[test]
fn replace_ranges_lands_on_the_range_after_typing_upstream() {
    let doc = after_typing_upstream();
    let count = doc
        .replace_ranges(
            &[ReplaceRange {
                position: 35,
                length: 4,
                replacement: "NEXT".to_string(),
            }],
            &ReplaceOptions::default(),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "abFirst sentence. Second sentence.\nNEXT paragraph here.\nThird one."
    );
}

#[test]
fn replace_ranges_lands_on_the_range_after_deleting_upstream() {
    let doc = after_deleting_upstream();
    let count = doc
        .replace_ranges(
            &[ReplaceRange {
                position: 33,
                length: 4,
                replacement: "NEXT".to_string(),
            }],
            &ReplaceOptions::default(),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "First sentence. Second sentence.\nNEXT paragraph here.\nThird one."
    );
}

#[test]
fn a_list_covers_the_selected_paragraph_alone_after_typing_upstream() {
    let doc = after_typing_upstream();
    let cursor = doc.cursor_at(35);
    cursor.set_position(55, MoveMode::KeepAnchor);
    assert_eq!(cursor.selected_text().unwrap(), "Next paragraph here.");
    cursor.create_list(ListStyle::Disc).unwrap();
    let html = doc.to_html().unwrap();
    assert!(
        html.contains("<li>Next paragraph here.</li>"),
        "the selected paragraph must become the list item: {html}"
    );
    assert!(
        !html.contains("<li>Third one.</li>") && !html.contains("<li>abFirst"),
        "no neighbouring paragraph may be pulled into the list: {html}"
    );
}

#[test]
fn a_blockquote_wraps_the_paragraph_under_the_caret_after_typing_upstream() {
    let doc = after_typing_upstream();
    let cursor = doc.cursor_at(40);
    cursor.insert_blockquote().unwrap();
    let html = doc.to_html().unwrap();
    assert!(
        html.contains("<blockquote><p>Next paragraph here.</p></blockquote>"),
        "the quoted paragraph must be the one under the caret: {html}"
    );
}
