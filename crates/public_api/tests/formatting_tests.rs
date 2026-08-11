use text_document::{Alignment, BlockFormat, MoveMode, TextDocument, TextFormat};

fn new_doc_with_text(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

#[test]
fn char_format_at_position() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor();
    let fmt = cursor.char_format().unwrap();
    // Default format: all None
    assert_eq!(fmt.font_bold, None);
    assert_eq!(fmt.font_italic, None);
}

#[test]
fn set_char_format_bold() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor();
    // Select all text
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(5, MoveMode::KeepAnchor);

    let fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    cursor.set_char_format(&fmt).unwrap();

    // Check format at position 0
    let read_cursor = doc.cursor_at(0);
    let result_fmt = read_cursor.char_format().unwrap();
    assert_eq!(result_fmt.font_bold, Some(true));
}

#[test]
fn merge_char_format_preserves_existing() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(5, MoveMode::KeepAnchor);

    // First set bold
    let bold_fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    cursor.set_char_format(&bold_fmt).unwrap();

    // Then merge italic only — bold should be preserved (None = don't touch)
    let italic_fmt = TextFormat {
        font_italic: Some(true),
        ..Default::default()
    };
    cursor.merge_char_format(&italic_fmt).unwrap();

    let read_cursor = doc.cursor_at(0);
    let result_fmt = read_cursor.char_format().unwrap();
    assert_eq!(result_fmt.font_bold, Some(true));
    assert_eq!(result_fmt.font_italic, Some(true));
}

#[test]
fn block_format_at_position() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor();
    let fmt = cursor.block_format().unwrap();
    // Default: no alignment set
    assert_eq!(fmt.alignment, None);
}

#[test]
fn set_block_format_alignment() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(5, MoveMode::KeepAnchor);

    let fmt = BlockFormat {
        alignment: Some(Alignment::Center),
        ..Default::default()
    };
    cursor.set_block_format(&fmt).unwrap();

    let read_cursor = doc.cursor_at(0);
    let result_fmt = read_cursor.block_format().unwrap();
    assert_eq!(result_fmt.alignment, Some(Alignment::Center));
}

#[test]
fn set_char_format_is_undoable() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(5, MoveMode::KeepAnchor);

    let fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    cursor.set_char_format(&fmt).unwrap();
    assert!(doc.can_undo());

    doc.undo().unwrap();
    let read_cursor = doc.cursor_at(0);
    let result_fmt = read_cursor.char_format().unwrap();
    // After undo, bold should be reverted
    assert_ne!(result_fmt.font_bold, Some(true));
}

// ── A caret at the end of a paragraph ────────────────────────────────────────
//
// That offset is the inter-block separator's character index, which
// `get_block_at_position` assigns to the block *after* it — right for walking
// text, wrong for a cursor. Every formatting query is asked about a cursor, so
// all three paths below used to answer about the next paragraph.

/// Heading level per block, read off the blocks themselves so the assertion does
/// not lean on the very queries under test.
fn headings(doc: &TextDocument) -> Vec<u8> {
    doc.blocks()
        .iter()
        .map(|b| b.block_format().heading_level.unwrap_or(0))
        .collect()
}

fn two_paragraphs() -> (TextDocument, usize) {
    let doc = new_doc_with_text("First para.\nSecond para.");
    (doc, "First para.".chars().count())
}

#[test]
fn reading_a_block_format_at_a_paragraph_end_reads_that_paragraph() {
    let (doc, end) = two_paragraphs();
    // Centre the FIRST block only.
    let c = doc.cursor_at(0);
    c.set_position(0, MoveMode::MoveAnchor);
    c.set_block_format(&BlockFormat {
        alignment: Some(Alignment::Center),
        ..Default::default()
    })
    .unwrap();

    assert_eq!(
        doc.block_format_at(end).unwrap().alignment,
        Some(Alignment::Center),
        "the caret has not left the first paragraph, so its format is the answer"
    );

    // And the cursor's own query — what an editor's format panel reads.
    let caret = doc.cursor_at(end);
    caret.set_position(end, MoveMode::MoveAnchor);
    assert_eq!(
        caret.block_format().unwrap().alignment,
        Some(Alignment::Center)
    );

    // One character further along is genuinely the second paragraph.
    assert_eq!(doc.block_format_at(end + 1).unwrap().alignment, None);
}

/// The write half, and the worse one: a collapsed caret at a paragraph's end
/// matched no block at all under the half-open overlap test, so applying a
/// heading there silently did nothing.
#[test]
fn applying_a_block_format_at_a_paragraph_end_formats_that_paragraph() {
    let (doc, end) = two_paragraphs();
    let caret = doc.cursor_at(end);
    caret.set_position(end, MoveMode::MoveAnchor);
    caret
        .set_block_format(&BlockFormat {
            heading_level: Some(1),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(
        headings(&doc),
        vec![1, 0],
        "the heading belongs to the paragraph the caret was in — and must not \
         be dropped on the floor"
    );
}

#[test]
fn applying_a_block_format_at_a_paragraph_start_formats_that_paragraph() {
    let (doc, end) = two_paragraphs();
    let caret = doc.cursor_at(end + 1);
    caret.set_position(end + 1, MoveMode::MoveAnchor);
    caret
        .set_block_format(&BlockFormat {
            heading_level: Some(1),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(headings(&doc), vec![0, 1], "the crossing still happens");
}

/// A collapsed caret formats exactly one block, never two — the end-inclusive
/// rule must not make a paragraph end match both its own block and the next.
#[test]
fn a_collapsed_caret_formats_exactly_one_block_at_every_position() {
    for pos in 0..=new_doc_with_text("First para.\nSecond para.").character_count() {
        let (doc, _) = two_paragraphs();
        let caret = doc.cursor_at(pos);
        caret.set_position(pos, MoveMode::MoveAnchor);
        caret
            .set_block_format(&BlockFormat {
                heading_level: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            headings(&doc).iter().filter(|h| **h == 1).count(),
            1,
            "exactly one block must take the format (caret at {pos})"
        );
    }
}

/// An empty paragraph is still a block a caret can format.
#[test]
fn a_caret_in_an_empty_paragraph_formats_that_empty_paragraph() {
    let doc = new_doc_with_text("Text.\n\nMore.");
    let blank = "Text.\n".chars().count();
    let caret = doc.cursor_at(blank);
    caret.set_position(blank, MoveMode::MoveAnchor);
    caret
        .set_block_format(&BlockFormat {
            heading_level: Some(2),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(headings(&doc), vec![0, 2, 0]);
}
