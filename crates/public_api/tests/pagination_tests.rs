//! One document, every writer: what `Block::fmt_page_break_before` turns into.
//!
//! A page break is the one piece of formatting whose whole point is that it reaches the
//! *paper*, so it is worth asserting per format rather than trusting that adding a field
//! to the model was enough. Each format expresses it differently, and two of them
//! deliberately express it as nothing at all unless asked.

use text_document::{
    BlockFormat, MarkdownExportOptions, MoveMode, PlainTextExportOptions, TextDocument,
};

/// A two-chapter document whose second chapter opens a page.
const TWO_CHAPTERS: &str = "\
# Chapter One

The first chapter.

{page_break_before=true}
# Chapter Two

The second chapter.";

fn doc() -> TextDocument {
    let d = TextDocument::new();
    d.set_djot_sync(TWO_CHAPTERS).expect("set_djot");
    d
}

// ── the paginated formats ───────────────────────────────────────────────────

#[test]
fn html_emits_both_css_spellings() {
    let html = doc().to_html().expect("to_html");
    // CSS3 and its CSS2 predecessor: reading systems and print engines are split
    // between them, so neither alone reaches everything.
    assert!(html.contains("break-before: page"), "{html}");
    assert!(html.contains("page-break-before: always"), "{html}");
}

#[test]
fn the_break_lands_on_the_second_chapter_and_not_the_first() {
    let html = doc().to_html().expect("to_html");
    let one = html.find("Chapter One").expect("chapter one");
    let two = html.find("Chapter Two").expect("chapter two");
    let brk = html.find("break-before: page").expect("a break");
    assert!(
        brk > one && brk < two,
        "the break must sit between the two chapters, not before both:\n{html}"
    );
}

#[test]
fn latex_emits_a_newpage() {
    let tex = doc().to_latex("article", true).expect("to_latex");
    assert!(tex.contains("\\newpage"), "{tex}");
    let brk = tex.find("\\newpage").expect("a break");
    let two = tex.find("Chapter Two").expect("chapter two");
    assert!(brk < two, "the break must precede its chapter:\n{tex}");
}

#[test]
fn djot_round_trips_the_flag() {
    let dj = doc().to_djot().expect("to_djot");
    assert!(dj.contains("page_break_before=true"), "{dj}");
}

// ── the flowing formats: nothing, unless asked ──────────────────────────────

#[test]
fn plain_text_is_unchanged_by_default() {
    // `to_plain_text` is pinned to the document's addressable text — the text search
    // computes offsets against — so a form feed here would silently move every offset
    // after it.
    let txt = doc().to_plain_text().expect("to_plain_text");
    assert!(!txt.contains('\u{000C}'), "{txt:?}");
}

#[test]
fn plain_text_emits_a_form_feed_when_asked() {
    let txt = doc()
        .to_plain_text_with(PlainTextExportOptions::presentation())
        .expect("to_plain_text_with");
    let brk = txt.find('\u{000C}').expect("a form feed");
    let two = txt.find("Chapter Two").expect("chapter two");
    assert!(brk < two, "the form feed must lead its block:\n{txt:?}");
}

/// The opt-in also has to leave the *rest* of the text alone — the same words in the
/// same order, one control character richer.
#[test]
fn the_form_feed_is_the_only_difference() {
    let d = doc();
    let plain = d.to_plain_text().expect("plain");
    let paged = d
        .to_plain_text_with(PlainTextExportOptions {
            quote_indent: false,
            page_breaks: true,
        })
        .expect("paged");
    assert_eq!(paged.replace('\u{000C}', ""), plain);
}

#[test]
fn markdown_is_clean_by_default() {
    let md = doc().to_markdown().expect("to_markdown");
    assert!(
        !md.contains("<div"),
        "raw HTML has no business in a Markdown export nobody asked to paginate:\n{md}"
    );
}

#[test]
fn markdown_emits_a_raw_html_break_when_asked() {
    let md = doc()
        .to_markdown_with(MarkdownExportOptions { page_breaks: true })
        .expect("to_markdown_with");
    assert!(md.contains("break-before: page"), "{md}");
    let brk = md.find("<div").expect("a break");
    let two = md.find("Chapter Two").expect("chapter two");
    assert!(brk < two, "the break must precede its chapter:\n{md}");
}

