// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Preamble + image policy for LaTeX export.
//!
//! LaTeX was, until M-T3, the last export format in this crate still taking its knobs as bare
//! positional arguments (`to_latex(document_class, include_preamble)` /
//! `to_latex_with_options(document_class, include_preamble, omit_images)`) instead of an options
//! struct — every sibling already had one: `DjotExportOptions`, `DocxExportOptions`,
//! `EpubExportOptions`, `HtmlExportOptions`, `MarkdownExportOptions`, `OdtExportOptions`,
//! `PdfExportOptions`, `PlainTextExportOptions`. [`LatexExportOptions`] closes that gap: the same
//! three knobs, the same defaults, now named fields on a struct so a fourth knob never means
//! renumbering every call site's positional argument list.
//!
//! Unlike [`super::docx_options::DocxExportOptions`] or [`super::odt_options::OdtExportOptions`],
//! **LaTeX carries no comment support, and this struct never will**: there is no LaTeX importer
//! anywhere in this crate (or planned — a `.tex` file is an arbitrary macro-expansion target, not
//! a document format this crate can read back), so an anchored comment thread round-tripped into
//! LaTeX would be structurally one-way, editorial notes going in and never coming back out. That
//! was a deliberate scope cut in the comment-export feature, not an oversight here — do not add a
//! `comments` field to this struct.
//!
//! LaTeX also has no page geometry or base-typography knobs the way DOCX/ODT do:
//! `\documentclass` and its own options already own page size and base font (`article`,
//! `report`, `book`, or a caller's own class), so there is nothing here to mirror
//! `DocxExportOptions`' twips/half-points fields — a caller who wants those characteristics picks
//! a document class that has them, or supplies its own preamble around the body this crate
//! returns when [`include_preamble`](LatexExportOptions::include_preamble) is `false`.

use serde::{Deserialize, Serialize};

/// The three export-time choices `to_latex`/`to_latex_with_options` have always taken: which
/// `\documentclass` to open with, whether to wrap the rendered body in a full compilable
/// document at all, and whether inline images are emitted or dropped.
///
/// [`Default`] reproduces exactly what bare `to_latex(document_class, include_preamble)` always
/// did: an empty [`document_class`](Self::document_class) falls back to `"article"` inside the
/// writer (`document_io::use_cases::export_latex_uc::ExportLatexUseCase::execute`), and
/// [`omit_images`](Self::omit_images) is `false` — images are emitted as
/// `\includegraphics{src}` unless a caller opts out.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LatexExportOptions {
    /// The `\documentclass{…}` to open the document with, used only when
    /// [`include_preamble`](Self::include_preamble) is set. Empty ⇒ `"article"` — the writer's
    /// fallback, not this struct's: an empty string round-trips through serde the same way
    /// `None` would on an `Option<String>` field, without making this the one options struct in
    /// the crate that special-cases an empty string as an error.
    ///
    /// Ignored when `include_preamble` is `false`: a body-only fragment has no `\documentclass`
    /// line to open.
    pub document_class: String,
    /// Wrap the rendered body in `\documentclass{…} … \begin{document} … \end{document}`, plus
    /// the small fixed preamble the writer needs for hyperlinks, strikeout, images, and line
    /// spacing, with `secnumdepth` forced to `-1` so LaTeX's own section counters never print
    /// beside a heading this crate already numbered (see `export_latex_uc`'s own doc comment for
    /// the full package list and reasoning). `false` returns just the body — for a caller
    /// embedding the result inside a larger LaTeX document that owns its own preamble and
    /// section-numbering policy.
    pub include_preamble: bool,
    /// Drop inline images instead of emitting `\includegraphics{…}`.
    ///
    /// LaTeX resolves a graphic against the filesystem when the document is compiled, so a
    /// caller that will not place the files beside the `.tex` is choosing between a build error
    /// and a missing picture. `false` (the historical default) still emits the reference — this
    /// crate writes no image files itself, for LaTeX or any other format, so nothing here ever
    /// changes that; it only changes whether the reference is written at all.
    #[serde(default)]
    pub omit_images: bool,
}
