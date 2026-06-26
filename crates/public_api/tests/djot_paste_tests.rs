//! Public-API paste tests for `TextCursor::insert_djot`.

use text_document::TextDocument;

#[test]
fn cursor_insert_djot_inline() {
    let doc = TextDocument::new();
    doc.set_plain_text("Hello World").unwrap();
    doc.cursor_at(5).insert_djot(" *bold*").unwrap();
    let text = doc.to_plain_text().unwrap();
    assert!(text.contains("Hello"), "{text}");
    assert!(text.contains("bold"), "{text}");
    assert!(text.contains("World"), "{text}");
    // The inserted run is bold in the exported djot.
    assert!(doc.to_djot().unwrap().contains("*bold*"));
}

#[test]
fn cursor_insert_djot_blocks() {
    let doc = TextDocument::new();
    doc.set_plain_text("intro").unwrap();
    doc.cursor_at(5)
        .insert_djot("\n\n# Heading\n\nbody")
        .unwrap();
    let dj = doc.to_djot().unwrap();
    assert!(dj.contains("# Heading"), "{dj}");
    assert!(dj.contains("body"), "{dj}");
}

#[test]
fn cursor_insert_djot_is_undoable() {
    let doc = TextDocument::new();
    doc.set_plain_text("base").unwrap();
    doc.cursor_at(4).insert_djot("\n\n*extra*").unwrap();
    assert!(doc.to_plain_text().unwrap().contains("extra"));
    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "base");
}
