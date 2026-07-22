use crate::SetBlockFormatDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::block_char_length;
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::entities::{Block, Document, Frame, Root};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait SetBlockFormatUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn SetBlockFormatUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Snapshot")]
#[macros::uow_action(entity = "Document", action = "Restore")]
#[macros::uow_action(entity = "Frame", action = "Get")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "Get")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "Update")]
#[macros::uow_action(entity = "Block", action = "UpdateMulti")]
pub trait SetBlockFormatUnitOfWorkTrait: CommandUnitOfWork {}

fn alignment_to_entity(a: &crate::dtos::Alignment) -> common::entities::Alignment {
    match a {
        crate::dtos::Alignment::Left => common::entities::Alignment::Left,
        crate::dtos::Alignment::Right => common::entities::Alignment::Right,
        crate::dtos::Alignment::Center => common::entities::Alignment::Center,
        crate::dtos::Alignment::Justify => common::entities::Alignment::Justify,
    }
}

fn marker_to_entity(m: &crate::dtos::MarkerType) -> common::entities::MarkerType {
    match m {
        crate::dtos::MarkerType::NoMarker => common::entities::MarkerType::NoMarker,
        crate::dtos::MarkerType::Unchecked => common::entities::MarkerType::Unchecked,
        crate::dtos::MarkerType::Checked => common::entities::MarkerType::Checked,
    }
}

fn direction_to_entity(d: &crate::dtos::TextDirection) -> common::entities::TextDirection {
    match d {
        crate::dtos::TextDirection::LeftToRight => common::entities::TextDirection::LeftToRight,
        crate::dtos::TextDirection::RightToLeft => common::entities::TextDirection::RightToLeft,
    }
}

fn execute_set_block_format(
    uow: &mut Box<dyn SetBlockFormatUnitOfWorkTrait>,
    dto: &SetBlockFormatDto,
) -> Result<EntityTreeSnapshot> {
    // Get Root -> Document
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

    // Snapshot for undo before mutation
    let snapshot = uow.snapshot_document(&[doc_id])?;

    // Get frames
    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;

    // Get block IDs from all frames
    let mut all_block_ids = Vec::new();
    for fid in &frame_ids {
        let block_ids = uow.get_frame_relationship(fid, &FrameRelationshipField::Blocks)?;
        all_block_ids.extend(block_ids);
    }

    // Get all blocks
    let blocks_opt = uow.get_block_multi(&all_block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
    blocks.sort_by_key(|b| b.document_position);

    // Determine the range
    let range_start = std::cmp::min(dto.position, dto.anchor);
    let range_end = std::cmp::max(dto.position, dto.anchor);

    // Find blocks that overlap the range
    let mut blocks_to_update: Vec<Block> = Vec::new();
    let store = uow.store();
    for block in &blocks {
        let block_start = block.document_position;
        let block_end = block_start + block_char_length(block, &store);

        // Block overlaps the range if its text interval [block_start, block_end)
        // intersects [range_start, range_end].  Empty blocks (text_length == 0)
        // are included when their position falls inside the range.
        let overlaps = if block_char_length(block, &store) > 0 {
            block_start <= range_end && block_end > range_start
        } else {
            block_start >= range_start && block_start <= range_end
        };
        if overlaps {
            let mut updated = block.clone();
            if let Some(ref a) = dto.alignment {
                updated.fmt_alignment = Some(alignment_to_entity(a));
            }
            if let Some(v) = dto.heading_level {
                updated.fmt_heading_level = Some(v.clamp(0, 6));
            }
            if let Some(v) = dto.indent {
                updated.fmt_indent = Some(v);
            }
            if let Some(ref m) = dto.marker {
                updated.fmt_marker = Some(marker_to_entity(m));
            }
            if let Some(v) = dto.line_height {
                updated.fmt_line_height = Some(v);
            }
            if let Some(v) = dto.non_breakable_lines {
                updated.fmt_non_breakable_lines = Some(v);
            }
            if dto.clear_direction {
                updated.fmt_direction = None;
            } else if let Some(ref d) = dto.direction {
                updated.fmt_direction = Some(direction_to_entity(d));
            }
            if let Some(ref c) = dto.background_color {
                updated.fmt_background_color = Some(c.clone());
            }
            if let Some(v) = dto.is_code_block {
                updated.fmt_is_code_block = Some(v);
            }
            if let Some(ref l) = dto.code_language {
                updated.fmt_code_language = Some(l.clone());
            }
            if let Some(v) = dto.hyphenate {
                updated.fmt_hyphenate = Some(v);
            }
            if let Some(ref l) = dto.language {
                updated.fmt_language = Some(l.clone());
            }
            if let Some(v) = dto.top_margin {
                updated.fmt_top_margin = Some(v);
            }
            if let Some(v) = dto.bottom_margin {
                updated.fmt_bottom_margin = Some(v);
            }
            if let Some(v) = dto.left_margin {
                updated.fmt_left_margin = Some(v);
            }
            if let Some(v) = dto.right_margin {
                updated.fmt_right_margin = Some(v);
            }
            if let Some(v) = dto.text_indent {
                updated.fmt_text_indent = Some(v);
            }
            updated.updated_at = chrono::Utc::now();
            blocks_to_update.push(updated);
        }
    }

    if !blocks_to_update.is_empty() {
        uow.update_block_multi(&blocks_to_update)?;
    }

    Ok(snapshot)
}

pub struct SetBlockFormatUseCase {
    uow_factory: Box<dyn SetBlockFormatUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<SetBlockFormatDto>,
}

impl SetBlockFormatUseCase {
    pub fn new(uow_factory: Box<dyn SetBlockFormatUnitOfWorkFactoryTrait>) -> Self {
        SetBlockFormatUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &SetBlockFormatDto) -> Result<()> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let snapshot = execute_set_block_format(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(())
    }
}

impl UndoRedoCommand for SetBlockFormatUseCase {
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
        let snapshot = execute_set_block_format(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
