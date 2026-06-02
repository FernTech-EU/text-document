use super::editing_helpers::{
    collect_block_ids_recursive, find_block_at_position, is_word_boundary_punct,
};
use crate::DeleteTextDto;
use crate::DeleteTextResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{block_char_length, block_content_via_store};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::direct_access::table::TableRelationshipField;
use common::entities::{Block, Document, Frame, Root, Table, TableCell};
use common::format_runs::{
    FormatRun, ImageAnchor, debug_assert_well_formed, logical_offset_to_byte,
    shift_images_for_delete, shift_runs_for_delete,
};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;
use std::time::Instant;

pub trait DeleteTextUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn DeleteTextUnitOfWorkTrait>;
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
#[macros::uow_action(entity = "Block", action = "Create")]
#[macros::uow_action(entity = "Block", action = "Remove")]
#[macros::uow_action(entity = "Block", action = "GetRelationship")]
#[macros::uow_action(entity = "Table", action = "Get")]
#[macros::uow_action(entity = "Table", action = "GetRelationship")]
#[macros::uow_action(entity = "Table", action = "Remove")]
#[macros::uow_action(entity = "TableCell", action = "GetMulti")]
#[macros::uow_action(entity = "TableCell", action = "Remove")]
#[macros::uow_action(entity = "Frame", action = "Remove")]
#[macros::uow_action(entity = "List", action = "Remove")]
pub trait DeleteTextUnitOfWorkTrait: CommandUnitOfWork {}

pub struct DeleteTextUseCase {
    uow_factory: Box<dyn DeleteTextUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<DeleteTextDto>,
    last_result: Option<DeleteTextResultDto>,
    last_merge_time: Option<Instant>,
    is_single_char_origin: bool,
}

/// Read the per-block format_runs + block_images vectors. Used by callers
/// that want to manipulate the new run/image tables directly.
fn read_block_runs_and_images(
    uow: &dyn DeleteTextUnitOfWorkTrait,
    block_id: EntityId,
) -> (Vec<FormatRun>, Vec<ImageAnchor>) {
    let store = uow.store();
    let runs = store
        .format_runs
        .read()
        .get(&block_id)
        .cloned()
        .unwrap_or_default();
    let images = store
        .block_images
        .read()
        .get(&block_id)
        .cloned()
        .unwrap_or_default();
    (runs, images)
}

/// Reset a block to empty state: clears plain_text, text_length,
/// format_runs and block_images. Also rebuilds the legacy inline_elements
/// view to one Empty element so downstream legacy readers stay consistent.
fn clear_block(
    uow: &mut Box<dyn DeleteTextUnitOfWorkTrait>,
    block: &Block,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let mut updated = block.clone();
    updated.updated_at = now;
    uow.update_block(&updated)?;
    let store = uow.store();
    common::database::rope_helpers::rope_replace_block_content(&store, block.id, "");
    store.format_runs.write().insert(block.id, Vec::new());
    store.block_images.write().insert(block.id, Vec::new());
    Ok(())
}

/// Drop the per-block run/image/inline_elements tables for a block that's
/// about to be removed entirely. Idempotent.
fn drop_block_runs_and_images(uow: &dyn DeleteTextUnitOfWorkTrait, block_id: EntityId) {
    let store = uow.store();
    store.format_runs.write().remove(&block_id);
    store.block_images.write().remove(&block_id);
}

/// Recursive walk of the frame tree rooted at `root_id` to find which
/// frame's `child_order` contains the positive entry `target`. Used by
/// the cross-block merge to correctly resolve sub-frame ownership when
/// the deletion crosses a frame boundary — the cell-only
/// `block_to_cell_frame` map cannot answer this for blockquote frames.
fn find_block_owner_frame(
    uow: &dyn DeleteTextUnitOfWorkTrait,
    root_id: EntityId,
    target: EntityId,
) -> Result<Option<EntityId>> {
    let f = uow
        .get_frame(&root_id)?
        .ok_or_else(|| anyhow!("Frame not found"))?;
    for &entry in &f.child_order {
        if entry > 0 && entry as EntityId == target {
            return Ok(Some(root_id));
        }
        if entry < 0 {
            let sub = (-entry) as EntityId;
            if let Some(o) = find_block_owner_frame(uow, sub, target)? {
                return Ok(Some(o));
            }
        }
    }
    Ok(None)
}

