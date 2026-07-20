use crate::SetListFormatDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::entities::{Document, List, Root};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait SetListFormatUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn SetListFormatUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "Snapshot")]
#[macros::uow_action(entity = "Document", action = "Restore")]
#[macros::uow_action(entity = "List", action = "Get")]
#[macros::uow_action(entity = "List", action = "Update")]
pub trait SetListFormatUnitOfWorkTrait: CommandUnitOfWork {}

fn style_to_entity(s: &crate::dtos::ListStyle) -> common::entities::ListStyle {
    match s {
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

fn execute_set_list_format(
    uow: &mut Box<dyn SetListFormatUnitOfWorkTrait>,
    dto: &SetListFormatDto,
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

    // Look up list by ID
    let list_id = dto.list_id as EntityId;
    let list = uow
        .get_list(&list_id)?
        .ok_or_else(|| anyhow!("List not found with id {}", dto.list_id))?;

    // Set list format fields -- None means preserve existing value.
    let mut updated = list.clone();
    if let Some(ref s) = dto.style {
        updated.style = style_to_entity(s);
    }
    if let Some(v) = dto.indent {
        updated.indent = v;
    }
    if let Some(ref v) = dto.prefix {
        updated.prefix = v.clone();
    }
    if let Some(ref v) = dto.suffix {
        updated.suffix = v.clone();
    }
    updated.updated_at = chrono::Utc::now();
    uow.update_list(&updated)?;

    Ok(snapshot)
}

pub struct SetListFormatUseCase {
    uow_factory: Box<dyn SetListFormatUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<SetListFormatDto>,
}

impl SetListFormatUseCase {
    pub fn new(uow_factory: Box<dyn SetListFormatUnitOfWorkFactoryTrait>) -> Self {
        SetListFormatUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &SetListFormatDto) -> Result<()> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let snapshot = execute_set_list_format(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(())
    }
}

impl UndoRedoCommand for SetListFormatUseCase {
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
        let snapshot = execute_set_list_format(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
