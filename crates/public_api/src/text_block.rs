//! Read-only block (paragraph) handle.

use std::cell::{Cell, OnceCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;

use frontend::commands::{block_commands, document_commands, frame_commands, list_commands};
use frontend::common::format_runs::{FormatRun, ImageAnchor, synth_element_id};
use frontend::common::types::EntityId;

use crate::convert::to_usize;
use crate::flow::{
    AddressablePiece, BlockSnapshot, FragmentContent, ListInfo, TableCellContext, TableCellRef,
};
use crate::inner::TextDocumentInner;
use crate::text_frame::TextFrame;
use crate::text_list::TextList;
use crate::text_table::TextTable;
use crate::{BlockFormat, ListStyle, TextFormat};

/// A lightweight, read-only handle to a single block (paragraph).
///
/// Holds a stable entity ID — the handle remains valid across edits
/// that insert or remove other blocks. Each method acquires the
/// document lock independently. For consistent reads across multiple
/// fields, use [`snapshot()`](TextBlock::snapshot).
#[derive(Clone)]
pub struct TextBlock {
    pub(crate) doc: Arc<Mutex<TextDocumentInner>>,
    pub(crate) block_id: usize,
}

impl TextBlock {
    // ── Content ──────────────────────────────────────────────

    /// Block's plain text. O(1).
    pub fn text(&self) -> String {
        let inner = self.doc.lock();
        let store = inner.ctx.db_context.get_store();
        block_commands::get_block(&inner.ctx, &(self.block_id as u64))
            .ok()
            .flatten()
            .map(|b| {
                let entity: common::entities::Block = b.into();
                common::database::rope_helpers::block_content_via_store(&entity, store)
            })
            .unwrap_or_default()
    }

    /// Character count. O(1).
    pub fn length(&self) -> usize {
        let inner = self.doc.lock();
        let store = inner.ctx.db_context.get_store();
        block_commands::get_block(&inner.ctx, &(self.block_id as u64))
            .ok()
            .flatten()
            .map(|b| {
                let entity: common::entities::Block = b.into();
                to_usize(common::database::rope_helpers::block_char_length(
                    &entity, store,
                ))
            })
            .unwrap_or(0)
    }

    /// `length() == 0`. O(1).
    pub fn is_empty(&self) -> bool {
        let inner = self.doc.lock();
        let store = inner.ctx.db_context.get_store();
        block_commands::get_block(&inner.ctx, &(self.block_id as u64))
            .ok()
            .flatten()
            .map(|b| {
                let entity: common::entities::Block = b.into();
                common::database::rope_helpers::block_char_length(&entity, store) == 0
            })
            .unwrap_or(true)
    }

    /// Block entity still exists in the database. O(1).
    pub fn is_valid(&self) -> bool {
        let inner = self.doc.lock();
        block_commands::get_block(&inner.ctx, &(self.block_id as u64))
            .ok()
            .flatten()
            .is_some()
    }

    // ── Identity and Position ────────────────────────────────

    /// Stable entity ID (stored in the handle). O(1).
    pub fn id(&self) -> usize {
        self.block_id
    }

    /// Character offset of this block's start in the document. O(log n)
    /// via the rope index for rope-clean documents; O(1) read of the
    /// stored field for tabled documents.
    pub fn position(&self) -> usize {
        let inner = self.doc.lock();
        let Some(mut dto) = block_commands::get_block(&inner.ctx, &(self.block_id as u64))
            .ok()
            .flatten()
        else {
            return 0;
        };
        let store = inner.ctx.db_context.get_store();
        crate::inner::refresh_block_position(&mut dto, store);
        to_usize(dto.document_position)
    }

    /// Global 0-indexed block number. **O(n)**: requires scanning all blocks
    /// sorted by `document_position`. Prefer [`id()`](TextBlock::id) for
    /// identity and [`position()`](TextBlock::position) for ordering.
    pub fn block_number(&self) -> usize {
        let inner = self.doc.lock();
        compute_block_number(&inner, self.block_id as u64)
    }

    /// The next block in document order. **O(n)**.
    /// Returns `None` if this is the last block.
    pub fn next(&self) -> Option<TextBlock> {
        let inner = self.doc.lock();
        let all_blocks = block_commands::get_all_block(&inner.ctx).ok()?;
        let mut sorted: Vec<_> = all_blocks.into_iter().collect();
        let store = inner.ctx.db_context.get_store();
        crate::inner::refresh_block_positions(&mut sorted, store);
        sorted.sort_by_key(|b| b.document_position);
        let idx = sorted.iter().position(|b| b.id == self.block_id as u64)?;
        sorted.get(idx + 1).map(|b| TextBlock {
            doc: Arc::clone(&self.doc),
            block_id: b.id as usize,
        })
    }

    /// The previous block in document order. **O(n)**.
    /// Returns `None` if this is the first block.
    pub fn previous(&self) -> Option<TextBlock> {
        let inner = self.doc.lock();
        let all_blocks = block_commands::get_all_block(&inner.ctx).ok()?;
        let mut sorted: Vec<_> = all_blocks.into_iter().collect();
        let store = inner.ctx.db_context.get_store();
        crate::inner::refresh_block_positions(&mut sorted, store);
        sorted.sort_by_key(|b| b.document_position);
        let idx = sorted.iter().position(|b| b.id == self.block_id as u64)?;
        if idx == 0 {
            return None;
        }
        sorted.get(idx - 1).map(|b| TextBlock {
            doc: Arc::clone(&self.doc),
            block_id: b.id as usize,
        })
    }

    // ── Structural Context ───────────────────────────────────

    /// Parent frame. O(1).
    pub fn frame(&self) -> TextFrame {
        let inner = self.doc.lock();
        let frame_id = find_parent_frame(&inner, self.block_id as u64);
        TextFrame {
            doc: Arc::clone(&self.doc),
            frame_id: frame_id.map(|id| id as usize).unwrap_or(0),
        }
    }

    /// If inside a table cell, returns table and cell coordinates.
    ///
    /// Finds the block's parent frame, then checks if any table cell
    /// references that frame as its `cell_frame`. If so, identifies the
    /// owning table.
    pub fn table_cell(&self) -> Option<TableCellRef> {
        let inner = self.doc.lock();
        let frame_id = find_parent_frame(&inner, self.block_id as u64)?;

        // Check if this frame is referenced as a cell_frame by any table cell.
        // First try the fast path: if the frame has a `table` field, use it.
        let frame_dto = frame_commands::get_frame(&inner.ctx, &frame_id)
            .ok()
            .flatten()?;

        if let Some(table_entity_id) = frame_dto.table {
            // This frame is a table anchor frame (not a cell frame).
            // Anchor frames don't contain blocks directly — cell frames do.
            // So this path shouldn't match, but check cells just in case.
            let table_dto =
                frontend::commands::table_commands::get_table(&inner.ctx, &{ table_entity_id })
                    .ok()
                    .flatten()?;
            for &cell_id in &table_dto.cells {
                if let Some(cell_dto) =
                    frontend::commands::table_cell_commands::get_table_cell(&inner.ctx, &{
                        cell_id
                    })
                    .ok()
                    .flatten()
                    && cell_dto.cell_frame == Some(frame_id)
                {
                    return Some(TableCellRef {
                        table: TextTable {
                            doc: Arc::clone(&self.doc),
                            table_id: table_entity_id as usize,
                        },
                        row: to_usize(cell_dto.row),
                        column: to_usize(cell_dto.column),
                    });
                }
            }
        }

        // Slow path: this frame has no `table` field (cell frames don't).
        // Scan all tables to find if any cell references this frame.
        let all_tables =
            frontend::commands::table_commands::get_all_table(&inner.ctx).unwrap_or_default();
        for table_dto in &all_tables {
            for &cell_id in &table_dto.cells {
                if let Some(cell_dto) =
                    frontend::commands::table_cell_commands::get_table_cell(&inner.ctx, &{
                        cell_id
                    })
                    .ok()
                    .flatten()
                    && cell_dto.cell_frame == Some(frame_id)
                {
                    return Some(TableCellRef {
                        table: TextTable {
                            doc: Arc::clone(&self.doc),
                            table_id: table_dto.id as usize,
                        },
                        row: to_usize(cell_dto.row),
                        column: to_usize(cell_dto.column),
                    });
                }
            }
        }

        None
    }

    // ── Formatting ──────────────────────────────────────────

    /// Block format (alignment, margins, indent, heading level, marker, tabs). O(1).
    pub fn block_format(&self) -> BlockFormat {
        let inner = self.doc.lock();
        block_commands::get_block(&inner.ctx, &(self.block_id as u64))
            .ok()
            .flatten()
            .map(|b| BlockFormat::from(&b))
            .unwrap_or_default()
    }

    /// Character format at a block-relative character offset. **O(k)**
    /// where k = format runs + image anchors in this block.
    ///
    /// Returns the [`TextFormat`] of the fragment containing the given
    /// offset. Returns `None` if the offset is out of range or the
    /// block has no fragments.
    pub fn char_format_at(&self, offset: usize) -> Option<TextFormat> {
        let inner = self.doc.lock();
        let fragments = build_fragments(&inner, self.block_id as u64);
        for frag in &fragments {
            match frag {
                FragmentContent::Text {
                    format,
                    offset: frag_offset,
                    length,
                    ..
                } => {
                    if offset >= *frag_offset && offset < frag_offset + length {
                        return Some(format.clone());
                    }
                }
                FragmentContent::Image {
                    format,
                    offset: frag_offset,
                    ..
                }
                | FragmentContent::FootnoteReference {
                    format,
                    offset: frag_offset,
                    ..
                } => {
                    if offset == *frag_offset {
                        return Some(format.clone());
                    }
                }
            }
        }
        None
    }

    // ── Fragments ───────────────────────────────────────────

    /// Shaping-input fragments: base formatting plus any *metric-affecting*
    /// syntax highlights (bold / italic / size / family / spacing). This is
    /// what the layout engine shapes. **Paint-only highlights (colors,
    /// underline decorations) are NOT merged here** — they are kept separate
    /// in [`BlockSnapshot::paint_highlights`](crate::BlockSnapshot::paint_highlights)
    /// as a post-shape recolor overlay, so the shaping input stays stable
    /// across paint-only highlight changes. For the fully-merged *visual*
    /// fragments, use [`display_fragments`](Self::display_fragments).
    ///
    /// O(k) where k = format runs + image anchors in this block.
    pub fn fragments(&self) -> Vec<FragmentContent> {
        let inner = self.doc.lock();
        build_fragments(&inner, self.block_id as u64)
    }

    /// Fragments as they should be *displayed*: base formatting with **all**
    /// active syntax highlights merged in, including paint-only ones. This is
    /// the "what it looks like" view — useful for a non-optimized renderer,
    /// for accessibility, or for tests. The optimized layout path instead uses
    /// [`fragments`](Self::fragments) (shaping input) plus the separate
    /// [`BlockSnapshot::paint_highlights`](crate::BlockSnapshot::paint_highlights)
    /// overlay. Equivalent to the pre-overlay behaviour of `fragments()`.
    pub fn display_fragments(&self) -> Vec<FragmentContent> {
        let inner = self.doc.lock();
        let fragments = build_raw_fragments(
            &inner,
            self.block_id as u64,
            None,
            &SnapshotLookups::default(),
        );
        // The fully-merged visual view: every session, regardless of the paint-vs-metric
        // split the optimized path draws on.
        let spans = crate::highlight::merged_spans_for_block(
            &inner,
            self.block_id,
            &crate::highlight::HighlightMask::ALL,
        );
        if !spans.is_empty() {
            return crate::highlight::merge_highlight_spans(fragments, &spans);
        }
        fragments
    }

    // ── Addressable Inline Pieces ─────────────────────────────

    /// The block's inline pieces (text runs, images, footnote references), addressed in
    /// the document's own addressable character space rather than block-relative — see
    /// [`AddressablePiece`]. **O(k)** where k = format runs + image anchors + footnote
    /// references in this block.
    ///
    /// Reach for this instead of [`fragments`](Self::fragments) /
    /// [`display_fragments`](Self::display_fragments) — whose `offset` is block-relative,
    /// fine for the layout engine that consumes them one block at a time — whenever a piece
    /// boundary needs comparing against something addressed in the *document's* own space:
    /// a comment's stored character range, a
    /// [`TextDocument::find_all`](crate::TextDocument::find_all) match position,
    /// another block's [`position()`](Self::position).
    ///
    /// Thin wrapper over [`common::format_runs_query::addressable_inline_pieces_for_block`]
    /// — the same accessor a DOCX/ODT export writer reaches for below the public API, so a
    /// piece boundary computed here and one computed there can never disagree.
    pub fn addressable_inline_pieces(&self) -> Vec<AddressablePiece> {
        let inner = self.doc.lock();
        let Some(block_dto) = block_commands::get_block(&inner.ctx, &(self.block_id as u64))
            .ok()
            .flatten()
        else {
            return Vec::new();
        };
        let entity: common::entities::Block = block_dto.into();
        let store = inner.ctx.db_context.get_store();
        let plain_text = common::database::rope_helpers::block_content_via_store(&entity, store);
        common::format_runs_query::addressable_inline_pieces_for_block(store, &entity, &plain_text)
            .into_iter()
            .map(|p| AddressablePiece {
                start: to_usize(p.start as i64),
                end: to_usize(p.end as i64),
                content: p.content,
                format: TextFormat::from(&p.format),
            })
            .collect()
    }

    // ── List Membership ─────────────────────────────────────

    /// List this block belongs to. O(1).
    pub fn list(&self) -> Option<TextList> {
        let inner = self.doc.lock();
        let block_dto = block_commands::get_block(&inner.ctx, &(self.block_id as u64))
            .ok()
            .flatten()?;
        let list_id = block_dto.list?;
        Some(TextList {
            doc: Arc::clone(&self.doc),
            list_id: list_id as usize,
        })
    }

    /// 0-based position within its list. **O(n)** where n = total blocks.
    pub fn list_item_index(&self) -> Option<usize> {
        let inner = self.doc.lock();
        let block_dto = block_commands::get_block(&inner.ctx, &(self.block_id as u64))
            .ok()
            .flatten()?;
        let list_id = block_dto.list?;
        Some(compute_list_item_index(
            &inner,
            list_id,
            self.block_id as u64,
        ))
    }

    // ── Snapshot ─────────────────────────────────────────────

    /// All layout-relevant data in one lock acquisition. O(k+n).
    pub fn snapshot(&self) -> BlockSnapshot {
        let inner = self.doc.lock();
        build_block_snapshot(
            &inner,
            self.block_id as u64,
            crate::highlight::SnapshotHighlights {
                kind: inner.highlight_kind,
                mask: &crate::highlight::HighlightMask::ALL,
                suppress_paint: false,
            },
        )
        .unwrap_or_else(|| BlockSnapshot {
            block_id: self.block_id,
            position: 0,
            length: 0,
            text: String::new(),
            fragments: Vec::new(),
            block_format: BlockFormat::default(),
            list_info: None,
            parent_frame_id: None,
            table_cell: None,
            paint_highlights: Vec::new(),
        })
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Internal helpers (called while lock is held)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Find the parent frame of a block by scanning all frames.
pub(crate) fn find_parent_frame(inner: &TextDocumentInner, block_id: u64) -> Option<EntityId> {
    whole_document_read();
    let all_frames = frame_commands::get_all_frame(&inner.ctx).ok()?;
    let block_entity_id = block_id as EntityId;
    for frame in &all_frames {
        if frame.blocks.contains(&block_entity_id) {
            return Some(frame.id as EntityId);
        }
    }
    None
}

/// O(1) fast check used by the snapshot hot path: returns true iff the
/// store has zero table entities. Used to skip the expensive
/// `find_table_cell_context` walks for documents that have no tables
/// (e.g. typical markdown documents in an editor).
fn document_has_no_tables(inner: &TextDocumentInner) -> bool {
    inner.ctx.db_context.get_store().tables.read().is_empty()
}

/// Find table cell context for a block (snapshot-friendly, no live handles).
/// Returns `None` if the block is not inside a table cell.
fn find_table_cell_context(inner: &TextDocumentInner, block_id: u64) -> Option<TableCellContext> {
    // Fast exit: a doc with no tables can't have any cell-bound blocks.
    // Avoids per-block `get_all_frame` + `get_all_table` walks during
    // snapshot_flow, which is called per editor pane on every keystroke.
    if document_has_no_tables(inner) {
        return None;
    }
    whole_document_read();
    let frame_id = find_parent_frame(inner, block_id)?;

    let frame_dto = frame_commands::get_frame(&inner.ctx, &frame_id)
        .ok()
        .flatten()?;

    // Fast path: anchor frame with `table` field set
    if let Some(table_entity_id) = frame_dto.table {
        let table_dto =
            frontend::commands::table_commands::get_table(&inner.ctx, &{ table_entity_id })
                .ok()
                .flatten()?;
        for &cell_id in &table_dto.cells {
            if let Some(cell_dto) =
                frontend::commands::table_cell_commands::get_table_cell(&inner.ctx, &{ cell_id })
                    .ok()
                    .flatten()
                && cell_dto.cell_frame == Some(frame_id)
            {
                return Some(TableCellContext {
                    table_id: table_entity_id as usize,
                    row: to_usize(cell_dto.row),
                    column: to_usize(cell_dto.column),
                });
            }
        }
    }

    // Slow path: scan all tables for a cell referencing this frame
    let all_tables =
        frontend::commands::table_commands::get_all_table(&inner.ctx).unwrap_or_default();
    for table_dto in &all_tables {
        for &cell_id in &table_dto.cells {
            if let Some(cell_dto) =
                frontend::commands::table_cell_commands::get_table_cell(&inner.ctx, &{ cell_id })
                    .ok()
                    .flatten()
                && cell_dto.cell_frame == Some(frame_id)
            {
                return Some(TableCellContext {
                    table_id: table_dto.id as usize,
                    row: to_usize(cell_dto.row),
                    column: to_usize(cell_dto.column),
                });
            }
        }
    }

    None
}

/// Whole-document facts a block snapshot reads, each worked out at most once per snapshot
/// and then shared by every block it covers.
///
/// A list item's number, the frame that lists a block, the table cell it sits in, what a
/// footnote reference prints and the document's default language are facts about the
/// whole document: finding one for a single block walks every block, frame, table or note
/// in the store. A flow snapshot used to repeat that walk for every block, so its cost grew
/// with the square of the text: opening a long document holding a list, a table or
/// footnotes froze the editor for seconds, and every keystroke that took a new snapshot
/// paid it again. The answers here are the ones the per-block walks give.
///
/// The three facts that differ from block to block (its parent frame, its table cell, its
/// list number) are indexed only once a second block asks: see [`OnSecondAsk`]. The other
/// three are one value for the whole document, kept from the first ask.
///
/// Lives for one hold of the document lock, like the snapshot it serves.
#[derive(Default)]
pub(crate) struct SnapshotLookups {
    frames: OnSecondAsk<FrameLookup>,
    cells: OnSecondAsk<CellLookup>,
    list_item_indices: OnSecondAsk<HashMap<(EntityId, EntityId), usize>>,
    default_language: OnceCell<Option<String>>,
    footnote_markers: OnceCell<HashMap<String, String>>,
    self_footnote_numbers: OnceCell<HashMap<String, usize>>,
}

/// An index over the whole document, built only once a second question needs it.
///
/// The first question is answered by the walk that serves one block, which costs less than
/// building the index, so a snapshot of a single block (the incremental relayout path, a
/// code editor's visible rows) costs what it did before. A second question means more are
/// coming, and the index then answers every one of them for the price of one more walk.
struct OnSecondAsk<T> {
    asked: Cell<bool>,
    index: OnceCell<T>,
}

impl<T> Default for OnSecondAsk<T> {
    fn default() -> Self {
        Self {
            asked: Cell::new(false),
            index: OnceCell::new(),
        }
    }
}

impl<T> OnSecondAsk<T> {
    /// `None` on the first ask, which the caller answers with its one-block walk; the
    /// index, built with `build` if need be, on every later one.
    fn get(&self, build: impl FnOnce() -> T) -> Option<&T> {
        if let Some(index) = self.index.get() {
            return Some(index);
        }
        if !self.asked.replace(true) {
            return None;
        }
        Some(self.index.get_or_init(build))
    }

    /// The index, built with `build` if need be, whatever was asked before: for a question
    /// that already had its own first ask.
    fn force(&self, build: impl FnOnce() -> T) -> &T {
        self.asked.set(true);
        self.index.get_or_init(build)
    }
}

/// Mark one read of a whole-document fact on the snapshot path: a walk of every block,
/// frame, table or note, or a copy of a document-wide list. Unit tests count them, which is
/// how `tests::a_flow_snapshot_reads_each_whole_document_fact_a_fixed_number_of_times`
/// guards the snapshot's cost in the debug builds CI tests with, where timing it cannot.
fn whole_document_read() {
    #[cfg(test)]
    tests::WHOLE_DOCUMENT_READS.with(|reads| reads.set(reads.get() + 1));
}

/// Every frame's blocks and table, from one pass over the frames.
struct FrameLookup {
    /// The frame [`find_parent_frame`] names for each block: the first frame, in the
    /// store's order, whose `blocks` list it.
    parent_of_block: HashMap<EntityId, EntityId>,
    /// Each frame's `table` field.
    table_of_frame: HashMap<EntityId, Option<EntityId>>,
}

impl FrameLookup {
    fn build(inner: &TextDocumentInner) -> Self {
        whole_document_read();
        let all_frames = frame_commands::get_all_frame(&inner.ctx).unwrap_or_default();
        let mut parent_of_block = HashMap::new();
        let mut table_of_frame = HashMap::with_capacity(all_frames.len());
        for frame in &all_frames {
            for &block_id in &frame.blocks {
                parent_of_block.entry(block_id).or_insert(frame.id);
            }
            table_of_frame.insert(frame.id, frame.table);
        }
        Self {
            parent_of_block,
            table_of_frame,
        }
    }
}

/// Every table cell's coordinates, from one pass over the tables, keyed the two ways
/// [`find_table_cell_context`] searches for a cell frame.
struct CellLookup {
    tables: HashSet<EntityId>,
    /// The first cell in `table`'s own list whose frame is `frame`, by `(table, frame)`.
    in_table: HashMap<(EntityId, EntityId), TableCellContext>,
    /// The first cell whose frame is `frame`, tables in the store's order, by frame.
    anywhere: HashMap<EntityId, TableCellContext>,
}

impl CellLookup {
    fn build(inner: &TextDocumentInner) -> Self {
        whole_document_read();
        let all_tables =
            frontend::commands::table_commands::get_all_table(&inner.ctx).unwrap_or_default();
        let mut tables = HashSet::with_capacity(all_tables.len());
        let mut in_table = HashMap::new();
        let mut anywhere = HashMap::new();
        for table_dto in &all_tables {
            tables.insert(table_dto.id);
            for &cell_id in &table_dto.cells {
                let Some(cell_dto) =
                    frontend::commands::table_cell_commands::get_table_cell(&inner.ctx, &{
                        cell_id
                    })
                    .ok()
                    .flatten()
                else {
                    continue;
                };
                let Some(frame_id) = cell_dto.cell_frame else {
                    continue;
                };
                let context = TableCellContext {
                    table_id: table_dto.id as usize,
                    row: to_usize(cell_dto.row),
                    column: to_usize(cell_dto.column),
                };
                in_table
                    .entry((table_dto.id, frame_id))
                    .or_insert_with(|| context.clone());
                anywhere.entry(frame_id).or_insert(context);
            }
        }
        Self {
            tables,
            in_table,
            anywhere,
        }
    }
}

impl SnapshotLookups {
    /// The frame whose `blocks` list `block_id`: [`find_parent_frame`]'s answer.
    fn parent_frame(&self, inner: &TextDocumentInner, block_id: u64) -> Option<EntityId> {
        match self.frames.get(|| FrameLookup::build(inner)) {
            Some(frames) => frames.parent_of_block.get(&block_id).copied(),
            None => find_parent_frame(inner, block_id),
        }
    }

    /// The table cell `block_id` sits in: [`find_table_cell_context`]'s answer.
    fn table_cell_context(
        &self,
        inner: &TextDocumentInner,
        block_id: u64,
    ) -> Option<TableCellContext> {
        if document_has_no_tables(inner) {
            return None;
        }
        let Some(cells) = self.cells.get(|| CellLookup::build(inner)) else {
            return find_table_cell_context(inner, block_id);
        };
        let frames = self.frames.force(|| FrameLookup::build(inner));
        let frame_id = *frames.parent_of_block.get(&block_id)?;
        // A frame naming a table is looked for in that table first, and a table it names
        // that no longer exists ends the search, as in the one-block walk.
        if let Some(Some(table_id)) = frames.table_of_frame.get(&frame_id) {
            if !cells.tables.contains(table_id) {
                return None;
            }
            if let Some(context) = cells.in_table.get(&(*table_id, frame_id)) {
                return Some(context.clone());
            }
        }
        cells.anywhere.get(&frame_id).cloned()
    }

    /// `block_id`'s 0-based index among `list_id`'s blocks: [`compute_list_item_index`]'s
    /// answer.
    fn list_item_index(
        &self,
        inner: &TextDocumentInner,
        list_id: EntityId,
        block_id: u64,
    ) -> usize {
        let build = || {
            whole_document_read();
            let mut all_blocks = block_commands::get_all_block(&inner.ctx).unwrap_or_default();
            let store = inner.ctx.db_context.get_store();
            crate::inner::refresh_block_positions(&mut all_blocks, store);
            // Each list's blocks in the order the store gives them, then sorted stably by
            // position: the order the one-block walk sorts each list into.
            let mut members: HashMap<EntityId, Vec<(i64, EntityId)>> = HashMap::new();
            for block in &all_blocks {
                if let Some(list) = block.list {
                    members
                        .entry(list)
                        .or_default()
                        .push((block.document_position, block.id));
                }
            }
            let mut indices = HashMap::new();
            for (list, mut blocks) in members {
                blocks.sort_by_key(|&(position, _)| position);
                for (index, (_, id)) in blocks.into_iter().enumerate() {
                    indices.entry((list, id)).or_insert(index);
                }
            }
            indices
        };
        match self.list_item_indices.get(build) {
            Some(indices) => indices.get(&(list_id, block_id)).copied().unwrap_or(0),
            None => compute_list_item_index(inner, list_id, block_id),
        }
    }

    /// The language a block that sets none inherits: the document's default.
    fn default_language(&self, inner: &TextDocumentInner) -> Option<String> {
        self.default_language
            .get_or_init(|| {
                whole_document_read();
                document_commands::get_document(&inner.ctx, &inner.document_id)
                    .ok()
                    .flatten()
                    .and_then(|d| d.default_language)
            })
            .clone()
    }

    /// What the host says each footnote label prints.
    fn footnote_markers(&self, inner: &TextDocumentInner) -> &HashMap<String, String> {
        self.footnote_markers.get_or_init(|| {
            whole_document_read();
            inner
                .ctx
                .db_context
                .get_store()
                .footnote_markers
                .read()
                .clone()
        })
    }

    /// Every footnote label's number in the document's own reading order.
    fn self_footnote_numbers(&self, inner: &TextDocumentInner) -> &HashMap<String, usize> {
        self.self_footnote_numbers
            .get_or_init(|| document_self_footnote_numbers(inner.ctx.db_context.get_store()))
    }
}

/// Compute 0-indexed block number by scanning all blocks sorted by document_position.
fn compute_block_number(inner: &TextDocumentInner, block_id: u64) -> usize {
    let mut all_blocks = block_commands::get_all_block(&inner.ctx).unwrap_or_default();
    let store = inner.ctx.db_context.get_store();
    crate::inner::refresh_block_positions(&mut all_blocks, store);
    let mut sorted: Vec<_> = all_blocks.iter().collect();
    sorted.sort_by_key(|b| b.document_position);
    sorted.iter().position(|b| b.id == block_id).unwrap_or(0)
}

/// Build fragments for a block from its format runs and image anchors,
/// with highlight spans merged in when a syntax highlighter is attached.
pub(crate) fn build_fragments(inner: &TextDocumentInner, block_id: u64) -> Vec<FragmentContent> {
    build_fragments_with_text(
        inner,
        block_id,
        None,
        crate::highlight::SnapshotHighlights {
            kind: inner.highlight_kind,
            mask: &crate::highlight::HighlightMask::ALL,
            suppress_paint: false,
        },
        &SnapshotLookups::default(),
    )
}

/// Like `build_fragments` but accepts a pre-materialized block text to
/// avoid the double `block_content_via_store` allocation when the
/// caller (e.g. `build_block_snapshot_with_position_and_parent`)
/// already has the text. Per-block snapshot cost halves for typing in
/// a multi-block document.
pub(crate) fn build_fragments_with_text(
    inner: &TextDocumentInner,
    block_id: u64,
    prefetched_text: Option<&str>,
    hl: crate::highlight::SnapshotHighlights,
    lookups: &SnapshotLookups,
) -> Vec<FragmentContent> {
    let fragments = build_raw_fragments(inner, block_id, prefetched_text, lookups);

    // Only merge highlights into the shaping input when the effective kind is
    // metric-affecting. Paint-only sessions keep fragments as BASE and carry their spans
    // separately in `BlockSnapshot::paint_highlights`, so the engine can recolor without
    // reshaping. A "without highlights" (empty-mask) snapshot resolves to `kind == None`,
    // forcing base fragments regardless of the live sessions. See `HighlighterKind`.
    if hl.kind == crate::highlight::HighlighterKind::Metric {
        let spans = crate::highlight::merged_spans_for_block(inner, block_id as usize, hl.mask);
        if !spans.is_empty() {
            return crate::highlight::merge_highlight_spans(fragments, &spans);
        }
    }

    fragments
}

/// Every footnote label's number, counted the document numbers its own
/// references in reading order — blocks by `document_position`, then within a
/// block by byte offset, first appearance of a label wins, 1-based. A
/// definition frame's own blocks are excluded from the walk (a note that
/// itself cites another note must not number the inner reference by where the
/// *definition* sits).
///
/// This is `document_io::footnotes::Footnotes::build`'s `numbers` computation,
/// duplicated rather than shared: `document_io` is a backend/export crate this
/// one (`public_api`, i.e. the live editor) does not — and should not —
/// depend on, since it pulls in every exporter for what is, here, a handful
/// of lines over `common::database::Store`, which both crates already depend
/// on directly. If the numbering rule ever changes, it has to change in both
/// places — grep `Footnotes::build` in `document_io` before touching this.
///
/// The fallback tier `TextDocument::set_footnote_markers`'s doc promises
/// ("Leave it unset and the document numbers its own references in reading
/// order, which is right when the document *is* the whole text") and that
/// `document_io::Footnotes::marker` actually implements as ITS fallback
/// before finally falling back to the raw label — the exact tier
/// `build_raw_fragments` was missing, which is why a host that never calls
/// `set_footnote_markers` (the documented, supported "unset" case — every
/// host does not manage its own note numbering the way Skribisto does) saw
/// the live editor draw raw labels while every export numbered correctly.
fn document_self_footnote_numbers(
    store: &common::database::Store,
) -> std::collections::HashMap<String, usize> {
    whole_document_read();
    let definition_blocks: std::collections::HashSet<common::types::EntityId> = store
        .frames
        .read()
        .values()
        .filter(|f| f.footnote_label.is_some())
        .flat_map(|f| f.child_order.iter().copied())
        .filter(|child| *child > 0)
        .map(|child| child as common::types::EntityId)
        .collect();

    let mut ordered: Vec<(i64, common::types::EntityId)> = store
        .blocks
        .read()
        .values()
        .filter(|b| !definition_blocks.contains(&b.id))
        .map(|b| (b.document_position, b.id))
        .collect();
    ordered.sort_unstable();

    let refs = store.block_footnote_refs.read();
    let mut numbers: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut next = 1usize;
    for (_, block_id) in ordered {
        let Some(anchors) = refs.get(&block_id) else {
            continue;
        };
        let mut in_block: Vec<_> = anchors.iter().collect();
        in_block.sort_by_key(|a| a.byte_offset);
        for anchor in in_block {
            numbers.entry(anchor.label.clone()).or_insert_with(|| {
                let n = next;
                next += 1;
                n
            });
        }
    }
    numbers
}

/// Build raw fragments from the block's format_runs and block_images
/// tables (Phase 1 of the rope migration). Reads the per-block plain_text
/// from the Block DTO and uses the format-run byte ranges + image
/// anchors to produce a stream of `FragmentContent::{Text, Image}`
/// values in document order.
///
/// `element_id` is synthesized from (block_id, byte_start) via
/// `synth_element_id`. Synthesized ids are stable for the same
/// (block, byte_start) pair and never collide with real entity ids
/// (top bit set).
///
/// Uncovered byte ranges between runs (or before the first run / after
/// the last) emit Text fragments with `TextFormat::default()` — the
/// "no character formatting" case.
fn build_raw_fragments(
    inner: &TextDocumentInner,
    block_id: u64,
    prefetched_text: Option<&str>,
    lookups: &SnapshotLookups,
) -> Vec<FragmentContent> {
    let _block_dto = match block_commands::get_block(&inner.ctx, &block_id)
        .ok()
        .flatten()
    {
        Some(b) => b,
        None => return Vec::new(),
    };

    let plain_owned;
    let plain: &str = match prefetched_text {
        Some(t) => t,
        None => {
            let entity: common::entities::Block = _block_dto.clone().into();
            plain_owned = common::database::rope_helpers::block_content_via_store(
                &entity,
                inner.ctx.db_context.get_store(),
            );
            &plain_owned
        }
    };

    let (runs, images, notes) = {
        let store = inner.ctx.db_context.get_store();
        let runs: Vec<FormatRun> = store
            .format_runs
            .read()
            .get(&block_id)
            .cloned()
            .unwrap_or_default();
        let images: Vec<ImageAnchor> = store
            .block_images
            .read()
            .get(&block_id)
            .cloned()
            .unwrap_or_default();
        let notes = store
            .block_footnote_refs
            .read()
            .get(&block_id)
            .cloned()
            .unwrap_or_default();
        (runs, images, notes)
    };

    // One shared weave of runs + anchors (see
    // `common::format_runs::merge_runs_and_anchors`). This used to be a second,
    // hand-written copy of that algorithm, and the two disagreed on an image
    // sitting exactly on a run boundary.
    let anchors = frontend::common::format_runs::block_anchors(&images, &notes);
    let pieces = frontend::common::format_runs::merge_runs_and_anchors(plain, &runs, &anchors);

    let mut fragments = Vec::with_capacity(pieces.len());
    let mut char_offset: usize = 0;

    for piece in pieces {
        match piece {
            frontend::common::format_runs::InlinePiece::Text { start, end, format } => {
                let text = &plain[start as usize..end as usize];
                let length = text.chars().count();
                let word_starts = compute_word_starts(text);
                fragments.push(FragmentContent::Text {
                    text: text.to_string(),
                    format: format.map(TextFormat::from).unwrap_or_default(),
                    offset: char_offset,
                    length,
                    element_id: synth_element_id(block_id, start),
                    word_starts,
                });
                char_offset += length;
            }
            frontend::common::format_runs::InlinePiece::FootnoteRef(note) => {
                fragments.push(FragmentContent::FootnoteReference {
                    label: note.label.clone(),
                    // What the host says this note prints — a number, usually.
                    //
                    // It has to come from outside: which note this is depends on
                    // how many references precede it in the *document*, and a
                    // host that owns note storage (Skribisto keeps bodies in its
                    // own store) knows more still — that this text is chapter
                    // five of a book, and where its numbering starts.
                    //
                    // Falls back, in order: the host's override map; then this
                    // document's OWN reading-order count (`document_self_
                    // footnote_numbers` — the tier `document_io::Footnotes::
                    // marker` implements and `set_footnote_markers` documents,
                    // "right when the document *is* the whole text"); then,
                    // only for a reference that resolves in neither, the raw
                    // label — visible and traceable rather than a blank
                    // marker, matching `Footnotes::marker`'s own last resort.
                    //
                    // Both maps are whole-document facts, read once per
                    // snapshot (see `SnapshotLookups`), and the reading-order
                    // count only when a reference has no marker: the common
                    // case (a host that manages its own numbering, like
                    // Skribisto, always pushes a full marker map before any
                    // document paints) never runs that scan.
                    marker: lookups
                        .footnote_markers(inner)
                        .get(&note.label)
                        .cloned()
                        .unwrap_or_else(|| {
                            lookups
                                .self_footnote_numbers(inner)
                                .get(&note.label)
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| note.label.clone())
                        }),
                    format: TextFormat::from(&note.format),
                    offset: char_offset,
                    element_id: synth_element_id(block_id, note.byte_offset),
                });
                // One logical character, like an image — the U+FFFC in the rope
                // holds its position.
                char_offset += 1;
            }
            frontend::common::format_runs::InlinePiece::Image(img) => {
                fragments.push(FragmentContent::Image {
                    name: img.name.clone(),
                    alt: img.alt.clone(),
                    width: img.width as u32,
                    height: img.height as u32,
                    quality: img.quality as u32,
                    format: TextFormat::from(&img.format),
                    offset: char_offset,
                    element_id: synth_element_id(block_id, img.byte_offset),
                });
                // An image contributes exactly one logical character and zero
                // bytes; the U+FFFC sentinel in the rope holds its position.
                char_offset += 1;
            }
        }
    }

    fragments
}

/// Compute character-index-based word starts for a text slice,
/// following Unicode Standard Annex #29. Returned indices are
/// positions within `text.chars()`, NOT byte offsets — matches
/// AccessKit's `word_starts` contract where each entry is an index
/// into `character_lengths`.
fn compute_word_starts(text: &str) -> Vec<u8> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut result = Vec::new();
    // `unicode_word_indices` yields (byte_offset, word_slice) for each
    // Unicode-word match. Convert each byte offset to a character
    // index by counting `char_indices` up to that offset.
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
        // Saturating cast — text runs longer than 255 chars get their
        // later word starts dropped. That's the AccessKit contract:
        // `word_starts` is Box<[u8]>. Runs longer than ~255 chars are
        // unusual for a single format run, and the first 255 word
        // starts cover the viewport almost always. Documented in the
        // plan.
        if let Ok(idx) = u8::try_from(char_idx) {
            result.push(idx);
        } else {
            break;
        }
    }
    result
}

