//! Page geometry + typography + font bytes for PDF export (via embedded Typst).
//!
//! Unlike [`super::docx_options::DocxExportOptions`] (whose `None`s mean "use docx-rs's own
//! built-in defaults"), a Typst document is compiled from markup this crate emits itself, so
//! there is no "plain" PDF the way `to_docx` has docx-rs's defaults — every geometry/typography
//! field here always lands in the generated `#set` preamble. [`Default`] therefore picks
//! concrete, reasonable values (A4, 12pt body text, a modest first-paragraph-less layout)
//! rather than leaving them unset.
//!
//! **Font bytes are supplied by the caller, not looked up here.** `text-document` has no font
//! file access or font-selection policy of its own (see [`font_bytes`](Self::font_bytes)) — the
//! caller (e.g. Skribisto's compiler, which owns the bundled OFL fonts and the writer's chosen
//! family) hands over raw TTF/OTF blobs, which are passed straight to the embedded Typst
//! compiler's in-memory font book. No system/typst-kit font search ever runs.

use serde::{Deserialize, Serialize};

/// Page geometry + typography + embedded font bytes for a PDF export.
///
/// All lengths are in **millimetres** (matching `Preset`'s existing unit in callers such as
/// Skribisto's compiler); they are converted to Typst's `cm`/`pt` length literals at
/// markup-emission time, not here — this struct stays a plain, unit-tagged data record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfExportOptions {
    /// Page width, in millimetres.
    pub page_width_mm: f32,
    /// Page height, in millimetres.
    pub page_height_mm: f32,
    /// Top page margin, in millimetres.
    pub margin_top_mm: f32,
    /// Bottom page margin, in millimetres.
    pub margin_bottom_mm: f32,
    /// Left page margin, in millimetres.
    pub margin_left_mm: f32,
    /// Right page margin, in millimetres.
    pub margin_right_mm: f32,

    /// Body font family name, emitted as `#set text(font: ..)`. Empty ⇒ the emitter omits the
    /// `font:` argument entirely, letting Typst fall back to its own built-in default face.
    pub font_family: String,
    /// Raw TTF/OTF font blobs, handed directly to the embedded Typst compiler's `.fonts(..)`.
    /// At least one entry is required for [`crate::typst_compile::compile_typst_pdf`] to
    /// succeed; this struct itself does not enforce that (`Default` is an empty list).
    pub font_bytes: Vec<Vec<u8>>,
    /// Body font size, in points.
    pub font_size_pt: f32,
    /// Paragraph line spacing (Typst `leading`), in em. Typst's own default is `0.65`.
    pub line_spacing: f32,
    /// First-line paragraph indent, in millimetres. `None` ⇒ no first-line indent.
    pub first_line_indent_mm: Option<f32>,
    /// Extra space after each paragraph, in points. `None` ⇒ Typst's own default spacing.
    pub paragraph_spacing_pt: Option<f32>,
    /// Justify body paragraphs (document-level default; per-block alignment still overrides).
    pub justify: bool,

    /// Document-level base reading direction. Per-block RTL (`fmt_direction`) still wraps its
    /// own content in a local `#text(dir: ..)` override regardless of this value.
    pub base_rtl: bool,
    /// Document-wide primary language, as an ISO 639-1 tag (e.g. `"en"`, `"fr"`), emitted as
    /// `#set text(lang: ..)`. `None` ⇒ the emitter omits the `lang:` argument.
    pub lang: Option<String>,

    /// PDF `/Title` metadata (via `#set document(title: ..)`). `None` ⇒ no title is emitted —
    /// Typst/krilla's own defaults apply (no explicit `/Title`).
    pub title: Option<String>,
    /// PDF `/Author` metadata (via `#set document(author: ..)`). `None` ⇒ no author is emitted.
    pub author: Option<String>,

    /// Emit the `#set page/text/par/smartquote/heading` preamble. Mirrors
    /// `ExportLatexDto.include_preamble` — `false` produces a bare body, e.g. for embedding this
    /// export's markup inside a larger hand-authored Typst document.
    pub include_preamble: bool,
}

impl Default for PdfExportOptions {
    fn default() -> Self {
        Self {
            // A4.
            page_width_mm: 210.0,
            page_height_mm: 297.0,
            margin_top_mm: 25.0,
            margin_bottom_mm: 25.0,
            margin_left_mm: 20.0,
            margin_right_mm: 20.0,

            font_family: String::new(),
            font_bytes: Vec::new(),
            font_size_pt: 12.0,
            line_spacing: 0.65,
            first_line_indent_mm: None,
            paragraph_spacing_pt: None,
            justify: true,

            base_rtl: false,
            lang: None,

            title: None,
            author: None,

            include_preamble: true,
        }
    }
}
