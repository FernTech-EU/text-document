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

use text_document::{DjotImportOptions, PlainTextExportOptions, TextDocument, djot_to_plain_text};

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
    assert!(out.contains("[^n1]:"), "definition lost: {out:?}");
    assert!(out.contains("The note body"), "note body lost: {out:?}");
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
    let doc =
        doc_from("First[^b] then second[^a].\n\n[^a]: Defined first.\n\n[^b]: Defined second.\n");
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

const WITH_NOTE: &str = "Prose[^n1] here.\n\n[^n1]: The note body.\n";

/// LaTeX puts the note's text *at the reference* — there is no definition site
/// and no label. That inverts what DOCX and HTML do, which is why the body is
/// rendered before the prose that cites it.
#[test]
fn latex_carries_the_body_at_the_reference() {
    let latex = doc_from(WITH_NOTE)
        .to_latex("article", true)
        .expect("latex");
    assert!(latex.contains("\\footnote{"), "no native footnote: {latex}");
    assert!(
        latex.contains("The note body"),
        "the body never reached the reference: {latex}"
    );
    // The body must not *also* appear as a stray paragraph.
    assert_eq!(
        latex.matches("The note body").count(),
        1,
        "the body was rendered twice: {latex}"
    );
}

/// Markdown's footnote extension uses djot's own shape, so this is the native
/// construct rather than a fallback.
#[test]
fn markdown_emits_a_reference_and_a_definition() {
    let md = doc_from(WITH_NOTE).to_markdown().expect("markdown");
    assert!(md.contains("[^n1]"), "no reference: {md}");
    assert!(md.contains("[^n1]:"), "no definition: {md}");
    assert!(md.contains("The note body"), "no body: {md}");
}