/// Recursively prune empty non-table sub-frames under `frame_id` (post-order).
/// A frame is removed iff its direct block list is empty AND its `child_order`
/// has no surviving sub-frame entries. Table-anchor frames (`table.is_some()`)
/// are never pruned — the table's cell frames carry the blocks separately and
/// the anchor must persist for as long as the table does.
fn prune_empty_subframes_recursive(
    uow: &mut Box<dyn DeleteTextUnitOfWorkTrait>,
    frame_id: EntityId,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let frame = match uow.get_frame(&frame_id)? {
        Some(f) => f,
        None => return Ok(()),
    };

    let sub_frame_ids: Vec<EntityId> = frame
        .child_order
        .iter()
        .filter_map(|&e| if e < 0 { Some((-e) as EntityId) } else { None })
        .collect();

    for sf_id in &sub_frame_ids {
        prune_empty_subframes_recursive(uow, *sf_id, now)?;
    }

    let frame = match uow.get_frame(&frame_id)? {
        Some(f) => f,
        None => return Ok(()),
    };

    let mut sub_frames_to_remove: Vec<EntityId> = Vec::new();
    for &entry in &frame.child_order {
        if entry < 0 {
            let sf_id = (-entry) as EntityId;
            if let Some(sf) = uow.get_frame(&sf_id)? {
                if sf.table.is_some() {
                    continue;
                }
                let blk_ids =
                    uow.get_frame_relationship(&sf_id, &FrameRelationshipField::Blocks)?;
                let has_surviving_subframes = sf.child_order.iter().any(|&e| e < 0);
                if blk_ids.is_empty() && !has_surviving_subframes {
                    sub_frames_to_remove.push(sf_id);
                }
            }
        }
    }

    if !sub_frames_to_remove.is_empty() {
        for &sf_id in &sub_frames_to_remove {
            uow.remove_frame(&sf_id)?;
        }
        let mut updated = uow
            .get_frame(&frame_id)?
            .ok_or_else(|| anyhow!("Frame not found"))?;
        updated.child_order.retain(|entry| {
            if *entry < 0 {
                let sf_id = (-entry) as EntityId;
                !sub_frames_to_remove.contains(&sf_id)
            } else {
                true
            }
        });
        updated.updated_at = now;
        uow.update_frame(&updated)?;
    }

    Ok(())
}

