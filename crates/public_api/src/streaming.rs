// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Append-only streaming: the log/console path.
//!
//! A view tailing output does two things in a loop — append a line at the end,
//! and drop the oldest once it is over its scrollback cap. Both are expressible
//! with the ordinary editing API, and both are O(N) that way: appending one line
//! to a 10 000-line document costs ~15.9 ms, and evicting 20 lines ~55 ms
//! (`docs/streaming-baseline.md`). Neither cost is inherent — it comes from
//! general-purpose machinery that a tail-append does not need:
//!
//! * `character_count()` / `cursor_at()` each run `get_document_stats`, which
//!   materializes every block's text to compute a word count the caller never
//!   asked for — just to locate the end of the document.
//! * `insert_block` fetches *every* block entity (via
//!   `collect_block_ids_recursive` and `get_block_multi`) before it can find
//!   the insertion point. `insert_text` has an O(log n) rope fast path;
//!   `insert_block` never received one.
//! * `delete_text` walks the whole `child_order` refreshing every block's stored
//!   position, with a rope lookup per block, regardless of how little was
//!   deleted.
//!
//! None of that is needed when the insertion point is, by construction, the end:
//! the rope already knows where the end is, nothing after it needs shifting, and
//! no block above it is touched. This module takes that shortcut directly, via
//! the `rope_helpers` primitives (`rope_append_block` is a rope insert at the
//! tail plus an O(1) amortized `push_block` — nothing shifts, because appending
//! at the end shifts nothing).
//!
//! # Not editing
//!
//! These are *not* undoable and deliberately so: a million appended lines must
//! not become a million undo entries, and clearing the shared stack per line
//! would destroy the user's real history. The entity writes are therefore routed
//! through a private throwaway stack that is cleared as they go, leaving the
//! document's own undo stack untouched. Interleaving these with ordinary editing
//! on the same document is not meaningful; they are for buffers whose content
//! arrives from elsewhere.
//!
//! # Residual cost
//!
//! `Frame.child_order` is a `Vec<i64>`, so appending clones it — an O(N) memcpy
//! (tens of microseconds at 100 k lines). Small against the 15.9 ms it replaces,
//! but genuinely not O(1); removing it would mean reshaping a core entity that
//! the whole engine and its property tests depend on.

use frontend::block::dtos::CreateBlockDto;
use frontend::commands::{block_commands, document_commands, frame_commands, undo_redo_commands};
use frontend::common::types::EntityId;
use frontend::document::dtos::UpdateDocumentDto;
use frontend::frame::dtos::UpdateFrameDto;

use crate::document::get_main_frame_id;
use crate::events::DocumentEvent;
use crate::inner::TextDocumentInner;
use crate::{DocumentError, Result, TextDocument};

impl TextDocument {
    /// Append `text` as a new block at the end, returning the document's new
    /// block count.
    ///
    /// The append half of a streaming buffer. Costs the rope insert and one
    /// entity write, whatever the buffer already holds — against ~15.9 ms per
    /// line at 10 000 lines through the ordinary editing path
    /// (`docs/streaming-baseline.md`).
    ///
    /// The returned count is what a scrollback cap is checked against, so a
    /// caller never needs a separate count call — which matters, because
    /// [`block_count`](Self::block_count) walks the whole document:
    ///
    /// ```ignore
    /// let count = doc.append_line(line)?;
    /// if count > CAP {
    ///     doc.truncate_front(count - CAP)?;
    /// }
    /// ```
    ///
    /// `text` is taken as a single line: it must not contain `\n`, since a block
    /// is one line by construction here (embedded newlines would desynchronize
    /// the rope from the block index). Returns [`DocumentError::InvalidArgument`]
    /// if it does.
    ///
    /// **Not undoable**, by design — see the module docs.
    pub fn append_line(&self, text: &str) -> Result<usize> {
        if text.contains('\n') {
            return Err(DocumentError::InvalidArgument(
                "append_line takes a single line; text must not contain '\\n'".into(),
            ));
        }

        let mut inner = self.inner.lock();
        let frame_id = get_main_frame_id(&inner);
        if frame_id == 0 {
            return Err(DocumentError::InvalidArgument(
                "document has no main frame".into(),
            ));
        }

        let edit_pos = append_one(&mut inner, frame_id, text)?;
        let new_count = commit_counts(&mut inner, 1, text.chars().count() as i64)?;
        finish(&mut inner, edit_pos, 0, text.chars().count() + 1);

        inner.queue_event(DocumentEvent::BlockCountChanged(new_count));
        // A pure tail append: the new element lands at the end of the flow, so
        // the index is the previous count. Emitted directly rather than through
        // the generic `check_flow_changed`, which diffs the whole `child_order`
        // on every edit — O(N) again, to rediscover what is known here.
        inner.queue_event(DocumentEvent::FlowElementsInserted {
            flow_index: new_count - 1,
            count: 1,
        });
        Ok(new_count)
    }

