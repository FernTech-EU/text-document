use super::editing_helpers::find_block_at_position;
use crate::InsertHtmlAtPositionDto;
use crate::InsertHtmlAtPositionResultDto;
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

pub trait InsertHtmlAtPositionUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn InsertHtmlAtPositionUnitOfWorkTrait>;
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
pub trait InsertHtmlAtPositionUnitOfWorkTrait: CommandUnitOfWork {}

/// Write `format_runs` and `block_images` for `block_id`, then reverse-sync
/// the legacy inline_elements bridge so unmigrated readers see the same
/// content. An empty `runs` or `images` vector is treated as "drop entry"
/// rather than "insert empty".
fn write_block_state(
    uow: &mut Box<dyn InsertHtmlAtPositionUnitOfWorkTrait>,
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

fn parsed_block_payload(parsed: &ParsedBlock) -> (String, Vec<FormatRun>) {
    format_runs_from_spans(&parsed.spans, parsed.is_code_block)
}

fn execute_content_insert(
    uow: &mut Box<dyn InsertHtmlAtPositionUnitOfWorkTrait>,
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
    // Capture the pre-mutation char length so that downstream position
    // math (head_delta, running_position, original_next_pos) doesn't
    // re-read it from a rope that has since been overwritten by the
    // head update — `original_current_char_length` would
    // return the post-mutation length, not the original.
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
        let mut new_plain = String::with_capacity(current_block_text.len() + inserted_plain.len());
        new_plain.push_str(&current_block_text[..byte_offset as usize]);
        new_plain.push_str(&inserted_plain);
        new_plain.push_str(&current_block_text[byte_offset as usize..]);

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

    // ── Block-splitting path ──
    let text_before = current_block_text[..byte_offset as usize].to_string();
    let text_after = current_block_text[byte_offset as usize..].to_string();
    let text_before_chars = text_before.chars().count() as i64;
    let text_after_chars = text_after.chars().count() as i64;

    let (left_runs, right_runs) = split_runs_at(&current_runs, byte_offset);
    let (left_images, right_images) = split_images_at(&current_images, byte_offset);
    let left_image_count = left_images.len() as i64;
    let right_image_count = right_images.len() as i64;

    if parsed_blocks.len() >= 2 {
        // ── Multi-block ──
        let first_parsed = &parsed_blocks[0];
        let last_parsed = &parsed_blocks[parsed_blocks.len() - 1];
        let merge_first = first_parsed.is_inline_only();
        let merge_last = last_parsed.is_inline_only();

        let (first_plain, first_runs_at_zero) = parsed_block_payload(first_parsed);
        let first_len = first_plain.chars().count() as i64;

        // When text_before is empty and we can't merge, overwrite the
        // current block with the first parsed block instead of leaving
        // an empty orphan.
        let overwrite_head = text_before.is_empty() && !merge_first;

        let mut list_grouper = ListGrouper::new();
        if !overwrite_head
            && let Some(list_id) = current_block.list
            && let Ok(Some(list_entity)) = uow.get_list(&list_id)
        {
            list_grouper.register(
                list_id,
                list_entity.style.clone(),
                list_entity.indent as u32,
            );
        }

        let mut updated_current = current_block.clone();
        let (head_plain, head_runs, head_images) = if overwrite_head {
            let head_list_id = if let Some(ref list_style) = first_parsed.list_style {
                if let Some(existing_id) =
                    list_grouper.try_reuse(list_style, first_parsed.list_indent)
                {
                    Some(existing_id)
                } else {
                    let list = List {
                        id: 0,
                        created_at: now,
                        updated_at: now,
                        style: list_style.clone(),
                        indent: first_parsed.list_indent as i64,
                        prefix: String::new(),
                        suffix: String::new(),
                    };
                    let created_list = uow.create_list(&list, doc_id, -1)?;
                    list_grouper.register(
                        created_list.id,
                        list_style.clone(),
                        first_parsed.list_indent,
                    );
                    Some(created_list.id)
                }
            } else {
                list_grouper.reset();
                None
            };
            updated_current.list = head_list_id;
            updated_current.fmt_heading_level = first_parsed.heading_level;
            updated_current.fmt_line_height = first_parsed.line_height;
            updated_current.fmt_non_breakable_lines = first_parsed.non_breakable_lines;
            updated_current.fmt_direction = first_parsed.direction.clone();
            updated_current.fmt_background_color = first_parsed.background_color.clone();
            updated_current.updated_at = now;
            uow.update_block_with_relationships(&updated_current)?;
            // The old inline_elements list for this block is now stale;
            // rebuild from the new (plain_text, runs, images).
            (first_plain.clone(), first_runs_at_zero.clone(), Vec::new())
        } else if merge_first {
            let mut hp = String::with_capacity(text_before.len() + first_plain.len());
            hp.push_str(&text_before);
            hp.push_str(&first_plain);
            let mut runs = left_runs.clone();
            let first_offset = text_before.len() as u32;
            for r in first_runs_at_zero.iter().cloned() {
                runs.push(FormatRun {
                    byte_start: r.byte_start + first_offset,
                    byte_end: r.byte_end + first_offset,
                    format: r.format,
                });
            }
            coalesce_in_place(&mut runs);
            let _ = text_before_chars + first_len + left_image_count;
            updated_current.updated_at = now;
            uow.update_block(&updated_current)?;
            (hp, runs, left_images.clone())
        } else {
            updated_current.updated_at = now;
            uow.update_block(&updated_current)?;
            (text_before.clone(), left_runs.clone(), left_images.clone())
        };
        write_block_state(uow, current_block.id, head_runs, head_images);

        // Mirror the head's new content into the rope. Compute the
        // post-head byte position so subsequent block inserts can be
        // placed contiguously. If `current_block` is not registered
        // in `block_offsets` (e.g. tests that build a document via
        // `setup_with_text` without seeding the rope), skip the
        // mirror altogether — the divergence guard in
        // `block_content_via_store` handles reads from `plain_text`.
        let head_rope_start = store
            .block_offsets
            .read()
            .range_of_block(current_block.id)
            .map(|(s, _)| s);
        let mut next_rope_byte_opt = head_rope_start.map(|s| {
            rope_replace_block_content(&store, current_block.id, &head_plain);
            s + head_plain.len() as u32
        });

        let head_delta = block_char_length(&updated_current, &store) - original_current_char_length;
        let mut new_block_ids: Vec<EntityId> = Vec::new();
        let mut total_new_chars: i64 = if merge_first || overwrite_head {
            head_delta
        } else {
            0
        };
        let mut running_position =
            current_block.document_position + block_char_length(&updated_current, &store) + 1;

        let middle_start = if merge_first || overwrite_head { 1 } else { 0 };
        let middle_end = if merge_last {
            parsed_blocks.len() - 1
        } else {
            parsed_blocks.len()
        };

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
                fmt_line_height: parsed.line_height,
                fmt_non_breakable_lines: parsed.non_breakable_lines,
                fmt_direction: parsed.direction.clone(),
                fmt_background_color: parsed.background_color.clone(),
                fmt_is_code_block: None,
                fmt_code_language: None,
                fmt_hyphenate: None,
                fmt_language: None,
            };

            let insert_index = (block_idx + 1 + new_block_ids.len()) as i32;
            let created_block = uow.create_block(&new_block, frame_id, insert_index)?;
            write_block_state(uow, created_block.id, block_runs, Vec::new());

            // Mirror the new middle block into the rope at the
            // running byte cursor (prepends a `\n` boundary). Skipped
            // when the head wasn't in the rope (unseeded test docs).
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

        let (tail_plain, tail_runs, tail_images, _tail_chars) = if merge_last {
            let mut tp = String::with_capacity(last_plain.len() + text_after.len());
            tp.push_str(&last_plain);
            tp.push_str(&text_after);
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
            let tail_chars = last_len + text_after_chars;
            (tp, runs, images, tail_chars)
        } else {
            (
                text_after.clone(),
                right_runs.clone(),
                right_images.clone(),
                text_after_chars,
            )
        };
        if merge_last {
            total_new_chars += last_len;
        }

        // Skip the tail block when the original split point sat exactly at
        // the end of the current block AND we're not merging text into the
        // tail. Otherwise an empty orphan block would appear.
        let skip_tail = tail_plain.is_empty() && !merge_last && right_image_count == 0;

        let mut tail_doc_pos: i64 = 0;
        if !skip_tail {
            let tail_block = Block {
                id: 0,
                created_at: now,
                updated_at: now,
                list: if overwrite_head {
                    None
                } else {
                    current_block.list
                },
                document_position: running_position,
                fmt_alignment: if overwrite_head {
                    None
                } else {
                    current_block.fmt_alignment.clone()
                },
                fmt_top_margin: if overwrite_head {
                    None
                } else {
                    current_block.fmt_top_margin
                },
                fmt_bottom_margin: if overwrite_head {
                    None
                } else {
                    current_block.fmt_bottom_margin
                },
                fmt_left_margin: if overwrite_head {
                    None
                } else {
                    current_block.fmt_left_margin
                },
                fmt_right_margin: if overwrite_head {
                    None
                } else {
                    current_block.fmt_right_margin
                },
                fmt_heading_level: if overwrite_head {
                    None
                } else {
                    current_block.fmt_heading_level
                },
                fmt_indent: if overwrite_head {
                    None
                } else {
                    current_block.fmt_indent
                },
                fmt_text_indent: if overwrite_head {
                    None
                } else {
                    current_block.fmt_text_indent
                },
                fmt_marker: if overwrite_head {
                    None
                } else {
                    current_block.fmt_marker.clone()
                },
                fmt_tab_positions: if overwrite_head {
                    vec![]
                } else {
                    current_block.fmt_tab_positions.clone()
                },
                fmt_line_height: if overwrite_head {
                    None
                } else {
                    current_block.fmt_line_height
                },
                fmt_non_breakable_lines: if overwrite_head {
                    None
                } else {
                    current_block.fmt_non_breakable_lines
                },
                fmt_direction: if overwrite_head {
                    None
                } else {
                    current_block.fmt_direction.clone()
                },
                fmt_background_color: if overwrite_head {
                    None
                } else {
                    current_block.fmt_background_color.clone()
                },
                fmt_is_code_block: if overwrite_head {
                    None
                } else {
                    current_block.fmt_is_code_block
                },
                fmt_code_language: if overwrite_head {
                    None
                } else {
                    current_block.fmt_code_language.clone()
                },
                fmt_hyphenate: if overwrite_head {
                    None
                } else {
                    current_block.fmt_hyphenate
                },
                fmt_language: if overwrite_head {
                    None
                } else {
                    current_block.fmt_language.clone()
                },
            };

            tail_doc_pos = running_position;
            let tail_insert_index = (block_idx + 1 + new_block_ids.len()) as i32;
            let created_tail = uow.create_block(&tail_block, frame_id, tail_insert_index)?;
            write_block_state(uow, created_tail.id, tail_runs, tail_images);

            // Mirror the tail block into the rope.
            if let Some(next_rope_byte) = next_rope_byte_opt {
                rope_insert_block_at(&store, next_rope_byte, created_tail.id, &tail_plain);
            }

            new_block_ids.push(created_tail.id);
            running_position += block_char_length(&created_tail, &store) + 1;
        }

        let mut updated_frame = frame.clone();
        let child_order_insert_pos = (block_idx + 1).min(updated_frame.child_order.len());
        let new_child_ids: Vec<i64> = new_block_ids.iter().map(|id| *id as i64).collect();
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
        let blocks_added = standalone_count + if skip_tail { 0 } else { 1 };
        let original_next_pos = current_block.document_position + original_current_char_length + 1;
        let pos_shift = running_position - original_next_pos;

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

        let new_position = if skip_tail {
            running_position - 1
        } else if merge_last {
            tail_doc_pos + last_len
        } else {
            tail_doc_pos
        };
        Ok((new_position, blocks_added, snapshot))
    } else {
        // ── Single block with block-level formatting ──
        let parsed = &parsed_blocks[0];
        let (block_plain, block_runs) = parsed_block_payload(parsed);
        let block_text_len = block_plain.chars().count() as i64;

        let overwrite_head = text_before.is_empty();
        let skip_tail = text_after.is_empty() && right_image_count == 0;

        if overwrite_head {
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

            let mut updated_current = current_block.clone();
            updated_current.list = list_id;
            updated_current.fmt_heading_level = parsed.heading_level;
            updated_current.fmt_line_height = parsed.line_height;
            updated_current.fmt_non_breakable_lines = parsed.non_breakable_lines;
            updated_current.fmt_direction = parsed.direction.clone();
            updated_current.fmt_background_color = parsed.background_color.clone();
            updated_current.updated_at = now;
            uow.update_block_with_relationships(&updated_current)?;
            write_block_state(uow, current_block.id, block_runs, Vec::new());

            // Mirror the head's new content into the rope (skipped
            // when the head wasn't in the rope, e.g. unseeded tests).
            // The lookup runs in its own scope so the
            // `block_offsets.read()` guard drops BEFORE
            // `rope_replace_block_content` acquires its write
            // guard — otherwise the same-thread upgrade deadlocks.
            let head_rope_start = store
                .block_offsets
                .read()
                .range_of_block(current_block.id)
                .map(|(s, _)| s);
            let next_rope_byte_opt = head_rope_start.map(|s| {
                rope_replace_block_content(&store, current_block.id, &block_plain);
                s + block_plain.len() as u32
            });

            let mut running_position =
                current_block.document_position + block_char_length(&updated_current, &store) + 1;

            let mut new_block_ids: Vec<EntityId> = Vec::new();
            if !skip_tail {
                let tail_block = Block {
                    id: 0,
                    created_at: now,
                    updated_at: now,
                    list: None,
                    document_position: running_position,
                    fmt_alignment: None,
                    fmt_top_margin: None,
                    fmt_bottom_margin: None,
                    fmt_left_margin: None,
                    fmt_right_margin: None,
                    fmt_heading_level: None,
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
                let created_tail =
                    uow.create_block(&tail_block, frame_id, (block_idx + 1) as i32)?;
                write_block_state(
                    uow,
                    created_tail.id,
                    right_runs.clone(),
                    right_images.clone(),
                );
                // Mirror the tail block into the rope.
                if let Some(next_rope_byte) = next_rope_byte_opt {
                    rope_insert_block_at(&store, next_rope_byte, created_tail.id, &text_after);
                }

                new_block_ids.push(created_tail.id);
                running_position += block_char_length(&created_tail, &store) + 1;
            }

            if !new_block_ids.is_empty() {
                let mut updated_frame = frame.clone();
                let child_order_insert_pos = (block_idx + 1).min(updated_frame.child_order.len());
                for (i, &block_id) in new_block_ids.iter().enumerate() {
                    updated_frame
                        .child_order
                        .insert(child_order_insert_pos + i, block_id as i64);
                }
                updated_frame.updated_at = now;
                updated_frame.blocks =
                    uow.get_frame_relationship(&frame_id, &FrameRelationshipField::Blocks)?;
                uow.update_frame(&updated_frame)?;
            }

            let blocks_added: i64 = if skip_tail { 0 } else { 1 };
            let head_delta =
                block_char_length(&updated_current, &store) - original_current_char_length;
            let original_next_pos =
                current_block.document_position + original_current_char_length + 1;
            let pos_shift = running_position - original_next_pos;

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
            updated_doc.character_count += head_delta;
            updated_doc.updated_at = now;
            uow.update_document(&updated_doc)?;

            Ok((
                current_block.document_position + block_char_length(&updated_current, &store),
                blocks_added,
                snapshot,
            ))
        } else {
            // Normal path: text_before is not empty.
            let mut updated_current = current_block.clone();
            updated_current.updated_at = now;
            uow.update_block(&updated_current)?;
            write_block_state(
                uow,
                current_block.id,
                left_runs.clone(),
                left_images.clone(),
            );

            // Mirror the head's new content into the rope (skipped
            // when the head wasn't in the rope, e.g. unseeded tests).
            // Lookup is in its own scope to drop the
            // `block_offsets.read()` guard before
            // `rope_replace_block_content` takes the write guard —
            // otherwise the same-thread upgrade deadlocks.
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
                fmt_line_height: parsed.line_height,
                fmt_non_breakable_lines: parsed.non_breakable_lines,
                fmt_direction: parsed.direction.clone(),
                fmt_background_color: parsed.background_color.clone(),
                fmt_is_code_block: None,
                fmt_code_language: None,
                fmt_hyphenate: None,
                fmt_language: None,
            };

            let created_block = uow.create_block(&new_block, frame_id, (block_idx + 1) as i32)?;
            write_block_state(uow, created_block.id, block_runs, Vec::new());

            // Mirror the inserted block into the rope (skipped when
            // the head wasn't in the rope).
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
            write_block_state(
                uow,
                created_tail.id,
                right_runs.clone(),
                right_images.clone(),
            );

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
            let original_next_pos =
                current_block.document_position + original_current_char_length + 1;
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
}

fn execute_insert_html(
    uow: &mut Box<dyn InsertHtmlAtPositionUnitOfWorkTrait>,
    dto: &InsertHtmlAtPositionDto,
) -> Result<(InsertHtmlAtPositionResultDto, EntityTreeSnapshot)> {
    let parsed_blocks = content_parser::parse_html(&dto.html);
    let (new_position, blocks_added, snapshot) =
        execute_content_insert(uow, dto.position, dto.anchor, &parsed_blocks)?;
    Ok((
        InsertHtmlAtPositionResultDto {
            new_position,
            blocks_added,
        },
        snapshot,
    ))
}

pub struct InsertHtmlAtPositionUseCase {
    uow_factory: Box<dyn InsertHtmlAtPositionUnitOfWorkFactoryTrait>,
    undo_snapshot: Option<EntityTreeSnapshot>,
    last_dto: Option<InsertHtmlAtPositionDto>,
}

impl InsertHtmlAtPositionUseCase {
    pub fn new(uow_factory: Box<dyn InsertHtmlAtPositionUnitOfWorkFactoryTrait>) -> Self {
        InsertHtmlAtPositionUseCase {
            uow_factory,
            undo_snapshot: None,
            last_dto: None,
        }
    }

    pub fn execute(
        &mut self,
        dto: &InsertHtmlAtPositionDto,
    ) -> Result<InsertHtmlAtPositionResultDto> {
        let mut uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let (result, snapshot) = execute_insert_html(&mut uow, dto)?;
        self.undo_snapshot = Some(snapshot);
        self.last_dto = Some(dto.clone());

        uow.commit()?;
        Ok(result)
    }
}

impl UndoRedoCommand for InsertHtmlAtPositionUseCase {
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
        let (_, snapshot) = execute_insert_html(&mut uow, &dto)?;
        self.undo_snapshot = Some(snapshot);
        uow.commit()?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