fn execute_delete(
    uow: &mut Box<dyn DeleteTextUnitOfWorkTrait>,
    dto: &DeleteTextDto,
) -> Result<(DeleteTextResultDto, EntityTreeSnapshot)> {
    if dto.position == dto.anchor {
        let root = uow
            .get_root(&ROOT_ENTITY_ID)?
            .ok_or_else(|| anyhow!("Root entity not found"))?;
        let doc_ids = uow.get_root_relationship(&root.id, &RootRelationshipField::Document)?;
        let doc_id = *doc_ids
            .first()
            .ok_or_else(|| anyhow!("Root has no document"))?;
        let snapshot = uow.snapshot_document(&[doc_id])?;
        return Ok((
            DeleteTextResultDto {
                new_position: dto.position,
                deleted_text: String::new(),
            },
            snapshot,
        ));
    }

    let start = std::cmp::min(dto.position, dto.anchor);
    let end = std::cmp::max(dto.position, dto.anchor);

    let store = uow.store();

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

    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;

    let get_table_cell_frames = |table_id: &EntityId| -> anyhow::Result<Vec<EntityId>> {
        let cell_ids = uow.get_table_relationship(table_id, &TableRelationshipField::Cells)?;
        let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
        let mut cells: Vec<TableCell> = cells_opt.into_iter().flatten().collect();
        cells.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));
        Ok(cells.into_iter().filter_map(|c| c.cell_frame).collect())
    };
    let all_block_ids = collect_block_ids_recursive(
        &|id| uow.get_frame(id),
        &|id, field| uow.get_frame_relationship(id, field),
        &get_table_cell_frames,
        &frame_id,
    )?;

    let blocks_opt = uow.get_block_multi(&all_block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();

    // Refresh stored block positions from child_order + text_length, since
    // insert_text's fast path leaves them stale. Cell-frame blocks remain
    // in their cell-local position space.
    let root_frame = uow
        .get_frame(&frame_id)?
        .ok_or_else(|| anyhow!("Root frame not found"))?;
    let mut running: i64 = 0;
    let mut blocks_to_refresh: Vec<Block> = Vec::new();
    for &entry in &root_frame.child_order {
        if entry <= 0 {
            continue;
        }
        let id = entry as EntityId;
        if let Some(b) = blocks.iter_mut().find(|b| b.id == id) {
            if b.document_position != running {
                b.document_position = running;
                blocks_to_refresh.push(b.clone());
            }
            running += block_char_length(b, &store) + 1;
        }
    }
    if !blocks_to_refresh.is_empty() {
        uow.update_block_multi(&blocks_to_refresh)?;
    }
    blocks.sort_by_key(|b| b.document_position);

    let (start_block, start_block_idx, start_offset) =
        find_block_at_position(&blocks, start, &uow.store())?;

    // ── Cell selection safety: detect cross-cell deletion ──────────
    let table_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Tables)?;
    let mut block_to_cell_frame: std::collections::HashMap<EntityId, EntityId> =
        std::collections::HashMap::new();
    for &tid in &table_ids {
        let cell_ids = uow.get_table_relationship(&tid, &TableRelationshipField::Cells)?;
        let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
        for cell in cells_opt.into_iter().flatten() {
            if let Some(cf_id) = cell.cell_frame {
                let blk_ids =
                    uow.get_frame_relationship(&cf_id, &FrameRelationshipField::Blocks)?;
                for bid in blk_ids {
                    block_to_cell_frame.insert(bid, cf_id);
                }
            }
        }
    }

    let is_cross_cell = {
        let mut first_cell: Option<Option<EntityId>> = None;
        let mut cross = false;
        for block in &blocks {
            if block.document_position + block_char_length(block, &store) < start
                || block.document_position > end
            {
                continue;
            }
            let cell = block_to_cell_frame.get(&block.id).copied();
            match first_cell {
                None => first_cell = Some(cell),
                Some(fc) if fc != cell => {
                    cross = true;
                    break;
                }
                _ => {}
            }
        }
        cross
    };

    if is_cross_cell {
        let now = chrono::Utc::now();
        let mut total_chars_removed: i64 = 0;

        let mut affected_set: std::collections::HashSet<EntityId> =
            std::collections::HashSet::new();
        let mut affected_cell_frames: Vec<EntityId> = Vec::new();
        for block in &blocks {
            if block.document_position + block_char_length(block, &store) >= start
                && block.document_position <= end
                && let Some(&cf_id) = block_to_cell_frame.get(&block.id)
                && affected_set.insert(cf_id)
            {
                affected_cell_frames.push(cf_id);
            }
        }

        for cf_id in &affected_cell_frames {
            let frame = uow
                .get_frame(cf_id)?
                .ok_or_else(|| anyhow!("Cell frame not found"))?;
            let blk_ids = uow.get_frame_relationship(cf_id, &FrameRelationshipField::Blocks)?;
            let blk_opts = uow.get_block_multi(&blk_ids)?;
            let mut cell_blocks: Vec<Block> = blk_opts.into_iter().flatten().collect();
            cell_blocks.sort_by_key(|b| b.document_position);

            if cell_blocks.is_empty() {
                continue;
            }

            let cell_chars: i64 = cell_blocks
                .iter()
                .map(|b| block_char_length(b, &store))
                .sum();
            total_chars_removed += cell_chars;

            clear_block(uow, &cell_blocks[0], now)?;

            let extra_block_ids: Vec<EntityId> = cell_blocks[1..].iter().map(|b| b.id).collect();
            for &eid in &extra_block_ids {
                drop_block_runs_and_images(uow.as_ref(), eid);
                uow.remove_block(&eid)?;
            }

            let mut updated_frame = frame.clone();
            updated_frame.child_order = vec![cell_blocks[0].id as i64];
            updated_frame.updated_at = now;
            uow.update_frame(&updated_frame)?;
        }

        let mut tables_to_remove: Vec<EntityId> = Vec::new();
        for &tid in &table_ids {
            let cell_ids = uow.get_table_relationship(&tid, &TableRelationshipField::Cells)?;
            let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
            let cells: Vec<TableCell> = cells_opt.into_iter().flatten().collect();

            let all_affected = cells
                .iter()
                .all(|c| c.cell_frame.is_some_and(|cf| affected_set.contains(&cf)));
            if !all_affected || cells.is_empty() {
                continue;
            }

            let mut table_min_pos = i64::MAX;
            let mut table_max_pos = i64::MIN;
            for c in &cells {
                if let Some(cf_id) = c.cell_frame {
                    let blk_ids =
                        uow.get_frame_relationship(&cf_id, &FrameRelationshipField::Blocks)?;
                    let blk_opts = uow.get_block_multi(&blk_ids)?;
                    for b in blk_opts.into_iter().flatten() {
                        table_min_pos = table_min_pos.min(b.document_position);
                        table_max_pos =
                            table_max_pos.max(b.document_position + block_char_length(&b, &store));
                    }
                }
            }

            if start < table_min_pos || end > table_max_pos {
                for c in &cells {
                    if let Some(cf_id) = c.cell_frame {
                        uow.remove_frame(&cf_id)?;
                    }
                    uow.remove_table_cell(&c.id)?;
                }

                let root_frame = uow
                    .get_frame(&frame_id)?
                    .ok_or_else(|| anyhow!("Root frame not found"))?;
                for &entry in &root_frame.child_order {
                    if entry < 0 {
                        let anchor_id = (-entry) as EntityId;
                        if let Some(anchor) = uow.get_frame(&anchor_id)?
                            && anchor.table == Some(tid)
                        {
                            uow.remove_frame(&anchor_id)?;
                            break;
                        }
                    }
                }

                uow.remove_table(&tid)?;
                tables_to_remove.push(tid);
            }
        }

        if !tables_to_remove.is_empty() {
            let root_frame = uow
                .get_frame(&frame_id)?
                .ok_or_else(|| anyhow!("Root frame not found"))?;
            let mut updated_root = root_frame.clone();
            updated_root.child_order.retain(|entry| {
                if *entry < 0 {
                    let anchor_id = (-entry) as EntityId;
                    !tables_to_remove
                        .iter()
                        .any(|_| uow.get_frame(&anchor_id).ok().flatten().is_none())
                } else {
                    true
                }
            });
            updated_root.updated_at = now;
            uow.update_frame(&updated_root)?;
        }

        // ── Handle non-cell blocks in the selection range ──────────
        let mut non_cell_blocks_to_remove: Vec<EntityId> = Vec::new();
        let mut first_non_cell: Option<&Block> = None;
        let mut last_non_cell: Option<&Block> = None;

        for block in &blocks {
            let block_start = block.document_position;
            let block_end = block_start + block_char_length(block, &store);
            if block_end < start || block_start >= end {
                continue;
            }
            if block_to_cell_frame.contains_key(&block.id) {
                continue;
            }
            if first_non_cell.is_none() {
                first_non_cell = Some(block);
            }
            last_non_cell = Some(block);
        }

        let first_id = first_non_cell.map(|b| b.id);
        let last_id = last_non_cell.map(|b| b.id);
        let first_is_partial = first_non_cell.is_some_and(|b| start > b.document_position);
        let last_is_partial =
            last_non_cell.is_some_and(|b| end < b.document_position + block_char_length(b, &store));

        for block in &blocks {
            let block_start = block.document_position;
            let block_end = block_start + block_char_length(block, &store);
            if block_end < start || block_start >= end {
                continue;
            }
            if block_to_cell_frame.contains_key(&block.id) {
                continue;
            }

            let is_first = Some(block.id) == first_id && first_is_partial;
            let is_last = Some(block.id) == last_id && last_is_partial;

            if is_first || is_last {
                let local_char_start = if is_first {
                    (start - block_start) as i64
                } else {
                    0
                };
                let local_char_end = if is_last {
                    (end - block_start) as i64
                } else {
                    block_char_length(block, &store)
                };
                let chars_removed_this =
                    delete_char_range_in_block(uow, block, local_char_start, local_char_end)?;
                total_chars_removed += chars_removed_this;
            } else {
                total_chars_removed += block_char_length(block, &store);
                drop_block_runs_and_images(uow.as_ref(), block.id);
                uow.remove_block(&block.id)?;
                non_cell_blocks_to_remove.push(block.id);
            }
        }

        if !non_cell_blocks_to_remove.is_empty() {
            let all_frame_ids =
                uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
            for &fid in &all_frame_ids {
                if let Some(f) = uow.get_frame(&fid)? {
                    let old_len = f.child_order.len();
                    let mut updated = f.clone();
                    updated
                        .child_order
                        .retain(|id| !non_cell_blocks_to_remove.contains(&(*id as EntityId)));
                    if updated.child_order.len() != old_len {
                        updated.updated_at = now;
                        uow.update_frame(&updated)?;
                    }
                }
            }
        }

        // Recursive prune: walk the whole frame tree and remove every
        // non-table sub-frame that lost all its blocks and sub-frames.
        // The previous root-only walk left nested blockquotes (depth >= 2)
        // orphaned in the entity store when their content was deleted.
        prune_empty_subframes_recursive(uow, frame_id, now)?;

        {
            let list_ids =
                uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Lists)?;
            let mut lists_to_remove: Vec<EntityId> = Vec::new();
            let remaining_frame_ids =
                uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
            let mut all_remaining_block_ids: Vec<EntityId> = Vec::new();
            for &fid in &remaining_frame_ids {
                let blk_ids = uow.get_frame_relationship(&fid, &FrameRelationshipField::Blocks)?;
                all_remaining_block_ids.extend(blk_ids);
            }
            let remaining_blocks_opt = uow.get_block_multi(&all_remaining_block_ids)?;
            let remaining_list_refs: std::collections::HashSet<EntityId> = remaining_blocks_opt
                .into_iter()
                .flatten()
                .filter_map(|b| b.list)
                .collect();
            for &lid in &list_ids {
                if !remaining_list_refs.contains(&lid) {
                    lists_to_remove.push(lid);
                }
            }
            for &lid in &lists_to_remove {
                uow.remove_list(&lid)?;
            }
        }

        let remaining_block_count = {
            let get_tcf = |table_id: &EntityId| -> anyhow::Result<Vec<EntityId>> {
                let cids = uow.get_table_relationship(table_id, &TableRelationshipField::Cells)?;
                let cs = uow.get_table_cell_multi(&cids)?;
                let mut s: Vec<TableCell> = cs.into_iter().flatten().collect();
                s.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));
                Ok(s.into_iter().filter_map(|c| c.cell_frame).collect())
            };
            let candidate_ids = collect_block_ids_recursive(
                &|id| uow.get_frame(id),
                &|id, field| uow.get_frame_relationship(id, field),
                &get_tcf,
                &frame_id,
            )?;
            let opts = uow.get_block_multi(&candidate_ids)?;
            opts.into_iter().flatten().count()
        };
        if remaining_block_count == 0 {
            let empty_block = Block {
                document_position: 0,
                ..Block::default()
            };
            let created = uow.create_block(&empty_block, frame_id, -1)?;
            let f = uow
                .get_frame(&frame_id)?
                .ok_or_else(|| anyhow!("Frame not found"))?;
            let mut uf = f.clone();
            uf.child_order.push(created.id as i64);
            uf.updated_at = now;
            uow.update_frame(&uf)?;

            // Cross-block delete can leave stale rope-offset entries (e.g.
            // table-cell blocks that were cascade-removed via frame
            // deletion never went through `rope_remove_block`, and the
            // table-anchor sentinel can survive too). Now that every
            // entity-store block is gone, drop everything in the rope and
            // re-register a single empty block matching the entity we just
            // created. No-op under default backend.
            common::database::rope_helpers::rope_reset(&uow.store());
            common::database::rope_helpers::rope_append_empty_block(&uow.store(), created.id);
        }

        let actual_block_count = {
            let all_fids =
                uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
            let mut count = 0i64;
            for &fid in &all_fids {
                let blk_ids = uow.get_frame_relationship(&fid, &FrameRelationshipField::Blocks)?;
                count += blk_ids.len() as i64;
            }
            count
        };
        let mut updated_doc = document.clone();
        updated_doc.character_count -= total_chars_removed;
        if updated_doc.character_count < 0 {
            updated_doc.character_count = 0;
        }
        updated_doc.block_count = actual_block_count;
        updated_doc.updated_at = now;
        uow.update_document(&updated_doc)?;

        return Ok((
            DeleteTextResultDto {
                new_position: start,
                deleted_text: String::new(),
            },
            snapshot,
        ));
    }
    // ── End cell selection safety ──────────────────────────────────

    let (end_block, end_block_idx, end_offset) =
        find_block_at_position(&blocks, end, &uow.store())?;
    let delete_len = end - start;

    if start_block_idx == end_block_idx {
        // Same-block delete: splice plain_text + format_runs + block_images.
        let (_, images) = read_block_runs_and_images(&**uow, start_block.id);
        let store = uow.store();
        let start_block_text = block_content_via_store(&start_block, &store);
        let byte_so = logical_offset_to_byte(&start_block_text, &images, start_offset);
        let byte_eo = logical_offset_to_byte(&start_block_text, &images, end_offset);

        let deleted_text: String = start_block_text[byte_so as usize..byte_eo as usize].to_string();

        let mut new_plain =
            String::with_capacity(start_block_text.len() - (byte_eo - byte_so) as usize);
        new_plain.push_str(&start_block_text[..byte_so as usize]);
        new_plain.push_str(&start_block_text[byte_eo as usize..]);
        {
            let mut runs_map = store.format_runs.write();
            let runs = runs_map.entry(start_block.id).or_default();
            shift_runs_for_delete(runs, byte_so, byte_eo);
            debug_assert_well_formed(runs, new_plain.len());
        }
        let _images_removed = {
            let mut images_map = store.block_images.write();
            let images = images_map.entry(start_block.id).or_default();
            shift_images_for_delete(images, byte_so, byte_eo) as i64
        };

        // Same-block delete: splice the deleted bytes out of the rope.
        // The cross-block merge path below handles the boundary-newline
        // collapse separately.
        common::database::rope_helpers::rope_delete_in_block(
            &store,
            start_block.id,
            byte_so,
            byte_eo,
        );

        let mut updated_block = start_block.clone();
        updated_block.updated_at = chrono::Utc::now();
        uow.update_block(&updated_block)?;

        // Position-refresh loop: only run when rope can't be the
        // source of truth. For rope-clean docs, readers derive from
        // `BlockOffsetIndex`; this O(N) walk would be wasted work.
        if !common::database::rope_helpers::rope_positions_match_flow(&store) {
            let mut blocks_to_update: Vec<Block> = Vec::new();
            for b in &blocks[(start_block_idx + 1)..] {
                let mut ub = b.clone();
                ub.document_position -= delete_len;
                ub.updated_at = chrono::Utc::now();
                blocks_to_update.push(ub);
            }
            if !blocks_to_update.is_empty() {
                uow.update_block_multi(&blocks_to_update)?;
            }
        }

        let mut updated_doc = document.clone();
        updated_doc.character_count -= delete_len;
        updated_doc.updated_at = chrono::Utc::now();
        uow.update_document(&updated_doc)?;

        Ok((
            DeleteTextResultDto {
                new_position: start,
                deleted_text,
            },
            snapshot,
        ))
    } else {
        // Cross-block delete: merge end_block's tail into start_block.
        let now = chrono::Utc::now();

        // Compute byte offsets in each affected block.
        let store_for_text = uow.store();
        let start_block_text = block_content_via_store(&start_block, &store_for_text);
        let end_block_text = block_content_via_store(&end_block, &store_for_text);
        let middle_block_texts: Vec<String> = blocks[(start_block_idx + 1)..end_block_idx]
            .iter()
            .map(|b| block_content_via_store(b, &store_for_text))
            .collect();
        drop(store_for_text);
        let (_, start_images) = read_block_runs_and_images(&**uow, start_block.id);
        let byte_so = logical_offset_to_byte(&start_block_text, &start_images, start_offset);
        let (_, end_images) = read_block_runs_and_images(&**uow, end_block.id);
        let byte_eo = logical_offset_to_byte(&end_block_text, &end_images, end_offset);

        // Collect deleted_text for the result DTO.
        let mut deleted_text = String::new();
        deleted_text.push_str(&start_block_text[byte_so as usize..]);
        for mt in &middle_block_texts {
            deleted_text.push('\n');
            deleted_text.push_str(mt);
        }
        deleted_text.push('\n');
        deleted_text.push_str(&end_block_text[..byte_eo as usize]);

        // Build merged plain_text: start_block[..byte_so] + end_block[byte_eo..]
        let start_kept = &start_block_text[..byte_so as usize];
        let end_kept = &end_block_text[byte_eo as usize..];
        let merged_plain = format!("{}{}", start_kept, end_kept);

        // Build merged format_runs:
        //   start_runs clipped to [..byte_so), then end_runs from [byte_eo..)
        //   rebased to start at (byte_so - byte_eo) shift.
        let store = uow.store();
        let (start_runs_orig, _) = read_block_runs_and_images(&**uow, start_block.id);
        let (end_runs_orig, _) = read_block_runs_and_images(&**uow, end_block.id);

        let mut merged_runs: Vec<FormatRun> = Vec::new();
        // Left half: keep runs strictly before byte_so, clip straddling.
        for run in &start_runs_orig {
            if run.byte_end <= byte_so {
                merged_runs.push(run.clone());
            } else if run.byte_start < byte_so {
                merged_runs.push(FormatRun {
                    byte_start: run.byte_start,
                    byte_end: byte_so,
                    format: run.format.clone(),
                });
            }
        }
        // Right half: take end_block runs from byte_eo onwards, rebase to byte_so.
        for run in &end_runs_orig {
            if run.byte_start >= byte_eo {
                merged_runs.push(FormatRun {
                    byte_start: run.byte_start - byte_eo + byte_so,
                    byte_end: run.byte_end - byte_eo + byte_so,
                    format: run.format.clone(),
                });
            } else if run.byte_end > byte_eo {
                merged_runs.push(FormatRun {
                    byte_start: byte_so,
                    byte_end: run.byte_end - byte_eo + byte_so,
                    format: run.format.clone(),
                });
            }
        }
        common::format_runs::coalesce_in_place(&mut merged_runs);
        debug_assert_well_formed(&merged_runs, merged_plain.len());

        // Build merged block_images.
        let mut merged_images: Vec<ImageAnchor> = Vec::new();
        for img in &start_images {
            if img.byte_offset < byte_so {
                merged_images.push(img.clone());
            }
        }
        for img in &end_images {
            if img.byte_offset >= byte_eo {
                let mut new_img = img.clone();
                new_img.byte_offset = new_img.byte_offset - byte_eo + byte_so;
                merged_images.push(new_img);
            }
        }

        // Write merged state to start_block.
        let mut updated_start = start_block.clone();
        updated_start.updated_at = now;
        uow.update_block(&updated_start)?;

        store
            .format_runs
            .write()
            .insert(start_block.id, merged_runs);
        store
            .block_images
            .write()
            .insert(start_block.id, merged_images);

        // Cross-block merge: delete the rope range from
        // `start_block + byte_so` through `end_block + byte_eo`,
        // remove the intermediate + end-block index entries, and
        // shift subsequent offsets.
        common::database::rope_helpers::rope_merge_block_range(
            &store,
            start_block.id,
            byte_so,
            end_block.id,
            byte_eo,
        );

        // Remove intermediate and end blocks.
        let blocks_to_remove: Vec<EntityId> = blocks[(start_block_idx + 1)..=end_block_idx]
            .iter()
            .map(|b| b.id)
            .collect();
        let removed_count = blocks_to_remove.len() as i64;

        for block_id in &blocks_to_remove {
            drop_block_runs_and_images(uow.as_ref(), *block_id);
            // `rope_merge_block_range` only drains entries in the
            // rope-adjacent slice [start_idx+1..=end_idx]. Blocks
            // whose rope position is outside that slice (notably
            // table cells, which live at top_level_frame_end_byte
            // for their parent frame, far from the main-flow
            // selection) stay in `block_offsets` with stale entries.
            // Drop them here so the rope index doesn't carry
            // dangling block ids past delete_text.
            common::database::rope_helpers::rope_remove_block(&uow.store(), *block_id);
            uow.remove_block(block_id)?;
        }

        // Group removed blocks by their owning frame, then update each
        // affected frame's child_order. Without this, sub-frame (e.g.
        // blockquote) child_order can be left with dangling entries when
        // the cross-block merge crosses a frame boundary — the cell-only
        // `block_to_cell_frame` map silently falls back to the root.
        let now = chrono::Utc::now();
        let mut blocks_by_frame: std::collections::HashMap<EntityId, Vec<EntityId>> =
            std::collections::HashMap::new();
        for &bid in &blocks_to_remove {
            let owning = if let Some(&cf) = block_to_cell_frame.get(&bid) {
                cf
            } else {
                find_block_owner_frame(uow.as_ref(), frame_id, bid)?.unwrap_or(frame_id)
            };
            blocks_by_frame.entry(owning).or_default().push(bid);
        }
        for (owning_frame_id, removed_in_frame) in blocks_by_frame {
            let frame = uow
                .get_frame(&owning_frame_id)?
                .ok_or_else(|| anyhow!("Frame not found"))?;
            let mut updated_frame = frame.clone();
            updated_frame
                .child_order
                .retain(|entry| !(*entry > 0 && removed_in_frame.contains(&(*entry as EntityId))));
            updated_frame.blocks =
                uow.get_frame_relationship(&owning_frame_id, &FrameRelationshipField::Blocks)?;
            updated_frame.updated_at = now;
            uow.update_frame(&updated_frame)?;
        }

        // A cross-block merge can empty a sub-frame (the user deleted all
        // its blocks in one sweep). Prune empty non-table frames at every
        // depth so the entity store never carries orphans.
        prune_empty_subframes_recursive(uow, frame_id, now)?;

        // Use the pre-mutation texts captured at line 653 — by now the
        // rope merge has run and `block_char_length(start_block)` reflects
        // the post-merge state (start_kept + end_kept), not the original.
        let start_chars = start_block_text.chars().count() as i64;
        let chars_from_start = start_chars - start_offset;
        let chars_from_middle: i64 = middle_block_texts
            .iter()
            .map(|t| t.chars().count() as i64)
            .sum();
        let chars_from_end = end_offset;
        let chars_removed = chars_from_start + chars_from_middle + chars_from_end;

        // Position-refresh loop: see same gate in the same-block
        // delete path above for rationale.
        if !common::database::rope_helpers::rope_positions_match_flow(&store) {
            let mut blocks_to_update: Vec<Block> = Vec::new();
            for b in &blocks[(end_block_idx + 1)..] {
                let mut ub = b.clone();
                ub.document_position -= delete_len;
                ub.updated_at = chrono::Utc::now();
                blocks_to_update.push(ub);
            }
            if !blocks_to_update.is_empty() {
                uow.update_block_multi(&blocks_to_update)?;
            }
        }

        let mut updated_doc = document.clone();
        updated_doc.character_count -= chars_removed;
        updated_doc.block_count -= removed_count;
        updated_doc.updated_at = chrono::Utc::now();
        uow.update_document(&updated_doc)?;

        Ok((
            DeleteTextResultDto {
                new_position: start,
                deleted_text,
            },
            snapshot,
        ))
    }
}

