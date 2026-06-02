//! Tests for the typed public error API (`DocumentError`).
//!
//! These assert that callers can match on specific failure categories
//! rather than only stringifying an opaque error.

use text_document::{DocumentError, ListFormat, TextDocument};

#[test]
fn table_op_outside_table_returns_invalid_cursor_context() {
    let doc = TextDocument::new();
    doc.set_plain_text("plain text, no table").unwrap();

    let err = doc.cursor().merge_selected_cells().unwrap_err();

    assert!(
        matches!(err, DocumentError::InvalidCursorContext(_)),
        "expected InvalidCursorContext, got {err:?}"
    );
}

#[test]
fn list_op_outside_list_returns_invalid_cursor_context() {
    let doc = TextDocument::new();
    doc.set_plain_text("plain, no list").unwrap();

    let err = doc
        .cursor()
        .set_current_list_format(&ListFormat::default())
        .unwrap_err();

    assert!(
        matches!(err, DocumentError::InvalidCursorContext(_)),
        "expected InvalidCursorContext, got {err:?}"
    );
}

#[test]
fn typed_error_display_preserves_message() {
    // Display text is unchanged from the pre-typed-error API, so existing
    // string-based handling still works.
    let doc = TextDocument::new();
    doc.set_plain_text("plain").unwrap();

    let err = doc.cursor().merge_selected_cells().unwrap_err();
    assert!(err.to_string().contains("not inside a table"), "got {err}");
}
