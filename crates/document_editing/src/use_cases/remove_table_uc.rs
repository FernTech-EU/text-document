use super::editing_helpers::{
    NestedContentReader, Swept, collect_block_ids_recursive, impl_nested_content_reader,
};
use crate::RemoveTableDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{rope_insert_empty_block_first, rope_remove_markers};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::entities::{Block, Document, Frame, Root, Table, TableCell};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;
use std::collections::HashSet;

pub trait RemoveTableUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn RemoveTableUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "Update")]
#[macros::uow_action(entity = "Document", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Snapshot")]
#[macros::uow_action(entity = "Document", action = "Restore")]
#[macros::uow_action(entity = "Frame", action = "Get")]
#[macros::uow_action(entity = "Frame", action = "GetMulti")]
#[macros::uow_action(entity = "Frame", action = "Update")]
#[macros::uow_action(entity = "Frame", action = "Remove")]
#[macros::uow_action(entity = "Frame", action = "RemoveMulti")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "UpdateMulti")]
#[macros::uow_action(entity = "Block", action = "Create")]
#[macros::uow_action(entity = "Table", action = "Get")]
#[macros::uow_action(entity = "Table", action = "Remove")]
#[macros::uow_action(entity = "Table", action = "RemoveMulti")]
#[macros::uow_action(entity = "Table", action = "GetRelationship")]
#[macros::uow_action(entity = "TableCell", action = "GetMulti")]
pub trait RemoveTableUnitOfWorkTrait: CommandUnitOfWork {}

impl_nested_content_reader!(dyn RemoveTableUnitOfWorkTrait);

pub struct RemoveTableUseCase {
    uow_factory: Box<dyn RemoveTableUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<RemoveTableDto>,
}

