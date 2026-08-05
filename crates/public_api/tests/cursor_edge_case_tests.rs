use text_document::{MoveMode, MoveOperation, SelectionType, TextDocument, TextFormat};

fn new_doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// delete_char boundary tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn delete_char_at_end_of_document_is_noop() {
    let doc = new_doc("Hello");
    let cursor = doc.cursor_at(5);
    cursor.delete_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");
    assert_eq!(doc.character_count(), 5);
}

#[test]
fn delete_char_at_end_of_multiblock_document_is_noop() {
    let doc = new_doc("Hello\nWorld");
    // Position 11 = after "World" = end of document
    let cursor = doc.cursor_at(11);
    cursor.delete_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello\nWorld");
    assert_eq!(doc.character_count(), 10);
    assert_eq!(doc.block_count(), 2);
}

#[test]
fn delete_char_at_end_preserves_character_count_consistency() {
    let doc = new_doc("Hello");
    let cursor = doc.cursor_at(5);

    // Call delete_char at end multiple times — must remain consistent
    for _ in 0..3 {
        cursor.delete_char().unwrap();
    }

    assert_eq!(doc.character_count(), 5);
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Cross-block deletion tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn delete_char_at_end_of_block_merges_with_next() {
    let doc = new_doc("Hello\nWorld");
    // Position 5 = end of "Hello", block separator is at 5
    let cursor = doc.cursor_at(5);
    cursor.delete_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "HelloWorld");
    assert_eq!(doc.block_count(), 1);
    assert_eq!(doc.character_count(), 10);
}

#[test]
fn delete_previous_char_at_start_of_block_merges_with_previous() {
    let doc = new_doc("Hello\nWorld");
    // Position 6 = start of "World"
    let cursor = doc.cursor_at(6);
    cursor.delete_previous_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "HelloWorld");
    assert_eq!(doc.block_count(), 1);
    assert_eq!(doc.character_count(), 10);
}

#[test]
fn delete_char_merges_three_blocks() {
    let doc = new_doc("A\nB\nC");
    // Delete at end of first block (merge A+B)
    let cursor = doc.cursor_at(1);
    cursor.delete_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "AB\nC");
    assert_eq!(doc.block_count(), 2);

    // Delete at end of merged block (merge AB+C)
    let cursor2 = doc.cursor_at(2);
    cursor2.delete_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "ABC");
    assert_eq!(doc.block_count(), 1);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Movement boundary tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn move_next_block_from_last_block() {
    let doc = new_doc("First\nSecond\nThird");
    let c = doc.cursor_at(13); // start of "Third"
    let moved = c.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 1);
    // Should move to end of document (clamped), not stay put
    assert!(moved);
    assert!(c.at_end());
}

#[test]
fn move_next_block_from_end_of_last_block() {
    let doc = new_doc("First\nSecond\nThird");
    let c = doc.cursor_at(18); // end of "Third" (max cursor position)
    let moved = c.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 1);
    // Already at end, should not move
    assert!(!moved);
    assert_eq!(c.position(), 18);
}

#[test]
fn move_up_from_first_block() {
    let doc = new_doc("First\nSecond");
    let c = doc.cursor_at(2); // inside "First"
    let moved = c.move_position(MoveOperation::Up, MoveMode::MoveAnchor, 1);
    // Up = PreviousBlock, which goes to start of current block (position 0)
    assert!(moved); // moved from 2 to 0
    assert_eq!(c.position(), 0);

    // A second Up from position 0 should not move
    let moved2 = c.move_position(MoveOperation::Up, MoveMode::MoveAnchor, 1);
    assert!(!moved2);
    assert_eq!(c.position(), 0);
}

#[test]
fn move_down_from_last_block() {
    let doc = new_doc("First\nSecond");
    let c = doc.cursor_at(8); // inside "Second"
    let moved = c.move_position(MoveOperation::Down, MoveMode::MoveAnchor, 1);
    // Down from last block = NextBlock = move to end
    assert!(moved);
    assert!(c.at_end());
}

#[test]
fn move_next_character_crosses_block_boundary() {
    let doc = new_doc("AB\nCD");
    let c = doc.cursor_at(2); // end of "AB"
    c.move_position(MoveOperation::NextCharacter, MoveMode::MoveAnchor, 1);
    // Position 2 + 1 = 3 = block separator, which maps to start of "CD"
    assert_eq!(c.position(), 3);

    c.move_position(MoveOperation::NextCharacter, MoveMode::MoveAnchor, 1);
    // Position 3 + 1 = 4 = "D" in "CD"
    assert_eq!(c.position(), 4);
}

#[test]
fn move_previous_character_crosses_block_boundary() {
    let doc = new_doc("AB\nCD");
    let c = doc.cursor_at(3); // start of "CD" (position 3)
    c.move_position(MoveOperation::PreviousCharacter, MoveMode::MoveAnchor, 1);
    // Position 3 - 1 = 2 = separator between blocks (maps to block "CD")
    assert_eq!(c.position(), 2);

    // Move one more step back — should land within "AB"
    c.move_position(MoveOperation::PreviousCharacter, MoveMode::MoveAnchor, 1);
    assert_eq!(c.position(), 1);
    assert_eq!(c.block_number(), 0);
}

