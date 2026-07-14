//! Tests that exercise undo/redo paths on all undoable editing and formatting operations.

use text_document::{
    Alignment, BlockFormat, CharVerticalAlignment, FindOptions, ListStyle, MarkerType, MoveMode,
    MoveOperation, SelectionType, TextDocument, TextFormat, UnderlineStyle,
};

fn new_doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

// ── insert_text undo/redo ───────────────────────────────────────

#[test]
fn undo_redo_insert_text() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);
    c.insert_text(" world").unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello world");

    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");

    doc.redo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello world");
}

// ── delete_text undo/redo ───────────────────────────────────────

#[test]
fn undo_redo_delete_text() {
    let doc = new_doc("Hello world");
    let c = doc.cursor();
    c.move_position(MoveOperation::NextCharacter, MoveMode::KeepAnchor, 5);
    c.remove_selected_text().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), " world");

    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello world");

    doc.redo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), " world");
}

// ── insert_block undo/redo ──────────────────────────────────────

#[test]
fn undo_redo_insert_block() {
    let doc = new_doc("Hello world");
    let c = doc.cursor_at(5);
    c.insert_block().unwrap();
    assert_eq!(doc.block_count(), 2);

    doc.undo().unwrap();
    assert_eq!(doc.block_count(), 1);

    doc.redo().unwrap();
    assert_eq!(doc.block_count(), 2);
}

// ── insert_image undo/redo ──────────────────────────────────────

#[test]
fn undo_redo_insert_image() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);
    c.insert_image("test.png", 100, 50).unwrap();
    assert_eq!(doc.stats().image_count, 1);
    let char_count_after = doc.character_count();

    doc.undo().unwrap();
    assert_eq!(doc.stats().image_count, 0);
    assert_eq!(doc.character_count(), 5);

    doc.redo().unwrap();
    assert_eq!(doc.stats().image_count, 1);
    assert_eq!(doc.character_count(), char_count_after);
}

// ── insert_image at start of text (Empty element case) ──────────

#[test]
fn insert_image_into_empty_doc() {
    let doc = TextDocument::new();
    let c = doc.cursor();
    c.insert_image("img.png", 200, 100).unwrap();
    assert_eq!(doc.stats().image_count, 1);
}

// ── insert_image after another image ─────────────────────────────

#[test]
fn insert_image_after_image() {
    let doc = TextDocument::new();
    let c = doc.cursor();
    c.insert_image("img1.png", 100, 100).unwrap();
    // Now cursor is at position 1 (after the image)
    c.insert_image("img2.png", 200, 200).unwrap();
    assert_eq!(doc.stats().image_count, 2);
}

// ── insert_formatted_text undo/redo ─────────────────────────────

#[test]
fn undo_redo_insert_formatted_text() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);
    let fmt = TextFormat {
        font_bold: Some(true),
        font_italic: Some(true),
        font_family: Some("Arial".into()),
        font_point_size: Some(14),
        ..Default::default()
    };
    c.insert_formatted_text(" bold", &fmt).unwrap();
    assert!(doc.to_plain_text().unwrap().contains("bold"));

    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");

    doc.redo().unwrap();
    assert!(doc.to_plain_text().unwrap().contains("bold"));
}

// ── insert_frame undo/redo ──────────────────────────────────────

#[test]
fn undo_redo_insert_frame() {
    let doc = new_doc("Hello");
    let frames_before = doc.stats().frame_count;
    let c = doc.cursor();
    c.insert_frame().unwrap();
    assert!(doc.stats().frame_count > frames_before);

    doc.undo().unwrap();
    assert_eq!(doc.stats().frame_count, frames_before);

    doc.redo().unwrap();
    assert!(doc.stats().frame_count > frames_before);
}

// ── insert_fragment undo/redo ───────────────────────────────────

#[test]
fn undo_redo_insert_fragment() {
    let source = new_doc("Fragment text");
    let frag = text_document::DocumentFragment::from_document(&source).unwrap();

    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);
    c.insert_fragment(&frag).unwrap();
    let after_insert = doc.to_plain_text().unwrap();
    assert!(after_insert.contains("Fragment"));

    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");

    doc.redo().unwrap();
    let after_redo = doc.to_plain_text().unwrap();
    assert!(after_redo.contains("Fragment"));
}

// ── insert_html undo/redo ───────────────────────────────────────

#[test]
fn undo_redo_insert_html() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);
    c.insert_html("<b>World</b>").unwrap();
    assert!(doc.to_plain_text().unwrap().contains("World"));

    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");

    doc.redo().unwrap();
    assert!(doc.to_plain_text().unwrap().contains("World"));
}

