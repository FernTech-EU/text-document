use super::editing_helpers::{
    Swept, collect_block_ids_recursive, find_block_at_position, impl_nested_content_reader,
    is_word_boundary_punct, position_roots,
};
use crate::DeleteTextDto;
use crate::DeleteTextResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::block_offset_index::OffsetMarker;
use common::database::rope_helpers::{
    block_char_length, block_content_via_store, range_covers_table_anchor, refresh_block_positions,
    rope_remove_markers, snap_off_table_anchor, table_anchor_position,
};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::direct_access::table::TableRelationshipField;
use common::entities::{Block, Document, Frame, Root, Table, TableCell};
use common::format_runs::{
    FootnoteRefAnchor, FormatRun, ImageAnchor, debug_assert_well_formed, logical_offset_to_byte,
    shift_footnote_refs_for_delete, shift_images_for_delete, shift_runs_for_delete,
};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;
use std::collections::{HashMap, HashSet};
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
#[macros::uow_action(entity = "Block", action = "RemoveMulti")]
#[macros::uow_action(entity = "Block", action = "GetRelationship")]
#[macros::uow_action(entity = "Table", action = "Get")]
#[macros::uow_action(entity = "Table", action = "GetRelationship")]
#[macros::uow_action(entity = "Table", action = "Remove")]
#[macros::uow_action(entity = "Table", action = "RemoveMulti")]
#[macros::uow_action(entity = "TableCell", action = "GetMulti")]
#[macros::uow_action(entity = "TableCell", action = "Remove")]
#[macros::uow_action(entity = "TableCell", action = "RemoveMulti")]
#[macros::uow_action(entity = "Frame", action = "Remove")]
#[macros::uow_action(entity = "Frame", action = "RemoveMulti")]
#[macros::uow_action(entity = "List", action = "Remove")]
#[macros::uow_action(entity = "List", action = "RemoveMulti")]
pub trait DeleteTextUnitOfWorkTrait: CommandUnitOfWork {}

impl_nested_content_reader!(dyn DeleteTextUnitOfWorkTrait);

/// The refusal of a deletion that would remove nothing: its range, once off a
/// table's anchor, is empty, or it only meets the blocks at its ends
/// (Backspace or Delete next to a table, between two table cells, or at the
/// edge of a footnote's body, none of which joins anything). It carries where
/// the caret goes.
///
/// A use case the controller runs without an error goes on the undo stack, so
/// a deletion that succeeded at doing nothing gave the Edit menu an Undo that
/// changed nothing, and the writer had to undo twice to reach their last real
/// edit. Callers that delete on behalf of a keystroke treat this error as an
/// edit that removed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NothingToDelete {
    /// Where the caret goes.
    pub new_position: i64,
}

impl std::fmt::Display for NothingToDelete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "nothing to delete (caret to {})", self.new_position)
    }
}

impl std::error::Error for NothingToDelete {}

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

/// Read a block's footnote references.
fn read_block_footnote_refs(
    uow: &dyn DeleteTextUnitOfWorkTrait,
    block_id: EntityId,
) -> Vec<FootnoteRefAnchor> {
    uow.store()
        .block_footnote_refs
        .read()
        .get(&block_id)
        .cloned()
        .unwrap_or_default()
}

/// Store `notes` as `block_id`'s footnote references, leaving no entry for a
/// block that holds none.
fn write_block_footnote_refs(
    uow: &dyn DeleteTextUnitOfWorkTrait,
    block_id: EntityId,
    notes: Vec<FootnoteRefAnchor>,
) {
    let store = uow.store();
    let mut notes_map = store.block_footnote_refs.write();
    if notes.is_empty() {
        notes_map.remove(&block_id);
    } else {
        notes_map.insert(block_id, notes);
    }
}

/// Delete `[byte_start..byte_end)` from a block's footnote references: the
/// ones inside go with the text, the ones after move back.
fn delete_block_footnote_refs(
    uow: &dyn DeleteTextUnitOfWorkTrait,
    block_id: EntityId,
    byte_start: u32,
    byte_end: u32,
) {
    let mut notes = read_block_footnote_refs(uow, block_id);
    if notes.is_empty() {
        return;
    }
    shift_footnote_refs_for_delete(&mut notes, byte_start, byte_end);
    write_block_footnote_refs(uow, block_id, notes);
}

