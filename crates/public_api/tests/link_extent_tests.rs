//! Finding the whole of a link, not just the part under the caret.
//!
//! A link is a stretch of runs agreeing on a destination, and format runs split
//! on **any** field difference — so italicising one word inside a link cuts it
//! into three runs carrying the same href. An extent query that returned only
//! the run under the caret would report a third of the link, and "Edit link"
//! built on that answer would rewrite a third of the writer's text while
//! leaving the rest still linked.
//!
//! These tests pin the coalescing, the boundaries, and the cases that must
//! *not* coalesce.

use text_document::{MoveMode, TextCursor, TextDocument};

/// A document over `djot`, and one cursor parked at `position`.
///
/// The cursor is returned rather than re-fetched, because `TextDocument::
/// cursor()` is a **factory**: every call mints a fresh cursor at 0, so
/// setting a position on one and reading from another silently asks about
/// position 0.
fn doc_at(djot: &str, position: usize) -> (TextDocument, TextCursor) {
    let doc = TextDocument::new();
    doc.set_djot(djot).unwrap().wait().unwrap();
    let cursor = doc.cursor_at(position);
    (doc, cursor)
}

#[test]
fn a_caret_inside_a_link_finds_it() {
    let (_doc, cursor) = doc_at("[example](https://example.com)", 3);
    let extent = cursor.link_at_caret().expect("caret is in a link");

    assert_eq!(extent.href, "https://example.com");
    assert_eq!(extent.text, "example");
    assert_eq!(extent.start, 0);
    assert_eq!(extent.end, 7);
}

#[test]
fn a_caret_in_plain_prose_finds_nothing() {
    let (_doc, cursor) = doc_at("plain prose, no link here", 5);
    assert!(cursor.link_at_caret().is_none());
}

#[test]
fn a_link_split_by_an_inner_mark_reports_its_whole_reach() {
    // THE case this module exists for. `_stormy_` splits the link into three
    // runs — "a ", "stormy", " night" — all carrying the same destination.
    // Parking the caret in the middle run must still report all three.
    let (_doc, cursor) = doc_at("[a _stormy_ night](https://example.com)", 5);
    let extent = cursor.link_at_caret().expect("caret is in a link");

    assert_eq!(
        extent.text, "a stormy night",
        "the extent must span every run the mark split the link into"
    );
    assert_eq!(extent.start, 0);
    assert_eq!(extent.end, 14);
    assert_eq!(extent.href, "https://example.com");
}

#[test]
fn the_whole_reach_is_reported_from_any_run_of_it() {
    // Same document, caret in each of the three runs in turn: the answer must
    // not depend on which splinter the writer happened to click.
    let expected = ("a stormy night".to_string(), 0usize, 14usize);
    for position in [1, 5, 12] {
        let (_doc, cursor) = doc_at("[a _stormy_ night](https://example.com)", position);
        let extent = cursor
            .link_at_caret()
            .unwrap_or_else(|| panic!("caret at {position} is in a link"));
        assert_eq!(
            (extent.text, extent.start, extent.end),
            expected,
            "extent from position {position}"
        );
    }
}

#[test]
fn two_adjacent_links_do_not_bleed_into_each_other() {
    // Adjacent, different destinations. They must stay two links — which is
    // why the walk compares destinations rather than "is this a link at all".
    let (_doc, cursor) = doc_at("[one](https://one.example)[two](https://two.example)", 1);
    let first = cursor.link_at_caret().expect("caret is in a link");
    assert_eq!(first.text, "one");
    assert_eq!(first.href, "https://one.example");
    assert_eq!(first.end, 3);

    cursor.set_position(5, MoveMode::MoveAnchor);
    let second = cursor.link_at_caret().expect("caret is in a link");
    assert_eq!(second.text, "two");
    assert_eq!(second.href, "https://two.example");
    assert_eq!(second.start, 3);
}

#[test]
fn a_caret_on_either_edge_is_still_inside() {
    // Caret semantics, matching `block_format`: a caret at the end of a link is
    // still in it, exactly as a caret at the end of a paragraph is still in
    // that paragraph. Without this, clicking just past a link and choosing
    // "Edit link" would find nothing.
    let (_doc, cursor) = doc_at("[example](https://example.com)", 0);
    assert_eq!(
        cursor.link_at_caret().map(|e| e.text),
        Some("example".to_string()),
        "the leading edge counts as inside"
    );

    cursor.set_position(7, MoveMode::MoveAnchor);
    assert_eq!(
        cursor.link_at_caret().map(|e| e.text),
        Some("example".to_string()),
        "the trailing edge counts as inside"
    );
}

#[test]
fn a_link_surrounded_by_prose_stops_at_its_own_bounds() {
    let (_doc, cursor) = doc_at("before [middle](https://example.com) after", 10);
    let extent = cursor.link_at_caret().expect("caret is in a link");

    assert_eq!(extent.text, "middle");
    assert_eq!(extent.start, 7, "must not swallow the prose before it");
    assert_eq!(extent.end, 13, "must not swallow the prose after it");
}

