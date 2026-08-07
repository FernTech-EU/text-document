//! Footnotes survive the Markdown import path — reference, definition, and
//! neither requiring the other.
//!
//! Modelled on `footnote_roundtrip_tests.rs` (the Djot equivalent): `parse_markdown`
//! used to have no footnote handling at all — `[^label]` and `[^label]: body` both
//! fell through to plain text — so this file exercises the same shape of guarantee
//! through `TextDocument::set_markdown` instead of `set_djot`.
//!
//! The load-bearing case is the **dangling** reference. A host that owns note
//! bodies itself — Skribisto keeps them in its own store, so it can search, undo
//! and save them — puts `[^label]` in the prose and no definition anywhere. That
//! is not a degenerate input to tolerate; it is the normal state, and if it did
//! not round-trip the writer's references would vanish on the next save.

use text_document::{PlainTextExportOptions, TextDocument};

fn doc_from(markdown: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_markdown(markdown)
        .expect("import")
        .wait()
        .expect("import");
    doc
}

/// A reference with no definition anywhere survives unchanged.
///
/// pulldown-cmark parses `[^label]` purely syntactically — per its own docs,
/// "Definitions and references to them may occur in any order" and a reference
/// needs no matching definition at all — so the obligation is entirely on the
/// model, exactly as for the Djot importer.
#[test]
fn a_reference_with_no_definition_survives() {
    let doc = doc_from("Text with a note[^solo] in it.\n");
    let out = doc.to_markdown().expect("export");
    assert!(
        out.contains("[^solo]"),
        "the reference must survive with no definition present, got {out:?}"
    );
}

/// Reference plus definition, both preserved.
#[test]
fn a_reference_and_its_definition_both_survive() {
    let doc = doc_from("Prose[^n1] here.\n\n[^n1]: The note body.\n");
    let out = doc.to_markdown().expect("export");
    assert!(out.contains("[^n1]"), "reference lost: {out:?}");
    assert!(out.contains("[^n1]:"), "definition lost: {out:?}");
    assert!(out.contains("The note body"), "note body lost: {out:?}");
}

/// Export is a fixpoint: a second import/export pass changes nothing.
#[test]
fn the_footnote_round_trip_is_a_fixpoint() {
    for seed in [
        "A note[^a] here.\n",
        "Prose[^a] and more[^b].\n\n[^a]: First.\n\n[^b]: Second.\n",
        "Before[^only] after.\n\n[^only]: Body.\n",
    ] {
        let once = doc_from(seed).to_markdown().expect("export");
        let twice = doc_from(&once).to_markdown().expect("re-export");
        assert_eq!(once, twice, "not a fixpoint for {seed:?}");
    }
}

/// A definition that textually precedes its reference still resolves — GFM
/// footnotes, like Djot's, place no ordering requirement on the source.
#[test]
fn a_definition_before_its_reference_still_resolves() {
    let doc = doc_from("[^n1]: The note body.\n\nProse[^n1] here.\n");
    let out = doc.to_markdown().expect("export");
    assert!(out.contains("[^n1]"), "reference lost: {out:?}");
    assert!(out.contains("The note body"), "note body lost: {out:?}");
}

/// A multi-paragraph note body — the GFM continuation-line shape the
/// Markdown exporter itself writes (four-space indent) — survives whole,
/// exercising the importer's block-collecting loop beyond a single block.
#[test]
fn a_multi_paragraph_definition_survives() {
    let doc = doc_from("Prose[^n1] here.\n\n[^n1]: First paragraph.\n\n    Second paragraph.\n");
    let out = doc.to_markdown().expect("export");
    assert!(
        out.contains("First paragraph"),
        "first paragraph of the note lost: {out:?}"
    );
    assert!(
        out.contains("Second paragraph"),
        "second paragraph of the note lost: {out:?}"
    );
}

/// A reference costs exactly one character of the document.
///
/// The marker a reader sees is generated at render time and is not in the
/// text, so however wide it prints, the document holds one `U+FFFC`. Every
/// offset past it — a search hit, a comment's anchor — depends on this being
/// exact.
#[test]
fn a_reference_costs_exactly_one_character() {
    let without = doc_from("ab cd\n");
    let with = doc_from("ab[^n] cd\n");
    assert_eq!(
        with.character_count() - without.character_count(),
        1,
        "a reference must cost one character, no more and no less"
    );
}

/// The generic addressable plain-text view agrees with the document about
/// length, whichever importer built the document. This is the same
/// off-by-one class of bug the `.txt` view had for images.
#[test]
fn the_addressable_view_counts_the_reference_too() {
    let doc = doc_from("ab[^n] cd\n\n[^n]: A note.\n");
    let addressable = doc
        .to_plain_text_with(PlainTextExportOptions::addressable())
        .expect("addressable");
    assert_eq!(
        addressable.chars().count(),
        doc.character_count(),
        "the addressable view and the document disagree about length: \
         {addressable:?} vs {} chars",
        doc.character_count()
    );
}

