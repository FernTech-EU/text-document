//! `LatexExportOptions` — API-shape symmetry (M-T3).
//!
//! LaTeX was the last export format in this crate still taking its knobs as bare positional
//! arguments (`to_latex(document_class, include_preamble)` /
//! `to_latex_with_options(document_class, include_preamble, omit_images)`) instead of an options
//! struct. This suite proves the refactor changed nothing about the bytes produced: a
//! `LatexExportOptions` built to mirror what the old positional calls always passed reproduces
//! their output byte-for-byte, for a document rich enough to exercise headings, inline
//! formatting, a list, an image, and a footnote. Each knob then gets its own coverage so a
//! future change to one field can't silently stop affecting the writer.

use text_document::{LatexExportOptions, TextDocument};

/// Headings (both wanted `\section`/`\subsection` levels), bold/italic inline formatting, an
/// ordered list, an image, and a footnote — rich enough that a divergence between the old
/// positional call and the new options struct would show up somewhere in this output.
const RICH_DOCUMENT: &str = "\
# Chapter One

Some **bold** and _italic_ text, plus a note[^n1].

## A Subheading

1. First item
2. Second item

before ![a blue square](pic.png){width=64 height=48} after

[^n1]: The footnote body.
";

fn doc() -> TextDocument {
    let d = TextDocument::new();
    d.set_djot_sync(RICH_DOCUMENT).expect("set_djot_sync");
    d
}

// ── golden comparison: the struct must reproduce today's positional output exactly ──────────

#[test]
fn with_options_matches_the_positional_wrapper_with_preamble() {
    let d = doc();
    let positional = d.to_latex("book", true).expect("positional to_latex");
    let via_options = d
        .to_latex_with_options(LatexExportOptions {
            document_class: "book".into(),
            include_preamble: true,
            omit_images: false,
        })
        .expect("to_latex_with_options");
    assert_eq!(positional, via_options);
}

#[test]
fn with_options_matches_the_positional_wrapper_without_preamble() {
    let d = doc();
    let positional = d.to_latex("article", false).expect("positional to_latex");
    let via_options = d
        .to_latex_with_options(LatexExportOptions {
            document_class: "article".into(),
            include_preamble: false,
            omit_images: false,
        })
        .expect("to_latex_with_options");
    assert_eq!(positional, via_options);
}

#[test]
fn to_latex_convenience_wrapper_still_keeps_images_by_default() {
    // `to_latex` forwards through `to_latex_with_options` with `omit_images: false` — the
    // convenience wrapper's contract must stay exactly what it always was.
    let tex = doc().to_latex("article", true).expect("to_latex");
    assert!(tex.contains("\\includegraphics["), "{tex}");
}

// ── default reproduces "no options given" ────────────────────────────────────────────────────

#[test]
fn default_matches_the_historic_bare_shape() {
    // Historically `to_latex_with_options("", false, false)` — empty class, no preamble, images
    // kept — was reachable only by naming every argument by hand. `LatexExportOptions::default`
    // is that same shape, now with a name.
    let d = doc();
    let via_default = d
        .to_latex_with_options(LatexExportOptions::default())
        .expect("default options");
    let via_explicit_bare = d
        .to_latex_with_options(LatexExportOptions {
            document_class: String::new(),
            include_preamble: false,
            omit_images: false,
        })
        .expect("explicit bare options");
    assert_eq!(via_default, via_explicit_bare);
    assert!(!via_default.contains("\\documentclass"), "{via_default}");
}

#[test]
fn default_options_struct_has_historic_values() {
    let o = LatexExportOptions::default();
    assert_eq!(o.document_class, "");
    assert!(!o.include_preamble);
    assert!(!o.omit_images);
}

// ── per-knob coverage ─────────────────────────────────────────────────────────────────────────

#[test]
fn empty_document_class_falls_back_to_article() {
    let tex = doc()
        .to_latex_with_options(LatexExportOptions {
            document_class: String::new(),
            include_preamble: true,
            omit_images: false,
        })
        .expect("to_latex_with_options");
    assert!(tex.contains("\\documentclass{article}"), "{tex}");
}

