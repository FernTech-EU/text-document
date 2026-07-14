//! `djot_to_plain_text` — the cheap corpus extraction a project-wide search runs on.
//!
//! Two things must both be true, or it is worse than useless:
//!
//!   1. It is **cheap**. A host app searching a manuscript asks "does this scene contain
//!      that word" of thousands of Djot rows on every keystroke. Doing that by importing
//!      each row into a document is not a slow feature, it is a frozen app.
//!
//!   2. It is **the same text the document searches** — character for character, including
//!      the things that are not prose at all. A cheaper second definition of "the text"
//!      that drifts is a trap: an occurrence count taken from it would not agree with what
//!      a replace re-derives inside the real document, and the replace's "the text moved
//!      under me, skip this field" guard would fire on good rows.
//!
//! Property (2) is pinned across the whole generated feature set by the parity assertion in
//! `djot_roundtrip_tests` — **including tables**, whose absence from that generator once
//! hid a real bug (a table's `U+FFFC` anchor was being dropped, so every offset after a
//! table was two characters short). What is here is the *why*, and the cost.

use std::time::Instant;

use text_document::{
    DjotImportOptions, FindOptions, TABLE_ANCHOR, TextDocument, djot_to_plain_text,
};

fn plain(djot: &str) -> String {
    djot_to_plain_text(djot, &DjotImportOptions::default())
}

/// **The bug this exists to kill.** Searching the Djot *source* means searching markup: a
/// query hits link URLs, emphasis markers and attribute syntax — text the writer never
/// wrote and cannot see.
#[test]
fn the_prose_is_extracted_and_the_markup_is_not() {
    let djot = "She read [the note](https://example.test/note) and *whispered* his name.";

    let text = plain(djot);
    assert_eq!(
        text, "She read the note and whispered his name.",
        "the link's URL and the emphasis markers are markup, not prose"
    );

    assert!(
        djot.contains("https"),
        "the fixture must actually contain a URL"
    );
    assert!(
        !text.contains("https"),
        "a search for `https` must not hit a link's destination"
    );
    assert!(
        !text.contains('*'),
        "a search for `*` must not hit an emphasis marker"
    );
}

/// Structure becomes separators, not text: a heading's `#`, a list's `-`, a quote's `>`
/// and a fence's backticks are all markup.
#[test]
fn block_markers_do_not_leak_into_the_prose() {
    let text = plain("# A Heading\n\n- first item\n\n- second item\n\n> a quotation");
    assert_eq!(text, "A Heading\nfirst item\nsecond item\na quotation");
    assert!(!text.contains('#') && !text.contains('-') && !text.contains('>'));
}

/// The extracted text IS the text the document searches — so an offset found in it is an
/// offset the document agrees with. (The general case is a property in
/// `djot_roundtrip_tests`; this is the readable instance of it.)
#[test]
fn an_offset_found_in_the_extract_is_an_offset_the_document_agrees_with() {
    let djot =
        "She called *Aurélien* into the trees, and [Aurélien](https://x.test/a) did not answer.";
    let doc = TextDocument::new();
    doc.set_djot(djot).unwrap().wait().unwrap();

    let text = plain(djot);
    let from_document: Vec<(usize, usize)> = doc
        .find_all("Aurélien", &FindOptions::default())
        .unwrap()
        .into_iter()
        .map(|m| (m.position, m.length))
        .collect();

    let from_extract: Vec<(usize, usize)> = text
        .match_indices("Aurélien")
        .map(|(byte, m)| (text[..byte].chars().count(), m.chars().count()))
        .collect();

    assert_eq!(
        from_document.len(),
        2,
        "both occurrences, incl. the link label"
    );
    assert_eq!(
        from_document, from_extract,
        "the document and the extract must agree on WHERE the matches are, or a count \
         taken from one will not survive a replace performed through the other"
    );
}

