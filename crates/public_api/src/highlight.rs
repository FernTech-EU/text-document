//! Syntax highlighting support.
//!
//! Provides a [`SyntaxHighlighter`] trait inspired by Qt's `QSyntaxHighlighter`.
//! Implementors produce shadow formatting that is merged into
//! [`FragmentContent`] at layout time but never touches the stored
//! `format_runs` / `block_images` tables — export, cursor, undo, and
//! search remain unaffected.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use frontend::commands::block_commands;

use crate::flow::FragmentContent;
use crate::inner::TextDocumentInner;
use crate::{CharVerticalAlignment, Color, TextFormat, UnderlineStyle};

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Public types
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Formatting applied by a syntax highlighter to a text range.
///
/// All fields are `Option`: `None` means "don't override the real format."
/// Only non-`None` fields take precedence over the corresponding
/// [`TextFormat`] field for display purposes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HighlightFormat {
    pub foreground_color: Option<Color>,
    pub background_color: Option<Color>,
    pub underline_color: Option<Color>,
    pub font_family: Option<String>,
    pub font_point_size: Option<u32>,
    pub font_weight: Option<u32>,
    pub font_bold: Option<bool>,
    pub font_italic: Option<bool>,
    pub font_underline: Option<bool>,
    pub font_overline: Option<bool>,
    pub font_strikeout: Option<bool>,
    pub letter_spacing: Option<i32>,
    pub word_spacing: Option<i32>,
    pub underline_style: Option<UnderlineStyle>,
    pub vertical_alignment: Option<CharVerticalAlignment>,
    pub tooltip: Option<String>,
}

/// A single highlight span within a block.
///
/// `start` and `length` are block-relative **character** offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub length: usize,
    pub format: HighlightFormat,
}

/// Context passed to [`SyntaxHighlighter::highlight_block`].
///
/// Provides methods to set highlight formatting and manage per-block state.
pub struct HighlightContext {
    spans: Vec<HighlightSpan>,
    previous_state: i64,
    current_state: i64,
    block_id: usize,
    user_data: Option<Box<dyn Any + Send + Sync>>,
}

impl HighlightContext {
    /// Create a new context for highlighting a block.
    pub fn new(
        block_id: usize,
        previous_state: i64,
        user_data: Option<Box<dyn Any + Send + Sync>>,
    ) -> Self {
        Self {
            spans: Vec::new(),
            previous_state,
            current_state: -1,
            block_id,
            user_data,
        }
    }

    /// Apply a highlight format to a character range within the current block.
    ///
    /// Zero-length spans are silently ignored.
    pub fn set_format(&mut self, start: usize, length: usize, format: HighlightFormat) {
        if length == 0 {
            return;
        }
        self.spans.push(HighlightSpan {
            start,
            length,
            format,
        });
    }

    /// Get the block state of the previous block (−1 if no state was set).
    pub fn previous_block_state(&self) -> i64 {
        self.previous_state
    }

    /// Set the block state for the current block.
    ///
    /// If the new state differs from the previously stored value, the next
    /// block will be re-highlighted automatically (cascade).
    pub fn set_current_block_state(&mut self, state: i64) {
        self.current_state = state;
    }

    /// Get the current block state (defaults to −1).
    pub fn current_block_state(&self) -> i64 {
        self.current_state
    }

    /// Get the block ID.
    pub fn block_id(&self) -> usize {
        self.block_id
    }

    /// Set per-block user data (replaces any existing data).
    pub fn set_user_data(&mut self, data: Box<dyn Any + Send + Sync>) {
        self.user_data = Some(data);
    }

    /// Get a reference to the per-block user data.
    pub fn user_data(&self) -> Option<&(dyn Any + Send + Sync)> {
        self.user_data.as_deref()
    }

    /// Get a mutable reference to the per-block user data.
    pub fn user_data_mut(&mut self) -> Option<&mut (dyn Any + Send + Sync)> {
        self.user_data.as_deref_mut()
    }

    /// Consume the context and return the accumulated spans, final state,
    /// and user data.
    pub fn into_parts(self) -> (Vec<HighlightSpan>, i64, Option<Box<dyn Any + Send + Sync>>) {
        (self.spans, self.current_state, self.user_data)
    }
}

