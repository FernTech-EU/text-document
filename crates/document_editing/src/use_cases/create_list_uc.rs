use crate::CreateListDto;
use crate::CreateListResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::block_char_length;
use common::direct_access::block::block_repository::BlockRelationshipField;
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::entities::{Block, Document, Frame, List, Root};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait CreateListUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn CreateListUnitOfWorkTrait>;
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
#[macros::uow_action(entity = "Block", action = "SetRelationship")]
#[macros::uow_action(entity = "List", action = "Create")]
pub trait CreateListUnitOfWorkTrait: CommandUnitOfWork {}

pub struct CreateListUseCase {
    uow_factory: Box<dyn CreateListUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<CreateListDto>,
}

/// Convert from crate's ListStyle to common::entities::ListStyle
fn convert_list_style(style: &crate::dtos::ListStyle) -> common::entities::ListStyle {
    match style {
        crate::dtos::ListStyle::Disc => common::entities::ListStyle::Disc,
        crate::dtos::ListStyle::Circle => common::entities::ListStyle::Circle,
        crate::dtos::ListStyle::Square => common::entities::ListStyle::Square,
        crate::dtos::ListStyle::Decimal => common::entities::ListStyle::Decimal,
        crate::dtos::ListStyle::LowerAlpha => common::entities::ListStyle::LowerAlpha,
        crate::dtos::ListStyle::UpperAlpha => common::entities::ListStyle::UpperAlpha,
        crate::dtos::ListStyle::LowerRoman => common::entities::ListStyle::LowerRoman,
        crate::dtos::ListStyle::UpperRoman => common::entities::ListStyle::UpperRoman,
    }
}

fn execute_create_list(
    uow: &mut Box<dyn CreateListUnitOfWorkTrait>,
    dto: &CreateListDto,
) -> Result<(CreateListResultDto, EntityTreeSnapshot)> {
    let sel_start = std::cmp::min(dto.position, dto.anchor);
    let sel_end = std::cmp::max(dto.position, dto.anchor);

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
    let frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;

    // Get block IDs from frame
    let block_ids = uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;

    // Get all blocks
    let blocks_opt = uow.get_block_multi(&block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
    // The stored field lags the rope by whatever was typed since the last deletion, and the
    // caller's positions are rope positions: read the rope's.
    common::database::rope_helpers::refresh_block_positions(&mut blocks, &uow.store());
    blocks.sort_by_key(|b| b.document_position);

    // Create the List entity
    let now = chrono::Utc::now();
    let list = List {
        id: 0,
        created_at: now,
        updated_at: now,
        style: convert_list_style(&dto.style),
        indent: 0,
        prefix: String::new(),
        suffix: String::new(),
    };

    let created_list = uow.create_list(&list, doc_id, -1)?;

    // Find all blocks in range [sel_start, sel_end] and assign the list relationship
    let store = uow.store();
    for block in &blocks {
        let block_start = block.document_position;
        let block_end = block_start + block_char_length(block, &store);
        // A block is in range if it overlaps with [sel_start, sel_end]
        if block_end >= sel_start && block_start <= sel_end {
            uow.set_block_relationship(
                &block.id,
                &BlockRelationshipField::List,
                &[created_list.id],
            )?;
        }
    }

    Ok((
        CreateListResultDto {
            list_id: created_list.id as i64,
        },
        snapshot,
    ))
}

impl CreateListUseCase {
    pub fn new(uow_factory: Box<dyn CreateListUnitOfWorkFactoryTrait>) -> Self {
        CreateListUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &CreateListDto) -> Result<CreateListResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_create_list(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for CreateListUseCase {
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
        let (_, snapshot) = execute_create_list(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