#[test]
fn clearing_a_link_over_its_extent_removes_it() {
    // The round trip the Remove-link command depends on: find the extent,
    // select exactly it, clear. A zero-width selection formats nothing, which
    // is why the select step is not optional.
    let (doc, cursor) = doc_at("[example](https://example.com)", 3);
    let extent = cursor.link_at_caret().expect("caret is in a link");

    cursor.set_position(extent.start, MoveMode::MoveAnchor);
    cursor.set_position(extent.end, MoveMode::KeepAnchor);
    cursor.clear_char_anchor().expect("clear");

    cursor.set_position(3, MoveMode::MoveAnchor);
    assert!(
        cursor.link_at_caret().is_none(),
        "the link must be gone after clearing its extent"
    );
    assert_eq!(
        doc.to_djot().unwrap().trim(),
        "example",
        "and the text it covered must survive as plain prose"
    );
}

#[test]
fn a_link_applied_as_a_character_format_saves_as_djot() {
    // The convergence proof. The app builds links by merging a character
    // format over a range, never by inserting `[text](url)` markup — so this
    // asserts the two paths meet at the wire format. If they ever diverge, a
    // link the writer made would not be the link the file records.
    let (doc, cursor) = doc_at("Read the manual today", 0);

    cursor.set_position(9, MoveMode::MoveAnchor);
    cursor.set_position(15, MoveMode::KeepAnchor);
    cursor
        .merge_char_format(&text_document::TextFormat {
            anchor_href: Some("https://example.com".into()),
            ..Default::default()
        })
        .expect("merge");

    assert_eq!(
        doc.to_djot().unwrap().trim(),
        "Read the [manual](https://example.com) today"
    );
}

#[test]
fn a_link_applied_over_a_mark_keeps_the_mark() {
    // The reason a link is a format merge rather than a text reinsertion: the
    // writer's existing italic survives being linked, and both reach the file.
    let (doc, cursor) = doc_at("Read the _manual_ today", 0);

    cursor.set_position(9, MoveMode::MoveAnchor);
    cursor.set_position(15, MoveMode::KeepAnchor);
    cursor
        .merge_char_format(&text_document::TextFormat {
            anchor_href: Some("https://example.com".into()),
            ..Default::default()
        })
        .expect("merge");

    // The link nests innermost, inside the emphasis — the nesting
    // `link_marker_nesting_tests` pins, reached here from the format side.
    assert_eq!(
        doc.to_djot().unwrap().trim(),
        "Read the _[manual](https://example.com)_ today"
    );
}

// ── Paste ───────────────────────────────────────────────────────
//
// Both routes into a document have to carry a link, and they are different
// code: an internal paste re-inserts a stored `DocumentFragment`, while an
// external one parses `text/html` from another application. A link that
// survives one and not the other is the kind of gap nobody notices until a
// writer pastes a research page and gets flat prose.

#[test]
fn an_external_html_paste_keeps_its_links() {
    // The shape a browser, Word or Google Docs puts on the clipboard.
    let (doc, cursor) = doc_at("", 0);
    cursor
        .insert_html(r#"<p>See <a href="https://example.com">the source</a> for more.</p>"#)
        .expect("paste html");

    assert_eq!(
        doc.to_djot().unwrap().trim(),
        "See [the source](https://example.com) for more."
    );
}

#[test]
fn an_external_html_paste_marks_the_run_as_a_link() {
    // Not just the Djot: the run itself has to read as a link, or it would
    // neither render as one nor be found by "the link under the caret".
    let (_doc, cursor) = doc_at("", 0);
    cursor
        .insert_html(r#"<a href="https://example.com">the source</a>"#)
        .expect("paste html");

    cursor.set_position(3, MoveMode::MoveAnchor);
    let extent = cursor.link_at_caret().expect("the pasted run is a link");
    assert_eq!(extent.href, "https://example.com");
    assert_eq!(extent.text, "the source");
}

#[test]
fn an_internal_copy_paste_keeps_its_links() {
    // Copy inside the app: the selection is carried as a `DocumentFragment`,
    // not as markup, so this exercises the format-run path rather than a
    // parser.
    let (source, source_cursor) = doc_at("Read [the manual](https://example.com) today", 0);
    source_cursor.set_position(5, MoveMode::MoveAnchor);
    source_cursor.set_position(15, MoveMode::KeepAnchor);
    let copied = source_cursor.selection();

    let target = TextDocument::new();
    target.set_plain_text("Start: ").expect("seed");
    let target_cursor = target.cursor_at(7);
    target_cursor.insert_fragment(&copied).expect("paste");

    assert_eq!(
        target.to_djot().unwrap().trim(),
        "Start: [the manual](https://example.com)"
    );
    let _ = source;
}

#[test]
fn copying_out_of_the_app_publishes_the_link_as_html() {
    // The outbound half: what lands on the clipboard for *another* app. The
    // editor publishes `text/html` from the fragment, so a link copied out of
    // a manuscript arrives in a browser or Word as a link, not as bare words.
    let (_doc, cursor) = doc_at("Read [the manual](https://example.com) today", 0);
    cursor.set_position(5, MoveMode::MoveAnchor);
    cursor.set_position(15, MoveMode::KeepAnchor);

    let html = cursor.selection().to_html();
    assert!(
        html.contains(r#"<a href="https://example.com">"#),
        "the clipboard HTML must carry the anchor — got {html}"
    );
    assert!(html.contains("the manual"));
}
