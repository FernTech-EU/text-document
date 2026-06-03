use super::editing_helpers::{
    CellFrameCreator, collect_block_ids_recursive, create_cell_frame, find_block_at_position,
    impl_cell_frame_creator,
};
use crate::InsertFragmentDto;
use crate::InsertFragmentResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{
    block_char_length, rope_append_block, rope_delete_in_block, rope_insert_block_at,
    rope_insert_block_boundary, rope_insert_in_block, rope_insert_table_anchor, rope_split_block,
    top_level_frame_end_byte,
};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::direct_access::table::TableRelationshipField;
use common::entities::{Block, Document, Frame, List, Root, Table, TableCell};
use common::format_runs::{
    FormatRun, ImageAnchor, InlineSegment, character_format_from_segment, coalesce_in_place,
    logical_offset_to_byte, split_images_at, split_runs_at,
};

use common::parser_tools::fragment_schema::{FragmentBlock, FragmentData, FragmentTable};
use common::parser_tools::list_grouper::ListGrouper;
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;
use std::collections::HashMap;

pub trait InsertFragmentUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn InsertFragmentUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "Update")]
#[macros::uow_action(entity = "Document", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Snapshot")]
#[macros::uow_action(entity = "Document", action = "Restore")]
#[macros::uow_action(entity = "Frame", action = "Get")]
#[macros::uow_action(entity = "Frame", action = "Update")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "Get")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "Update")]
#[macros::uow_action(entity = "Block", action = "UpdateMulti")]
#[macros::uow_action(entity = "Block", action = "UpdateWithRelationships")]
#[macros::uow_action(entity = "Block", action = "Create")]
#[macros::uow_action(entity = "Block", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "Remove")]
#[macros::uow_action(entity = "List", action = "Get")]
#[macros::uow_action(entity = "List", action = "Create")]
#[macros::uow_action(entity = "Frame", action = "Create")]
#[macros::uow_action(entity = "Table", action = "Get")]
#[macros::uow_action(entity = "Table", action = "Create")]
#[macros::uow_action(entity = "Table", action = "GetRelationship")]
#[macros::uow_action(entity = "TableCell", action = "GetMulti")]
#[macros::uow_action(entity = "TableCell", action = "Create")]
pub trait InsertFragmentUnitOfWorkTrait: CommandUnitOfWork {}

impl_cell_frame_creator!(dyn InsertFragmentUnitOfWorkTrait);

/// Convert a `FragmentBlock` to the (plain_text, format_runs,
/// block_images) representation expected by the store. The plain_text is
/// copied verbatim; runs and images are derived from the block's
/// `elements`, which mirror the InlineSegment model. Empty
/// elements collapse to nothing; Image elements become ImageAnchors at
/// their running byte offset; Text elements emit FormatRuns over their
/// UTF-8 byte range (with adjacent equal-format runs coalesced).
fn frag_block_state(fb: &FragmentBlock) -> (Vec<FormatRun>, Vec<ImageAnchor>) {
    use common::format_runs::{ImageAnchor, InlineContent};

    let mut runs: Vec<FormatRun> = Vec::new();
    let mut images: Vec<ImageAnchor> = Vec::new();
    let mut byte_offset: u32 = 0;

    for elem in &fb.elements {
        let fmt = character_format_from_segment(&InlineSegment {
            content: elem.content.clone(),
            fmt_font_family: elem.fmt_font_family.clone(),
            fmt_font_point_size: elem.fmt_font_point_size,
            fmt_font_weight: elem.fmt_font_weight,
            fmt_font_bold: elem.fmt_font_bold,
            fmt_font_italic: elem.fmt_font_italic,
            fmt_font_underline: elem.fmt_font_underline,
            fmt_font_overline: elem.fmt_font_overline,
            fmt_font_strikeout: elem.fmt_font_strikeout,
            fmt_letter_spacing: elem.fmt_letter_spacing,
            fmt_word_spacing: elem.fmt_word_spacing,
            fmt_anchor_href: elem.fmt_anchor_href.clone(),
            fmt_anchor_names: elem.fmt_anchor_names.clone(),
            fmt_is_anchor: elem.fmt_is_anchor,
            fmt_tooltip: elem.fmt_tooltip.clone(),
            fmt_underline_style: elem.fmt_underline_style.clone(),
            fmt_vertical_alignment: elem.fmt_vertical_alignment.clone(),
        });

        match &elem.content {
            InlineContent::Empty => {}
            InlineContent::Text(s) => {
                let len = s.len() as u32;
                if len > 0 {
                    runs.push(FormatRun {
                        byte_start: byte_offset,
                        byte_end: byte_offset + len,
                        format: fmt,
                    });
                    byte_offset += len;
                }
            }
            InlineContent::Image {
                name,
                width,
                height,
                quality,
            } => {
                images.push(ImageAnchor {
                    byte_offset,
                    name: name.clone(),
                    width: *width,
                    height: *height,
                    quality: *quality,
                    format: fmt,
                });
            }
        }
    }

    coalesce_in_place(&mut runs);
    (runs, images)
}

/// Write `format_runs` and `block_images` for `block_id`, then reverse-sync
/// the legacy inline_elements bridge.
fn write_block_state(
    uow: &mut Box<dyn InsertFragmentUnitOfWorkTrait>,
    block_id: EntityId,
    runs: Vec<FormatRun>,
    images: Vec<ImageAnchor>,
) {
    let store = uow.store();
    {
        let mut runs_map = store.format_runs.write();
        if runs.is_empty() {
            runs_map.remove(&block_id);
        } else {
            runs_map.insert(block_id, runs);
        }
    }
    {
        let mut images_map = store.block_images.write();
        if images.is_empty() {
            images_map.remove(&block_id);
        } else {
            images_map.insert(block_id, images);
        }
    }
}

/// Clear all per-block state (format_runs + block_images) for a block
/// that's about to be repurposed in place. The legacy inline_elements
/// will be reverse-synced from the new (empty) state by a later
/// `write_block_state` or `rebuild_block_inline_elements` call.
fn clear_block_state(uow: &mut Box<dyn InsertFragmentUnitOfWorkTrait>, block_id: EntityId) {
    let store = uow.store();
    store.format_runs.write().remove(&block_id);
    store.block_images.write().remove(&block_id);
}

/// Collect all blocks from a frame tree and map each block to its owning frame.
/// Traverses blockquote sub-frames and table cell frames recursively.
fn collect_all_blocks_with_frame(
    uow: &dyn InsertFragmentUnitOfWorkTrait,
    frame_id: &EntityId,
    block_to_frame: &mut HashMap<EntityId, EntityId>,
) -> Result<()> {
    let frame = match uow.get_frame(frame_id)? {
        Some(f) => f,
        None => return Ok(()),
    };

    if !frame.child_order.is_empty() {
        for &entry in &frame.child_order {
            if entry > 0 {
                block_to_frame.insert(entry as EntityId, *frame_id);
            } else if entry < 0 {
                let sub_id = (-entry) as EntityId;
                if let Some(sub) = uow.get_frame(&sub_id)? {
                    if let Some(tid) = sub.table {
                        let cell_ids =
                            uow.get_table_relationship(&tid, &TableRelationshipField::Cells)?;
                        let cells = uow.get_table_cell_multi(&cell_ids)?;
                        for c in cells.into_iter().flatten() {
                            if let Some(cf) = c.cell_frame {
                                collect_all_blocks_with_frame(uow, &cf, block_to_frame)?;
                            }
                        }
                    } else {
                        collect_all_blocks_with_frame(uow, &sub_id, block_to_frame)?;
                    }
                }
            }
        }
    } else {
        let blk_ids = uow.get_frame_relationship(frame_id, &FrameRelationshipField::Blocks)?;
        for bid in blk_ids {
            block_to_frame.insert(bid, *frame_id);
        }
    }
    Ok(())
}

/// Build a mapping from block_id → (cell_frame_id, table_id) for all tables in the document.
fn build_block_to_cell_map(
    uow: &dyn InsertFragmentUnitOfWorkTrait,
    doc_id: EntityId,
) -> Result<HashMap<EntityId, (EntityId, EntityId)>> {
    let table_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Tables)?;
    let mut map: HashMap<EntityId, (EntityId, EntityId)> = HashMap::new();
    for &tid in &table_ids {
        let cell_ids = uow.get_table_relationship(&tid, &TableRelationshipField::Cells)?;
        let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
        for cell in cells_opt.into_iter().flatten() {
            if let Some(cf_id) = cell.cell_frame {
                let blk_ids =
                    uow.get_frame_relationship(&cf_id, &FrameRelationshipField::Blocks)?;
                for bid in blk_ids {
                    map.insert(bid, (cf_id, tid));
                }
            }
        }
    }
    Ok(map)
}

