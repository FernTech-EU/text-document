//! `to_plain_text()` must return the document's prose in **reading order**.
//!
//! It did not. `export_plain_text_uc` sorted each frame's blocks against only *their own
//! frame's* siblings, then concatenated the frames in **frame-creation order** — silently
//! assuming creation order equals document order. A blockquote becomes a *child frame*, and
//! it is created after the root frame, so every blockquote's prose was hoisted to the **end**
//! of the export:
//!
//! ```text
//! djot            "> a0\n\na"
//! to_plain_text   "a\na0"     ← the paragraph came FIRST
//! to_djot         "> a0\n\na" ← the djot exporter had it right
//! find_all sees   "a0\na"     ← so did search
//! ```
//!
//! `to_plain_text` was the only one of the three walks that was wrong. `document_position` is
//! a single global counter across the whole parse (root and every nesting depth alike), so
//! pooling every frame's blocks and sorting **once, globally** reconstructs true reading
//! order — which is exactly what `find_all` already did.
//!
//! Reachable today through this crate's own CLI (`cat` / `convert` / `replace`), which wrote
//! `to_plain_text()` straight to stdout: converting any document with a blockquote silently
//! scrambled the paragraphs.

use text_document::{DjotImportOptions, TABLE_ANCHOR, TextDocument, djot_to_plain_text};

fn plain(djot: &str) -> String {
    let doc = TextDocument::new();
    doc.set_djot(djot).unwrap().wait().unwrap();
    doc.to_plain_text().unwrap()
}

/// The reported case, minimal.
#[test]
fn a_blockquote_is_not_hoisted_to_the_end() {
    assert_eq!(
        plain("> a0\n\na"),
        "a0\na",
        "the quotation is written first, so it must be read first"
    );
}

/// The interleaving must survive, not just the first pair.
#[test]
fn quotes_and_paragraphs_keep_their_interleaving() {
    assert_eq!(plain("p1\n\n> q1\n\np2\n\n> q2"), "p1\nq1\np2\nq2");
    assert_eq!(plain("a\n\n> a0\n\nb"), "a\na0\nb");
    assert_eq!(plain("> a0\n\na\n\n> a1"), "a0\na\na1");
    assert_eq!(plain("> a0\n\n> a1\n\na"), "a0\na1\na");
}

/// Hoisting was not specific to paragraphs — any top-level block after a quote was pulled in
/// front of it.
#[test]
fn a_quote_is_not_hoisted_past_a_heading_a_list_or_a_fence() {
    assert_eq!(plain("> a0\n\n# h"), "a0\nh");
    assert_eq!(plain("> a0\n\n- item"), "a0\nitem");
    assert_eq!(plain("> a0\n\n```\ncode\n```"), "a0\ncode");
}

/// Nested blockquotes, and a quote holding several blocks, must also stay in place.
#[test]
fn nested_and_multi_block_quotes_stay_in_place() {
    assert_eq!(
        plain("> outer\n>\n> > inner\n\nafter"),
        "outer\ninner\nafter"
    );
    assert_eq!(
        plain("> p1\n>\n> p2\n\nafter"),
        "p1\np2\nafter",
        "a quote's own blocks stay in order AND stay before what follows the quote"
    );
}

/// Table cells sit where the table sits.
#[test]
fn a_table_stays_where_it_was_written() {
    assert_eq!(
        plain("intro\n\n| a | b |\n| - | - |\n| c | d |\n\nafter"),
        "intro\na\nb\nc\nd\nafter"
    );
}

/// **The tie that makes a future divergence impossible to miss.**
///
/// `to_plain_text()` is the *human-readable* view: prose, with no object anchors — which is
/// why the crate's own fast path bails the moment a table exists. `djot_to_plain_text()` is
/// the *addressable* view: character-for-character the text the document searches, table
/// anchors and all.
///
/// They are allowed to differ in exactly one way — the anchors — and in no other. Pin that,
/// so the two can never drift on ORDER again, which is the drift that caused this bug.
#[test]
fn the_human_view_is_the_addressable_view_minus_its_anchors() {
    for src in [
        "> a0\n\na",
        "p1\n\n> q1\n\np2\n\n> q2",
        "intro\n\n| a | b |\n| - | - |\n| c | d |\n\nafter",
        "> quoted\n\n# head\n\n- item\n\n| x |\n| - |\n| y |",
        "a\n\nb\n\nc",
    ] {
        let addressable = djot_to_plain_text(src, &DjotImportOptions::default());
        let without_anchors: Vec<&str> = addressable
            .split('\n')
            .filter(|line| *line != TABLE_ANCHOR)
            .collect();

        assert_eq!(
            plain(src),
            without_anchors.join("\n"),
            "to_plain_text() and the document's addressable text disagree about more than \
             anchors, for {src:?} — the only sanctioned difference is the object anchors"
        );
    }
}