/// A reference carries `SuperScript` formatting whether it arrived via
/// Markdown or Djot — `format_runs_from_spans` sets this on the anchor itself
/// so every consumer (editor, exporters) agrees, and the Markdown importer
/// must feed it the same `footnote_ref` span the Djot importer does.
#[test]
fn a_parsed_reference_is_superscript() {
    use text_document::CharVerticalAlignment::SuperScript;

    let doc = doc_from("Prose[^n1] here.\n");
    let raised = doc
        .flow()
        .iter()
        .filter_map(|e| match e {
            text_document::FlowElement::Block(b) => Some(b),
            _ => None,
        })
        .flat_map(|b| b.fragments())
        .find_map(|f| match f {
            text_document::FragmentContent::FootnoteReference { format, .. } => {
                Some(format.vertical_alignment == Some(SuperScript))
            }
            _ => None,
        });
    assert_eq!(
        raised,
        Some(true),
        "a Markdown-parsed reference must be superscript"
    );
}

/// The seam a host uses to tie its own note storage to the prose: where the
/// references are, and which note each names. Generic on the document model,
/// so it must work the same after a Markdown import as after a Djot one.
#[test]
fn references_are_reportable_by_position_and_label() {
    let doc = doc_from("One[^a] two[^b] three.\n");
    let refs = doc.footnote_references();

    assert_eq!(
        refs.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>(),
        vec!["a", "b"],
        "references must come back in reading order"
    );

    let (pos_a, _) = refs[0];
    assert_eq!(doc.footnote_reference_at(pos_a).as_deref(), Some("a"));
    assert_eq!(doc.footnote_reference_at(pos_a + 1), None);
}

/// Positions are character offsets, not byte offsets — Markdown prose is just
/// as capable of holding multi-byte characters before a reference as Djot's.
#[test]
fn reference_positions_are_characters_not_bytes() {
    let doc = doc_from("café—dash[^a]\n");
    let refs = doc.footnote_references();
    assert_eq!(refs.len(), 1);

    // "café—dash" is 9 characters but 12 bytes.
    assert_eq!(refs[0].0, 9, "the position must be in characters");
    assert_eq!(doc.footnote_reference_at(9).as_deref(), Some("a"));
}

/// A note's body must not also appear as ordinary prose: the definition
/// becomes a **detached** frame (in no frame's `child_order`), so the
/// document's every-frame exporters must skip it at its own writing
/// position and render it only where notes belong.
#[test]
fn a_note_body_is_not_also_rendered_as_prose() {
    let doc = doc_from("Prose[^n] here.\n\n[^n]: UNIQUEBODYTEXT.\n");
    let html = doc.to_html().expect("html");
    assert_eq!(
        html.matches("UNIQUEBODYTEXT").count(),
        1,
        "the note body was rendered more than once: {html}"
    );
}

/// HTML renders the reading-system idiom regardless of which importer built
/// the document: a `noteref` marker linked to a `doc-footnote` aside.
#[test]
fn html_renders_a_noteref_and_its_aside() {
    let doc = doc_from("Prose[^n1] here.\n\n[^n1]: The note body.\n");
    let html = doc.to_html().expect("html");

    assert!(
        html.contains(r#"role="doc-noteref""#),
        "no noteref marker: {html}"
    );
    assert!(
        html.contains(r#"epub:type="footnote""#) && html.contains(r#"role="doc-footnote""#),
        "no footnote aside: {html}"
    );
    assert!(
        html.contains("The note body"),
        "the note's body never rendered: {html}"
    );
}

/// Plain text has no page to put a note at the foot of, so notes become a
/// numbered endnote list — but only in the presentation view. The addressable
/// view must stay character-for-character the document, whichever importer
/// produced it.
#[test]
fn plain_text_lists_notes_only_in_the_presentation_view() {
    let doc = doc_from("Prose[^n1] here.\n\n[^n1]: The note body.\n");

    let presented = doc
        .to_plain_text_with(PlainTextExportOptions::presentation())
        .expect("presentation");
    assert!(
        presented.contains("1. The note body"),
        "no endnote list: {presented:?}"
    );

    let addressable = doc
        .to_plain_text_with(PlainTextExportOptions::addressable())
        .expect("addressable");
    assert_eq!(
        addressable.chars().count(),
        doc.character_count(),
        "the addressable view stopped matching the document: {addressable:?}"
    );
    assert!(
        !addressable.contains("1. The note body"),
        "an endnote list leaked into the addressable view: {addressable:?}"
    );
}