/// Replace cell contents in an existing table with fragment data.
/// Returns Ok(Some(result)) if replacement was performed, Ok(None) if not applicable.
fn try_replace_table_cells(
    uow: &mut Box<dyn InsertFragmentUnitOfWorkTrait>,
    dto: &InsertFragmentDto,
    fragment_data: &FragmentData,
    doc_id: EntityId,
) -> Result<Option<(InsertFragmentResultDto, EntityTreeSnapshot)>> {
    if fragment_data.tables.len() != 1 {
        return Ok(None);
    }
    let frag_table = &fragment_data.tables[0];

    let block_to_cell = build_block_to_cell_map(&**uow, doc_id)?;

    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;

    let block_ids = uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;
    let blocks_opt = uow.get_block_multi(&block_ids)?;
    let mut all_blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();

    for &tid in &uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Tables)? {
        let cell_ids = uow.get_table_relationship(&tid, &TableRelationshipField::Cells)?;
        let cells = uow.get_table_cell_multi(&cell_ids)?;
        for cell in cells.into_iter().flatten() {
            if let Some(cf_id) = cell.cell_frame {
                let cf_blk_ids =
                    uow.get_frame_relationship(&cf_id, &FrameRelationshipField::Blocks)?;
                let cf_blks = uow.get_block_multi(&cf_blk_ids)?;
                all_blocks.extend(cf_blks.into_iter().flatten());
            }
        }
    }
    all_blocks.sort_by_key(|b| b.document_position);

    let (cursor_block, _, _) = find_block_at_position(&all_blocks, dto.position, &uow.store())?;

    let target_table_id = match block_to_cell.get(&cursor_block.id) {
        Some((_, tid)) => *tid,
        None => return Ok(None),
    };

    let target_table = uow
        .get_table(&target_table_id)?
        .ok_or_else(|| anyhow!("Target table not found"))?;
    let target_cell_ids =
        uow.get_table_relationship(&target_table_id, &TableRelationshipField::Cells)?;
    let target_cells_opt = uow.get_table_cell_multi(&target_cell_ids)?;
    let target_cells: Vec<TableCell> = target_cells_opt.into_iter().flatten().collect();

    let now = chrono::Utc::now();
    let snapshot = uow.snapshot_document(&[doc_id])?;

    let cursor_cf = block_to_cell.get(&cursor_block.id).map(|(cf, _)| *cf);
    let cursor_cell = target_cells.iter().find(|c| c.cell_frame == cursor_cf);
    let (base_row, base_col) = cursor_cell
        .map(|c| (c.row as usize, c.column as usize))
        .unwrap_or((0, 0));

    let max_frag_row = frag_table.cells.iter().map(|c| c.row).max().unwrap_or(0);
    let max_frag_col = frag_table.cells.iter().map(|c| c.column).max().unwrap_or(0);
    if base_row + max_frag_row >= target_table.rows as usize
        || base_col + max_frag_col >= target_table.columns as usize
    {
        return Ok(None);
    }

    for frag_cell in &frag_table.cells {
        let target_row = base_row + frag_cell.row;
        let target_col = base_col + frag_cell.column;

        let target = target_cells
            .iter()
            .find(|c| c.row as usize == target_row && c.column as usize == target_col);
        let target = match target {
            Some(t) => t,
            None => continue,
        };

        let cf_id = match target.cell_frame {
            Some(id) => id,
            None => continue,
        };

        let existing_blk_ids =
            uow.get_frame_relationship(&cf_id, &FrameRelationshipField::Blocks)?;
        let existing_blks_opt = uow.get_block_multi(&existing_blk_ids)?;
        let existing_blks: Vec<Block> = existing_blks_opt.into_iter().flatten().collect();

        // Drop all blocks except the first (we'll reuse it).
        for blk in existing_blks.iter().skip(1) {
            clear_block_state(uow, blk.id);
            uow.remove_block(&blk.id)?;
        }

        if let Some(first_blk) = existing_blks.first() {
            clear_block_state(uow, first_blk.id);

            if let Some(first_frag_blk) = frag_cell.blocks.first() {
                let (runs, images) = frag_block_state(first_frag_blk);
                let mut updated = first_blk.clone();
                updated.updated_at = now;
                uow.update_block(&updated)?;
                write_block_state(uow, first_blk.id, runs, images);

                for extra_frag in &frag_cell.blocks[1..] {
                    let (xruns, ximages) = frag_block_state(extra_frag);
                    let extra_block = Block {
                        id: 0,
                        created_at: now,
                        updated_at: now,
                        list: None,
                        document_position: 0,
                        ..Default::default()
                    };
                    let created = uow.create_block(&extra_block, cf_id, -1)?;
                    write_block_state(uow, created.id, xruns, ximages);
                }
            } else {
                let mut updated = first_blk.clone();
                updated.updated_at = now;
                uow.update_block(&updated)?;
                write_block_state(uow, first_blk.id, Vec::new(), Vec::new());
            }
        }
    }

    Ok(Some((
        InsertFragmentResultDto {
            new_position: dto.position,
            blocks_added: 0,
        },
        snapshot,
    )))
}

