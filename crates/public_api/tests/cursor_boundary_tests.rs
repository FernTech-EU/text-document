use text_document::{ListStyle, MoveMode, MoveOperation, SelectionType, TextDocument, TextFormat};

fn new_doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

// ── Boundary queries ────────────────────────────────────────────

#[test]
fn at_start() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    assert!(c.at_start());
    c.move_position(MoveOperation::NextCharacter, MoveMode::MoveAnchor, 1);
    assert!(!c.at_start());
}

#[test]
fn at_end() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    assert!(!c.at_end());
    c.move_position(MoveOperation::End, MoveMode::MoveAnchor, 1);
    assert!(c.at_end());
}

#[test]
fn at_block_start() {
    let doc = new_doc("Hello\nWorld");
    let c = doc.cursor();
    assert!(c.at_block_start());
    c.move_position(MoveOperation::NextCharacter, MoveMode::MoveAnchor, 1);
    assert!(!c.at_block_start());
    // Move to start of second block
    c.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 1);
    assert!(c.at_block_start());
}

#[test]
fn at_block_end() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    assert!(!c.at_block_end());
    c.move_position(MoveOperation::EndOfBlock, MoveMode::MoveAnchor, 1);
    assert!(c.at_block_end());
}

#[test]
fn block_number() {
    let doc = new_doc("Line 1\nLine 2\nLine 3");
    let c = doc.cursor();
    assert_eq!(c.block_number(), 0);
    c.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 1);
    assert_eq!(c.block_number(), 1);
    c.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 1);
    assert_eq!(c.block_number(), 2);
}

#[test]
fn position_in_block() {
    let doc = new_doc("Hello\nWorld");
    let c = doc.cursor_at(2);
    assert_eq!(c.position_in_block(), 2);
    // Move to second block, position 1
    let c2 = doc.cursor_at(7); // "W" in "World"
    assert_eq!(c2.position_in_block(), 1);
}

// ── Selection types ─────────────────────────────────────────────

#[test]
fn select_word_under_cursor() {
    let doc = new_doc("Hello world");
    let c = doc.cursor_at(2); // inside "Hello"
    c.select(SelectionType::WordUnderCursor);
    assert!(c.has_selection());
    assert_eq!(c.selected_text().unwrap(), "Hello");
}

#[test]
fn select_block_under_cursor() {
    let doc = new_doc("First block\nSecond block");
    let c = doc.cursor_at(3);
    c.select(SelectionType::BlockUnderCursor);
    assert_eq!(c.selected_text().unwrap(), "First block");
}

#[test]
fn select_line_under_cursor() {
    let doc = new_doc("First line\nSecond line");
    let c = doc.cursor_at(3);
    c.select(SelectionType::LineUnderCursor);
    assert_eq!(c.selected_text().unwrap(), "First line");
}

#[test]
fn select_document() {
    let doc = new_doc("All text here");
    let c = doc.cursor_at(3);
    c.select(SelectionType::Document);
    assert_eq!(c.selected_text().unwrap(), "All text here");
}

// ── Movement operations ─────────────────────────────────────────

#[test]
fn move_start() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(3);
    c.move_position(MoveOperation::Start, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 0);
}

#[test]
fn move_end() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.move_position(MoveOperation::End, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 5);
}

#[test]
fn move_no_move() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(2);
    let moved = c.move_position(MoveOperation::NoMove, MoveMode::MoveAnchor, 1);
    assert!(!moved);
    assert_eq!(c.position(), 2);
}

#[test]
fn move_next_char() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.move_position(MoveOperation::NextCharacter, MoveMode::MoveAnchor, 3);
    assert_eq!(c.position(), 3);
}

#[test]
fn move_prev_char() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(3);
    c.move_position(MoveOperation::PreviousCharacter, MoveMode::MoveAnchor, 2);
    assert_eq!(c.position(), 1);
}

#[test]
fn move_left_right() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.move_position(MoveOperation::Right, MoveMode::MoveAnchor, 2);
    assert_eq!(c.position(), 2);
    c.move_position(MoveOperation::Left, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 1);
}

#[test]
fn move_start_of_block() {
    let doc = new_doc("Hello\nWorld");
    let c = doc.cursor_at(8); // "rl" in World
    c.move_position(MoveOperation::StartOfBlock, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 6); // start of "World"
}

#[test]
fn move_end_of_block() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.move_position(MoveOperation::EndOfBlock, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 5);
}

#[test]
fn move_start_end_of_line() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(3);
    c.move_position(MoveOperation::StartOfLine, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 0);
    c.move_position(MoveOperation::EndOfLine, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 5);
}