    /// Append several lines in one go, returning the document's new block count.
    ///
    /// Equivalent to [`append_line`](Self::append_line) per line, but pays the
    /// document-count write and the throwaway-stack clear once for the batch
    /// rather than per line. A view draining a channel once per frame should
    /// prefer this.
    ///
    /// No line may contain `\n`.
    pub fn append_lines<I, S>(&self, lines: I) -> Result<usize>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let lines: Vec<String> = lines.into_iter().map(|s| s.as_ref().to_owned()).collect();
        if lines.iter().any(|l| l.contains('\n')) {
            return Err(DocumentError::InvalidArgument(
                "append_lines takes single lines; none may contain '\\n'".into(),
            ));
        }
        if lines.is_empty() {
            let inner = self.inner.lock();
            return Ok(current_block_count(&inner)? as usize);
        }

        let mut inner = self.inner.lock();
        let frame_id = get_main_frame_id(&inner);
        if frame_id == 0 {
            return Err(DocumentError::InvalidArgument(
                "document has no main frame".into(),
            ));
        }

        let first_edit_pos = {
            let mut first = None;
            let mut chars_added = 0usize;
            for line in &lines {
                let pos = append_one(&mut inner, frame_id, line)?;
                first.get_or_insert(pos);
                chars_added += line.chars().count() + 1;
            }
            (first.unwrap_or(0), chars_added)
        };

        let chars: i64 = lines.iter().map(|l| l.chars().count() as i64).sum();
        let new_count = commit_counts(&mut inner, lines.len() as i64, chars)?;
        finish(&mut inner, first_edit_pos.0, 0, first_edit_pos.1);

        inner.queue_event(DocumentEvent::BlockCountChanged(new_count));
        inner.queue_event(DocumentEvent::FlowElementsInserted {
            flow_index: new_count - lines.len(),
            count: lines.len(),
        });
        Ok(new_count)
    }

    /// Drop the first `n` blocks, returning how many were actually removed.
    ///
    /// The eviction half of a streaming buffer. Returns less than `n` when the
    /// document holds fewer blocks; a document is never emptied completely —
    /// one block always remains, since an empty document is not a valid state
    /// here (it is created with one block, and the rest of the API assumes at
    /// least one exists).
    ///
    /// **Not undoable**, by design — see the module docs.
    pub fn truncate_front(&self, n: usize) -> Result<usize> {
        if n == 0 {
            return Ok(0);
        }

        let mut inner = self.inner.lock();
        let frame_id = get_main_frame_id(&inner);
        if frame_id == 0 {
            return Err(DocumentError::InvalidArgument(
                "document has no main frame".into(),
            ));
        }

        let stack = streaming_stack(&mut inner);

        // Which blocks to drop, capped so that at least one always survives.
        let (victims, chars_removed) = {
            let frame = frame_commands::get_frame(&inner.ctx, &frame_id)?
                .ok_or_else(|| DocumentError::InvalidArgument("main frame missing".into()))?;
            let block_ids: Vec<EntityId> = frame
                .child_order
                .iter()
                .filter(|e| **e > 0)
                .map(|e| *e as EntityId)
                .collect();
            let take = n.min(block_ids.len().saturating_sub(1));
            let victims: Vec<EntityId> = block_ids.into_iter().take(take).collect();

            let store = inner.ctx.db_context.get_store();
            let chars: i64 = victims
                .iter()
                .filter_map(|id| frontend::commands::block_commands::get_block(&inner.ctx, id).ok())
                .flatten()
                .map(|b| {
                    let entity: common::entities::Block = b.into();
                    common::database::rope_helpers::block_char_length(&entity, store)
                })
                .sum();
            (victims, chars)
        };

        if victims.is_empty() {
            return Ok(0);
        }

        // Unmirror from the rope first: `rope_remove_block` resolves each block
        // through the offset index, which the entity must still exist for.
        {
            let store = inner.ctx.db_context.get_store();
            for id in &victims {
                common::database::rope_helpers::rope_remove_block(store, *id);
            }
        }

        // Drop the entities, then the frame's references to them in one write.
        for id in &victims {
            block_commands::remove_block(&inner.ctx, Some(stack), id)?;
        }
        {
            let frame = frame_commands::get_frame(&inner.ctx, &frame_id)?
                .ok_or_else(|| DocumentError::InvalidArgument("main frame missing".into()))?;
            let mut update: UpdateFrameDto = frame.into();
            // Drop exactly the evicted ids, rather than the first N entries:
            // `remove_block` may already have pruned them, and `child_order`
            // can hold negative entries (sub-frames) that are not blocks, so
            // positional removal would take out survivors.
            let evicted: std::collections::HashSet<i64> =
                victims.iter().map(|id| *id as i64).collect();
            let before = update.child_order.len();
            update.child_order.retain(|e| !evicted.contains(e));
            if update.child_order.len() != before {
                frame_commands::update_frame(&inner.ctx, Some(stack), &update)?;
            }
        }

        let removed = victims.len();
        let new_count = commit_counts(&mut inner, -(removed as i64), -chars_removed)?;
        undo_redo_commands::clear_stack(&inner.ctx, stack);

        // Everything shifts down by what was cut off the front.
        inner.adjust_cursors(0, chars_removed as usize + removed, 0);
        inner.modified = true;
        inner.queue_event(DocumentEvent::ContentsChanged {
            position: 0,
            chars_removed: chars_removed as usize + removed,
            chars_added: 0,
            blocks_affected: removed,
        });
        inner.queue_event(DocumentEvent::BlockCountChanged(new_count));
        inner.queue_event(DocumentEvent::FlowElementsRemoved {
            flow_index: 0,
            count: removed,
        });
        Ok(removed)
    }
}

