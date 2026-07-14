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

/// Identifies one registered highlight session (see [`crate::TextDocument::add_syntax_session`]
/// / [`crate::TextDocument::add_range_session`]).
///
/// A document can carry several highlight layers at once — a syntax highlighter, a
/// spell-checker, and one find session *per view*. Each is a session with its own id, so a
/// per-view [`HighlightMask`] can name exactly the ones a given pane should render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u64);

/// One highlight range carried by a *range session*, in **absolute document char offsets** —
/// the same coordinate space [`crate::FindMatch`] and replace report in.
///
/// A range session is the shape used for search highlighting and (eventually) an
/// externally-driven spell-checker: the host computes the ranges and hands the whole set over
/// with [`crate::TextDocument::set_session_ranges`], rather than implementing a per-block
/// callback. The document slices these absolute ranges to per-block spans at snapshot time
/// (a block's absolute char start is its `document_position`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeHighlight {
    /// Absolute char offset into the document text.
    pub start: usize,
    pub length: usize,
    pub format: HighlightFormat,
}

/// Which highlight sessions a particular **view** renders.
///
/// Two panes over one shared document can carry different find queries, so "which highlights
/// to show" is a property of the *view*, not the document. A snapshot is built under a mask;
/// only the sessions the mask admits contribute spans, and the effective
/// [`HighlighterKind`](enum@HighlighterKind) is the join over just those.
///
/// The default ([`HighlightMask::all`]) shows every session — the behaviour of the old
/// `snapshot_flow()`. [`HighlightMask::none`] shows none — the old
/// `snapshot_flow_without_highlights()`, and it must stay exactly as cheap, since every
/// read-only preview pane uses it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HighlightMask {
    /// `None` = admit every session (the default). `Some(set)` = admit only these ids.
    included: Option<Vec<SessionId>>,
}

impl HighlightMask {
    /// The all-admitting mask as a `const`, so a snapshot builder can reference "show
    /// everything" with a `'static` lifetime — no temporary to outlive.
    pub(crate) const ALL: HighlightMask = HighlightMask { included: None };

    /// Show every session attached to the document. The default, and the shape of a plain
    /// `snapshot_flow()`.
    pub fn all() -> Self {
        Self { included: None }
    }

    /// Show no highlights at all — as cheap as the old `show_highlights = false`.
    pub fn none() -> Self {
        Self {
            included: Some(Vec::new()),
        }
    }

    /// Show only the named sessions (e.g. the shared syntax + spell sessions plus *this*
    /// view's own find session).
    pub fn only(ids: impl IntoIterator<Item = SessionId>) -> Self {
        Self {
            included: Some(ids.into_iter().collect()),
        }
    }

    /// Add a session to a mask that already names some (a no-op on [`HighlightMask::all`],
    /// which already admits everything).
    pub fn with(mut self, id: SessionId) -> Self {
        if let Some(ids) = &mut self.included
            && !ids.contains(&id)
        {
            ids.push(id);
        }
        self
    }

    /// Whether this mask admits `id`.
    pub(crate) fn admits(&self, id: SessionId) -> bool {
        match &self.included {
            None => true,
            Some(ids) => ids.contains(&id),
        }
    }

    /// Whether this mask admits nothing — the fast path an empty/no-op preview takes, which
    /// must be exactly as cheap as the old boolean `false`.
    pub(crate) fn is_empty(&self) -> bool {
        matches!(&self.included, Some(ids) if ids.is_empty())
    }
}

/// What a snapshot renders, resolved **once at the root** and threaded down unchanged: the
/// effective [`HighlighterKind`](enum@HighlighterKind) (the join over the view's admitted
/// sessions) and the mask that selected them.
///
/// This replaces the plain `effective_kind: HighlighterKind` the block/frame builders used to
/// take — carrying the mask alongside so the leaf that resolves a block's spans knows which
/// sessions this view shows, without re-deriving the kind per block.
#[derive(Clone, Copy)]
pub(crate) struct SnapshotHighlights<'a> {
    pub kind: HighlighterKind,
    pub mask: &'a HighlightMask,
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

