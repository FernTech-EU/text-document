//! The live-document counter, on its own.
//!
//! **Its own test binary, and that is the point.** The counter is process-global,
//! so a test asserting an exact delta cannot share a process with tests that
//! create documents on other threads — cargo runs each integration-test file as
//! its own binary, so a file holding one test holds the process alone.

use text_document::TextDocument;

fn new_doc_with_text(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

/// The live-document count rises with each document and falls when the last handle
/// to one goes.
///
/// A `TextDocument` is a handle onto a shared body, so "did the application let go"
/// is a question no owner can answer from the outside — and a host that opens a
/// document per scene and never drops one grows by a whole manuscript per project.
/// This is the counter that says so, and this is what makes it trustworthy: a clone
/// must not move it, and a drop must.
#[test]
fn the_live_count_follows_the_bodies_and_not_the_handles() {
    let before = text_document::live_document_count();

    let doc = new_doc_with_text("One");
    assert_eq!(text_document::live_document_count(), before + 1);

    let clone = doc.clone();
    assert_eq!(
        text_document::live_document_count(),
        before + 1,
        "a clone shares the body, so it is not a second document"
    );

    let second = new_doc_with_text("Two");
    assert_eq!(text_document::live_document_count(), before + 2);

    drop(clone);
    assert_eq!(
        text_document::live_document_count(),
        before + 2,
        "one handle of two going does not free the body"
    );
    drop(doc);
    assert_eq!(text_document::live_document_count(), before + 1);
    drop(second);
    assert_eq!(text_document::live_document_count(), before);
}
