use super::replace_core::{self, RangeSpec};
use super::search_helpers::{build_full_text_via_store, find_all_matches};
use crate::ReplaceResultDto;
use crate::ReplaceTextDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::rope_flat_text_if_simple;
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::entities::{Block, Document, Frame, Root};
use common::format_runs::ReplaceFormatPolicy;

use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;
use std::collections::HashMap;

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

/// Find every match and replace it with the same string — which is all `replace_text` can
/// express. The splice itself is [`replace_core`], shared with `replace_ranges`.
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

    let mut matches = find_all_matches(
        &full_text,
        &dto.query,
        dto.case_sensitive,
        dto.whole_word,
        dto.use_regex,
    )?;
    if !dto.replace_all {
        matches.truncate(1);
    }

    // The same replacement at every match — which is all `replace_text` can express. A
    // caller that needs a different replacement per occurrence (a rename that preserves the
    // case it found), or that needs to skip some, uses `replace_ranges` instead. Both go
    // through the SAME splice: see `replace_core`.
    let specs: Vec<RangeSpec> = matches
        .iter()
        .map(|&(position, length)| RangeSpec {
            position,
            length,
            replacement: dto.replacement.clone(),
        })
        .collect();

    let applied = apply_specs(uow, doc_id, &blocks, &specs, dto.format_policy)?;

    Ok((
        ReplaceResultDto {
            replacements_count: applied.replacements_count,
            skipped_cross_block: applied.skipped_cross_block,
        },
        snapshot,
    ))
}

/// Apply `specs` through the shared splice (see [`replace_core`]), and persist the result.
///
/// This is the only part `replace_text` and `replace_ranges` cannot share: each use case
/// has its own unit-of-work trait, so the *plumbing* is written twice. Everything with a
/// decision in it — which ranges are legal, in what order they are spliced, how positions
/// are rebased afterwards — lives once, in `replace_core`.
fn apply_specs(
    uow: &mut Box<dyn ReplaceTextUnitOfWorkTrait>,
    doc_id: EntityId,
    blocks: &[Block],
    specs: &[RangeSpec],
    policy: ReplaceFormatPolicy,
) -> Result<replace_core::Applied> {
    let store = uow.store();
    let plan = replace_core::plan(blocks, specs, &store);

    if plan.edits.is_empty() {
        return Ok(replace_core::Applied {
            replacements_count: 0,
            skipped_cross_block: plan.skipped_cross_block,
            skipped_overlapping: plan.skipped_overlapping,
        });
    }

    // DESCENDING. An earlier edit's length change must not move a range a later edit still
    // has to address.
    let mut delta_by_block_id: HashMap<EntityId, i64> = HashMap::new();
    for edit in plan.edits.iter().rev() {
        let block = uow
            .get_block(&blocks[edit.block_idx].id)?
            .ok_or_else(|| anyhow!("Block not found"))?;

        let updated = replace_core::apply_in_block(
            &store,
            &block,
            edit.block_offset,
            edit.block_offset + edit.length,
            &edit.replacement,
            policy,
        )?;
        uow.update_block(&updated)?;

        *delta_by_block_id.entry(block.id).or_insert(0) += replace_core::char_delta(edit);
    }

    let (moved, total_delta) = replace_core::rebase_positions(blocks, &delta_by_block_id);
    if !moved.is_empty() {
        uow.update_block_multi(&moved)?;
    }

    let mut document = uow
        .get_document(&doc_id)?
        .ok_or_else(|| anyhow!("Document not found"))?;
    document.character_count += total_delta;
    document.updated_at = chrono::Utc::now();
    uow.update_document(&document)?;

    Ok(replace_core::Applied {
        replacements_count: plan.edits.len() as i64,
        skipped_cross_block: plan.skipped_cross_block,
        skipped_overlapping: plan.skipped_overlapping,
    })
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
