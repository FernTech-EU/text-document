use super::editing_helpers::{
    CellBlockReader, compute_table_base_pos, impl_cell_block_reader, reassign_cell_block_positions,
};
use crate::RemoveTableRowDto;
use crate::RemoveTableRowResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{rope_remove_block, rope_restore_table_reading_order};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::direct_access::table::table_repository::TableRelationshipField;
use common::entities::{Block, Document, Frame, Root, Table, TableCell};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait RemoveTableRowUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn RemoveTableRowUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "Update")]
#[macros::uow_action(entity = "Document", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Snapshot")]
#[macros::uow_action(entity = "Document", action = "Restore")]
#[macros::uow_action(entity = "Frame", action = "GetMulti")]
#[macros::uow_action(entity = "Frame", action = "Remove")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "UpdateMulti")]
#[macros::uow_action(entity = "Table", action = "Get")]
#[macros::uow_action(entity = "Table", action = "Update")]
#[macros::uow_action(entity = "Table", action = "GetRelationship")]
#[macros::uow_action(entity = "TableCell", action = "GetMulti")]
#[macros::uow_action(entity = "TableCell", action = "Remove")]
#[macros::uow_action(entity = "TableCell", action = "RemoveMulti")]
#[macros::uow_action(entity = "TableCell", action = "UpdateMulti")]
pub trait RemoveTableRowUnitOfWorkTrait: CommandUnitOfWork {}

impl_cell_block_reader!(dyn RemoveTableRowUnitOfWorkTrait);

pub struct RemoveTableRowUseCase {
    uow_factory: Box<dyn RemoveTableRowUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<RemoveTableRowDto>,
}

