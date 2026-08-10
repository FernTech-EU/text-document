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

use crate::entities::Alignment;
use serde::{Deserialize, Serialize};

/// How one `HeadingN` paragraph style is **defined** in the output.
///
/// Paragraphs carrying a heading level reference a style id (`Heading1`…`Heading6`), and a
/// referenced-but-undefined id is not an error in OOXML — the reader silently substitutes its
/// own built-in. That is why an export could ask for a 24 pt centred chapter title and open
/// as whatever Word felt like: nothing in the file ever said what `Heading1` *is*.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocxHeadingStyle {
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
    /// be left stranded alone at the foot of a page.
    pub keep_with_next: bool,
    /// Start the heading on a new page. This is the *style-level* rule ("every heading at
    /// this level opens a page"); a single block can also ask for it through
    /// `Block::fmt_page_break_before`, and either one is enough.
    pub page_break_before: bool,
}

impl Default for DocxHeadingStyle {
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

impl DocxHeadingStyle {
    /// The conventional six-level ramp, scaled off `body_half_points`: each level a little
    /// smaller than the one above, bold, opening with space and never orphaned from its text.
    /// Deliberately close to what a reader's own built-in headings look like, because this
    /// exists to make the file *say* what it was already silently relying on.
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
    /// Definitions for `Heading1`…`Heading6`, index 0 being level 1. Empty ⇒ the writer
    /// falls back to [`DocxHeadingStyle::default_ramp`] over the body size, because the one
    /// thing it must never do is leave the ids undefined for the reader to guess at.
    #[serde(default)]
    pub heading_styles: Vec<DocxHeadingStyle>,
    /// Bytes for the document's inline images, keyed by their `src`.
    ///
    /// Supplied by the caller for the same reason [`PdfExportOptions::font_bytes`]
    /// is: this crate resolves no paths and reads no files. An image whose `src`
    /// is absent here is exported as its alt text instead of failing the export —
    /// a missing picture must not cost the writer their manuscript.
    ///
    /// [`PdfExportOptions::font_bytes`]: super::pdf_options::PdfExportOptions::font_bytes
    #[serde(default)]
    pub images: super::image_options::ExportImages,
    /// Comment threads to anchor into the exported `.docx` as real, native Word comments
    /// (`w:commentRangeStart`/`w:commentRangeEnd` around the anchored text, `w15:done` for a
    /// resolved thread, replies threaded via `w15:paraIdParent`). Empty ⇒ no comments are
    /// written, matching plain `to_docx`.
    ///
    /// Ranges are in the document's addressable character space — see
    /// [`super::comment_options::DocumentComment`]'s doc comment for what that means and why
    /// it is not the same space `FormatRun` byte offsets live in.
    #[serde(default)]
    pub comments: super::comment_options::DocumentComments,
    /// Named positions and ranges to anchor into the exported `.docx` as
    /// `w:bookmarkStart`/`w:bookmarkEnd` — the carrier a host uses to recognise its own rows and
    /// comments when the file comes back from an editor. Empty ⇒ none are written.
    ///
    /// Bookmarks and not a private attribute, because Word does not preserve a private attribute
    /// on `<w:comment>` — see [`super::mark_options`]'s module doc for the measurement. Same
    /// addressable character space as [`comments`](Self::comments).
    #[serde(default)]
    pub marks: super::mark_options::DocumentMarks,
}

impl DocxExportOptions {
    /// docx-rs's built-in defaults — no manuscript styling (what plain `to_docx` uses).
    pub fn plain() -> Self {
        Self::default()
    }

    /// The heading styles to write, resolved: the caller's when it gave any, otherwise the
    /// default ramp scaled off whatever body size this export uses.
    pub fn resolved_heading_styles(&self) -> Vec<DocxHeadingStyle> {
        if self.heading_styles.is_empty() {
            DocxHeadingStyle::default_ramp(self.font_half_points.unwrap_or(24))
        } else {
            self.heading_styles.clone()
        }
    }
}
