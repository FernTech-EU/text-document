//! Text typed at the start of a paragraph takes the format of the paragraph's
//! first character, as in Qt (`QTextCursor::charFormat` at a block start),
//! Word and TextEdit: typed at the start of a bold paragraph, it is bold.
//! Anywhere else typed text takes the format of the character before it.

use text_document::{MoveMode, TextDocument, TextFormat};

/// A document of `text`, with `[start, end)` made bold.
fn doc_with_bold(text: &str, start: usize, end: usize) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    let cursor = doc.cursor_at(start);
    cursor.set_position(end, MoveMode::KeepAnchor);
    cursor
        .merge_char_format(&TextFormat {
            font_bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    doc.clear_undo_redo();
    doc
}

fn bold_at(doc: &TextDocument, at: usize) -> bool {
    doc.cursor_at(at).char_format().unwrap().font_bold == Some(true)
}

#[test]
fn typing_at_the_start_of_a_bold_paragraph_is_bold() {
    let doc = doc_with_bold("Bold", 0, 4);
    doc.cursor_at(0).insert_text("XY").unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "XYBold");
    assert!(
        (0..6).all(|at| bold_at(&doc, at)),
        "the typed text takes the first character's format"
    );
}

#[test]
fn typing_at_the_start_of_a_later_paragraph_takes_its_first_character() {
    let doc = doc_with_bold("plain\nBold", 6, 10);
    doc.cursor_at(6).insert_text("X").unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "plain\nXBold");
    assert!(bold_at(&doc, 6), "not the previous paragraph's format");
    assert!(!bold_at(&doc, 4));
}

#[test]
fn typing_at_the_start_of_a_paragraph_that_starts_plain_is_plain() {
    let doc = doc_with_bold("ab cd", 3, 5);
    doc.cursor_at(0).insert_text("X").unwrap();
    assert!(!bold_at(&doc, 0));
    assert!(bold_at(&doc, 4), "the bold word is untouched");
}

#[test]
fn typing_in_a_paragraph_still_takes_the_character_before() {
    let doc = doc_with_bold("ab cd", 0, 2);
    // After the bold "ab": bold. Before the plain "cd", after the space: plain.
    doc.cursor_at(2).insert_text("X").unwrap();
    assert!(bold_at(&doc, 2));
    doc.cursor_at(4).insert_text("Y").unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "abX Ycd");
    assert!(!bold_at(&doc, 4));
}

#[test]
fn typing_in_an_empty_paragraph_is_plain() {
    let doc = doc_with_bold("Bold\n\nafter", 0, 4);
    doc.cursor_at(5).insert_text("X").unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Bold\nX\nafter");
    assert!(!bold_at(&doc, 5), "an empty paragraph keeps no format");
}

#[test]
fn undo_takes_the_typed_text_and_its_format_back() {
    let doc = doc_with_bold("Bold", 0, 4);
    doc.cursor_at(0).insert_text("X").unwrap();
    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Bold");
    assert!((0..4).all(|at| bold_at(&doc, at)));
    doc.redo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "XBold");
    assert!(bold_at(&doc, 0));
}

#[test]
fn a_composition_at_the_start_of_a_bold_paragraph_is_shown_and_committed_bold() {
    let doc = doc_with_bold("Bold", 0, 4);
    let cursor = doc.cursor_at(0);
    cursor.insert_text_transient("ni").unwrap();
    assert!(bold_at(&doc, 0), "shown bold");
    cursor.commit_transient_text("\u{4F60}").unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "\u{4F60}Bold");
    assert!(bold_at(&doc, 0), "committed bold");
}

#[test]
fn typing_before_a_link_that_opens_a_paragraph_is_not_part_of_the_link() {
    let doc = TextDocument::new();
    doc.set_html(r#"<p><a href="https://example.org"><b>Link</b></a> after</p>"#)
        .unwrap()
        .wait()
        .unwrap();
    doc.cursor_at(0).insert_text("X").unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "XLink after");
    let typed = doc.cursor_at(0).char_format().unwrap();
    assert_eq!(
        typed.anchor_href, None,
        "the typed text is not part of the link"
    );
    assert_eq!(typed.is_anchor, None);
    assert_eq!(
        typed.font_bold,
        Some(true),
        "it keeps the rest of the format"
    );
    let linked = doc.cursor_at(1).char_format().unwrap();
    assert_eq!(linked.anchor_href.as_deref(), Some("https://example.org"));
}
