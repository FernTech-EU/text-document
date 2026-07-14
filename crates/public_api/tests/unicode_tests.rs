use text_document::{MoveMode, MoveOperation, TextDocument};

fn new_doc_with_text(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

#[test]
fn unicode_character_count() {
    // "café" is 4 unicode chars (é is 1 char, 2 bytes)
    let doc = new_doc_with_text("café");
    assert_eq!(doc.character_count(), 4);
}

#[test]
fn unicode_cjk_character_count() {
    // 3 CJK characters
    let doc = new_doc_with_text("日本語");
    assert_eq!(doc.character_count(), 3);
}

#[test]
fn unicode_emoji_character_count() {
    // "Hi 🌍" is 4 chars: H, i, space, 🌍
    let doc = new_doc_with_text("Hi 🌍");
    assert_eq!(doc.character_count(), 4);
}

#[test]
fn unicode_text_at_position() {
    let doc = new_doc_with_text("café");
    let text = doc.text_at(0, 4).unwrap();
    assert_eq!(text, "café");
    // Get just "caf"
    let text = doc.text_at(0, 3).unwrap();
    assert_eq!(text, "caf");
}

#[test]
fn unicode_insert_text() {
    let doc = new_doc_with_text("世界");
    let cursor = doc.cursor();
    cursor.insert_text("你好").unwrap();
    let text = doc.to_plain_text().unwrap();
    assert_eq!(text, "你好世界");
    // Cursor at position 2 (after "你好", 2 chars)
    assert_eq!(cursor.position(), 2);
}

#[test]
fn unicode_delete_char() {
    let doc = new_doc_with_text("café");
    let cursor = doc.cursor();
    // Delete 'c'
    cursor.delete_char().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "afé");
}

#[test]
fn unicode_move_next_char() {
    let doc = new_doc_with_text("café");
    let cursor = doc.cursor();
    // Move right 3 times: c, a, f
    cursor.move_position(MoveOperation::NextCharacter, MoveMode::MoveAnchor, 3);
    assert_eq!(cursor.position(), 3);
    // Now at position 3, which is the 'é' character
    cursor.move_position(MoveOperation::NextCharacter, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), 4); // past 'é', at end
}

#[test]
fn unicode_selection() {
    let doc = new_doc_with_text("Héllo");
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(2, MoveMode::KeepAnchor);
    let selected = cursor.selected_text().unwrap();
    assert_eq!(selected, "Hé");
}

#[test]
fn unicode_find() {
    let doc = new_doc_with_text("café au lait");
    let opts = text_document::FindOptions::default();
    let result = doc.find("café", 0, &opts).unwrap();
    assert!(result.is_some());
    let m = result.unwrap();
    assert_eq!(m.position, 0);
    assert_eq!(m.length, 4); // 4 unicode chars
}

#[test]
fn unicode_replace() {
    let doc = new_doc_with_text("café");
    let opts = text_document::FindOptions::default();
    let count = doc
        .replace_text(
            "café",
            "coffee",
            true,
            &text_document::ReplaceOptions::new(opts.clone()),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(doc.to_plain_text().unwrap(), "coffee");
}

#[test]
fn unicode_multi_cursor_adjustment() {
    let doc = new_doc_with_text("日本語テスト");
    let c1 = doc.cursor_at(0);
    let c2 = doc.cursor_at(3); // at 'テ'

    // Insert 2 CJK chars at position 0
    c1.insert_text("漢字").unwrap();
    assert_eq!(c1.position(), 2);
    assert_eq!(c2.position(), 5); // shifted by 2
}