/// Compute 0-based index of a block within its list.
fn compute_list_item_index(inner: &TextDocumentInner, list_id: EntityId, block_id: u64) -> usize {
    whole_document_read();
    let mut all_blocks = block_commands::get_all_block(&inner.ctx).unwrap_or_default();
    let store = inner.ctx.db_context.get_store();
    crate::inner::refresh_block_positions(&mut all_blocks, store);
    let mut list_blocks: Vec<_> = all_blocks
        .iter()
        .filter(|b| b.list == Some(list_id))
        .collect();
    list_blocks.sort_by_key(|b| b.document_position);
    list_blocks
        .iter()
        .position(|b| b.id == block_id)
        .unwrap_or(0)
}

/// Format a list marker for the given item index.
pub(crate) fn format_list_marker(
    list_dto: &frontend::list::dtos::ListDto,
    item_index: usize,
) -> String {
    let number = item_index + 1; // 1-based for display
    let marker_body = match list_dto.style {
        ListStyle::Disc => "\u{2022}".to_string(),   // •
        ListStyle::Circle => "\u{25E6}".to_string(), // ◦
        ListStyle::Square => "\u{25AA}".to_string(), // ▪
        ListStyle::Decimal => format!("{number}"),
        ListStyle::LowerAlpha => {
            if number <= 26 {
                ((b'a' + (number as u8 - 1)) as char).to_string()
            } else {
                format!("{number}")
            }
        }
        ListStyle::UpperAlpha => {
            if number <= 26 {
                ((b'A' + (number as u8 - 1)) as char).to_string()
            } else {
                format!("{number}")
            }
        }
        ListStyle::LowerRoman => to_roman_lower(number),
        ListStyle::UpperRoman => to_roman_upper(number),
    };
    format!("{}{marker_body}{}", list_dto.prefix, list_dto.suffix)
}