/// Reset a block to empty state: clears its format_runs, block_images and
/// footnote references and bumps `updated_at`. Its text leaves the rope
/// separately, through `rope_clear_blocks`, which empties every cleared block
/// in one walk of the offset index where clearing them one at a time walked it
/// once per block.
fn clear_block(
    uow: &mut Box<dyn DeleteTextUnitOfWorkTrait>,
    block: &Block,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let mut updated = block.clone();
    updated.updated_at = now;
    uow.update_block(&updated)?;
    let store = uow.store();
    store.format_runs.write().insert(block.id, Vec::new());
    store.block_images.write().insert(block.id, Vec::new());
    store.block_footnote_refs.write().remove(&block.id);
    Ok(())
}

/// Drop the per-block run/image/footnote tables for a block that's about to
/// be removed entirely. Idempotent.
fn drop_block_runs_and_images(uow: &dyn DeleteTextUnitOfWorkTrait, block_id: EntityId) {
    let store = uow.store();
    store.format_runs.write().remove(&block_id);
    store.block_images.write().remove(&block_id);
    store.block_footnote_refs.write().remove(&block_id);
}

/// Remove every footnote definition frame the deletion emptied. A definition
/// is top-level, so the sub-frame prune under the main frame never sees it,
/// and one left without blocks is a note with no body that every writer
/// still visits.
fn prune_empty_definition_frames(
    uow: &mut Box<dyn DeleteTextUnitOfWorkTrait>,
    definition_frames: &[EntityId],
) -> Result<()> {
    let mut empty: Vec<EntityId> = Vec::new();
    for frame_id in definition_frames {
        let Some(frame) = uow.get_frame(frame_id)? else {
            continue;
        };
        let blocks = uow.get_frame_relationship(frame_id, &FrameRelationshipField::Blocks)?;
        if blocks.is_empty() && !frame.child_order.iter().any(|entry| *entry < 0) {
            empty.push(*frame_id);
        }
    }
    if !empty.is_empty() {
        uow.remove_frame_multi(&empty)?;
    }
    Ok(())
}

