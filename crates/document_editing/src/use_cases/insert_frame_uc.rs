use crate::InsertFrameDto;
use crate::InsertFrameResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::block_offset_index::OffsetMarker;
use common::database::rope_helpers::block_char_length;
use common::database::rope_helpers::{rope_append_empty_block, rope_insert_run_after};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::entities::{Block, Document, Frame, Root};
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait InsertFrameUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn InsertFrameUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "Update")]
#[macros::uow_action(entity = "Document", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Snapshot")]
#[macros::uow_action(entity = "Document", action = "Restore")]
#[macros::uow_action(entity = "Frame", action = "Get")]
#[macros::uow_action(entity = "Frame", action = "Create")]
#[macros::uow_action(entity = "Frame", action = "Update")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "Create")]
pub trait InsertFrameUnitOfWorkTrait: CommandUnitOfWork {}

pub struct InsertFrameUseCase {
    uow_factory: Box<dyn InsertFrameUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<InsertFrameDto>,
}

/// Find which frame contains the given document position by walking
/// frames -> blocks and checking document_position ranges.
/// Returns the frame and the id of its block closest to position.
fn find_frame_at_position(
    uow: &dyn InsertFrameUnitOfWorkTrait,
    frame_ids: &[EntityId],
    position: i64,
) -> Result<Option<(Frame, EntityId)>> {
    let store = uow.store();
    for frame_id in frame_ids {
        let frame = match uow.get_frame(frame_id)? {
            Some(f) => f,
            None => continue,
        };
        let block_ids = uow.get_frame_relationship(frame_id, &FrameRelationshipField::Blocks)?;
        if block_ids.is_empty() {
            continue;
        }
        let blocks_opt = uow.get_block_multi(&block_ids)?;
        let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
        // The stored field lags the rope by whatever was typed since the last deletion, and the
        // caller's positions are rope positions: read the rope's.
        common::database::rope_helpers::refresh_block_positions(&mut blocks, &store);
        blocks.sort_by_key(|b| b.document_position);

        if let (Some(first), Some(last)) = (blocks.first(), blocks.last()) {
            let frame_start = first.document_position;
            let frame_end = last.document_position + block_char_length(last, &store);
            if position >= frame_start && position <= frame_end {
                // Find block index closest to position
                let mut block_idx = 0;
                for (i, block) in blocks.iter().enumerate() {
                    if position <= block.document_position + block_char_length(block, &store) {
                        block_idx = i;
                        break;
                    }
                    block_idx = i;
                }
                return Ok(Some((frame, blocks[block_idx].id)));
            }
        }
    }
    Ok(None)
}

