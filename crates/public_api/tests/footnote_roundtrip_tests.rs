//! Footnotes survive the round trip — reference, definition, and neither
//! requiring the other.
//!
//! Modelled on `image_roundtrip_tests.rs` rather than folded into the shared
//! djot proptest: a reference and its definition are paired by label, and
//! generating well-formed pairs is exactly the structural constraint
//! property-based generation is worst at.
//!
//! The load-bearing case is the **dangling** reference. A host that owns note
//! bodies itself — Skribisto keeps them in its own store, so it can search,
//! undo and save them — puts `[^label]` in the prose and no definition anywhere.
//! That is not a degenerate input to tolerate; it is the normal state, and if it
//! did not round-trip the writer's references would vanish on the next save.

use text_document::{DjotImportOptions, TextDocument, djot_to_plain_text};

fn doc_from(djot: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot(djot).expect("import").wait().expect("import");
    doc
}

/// A reference with no definition anywhere survives unchanged.
///
/// jotdown parses `[^label]` purely syntactically — it never checks that a
/// matching definition exists — so the obligation is entirely on the model.
#[test]
fn a_reference_with_no_definition_survives() {
    let doc = doc_from("Text with a note[^solo] in it.\n");
    let out = doc.to_djot().expect("export");
    assert!(
        out.contains("[^solo]"),
        "the reference must survive with no definition present, got {out:?}"
    );
}

/// Reference plus definition, both preserved.
#[test]
fn a_reference_and_its_definition_both_survive() {
    let doc = doc_from("Prose[^n1] here.\n\n[^n1]: The note body.\n");
    let out = doc.to_djot().expect("export");
    assert!(out.contains("[^n1]"), "reference lost: {out:?}");
    assert!(
        out.contains("[^n1]:"),
        "definition lost: {out:?}"
    );
    assert!(
        out.contains("The note body"),
        "note body lost: {out:?}"
    );
}

/// Export is a fixpoint: a second pass changes nothing.
#[test]
fn the_footnote_round_trip_is_a_fixpoint() {
    for seed in [
        "A note[^a] here.\n",
        "Prose[^a] and more[^b].\n\n[^a]: First.\n\n[^b]: Second.\n",
        "Before[^only] after.\n\n[^only]: Body.\n",
    ] {
        let once = doc_from(seed).to_djot().expect("export");
        let twice = doc_from(&once).to_djot().expect("re-export");
        assert_eq!(once, twice, "not a fixpoint for {seed:?}");
    }
}

/// A reference costs exactly one character of the document.
///
/// The marker a reader sees is generated at render time and is not in the text,
/// so however wide it prints, the document holds one `U+FFFC`. Every offset past
/// it — a search hit, a comment's anchor — depends on this being exact.
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

/// The addressable view agrees with the document about that one character.
///
/// `djot_to_plain_text` promises to be byte-identical to the text the document
/// searches. It builds from parsed spans, and a reference's span carries no
/// prose — so without deliberately contributing the sentinel it would come back
/// one character short, and every offset after a footnote would be wrong. This
/// is the same failure the `.txt` view had for images.
#[test]
fn the_addressable_view_counts_the_reference_too() {
    let djot = "ab[^n] cd\n";
    let addressable = djot_to_plain_text(djot, &DjotImportOptions::default());
    let doc = doc_from(djot);
    assert_eq!(
        addressable.chars().count(),
        doc.character_count(),
        "the addressable view and the document disagree about length: \
         {addressable:?} vs {} chars",
        doc.character_count()
    );
    assert!(
        addressable.contains('\u{FFFC}'),
        "the reference's sentinel must be present: {addressable:?}"
    );
}

/// A reference inside emphasis keeps both, and does not acquire djot's
/// superscript markers around its own syntax.
///
/// A reference carries `SuperScript` formatting — that is what draws the marker
/// raised in an editor — so an exporter that let it fall through the ordinary
/// mark-wrapping cascade would emit `^[^label]^`: superscript markup wrapped
/// around syntax every djot reader already renders raised, which re-parses as a
/// superscript containing a footnote rather than as a footnote.
#[test]
fn a_reference_is_not_wrapped_in_superscript_markers() {
    let out = doc_from("Prose[^n] here.\n").to_djot().expect("export");
    assert!(
        !out.contains("^[^n]^"),
        "the reference was wrapped in superscript markers: {out:?}"
    );
    assert!(out.contains("[^n]"), "reference lost: {out:?}");
}

/// A paragraph that merely *looks* like a definition is not turned into one.
///
/// `[^label]:` at the start of a paragraph is djot's definition syntax. Prose
/// that literally begins that way has to survive as prose — the escaping that
/// protects link-reference definitions must cover this too, and this test is
/// what says whether it does rather than assuming it.
#[test]
fn a_paragraph_that_looks_like_a_definition_stays_prose() {
    let seed = "[^solo]: not a real definition\n";
    let once = doc_from(seed).to_djot().expect("export");
    let twice = doc_from(&once).to_djot().expect("re-export");
    assert_eq!(
        once, twice,
        "a paragraph shaped like a definition must be a fixpoint"
    );
    assert!(
        once.contains("not a real definition"),
        "the prose was consumed as a definition body: {once:?}"
    );
}

/// HTML renders the reading-system idiom: a `noteref` marker linked to a
/// `doc-footnote` aside, numbered in reading order.
///
/// A reflowable book has no page bottom, so this pair *is* the footnote — it is
/// what Apple Books and others turn into a pop-up. Both the `epub:type` and the
/// DPUB-ARIA role, because `epub:type` alone reaches no assistive technology.
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
    // The marker is the derived number, not the stored label.
    assert!(
        html.contains("<sup>1</sup>"),
        "the marker should be the number 1, not the label: {html}"
    );
    assert!(
        !html.contains("<sup>n1</sup>"),
        "the raw label leaked into the marker: {html}"
    );
}

/// Numbering follows the order references are *read*, not the order notes were
/// written. A writer who collects their definitions at the bottom of the file
/// still gets 1, 2, 3 down the page.
#[test]
fn notes_are_numbered_in_reading_order_not_definition_order() {
    let doc = doc_from(
        "First[^b] then second[^a].\n\n[^a]: Defined first.\n\n[^b]: Defined second.\n",
    );
    let html = doc.to_html().expect("html");

    let first_marker = html.find("<sup>1</sup>").expect("a first marker");
    let second_marker = html.find("<sup>2</sup>").expect("a second marker");
    assert!(
        first_marker < second_marker,
        "markers are out of order: {html}"
    );
    // `b` is referenced first, so it is note 1 even though `a` is defined first.
    let b_ref = html.find("fn-b").expect("a reference to b");
    let a_ref = html.find("fn-a").expect("a reference to a");
    assert!(
        b_ref < a_ref,
        "the note referenced first must be numbered first: {html}"
    );
}

/// A note's body must not also appear as ordinary prose.
///
/// Definitions are top-level frames, and every exporter's outer loop walks all
/// of them — so without a skip-set the body renders twice: once inline where the
/// definition was typed, and once as the note.
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