/// Walk the frame trees rooted at `root_ids` once and map every block to the
/// frame whose `child_order` lists it. Used by the cross-block merge to
/// correctly resolve sub-frame ownership when the deletion crosses a frame
/// boundary: the cell-only `block_to_cell_frame` map cannot answer this for
/// blockquote frames.
///
/// Frames are visited depth first in `child_order` order and the first frame
/// listing a block wins: the answer a search from the root for that one block
/// gives. The merge used to run that search once per deleted block, a walk
/// of the document per paragraph.
///
/// The walk stops at the first frame it cannot find and hands that error back
/// beside what it mapped. A block mapped before that point is one the search
/// finds before meeting the missing frame; for any other block the search
/// meets the missing frame first and fails, and so must the caller.
fn block_owner_frames(
    uow: &dyn DeleteTextUnitOfWorkTrait,
    root_ids: &[EntityId],
) -> (HashMap<EntityId, EntityId>, Option<anyhow::Error>) {
    fn walk(
        uow: &dyn DeleteTextUnitOfWorkTrait,
        frame_id: EntityId,
        owners: &mut HashMap<EntityId, EntityId>,
    ) -> Result<()> {
        let f = uow
            .get_frame(&frame_id)?
            .ok_or_else(|| anyhow!("Frame not found"))?;
        for &entry in &f.child_order {
            if entry > 0 {
                owners.entry(entry as EntityId).or_insert(frame_id);
            }
            if entry < 0 {
                walk(uow, (-entry) as EntityId, owners)?;
            }
        }
        Ok(())
    }
    let mut owners = HashMap::new();
    let stopped = root_ids
        .iter()
        .find_map(|root_id| walk(uow, *root_id, &mut owners).err());
    (owners, stopped)
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
        // In one call: each single removal rewrites and re-validates the
        // document's whole frame list, so emptying a document of many
        // quotations one frame at a time was quadratic in them.
        uow.remove_frame_multi(&sub_frames_to_remove)?;
        let removed: HashSet<EntityId> = sub_frames_to_remove.iter().copied().collect();
        let mut updated = uow
            .get_frame(&frame_id)?
            .ok_or_else(|| anyhow!("Frame not found"))?;
        updated.child_order.retain(|entry| {
            if *entry < 0 {
                let sf_id = (-entry) as EntityId;
                !removed.contains(&sf_id)
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

    let store = uow.store();

    // A range endpoint on a table's anchor stands for the table, not for a
    // block: a start moves into the table, an end back before it.
    let start = snap_off_table_anchor(&store, std::cmp::min(dto.position, dto.anchor), true);
    let end = snap_off_table_anchor(&store, std::cmp::max(dto.position, dto.anchor), false);

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

    if start >= end {
        return Err(NothingToDelete {
            new_position: std::cmp::min(dto.position, dto.anchor),
        }
        .into());
    }

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
    // The main flow's blocks and every footnote definition's: the rope holds
    // a definition where it was written, so a range across one covers its
    // blocks. Left out of this list, a merge took their text out of the rope
    // and left the blocks behind, and every read of them then sliced text
    // that was gone.
    let roots = position_roots(&|id| uow.get_frame(id), &frame_ids)?;
    let definition_frames: Vec<EntityId> = roots.iter().skip(1).copied().collect();
    let mut all_block_ids: Vec<EntityId> = Vec::new();
    // Which of those trees each block belongs to: the main text, or one
    // footnote's body.
    let mut root_of_block: HashMap<EntityId, EntityId> = HashMap::new();
    for root in &roots {
        let root_blocks = collect_block_ids_recursive(
            &|id| uow.get_frame(id),
            &|id, field| uow.get_frame_relationship(id, field),
            &get_table_cell_frames,
            root,
        )?;
        root_of_block.extend(root_blocks.iter().map(|block_id| (*block_id, *root)));
        all_block_ids.extend(root_blocks);
    }

    let blocks_opt = uow.get_block_multi(&all_block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();

    // Order the blocks by where they start. The stored `document_position`
    // lags the rope by whatever was typed since something last wrote it, so
    // it is read from the rope, as every use case reading positions does (see
    // `refresh_block_positions`). This used to recount it from the main
    // frame's own blocks alone, which left every blockquote, table cell and
    // footnote body out of the count: the blocks after a quotation were
    // written back at the wrong positions, sorted out of order, and a range
    // across them resolved its end before its start.
    refresh_block_positions(&mut blocks, &store);
    blocks.sort_by_key(|b| b.document_position);

    let (start_block, start_block_idx, start_offset) =
        find_block_at_position(&blocks, start, &uow.store())?;
    let (end_block, end_block_idx, end_offset) =
        find_block_at_position(&blocks, end, &uow.store())?;

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

    // Whether the range takes some of a block: it overlaps the block's text,
    // or the block is empty and the range runs across it, from before it to
    // past it. A range that only meets a block at one of its ends takes
    // nothing of it, and a cell the range takes something of is emptied whole
    // below. Counting a block the range only meets lost text: Backspace at
    // the start of the paragraph after a table cleared the table's last cell;
    // Backspace at the start of a cell cleared the cell before when that one
    // ended with an empty paragraph, and cleared its own cell, other
    // paragraphs and all, when its first paragraph was empty.
    let takes_text_of = |block: &Block| {
        let block_start = block.document_position;
        let block_end = block_start + block_char_length(block, &store);
        if block_start == block_end {
            start < block_start && block_start < end
        } else {
            block_start < end && block_end > start
        }
    };
    // Where the caller's range starts, before any snap off a table's anchor:
    // a range starting on the anchor holds the table's start.
    let requested_start = std::cmp::min(dto.position, dto.anchor);
    // A range whose ends lie in different cells, or one in a cell and one
    // outside the table, cannot be closed by joining its two blocks: the
    // join would pull a paragraph into a cell or a cell out of its table.
    let ends_in_different_cells =
        block_to_cell_frame.get(&start_block.id) != block_to_cell_frame.get(&end_block.id);
    // Nor can one whose ends lie in different trees: the main text and a
    // footnote's body, or two bodies. The rope holds a body between the
    // paragraphs around it, where no view shows it, so Backspace at the start
    // of the paragraph after a body joined that paragraph to the hidden note,
    // and Delete at the end of the paragraph before one pulled the note into
    // the prose and left its reference without a body. Such a range takes
    // what it covers of each block and joins nothing, as a range across cells
    // does; at the boundary itself it takes nothing.
    let ends_in_different_roots =
        root_of_block.get(&start_block.id) != root_of_block.get(&end_block.id);
    // Nor one that covers the start of a table, whatever its ends: from the
    // end of the paragraph before a table of one cell to the start of the
    // paragraph after it, the range takes text from that one cell only, and
    // joining its two ends removed the cell's blocks and left the table
    // itself behind, an anchor in the frames with nothing in the rope.
    let covers_a_table = range_covers_table_anchor(&store, start, end);
    let is_cross_cell = ends_in_different_cells || ends_in_different_roots || covers_a_table || {
        let mut first_cell: Option<Option<EntityId>> = None;
        let mut cross = false;
        for block in &blocks {
            if !takes_text_of(block) {
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
        // Where each block starts, read from the rope above: a cell's blocks
        // fetched again below carry the stored field, which lags it.
        let position_of: HashMap<EntityId, i64> =
            blocks.iter().map(|b| (b.id, b.document_position)).collect();
        // Where each block ends, before anything below empties it.
        let end_of: HashMap<EntityId, i64> = blocks
            .iter()
            .map(|b| (b.id, b.document_position + block_char_length(b, &store)))
            .collect();
        let starts_at = |b: &Block| {
            position_of
                .get(&b.id)
                .copied()
                .unwrap_or(b.document_position)
        };
        // Where each table starts (its anchor) and where the text of its last
        // cell ends, read before anything below changes the document. The
        // cells are emptied and their other paragraphs removed before the
        // tables are looked at: read afterwards, the removed paragraphs no
        // longer counted towards the table's end, and the rope, which still
        // held them, was no longer the position space, so the anchor had no
        // position and the table was taken to start at its first cell. A
        // range starting at the first cell, which only empties the table,
        // then removed it.
        let mut table_extents: Vec<(EntityId, i64, i64)> = Vec::new();
        for &tid in &table_ids {
            let cell_ids = uow.get_table_relationship(&tid, &TableRelationshipField::Cells)?;
            let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
            let mut table_min_pos = i64::MAX;
            let mut table_max_pos = i64::MIN;
            for cell in cells_opt.into_iter().flatten() {
                if let Some(cf_id) = cell.cell_frame {
                    let blk_ids =
                        uow.get_frame_relationship(&cf_id, &FrameRelationshipField::Blocks)?;
                    let blk_opts = uow.get_block_multi(&blk_ids)?;
                    for b in blk_opts.into_iter().flatten() {
                        let b_start = starts_at(&b);
                        table_min_pos = table_min_pos.min(b_start);
                        table_max_pos =
                            table_max_pos.max(end_of.get(&b.id).copied().unwrap_or(b_start));
                    }
                }
            }
            if table_min_pos > table_max_pos {
                continue;
            }
            // Where the table starts: its anchor. Where the rope is not the
            // position space the anchor takes no position, and the table
            // starts right before its first cell.
            let table_start = table_anchor_position(&store, tid).unwrap_or(table_min_pos - 1);
            table_extents.push((tid, table_start, table_max_pos));
        }
        // Everything the entity removals below take out of the document
        // leaves the rope in one pass at the end (see `rope_remove_markers`).
        // The removals only ever touched the entities, so the text of every
        // paragraph, table and extra cell block a deletion swept up stayed in
        // the rope, under index entries naming blocks that were gone.
        let mut leaving_rope: Vec<OffsetMarker> = Vec::new();

        let mut affected_set: std::collections::HashSet<EntityId> =
            std::collections::HashSet::new();
        let mut affected_cell_frames: Vec<EntityId> = Vec::new();
        for block in &blocks {
            if takes_text_of(block)
                && let Some(&cf_id) = block_to_cell_frame.get(&block.id)
                && affected_set.insert(cf_id)
            {
                affected_cell_frames.push(cf_id);
            }
        }

        // Every affected cell's blocks past its first, removed in one call below:
        // each removal call finds and rewrites the owners of what it removes, a
        // walk of every frame, so one call per cell cost a walk per cell. The
        // first blocks, emptied, leave the rope together the same way.
        let mut extra_block_ids: Vec<EntityId> = Vec::new();
        let mut cleared_block_ids: Vec<EntityId> = Vec::new();
        // What is nested in the cleared cells and in the removed tables,
        // removed after the tables loop.
        let mut swept = Swept::default();
        for cf_id in &affected_cell_frames {
            let frame = uow
                .get_frame(cf_id)?
                .ok_or_else(|| anyhow!("Cell frame not found"))?;
            let blk_ids = uow.get_frame_relationship(cf_id, &FrameRelationshipField::Blocks)?;
            let blk_opts = uow.get_block_multi(&blk_ids)?;
            let mut cell_blocks: Vec<Block> = blk_opts.into_iter().flatten().collect();
            cell_blocks.sort_by_key(starts_at);

            if cell_blocks.is_empty() {
                continue;
            }

            let cell_chars: i64 = cell_blocks
                .iter()
                .map(|b| block_char_length(b, &store))
                .sum();
            total_chars_removed += cell_chars;

            clear_block(uow, &cell_blocks[0], now)?;
            cleared_block_ids.push(cell_blocks[0].id);

            for extra in &cell_blocks[1..] {
                drop_block_runs_and_images(uow.as_ref(), extra.id);
                extra_block_ids.push(extra.id);
                leaving_rope.push(OffsetMarker::Block(extra.id));
            }
            // The cell keeps only its first block, so whatever is nested in
            // it goes too.
            swept.sweep_frame(&*uow, *cf_id, false)?;

            // `update_frame` writes `child_order` and keeps the frame's block
            // list as the store has it, so the removal below trims that list
            // whether it runs before this or after.
            let mut updated_frame = frame.clone();
            updated_frame.child_order = vec![cell_blocks[0].id as i64];
            updated_frame.updated_at = now;
            uow.update_frame(&updated_frame)?;
        }
        // Before anything below measures a cell block: the tables loop reads
        // the emptied blocks' lengths.
        common::database::rope_helpers::rope_clear_blocks(&store, &cleared_block_ids);
        if !extra_block_ids.is_empty() {
            uow.remove_block_multi(&extra_block_ids)?;
        }

        // What removing the tables the selection covers takes away, gathered
        // table by table and removed in one call per kind after the loop: each
        // removal call rewrites the whole list of the owner it removes from
        // (the document's frames or tables) and walks every owner to find it,
        // so one call per table cost a walk of the document per table.
        //
        // Each table's anchor frame, by the table it names: wherever it sits,
        // in the main frame, a quotation, a cell or a footnote. Looking in the
        // main frame's `child_order` alone left the anchor of a table pasted
        // anywhere else in place, naming a table that was gone.
        let mut anchor_frames: HashMap<EntityId, Vec<EntityId>> = HashMap::new();
        for fid in &frame_ids {
            if let Some(anchor) = uow.get_frame(fid)?
                && let Some(named) = anchor.table
            {
                anchor_frames.entry(named).or_default().push(*fid);
            }
        }
        for &(tid, table_start, table_max_pos) in &table_extents {
            if swept.table_set.contains(&tid) {
                // Nested in a cell the deletion cleared: already going.
                continue;
            }
            // The table goes when the range holds all of it, from its start
            // to the end of its last cell; a range over its cells alone
            // empties them. Whether every cell was touched used to decide it,
            // which held only while an empty cell at the range's end counted as
            // touched, and counting it lost the text of the cells beside it.
            if requested_start <= table_start && end >= table_max_pos {
                swept.sweep_table(&*uow, tid)?;
            }
        }
        // Every anchor frame of a table going away, nested ones included.
        let mut removed_anchor_parents: HashSet<EntityId> = HashSet::new();
        for tid in &swept.tables {
            for anchor_id in anchor_frames.get(tid).into_iter().flatten() {
                if let Some(parent) = uow.get_frame(anchor_id)?.and_then(|a| a.parent_frame) {
                    removed_anchor_parents.insert(parent);
                }
                swept.frames.push(*anchor_id);
            }
        }
        swept.frames.sort_unstable();
        swept.frames.dedup();
        leaving_rope.extend(swept.markers.iter().copied());

        if !swept.frames.is_empty() || !swept.tables.is_empty() {
            // The frames first: their blocks go with them.
            if !swept.frames.is_empty() {
                uow.remove_frame_multi(&swept.frames)?;
            }
            if !swept.cells.is_empty() {
                uow.remove_table_cell_multi(&swept.cells)?;
            }
            if !swept.tables.is_empty() {
                uow.remove_table_multi(&swept.tables)?;
            }

            // Drop every sub-frame entry whose frame is gone from each frame
            // that held a removed anchor: the removed anchors, and any other
            // entry already dangling there, as before.
            removed_anchor_parents.insert(frame_id);
            for parent_id in &removed_anchor_parents {
                let Some(parent) = uow.get_frame(parent_id)? else {
                    continue;
                };
                let mut updated_parent = parent.clone();
                updated_parent.child_order.retain(|entry| {
                    if *entry < 0 {
                        let anchor_id = (-entry) as EntityId;
                        uow.get_frame(&anchor_id).ok().flatten().is_some()
                    } else {
                        true
                    }
                });
                if updated_parent.child_order != parent.child_order {
                    updated_parent.updated_at = now;
                    uow.update_frame(&updated_parent)?;
                }
            }
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
                non_cell_blocks_to_remove.push(block.id);
                leaving_rope.push(OffsetMarker::Block(block.id));
            }
        }
        rope_remove_markers(&store, &leaving_rope);

        if !non_cell_blocks_to_remove.is_empty() {
            // In one call: each single removal rewrites and re-validates the
            // owning frame's whole block list, so deleting N paragraphs one at
            // a time cost N(N+1)/2 of those.
            uow.remove_block_multi(&non_cell_blocks_to_remove)?;
            let removed: HashSet<EntityId> = non_cell_blocks_to_remove.iter().copied().collect();
            let all_frame_ids =
                uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
            for &fid in &all_frame_ids {
                if let Some(f) = uow.get_frame(&fid)? {
                    let old_len = f.child_order.len();
                    let mut updated = f.clone();
                    updated
                        .child_order
                        .retain(|id| !removed.contains(&(*id as EntityId)));
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
        prune_empty_definition_frames(uow, &definition_frames)?;

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
            if !lists_to_remove.is_empty() {
                uow.remove_list_multi(&lists_to_remove)?;
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

            // The main flow is empty, but a footnote's body may still be in
            // the rope: the new block goes in front of whatever is left there.
            // Resetting the rope here, as this used to, threw a surviving
            // note's text away and left its blocks with no text at all.
            common::database::rope_helpers::rope_insert_empty_block_first(&uow.store(), created.id);
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

        // A cell the range starts in is emptied whole, so the caret goes to
        // where that cell starts: `start` itself may no longer exist once the
        // cell's text before it is gone. When the range took the cell's table
        // away, which it only does from the table's anchor or before it, the
        // caret goes where the range starts: the cell is gone with the table.
        let new_position = match block_to_cell_frame.get(&start_block.id) {
            Some(cell_frame) if swept.frames.contains(cell_frame) => requested_start.min(start),
            Some(cell_frame) if affected_set.contains(cell_frame) => blocks
                .iter()
                .filter(|b| block_to_cell_frame.get(&b.id) == Some(cell_frame))
                .map(|b| b.document_position)
                .min()
                .map_or(start, |cell_start| cell_start.min(start)),
            _ => start,
        };
        // A range that only meets the blocks at its ends, between two cells
        // or at the edge of a table or of a footnote's body, took nothing.
        // Whatever the path above wrote along the way is rolled back with the
        // transaction when the error returns.
        let took_nothing = affected_cell_frames.is_empty()
            && swept.markers.is_empty()
            && swept.frames.is_empty()
            && non_cell_blocks_to_remove.is_empty()
            && total_chars_removed == 0;
        if took_nothing {
            return Err(NothingToDelete { new_position }.into());
        }
        return Ok((
            DeleteTextResultDto {
                new_position,
                deleted_text: String::new(),
            },
            snapshot,
        ));
    }
    // ── End cell selection safety ──────────────────────────────────

    if end_block_idx < start_block_idx
        || (end_block_idx == start_block_idx && end_offset < start_offset)
    {
        // The blocks above are in rope order, so this means the two ends of
        // the range resolved against different position spaces. Refuse the
        // edit rather than merge blocks it never covered.
        return Err(anyhow!(
            "Delete range {start}..{end} resolves its end before its start"
        ));
    }
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
        delete_block_footnote_refs(uow.as_ref(), start_block.id, byte_so, byte_eo);

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

        // And the merged footnote references, by the same rule. They were
        // left as they were: the start block kept references past the cut,
        // pointing into text that was gone, and the end block's references
        // were dropped with it while their sentinels moved into the merged
        // text as bare characters.
        let mut merged_notes: Vec<FootnoteRefAnchor> =
            read_block_footnote_refs(uow.as_ref(), start_block.id)
                .into_iter()
                .filter(|note| note.byte_offset < byte_so)
                .collect();
        merged_notes.extend(
            read_block_footnote_refs(uow.as_ref(), end_block.id)
                .into_iter()
                .filter(|note| note.byte_offset >= byte_eo)
                .map(|note| FootnoteRefAnchor {
                    byte_offset: note.byte_offset - byte_eo + byte_so,
                    ..note
                }),
        );

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
        write_block_footnote_refs(uow.as_ref(), start_block.id, merged_notes);

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
            // rope-adjacent slice [start_idx+1..=end_idx]. A block the
            // frames place between the two ends but the rope holds
            // elsewhere would stay in `block_offsets` with a stale
            // entry; the layouts the editing use cases build keep the
            // rope in flow order, so this is a guard, not a path taken.
            // Drop them here so the rope index doesn't carry dangling
            // block ids past delete_text.
            common::database::rope_helpers::rope_remove_block(&uow.store(), *block_id);
        }
        // In one call: each single removal rewrites and re-validates the
        // owning frame's whole block list, so deleting N paragraphs one at a
        // time cost N(N+1)/2 of those.
        uow.remove_block_multi(&blocks_to_remove)?;

        // Group removed blocks by their owning frame, then update each
        // affected frame's child_order. Without this, sub-frame (e.g.
        // blockquote) child_order can be left with dangling entries when
        // the cross-block merge crosses a frame boundary — the cell-only
        // `block_to_cell_frame` map silently falls back to the root.
        let now = chrono::Utc::now();
        let mut blocks_by_frame: HashMap<EntityId, HashSet<EntityId>> = HashMap::new();
        // Walked once, on the first block that needs it: a search from the
        // root per block cost a walk of the document per deleted paragraph.
        let mut owners: Option<(HashMap<EntityId, EntityId>, Option<anyhow::Error>)> = None;
        for &bid in &blocks_to_remove {
            let owning = if let Some(&cf) = block_to_cell_frame.get(&bid) {
                cf
            } else {
                let (owner_of, stopped) =
                    owners.get_or_insert_with(|| block_owner_frames(uow.as_ref(), &roots));
                match owner_of.get(&bid) {
                    Some(&owner) => owner,
                    None => match stopped.take() {
                        Some(error) => return Err(error),
                        None => frame_id,
                    },
                }
            };
            blocks_by_frame.entry(owning).or_default().insert(bid);
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
        prune_empty_definition_frames(uow, &definition_frames)?;

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
    delete_block_footnote_refs(uow.as_ref(), block.id, byte_start, byte_end);

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
