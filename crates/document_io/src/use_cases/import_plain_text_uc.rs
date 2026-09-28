use crate::ImportPlainTextDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{rope_append_block, rope_insert_block_boundary, rope_reset};
use common::entities::{Block, Document, Frame, Root};

use common::types::{EntityId, ROOT_ENTITY_ID};

pub trait ImportPlainTextUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn ImportPlainTextUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "Update")]
#[macros::uow_action(entity = "Document", action = "GetRelationship")]
#[macros::uow_action(entity = "Frame", action = "Get")]
#[macros::uow_action(entity = "Frame", action = "Create")]
#[macros::uow_action(entity = "Frame", action = "Update")]
#[macros::uow_action(entity = "Frame", action = "RemoveMulti")]
#[macros::uow_action(entity = "Table", action = "RemoveMulti")]
#[macros::uow_action(entity = "List", action = "RemoveMulti")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "CreateMulti")]
pub trait ImportPlainTextUnitOfWorkTrait: CommandUnitOfWork {}

pub struct ImportPlainTextUseCase {
    uow_factory: Box<dyn ImportPlainTextUnitOfWorkFactoryTrait>,
}

impl ImportPlainTextUseCase {
    pub fn new(uow_factory: Box<dyn ImportPlainTextUnitOfWorkFactoryTrait>) -> Self {
        ImportPlainTextUseCase { uow_factory }
    }

    pub fn execute(&mut self, dto: &ImportPlainTextDto) -> Result<()> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let root = uow
            .get_root(&ROOT_ENTITY_ID)?
            .ok_or_else(|| anyhow!("Root entity not found"))?;

        let doc_ids = uow.get_root_relationship(
            &root.id,
            &common::direct_access::root::RootRelationshipField::Document,
        )?;
        let doc_id = *doc_ids
            .first()
            .ok_or_else(|| anyhow!("Root has no associated Document"))?;

        let frame_ids = uow.get_document_relationship(
            &doc_id,
            &common::direct_access::document::DocumentRelationshipField::Frames,
        )?;
        // In one call: each single removal rescans every remaining frame, so
        // removing them one by one is quadratic in the frames being replaced.
        uow.remove_frame_multi(&frame_ids)?;
        // The replaced content's tables (their cells with them) and lists go too:
        // see the Djot importer. `TextDocument::clear` loads through here.
        let table_ids = uow.get_document_relationship(
            &doc_id,
            &common::direct_access::document::DocumentRelationshipField::Tables,
        )?;
        uow.remove_table_multi(&table_ids)?;
        let list_ids = uow.get_document_relationship(
            &doc_id,
            &common::direct_access::document::DocumentRelationshipField::Lists,
        )?;
        uow.remove_list_multi(&list_ids)?;

        let new_frame = Frame::default();
        let created_frame = uow.create_frame(&new_frame, doc_id, -1)?;

        // Reset the rope+block_offsets before appending the new content.
        rope_reset(&uow.store());

        let normalized = dto.plain_text.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<&str> = normalized.split('\n').collect();
        let num_blocks = lines.len() as i64;
        let mut total_chars: i64 = 0;
        let mut document_position: i64 = 0;
        let mut blocks: Vec<Block> = Vec::with_capacity(lines.len());

        for (i, line) in lines.iter().enumerate() {
            let line_chars = line.chars().count() as i64;

            blocks.push(Block {
                document_position,
                ..Block::default()
            });

            total_chars += line_chars;
            document_position += line_chars;
            if i < lines.len() - 1 {
                document_position += 1;
            }
        }

        // Every block in one call. Creating them one at a time appends each to
        // the frame's `blocks`, and every append rewrites, re-validates and
        // re-announces the whole list so far: quadratic in the line count.
        // Ids are handed out in slice order, as the per-line creation did.
        let created_blocks = uow.create_block_multi(&blocks, created_frame.id, -1)?;

        // format_runs / block_images stay empty for plain-text import: an
        // absent or empty run vector means "default format everywhere".
        let mut block_ids: Vec<i64> = Vec::with_capacity(created_blocks.len());
        for (i, (created_block, line)) in created_blocks.iter().zip(&lines).enumerate() {
            // Mirror the block's text into the global rope. Insert an
            // inter-block `\n` before every block after the first.
            if i > 0 {
                rope_insert_block_boundary(&uow.store());
            }
            rope_append_block(&uow.store(), created_block.id, line);
            block_ids.push(created_block.id as i64);
        }

        let mut updated_frame = uow
            .get_frame(&created_frame.id)?
            .ok_or_else(|| anyhow!("Created frame not found"))?;
        updated_frame.child_order = block_ids;
        uow.update_frame(&updated_frame)?;

        let mut updated_doc = uow
            .get_document(&doc_id)?
            .ok_or_else(|| anyhow!("Document not found after import"))?;
        updated_doc.character_count = total_chars;
        updated_doc.block_count = num_blocks;
        uow.update_document(&updated_doc)?;

        // Plan §1.6 Frame.byte_range maintenance happens centrally in
        // Transaction::commit — no explicit call needed.
        uow.commit()?;
        Ok(())
    }
}