/// A **syntax session**: a callback highlighter and its per-block cascade cache. This is
/// exactly the old single-highlighter storage — one `SyntaxHighlighter` invoked once per
/// block, with its own `previous_state`/`current_state` timeline and per-block user data.
///
/// Each syntax session owns its cascade **independently**. A multiline-comment state from one
/// highlighter must never leak into another's `previous_block_state`, so the state is threaded
/// per session, never shared.
pub(crate) struct SyntaxSession {
    pub highlighter: Arc<dyn SyntaxHighlighter>,
    pub blocks: HashMap<usize, BlockHighlightData>,
}

/// A **range session**: absolute-offset ranges, set wholesale by the host and sliced to
/// per-block spans on demand. No callback, no cascade — used for find and (later) spell.
pub(crate) struct RangeSession {
    pub ranges: Vec<RangeHighlight>,
}

/// The two session shapes.
pub(crate) enum SessionBody {
    Syntax(SyntaxSession),
    Range(RangeSession),
}

/// One registered session with its stable id.
pub(crate) struct Session {
    pub id: SessionId,
    pub body: SessionBody,
}

/// Every highlight session on the document, in **registration order** — which is the merge
/// order: when two sessions format the same character, the later-registered one wins, field
/// by field (see [`merge_overlapping_highlights`]). Replaces the old single
/// `Option<HighlightData>` slot.
#[derive(Default)]
pub(crate) struct HighlightRegistry {
    pub sessions: Vec<Session>,
    next_id: u64,
}

impl HighlightRegistry {
    /// Mint the next session id.
    fn alloc_id(&mut self) -> SessionId {
        let id = SessionId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Register a syntax session (empty cache; the caller rehighlights).
    pub(crate) fn add_syntax(&mut self, highlighter: Arc<dyn SyntaxHighlighter>) -> SessionId {
        let id = self.alloc_id();
        self.sessions.push(Session {
            id,
            body: SessionBody::Syntax(SyntaxSession {
                highlighter,
                blocks: HashMap::new(),
            }),
        });
        id
    }

    /// Register an empty range session.
    pub(crate) fn add_range(&mut self) -> SessionId {
        let id = self.alloc_id();
        self.sessions.push(Session {
            id,
            body: SessionBody::Range(RangeSession { ranges: Vec::new() }),
        });
        id
    }

    /// Replace a range session's ranges. Returns `false` if `id` is not a range session (or
    /// does not exist) — a caller handing ranges to a syntax session is a bug, not a silent
    /// no-op to swallow.
    pub(crate) fn set_ranges(&mut self, id: SessionId, ranges: Vec<RangeHighlight>) -> bool {
        for s in &mut self.sessions {
            if s.id == id {
                if let SessionBody::Range(r) = &mut s.body {
                    r.ranges = ranges;
                    return true;
                }
                return false;
            }
        }
        false
    }

    /// Retire a session. Returns whether it existed.
    pub(crate) fn remove(&mut self, id: SessionId) -> bool {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.id != id);
        self.sessions.len() != before
    }

    /// Drop every syntax session, leaving range sessions in place. Backs the classic
    /// single-highlighter `set_syntax_highlighter`, whose contract is "replace the syntax
    /// highlighter" — never touching a find/spell range session.
    pub(crate) fn remove_all_syntax(&mut self) {
        self.sessions
            .retain(|s| !matches!(s.body, SessionBody::Syntax(_)));
    }

