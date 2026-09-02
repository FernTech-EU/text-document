use text_document::{MoveMode, TextDocument};

fn new_doc() -> TextDocument {
    TextDocument::new()
}

fn new_doc_with_text(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

#[test]
fn new_document_is_empty() {
    let doc = new_doc();
    assert!(doc.is_empty());
    assert_eq!(doc.character_count(), 0);
}

#[test]
fn set_and_get_plain_text() {
    let doc = new_doc();
    doc.set_plain_text("Hello world").unwrap();
    let text = doc.to_plain_text().unwrap();
    assert_eq!(text, "Hello world");
}

#[test]
fn set_plain_text_clears_previous() {
    let doc = new_doc_with_text("First");
    doc.set_plain_text("Second").unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Second");
}

#[test]
fn character_count() {
    let doc = new_doc_with_text("Hello");
    assert_eq!(doc.character_count(), 5);
}

#[test]
fn is_empty_after_clear() {
    let doc = new_doc_with_text("Hello");
    doc.clear().unwrap();
    assert!(doc.is_empty());
}

/// The cheap counts and the expensive ones must agree, on a document with
/// several blocks and an edit behind it.
///
/// `character_count` and `block_count` read the two counts off the `Document`
/// entity; `stats()` recomputes the same two fields inside `get_document_stats`,
/// on the far side of a transaction and a walk over every block. They are the
/// same numbers by construction today, and this is what says so if either side
/// is ever rewritten: a drift here is a caret clamping to the wrong end of the
/// document and a host laying out a scene at the wrong height, neither of which
/// looks like a counting bug from the outside.
#[test]
fn the_cheap_counts_agree_with_the_walked_ones() {
    let doc = new_doc_with_text("One\nTwo\nThree");
    let cursor = doc.cursor();
    cursor.set_position(3, MoveMode::MoveAnchor);
    cursor.insert_text(" more").unwrap();

    let stats = doc.stats();
    assert_eq!(doc.character_count(), stats.character_count);
    assert_eq!(doc.block_count(), stats.block_count);
    assert_eq!(doc.block_count(), 3);
}

#[test]
fn stats_returns_correct_values() {
    let doc = new_doc_with_text("Hello world");
    let stats = doc.stats();
    assert_eq!(stats.character_count, 11);
    assert_eq!(stats.word_count, 2);
    assert_eq!(stats.block_count, 1);
}

#[test]
fn text_at_position() {
    let doc = new_doc_with_text("Hello world");
    let text = doc.text_at(0, 5).unwrap();
    assert_eq!(text, "Hello");
    let text = doc.text_at(6, 5).unwrap();
    assert_eq!(text, "world");
}

#[test]
fn block_at_position() {
    let doc = new_doc_with_text("Hello");
    let info = doc.block_at(0).unwrap();
    assert_eq!(info.block_number, 0);
    assert_eq!(info.start, 0);
    assert_eq!(info.length, 5);
}

#[test]
fn document_title() {
    let doc = new_doc();
    assert_eq!(doc.title(), "");
    doc.set_title("My Doc").unwrap();
    assert_eq!(doc.title(), "My Doc");
}

#[test]
fn document_clone_shares_state() {
    let doc = new_doc_with_text("Hello");
    let doc2 = doc.clone();
    assert_eq!(doc2.to_plain_text().unwrap(), "Hello");
    doc.set_plain_text("Changed").unwrap();
    assert_eq!(doc2.to_plain_text().unwrap(), "Changed");
}
