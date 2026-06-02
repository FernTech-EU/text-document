use super::editing_helpers::collect_block_ids_recursive;
use crate::InsertFormattedTextDto;
use crate::InsertFormattedTextResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{
    block_char_length, block_content_via_store, rope_delete_in_block, rope_insert_in_block,
};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::direct_access::table::TableRelationshipField;
use common::entities::{Block, Document, Frame, Root, TableCell};
use common::format_runs::{
    CharacterFormat, FormatRun, ImageAnchor, debug_assert_well_formed, logical_offset_to_byte,
    shift_images_for_delete, shift_images_for_insert, shift_runs_for_delete, shift_runs_for_insert,
    splice_range,
};

use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait InsertFormattedTextUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn InsertFormattedTextUnitOfWorkTrait>;
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
#[macros::uow_action(entity = "Block", action = "SetRelationship")]
#[macros::uow_action(entity = "Table", action = "GetRelationship")]
#[macros::uow_action(entity = "TableCell", action = "GetMulti")]
pub trait InsertFormattedTextUnitOfWorkTrait: CommandUnitOfWork {}

/// Lightweight undo data for the simple (no-selection) path.
struct SimpleUndoData {
    block_id: EntityId,
    original_block: Block,
    original_format_runs: Vec<FormatRun>,
    original_block_images: Vec<ImageAnchor>,
    doc_id: EntityId,
    original_character_count: i64,
    /// Byte offset inside the block where the new text was inserted.
    inserted_byte_offset: u32,
    /// Number of bytes inserted into the rope by this command.
    inserted_byte_len: u32,
}

enum InsertFormattedTextUndo {
    Simple(Box<SimpleUndoData>),
    SelectionReplacement(common::snapshot::EntityTreeSnapshot),
}

pub struct InsertFormattedTextUseCase {
    uow_factory: Box<dyn InsertFormattedTextUnitOfWorkFactoryTrait>,
    undo_data: Option<InsertFormattedTextUndo>,
    last_dto: Option<InsertFormattedTextDto>,
}

fn dto_to_character_format(dto: &InsertFormattedTextDto) -> CharacterFormat {
    CharacterFormat {
        font_family: Some(dto.font_family.clone()),
        font_point_size: Some(dto.font_point_size),
        font_weight: None,
        font_bold: Some(dto.font_bold),
        font_italic: Some(dto.font_italic),
        font_underline: Some(dto.font_underline),
        font_overline: None,
        font_strikeout: Some(dto.font_strikeout),
        letter_spacing: None,
        word_spacing: None,
        anchor_href: None,
        anchor_names: Vec::new(),
        is_anchor: None,
        tooltip: None,
        underline_style: None,
        vertical_alignment: None,
    }
}

