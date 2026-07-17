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
//! # Not undoable — and not an oversight
//!
//! **Nothing in this module participates in undo/redo.** An appended or evicted
//! line cannot be brought back with [`TextDocument::undo`], does not appear on
//! the undo stack, and does not affect [`TextDocument::can_undo`].
//!
//! That is deliberate on both counts. A million streamed lines must not become a
//! million undo entries — the stack is unbounded, so that is a leak, not a
//! feature. And the alternative escape hatch the rest of the crate uses for
//! non-undoable work (perform, then `clear_stack`, as `set_plain_text` does)
//! would throw away the *user's* history every time a line arrived from a
//! background thread.
//!
//! So the entity writes are routed through a private stack of their own, cleared
//! as they go. Note that passing `None` as the stack id would not have achieved
//! this: `add_command_to_stack` resolves `None` to stack 0 rather than skipping,
//! so it is not an opt-out.
//!
//! Consequences worth being deliberate about:
//!
//! * **The document's own undo stack is untouched.** A user's edits stay
//!   undoable across any amount of streaming, and undo will never surface or
//!   swallow a streamed line. This is covered by a test.
//! * **Streaming is not reversible.** Content that arrived from elsewhere is not
//!   the user's to undo; if a caller needs to drop it, that is
//!   [`truncate_front`](TextDocument::truncate_front), not undo.
//! * **`modified` is still set**, and change events are still emitted, so views
//!   and dirty-tracking behave normally.
//!
//! Mixing streaming with ordinary editing on one document works and is tested
//! (the rope and the block index stay coherent), but it is not the intended
//! shape: these are for buffers whose content arrives from elsewhere.
//!
//! # All-or-nothing
//!
//! Every entry point here is atomic: it either lands completely or leaves the
//! document exactly as it was. That needs saying because it does not come for
//! free. These paths hand-maintain four things that must agree — the rope, the
//! block offset index, the frame's `child_order`, and the document's cached
//! counts — across several fallible calls, and the unit-of-work layer that
//! normally guarantees that is precisely what they bypass for speed. Without
//! care, any early `?` would leave text in the rope that no block owns, or a
//! block the frame never references, or counts describing a document that no
//! longer exists — none of which surfaces as an error, only as a document that
//! quietly disagrees with itself.
//!
//! So each operation takes a savepoint first and rolls back on any failure,
//! including a panic. A savepoint is a whole-store snapshot, but a cheap one:
//! the rope is persistent and the entity tables are `im::HashMap`s, so it is
//! pointer copies, not a deep clone.
//!
//! Note that a rolled-back append still consumes the entity ids it allocated —
//! the store's counters are deliberately not rewound — so ids may show gaps.
//! Nothing depends on them being contiguous.
//!
//! # This is still O(N) — know the ceiling before using it
//!
//! Measured, appending one line (`docs/streaming-baseline.md`):
//!
//! | lines in document | ordinary editing path | this |
//! |---|---|---|
//! | 1 000 | 986 µs | 174 µs |
//! | 10 000 | 28.1 ms | 2.90 ms |
//!
//! Roughly 10× cheaper — but 174 µs → 2.90 ms is 16.7× for 10× the document, so
//! **this is still O(N), not O(1)**. It moves the ceiling; it does not remove it.
//! Around 10 000 lines an append costs ~2.9 ms (~345 lines/second), which suits
//! an application's own event log. It does not suit `tail -f` of a large file:
//! at 100 000 lines the cost extrapolates to ~29 ms per line. Cap the buffer
//! accordingly, and evict with [`truncate_front`](TextDocument::truncate_front).
//!
//! `truncate_front` is barely better than the ordinary path (1.19× at 10 000)
//! and is *slower* below a few thousand lines, because the ordinary path makes a
//! single bulk `delete_text` call where this makes one entity removal per evicted
//! block. It exists for the coherence of the streaming API — appending without
//! being able to evict is useless — not because it is fast.
//!
//! The rope is not the reason. `rope_append_block` is O(log n) as intended; the
//! residual is the generic entity-update path. `UndoableUpdateUseCase::
//! execute_multi` fetches the old entity for undo and stashes it on every write,
//! so each `update_frame` clones the `Frame` — its whole `child_order: Vec<i64>`
//! included — several times over. Every appended line therefore touches the
//! entire `child_order`, whatever the rope does.
//!
//! Reaching O(1) means `child_order` no longer being an N-element `Vec` cloned
//! per write. That reshapes a core entity the whole engine and its property tests
//! depend on, and is deliberately out of scope here.

