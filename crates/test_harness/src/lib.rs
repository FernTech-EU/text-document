//! Shared test setup utilities for text-document crate tests.
//!
//! Provides helpers to create an in-memory document with content,
//! export text, and traverse the entity tree. This crate depends only
//! on `common` and `direct_access` — it reimplements plain-text import
//! and export directly via entity controllers, so it does **not** depend
//! on `document_io` or any feature crate, breaking the circular
//! dev-dependency chain.

use anyhow::Result;
use common::types::EntityId;
use common::undo_redo::UndoRedoManager;
use std::sync::Arc;

// Re-export commonly used types and controllers for convenience
pub use common::database::db_context::DbContext;
pub use common::event::EventHub;
pub use common::direct_access::block::block_repository::BlockRelationshipField;
pub use common::direct_access::document::document_repository::DocumentRelationshipField;
pub use common::direct_access::frame::frame_repository::FrameRelationshipField;
pub use common::direct_access::root::root_repository::RootRelationshipField;
pub use common::format_runs::InlineSegment;
pub use common::format_runs::{FormatRun, ImageAnchor};
pub use common::format_runs_query::{get_block_images, get_format_runs, inline_segments_for_block};

pub use common::direct_access::table::table_repository::TableRelationshipField;
pub use common::direct_access::table_cell::table_cell_repository::TableCellRelationshipField;
pub use direct_access::block::block_controller;
pub use direct_access::block::dtos::{
    BlockDto, BlockRelationshipDto, CreateBlockDto, UpdateBlockDto,
};
pub use direct_access::document::document_controller;
pub use direct_access::document::dtos::CreateDocumentDto;
pub use direct_access::frame::dtos::{CreateFrameDto, FrameDto};
pub use direct_access::frame::frame_controller;
pub use direct_access::list::dtos::CreateListDto as CreateListEntityDto;
pub use direct_access::list::list_controller;
pub use direct_access::root::dtos::CreateRootDto;
pub use direct_access::root::root_controller;
pub use direct_access::table::dtos::{CreateTableDto, TableDto};
pub use direct_access::table::table_controller;
pub use direct_access::table_cell::dtos::{CreateTableCellDto, TableCellDto};
pub use direct_access::table_cell::table_cell_controller;

/// Create an in-memory database with a Root and empty Document.
///
/// Returns `(DbContext, Arc<EventHub>, UndoRedoManager)`.
pub fn setup() -> Result<(DbContext, Arc<EventHub>, UndoRedoManager)> {
    let db_context = DbContext::new()?;
    let event_hub = Arc::new(EventHub::new());
    let mut undo_redo_manager = UndoRedoManager::new();

    let root = root_controller::create_orphan(&db_context, &event_hub, &CreateRootDto::default())?;

    let _doc = document_controller::create(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &CreateDocumentDto::default(),
        root.id,
        -1,
    )?;

    Ok((db_context, event_hub, undo_redo_manager))
}