fn execute_insert_frame(
    uow: &mut Box<dyn InsertFrameUnitOfWorkTrait>,
    dto: &InsertFrameDto,
) -> Result<(InsertFrameResultDto, EntityTreeSnapshot)> {
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

    // Snapshot for undo before mutation
    let snapshot = uow.snapshot_document(&[doc_id])?;

    let now = chrono::Utc::now();

    // Determine the parent frame from the position
    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;

    let (parent_frame_id, child_order_insert_idx, host_block) =
        match find_frame_at_position(&**uow, &frame_ids, dto.position)? {
            Some((parent_frame, block_id)) => {
                // Insert the new sub-frame into the parent's child_order
                // right after that block. Its index among the frame's blocks
                // alone put the sub-frame earlier whenever another sub-frame
                // came before the block.
                let insert_idx = parent_frame
                    .child_order
                    .iter()
                    .position(|entry| *entry == block_id as i64)
                    .map_or(parent_frame.child_order.len(), |i| i + 1);
                (Some(parent_frame.id), insert_idx, Some(block_id))
            }
            None => {
                // Position doesn't fall in any frame — append as top-level
                (None, 0, None)
            }
        };

    // Create a new Frame with parent reference
    let new_frame = Frame {
        id: 0,
        created_at: now,
        updated_at: now,
        parent_frame: parent_frame_id,
        blocks: vec![],
        child_order: vec![],
        fmt_height: None,
        fmt_width: None,
        fmt_top_margin: None,
        fmt_bottom_margin: None,
        fmt_left_margin: None,
        fmt_right_margin: None,
        fmt_padding: None,
        fmt_border: None,
        fmt_position: None,
        fmt_is_blockquote: None,
        fmt_semantic_role: None,
        table: None,
        byte_range: (0, 0),
        footnote_label: None,
    };

    let created_frame = uow.create_frame(&new_frame, doc_id, -1)?;

    // Create an empty block inside the new frame.
    // Set document_position to the insertion point so it sorts correctly
    // for operations that use stored positions (delete_text, insert_block, etc.)
    let new_block = Block {
        id: 0,
        created_at: now,
        updated_at: now,
        list: None,
        document_position: dto.position,
        ..Default::default()
    };

    let created_block = uow.create_block(&new_block, created_frame.id, -1)?;

    // Update the new frame's child_order with its block
    let mut updated_new_frame = created_frame.clone();
    updated_new_frame.child_order = vec![created_block.id as i64];
    updated_new_frame.updated_at = now;
    uow.update_frame(&updated_new_frame)?;

    // If there's a parent frame, insert the new frame into its child_order
    if let Some(parent_id) = parent_frame_id {
        let parent_frame = uow
            .get_frame(&parent_id)?
            .ok_or_else(|| anyhow!("Parent frame not found"))?;
        let mut updated_parent = parent_frame.clone();
        let idx = child_order_insert_idx.min(updated_parent.child_order.len());
        // Use negative IDs to distinguish sub-frame references from block IDs
        // Convention: positive = block ID, negative = -(frame ID)
        updated_parent
            .child_order
            .insert(idx, -(created_frame.id as i64));
        updated_parent.updated_at = now;
        uow.update_frame(&updated_parent)?;
    }

    // Update Document (increment block_count for the new block)
    let mut updated_doc = document.clone();
    updated_doc.block_count += 1;
    updated_doc.updated_at = now;
    uow.update_document(&updated_doc)?;

    // Mirror to rope so every block stays indexed and
    // `find_block_at_char_position` keeps using its O(log n) fast path.
    // Frame.byte_range is recomputed centrally in Transaction::commit.
    //
    // - Top-level (parent = None) frame: append at the rope end with a
    //   `\n` boundary.
    // - Nested (parent = Some) frame: insert the new empty block right
    //   after the current block at `dto.position`. The byte position
    //   chosen is the end of the current block's content (= the byte
    //   index of its trailing `\n`, or the rope end if it has no
    //   successor). The new sub-frame's negative entry in parent's
    //   `child_order` is placed at `block_idx + 1` (above), so the
    //   rope order matches the entity order. If the rope is currently
    //   inconsistent (an older unmirrored sub-frame already disabled
    //   the fast path), fall back to `rope_append_empty_block` to at
    //   least register the new block in the index — that closes the
    //   gap going forward without trying to retrofit the past.
    //
    // The block the new frame follows is the one it follows in the parent's
    // `child_order`. It used to be looked up in the rope by position after the
    // new block had been created, when the index no longer held every block,
    // so the lookup always failed and every new sub-frame went to the end of
    // the rope, whatever it followed in the frames.
    if parent_frame_id.is_some() {
        let store = uow.store();
        let inserted = host_block.is_some_and(|block_id| {
            rope_insert_run_after(
                &store,
                OffsetMarker::Block(block_id),
                &[(OffsetMarker::Block(created_block.id), "")],
            )
        });
        if !inserted {
            rope_append_empty_block(&store, created_block.id);
        }
    } else {
        rope_append_empty_block(&uow.store(), created_block.id);
    }

    Ok((
        InsertFrameResultDto {
            frame_id: created_frame.id as i64,
        },
        snapshot,
    ))
}

impl InsertFrameUseCase {
    pub fn new(uow_factory: Box<dyn InsertFrameUnitOfWorkFactoryTrait>) -> Self {
        InsertFrameUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(&mut self, dto: &InsertFrameDto) -> Result<InsertFrameResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_insert_frame(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for InsertFrameUseCase {
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
        let (_, snapshot) = execute_insert_frame(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
