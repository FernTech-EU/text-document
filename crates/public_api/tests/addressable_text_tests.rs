//! `to_addressable_text()` must be **the** string every document offset indexes into.
//!
//! The bug this guards against, as it actually happened downstream: a comment-anchoring
//! layer paired `to_plain_text()` with `blocks().position()` and with editor-widget
//! selection offsets. Those offsets live in the document's own char space — the space
//! `find_all` reports in, where an embedded table occupies its `U+FFFC` anchor plus a
//! `\n` separator. `to_plain_text()` is the human-readable *export* and omits the anchor,
//! so every offset after a table landed two characters short in it: a comment made on
//! `"salt-bleached"` stored its quote as `"lt-bleached d"`. The mismatch was invisible on
//! any document without a table, which is why it survived.
//!
//! The cure is not to change either space — the export must stay anchor-free, the
//! document must keep counting what it holds — but to make the addressable string
//! *reachable*, and to pin that it really is the string the offsets mean.

use text_document::{
    DjotImportOptions, FindOptions, ReplaceOptions, TABLE_ANCHOR, TextDocument, djot_to_plain_text,
};

/// Documents that exercise every construct with a position story: tables (the anchor),
/// quotes (child frames), images and code (inline objects, verbatim), and combinations.
const BATTERY: &[&str] = &[
    "First paragraph.\n\nSecond paragraph.\n\nThird.",
    "intro\n\n| a | b |\n| - | - |\n| c | d |\n\nafter",
    "> a0\n\na",
    "p1\n\n> q1\n\np2\n\n> q2",
    "> quoted\n\n# head\n\n- item\n\n| x |\n| - |\n| y |",
    "before ![alt](pic.png) after",
    "a\n\n| t |\n| - |\n| u |\n\nb\n\n| v |\n| - |\n| w |\n\nc",
    "",
];

fn doc_of(djot: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot(djot).unwrap().wait().unwrap();
    doc
}

/// Every block's reported start, applied to the addressable text, must land exactly on
/// that block's own text. This is the contract a downstream pairing of
/// `(to_addressable_text(), blocks().position())` rests on — comment anchoring is built
/// on exactly this pair.
fn assert_blocks_index_the_addressable_text(doc: &TextDocument, label: &str) {
    let addressable = doc.to_addressable_text().unwrap();
    let chars: Vec<char> = addressable.chars().collect();

    for (i, block) in doc.blocks().into_iter().enumerate() {
        let start = block.position();
        let text = block.text();
        let len = text.chars().count();
        assert!(
            start + len <= chars.len(),
            "{label}: block {i} claims [{start}, {}) in a {} char text\n\
             addressable = {addressable:?}",
            start + len,
            chars.len()
        );
        let slice: String = chars[start..start + len].iter().collect();
        assert_eq!(
            slice, text,
            "{label}: block {i}'s reported start must land on the block's own text\n\
             addressable = {addressable:?}"
        );
        if start > 0 {
            assert_eq!(
                chars[start - 1],
                '\n',
                "{label}: block {i} starts at {start}, but the char before it is not the \
                 block separator\naddressable = {addressable:?}"
            );
        }
    }
}

/// The pairing the downstream comment layer performs, proven for every construct.
#[test]
fn block_starts_index_the_addressable_text() {
    for src in BATTERY {
        assert_blocks_index_the_addressable_text(&doc_of(src), &format!("{src:?}"));
    }
}

/// The reproduction that started this: prose after a table, addressed the way a comment
/// addresses it. The last block's reported start must land exactly on its text.
#[test]
fn prose_after_a_table_resolves_where_its_block_says_it_does() {
    let doc = doc_of("intro\n\n| a | b |\n| - | - |\n| c | d |\n\nthe salt-bleached door");
    let addressable = doc.to_addressable_text().unwrap();
    let chars: Vec<char> = addressable.chars().collect();

    let last = doc.blocks().last().unwrap().position();
    let tail: String = chars[last..].iter().collect();
    assert_eq!(
        tail, "the salt-bleached door",
        "the last block's reported start must land exactly on the last block's text\n\
         addressable = {addressable:?}"
    );
}

/// The capture half of the same reproduction: a selection offset (what an editor widget
/// reports, i.e. a `find_all` position) must slice the addressable text back to the
/// selected words — this is the exact pairing that once stored `"lt-bleached d"`.
#[test]
fn a_selection_offset_after_a_table_slices_the_addressable_text() {
    let doc = doc_of("intro\n\n| a | b |\n| - | - |\n| c | d |\n\nthe salt-bleached door");
    let position = doc
        .find_all("salt-bleached", &FindOptions::default())
        .unwrap()
        .first()
        .unwrap()
        .position;

    let addressable = doc.to_addressable_text().unwrap();
    let chars: Vec<char> = addressable.chars().collect();
    let captured: String = chars[position..position + "salt-bleached".chars().count()]
        .iter()
        .collect();

    assert_eq!(
        captured, "salt-bleached",
        "the quote captured at the document's own offset must be the selected words\n\
         addressable = {addressable:?}\nposition = {position}"
    );
}