#[test]
fn move_next_block() {
    let doc = new_doc("First\nSecond\nThird");
    let c = doc.cursor();
    c.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 6);
    c.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 13);
}

#[test]
fn move_previous_block() {
    let doc = new_doc("First\nSecond\nThird");
    let c = doc.cursor_at(14); // inside "Third"
    c.move_position(MoveOperation::PreviousBlock, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 6); // start of "Second"
    c.move_position(MoveOperation::PreviousBlock, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 0); // start of "First"
}

#[test]
fn move_previous_block_from_start() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    let moved = c.move_position(MoveOperation::PreviousBlock, MoveMode::MoveAnchor, 1);
    assert!(!moved);
    assert_eq!(c.position(), 0);
}

#[test]
fn move_next_word() {
    let doc = new_doc("Hello world foo");
    let c = doc.cursor();
    c.move_position(MoveOperation::NextWord, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 5);
}

#[test]
fn move_previous_word() {
    let doc = new_doc("Hello world");
    let c = doc.cursor_at(8); // inside "world"
    c.move_position(MoveOperation::PreviousWord, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 6); // start of "world"
}

#[test]
fn move_start_of_word() {
    let doc = new_doc("Hello world");
    let c = doc.cursor_at(8);
    c.move_position(MoveOperation::StartOfWord, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 6);
}

#[test]
fn move_end_of_word() {
    let doc = new_doc("Hello world");
    let c = doc.cursor_at(1); // inside "Hello"
    c.move_position(MoveOperation::EndOfWord, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 5);
}

#[test]
fn move_word_left() {
    let doc = new_doc("Hello world");
    let c = doc.cursor_at(8);
    c.move_position(MoveOperation::WordLeft, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 6);
}

#[test]
fn move_word_right() {
    let doc = new_doc("Hello world");
    let c = doc.cursor();
    c.move_position(MoveOperation::WordRight, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 5);
}

#[test]
fn move_up() {
    let doc = new_doc("First\nSecond");
    let c = doc.cursor_at(8); // inside "Second"
    let moved = c.move_position(MoveOperation::Up, MoveMode::MoveAnchor, 1);
    // Up is treated as PreviousBlock
    assert!(moved);
    assert_eq!(c.position(), 0);
}

#[test]
fn move_down() {
    let doc = new_doc("First\nSecond");
    let c = doc.cursor_at(2);
    c.move_position(MoveOperation::Down, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 6);
}

// ── Keep anchor mode ────────────────────────────────────────────

#[test]
fn move_keep_anchor_creates_selection() {
    let doc = new_doc("Hello world");
    let c = doc.cursor();
    c.move_position(MoveOperation::NextCharacter, MoveMode::KeepAnchor, 5);
    assert!(c.has_selection());
    assert_eq!(c.anchor(), 0);
    assert_eq!(c.position(), 5);
    assert_eq!(c.selection_start(), 0);
    assert_eq!(c.selection_end(), 5);
    assert_eq!(c.selected_text().unwrap(), "Hello");
}

// ── clear_selection ─────────────────────────────────────────────

#[test]
fn clear_selection() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.move_position(MoveOperation::NextCharacter, MoveMode::KeepAnchor, 3);
    assert!(c.has_selection());
    c.clear_selection();
    assert!(!c.has_selection());
    assert_eq!(c.position(), c.anchor());
}

// ── set_position with clamp ─────────────────────────────────────

#[test]
fn set_position_clamps_to_end() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.set_position(1000, MoveMode::MoveAnchor);
    assert_eq!(c.position(), 5);
}

#[test]
fn set_position_keep_anchor() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.set_position(3, MoveMode::KeepAnchor);
    assert_eq!(c.position(), 3);
    assert_eq!(c.anchor(), 0);
    assert!(c.has_selection());
}

// ── Editing operations ──────────────────────────────────────────

#[test]
fn insert_block() {
    let doc = new_doc("Hello world");
    let c = doc.cursor_at(5);
    c.insert_block().unwrap();
    assert_eq!(doc.block_count(), 2);
}

#[test]
fn insert_html() {
    let doc = new_doc("Hello ");
    let c = doc.cursor_at(6);
    c.insert_html("<b>world</b>").unwrap();
    let text = doc.to_plain_text().unwrap();
    assert!(text.contains("world"));
}

#[test]
fn insert_markdown() {
    let doc = new_doc("Hello ");
    let c = doc.cursor_at(6);
    c.insert_markdown("**world**").unwrap();
    let text = doc.to_plain_text().unwrap();
    assert!(text.contains("world"));
}

#[test]
fn insert_image() {
    let doc = new_doc("Hello ");
    let c = doc.cursor_at(6);
    c.insert_image("test.png", "", 100, 100).unwrap();
    let stats = doc.stats();
    assert_eq!(stats.image_count, 1);
}