use std::sync::Arc;

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

        // Everything below either all lands or none of it does.
        let atomic = Atomic::begin(&inner);
        let appended = append_one(&mut inner, frame_id, text)?;
        let new_count = commit_counts(&mut inner, 1, text.chars().count() as i64)?;
        atomic.commit();

        finish(&mut inner, appended.edit_pos, appended.chars_added, 1);

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
    ///
    /// **Not undoable**, by design — see the module docs.
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

        // The batch is one unit: a line failing halfway would otherwise leave
        // its predecessors appended with the document's counts never updated,
        // since those are written once at the end.
        let atomic = Atomic::begin(&inner);
        let mut edit_pos = None;
        let mut chars_added = 0usize;
        for line in &lines {
            let appended = append_one(&mut inner, frame_id, line)?;
            // The batch's edit starts where its first line did.
            edit_pos.get_or_insert(appended.edit_pos);
            chars_added += appended.chars_added;
        }
        let edit_pos = edit_pos.unwrap_or(0);

        let chars: i64 = lines.iter().map(|l| l.chars().count() as i64).sum();
        let new_count = commit_counts(&mut inner, lines.len() as i64, chars)?;
        atomic.commit();

        finish(&mut inner, edit_pos, chars_added, lines.len());

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

            // Every victim's length must be counted, so a lookup that fails is
            // an error rather than a silent zero: swallowing it would subtract
            // too little from `character_count`, and since all later deltas are
            // relative, the document's count would stay wrong forever with
            // nothing reporting it.
            let mut chars: i64 = 0;
            for id in &victims {
                let block = block_commands::get_block(&inner.ctx, id)?.ok_or_else(|| {
                    DocumentError::InvalidArgument(format!(
                        "block {id} is referenced by the frame but does not exist"
                    ))
                })?;
                let entity: common::entities::Block = block.into();
                let store = inner.ctx.db_context.get_store();
                chars += common::database::rope_helpers::block_char_length(&entity, store);
            }
            (victims, chars)
        };

        if victims.is_empty() {
            return Ok(0);
        }

        // Everything below either all lands or none of it does. Without this,
        // a failure partway through the entity loop would leave the rope
        // already stripped of victims whose blocks still exist and are still
        // referenced by the frame.
        let atomic = Atomic::begin(&inner);

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
        atomic.commit();

        undo_redo_commands::clear_stack(&inner.ctx, stack);

        // Everything shifts down by what was cut off the front.
        inner.invalidate_text_cache();
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

/// All-or-nothing around a streaming mutation.
///
/// These paths hand-maintain four things that must agree — the rope, the block
/// offset index, the frame's `child_order`, and the document's cached counts —
/// across several fallible calls. The ordinary editing path gets that
/// consistency from the unit-of-work layer; taking the shortcut past it takes
/// the shortcut past its atomicity too, so every early `?` would otherwise be a
/// chance to leave the four disagreeing: text in the rope that no block owns, a
/// block absent from `child_order`, counts describing a document that no longer
/// exists.
///
/// A savepoint is a whole-store snapshot, but a cheap one — the rope is a
/// persistent structure and the entity tables are `im::HashMap`s, so it is
/// pointer-copies rather than a deep clone. Restoring it undoes everything
/// since, the inner commands' own writes included, because each savepoint is an
/// independent snapshot rather than a stack position.
///
/// Deliberately not `Transaction`, which is the same savepoint plus an
/// O(N) `recompute_all_frame_byte_ranges` on commit — a cost this path exists
/// to avoid, and one the inner commands' own transactions already pay.
///
/// Rolls back on drop unless [`commit`](Self::commit) ran, so an early `?` — or
/// a panic — cannot leave the document half-written.
struct Atomic {
    /// Owned rather than borrowed: holding a reference would borrow the
    /// document's interior for the guard's whole lifetime, which is exactly the
    /// span in which the work needs `&mut` access to it.
    store: Arc<common::database::Store>,
    savepoint: Option<u64>,
}

