use crate::UnwrapBlockFromFrameDto;
use crate::UnwrapBlockFromFrameResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
#[allow(unused_imports)]
use common::entities::{Block, Document, Frame, Root};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait UnwrapBlockFromFrameUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn UnwrapBlockFromFrameUnitOfWorkTrait>;
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
#[macros::uow_action(entity = "Frame", action = "Remove")]
#[macros::uow_action(entity = "Block", action = "Get")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "Update")]
pub trait UnwrapBlockFromFrameUnitOfWorkTrait: CommandUnitOfWork {}

pub struct UnwrapBlockFromFrameUseCase {
    uow_factory: Box<dyn UnwrapBlockFromFrameUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<UnwrapBlockFromFrameDto>,
}

/// Walk the frame tree under `root_id` to find which frame's
/// `child_order` contains the positive entry `target`.
fn find_block_owner_frame(
    uow: &dyn UnwrapBlockFromFrameUnitOfWorkTrait,
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

fn execute_unwrap_block_from_frame(
    uow: &mut Box<dyn UnwrapBlockFromFrameUnitOfWorkTrait>,
    dto: &UnwrapBlockFromFrameDto,
) -> Result<(UnwrapBlockFromFrameResultDto, EntityTreeSnapshot)> {
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

    let block_id = dto.block_id as EntityId;
    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let root_frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;

    let owner_id = find_block_owner_frame(uow.as_ref(), root_frame_id, block_id)?
        .ok_or_else(|| anyhow!("block_id not found in any frame's child_order"))?;
    let owner = uow
        .get_frame(&owner_id)?
        .ok_or_else(|| anyhow!("Owning frame not found"))?;
    let parent_id = owner
        .parent_frame
        .ok_or_else(|| anyhow!("Cannot unwrap a block from the root frame"))?;
    let parent = uow
        .get_frame(&parent_id)?
        .ok_or_else(|| anyhow!("Parent frame not found"))?;
    if owner.table.is_some() {
        return Err(anyhow!("Cannot unwrap a block from a table-anchor frame"));
    }

    let block_idx = owner
        .child_order
        .iter()
        .position(|&e| e > 0 && e as EntityId == block_id)
        .ok_or_else(|| anyhow!("block_id not in owning frame's child_order"))?;

    let entries_before: Vec<i64> = owner.child_order[..block_idx].to_vec();
    let entries_after: Vec<i64> = owner.child_order[block_idx + 1..].to_vec();

    // Decide whether the source frame survives (still has content) or
    // collapses (no remaining blocks AND no remaining sub-frames). A
    // surviving source keeps `entries_before`. An after-sibling frame is
    // only created when `entries_after` is non-empty.
    let source_survives = !entries_before.is_empty();
    let after_sibling_needed = !entries_after.is_empty();

    // Optionally create a sibling frame to host `entries_after`. Use the
    // same FrameFormat as the source so visual depth is preserved.
    let after_sibling_id: Option<EntityId> = if after_sibling_needed {
        let template = Frame {
            id: 0,
            created_at: now,
            updated_at: now,
            parent_frame: Some(parent_id),
            blocks: vec![],
            child_order: vec![],
            fmt_height: owner.fmt_height,
            fmt_width: owner.fmt_width,
            fmt_top_margin: owner.fmt_top_margin,
            fmt_bottom_margin: owner.fmt_bottom_margin,
            fmt_left_margin: owner.fmt_left_margin,
            fmt_right_margin: owner.fmt_right_margin,
            fmt_padding: owner.fmt_padding,
            fmt_border: owner.fmt_border,
            fmt_position: owner.fmt_position.clone(),
            fmt_is_blockquote: owner.fmt_is_blockquote,
            table: None,
            byte_range: (0, 0),
        };
        let created = uow.create_frame(&template, doc_id, -1)?;
        let mut updated = created.clone();
        updated.child_order = entries_after.clone();
        updated.blocks = entries_after
            .iter()
            .filter_map(|&e| if e > 0 { Some(e as EntityId) } else { None })
            .collect();
        updated.updated_at = now;
        uow.update_frame_with_relationships(&updated)?;

        // Re-parent any sub-frames that moved into the sibling.
        for &entry in &entries_after {
            if entry < 0 {
                let sub_id = (-entry) as EntityId;
                if let Some(sub) = uow.get_frame(&sub_id)? {
                    let mut updated_sub = sub.clone();
                    updated_sub.parent_frame = Some(created.id);
                    updated_sub.updated_at = now;
                    uow.update_frame_with_relationships(&updated_sub)?;
                }
            }
        }
        Some(created.id)
    } else {
        None
    };

    // Update or remove the source frame.
    if source_survives {
        let mut updated_owner = owner.clone();
        updated_owner.child_order = entries_before.clone();
        updated_owner.blocks = entries_before
            .iter()
            .filter_map(|&e| if e > 0 { Some(e as EntityId) } else { None })
            .collect();
        updated_owner.updated_at = now;
        uow.update_frame_with_relationships(&updated_owner)?;
    } else {
        // Clear blocks/child_order before remove. `remove_frame` cascades
        // into `Frame.blocks` and deletes referenced blocks — which would
        // destroy the very block we just lifted to the parent.
        let mut empty_owner = owner.clone();
        empty_owner.blocks = vec![];
        empty_owner.child_order = vec![];
        empty_owner.updated_at = now;
        uow.update_frame_with_relationships(&empty_owner)?;
        uow.remove_frame(&owner_id)?;
    }

    // Splice the parent's child_order: replace -(owner_id) with the new
    // sequence [ -(owner) (if survived), block_id, -(after_sibling) (if
    // any) ].
    let owner_neg_entry = -(owner_id as i64);
    let parent_neg_idx = parent
        .child_order
        .iter()
        .position(|&e| e == owner_neg_entry)
        .ok_or_else(|| anyhow!("Source frame entry not present in parent's child_order"))?;
    let mut splice_seq: Vec<i64> = Vec::with_capacity(3);
    if source_survives {
        splice_seq.push(owner_neg_entry);
    }
    splice_seq.push(block_id as i64);
    if let Some(after_id) = after_sibling_id {
        splice_seq.push(-(after_id as i64));
    }
    let mut updated_parent = parent.clone();
    updated_parent
        .child_order
        .splice(parent_neg_idx..=parent_neg_idx, splice_seq);
    if !updated_parent.blocks.contains(&block_id) {
        updated_parent.blocks.push(block_id);
    }
    updated_parent.updated_at = now;
    uow.update_frame_with_relationships(&updated_parent)?;

    let new_position = uow
        .get_block(&block_id)?
        .map(|b| b.document_position)
        .unwrap_or(0);

    Ok((UnwrapBlockFromFrameResultDto { new_position }, snapshot))
}

impl UnwrapBlockFromFrameUseCase {
    pub fn new(uow_factory: Box<dyn UnwrapBlockFromFrameUnitOfWorkFactoryTrait>) -> Self {
        UnwrapBlockFromFrameUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(
        &mut self,
        dto: &UnwrapBlockFromFrameDto,
    ) -> Result<UnwrapBlockFromFrameResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_unwrap_block_from_frame(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for UnwrapBlockFromFrameUseCase {
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
        let (_, snapshot) = execute_unwrap_block_from_frame(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