/// Create an in-memory database with a Root, Document, and imported text content.
///
/// Splits the text on `\n` and creates one Block per line carrying its
/// `plain_text` field. `format_runs` and `block_images` stay empty —
/// matches what `document_io::import_plain_text` does without depending
/// on the `document_io` crate.
///
/// Does NOT populate the global rope — for tests that need rope state
/// to be seeded, use `setup_with_imported_text` instead.
///
/// Returns `(DbContext, Arc<EventHub>, UndoRedoManager)`.
pub fn setup_with_text(text: &str) -> Result<(DbContext, Arc<EventHub>, UndoRedoManager)> {
    let (db_context, event_hub, mut undo_redo_manager) = setup()?;

    // Get Root -> Document -> existing Frame
    let root_rels =
        root_controller::get_relationship(&db_context, &1, &RootRelationshipField::Document)?;
    let doc_id = root_rels[0];
    let frame_ids = document_controller::get_relationship(
        &db_context,
        &doc_id,
        &DocumentRelationshipField::Frames,
    )?;

    // Remove existing frames (the setup creates one empty frame)
    for fid in &frame_ids {
        frame_controller::remove(&db_context, &event_hub, &mut undo_redo_manager, None, fid)?;
    }

    // Frame removal cascades entities but does not clean up the rope-side
    // offset index. Reset the rope so the new block we're about to create
    // starts from a clean slate (no stale Block(b0) entry from initialize()).
    common::database::rope_helpers::rope_reset(db_context.get_store());

    // Create a fresh frame
    let frame = frame_controller::create(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &CreateFrameDto::default(),
        doc_id,
        -1,
    )?;

    // Split text into lines and create blocks
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut document_position: i64 = 0;
    let mut total_chars: i64 = 0;
    let mut child_order: Vec<i64> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let line_len = line.chars().count() as i64;

        let block_dto = CreateBlockDto {
            document_position,
            ..Default::default()
        };

        let block = block_controller::create(
            &db_context,
            &event_hub,
            &mut undo_redo_manager,
            None,
            &block_dto,
            frame.id,
            i as i32,
        )?;

        // Seed the rope with the line's content so block_content_via_store
        // returns the right value. Mirrors what document_io::import_plain_text
        // does at the use-case layer.
        if i > 0 {
            common::database::rope_helpers::rope_insert_block_boundary(db_context.get_store());
        }
        common::database::rope_helpers::rope_append_block(db_context.get_store(), block.id, line);

        child_order.push(block.id as i64);

        total_chars += line_len;
        document_position += line_len;
        if i < lines.len() - 1 {
            document_position += 1; // block separator
        }
    }

    // Update frame child_order to include all blocks
    let mut updated_frame = frame_controller::get(&db_context, &frame.id)?
        .ok_or_else(|| anyhow::anyhow!("Frame not found"))?;
    updated_frame.child_order = child_order;
    frame_controller::update(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &updated_frame.into(),
    )?;

    // Update document cached fields
    let mut doc = document_controller::get(&db_context, &doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found"))?;
    doc.character_count = total_chars;
    doc.block_count = lines.len() as i64;
    document_controller::update(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &doc.into(),
    )?;

    // Clear undo history so test starts clean
    undo_redo_manager.clear_all_stacks();

    Ok((db_context, event_hub, undo_redo_manager))
}

/// Alias for [`setup_with_text`]. The rope is the sole content
/// source, so `setup_with_text` always seeds it. Kept for
/// source-compatibility with existing tests.
pub fn setup_with_imported_text(text: &str) -> Result<(DbContext, Arc<EventHub>, UndoRedoManager)> {
    setup_with_text(text)
}

/// Export the current document as plain text by reading blocks and
/// concatenating their `plain_text` fields with `\n` separators.
pub fn export_text(db_context: &DbContext, _event_hub: &Arc<EventHub>) -> Result<String> {
    let block_ids = get_block_ids(db_context)?;
    let mut blocks = Vec::new();
    for id in &block_ids {
        if let Some(b) = block_controller::get(db_context, id)? {
            blocks.push(b);
        }
    }
    blocks.sort_by_key(|b| b.document_position);
    let store = db_context.get_store();
    let parts: Vec<String> = blocks
        .iter()
        .map(|b| {
            let entity: common::entities::Block = b.clone().into();
            common::database::rope_helpers::block_content_via_store(&entity, store)
        })
        .collect();
    Ok(parts.join("\n"))
}

/// Get the first frame's block IDs.
pub fn get_block_ids(db_context: &DbContext) -> Result<Vec<EntityId>> {
    let root_rels =
        root_controller::get_relationship(db_context, &1, &RootRelationshipField::Document)?;
    let doc_id = root_rels[0];
    let frame_ids = document_controller::get_relationship(
        db_context,
        &doc_id,
        &DocumentRelationshipField::Frames,
    )?;
    let frame_id = frame_ids[0];
    frame_controller::get_relationship(db_context, &frame_id, &FrameRelationshipField::Blocks)
}

