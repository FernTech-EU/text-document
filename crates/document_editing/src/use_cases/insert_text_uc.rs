use super::editing_helpers::{collect_block_ids_recursive, is_word_boundary_punct};
use crate::InsertTextDto;
use crate::InsertTextResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{
    block_char_length, block_content_via_store, find_block_at_char_position, rope_delete_in_block,
    rope_insert_in_block,
};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::direct_access::table::TableRelationshipField;
use common::entities::{Block, Document, Frame, Root, TableCell};
use common::format_runs::{
    FormatRun, ImageAnchor, debug_assert_well_formed, logical_offset_to_byte,
    shift_images_for_delete, shift_images_for_insert, shift_runs_for_delete, shift_runs_for_insert,
};

use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;
use std::time::Instant;

pub trait InsertTextUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn InsertTextUnitOfWorkTrait>;
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
#[macros::uow_action(entity = "Table", action = "GetRelationship")]
#[macros::uow_action(entity = "TableCell", action = "GetMulti")]
pub trait InsertTextUnitOfWorkTrait: CommandUnitOfWork {}

/// Lightweight undo data for the no-selection insert path. The cloned
/// format_runs / block_images vectors serve as a per-block backup so
/// undo can restore the run table verbatim.
struct UndoData {
    block_id: EntityId,
    original_block: Block,
    original_format_runs: Vec<FormatRun>,
    original_block_images: Vec<ImageAnchor>,
    doc_id: EntityId,
    original_character_count: i64,
    /// Byte range in the block where the text was inserted. Used by
    /// undo to delete those bytes from the rope.
    inserted_byte_offset: u32,
    inserted_byte_len: u32,
}

enum InsertTextUndo {
    Simple(Box<UndoData>),
    SelectionReplacement(common::snapshot::EntityTreeSnapshot),
}

/// Delete a logical character range `[start_offset..end_offset)` inside a
/// single block. Mutates `block.plain_text`, `block_char_length(&block, &store)`,
/// `format_runs[block.id]`, and `block_images[block.id]` consistently.
/// Returns the count of logical positions removed (text chars + images).
fn delete_range_in_block(
    uow: &mut Box<dyn InsertTextUnitOfWorkTrait>,
    block: &Block,
    start_offset: i64,
    end_offset: i64,
) -> Result<i64> {
    if end_offset <= start_offset {
        return Ok(0);
    }

    let store = uow.store();
    let images_before = store
        .block_images
        .read()
        .get(&block.id)
        .cloned()
        .unwrap_or_default();

    let block_text = block_content_via_store(block, &store);
    let byte_start = logical_offset_to_byte(&block_text, &images_before, start_offset);
    let byte_end = logical_offset_to_byte(&block_text, &images_before, end_offset);

    let removed_text_chars = block_text[byte_start as usize..byte_end as usize]
        .chars()
        .count() as i64;

    let mut new_plain = String::with_capacity(block_text.len() - (byte_end - byte_start) as usize);
    new_plain.push_str(&block_text[..byte_start as usize]);
    new_plain.push_str(&block_text[byte_end as usize..]);

    // Mutate format_runs.
    {
        let mut runs_map = store.format_runs.write();
        let runs = runs_map.entry(block.id).or_default();
        shift_runs_for_delete(runs, byte_start, byte_end);
        debug_assert_well_formed(runs, new_plain.len());
    }

    // Mutate block_images and capture how many were removed.
    let images_removed = {
        let mut images_map = store.block_images.write();
        let images = images_map.entry(block.id).or_default();
        shift_images_for_delete(images, byte_start, byte_end) as i64
    };

    rope_delete_in_block(&store, block.id, byte_start, byte_end);

    let positions_removed = removed_text_chars + images_removed;

    let mut updated_block = block.clone();
    updated_block.updated_at = chrono::Utc::now();
    uow.update_block(&updated_block)?;

    Ok(positions_removed)
}