// ---------------------------------------------------------------------------
// The indented export — the one sanctioned way to differ from the addressable view
// ---------------------------------------------------------------------------

fn plain_indented(djot: &str) -> String {
    let doc = TextDocument::new();
    doc.set_djot(djot).unwrap().wait().unwrap();
    doc.to_plain_text_indented().unwrap()
}

/// `.txt` has no markup to say "this is set-off matter", so the export indents quoted
/// blocks instead — four spaces per level. This is what makes an epigraph still read as a
/// quotation in a plain-text manuscript rather than dissolving into the body.
#[test]
fn the_indented_export_sets_quoted_blocks_in() {
    assert_eq!(plain_indented("> a0\n\na"), "    a0\na");
    assert_eq!(
        plain_indented("p1\n\n> q1\n\np2"),
        "p1\n    q1\np2",
        "only the quoted block moves; the surrounding prose stays flush"
    );
}

/// Depth is per enclosing blockquote, so a quote inside a quote is indented twice.
#[test]
fn the_indented_export_nests() {
    assert_eq!(plain_indented("> > deep"), "        deep");
}

/// The whole reason indentation is a separate method: `to_plain_text()` is pinned to the
/// document's addressable text, so it must stay flush no matter what the indented one does.
/// If these two ever agree on a quoted document, the offsets `find_all`/`replace_text`
/// hand out have silently moved.
#[test]
fn the_plain_export_stays_flush_so_offsets_survive() {
    for src in ["> a0\n\na", "p1\n\n> q1\n\np2\n\n> q2", "> > deep"] {
        let flush = plain(src);
        assert!(
            !flush.lines().any(|l| l.starts_with(' ')),
            "to_plain_text() must not indent {src:?}, it is the addressable view: {flush:?}"
        );
        assert_ne!(
            flush,
            plain_indented(src),
            "the indented export must actually differ for {src:?}, or it is doing nothing"
        );
    }
}

/// An epigraph as Skribisto writes one: quotation, blank line, attribution — all inside one
/// blockquote, so all of it indents together and the attribution cannot drift out of the
/// quote it belongs to.
#[test]
fn an_epigraph_indents_as_one_unit_attribution_included() {
    assert_eq!(
        plain_indented("> The sea is a going.\n>\n> — Anon.\n\nThe first paragraph."),
        "    The sea is a going.\n    — Anon.\nThe first paragraph."
    );
}

// ---------------------------------------------------------------------------
// The semantic role — what lets a format say "epigraph" rather than "quotation"
// ---------------------------------------------------------------------------

const EPIGRAPH_DJOT: &str = "> {semantic_role=epigraph}\n> The sea is a going.\n>\n> {alignment=right}\n> — Anon.";

fn doc_of(djot: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot(djot).unwrap().wait().unwrap();
    doc
}

/// The role survives djot → model → djot. Without this the marker is write-only and a
/// project saved and reloaded loses every epigraph's semantics.
#[test]
fn the_semantic_role_round_trips_through_djot() {
    let out = doc_of(EPIGRAPH_DJOT).to_djot().unwrap();
    assert!(
        out.contains("semantic_role=epigraph"),
        "the role must be written back: {out}"
    );
}

/// HTML and EPUB carry both the EPUB Structural Semantics type and the DPUB-ARIA role.
/// Both are required: `epub:type` alone reaches no assistive technology, which is the
/// entire reason for marking it.
#[test]
fn an_epigraph_is_marked_up_semantically_in_html() {
    let html = doc_of(EPIGRAPH_DJOT).to_html().unwrap();
    assert!(html.contains(r#"epub:type="epigraph""#), "{html}");
    assert!(html.contains(r#"role="doc-epigraph""#), "{html}");
}

/// An ordinary quotation must NOT be claimed as an epigraph — the marker has to mean
/// something, and a writer's quoted letter is not front matter.
#[test]
fn a_plain_quotation_is_not_marked_as_an_epigraph() {
    let html = doc_of("> Just a quotation.").to_html().unwrap();
    assert!(html.contains("<blockquote>"), "still a blockquote: {html}");
    assert!(!html.contains("epigraph"), "but not an epigraph: {html}");
}

/// An unknown role from a future version degrades to a plain blockquote rather than
/// failing the parse — the same way an unknown alignment value does.
#[test]
fn an_unknown_semantic_role_degrades_to_a_plain_quotation() {
    let html = doc_of("> {semantic_role=colophon}\n> From the future.")
        .to_html()
        .unwrap();
    assert!(html.contains("<blockquote>"), "{html}");
    assert!(!html.contains("colophon"), "{html}");
}
