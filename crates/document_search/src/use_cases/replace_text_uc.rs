use super::search_helpers::{build_full_text_via_store, find_all_matches};
use crate::ReplaceResultDto;
use crate::ReplaceTextDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::block_char_length;
use common::database::rope_helpers::rope_flat_text_if_simple;
use common::database::rope_helpers::{
    block_content_via_store, rope_delete_in_block, rope_insert_in_block,
};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::entities::{Block, Document, Frame, Root};
use common::format_runs::{
    debug_assert_well_formed, logical_offset_to_byte, shift_images_for_delete,
    shift_images_for_insert, shift_runs_for_delete, shift_runs_for_insert,
};

use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait ReplaceTextUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn ReplaceTextUnitOfWorkTrait>;
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
pub trait ReplaceTextUnitOfWorkTrait: CommandUnitOfWork {}

fn fetch_blocks_and_build_text(
    uow: &dyn ReplaceTextUnitOfWorkTrait,
) -> Result<(String, Vec<Block>)> {
    let root = uow
        .get_root(&ROOT_ENTITY_ID)?
        .ok_or_else(|| anyhow!("Root entity not found"))?;

    let doc_ids = uow.get_root_relationship(&root.id, &RootRelationshipField::Document)?;
    let doc_id = *doc_ids
        .first()
        .ok_or_else(|| anyhow!("Root has no document"))?;

    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;

    let mut all_block_ids: Vec<EntityId> = Vec::new();
    for frame_id in &frame_ids {
        let block_ids = uow.get_frame_relationship(frame_id, &FrameRelationshipField::Blocks)?;
        all_block_ids.extend(block_ids);
    }

    let blocks_opt = uow.get_block_multi(&all_block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
    blocks.sort_by_key(|b| b.document_position);

    // Fast path: flat single-frame doc — rope contents == full plain text.
    let full_text = rope_flat_text_if_simple(&uow.store(), frame_ids.len())
        .unwrap_or_else(|| build_full_text_via_store(&blocks, &uow.store()));

    Ok((full_text, blocks))
}

fn find_block_for_position(
    blocks: &[Block],
    position: usize,
    store: &common::database::Store,
) -> Option<(usize, usize)> {
    for (i, block) in blocks.iter().enumerate() {
        let block_start = block.document_position as usize;
        let block_end = block_start + block_char_length(block, store) as usize;
        if position >= block_start && position < block_end {
            let offset = position - block_start;
            return Some((i, offset));
        }
    }
    None
}

fn match_in_single_block(
    blocks: &[Block],
    match_pos: usize,
    match_len: usize,
    store: &common::database::Store,
) -> Option<(usize, usize)> {
    if let Some((block_idx, offset)) = find_block_for_position(blocks, match_pos, store) {
        let block = &blocks[block_idx];
        let block_end_offset = block_char_length(block, store) as usize;
        if offset + match_len <= block_end_offset {
            return Some((block_idx, offset));
        }
    }
    None
}

/// Replace a logical char range `[char_start..char_end)` inside one
/// block with `replacement`. Mutates plain_text, format_runs (clearing
/// the deleted range, then re-shifting + leaving the replacement bytes
/// uncovered = default-format inheritance per Qt), block_images, the
/// block entity, and reverse-syncs inline_elements.
fn replace_in_block(
    uow: &mut Box<dyn ReplaceTextUnitOfWorkTrait>,
    block: &Block,
    char_start: usize,
    char_end: usize,
    replacement: &str,
) -> Result<()> {
    let store = uow.store();
    let images_before = store
        .block_images
        .read()
        .get(&block.id)
        .cloned()
        .unwrap_or_default();
    let block_text = block_content_via_store(block, &store);

    let byte_start = logical_offset_to_byte(&block_text, &images_before, char_start as i64);
    let byte_end = logical_offset_to_byte(&block_text, &images_before, char_end as i64);

    // First splice the existing range out, then insert the replacement.
    let mut new_plain = String::with_capacity(
        block_text.len() - (byte_end - byte_start) as usize + replacement.len(),
    );
    new_plain.push_str(&block_text[..byte_start as usize]);
    new_plain.push_str(replacement);
    new_plain.push_str(&block_text[byte_end as usize..]);

    let inserted_byte_len = replacement.len() as u32;
    let _replacement_char_len = replacement.chars().count() as i64;
    let _removed_text_chars = block_text[byte_start as usize..byte_end as usize]
        .chars()
        .count() as i64;

    // Mutate format_runs: first delete the range, then make room for the
    // insertion (no run carries formatting — replacement inherits
    // surrounding format via shift_runs_for_insert).
    {
        let mut runs_map = store.format_runs.write();
        let runs = runs_map.entry(block.id).or_default();
        shift_runs_for_delete(runs, byte_start, byte_end);
        // After delete, the trailing runs sit at the (byte_start) cursor.
        // shift_runs_for_insert at byte_start expands the run whose
        // byte_end == byte_start (Qt convention).
        shift_runs_for_insert(runs, byte_start, inserted_byte_len);
        debug_assert_well_formed(runs, new_plain.len());
    }
    let _images_removed = {
        let mut images_map = store.block_images.write();
        let images = images_map.entry(block.id).or_default();
        let removed = shift_images_for_delete(images, byte_start, byte_end);
        shift_images_for_insert(images, byte_start, inserted_byte_len);
        removed as i64
    };

    let mut updated_block = block.clone();
    updated_block.updated_at = chrono::Utc::now();
    uow.update_block(&updated_block)?;

    // Mirror the in-block splice into the global rope: delete the old
    // byte range, then insert the replacement at the same position.
    // No-op under default backend.
    rope_delete_in_block(&store, block.id, byte_start as u32, byte_end as u32);
    rope_insert_in_block(&store, block.id, byte_start as u32, replacement);

    Ok(())
}

fn execute_replace(
    uow: &mut Box<dyn ReplaceTextUnitOfWorkTrait>,
    dto: &ReplaceTextDto,
) -> Result<(ReplaceResultDto, EntityTreeSnapshot)> {
    let root = uow
        .get_root(&ROOT_ENTITY_ID)?
        .ok_or_else(|| anyhow!("Root entity not found"))?;
    let doc_ids = uow.get_root_relationship(&root.id, &RootRelationshipField::Document)?;
    let doc_id = *doc_ids
        .first()
        .ok_or_else(|| anyhow!("Root has no document"))?;

    let snapshot = uow.snapshot_document(&[doc_id])?;

    let (full_text, blocks) = fetch_blocks_and_build_text(uow.as_ref())?;

    let all_matches = find_all_matches(
        &full_text,
        &dto.query,
        dto.case_sensitive,
        dto.whole_word,
        dto.use_regex,
    )?;

    if all_matches.is_empty() {
        return Ok((
            ReplaceResultDto {
                replacements_count: 0,
                skipped_cross_block: 0,
            },
            snapshot,
        ));
    }

    let mut valid_matches: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut skipped_cross_block: i64 = 0;
    let store = uow.store();
    for &(match_pos, match_len) in &all_matches {
        if let Some((block_idx, block_offset)) =
            match_in_single_block(&blocks, match_pos, match_len, &store)
        {
            valid_matches.push((match_pos, match_len, block_idx, block_offset));
        } else {
            skipped_cross_block += 1;
        }
    }

    if !dto.replace_all {
        valid_matches.truncate(1);
    }

    if valid_matches.is_empty() {
        return Ok((
            ReplaceResultDto {
                replacements_count: 0,
                skipped_cross_block,
            },
            snapshot,
        ));
    }

    let replacement = &dto.replacement;
    let replacement_char_len = replacement.chars().count() as i64;
    let replacements_count = valid_matches.len() as i64;

    let mut cumulative_delta: i64 = 0;

    for &(_match_pos, match_len, block_idx, block_offset) in valid_matches.iter().rev() {
        let match_char_len = match_len as i64;
        let delta = replacement_char_len - match_char_len;

        let block = uow
            .get_block(&blocks[block_idx].id)?
            .ok_or_else(|| anyhow!("Block not found"))?;

        replace_in_block(
            uow,
            &block,
            block_offset,
            block_offset + match_len,
            replacement,
        )?;

        cumulative_delta += delta;
    }

    let mut all_block_ids: Vec<EntityId> = Vec::new();
    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    for frame_id in &frame_ids {
        let block_ids = uow.get_frame_relationship(frame_id, &FrameRelationshipField::Blocks)?;
        all_block_ids.extend(block_ids);
    }
    let blocks_opt = uow.get_block_multi(&all_block_ids)?;
    let mut current_blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
    current_blocks.sort_by_key(|b| b.document_position);

    let mut delta_by_block: std::collections::HashMap<usize, i64> =
        std::collections::HashMap::new();
    for &(_match_pos, match_len, block_idx, _block_offset) in &valid_matches {
        let delta = replacement_char_len - match_len as i64;
        *delta_by_block.entry(block_idx).or_insert(0) += delta;
    }

    let mut cumulative_shift: i64 = 0;
    let mut blocks_to_update: Vec<Block> = Vec::new();
    for block in current_blocks.iter() {
        let orig_idx = blocks.iter().position(|b| b.id == block.id);

        if let Some(oidx) = orig_idx {
            if let Some(&d) = delta_by_block.get(&oidx) {
                if cumulative_shift != 0 {
                    let mut ub = block.clone();
                    ub.document_position += cumulative_shift;
                    ub.updated_at = chrono::Utc::now();
                    blocks_to_update.push(ub);
                }
                cumulative_shift += d;
            } else if cumulative_shift != 0 {
                let mut ub = block.clone();
                ub.document_position += cumulative_shift;
                ub.updated_at = chrono::Utc::now();
                blocks_to_update.push(ub);
            }
        } else if cumulative_shift != 0 {
            let mut ub = block.clone();
            ub.document_position += cumulative_shift;
            ub.updated_at = chrono::Utc::now();
            blocks_to_update.push(ub);
        }
    }

    if !blocks_to_update.is_empty() {
        uow.update_block_multi(&blocks_to_update)?;
    }

    let total_delta = cumulative_delta;
    let mut document = uow
        .get_document(&doc_id)?
        .ok_or_else(|| anyhow!("Document not found"))?;
    document.character_count += total_delta;
    document.updated_at = chrono::Utc::now();
    uow.update_document(&document)?;

    Ok((
        ReplaceResultDto {
            replacements_count,
            skipped_cross_block,
        },
        snapshot,
    ))
}

pub struct ReplaceTextUseCase {
    uow_factory: Box<dyn ReplaceTextUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<ReplaceTextDto>,
}

impl ReplaceTextUseCase {
    pub fn new(uow_factory: Box<dyn ReplaceTextUnitOfWorkFactoryTrait>) -> Self {
        ReplaceTextUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &ReplaceTextDto) -> Result<ReplaceResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_replace(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for ReplaceTextUseCase {
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
        let (_, snapshot) = execute_replace(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
