//! PDF compilation via an embedded Typst compiler (`typst-as-lib` + `typst-pdf`).
//!
//! This module owns the ONLY place in `text-document` that touches the `typst`/`typst-pdf`
//! crates — [`crate::typst_markup`] emits plain Typst markup text with no knowledge of how (or
//! whether) it gets compiled, and [`crate::use_cases::export_pdf_uc`] is the only caller of
//! [`compile_typst_pdf`]. This keeps the (sizeable, pre-1.0) Typst dependency graph confined to
//! one module, entirely behind the `pdf` cargo feature.
//!
//! No filesystem/network access ever happens here: `markup` is compiled as a **detached** main
//! source file (no virtual path), and fonts are supplied purely as in-memory blobs — no
//! `typst-kit` system font scanning, no package resolution.

use anyhow::{Context, anyhow, bail};
use typst::diag::{SourceDiagnostic, Warned};
use typst::ecow::EcoVec;
use typst_as_lib::TypstEngine;
use typst_layout::PagedDocument;
use typst_pdf::PdfOptions;

/// Compile `markup` (a complete Typst source, used directly as the main/detached file) to PDF
/// bytes, using only the font blobs in `fonts` — no system or `typst-kit` font search.
///
/// Returns a human-readable, possibly multi-line error on:
/// - an empty `fonts` list (at least one embedded font is required to produce readable output);
/// - a font blob that cannot be parsed as a font at all (see the pre-validation note below);
/// - bad markup (a Typst compile error — unknown variable/function, syntax error, …);
/// - a `typst_pdf::pdf` failure (rare — e.g. a requested PDF/A-standard conformance violation).
///
/// A **missing font family** named in the markup's own `#set text(font: ..)` is not one of
/// these failure modes: Typst treats that as a non-fatal warning (the PDF still compiles, with
/// fallback/tofu glyphs for the affected runs) and this function logs it via `eprintln!` rather
/// than rejecting the export — rejecting would mean one unresolvable font family in a
/// multi-language manuscript aborts the entire export, which is a worse failure mode than a
/// visually-broken run of text the caller can notice and fix.
pub fn compile_typst_pdf(markup: &str, fonts: Vec<Vec<u8>>) -> anyhow::Result<Vec<u8>> {
    if fonts.is_empty() {
        bail!("no fonts supplied: at least one embedded font is required");
    }

    // `TypstEngine::fonts()` accepts anything implementing `IntoFonts` (`&[u8]`/`Vec<u8>`/
    // `Bytes`/`Font`); internally each blob goes through `typst::text::Font::iter`, which
    // SILENTLY DROPS a blob it can't parse (an empty sub-iterator, no error signal at all — a
    // font collection file can yield several `Font`s, a corrupt/non-font blob yields zero). Left
    // unchecked, a bad font blob would simply vanish from the resulting PDF with no diagnostic
    // anywhere — `Result::Ok` and missing glyphs. Pre-validate ourselves so that failure mode
    // becomes a clear, attributable error instead.
    for (i, bytes) in fonts.iter().enumerate() {
        let n = typst::text::Font::iter(typst::foundations::Bytes::new(bytes.clone())).count();
        if n == 0 {
            bail!(
                "font #{i} ({} bytes) could not be parsed as a font (corrupt or unsupported format)",
                bytes.len()
            );
        }
    }

    let engine = TypstEngine::builder()
        .main_file(markup.to_string())
        .fonts(fonts)
        .build();

    let Warned { output, warnings } = engine.compile::<PagedDocument>();

    let doc = output.map_err(|err| anyhow!(render_compile_error(err, &warnings)))?;

    if !warnings.is_empty() {
        // Non-fatal (typically "unknown font family: X") — the compile still succeeded and the
        // PDF was produced, but the affected text may render with fallback/blank glyphs. See the
        // doc comment above for why this is logged rather than escalated to a hard error.
        eprintln!("typst compile warnings:\n{}", render_diagnostics(&warnings));
    }

    let options = PdfOptions::default();

    typst_pdf::pdf(&doc, &options)
        .map_err(|diags| anyhow!(render_diagnostics(&diags)))
        .context("typst_pdf::pdf failed")
}

