use crate::InsertTableDto;
use crate::InsertTableResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{
    block_char_length, rope_insert_block_at, rope_insert_table_anchor,
};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::entities::{Block, Document, Frame, Root, Table, TableCell};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

use super::editing_helpers::{
    CellFrameCreator, create_cell_frame, find_block_at_position, impl_cell_frame_creator,
};

pub trait InsertTableUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn InsertTableUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "Update")]
#[macros::uow_action(entity = "Document", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Snapshot")]
#[macros::uow_action(entity = "Document", action = "Restore")]
#[macros::uow_action(entity = "Frame", action = "Get")]
#[macros::uow_action(entity = "Frame", action = "Create")]
#[macros::uow_action(entity = "Frame", action = "Update")]
#[macros::uow_action(entity = "Frame", action = "UpdateWithRelationships")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "Create")]
#[macros::uow_action(entity = "Block", action = "Update")]
#[macros::uow_action(entity = "Block", action = "UpdateMulti")]
#[macros::uow_action(entity = "Table", action = "Create")]
#[macros::uow_action(entity = "TableCell", action = "Create")]
pub trait InsertTableUnitOfWorkTrait: CommandUnitOfWork {}

impl_cell_frame_creator!(dyn InsertTableUnitOfWorkTrait);

pub struct InsertTableUseCase {
    uow_factory: Box<dyn InsertTableUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<InsertTableDto>,
}