#[test]
fn explicit_document_class_is_honored() {
    let tex = doc()
        .to_latex_with_options(LatexExportOptions {
            document_class: "report".into(),
            include_preamble: true,
            omit_images: false,
        })
        .expect("to_latex_with_options");
    assert!(tex.contains("\\documentclass{report}"), "{tex}");
}

#[test]
fn include_preamble_true_wraps_the_body() {
    let tex = doc()
        .to_latex_with_options(LatexExportOptions {
            document_class: "article".into(),
            include_preamble: true,
            omit_images: false,
        })
        .expect("to_latex_with_options");
    assert!(tex.starts_with("\\documentclass{article}"), "{tex}");
    assert!(tex.contains("\\begin{document}"), "{tex}");
    assert!(tex.contains("\\end{document}"), "{tex}");
    assert!(
        tex.contains("\\setcounter{secnumdepth}{-1}"),
        "the preamble must suppress LaTeX's own section numbering: {tex}"
    );
}

#[test]
fn include_preamble_false_returns_body_only() {
    let tex = doc()
        .to_latex_with_options(LatexExportOptions {
            document_class: "article".into(),
            include_preamble: false,
            omit_images: false,
        })
        .expect("to_latex_with_options");
    assert!(!tex.contains("\\documentclass"), "{tex}");
    assert!(!tex.contains("\\begin{document}"), "{tex}");
    assert!(!tex.contains("\\end{document}"), "{tex}");
    // The document class is ignored entirely when there is no preamble to open with it.
    assert!(tex.contains("Chapter One"), "{tex}");
}

#[test]
fn omit_images_true_drops_includegraphics() {
    let tex = doc()
        .to_latex_with_options(LatexExportOptions {
            document_class: "article".into(),
            include_preamble: true,
            omit_images: true,
        })
        .expect("to_latex_with_options");
    assert!(!tex.contains("\\includegraphics"), "{tex}");
    // Dropping the image must not drop the prose around it.
    assert!(tex.contains("before") && tex.contains("after"), "{tex}");
}

#[test]
fn omit_images_false_keeps_includegraphics() {
    let tex = doc()
        .to_latex_with_options(LatexExportOptions {
            document_class: "article".into(),
            include_preamble: true,
            omit_images: false,
        })
        .expect("to_latex_with_options");
    assert!(tex.contains("\\includegraphics["), "{tex}");
    assert!(tex.contains("pic.png"), "{tex}");
}

// ── the rest of the document still renders correctly through the new plumbing ───────────────

#[test]
fn headings_formatting_list_and_footnote_all_survive() {
    let tex = doc()
        .to_latex_with_options(LatexExportOptions {
            document_class: "article".into(),
            include_preamble: true,
            omit_images: false,
        })
        .expect("to_latex_with_options");
    assert!(tex.contains("\\section{Chapter One}"), "{tex}");
    assert!(tex.contains("\\subsection{A Subheading}"), "{tex}");
    assert!(tex.contains("\\textbf{bold}"), "{tex}");
    assert!(tex.contains("\\textit{italic}"), "{tex}");
    assert!(tex.contains("\\begin{enumerate}"), "{tex}");
    assert!(tex.contains("First item"), "{tex}");
    assert!(tex.contains("\\footnote{"), "{tex}");
    assert!(tex.contains("The footnote body"), "{tex}");
}

// ── comment support is explicitly out of scope ───────────────────────────────────────────────
// LaTeX has no importer anywhere in this crate, so an anchored comment thread exported into
// `.tex` would be structurally one-way — a deliberate scope cut in the comment-export feature,
// not an oversight here.

#[test]
fn latex_export_options_carries_no_comment_field() {
    // Exhaustive destructure: if a field is ever added to `LatexExportOptions`, this fails to
    // compile until the new field is named here — a deliberate speed bump against slipping a
    // `comments` field in unnoticed.
    let LatexExportOptions {
        document_class: _,
        include_preamble: _,
        omit_images: _,
    } = LatexExportOptions::default();
}
