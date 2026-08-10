//! Page geometry + base typography for ODT (OpenDocument Text) export.
//!
//! Mirrors [`super::docx_options::DocxExportOptions`] field-for-field and unit-for-unit: **every
//! length here is still in DOCX-style units** — twips (1/1440 inch) for lengths, half-points for
//! font size — even though ODF's own native vocabulary is centimeters/points with a unit suffix
//! baked into every attribute value. That is a deliberate choice, not an oversight: keeping one
//! shared unit convention across every export writer's *options* struct is what lets a caller
//! (e.g. Skribisto's compiler, which already produces twips for `DocxExportOptions`) hand the
//! same numbers to both writers without a second conversion table of its own. The twips→ODF
//! (`fo:*="…pt"`) conversion happens once, inside the ODT writer itself
//! (`document_io::use_cases::export_odt_uc`), which is the only place that needs to know ODF's
//! own unit spelling.
//!
//! Per-block **RTL is not an option here**, for the same reason as DOCX: it is read from each
//! block's own `fmt_direction` and emitted as a paragraph-level `style:writing-mode="rl-tb"` (the
//! ODF analog of `<w:bidi/>`). A document that mixes LTR and RTL scenes is therefore handled per
//! paragraph, independently of these options.

use crate::entities::Alignment;
use serde::{Deserialize, Serialize};

/// How one heading level's paragraph style is **defined** in the output.
///
/// The ODF analog of [`super::docx_options::DocxHeadingStyle`] — same fields, same reasoning:
/// a `<text:h text:outline-level="N">` carries its level as an explicit attribute (so, unlike
/// OOXML, a reader never has to *guess* the level from a style name), but what that heading
/// **looks like** — size, weight, spacing, whether it starts a new page — still has to be said
/// somewhere, or every heading opens looking like plain body text with a reader's arbitrary
/// built-in substituted in its place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OdtHeadingStyle {
    /// Size in half-points (24 = 12 pt). `None` ⇒ the document's body size.
    pub size_half_points: Option<usize>,
    pub bold: bool,
    pub italic: bool,
    /// Paragraph alignment. `None` ⇒ inherit (left, or right in an RTL paragraph).
    pub alignment: Option<Alignment>,
    /// Space above, in twips (pt × 20).
    pub space_before_twips: Option<i32>,
    /// Space below, in twips.
    pub space_after_twips: Option<i32>,
    /// Keep the heading on the same page as what follows it, so a chapter title can never
    /// be left stranded alone at the foot of a page. Emitted as `fo:keep-with-next="always"`.
    pub keep_with_next: bool,
    /// Start the heading on a new page. This is the *style-level* rule ("every heading at
    /// this level opens a page"); a single block can also ask for it through
    /// `Block::fmt_page_break_before`, and either one is enough.
    pub page_break_before: bool,
}

impl Default for OdtHeadingStyle {
    fn default() -> Self {
        Self {
            size_half_points: None,
            bold: true,
            italic: false,
            alignment: None,
            space_before_twips: None,
            space_after_twips: None,
            keep_with_next: true,
            page_break_before: false,
        }
    }
}

impl OdtHeadingStyle {
    /// The conventional six-level ramp, scaled off `body_half_points` — byte-for-byte the
    /// same numbers [`super::docx_options::DocxHeadingStyle::default_ramp`] produces, so a
    /// document exported to both DOCX and ODT with default options looks the same in either
    /// reader. Deliberately close to what a reader's own built-in headings look like, because
    /// this exists to make the file *say* what it was already silently relying on.
    pub fn default_ramp(body_half_points: usize) -> Vec<Self> {
        // (size multiple, space above in points, space below in points)
        const RAMP: [(f32, f32, f32); 6] = [
            (1.80, 24.0, 12.0),
            (1.50, 18.0, 9.0),
            (1.25, 14.0, 7.0),
            (1.10, 12.0, 6.0),
            (1.00, 12.0, 6.0),
            (1.00, 12.0, 6.0),
        ];
        RAMP.iter()
            .enumerate()
            .map(|(i, &(scale, before_pt, after_pt))| Self {
                size_half_points: Some(((body_half_points as f32 * scale).round() as usize).max(2)),
                bold: true,
                // Level 6 is the one conventionally set apart by slope rather than size,
                // since it is already at body size and cannot get smaller.
                italic: i == 5,
                alignment: None,
                space_before_twips: Some((before_pt * 20.0) as i32),
                space_after_twips: Some((after_pt * 20.0) as i32),
                keep_with_next: true,
                page_break_before: false,
            })
            .collect()
    }
}