/// Plain text has no page to put a note at the foot of, so notes become a
/// numbered endnote list — but only in the presentation view. The addressable
/// view must stay character-for-character the document.
#[test]
fn plain_text_lists_notes_only_in_the_presentation_view() {
    use text_document::PlainTextExportOptions;
    let doc = doc_from(WITH_NOTE);

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

/// The seam a host uses to tie its own note storage to the prose: where the
/// references are, and which note each names.
#[test]
fn references_are_reportable_by_position_and_label() {
    let doc = doc_from("One[^a] two[^b] three.\n");
    let refs = doc.footnote_references();

    assert_eq!(
        refs.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>(),
        vec!["a", "b"],
        "references must come back in reading order"
    );

    // Positions are document-absolute character offsets, so a caret lands on
    // exactly one of them.
    let (pos_a, _) = refs[0];
    assert_eq!(doc.footnote_reference_at(pos_a).as_deref(), Some("a"));
    assert_eq!(doc.footnote_reference_at(pos_a + 1), None);
}

/// Positions are character offsets, not byte offsets.
///
/// Prose is full of characters that are not one byte — an em-dash, an accent, a
/// curly quote — and the two spaces diverge at the first of them. A host
/// comparing a caret (characters) against a byte offset would put every note
/// after such a character in the wrong place.
#[test]
fn reference_positions_are_characters_not_bytes() {
    let doc = doc_from("café—dash[^a]\n");
    let refs = doc.footnote_references();
    assert_eq!(refs.len(), 1);

    // "café—dash" is 9 characters but 12 bytes.
    assert_eq!(refs[0].0, 9, "the position must be in characters");
    assert_eq!(doc.footnote_reference_at(9).as_deref(), Some("a"));
}

/// Inserting a reference at the caret puts one there, costs one character, and
/// survives the save/reload the editor performs constantly.
#[test]
fn a_reference_can_be_inserted_at_the_caret() {
    let doc = doc_from("Before after.\n");
    let before = doc.character_count();

    let cursor = doc.cursor();
    cursor.set_position(6, text_document::MoveMode::MoveAnchor);
    cursor.insert_footnote_reference("mynote").expect("insert");

    assert_eq!(
        doc.character_count() - before,
        1,
        "an inserted reference must cost exactly one character"
    );
    assert_eq!(
        doc.footnote_reference_at(6).as_deref(),
        Some("mynote"),
        "the reference is not where it was inserted: {:?}",
        doc.footnote_references()
    );

    // And it round-trips, which is the half a dedicated insert path would miss.
    let out = doc.to_djot().expect("export");
    assert!(
        out.contains("[^mynote]"),
        "insertion did not survive: {out:?}"
    );
}

/// The marker a reader sees is what the **host** says it is.
///
/// Which note a reference is depends on how many precede it in the document —
/// and a host that owns note storage knows more still: that this text is chapter
/// five of a book, and where its numbering starts. So the marker is pushed in,
/// and the fragment the editor lays out has to actually use it. It did not: the
/// fragment builder drew `label` and the override was consulted only by the
/// exporters, so the writer's prose showed `fn4` while the exported HTML showed
/// `1`.
#[test]
fn the_host_decides_what_a_marker_prints() {
    let doc = doc_from("Prose[^n1] here.\n");
    let mut markers = std::collections::HashMap::new();
    markers.insert("n1".to_string(), "17".to_string());
    doc.set_footnote_markers(markers);

    let drawn = doc
        .flow()
        .iter()
        .filter_map(|e| match e {
            text_document::FlowElement::Block(b) => Some(b),
            _ => None,
        })
        .flat_map(|b| b.fragments())
        .find_map(|f| match f {
            text_document::FragmentContent::FootnoteReference { marker, label, .. } => {
                Some((label.clone(), marker.clone()))
            }
            _ => None,
        })
        .expect("a reference fragment");
    assert_eq!(drawn.0, "n1", "the label is what identifies the note");
    assert_eq!(drawn.1, "17", "the marker is what the host supplied");
}

/// A reference is superscript however ordinary the prose around it is.
///
/// Both ways in have to agree — parsing `[^label]` off the wire and inserting
/// one at a caret — or a note typed today sits on the baseline while an
/// identical one that survived a save and reload is raised, and the difference
/// is stored in the file.
#[test]
fn a_reference_is_raised_whichever_way_it_arrived() {
    use text_document::CharVerticalAlignment::SuperScript;

    let raised = |doc: &TextDocument| -> Option<bool> {
        doc.flow()
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
            })
    };

    assert_eq!(
        raised(&doc_from("Prose[^n1] here.\n")),
        Some(true),
        "a parsed reference must be superscript"
    );

    let typed = doc_from("Prose here.\n");
    let cursor = typed.cursor();
    cursor.set_position(5, text_document::MoveMode::MoveAnchor);
    cursor
        .insert_footnote_reference("typed")
        .expect("insert a reference");
    assert_eq!(
        raised(&typed),
        Some(true),
        "a reference inserted at the caret must be superscript too"
    );
}

// --- repeat citations: one note, one number, no duplicated body -----------

const REPEAT_NOTE: &str = "First[^n1] and second[^n1] citation.\n\n[^n1]: The note body.\n";

/// `footnotes.rs`'s own invariant ("a label referenced twice keeps one
/// number — it is one note") must hold in the LaTeX writer's own idiom: the
/// first citation defines the footnote, a repeat reuses it via
/// `\footnotemark[\getrefnumber{…}]` rather than opening — and duplicating
/// the body of — a second one.
#[test]
fn latex_repeat_citation_reuses_one_footnote() {
    let latex = doc_from(REPEAT_NOTE)
        .to_latex("article", true)
        .expect("latex");
    assert_eq!(
        latex.matches("The note body").count(),
        1,
        "the body must not be duplicated under a second footnote: {latex}"
    );
    assert_eq!(
        latex.matches("\\footnote{").count(),
        1,
        "only the first citation may define the footnote: {latex}"
    );
    assert!(
        latex.contains("\\footnotemark[\\getrefnumber{"),
        "the repeat citation must reuse the first footnote's number: {latex}"
    );

    // It is not enough that SOME `\label{…}`/`\getrefnumber{…}` pair exists — the repeat
    // citation's `\getrefnumber{…}` must point at the SAME anchor the footnote's own
    // `\label{…}` defines, or the two markers resolve to different (or nonexistent) targets.
    let label_start = latex.find("\\label{").expect("no \\label{...} in output") + "\\label{".len();
    let label_end = label_start
        + latex[label_start..]
            .find('}')
            .expect("unterminated \\label{");
    let label_anchor = &latex[label_start..label_end];

    let getref_start = latex
        .find("\\getrefnumber{")
        .expect("no \\getrefnumber{...} in output")
        + "\\getrefnumber{".len();
    let getref_end = getref_start
        + latex[getref_start..]
            .find('}')
            .expect("unterminated \\getrefnumber{");
    let getref_anchor = &latex[getref_start..getref_end];

    assert_eq!(
        label_anchor, getref_anchor,
        "the repeat citation's \\getrefnumber must reference the SAME anchor the \
         footnote's own \\label defines, not a different or stray one: {latex}"
    );
}