/// Get the synthesized element IDs for a given block.
///
/// After Phase 1.14 the `inline_elements` table no longer exists; the
/// "elements" of a block are now synthesized on demand from its
/// `(plain_text, format_runs, block_images)`. The returned IDs are
/// stable derivations of `(block_id, byte_start)` produced by
/// [`common::format_runs::synth_element_id`], so callers can
/// round-trip an id through [`inline_element_controller::get`] (the
/// shim below) to fetch the corresponding `InlineSegment` view.
pub fn get_element_ids(db_context: &DbContext, block_id: &EntityId) -> Result<Vec<EntityId>> {
    use common::format_runs::{InlineContent, synth_element_id};

    let segments = synth_block_elements(db_context, *block_id)?;
    let mut ids = Vec::new();
    let mut byte_offset: u32 = 0;

    for seg in segments {
        ids.push(synth_element_id(*block_id, byte_offset));
        // Advance byte offset (Text contributes UTF-8 length, Image/Empty contributes 0)
        if let InlineContent::Text(s) = &seg.content {
            byte_offset += s.len() as u32;
        }
    }

    Ok(ids)
}

/// Get the first block's synthesized element IDs.
pub fn get_first_block_element_ids(db_context: &DbContext) -> Result<Vec<EntityId>> {
    let block_ids = get_block_ids(db_context)?;
    get_element_ids(db_context, &block_ids[0])
}

/// Compatibility shim: the legacy `inline_element_controller::get`
/// looked up an entity row in the `inline_elements` table by id. After
/// Phase 1.14 the table is gone; this shim walks every block's
/// synthesized inline-segment view to find the matching synthetic id. Used by
/// tests that previously did `inline_element_controller::get(db, &id)`.
pub mod inline_element_controller {
    use super::*;
    use common::format_runs::synth_element_id;

    pub fn get(db_context: &DbContext, elem_id: &EntityId) -> Result<Option<InlineSegment>> {
        for bid in get_all_block_ids(db_context)? {
            let _block = block_controller::get(db_context, &bid)?
                .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
            let segments = synth_block_elements(db_context, bid)?;

            let mut byte_offset: u32 = 0;
            for seg in segments {
                let synth_id = synth_element_id(bid, byte_offset);
                if synth_id == *elem_id {
                    return Ok(Some(seg));
                }
                // Advance byte offset (Text contributes UTF-8 length, Image/Empty contributes 0)
                if let common::format_runs::InlineContent::Text(s) = &seg.content {
                    byte_offset += s.len() as u32;
                }
            }
        }
        Ok(None)
    }
}

/// Synthesize the inline-element view of a block from format_runs +
/// block_images. Use this in tests that previously called
/// `inline_element_controller::get` on each element id — after the
/// writer migration, the inline_elements table no longer reflects
/// images created by feature use cases.
pub fn synth_block_elements(
    db_context: &DbContext,
    block_id: EntityId,
) -> Result<Vec<InlineSegment>> {
    let block_dto = block_controller::get(db_context, &block_id)?
        .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
    let block: common::entities::Block = block_dto.into();
    let plain_text =
        common::database::rope_helpers::block_content_via_store(&block, db_context.get_store());
    Ok(inline_segments_for_block(
        db_context.get_store(),
        block_id,
        &plain_text,
    ))
}

/// Synthesized inline-segment view of the first block.
pub fn synth_first_block_elements(db_context: &DbContext) -> Result<Vec<InlineSegment>> {
    let block_ids = get_block_ids(db_context)?;
    synth_block_elements(db_context, block_ids[0])
}

/// Image anchors stored on a block (post-Phase-1 source of truth for images).
pub fn get_block_image_anchors(db_context: &DbContext, block_id: EntityId) -> Vec<ImageAnchor> {
    get_block_images(db_context.get_store(), block_id)
}

/// Read a block's content from the rope via a `BlockDto`.
pub fn block_text_dto(db_context: &DbContext, dto: &BlockDto) -> String {
    let entity: common::entities::Block = dto.clone().into();
    common::database::rope_helpers::block_content_via_store(&entity, db_context.get_store())
}

/// Get the first frame ID for the document.
pub fn get_frame_id(db_context: &DbContext) -> Result<EntityId> {
    let root_rels =
        root_controller::get_relationship(db_context, &1, &RootRelationshipField::Document)?;
    let doc_id = root_rels[0];
    let frame_ids = document_controller::get_relationship(
        db_context,
        &doc_id,
        &DocumentRelationshipField::Frames,
    )?;
    Ok(frame_ids[0])
}

