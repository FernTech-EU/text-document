//! Regression coverage: inserting an image inside a nested frame.
//!
//! `insert_image` used to read blocks from only the document's first top-level
//! frame, unlike `insert_text` / `insert_block` / `delete_text`, which all walk
//! sub-frames recursively. Blocks inside a blockquote or a table cell live in
//! sub-frames, so the target block could not be matched and the insertion fell
//! through to a scan over top-level blocks — anchoring the image to the wrong
//! block, or appending it to the last one.

use text_document::TextDocument;

/// Position of the `n`-th character of `needle` within the document's plain
/// text, as a character offset (what the cursor API speaks).
fn char_pos_of(doc: &TextDocument, needle: &str) -> usize {
    let text = doc.to_plain_text().expect("plain text");
    let byte = text.find(needle).unwrap_or_else(|| {
        panic!("{needle:?} not found in {text:?}");
    });
    text[..byte].chars().count()
}

#[test]
fn an_image_inserted_inside_a_blockquote_stays_in_the_blockquote() {
    let doc = TextDocument::new();
    doc.set_djot_sync("Before the quote.\n\n> quoted words here\n\nAfter the quote.\n")
        .expect("import");

    // Land the caret between "quoted" and " words", inside the quoted block.
    let pos = char_pos_of(&doc, "quoted") + "quoted".len();
    doc.add_resource(
        text_document::ResourceType::Image,
        "pic.png",
        "image/png",
        b"fake",
    )
    .expect("resource");
    doc.cursor_at(pos)
        .insert_image("pic.png", "a picture", 32, 32)
        .expect("insert");

    let djot = doc.to_djot().expect("export");
    let quoted_line = djot
        .lines()
        .find(|l| l.contains("quoted"))
        .unwrap_or_else(|| panic!("no quoted line in {djot:?}"));

    assert!(
        quoted_line.starts_with('>'),
        "the image must stay on the blockquote line, got {djot:?}"
    );
    assert!(
        quoted_line.contains("pic.png"),
        "the image landed outside the quoted block: {djot:?}"
    );
}

/// Differential: at the same document position, an image must anchor exactly
/// where text would be inserted.
///
/// This is the strongest available invariant and the one that matters —
/// `insert_text` has always walked sub-frames correctly, so agreeing with it
/// proves the recursive collection is right without this test having to model
/// the document's position arithmetic itself. (A plain-text character offset is
/// *not* a document position once tables are involved: the table's own
/// structure consumes positions, which is exactly the kind of detail a test
/// should not be re-deriving.)
fn assert_image_lands_where_text_does(source: &str, position: usize) {
    let with_text = TextDocument::new();
    with_text.set_djot_sync(source).expect("import");
    with_text
        .cursor_at(position)
        .insert_text("\u{FFFC}")
        .expect("insert text");

    let with_image = TextDocument::new();
    with_image.set_djot_sync(source).expect("import");
    with_image
        .add_resource(
            text_document::ResourceType::Image,
            "pic.png",
            "image/png",
            b"fake",
        )
        .expect("resource");
    with_image
        .cursor_at(position)
        .insert_image("pic.png", "", 16, 16)
        .expect("insert image");

    assert_eq!(
        with_image.to_plain_text().expect("plain text"),
        with_text.to_plain_text().expect("plain text"),
        "image and text disagree about position {position} in {source:?}"
    );
}

#[test]
fn an_image_inserted_inside_a_table_cell_stays_in_the_cell() {
    let source = "| alpha | beta |\n|---|---|\n| gamma | delta |\n";
    // Sweep every position in the table rather than picking one, so a
    // regression anywhere in the cell walk is caught.
    let doc = TextDocument::new();
    doc.set_djot_sync(source).expect("import");
    let count = doc.character_count();
    drop(doc);

    for position in 0..=count {
        assert_image_lands_where_text_does(source, position);
    }
}

#[test]
fn an_image_inserted_anywhere_in_a_blockquote_agrees_with_text_insertion() {
    let source = "Before.\n\n> quoted words here\n\nAfter.\n";
    let doc = TextDocument::new();
    doc.set_djot_sync(source).expect("import");
    let count = doc.character_count();
    drop(doc);

    for position in 0..=count {
        assert_image_lands_where_text_does(source, position);
    }
}

#[test]
fn an_image_in_a_plain_paragraph_still_works() {
    // The unnested path must not regress while fixing the nested one.
    let doc = TextDocument::new();
    doc.set_djot_sync("just a paragraph\n").expect("import");

    let pos = char_pos_of(&doc, "just") + "just".len();
    doc.add_resource(
        text_document::ResourceType::Image,
        "flat.png",
        "image/png",
        b"fake",
    )
    .expect("resource");
    doc.cursor_at(pos)
        .insert_image("flat.png", "", 8, 8)
        .expect("insert");

    // Read through a selection, not `to_plain_text`: the latter is the `.txt`
    // export and omits images by design.
    let count = doc.character_count();
    let cursor = doc.cursor_at(0);
    cursor.set_position(count, text_document::MoveMode::KeepAnchor);
    let text = cursor.selected_text().expect("selection");
    assert_eq!(text.find('\u{FFFC}'), Some("just".len()));
}
