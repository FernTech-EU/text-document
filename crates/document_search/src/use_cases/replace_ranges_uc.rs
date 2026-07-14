//! `replace_ranges` — replace an explicit set of ranges, each with its own replacement.
//!
//! ## Why this exists
//!
//! `replace_text` can only put the *same* string at every match. A reviewed bulk rename
//! needs neither of those things: the writer unticks some occurrences, and a rename may
//! have to preserve the case it found (`AURÉLIEN` → `AURÉLIAN`, not `aurélian`). So the
//! caller decides, per occurrence, and hands the whole set over to be applied in one shot.
//!
//! ## Why the caller must not "find, then replace"
//!
//! Doing it in two calls is a race: the document can move between them, and the ranges then
//! address text that is no longer there — which does not fail, it rewrites *the wrong
//! words*. The public API therefore takes the scan and the splice under **one** lock (see
//! `TextDocument::find_and_replace`), and this use case is the second half of that.
//!
//! ## What it refuses
//!
//! It never guesses. A range that straddles a block boundary, or that overlaps one already
//! accepted, is **skipped and reported** — the caller learns exactly what did not happen
//! rather than being handed a silently half-applied edit.

use crate::ReplaceRangesDto;
use crate::ReplaceRangesResultDto;
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

use super::replace_core::{self, RangeSpec};
use super::search_helpers::build_full_text_via_store;

pub trait ReplaceRangesUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn ReplaceRangesUnitOfWorkTrait>;
}

// Exactly the same entities and actions as `replace_text`: this is the same edit, with the
// ranges chosen by the caller instead of by a query.
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
pub trait ReplaceRangesUnitOfWorkTrait: CommandUnitOfWork {}

/// The document's blocks in reading order, and the text a range's offsets address.
///
/// Blocks are pooled across every frame and sorted **once, globally**, by
/// `document_position` — a blockquote's prose lives in a child frame, and sorting per frame
/// would put it in the wrong place (which is exactly the bug `to_plain_text` had).
fn fetch_blocks_and_build_text(
    uow: &dyn ReplaceRangesUnitOfWorkTrait,
) -> Result<(String, Vec<Block>, EntityId)> {
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
        all_block_ids
            .extend(uow.get_frame_relationship(frame_id, &FrameRelationshipField::Blocks)?);
    }

    let mut blocks: Vec<Block> = uow
        .get_block_multi(&all_block_ids)?
        .into_iter()
        .flatten()
        .collect();
    blocks.sort_by_key(|b| b.document_position);

    let full_text = rope_flat_text_if_simple(&uow.store(), frame_ids.len())
        .unwrap_or_else(|| build_full_text_via_store(&blocks, &uow.store()));

    Ok((full_text, blocks, doc_id))
}

/// Apply `specs` through the shared splice (see [`replace_core`]), and persist the result.
///
/// The only part `replace_text` and `replace_ranges` cannot share: each use case has its own
/// unit-of-work trait, so the *plumbing* is written twice. Everything with a decision in it —
/// which ranges are legal, in what order they are spliced, how positions are rebased
/// afterwards — lives once, in `replace_core`.
fn apply_specs(
    uow: &mut Box<dyn ReplaceRangesUnitOfWorkTrait>,
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

/// The three parallel lists are the DTO's shape (a list of structs is not expressible
/// there); this is where they become a typed set again. Mismatched lengths are an error, not
/// something to silently truncate to the shortest — a caller whose lists have drifted apart
/// is about to rewrite the wrong text.
fn specs_from(dto: &ReplaceRangesDto) -> Result<Vec<RangeSpec>> {
    if dto.positions.len() != dto.lengths.len() || dto.positions.len() != dto.replacements.len() {
        return Err(anyhow!(
            "replace_ranges: positions ({}), lengths ({}) and replacements ({}) must be the \
             same length — they are parallel lists describing the same ranges",
            dto.positions.len(),
            dto.lengths.len(),
            dto.replacements.len()
        ));
    }

    let mut specs = Vec::with_capacity(dto.positions.len());
    for i in 0..dto.positions.len() {
        let (position, length) = (dto.positions[i], dto.lengths[i]);
        if position < 0 || length < 0 {
            return Err(anyhow!(
                "replace_ranges: range {i} is {position}..+{length}; offsets are char \
                 positions and cannot be negative"
            ));
        }
        specs.push(RangeSpec {
            position: position as usize,
            length: length as usize,
            replacement: dto.replacements[i].clone(),
        });
    }
    Ok(specs)
}

fn execute_replace_ranges(
    uow: &mut Box<dyn ReplaceRangesUnitOfWorkTrait>,
    dto: &ReplaceRangesDto,
) -> Result<(ReplaceRangesResultDto, EntityTreeSnapshot)> {
    let specs = specs_from(dto)?;

    let (_full_text, blocks, doc_id) = fetch_blocks_and_build_text(uow.as_ref())?;
    let snapshot = uow.snapshot_document(&[doc_id])?;

    let applied = apply_specs(uow, doc_id, &blocks, &specs, dto.format_policy)?;

    Ok((
        ReplaceRangesResultDto {
            replacements_count: applied.replacements_count,
            skipped_cross_block: applied.skipped_cross_block,
            skipped_overlapping: applied.skipped_overlapping,
        },
        snapshot,
    ))
}

pub struct ReplaceRangesUseCase {
    uow_factory: Box<dyn ReplaceRangesUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<ReplaceRangesDto>,
}

impl ReplaceRangesUseCase {
    pub fn new(uow_factory: Box<dyn ReplaceRangesUnitOfWorkFactoryTrait>) -> Self {
        ReplaceRangesUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &ReplaceRangesDto) -> Result<ReplaceRangesResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_replace_ranges(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for ReplaceRangesUseCase {
    /// One undo puts the whole batch back — however many ranges it touched. A bulk rewrite of
    /// someone's prose that could not be taken back in one action is the single most
    /// destructive thing this API offers.
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
        let (_, snapshot) = execute_replace_ranges(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