fn execute_remove_table_row(
    uow: &mut Box<dyn RemoveTableRowUnitOfWorkTrait>,
    dto: &RemoveTableRowDto,
) -> Result<(RemoveTableRowResultDto, EntityTreeSnapshot)> {
    let table_id = dto.table_id as EntityId;
    let now = chrono::Utc::now();

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

    // Get and validate table
    let table = uow
        .get_table(&table_id)?
        .ok_or_else(|| anyhow!("Table {} not found", table_id))?;

    if dto.row_index < 0 || dto.row_index >= table.rows {
        return Err(anyhow!(
            "Row index {} out of range [0, {})",
            dto.row_index,
            table.rows
        ));
    }

    if table.rows <= 1 {
        return Err(anyhow!("Cannot remove the last row of a table"));
    }

    // Snapshot for undo before mutation
    let snapshot = uow.snapshot_document(&[doc_id])?;

    // Get all cells of this table
    let cell_ids = uow.get_table_relationship(&table_id, &TableRelationshipField::Cells)?;
    let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
    let cells: Vec<TableCell> = cells_opt.into_iter().flatten().collect();

    // Identify cells that start exactly at the target row and have row_span == 1
    // These are the only cells to fully remove.
    // Cells that start at the target row but span multiple rows are shifted down with span decremented.
    // Cells that start before the target row but span across it just get span decremented.
    let mut cells_to_remove: Vec<&TableCell> = Vec::new();
    let mut cells_to_update: Vec<TableCell> = Vec::new();

    // Compute base_pos from ALL cells BEFORE any removal, so the table starting
    // position is correct even when the first row is being removed.
    let all_cell_frame_ids: Vec<EntityId> = cells.iter().filter_map(|c| c.cell_frame).collect();
    let base_pos = compute_table_base_pos(&*uow, &all_cell_frame_ids)?;

    for cell in &cells {
        if cell.row == dto.row_index && cell.row_span == 1 {
            // Single-row cell at the target row — remove it
            cells_to_remove.push(cell);
        } else if cell.row == dto.row_index && cell.row_span > 1 {
            // Multi-row cell starting at target row — shift down and shrink span
            let mut updated = cell.clone();
            updated.row_span -= 1;
            updated.updated_at = now;
            cells_to_update.push(updated);
        } else if cell.row < dto.row_index && cell.row + cell.row_span > dto.row_index {
            // Cell starts before target row but spans across it — shrink span
            let mut updated = cell.clone();
            updated.row_span -= 1;
            updated.updated_at = now;
            cells_to_update.push(updated);
        } else if cell.row > dto.row_index {
            // Cell starts after the target row — shift up
            let mut shifted = cell.clone();
            shifted.row -= 1;
            shifted.updated_at = now;
            cells_to_update.push(shifted);
        }
    }

    // Remove cell frames for cells being fully removed. Before
    // removal, collect the block IDs owned by those cell frames so we
    // can strip their entries from the global rope.
    let row_cell_frame_ids: Vec<EntityId> = cells_to_remove
        .iter()
        .filter_map(|c| c.cell_frame)
        .collect();
    let mut removed_cell_block_ids: Vec<EntityId> = Vec::new();
    for fid in &row_cell_frame_ids {
        let bids = uow.get_frame_relationship(fid, &FrameRelationshipField::Blocks)?;
        removed_cell_block_ids.extend(bids);
    }
    for fid in &row_cell_frame_ids {
        uow.remove_frame(fid)?;
    }
    // Mirror the cell removal into the global rope.
    {
        let store = uow.store();
        for bid in &removed_cell_block_ids {
            rope_remove_block(&store, *bid);
        }
    }

    // Remove the TableCell entities
    let row_cell_ids: Vec<EntityId> = cells_to_remove.iter().map(|c| c.id).collect();
    if !row_cell_ids.is_empty() {
        uow.remove_table_cell_multi(&row_cell_ids)?;
    }

    // Apply updates (shifts and span changes)
    if !cells_to_update.is_empty() {
        uow.update_table_cell_multi(&cells_to_update)?;
    }

    // Update table row count
    let mut updated_table = table.clone();
    updated_table.rows -= 1;
    updated_table.updated_at = now;
    uow.update_table(&updated_table)?;

    // A cell that spanned the removed row from it keeps its row, and the row
    // below moves up beside it: its cells of earlier columns now come first in
    // reading order, where the rope still held the spanning cell's text first.
    rope_restore_table_reading_order(&uow.store(), table_id);

    // Recalculate document_positions for remaining cell blocks
    let remaining_cell_ids =
        uow.get_table_relationship(&table_id, &TableRelationshipField::Cells)?;
    let remaining_cells_opt = uow.get_table_cell_multi(&remaining_cell_ids)?;
    let mut remaining_cells: Vec<TableCell> = remaining_cells_opt.into_iter().flatten().collect();
    remaining_cells.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));

    let remaining_frame_ids: Vec<EntityId> = remaining_cells
        .iter()
        .filter_map(|c| c.cell_frame)
        .collect();

    // Assign positions in row-major order (handles multi-block cells)
    let (cell_blocks_to_update, _) =
        reassign_cell_block_positions(&*uow, &remaining_cells, base_pos, now)?;
    if !cell_blocks_to_update.is_empty() {
        uow.update_block_multi(&cell_blocks_to_update)?;
    }

    // Shift non-table blocks after the table
    let removed_cells = cells_to_remove.len() as i64;
    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let cell_frame_set: std::collections::HashSet<EntityId> =
        remaining_frame_ids.into_iter().collect();
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
                shifted.document_position -= removed_cells;
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
    updated_doc.block_count -= removed_cells;
    updated_doc.updated_at = now;
    uow.update_document(&updated_doc)?;

    Ok((
        RemoveTableRowResultDto {
            new_row_count: updated_table.rows,
        },
        snapshot,
    ))
}

impl RemoveTableRowUseCase {
    pub fn new(uow_factory: Box<dyn RemoveTableRowUnitOfWorkFactoryTrait>) -> Self {
        RemoveTableRowUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &RemoveTableRowDto) -> Result<RemoveTableRowResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;
        let (result, snapshot) = execute_remove_table_row(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());
        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for RemoveTableRowUseCase {
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
        let (_, snapshot) = execute_remove_table_row(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