fn to_roman_upper(mut n: usize) -> String {
    const VALUES: &[(usize, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut result = String::new();
    for &(val, sym) in VALUES {
        while n >= val {
            result.push_str(sym);
            n -= val;
        }
    }
    result
}

fn to_roman_lower(n: usize) -> String {
    to_roman_upper(n).to_lowercase()
}

/// Build a ListInfo for a block. Called while lock is held.
fn build_list_info(
    inner: &TextDocumentInner,
    block_dto: &frontend::block::dtos::BlockDto,
    lookups: &SnapshotLookups,
) -> Option<ListInfo> {
    let list_id = block_dto.list?;
    let list_dto = list_commands::get_list(&inner.ctx, &{ list_id })
        .ok()
        .flatten()?;

    let item_index = lookups.list_item_index(inner, list_id, block_dto.id);
    let marker = format_list_marker(&list_dto, item_index);

    Some(ListInfo {
        list_id: list_id as usize,
        style: list_dto.style.clone(),
        indent: list_dto.indent as u8,
        marker,
        item_index,
    })
}

/// Build a BlockSnapshot for a block. Called while lock is held.
pub(crate) fn build_block_snapshot(
    inner: &TextDocumentInner,
    block_id: u64,
    hl: crate::highlight::SnapshotHighlights,
) -> Option<BlockSnapshot> {
    build_block_snapshot_with_position_and_parent(
        inner,
        block_id,
        None,
        None,
        hl,
        &SnapshotLookups::default(),
    )
}

/// Build a BlockSnapshot, optionally overriding the position with a computed value.
/// When `computed_position` is Some, it's used instead of `block_dto.document_position`
/// (which may be stale if position updates are deferred).
///
/// `lookups` carries the whole-document facts across the blocks of one snapshot:
/// [`SnapshotLookups::default`] for a snapshot of this block alone.
pub(crate) fn build_block_snapshot_with_position(
    inner: &TextDocumentInner,
    block_id: u64,
    computed_position: Option<usize>,
    hl: crate::highlight::SnapshotHighlights,
    lookups: &SnapshotLookups,
) -> Option<BlockSnapshot> {
    build_block_snapshot_with_position_and_parent(
        inner,
        block_id,
        computed_position,
        None,
        hl,
        lookups,
    )
}

/// Build a BlockSnapshot with an optional `parent_frame_hint`. When the
/// caller already knows which frame owns the block (e.g. snapshot_flow's
/// per-frame walk), passing it here skips the per-block `find_parent_frame`
/// call — which would otherwise fetch every Frame in the store on every
/// invocation. That walk was a major contributor to per-keystroke
/// editor lag.
///
/// `lookups` carries the whole-document facts (list numbers, table cells,
/// footnote markers, the default language) across the blocks of one
/// snapshot, so each is worked out once rather than once per block.
pub(crate) fn build_block_snapshot_with_position_and_parent(
    inner: &TextDocumentInner,
    block_id: u64,
    computed_position: Option<usize>,
    parent_frame_hint: Option<EntityId>,
    hl: crate::highlight::SnapshotHighlights,
    lookups: &SnapshotLookups,
) -> Option<BlockSnapshot> {
    let mut block_dto = block_commands::get_block(&inner.ctx, &block_id)
        .ok()
        .flatten()?;
    let store_for_pos = inner.ctx.db_context.get_store();
    crate::inner::refresh_block_position(&mut block_dto, store_for_pos);

    let mut block_format = BlockFormat::from(&block_dto);
    // Inherit the document-wide default language when the block sets none,
    // so hyphenation has a language for every block. The bridge still
    // falls back to English if this is also unset.
    if block_format.language.is_none() {
        block_format.language = lookups.default_language(inner);
    }
    let list_info = build_list_info(inner, &block_dto, lookups);

    let parent_frame_id = parent_frame_hint
        .or_else(|| lookups.parent_frame(inner, block_id))
        .map(|id| id as usize);
    let table_cell = lookups.table_cell_context(inner, block_id);

    // The flow-snapshot position MUST agree with the space the editing path
    // resolves cursor positions against. When the rope mirrors every block
    // (now true even with tables, since cell content is mirrored inline), the
    // rope is the single source of truth: its char order — including the
    // 1-char table-anchor sentinel — is what `find_block_at_char_position`
    // uses. So derive `position` from the rope-refreshed `document_position`
    // (set above) rather than the caller's running counter, which omits the
    // sentinel and would drift past every table. Only when the rope is NOT
    // authoritative (programmatically-inserted sub-frames whose blocks aren't
    // mirrored) do we fall back to the caller's computed running position.
    let position = if common::database::rope_helpers::rope_positions_match_flow(store_for_pos) {
        to_usize(block_dto.document_position)
    } else {
        computed_position.unwrap_or_else(|| to_usize(block_dto.document_position))
    };

    // Materialize the block text once and pass it to build_fragments
    // and into the snapshot's `text` field — saves one redundant rope
    // slice + String allocation per block per snapshot_flow call.
    let entity: common::entities::Block = block_dto.clone().into();
    let store = inner.ctx.db_context.get_store();
    let text = common::database::rope_helpers::block_content_via_store(&entity, store);
    let length = to_usize(common::database::rope_helpers::block_char_length(
        &entity, store,
    ));
    let fragments = build_fragments_with_text(inner, block_id, Some(&text), hl, lookups);

    // Paint-only sessions: emit the merged spans as a separate overlay (fragments stayed base
    // above). Metric / none: empty (highlights merged into fragments, or none). A "without
    // highlights" (empty-mask) snapshot resolves to `kind == None`, so this is empty
    // regardless of the live sessions.
    let paint_highlights =
        if hl.kind == crate::highlight::HighlighterKind::PaintOnly && !hl.suppress_paint {
            let spans = crate::highlight::merged_spans_for_block(inner, block_id as usize, hl.mask);
            crate::highlight::extract_paint_spans(&spans, length)
        } else {
            Vec::new()
        };

    Some(BlockSnapshot {
        block_id: block_id as usize,
        position,
        length,
        text,
        fragments,
        block_format,
        list_info,
        parent_frame_id,
        table_cell,
        paint_highlights,
    })
}

/// Build BlockSnapshots for all blocks in a frame, sorted by document_position.
/// `lookups` is shared with the rest of the snapshot this frame belongs to, if any.
pub(crate) fn build_blocks_snapshot_for_frame(
    inner: &TextDocumentInner,
    frame_id: u64,
    hl: crate::highlight::SnapshotHighlights,
    lookups: &SnapshotLookups,
) -> Vec<BlockSnapshot> {
    let frame_dto = match frame_commands::get_frame(&inner.ctx, &(frame_id as EntityId))
        .ok()
        .flatten()
    {
        Some(f) => f,
        None => return Vec::new(),
    };

    let mut block_dtos: Vec<_> = frame_dto
        .blocks
        .iter()
        .filter_map(|&id| {
            block_commands::get_block(&inner.ctx, &{ id })
                .ok()
                .flatten()
        })
        .collect();
    let store = inner.ctx.db_context.get_store();
    crate::inner::refresh_block_positions(&mut block_dtos, store);
    block_dtos.sort_by_key(|b| b.document_position);

    block_dtos
        .iter()
        .filter_map(|b| {
            build_block_snapshot_with_position_and_parent(inner, b.id, None, None, hl, lookups)
        })
        .collect()
}

/// Build BlockSnapshots with computed positions starting from `start_pos`.
///
/// Returns `(snapshots, running_pos_after_last_block)`.
/// Positions are computed sequentially from `start_pos` using each block's
/// `text_length`, matching the logic in `find_block_at_position_sequential`.
/// `lookups` is shared with the rest of the snapshot this frame belongs to.
pub(crate) fn build_blocks_snapshot_for_frame_with_positions(
    inner: &TextDocumentInner,
    frame_id: u64,
    start_pos: usize,
    hl: crate::highlight::SnapshotHighlights,
    lookups: &SnapshotLookups,
) -> (Vec<BlockSnapshot>, usize) {
    let frame_dto = match frame_commands::get_frame(&inner.ctx, &(frame_id as EntityId))
        .ok()
        .flatten()
    {
        Some(f) => f,
        None => return (Vec::new(), start_pos),
    };

    let mut block_dtos: Vec<_> = frame_dto
        .blocks
        .iter()
        .filter_map(|&id| {
            block_commands::get_block(&inner.ctx, &{ id })
                .ok()
                .flatten()
        })
        .collect();
    let store = inner.ctx.db_context.get_store();
    crate::inner::refresh_block_positions(&mut block_dtos, store);
    block_dtos.sort_by_key(|b| b.document_position);

    let mut running_pos = start_pos;
    let mut snapshots = Vec::with_capacity(block_dtos.len());
    for b in &block_dtos {
        if let Some(snap) =
            build_block_snapshot_with_position(inner, b.id, Some(running_pos), hl, lookups)
        {
            running_pos += snap.length + 1; // +1 for block separator
            snapshots.push(snap);
        }
    }
    (snapshots, running_pos)
}

#[cfg(test)]
mod tests {
    use crate::TextDocument;
    use crate::flow::FlowElementSnapshot;
    use std::cell::Cell;

    thread_local! {
        /// Whole-document reads [`super::whole_document_read`] marked on this thread.
        pub(super) static WHOLE_DOCUMENT_READS: Cell<usize> = const { Cell::new(0) };
    }

    /// A text of `paragraphs` paragraphs with every whole-document fact a block snapshot
    /// reads: footnotes, lists, tables and quotations spread through it.
    fn novel(paragraphs: usize) -> String {
        let mut text = String::new();
        for i in 0..paragraphs {
            if i % 7 == 3 {
                text.push_str(&format!("Paragraph {i}, with a note[^n{i}].\n\n"));
                text.push_str(&format!("[^n{i}]: Note {i}.\n\n"));
            } else if i % 17 == 5 {
                text.push_str(&format!("> Quoted paragraph {i}.\n\n"));
            } else {
                text.push_str(&format!("Paragraph {i} of the text.\n\n"));
            }
            if i % 11 == 4 {
                text.push_str(&format!("- item {i}\n- item {i}, second\n\n"));
            }
            if i % 13 == 6 {
                text.push_str(&format!("| a{i} | b |\n| c | d |\n\n"));
            }
        }
        text
    }

    /// Every block snapshot among `elements`, table cells and sub-frames included.
    fn all_blocks(elements: &[FlowElementSnapshot], out: &mut Vec<crate::flow::BlockSnapshot>) {
        for element in elements {
            match element {
                FlowElementSnapshot::Block(block) => out.push(block.clone()),
                FlowElementSnapshot::Table(table) => {
                    for cell in &table.cells {
                        out.extend(cell.blocks.iter().cloned());
                    }
                }
                FlowElementSnapshot::Frame(frame) => all_blocks(&frame.elements, out),
            }
        }
    }

    /// A flow snapshot works each block's whole-document facts out from indexes it shares
    /// across its blocks; a snapshot of one block works them out by walking the document.
    /// The two must give every block the same snapshot: list numbers across several lists,
    /// cells of tables in and out of quotations, cells holding several paragraphs and a
    /// list, notes numbered by the document, by the host, or partly by each.
    #[test]
    fn a_flow_snapshot_agrees_with_a_snapshot_of_each_block_alone() {
        let mixed = "# Title\n\nA note[^a] and another[^b].\n\n- one\n- two\n- three\n\n\
                     Between lists.\n\n1. first\n2. second\n\n> Quoted, with a note[^c].\n>\n\
                     > - quoted item\n> - quoted item two\n>\n> > nested quote\n\n\
                     | h1 | h2 |\n|----|----|\n| a | b[^a] |\n| c | d |\n\n\
                     - again one\n- again two\n\n> | q1 | q2 |\n> | q3 | q4 |\n\n\
                     Last paragraph[^d].\n\n[^a]: Note a, citing [^b].\n\n[^b]: Note b.\n\n\
                     [^c]: Note c.\n\n[^d]: Note d.\n";
        let html = "<p>Before.</p><table><tr><td><p>c1 p1</p><p>c1 p2</p></td><td><ul>\
                    <li>in cell</li><li>in cell 2</li></ul></td></tr><tr><td>plain</td><td>\
                    <p>x</p><p>y</p></td></tr></table><ol><li>o1</li><li>o2</li></ol>\
                    <blockquote><p>q</p><ul><li>qi</li></ul></blockquote><p>After.</p>";

        let from_djot = |text: &str| {
            let doc = TextDocument::new();
            doc.set_djot_sync(text).unwrap();
            doc
        };
        let numbered_by_the_document = from_djot(mixed);
        let partly_by_the_host = from_djot(mixed);
        partly_by_the_host.set_footnote_markers(
            [("a", "i"), ("c", "iii")]
                .into_iter()
                .map(|(label, marker)| (label.to_string(), marker.to_string()))
                .collect(),
        );
        partly_by_the_host.set_default_language("fr").unwrap();
        let with_cells_of_paragraphs = TextDocument::new();
        with_cells_of_paragraphs
            .set_html(html)
            .unwrap()
            .wait()
            .unwrap();
        let long = from_djot(&novel(120));

        for (label, doc) in [
            ("notes numbered by the document", &numbered_by_the_document),
            ("notes partly numbered by the host", &partly_by_the_host),
            ("cells holding paragraphs", &with_cells_of_paragraphs),
            ("a long text", &long),
        ] {
            let mut blocks = Vec::new();
            all_blocks(&doc.snapshot_flow().elements, &mut blocks);
            assert!(
                blocks.iter().any(|b| b.table_cell.is_some())
                    && blocks.iter().any(|b| b.list_info.is_some()),
                "{label}: the text holds table cells and list items"
            );
            for from_flow in blocks {
                let alone = doc
                    .block_by_id(from_flow.block_id)
                    .expect("a block of the flow exists")
                    .snapshot();
                assert_eq!(
                    alone, from_flow,
                    "{label}: block {} alone and in the flow",
                    from_flow.block_id
                );
            }
        }
    }

    /// Whole-document reads one flow snapshot of `doc` makes, and the snapshot.
    fn reads_for_one_snapshot(doc: &TextDocument) -> (usize, crate::flow::FlowSnapshot) {
        WHOLE_DOCUMENT_READS.with(|reads| reads.set(0));
        let flow = doc.snapshot_flow();
        (WHOLE_DOCUMENT_READS.with(Cell::get), flow)
    }

    /// A block's list number, its table cell, its footnote numbers and its default language
    /// are whole-document facts. A flow snapshot used to read each of them again for every
    /// block that needed it, so opening a long text holding lists, tables or footnotes cost
    /// time in proportion to the square of its length. It must read each one a fixed number
    /// of times, however long the text.
    #[test]
    fn a_flow_snapshot_reads_each_whole_document_fact_a_fixed_number_of_times() {
        let mut counts = Vec::new();
        for paragraphs in [60, 480] {
            let doc = TextDocument::new();
            doc.set_djot_sync(&novel(paragraphs)).unwrap();
            let (numbered_by_the_document, flow) = reads_for_one_snapshot(&doc);

            let blocks: Vec<_> = flow
                .elements
                .iter()
                .filter_map(|element| match element {
                    FlowElementSnapshot::Block(block) => Some(block),
                    _ => None,
                })
                .collect();
            assert!(
                blocks.iter().filter(|b| b.list_info.is_some()).count() > 4,
                "the text holds list items"
            );
            assert!(
                flow.elements
                    .iter()
                    .filter(|e| matches!(e, FlowElementSnapshot::Table(_)))
                    .count()
                    > 2,
                "the text holds tables"
            );
            assert!(
                flow.elements
                    .iter()
                    .any(|e| matches!(e, FlowElementSnapshot::Frame(_))),
                "the text holds quotations"
            );

            // The host numbering the notes itself, as Skribisto does.
            let markers = (0..paragraphs)
                .filter(|i| i % 7 == 3)
                .enumerate()
                .map(|(k, i)| (format!("n{i}"), (k + 1).to_string()))
                .collect();
            doc.set_footnote_markers(markers);
            let (numbered_by_the_host, _) = reads_for_one_snapshot(&doc);

            counts.push((paragraphs, numbered_by_the_document, numbered_by_the_host));
        }
        let (small, small_self, small_host) = counts[0];
        let (large, large_self, large_host) = counts[1];
        println!(
            "whole-document reads per flow snapshot: {small_self} and {small_host} (notes \
             numbered by the document, by the host) at {small} paragraphs, {large_self} and \
             {large_host} at {large}"
        );
        assert_eq!(
            large_self, small_self,
            "a flow snapshot of {large} paragraphs read whole-document facts {large_self} \
             times against {small_self} for {small}: one of them is being read again for \
             each block that needs it"
        );
        assert_eq!(
            large_host, small_host,
            "with host footnote markers, a flow snapshot of {large} paragraphs read \
             whole-document facts {large_host} times against {small_host} for {small}"
        );
        // Each fact is read at most twice (the one-block walk that answers the first block,
        // then the index for the rest), plus the walks the direct table-cell search itself
        // makes the first time.
        assert!(
            small_self <= 12 && small_host <= 12,
            "{small_self} and {small_host} whole-document reads for one snapshot"
        );
    }
}