fn execute_insert_table(
    uow: &mut Box<dyn InsertTableUnitOfWorkTrait>,
    dto: &InsertTableDto,
) -> Result<(InsertTableResultDto, EntityTreeSnapshot)> {
    if dto.rows < 1 || dto.columns < 1 {
        return Err(anyhow!("Table must have at least 1 row and 1 column"));
    }

    let now = chrono::Utc::now();

    // Get Root -> Document
    let root = uow
        .get_root(&ROOT_ENTITY_ID)?
        .ok_or_else(|| anyhow!("Root entity not found"))?;
    let doc_ids = uow.get_root_relationship(&root.id, &RootRelationshipField::Document)?;
    let doc_id = *doc_ids
        .first()
        .ok_or_else(|| anyhow!("Root has no document"))?;

    let document = uow
        .get_document(&doc_id)?
        .ok_or_else(|| anyhow!("Document not found"))?;

    // Snapshot for undo before mutation
    let snapshot = uow.snapshot_document(&[doc_id])?;

    // Catch up stored `Block.document_position` from the rope for all
    // top-level blocks. `insert_text_uc` skips its position-refresh
    // loop for rope-clean documents (readers derive from
    // `BlockOffsetIndex` directly), so the stored field may be stale.
    // Inserting a table transitions the doc to tabled state — after
    // which cells contribute to flow positions but not to the rope,
    // so the rope can no longer derive flow positions and the stored
    // field becomes the authoritative source. Bring it up to date now
    // before any cell-position computations below use it.
    {
        let store = uow.store();
        let catchup: Vec<(common::types::EntityId, i64)> =
            if common::database::rope_helpers::rope_positions_match_flow(&store) {
                let offsets = store.block_offsets.read().unwrap();
                let rope = store.rope.read().unwrap();
                offsets
                    .entries
                    .iter()
                    .filter_map(|(marker, byte_start)| match marker {
                        common::database::block_offset_index::OffsetMarker::Block(id) => {
                            Some((*id, rope.byte_to_char(*byte_start as usize) as i64))
                        }
                        _ => None,
                    })
                    .collect()
            } else {
                Vec::new()
            };
        if !catchup.is_empty() {
            let ids: Vec<common::types::EntityId> = catchup.iter().map(|(id, _)| *id).collect();
            let blocks_opt = uow.get_block_multi(&ids)?;
            let mut updates: Vec<Block> = Vec::new();
            for ((_, fresh_pos), maybe_b) in catchup.into_iter().zip(blocks_opt) {
                if let Some(b) = maybe_b
                    && b.document_position != fresh_pos
                {
                    let mut ub = b;
                    ub.document_position = fresh_pos;
                    ub.updated_at = chrono::Utc::now();
                    updates.push(ub);
                }
            }
            if !updates.is_empty() {
                uow.update_block_multi(&updates)?;
            }
        }
    }

    // Find the insertion position — determine the parent frame and where in child_order
    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;

    // Get all blocks across all frames to find the insertion point
    let mut all_blocks: Vec<Block> = Vec::new();
    for fid in &frame_ids {
        let block_ids = uow.get_frame_relationship(fid, &FrameRelationshipField::Blocks)?;
        if !block_ids.is_empty() {
            let blocks_opt = uow.get_block_multi(&block_ids)?;
            all_blocks.extend(blocks_opt.into_iter().flatten());
        }
    }
    all_blocks.sort_by_key(|b| b.document_position);

    // Resolve selection: use min position, delete selection if any
    let insert_pos = dto.position.min(dto.anchor);

    // Find the frame containing the insertion position. Also remember
    // the cursor-adjacent block id (None for empty docs) so the rope
    // mirror knows where to place the U+FFFC sentinel, and compute
    // `cell_start_pos` — the document-position-space coordinate where
    // the first cell will live, *matching what `snapshot_from_child_order`
    // will compute* once the table anchor is inserted into the parent
    // frame's child_order. That's the critical invariant: cell
    // `document_position` values must equal the snapshot's running_pos,
    // otherwise lookups like `insert_text` (which sort blocks by
    // `document_position`) drift from the cursor positions the
    // rendered snapshot reports.
    let (parent_frame_id, child_order_insert_idx, rope_anchor, cell_start_pos): (
        EntityId,
        usize,
        Option<(EntityId, bool)>,
        i64,
    ) = if all_blocks.is_empty() {
        // Empty document — use the first frame. No host block, so the
        // table goes first in child_order; cells start at the cursor
        // position (which is 0 in a fresh empty doc).
        let first_frame_id = frame_ids
            .first()
            .ok_or_else(|| anyhow!("Document has no frames"))?;
        (*first_frame_id, 0usize, None, insert_pos)
    } else {
        let (target_block, _, offset) =
            find_block_at_position(&all_blocks, insert_pos, &uow.store())?;
        let target_length = block_char_length(&target_block, &uow.store());

        // Find which frame owns this block AND the block's position in
        // that frame's `child_order` (not the blocks-only list!). The
        // anchor frame for the new table is inserted into `child_order`,
        // which interleaves positive block ids and negative sub-frame
        // ids. Using the blocks-only index here drifts whenever the
        // parent frame contains sub-frames before the target — concretely:
        // an imported GFM table earlier in the same frame makes
        // `blocks.index_of(target)` smaller than
        // `child_order.index_of(target)` by the number of preceding
        // sub-frames, so the new table gets inserted that many slots
        // too early in flow order. Reproduced by
        // `insert_table_after_imported_gfm_table_lands_at_doc_end` and
        // `rich_text_editor_demo_end_to_end_insert_table_at_end` in
        // `crates/public_api/tests/table_editing_tests.rs`.
        let target_block_entry = target_block.id as i64;
        let mut found_frame_id = frame_ids[0];
        let mut found_child_idx = 0usize;
        'outer: for fid in &frame_ids {
            let frame = uow
                .get_frame(fid)?
                .ok_or_else(|| anyhow!("Frame {fid} not found"))?;
            for (ci, entry) in frame.child_order.iter().enumerate() {
                if *entry == target_block_entry {
                    found_frame_id = *fid;
                    found_child_idx = ci;
                    break 'outer;
                }
            }
        }
        // When at the very start of the block (offset == 0), place the
        // table before it so the table can be the first flow element.
        // Otherwise (cursor anywhere inside or at the end of the host
        // block) the table goes after the whole host block — it is
        // NOT split — so cell positions follow the host block's full
        // extent, regardless of how far in the cursor sits.
        let after = offset > 0;
        let after_idx = if after { 1 } else { 0 };
        let cell_start = if after {
            // Cells live one boundary past the host block's end.
            target_block.document_position + target_length + 1
        } else {
            // Cells take the host block's current spot; the host
            // shifts to past the cells (handled by the shift loop
            // below at `document_position >= insert_pos`).
            target_block.document_position
        };

        // If the target block lives inside a table cell, the user
        // clicked "Insert Table" from within an existing table. Don't
        // nest — that produces an invisible block because the renderer
        // doesn't recurse into cell-nested anchor frames. Instead,
        // hoist the new table OUT to be a sibling of the containing
        // table, immediately after it. Reproduced by
        // `insert_table_from_inside_cell_lands_after_containing_table`.
        let owning_frame = uow
            .get_frame(&found_frame_id)?
            .ok_or_else(|| anyhow!("Owning frame {found_frame_id} not found"))?;
        let mut cell_anchor: Option<(EntityId, EntityId)> = None;
        if let Some(parent_id) = owning_frame.parent_frame {
            let parent = uow
                .get_frame(&parent_id)?
                .ok_or_else(|| anyhow!("Parent frame {parent_id} not found"))?;
            if parent.table.is_some()
                && let Some(grandparent_id) = parent.parent_frame
            {
                cell_anchor = Some((parent_id, grandparent_id));
            }
        }

        if let Some((anchor_frame_id, grandparent_id)) = cell_anchor {
            let grandparent = uow
                .get_frame(&grandparent_id)?
                .ok_or_else(|| anyhow!("Grandparent frame {grandparent_id} not found"))?;
            let anchor_entry = -(anchor_frame_id as i64);
            let anchor_idx = grandparent
                .child_order
                .iter()
                .position(|e| *e == anchor_entry)
                .ok_or_else(|| anyhow!("Anchor frame missing from grandparent child_order"))?;

            // cell_start: position right after the existing table's
            // last cell. Cells of the existing table are blocks in
            // frames whose `parent_frame == anchor_frame_id`. The
            // snapshot walker assigns running positions where each
            // block contributes (length + 1) — the +1 is the per-block
            // boundary the walker emits between siblings. So the next
            // free slot in the grandparent's flow is
            // `max(cell_block.document_position + cell_block.length) + 1`.
            let mut last_end: i64 = 0;
            let mut any = false;
            for fid in &frame_ids {
                let f = uow
                    .get_frame(fid)?
                    .ok_or_else(|| anyhow!("Frame {fid} not found"))?;
                if f.parent_frame != Some(anchor_frame_id) {
                    continue;
                }
                let block_ids = uow.get_frame_relationship(fid, &FrameRelationshipField::Blocks)?;
                if block_ids.is_empty() {
                    continue;
                }
                let blocks_opt = uow.get_block_multi(&block_ids)?;
                for b in blocks_opt.into_iter().flatten() {
                    let end = b.document_position + block_char_length(&b, &uow.store());
                    if !any || end > last_end {
                        last_end = end;
                        any = true;
                    }
                }
            }
            let hoisted_cell_start = last_end + 1;
            (
                grandparent_id,
                anchor_idx + 1,
                // Rope mirror: anchor after target_block (a cell of the
                // existing table). No-op under the default backend.
                Some((target_block.id, true)),
                hoisted_cell_start,
            )
        } else {
            (
                found_frame_id,
                found_child_idx + after_idx,
                Some((target_block.id, after)),
                cell_start,
            )
        }
    };

    // 1. Create the Table entity (owned by Document)
    let table = Table {
        id: 0,
        created_at: now,
        updated_at: now,
        cells: vec![],
        rows: dto.rows,
        columns: dto.columns,
        column_widths: vec![0; dto.columns as usize],
        fmt_border: None,
        fmt_cell_spacing: None,
        fmt_cell_padding: None,
        fmt_width: None,
        fmt_alignment: None,
    };
    let created_table = uow.create_table(&table, doc_id, -1)?;

    // 2. Create cell frames and TableCells in row-major order
    let total_cells = dto.rows * dto.columns;
    let mut cell_blocks: Vec<Block> = Vec::with_capacity(total_cells as usize);
    let mut cell_frame_ids: Vec<EntityId> = Vec::with_capacity(total_cells as usize);

    for r in 0..dto.rows {
        for c in 0..dto.columns {
            // Create a cell frame with an empty block
            let (cell_frame_id, created_block) = create_cell_frame(uow, doc_id, now)?;

            cell_blocks.push(created_block);
            cell_frame_ids.push(cell_frame_id);

            // Create the TableCell entity
            let cell = TableCell {
                id: 0,
                created_at: now,
                updated_at: now,
                row: r,
                column: c,
                row_span: 1,
                column_span: 1,
                cell_frame: Some(cell_frame_id),
                fmt_padding: None,
                fmt_border: None,
                fmt_vertical_alignment: None,
                fmt_background_color: None,
            };
            uow.create_table_cell(&cell, created_table.id, -1)?;
        }
    }

    // 3. Create the anchor frame (the frame that represents the table in the document flow)
    let anchor_frame = Frame {
        id: 0,
        created_at: now,
        updated_at: now,
        parent_frame: Some(parent_frame_id),
        blocks: vec![],
        child_order: vec![],
        fmt_height: None,
        fmt_width: None,
        fmt_top_margin: None,
        fmt_bottom_margin: None,
        fmt_left_margin: None,
        fmt_right_margin: None,
        fmt_padding: None,
        fmt_border: None,
        fmt_position: None,
        fmt_is_blockquote: None,
        table: Some(created_table.id),
        byte_range: (0, 0),
    };
    let created_anchor = uow.create_frame(&anchor_frame, doc_id, -1)?;

    // Backfill each cell frame's `parent_frame` to point at the anchor
    // frame we just created. Cell frames are created before the anchor
    // (we don't know the anchor id yet), so this can't be set at
    // creation time. Without it, walking up from a cell to find its
    // containing table fails — which breaks the "is the cursor inside
    // an existing table?" check below in a future insert_table call.
    //
    // Use `update_with_relationships` (not the scalar `update_frame`):
    // the latter intentionally preserves the existing `parent_frame`
    // to keep stale-entity writes from clobbering the relationship,
    // which would silently no-op us. See
    // crates/common/src/direct_access/frame/frame_table.rs `update`.
    for cell_frame_id in &cell_frame_ids {
        if let Some(cf) = uow.get_frame(cell_frame_id)? {
            let mut updated = cf;
            updated.parent_frame = Some(created_anchor.id);
            uow.update_frame_with_relationships(&updated)?;
        }
    }

    // Insert the anchor frame into the parent frame's child_order
    let parent_frame = uow
        .get_frame(&parent_frame_id)?
        .ok_or_else(|| anyhow!("Parent frame not found"))?;
    let mut updated_parent = parent_frame.clone();
    let idx = child_order_insert_idx.min(updated_parent.child_order.len());
    // Convention: negative = -(frame ID) for sub-frame references in child_order
    updated_parent
        .child_order
        .insert(idx, -(created_anchor.id as i64));
    updated_parent.updated_at = now;
    uow.update_frame(&updated_parent)?;

    // Inserts a U+FFFC sentinel + boundary newline into the global
    // rope and registers a TableAnchor(table_id) marker in the
    // offset index. Cell-internal content is not yet tracked in
    // BlockOffsetIndex — plan §1.6's Frame.byte_range model is a
    // follow-up commit.
    if let Some((target_block_id, after)) = rope_anchor {
        rope_insert_table_anchor(&uow.store(), created_table.id, target_block_id, after);
    }

    // 4. Assign document_position to all cell blocks in row-major
    // order. `cell_start_pos` was computed up front to match exactly
    // what `snapshot_from_child_order` (text_frame.rs) will report
    // for the first cell once the table anchor is part of the parent
    // frame's child_order. Each subsequent empty cell adds 1 (the
    // per-cell boundary) so cells occupy a contiguous run of N
    // positions starting at `cell_start_pos`.
    //
    // The two cases that contribute to `cell_start_pos` correctness:
    //
    //   * "after" insertion (cursor offset > 0 within the host block):
    //     the table goes *after* the whole host block in child_order
    //     — it is not split. So cells must start at
    //     `host.document_position + host.length + 1`, not at
    //     `insert_pos + 1`. The two are equal only when the cursor
    //     is exactly at the end of the host block (offset == length);
    //     elsewhere, `insert_pos + 1` lands inside the host's
    //     document-position range, putting cell positions out of
    //     sync with the snapshot the user sees.
    //
    //   * "before" insertion (cursor offset == 0) and empty doc:
    //     cells take the cursor's position; the displaced host
    //     block (if any) shifts to past the cells via the
    //     `document_position >= insert_pos` loop below.
    //
    // Tests: `inserted_3x3_typing_in_each_cell_lands_in_that_cell`
    // (after-at-end-of-block), `inserted_2x2_at_start_of_block_lands_before_it`
    // (before), `inserted_2x2_in_empty_document_starts_at_zero` (empty),
    // `inserted_2x2_deep_in_long_block_document_position_matches_snapshot`
    // (after-deep-in-block — the user-reported "table inserted before
    // the current block" regression, where document_position drifted
    // from snapshot in proportion to `host.length - offset`).
    let mut blocks_to_update: Vec<Block> = Vec::new();
    for (offset, cell_block) in cell_blocks.iter().enumerate() {
        let mut updated_block = cell_block.clone();
        updated_block.document_position = cell_start_pos + offset as i64;
        updated_block.updated_at = now;
        blocks_to_update.push(updated_block);
    }
    if !blocks_to_update.is_empty() {
        uow.update_block_multi(&blocks_to_update)?;
    }

    // Mirror cell-block creation into the global rope INLINE, immediately
    // after the table-anchor sentinel — matching the markdown importer's
    // layout. This keeps the rope's char order identical to the document
    // (flow) order: `… host content, U+FFFC table sentinel, cellA, cellB, …,
    // following content`. Flow positions are derived from this same rope
    // (see `text_block::build_block_snapshot_with_position_and_parent`), so
    // the editor's caret space and the editing path's resolution agree —
    // including cursor navigation INTO the table, which a frame-end cell
    // layout (cells parked after all following content in rope order) would
    // break.
    //
    // The sentinel is `U+FFFC` (3 bytes) starting at the TableAnchor marker's
    // byte; we splice the cells in just past it. `rope_insert_block_at`
    // prepends a `\n` boundary per cell and shifts following entries, so
    // advancing `byte_pos` by `1 + content_len` per cell lays them out
    // contiguously. Newly-created cells are empty (`content_len == 0`).
    //
    // No-op under a backend that doesn't register the anchor.
    {
        let store = uow.store();
        let anchor_start = store
            .block_offsets
            .read()
            .unwrap()
            .range_of(common::database::block_offset_index::OffsetMarker::TableAnchor(
                created_table.id,
            ))
            .map(|(start, _)| start);
        if let Some(anchor_start) = anchor_start {
            const SENTINEL_BYTES: u32 = 3; // U+FFFC
            let mut byte_pos = anchor_start + SENTINEL_BYTES;
            for cell_block in cell_blocks.iter() {
                rope_insert_block_at(&store, byte_pos, cell_block.id, "");
                byte_pos += 1; // one `\n` boundary; empty cell content adds 0
            }
        }
    }
    // NB: the stored `document_position` written for the cells (step 4) and
    // for the shifted following blocks (step 5) is now in the OLD running-
    // counter space, which omits the sentinel. That is harmless: with the
    // cells mirrored inline the rope is authoritative, so every read path
    // (`build_block_snapshot…` and `find_block_at_char_position`) re-derives
    // positions from the rope and ignores the stale stored field. The stored
    // writes still matter for the rope-unavailable backend, so they stay.

    // 5. Shift document_position for all blocks that end up positioned
    // at or past the new table's first cell. Use `cell_start_pos` (not
    // `insert_pos`) as the threshold so the cell-inside-existing-table
    // branch shifts the right set: when the user clicks inside cell
    // (i, j) of an existing table and the new table is hoisted to land
    // immediately AFTER that table, the threshold is the position
    // right after the existing table — not the cursor inside the cell.
    // For the normal branches, `cell_start_pos` either equals
    // `insert_pos` (offset == 0) or differs by exactly `target.length`
    // (offset > 0, after=true). In the latter case the gap
    // `[insert_pos, cell_start_pos)` is wholly inside the host block,
    // so no other block sits there and either threshold shifts the
    // same set.
    let table_size = total_cells; // Each cell block occupies 1 position (empty block = separator)
    let mut shifted_blocks: Vec<Block> = Vec::new();
    for block in &all_blocks {
        if block.document_position >= cell_start_pos {
            let mut shifted = block.clone();
            shifted.document_position += table_size;
            shifted.updated_at = now;
            shifted_blocks.push(shifted);
        }
    }
    if !shifted_blocks.is_empty() {
        uow.update_block_multi(&shifted_blocks)?;
    }

    // 6. Update Document stats
    let mut updated_doc = document.clone();
    updated_doc.block_count += total_cells;
    updated_doc.updated_at = now;
    uow.update_document(&updated_doc)?;

    let new_position = insert_pos;

    Ok((
        InsertTableResultDto {
            table_id: created_table.id as i64,
            new_position,
        },
        snapshot,
    ))
}

impl InsertTableUseCase {
    pub fn new(uow_factory: Box<dyn InsertTableUnitOfWorkFactoryTrait>) -> Self {
        InsertTableUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &InsertTableDto) -> Result<InsertTableResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_insert_table(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for InsertTableUseCase {
    fn undo(&mut self) -> Result<()> {
        let snapshot = self
            .undo_snapshot
            .as_ref()
            .ok_or_else(|| anyhow!("No snapshot available for undo"))?
            .clone();

        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;
        uow.restore_document(&snapshot)?;
        uow.commit()?;
        Ok(())
    }

    fn redo(&mut self) -> Result<()> {
        let dto = self
            .last_dto
            .as_ref()
            .ok_or_else(|| anyhow!("No DTO available for redo"))?
            .clone();

        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;
        let (_, snapshot) = execute_insert_table(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