/// Djot block attributes are only read for standalone paragraphs and headings — a list
/// item normalises its styling away, and this flag inherits that boundary rather than
/// carving an exception in it.
#[test]
fn a_list_item_normalises_the_attribute_away_like_every_other() {
    let d = TextDocument::new();
    d.set_djot_sync("- one\n- two\n\n{page_break_before=true}\n- three")
        .expect("set_djot");
    assert!(!d.to_djot().expect("to_djot").contains("page_break_before"));
}

/// Reached through the formatting API instead, which has no such boundary. A list run is
/// collapsed into ONE construct per format, so a break inside one has to end the run —
/// otherwise it is silently dropped into a gap that does not exist.
#[test]
fn a_break_inside_a_list_ends_the_run_rather_than_vanishing() {
    let d = TextDocument::new();
    d.set_djot_sync("- one\n- two\n- three").expect("set_djot");
    let c = d.cursor();
    // Into the third item: "one\ntwo\nthree" — past both preceding items.
    c.set_position(9, MoveMode::MoveAnchor);
    c.set_block_format(&BlockFormat {
        page_break_before: Some(true),
        ..Default::default()
    })
    .expect("set_block_format");

    let tex = d.to_latex("article", true).expect("to_latex");
    let brk = tex
        .find("\\newpage")
        .unwrap_or_else(|| panic!("no break in:\n{tex}"));
    let three = tex.find("three").expect("third item");
    assert!(brk < three, "{tex}");
    // …and the run really did end: two lists, not one with a stray command inside.
    assert_eq!(
        tex.matches("\\begin{itemize}").count(),
        2,
        "the run must be split in two:\n{tex}"
    );
}

// ── the two gaps the LaTeX writer used to have ──────────────────────────────

/// Alignment was never emitted at all, so a centred scene-break glyph — or a centred
/// title page — arrived flush left in every LaTeX export.
#[test]
fn latex_centres_a_centred_block() {
    let d = TextDocument::new();
    d.set_djot_sync("{alignment=center}\n#").expect("set_djot");
    let tex = d.to_latex("article", true).expect("to_latex");
    assert!(tex.contains("\\begin{center}"), "{tex}");
    assert!(tex.contains("\\end{center}"), "{tex}");
}

#[test]
fn latex_right_aligns_a_right_aligned_block() {
    let d = TextDocument::new();
    d.set_djot_sync("{alignment=right}\nTo the right.")
        .expect("set_djot");
    let tex = d.to_latex("article", true).expect("to_latex");
    assert!(tex.contains("\\begin{flushright}"), "{tex}");
}

/// Left and Justify are LaTeX's own defaults, so wrapping them would add an environment
/// that changes nothing but the output's readability.
#[test]
fn latex_leaves_the_default_alignments_unwrapped() {
    let d = TextDocument::new();
    d.set_djot_sync("{alignment=justify}\nOrdinary prose.")
        .expect("set_djot");
    let tex = d.to_latex("article", true).expect("to_latex");
    assert!(!tex.contains("\\begin{center}"), "{tex}");
    assert!(!tex.contains("\\begin{flushright}"), "{tex}");
}

/// Every other writer names an epigraph; LaTeX rendered it as an ordinary quotation.
/// The quotation is italic and the attribution is not — decided, as everywhere else, by
/// the right alignment the author already gave the source line.
#[test]
fn latex_sets_an_epigraph_apart_from_a_plain_quotation() {
    let d = TextDocument::new();
    d.set_djot_sync(
        "> {semantic_role=epigraph}\n> All happy families.\n>\n> {alignment=right}\n> Tolstoy",
    )
    .expect("set_djot");
    let tex = d.to_latex("article", true).expect("to_latex");
    assert!(tex.contains("\\begin{quote}"), "{tex}");
    assert!(
        tex.contains("\\itshape"),
        "the quotation itself must be italic:\n{tex}"
    );
    let italic = tex.find("\\itshape").expect("italics");
    let attribution = tex.find("Tolstoy").expect("attribution");
    let flush = tex
        .find("\\begin{flushright}")
        .expect("right-aligned source");
    assert!(
        italic < flush && flush < attribution,
        "the attribution is right-aligned and outside the italics:\n{tex}"
    );
}

#[test]
fn a_plain_quotation_is_not_italicised() {
    let d = TextDocument::new();
    d.set_djot_sync("> Just a quotation.").expect("set_djot");
    let tex = d.to_latex("article", true).expect("to_latex");
    assert!(tex.contains("\\begin{quote}"), "{tex}");
    assert!(!tex.contains("\\itshape"), "{tex}");
}
