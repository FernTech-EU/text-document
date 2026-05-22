//! Flow types for document traversal and layout engine support.
//!
//! The layout engine processes [`FlowElement`]s in order to build its layout
//! tree. Snapshot types capture consistent views for thread-safe reads.

use crate::text_block::TextBlock;
use crate::text_frame::TextFrame;
use crate::text_table::TextTable;
use crate::{Alignment, BlockFormat, FrameFormat, ListStyle, TextFormat};

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// FlowElement
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// An element in the document's visual flow.
///
/// The layout engine processes these in order to build its layout tree.
/// Obtained from [`TextDocument::flow()`](crate::TextDocument::flow) or
/// [`TextFrame::flow()`].
#[derive(Clone)]
pub enum FlowElement {
    /// A paragraph or heading. Layout as a text block.
    Block(TextBlock),

    /// A table at this position in the flow. Layout as a grid.
    /// The anchor frame's `table` field identifies the table entity.
    Table(TextTable),

    /// A non-table sub-frame (float, sidebar, blockquote).
    /// Contains its own nested flow, accessible via
    /// [`TextFrame::flow()`].
    Frame(TextFrame),
}

impl FlowElement {
    /// Snapshot this element into a thread-safe, plain-data representation.
    ///
    /// Dispatches to [`TextBlock::snapshot()`], [`TextTable::snapshot()`],
    /// or [`TextFrame::snapshot()`] as appropriate.
    pub fn snapshot(&self) -> FlowElementSnapshot {
        match self {
            FlowElement::Block(b) => FlowElementSnapshot::Block(b.snapshot()),
            FlowElement::Table(t) => FlowElementSnapshot::Table(t.snapshot()),
            FlowElement::Frame(f) => FlowElementSnapshot::Frame(f.snapshot()),
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// FragmentContent
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// A contiguous run of content with uniform formatting within a block.
///
/// Offsets are **block-relative**: `offset` is the character position
/// within the block where this fragment starts (0 = block start).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FragmentContent {
    /// A text run. The layout engine shapes these into glyphs.
    Text {
        text: String,
        format: TextFormat,
        /// Character offset within the block (block-relative).
        offset: usize,
        /// Character count.
        length: usize,
        /// Stable synthesized id for the underlying format run
        /// (see [`synth_element_id`](common::format_runs::synth_element_id)).
        /// Survives edits that don't delete the run (character insertions
        /// inside the run keep the same id). Used by accessibility layers
        /// to build stable `NodeId`s for AccessKit `TextRun` children.
        element_id: u64,
        /// Unicode word starts within `text`, expressed as character
        /// indices (not byte offsets). Computed per UAX #29 via
        /// `unicode-segmentation`. Fed directly into AccessKit's
        /// `set_word_starts` on the corresponding `Role::TextRun`.
        word_starts: Vec<u8>,
    },
    /// An inline image. The layout engine reserves space for it.
    ///
    /// To retrieve the image pixel data, use the existing
    /// [`TextDocument::resource(name)`](crate::TextDocument::resource) method.
    Image {
        name: String,
        width: u32,
        height: u32,
        quality: u32,
        format: TextFormat,
        /// Character offset within the block (block-relative).
        offset: usize,
        /// Stable synthesized id for the underlying image anchor
        /// (see [`synth_element_id`](common::format_runs::synth_element_id)).
        element_id: u64,
    },
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// BlockSnapshot
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// All layout-relevant data for one block, captured atomically.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockSnapshot {
    pub block_id: usize,
    pub position: usize,
    pub length: usize,
    pub text: String,
    pub fragments: Vec<FragmentContent>,
    pub block_format: BlockFormat,
    pub list_info: Option<ListInfo>,
    /// Parent frame ID. Needed to know where this block lives in the
    /// frame tree (e.g. main frame vs. a sub-frame or table cell frame).
    pub parent_frame_id: Option<usize>,
    /// If this block is inside a table cell, the cell coordinates.
    /// Needed so the typesetter can propagate height changes to the
    /// enclosing table row.
    pub table_cell: Option<TableCellContext>,
    /// Paint-only highlight overlay for this block.
    ///
    /// Non-empty **only** when the active syntax highlighter is paint-only
    /// (colors / underline decorations, no metric changes). In that case
    /// `fragments` carry the *base* formatting (no highlight merge) and the
    /// layout engine applies these spans as a post-shape recolor — no
    /// reshaping. When a metric-affecting highlighter is active, highlights
    /// are merged into `fragments` as usual and this is empty.
    pub paint_highlights: Vec<PaintHighlightSpan>,
}

/// A resolved paint-only highlight span for one character range of a block.
///
/// Char offsets are block-relative, matching [`HighlightSpan`](crate::HighlightSpan).
/// Each color field is `None` when the highlight does not override it. This is
/// the post-shape overlay counterpart of the merged-into-`fragments` path —
/// it carries only attributes that do not change glyph metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaintHighlightSpan {
    pub start: usize,
    pub length: usize,
    pub foreground_color: Option<crate::Color>,
    pub background_color: Option<crate::Color>,
    pub underline_color: Option<crate::Color>,
    pub underline_style: Option<crate::UnderlineStyle>,
    pub font_underline: Option<bool>,
    pub font_overline: Option<bool>,
    pub font_strikeout: Option<bool>,
}

/// Snapshot-friendly reference to a table cell (plain IDs, no live handles).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCellContext {
    pub table_id: usize,
    pub row: usize,
    pub column: usize,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// ListInfo
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// List membership and marker information for a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListInfo {
    pub list_id: usize,
    /// The list style (Disc, Decimal, LowerAlpha, etc.).
    pub style: ListStyle,
    /// Indentation level.
    pub indent: u8,
    /// Pre-formatted marker text: "•", "3.", "(c)", "IV.", etc.
    pub marker: String,
    /// 0-based index of this item within its list.
    pub item_index: usize,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// TableCellRef
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Reference to a table cell that contains a block.
#[derive(Clone)]
pub struct TableCellRef {
    pub table: TextTable,
    pub row: usize,
    pub column: usize,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// CellRange / SelectionKind
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// A rectangular range of cells within a single table (inclusive bounds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellRange {
    pub table_id: usize,
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

impl CellRange {
    /// Expand the range so that every merged cell whose span overlaps the
    /// rectangle is fully included. `cells` is a slice of
    /// `(row, col, row_span, col_span)` for every cell in the table.
    ///
    /// Uses fixed-point iteration (converges in 1-2 rounds for typical tables).
    pub fn expand_for_spans(mut self, cells: &[(usize, usize, usize, usize)]) -> Self {
        loop {
            let mut expanded = false;
            for &(row, col, rs, cs) in cells {
                let cell_bottom = row + rs - 1;
                let cell_right = col + cs - 1;
                // Check overlap with current range
                if row <= self.end_row
                    && cell_bottom >= self.start_row
                    && col <= self.end_col
                    && cell_right >= self.start_col
                {
                    if row < self.start_row {
                        self.start_row = row;
                        expanded = true;
                    }
                    if cell_bottom > self.end_row {
                        self.end_row = cell_bottom;
                        expanded = true;
                    }
                    if col < self.start_col {
                        self.start_col = col;
                        expanded = true;
                    }
                    if cell_right > self.end_col {
                        self.end_col = cell_right;
                        expanded = true;
                    }
                }
            }
            if !expanded {
                break;
            }
        }
        self
    }
}

/// Describes what kind of selection the cursor currently has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionKind {
    /// No selection (position == anchor).
    None,
    /// Normal text selection within a single cell or outside any table.
    Text,
    /// Rectangular cell selection within a table.
    Cells(CellRange),
    /// Selection crosses a table boundary (starts/ends outside the table).
    /// The table portion is a rectangular cell range; `text_before` /
    /// `text_after` indicate whether text outside the table is also selected.
    Mixed {
        cell_range: CellRange,
        text_before: bool,
        text_after: bool,
    },
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Table format types
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Table-level formatting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableFormat {
    pub border: Option<i32>,
    pub cell_spacing: Option<i32>,
    pub cell_padding: Option<i32>,
    pub width: Option<i32>,
    pub alignment: Option<Alignment>,
}

/// Cell-level formatting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellFormat {
    pub padding: Option<i32>,
    pub border: Option<i32>,
    pub vertical_alignment: Option<CellVerticalAlignment>,
    pub background_color: Option<String>,
}

/// Vertical alignment within a table cell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CellVerticalAlignment {
    #[default]
    Top,
    Middle,
    Bottom,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Table and Cell Snapshots
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Consistent snapshot of a table's structure and all cell content.
#[derive(Debug, Clone, PartialEq)]
pub struct TableSnapshot {
    pub table_id: usize,
    pub rows: usize,
    pub columns: usize,
    pub column_widths: Vec<i32>,
    pub format: TableFormat,
    pub cells: Vec<CellSnapshot>,
}

/// Snapshot of one table cell including its block content.
#[derive(Debug, Clone, PartialEq)]
pub struct CellSnapshot {
    pub row: usize,
    pub column: usize,
    pub row_span: usize,
    pub column_span: usize,
    pub format: CellFormat,
    pub blocks: Vec<BlockSnapshot>,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Flow Snapshots
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Consistent snapshot of the entire document flow, captured in a
/// single lock acquisition.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowSnapshot {
    pub elements: Vec<FlowElementSnapshot>,
}

/// Snapshot of one flow element.
// `Block` is by far the most common variant in a document flow, so boxing it
// to shrink the enum would add a heap allocation on the hot path for no real
// gain — the large-variant cost only bites the rare `Table`/`Frame` elements.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum FlowElementSnapshot {
    Block(BlockSnapshot),
    Table(TableSnapshot),
    Frame(FrameSnapshot),
}

/// Snapshot of a sub-frame and its contents.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameSnapshot {
    pub frame_id: usize,
    pub format: FrameFormat,
    pub elements: Vec<FlowElementSnapshot>,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// FormatChangeKind
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// What kind of formatting changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatChangeKind {
    /// Block-level: alignment, margins, indent, heading level.
    /// Requires paragraph relayout.
    Block,
    /// Character-level: font, bold, italic, underline, color.
    /// Requires reshaping but not necessarily reflow.
    Character,
    /// List-level: style, indent, prefix, suffix.
    /// Requires marker relayout for list items.
    List,
}
