use crate::WrapBlocksInFrameDto;
use crate::WrapBlocksInFrameResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
#[allow(unused_imports)]
use common::entities::{Block, Document, Frame, Root};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait WrapBlocksInFrameUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn WrapBlocksInFrameUnitOfWorkTrait>;
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
#[macros::uow_action(entity = "Block", action = "Get")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "Update")]
pub trait WrapBlocksInFrameUnitOfWorkTrait: CommandUnitOfWork {}

pub struct WrapBlocksInFrameUseCase {
    uow_factory: Box<dyn WrapBlocksInFrameUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<WrapBlocksInFrameDto>,
}

/// Recursive walk of the frame tree rooted at `root_id` to find which
/// frame's `child_order` contains the positive entry `target`.
fn find_block_owner_frame(
    uow: &dyn WrapBlocksInFrameUnitOfWorkTrait,
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

fn execute_wrap_blocks_in_frame(
    uow: &mut Box<dyn WrapBlocksInFrameUnitOfWorkTrait>,
    dto: &WrapBlocksInFrameDto,
) -> Result<(WrapBlocksInFrameResultDto, EntityTreeSnapshot)> {
    let root = uow
        .get_root(&ROOT_ENTITY_ID)?
        .ok_or_else(|| anyhow!("Root entity not found"))?;
    let doc_ids = uow.get_root_relationship(&root.id, &RootRelationshipField::Document)?;
    let doc_id = *doc_ids
        .first()
        .ok_or_else(|| anyhow!("Root has no document"))?;

    let _document = uow
        .get_document(&doc_id)?
        .ok_or_else(|| anyhow!("Document not found"))?;

    let snapshot = uow.snapshot_document(&[doc_id])?;
    let now = chrono::Utc::now();

    let start_block_id = dto.start_block_id as EntityId;
    let end_block_id = dto.end_block_id as EntityId;

    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let root_frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;

    let owner_id = find_block_owner_frame(uow.as_ref(), root_frame_id, start_block_id)?
        .ok_or_else(|| anyhow!("start_block_id not found in any frame"))?;
    let end_owner_id = find_block_owner_frame(uow.as_ref(), root_frame_id, end_block_id)?
        .ok_or_else(|| anyhow!("end_block_id not found in any frame"))?;
    if owner_id != end_owner_id {
        return Err(anyhow!(
            "wrap_blocks_in_frame: start_block_id and end_block_id must live in the same frame"
        ));
    }

    let owner = uow
        .get_frame(&owner_id)?
        .ok_or_else(|| anyhow!("Owning frame not found"))?;

    let start_idx = owner
        .child_order
        .iter()
        .position(|&e| e > 0 && e as EntityId == start_block_id)
        .ok_or_else(|| anyhow!("start_block_id not in owning frame's child_order"))?;
    let end_idx = owner
        .child_order
        .iter()
        .position(|&e| e > 0 && e as EntityId == end_block_id)
        .ok_or_else(|| anyhow!("end_block_id not in owning frame's child_order"))?;
    if end_idx < start_idx {
        return Err(anyhow!(
            "wrap_blocks_in_frame: end_block must not appear before start_block in document order"
        ));
    }

    let moved_slice: Vec<i64> = owner.child_order[start_idx..=end_idx].to_vec();
    let moved_block_ids: Vec<EntityId> = moved_slice
        .iter()
        .filter_map(|&e| if e > 0 { Some(e as EntityId) } else { None })
        .collect();

    let new_frame = Frame {
        id: 0,
        created_at: now,
        updated_at: now,
        parent_frame: Some(owner_id),
        blocks: vec![],
        child_order: vec![],
        fmt_height: None,
        fmt_width: None,
        fmt_top_margin: dto.top_margin,
        fmt_bottom_margin: dto.bottom_margin,
        fmt_left_margin: dto.left_margin,
        fmt_right_margin: dto.right_margin,
        fmt_padding: dto.padding,
        fmt_border: dto.border,
        fmt_position: dto.position.clone(),
        fmt_is_blockquote: dto.is_blockquote,
        table: None,
        byte_range: (0, 0),
    };

    let created_frame = uow.create_frame(&new_frame, doc_id, -1)?;

    // Populate the new frame: take the moved slice as its child_order,
    // and its positive entries (blocks) as its blocks list.
    let mut updated_new_frame = created_frame.clone();
    updated_new_frame.child_order = moved_slice.clone();
    updated_new_frame.blocks = moved_block_ids.clone();
    updated_new_frame.updated_at = now;
    uow.update_frame_with_relationships(&updated_new_frame)?;

    // Re-parent any sub-frames inside the moved slice — their
    // `parent_frame` was the source frame; now it must be the new frame.
    for &entry in &moved_slice {
        if entry < 0 {
            let sub_id = (-entry) as EntityId;
            if let Some(sub) = uow.get_frame(&sub_id)? {
                let mut updated_sub = sub.clone();
                updated_sub.parent_frame = Some(created_frame.id);
                updated_sub.updated_at = now;
                uow.update_frame_with_relationships(&updated_sub)?;
            }
        }
    }

    // Update the source frame: remove the moved slice; insert the new
    // sub-frame's negative entry at the original start position.
    let mut updated_owner = owner.clone();
    let new_negative_entry = -(created_frame.id as i64);
    updated_owner.child_order.splice(
        start_idx..=end_idx,
        std::iter::once(new_negative_entry),
    );
    // Refresh the owning frame's `blocks` from the relationship store
    // (mirrors insert_block_uc:236-238). Removing blocks from a frame
    // doesn't delete the Block entity, but the relationship vec on the
    // entity must stay consistent with child_order.
    updated_owner.blocks = updated_owner
        .blocks
        .iter()
        .copied()
        .filter(|id| !moved_block_ids.contains(id))
        .collect();
    updated_owner.updated_at = now;
    uow.update_frame_with_relationships(&updated_owner)?;

    // Cursor position: the first block of the wrapped range stays at the
    // same document_position. Frame.byte_range is recomputed by
    // Transaction::commit.
    let start_block = uow
        .get_block(&start_block_id)?
        .ok_or_else(|| anyhow!("start block disappeared mid-op"))?;
    let new_position = start_block.document_position;

    Ok((
        WrapBlocksInFrameResultDto {
            new_frame_id: created_frame.id as i64,
            new_position,
        },
        snapshot,
    ))
}

impl WrapBlocksInFrameUseCase {
    pub fn new(uow_factory: Box<dyn WrapBlocksInFrameUnitOfWorkFactoryTrait>) -> Self {
        WrapBlocksInFrameUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &WrapBlocksInFrameDto) -> Result<WrapBlocksInFrameResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_wrap_blocks_in_frame(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for WrapBlocksInFrameUseCase {
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
        let (_, snapshot) = execute_wrap_blocks_in_frame(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