/// A user-implemented syntax highlighter that applies visual-only formatting.
///
/// Inspired by Qt's `QSyntaxHighlighter`. Implement this trait and attach it
/// to a document via [`TextDocument::set_syntax_highlighter`](crate::TextDocument::set_syntax_highlighter).
///
/// The highlighter is called once per block when the document content changes.
/// Use [`HighlightContext::set_format`] to apply highlight spans. Use
/// [`HighlightContext::set_current_block_state`] and
/// [`HighlightContext::previous_block_state`] for multi-block constructs
/// (e.g., multiline comments).
pub trait SyntaxHighlighter: Send + Sync {
    /// Called for each block that needs re-highlighting.
    fn highlight_block(&self, text: &str, ctx: &mut HighlightContext);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Internal storage
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Per-block highlight state.
pub(crate) struct BlockHighlightData {
    pub spans: Vec<HighlightSpan>,
    pub state: i64,
    pub user_data: Option<Box<dyn Any + Send + Sync>>,
}

/// All highlight data for the document.
pub(crate) struct HighlightData {
    pub highlighter: Arc<dyn SyntaxHighlighter>,
    pub blocks: HashMap<usize, BlockHighlightData>,
}

/// Classification of the active highlighter's output.
///
/// Drives whether highlights are merged into the shaping input
/// (`fragments`) or kept as a separate post-shape recolor overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HighlighterKind {
    /// No highlighter attached.
    None,
    /// Every span touches only paint attributes (colors, underline
    /// style, underline/overline/strikeout, tooltip). Glyph metrics are
    /// unchanged, so the layout engine can recolor without reshaping —
    /// `fragments` stay base and the spans ride as a paint overlay.
    PaintOnly,
    /// At least one span touches a metric-affecting field. Highlights
    /// are merged into `fragments` (reshape required on change).
    Metric,
}