    /// Whether any session is attached.
    pub(crate) fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
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

/// Returns `true` if this format sets a metric-affecting field, i.e. one that changes glyph
/// advances or line height: font family / size / weight / bold / italic, letter / word
/// spacing, or vertical alignment (sub/superscript). The color and underline-decoration fields
/// are paint-only and never trigger `true`.
pub(crate) fn format_touches_metrics(f: &HighlightFormat) -> bool {
    f.font_family.is_some()
        || f.font_point_size.is_some()
        || f.font_weight.is_some()
        || f.font_bold.is_some()
        || f.font_italic.is_some()
        || f.letter_spacing.is_some()
        || f.word_spacing.is_some()
        || f.vertical_alignment.is_some()
}

/// Returns `true` if any span sets a metric-affecting field. See [`format_touches_metrics`].
pub(crate) fn spans_touch_metrics(spans: &[HighlightSpan]) -> bool {
    spans.iter().any(|s| format_touches_metrics(&s.format))
}

impl HighlighterKind {
    /// None < PaintOnly < Metric. The join over a view's admitted sessions is the max: one
    /// metric-affecting session forces the reshape path for the whole snapshot.
    fn rank(self) -> u8 {
        match self {
            HighlighterKind::None => 0,
            HighlighterKind::PaintOnly => 1,
            HighlighterKind::Metric => 2,
        }
    }

