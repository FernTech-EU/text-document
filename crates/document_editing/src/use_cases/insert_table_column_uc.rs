use super::editing_helpers::{
    CellBlockReader, CellFrameCreator, compute_table_base_pos, create_cell_frame,
    impl_cell_block_reader, impl_cell_frame_creator, reassign_cell_block_positions,
};
use crate::InsertTableColumnDto;
use crate::InsertTableColumnResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{rope_insert_block_at, top_level_frame_end_byte};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::direct_access::table::table_repository::TableRelationshipField;
use common::entities::{Block, Document, Frame, Root, Table, TableCell};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait InsertTableColumnUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn InsertTableColumnUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "Update")]
#[macros::uow_action(entity = "Document", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Snapshot")]
#[macros::uow_action(entity = "Document", action = "Restore")]
#[macros::uow_action(entity = "Frame", action = "Create")]
#[macros::uow_action(entity = "Frame", action = "Update")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "Create")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "UpdateMulti")]
#[macros::uow_action(entity = "Table", action = "Get")]
#[macros::uow_action(entity = "Table", action = "Update")]
#[macros::uow_action(entity = "Table", action = "GetRelationship")]
#[macros::uow_action(entity = "TableCell", action = "GetMulti")]
#[macros::uow_action(entity = "TableCell", action = "Create")]
#[macros::uow_action(entity = "TableCell", action = "UpdateMulti")]
pub trait InsertTableColumnUnitOfWorkTrait: CommandUnitOfWork {}

impl_cell_frame_creator!(dyn InsertTableColumnUnitOfWorkTrait);
impl_cell_block_reader!(dyn InsertTableColumnUnitOfWorkTrait);

pub struct InsertTableColumnUseCase {
    uow_factory: Box<dyn InsertTableColumnUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<InsertTableColumnDto>,
}