fn execute_remove_table(
    uow: &mut Box<dyn RemoveTableUnitOfWorkTrait>,
    dto: &RemoveTableDto,
) -> Result<EntityTreeSnapshot> {
    let table_id = dto.table_id as EntityId;

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

    // Verify the table exists
    let _table = uow
        .get_table(&table_id)?
        .ok_or_else(|| anyhow!("Table {} not found", table_id))?;

    // Snapshot for undo before mutation
    let snapshot = uow.snapshot_document(&[doc_id])?;

    let now = chrono::Utc::now();
    let store = uow.store();

    // The table with everything it holds: its cells and their frames, what a
    // cell nests (a quotation, another table), and the rope entry of each
    // block and table among them. Removing the cells' frames alone left what
    // was nested in them behind: frames nothing reached, tables with no
    // anchor in any flow, and their text in the rope.
    let mut swept = Swept::default();
    swept.sweep_table(&*uow, table_id)?;
    let removed_block_ids: Vec<EntityId> = swept
        .markers
        .iter()
        .filter_map(|marker| marker.as_block())
        .collect();

    // Where the table's blocks started, for the stored positions of the
    // blocks after it (see below).
    let removed_blocks: Vec<Block> = uow
        .get_block_multi(&removed_block_ids)?
        .into_iter()
        .flatten()
        .collect();
    let min_cell_position = removed_blocks.iter().map(|b| b.document_position).min();
    let total_cell_blocks = removed_blocks.len() as i64;

    // The anchor frame of each table going away, wherever it sits: in the
    // main frame, a quotation, a table cell or a footnote's body.
    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let frames: Vec<Frame> = uow
        .get_frame_multi(&frame_ids)?
        .into_iter()
        .flatten()
        .collect();
    for frame in &frames {
        if frame
            .table
            .is_some_and(|named| swept.table_set.contains(&named))
        {
            swept.frames.push(frame.id);
        }
    }
    swept.frames.sort_unstable();
    swept.frames.dedup();
    let removed_frames: HashSet<EntityId> = swept.frames.iter().copied().collect();

    // Out of the rope in one pass: every block and anchor gathered above,
    // each with one boundary. This used to remove the cells' blocks one at a
    // time and then the anchor, whose removal shifted every entry from the
    // start of what it cut: an empty paragraph right before the table starts
    // there, so it moved back into the paragraph before it and the next save
    // split that paragraph and lost a letter. A table that was the whole
    // document had four bytes cut from a rope of three, and panicked.
    rope_remove_markers(&store, &swept.markers);

    // Every frame listing a removed frame drops it from its order.
    let is_removed_entry =
        |entry: &i64| *entry < 0 && removed_frames.contains(&((-*entry) as EntityId));
    for frame in &frames {
        if removed_frames.contains(&frame.id) || !frame.child_order.iter().any(is_removed_entry) {
            continue;
        }
        let mut updated = frame.clone();
        updated.child_order.retain(|entry| !is_removed_entry(entry));
        updated.updated_at = now;
        uow.update_frame(&updated)?;
    }

    // The frames first, their blocks going with them, then the tables, their
    // cells going with them. One call each: every removal rewrites the whole
    // list it removes from.
    if !swept.frames.is_empty() {
        uow.remove_frame_multi(&swept.frames)?;
    }
    uow.remove_table_multi(&swept.tables)?;

    // A table that was all of the main text leaves it without a paragraph,
    // where no caret can stand: it gets an empty one, as a deletion of all
    // the text does.
    let main_frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;
    let get_table_cell_frames = |id: &EntityId| -> Result<Vec<EntityId>> {
        let mut cells = uow.ncr_table_cells(id)?;
        cells.sort_by_key(|cell| (cell.row, cell.column));
        Ok(cells
            .into_iter()
            .filter_map(|cell| cell.cell_frame)
            .collect())
    };
    let main_blocks = collect_block_ids_recursive(
        &|id| uow.get_frame(id),
        &|id, field| uow.get_frame_relationship(id, field),
        &get_table_cell_frames,
        &main_frame_id,
    )?;
    let mut created_blocks: i64 = 0;
    if main_blocks.is_empty() {
        let empty_block = Block {
            document_position: 0,
            ..Block::default()
        };
        let created = uow.create_block(&empty_block, main_frame_id, -1)?;
        let mut main_frame = uow
            .get_frame(&main_frame_id)?
            .ok_or_else(|| anyhow!("Frame not found"))?;
        main_frame.child_order.push(created.id as i64);
        main_frame.updated_at = now;
        uow.update_frame(&main_frame)?;
        rope_insert_empty_block_first(&store, created.id);
        created_blocks = 1;
    }

    // Shift document_position for blocks after the removed table
    if let Some(table_start_pos) = min_cell_position {
        // Get all remaining blocks and shift those after the table
        let remaining_frame_ids =
            uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
        let mut blocks_to_shift: Vec<Block> = Vec::new();
        for fid in &remaining_frame_ids {
            let block_ids = uow.get_frame_relationship(fid, &FrameRelationshipField::Blocks)?;
            if !block_ids.is_empty() {
                let blocks_opt = uow.get_block_multi(&block_ids)?;
                for block in blocks_opt.into_iter().flatten() {
                    if block.document_position >= table_start_pos {
                        let mut shifted = block;
                        shifted.document_position -= total_cell_blocks;
                        shifted.updated_at = now;
                        blocks_to_shift.push(shifted);
                    }
                }
            }
        }
        if !blocks_to_shift.is_empty() {
            uow.update_block_multi(&blocks_to_shift)?;
        }
    }

    // Update Document stats
    let mut updated_doc = document.clone();
    updated_doc.block_count = (updated_doc.block_count - total_cell_blocks + created_blocks).max(0);
    updated_doc.updated_at = now;
    uow.update_document(&updated_doc)?;

    Ok(snapshot)
}

impl RemoveTableUseCase {
    pub fn new(uow_factory: Box<dyn RemoveTableUnitOfWorkFactoryTrait>) -> Self {
        RemoveTableUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &RemoveTableDto) -> Result<()> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let snapshot = execute_remove_table(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(())
    }
}

impl UndoRedoCommand for RemoveTableUseCase {
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
        let snapshot = execute_remove_table(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
