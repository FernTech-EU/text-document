// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! `TextCursor::insert_text_transient_merged`: transient text in the format it
//! inherits, with the fields a partial format sets merged over it. It is a
//! transient edit like the others: a revert gives the recorded document back,
//! formats included; a commit records the text it is given; a recorded write
//! ends it; and what the gate refuses leaves the document as it was.

use text_document::{DocumentError, MoveMode, TextDocument, TextFormat, TransientRefusal};

fn bold() -> TextFormat {
    TextFormat {
        font_bold: Some(true),
        ..Default::default()
    }
}

/// "ab cd", with "ab" italic and in a family of its own.
fn doc() -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text("ab cd").unwrap();
    let cursor = doc.cursor_at(0);
    cursor.set_position(2, MoveMode::KeepAnchor);
    cursor
        .merge_char_format(&TextFormat {
            font_italic: Some(true),
            font_family: Some("Serif".into()),
            ..Default::default()
        })
        .unwrap();
    doc.clear_undo_redo();
    doc
}

fn format_at(doc: &TextDocument, at: usize) -> TextFormat {
    doc.cursor_at(at).char_format().unwrap()
}

fn formats(doc: &TextDocument) -> Vec<TextFormat> {
    let len = doc.to_plain_text().unwrap().chars().count();
    (0..len).map(|at| format_at(doc, at)).collect()
}

#[test]
fn the_text_inherits_its_format_with_the_merged_fields_over_it() {
    let doc = doc();
    let cursor = doc.cursor_at(2);
    cursor.insert_text_transient_merged("ni", &bold()).unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "abni cd");
    let mut expected = format_at(&doc, 1);
    assert_eq!(expected.font_italic, Some(true));
    assert_eq!(expected.font_family.as_deref(), Some("Serif"));
    expected.font_bold = Some(true);
    for at in 2..4 {
        assert_eq!(
            format_at(&doc, at),
            expected,
            "the inherited format (italic, the family), the merged field over it"
        );
    }
    assert_ne!(
        format_at(&doc, 1).font_bold,
        Some(true),
        "the text before is left alone"
    );
    assert_eq!(cursor.position(), 4, "the cursor after the text");
    assert!(!doc.can_undo(), "nothing recorded");
}

#[test]
fn unset_fields_stay_inherited_where_the_text_inherits_nothing() {
    let doc = doc();
    let cursor = doc.cursor_at(3);
    cursor.insert_text_transient_merged("ni", &bold()).unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "ab nicd");
    let format = format_at(&doc, 3);
    assert_eq!(format.font_bold, Some(true));
    assert_eq!(format.font_family, None, "no family is made up");
    assert_eq!(format.font_point_size, None, "nor a size");
}

#[test]
fn each_update_replaces_the_last_and_a_revert_gives_everything_back() {
    let doc = doc();
    let before = formats(&doc);
    let cursor = doc.cursor_at(5);
    cursor.insert_text_transient_merged("n", &bold()).unwrap();
    let shown = doc.transient_range().unwrap();
    cursor.set_position(shown.start, MoveMode::MoveAnchor);
    cursor.set_position(shown.end, MoveMode::KeepAnchor);
    cursor.insert_text_transient_merged("ni", &bold()).unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "ab cdni");
    assert!(format_at(&doc, 6).font_bold == Some(true));

    assert!(doc.revert_transient_edits());
    assert_eq!(doc.to_plain_text().unwrap(), "ab cd");
    assert_eq!(formats(&doc), before, "the formats are the recorded ones");
    assert!(
        !doc.can_undo() && !doc.can_redo(),
        "the history is untouched"
    );
}

#[test]
fn a_commit_records_the_text_in_the_format_it_inherits() {
    let doc = doc();
    let cursor = doc.cursor_at(5);
    cursor.insert_text_transient_merged("ni", &bold()).unwrap();
    cursor.commit_transient_text("\u{4F60}").unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "ab cd\u{4F60}");
    assert_ne!(
        format_at(&doc, 5).font_bold,
        Some(true),
        "the merged format went with the transient edits"
    );
    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "ab cd", "one step");
}

#[test]
fn a_host_records_the_format_with_the_commit_in_one_step() {
    let doc = doc();
    let cursor = doc.cursor_at(5);
    cursor.insert_text_transient_merged("ni", &bold()).unwrap();
    cursor.begin_edit_block();
    cursor.commit_transient_text("\u{4F60}").unwrap();
    let range = doc.cursor_at(5);
    range.set_position(6, MoveMode::KeepAnchor);
    range.merge_char_format(&bold()).unwrap();
    cursor.end_edit_block();
    assert_eq!(format_at(&doc, 5).font_bold, Some(true));
    doc.undo().unwrap();
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "ab cd",
        "one step takes both back"
    );
    assert!(!doc.can_undo());
}

#[test]
fn a_recorded_write_ends_it() {
    let doc = doc();
    let cursor = doc.cursor_at(5);
    cursor.insert_text_transient_merged("ni", &bold()).unwrap();
    doc.cursor_at(0).insert_text("X").unwrap();
    assert!(!doc.has_transient_edits());
    assert_eq!(doc.to_plain_text().unwrap(), "Xab cd");
    assert!(formats(&doc).iter().all(|f| f.font_bold != Some(true)));
}

#[test]
fn a_refused_insert_leaves_the_document_as_it_was() {
    let doc = doc();
    let table = doc.cursor_at(5).insert_table(1, 2).unwrap();
    let first = table.cell(0, 0).unwrap().blocks()[0].position();
    let second = table.cell(0, 1).unwrap().blocks()[0].position();
    doc.cursor_at(first).insert_text("left").unwrap();
    doc.cursor_at(second + 4).insert_text("right").unwrap();
    doc.clear_undo_redo();
    let before = doc.to_plain_text().unwrap();

    let cursor = doc.cursor_at(first + 2);
    cursor.set_position(second + 6, MoveMode::KeepAnchor);
    let refused = cursor
        .insert_text_transient_merged("X", &bold())
        .unwrap_err();
    assert!(matches!(
        refused,
        DocumentError::TransientEdits(TransientRefusal::CrossesTableCells)
    ));
    assert!(!doc.has_transient_edits());
    assert_eq!(doc.to_plain_text().unwrap(), before);
}