fn execute_insert_table_column(
    uow: &mut Box<dyn InsertTableColumnUnitOfWorkTrait>,
    dto: &InsertTableColumnDto,
) -> Result<(InsertTableColumnResultDto, EntityTreeSnapshot)> {
    let table_id = dto.table_id as EntityId;
    let now = chrono::Utc::now();

    // Get Root -> Document
    let root = uow
        .get_root(&ROOT_ENTITY_ID)?
        .ok_or_else(|| anyhow!("Root not found"))?;
    let doc_ids = uow.get_root_relationship(&root.id, &RootRelationshipField::Document)?;
    let doc_id = *doc_ids.first().ok_or_else(|| anyhow!("No document"))?;
    let document = uow
        .get_document(&doc_id)?
        .ok_or_else(|| anyhow!("Document not found"))?;

    let table = uow
        .get_table(&table_id)?
        .ok_or_else(|| anyhow!("Table not found"))?;

    if dto.column_index < 0 || dto.column_index > table.columns {
        return Err(anyhow!(
            "Column index {} out of range [0, {}]",
            dto.column_index,
            table.columns
        ));
    }

    let snapshot = uow.snapshot_document(&[doc_id])?;

    // Get existing cells and compute base_pos BEFORE creating new cells
    let cell_ids = uow.get_table_relationship(&table_id, &TableRelationshipField::Cells)?;
    let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
    let cells: Vec<TableCell> = cells_opt.into_iter().flatten().collect();

    // Find base position from existing cell blocks (must be done before new cells are created)
    let existing_cell_frame_ids: Vec<EntityId> =
        cells.iter().filter_map(|c| c.cell_frame).collect();
    let base_pos = compute_table_base_pos(&*uow, &existing_cell_frame_ids)?;

    let mut cells_to_update: Vec<TableCell> = Vec::new();
    for cell in &cells {
        if cell.column >= dto.column_index {
            // Cell starts at or after the insertion point — shift it right
            let mut shifted = cell.clone();
            shifted.column += 1;
            shifted.updated_at = now;
            cells_to_update.push(shifted);
        } else if cell.column + cell.column_span > dto.column_index {
            // Cell starts before the insertion point but spans across it — expand span
            let mut expanded = cell.clone();
            expanded.column_span += 1;
            expanded.updated_at = now;
            cells_to_update.push(expanded);
        }
    }
    if !cells_to_update.is_empty() {
        uow.update_table_cell_multi(&cells_to_update)?;
    }

    // Create new cells for the inserted column (one per row), collecting
    // their blocks so we can mirror them into the rope after the entity
    // graph is in place.
    let mut new_cell_blocks: Vec<Block> = Vec::with_capacity(table.rows as usize);
    for r in 0..table.rows {
        let (cell_frame_id, created_block) = create_cell_frame(&mut *uow, doc_id, now)?;
        new_cell_blocks.push(created_block);

        let cell = TableCell {
            id: 0,
            created_at: now,
            updated_at: now,
            row: r,
            column: dto.column_index,
            row_span: 1,
            column_span: 1,
            cell_frame: Some(cell_frame_id),
            fmt_padding: None,
            fmt_border: None,
            fmt_vertical_alignment: None,
            fmt_background_color: None,
        };
        uow.create_table_cell(&cell, table_id, -1)?;
    }

    // Update table column count and column_widths
    let mut updated_table = table.clone();
    updated_table.columns += 1;
    let insert_idx = (dto.column_index as usize).min(updated_table.column_widths.len());
    updated_table.column_widths.insert(insert_idx, 0);
    updated_table.updated_at = now;
    uow.update_table(&updated_table)?;

    // Mirror the new cell blocks into the global rope. Place them at
    // the end of the table's parent top-level frame (matching the
    // convention from `insert_table_uc`). Each cell is empty so the
    // text is `""`; `rope_insert_block_at` prepends a `\n` boundary.
    {
        let store = uow.store();
        let parent_frame_id_opt = {
            let frames = store.frames.read();
            let anchor = frames.values().find(|f| f.table == Some(table_id)).cloned();
            anchor.and_then(|f| f.parent_frame)
        };
        if let Some(parent_frame_id) = parent_frame_id_opt {
            let start_byte = top_level_frame_end_byte(&store, parent_frame_id);
            for (next_byte, cell_block) in (start_byte..).zip(new_cell_blocks.iter()) {
                // Newly-created cells are empty (`""`).
                rope_insert_block_at(&store, next_byte, cell_block.id, "");
            }
        }
    }

    // Recalculate document positions for all table cell blocks (using base_pos computed earlier)
    let all_cell_ids = uow.get_table_relationship(&table_id, &TableRelationshipField::Cells)?;
    let all_cells_opt = uow.get_table_cell_multi(&all_cell_ids)?;
    let mut all_cells: Vec<TableCell> = all_cells_opt.into_iter().flatten().collect();
    all_cells.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));

    let cell_frame_ids: Vec<EntityId> = all_cells.iter().filter_map(|c| c.cell_frame).collect();

    // Assign positions in row-major order (handles multi-block cells)
    let (cell_blocks_to_update, _) =
        reassign_cell_block_positions(&*uow, &all_cells, base_pos, now)?;
    if !cell_blocks_to_update.is_empty() {
        uow.update_block_multi(&cell_blocks_to_update)?;
    }

    // Shift non-table blocks after the table
    let added_cells = table.rows;
    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let cell_frame_set: std::collections::HashSet<EntityId> = cell_frame_ids.into_iter().collect();
    let mut shifted_blocks: Vec<Block> = Vec::new();
    for fid in &frame_ids {
        if cell_frame_set.contains(fid) {
            continue;
        }
        let block_ids = uow.get_frame_relationship(fid, &FrameRelationshipField::Blocks)?;
        if block_ids.is_empty() {
            continue;
        }
        let blocks_opt = uow.get_block_multi(&block_ids)?;
        for block in blocks_opt.into_iter().flatten() {
            if block.document_position >= base_pos {
                let mut shifted = block;
                shifted.document_position += added_cells;
                shifted.updated_at = now;
                shifted_blocks.push(shifted);
            }
        }
    }
    if !shifted_blocks.is_empty() {
        uow.update_block_multi(&shifted_blocks)?;
    }

    // Update Document stats
    let mut updated_doc = document.clone();
    updated_doc.block_count += table.rows;
    updated_doc.updated_at = now;
    uow.update_document(&updated_doc)?;

    Ok((
        InsertTableColumnResultDto {
            new_column_count: updated_table.columns,
        },
        snapshot,
    ))
}

impl InsertTableColumnUseCase {
    pub fn new(uow_factory: Box<dyn InsertTableColumnUnitOfWorkFactoryTrait>) -> Self {
        InsertTableColumnUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &InsertTableColumnDto) -> Result<InsertTableColumnResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;
        let (result, snapshot) = execute_insert_table_column(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());
        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for InsertTableColumnUseCase {
    fn undo(&mut self) -> Result<()> {
        let snapshot = self
            .undo_snapshot
            .as_ref()
            .ok_or_else(|| anyhow!("No snapshot for undo"))?
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
            .ok_or_else(|| anyhow!("No DTO for redo"))?
            .clone();
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;
        let (_, snapshot) = execute_insert_table_column(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