/// Every `find_all` match must slice out of the addressable text as exactly its
/// `matched_text` — the two APIs claim the same offset space, so prove it, in every
/// construct the battery has.
#[test]
fn find_all_offsets_slice_the_addressable_text() {
    for src in BATTERY {
        let doc = doc_of(src);
        let addressable = doc.to_addressable_text().unwrap();
        let chars: Vec<char> = addressable.chars().collect();
        for m in doc.find_all("a", &FindOptions::default()).unwrap() {
            let slice: String = chars[m.position..m.position + m.length].iter().collect();
            assert_eq!(
                slice, m.matched_text,
                "a find_all match must occupy its reported range in the addressable text, \
                 for {src:?}\naddressable = {addressable:?}"
            );
        }
    }
}

/// A live document and bare Djot source must yield the same addressable string —
/// `to_addressable_text()` and `djot_to_plain_text()` are one definition of "the text",
/// reached with and without a document in hand.
///
/// (Footnote definitions are the known, pinned exception — see
/// [`footnote_bodies_are_searched_in_the_live_document`].)
#[test]
fn the_live_view_and_the_djot_view_are_the_same_string() {
    for src in BATTERY {
        assert_eq!(
            doc_of(src).to_addressable_text().unwrap(),
            djot_to_plain_text(src, &DjotImportOptions::default()),
            "to_addressable_text() and djot_to_plain_text() disagree for {src:?}"
        );
    }
}

/// The export view is the addressable view minus its object anchors — nothing more.
/// The existing `plain_text_order_tests` pin this through `djot_to_plain_text`; this is
/// the same tie proven on the live accessor, so the two public methods can never drift.
#[test]
fn the_export_view_is_the_addressable_view_minus_its_anchors() {
    for src in BATTERY {
        let doc = doc_of(src);
        let addressable = doc.to_addressable_text().unwrap();
        let without_anchors: Vec<&str> = addressable
            .split('\n')
            .filter(|line| *line != TABLE_ANCHOR)
            .collect();
        assert_eq!(
            doc.to_plain_text().unwrap(),
            without_anchors.join("\n"),
            "to_plain_text() must be to_addressable_text() minus anchor lines, for {src:?}"
        );
    }
}

/// The contract must survive **editing**, not just importing — the downstream pairing
/// reads a live document mid-session, after the rope has been spliced.
#[test]
fn the_addressable_text_stays_true_after_an_edit() {
    let doc = doc_of("intro\n\n| a | b |\n| - | - |\n| c | d |\n\nthe salt-bleached door");
    let replaced = doc
        .replace_text(
            "intro",
            "a much longer opening line",
            true,
            &ReplaceOptions::default(),
        )
        .unwrap();
    assert_eq!(replaced, 1, "the edit must actually happen");

    assert_blocks_index_the_addressable_text(&doc, "after replace_text");

    let position = doc
        .find_all("salt-bleached", &FindOptions::default())
        .unwrap()
        .first()
        .unwrap()
        .position;
    let chars: Vec<char> = doc.to_addressable_text().unwrap().chars().collect();
    let captured: String = chars[position..position + "salt-bleached".chars().count()]
        .iter()
        .collect();
    assert_eq!(captured, "salt-bleached");
}

/// Current behaviour, pinned deliberately: a footnote definition's body IS part of the
/// live document's addressable text — its blocks are mirrored into the rope, an
/// in-document search runs over them, and their `position()` counts them.
/// `djot_to_plain_text` and `character_count()` treat the body as out of flow and omit
/// it, so the live view and the djot view differ by exactly the note bodies on such a
/// document. Whether search *should* see note bodies is an open product question; what
/// must hold either way is that `to_addressable_text()` matches what search actually
/// does — which is what this test asserts, body text included.
#[test]
fn footnote_bodies_are_searched_in_the_live_document() {
    let doc = doc_of("A claim.[^n]\n\n[^n]: The note body.\n\nAfter.");

    assert_blocks_index_the_addressable_text(&doc, "footnote document");

    let matches = doc
        .find_all("The note body", &FindOptions::default())
        .unwrap();
    let m = matches
        .first()
        .expect("today, an in-document search finds text inside a footnote's body");
    let chars: Vec<char> = doc.to_addressable_text().unwrap().chars().collect();
    let slice: String = chars[m.position..m.position + m.length].iter().collect();
    assert_eq!(
        slice, "The note body",
        "to_addressable_text() must include what search searches — the note body"
    );
}

/// An empty document still answers, with an empty string, and its single empty block
/// starts at 0.
#[test]
fn an_empty_document_has_an_empty_addressable_text() {
    let doc = doc_of("");
    assert_eq!(doc.to_addressable_text().unwrap(), "");
    let blocks = doc.blocks();
    assert_eq!(blocks.len(), 1, "an empty document holds one empty block");
    assert_eq!(blocks[0].position(), 0);
}