// DOCX's own idiom for the same invariant (only the first citation becomes a
// real `<w:footnoteReference>`, proven against the packaged
// `word/footnotes.xml`) is covered in `docx_export_tests.rs`, the container
// text-document-io owns the `docx_rs` types to inspect it with — this crate
// exposes only the file-writing `to_docx`, not the in-memory builder.

// --- nested citations: refused, not left dangling --------------------------

const NESTED_NOTE: &str = "Prose[^a] here.\n\n[^a]: See also[^b].\n\n[^b]: Extra detail about b.\n";

/// A footnote cited only from inside another note's own body
/// (`Footnotes::is_nested_reference` — see its doc for why this is refused
/// rather than numbered) must never be linked: HTML must not emit an `href`
/// to a `#fn-b` that nothing ever writes, in either the plain-HTML or the
/// (shared-renderer) EPUB writer.
#[test]
fn html_nested_citation_is_not_a_dangling_link() {
    let html = doc_from(NESTED_NOTE).to_html().expect("html");

    // Note "a" is cited from real prose — fully resolved, and unaffected.
    assert!(
        html.contains("id=\"fn-a\"") && html.contains("See also"),
        "the resolved outer note must still render normally: {html}"
    );

    // Note "b" is cited only from inside "a"'s body: no aside is ever built
    // for it (`in_print_order` excludes it), so nothing may link to it.
    assert!(
        !html.contains("href=\"#fn-b\""),
        "a nested citation must not carry a dangling href: {html}"
    );
    assert!(
        !html.contains("id=\"fn-b\""),
        "no aside may exist for a note nothing numbers: {html}"
    );
    assert!(
        !html.contains("Extra detail about b"),
        "a nested note's body must never be emitted anywhere: {html}"
    );
    // The citation itself still shows something, just not a link.
    assert!(
        html.contains("<sup>b</sup>"),
        "the nested citation must still show a visible, traceable marker: {html}"
    );
}

/// The same nested case in Markdown: the reference syntax must not survive
/// without its definition, or a reader/tool sees a dangling `[^b]`.
#[test]
fn markdown_nested_citation_does_not_leave_a_dangling_reference() {
    let md = doc_from(NESTED_NOTE).to_markdown().expect("markdown");

    assert!(
        md.contains("[^a]") && md.contains("[^a]:"),
        "outer note lost: {md}"
    );
    assert!(
        !md.contains("[^b]"),
        "a nested citation must not keep the live [^label] syntax: {md}"
    );
    assert!(
        !md.contains("[^b]:"),
        "no definition may exist for a note nothing numbers: {md}"
    );
    assert!(
        !md.contains("Extra detail about b"),
        "a nested note's body must never be emitted anywhere: {md}"
    );
}

/// An ordinary DANGLING reference (no definition anywhere — the documented,
/// supported "a host owns note storage itself" case) is NOT the nested case
/// above and must be left exactly as before in both formats: still linked in
/// HTML, still live `[^label]` syntax in Markdown.
#[test]
fn a_genuinely_dangling_reference_keeps_its_ordinary_rendering() {
    let doc = doc_from("Text with a note[^solo] in it.\n");

    let html = doc.to_html().expect("html");
    assert!(
        html.contains("href=\"#fn-solo\""),
        "a dangling reference must keep its ordinary href, unlike a nested one: {html}"
    );

    let md = doc.to_markdown().expect("markdown");
    assert!(
        md.contains("[^solo]"),
        "a dangling reference must keep its live syntax, unlike a nested one: {md}"
    );
}