/// **The cost.** The extractor must be meaningfully cheaper than a full import — that is
/// its entire reason to exist. A full import creates a `Block` entity per paragraph, list
/// item and table cell, mirrors each into the rope and writes its format runs; this stops
/// at the parse.
///
/// The bar is deliberately loose (a mere 2x) so the test measures the *shape* of the cost
/// and does not flake on a loaded machine — if this ever fails, the extractor has started
/// doing the import's work.
#[test]
fn extraction_is_substantially_cheaper_than_a_full_import() {
    // A scene-sized chunk of prose, with the structure a real one has.
    let scene: String = (0..40)
        .map(|i| {
            format!(
                "## Section {i}\n\nShe called *Aurélien* into the trees, and [the note]\
                 (https://example.test/{i}) said nothing at all. He waited.\n\n\
                 - a thought\n\n- another\n\n> and a quotation\n\n"
            )
        })
        .collect();

    const ROUNDS: u32 = 5;

    let t0 = Instant::now();
    for _ in 0..ROUNDS {
        let text = plain(&scene);
        std::hint::black_box(text);
    }
    let extract = t0.elapsed();

    let t1 = Instant::now();
    for _ in 0..ROUNDS {
        let doc = TextDocument::new();
        doc.set_djot(&scene).unwrap().wait().unwrap();
        std::hint::black_box(doc.to_plain_text().unwrap());
    }
    let import = t1.elapsed();

    println!("extract: {extract:?}   full import: {import:?}");
    assert!(
        extract * 2 < import,
        "extraction ({extract:?}) is not meaningfully cheaper than a full import \
         ({import:?}) — it exists precisely so a project-wide search does not have to \
         import every scene on every keystroke"
    );
}

/// **A table occupies a position, and the extract has to say so.**
///
/// A table is not prose, but the document holds a `U+FFFC` anchor where it sits (the import
/// mirrors it into the rope, then the cells as ordinary blocks). Omitting it made the
/// extract *shorter than the text the document searches*, so every offset after the first
/// table was short by two characters — and a snippet taken from it would be sliced in the
/// wrong place.
///
/// This is a nasty one to catch by eye: `to_plain_text()` omits the anchor too, so the two
/// plain-text APIs agreed with **each other** and were **both** wrong. Only the document's
/// own search disagreed.
#[test]
fn a_table_carries_its_anchor_so_offsets_after_it_stay_true() {
    let djot = "intro\n\n| a | b |\n| - | - |\n| c | d |\n\nafter";

    let text = plain(djot);
    assert_eq!(
        text,
        format!("intro\n{TABLE_ANCHOR}\na\nb\nc\nd\nafter"),
        "the table announces itself with its anchor, then its cells row by row"
    );

    // And the offsets it produces are the document's offsets — the whole point.
    let doc = TextDocument::new();
    doc.set_djot(djot).unwrap().wait().unwrap();
    let hits = doc.find_all("after", &FindOptions::default()).unwrap();
    let from_extract = text.chars().count() - "after".chars().count();
    assert_eq!(
        hits[0].position, from_extract,
        "a word AFTER a table must sit at the same offset in the extract as it does in the \
         document — without the anchor the extract is two characters short and every offset \
         past the table is wrong"
    );
}

/// An **empty** block is still a block: the document holds an empty line for it, so the
/// extract must too. Deciding the separator by "is the output still empty" instead of "is
/// this the first block" swallowed both the block and its separator, shifting every offset
/// after it by one. The proptest caught this within seconds of tables being added to it.
#[test]
fn an_empty_block_still_occupies_a_line() {
    let djot = "```\n```\n\nA";
    let text = plain(djot);
    assert_eq!(text, "\nA", "the empty code fence keeps its (empty) line");

    let doc = TextDocument::new();
    doc.set_djot(djot).unwrap().wait().unwrap();
    let hits = doc.find_all("A", &FindOptions::default()).unwrap();
    assert_eq!(
        hits[0].position, 1,
        "the document puts `A` at offset 1, after the empty fence's line — the extract must \
         agree, or it is a character short"
    );
}

/// Degenerate inputs must not panic, and must not fabricate prose that is not there.
///
/// A bare metacharacter is markup with nothing inside it, and yields nothing — which is
/// the point: whatever the parser decides a construct *means*, the extractor reports its
/// prose and never its syntax.
#[test]
fn degenerate_input_yields_no_prose_and_does_not_panic() {
    for source in ["", "\n\n", "*", "#", "> ", "```\n```", "{}"] {
        let text = plain(source);
        assert!(
            text.trim().is_empty(),
            "{source:?} carries no prose, but the extractor produced {text:?}"
        );
    }
}