impl Atomic {
    fn begin(inner: &TextDocumentInner) -> Self {
        let store = Arc::clone(inner.ctx.db_context.get_store());
        let savepoint = Some(store.create_savepoint());
        Self { store, savepoint }
    }

    /// Keep the mutations and drop the snapshot.
    fn commit(mut self) {
        if let Some(sp) = self.savepoint.take() {
            self.store.discard_savepoint(sp);
        }
    }
}

impl Drop for Atomic {
    fn drop(&mut self) {
        // Still holding the savepoint means `commit` never ran: the operation
        // left early, so put the document back exactly as it was.
        if let Some(sp) = self.savepoint.take() {
            self.store.restore_savepoint(sp);
            self.store.discard_savepoint(sp);
        }
    }
}

/// What one appended line did to the document, as the caller's bookkeeping
/// needs to describe it.
struct Appended {
    /// Document position the insertion began at.
    edit_pos: usize,
    /// Characters inserted there — the text, plus the separator when one was
    /// needed. Not derivable from the text alone, which is the whole reason
    /// this is reported rather than recomputed.
    chars_added: usize,
}

/// Create one block, mirror it into the rope at the tail, and reference it from
/// the frame.
fn append_one(inner: &mut TextDocumentInner, frame_id: EntityId, text: &str) -> Result<Appended> {
    let stack = streaming_stack(inner);

    // Read the frame *before* creating the block: the generic create path adds
    // the block to the junction table but not to `child_order`, so this sees
    // exactly the blocks that already existed.
    let frame = frame_commands::get_frame(&inner.ctx, &frame_id)?
        .ok_or_else(|| DocumentError::InvalidArgument("main frame missing".into()))?;

    // Blocks after the first in a frame are separated by a `\n` sentinel, and
    // `rope_append_block` does not add one itself.
    //
    // The condition is "does the frame already hold a block", NOT "is the rope
    // non-empty". A freshly created document already holds one *empty* block,
    // registered at byte 0, with an empty rope — so keying off the rope would
    // skip the separator and register the new block at byte 0 as well, leaving
    // two blocks at the same offset with no `\n` between them. That is not a
    // hypothetical: `TextDocument::new()` followed by `append_line` is the most
    // direct way to use this API.
    let needs_boundary = frame.child_order.iter().any(|entry| *entry > 0);

    let block = block_commands::create_block(
        &inner.ctx,
        Some(stack),
        &CreateBlockDto::default(),
        frame_id,
        -1,
    )?;

    let appended = {
        let store = inner.ctx.db_context.get_store();
        // Where the insertion begins — captured before it, so it covers the
        // separator too when one is written.
        let edit_pos = store.rope.read().len_chars();
        if needs_boundary {
            common::database::rope_helpers::rope_insert_block_boundary(store);
        }
        common::database::rope_helpers::rope_append_block(store, block.id, text);
        Appended {
            edit_pos,
            chars_added: text.chars().count() + usize::from(needs_boundary),
        }
    };

    // The generic create path adds the block to the junction table but not to
    // `child_order`, which is what `flow()` reads — `initialize` notes the same.
    let mut update: UpdateFrameDto = frame.into();
    update.child_order.push(block.id as i64);
    frame_commands::update_frame(&inner.ctx, Some(stack), &update)?;

    Ok(appended)
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
///
/// `blocks_affected` is passed rather than assumed: a batch adds as many blocks
/// as it has lines, and consumers size work off that number.
fn finish(inner: &mut TextDocumentInner, edit_pos: usize, added: usize, blocks_affected: usize) {
    let stack = streaming_stack(inner);
    // Discard the throwaway history: these writes are not undoable, and letting
    // it grow would leak a command per appended line.
    undo_redo_commands::clear_stack(&inner.ctx, stack);

    // The document's text just changed, so the lazily-built plain-text cache no
    // longer describes it. Every ordinary edit clears this; bypassing the
    // editing path means bypassing that too, and a stale cache is silent —
    // `blocks()` reads the live document while `to_plain_text()` keeps serving
    // whatever the text was when it was last asked.
    inner.invalidate_text_cache();
    inner.adjust_cursors(edit_pos, 0, added);
    inner.modified = true;
    inner.queue_event(DocumentEvent::ContentsChanged {
        position: edit_pos,
        chars_removed: 0,
        chars_added: added,
        blocks_affected,
    });
}
