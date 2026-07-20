use crate::MergeTextFormatDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{block_char_length, block_char_to_byte_in_block};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::entities::{Block, Document, Frame, Root};
use common::format_runs::{
    CharacterFormat, FormatRun, capture_image_formats_in_range, capture_runs_in_range,
    debug_assert_well_formed, splice_range,
};
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait MergeTextFormatUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn MergeTextFormatUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "GetRelationship")]
#[macros::uow_action(entity = "Frame", action = "Get")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "Get")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "GetRelationship")]
pub trait MergeTextFormatUnitOfWorkTrait: CommandUnitOfWork {}

/// Per-block captured state for hand-rolled undo. Built during the
/// mutation pass; consumed by `undo()` to restore the prior state.
#[derive(Clone, Debug)]
struct BlockFormatInverse {
    block_id: EntityId,
    byte_range: (u32, u32),
    prior_runs: Vec<FormatRun>,
    prior_image_formats: Vec<(u32, CharacterFormat)>,
}

/// Apply the merge dto onto a CharacterFormat, overwriting only fields the
/// dto sets to `Some(_)`. Non-empty `font_family` follows the original
/// semantic (empty string was treated as "no change" by the legacy code).
fn merge_dto(base: &CharacterFormat, dto: &MergeTextFormatDto) -> CharacterFormat {
    let mut out = base.clone();
    if let Some(ref family) = dto.font_family
        && !family.is_empty()
    {
        out.font_family = Some(family.clone());
    }
    if let Some(v) = dto.font_bold {
        out.font_bold = Some(v);
    }
    if let Some(v) = dto.font_italic {
        out.font_italic = Some(v);
    }
    if let Some(v) = dto.font_underline {
        out.font_underline = Some(v);
    }
    if let Some(v) = dto.font_strikeout {
        out.font_strikeout = Some(v);
    }
    if let Some(ref v) = dto.vertical_alignment {
        out.vertical_alignment =
            Some(crate::use_cases::set_text_format_uc::vertical_alignment_to_entity(v));
    }
    out
}

fn build_replacement_runs(
    existing_runs: &[FormatRun],
    byte_start: u32,
    byte_end: u32,
    dto: &MergeTextFormatDto,
) -> Vec<FormatRun> {
    let mut out: Vec<FormatRun> = Vec::new();
    let mut cursor = byte_start;
    for run in existing_runs {
        if run.byte_end <= byte_start || run.byte_start >= byte_end {
            continue;
        }
        let overlap_start = std::cmp::max(run.byte_start, byte_start);
        let overlap_end = std::cmp::min(run.byte_end, byte_end);
        if overlap_start > cursor {
            out.push(FormatRun {
                byte_start: cursor,
                byte_end: overlap_start,
                format: merge_dto(&CharacterFormat::default(), dto),
            });
        }
        out.push(FormatRun {
            byte_start: overlap_start,
            byte_end: overlap_end,
            format: merge_dto(&run.format, dto),
        });
        cursor = overlap_end;
    }
    if cursor < byte_end {
        out.push(FormatRun {
            byte_start: cursor,
            byte_end,
            format: merge_dto(&CharacterFormat::default(), dto),
        });
    }
    out
}

