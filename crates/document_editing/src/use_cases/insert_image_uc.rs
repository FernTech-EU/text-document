use super::editing_helpers::{collect_block_ids_recursive, find_block_at_position};
use crate::InsertImageDto;
use crate::InsertImageResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{block_content_via_store, rope_insert_in_block};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::direct_access::table::TableRelationshipField;
use common::entities::{Block, Document, Frame, Root, TableCell};
use common::format_runs::{
    ImageAnchor, logical_offset_to_byte, shift_images_for_insert, shift_runs_for_insert,
    synth_element_id,
};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait InsertImageUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn InsertImageUnitOfWorkTrait>;
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
pub trait InsertImageUnitOfWorkTrait: CommandUnitOfWork {}

pub struct InsertImageUseCase {
    uow_factory: Box<dyn InsertImageUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<InsertImageDto>,
}

fn execute_insert_image(
    uow: &mut Box<dyn InsertImageUnitOfWorkTrait>,
    dto: &InsertImageDto,
) -> Result<(InsertImageResultDto, EntityTreeSnapshot)> {
    if dto.position != dto.anchor {
        return Err(anyhow!(
            "Selection replacement is not supported for image insertion"
        ));
    }

    if dto.width <= 0 || dto.height <= 0 {
        return Err(anyhow!(
            "Image dimensions must be positive (got {}x{})",
            dto.width,
            dto.height
        ));
    }

    if dto.quality < 0 || dto.quality > 100 {
        return Err(anyhow!(
            "Image quality must be within 1..=100 (got {})",
            dto.quality
        ));
    }

    let position = dto.position;

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

    // Snapshot for undo before mutation (covers blocks, block_images, format_runs, document).
    let snapshot = uow.snapshot_document(&[doc_id])?;

    // Get frames
    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;

    // Blocks in linear order, recursing into sub-frames.
    //
    // Reading only the top-level frame's blocks — which this did — mislocates
    // any insertion inside a blockquote or a table cell: those blocks live in
    // sub-frames, so `find_block_at_position` cannot match one and falls
    // through to a scan that resolves the wrong block, or appends to the last
    // top-level one. `insert_text`, `insert_block` and `delete_text` all
    // already collect recursively; this is the same walk.
    let get_table_cell_frames = |table_id: &EntityId| -> Result<Vec<EntityId>> {
        let cell_ids = uow.get_table_relationship(table_id, &TableRelationshipField::Cells)?;
        let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
        let mut cells: Vec<TableCell> = cells_opt.into_iter().flatten().collect();
        cells.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));
        Ok(cells.into_iter().filter_map(|c| c.cell_frame).collect())
    };
    let block_ids = collect_block_ids_recursive(
        &|id| uow.get_frame(id),
        &|id, field| uow.get_frame_relationship(id, field),
        &get_table_cell_frames,
        &frame_id,
    )?;

    // Get all blocks
    let blocks_opt = uow.get_block_multi(&block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
    blocks.sort_by_key(|b| b.document_position);

    // Find block at position
    let (block, block_idx, offset) = find_block_at_position(&blocks, position, &uow.store())?;

    // byte_offset = position inside the block's text where the new image is
    // anchored.
    //
    // This is the same char→byte mapping every other editing use case uses, and
    // for the same reason: an image occupies one character *and* the three
    // bytes of its `U+FFFC` sentinel. The previous version walked the
    // synthesized segments and added up only the `Text` ones, treating an image
    // as zero bytes — so inserting an image after an existing image anchored it
    // three bytes short of where it belonged, once per preceding image.
    let block_text = block_content_via_store(&block, &uow.store());
    let images_before = {
        let store = uow.store();
        let map = store.block_images.read();
        map.get(&block.id).cloned().unwrap_or_default()
    };
    let byte_offset: u32 = logical_offset_to_byte(&block_text, &images_before, offset);

    let now = chrono::Utc::now();

    // The sentinel mirrored into the rope below occupies real bytes, so
    // everything anchored past the insertion point has to move by exactly that
    // much — the same shift any text insertion performs.
    //
    // This was missing. `rope_insert_in_block` shifts *block* offsets, not the
    // per-block format runs and image anchors, so inserting an image before
    // existing formatting or before another image left those pointing three
    // bytes short. Two images in one paragraph was enough to reproduce it: the
    // second anchor still referenced the first image's sentinel, and the
    // paragraph read back with a doubled image and a lost character.
    const SENTINEL_BYTES: u32 = '\u{FFFC}'.len_utf8() as u32;

    // Insert ImageAnchor directly into block_images, maintaining sort order
    // (ascending by byte_offset; equal byte_offsets keep insertion order, so
    // the new image goes AFTER any existing anchors at the same byte position).
    {
        let store = uow.store();
        let mut images_map = store.block_images.write();
        let images = images_map.entry(block.id).or_default();
        shift_images_for_insert(images, byte_offset, SENTINEL_BYTES);
        let insert_idx = images
            .iter()
            .position(|a| a.byte_offset > byte_offset)
            .unwrap_or(images.len());
        images.insert(
            insert_idx,
            ImageAnchor {
                byte_offset,
                name: dto.image_name.clone(),
                alt: dto.alt.clone(),
                width: dto.width,
                height: dto.height,
                quality: if dto.quality == 0 { 100 } else { dto.quality },
                format: Default::default(),
            },
        );
    }

    {
        let store = uow.store();
        let mut runs_map = store.format_runs.write();
        if let Some(runs) = runs_map.get_mut(&block.id) {
            shift_runs_for_insert(runs, byte_offset, SENTINEL_BYTES);
        }
    }

    // Mirror to the global rope: insert U+FFFC OBJECT REPLACEMENT
    // CHARACTER at the same byte offset per plan §1.6. The sentinel is
    // 3 UTF-8 bytes but contributes 1 logical position.
    rope_insert_in_block(&uow.store(), block.id, byte_offset, "\u{FFFC}");

    let mut updated_block = block.clone();
    updated_block.updated_at = now;
    uow.update_block(&updated_block)?;

    // Shift subsequent blocks' document_position by +1. NOT gated on
    // `rope_positions_match_flow` (unlike insert_text_uc): this use
    // case locates the edit via `find_block_at_position`, which reads
    // the stored `document_position`, so the field must stay accurate
    // here. Gating would require an O(N) catch-up that cancels the
    // saving — net zero. Migrating the lookup to the rope-based
    // `find_block_at_char_position` (Cause B) is the prerequisite for
    // gating this loop.
    let mut blocks_to_update: Vec<Block> = Vec::new();
    for b in &blocks[(block_idx + 1)..] {
        let mut ub = b.clone();
        ub.document_position += 1;
        ub.updated_at = now;
        blocks_to_update.push(ub);
    }
    if !blocks_to_update.is_empty() {
        uow.update_block_multi(&blocks_to_update)?;
    }

    let mut updated_doc = document.clone();
    updated_doc.character_count += 1;
    updated_doc.updated_at = now;
    uow.update_document(&updated_doc)?;

    Ok((
        InsertImageResultDto {
            new_position: position + 1,
            element_id: synth_element_id(block.id, byte_offset) as i64,
        },
        snapshot,
    ))
}

impl InsertImageUseCase {
    pub fn new(uow_factory: Box<dyn InsertImageUnitOfWorkFactoryTrait>) -> Self {
        InsertImageUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &InsertImageDto) -> Result<InsertImageResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_insert_image(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for InsertImageUseCase {
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
        let (_, snapshot) = execute_insert_image(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
