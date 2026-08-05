//! Regression coverage: logical character offsets around an inline image.
//!
//! An image is represented twice — as an `ImageAnchor` in the block's anchor
//! list, and as a `U+FFFC` OBJECT REPLACEMENT CHARACTER mirrored into the rope
//! at that anchor's `byte_offset`. Two independent places used to count *both*,
//! so every image advanced a logical offset by two instead of one:
//!
//! * `logical_offset_to_byte` walked the anchor list on top of the text's own
//!   `char_indices()`, which every editing use case relies on to map a cursor
//!   position to a byte position. Deleting a range containing an image removed
//!   too little.
//! * the runs/images merge emitted the image piece *and* left the sentinel in
//!   the following text piece, so reading a selection back produced a doubled
//!   sentinel and dropped the character after the image.
//!
//! These tests work in the public coordinate space (`character_count`, cursor
//! positions, `selected_text`) rather than in byte offsets, because that is the
//! space the bugs were visible in.

use text_document::{MoveMode, ResourceType, TextDocument};

/// The document's text *including* image sentinels.
///
/// `to_plain_text` is the `.txt` **export**, and a `.txt` cannot contain a
/// picture — it omits images deliberately. These tests are about the document's
/// coordinate space, where an image really does occupy one character, so they
/// read the text back through a full-document selection instead.
fn text_with_images(doc: &TextDocument) -> String {
    let count = doc.character_count();
    if count == 0 {
        return String::new();
    }
    let cursor = doc.cursor_at(0);
    cursor.set_position(count, MoveMode::KeepAnchor);
    cursor.selected_text().expect("selection")
}

/// "abc<image>def" — one image at logical position 3.
fn doc_with_image() -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_sync("abcdef\n").expect("import");
    doc.add_resource(ResourceType::Image, "p.png", "image/png", b"fake")
        .expect("resource");
    doc.cursor_at(3)
        .insert_image("p.png", "a picture", 9, 9)
        .expect("insert");
    doc
}

#[test]
fn an_image_counts_as_exactly_one_character() {
    let doc = doc_with_image();
    assert_eq!(
        doc.character_count(),
        7,
        "six letters plus one image; counting the image twice gives 8"
    );
    assert_eq!(text_with_images(&doc), "abc\u{FFFC}def");
}

#[test]
fn selecting_across_an_image_reads_back_the_right_characters() {
    let doc = doc_with_image();
    let cursor = doc.cursor_at(2);
    cursor.set_position(5, MoveMode::KeepAnchor);
    assert_eq!(
        cursor.selected_text().unwrap(),
        "c\u{FFFC}d",
        "the selection must be 'c', the image, 'd' — not a doubled sentinel"
    );
}

#[test]
fn every_selection_length_reads_back_that_many_characters() {
    // Sweep the whole document: a selection of n characters must return n.
    let doc = doc_with_image();
    let count = doc.character_count();
    for start in 0..count {
        for end in (start + 1)..=count {
            let cursor = doc.cursor_at(start);
            cursor.set_position(end, MoveMode::KeepAnchor);
            let selected = cursor.selected_text().unwrap();
            assert_eq!(
                selected.chars().count(),
                end - start,
                "selection {start}..{end} returned {selected:?}"
            );
        }
    }
}

#[test]
fn deleting_across_an_image_removes_exactly_the_selected_range() {
    let doc = doc_with_image();
    let cursor = doc.cursor_at(2);
    cursor.set_position(5, MoveMode::KeepAnchor);
    cursor.remove_selected_text().expect("delete");
    assert_eq!(
        text_with_images(&doc),
        "abef",
        "'c', the image and 'd' must all go — leaving the 'd' means the range \
         was measured one character short"
    );
    assert_eq!(doc.character_count(), 4);
}

#[test]
fn deleting_across_an_image_is_undoable() {
    let doc = doc_with_image();
    let before = text_with_images(&doc);
    let cursor = doc.cursor_at(2);
    cursor.set_position(5, MoveMode::KeepAnchor);
    cursor.remove_selected_text().expect("delete");
    doc.undo().expect("undo");
    assert_eq!(text_with_images(&doc), before);
    assert_eq!(doc.character_count(), 7);
}

#[test]
fn text_inserted_after_an_image_lands_after_it() {
    let doc = doc_with_image();
    // Position 4 is immediately past the image.
    doc.cursor_at(4).insert_text("X").expect("insert");
    assert_eq!(text_with_images(&doc), "abc\u{FFFC}Xdef");
}

#[test]
fn text_inserted_before_an_image_lands_before_it() {
    let doc = doc_with_image();
    doc.cursor_at(3).insert_text("X").expect("insert");
    assert_eq!(text_with_images(&doc), "abcX\u{FFFC}def");
}

#[test]
fn two_images_in_one_block_each_count_once() {
    let doc = TextDocument::new();
    doc.set_djot_sync("abcdef\n").expect("import");
    doc.add_resource(ResourceType::Image, "p.png", "image/png", b"fake")
        .expect("resource");
    doc.cursor_at(5).insert_image("p.png", "", 9, 9).expect("b");
    doc.cursor_at(2).insert_image("p.png", "", 9, 9).expect("a");

    assert_eq!(doc.character_count(), 8, "six letters plus two images");
    assert_eq!(text_with_images(&doc), "ab\u{FFFC}cde\u{FFFC}f");
}

#[test]
fn an_image_inserted_after_another_anchors_in_the_right_place() {
    // Byte offsets used to be derived by summing only the text segments,
    // treating each image as zero bytes — so every preceding image put the new
    // anchor three bytes short of where it belonged.
    let doc = TextDocument::new();
    doc.set_djot_sync("abcdef\n").expect("import");
    doc.add_resource(ResourceType::Image, "p.png", "image/png", b"fake")
        .expect("resource");
    doc.cursor_at(2)
        .insert_image("p.png", "", 9, 9)
        .expect("first");
    // Position 5 is two characters past the first image.
    doc.cursor_at(5)
        .insert_image("p.png", "", 9, 9)
        .expect("second");

    assert_eq!(doc.character_count(), 8);
    assert_eq!(text_with_images(&doc), "ab\u{FFFC}cd\u{FFFC}ef");
}

#[test]
fn inserting_an_image_does_not_shift_existing_formatting() {
    // `rope_insert_in_block` moves block offsets but not the block's own format
    // runs, so the sentinel's bytes have to be applied to them explicitly.
    let doc = TextDocument::new();
    doc.set_djot_sync("plain _italic here_\n").expect("import");
    doc.add_resource(ResourceType::Image, "p.png", "image/png", b"fake")
        .expect("resource");
    // Insert at the very start, before the emphasised run.
    doc.cursor_at(0)
        .insert_image("p.png", "", 9, 9)
        .expect("insert");

    let djot = doc.to_djot().expect("export");
    assert!(
        djot.contains("_italic here_"),
        "the emphasis moved when the image was inserted before it: {djot:?}"
    );
}