fn execute_insert_with_selection(
    uow: &mut Box<dyn InsertTextUnitOfWorkTrait>,
    dto: &InsertTextDto,
) -> Result<(InsertTextResultDto, InsertTextUndo)> {
    let root = uow
        .get_root(&ROOT_ENTITY_ID)?
        .ok_or_else(|| anyhow!("Root entity not found"))?;
    let doc_ids = uow.get_root_relationship(&root.id, &RootRelationshipField::Document)?;
    let doc_id = *doc_ids
        .first()
        .ok_or_else(|| anyhow!("Root has no document"))?;

    let mut document = uow
        .get_document(&doc_id)?
        .ok_or_else(|| anyhow!("Document not found"))?;

    let snapshot = uow.snapshot_document(&[doc_id])?;

    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;

    let get_table_cell_frames = |table_id: &EntityId| -> anyhow::Result<Vec<EntityId>> {
        let cell_ids = uow.get_table_relationship(table_id, &TableRelationshipField::Cells)?;
        let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
        let mut cells: Vec<TableCell> = cells_opt.into_iter().flatten().collect();
        cells.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));
        Ok(cells.into_iter().filter_map(|c| c.cell_frame).collect())
    };
    let all_block_ids = collect_block_ids_recursive(
        &|id| uow.get_frame(id),
        &|id, field| uow.get_frame_relationship(id, field),
        &get_table_cell_frames,
        &frame_id,
    )?;
    let blocks_opt = uow.get_block_multi(&all_block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
    blocks.sort_by_key(|b| b.document_position);

    let sel_start = std::cmp::min(dto.position, dto.anchor);
    let sel_end = std::cmp::max(dto.position, dto.anchor);
    let position = sel_start;

    let (sel_block, sel_block_idx, sel_start_offset) =
        super::editing_helpers::find_block_at_position(&blocks, sel_start, &uow.store())?;
    let (_, sel_end_block_idx, sel_end_offset) =
        super::editing_helpers::find_block_at_position(&blocks, sel_end, &uow.store())?;

    if sel_block_idx != sel_end_block_idx {
        return Err(anyhow!(
            "Cross-block selection replacement is not supported by insert_text. \
             Use delete_text first, then insert_text."
        ));
    }

    let chars_removed = delete_range_in_block(uow, &sel_block, sel_start_offset, sel_end_offset)?;

    document.character_count -= chars_removed;
    document.updated_at = chrono::Utc::now();
    uow.update_document(&document)?;

    let mut to_update = Vec::new();
    for b in &blocks[(sel_block_idx + 1)..] {
        let mut ub = b.clone();
        ub.document_position -= chars_removed;
        ub.updated_at = chrono::Utc::now();
        to_update.push(ub);
    }
    if !to_update.is_empty() {
        uow.update_block_multi(&to_update)?;
    }

    let blocks_opt = uow.get_block_multi(&all_block_ids)?;
    blocks = blocks_opt.into_iter().flatten().collect();
    blocks.sort_by_key(|b| b.document_position);
    document = uow
        .get_document(&doc_id)?
        .ok_or_else(|| anyhow!("Document not found"))?;

    let (block, block_idx, offset) =
        super::editing_helpers::find_block_at_position(&blocks, position, &uow.store())?;

    let store = uow.store();
    let images = store
        .block_images
        .read()
        .get(&block.id)
        .cloned()
        .unwrap_or_default();
    let block_text = block_content_via_store(&block, &store);
    let byte_offset = logical_offset_to_byte(&block_text, &images, offset);
    let inserted_byte_len = dto.text.len() as u32;
    let inserted_char_len = dto.text.chars().count() as i64;

    let mut new_plain = block_text.clone();
    new_plain.insert_str(byte_offset as usize, &dto.text);

    let mut updated_block = block.clone();
    updated_block.updated_at = chrono::Utc::now();
    uow.update_block(&updated_block)?;

    {
        let mut runs_map = store.format_runs.write();
        let runs = runs_map.entry(block.id).or_default();
        shift_runs_for_insert(runs, byte_offset, inserted_byte_len);
        debug_assert_well_formed(runs, new_plain.len());
    }
    {
        let mut images_map = store.block_images.write();
        if let Some(images) = images_map.get_mut(&block.id) {
            shift_images_for_insert(images, byte_offset, inserted_byte_len);
        }
    }

    rope_insert_in_block(&store, block.id, byte_offset, &dto.text);

    let mut blocks_to_update: Vec<Block> = Vec::new();
    for b in &blocks[(block_idx + 1)..] {
        let mut ub = b.clone();
        ub.document_position += inserted_char_len;
        ub.updated_at = chrono::Utc::now();
        blocks_to_update.push(ub);
    }
    if !blocks_to_update.is_empty() {
        uow.update_block_multi(&blocks_to_update)?;
    }

    let mut updated_doc = document.clone();
    updated_doc.character_count += inserted_char_len;
    updated_doc.updated_at = chrono::Utc::now();
    uow.update_document(&updated_doc)?;

    Ok((
        InsertTextResultDto {
            new_position: block.document_position + offset + inserted_char_len,
            blocks_affected: 1,
        },
        InsertTextUndo::SelectionReplacement(snapshot),
    ))
}