/// Delete a char range inside a single block (used by cross-cell partial-
/// truncation path). Returns the number of logical positions removed.
fn delete_char_range_in_block(
    uow: &mut Box<dyn DeleteTextUnitOfWorkTrait>,
    block: &Block,
    start_offset: i64,
    end_offset: i64,
) -> Result<i64> {
    if end_offset <= start_offset {
        return Ok(0);
    }
    let store = uow.store();
    let images_before = store
        .block_images
        .read()
        .get(&block.id)
        .cloned()
        .unwrap_or_default();

    let block_text = block_content_via_store(block, &store);
    let byte_start = logical_offset_to_byte(&block_text, &images_before, start_offset);
    let byte_end = logical_offset_to_byte(&block_text, &images_before, end_offset);

    let removed_text_chars = block_text[byte_start as usize..byte_end as usize]
        .chars()
        .count() as i64;

    let mut new_plain = String::with_capacity(block_text.len() - (byte_end - byte_start) as usize);
    new_plain.push_str(&block_text[..byte_start as usize]);
    new_plain.push_str(&block_text[byte_end as usize..]);

    {
        let mut runs_map = store.format_runs.write();
        let runs = runs_map.entry(block.id).or_default();
        shift_runs_for_delete(runs, byte_start, byte_end);
        debug_assert_well_formed(runs, new_plain.len());
    }
    let images_removed = {
        let mut images_map = store.block_images.write();
        let images = images_map.entry(block.id).or_default();
        shift_images_for_delete(images, byte_start, byte_end) as i64
    };

    // Mirror the delete into the global rope.
    common::database::rope_helpers::rope_delete_in_block(&store, block.id, byte_start, byte_end);

    let positions_removed = removed_text_chars + images_removed;
    let mut updated = block.clone();
    updated.updated_at = chrono::Utc::now();
    uow.update_block(&updated)?;
    Ok(positions_removed)
}