/// Delete a logical char range from a single block, mutating
/// plain_text + format_runs + block_images consistently. Returns the
/// count of logical positions removed.
fn delete_range_in_block(
    uow: &mut Box<dyn InsertFormattedTextUnitOfWorkTrait>,
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

    {
        let mut runs_map = store.format_runs.write();
        let runs = runs_map.entry(block.id).or_default();
        shift_runs_for_delete(runs, byte_start, byte_end);
        debug_assert_well_formed(runs, new_plain.len());
    }
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

/// Core mutation: insert `dto.text` at `(block, char_offset)` with the
/// dto's character format, updating plain_text, format_runs, block_images,
/// the block entity, and the legacy inline_elements view (reverse-sync).
fn insert_formatted_at(
    uow: &mut Box<dyn InsertFormattedTextUnitOfWorkTrait>,
    block: &Block,
    char_offset: i64,
    dto: &InsertFormattedTextDto,
) -> Result<(u32, u32)> {
    let store = uow.store();
    let images_before = store
        .block_images
        .read()
        .get(&block.id)
        .cloned()
        .unwrap_or_default();
    let block_text = block_content_via_store(block, &store);
    let byte_offset = logical_offset_to_byte(&block_text, &images_before, char_offset);
    let inserted_byte_len = dto.text.len() as u32;

    let mut new_plain = block_text.clone();
    new_plain.insert_str(byte_offset as usize, &dto.text);

    let mut updated_block = block.clone();
    updated_block.updated_at = chrono::Utc::now();
    uow.update_block(&updated_block)?;

    // Step 1: shift existing runs to make room for the inserted bytes.
    // Step 2: splice the inserted byte range with a single run carrying
    // the dto's format (this overrides whatever the shift would have
    // inherited from a straddling run).
    {
        let mut runs_map = store.format_runs.write();
        let runs = runs_map.entry(block.id).or_default();
        shift_runs_for_insert(runs, byte_offset, inserted_byte_len);
        let new_run = FormatRun {
            byte_start: byte_offset,
            byte_end: byte_offset + inserted_byte_len,
            format: dto_to_character_format(dto),
        };
        splice_range(
            runs,
            byte_offset..(byte_offset + inserted_byte_len),
            vec![new_run],
        );
        debug_assert_well_formed(runs, new_plain.len());
    }
    {
        let mut images_map = store.block_images.write();
        if let Some(images) = images_map.get_mut(&block.id) {
            shift_images_for_insert(images, byte_offset, inserted_byte_len);
        }
    }

    rope_insert_in_block(&store, block.id, byte_offset, &dto.text);

    Ok((byte_offset, inserted_byte_len))
}

fn execute_with_selection(
    uow: &mut Box<dyn InsertFormattedTextUnitOfWorkTrait>,
    dto: &InsertFormattedTextDto,
) -> Result<(InsertFormattedTextResultDto, InsertFormattedTextUndo)> {
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
    let ordered_block_ids = collect_block_ids_recursive(
        &|id| uow.get_frame(id),
        &|id, field| uow.get_frame_relationship(id, field),
        &get_table_cell_frames,
        &frame_id,
    )?;

    if ordered_block_ids.is_empty() {
        return Err(anyhow!("No blocks in document"));
    }

    let sel_start = std::cmp::min(dto.position, dto.anchor);
    let sel_end = std::cmp::max(dto.position, dto.anchor);

    let (sel_block, sel_block_idx, sel_block_pos) =
        find_block_at_position_sequential(&**uow, &ordered_block_ids, sel_start)?;
    let (_end_block, sel_end_block_idx, _) =
        find_block_at_position_sequential(&**uow, &ordered_block_ids, sel_end)?;

    if sel_block_idx != sel_end_block_idx {
        return Err(anyhow!(
            "Cross-block selection replacement is not supported by insert_formatted_text. \
             Use delete_text first, then insert_formatted_text."
        ));
    }

    let start_offset = sel_start - sel_block_pos;
    let end_offset = sel_end - sel_block_pos;

    let chars_removed = delete_range_in_block(uow, &sel_block, start_offset, end_offset)?;

    document.character_count -= chars_removed;
    document.updated_at = chrono::Utc::now();
    uow.update_document(&document)?;

    // Re-read block after deletion (plain_text shrunk).
    let block = uow
        .get_block(&sel_block.id)?
        .ok_or_else(|| anyhow!("Block not found after deletion"))?;

    let _ = insert_formatted_at(uow, &block, start_offset, dto)?;

    let text_len = dto.text.chars().count() as i64;
    let mut updated_doc = uow
        .get_document(&doc_id)?
        .ok_or_else(|| anyhow!("Document not found"))?;
    updated_doc.character_count += text_len;
    updated_doc.updated_at = chrono::Utc::now();
    uow.update_document(&updated_doc)?;

    Ok((
        InsertFormattedTextResultDto {
            new_position: sel_start + text_len,
        },
        InsertFormattedTextUndo::SelectionReplacement(snapshot),
    ))
}

fn execute_insert_simple(
    uow: &mut Box<dyn InsertFormattedTextUnitOfWorkTrait>,
    dto: &InsertFormattedTextDto,
) -> Result<(InsertFormattedTextResultDto, InsertFormattedTextUndo)> {
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
    let store = uow.store();
    let offset = (position - block_pos).clamp(0, block_char_length(&block, &store));

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

    let (inserted_byte_offset, inserted_byte_len) = insert_formatted_at(uow, &block, offset, dto)?;

    let text_len = dto.text.chars().count() as i64;
    let mut updated_doc = document.clone();
    updated_doc.character_count += text_len;
    updated_doc.updated_at = chrono::Utc::now();
    uow.update_document(&updated_doc)?;

    let undo_data = SimpleUndoData {
        block_id: block.id,
        original_block,
        original_format_runs,
        original_block_images,
        doc_id,
        original_character_count: document.character_count,
        inserted_byte_offset,
        inserted_byte_len,
    };

    Ok((
        InsertFormattedTextResultDto {
            new_position: position + text_len,
        },
        InsertFormattedTextUndo::Simple(Box::new(undo_data)),
    ))
}

fn find_block_at_position_sequential(
    uow: &dyn InsertFormattedTextUnitOfWorkTrait,
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

impl InsertFormattedTextUseCase {
    pub fn new(uow_factory: Box<dyn InsertFormattedTextUnitOfWorkFactoryTrait>) -> Self {
        InsertFormattedTextUseCase {
            uow_factory,
            undo_data: None,
            last_dto: None,
        }
    }

    pub fn execute(
        &mut self,
        dto: &InsertFormattedTextDto,
    ) -> Result<InsertFormattedTextResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let has_selection = dto.position != dto.anchor;
        let (result, undo_data) = if has_selection {
            execute_with_selection(&mut uow, dto)?
        } else {
            execute_insert_simple(&mut uow, dto)?
        };
        self.undo_data = Some(undo_data);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for InsertFormattedTextUseCase {
    fn undo(&mut self) -> Result<()> {
        let undo_data = self
            .undo_data
            .take()
            .ok_or_else(|| anyhow!("No undo data available"))?;

        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        match &undo_data {
            InsertFormattedTextUndo::Simple(data) => {
                uow.update_block(&data.original_block)?;
                let store = uow.store();

                // Revert the rope mutation done by the forward path. Must
                // happen BEFORE format_runs/block_images restore so any
                // debug_assert_well_formed inside the rope helper sees a
                // consistent runs-vs-rope state.
                if data.inserted_byte_len > 0 {
                    rope_delete_in_block(
                        &store,
                        data.block_id,
                        data.inserted_byte_offset,
                        data.inserted_byte_offset + data.inserted_byte_len,
                    );
                }

                store
                    .format_runs
                    .write()
                    .insert(data.block_id, data.original_format_runs.clone());
                store
                    .block_images
                    .write()
                    .insert(data.block_id, data.original_block_images.clone());

                let mut doc = uow
                    .get_document(&data.doc_id)?
                    .ok_or_else(|| anyhow!("Document not found"))?;
                doc.character_count = data.original_character_count;
                doc.updated_at = chrono::Utc::now();
                uow.update_document(&doc)?;
            }
            InsertFormattedTextUndo::SelectionReplacement(snapshot) => {
                uow.restore_document(snapshot)?;
            }
        }

        uow.commit()?;
        self.undo_data = Some(undo_data);
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
        let (_, undo_data) = if has_selection {
            execute_with_selection(&mut uow, &dto)?
        } else {
            execute_insert_simple(&mut uow, &dto)?
        };
        self.undo_data = Some(undo_data);

        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