fn execute_insert_simple(
    uow: &mut Box<dyn InsertTextUnitOfWorkTrait>,
    dto: &InsertTextDto,
) -> Result<(InsertTextResultDto, InsertTextUndo)> {
    let position = dto.position;

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

    let store = uow.store();
    // Fast path: O(log n) block lookup via the rope index. Returns
    // None for tabled documents — they need the per-block walk below
    // because table cell content lives at separate rope byte ranges
    // (plan §1.6) so byte→block lookup would find the wrong block.
    let (block, block_pos, offset) = match find_block_at_char_position(&store, position) {
        Some((block_id, char_in_block, block_char_start)) => {
            let block = uow
                .get_block(&block_id)?
                .ok_or_else(|| anyhow!("Block not found"))?;
            let offset = char_in_block.clamp(0, block_char_length(&block, &store));
            (block, block_char_start, offset)
        }
        None => {
            // Slow path: tabled document. Walk blocks to find the cursor's
            // position in user-visible flow order.
            let frame_ids =
                uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
            let frame_id = *frame_ids
                .first()
                .ok_or_else(|| anyhow!("Document has no frames"))?;
            let get_table_cell_frames = |table_id: &EntityId| -> anyhow::Result<Vec<EntityId>> {
                let cell_ids =
                    uow.get_table_relationship(table_id, &TableRelationshipField::Cells)?;
                let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
                let mut cells: Vec<TableCell> = cells_opt.into_iter().flatten().collect();
                cells.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));
                Ok(cells.into_iter().filter_map(|c| c.cell_frame).collect())
            };
            let ordered_block_ids = collect_block_ids_recursive(
                &|id| uow.get_frame(id),
                &|id, field| uow.get_frame_relationship(id, field),
                &get_table_cell_frames,
                &frame_id,
            )?;
            if ordered_block_ids.is_empty() {
                return Err(anyhow!("No blocks in document"));
            }
            let (block, _block_idx, block_pos) =
                find_block_at_position_sequential(&**uow, &ordered_block_ids, position)?;
            let offset = (position - block_pos).clamp(0, block_char_length(&block, &store));
            (block, block_pos, offset)
        }
    };

    let original_block = block.clone();
    let original_format_runs = store
        .format_runs
        .read()
        .get(&block.id)
        .cloned()
        .unwrap_or_default();
    let original_block_images = store
        .block_images
        .read()
        .get(&block.id)
        .cloned()
        .unwrap_or_default();

    let block_text = block_content_via_store(&block, &store);
    let byte_offset = logical_offset_to_byte(&block_text, &original_block_images, offset);
    let inserted_byte_len = dto.text.len() as u32;
    let inserted_char_len = dto.text.chars().count() as i64;

    let mut new_plain = block_text.clone();
    new_plain.insert_str(byte_offset as usize, &dto.text);

    let mut updated_block = block.clone();
    updated_block.updated_at = chrono::Utc::now();
    uow.update_block(&updated_block)?;

    {
        let mut runs_map = store.format_runs.write();
        let runs = runs_map.entry(block.id).or_default();
        shift_runs_for_insert(runs, byte_offset, inserted_byte_len);
        debug_assert_well_formed(runs, new_plain.len());
    }
    {
        let mut images_map = store.block_images.write();
        if let Some(images) = images_map.get_mut(&block.id) {
            shift_images_for_insert(images, byte_offset, inserted_byte_len);
        }
    }

    rope_insert_in_block(&store, block.id, byte_offset, &dto.text);

    // Shift `document_position` for every block sitting *after* the
    // inserted-into one in document order — but only when the rope
    // can't be used as the source of truth (tables present or
    // sub-frames not mirrored). For rope-clean documents, readers
    // derive positions directly from `BlockOffsetIndex`, so the O(N)
    // walk here is unnecessary and dominated per-keystroke cost on
    // /large/1000para benches. The catch-up logic in `insert_table_uc`
    // brings stored values back into sync if a table is later added.
    if !common::database::rope_helpers::rope_positions_match_flow(&store) {
        let frame_ids =
            uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
        let mut all_blocks: Vec<Block> = Vec::new();
        for fid in &frame_ids {
            let block_ids = uow.get_frame_relationship(fid, &FrameRelationshipField::Blocks)?;
            if !block_ids.is_empty() {
                let blocks_opt = uow.get_block_multi(&block_ids)?;
                all_blocks.extend(blocks_opt.into_iter().flatten());
            }
        }
        let mut blocks_to_update: Vec<Block> = Vec::new();
        for b in all_blocks {
            if b.id != block.id && b.document_position > block.document_position {
                let mut ub = b;
                ub.document_position += inserted_char_len;
                ub.updated_at = chrono::Utc::now();
                blocks_to_update.push(ub);
            }
        }
        if !blocks_to_update.is_empty() {
            uow.update_block_multi(&blocks_to_update)?;
        }
    }

    let mut updated_doc = document.clone();
    updated_doc.character_count += inserted_char_len;
    updated_doc.updated_at = chrono::Utc::now();
    uow.update_document(&updated_doc)?;

    let undo_data = UndoData {
        block_id: block.id,
        original_block,
        original_format_runs,
        original_block_images,
        doc_id,
        original_character_count: document.character_count,
        inserted_byte_offset: byte_offset,
        inserted_byte_len,
    };

    Ok((
        InsertTextResultDto {
            new_position: block_pos + offset + inserted_char_len,
            blocks_affected: 1,
        },
        InsertTextUndo::Simple(Box::new(undo_data)),
    ))
}