/// Insert a table-only fragment at the cursor position.
fn insert_table_fragment(
    uow: &mut Box<dyn InsertFragmentUnitOfWorkTrait>,
    dto: &InsertFragmentDto,
    fragment_data: &FragmentData,
) -> Result<(InsertFragmentResultDto, EntityTreeSnapshot)> {
    let now = chrono::Utc::now();

    let root = uow
        .get_root(&ROOT_ENTITY_ID)?
        .ok_or_else(|| anyhow!("Root entity not found"))?;
    let doc_ids = uow.get_root_relationship(&root.id, &RootRelationshipField::Document)?;
    let doc_id = *doc_ids
        .first()
        .ok_or_else(|| anyhow!("Root has no document"))?;

    if let Some(result) = try_replace_table_cells(uow, dto, fragment_data, doc_id)? {
        return Ok(result);
    }

    let document = uow
        .get_document(&doc_id)?
        .ok_or_else(|| anyhow!("Document not found"))?;

    let snapshot = uow.snapshot_document(&[doc_id])?;

    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;

    let block_ids = uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;
    let blocks_opt = uow.get_block_multi(&block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
    blocks.sort_by_key(|b| b.document_position);

    let insert_pos = dto.position;

    let child_order_insert_idx = if blocks.is_empty() {
        0usize
    } else {
        let (target_block, _, _) = find_block_at_position(&blocks, insert_pos, &uow.store())?;
        let blk_ids = uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;
        blk_ids
            .iter()
            .position(|&bid| bid == target_block.id)
            .map(|i| i + 1)
            .unwrap_or(0)
    };

    let mut total_blocks_added: i64 = 0;
    let mut total_chars_added: i64 = 0;
    let mut current_child_idx = child_order_insert_idx;
    let mut current_pos = insert_pos;

    // For the rope mirror at the end: per table, remember
    //   (created_table_id, target_block_id, anchor_after,
    //    cell_payload: Vec<Vec<(block_id, plain_text)>>)
    // so we can replay the same shape into the rope after entity mutations.
    type CellPayload = Vec<(EntityId, String)>;
    type TableMirror = (EntityId, EntityId, bool, Vec<CellPayload>);
    let mut table_mirror: Vec<TableMirror> = Vec::new();

    for frag_table in &fragment_data.tables {
        if frag_table.rows == 0 || frag_table.columns == 0 || frag_table.cells.is_empty() {
            continue;
        }

        // Per-table rope-mirror info. We capture cell payloads as the
        // entity mutations proceed so the rope replay below has the
        // exact IDs to wire up.
        let mut this_table_cells: Vec<CellPayload> = Vec::new();
        // Determine the target block + anchor side for this table.
        let (anchor_target, anchor_after) = if let Some(first) = blocks.first() {
            // Find the block currently at `insert_pos` (or fall back to
            // the first block) and decide before/after based on offset.
            //
            // `offset > 0` is the right test (rather than `offset >=
            // text_length`): for an empty block at the cursor we want
            // `after=false` so the anchor takes the empty block's
            // position and the (shifted) block lands after it. Without
            // this, `rope_insert_table_anchor` with `after=true` on a
            // last-and-empty target produces an unsorted block_offsets
            // vec, breaking range lookups.
            match find_block_at_position(&blocks, insert_pos, &uow.store()) {
                Ok((tb, _, offset)) => (tb.id, offset > 0),
                Err(_) => (first.id, false),
            }
        } else {
            // Empty-frame edge case: defer the rope mirror for this
            // table (no rope target exists to anchor against).
            (0, false)
        };

        let table = Table {
            id: 0,
            created_at: now,
            updated_at: now,
            cells: vec![],
            rows: frag_table.rows as i64,
            columns: frag_table.columns as i64,
            column_widths: if frag_table.column_widths.is_empty() {
                vec![0; frag_table.columns]
            } else {
                frag_table.column_widths.clone()
            },
            fmt_border: frag_table.fmt_border,
            fmt_cell_spacing: frag_table.fmt_cell_spacing,
            fmt_cell_padding: frag_table.fmt_cell_padding,
            fmt_width: frag_table.fmt_width,
            fmt_alignment: frag_table.fmt_alignment.clone(),
        };
        let created_table = uow.create_table(&table, doc_id, -1)?;

        let mut cell_blocks_to_update: Vec<Block> = Vec::new();

        for frag_cell in &frag_table.cells {
            let (cell_frame_id, created_block) = create_cell_frame(uow, doc_id, now)?;
            let mut this_cell_blocks: CellPayload = Vec::new();

            if !frag_cell.blocks.is_empty() {
                let first_frag = &frag_cell.blocks[0];
                let (runs, images) = frag_block_state(first_frag);
                let first_chars = first_frag.plain_text.chars().count() as i64;
                let first_len = first_chars + images.len() as i64;

                let mut updated_block = created_block.clone();
                updated_block.document_position = current_pos;
                updated_block.updated_at = now;
                cell_blocks_to_update.push(updated_block);
                write_block_state(uow, created_block.id, runs, images);
                this_cell_blocks.push((created_block.id, first_frag.plain_text.clone()));

                current_pos += first_len + 1;
                total_blocks_added += 1;
                total_chars_added += first_len;

                for extra_frag in &frag_cell.blocks[1..] {
                    let (xruns, ximages) = frag_block_state(extra_frag);
                    let extra_chars = extra_frag.plain_text.chars().count() as i64;
                    let extra_len = extra_chars + ximages.len() as i64;
                    let extra_block = Block {
                        id: 0,
                        created_at: now,
                        updated_at: now,
                        list: None,
                        document_position: current_pos,
                        ..Default::default()
                    };
                    let created_extra = uow.create_block(&extra_block, cell_frame_id, -1)?;
                    write_block_state(uow, created_extra.id, xruns, ximages);
                    this_cell_blocks.push((created_extra.id, extra_frag.plain_text.clone()));
                    current_pos += extra_len + 1;
                    total_blocks_added += 1;
                    total_chars_added += extra_len;
                }
            } else {
                let mut updated_block = created_block.clone();
                updated_block.document_position = current_pos;
                updated_block.updated_at = now;
                cell_blocks_to_update.push(updated_block);
                this_cell_blocks.push((created_block.id, String::new()));
                current_pos += 1;
                total_blocks_added += 1;
            }
            this_table_cells.push(this_cell_blocks);

            let cell = TableCell {
                id: 0,
                created_at: now,
                updated_at: now,
                row: frag_cell.row as i64,
                column: frag_cell.column as i64,
                row_span: frag_cell.row_span as i64,
                column_span: frag_cell.column_span as i64,
                cell_frame: Some(cell_frame_id),
                fmt_padding: frag_cell.fmt_padding,
                fmt_border: frag_cell.fmt_border,
                fmt_vertical_alignment: frag_cell.fmt_vertical_alignment.clone(),
                fmt_background_color: frag_cell.fmt_background_color.clone(),
            };
            uow.create_table_cell(&cell, created_table.id, -1)?;
        }

        if !cell_blocks_to_update.is_empty() {
            uow.update_block_multi(&cell_blocks_to_update)?;
        }

        let anchor_frame = Frame {
            id: 0,
            created_at: now,
            updated_at: now,
            parent_frame: Some(frame_id),
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

        let parent_frame = uow
            .get_frame(&frame_id)?
            .ok_or_else(|| anyhow!("Parent frame not found"))?;
        let mut updated_parent = parent_frame;
        let idx = current_child_idx.min(updated_parent.child_order.len());
        updated_parent
            .child_order
            .insert(idx, -(created_anchor.id as i64));
        updated_parent.updated_at = now;
        uow.update_frame(&updated_parent)?;

        current_child_idx += 1;

        // Remember this table for the rope mirror at the end (only if
        // we have a valid anchor target — empty-frame edge case skipped).
        if anchor_target != 0 {
            table_mirror.push((
                created_table.id,
                anchor_target,
                anchor_after,
                this_table_cells,
            ));
        }
    }

    // ── Rope mirror (insert_table_fragment) ──
    // For each table created above, insert its anchor sentinel in the
    // rope and place each cell's block(s) at the end of the containing
    // top-level frame's range, splitting subsequent cell-internal
    // blocks off the first cell block.
    // No-op under default backend.
    {
        let store = uow.store();
        for (table_id, target_block_id, after, cells) in &table_mirror {
            rope_insert_table_anchor(&store, *table_id, *target_block_id, *after);
            for cell_blocks in cells {
                let mut iter = cell_blocks.iter();
                if let Some((first_id, first_text)) = iter.next() {
                    // First cell-block goes at top_level_frame_end_byte
                    // of the table's parent frame (= frame_id, the
                    // document's top-level frame in this UC).
                    let pos = top_level_frame_end_byte(&store, frame_id);
                    rope_insert_block_at(&store, pos, *first_id, first_text);
                    let mut prev_id = *first_id;
                    let mut prev_byte_len = first_text.len() as u32;
                    for (extra_id, extra_text) in iter {
                        rope_split_block(&store, prev_id, prev_byte_len, *extra_id);
                        if !extra_text.is_empty() {
                            rope_insert_in_block(&store, *extra_id, 0, extra_text);
                        }
                        prev_id = *extra_id;
                        prev_byte_len = extra_text.len() as u32;
                    }
                }
            }
        }
    }

    let pos_shift = current_pos - insert_pos;
    if pos_shift > 0 {
        let mut shifted: Vec<Block> = Vec::new();
        for block in &blocks {
            if block.document_position >= insert_pos {
                let mut ub = block.clone();
                ub.document_position += pos_shift;
                ub.updated_at = now;
                shifted.push(ub);
            }
        }
        if !shifted.is_empty() {
            uow.update_block_multi(&shifted)?;
        }
    }

    let mut updated_doc = document.clone();
    updated_doc.block_count += total_blocks_added;
    updated_doc.character_count += total_chars_added;
    updated_doc.updated_at = now;
    uow.update_document(&updated_doc)?;

    Ok((
        InsertFragmentResultDto {
            new_position: insert_pos,
            blocks_added: total_blocks_added,
        },
        snapshot,
    ))
}

/// Apply a head update (text_before + optional first-frag-block merge or
/// full overwrite) and return the head's (runs, images) for write-back.
fn build_head_state(
    text_before: &str,
    left_runs: &[FormatRun],
    left_images: &[ImageAnchor],
    merge_first: bool,
    overwrite_head: bool,
    first_fb: Option<&FragmentBlock>,
) -> (String, Vec<FormatRun>, Vec<ImageAnchor>) {
    if overwrite_head {
        let fb = first_fb.expect("overwrite_head requires a first fragment block");
        let (runs, images) = frag_block_state(fb);
        (fb.plain_text.clone(), runs, images)
    } else if merge_first {
        let fb = first_fb.expect("merge_first requires a first fragment block");
        let mut plain = String::with_capacity(text_before.len() + fb.plain_text.len());
        plain.push_str(text_before);
        plain.push_str(&fb.plain_text);
        let (frag_runs, frag_images) = frag_block_state(fb);
        let first_offset = text_before.len() as u32;
        let mut runs: Vec<FormatRun> = left_runs.to_vec();
        for r in frag_runs {
            runs.push(FormatRun {
                byte_start: r.byte_start + first_offset,
                byte_end: r.byte_end + first_offset,
                format: r.format,
            });
        }
        coalesce_in_place(&mut runs);
        let mut images: Vec<ImageAnchor> = left_images.to_vec();
        for img in frag_images {
            images.push(ImageAnchor {
                byte_offset: img.byte_offset + first_offset,
                ..img
            });
        }
        (plain, runs, images)
    } else {
        (
            text_before.to_string(),
            left_runs.to_vec(),
            left_images.to_vec(),
        )
    }
}

/// Build the tail block's (plain_text, runs, images) by optionally
/// prepending `last_frag`'s inline content to `text_after` and rebasing
/// the right-side runs/images.
fn build_tail_state(
    text_after: &str,
    right_runs: &[FormatRun],
    right_images: &[ImageAnchor],
    last_frag: Option<&FragmentBlock>,
) -> (String, Vec<FormatRun>, Vec<ImageAnchor>) {
    if let Some(fb) = last_frag {
        let (frag_runs, frag_images) = frag_block_state(fb);
        let mut plain = String::with_capacity(fb.plain_text.len() + text_after.len());
        plain.push_str(&fb.plain_text);
        plain.push_str(text_after);
        let last_offset = fb.plain_text.len() as u32;
        let mut runs: Vec<FormatRun> = frag_runs;
        for r in right_runs.iter().cloned() {
            runs.push(FormatRun {
                byte_start: r.byte_start + last_offset,
                byte_end: r.byte_end + last_offset,
                format: r.format,
            });
        }
        coalesce_in_place(&mut runs);
        let mut images: Vec<ImageAnchor> = frag_images;
        for img in right_images.iter().cloned() {
            images.push(ImageAnchor {
                byte_offset: img.byte_offset + last_offset,
                ..img
            });
        }
        (plain, runs, images)
    } else {
        (
            text_after.to_string(),
            right_runs.to_vec(),
            right_images.to_vec(),
        )
    }
}

/// Insert a mixed fragment (both blocks and tables) at the cursor position.
///
/// Handles a paste containing BOTH blocks and tables interleaved (e.g.
/// copying a section that contains prose AND a table). Updates both the
/// entity tree and the global rope: the head block is replaced in place,
/// then each subsequent block and each table (a 1-char anchor sentinel
/// plus its cell content) is mirrored into the rope in document order —
/// see the "Rope mirror" sections below. Table anchors are placed
/// relative to the last inserted block via `rope_insert_table_anchor`
/// (with an `after` flag that handles the still-empty-head case), and
/// cell blocks at the parent frame's `top_level_frame_end_byte`, so no
/// "insert after a table anchor" primitive is required.
fn insert_mixed_fragment(
    uow: &mut Box<dyn InsertFragmentUnitOfWorkTrait>,
    dto: &InsertFragmentDto,
    fragment_data: &FragmentData,
) -> Result<(InsertFragmentResultDto, EntityTreeSnapshot)> {
    let now = chrono::Utc::now();

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
    let snapshot = uow.snapshot_document(&[doc_id])?;

    if dto.position != dto.anchor {
        return Err(anyhow!(
            "Selection replacement is not supported. Use delete_text first."
        ));
    }

    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let root_frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;

    let mut block_to_frame: HashMap<EntityId, EntityId> = HashMap::new();
    collect_all_blocks_with_frame(&**uow, &root_frame_id, &mut block_to_frame)?;

    let all_block_ids: Vec<EntityId> = {
        let get_table_cell_frames = |table_id: &EntityId| -> Result<Vec<EntityId>> {
            let cell_ids = uow.get_table_relationship(table_id, &TableRelationshipField::Cells)?;
            let cells = uow.get_table_cell_multi(&cell_ids)?;
            let mut sorted: Vec<_> = cells.into_iter().flatten().collect();
            sorted.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));
            Ok(sorted.into_iter().filter_map(|c| c.cell_frame).collect())
        };
        collect_block_ids_recursive(
            &|id| uow.get_frame(id),
            &|id, field| uow.get_frame_relationship(id, field),
            &get_table_cell_frames,
            &root_frame_id,
        )?
    };

    let blocks_opt = uow.get_block_multi(&all_block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
    blocks.sort_by_key(|b| b.document_position);

    let (current_block, block_idx, offset) =
        find_block_at_position(&blocks, dto.position, &uow.store())?;

    let frame_id = block_to_frame
        .get(&current_block.id)
        .copied()
        .unwrap_or(root_frame_id);
    let frame = uow
        .get_frame(&frame_id)?
        .ok_or_else(|| anyhow!("Frame not found"))?;

    let store = uow.store();
    let (current_runs, current_images) = (
        store
            .format_runs
            .read()
            .get(&current_block.id)
            .cloned()
            .unwrap_or_default(),
        store
            .block_images
            .read()
            .get(&current_block.id)
            .cloned()
            .unwrap_or_default(),
    );

    let current_block_text =
        common::database::rope_helpers::block_content_via_store(&current_block, &store);
    let byte_offset = logical_offset_to_byte(&current_block_text, &current_images, offset);
    let text_before = current_block_text[..byte_offset as usize].to_string();
    let text_after = current_block_text[byte_offset as usize..].to_string();
    let text_before_chars = text_before.chars().count() as i64;

    let (left_runs, right_runs) = split_runs_at(&current_runs, byte_offset);
    let (left_images, right_images) = split_images_at(&current_images, byte_offset);
    let left_image_count = left_images.len() as i64;
    let right_image_count = right_images.len() as i64;

    // Pre-mutation char length of the current block (later position math
    // can't rely on `block_char_length(&current_block, &store)` because by
    // then the head update has overwritten the block's rope content).
    let original_current_char_length = text_before_chars
        + text_after.chars().count() as i64
        + left_image_count
        + right_image_count;

    enum FragItem<'a> {
        Block(&'a FragmentBlock),
        Table(&'a FragmentTable),
    }

    let mut sorted_tables: Vec<&FragmentTable> = fragment_data.tables.iter().collect();
    sorted_tables.sort_by_key(|t| t.block_insert_index);

    let mut items: Vec<FragItem> = Vec::new();
    let mut blk_cursor = 0;
    for frag_table in &sorted_tables {
        let idx = frag_table
            .block_insert_index
            .min(fragment_data.blocks.len());
        while blk_cursor < idx {
            items.push(FragItem::Block(&fragment_data.blocks[blk_cursor]));
            blk_cursor += 1;
        }
        items.push(FragItem::Table(frag_table));
    }
    while blk_cursor < fragment_data.blocks.len() {
        items.push(FragItem::Block(&fragment_data.blocks[blk_cursor]));
        blk_cursor += 1;
    }

    let merge_first = matches!(items.first(), Some(FragItem::Block(b)) if b.is_inline_only());
    let merge_last = fragment_data.blocks.len() >= 2
        && matches!(items.last(), Some(FragItem::Block(b)) if b.is_inline_only());
    let overwrite_head =
        text_before.is_empty() && !merge_first && matches!(items.first(), Some(FragItem::Block(_)));

    let first_fb = if merge_first || overwrite_head {
        items.first().and_then(|it| match it {
            FragItem::Block(b) => Some(*b),
            _ => None,
        })
    } else {
        None
    };

    let first_chars = first_fb
        .map(|b| b.plain_text.chars().count() as i64)
        .unwrap_or(0);

    // ── Update the head block ──
    let (head_plain, head_runs, head_images) = build_head_state(
        &text_before,
        &left_runs,
        &left_images,
        merge_first,
        overwrite_head,
        first_fb,
    );

    let mut updated_current = current_block.clone();
    if overwrite_head {
        let fb = first_fb.unwrap();
        let head_list_id = if let Some(ref frag_list) = fb.list {
            let list = frag_list.to_entity();
            let created_list = uow.create_list(&list, doc_id, -1)?;
            Some(created_list.id)
        } else {
            None
        };
        updated_current.list = head_list_id;
        updated_current.fmt_alignment = fb.alignment.clone();
        updated_current.fmt_top_margin = fb.top_margin;
        updated_current.fmt_bottom_margin = fb.bottom_margin;
        updated_current.fmt_left_margin = fb.left_margin;
        updated_current.fmt_right_margin = fb.right_margin;
        updated_current.fmt_heading_level = fb.heading_level;
        updated_current.fmt_indent = fb.indent;
        updated_current.fmt_text_indent = fb.text_indent;
        updated_current.fmt_marker = fb.marker.clone();
        updated_current.fmt_tab_positions = fb.tab_positions.clone();
        updated_current.fmt_line_height = fb.line_height;
        updated_current.fmt_non_breakable_lines = fb.non_breakable_lines;
        updated_current.fmt_direction = fb.direction.clone();
        updated_current.fmt_background_color = fb.background_color.clone();
        updated_current.fmt_is_code_block = fb.is_code_block;
        updated_current.fmt_code_language = fb.code_language.clone();
        updated_current.updated_at = now;
        uow.update_block_with_relationships(&updated_current)?;
    } else {
        let _ = if merge_first {
            text_before_chars + first_chars + left_image_count
        } else {
            text_before_chars + left_image_count
        };
        updated_current.updated_at = now;
        uow.update_block(&updated_current)?;
    }
    write_block_state(uow, current_block.id, head_runs, head_images);

    // ── Rope mirror: head ──
    // Push the new head content into the rope so subsequent block /
    // table inserts can be placed at a known cursor byte. `last_block_id`
    // tracks the most recent block to use as a target for table-anchor
    // insertion (`after = true`).
    let store = uow.store();
    let head_rope_start = store
        .block_offsets
        .read()
        .range_of_block(current_block.id)
        .map(|(s, _)| s);
    let mut next_rope_byte_opt = head_rope_start.map(|s| {
        common::database::rope_helpers::rope_replace_block_content(
            &store,
            current_block.id,
            &head_plain,
        );
        s + head_plain.len() as u32
    });
    let mut last_block_id: EntityId = current_block.id;
    let mut last_block_has_content = !head_plain.is_empty();

    let mut running_position =
        current_block.document_position + block_char_length(&updated_current, &store) + 1;
    let mut new_child_order_entries: Vec<i64> = Vec::new();
    // `block_char_length(&current_block)` would now reflect the
    // post-head-update content (same id as updated_current), so use the
    // pre-mutation char length captured at the top of the function.
    let head_delta = block_char_length(&updated_current, &store) - original_current_char_length;
    let mut total_new_chars: i64 = if merge_first || overwrite_head {
        head_delta
    } else {
        0
    };
    let mut total_blocks_added: i64 = 0;

    let skip_first = merge_first || overwrite_head;
    let skip_last = merge_last;
    let mut block_index = 0usize;

    let mut list_grouper = ListGrouper::new();
    if !overwrite_head
        && let Some(list_id) = current_block.list
        && let Ok(Some(list_entity)) = uow.get_list(&list_id)
    {
        list_grouper.register(
            list_id,
            list_entity.style.clone(),
            list_entity.indent as u32,
        );
    }

    for item in &items {
        match item {
            FragItem::Block(frag_block) => {
                let is_first = block_index == 0;
                let is_last = block_index == fragment_data.blocks.len() - 1;
                block_index += 1;

                if is_first && skip_first {
                    continue;
                }
                if is_last && skip_last {
                    continue;
                }

                let (runs, images) = frag_block_state(frag_block);
                let block_chars = frag_block.plain_text.chars().count() as i64;
                let block_text_len = block_chars + images.len() as i64;

                let list_id = if let Some(ref frag_list) = frag_block.list {
                    if let Some(existing_id) =
                        list_grouper.try_reuse(&frag_list.style, frag_list.indent as u32)
                    {
                        Some(existing_id)
                    } else {
                        let list = frag_list.to_entity();
                        let created_list = uow.create_list(&list, doc_id, -1)?;
                        list_grouper.register(
                            created_list.id,
                            frag_list.style.clone(),
                            frag_list.indent as u32,
                        );
                        Some(created_list.id)
                    }
                } else {
                    list_grouper.reset();
                    None
                };

                let new_block = Block {
                    id: 0,
                    created_at: now,
                    updated_at: now,
                    list: list_id,
                    document_position: running_position,
                    fmt_alignment: frag_block.alignment.clone(),
                    fmt_top_margin: frag_block.top_margin,
                    fmt_bottom_margin: frag_block.bottom_margin,
                    fmt_left_margin: frag_block.left_margin,
                    fmt_right_margin: frag_block.right_margin,
                    fmt_heading_level: frag_block.heading_level,
                    fmt_indent: frag_block.indent,
                    fmt_text_indent: frag_block.text_indent,
                    fmt_marker: frag_block.marker.clone(),
                    fmt_tab_positions: frag_block.tab_positions.clone(),
                    fmt_line_height: frag_block.line_height,
                    fmt_non_breakable_lines: frag_block.non_breakable_lines,
                    fmt_direction: frag_block.direction.clone(),
                    fmt_background_color: frag_block.background_color.clone(),
                    fmt_is_code_block: frag_block.is_code_block,
                    fmt_code_language: frag_block.code_language.clone(),
                    fmt_hyphenate: frag_block.hyphenate,
                    fmt_language: frag_block.language.clone(),
                };

                let created_block = uow.create_block(&new_block, frame_id, -1)?;
                write_block_state(uow, created_block.id, runs, images);

                // ── Rope mirror: middle block ──
                if let Some(next_rope_byte) = next_rope_byte_opt.as_mut() {
                    common::database::rope_helpers::rope_insert_block_at(
                        &store,
                        *next_rope_byte,
                        created_block.id,
                        &frag_block.plain_text,
                    );
                    *next_rope_byte += 1 + frag_block.plain_text.len() as u32;
                    last_block_id = created_block.id;
                    last_block_has_content = !frag_block.plain_text.is_empty();
                }

                new_child_order_entries.push(created_block.id as i64);
                total_new_chars += block_text_len;
                total_blocks_added += 1;
                running_position += block_text_len + 1;
            }
            FragItem::Table(frag_table) => {
                if frag_table.rows == 0 || frag_table.columns == 0 || frag_table.cells.is_empty() {
                    continue;
                }

                let table = Table {
                    id: 0,
                    created_at: now,
                    updated_at: now,
                    cells: vec![],
                    rows: frag_table.rows as i64,
                    columns: frag_table.columns as i64,
                    column_widths: if frag_table.column_widths.is_empty() {
                        vec![0; frag_table.columns]
                    } else {
                        frag_table.column_widths.clone()
                    },
                    fmt_border: frag_table.fmt_border,
                    fmt_cell_spacing: frag_table.fmt_cell_spacing,
                    fmt_cell_padding: frag_table.fmt_cell_padding,
                    fmt_width: frag_table.fmt_width,
                    fmt_alignment: frag_table.fmt_alignment.clone(),
                };
                let created_table = uow.create_table(&table, doc_id, -1)?;

                let mut cell_blocks_to_update: Vec<Block> = Vec::new();
                // (block_id, content) tuples in cell order, for the
                // rope mirror below.
                let mut this_table_cell_blocks: Vec<Vec<(EntityId, String)>> = Vec::new();

                for frag_cell in &frag_table.cells {
                    let (cell_frame_id, created_block) = create_cell_frame(uow, doc_id, now)?;
                    let mut this_cell_blocks: Vec<(EntityId, String)> = Vec::new();

                    if !frag_cell.blocks.is_empty() {
                        let first_cb = &frag_cell.blocks[0];
                        let (runs, images) = frag_block_state(first_cb);
                        let cb_chars = first_cb.plain_text.chars().count() as i64;
                        let cb_len = cb_chars + images.len() as i64;

                        let mut updated_block = created_block.clone();
                        updated_block.document_position = running_position;
                        updated_block.updated_at = now;
                        cell_blocks_to_update.push(updated_block);
                        write_block_state(uow, created_block.id, runs, images);
                        this_cell_blocks.push((created_block.id, first_cb.plain_text.clone()));

                        running_position += cb_len + 1;
                        total_blocks_added += 1;
                        total_new_chars += cb_len;

                        for extra_frag in &frag_cell.blocks[1..] {
                            let (xruns, ximages) = frag_block_state(extra_frag);
                            let extra_chars = extra_frag.plain_text.chars().count() as i64;
                            let extra_len = extra_chars + ximages.len() as i64;
                            let extra_block = Block {
                                id: 0,
                                created_at: now,
                                updated_at: now,
                                list: None,
                                document_position: running_position,
                                ..Default::default()
                            };
                            let created_extra =
                                uow.create_block(&extra_block, cell_frame_id, -1)?;
                            write_block_state(uow, created_extra.id, xruns, ximages);
                            this_cell_blocks
                                .push((created_extra.id, extra_frag.plain_text.clone()));
                            running_position += extra_len + 1;
                            total_blocks_added += 1;
                            total_new_chars += extra_len;
                        }
                    } else {
                        let mut updated_block = created_block.clone();
                        updated_block.document_position = running_position;
                        updated_block.updated_at = now;
                        cell_blocks_to_update.push(updated_block);
                        this_cell_blocks.push((created_block.id, String::new()));
                        running_position += 1;
                        total_blocks_added += 1;
                    }

                    let cell = TableCell {
                        id: 0,
                        created_at: now,
                        updated_at: now,
                        row: frag_cell.row as i64,
                        column: frag_cell.column as i64,
                        row_span: frag_cell.row_span as i64,
                        column_span: frag_cell.column_span as i64,
                        cell_frame: Some(cell_frame_id),
                        fmt_padding: frag_cell.fmt_padding,
                        fmt_border: frag_cell.fmt_border,
                        fmt_vertical_alignment: frag_cell.fmt_vertical_alignment.clone(),
                        fmt_background_color: frag_cell.fmt_background_color.clone(),
                    };
                    uow.create_table_cell(&cell, created_table.id, -1)?;
                    this_table_cell_blocks.push(this_cell_blocks);
                }

                if !cell_blocks_to_update.is_empty() {
                    uow.update_block_multi(&cell_blocks_to_update)?;
                }

                let anchor_frame = Frame {
                    id: 0,
                    created_at: now,
                    updated_at: now,
                    parent_frame: Some(frame_id),
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
                new_child_order_entries.push(-(created_anchor.id as i64));

                // Splice the anchor into the parent frame's child_order
                // NOW (rather than after the items loop) so the per-cell
                // `top_level_frame_end_byte` walks include the
                // TableAnchor's bytes — otherwise it returns the byte
                // position BEFORE the sentinel and cells end up spliced
                // in front of the anchor, corrupting their content
                // ranges. The post-loop update below is then idempotent
                // for entries we already added here.
                {
                    let parent = uow
                        .get_frame(&frame_id)?
                        .ok_or_else(|| anyhow!("Parent frame not found"))?;
                    let neg_anchor = -(created_anchor.id as i64);
                    if !parent.child_order.contains(&neg_anchor) {
                        let mut updated_parent = parent;
                        updated_parent.child_order.push(neg_anchor);
                        updated_parent.updated_at = now;
                        uow.update_frame(&updated_parent)?;
                    }
                }

                // ── Rope mirror: table anchor + cells ──
                // Insert anchor sentinel relative to `last_block_id`
                // (after=true except when the head block is still
                // empty — same fix as the empty-target case in
                // `insert_table_fragment`). Cells go at
                // `top_level_frame_end_byte` for the parent frame, in
                // the same per-cell shape as `insert_table_fragment`.
                if next_rope_byte_opt.is_some() {
                    let after = last_block_id != current_block.id || last_block_has_content;
                    common::database::rope_helpers::rope_insert_table_anchor(
                        &store,
                        created_table.id,
                        last_block_id,
                        after,
                    );
                    for cell_blocks in &this_table_cell_blocks {
                        let mut iter = cell_blocks.iter();
                        if let Some((first_id, first_text)) = iter.next() {
                            let pos = common::database::rope_helpers::top_level_frame_end_byte(
                                &store, frame_id,
                            );
                            common::database::rope_helpers::rope_insert_block_at(
                                &store, pos, *first_id, first_text,
                            );
                            let mut prev_id = *first_id;
                            let mut prev_byte_len = first_text.len() as u32;
                            for (extra_id, extra_text) in iter {
                                common::database::rope_helpers::rope_split_block(
                                    &store,
                                    prev_id,
                                    prev_byte_len,
                                    *extra_id,
                                );
                                if !extra_text.is_empty() {
                                    common::database::rope_helpers::rope_insert_in_block(
                                        &store, *extra_id, 0, extra_text,
                                    );
                                }
                                prev_id = *extra_id;
                                prev_byte_len = extra_text.len() as u32;
                            }
                        }
                    }
                    // The anchor + cells together extend to
                    // `top_level_frame_end_byte` of the parent frame.
                    // Cursor advances past them so the next block
                    // (or tail) lands AFTER all table-related bytes.
                    if let Some(next_rope_byte) = next_rope_byte_opt.as_mut() {
                        *next_rope_byte = common::database::rope_helpers::top_level_frame_end_byte(
                            &store, frame_id,
                        );
                    }
                }
            }
        }
    }

    let last_frag = if skip_last {
        fragment_data.blocks.last()
    } else {
        None
    };
    let last_chars = last_frag
        .map(|b| b.plain_text.chars().count() as i64)
        .unwrap_or(0);

    let (tail_plain, tail_runs, tail_images) =
        build_tail_state(&text_after, &right_runs, &right_images, last_frag);
    let tail_chars = tail_plain.chars().count() as i64;
    let tail_image_count = tail_images.len() as i64;
    let tail_text_length = tail_chars + tail_image_count;

    let skip_tail_block = tail_plain.is_empty() && last_frag.is_none() && right_image_count == 0;

    #[allow(unused_assignments)]
    let mut tail_text_len: i64 = 0;

    if !skip_tail_block {
        let tail_block = Block {
            id: 0,
            created_at: now,
            updated_at: now,
            list: if overwrite_head {
                None
            } else {
                current_block.list
            },
            document_position: running_position,
            fmt_alignment: if overwrite_head {
                None
            } else {
                current_block.fmt_alignment.clone()
            },
            fmt_top_margin: if overwrite_head {
                None
            } else {
                current_block.fmt_top_margin
            },
            fmt_bottom_margin: if overwrite_head {
                None
            } else {
                current_block.fmt_bottom_margin
            },
            fmt_left_margin: if overwrite_head {
                None
            } else {
                current_block.fmt_left_margin
            },
            fmt_right_margin: if overwrite_head {
                None
            } else {
                current_block.fmt_right_margin
            },
            fmt_heading_level: if overwrite_head {
                None
            } else {
                current_block.fmt_heading_level
            },
            fmt_indent: if overwrite_head {
                None
            } else {
                current_block.fmt_indent
            },
            fmt_text_indent: if overwrite_head {
                None
            } else {
                current_block.fmt_text_indent
            },
            fmt_marker: if overwrite_head {
                None
            } else {
                current_block.fmt_marker.clone()
            },
            fmt_tab_positions: if overwrite_head {
                vec![]
            } else {
                current_block.fmt_tab_positions.clone()
            },
            fmt_line_height: if overwrite_head {
                None
            } else {
                current_block.fmt_line_height
            },
            fmt_non_breakable_lines: if overwrite_head {
                None
            } else {
                current_block.fmt_non_breakable_lines
            },
            fmt_direction: if overwrite_head {
                None
            } else {
                current_block.fmt_direction.clone()
            },
            fmt_background_color: if overwrite_head {
                None
            } else {
                current_block.fmt_background_color.clone()
            },
            fmt_is_code_block: if overwrite_head {
                None
            } else {
                current_block.fmt_is_code_block
            },
            fmt_code_language: if overwrite_head {
                None
            } else {
                current_block.fmt_code_language.clone()
            },
            fmt_hyphenate: if overwrite_head {
                None
            } else {
                current_block.fmt_hyphenate
            },
            fmt_language: if overwrite_head {
                None
            } else {
                current_block.fmt_language.clone()
            },
        };

        let created_tail = uow.create_block(&tail_block, frame_id, -1)?;
        // Use the pre-computed char count rather than re-reading from the
        // rope (the rope insert below hasn't happened yet, so a fresh
        // `block_char_length(&created_tail)` would return 0).
        tail_text_len = tail_text_length;
        write_block_state(uow, created_tail.id, tail_runs, tail_images);

        // ── Rope mirror: tail block ──
        if let Some(next_rope_byte) = next_rope_byte_opt {
            common::database::rope_helpers::rope_insert_block_at(
                &store,
                next_rope_byte,
                created_tail.id,
                &tail_plain,
            );
        }

        new_child_order_entries.push(created_tail.id as i64);
        total_blocks_added += 1;
    }
    if last_frag.is_some() {
        total_new_chars += last_chars;
    }

    let mut updated_frame = frame.clone();
    let child_order_insert_pos = (block_idx + 1).min(updated_frame.child_order.len());
    for (i, entry) in new_child_order_entries.iter().enumerate() {
        updated_frame
            .child_order
            .insert(child_order_insert_pos + i, *entry);
    }
    updated_frame.updated_at = now;
    updated_frame.blocks =
        uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;
    uow.update_frame(&updated_frame)?;

    // `original_current_char_length` was captured at the top of the
    // function — the rope has since been overwritten by the head update.
    let original_next_pos = current_block.document_position + original_current_char_length + 1;
    let new_next_pos = if skip_tail_block {
        running_position
    } else {
        running_position + tail_text_len + 1
    };
    let pos_shift = new_next_pos - original_next_pos;

    let mut blocks_to_update: Vec<Block> = Vec::new();
    for b in &blocks[(block_idx + 1)..] {
        let mut ub = b.clone();
        ub.document_position += pos_shift;
        ub.updated_at = now;
        blocks_to_update.push(ub);
    }
    if !blocks_to_update.is_empty() {
        uow.update_block_multi(&blocks_to_update)?;
    }

    let mut updated_doc = document.clone();
    updated_doc.block_count += total_blocks_added;
    updated_doc.character_count += total_new_chars;
    updated_doc.updated_at = now;
    uow.update_document(&updated_doc)?;

    let new_position = if skip_tail_block {
        running_position - 1
    } else if last_frag.is_some() {
        running_position + last_chars
    } else {
        running_position
    };

    Ok((
        InsertFragmentResultDto {
            new_position,
            blocks_added: total_blocks_added,
        },
        snapshot,
    ))
}

fn execute_insert_fragment(
    uow: &mut Box<dyn InsertFragmentUnitOfWorkTrait>,
    dto: &InsertFragmentDto,
) -> Result<(InsertFragmentResultDto, EntityTreeSnapshot)> {
    const MAX_FRAGMENT_SIZE: usize = 64 * 1024 * 1024;
    if dto.fragment_data.len() > MAX_FRAGMENT_SIZE {
        return Err(anyhow!(
            "Fragment data exceeds maximum size ({} bytes, limit {})",
            dto.fragment_data.len(),
            MAX_FRAGMENT_SIZE
        ));
    }

    let fragment_data: FragmentData = serde_json::from_str(&dto.fragment_data)
        .map_err(|e| anyhow!("Invalid fragment_data JSON: {}", e))?;

    if fragment_data.blocks.is_empty() && fragment_data.tables.is_empty() {
        return Err(anyhow!("Fragment contains no blocks or tables"));
    }

    if !fragment_data.tables.is_empty() && fragment_data.blocks.is_empty() {
        return insert_table_fragment(uow, dto, &fragment_data);
    }

    if !fragment_data.tables.is_empty() && !fragment_data.blocks.is_empty() {
        return insert_mixed_fragment(uow, dto, &fragment_data);
    }

    // ── Block-only fragment ──
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

    let snapshot = uow.snapshot_document(&[doc_id])?;

    if dto.position != dto.anchor {
        return Err(anyhow!(
            "Selection replacement is not supported. Use delete_text first."
        ));
    }

    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let root_frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;

    let mut block_to_frame: HashMap<EntityId, EntityId> = HashMap::new();
    collect_all_blocks_with_frame(&**uow, &root_frame_id, &mut block_to_frame)?;

    let all_block_ids: Vec<EntityId> = {
        let get_table_cell_frames = |table_id: &EntityId| -> Result<Vec<EntityId>> {
            let cell_ids = uow.get_table_relationship(table_id, &TableRelationshipField::Cells)?;
            let cells = uow.get_table_cell_multi(&cell_ids)?;
            let mut sorted: Vec<_> = cells.into_iter().flatten().collect();
            sorted.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));
            Ok(sorted.into_iter().filter_map(|c| c.cell_frame).collect())
        };
        collect_block_ids_recursive(
            &|id| uow.get_frame(id),
            &|id, field| uow.get_frame_relationship(id, field),
            &get_table_cell_frames,
            &root_frame_id,
        )?
    };

    let blocks_opt = uow.get_block_multi(&all_block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
    blocks.sort_by_key(|b| b.document_position);

    let (current_block, block_idx, offset) =
        find_block_at_position(&blocks, dto.position, &uow.store())?;

    let frame_id = block_to_frame
        .get(&current_block.id)
        .copied()
        .unwrap_or(root_frame_id);
    let frame = uow
        .get_frame(&frame_id)?
        .ok_or_else(|| anyhow!("Frame not found"))?;

    let store = uow.store();
    let (current_runs, current_images) = (
        store
            .format_runs
            .read()
            .get(&current_block.id)
            .cloned()
            .unwrap_or_default(),
        store
            .block_images
            .read()
            .get(&current_block.id)
            .cloned()
            .unwrap_or_default(),
    );

    let current_block_text =
        common::database::rope_helpers::block_content_via_store(&current_block, &store);
    let original_current_char_length =
        current_block_text.chars().count() as i64 + current_images.len() as i64;
    let byte_offset = logical_offset_to_byte(&current_block_text, &current_images, offset);
    let now = chrono::Utc::now();

    // ── Inline merge: single block with no block-level formatting ──
    if fragment_data.blocks.len() == 1 && fragment_data.blocks[0].is_inline_only() {
        let frag_block = &fragment_data.blocks[0];
        let inserted_plain = &frag_block.plain_text;
        let (frag_runs, frag_images) = frag_block_state(frag_block);
        let inserted_chars = inserted_plain.chars().count() as i64;
        let inserted_len = inserted_chars + frag_images.len() as i64;

        if inserted_len == 0 {
            return Ok((
                InsertFragmentResultDto {
                    new_position: dto.position,
                    blocks_added: 0,
                },
                snapshot,
            ));
        }

        let inserted_bytes = inserted_plain.len() as u32;

        // Build new plain_text.
        let mut new_plain = String::with_capacity(current_block_text.len() + inserted_plain.len());
        new_plain.push_str(&current_block_text[..byte_offset as usize]);
        new_plain.push_str(inserted_plain);
        new_plain.push_str(&current_block_text[byte_offset as usize..]);

        // Splice format_runs over the inserted byte range. Surrounding format
        // is preserved outside the inserted region.
        let mut runs = current_runs.clone();
        common::format_runs::shift_runs_for_insert(&mut runs, byte_offset, inserted_bytes);
        let inserted_at_offset: Vec<FormatRun> = frag_runs
            .into_iter()
            .map(|r| FormatRun {
                byte_start: r.byte_start + byte_offset,
                byte_end: r.byte_end + byte_offset,
                format: r.format,
            })
            .collect();
        common::format_runs::splice_range(
            &mut runs,
            byte_offset..byte_offset + inserted_bytes,
            inserted_at_offset,
        );
        coalesce_in_place(&mut runs);

        let mut images = current_images.clone();
        common::format_runs::shift_images_for_insert(&mut images, byte_offset, inserted_bytes);
        for img in frag_images {
            images.push(ImageAnchor {
                byte_offset: img.byte_offset + byte_offset,
                ..img
            });
        }
        images.sort_by_key(|a| a.byte_offset);

        let mut updated_block = current_block.clone();
        updated_block.updated_at = now;
        uow.update_block(&updated_block)?;
        write_block_state(uow, current_block.id, runs, images);

        // Mirror the inline-merge splice into the rope. No-op under default.
        rope_insert_in_block(&uow.store(), current_block.id, byte_offset, inserted_plain);

        let mut blocks_to_update: Vec<Block> = Vec::new();
        for b in &blocks[(block_idx + 1)..] {
            let mut ub = b.clone();
            ub.document_position += inserted_len;
            ub.updated_at = now;
            blocks_to_update.push(ub);
        }
        if !blocks_to_update.is_empty() {
            uow.update_block_multi(&blocks_to_update)?;
        }

        let mut updated_doc = document.clone();
        updated_doc.character_count += inserted_len;
        updated_doc.updated_at = now;
        uow.update_document(&updated_doc)?;

        return Ok((
            InsertFragmentResultDto {
                new_position: dto.position + inserted_len,
                blocks_added: 0,
            },
            snapshot,
        ));
    }

    // ── Block-splitting path ──
    let text_before = current_block_text[..byte_offset as usize].to_string();
    let text_after = current_block_text[byte_offset as usize..].to_string();
    let text_before_chars = text_before.chars().count() as i64;
    let text_after_chars = text_after.chars().count() as i64;

    let (left_runs, right_runs) = split_runs_at(&current_runs, byte_offset);
    let (left_images, right_images) = split_images_at(&current_images, byte_offset);
    let left_image_count = left_images.len() as i64;
    let right_image_count = right_images.len() as i64;

    if fragment_data.blocks.len() >= 2 {
        let first_frag = &fragment_data.blocks[0];
        let last_frag = &fragment_data.blocks[fragment_data.blocks.len() - 1];
        let merge_first = first_frag.is_inline_only();
        let merge_last = last_frag.is_inline_only();

        let first_chars = first_frag.plain_text.chars().count() as i64;
        let overwrite_head = text_before.is_empty() && !merge_first;

        let (head_plain, head_runs, head_images) = build_head_state(
            &text_before,
            &left_runs,
            &left_images,
            merge_first,
            overwrite_head,
            if merge_first || overwrite_head {
                Some(first_frag)
            } else {
                None
            },
        );

        let mut list_grouper = ListGrouper::new();
        if !overwrite_head
            && let Some(list_id) = current_block.list
            && let Ok(Some(list_entity)) = uow.get_list(&list_id)
        {
            list_grouper.register(
                list_id,
                list_entity.style.clone(),
                list_entity.indent as u32,
            );
        }

        let mut updated_current = current_block.clone();
        if overwrite_head {
            let head_list_id = if let Some(ref frag_list) = first_frag.list {
                if let Some(existing_id) =
                    list_grouper.try_reuse(&frag_list.style, frag_list.indent as u32)
                {
                    Some(existing_id)
                } else {
                    let list = frag_list.to_entity();
                    let created_list = uow.create_list(&list, doc_id, -1)?;
                    list_grouper.register(
                        created_list.id,
                        frag_list.style.clone(),
                        frag_list.indent as u32,
                    );
                    Some(created_list.id)
                }
            } else {
                list_grouper.reset();
                None
            };
            updated_current.list = head_list_id;
            updated_current.fmt_alignment = first_frag.alignment.clone();
            updated_current.fmt_top_margin = first_frag.top_margin;
            updated_current.fmt_bottom_margin = first_frag.bottom_margin;
            updated_current.fmt_left_margin = first_frag.left_margin;
            updated_current.fmt_right_margin = first_frag.right_margin;
            updated_current.fmt_heading_level = first_frag.heading_level;
            updated_current.fmt_indent = first_frag.indent;
            updated_current.fmt_text_indent = first_frag.text_indent;
            updated_current.fmt_marker = first_frag.marker.clone();
            updated_current.fmt_tab_positions = first_frag.tab_positions.clone();
            updated_current.fmt_line_height = first_frag.line_height;
            updated_current.fmt_non_breakable_lines = first_frag.non_breakable_lines;
            updated_current.fmt_direction = first_frag.direction.clone();
            updated_current.fmt_background_color = first_frag.background_color.clone();
            updated_current.fmt_is_code_block = first_frag.is_code_block;
            updated_current.fmt_code_language = first_frag.code_language.clone();
            updated_current.updated_at = now;
            uow.update_block_with_relationships(&updated_current)?;
        } else if merge_first {
            let _ = text_before_chars + first_chars + left_image_count;
            updated_current.updated_at = now;
            uow.update_block(&updated_current)?;
        } else {
            updated_current.updated_at = now;
            uow.update_block(&updated_current)?;
        }
        // The rope mirror runs LATER (after middle blocks are created),
        // so `block_char_length(&updated_current)` here still reflects
        // the pre-mutation rope content (= 0 for our overwrite/merge
        // cases that just got `head_plain` queued). Use the new char
        // length implied by `head_plain` + `head_images` directly.
        let updated_current_char_length =
            head_plain.chars().count() as i64 + head_images.len() as i64;
        write_block_state(uow, current_block.id, head_runs, head_images);

        let mut new_block_ids: Vec<EntityId> = Vec::new();
        // Track (created_block_id, plain_text) for the rope mirror.
        let mut middle_block_payload: Vec<(EntityId, String)> = Vec::new();
        let head_delta = updated_current_char_length - original_current_char_length;
        let mut total_new_chars: i64 = if merge_first || overwrite_head {
            head_delta
        } else {
            0
        };
        let mut running_position =
            current_block.document_position + updated_current_char_length + 1;

        let middle_start = if merge_first || overwrite_head { 1 } else { 0 };
        let middle_end = if merge_last {
            fragment_data.blocks.len() - 1
        } else {
            fragment_data.blocks.len()
        };

        for frag_block in &fragment_data.blocks[middle_start..middle_end] {
            let (runs, images) = frag_block_state(frag_block);
            let block_chars = frag_block.plain_text.chars().count() as i64;
            let block_text_len = block_chars + images.len() as i64;

            let list_id = if let Some(ref frag_list) = frag_block.list {
                if let Some(existing_id) =
                    list_grouper.try_reuse(&frag_list.style, frag_list.indent as u32)
                {
                    Some(existing_id)
                } else {
                    let list = frag_list.to_entity();
                    let created_list = uow.create_list(&list, doc_id, -1)?;
                    list_grouper.register(
                        created_list.id,
                        frag_list.style.clone(),
                        frag_list.indent as u32,
                    );
                    Some(created_list.id)
                }
            } else {
                list_grouper.reset();
                None
            };

            let new_block = Block {
                id: 0,
                created_at: now,
                updated_at: now,
                list: list_id,
                document_position: running_position,
                fmt_alignment: frag_block.alignment.clone(),
                fmt_top_margin: frag_block.top_margin,
                fmt_bottom_margin: frag_block.bottom_margin,
                fmt_left_margin: frag_block.left_margin,
                fmt_right_margin: frag_block.right_margin,
                fmt_heading_level: frag_block.heading_level,
                fmt_indent: frag_block.indent,
                fmt_text_indent: frag_block.text_indent,
                fmt_marker: frag_block.marker.clone(),
                fmt_tab_positions: frag_block.tab_positions.clone(),
                fmt_line_height: frag_block.line_height,
                fmt_non_breakable_lines: frag_block.non_breakable_lines,
                fmt_direction: frag_block.direction.clone(),
                fmt_background_color: frag_block.background_color.clone(),
                fmt_is_code_block: frag_block.is_code_block,
                fmt_code_language: frag_block.code_language.clone(),
                fmt_hyphenate: frag_block.hyphenate,
                fmt_language: frag_block.language.clone(),
            };

            let insert_index = (block_idx + 1 + new_block_ids.len()) as i32;
            let created_block = uow.create_block(&new_block, frame_id, insert_index)?;
            write_block_state(uow, created_block.id, runs, images);

            middle_block_payload.push((created_block.id, frag_block.plain_text.clone()));
            new_block_ids.push(created_block.id);
            total_new_chars += block_text_len;
            running_position += block_text_len + 1;
        }

        let last_chars = last_frag.plain_text.chars().count() as i64;
        let (tail_plain, tail_runs, tail_images) = build_tail_state(
            &text_after,
            &right_runs,
            &right_images,
            if merge_last { Some(last_frag) } else { None },
        );
        if merge_last {
            total_new_chars += last_chars;
        }

        let tail_chars = tail_plain.chars().count() as i64;
        let tail_image_count = tail_images.len() as i64;
        let tail_text_length = tail_chars + tail_image_count;
        let skip_tail = tail_plain.is_empty() && !merge_last && right_image_count == 0;

        let mut created_tail_id: Option<EntityId> = None;
        let mut tail_text_len: i64 = 0;

        if !skip_tail {
            let tail_block = Block {
                id: 0,
                created_at: now,
                updated_at: now,
                list: if overwrite_head {
                    None
                } else {
                    current_block.list
                },
                document_position: running_position,
                fmt_alignment: if overwrite_head {
                    None
                } else {
                    current_block.fmt_alignment.clone()
                },
                fmt_top_margin: if overwrite_head {
                    None
                } else {
                    current_block.fmt_top_margin
                },
                fmt_bottom_margin: if overwrite_head {
                    None
                } else {
                    current_block.fmt_bottom_margin
                },
                fmt_left_margin: if overwrite_head {
                    None
                } else {
                    current_block.fmt_left_margin
                },
                fmt_right_margin: if overwrite_head {
                    None
                } else {
                    current_block.fmt_right_margin
                },
                fmt_heading_level: if overwrite_head {
                    None
                } else {
                    current_block.fmt_heading_level
                },
                fmt_indent: if overwrite_head {
                    None
                } else {
                    current_block.fmt_indent
                },
                fmt_text_indent: if overwrite_head {
                    None
                } else {
                    current_block.fmt_text_indent
                },
                fmt_marker: if overwrite_head {
                    None
                } else {
                    current_block.fmt_marker.clone()
                },
                fmt_tab_positions: if overwrite_head {
                    vec![]
                } else {
                    current_block.fmt_tab_positions.clone()
                },
                fmt_line_height: if overwrite_head {
                    None
                } else {
                    current_block.fmt_line_height
                },
                fmt_non_breakable_lines: if overwrite_head {
                    None
                } else {
                    current_block.fmt_non_breakable_lines
                },
                fmt_direction: if overwrite_head {
                    None
                } else {
                    current_block.fmt_direction.clone()
                },
                fmt_background_color: if overwrite_head {
                    None
                } else {
                    current_block.fmt_background_color.clone()
                },
                fmt_is_code_block: if overwrite_head {
                    None
                } else {
                    current_block.fmt_is_code_block
                },
                fmt_code_language: if overwrite_head {
                    None
                } else {
                    current_block.fmt_code_language.clone()
                },
                fmt_hyphenate: if overwrite_head {
                    None
                } else {
                    current_block.fmt_hyphenate
                },
                fmt_language: if overwrite_head {
                    None
                } else {
                    current_block.fmt_language.clone()
                },
            };

            let tail_insert_index = (block_idx + 1 + new_block_ids.len()) as i32;
            let created_tail = uow.create_block(&tail_block, frame_id, tail_insert_index)?;
            tail_text_len = tail_text_length;
            created_tail_id = Some(created_tail.id);
            write_block_state(uow, created_tail.id, tail_runs, tail_images);
        }

        let mut updated_frame = frame.clone();
        let child_order_insert_pos = (block_idx + 1).min(updated_frame.child_order.len());
        let mut new_child_ids: Vec<i64> = new_block_ids.iter().map(|id| *id as i64).collect();
        if let Some(tid) = created_tail_id {
            new_child_ids.push(tid as i64);
        }
        for (i, id) in new_child_ids.iter().enumerate() {
            updated_frame
                .child_order
                .insert(child_order_insert_pos + i, *id);
        }
        updated_frame.updated_at = now;
        updated_frame.blocks =
            uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;
        uow.update_frame(&updated_frame)?;

        // ── Rope mirror (block-splitting path) ──
        // Now that entity mutations are done, replay the same shape
        // into the rope. The current block is already in the rope at
        // its original byte position; we splice its content to match
        // `head_plain`, then for each created middle/tail block we
        // split off after the previous block and insert that block's
        // text. No-op under default backend.
        {
            let store = uow.store();
            // 1. Sync the head: original byte range was
            //    [byte_offset .. byte_offset + text_after.len()) =
            //    text_after; replace with the head's "new" portion
            //    (= head_plain after the unchanged text_before prefix).
            let text_after_bytes = text_after.len() as u32;
            if text_after_bytes > 0 {
                rope_delete_in_block(
                    &store,
                    current_block.id,
                    byte_offset,
                    byte_offset + text_after_bytes,
                );
            }
            let head_extra = if head_plain.len() > text_before.len() {
                &head_plain[text_before.len()..]
            } else {
                ""
            };
            if !head_extra.is_empty() {
                rope_insert_in_block(&store, current_block.id, byte_offset, head_extra);
            }

            // 2. For each middle block: split off after the previous
            //    block (which currently has no successor blocks yet
            //    inside the rope), then fill its content.
            let mut prev_block_id = current_block.id;
            let mut prev_block_byte_len = head_plain.len() as u32;
            for (created_id, frag_plain) in &middle_block_payload {
                rope_split_block(&store, prev_block_id, prev_block_byte_len, *created_id);
                if !frag_plain.is_empty() {
                    rope_insert_in_block(&store, *created_id, 0, frag_plain);
                }
                prev_block_id = *created_id;
                prev_block_byte_len = frag_plain.len() as u32;
            }

            // 3. If a tail block was created, split off after the
            //    last block and insert tail_plain.
            if let Some(tail_id) = created_tail_id {
                rope_split_block(&store, prev_block_id, prev_block_byte_len, tail_id);
                if !tail_plain.is_empty() {
                    rope_insert_in_block(&store, tail_id, 0, &tail_plain);
                }
            }
            let _ = rope_append_block; // silence unused-import warning for variants used elsewhere
            let _ = rope_insert_block_boundary;
        }

        let standalone_count = (middle_end - middle_start) as i64;
        let tail_count: i64 = if skip_tail { 0 } else { 1 };
        let blocks_added = standalone_count + tail_count;
        // `original_current_char_length` was captured at function entry;
        // the rope has since been overwritten by the head update.
        let original_next_pos = current_block.document_position + original_current_char_length + 1;
        let new_next_pos = if skip_tail {
            running_position
        } else {
            running_position + tail_text_len + 1
        };
        let pos_shift = new_next_pos - original_next_pos;

        let mut blocks_to_update: Vec<Block> = Vec::new();
        for b in &blocks[(block_idx + 1)..] {
            let mut ub = b.clone();
            ub.document_position += pos_shift;
            ub.updated_at = now;
            blocks_to_update.push(ub);
        }
        if !blocks_to_update.is_empty() {
            uow.update_block_multi(&blocks_to_update)?;
        }

        let mut updated_doc = document.clone();
        updated_doc.block_count += blocks_added;
        updated_doc.character_count += total_new_chars;
        updated_doc.updated_at = now;
        uow.update_document(&updated_doc)?;

        let new_position = if skip_tail {
            running_position - 1
        } else if merge_last {
            running_position + last_chars
        } else {
            running_position
        };

        Ok((
            InsertFragmentResultDto {
                new_position,
                blocks_added,
            },
            snapshot,
        ))
    } else {
        // ── Single block with block-level formatting ──
        let frag_block = &fragment_data.blocks[0];
        let (block_runs, block_images) = frag_block_state(frag_block);
        let block_chars = frag_block.plain_text.chars().count() as i64;
        let block_text_len = block_chars + block_images.len() as i64;

        let overwrite_head = text_before.is_empty();

        if overwrite_head {
            let list_id = if let Some(ref frag_list) = frag_block.list {
                let list = frag_list.to_entity();
                let created_list = uow.create_list(&list, doc_id, -1)?;
                Some(created_list.id)
            } else {
                None
            };
            let mut updated_current = current_block.clone();
            updated_current.list = list_id;
            updated_current.fmt_alignment = frag_block.alignment.clone();
            updated_current.fmt_top_margin = frag_block.top_margin;
            updated_current.fmt_bottom_margin = frag_block.bottom_margin;
            updated_current.fmt_left_margin = frag_block.left_margin;
            updated_current.fmt_right_margin = frag_block.right_margin;
            updated_current.fmt_heading_level = frag_block.heading_level;
            updated_current.fmt_indent = frag_block.indent;
            updated_current.fmt_text_indent = frag_block.text_indent;
            updated_current.fmt_marker = frag_block.marker.clone();
            updated_current.fmt_tab_positions = frag_block.tab_positions.clone();
            updated_current.fmt_line_height = frag_block.line_height;
            updated_current.fmt_non_breakable_lines = frag_block.non_breakable_lines;
            updated_current.fmt_direction = frag_block.direction.clone();
            updated_current.fmt_background_color = frag_block.background_color.clone();
            updated_current.fmt_is_code_block = frag_block.is_code_block;
            updated_current.fmt_code_language = frag_block.code_language.clone();
            updated_current.updated_at = now;
            uow.update_block_with_relationships(&updated_current)?;
            write_block_state(uow, current_block.id, block_runs, block_images);

            let mut running_position = current_block.document_position + block_text_len + 1;
            let skip_tail = text_after.is_empty() && right_image_count == 0;
            let mut blocks_added: i64 = 0;
            #[allow(unused_assignments)]
            let mut tail_text_len: i64 = 0;
            let mut created_tail_id_overwrite: Option<EntityId> = None;

            if !skip_tail {
                let tail_block = Block {
                    id: 0,
                    created_at: now,
                    updated_at: now,
                    list: None,
                    document_position: running_position,
                    fmt_alignment: None,
                    fmt_top_margin: None,
                    fmt_bottom_margin: None,
                    fmt_left_margin: None,
                    fmt_right_margin: None,
                    fmt_heading_level: None,
                    fmt_indent: None,
                    fmt_text_indent: None,
                    fmt_marker: None,
                    fmt_tab_positions: vec![],
                    fmt_line_height: None,
                    fmt_non_breakable_lines: None,
                    fmt_direction: None,
                    fmt_background_color: None,
                    fmt_is_code_block: None,
                    fmt_code_language: None,
                    fmt_hyphenate: None,
                    fmt_language: None,
                };

                let created_tail =
                    uow.create_block(&tail_block, frame_id, (block_idx + 1) as i32)?;
                tail_text_len = block_char_length(&created_tail, &store);
                blocks_added = 1;
                created_tail_id_overwrite = Some(created_tail.id);
                write_block_state(
                    uow,
                    created_tail.id,
                    right_runs.clone(),
                    right_images.clone(),
                );

                let mut updated_frame = frame.clone();
                let child_order_insert_pos = (block_idx + 1).min(updated_frame.child_order.len());
                updated_frame
                    .child_order
                    .insert(child_order_insert_pos, created_tail.id as i64);
                updated_frame.updated_at = now;
                updated_frame.blocks =
                    uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;
                uow.update_frame(&updated_frame)?;

                running_position += tail_text_len + 1;
            }

            // ── Rope mirror (single-block-with-formatting, overwrite_head) ──
            // Current block's content went from text_after (= original full
            // block text, since text_before was empty) to frag_block.plain_text.
            // Optionally a tail block holding text_after is appended.
            {
                let store = uow.store();
                let text_after_bytes = text_after.len() as u32;
                if text_after_bytes > 0 {
                    rope_delete_in_block(&store, current_block.id, 0, text_after_bytes);
                }
                if !frag_block.plain_text.is_empty() {
                    rope_insert_in_block(&store, current_block.id, 0, &frag_block.plain_text);
                }
                if let Some(tail_id) = created_tail_id_overwrite {
                    rope_split_block(
                        &store,
                        current_block.id,
                        frag_block.plain_text.len() as u32,
                        tail_id,
                    );
                    if !text_after.is_empty() {
                        rope_insert_in_block(&store, tail_id, 0, &text_after);
                    }
                }
            }

            // Pre-mutation char length — the rope's current_block range
            // has been overwritten by the head update, so a fresh
            // `block_char_length(&current_block)` would return the new
            // (post-mutation) length.
            let original_next_pos =
                current_block.document_position + original_current_char_length + 1;
            let new_next_pos = if skip_tail {
                current_block.document_position + block_text_len + 1
            } else {
                running_position
            };
            let pos_shift = new_next_pos - original_next_pos;

            let mut blocks_to_update: Vec<Block> = Vec::new();
            for b in &blocks[(block_idx + 1)..] {
                let mut ub = b.clone();
                ub.document_position += pos_shift;
                ub.updated_at = now;
                blocks_to_update.push(ub);
            }
            if !blocks_to_update.is_empty() {
                uow.update_block_multi(&blocks_to_update)?;
            }

            let char_delta = block_text_len - original_current_char_length;
            let mut updated_doc = document.clone();
            updated_doc.block_count += blocks_added;
            updated_doc.character_count += char_delta;
            updated_doc.updated_at = now;
            uow.update_document(&updated_doc)?;

            let new_position = current_block.document_position + block_text_len;
            Ok((
                InsertFragmentResultDto {
                    new_position,
                    blocks_added,
                },
                snapshot,
            ))
        } else {
            // Normal path: text_before is not empty, keep the head block.
            let mut updated_current = current_block.clone();
            updated_current.updated_at = now;
            uow.update_block(&updated_current)?;
            write_block_state(
                uow,
                current_block.id,
                left_runs.clone(),
                left_images.clone(),
            );

            let mut running_position =
                current_block.document_position + block_char_length(&updated_current, &store) + 1;

            let list_id = if let Some(ref frag_list) = frag_block.list {
                let list = frag_list.to_entity();
                let created_list = uow.create_list(&list, doc_id, -1)?;
                Some(created_list.id)
            } else {
                None
            };

            let new_block = Block {
                id: 0,
                created_at: now,
                updated_at: now,
                list: list_id,
                document_position: running_position,
                fmt_alignment: frag_block.alignment.clone(),
                fmt_top_margin: frag_block.top_margin,
                fmt_bottom_margin: frag_block.bottom_margin,
                fmt_left_margin: frag_block.left_margin,
                fmt_right_margin: frag_block.right_margin,
                fmt_heading_level: frag_block.heading_level,
                fmt_indent: frag_block.indent,
                fmt_text_indent: frag_block.text_indent,
                fmt_marker: frag_block.marker.clone(),
                fmt_tab_positions: frag_block.tab_positions.clone(),
                fmt_line_height: frag_block.line_height,
                fmt_non_breakable_lines: frag_block.non_breakable_lines,
                fmt_direction: frag_block.direction.clone(),
                fmt_background_color: frag_block.background_color.clone(),
                fmt_is_code_block: frag_block.is_code_block,
                fmt_code_language: frag_block.code_language.clone(),
                fmt_hyphenate: frag_block.hyphenate,
                fmt_language: frag_block.language.clone(),
            };

            let created_block = uow.create_block(&new_block, frame_id, (block_idx + 1) as i32)?;
            write_block_state(uow, created_block.id, block_runs, block_images);

            running_position += block_text_len + 1;

            let tail_text_length = text_after_chars + right_image_count;
            let tail_block = Block {
                id: 0,
                created_at: now,
                updated_at: now,
                list: current_block.list,
                document_position: running_position,
                fmt_alignment: current_block.fmt_alignment.clone(),
                fmt_top_margin: current_block.fmt_top_margin,
                fmt_bottom_margin: current_block.fmt_bottom_margin,
                fmt_left_margin: current_block.fmt_left_margin,
                fmt_right_margin: current_block.fmt_right_margin,
                fmt_heading_level: current_block.fmt_heading_level,
                fmt_indent: current_block.fmt_indent,
                fmt_text_indent: current_block.fmt_text_indent,
                fmt_marker: current_block.fmt_marker.clone(),
                fmt_tab_positions: current_block.fmt_tab_positions.clone(),
                fmt_line_height: current_block.fmt_line_height,
                fmt_non_breakable_lines: current_block.fmt_non_breakable_lines,
                fmt_direction: current_block.fmt_direction.clone(),
                fmt_background_color: current_block.fmt_background_color.clone(),
                fmt_is_code_block: current_block.fmt_is_code_block,
                fmt_code_language: current_block.fmt_code_language.clone(),
                fmt_hyphenate: current_block.fmt_hyphenate,
                fmt_language: current_block.fmt_language.clone(),
            };

            let created_tail = uow.create_block(&tail_block, frame_id, (block_idx + 2) as i32)?;
            write_block_state(
                uow,
                created_tail.id,
                right_runs.clone(),
                right_images.clone(),
            );

            let mut updated_frame = frame.clone();
            let child_order_insert_pos = (block_idx + 1).min(updated_frame.child_order.len());
            let new_child_ids = [created_block.id as i64, created_tail.id as i64];
            for (i, id) in new_child_ids.iter().enumerate() {
                updated_frame
                    .child_order
                    .insert(child_order_insert_pos + i, *id);
            }
            updated_frame.updated_at = now;
            updated_frame.blocks =
                uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;
            uow.update_frame(&updated_frame)?;

            let blocks_added: i64 = 2;
            // Use pre-mutation length captured at top of function — the
            // rope content for current_block is unchanged in this
            // "Normal path" branch (only text_after was split off), but
            // a fresh `block_char_length(&current_block)` would now reflect
            // the post-split content (text_before only). Pre-mutation
            // length is what `pos_shift` math expects.
            let original_next_pos =
                current_block.document_position + original_current_char_length + 1;
            let new_next_pos = running_position + tail_text_length + 1;
            let pos_shift = new_next_pos - original_next_pos;

            let mut blocks_to_update: Vec<Block> = Vec::new();
            for b in &blocks[(block_idx + 1)..] {
                let mut ub = b.clone();
                ub.document_position += pos_shift;
                ub.updated_at = now;
                blocks_to_update.push(ub);
            }
            if !blocks_to_update.is_empty() {
                uow.update_block_multi(&blocks_to_update)?;
            }

            let mut updated_doc = document.clone();
            updated_doc.block_count += blocks_added;
            updated_doc.character_count += block_text_len;
            updated_doc.updated_at = now;
            uow.update_document(&updated_doc)?;

            // ── Rope mirror (single-block-with-formatting, normal path) ──
            // Current block now holds text_before only. Two new blocks were
            // created: the middle block (= frag_block.plain_text) and the
            // tail block (= text_after). Splice the rope to match.
            {
                let store = uow.store();
                let text_after_bytes = text_after.len() as u32;
                if text_after_bytes > 0 {
                    rope_delete_in_block(
                        &store,
                        current_block.id,
                        byte_offset,
                        byte_offset + text_after_bytes,
                    );
                }
                rope_split_block(
                    &store,
                    current_block.id,
                    text_before.len() as u32,
                    created_block.id,
                );
                if !frag_block.plain_text.is_empty() {
                    rope_insert_in_block(&store, created_block.id, 0, &frag_block.plain_text);
                }
                rope_split_block(
                    &store,
                    created_block.id,
                    frag_block.plain_text.len() as u32,
                    created_tail.id,
                );
                if !text_after.is_empty() {
                    rope_insert_in_block(&store, created_tail.id, 0, &text_after);
                }
            }

            Ok((
                InsertFragmentResultDto {
                    new_position: running_position,
                    blocks_added: 1,
                },
                snapshot,
            ))
        }
    }
}

pub struct InsertFragmentUseCase {
    uow_factory: Box<dyn InsertFragmentUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<InsertFragmentDto>,
}

impl InsertFragmentUseCase {
    pub fn new(uow_factory: Box<dyn InsertFragmentUnitOfWorkFactoryTrait>) -> Self {
        InsertFragmentUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &InsertFragmentDto) -> Result<InsertFragmentResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_insert_fragment(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for InsertFragmentUseCase {
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
        let (_, snapshot) = execute_insert_fragment(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
