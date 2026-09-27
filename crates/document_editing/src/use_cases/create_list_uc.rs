use super::editing_helpers::{
    collect_blocks_with_owner_recursive, find_block_at_position, position_roots,
};
use crate::CreateListDto;
use crate::CreateListResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::refresh_block_positions;
use common::direct_access::block::block_repository::BlockRelationshipField;
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::direct_access::table::TableRelationshipField;
use common::entities::{Block, Document, Frame, List, Root, TableCell};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;
use std::collections::{HashMap, HashSet};

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
#[macros::uow_action(entity = "Table", action = "GetRelationship")]
#[macros::uow_action(entity = "TableCell", action = "GetMulti")]
#[macros::uow_action(entity = "List", action = "Create")]
#[macros::uow_action(entity = "List", action = "Remove")]
pub trait CreateListUnitOfWorkTrait: CommandUnitOfWork {}

pub struct CreateListUseCase {
    uow_factory: Box<dyn CreateListUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    /// The document as the execution left it, which redo puts back.
    redo_snapshot: Option<EntityTreeSnapshot>,
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

/// Where a block sits in the frame that lists it: its index in the frame's
/// `child_order`, which also counts the quotations and tables between blocks,
/// or in its block list when the frame keeps no order (it then holds nothing
/// else).
fn index_in_frame(frame: &Frame, block_id: EntityId) -> Option<usize> {
    if frame.child_order.is_empty() {
        frame.blocks.iter().position(|id| *id == block_id)
    } else {
        frame
            .child_order
            .iter()
            .position(|entry| *entry == block_id as i64)
    }
}

/// Turn the blocks from the one at the selection's start to the one at its
/// end into list items, wherever they sit: the main text, a quotation (at any
/// depth), a table cell or a footnote's body.
///
/// A list is written, and read back, inside one frame, and only as long as
/// nothing comes between its items: Djot, Markdown and HTML have no list that
/// runs on across a quotation or a table. The blocks are therefore taken in
/// runs, each run the blocks that follow one another in one frame, and each
/// run gets a list of its own. A selection held in one frame and crossing no
/// quotation or table is one run, and one list.
///
/// This used to look at the main frame's own blocks alone. With the caret in
/// a quotation or a table cell it found none: it created a list that held no
/// item and left the paragraph as it was, so moving a list item there one
/// level in or out (take the item out of its list, then make a list of it at
/// the new level) took it out of its list for good.
///
/// A list whose every item joins a new list is removed, as it would be had
/// its items left it one by one; no list is created when the selection holds
/// no block.
fn execute_create_list(
    uow: &mut Box<dyn CreateListUnitOfWorkTrait>,
    dto: &CreateListDto,
) -> Result<(CreateListResultDto, EntityTreeSnapshot, EntityTreeSnapshot)> {
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

    // Every block a position can fall in, with the frame that lists it: the
    // main flow (quotations and table cells included) and every footnote
    // definition.
    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    if frame_ids.is_empty() {
        return Err(anyhow!("Document has no frames"));
    }
    let get_table_cell_frames = |table_id: &EntityId| -> Result<Vec<EntityId>> {
        let cell_ids = uow.get_table_relationship(table_id, &TableRelationshipField::Cells)?;
        let mut cells: Vec<TableCell> = uow
            .get_table_cell_multi(&cell_ids)?
            .into_iter()
            .flatten()
            .collect();
        cells.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));
        Ok(cells.into_iter().filter_map(|c| c.cell_frame).collect())
    };
    let mut owner_of: HashMap<EntityId, EntityId> = HashMap::new();
    let mut block_ids: Vec<EntityId> = Vec::new();
    for root_frame in position_roots(&|id| uow.get_frame(id), &frame_ids)? {
        for (block_id, frame_id) in collect_blocks_with_owner_recursive(
            &|id| uow.get_frame(id),
            &|id, field| uow.get_frame_relationship(id, field),
            &get_table_cell_frames,
            &root_frame,
        )? {
            owner_of.insert(block_id, frame_id);
            block_ids.push(block_id);
        }
    }
    let mut blocks: Vec<Block> = uow
        .get_block_multi(&block_ids)?
        .into_iter()
        .flatten()
        .collect();
    // The stored field lags the rope by whatever was typed since the last deletion, and the
    // caller's positions are rope positions: read the rope's.
    let store = uow.store();
    refresh_block_positions(&mut blocks, &store);
    blocks.sort_by_key(|b| b.document_position);

    // The blocks the selection runs over: from the one at one end to the one
    // at the other, as a caret at either end would find them.
    let (caret_block, at_caret, _) = find_block_at_position(&blocks, dto.position, &store)?;
    let (_, at_anchor, _) = find_block_at_position(&blocks, dto.anchor, &store)?;
    let selected = &blocks[at_caret.min(at_anchor)..=at_caret.max(at_anchor)];

    // Split them into runs: one frame each, with nothing between two items.
    let mut frames: HashMap<EntityId, Frame> = HashMap::new();
    let mut runs: Vec<Vec<EntityId>> = Vec::new();
    let mut previous: Option<(EntityId, Option<usize>)> = None;
    for block in selected {
        let frame_id = *owner_of
            .get(&block.id)
            .ok_or_else(|| anyhow!("Block {} is in no frame", block.id))?;
        if let std::collections::hash_map::Entry::Vacant(slot) = frames.entry(frame_id) {
            let frame = uow
                .get_frame(&frame_id)?
                .ok_or_else(|| anyhow!("Frame {frame_id} not found"))?;
            slot.insert(frame);
        }
        let index = frames
            .get(&frame_id)
            .and_then(|frame| index_in_frame(frame, block.id));
        let follows = matches!(
            previous,
            Some((previous_frame, Some(previous_index)))
                if previous_frame == frame_id && index == Some(previous_index + 1)
        );
        match runs.last_mut() {
            Some(run) if follows => run.push(block.id),
            _ => runs.push(vec![block.id]),
        }
        previous = Some((frame_id, index));
    }

    // The lists the items leave, to remove those left without an item.
    let moved: HashSet<EntityId> = selected.iter().map(|b| b.id).collect();
    let left: HashSet<EntityId> = selected.iter().filter_map(|b| b.list).collect();

    // The list reported is the one holding the caret's block.
    let now = chrono::Utc::now();
    let mut result_list: Option<EntityId> = None;
    for run in &runs {
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
        for block_id in run {
            uow.set_block_relationship(
                block_id,
                &BlockRelationshipField::List,
                &[created_list.id],
            )?;
        }
        if result_list.is_none() || run.contains(&caret_block.id) {
            result_list = Some(created_list.id);
        }
    }

    let still_used: HashSet<EntityId> = blocks
        .iter()
        .filter(|b| !moved.contains(&b.id))
        .filter_map(|b| b.list)
        .collect();
    for list_id in left {
        if !still_used.contains(&list_id) {
            uow.remove_list(&list_id)?;
        }
    }

    let list_id = result_list.ok_or_else(|| anyhow!("The selection holds no block"))?;
    Ok((
        CreateListResultDto {
            list_id: list_id as i64,
        },
        snapshot,
        uow.snapshot_document(&[doc_id])?,
    ))
}

impl CreateListUseCase {
    pub fn new(uow_factory: Box<dyn CreateListUnitOfWorkFactoryTrait>) -> Self {
        CreateListUseCase {
            uow_factory,
            undo_snapshot: None,
            redo_snapshot: None,
        }
    }

    pub fn execute(&mut self, dto: &CreateListDto) -> Result<CreateListResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot, after) = execute_create_list(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.redo_snapshot = Some(after);

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

    /// Puts back the document as the first execution left it, rather than
    /// running it again. Run again, it made its list under a new id, and a
    /// later command of the same edit that names that list failed to redo: an
    /// editor moving a list item a level in makes a list of it and then sets
    /// that list's level, and the list the second step named had gone with the
    /// undo. The ids of the entities this made are kept this way.
    fn redo(&mut self) -> Result<()> {
        let snapshot = self
            .redo_snapshot
            .as_ref()
            .ok_or_else(|| anyhow!("No snapshot available for redo"))?
            .clone();

        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;
        uow.restore_document(&snapshot)?;
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