/// Returns `true` if any span sets a metric-affecting field, i.e. one
/// that changes glyph advances or line height: font family / size /
/// weight / bold / italic, letter / word spacing, or vertical
/// alignment (sub/superscript). The color and underline-decoration
/// fields are paint-only and never trigger `true`.
pub(crate) fn spans_touch_metrics(spans: &[HighlightSpan]) -> bool {
    spans.iter().any(|s| {
        let f = &s.format;
        f.font_family.is_some()
            || f.font_point_size.is_some()
            || f.font_weight.is_some()
            || f.font_bold.is_some()
            || f.font_italic.is_some()
            || f.letter_spacing.is_some()
            || f.word_spacing.is_some()
            || f.vertical_alignment.is_some()
    })
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Merge algorithm
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Apply highlight format overrides onto a base `TextFormat`.
fn apply_highlight(base: &TextFormat, hl: &HighlightFormat) -> TextFormat {
    TextFormat {
        font_family: hl.font_family.clone().or_else(|| base.font_family.clone()),
        font_point_size: hl.font_point_size.or(base.font_point_size),
        font_weight: hl.font_weight.or(base.font_weight),
        font_bold: hl.font_bold.or(base.font_bold),
        font_italic: hl.font_italic.or(base.font_italic),
        font_underline: hl.font_underline.or(base.font_underline),
        font_overline: hl.font_overline.or(base.font_overline),
        font_strikeout: hl.font_strikeout.or(base.font_strikeout),
        letter_spacing: hl.letter_spacing.or(base.letter_spacing),
        word_spacing: hl.word_spacing.or(base.word_spacing),
        underline_style: hl
            .underline_style
            .clone()
            .or_else(|| base.underline_style.clone()),
        vertical_alignment: hl
            .vertical_alignment
            .clone()
            .or_else(|| base.vertical_alignment.clone()),
        tooltip: hl.tooltip.clone().or_else(|| base.tooltip.clone()),
        foreground_color: hl.foreground_color.or(base.foreground_color),
        background_color: hl.background_color.or(base.background_color),
        underline_color: hl.underline_color.or(base.underline_color),
        // Anchors are not overridden by highlights.
        anchor_href: base.anchor_href.clone(),
        anchor_names: base.anchor_names.clone(),
        is_anchor: base.is_anchor,
    }
}

/// Merge a set of overlapping highlights into a single `HighlightFormat`.
/// Later spans override earlier spans for the same field.
fn merge_overlapping_highlights(spans: &[&HighlightSpan]) -> HighlightFormat {
    let mut merged = HighlightFormat::default();
    for span in spans {
        let f = &span.format;
        if f.foreground_color.is_some() {
            merged.foreground_color = f.foreground_color;
        }
        if f.background_color.is_some() {
            merged.background_color = f.background_color;
        }
        if f.underline_color.is_some() {
            merged.underline_color = f.underline_color;
        }
        if f.font_family.is_some() {
            merged.font_family = f.font_family.clone();
        }
        if f.font_point_size.is_some() {
            merged.font_point_size = f.font_point_size;
        }
        if f.font_weight.is_some() {
            merged.font_weight = f.font_weight;
        }
        if f.font_bold.is_some() {
            merged.font_bold = f.font_bold;
        }
        if f.font_italic.is_some() {
            merged.font_italic = f.font_italic;
        }
        if f.font_underline.is_some() {
            merged.font_underline = f.font_underline;
        }
        if f.font_overline.is_some() {
            merged.font_overline = f.font_overline;
        }
        if f.font_strikeout.is_some() {
            merged.font_strikeout = f.font_strikeout;
        }
        if f.letter_spacing.is_some() {
            merged.letter_spacing = f.letter_spacing;
        }
        if f.word_spacing.is_some() {
            merged.word_spacing = f.word_spacing;
        }
        if f.underline_style.is_some() {
            merged.underline_style = f.underline_style.clone();
        }
        if f.vertical_alignment.is_some() {
            merged.vertical_alignment = f.vertical_alignment.clone();
        }
        if f.tooltip.is_some() {
            merged.tooltip = f.tooltip.clone();
        }
    }
    merged
}

/// Flatten a block's stored highlight spans into a list of
/// [`PaintHighlightSpan`](crate::flow::PaintHighlightSpan)s for the
/// paint-overlay path.
///
/// Only called when the active highlighter is [`HighlighterKind::PaintOnly`],
/// so metric fields are guaranteed absent and ignored here. Overlapping
/// spans are resolved exactly like `merge_highlight_spans` (split at every
/// boundary, last-wins per field) so the overlay matches what the merged
/// path would have produced. `block_len` is the block's character length.
/// Sub-ranges with no paint field set are skipped.
pub(crate) fn extract_paint_spans(
    spans: &[HighlightSpan],
    block_len: usize,
) -> Vec<crate::flow::PaintHighlightSpan> {
    if spans.is_empty() || block_len == 0 {
        return Vec::new();
    }

    // Collect and dedupe all span boundaries within (0, block_len).
    let mut boundaries = vec![0usize, block_len];
    for s in spans {
        let end = s.start.saturating_add(s.length);
        if s.start > 0 && s.start < block_len {
            boundaries.push(s.start);
        }
        if end > 0 && end < block_len {
            boundaries.push(end);
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut result = Vec::new();
    for w in boundaries.windows(2) {
        let (sub_start, sub_end) = (w[0], w[1]);
        if sub_end <= sub_start {
            continue;
        }
        let active: Vec<&HighlightSpan> = spans
            .iter()
            .filter(|s| s.start < sub_end && s.start + s.length > sub_start)
            .collect();
        if active.is_empty() {
            continue;
        }
        let merged = merge_overlapping_highlights(&active);
        if merged.foreground_color.is_none()
            && merged.background_color.is_none()
            && merged.underline_color.is_none()
            && merged.underline_style.is_none()
            && merged.font_underline.is_none()
            && merged.font_overline.is_none()
            && merged.font_strikeout.is_none()
        {
            continue;
        }
        result.push(crate::flow::PaintHighlightSpan {
            start: sub_start,
            length: sub_end - sub_start,
            foreground_color: merged.foreground_color,
            background_color: merged.background_color,
            underline_color: merged.underline_color,
            underline_style: merged.underline_style,
            font_underline: merged.font_underline,
            font_overline: merged.font_overline,
            font_strikeout: merged.font_strikeout,
        });
    }
    result
}

/// Merge highlight spans into a list of fragments.
///
/// Text fragments that overlap with highlight spans are split at span
/// boundaries. The highlight format is overlaid onto the base `TextFormat`.
/// Image fragments receive the overlay without splitting.
/// Local copy of the word-start computation from `text_block.rs`:
/// returns character indices (not byte offsets) where a Unicode word
/// starts, per UAX #29. Mirrors the upstream helper so highlight
/// splits produce accessibility-correct word_starts for each
/// sub-fragment without reaching into `text_block`.
fn compute_word_starts_local(text: &str) -> Vec<u8> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut result = Vec::new();
    let mut byte_to_char: Vec<(usize, usize)> = Vec::new();
    for (ci, (bi, _)) in text.char_indices().enumerate() {
        byte_to_char.push((bi, ci));
    }
    for (byte_off, _word) in text.unicode_word_indices() {
        let char_idx = byte_to_char
            .iter()
            .find(|(bi, _)| *bi == byte_off)
            .map(|(_, ci)| *ci)
            .unwrap_or(0);
        if let Ok(idx) = u8::try_from(char_idx) {
            result.push(idx);
        } else {
            break;
        }
    }
    result
}

pub(crate) fn merge_highlight_spans(
    fragments: Vec<FragmentContent>,
    spans: &[HighlightSpan],
) -> Vec<FragmentContent> {
    if spans.is_empty() {
        return fragments;
    }

    let mut result = Vec::with_capacity(fragments.len());

    for frag in fragments {
        match frag {
            FragmentContent::Text {
                ref text,
                ref format,
                offset,
                length,
                element_id,
                word_starts: _,
            } => {
                let frag_end = offset + length;

                // Collect highlight boundaries within this fragment's range.
                let mut boundaries = Vec::new();
                boundaries.push(offset);
                boundaries.push(frag_end);

                for span in spans {
                    let span_end = span.start + span.length;
                    // Does this span overlap the fragment?
                    if span.start < frag_end && span_end > offset {
                        if span.start > offset && span.start < frag_end {
                            boundaries.push(span.start);
                        }
                        if span_end > offset && span_end < frag_end {
                            boundaries.push(span_end);
                        }
                    }
                }

                boundaries.sort_unstable();
                boundaries.dedup();

                // Split the text at each boundary and apply overlapping highlights.
                let chars: Vec<char> = text.chars().collect();
                for window in boundaries.windows(2) {
                    let sub_start = window[0];
                    let sub_end = window[1];
                    let sub_len = sub_end - sub_start;
                    if sub_len == 0 {
                        continue;
                    }

                    // Collect all highlight spans overlapping [sub_start, sub_end).
                    let active: Vec<&HighlightSpan> = spans
                        .iter()
                        .filter(|s| {
                            let s_end = s.start + s.length;
                            s.start < sub_end && s_end > sub_start
                        })
                        .collect();

                    let char_start = sub_start - offset;
                    let char_end = char_start + sub_len;
                    let sub_text: String = chars[char_start..char_end].iter().collect();

                    let sub_format = if active.is_empty() {
                        format.clone()
                    } else {
                        let merged_hl = merge_overlapping_highlights(&active);
                        apply_highlight(format, &merged_hl)
                    };

                    let sub_word_starts = compute_word_starts_local(&sub_text);
                    result.push(FragmentContent::Text {
                        text: sub_text,
                        format: sub_format,
                        offset: sub_start,
                        length: sub_len,
                        // All sub-fragments split from one source
                        // `FragmentContent::Text` reference the same
                        // underlying format run — only the highlight
                        // formatting differs. Sharing the id is
                        // correct for accessibility (the underlying
                        // text belongs to one stable run) at the cost
                        // that synthetic NodeIds for highlighted
                        // sub-runs collide unless the caller further
                        // disambiguates.
                        // The bastyde-widgets layer handles that by
                        // mixing the `offset` into the synthetic-id
                        // hash alongside `element_id`.
                        element_id,
                        word_starts: sub_word_starts,
                    });
                }
            }
            FragmentContent::Image {
                ref name,
                width,
                height,
                quality,
                ref format,
                offset,
                element_id,
            } => {
                // Find overlapping highlights for this single-char position.
                let active: Vec<&HighlightSpan> = spans
                    .iter()
                    .filter(|s| {
                        let s_end = s.start + s.length;
                        s.start < offset + 1 && s_end > offset
                    })
                    .collect();

                let img_format = if active.is_empty() {
                    format.clone()
                } else {
                    let merged_hl = merge_overlapping_highlights(&active);
                    apply_highlight(format, &merged_hl)
                };

                result.push(FragmentContent::Image {
                    name: name.clone(),
                    width,
                    height,
                    quality,
                    format: img_format,
                    offset,
                    element_id,
                });
            }
        }
    }

    result
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Re-highlighting
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Get all block IDs sorted by document_position.
fn ordered_block_ids(inner: &TextDocumentInner) -> Vec<(u64, String)> {
    let mut blocks = block_commands::get_all_block(&inner.ctx).unwrap_or_default();
    let store = inner.ctx.db_context.get_store();
    crate::inner::refresh_block_positions(&mut blocks, store);
    blocks.sort_by_key(|b| b.document_position);
    blocks
        .into_iter()
        .map(|b| {
            let entity: common::entities::Block = b.clone().into();
            let text = common::database::rope_helpers::block_content_via_store(&entity, store);
            (b.id, text)
        })
        .collect()
}

impl TextDocumentInner {
    /// Re-highlight all blocks in the document.
    pub(crate) fn rehighlight_all(&mut self) {
        let hl = match self.highlight {
            Some(ref mut hl) => hl,
            None => return,
        };

        let highlighter = Arc::clone(&hl.highlighter);
        hl.blocks.clear();

        let blocks = ordered_block_ids(self);
        let mut previous_state: i64 = -1;

        for (block_id, text) in &blocks {
            let bid = *block_id as usize;
            let mut ctx = HighlightContext::new(bid, previous_state, None);
            highlighter.highlight_block(text, &mut ctx);
            let (spans, state, user_data) = ctx.into_parts();

            previous_state = state;

            // Only store if there's something to store.
            let hl = self.highlight.as_mut().unwrap();
            hl.blocks.insert(
                bid,
                BlockHighlightData {
                    spans,
                    state,
                    user_data,
                },
            );
        }

        self.recompute_highlight_kind();
    }

    /// Recompute [`highlight_kind`](TextDocumentInner::highlight_kind) from the
    /// currently stored spans. Paint-only iff a highlighter is attached and no
    /// stored span touches a metric field.
    pub(crate) fn recompute_highlight_kind(&mut self) {
        self.highlight_kind = match &self.highlight {
            None => HighlighterKind::None,
            Some(hl) => {
                if hl.blocks.values().any(|bd| spans_touch_metrics(&bd.spans)) {
                    HighlighterKind::Metric
                } else {
                    HighlighterKind::PaintOnly
                }
            }
        };
    }

    /// Re-highlight starting from a specific block, cascading until the
    /// block state stabilizes or the end of the document is reached.
    pub(crate) fn rehighlight_from_block(&mut self, start_block_id: usize) {
        let hl = match self.highlight {
            Some(ref hl) => hl,
            None => return,
        };

        let highlighter = Arc::clone(&hl.highlighter);
        let blocks = ordered_block_ids(self);

        // Find the starting index.
        let start_idx = match blocks
            .iter()
            .position(|(id, _)| *id as usize == start_block_id)
        {
            Some(idx) => idx,
            None => return,
        };

        for i in start_idx..blocks.len() {
            let (block_id, ref text) = blocks[i];
            let bid = block_id as usize;

            let hl = self.highlight.as_ref().unwrap();

            // Get previous block's state.
            let previous_state = if i == 0 {
                -1
            } else {
                let prev_bid = blocks[i - 1].0 as usize;
                hl.blocks.get(&prev_bid).map_or(-1, |d| d.state)
            };

            // Take existing user data if available.
            let user_data = self
                .highlight
                .as_mut()
                .unwrap()
                .blocks
                .get_mut(&bid)
                .and_then(|d| d.user_data.take());

            let old_state = self
                .highlight
                .as_ref()
                .unwrap()
                .blocks
                .get(&bid)
                .map_or(-1, |d| d.state);

            let mut ctx = HighlightContext::new(bid, previous_state, user_data);
            highlighter.highlight_block(text, &mut ctx);
            let (spans, state, user_data) = ctx.into_parts();

            let hl = self.highlight.as_mut().unwrap();
            hl.blocks.insert(
                bid,
                BlockHighlightData {
                    spans,
                    state,
                    user_data,
                },
            );

            // If we are past the initial block and the state didn't change,
            // stop cascading.
            if i > start_idx && state == old_state {
                break;
            }
        }

        self.recompute_highlight_kind();
    }

    /// Re-highlight blocks affected by a content change at the given
    /// document position.
    pub(crate) fn rehighlight_affected(&mut self, position: usize) {
        if self.highlight.is_none() {
            return;
        }

        let blocks = ordered_block_ids(self);

        let store = self.ctx.db_context.get_store();
        // Find the block that contains `position`.
        let target_bid = blocks
            .iter()
            .rev()
            .find_map(|(id, _)| {
                let mut dto = block_commands::get_block(&self.ctx, id).ok().flatten()?;
                crate::inner::refresh_block_position(&mut dto, store);
                let bp = dto.document_position as usize;
                if position >= bp {
                    Some(*id as usize)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| blocks.first().map_or(0, |(id, _)| *id as usize));

        if blocks.is_empty() {
            return;
        }

        self.rehighlight_from_block(target_bid);
    }
}
