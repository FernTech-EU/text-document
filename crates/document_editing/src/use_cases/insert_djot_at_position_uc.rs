use super::editing_helpers::find_block_at_position;
use crate::InsertDjotAtPositionDto;
use crate::InsertDjotAtPositionResultDto;
use anyhow::{Result, anyhow};
use common::database::CommandUnitOfWork;
use common::database::rope_helpers::{
    block_char_length, block_content_via_store, rope_insert_block_at, rope_insert_in_block,
    rope_replace_block_content,
};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::entities::{Block, Document, Frame, List, Root};
use common::format_runs::{
    FormatRun, coalesce_in_place, logical_offset_to_byte, shift_images_for_insert,
    shift_runs_for_insert, splice_range, split_images_at, split_runs_at,
};

use common::parser_tools::content_parser::{self, ParsedBlock, format_runs_from_spans};
use common::parser_tools::list_grouper::ListGrouper;
use common::snapshot::EntityTreeSnapshot;
use common::types::{EntityId, ROOT_ENTITY_ID};
use common::undo_redo::UndoRedoCommand;
use std::any::Any;

pub trait InsertDjotAtPositionUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn InsertDjotAtPositionUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "Update")]
#[macros::uow_action(entity = "Document", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Snapshot")]
#[macros::uow_action(entity = "Document", action = "Restore")]
#[macros::uow_action(entity = "Frame", action = "Get")]
#[macros::uow_action(entity = "Frame", action = "Update")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "Get")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "Update")]
#[macros::uow_action(entity = "Block", action = "UpdateMulti")]
#[macros::uow_action(entity = "Block", action = "Create")]
#[macros::uow_action(entity = "Block", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "UpdateWithRelationships")]
#[macros::uow_action(entity = "List", action = "Get")]
#[macros::uow_action(entity = "List", action = "Create")]
pub trait InsertDjotAtPositionUnitOfWorkTrait: CommandUnitOfWork {}

/// Write `format_runs` and `block_images` for `block_id`, then reverse-sync
/// the legacy inline_elements bridge. `runs` may be empty (treated as
/// "no runs / inherit default").
fn write_block_state(
    uow: &mut Box<dyn InsertDjotAtPositionUnitOfWorkTrait>,
    block_id: EntityId,
    runs: Vec<FormatRun>,
    images: Vec<common::format_runs::ImageAnchor>,
) {
    let store = uow.store();
    {
        let mut runs_map = store.format_runs.write();
        if runs.is_empty() {
            runs_map.remove(&block_id);
        } else {
            runs_map.insert(block_id, runs);
        }
    }
    {
        let mut images_map = store.block_images.write();
        if images.is_empty() {
            images_map.remove(&block_id);
        } else {
            images_map.insert(block_id, images);
        }
    }
}

/// Compute (plain_text, runs_at_zero) for the inline content of `parsed`,
/// where runs are relative to byte 0 of the parsed block's own text.
fn parsed_block_payload(parsed: &ParsedBlock) -> (String, Vec<FormatRun>) {
    format_runs_from_spans(&parsed.spans, parsed.is_code_block)
}