// ── insert_markdown undo/redo ───────────────────────────────────

#[test]
fn undo_redo_insert_markdown() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);
    c.insert_markdown(" **World**").unwrap();
    assert!(doc.to_plain_text().unwrap().contains("World"));

    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");

    doc.redo().unwrap();
    assert!(doc.to_plain_text().unwrap().contains("World"));
}

// ── create_list undo/redo ───────────────────────────────────────

#[test]
fn undo_redo_create_list() {
    let doc = new_doc("Item 1\nItem 2");
    let c = doc.cursor();
    c.select(SelectionType::Document);
    c.create_list(ListStyle::Disc).unwrap();
    assert!(doc.stats().list_count >= 1);

    doc.undo().unwrap();
    // After undo, list count should be back to 0
    assert_eq!(doc.stats().list_count, 0);

    doc.redo().unwrap();
    assert!(doc.stats().list_count >= 1);
}

// ── insert_list undo/redo ───────────────────────────────────────

#[test]
fn undo_redo_insert_list() {
    let doc = new_doc("Before");
    let c = doc.cursor_at(6);
    c.insert_list(ListStyle::Decimal).unwrap();
    let lists_after = doc.stats().list_count;
    assert!(lists_after >= 1);

    doc.undo().unwrap();
    assert_eq!(doc.stats().list_count, 0);

    doc.redo().unwrap();
    assert_eq!(doc.stats().list_count, lists_after);
}

// ── set_text_format undo/redo ───────────────────────────────────

#[test]
fn undo_redo_set_text_format() {
    let doc = new_doc("Hello world");
    let c = doc.cursor();
    c.move_position(MoveOperation::NextCharacter, MoveMode::KeepAnchor, 5);
    let fmt = TextFormat {
        font_bold: Some(true),
        font_italic: Some(true),
        font_underline: Some(true),
        font_overline: Some(true),
        font_strikeout: Some(true),
        font_weight: Some(700),
        font_point_size: Some(24),
        font_family: Some("Courier".into()),
        letter_spacing: Some(5),
        word_spacing: Some(10),
        underline_style: Some(UnderlineStyle::DashUnderline),
        vertical_alignment: Some(CharVerticalAlignment::SuperScript),
        ..Default::default()
    };
    c.set_char_format(&fmt).unwrap();

    // Verify format was applied
    let read_c = doc.cursor_at(0);
    let applied = read_c.char_format().unwrap();
    assert_eq!(applied.font_bold, Some(true));

    doc.undo().unwrap();
    // Format should be reverted
    let read_c = doc.cursor_at(0);
    let reverted = read_c.char_format().unwrap();
    assert_ne!(reverted.font_bold, Some(true));

    doc.redo().unwrap();
    // Format should be re-applied
    let read_c = doc.cursor_at(0);
    let reapplied = read_c.char_format().unwrap();
    assert_eq!(reapplied.font_bold, Some(true));
}

// ── merge_text_format undo/redo ─────────────────────────────────

#[test]
fn undo_redo_merge_text_format() {
    let doc = new_doc("Hello world");
    let c = doc.cursor();
    c.move_position(MoveOperation::NextCharacter, MoveMode::KeepAnchor, 5);

    // First set a format
    let fmt1 = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    c.set_char_format(&fmt1).unwrap();

    // Then merge additional formatting
    let fmt2 = TextFormat {
        font_italic: Some(true),
        font_underline: Some(true),
        font_family: Some("Monospace".into()),
        ..Default::default()
    };
    c.merge_char_format(&fmt2).unwrap();

    // Verify merge result
    let read_c = doc.cursor_at(0);
    let merged = read_c.char_format().unwrap();
    assert_eq!(merged.font_bold, Some(true));
    assert_eq!(merged.font_italic, Some(true));

    doc.undo().unwrap();
    // After undoing merge, italic should be reverted but bold preserved
    let read_c = doc.cursor_at(0);
    let after_undo = read_c.char_format().unwrap();
    assert_eq!(after_undo.font_bold, Some(true));
    assert_ne!(after_undo.font_italic, Some(true));

    doc.redo().unwrap();
    let read_c = doc.cursor_at(0);
    let after_redo = read_c.char_format().unwrap();
    assert_eq!(after_redo.font_bold, Some(true));
    assert_eq!(after_redo.font_italic, Some(true));
}

// ── set_block_format undo/redo ──────────────────────────────────

