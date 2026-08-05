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