impl DeleteTextUseCase {
    pub fn new(uow_factory: Box<dyn DeleteTextUnitOfWorkFactoryTrait>) -> Self {
        DeleteTextUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
            last_result: None,
            last_merge_time: None,
            is_single_char_origin: false,
        }
    }

    pub fn execute(&mut self, dto: &DeleteTextDto) -> Result<DeleteTextResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_delete(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());
        self.last_result = Some(result.clone());
        self.last_merge_time = Some(Instant::now());
        self.is_single_char_origin = (dto.position - dto.anchor).abs() == 1;

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for DeleteTextUseCase {
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
        let (_, snapshot) = execute_delete(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn can_merge(&self, other: &dyn UndoRedoCommand) -> bool {
        let Some(other_cmd) = other.as_any().downcast_ref::<DeleteTextUseCase>() else {
            return false;
        };

        let (Some(self_dto), Some(self_result), Some(self_time)) =
            (&self.last_dto, &self.last_result, &self.last_merge_time)
        else {
            return false;
        };
        let (Some(other_dto), Some(_other_result), Some(other_time)) = (
            &other_cmd.last_dto,
            &other_cmd.last_result,
            &other_cmd.last_merge_time,
        ) else {
            return false;
        };

        if other_time.duration_since(*self_time) > std::time::Duration::from_secs(2) {
            return false;
        }

        if !self.is_single_char_origin {
            return false;
        }
        if (other_dto.position - other_dto.anchor).abs() != 1 {
            return false;
        }

        let self_is_backspace = self_dto.position > self_dto.anchor;
        let other_is_backspace = other_dto.position > other_dto.anchor;
        if self_is_backspace != other_is_backspace {
            return false;
        }

        if self_is_backspace {
            if other_dto.position.max(other_dto.anchor) != self_result.new_position {
                return false;
            }
        } else if other_dto.position.min(other_dto.anchor) != self_result.new_position {
            return false;
        }

        let self_range = (self_dto.position - self_dto.anchor).abs();
        if self_range + 1 > 200 {
            return false;
        }

        if let Some(last_deleted_char) = self_result.deleted_text.chars().next()
            && (last_deleted_char.is_whitespace() || is_word_boundary_punct(last_deleted_char))
        {
            return false;
        }

        true
    }

    fn merge(&mut self, other: &dyn UndoRedoCommand) -> bool {
        let Some(other_cmd) = other.as_any().downcast_ref::<DeleteTextUseCase>() else {
            return false;
        };

        let Some(self_dto) = &self.last_dto else {
            return false;
        };
        let Some(other_result) = &other_cmd.last_result else {
            return false;
        };
        let Some(other_time) = &other_cmd.last_merge_time else {
            return false;
        };

        let self_is_backspace = self_dto.position > self_dto.anchor;

        let combined_dto = if self_is_backspace {
            DeleteTextDto {
                position: self_dto.position,
                anchor: self_dto.anchor - 1,
            }
        } else {
            DeleteTextDto {
                position: self_dto.position,
                anchor: self_dto.anchor + 1,
            }
        };

        self.last_dto = Some(combined_dto);
        self.last_result = Some(other_result.clone());
        self.last_merge_time = Some(*other_time);

        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