fn execute_content_insert(
    uow: &mut Box<dyn InsertDjotAtPositionUnitOfWorkTrait>,
    position: i64,
    anchor: i64,
    parsed_blocks: &[ParsedBlock],
) -> Result<(i64, i64, EntityTreeSnapshot)> {
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

    let snapshot = uow.snapshot_document(&[doc_id])?;

    if position != anchor {
        return Err(anyhow!(
            "Selection replacement is not supported. Use delete_text first."
        ));
    }

    let frame_ids = uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;
    let frame_id = *frame_ids
        .first()
        .ok_or_else(|| anyhow!("Document has no frames"))?;

    let frame = uow
        .get_frame(&frame_id)?
        .ok_or_else(|| anyhow!("Frame not found"))?;

    let block_ids = uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;

    let blocks_opt = uow.get_block_multi(&block_ids)?;
    let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
    blocks.sort_by_key(|b| b.document_position);

    let (current_block, block_idx, offset) =
        find_block_at_position(&blocks, position, &uow.store())?;

    // Snapshot the current block's format runs and images so we can split /
    // shift them without holding a long-lived borrow on the store.
    let store = uow.store();
    let (current_runs, current_images) = {
        let runs = store
            .format_runs
            .read()
            .get(&current_block.id)
            .cloned()
            .unwrap_or_default();
        let images = store
            .block_images
            .read()
            .get(&current_block.id)
            .cloned()
            .unwrap_or_default();
        (runs, images)
    };

    let current_block_text = block_content_via_store(&current_block, &store);
    let original_current_char_length =
        current_block_text.chars().count() as i64 + current_images.len() as i64;
    let byte_offset = logical_offset_to_byte(&current_block_text, &current_images, offset);

    let now = chrono::Utc::now();

    // ── Inline merge: single block with no block-level formatting ──
    if parsed_blocks.len() == 1 && parsed_blocks[0].is_inline_only() {
        let parsed = &parsed_blocks[0];
        let (inserted_plain, inserted_runs_at_zero) = parsed_block_payload(parsed);
        let inserted_len = inserted_plain.chars().count() as i64;

        if inserted_len == 0 {
            return Ok((position, 0, snapshot));
        }

        let inserted_bytes = inserted_plain.len() as u32;

        // Build the new plain_text.
        let mut new_plain = String::with_capacity(current_block_text.len() + inserted_plain.len());
        new_plain.push_str(&current_block_text[..byte_offset as usize]);
        new_plain.push_str(&inserted_plain);
        new_plain.push_str(&current_block_text[byte_offset as usize..]);

        // Shift existing runs/images for the insert, then splice the parsed
        // block's runs over the inserted byte range (overriding inherited
        // format for any byte covered by a parsed run; uncovered bytes
        // remain unformatted, so they pick up the block's default format
        // at render time).
        let mut runs = current_runs.clone();
        shift_runs_for_insert(&mut runs, byte_offset, inserted_bytes);
        let inserted_at_offset: Vec<FormatRun> = inserted_runs_at_zero
            .into_iter()
            .map(|r| FormatRun {
                byte_start: r.byte_start + byte_offset,
                byte_end: r.byte_end + byte_offset,
                format: r.format,
            })
            .collect();
        splice_range(
            &mut runs,
            byte_offset..byte_offset + inserted_bytes,
            inserted_at_offset,
        );
        coalesce_in_place(&mut runs);

        let mut images = current_images.clone();
        shift_images_for_insert(&mut images, byte_offset, inserted_bytes);

        let mut updated_block = current_block.clone();
        updated_block.updated_at = now;
        uow.update_block(&updated_block)?;

        write_block_state(uow, current_block.id, runs, images);

        // Mirror the inline insert into the global rope.
        rope_insert_in_block(&store, current_block.id, byte_offset, &inserted_plain);

        // Shift subsequent blocks' document_position.
        let mut blocks_to_update: Vec<Block> = Vec::new();
        for b in &blocks[(block_idx + 1)..] {
            let mut ub = b.clone();
            ub.document_position += inserted_len;
            ub.updated_at = now;
            blocks_to_update.push(ub);
        }
        if !blocks_to_update.is_empty() {
            uow.update_block_multi(&blocks_to_update)?;
        }

        let mut updated_doc = document.clone();
        updated_doc.character_count += inserted_len;
        updated_doc.updated_at = now;
        uow.update_document(&updated_doc)?;

        return Ok((position + inserted_len, 0, snapshot));
    }

    // ── Block-splitting path (multi-block or block-level formatting) ──
    let text_before = current_block_text[..byte_offset as usize].to_string();
    let text_after = current_block_text[byte_offset as usize..].to_string();
    let _text_after_chars = text_after.chars().count() as i64;

    let (left_runs, right_runs) = split_runs_at(&current_runs, byte_offset);
    let (left_images, right_images) = split_images_at(&current_images, byte_offset);
    let _left_image_count = left_images.len() as i64;

    if parsed_blocks.len() >= 2 {
        // ── Multi-block: merge inline-only first/last, standalone otherwise ──
        let first_parsed = &parsed_blocks[0];
        let last_parsed = &parsed_blocks[parsed_blocks.len() - 1];
        let merge_first = first_parsed.is_inline_only();
        let merge_last = last_parsed.is_inline_only();

        let (first_plain, first_runs_at_zero) = parsed_block_payload(first_parsed);
        let first_len = first_plain.chars().count() as i64;

        // The "head" (formerly current) block.
        let mut updated_current = current_block.clone();
        let (head_plain, head_runs) = if merge_first {
            let mut hp = String::with_capacity(text_before.len() + first_plain.len());
            hp.push_str(&text_before);
            hp.push_str(&first_plain);
            let mut runs = left_runs.clone();
            let first_offset = text_before.len() as u32;
            for r in first_runs_at_zero {
                runs.push(FormatRun {
                    byte_start: r.byte_start + first_offset,
                    byte_end: r.byte_end + first_offset,
                    format: r.format,
                });
            }
            coalesce_in_place(&mut runs);
            (hp, runs)
        } else {
            (text_before.clone(), left_runs.clone())
        };
        let _head_chars = head_plain.chars().count() as i64;
        updated_current.updated_at = now;
        write_block_state(uow, current_block.id, head_runs, left_images);

        // Mirror the head's new content into the rope (skipped when
        // the head wasn't in the rope, e.g. unseeded tests).
        // NOTE: the lookup runs in its own scope so the
        // `block_offsets.read()` guard drops BEFORE
        // `rope_replace_block_content` tries to acquire the write
        // guard — otherwise the same-thread read→write upgrade
        // deadlocks.
        let head_rope_start = store
            .block_offsets
            .read()
            .range_of_block(current_block.id)
            .map(|(s, _)| s);
        let mut next_rope_byte_opt = head_rope_start.map(|s| {
            rope_replace_block_content(&store, current_block.id, &head_plain);
            s + head_plain.len() as u32
        });

        let mut new_block_ids: Vec<EntityId> = Vec::new();
        let mut total_new_chars: i64 = if merge_first { first_len } else { 0 };
        let mut running_position =
            current_block.document_position + block_char_length(&updated_current, &store) + 1;

        let middle_start = if merge_first { 1 } else { 0 };
        let middle_end = if merge_last {
            parsed_blocks.len() - 1
        } else {
            parsed_blocks.len()
        };

        let mut list_grouper = ListGrouper::new();
        for parsed in &parsed_blocks[middle_start..middle_end] {
            let (block_plain, block_runs) = parsed_block_payload(parsed);
            let block_text_len = block_plain.chars().count() as i64;

            let list_id = if let Some(ref list_style) = parsed.list_style {
                if let Some(existing_id) = list_grouper.try_reuse(list_style, parsed.list_indent) {
                    Some(existing_id)
                } else {
                    let list = List {
                        id: 0,
                        created_at: now,
                        updated_at: now,
                        style: list_style.clone(),
                        indent: parsed.list_indent as i64,
                        prefix: String::new(),
                        suffix: String::new(),
                    };
                    let created_list = uow.create_list(&list, doc_id, -1)?;
                    list_grouper.register(created_list.id, list_style.clone(), parsed.list_indent);
                    Some(created_list.id)
                }
            } else {
                list_grouper.reset();
                None
            };

            let new_block = Block {
                id: 0,
                created_at: now,
                updated_at: now,
                list: list_id,
                document_position: running_position,
                fmt_alignment: None,
                fmt_top_margin: None,
                fmt_bottom_margin: None,
                fmt_left_margin: None,
                fmt_right_margin: None,
                fmt_heading_level: parsed.heading_level,
                fmt_indent: None,
                fmt_text_indent: None,
                fmt_marker: None,
                fmt_tab_positions: vec![],
                fmt_line_height: None,
                fmt_non_breakable_lines: None,
                fmt_direction: None,
                fmt_background_color: None,
                fmt_is_code_block: None,
                fmt_code_language: None,
                fmt_hyphenate: None,
                fmt_language: None,
            };

            let insert_index = (block_idx + 1 + new_block_ids.len()) as i32;
            let created_block = uow.create_block(&new_block, frame_id, insert_index)?;
            write_block_state(uow, created_block.id, block_runs, Vec::new());

            // Mirror the new middle block into the rope (skipped when
            // the head wasn't in the rope).
            if let Some(next_rope_byte) = next_rope_byte_opt.as_mut() {
                rope_insert_block_at(&store, *next_rope_byte, created_block.id, &block_plain);
                *next_rope_byte += 1 + block_plain.len() as u32;
            }

            new_block_ids.push(created_block.id);
            total_new_chars += block_text_len;
            running_position += block_text_len + 1;
        }

        let (last_plain, last_runs_at_zero) = parsed_block_payload(last_parsed);
        let last_len = last_plain.chars().count() as i64;

        // Build tail block.
        let (tail_plain, tail_runs, tail_images) = if merge_last {
            let mut tp = String::with_capacity(last_plain.len() + text_after.len());
            tp.push_str(&last_plain);
            tp.push_str(&text_after);

            // last_runs are relative to byte 0; right_runs are also at byte 0
            // (split_runs_at re-bases them). Shift right_runs by last_plain.len().
            let last_offset = last_plain.len() as u32;
            let mut runs: Vec<FormatRun> = last_runs_at_zero;
            for r in right_runs.iter().cloned() {
                runs.push(FormatRun {
                    byte_start: r.byte_start + last_offset,
                    byte_end: r.byte_end + last_offset,
                    format: r.format,
                });
            }
            coalesce_in_place(&mut runs);

            let mut images: Vec<common::format_runs::ImageAnchor> = Vec::new();
            for img in right_images.iter().cloned() {
                images.push(common::format_runs::ImageAnchor {
                    byte_offset: img.byte_offset + last_offset,
                    ..img
                });
            }
            (tp, runs, images)
        } else {
            (text_after.clone(), right_runs.clone(), right_images.clone())
        };
        if merge_last {
            total_new_chars += last_len;
        }

        let tail_chars = tail_plain.chars().count() as i64;
        let _ = tail_chars;

        let tail_block = Block {
            id: 0,
            created_at: now,
            updated_at: now,
            list: current_block.list,
            document_position: running_position,
            fmt_alignment: current_block.fmt_alignment.clone(),
            fmt_top_margin: current_block.fmt_top_margin,
            fmt_bottom_margin: current_block.fmt_bottom_margin,
            fmt_left_margin: current_block.fmt_left_margin,
            fmt_right_margin: current_block.fmt_right_margin,
            fmt_heading_level: current_block.fmt_heading_level,
            fmt_indent: current_block.fmt_indent,
            fmt_text_indent: current_block.fmt_text_indent,
            fmt_marker: current_block.fmt_marker.clone(),
            fmt_tab_positions: current_block.fmt_tab_positions.clone(),
            fmt_line_height: current_block.fmt_line_height,
            fmt_non_breakable_lines: current_block.fmt_non_breakable_lines,
            fmt_direction: current_block.fmt_direction.clone(),
            fmt_background_color: current_block.fmt_background_color.clone(),
            fmt_is_code_block: current_block.fmt_is_code_block,
            fmt_code_language: current_block.fmt_code_language.clone(),
            fmt_hyphenate: current_block.fmt_hyphenate,
            fmt_language: current_block.fmt_language.clone(),
        };

        let tail_insert_index = (block_idx + 1 + new_block_ids.len()) as i32;
        let created_tail = uow.create_block(&tail_block, frame_id, tail_insert_index)?;
        write_block_state(uow, created_tail.id, tail_runs, tail_images);

        // Mirror the tail block into the rope.
        if let Some(next_rope_byte) = next_rope_byte_opt {
            rope_insert_block_at(&store, next_rope_byte, created_tail.id, &tail_plain);
        }

        let mut updated_frame = frame.clone();
        let child_order_insert_pos = (block_idx + 1).min(updated_frame.child_order.len());
        let mut new_child_ids: Vec<i64> = new_block_ids.iter().map(|id| *id as i64).collect();
        new_child_ids.push(created_tail.id as i64);
        for (i, id) in new_child_ids.iter().enumerate() {
            updated_frame
                .child_order
                .insert(child_order_insert_pos + i, *id);
        }
        updated_frame.updated_at = now;
        updated_frame.blocks =
            uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;
        uow.update_frame(&updated_frame)?;

        let standalone_count = (middle_end - middle_start) as i64;
        let blocks_added = standalone_count + 1;
        let original_next_pos = current_block.document_position + original_current_char_length + 1;
        let new_next_pos = running_position + block_char_length(&created_tail, &store) + 1;
        let pos_shift = new_next_pos - original_next_pos;

        let mut blocks_to_update: Vec<Block> = Vec::new();
        for b in &blocks[(block_idx + 1)..] {
            let mut ub = b.clone();
            ub.document_position += pos_shift;
            ub.updated_at = now;
            blocks_to_update.push(ub);
        }
        if !blocks_to_update.is_empty() {
            uow.update_block_multi(&blocks_to_update)?;
        }

        let mut updated_doc = document.clone();
        updated_doc.block_count += blocks_added;
        updated_doc.character_count += total_new_chars;
        updated_doc.updated_at = now;
        uow.update_document(&updated_doc)?;

        let new_position = if merge_last {
            created_tail.document_position + last_len
        } else {
            created_tail.document_position
        };
        Ok((new_position, blocks_added, snapshot))
    } else {
        // ── Single block with block-level formatting ──
        let parsed = &parsed_blocks[0];
        let (block_plain, block_runs) = parsed_block_payload(parsed);
        let block_text_len = block_plain.chars().count() as i64;

        // Head keeps text_before only.
        let mut updated_current = current_block.clone();
        updated_current.updated_at = now;
        uow.update_block(&updated_current)?;
        write_block_state(uow, current_block.id, left_runs, left_images);

        // Mirror the head's new content into the rope (skipped when
        // the head wasn't in the rope). Lookup is in its own scope to
        // drop the read guard before `rope_replace_block_content`
        // takes the write guard — otherwise same-thread upgrade
        // deadlocks.
        let head_rope_start = store
            .block_offsets
            .read()
            .range_of_block(current_block.id)
            .map(|(s, _)| s);
        let mut next_rope_byte_opt = head_rope_start.map(|s| {
            rope_replace_block_content(&store, current_block.id, &text_before);
            s + text_before.len() as u32
        });

        let mut running_position =
            current_block.document_position + block_char_length(&updated_current, &store) + 1;

        let list_id = if let Some(ref list_style) = parsed.list_style {
            let list = List {
                id: 0,
                created_at: now,
                updated_at: now,
                style: list_style.clone(),
                indent: parsed.list_indent as i64,
                prefix: String::new(),
                suffix: String::new(),
            };
            let created_list = uow.create_list(&list, doc_id, -1)?;
            Some(created_list.id)
        } else {
            None
        };

        let new_block = Block {
            id: 0,
            created_at: now,
            updated_at: now,
            list: list_id,
            document_position: running_position,
            fmt_alignment: None,
            fmt_top_margin: None,
            fmt_bottom_margin: None,
            fmt_left_margin: None,
            fmt_right_margin: None,
            fmt_heading_level: parsed.heading_level,
            fmt_indent: None,
            fmt_text_indent: None,
            fmt_marker: None,
            fmt_tab_positions: vec![],
            fmt_line_height: None,
            fmt_non_breakable_lines: None,
            fmt_direction: None,
            fmt_background_color: None,
            fmt_is_code_block: None,
            fmt_code_language: None,
            fmt_hyphenate: None,
            fmt_language: None,
        };

        let created_block = uow.create_block(&new_block, frame_id, (block_idx + 1) as i32)?;
        write_block_state(uow, created_block.id, block_runs, Vec::new());

        // Mirror the inserted block into the rope.
        if let Some(next_rope_byte) = next_rope_byte_opt.as_mut() {
            rope_insert_block_at(&store, *next_rope_byte, created_block.id, &block_plain);
            *next_rope_byte += 1 + block_plain.len() as u32;
        }

        running_position += block_text_len + 1;

        let tail_block = Block {
            id: 0,
            created_at: now,
            updated_at: now,
            list: current_block.list,
            document_position: running_position,
            fmt_alignment: current_block.fmt_alignment.clone(),
            fmt_top_margin: current_block.fmt_top_margin,
            fmt_bottom_margin: current_block.fmt_bottom_margin,
            fmt_left_margin: current_block.fmt_left_margin,
            fmt_right_margin: current_block.fmt_right_margin,
            fmt_heading_level: current_block.fmt_heading_level,
            fmt_indent: current_block.fmt_indent,
            fmt_text_indent: current_block.fmt_text_indent,
            fmt_marker: current_block.fmt_marker.clone(),
            fmt_tab_positions: current_block.fmt_tab_positions.clone(),
            fmt_line_height: current_block.fmt_line_height,
            fmt_non_breakable_lines: current_block.fmt_non_breakable_lines,
            fmt_direction: current_block.fmt_direction.clone(),
            fmt_background_color: current_block.fmt_background_color.clone(),
            fmt_is_code_block: current_block.fmt_is_code_block,
            fmt_code_language: current_block.fmt_code_language.clone(),
            fmt_hyphenate: current_block.fmt_hyphenate,
            fmt_language: current_block.fmt_language.clone(),
        };

        let created_tail = uow.create_block(&tail_block, frame_id, (block_idx + 2) as i32)?;
        write_block_state(uow, created_tail.id, right_runs, right_images);

        // Mirror the tail block into the rope.
        if let Some(next_rope_byte) = next_rope_byte_opt {
            rope_insert_block_at(&store, next_rope_byte, created_tail.id, &text_after);
        }

        let mut updated_frame = frame.clone();
        let child_order_insert_pos = (block_idx + 1).min(updated_frame.child_order.len());
        let new_child_ids = [created_block.id as i64, created_tail.id as i64];
        for (i, id) in new_child_ids.iter().enumerate() {
            updated_frame
                .child_order
                .insert(child_order_insert_pos + i, *id);
        }
        updated_frame.updated_at = now;
        updated_frame.blocks =
            uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;
        uow.update_frame(&updated_frame)?;

        let blocks_added: i64 = 2;
        let original_next_pos = current_block.document_position + original_current_char_length + 1;
        let new_next_pos = running_position + block_char_length(&created_tail, &store) + 1;
        let pos_shift = new_next_pos - original_next_pos;

        let mut blocks_to_update: Vec<Block> = Vec::new();
        for b in &blocks[(block_idx + 1)..] {
            let mut ub = b.clone();
            ub.document_position += pos_shift;
            ub.updated_at = now;
            blocks_to_update.push(ub);
        }
        if !blocks_to_update.is_empty() {
            uow.update_block_multi(&blocks_to_update)?;
        }

        let mut updated_doc = document.clone();
        updated_doc.block_count += blocks_added;
        updated_doc.character_count += block_text_len;
        updated_doc.updated_at = now;
        uow.update_document(&updated_doc)?;

        Ok((running_position, 1, snapshot))
    }
}

