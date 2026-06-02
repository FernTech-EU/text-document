use super::editing_helpers::{collect_block_ids_recursive, find_block_at_position};
use crate::InsertBlockDto;
use crate::InsertBlockResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{block_content_via_store, rope_split_block};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::direct_access::table::TableRelationshipField;
use common::entities::{Block, Document, Frame, Root, TableCell};
use common::format_runs::{
    debug_assert_well_formed, logical_offset_to_byte, split_images_at, split_runs_at,
};

use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait InsertBlockUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn InsertBlockUnitOfWorkTrait>;
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
#[macros::uow_action(entity = "Block", action = "GetRelationship")]
#[macros::uow_action(entity = "Table", action = "GetRelationship")]
#[macros::uow_action(entity = "TableCell", action = "GetMulti")]
pub trait InsertBlockUnitOfWorkTrait: CommandUnitOfWork {}

pub struct InsertBlockUseCase {
    uow_factory: Box<dyn InsertBlockUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<InsertBlockDto>,
}

fn execute_insert_block(
    uow: &mut Box<dyn InsertBlockUnitOfWorkTrait>,
    dto: &InsertBlockDto,
) -> Result<(InsertBlockResultDto, EntityTreeSnapshot)> {
    let position = dto.position;

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
    let blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();

    let (current_block, block_idx, offset) =
        find_block_at_position(&blocks, position, &uow.store())?;

    // Split byte offset inside current_block.plain_text accounting for images.
    let store = uow.store();
    let current_runs = store
        .format_runs
        .read()
        .get(&current_block.id)
        .cloned()
        .unwrap_or_default();
    let current_images = store
        .block_images
        .read()
        .get(&current_block.id)
        .cloned()
        .unwrap_or_default();

    let current_block_text = block_content_via_store(&current_block, &store);
    let byte_split = logical_offset_to_byte(&current_block_text, &current_images, offset);
    let text_before = current_block_text[..byte_split as usize].to_string();
    let text_after = current_block_text[byte_split as usize..].to_string();
    let text_after_byte_len = text_after.len();
    let text_before_chars = text_before.chars().count() as i64;

    // Split format_runs and block_images at the byte boundary.
    let (left_runs, right_runs) = split_runs_at(&current_runs, byte_split);
    let (left_images, right_images) = split_images_at(&current_images, byte_split);

    let left_image_count = left_images.len() as i64;

    let now = chrono::Utc::now();

    // Update current block (now the "before" block).
    let mut updated_current = current_block.clone();
    updated_current.updated_at = now;
    uow.update_block(&updated_current)?;
    debug_assert_well_formed(&left_runs, text_before.len());
    store
        .format_runs
        .write()
        .insert(current_block.id, left_runs);
    store
        .block_images
        .write()
        .insert(current_block.id, left_images);

    // Create the "after" block. Use `text_before_chars` directly because the
    // rope split mirror (`rope_split_block` below) hasn't run yet — at this
    // point in the function `block_char_length(updated_current)` would still
    // return the unsplit length (chars in both halves).
    let new_block_position =
        current_block.document_position + text_before_chars + left_image_count + 1;
    let new_block = Block {
        id: 0,
        created_at: now,
        updated_at: now,
        list: current_block.list,
        document_position: new_block_position,
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
    };

    fn find_owner_frame(
        uow: &dyn InsertBlockUnitOfWorkTrait,
        fid: &EntityId,
        target_block_id: EntityId,
    ) -> Result<Option<EntityId>> {
        let f = uow
            .get_frame(fid)?
            .ok_or_else(|| anyhow!("Frame not found"))?;
        for &entry in &f.child_order {
            if entry > 0 && entry as EntityId == target_block_id {
                return Ok(Some(*fid));
            } else if entry < 0 {
                let sub = (-entry) as EntityId;
                if let Some(owner) = find_owner_frame(uow, &sub, target_block_id)? {
                    return Ok(Some(owner));
                }
            }
        }
        let block_ids = uow.get_frame_relationship(fid, &FrameRelationshipField::Blocks)?;
        if block_ids.contains(&target_block_id) {
            return Ok(Some(*fid));
        }
        Ok(None)
    }
    let owner_frame_id = find_owner_frame(&**uow, &frame_id, current_block.id)?.unwrap_or(frame_id);

    let created_block = uow.create_block(&new_block, owner_frame_id, -1)?;

    // Place the split format_runs / block_images on the new block.
    debug_assert_well_formed(&right_runs, text_after_byte_len);
    store
        .format_runs
        .write()
        .insert(created_block.id, right_runs);
    store
        .block_images
        .write()
        .insert(created_block.id, right_images);

    // Mirror the split into the rope: insert `\n` boundary at the
    // split point, register the new block's start, and shift
    // subsequent block offsets by +1. No-op under default backend.
    // No-op for blocks not in the rope index (e.g. table cells —
    // step 5.5e).
    rope_split_block(&store, current_block.id, byte_split, created_block.id);

    let frame = uow
        .get_frame(&owner_frame_id)?
        .ok_or_else(|| anyhow!("Owner frame not found"))?;
    let mut updated_frame = frame.clone();
    let co_idx = updated_frame
        .child_order
        .iter()
        .position(|&e| e > 0 && e as EntityId == current_block.id)
        .map(|i| i + 1)
        .unwrap_or(updated_frame.child_order.len());
    updated_frame
        .child_order
        .insert(co_idx, created_block.id as i64);
    updated_frame.updated_at = now;
    updated_frame.blocks =
        uow.get_frame_relationship(&owner_frame_id, &FrameRelationshipField::Blocks)?;
    uow.update_frame(&updated_frame)?;

    let mut blocks_to_update: Vec<Block> = Vec::new();
    for b in &blocks[(block_idx + 1)..] {
        let mut ub = b.clone();
        ub.document_position += 1;
        ub.updated_at = now;
        blocks_to_update.push(ub);
    }
    if !blocks_to_update.is_empty() {
        uow.update_block_multi(&blocks_to_update)?;
    }

    let mut updated_doc = document.clone();
    updated_doc.block_count += 1;
    updated_doc.updated_at = now;
    uow.update_document(&updated_doc)?;

    Ok((
        InsertBlockResultDto {
            new_position: new_block_position,
            new_block_id: created_block.id as i64,
        },
        snapshot,
    ))
}

impl InsertBlockUseCase {
    pub fn new(uow_factory: Box<dyn InsertBlockUnitOfWorkFactoryTrait>) -> Self {
        InsertBlockUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &InsertBlockDto) -> Result<InsertBlockResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_insert_block(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for InsertBlockUseCase {
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
        let (_, snapshot) = execute_insert_block(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