/// Slow-path block lookup for tabled documents. Walks the ordered
/// block list, computing positions sequentially. Only used when the
/// O(log n) fast path `find_block_at_char_position` returns `None`
/// (i.e. when the document contains tables — cell content lives at
/// separate rope ranges so byte→block lookup can't be used directly).
fn find_block_at_position_sequential(
    uow: &dyn InsertTextUnitOfWorkTrait,
    ordered_block_ids: &[EntityId],
    position: i64,
) -> Result<(Block, usize, i64)> {
    if ordered_block_ids.is_empty() {
        return Err(anyhow!("No blocks in document"));
    }

    let store = uow.store();
    let mut running_pos: i64 = 0;
    for (idx, &block_id) in ordered_block_ids.iter().enumerate() {
        let block = uow
            .get_block(&block_id)?
            .ok_or_else(|| anyhow!("Block not found"))?;
        let block_end = running_pos + block_char_length(&block, &store);

        if position >= running_pos && position <= block_end {
            return Ok((block, idx, running_pos));
        }
        running_pos = block_end + 1;
    }

    let last_idx = ordered_block_ids.len() - 1;
    let block = uow
        .get_block(&ordered_block_ids[last_idx])?
        .ok_or_else(|| anyhow!("Block not found"))?;
    let mut pos: i64 = 0;
    for &id in &ordered_block_ids[..last_idx] {
        if let Some(b) = uow.get_block(&id)? {
            pos += block_char_length(&b, &store) + 1;
        }
    }
    Ok((block, last_idx, pos))
}

pub struct InsertTextUseCase {
    uow_factory: Box<dyn InsertTextUnitOfWorkFactoryTrait>,
    undo_data: Option<InsertTextUndo>,
    last_dto: Option<InsertTextDto>,
    last_result: Option<InsertTextResultDto>,
    last_merge_time: Option<Instant>,
    was_selection_replacement: bool,
}

impl InsertTextUseCase {
    pub fn new(uow_factory: Box<dyn InsertTextUnitOfWorkFactoryTrait>) -> Self {
        InsertTextUseCase {
            uow_factory,
            undo_data: None,
            last_dto: None,
            last_result: None,
            last_merge_time: None,
            was_selection_replacement: false,
        }
    }

    pub fn execute(&mut self, dto: &InsertTextDto) -> Result<InsertTextResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let has_selection = dto.position != dto.anchor;
        let (result, undo) = if has_selection {
            execute_insert_with_selection(&mut uow, dto)?
        } else {
            execute_insert_simple(&mut uow, dto)?
        };