/// Get all table IDs in the document.
pub fn get_table_ids(db_context: &DbContext) -> Result<Vec<EntityId>> {
    let root_rels =
        root_controller::get_relationship(db_context, &1, &RootRelationshipField::Document)?;
    let doc_id = root_rels[0];
    document_controller::get_relationship(db_context, &doc_id, &DocumentRelationshipField::Tables)
}

/// Get all cell IDs for a given table.
pub fn get_table_cell_ids(db_context: &DbContext, table_id: &EntityId) -> Result<Vec<EntityId>> {
    table_controller::get_relationship(db_context, table_id, &TableRelationshipField::Cells)
}

/// Get all cells for a table, sorted by row then column.
pub fn get_sorted_cells(db_context: &DbContext, table_id: &EntityId) -> Result<Vec<TableCellDto>> {
    let cell_ids = get_table_cell_ids(db_context, table_id)?;
    let cells_opt = table_cell_controller::get_multi(db_context, &cell_ids)?;
    let mut cells: Vec<TableCellDto> = cells_opt.into_iter().flatten().collect();
    cells.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));
    Ok(cells)
}

/// Get all block IDs across all frames in the document (not just the first frame).
pub fn get_all_block_ids(db_context: &DbContext) -> Result<Vec<EntityId>> {
    let root_rels =
        root_controller::get_relationship(db_context, &1, &RootRelationshipField::Document)?;
    let doc_id = root_rels[0];
    let frame_ids = document_controller::get_relationship(
        db_context,
        &doc_id,
        &DocumentRelationshipField::Frames,
    )?;
    let mut all_block_ids = Vec::new();
    for fid in &frame_ids {
        let block_ids =
            frame_controller::get_relationship(db_context, fid, &FrameRelationshipField::Blocks)?;
        all_block_ids.extend(block_ids);
    }
    Ok(all_block_ids)
}

/// Basic document statistics retrieved directly from entity data.
pub struct BasicStats {
    pub character_count: i64,
    pub block_count: i64,
    pub frame_count: i64,
}