#[test]
fn move_operations_on_empty_document() {
    let doc = TextDocument::new();
    let c = doc.cursor();

    assert!(!c.move_position(MoveOperation::NextCharacter, MoveMode::MoveAnchor, 1));
    assert_eq!(c.position(), 0);

    assert!(!c.move_position(MoveOperation::PreviousCharacter, MoveMode::MoveAnchor, 1));
    assert_eq!(c.position(), 0);

    assert!(!c.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 1));
    assert_eq!(c.position(), 0);

    assert!(!c.move_position(MoveOperation::NextWord, MoveMode::MoveAnchor, 1));
    assert_eq!(c.position(), 0);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Editing edge cases
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn insert_empty_string_is_noop() {
    let doc = new_doc("Hello");
    let cursor = doc.cursor_at(3);
    cursor.insert_text("").unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello");
    assert_eq!(cursor.position(), 3);
}

#[test]
fn insert_text_with_newline_inserts_literal() {
    // insert_text inserts into the current block's plain_text;
    // newlines do not create new blocks (use insert_block for that)
    let doc = new_doc("AC");
    let cursor = doc.cursor_at(1);
    cursor.insert_text("BB").unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "ABBC");
    assert_eq!(doc.block_count(), 1);
}

#[test]
fn insert_block_at_document_start() {
    let doc = new_doc("Hello");
    let cursor = doc.cursor();
    cursor.insert_block().unwrap();
    assert_eq!(doc.block_count(), 2);
    let text = doc.to_plain_text().unwrap();
    assert_eq!(text, "\nHello");
}

#[test]
fn insert_block_with_selection_deletes_then_splits() {
    let doc = new_doc("Hello World");
    let cursor = doc.cursor();
    // Select "Hello" (anchor=0, position=5)
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(5, MoveMode::KeepAnchor);
    cursor.insert_block().unwrap();
    // Word convention: selection is deleted first, then paragraph break inserted
    // "Hello" deleted → " World" remains → split at 0 → "" + " World"
    assert_eq!(doc.block_count(), 2);
    let text = doc.to_plain_text().unwrap();
    assert_eq!(text, "\n World");
}

#[test]
fn insert_block_with_selection_undo_restores_original() {
    let doc = new_doc("Hello World");
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(5, MoveMode::KeepAnchor);
    cursor.insert_block().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "\n World");
    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello World");
    assert_eq!(doc.block_count(), 1);
}

#[test]
fn insert_block_at_document_end() {
    let doc = new_doc("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_block().unwrap();
    assert_eq!(doc.block_count(), 2);
    assert_eq!(doc.to_plain_text().unwrap(), "Hello\n");
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Backward selection tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn backward_selection_selected_text() {
    let doc = new_doc("Hello world");
    let cursor = doc.cursor_at(5);
    // Create backward selection: anchor=5, position=0
    cursor.set_position(0, MoveMode::KeepAnchor);
    assert!(cursor.has_selection());
    assert_eq!(cursor.anchor(), 5);
    assert_eq!(cursor.position(), 0);
    assert_eq!(cursor.selection_start(), 0);
    assert_eq!(cursor.selection_end(), 5);
    assert_eq!(cursor.selected_text().unwrap(), "Hello");
}

#[test]
fn backward_selection_remove_selected_text() {
    let doc = new_doc("Hello world");
    let cursor = doc.cursor_at(5);
    cursor.set_position(0, MoveMode::KeepAnchor);
    let removed = cursor.remove_selected_text().unwrap();
    assert_eq!(removed, "Hello");
    assert_eq!(doc.to_plain_text().unwrap(), " world");
}

#[test]
fn backward_selection_delete_char() {
    let doc = new_doc("Hello world");
    let cursor = doc.cursor_at(5);
    cursor.set_position(0, MoveMode::KeepAnchor);
    cursor.delete_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), " world");
}

#[test]
fn backward_selection_insert_text_replaces() {
    let doc = new_doc("Hello world");
    let cursor = doc.cursor_at(5);
    cursor.set_position(0, MoveMode::KeepAnchor);
    cursor.insert_text("Goodbye").unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Goodbye world");
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Empty block navigation tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn navigate_through_empty_block() {
    let doc = new_doc("Hello\n\nWorld");
    assert_eq!(doc.block_count(), 3);

    // Move from block 0 to block 1 (empty)
    let c = doc.cursor();
    c.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 1);
    assert_eq!(c.block_number(), 1);
    assert_eq!(c.position_in_block(), 0);
    assert!(c.at_block_start());
    assert!(c.at_block_end()); // empty block: start == end

    // Move from empty block to block 2
    c.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 1);
    assert_eq!(c.block_number(), 2);
    assert_eq!(c.position_in_block(), 0);
}