// --- plain text: the citation point must stay visible ----------------------

/// `strip_image_sentinels` used to strip every `U+FFFC` it found, and a
/// footnote reference shares that exact codepoint with an image — so asking
/// only to drop images silently erased citation points too, with nothing
/// left in their place.
#[test]
fn plain_text_presentation_view_shows_the_citation_marker() {
    let presented = doc_from(WITH_NOTE)
        .to_plain_text_with(PlainTextExportOptions::presentation())
        .expect("presentation");
    assert!(
        presented.contains("Prose[1] here."),
        "the citation point must show its printed marker: {presented:?}"
    );
    assert!(
        !presented.contains('\u{FFFC}'),
        "no raw sentinel should remain once its marker was printed: {presented:?}"
    );
}

/// `endnote_footnotes` alone (without `strip_images`) must also print the
/// citation marker — the two options are independent, and the marker's
/// whole point is to match the endnote list this same flag appends.
#[test]
fn endnote_footnotes_alone_still_prints_the_citation_marker() {
    let doc = doc_from(WITH_NOTE);
    let options = PlainTextExportOptions {
        strip_images: false,
        endnote_footnotes: true,
        ..Default::default()
    };
    let out = doc.to_plain_text_with(options).expect("export");
    assert!(
        out.contains("Prose[1] here."),
        "the citation marker must print even without strip_images: {out:?}"
    );
}

/// `strip_images` alone, on a single-frame document with a DANGLING
/// reference (so the rope fast path is eligible — see
/// `rope_flat_text_if_simple`), must not blindly erase the reference's
/// sentinel along with any image's: the fast path shares the same
/// blind-replace helper the slow path used to, and needs the same guard.
#[test]
fn strip_images_alone_does_not_eat_a_dangling_footnote_sentinel_on_the_fast_path() {
    let doc = doc_from("Text with a note[^solo] in it.\n");
    let options = PlainTextExportOptions {
        strip_images: true,
        ..Default::default()
    };
    let out = doc.to_plain_text_with(options).expect("export");
    assert!(
        out.contains('\u{FFFC}'),
        "a dangling reference's sentinel must survive a strip-images-only export: {out:?}"
    );
}

// --- the editor's own numbering fallback (finding 8) ------------------------

/// `TextDocument::set_footnote_markers`'s own doc promises: "Leave it unset
/// and the document numbers its own references in reading order, which is
/// right when the document *is* the whole text." `document_io::Footnotes::
/// marker` already implemented that fallback tier for every exporter;
/// `build_raw_fragments` (the live editor's own fragment builder) skipped
/// straight from "no host override" to the raw label, so a host that never
/// calls `set_footnote_markers` — the documented, supported "unset" case —
/// saw the live view draw raw labels while every export numbered correctly.
#[test]
fn the_editor_numbers_its_own_references_when_no_host_markers_are_set() {
    let doc = doc_from("First[^b] then second[^a].\n\n[^a]: x.\n\n[^b]: y.\n");

    let markers: Vec<(String, String)> = doc
        .flow()
        .iter()
        .filter_map(|e| match e {
            text_document::FlowElement::Block(b) => Some(b),
            _ => None,
        })
        .flat_map(|b| b.fragments())
        .filter_map(|f| match f {
            text_document::FragmentContent::FootnoteReference { label, marker, .. } => {
                Some((label.clone(), marker.clone()))
            }
            _ => None,
        })
        .collect();

    assert_eq!(
        markers,
        vec![
            ("b".to_string(), "1".to_string()),
            ("a".to_string(), "2".to_string()),
        ],
        "the editor must count its own references in reading order rather \
         than drawing raw labels, matching document_io::Footnotes::marker's \
         fallback: {markers:?}"
    );
}
