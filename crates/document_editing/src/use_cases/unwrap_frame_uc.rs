use crate::UnwrapFrameDto;
use crate::UnwrapFrameResultDto;
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

pub trait UnwrapFrameUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn UnwrapFrameUnitOfWorkTrait>;
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
#[macros::uow_action(entity = "Frame", action = "UpdateWithRelationships")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Frame", action = "Remove")]
#[macros::uow_action(entity = "Block", action = "Get")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "Update")]
pub trait UnwrapFrameUnitOfWorkTrait: CommandUnitOfWork {}

pub struct UnwrapFrameUseCase {
    uow_factory: Box<dyn UnwrapFrameUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<UnwrapFrameDto>,
}

fn execute_unwrap_frame(
    uow: &mut Box<dyn UnwrapFrameUnitOfWorkTrait>,
    dto: &UnwrapFrameDto,
) -> Result<(UnwrapFrameResultDto, EntityTreeSnapshot)> {
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

    let frame_id = dto.frame_id as EntityId;
    let frame = uow
        .get_frame(&frame_id)?
        .ok_or_else(|| anyhow!("Frame {} not found", frame_id))?;
    let parent_id = frame
        .parent_frame
        .ok_or_else(|| anyhow!("Cannot unwrap a top-level frame (no parent)"))?;
    if frame.table.is_some() {
        return Err(anyhow!("Cannot unwrap a table-anchor frame"));
    }

    let parent = uow
        .get_frame(&parent_id)?
        .ok_or_else(|| anyhow!("Parent frame {} not found", parent_id))?;

    let neg_entry = -(frame_id as i64);
    let neg_idx = parent
        .child_order
        .iter()
        .position(|&e| e == neg_entry)
        .ok_or_else(|| anyhow!("Sub-frame entry not present in parent's child_order"))?;

    let moved_slice: Vec<i64> = frame.child_order.clone();
    let moved_block_ids: Vec<EntityId> = moved_slice
        .iter()
        .filter_map(|&e| if e > 0 { Some(e as EntityId) } else { None })
        .collect();

    // Re-parent any sub-frame entries to the new parent.
    for &entry in &moved_slice {
        if entry < 0 {
            let sub_id = (-entry) as EntityId;
            if let Some(sub) = uow.get_frame(&sub_id)? {
                let mut updated_sub = sub.clone();
                updated_sub.parent_frame = Some(parent_id);
                updated_sub.updated_at = now;
                uow.update_frame_with_relationships(&updated_sub)?;
            }
        }
    }

    // Update parent: splice the sub-frame's children into parent's
    // child_order at the position the sub-frame occupied.
    let mut updated_parent = parent.clone();
    updated_parent
        .child_order
        .splice(neg_idx..=neg_idx, moved_slice.into_iter());
    let mut new_parent_blocks = updated_parent.blocks.clone();
    for b in &moved_block_ids {
        if !new_parent_blocks.contains(b) {
            new_parent_blocks.push(*b);
        }
    }
    updated_parent.blocks = new_parent_blocks;
    updated_parent.updated_at = now;
    uow.update_frame_with_relationships(&updated_parent)?;

    // Clear the frame's blocks/child_order BEFORE remove. The repository's
    // `remove_frame` cascades into `Frame.blocks` and deletes every block
    // referenced there — which would destroy the very blocks we just
    // lifted to the parent. Clearing first turns the cascade into a no-op.
    let mut empty_owner = frame.clone();
    empty_owner.blocks = vec![];
    empty_owner.child_order = vec![];
    empty_owner.updated_at = now;
    uow.update_frame_with_relationships(&empty_owner)?;
    uow.remove_frame(&frame_id)?;

    let new_position = if let Some(first_block_id) = moved_block_ids.first() {
        uow.get_block(first_block_id)?
            .map(|b| b.document_position)
            .unwrap_or(0)
    } else {
        0
    };

    Ok((UnwrapFrameResultDto { new_position }, snapshot))
}

impl UnwrapFrameUseCase {
    pub fn new(uow_factory: Box<dyn UnwrapFrameUnitOfWorkFactoryTrait>) -> Self {
        UnwrapFrameUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &UnwrapFrameDto) -> Result<UnwrapFrameResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_unwrap_frame(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for UnwrapFrameUseCase {
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
        let (_, snapshot) = execute_unwrap_frame(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
