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
/// `HighlighterKind` is the join over just those.
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
    /// Skip the paint-only overlay (`paint_highlights`) entirely: fragments are
    /// still split for metric sessions, but no `extract_paint_spans` runs. For
    /// consumers that read only the fragments/geometry and discard the visual
    /// overlay — the accessibility tree above all — this drops the whole
    /// paint-span computation, which is O(spans) per block (superlinear when a
    /// block carries thousands of ranges, e.g. a spell-checked Lorem scene).
    /// The produced `fragments` are byte-identical to a normal snapshot; only
    /// `paint_highlights` differs (always empty here).
    pub suppress_paint: bool,
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
/// per-block spans on demand. No callback, no cascade — used for find and spell.
///
/// The ranges carry a **per-block index** and a **cached kind**, both built once by
/// [`set_ranges`](HighlightRegistry::set_ranges) from the document's block layout at push time.
/// They replace two per-query full scans of the whole range vector that made a snapshot
/// O(blocks × ranges): a spell-check of a Lorem-Ipsum-dense document flags one range per word
/// (tens of thousands of them), and *every* block used to walk *all* of them.
pub(crate) struct RangeSession {
    pub ranges: Vec<RangeHighlight>,
    /// `block_id → indices into `ranges` that overlap that block`, at the block layout of the
    /// last push. A block **absent** from the map has no ranges for this session.
    ///
    /// Freshness: the ranges' absolute offsets and this index are a matched snapshot of one
    /// push. If the document is edited *without* a following push, both go stale together — the
    /// same window the un-indexed code already had (it, too, clipped last-push absolute ranges
    /// against fresh geometry). The one new shape is **missing vs. stale**: a block *created*
    /// since the last push has no key here, so it shows nothing (rather than a stale span) until
    /// the next push rebuilds the index. For the spell producer a structural edit re-tokenises
    /// and re-pushes on the next frame, closing the window; a brand-new empty block has nothing
    /// to flag anyway.
    block_index: HashMap<usize, Vec<u32>>,
    /// The session's [`HighlighterKind`], computed in the same pass as `block_index` so
    /// [`effective_kind`](HighlightRegistry::effective_kind) is O(1) per session instead of a
    /// full range scan on every snapshot root.
    kind: HighlighterKind,
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
    /// The session owned by the classic single-highlighter `set_syntax_highlighter` shim, if
    /// one is installed. Kept apart so the shim replaces **only its own** session and never a
    /// spell-checker or find layer another caller added via `add_syntax_session`.
    shim: Option<SessionId>,
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
            body: SessionBody::Range(RangeSession {
                ranges: Vec::new(),
                block_index: HashMap::new(),
                kind: HighlighterKind::None,
            }),
        });
        id
    }

    /// Replace a range session's ranges, building its per-block index and cached kind from the
    /// document's `block_positions` (each `(block_id, absolute_char_start)`, sorted by start).
    /// Returns `false` if `id` is not a range session (or does not exist) — a caller handing
    /// ranges to a syntax session is a bug, not a silent no-op to swallow.
    pub(crate) fn set_ranges(
        &mut self,
        id: SessionId,
        ranges: Vec<RangeHighlight>,
        block_positions: &[(u64, usize)],
    ) -> bool {
        for s in &mut self.sessions {
            if s.id == id {
                if let SessionBody::Range(r) = &mut s.body {
                    r.kind = compute_range_kind(&ranges);
                    r.block_index = build_block_index(&ranges, block_positions);
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

    /// Install / replace / clear the classic single-highlighter shim
    /// (`set_syntax_highlighter`). Replaces **only** the shim's own session — a spell-checker
    /// or any other layer registered independently via [`add_syntax`](Self::add_syntax) is left
    /// untouched. `None` clears the shim.
    pub(crate) fn set_shim(&mut self, highlighter: Option<Arc<dyn SyntaxHighlighter>>) {
        if let Some(id) = self.shim.take() {
            self.remove(id);
        }
        if let Some(hl) = highlighter {
            self.shim = Some(self.add_syntax(hl));
        }
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

/// The kind a set of ranges implies, from their formats. Computed **once** at
/// [`set_ranges`](HighlightRegistry::set_ranges) time and cached on the [`RangeSession`], so
/// [`effective_kind`](HighlightRegistry::effective_kind) never rescans the whole vector.
fn compute_range_kind(ranges: &[RangeHighlight]) -> HighlighterKind {
    let mut any = false;
    for r in ranges {
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

/// Bucket each range into every block it overlaps, from the document's block layout at push
/// time (`block_positions` = each `(block_id, absolute_char_start)`, **sorted by start**).
///
/// A block spans `[start_i, start_{i+1})` in absolute char space (the last runs to `MAX`); a
/// range is bucketed into a block when their half-open spans intersect. Almost always that is a
/// single block — the spell/find producers emit ranges within one paragraph — but a range that
/// happens to straddle a boundary is added to **every** block it touches, so the per-block clip
/// downstream sees it in each, exactly as the old full scan did.
fn build_block_index(
    ranges: &[RangeHighlight],
    block_positions: &[(u64, usize)],
) -> HashMap<usize, Vec<u32>> {
    let mut index: HashMap<usize, Vec<u32>> = HashMap::new();
    if block_positions.is_empty() {
        return index;
    }
    for (ri, r) in ranges.iter().enumerate() {
        let r_end = r.start.saturating_add(r.length); // exclusive
        // The block containing `r.start`: the last block whose start <= r.start.
        let mut bi = match block_positions.binary_search_by_key(&r.start, |&(_, p)| p) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        // Walk forward over every block the range still overlaps.
        while bi < block_positions.len() {
            let b_start = block_positions[bi].1;
            if b_start >= r_end {
                break; // this block (and all later ones) start past the range
            }
            let b_end = block_positions
                .get(bi + 1)
                .map(|&(_, p)| p)
                .unwrap_or(usize::MAX);
            // Half-open intersection [b_start, b_end) ∩ [r.start, r_end).
            if r.start < b_end && b_start < r_end {
                index
                    .entry(block_positions[bi].0 as usize)
                    .or_default()
                    .push(ri as u32);
            }
            bi += 1;
        }
    }
    index
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
                SessionBody::Range(r) => r.kind, // cached at set_ranges — no rescan
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

    // Sweep the boundaries left→right, maintaining the set of spans active in the
    // current window in ORIGINAL-INDEX order (a `BTreeSet` of indices). This
    // replaces the former O(boundaries × spans) rescan — which re-filtered every
    // span at every boundary and went quadratic on a block carrying thousands of
    // ranges (a spell-checked Lorem paragraph, where every window still walked all
    // ~m ranges) — with O(m log m + Σ|active|). The emitted spans are byte-identical:
    // `BTreeSet` iterates indices ascending, the same order the old
    // `spans.iter().filter()` produced, so `merge_overlapping_highlights` sees the
    // same (last-wins) sequence. Each span is inserted and removed exactly once as
    // the two monotonic pointers advance.
    let n = spans.len();
    let mut by_start: Vec<usize> = (0..n).collect();
    by_start.sort_by_key(|&i| spans[i].start);
    let mut by_end: Vec<usize> = (0..n).collect();
    by_end.sort_by_key(|&i| spans[i].start.saturating_add(spans[i].length));

    let mut active: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    let mut ps = 0usize; // next span to activate (ordered by start)
    let mut pe = 0usize; // next span to deactivate (ordered by end)
    let mut scratch: Vec<&HighlightSpan> = Vec::new();

    let mut result = Vec::new();
    for w in boundaries.windows(2) {
        let (sub_start, sub_end) = (w[0], w[1]);
        if sub_end <= sub_start {
            continue;
        }
        // A span is active in [sub_start, sub_end) iff start <= sub_start < end.
        // Activate every span that has started by the left edge, then deactivate
        // every span that has ended by it — add-before-remove so a zero-length
        // span (start == end == sub_start) is excluded, matching the old strict
        // `end > sub_start` test.
        while ps < n && spans[by_start[ps]].start <= sub_start {
            active.insert(by_start[ps]);
            ps += 1;
        }
        while pe < n
            && spans[by_end[pe]]
                .start
                .saturating_add(spans[by_end[pe]].length)
                <= sub_start
        {
            active.remove(&by_end[pe]);
            pe += 1;
        }
        if active.is_empty() {
            continue;
        }
        scratch.clear();
        scratch.extend(active.iter().map(|&i| &spans[i]));
        let merged = merge_overlapping_highlights(&scratch);
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
pub(crate) fn ordered_block_positions(inner: &TextDocumentInner) -> Vec<(u64, usize)> {
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
                // Only the ranges the index says touch this block — O(ranges-in-block), not
                // O(all-ranges). A block absent from the index (empty bucket) contributes
                // nothing; see [`RangeSession::block_index`] on the missing-vs-stale window.
                let Some(indices) = r.block_index.get(&block_id) else {
                    continue;
                };
                let (abs_start, len) = *geom.get_or_insert_with(|| block_geometry(inner, block_id));
                let block_end = abs_start + len;
                for &ri in indices {
                    let rng = &r.ranges[ri as usize];
                    // Saturating: a range session's offsets come from the host (an externally
                    // driven spell-checker included), so a wild `start + length` must clamp, not
                    // overflow-panic in debug / wrap in release.
                    let lo = rng.start.max(abs_start);
                    let hi = rng.start.saturating_add(rng.length).min(block_end);
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

#[cfg(test)]
mod paint_span_tests {
    use super::*;

    /// The previous O(boundaries × spans) implementation, kept verbatim as the
    /// oracle the sweep must reproduce byte-for-byte.
    fn extract_paint_spans_reference(
        spans: &[HighlightSpan],
        block_len: usize,
    ) -> Vec<crate::flow::PaintHighlightSpan> {
        if spans.is_empty() || block_len == 0 {
            return Vec::new();
        }
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

    /// A span whose single paint field is keyed by `k`, so that last-wins merging
    /// across overlaps produces observably different output when the active-set
    /// ORDER is wrong — the property most at risk in the rewrite.
    fn span(start: usize, length: usize, k: usize) -> HighlightSpan {
        let c = |v: usize| crate::Color {
            red: v as u8,
            green: (v >> 8) as u8,
            blue: 7,
            alpha: 255,
        };
        let format = match k % 4 {
            0 => HighlightFormat {
                background_color: Some(c(k)),
                ..Default::default()
            },
            1 => HighlightFormat {
                foreground_color: Some(c(k)),
                ..Default::default()
            },
            2 => HighlightFormat {
                underline_color: Some(c(k)),
                ..Default::default()
            },
            // No paint field: must be dropped by both implementations.
            _ => HighlightFormat {
                font_bold: Some(true),
                ..Default::default()
            },
        };
        HighlightSpan {
            start,
            length,
            format,
        }
    }

    #[test]
    fn sweep_matches_reference_on_edge_cases() {
        let cases: Vec<(Vec<HighlightSpan>, usize)> = vec![
            (vec![], 10),
            (vec![span(0, 4, 0)], 0),                  // empty block
            (vec![span(0, 5, 0)], 10),                 // single
            (vec![span(0, 3, 0), span(5, 3, 1)], 10),  // disjoint
            (vec![span(2, 5, 0), span(4, 5, 1)], 12),  // overlap: later wins in [4,7)
            (vec![span(0, 10, 0), span(3, 2, 1)], 10), // nested
            (vec![span(0, 3, 0), span(3, 3, 1)], 10),  // adjacent (touch, no overlap)
            (vec![span(4, 0, 0)], 10),                 // zero-length → nothing
            (vec![span(0, 4, 3)], 10),                 // no paint field → dropped
            (vec![span(8, 5, 0)], 10),                 // spills past block_len
            (vec![span(12, 3, 0)], 10),                // entirely past block_len
            (vec![span(0, 4, 0), span(0, 4, 1), span(0, 4, 2)], 10), // coincident, order matters
        ];
        for (i, (spans, len)) in cases.iter().enumerate() {
            assert_eq!(
                extract_paint_spans(spans, *len),
                extract_paint_spans_reference(spans, *len),
                "edge case {i}: spans={spans:?} block_len={len}"
            );
        }
    }

    #[test]
    fn sweep_matches_reference_randomized() {
        // Deterministic LCG — no rng dependency, reproducible across runs.
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = |bound: usize| -> usize {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as usize) % bound.max(1)
        };
        for trial in 0..2000 {
            let block_len = 1 + next(40);
            let m = next(14); // 0..13 spans, a mix of overlap densities
            let mut spans = Vec::with_capacity(m);
            for k in 0..m {
                let start = next(block_len + 4); // sometimes at/past the end
                let length = next(block_len + 2); // sometimes zero, sometimes spilling over
                spans.push(span(start, length, trial + k));
            }
            assert_eq!(
                extract_paint_spans(&spans, block_len),
                extract_paint_spans_reference(&spans, block_len),
                "trial {trial}: spans={spans:?} block_len={block_len}"
            );
        }
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;

    fn paint(start: usize, length: usize) -> RangeHighlight {
        RangeHighlight {
            start,
            length,
            format: HighlightFormat {
                background_color: Some(crate::Color {
                    red: 255,
                    green: 0,
                    blue: 0,
                    alpha: 255,
                }),
                ..Default::default()
            },
        }
    }

    fn metric(start: usize, length: usize) -> RangeHighlight {
        RangeHighlight {
            start,
            length,
            format: HighlightFormat {
                font_bold: Some(true),
                ..Default::default()
            },
        }
    }

    // ── compute_range_kind (B2-M2): the cached kind matches the old per-range classification ──

    #[test]
    fn kind_is_none_when_nothing_paints() {
        assert_eq!(compute_range_kind(&[]), HighlighterKind::None);
        // A zero-length paint range colours nothing → None.
        assert_eq!(compute_range_kind(&[paint(3, 0)]), HighlighterKind::None);
    }

    #[test]
    fn kind_is_paint_only_for_a_background_range() {
        assert_eq!(
            compute_range_kind(&[paint(0, 4)]),
            HighlighterKind::PaintOnly
        );
    }

    #[test]
    fn kind_is_metric_when_any_range_touches_metrics() {
        // One metric range among paint ones lifts the whole session to Metric (a bold run
        // reshapes, so the view must take the reshape path).
        assert_eq!(
            compute_range_kind(&[paint(0, 2), metric(4, 3), paint(8, 1)]),
            HighlighterKind::Metric
        );
    }

    #[test]
    fn set_ranges_caches_the_kind_read_back_by_effective_kind() {
        let mut reg = HighlightRegistry::default();
        let id = reg.add_range();
        let positions = [(0u64, 0usize)];
        assert_eq!(
            reg.effective_kind(&HighlightMask::all()),
            HighlighterKind::None
        );

        reg.set_ranges(id, vec![paint(0, 5)], &positions);
        assert_eq!(
            reg.effective_kind(&HighlightMask::all()),
            HighlighterKind::PaintOnly
        );

        reg.set_ranges(id, vec![metric(0, 5)], &positions);
        assert_eq!(
            reg.effective_kind(&HighlightMask::all()),
            HighlighterKind::Metric,
            "the cached kind updates on every push"
        );
    }

    // ── build_block_index: bucketing ──

    /// Blocks at 0, 10, 20 (each 10 wide). Ranges land in the block(s) they overlap.
    fn three_blocks() -> Vec<(u64, usize)> {
        vec![(100, 0), (101, 10), (102, 20)]
    }

    fn bucket(index: &std::collections::HashMap<usize, Vec<u32>>, block: usize) -> Vec<u32> {
        index.get(&block).cloned().unwrap_or_default()
    }

    #[test]
    fn a_range_lands_only_in_its_own_block() {
        let idx = build_block_index(&[paint(12, 3)], &three_blocks());
        assert_eq!(
            bucket(&idx, 101),
            vec![0],
            "12..15 is inside block 101 [10,20)"
        );
        assert!(bucket(&idx, 100).is_empty());
        assert!(bucket(&idx, 102).is_empty());
    }

    #[test]
    fn a_straddling_range_lands_in_every_block_it_touches() {
        // 8..22 spans blocks 100 [0,10), 101 [10,20), 102 [20,∞).
        let idx = build_block_index(&[paint(8, 14)], &three_blocks());
        assert_eq!(bucket(&idx, 100), vec![0]);
        assert_eq!(bucket(&idx, 101), vec![0]);
        assert_eq!(bucket(&idx, 102), vec![0]);
    }

    #[test]
    fn a_zero_length_range_buckets_into_its_block_but_paints_nothing() {
        // A degenerate zero-length range still buckets into the block that contains its point
        // (here 101), which is harmless: the per-block clip drops it (lo == hi), so it paints
        // nothing — proven end-to-end by the coverage differential in the integration tests.
        // It must not scatter into other blocks.
        let idx = build_block_index(&[paint(12, 0)], &three_blocks());
        assert_eq!(bucket(&idx, 101), vec![0]);
        assert!(bucket(&idx, 100).is_empty());
        assert!(bucket(&idx, 102).is_empty());
    }

    #[test]
    fn an_out_of_range_start_is_bucketed_into_the_last_block_only_if_it_overlaps() {
        // start far past the end: only the last (unbounded) block could contain it, and it does
        // — the last block runs to usize::MAX — so it buckets there. That is harmless: the
        // per-block clip against real geometry drops it (start > block_end).
        let idx = build_block_index(&[paint(9999, 3)], &three_blocks());
        assert_eq!(
            bucket(&idx, 102),
            vec![0],
            "the unbounded last block is the only candidate"
        );
        assert!(bucket(&idx, 100).is_empty());
        assert!(bucket(&idx, 101).is_empty());
    }

    #[test]
    fn empty_positions_yields_an_empty_index() {
        assert!(build_block_index(&[paint(0, 5)], &[]).is_empty());
    }
}