fn execute_insert_djot(
    uow: &mut Box<dyn InsertDjotAtPositionUnitOfWorkTrait>,
    dto: &InsertDjotAtPositionDto,
) -> Result<(InsertDjotAtPositionResultDto, EntityTreeSnapshot)> {
    let parsed_elements = content_parser::parse_djot(&dto.djot);
    let parsed_blocks = content_parser::ParsedElement::flatten_to_blocks(parsed_elements);
    let (new_position, blocks_added, snapshot) =
        execute_content_insert(uow, dto.position, dto.anchor, &parsed_blocks)?;
    Ok((
        InsertDjotAtPositionResultDto {
            new_position,
            blocks_added,
        },
        snapshot,
    ))
}

pub struct InsertDjotAtPositionUseCase {
    uow_factory: Box<dyn InsertDjotAtPositionUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<InsertDjotAtPositionDto>,
}

impl InsertDjotAtPositionUseCase {
    pub fn new(uow_factory: Box<dyn InsertDjotAtPositionUnitOfWorkFactoryTrait>) -> Self {
        InsertDjotAtPositionUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(
        &mut self,
        dto: &InsertDjotAtPositionDto,
    ) -> Result<InsertDjotAtPositionResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_insert_djot(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for InsertDjotAtPositionUseCase {
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
        let (_, snapshot) = execute_insert_djot(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