/// Page geometry + base typography overrides for an ODT export. Every field is optional; the
/// [`Default`] is "no overrides" — a plain "Standard"-styled document at ODF/LibreOffice's own
/// built-in page defaults (A4-ish, 2cm-ish margins — whatever the reader substitutes for an
/// unstyled `style:page-layout`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OdtExportOptions {
    /// Page width in twips (1/1440"). `None` ⇒ no `style:page-layout-properties` override for
    /// this edge (the reader's own default page size). Pair with [`page_height_twips`].
    ///
    /// [`page_height_twips`]: Self::page_height_twips
    pub page_width_twips: Option<u32>,
    /// Page height in twips. `None` ⇒ reader default.
    pub page_height_twips: Option<u32>,
    /// Top page margin in twips. `None` ⇒ reader default for that edge.
    pub margin_top_twips: Option<i32>,
    /// Bottom page margin in twips.
    pub margin_bottom_twips: Option<i32>,
    /// Left page margin in twips.
    pub margin_left_twips: Option<i32>,
    /// Right page margin in twips.
    pub margin_right_twips: Option<i32>,
    /// Base body font family, applied on the "Standard" paragraph style's text properties (so
    /// every other named/automatic style that descends from it inherits it, complex-script runs
    /// included — ODF has no separate "ascii vs. complex-script" font slot the way OOXML does;
    /// one `style:font-name` covers both). `None` ⇒ reader default.
    pub font_family: Option<String>,
    /// Base body font size in half-points (24 = 12 pt). `None` ⇒ reader default.
    pub font_half_points: Option<usize>,
    /// Body line spacing in twips (240 = single, 360 = 1.5×, 480 = double), applied per body
    /// paragraph as `fo:line-height` — headings keep their own style's spacing. `None` ⇒ default.
    pub line_spacing_twips: Option<i32>,
    /// First-line indent for body paragraphs, in twips. `None`/`0` ⇒ none.
    pub first_line_indent_twips: Option<i32>,
    /// Space after each body paragraph, in twips (pt × 20). `None`/`0` ⇒ none.
    pub paragraph_spacing_after_twips: Option<i32>,
    /// Justify body text; otherwise it is left-aligned (ragged), or right-aligned in an RTL
    /// block.
    pub justify: bool,
    /// Emit a running header carrying the page number (right-aligned) — the manuscript staple.
    /// Written as a `style:master-page`'s `style:header` holding a paragraph with a
    /// `<text:page-number>` field.
    pub page_numbers: bool,
    /// Optional running-header text shown before the page number (e.g. `"Lastname / TITLE"`).
    /// Only used when [`page_numbers`](Self::page_numbers) is set.
    pub running_header: Option<String>,
    /// Definitions for heading levels 1..6, index 0 being level 1. Empty ⇒ the writer falls
    /// back to [`OdtHeadingStyle::default_ramp`] over the body size, because the one thing it
    /// must never do is leave every heading looking like undifferentiated body text.
    #[serde(default)]
    pub heading_styles: Vec<OdtHeadingStyle>,
    /// Bytes for the document's inline images, keyed by their `src`.
    ///
    /// Supplied by the caller for the same reason [`super::docx_options::DocxExportOptions::images`]
    /// is: this crate resolves no paths and reads no files. An image whose `src` is absent here
    /// is exported as its alt text instead of failing the export — a missing picture must not
    /// cost the writer their manuscript.
    #[serde(default)]
    pub images: super::image_options::ExportImages,
    /// Comment threads to anchor into the exported `.odt` as real `office:annotation` ranges —
    /// the ODF analog of [`super::docx_options::DocxExportOptions::comments`]. A thread's
    /// opening note and every reply become their own `office:annotation`/
    /// `office:annotation-end` pair (paired by a generated `office:name`, all sharing the
    /// thread's own character range — see [`super::comment_options::DocumentComment`]'s doc
    /// comment for why a reply carries no range of its own), `loext:resolved` marks a resolved
    /// thread, and `loext:parent-name` threads a reply back to its root — the same
    /// LibreOffice-measured spelling `document_ingest::sources::odt`'s reader already expects
    /// (see `M-T2b`'s own doc comment in `document_io::use_cases::export_odt_uc` for the
    /// measurement this was checked against). Empty ⇒ no comments are written, matching plain
    /// `to_odt`.
    ///
    /// Ranges are in the document's addressable character space — see
    /// [`super::comment_options::DocumentComment`]'s doc comment for what that means and why it
    /// is not the same space `FormatRun` byte offsets live in.
    #[serde(default)]
    pub comments: super::comment_options::DocumentComments,
}

impl OdtExportOptions {
    /// No overrides — what plain `to_odt` uses.
    pub fn plain() -> Self {
        Self::default()
    }

    /// The heading styles to write, resolved: the caller's when it gave any, otherwise the
    /// default ramp scaled off whatever body size this export uses.
    pub fn resolved_heading_styles(&self) -> Vec<OdtHeadingStyle> {
        if self.heading_styles.is_empty() {
            OdtHeadingStyle::default_ramp(self.font_half_points.unwrap_or(24))
        } else {
            self.heading_styles.clone()
        }
    }
}