#[test]
fn insert_frame() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.insert_frame().unwrap();
    let stats = doc.stats();
    assert!(stats.frame_count >= 2); // root frame + new frame
}

#[test]
fn delete_char() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.delete_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "ello");
}

#[test]
fn delete_previous_char() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);
    c.delete_previous_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hell");
}

#[test]
fn delete_previous_char_at_start() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.delete_previous_char().unwrap(); // should be no-op
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");
}

#[test]
fn remove_selected_text() {
    let doc = new_doc("Hello world");
    let c = doc.cursor();
    c.move_position(MoveOperation::NextCharacter, MoveMode::KeepAnchor, 5);
    let removed = c.remove_selected_text().unwrap();
    assert_eq!(removed, "Hello");
    assert_eq!(doc.to_plain_text().unwrap(), " world");
}

#[test]
fn remove_selected_text_no_selection() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    let removed = c.remove_selected_text().unwrap();
    assert_eq!(removed, "");
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");
}

#[test]
fn delete_char_with_selection() {
    let doc = new_doc("Hello world");
    let c = doc.cursor();
    c.move_position(MoveOperation::NextCharacter, MoveMode::KeepAnchor, 5);
    c.delete_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), " world");
}

// ── Insert formatted text ───────────────────────────────────────

#[test]
fn insert_formatted_text() {
    let doc = new_doc("");
    let c = doc.cursor();
    let fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    c.insert_formatted_text("Bold text", &fmt).unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Bold text");
}

// ── List operations ─────────────────────────────────────────────

#[test]
fn create_list() {
    let doc = new_doc("Item 1\nItem 2\nItem 3");
    let c = doc.cursor();
    c.select(SelectionType::Document);
    c.create_list(ListStyle::Disc).unwrap();
    let stats = doc.stats();
    assert!(stats.list_count >= 1);
}

#[test]
fn insert_list() {
    let doc = new_doc("Before");
    let c = doc.cursor_at(6);
    c.insert_list(ListStyle::Decimal).unwrap();
    let stats = doc.stats();
    assert!(stats.list_count >= 1);
}

// ── Edit blocks (composite undo) ────────────────────────────────

#[test]
fn begin_end_edit_block() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);
    c.begin_edit_block();
    c.insert_text(" world").unwrap();
    c.insert_text("!").unwrap();
    c.end_edit_block();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello world!");

    // Single undo should revert both inserts
    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");
}

#[test]
fn join_previous_edit_block() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);
    c.join_previous_edit_block();
    c.insert_text("!").unwrap();
    c.end_edit_block();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello!");
}

// ── Cursor clone ────────────────────────────────────────────────

#[test]
fn cursor_clone_is_independent() {
    let doc = new_doc("Hello");
    let c1 = doc.cursor_at(2);
    let c2 = c1.clone();
    assert_eq!(c2.position(), 2);

    c1.move_position(MoveOperation::End, MoveMode::MoveAnchor, 1);
    assert_eq!(c1.position(), 5);
    assert_eq!(c2.position(), 2); // independent
}

// ── Format queries from cursor ──────────────────────────────────

#[test]
fn cursor_char_format() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    let fmt = c.char_format().unwrap();
    // Default format has no explicit bold
    assert_ne!(fmt.font_bold, Some(true));
}

#[test]
fn cursor_block_format() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    let fmt = c.block_format().unwrap();
    assert_eq!(fmt.alignment, None);
}

// ── Word boundary edge cases ────────────────────────────────────

#[test]
fn next_word_at_end_of_document() {
    let doc = new_doc("Hello");
    let c = doc.cursor_at(5);
    let moved = c.move_position(MoveOperation::NextWord, MoveMode::MoveAnchor, 1);
    assert!(!moved);
    assert_eq!(c.position(), 5);
}

#[test]
fn previous_word_at_start_of_document() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    let moved = c.move_position(MoveOperation::PreviousWord, MoveMode::MoveAnchor, 1);
    assert!(!moved);
}

#[test]
fn word_boundary_at_word_start() {
    let doc = new_doc("Hello world");
    let c = doc.cursor_at(6); // start of "world"
    c.move_position(MoveOperation::PreviousWord, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 0); // jumps to previous word start
}

// ── Empty document edge cases ───────────────────────────────────

#[test]
fn cursor_on_empty_document() {
    let doc = TextDocument::new();
    let c = doc.cursor();
    assert!(c.at_start());
    assert!(c.at_end());
    assert_eq!(c.position(), 0);
    assert_eq!(c.block_number(), 0);
    assert_eq!(c.position_in_block(), 0);
}