        self.undo_data = Some(undo);
        self.last_dto = Some(dto.clone());
        self.last_result = Some(result.clone());
        self.last_merge_time = Some(Instant::now());
        self.was_selection_replacement = has_selection;

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for InsertTextUseCase {
    fn undo(&mut self) -> Result<()> {
        let undo = self
            .undo_data
            .as_ref()
            .ok_or_else(|| anyhow!("No undo data available"))?;

        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        match undo {
            InsertTextUndo::SelectionReplacement(snapshot) => {
                uow.restore_document(&snapshot.clone())?;
            }
            InsertTextUndo::Simple(data) => {
                uow.update_block(&data.original_block)?;

                let store = uow.store();
                store
                    .format_runs
                    .write()
                    .insert(data.block_id, data.original_format_runs.clone());
                store
                    .block_images
                    .write()
                    .insert(data.block_id, data.original_block_images.clone());

                // Revert the rope mutation done by the forward path.
                if data.inserted_byte_len > 0 {
                    rope_delete_in_block(
                        &store,
                        data.block_id,
                        data.inserted_byte_offset,
                        data.inserted_byte_offset + data.inserted_byte_len,
                    );
                }

                let mut doc = uow
                    .get_document(&data.doc_id)?
                    .ok_or_else(|| anyhow!("Document not found"))?;
                doc.character_count = data.original_character_count;
                doc.updated_at = chrono::Utc::now();
                uow.update_document(&doc)?;
            }
        }

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
        let has_selection = dto.position != dto.anchor;
        let (_, undo) = if has_selection {
            execute_insert_with_selection(&mut uow, &dto)?
        } else {
            execute_insert_simple(&mut uow, &dto)?
        };
        self.undo_data = Some(undo);
        uow.commit()?;
        Ok(())
    }

    fn can_merge(&self, other: &dyn UndoRedoCommand) -> bool {
        let Some(other_cmd) = other.as_any().downcast_ref::<InsertTextUseCase>() else {
            return false;
        };

        let (Some(self_result), Some(self_time), Some(self_dto)) =
            (&self.last_result, &self.last_merge_time, &self.last_dto)
        else {
            return false;
        };
        let (Some(other_dto), Some(other_time)) = (&other_cmd.last_dto, &other_cmd.last_merge_time)
        else {
            return false;
        };

        if other_time.duration_since(*self_time) > std::time::Duration::from_secs(2) {
            return false;
        }

        if other_cmd.was_selection_replacement {
            return false;
        }

        if other_dto.position != self_result.new_position {
            return false;
        }

        if self_dto.text.chars().count() + other_dto.text.chars().count() > 200 {
            return false;
        }

        let self_text = &self_dto.text;
        let other_text = &other_dto.text;
        if let (Some(last_self), Some(first_other)) =
            (self_text.chars().next_back(), other_text.chars().next())
        {
            let self_is_boundary = last_self.is_whitespace() || is_word_boundary_punct(last_self);
            let other_is_word =
                !first_other.is_whitespace() && !is_word_boundary_punct(first_other);
            if self_is_boundary && other_is_word {
                return false;
            }
        }

        true
    }

    fn merge(&mut self, other: &dyn UndoRedoCommand) -> bool {
        let Some(other_cmd) = other.as_any().downcast_ref::<InsertTextUseCase>() else {
            return false;
        };

        if let (Some(self_dto), Some(other_dto)) = (&mut self.last_dto, &other_cmd.last_dto) {
            self_dto.text.push_str(&other_dto.text);
            self_dto.anchor = self_dto.position;
        }
        if let Some(other_result) = &other_cmd.last_result {
            self.last_result = Some(other_result.clone());
        }
        self.last_merge_time = other_cmd.last_merge_time;

        // Extend the combined insertion length so a single undo reverts
        // all merged keystrokes (not just the first one). The merge
        // criteria in can_merge() guarantee both inserts are contiguous
        // and in the same block, so widening the existing
        // (inserted_byte_offset, inserted_byte_len) span by the other's
        // length captures the combined effect.
        if let (Some(InsertTextUndo::Simple(self_undo)), Some(InsertTextUndo::Simple(other_undo))) =
            (&mut self.undo_data, &other_cmd.undo_data)
        {
            self_undo.inserted_byte_len += other_undo.inserted_byte_len;
        }

        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
