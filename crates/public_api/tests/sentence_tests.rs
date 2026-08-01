//! `TextDocument::sentence_at` over a real document.
//!
//! The per-language tailoring is unit-tested inside `src/sentence.rs`, against the pure
//! block-relative function. What can only be checked here is the document half: that the
//! returned offsets are **absolute**, that the query stops at block boundaries, and that it
//! survives the positions a caret can actually hold.

use text_document::TextDocument;

fn new_doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

/// The text a range names, so assertions read as prose rather than as arithmetic.
fn slice(doc: &TextDocument, range: (usize, usize)) -> String {
    doc.to_plain_text()
        .unwrap()
        .chars()
        .skip(range.0)
        .take(range.1 - range.0)
        .collect()
}

#[test]
fn offsets_are_absolute_not_block_relative() {
    let doc = new_doc("First para. Still first.\nSecond para. Still second.");

    // A caret in the SECOND block must report offsets past the first block, or a highlight
    // would land on the wrong paragraph entirely.
    let second_block_start = "First para. Still first.\n".chars().count();
    let range = doc
        .sentence_at(second_block_start + 2, Some("en"))
        .expect("a sentence");
    assert_eq!(slice(&doc, range), "Second para.");
    assert!(
        range.0 >= second_block_start,
        "offsets must be absolute: got {range:?}, block starts at {second_block_start}"
    );
}

#[test]
fn a_sentence_never_crosses_a_block_boundary() {
    // No terminator at the end of the first block: a sentence still stops there, because a
    // paragraph break ends one.
    let doc = new_doc("An unfinished line\nA second line.");
    let range = doc.sentence_at(3, Some("en")).expect("a sentence");
    assert_eq!(slice(&doc, range), "An unfinished line");
}

#[test]
fn every_caret_position_in_a_document_resolves_or_declines_cleanly() {
    // The caret can sit anywhere from 0 to the last position; none of them may panic, and any
    // range that comes back must be well-formed and inside the document.
    let doc = new_doc("One. Two.\n\nThree? \"Yes,\" he said.\n");
    let total = doc.character_count() + 3; // past the end, to prove clamping
    for pos in 0..=total {
        if let Some((start, end)) = doc.sentence_at(pos, Some("en")) {
            assert!(start < end, "empty range at {pos}: {start}..{end}");
            assert!(
                end <= doc.to_plain_text().unwrap().chars().count(),
                "range past the document at {pos}: {start}..{end}"
            );
        }
    }
}

#[test]
fn an_empty_block_has_no_sentence() {
    let doc = new_doc("Text.\n\nMore text.");
    // The blank block between the two paragraphs.
    let blank = "Text.\n".chars().count();
    assert_eq!(doc.sentence_at(blank, Some("en")), None);
}

#[test]
fn an_empty_document_has_no_sentence() {
    let doc = new_doc("");
    assert_eq!(doc.sentence_at(0, Some("en")), None);
}

/// The locale is a per-call argument, so the same document answers differently for two
/// languages — which is the whole point of not storing it.
#[test]
fn the_locale_argument_changes_the_answer_for_the_same_document() {
    let doc = new_doc("Mr. Smith went home.");
    // English knows the title; an untailored language does not.
    assert_eq!(
        slice(&doc, doc.sentence_at(0, Some("en")).unwrap()),
        "Mr. Smith went home."
    );
    assert_eq!(slice(&doc, doc.sentence_at(0, None).unwrap()), "Mr.");
}

/// Multi-byte text must report char offsets; byte offsets would put a caret mid-character and
/// slice a highlight through a glyph.
#[test]
fn offsets_are_char_based_over_multibyte_text() {
    let doc = new_doc("Émile hésita. « Vraiment ? » demanda-t-il.");
    let range = doc.sentence_at(20, Some("fr")).expect("a sentence");
    assert_eq!(slice(&doc, range), "« Vraiment ? » demanda-t-il.");
}

/// An edit shifts every later offset, so the query must read the document as it is now — this
/// is the guarantee a caret-driven highlight leans on after every keystroke.
#[test]
fn the_query_follows_the_document_across_an_edit() {
    let doc = new_doc("One. Two.");
    assert_eq!(slice(&doc, doc.sentence_at(6, Some("en")).unwrap()), "Two.");

    doc.set_plain_text("Zero. One. Two.").unwrap();
    let range = doc.sentence_at(12, Some("en")).unwrap();
    assert_eq!(slice(&doc, range), "Two.");
    assert_eq!(range.0, 11, "the sentence moved with the text before it");
}