/// Render a `.compile()` failure into a readable multi-line string, appending any warnings that
/// preceded the fatal error (a warning can be a clue to why the error happened, e.g. "unknown
/// font family" right before an unrelated-looking layout error).
fn render_compile_error(err: typst_as_lib::TypstAsLibError, warnings: &EcoVec<SourceDiagnostic>) -> String {
    use typst_as_lib::TypstAsLibError as E;
    match err {
        // The actual Typst compile diagnostics (bad markup): parse errors, unknown
        // variables/functions, type errors, and so on.
        E::TypstSource(diags) => {
            let mut s = render_diagnostics(&diags);
            if !warnings.is_empty() {
                s.push_str("\n--- warnings ---\n");
                s.push_str(&render_diagnostics(warnings));
            }
            s
        }
        // TypstFile (a referenced/imported file couldn't be resolved), MainSourceFileDoesNotExist,
        // HintedString, Unspecified — none reachable from a detached in-memory main file with no
        // imports, but rendered via `Display` regardless so a future change surfaces readably.
        other => format!("{other}"),
    }
}

/// Render a list of Typst diagnostics as one message per line, each followed by its hints
/// (indented). No line/column resolution is attempted — `SourceDiagnostic::span` needs the
/// originating `Source`/`World` to resolve into a human line:col, which `typst-as-lib` does not
/// do for the caller; the bare message text (e.g. "unknown variable: foo") is self-explanatory
/// enough for an export-time error without it.
fn render_diagnostics(diags: &EcoVec<SourceDiagnostic>) -> String {
    diags
        .iter()
        .map(|d| {
            let mut s = format!("{:?}: {}", d.severity, d.message);
            for hint in &d.hints {
                s.push_str(&format!("\n  hint: {}", hint.v));
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FONT: &[u8] = include_bytes!("../tests/assets/DejaVuSerif.ttf");

    #[test]
    fn compiles_trivial_markup_to_a_pdf() {
        let markup = "#set page(width: 10cm, height: 10cm)\nHello, world.";
        let pdf = compile_typst_pdf(markup, vec![TEST_FONT.to_vec()]).expect("compiles");
        assert!(pdf.starts_with(b"%PDF-"), "output must be a PDF");
        assert!(pdf.len() > 100, "a real PDF is not a handful of bytes");
    }

    #[test]
    fn empty_font_list_is_rejected() {
        let err = compile_typst_pdf("Hello.", vec![]).unwrap_err();
        assert!(err.to_string().contains("no fonts supplied"));
    }

    #[test]
    fn corrupt_font_bytes_are_rejected_not_silently_dropped() {
        let err = compile_typst_pdf("Hello.", vec![vec![0u8; 16]]).unwrap_err();
        assert!(
            err.to_string().contains("could not be parsed as a font"),
            "got: {err}"
        );
    }

    #[test]
    fn bad_markup_reports_a_readable_diagnostic() {
        let err =
            compile_typst_pdf("#foo_bar_does_not_exist()", vec![TEST_FONT.to_vec()]).unwrap_err();
        assert!(
            err.to_string().contains("unknown variable"),
            "got: {err}"
        );
    }

    #[test]
    fn missing_font_family_is_a_warning_not_an_error() {
        // Typst treats an unresolvable `#set text(font: ..)` family as non-fatal: the PDF still
        // compiles (with fallback glyphs), which is the behaviour `compile_typst_pdf` relies on
        // rather than escalating to a hard error.
        let markup = "#set text(font: \"Totally Not A Real Font\")\nHello.";
        let pdf = compile_typst_pdf(markup, vec![TEST_FONT.to_vec()]).expect("still compiles");
        assert!(pdf.starts_with(b"%PDF-"));
    }
}