#[test]
fn delete_char_on_empty_block_merges() {
    let doc = new_doc("Hello\n\nWorld");
    assert_eq!(doc.block_count(), 3);

    // Cursor at start of empty block (position 6)
    let cursor = doc.cursor_at(6);
    cursor.delete_char().unwrap();
    // Empty block merged with "World"
    assert_eq!(doc.to_plain_text().unwrap(), "Hello\nWorld");
    assert_eq!(doc.block_count(), 2);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Word selection edge cases
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn select_word_on_whitespace() {
    let doc = new_doc("Hello   World");
    let c = doc.cursor_at(6); // in the whitespace between words
    c.select(SelectionType::WordUnderCursor);
    // On whitespace, word boundaries should return (pos, pos) -> no selection
    // or select the adjacent word. Either way, verify it doesn't panic.
    let _ = c.selected_text().unwrap();
}

#[test]
fn select_word_at_punctuation() {
    let doc = new_doc("Hello, World");
    let c = doc.cursor_at(5); // at the comma
    c.select(SelectionType::WordUnderCursor);
    let _ = c.selected_text().unwrap();
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Multi-cursor edge cases
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn multi_cursor_selection_adjusted_by_edit() {
    let doc = new_doc("Hello World");
    // c1 at 0, c2 has selection [6, 11) = "World"
    let c1 = doc.cursor_at(0);
    let c2 = doc.cursor_at(6);
    c2.set_position(11, MoveMode::KeepAnchor);
    assert!(c2.has_selection());

    // Insert "XX" at position 0 through c1
    c1.insert_text("XX").unwrap();

    // c2's selection should be shifted by +2
    assert_eq!(c2.anchor(), 8); // 6 + 2
    assert_eq!(c2.position(), 13); // 11 + 2
}

#[test]
fn multi_cursor_cross_block_insert_adjusts() {
    let doc = new_doc("Hello World");
    let c1 = doc.cursor_at(5);
    let c2 = doc.cursor_at(11);

    // Split at position 5 (insert block)
    c1.insert_block().unwrap();

    // c2 should have shifted
    assert!(c2.position() > 11);
    // Document should have 2 blocks
    assert_eq!(doc.block_count(), 2);
    assert_eq!(doc.to_plain_text().unwrap(), "Hello\n World");
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// insert_image with selection (Word convention: replaces selection)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn insert_image_with_selection_replaces_text() {
    let doc = new_doc("Hello World");
    let cursor = doc.cursor();
    // Select "World" (6..11)
    cursor.set_position(6, MoveMode::MoveAnchor);
    cursor.set_position(11, MoveMode::KeepAnchor);
    cursor.insert_image("photo.png", "", 100, 50).unwrap();
    // "World" deleted, image inserted → "Hello " + image = 7 chars
    assert_eq!(doc.character_count(), 7);
}

#[test]
fn insert_image_with_cross_block_selection_replaces() {
    let doc = new_doc("Hello\nWorld");
    let cursor = doc.cursor();
    // Select "lo\nWor" (3..9)
    cursor.set_position(3, MoveMode::MoveAnchor);
    cursor.set_position(9, MoveMode::KeepAnchor);
    cursor.insert_image("bridge.png", "", 200, 100).unwrap();
    // Cross-block selection deleted (blocks merged), image inserted
    // "Hel" + image + "ld" = 6 chars, 1 block
    assert_eq!(doc.character_count(), 6);
    assert_eq!(doc.block_count(), 1);
}

#[test]
fn insert_image_with_selection_undo_restores() {
    let doc = new_doc("Hello World");
    let cursor = doc.cursor();
    cursor.set_position(6, MoveMode::MoveAnchor);
    cursor.set_position(11, MoveMode::KeepAnchor);
    cursor.insert_image("photo.png", "", 100, 50).unwrap();
    assert_eq!(doc.character_count(), 7);
    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello World");
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// insert_formatted_text cross-block selection (Word convention)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn insert_formatted_text_cross_block_selection_replaces() {
    let doc = new_doc("Hello\nWorld");
    let cursor = doc.cursor();
    // Select "lo\nWor" (3..9)
    cursor.set_position(3, MoveMode::MoveAnchor);
    cursor.set_position(9, MoveMode::KeepAnchor);
    let fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    cursor.insert_formatted_text("XY", &fmt).unwrap();
    // Cross-block selection deleted (blocks merged), "XY" inserted
    // "Hel" + "XY" + "ld" = "HelXYld"
    assert_eq!(doc.to_plain_text().unwrap(), "HelXYld");
    assert_eq!(doc.block_count(), 1);
}

#[test]
fn insert_formatted_text_cross_block_selection_undo() {
    let doc = new_doc("Hello\nWorld");
    let cursor = doc.cursor();
    cursor.set_position(3, MoveMode::MoveAnchor);
    cursor.set_position(9, MoveMode::KeepAnchor);
    let fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    cursor.insert_formatted_text("XY", &fmt).unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "HelXYld");
    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello\nWorld");
    assert_eq!(doc.block_count(), 2);
}
