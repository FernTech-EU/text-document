use super::editing_helpers::{find_block_at_position, find_segment_at_offset};
use crate::InsertImageDto;
use crate::InsertImageResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{block_content_via_store, rope_insert_in_block};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::entities::{Block, Document, Frame, Root};
use common::format_runs::{ImageAnchor, InlineContent, synth_element_id};
use common::format_runs_query::inline_segments_for_block;
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait InsertImageUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn InsertImageUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "Update")]
#[macros::uow_action(entity = "Document", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Snapshot")]
#[macros::uow_action(entity = "Document", action = "Restore")]
#[macros::uow_action(entity = "Frame", action = "Get")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "Get")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "Update")]
#[macros::uow_action(entity = "Block", action = "UpdateMulti")]
#[macros::uow_action(entity = "Block", action = "GetRelationship")]
pub trait InsertImageUnitOfWorkTrait: CommandUnitOfWork {}

pub struct InsertImageUseCase {
    uow_factory: Box<dyn InsertImageUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<InsertImageDto>,
}

fn execute_insert_image(
    uow: &mut Box<dyn InsertImageUnitOfWorkTrait>,
    dto: &InsertImageDto,
) -> Result<(InsertImageResultDto, EntityTreeSnapshot)> {
    if dto.position != dto.anchor {
        return Err(anyhow!(
            "Selection replacement is not supported for image insertion"
        ));
    }

    if dto.width <= 0 || dto.height <= 0 {
        return Err(anyhow!(
            "Image dimensions must be positive (got {}x{})",
            dto.width,
            dto.height
        ));
    }

    let position = dto.position;

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

    // Snapshot for undo before mutation (covers blocks, block_images, format_runs, document).
    let snapshot = uow.snapshot_document(&[doc_id])?;

    // Get frames
    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;

    // Get block IDs from frame
    let block_ids = uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;

    // Get all blocks
    let blocks_opt = uow.get_block_multi(&block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
    blocks.sort_by_key(|b| b.document_position);

    // Find block at position
    let (block, block_idx, offset) = find_block_at_position(&blocks, position, &uow.store())?;

    // Synthesize the inline-segment view of the target block from format_runs +
    // block_images. This is read-only — we use it to locate the byte offset
    // inside the block's content where the new image should be anchored.
    let block_text = block_content_via_store(&block, &uow.store());
    let segments = inline_segments_for_block(&uow.store(), block.id, &block_text);

    // byte_offset = position inside `block.plain_text` where the new image is
    // anchored. Empty blocks anchor at 0. Otherwise we walk the synthesized
    // segments: each Text contributes its UTF-8 byte length; Image / Empty
    // contribute zero.
    let byte_offset: u32 = if segments.is_empty() {
        0
    } else {
        let (segment, seg_idx, seg_offset) = find_segment_at_offset(&segments, offset)?;
        let mut bo: u32 = 0;
        for prev in &segments[..seg_idx] {
            if let InlineContent::Text(s) = &prev.content {
                bo += s.len() as u32;
            }
        }
        match &segment.content {
            InlineContent::Text(s) => {
                let split_byte = s
                    .char_indices()
                    .nth(seg_offset as usize)
                    .map(|(b, _)| b)
                    .unwrap_or(s.len());
                bo + split_byte as u32
            }
            InlineContent::Image { .. } | InlineContent::Empty => bo,
        }
    };

    let now = chrono::Utc::now();

    // Insert ImageAnchor directly into block_images, maintaining sort order
    // (ascending by byte_offset; equal byte_offsets keep insertion order, so
    // the new image goes AFTER any existing anchors at the same byte position).
    {
        let store = uow.store();
        let mut images_map = store.block_images.write();
        let images = images_map.entry(block.id).or_default();
        let insert_idx = images
            .iter()
            .position(|a| a.byte_offset > byte_offset)
            .unwrap_or(images.len());
        images.insert(
            insert_idx,
            ImageAnchor {
                byte_offset,
                name: dto.image_name.clone(),
                width: dto.width,
                height: dto.height,
                quality: 100,
                format: Default::default(),
            },
        );
    }

    // Mirror to the global rope: insert U+FFFC OBJECT REPLACEMENT
    // CHARACTER at the same byte offset per plan §1.6. The sentinel is
    // 3 UTF-8 bytes but contributes 1 logical position.
    rope_insert_in_block(&uow.store(), block.id, byte_offset, "\u{FFFC}");

    let mut updated_block = block.clone();
    updated_block.updated_at = now;
    uow.update_block(&updated_block)?;

    // Shift subsequent blocks' document_position by +1. NOT gated on
    // `rope_positions_match_flow` (unlike insert_text_uc): this use
    // case locates the edit via `find_block_at_position`, which reads
    // the stored `document_position`, so the field must stay accurate
    // here. Gating would require an O(N) catch-up that cancels the
    // saving — net zero. Migrating the lookup to the rope-based
    // `find_block_at_char_position` (Cause B) is the prerequisite for
    // gating this loop.
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
    updated_doc.character_count += 1;
    updated_doc.updated_at = now;
    uow.update_document(&updated_doc)?;

    Ok((
        InsertImageResultDto {
            new_position: position + 1,
            element_id: synth_element_id(block.id, byte_offset) as i64,
        },
        snapshot,
    ))
}

impl InsertImageUseCase {
    pub fn new(uow_factory: Box<dyn InsertImageUnitOfWorkFactoryTrait>) -> Self {
        InsertImageUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &InsertImageDto) -> Result<InsertImageResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_insert_image(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for InsertImageUseCase {
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
        let (_, snapshot) = execute_insert_image(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