    fn join(self, other: HighlighterKind) -> HighlighterKind {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// The kind of one syntax session, from its cached spans.
fn syntax_session_kind(s: &SyntaxSession) -> HighlighterKind {
    let mut any = false;
    for bd in s.blocks.values() {
        if spans_touch_metrics(&bd.spans) {
            return HighlighterKind::Metric;
        }
        any |= !bd.spans.is_empty();
    }
    if any {
        HighlighterKind::PaintOnly
    } else {
        HighlighterKind::None
    }
}

/// The kind of one range session, from its ranges' formats.
fn range_session_kind(s: &RangeSession) -> HighlighterKind {
    let mut any = false;
    for r in &s.ranges {
        if format_touches_metrics(&r.format) {
            return HighlighterKind::Metric;
        }
        any |= r.length > 0;
    }
    if any {
        HighlighterKind::PaintOnly
    } else {
        HighlighterKind::None
    }
}

impl HighlightRegistry {
    /// The effective [`HighlighterKind`](enum@HighlighterKind) for a view — the join over the
    /// sessions the mask admits. Computed **once at the snapshot root** and threaded down as a
    /// plain value, exactly like the old single document-wide kind; a view showing only
    /// paint-only sessions never pays the reshape path for a metric session it does not show.
    pub(crate) fn effective_kind(&self, mask: &HighlightMask) -> HighlighterKind {
        if mask.is_empty() {
            return HighlighterKind::None;
        }
        let mut kind = HighlighterKind::None;
        for s in &self.sessions {
            if !mask.admits(s.id) {
                continue;
            }
            let k = match &s.body {
                SessionBody::Syntax(syn) => syntax_session_kind(syn),
                SessionBody::Range(r) => range_session_kind(r),
            };
            kind = kind.join(k);
            if kind == HighlighterKind::Metric {
                break;
            }
        }
        kind
    }
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

/// Every block's id + text, sorted by `document_position`. The text materialization is the
/// expensive part (rope → String for the whole document), so callers that only need block
/// *positions* use [`ordered_block_positions`] instead.
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

/// Every block's id + absolute char start (`document_position`), sorted — **without**
/// materializing any block text. This is the cheap sibling of [`ordered_block_ids`]; the
/// double full-document scan it exists to prevent is described on [`TextDocumentInner::rehighlight_affected`].
fn ordered_block_positions(inner: &TextDocumentInner) -> Vec<(u64, usize)> {
    let mut blocks = block_commands::get_all_block(&inner.ctx).unwrap_or_default();
    let store = inner.ctx.db_context.get_store();
    crate::inner::refresh_block_positions(&mut blocks, store);
    blocks.sort_by_key(|b| b.document_position);
    blocks
        .into_iter()
        .map(|b| (b.id, b.document_position.max(0) as usize))
        .collect()
}

/// A block's absolute char start and char length — the geometry a range session needs to
/// slice its absolute ranges to this block. `document_position` is the block's absolute char
/// start, exactly as the find/replace path resolves offsets against it.
fn block_geometry(inner: &TextDocumentInner, block_id: usize) -> (usize, usize) {
    let store = inner.ctx.db_context.get_store();
    let Some(mut dto) = block_commands::get_block(&inner.ctx, &(block_id as u64))
        .ok()
        .flatten()
    else {
        return (0, 0);
    };
    crate::inner::refresh_block_position(&mut dto, store);
    let abs_start = dto.document_position.max(0) as usize;
    let entity: common::entities::Block = dto.into();
    let len = common::database::rope_helpers::block_char_length(&entity, store).max(0) as usize;
    (abs_start, len)
}

/// The block-relative highlight spans a **view** sees for one block: every session the mask
/// admits, in registration order (so a later session's field overrides an earlier one), with
/// range sessions' absolute ranges sliced to this block.
///
/// The block geometry needed to slice range sessions is fetched **lazily** — a document with
/// only syntax sessions (or a mask that admits none) pays nothing for it.
pub(crate) fn merged_spans_for_block(
    inner: &TextDocumentInner,
    block_id: usize,
    mask: &HighlightMask,
) -> Vec<HighlightSpan> {
    if mask.is_empty() || inner.highlights.is_empty() {
        return Vec::new();
    }

    let mut geom: Option<(usize, usize)> = None;
    let mut out: Vec<HighlightSpan> = Vec::new();

    for s in &inner.highlights.sessions {
        if !mask.admits(s.id) {
            continue;
        }
        match &s.body {
            SessionBody::Syntax(syn) => {
                if let Some(bd) = syn.blocks.get(&block_id) {
                    out.extend(bd.spans.iter().cloned());
                }
            }
            SessionBody::Range(r) => {
                if r.ranges.is_empty() {
                    continue;
                }
                let (abs_start, len) = *geom.get_or_insert_with(|| block_geometry(inner, block_id));
                let block_end = abs_start + len;
                for rng in &r.ranges {
                    let lo = rng.start.max(abs_start);
                    let hi = (rng.start + rng.length).min(block_end);
                    if lo < hi {
                        out.push(HighlightSpan {
                            start: lo - abs_start,
                            length: hi - lo,
                            format: rng.format.clone(),
                        });
                    }
                }
            }
        }
    }
    out
}

impl TextDocumentInner {
    /// Re-highlight every block, for every **syntax** session (range sessions carry their
    /// ranges directly and never run a callback).
    ///
    /// Each syntax session runs its **own** cascade — its own `previous_state` timeline,
    /// reset to −1 here, and its own per-block cache. State never crosses between sessions, or
    /// one highlighter's multiline-comment run would corrupt another's. The block text is
    /// materialized **once** and shared across sessions, though: the rope → String scan is the
    /// cost, and running it per session would be N× for no reason.
    pub(crate) fn rehighlight_all(&mut self) {
        if self.highlights.is_empty() {
            self.recompute_highlight_kind();
            return;
        }
        let blocks = ordered_block_ids(self);

        for si in 0..self.highlights.sessions.len() {
            let SessionBody::Syntax(syn) = &self.highlights.sessions[si].body else {
                continue;
            };
            let highlighter = Arc::clone(&syn.highlighter);

            let mut fresh: HashMap<usize, BlockHighlightData> = HashMap::new();
            let mut previous_state: i64 = -1;
            for (block_id, text) in &blocks {
                let bid = *block_id as usize;
                let mut ctx = HighlightContext::new(bid, previous_state, None);
                highlighter.highlight_block(text, &mut ctx);
                let (spans, state, user_data) = ctx.into_parts();
                previous_state = state;
                fresh.insert(
                    bid,
                    BlockHighlightData {
                        spans,
                        state,
                        user_data,
                    },
                );
            }
            if let SessionBody::Syntax(syn) = &mut self.highlights.sessions[si].body {
                syn.blocks = fresh;
            }
        }

        self.recompute_highlight_kind();
    }

    /// Recompute the cached document-wide [`highlight_kind`](TextDocumentInner::highlight_kind)
    /// — the effective kind under the all-sessions mask, which is what the unmasked
    /// `snapshot_flow()` uses. Masked snapshots derive their own kind at the root.
    pub(crate) fn recompute_highlight_kind(&mut self) {
        self.highlight_kind = self.highlights.effective_kind(&HighlightMask::all());
    }

    /// This syntax session's cached block state (−1 if unset / not a syntax session).
    fn syntax_block_state(&self, session_idx: usize, block_id: usize) -> i64 {
        match &self.highlights.sessions[session_idx].body {
            SessionBody::Syntax(s) => s.blocks.get(&block_id).map_or(-1, |d| d.state),
            _ => -1,
        }
    }

    /// Re-highlight from a block for every syntax session, cascading each until its own state
    /// stabilizes.
    pub(crate) fn rehighlight_from_block(&mut self, start_block_id: usize) {
        if self.highlights.is_empty() {
            return;
        }
        let blocks = ordered_block_ids(self);
        let Some(start_idx) = blocks
            .iter()
            .position(|(id, _)| *id as usize == start_block_id)
        else {
            return;
        };

        for si in 0..self.highlights.sessions.len() {
            if matches!(self.highlights.sessions[si].body, SessionBody::Syntax(_)) {
                self.rehighlight_session_from(si, start_idx, &blocks);
            }
        }

        self.recompute_highlight_kind();
    }

    /// One syntax session's cascade from `start_idx` (see [`Self::rehighlight_from_block`]).
    fn rehighlight_session_from(
        &mut self,
        session_idx: usize,
        start_idx: usize,
        blocks: &[(u64, String)],
    ) {
        let highlighter = match &self.highlights.sessions[session_idx].body {
            SessionBody::Syntax(s) => Arc::clone(&s.highlighter),
            _ => return,
        };

        for i in start_idx..blocks.len() {
            let (block_id, ref text) = blocks[i];
            let bid = block_id as usize;

            let previous_state = if i == 0 {
                -1
            } else {
                self.syntax_block_state(session_idx, blocks[i - 1].0 as usize)
            };

            // Reuse the block's existing user data (this session's own).
            let user_data = match &mut self.highlights.sessions[session_idx].body {
                SessionBody::Syntax(s) => s.blocks.get_mut(&bid).and_then(|d| d.user_data.take()),
                _ => None,
            };
            let old_state = self.syntax_block_state(session_idx, bid);

            let mut ctx = HighlightContext::new(bid, previous_state, user_data);
            highlighter.highlight_block(text, &mut ctx);
            let (spans, state, user_data) = ctx.into_parts();

            if let SessionBody::Syntax(s) = &mut self.highlights.sessions[session_idx].body {
                s.blocks.insert(
                    bid,
                    BlockHighlightData {
                        spans,
                        state,
                        user_data,
                    },
                );
            }

            // Past the start and the state is unchanged: this session's cascade has settled.
            if i > start_idx && state == old_state {
                break;
            }
        }
    }

    /// Re-highlight the blocks a content change at `position` affects.
    ///
    /// The target block is found from a **positions-only** scan (no block text materialized),
    /// then [`rehighlight_from_block`](Self::rehighlight_from_block) does the one text scan it
    /// needs. The old code materialized every block's text here *just to locate one block*, and
    /// `rehighlight_from_block` then materialized them all again — two full-document rope →
    /// String passes on every single keystroke, at N=1.
    pub(crate) fn rehighlight_affected(&mut self, position: usize) {
        if self.highlights.is_empty() {
            return;
        }
        let positions = ordered_block_positions(self);
        if positions.is_empty() {
            return;
        }
        // The last block whose start is at or before `position` contains it.
        let target_bid = positions
            .iter()
            .rev()
            .find(|(_, bp)| position >= *bp)
            .map(|(id, _)| *id as usize)
            .unwrap_or_else(|| positions[0].0 as usize);

        self.rehighlight_from_block(target_bid);
    }
}