#[test]
fn undo_redo_set_block_format() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    let fmt = BlockFormat {
        alignment: Some(Alignment::Center),
        heading_level: Some(1),
        indent: Some(2),
        marker: Some(MarkerType::Checked),
        ..Default::default()
    };
    c.set_block_format(&fmt).unwrap();

    // Verify format was applied
    let read_c = doc.cursor_at(0);
    let applied = read_c.block_format().unwrap();
    assert_eq!(applied.alignment, Some(Alignment::Center));
    assert_eq!(applied.heading_level, Some(1));

    doc.undo().unwrap();
    let read_c = doc.cursor_at(0);
    let reverted = read_c.block_format().unwrap();
    assert_ne!(reverted.alignment, Some(Alignment::Center));

    doc.redo().unwrap();
    let read_c = doc.cursor_at(0);
    let reapplied = read_c.block_format().unwrap();
    assert_eq!(reapplied.alignment, Some(Alignment::Center));
}

// ── set_frame_format undo/redo ──────────────────────────────────

#[test]
fn undo_redo_set_frame_format() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    // Get the frame ID from the document
    let frame_id = {
        let stats = doc.stats();
        assert!(stats.frame_count >= 1);
        // Frame ID 2 is typically the root frame in the default document
        2
    };
    let fmt = text_document::FrameFormat {
        width: Some(500),
        height: Some(300),
        top_margin: Some(10),
        bottom_margin: Some(10),
        left_margin: Some(20),
        right_margin: Some(20),
        padding: Some(5),
        border: Some(1),
        ..Default::default()
    };
    c.set_frame_format(frame_id, &fmt).unwrap();

    // Just verify undo/redo don't error — frame format read-back is
    // not exposed at block level, so we verify via the round-trip.
    doc.undo().unwrap();
    assert!(doc.can_redo());

    doc.redo().unwrap();
    assert!(doc.can_undo());
}

// ── replace_text undo/redo ──────────────────────────────────────

#[test]
fn undo_redo_replace_text() {
    let doc = new_doc("foo bar foo baz");
    let opts = FindOptions::default();
    let count = doc
        .replace_text(
            "foo",
            "XXX",
            true,
            &text_document::ReplaceOptions::new(opts.clone()),
        )
        .unwrap();
    assert_eq!(count, 2);
    assert_eq!(doc.to_plain_text().unwrap(), "XXX bar XXX baz");

    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "foo bar foo baz");

    doc.redo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "XXX bar XXX baz");
}

// ── Multiple undos ──────────────────────────────────────────────

#[test]
fn multiple_undo_redo_sequence() {
    let doc = new_doc("A");

    // Use edit blocks to prevent merge between the two inserts
    let c = doc.cursor_at(1);
    c.begin_edit_block();
    c.insert_text("B").unwrap();
    c.end_edit_block();
    assert_eq!(doc.to_plain_text().unwrap(), "AB");

    let c = doc.cursor_at(2);
    c.begin_edit_block();
    c.insert_text("C").unwrap();
    c.end_edit_block();
    assert_eq!(doc.to_plain_text().unwrap(), "ABC");

    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "AB");

    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "A");

    doc.redo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "AB");

    doc.redo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "ABC");
}

// ── Delete char with selection (covers delete_text with selection) ──

#[test]
fn undo_redo_delete_char_with_selection() {
    let doc = new_doc("Hello world");
    let c = doc.cursor();
    c.move_position(MoveOperation::NextCharacter, MoveMode::KeepAnchor, 5);
    c.delete_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), " world");

    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello world");

    doc.redo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), " world");
}

#[test]
fn undo_redo_delete_previous_char() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);
    c.delete_previous_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hell");

    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");

    doc.redo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hell");
}

// ── edit block groups operations ────────────────────────────────

#[test]
fn undo_edit_block_groups_operations() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);

    c.begin_edit_block();
    c.insert_text(" ").unwrap();
    c.insert_text("world").unwrap();
    c.end_edit_block();

    assert_eq!(doc.to_plain_text().unwrap(), "Hello world");

    // One undo should reverse both inserts
    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");
}

// ── undo with no history ────────────────────────────────────────

#[test]
fn no_undo_after_set_plain_text() {
    let doc = new_doc("Hello");
    assert!(!doc.can_undo());
}

#[test]
fn clear_undo_redo_stack() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);
    c.insert_text(" world").unwrap();
    assert!(doc.can_undo());

    doc.clear_undo_redo();
    assert!(!doc.can_undo());
    assert!(!doc.can_redo());
}