fn execute_merge_text_format(
    uow: &mut Box<dyn MergeTextFormatUnitOfWorkTrait>,
    dto: &MergeTextFormatDto,
) -> Result<Vec<BlockFormatInverse>> {
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

    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;

    let mut all_block_ids = Vec::new();
    for fid in &frame_ids {
        let block_ids = uow.get_frame_relationship(fid, &FrameRelationshipField::Blocks)?;
        all_block_ids.extend(block_ids);
    }

    let blocks_opt = uow.get_block_multi(&all_block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
    blocks.sort_by_key(|b| b.document_position);

    let range_start = std::cmp::min(dto.position, dto.anchor);
    let range_end = std::cmp::max(dto.position, dto.anchor);

    let mut inverse: Vec<BlockFormatInverse> = Vec::new();

    if range_start == range_end {
        return Ok(inverse);
    }

    let store = uow.store();
    for block in &blocks {
        let block_start = block.document_position;
        let block_end = block_start + block_char_length(block, &store);

        if block_end <= range_start || block_start >= range_end {
            continue;
        }

        let local_char_start = std::cmp::max(0, range_start - block_start) as usize;
        let local_char_end =
            std::cmp::min(block_char_length(block, &store), range_end - block_start) as usize;

        // Rope-native char->byte translation (clamps internally).
        let (byte_start, content_byte_len) =
            block_char_to_byte_in_block(&store, block.id, local_char_start);
        let (byte_end, _) = block_char_to_byte_in_block(&store, block.id, local_char_end);

        if byte_start >= byte_end {
            continue;
        }

        // Capture prior state before mutation.
        let prior_runs = {
            let runs_map = store.format_runs.read();
            runs_map
                .get(&block.id)
                .map(|runs| capture_runs_in_range(runs, byte_start, byte_end))
                .unwrap_or_default()
        };
        let prior_image_formats = {
            let images_map = store.block_images.read();
            images_map
                .get(&block.id)
                .map(|images| capture_image_formats_in_range(images, byte_start, byte_end))
                .unwrap_or_default()
        };

        {
            let mut runs_map = store.format_runs.write();
            let runs = runs_map.entry(block.id).or_default();
            let replacement = build_replacement_runs(runs, byte_start, byte_end, dto);
            splice_range(runs, byte_start..byte_end, replacement);
            debug_assert_well_formed(runs, content_byte_len);
        }

        // Update image anchor formats inside the selected byte range.
        {
            let mut images_map = store.block_images.write();
            if let Some(images) = images_map.get_mut(&block.id) {
                for img in images.iter_mut() {
                    if img.byte_offset >= byte_start && img.byte_offset < byte_end {
                        img.format = merge_dto(&img.format, dto);
                    }
                }
            }
        }

        inverse.push(BlockFormatInverse {
            block_id: block.id,
            byte_range: (byte_start, byte_end),
            prior_runs,
            prior_image_formats,
        });
    }

    Ok(inverse)
}

/// Restore the prior format-run and image-format state captured during
/// the forward mutation.
fn apply_inverse(
    uow: &mut Box<dyn MergeTextFormatUnitOfWorkTrait>,
    inverse: &[BlockFormatInverse],
) -> Result<()> {
    let store = uow.store();
    for entry in inverse {
        {
            let mut runs_map = store.format_runs.write();
            let runs = runs_map.entry(entry.block_id).or_default();
            splice_range(
                runs,
                entry.byte_range.0..entry.byte_range.1,
                entry.prior_runs.clone(),
            );
        }
        {
            let mut images_map = store.block_images.write();
            if let Some(images) = images_map.get_mut(&entry.block_id) {
                for (byte_offset, format) in &entry.prior_image_formats {
                    if let Some(img) = images.iter_mut().find(|i| i.byte_offset == *byte_offset) {
                        img.format = format.clone();
                    }
                }
            }
        }
    }
    Ok(())
}

pub struct MergeTextFormatUseCase {
    uow_factory: Box<dyn MergeTextFormatUnitOfWorkFactoryTrait>,
    inverse: Option<Vec<BlockFormatInverse>>,
    last_dto: Option<MergeTextFormatDto>,
}

impl MergeTextFormatUseCase {
    pub fn new(uow_factory: Box<dyn MergeTextFormatUnitOfWorkFactoryTrait>) -> Self {
        MergeTextFormatUseCase {
            uow_factory,
            inverse: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &MergeTextFormatDto) -> Result<()> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let inverse = execute_merge_text_format(&mut uow, dto)?;
        self.inverse = Some(inverse);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(())
    }
}

impl UndoRedoCommand for MergeTextFormatUseCase {
    fn undo(&mut self) -> Result<()> {
        let inverse = self
            .inverse
            .as_ref()
            .ok_or_else(|| anyhow!("No inverse data available for undo"))?
            .clone();

        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;
        apply_inverse(&mut uow, &inverse)?;
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
        let inverse = execute_merge_text_format(&mut uow, &dto)?;
        self.inverse = Some(inverse);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
