//! Page geometry + base typography for DOCX export.
//!
//! `TextDocument::to_docx` writes with docx-rs's built-in defaults (US-Letter, the default
//! font, single-spaced, 1" margins). [`DocxExportOptions`] lets a caller override that with a
//! *manuscript* style: page size, margins, body font, line spacing, first-line indent,
//! paragraph spacing, alignment, and an optional page-number header. **Everything is in DOCX
//! units** — twips (1/1440 inch) for lengths, half-points for the font size — so this crate
//! stays free of any point/inch or preset semantics; the caller (e.g. skribisto's compiler)
//! does the conversion.
//!
//! Per-block **RTL is not an option here**: it is read from each block's own `fmt_direction`
//! (set on the model) and emitted as a paragraph-level `<w:bidi/>`. A document that mixes LTR
//! and RTL scenes is therefore handled per paragraph, independently of these options.

use serde::{Deserialize, Serialize};

/// Page geometry + base typography overrides for a DOCX export. Every field is optional; the
/// [`Default`] is "no overrides" — exactly what plain `to_docx` produces.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DocxExportOptions {
    /// Page width in twips (1/1440"). `None` ⇒ docx default. Pair with [`page_height_twips`].
    ///
    /// [`page_height_twips`]: Self::page_height_twips
    pub page_width_twips: Option<u32>,
    /// Page height in twips. `None` ⇒ docx default.
    pub page_height_twips: Option<u32>,
    /// Top page margin in twips. `None` ⇒ docx default for that edge.
    pub margin_top_twips: Option<i32>,
    /// Bottom page margin in twips.
    pub margin_bottom_twips: Option<i32>,
    /// Left page margin in twips.
    pub margin_left_twips: Option<i32>,
    /// Right page margin in twips.
    pub margin_right_twips: Option<i32>,
    /// Base body font family, applied as the document default (ascii + complex-script slots, so
    /// it also covers RTL runs). `None` ⇒ docx default.
    pub font_family: Option<String>,
    /// Base body font size in half-points (24 = 12 pt). `None` ⇒ docx default.
    pub font_half_points: Option<usize>,
    /// Body line spacing in twips (240 = single, 360 = 1.5×, 480 = double), applied per body
    /// paragraph — headings keep their own style's spacing. `None` ⇒ default.
    pub line_spacing_twips: Option<i32>,
    /// First-line indent for body paragraphs, in twips. `None`/`0` ⇒ none.
    pub first_line_indent_twips: Option<i32>,
    /// Space after each body paragraph, in twips (pt × 20). `None`/`0` ⇒ none.
    pub paragraph_spacing_after_twips: Option<i32>,
    /// Justify body text; otherwise it is left-aligned (ragged), or right-aligned in an RTL
    /// block.
    pub justify: bool,
    /// Emit a running header carrying the page number (right-aligned) — the manuscript staple.
    pub page_numbers: bool,
    /// Optional running-header text shown before the page number (e.g. `"Lastname / TITLE"`).
    /// Only used when [`page_numbers`](Self::page_numbers) is set.
    pub running_header: Option<String>,
}

impl DocxExportOptions {
    /// docx-rs's built-in defaults — no manuscript styling (what plain `to_docx` uses).
    pub fn plain() -> Self {
        Self::default()
    }
}
