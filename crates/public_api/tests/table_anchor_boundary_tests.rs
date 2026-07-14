//! The rope's table anchor must be separated from the block before it — **even when that
//! block is empty**.
//!
//! `rope_append_table_anchor` decided whether to prepend its `\n` boundary by asking the rope
//! `len_bytes() == 0`. Every other element in the importer decides the same question from a
//! positional flag (`emitted_any_main_block`). Those two answers agree on every document
//! except one: a document whose **first block is empty** — an empty code fence, a blank
//! heading. The rope is then still zero bytes long *even though a block has been emitted*, so
//! the emptiness check skips the boundary that block is owed.
//!
//! The result is not a cosmetic missing newline. The empty block and the anchor both end up
//! registered at byte 0 in `block_offsets` — **two entities claiming one offset**, in the
//! index through which every offset-based edit resolves. And the document's searchable text
//! loses a character, so every offset after it is off by one: a rename in such a document
//! splices one char to the left of where the writer saw the match.
//!
//! Found by `djot_roundtrip_tests`' parity property, which searches each document for its own
//! extracted text — not by anyone reading the code.

use text_document::{DjotImportOptions, FindOptions, TextDocument, djot_to_plain_text};

fn doc_with(djot: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot(djot).unwrap().wait().unwrap();
    doc
}

/// The exact counterexample: an empty code fence, then a table.
#[test]
fn an_empty_first_block_still_separates_the_table_anchor() {
    let djot = "```\n\n```\n\n| Aa |\n|---|\n| aa |";
    let doc = doc_with(djot);

    // The cheap extractor's answer, which is the one the block structure implies:
    // empty block, `\n`, anchor, `\n`, cell, `\n`, cell.
    let extracted = djot_to_plain_text(djot, &DjotImportOptions::default());
    assert_eq!(extracted, "\n\u{FFFC}\nAa\naa");

    // …and the document must search exactly that. Before the fix it searched
    // "\u{FFFC}\nAa\naa" — one char short, with the empty block's boundary swallowed.
    let hits = doc.find_all(&extracted, &FindOptions::default()).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "the document must contain its own extracted text"
    );
    assert_eq!(hits[0].position, 0);
    assert_eq!(hits[0].length, extracted.chars().count());
}

/// The offset consequence, stated on its own: the anchor is at char 1, not char 0, because
/// the empty block's boundary occupies char 0. Every offset in the document depends on it.
#[test]
fn the_anchor_does_not_sit_on_top_of_the_empty_block() {
    let doc = doc_with("```\n\n```\n\n| Aa |\n|---|\n| aa |");
    let anchor = doc.find_all("\u{FFFC}", &FindOptions::default()).unwrap();
    assert_eq!(anchor.len(), 1);
    assert_eq!(
        anchor[0].position, 1,
        "char 0 is the empty block's boundary; the anchor comes after it"
    );

    // And the cells follow, in order, at the offsets that implies.
    let aa = doc.find_all("Aa", &FindOptions::default()).unwrap();
    assert_eq!(aa[0].position, 3);
}

/// A document that does *not* start with an empty block was always right, and must stay so —
/// the fix must not add a leading boundary where none belongs.
#[test]
fn a_table_that_starts_the_document_gets_no_leading_boundary() {
    let djot = "| Aa |\n|---|\n| aa |";
    let doc = doc_with(djot);

    let extracted = djot_to_plain_text(djot, &DjotImportOptions::default());
    assert_eq!(
        extracted, "\u{FFFC}\nAa\naa",
        "no boundary before the first element"
    );

    let hits = doc.find_all(&extracted, &FindOptions::default()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].position, 0);

    let anchor = doc.find_all("\u{FFFC}", &FindOptions::default()).unwrap();
    assert_eq!(anchor[0].position, 0, "the anchor IS the first char");
}

/// …and the ordinary case — prose, then a table — keeps its single boundary.
#[test]
fn prose_before_a_table_keeps_exactly_one_boundary() {
    let djot = "hello\n\n| Aa |\n|---|\n| aa |";
    let doc = doc_with(djot);

    let extracted = djot_to_plain_text(djot, &DjotImportOptions::default());
    assert_eq!(extracted, "hello\n\u{FFFC}\nAa\naa");

    let hits = doc.find_all(&extracted, &FindOptions::default()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].position, 0);
}

/// The Djot must still round-trip. The rope carries the anchor; the exporter must not start
/// emitting a stray blank line because of it.
#[test]
fn the_djot_still_round_trips() {
    for djot in [
        "```\n\n```\n\n| Aa |\n|---|\n| aa |",
        "| Aa |\n|---|\n| aa |",
        "hello\n\n| Aa |\n|---|\n| aa |",
    ] {
        let once = doc_with(djot).to_djot().unwrap();
        let twice = doc_with(&once).to_djot().unwrap();
        assert_eq!(once, twice, "round-trip is not a fixpoint for {djot:?}");
    }
}