/// The document's block count, read straight off the entity.
///
/// [`TextDocument::block_count`] answers the same question via
/// `get_document_stats`, which also computes a word count by materializing every
/// block's text — 3.18 ms at 10 000 lines. This is the cached value the entity
/// already carries, which is what `inner.rs`'s own `check_block_count_changed`
/// reads for exactly the same reason.
fn current_block_count(inner: &TextDocumentInner) -> Result<i64> {
    let doc = document_commands::get_document(&inner.ctx, &inner.document_id)?
        .ok_or_else(|| DocumentError::InvalidArgument("document missing".into()))?;
    Ok(doc.block_count)
}

/// A private undo stack for streaming writes, created on first use.
///
/// The entity commands always push an undo command — passing `None` does not
/// opt out, it resolves to stack 0 (`add_command_to_stack`). Routing these
/// writes to a stack of their own, cleared as they go, keeps them off the
/// document's real history without bounding-problems or clearing what the user
/// did.
fn streaming_stack(inner: &mut TextDocumentInner) -> u64 {
    if let Some(id) = inner.streaming_stack_id {
        return id;
    }
    let id = undo_redo_commands::create_new_stack(&inner.ctx);
    inner.streaming_stack_id = Some(id);
    id
}

/// Create one block, mirror it into the rope at the tail, and reference it from
/// the frame. Returns the document position the new content starts at.
fn append_one(inner: &mut TextDocumentInner, frame_id: EntityId, text: &str) -> Result<usize> {
    let stack = streaming_stack(inner);

    let block = block_commands::create_block(
        &inner.ctx,
        Some(stack),
        &CreateBlockDto::default(),
        frame_id,
        -1,
    )?;

    // Mirror into the rope. The generic create path does not touch it, which is
    // why `initialize` does the same thing for the document's first block.
    let edit_pos = {
        let store = inner.ctx.db_context.get_store();
        let was_empty = store.rope.read().len_bytes() == 0;
        if !was_empty {
            // Blocks after the first in a frame are separated by a `\n`
            // sentinel; `rope_append_block` does not add one itself.
            common::database::rope_helpers::rope_insert_block_boundary(store);
        }
        let byte_start = common::database::rope_helpers::rope_append_block(store, block.id, text);
        store.rope.read().byte_to_char(byte_start as usize)
    };

    // The generic create path adds the block to the junction table but not to
    // `child_order`, which is what `flow()` reads — `initialize` notes the same.
    let frame = frame_commands::get_frame(&inner.ctx, &frame_id)?
        .ok_or_else(|| DocumentError::InvalidArgument("main frame missing".into()))?;
    let mut update: UpdateFrameDto = frame.into();
    update.child_order.push(block.id as i64);
    frame_commands::update_frame(&inner.ctx, Some(stack), &update)?;

    Ok(edit_pos)
}

/// Apply deltas to the document's cached counts, returning the new block count.
fn commit_counts(
    inner: &mut TextDocumentInner,
    block_delta: i64,
    char_delta: i64,
) -> Result<usize> {
    let doc = document_commands::get_document(&inner.ctx, &inner.document_id)?
        .ok_or_else(|| DocumentError::InvalidArgument("document missing".into()))?;
    let stack = streaming_stack(inner);
    let mut update: UpdateDocumentDto = doc.into();
    update.block_count = (update.block_count + block_delta).max(0);
    update.character_count = (update.character_count + char_delta).max(0);
    let new_count = update.block_count as usize;
    document_commands::update_document(&inner.ctx, Some(stack), &update)?;
    Ok(new_count)
}

/// Post-append bookkeeping shared by the append entry points.
fn finish(inner: &mut TextDocumentInner, edit_pos: usize, removed: usize, added: usize) {
    let stack = streaming_stack(inner);
    // Discard the throwaway history: these writes are not undoable, and letting
    // it grow would leak a command per appended line.
    undo_redo_commands::clear_stack(&inner.ctx, stack);

    inner.adjust_cursors(edit_pos, removed, added);
    inner.modified = true;
    inner.queue_event(DocumentEvent::ContentsChanged {
        position: edit_pos,
        chars_removed: removed,
        chars_added: added,
        blocks_affected: 1,
    });
}