/// Get basic document statistics by reading the Document entity directly.
pub fn get_document_stats(db_context: &DbContext) -> Result<BasicStats> {
    let root_rels =
        root_controller::get_relationship(db_context, &1, &RootRelationshipField::Document)?;
    let doc_id = root_rels[0];
    let doc = document_controller::get(db_context, &doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found"))?;
    let frame_ids = document_controller::get_relationship(
        db_context,
        &doc_id,
        &DocumentRelationshipField::Frames,
    )?;
    Ok(BasicStats {
        character_count: doc.character_count,
        block_count: doc.block_count,
        frame_count: frame_ids.len() as i64,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// Test-only helpers that build richer documents (tables, lists, images,
// frames) using entity controllers directly — no feature-crate dependency.
// ═══════════════════════════════════════════════════════════════════════════

pub struct InsertTableResult {
    pub table_id: EntityId,
}

/// Insert a `rows x columns` table at `position` using entity controllers.
///
/// Creates the Table, one Frame+Block+EmptyElement per cell, and adjusts
/// `document_position` for all subsequent blocks.
pub fn insert_table(
    db_context: &DbContext,
    event_hub: &Arc<EventHub>,
    undo_redo_manager: &mut UndoRedoManager,
    position: i64,
    rows: i64,
    columns: i64,
) -> Result<InsertTableResult> {
    let doc_id = get_doc_id(db_context)?;

    // Create table owned by document
    let table = table_controller::create(
        db_context,
        event_hub,
        undo_redo_manager,
        None,
        &CreateTableDto {
            rows,
            columns,
            ..Default::default()
        },
        doc_id,
        -1,
    )?;

    let table_size = rows * columns;
    let mut cell_blocks: Vec<EntityId> = Vec::new();

    for r in 0..rows {
        for c in 0..columns {
            // Create cell frame owned by document
            let cell_frame = frame_controller::create(
                db_context,
                event_hub,
                undo_redo_manager,
                None,
                &CreateFrameDto::default(),
                doc_id,
                -1,
            )?;

            // Create block in cell frame
            let block = block_controller::create(
                db_context,
                event_hub,
                undo_redo_manager,
                None,
                &CreateBlockDto::default(),
                cell_frame.id,
                0,
            )?;

            // Create table cell owned by table
            table_cell_controller::create(
                db_context,
                event_hub,
                undo_redo_manager,
                None,
                &CreateTableCellDto {
                    row: r,
                    column: c,
                    row_span: 1,
                    column_span: 1,
                    cell_frame: Some(cell_frame.id),
                    ..Default::default()
                },
                table.id,
                -1,
            )?;

            cell_blocks.push(block.id);
        }
    }

    // Mirror cell-block creation into the global rope so subsequent
    // `rope_replace_block_content` / `block_content_via_store` calls
    // see them. The harness skips the anchor-frame/sentinel layout from
    // `insert_table_uc`; cells are simply appended as empty blocks at
    // the rope tail, which is enough for tests that only care about
    // per-cell content addressability.
    for &bid in &cell_blocks {
        common::database::rope_helpers::rope_append_empty_block(db_context.get_store(), bid);
    }

    // Assign document_positions to cell blocks
    for (current_pos, &bid) in (position..).zip(cell_blocks.iter()) {
        let mut b = block_controller::get(db_context, &bid)?
            .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
        b.document_position = current_pos;
        block_controller::update(db_context, event_hub, undo_redo_manager, None, &b.into())?;
    }

    // Shift existing blocks (not cell blocks) that are at or after position
    let all_bids = get_all_block_ids(db_context)?;
    for bid in &all_bids {
        if cell_blocks.contains(bid) {
            continue;
        }
        let b = block_controller::get(db_context, bid)?
            .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
        if b.document_position >= position {
            let mut updated = b.clone();
            updated.document_position += table_size;
            block_controller::update(
                db_context,
                event_hub,
                undo_redo_manager,
                None,
                &updated.into(),
            )?;
        }
    }

    undo_redo_manager.clear_all_stacks();
    Ok(InsertTableResult { table_id: table.id })
}

pub struct CreateListResult {
    pub list_id: EntityId,
}

/// Create a list spanning blocks in `[position, anchor]` using entity controllers.
pub fn create_list(
    db_context: &DbContext,
    event_hub: &Arc<EventHub>,
    undo_redo_manager: &mut UndoRedoManager,
    position: i64,
    anchor: i64,
    style: common::entities::ListStyle,
) -> Result<CreateListResult> {
    let doc_id = get_doc_id(db_context)?;
    let sel_start = std::cmp::min(position, anchor);
    let sel_end = std::cmp::max(position, anchor);

    let list = list_controller::create(
        db_context,
        event_hub,
        undo_redo_manager,
        None,
        &CreateListEntityDto {
            style,
            ..Default::default()
        },
        doc_id,
        -1,
    )?;

    // Find overlapping blocks and assign them to the list
    let all_bids = get_all_block_ids(db_context)?;
    let store = db_context.get_store();
    for bid in &all_bids {
        let b = block_controller::get(db_context, bid)?
            .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
        let block_start = b.document_position;
        let block_end = block_start
            + common::database::rope_helpers::block_char_length(&b.clone().into(), store);
        if block_end >= sel_start && block_start <= sel_end {
            block_controller::set_relationship(
                db_context,
                event_hub,
                undo_redo_manager,
                None,
                &BlockRelationshipDto {
                    id: b.id,
                    field: BlockRelationshipField::List,
                    right_ids: vec![list.id],
                },
            )?;
        }
    }

    undo_redo_manager.clear_all_stacks();
    Ok(CreateListResult { list_id: list.id })
}

pub struct InsertImageResult {
    pub new_position: i64,
    /// Byte offset of the anchored image inside the target block's
    /// `plain_text`. Used by tests that want to assert the new image's
    /// position; the legacy element-id no longer exists.
    pub byte_offset: u32,
}

/// Insert an image anchor at `position` by writing directly to the
/// store's `block_images` table.
///
/// The image is a single logical character at `position`, contributing
/// zero bytes to `plain_text`. Subsequent blocks shift by +1.
pub fn insert_image(
    db_context: &DbContext,
    event_hub: &Arc<EventHub>,
    undo_redo_manager: &mut UndoRedoManager,
    position: i64,
    image_name: &str,
    width: i64,
    height: i64,
) -> Result<InsertImageResult> {
    use common::format_runs::{ImageAnchor, logical_offset_to_byte};

    // Find block containing position.
    let all_bids = get_all_block_ids(db_context)?;
    let mut blocks = Vec::new();
    for bid in &all_bids {
        blocks.push(
            block_controller::get(db_context, bid)?
                .ok_or_else(|| anyhow::anyhow!("Block not found"))?,
        );
    }
    blocks.sort_by_key(|b| b.document_position);

    let store = db_context.get_store();
    let (target_block, offset) = blocks
        .iter()
        .find_map(|b| {
            let s = b.document_position;
            let e = s + common::database::rope_helpers::block_char_length(&b.clone().into(), store);
            if position >= s && position <= e {
                Some((b.clone(), position - s))
            } else {
                None
            }
        })
        .ok_or_else(|| anyhow::anyhow!("No block at position {}", position))?;

    let store = db_context.get_store();
    let images_at_block = store
        .block_images
        .read()
        .get(&target_block.id)
        .cloned()
        .unwrap_or_default();
    let target_entity: common::entities::Block = target_block.clone().into();
    let target_text =
        common::database::rope_helpers::block_content_via_store(&target_entity, store);
    let byte_offset = logical_offset_to_byte(&target_text, &images_at_block, offset);

    // Insert into block_images, keeping sort order by byte_offset (new
    // image goes AFTER any anchors at the same byte position to match
    // insert_image_uc's convention).
    {
        let mut images_map = store.block_images.write();
        let images = images_map.entry(target_block.id).or_default();
        let insert_idx = images
            .iter()
            .position(|a| a.byte_offset > byte_offset)
            .unwrap_or(images.len());
        images.insert(
            insert_idx,
            ImageAnchor {
                byte_offset,
                name: image_name.to_string(),
                width,
                height,
                quality: 100,
                format: Default::default(),
            },
        );
    }

    // Update block text_length (+1 for the new image's logical position)
    // and shift subsequent blocks.
    let upd_block = target_block.clone();
    block_controller::update(
        db_context,
        event_hub,
        undo_redo_manager,
        None,
        &upd_block.into(),
    )?;

    for b in &blocks {
        if b.id != target_block.id && b.document_position > target_block.document_position {
            let mut shifted = b.clone();
            shifted.document_position += 1;
            block_controller::update(
                db_context,
                event_hub,
                undo_redo_manager,
                None,
                &shifted.into(),
            )?;
        }
    }

    undo_redo_manager.clear_all_stacks();
    Ok(InsertImageResult {
        new_position: position + 1,
        byte_offset,
    })
}

pub struct InsertFrameResult {
    pub frame_id: EntityId,
}

/// Insert a sub-frame at `position` using entity controllers.
///
/// The new frame contains one empty block and is registered in
/// the document's frames collection.
pub fn insert_frame(
    db_context: &DbContext,
    event_hub: &Arc<EventHub>,
    undo_redo_manager: &mut UndoRedoManager,
    position: i64,
) -> Result<InsertFrameResult> {
    let doc_id = get_doc_id(db_context)?;

    let new_frame = frame_controller::create(
        db_context,
        event_hub,
        undo_redo_manager,
        None,
        &CreateFrameDto::default(),
        doc_id,
        -1,
    )?;

    let _block = block_controller::create(
        db_context,
        event_hub,
        undo_redo_manager,
        None,
        &CreateBlockDto {
            document_position: position,
            ..Default::default()
        },
        new_frame.id,
        0,
    )?;

    undo_redo_manager.clear_all_stacks();
    Ok(InsertFrameResult {
        frame_id: new_frame.id,
    })
}

fn get_doc_id(db_context: &DbContext) -> Result<EntityId> {
    let root_rels =
        root_controller::get_relationship(db_context, &1, &RootRelationshipField::Document)?;
    Ok(root_rels[0])
}
