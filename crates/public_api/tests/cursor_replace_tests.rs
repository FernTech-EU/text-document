//! `TextCursor::replace` — the interactive counterpart to `TextDocument::replace_text`.
//!
//! `insert_text` only ever gets `ReplaceFormatPolicy::InheritPreceding`: it composes a
//! delete then an insert, so the replacement always inherits whatever run preceded it and
//! anything under the range is dropped. `replace` is the same edit with the same atomicity
//! and undo guarantees, but lets the caller choose what the replacement wears — the
//! capability `replace_format_tests.rs` already pins for the batch `replace_text` path,
//! exercised here through the cursor a live editor actually drives.

use text_document::{DjotExportOptions, DjotImportOptions, MoveMode, ReplaceFormatPolicy, TextDocument};

fn new_doc_with_text(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

#[test]
fn default_policy_matches_plain_insert_text_within_one_block() {
    let via_replace = new_doc_with_text("Hello world");
    let cursor = via_replace.cursor();
    cursor.replace(6, 11, "Rust", ReplaceFormatPolicy::InheritPreceding).unwrap();

    let via_insert = new_doc_with_text("Hello world");
    let cursor2 = via_insert.cursor();
    cursor2.set_position(6, MoveMode::MoveAnchor);
    cursor2.set_position(11, MoveMode::KeepAnchor);
    cursor2.insert_text("Rust").unwrap();

    assert_eq!(via_replace.to_plain_text().unwrap(), "Hello Rust");
    assert_eq!(
        via_replace.to_plain_text().unwrap(),
        via_insert.to_plain_text().unwrap(),
        "InheritPreceding through replace() must match insert_text()'s composed delete+insert byte for byte"
    );
    assert_eq!(cursor.position(), cursor2.position());
}

#[test]
fn start_and_end_may_be_given_in_either_order() {
    let a = new_doc_with_text("Hello world");
    a.cursor().replace(6, 11, "Rust", ReplaceFormatPolicy::InheritPreceding).unwrap();

    let b = new_doc_with_text("Hello world");
    b.cursor().replace(11, 6, "Rust", ReplaceFormatPolicy::InheritPreceding).unwrap();

    assert_eq!(a.to_plain_text().unwrap(), b.to_plain_text().unwrap());
    assert_eq!(a.to_plain_text().unwrap(), "Hello Rust");
}

#[test]
fn zero_length_range_is_a_plain_insert() {
    let doc = new_doc_with_text("Hello world");
    doc.cursor().replace(5, 5, ",", ReplaceFormatPolicy::PreserveNothing).unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello, world");
}

#[test]
fn lands_as_one_undo_entry() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor();
    cursor.replace(6, 11, "Rust", ReplaceFormatPolicy::PreserveNothing).unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello Rust");

    doc.undo().unwrap();
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "Hello world",
        "one undo must restore the pre-replace text, exactly like insert_text's own guarantee"
    );

    doc.redo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Hello Rust");
}

#[test]
fn leaves_the_cursor_at_the_end_of_the_replacement() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor();
    cursor.replace(6, 11, "Rust", ReplaceFormatPolicy::InheritPreceding).unwrap();
    assert_eq!(cursor.position(), 10, "\"Hello \" (6) + \"Rust\" (4) = 10");
    assert_eq!(cursor.anchor(), 10, "no selection should remain after the replace");
}

#[test]
fn cross_block_selection_falls_back_to_compose_delete_insert() {
    let doc = new_doc_with_text("HelloWorld");
    let cursor = doc.cursor_at(5);
    cursor.insert_block().unwrap();
    assert_eq!(doc.block_count(), 2);
    assert_eq!(doc.to_plain_text().unwrap(), "Hello\nWorld");

    // Selection spans the block boundary: "o\nWo" -> replaced with "X".
    doc.cursor().replace(4, 8, "X", ReplaceFormatPolicy::PreserveIfFullyCovered).unwrap();
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "HellXrld",
        "cross-block replace must still succeed via the delete+insert fallback, \
         merging the two blocks back into one"
    );
}

fn replace_and_export(djot: &str, start: usize, end: usize, replacement: &str, policy: ReplaceFormatPolicy) -> String {
    let doc = TextDocument::new();
    doc.set_djot_with_options(djot, DjotImportOptions::default())
        .and_then(|op| op.wait())
        .expect("set_djot");

    doc.cursor().replace(start, end, replacement, policy).expect("replace");

    doc.to_djot_with_options(DjotExportOptions::default())
        .expect("to_djot")
        .trim()
        .to_string()
}

/// The exact scenario `replace_format_tests.rs` pins for the batch path — reproduced here
/// through the interactive cursor, confirming the format-policy choice survives the
/// `InsertTextDto`/`execute_insert_with_selection` path, not just `replace_text`'s.
#[test]
fn preserve_if_fully_covered_keeps_a_wholly_styled_name_through_the_cursor() {
    let out = replace_and_export(
        "She called *Aurélien* into the trees.",
        11, // start of "Aurélien" in the parsed plain text "She called Aurélien into the trees."
        19, // end of "Aurélien" (8 chars)
        "Aurélian",
        ReplaceFormatPolicy::PreserveIfFullyCovered,
    );
    assert_eq!(
        out, "She called *Aurélian* into the trees.",
        "a name that was entirely emphasised must stay emphasised across an interactive rename"
    );
}

#[test]
fn default_policy_drops_styling_through_the_cursor_same_as_the_batch_path() {
    let out = replace_and_export(
        "She called *Aurélien* into the trees.",
        11,
        19,
        "Aurélian",
        ReplaceFormatPolicy::InheritPreceding,
    );
    assert_eq!(
        out, "She called Aurélian into the trees.",
        "the default must stay pinned to the historical behaviour through the cursor too"
    );
}
