//! Shared document interior state.
//!
//! # Lock ordering (enforced by convention, never violated)
//!
//! 1. `TextDocumentInner` (the document lock)
//! 2. `CursorData` (individual cursor locks)
//!
//! Always acquire the document lock before any cursor lock.
//! Pure cursor-local reads (position, anchor, has_selection) may lock
//! only CursorData — this is safe because they never touch the document
//! lock in the same call. Editing methods must lock the document first,
//! then read/update cursor data while the document lock is held, and
//! call `adjust_cursors()` before releasing the document lock.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

/// A batch of events paired with the callbacks to invoke for each.
pub(crate) type QueuedEvents = Vec<(
    crate::events::DocumentEvent,
    Vec<Arc<dyn Fn(crate::events::DocumentEvent) + Send + Sync>>,
)>;

use anyhow::Result;
use frontend::AppContext;
use frontend::EventHubClient;
use frontend::common::types::EntityId;
use frontend::event_hub_client::SubscriptionToken;

use crate::DocumentEvent;
use crate::highlight::HighlightData;

/// Cursor position data stored inside the document for automatic adjustment.
pub(crate) struct CursorData {
    pub position: usize,
    pub anchor: usize,
    /// When set, overrides the computed `SelectionKind` to force cell selection.
    /// Cleared whenever position or anchor changes via normal editing/movement.
    pub cell_selection_override: Option<crate::flow::CellRange>,
}

/// Callback entry for document event subscriptions.
pub(crate) struct CallbackEntry {
    pub alive: Weak<AtomicBool>,
    pub callback: Arc<dyn Fn(DocumentEvent) + Send + Sync>,
}

/// The shared document interior, behind `Arc<Mutex<>>`.
pub(crate) struct TextDocumentInner {
    pub ctx: AppContext,
    pub event_client: EventHubClient,
    pub stack_id: u64,
    #[allow(dead_code)] // will be used for entity tree access
    pub root_id: EntityId,
    pub document_id: EntityId,
    pub modified: bool,

    // Cursor tracking
    pub cursors: Vec<Weak<Mutex<CursorData>>>,

    // Event dispatch — two independent delivery paths:
    //
    // 1. **Callback path** (`on_change`): `take_queued_events()` reads from
    //    `callback_cursor` to the end of `pending_events`, advances the cursor,
    //    and returns events + callbacks for dispatch outside the lock.
    //
    // 2. **Polling path** (`poll_events`): `poll_events()` reads from
    //    `poll_cursor` to the end of `pending_events` and advances the cursor.
    //
    // The two paths are independent — using both simultaneously is fine.
    // Events are trimmed from the front of `pending_events` when both cursors
    // have advanced past them.
    pub pending_events: Vec<DocumentEvent>,
    pub callbacks: Vec<CallbackEntry>,
    /// Next event index for callback dispatch.
    pub callback_cursor: usize,
    /// Next event index for `poll_events()`.
    pub poll_cursor: usize,

    // Resource name → entity ID cache for O(1) lookups by name.
    pub resource_cache: HashMap<String, u64>,

    // Cached plain text for the entire document. Populated lazily, invalidated
    // on any edit or document reset. Avoids O(blocks) reconstruction per search.
    pub plain_text_cache: Option<String>,

    // Last known block count, used to detect changes and emit BlockCountChanged.
    pub last_block_count: usize,

    // Last known child_order of the main frame, used to detect flow changes
    // and emit FlowElementsInserted/FlowElementsRemoved.
    pub last_child_order: Vec<i64>,

    // Syntax highlighting state (shadow formatting layer).
    pub highlight: Option<HighlightData>,

    // Classification of the active highlighter, recomputed after every
    // rehighlight pass. Drives whether highlights are merged into the
    // shaping input or kept as a separate paint overlay.
    pub highlight_kind: crate::highlight::HighlighterKind,

    // Holds SubscriptionTokens for LongOperation event bridges. Dropping a
    // token unsubscribes the callback, so these must outlive the document.
    pub long_op_subscriptions: Vec<SubscriptionToken>,
}

impl TextDocumentInner {
    /// Remove dead `Weak` refs from the cursor list.
    pub fn prune_dead_cursors(&mut self) {
        self.cursors.retain(|w| w.strong_count() > 0);
    }

    /// After an edit, adjust all tracked cursor positions.
    ///
    /// Called while the document lock is held. Then locks individual
    /// CursorData mutexes (safe per lock ordering: doc before cursor).
    pub fn adjust_cursors(&mut self, edit_pos: usize, removed: usize, added: usize) {
        self.prune_dead_cursors();
        for weak in &self.cursors {
            if let Some(cursor) = weak.upgrade() {
                let mut data = cursor.lock();
                data.position = adjust_offset(data.position, edit_pos, removed, added);
                data.anchor = adjust_offset(data.anchor, edit_pos, removed, added);
                // Cell selection override references table coordinates that may be
                // invalidated by the edit, so always clear it.
                data.cell_selection_override = None;
            }
        }
    }

    /// Register a new cursor and return its shared data.
    pub fn register_cursor(&mut self, position: usize) -> Arc<Mutex<CursorData>> {
        self.prune_dead_cursors();
        let data = Arc::new(Mutex::new(CursorData {
            position,
            anchor: position,
            cell_selection_override: None,
        }));
        self.cursors.push(Arc::downgrade(&data));
        data
    }

    /// Queue a DocumentEvent for deferred dispatch.
    ///
    /// Events are collected while the lock is held, then dispatched
    /// after the lock is released via [`dispatch_queued_events`].
    pub fn queue_event(&mut self, event: DocumentEvent) {
        self.pending_events.push(event);
    }

    /// Return events added since the last `poll_events()` call.
    ///
    /// This path is independent of callback dispatch — using both
    /// `poll_events()` and `on_change()` simultaneously is safe and
    /// each path sees every event exactly once.
    pub fn drain_poll_events(&mut self) -> Vec<DocumentEvent> {
        if self.poll_cursor >= self.pending_events.len() {
            self.trim_delivered_events();
            return Vec::new();
        }
        let events = self.pending_events[self.poll_cursor..].to_vec();
        self.poll_cursor = self.pending_events.len();
        self.trim_delivered_events();
        events
    }

    /// Collect un-dispatched events paired with live callbacks.
    ///
    /// Only events added since the last call are returned. The caller
    /// should release the lock and then invoke callbacks via
    /// [`dispatch_queued_events`].
    pub fn take_queued_events(&mut self) -> QueuedEvents {
        if self.callback_cursor >= self.pending_events.len() {
            return Vec::new();
        }

        self.callbacks
            .retain(|entry| entry.alive.strong_count() > 0);

        let live_callbacks: Vec<Arc<dyn Fn(DocumentEvent) + Send + Sync>> = self
            .callbacks
            .iter()
            .filter_map(|entry| {
                let alive = entry.alive.upgrade()?;
                if alive.load(std::sync::atomic::Ordering::Relaxed) {
                    Some(Arc::clone(&entry.callback))
                } else {
                    None
                }
            })
            .collect();

        if live_callbacks.is_empty() {
            self.callback_cursor = self.pending_events.len();
            return Vec::new();
        }

        let new_events: Vec<DocumentEvent> = self.pending_events[self.callback_cursor..].to_vec();
        self.callback_cursor = self.pending_events.len();

        new_events
            .into_iter()
            .map(|e| (e, live_callbacks.clone()))
            .collect()
    }

    /// Discard events that both delivery paths have consumed.
    fn trim_delivered_events(&mut self) {
        let min_cursor = self.callback_cursor.min(self.poll_cursor);
        if min_cursor > 0 {
            self.pending_events.drain(..min_cursor);
            self.callback_cursor -= min_cursor;
            self.poll_cursor -= min_cursor;
        }
    }

    /// Invalidate the cached plain text. Call after any edit.
    pub fn invalidate_text_cache(&mut self) {
        self.plain_text_cache = None;
    }

    /// Check the current block count and queue a `BlockCountChanged` event if it changed.
    pub fn check_block_count_changed(&mut self) {
        let doc = frontend::commands::document_commands::get_document(&self.ctx, &self.document_id);
        if let Ok(Some(doc)) = doc {
            let new_count = doc.block_count as usize;
            if new_count != self.last_block_count {
                self.last_block_count = new_count;
                self.queue_event(crate::DocumentEvent::BlockCountChanged(new_count));
            }
        }
    }

    /// Check whether the main frame's `child_order` changed and queue
    /// `FlowElementsInserted` / `FlowElementsRemoved` events.
    ///
    /// Uses a simple diff: finds the first index where old and new diverge,
    /// then determines whether elements were inserted, removed, or replaced.
    pub fn check_flow_changed(&mut self) {
        let current = self.main_frame_child_order();
        if current == self.last_child_order {
            return;
        }

        let old = &self.last_child_order;
        let new = &current;

        // Find first index of divergence
        let prefix_len = old
            .iter()
            .zip(new.iter())
            .take_while(|(a, b)| a == b)
            .count();

        // Find common suffix length (after the divergent region)
        let old_remaining = old.len() - prefix_len;
        let new_remaining = new.len() - prefix_len;
        let suffix_len = old[prefix_len..]
            .iter()
            .rev()
            .zip(new[prefix_len..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();

        let removed_count = old_remaining - suffix_len;
        let inserted_count = new_remaining - suffix_len;

        if removed_count > 0 {
            self.queue_event(crate::DocumentEvent::FlowElementsRemoved {
                flow_index: prefix_len,
                count: removed_count,
            });
        }
        if inserted_count > 0 {
            self.queue_event(crate::DocumentEvent::FlowElementsInserted {
                flow_index: prefix_len,
                count: inserted_count,
            });
        }

        self.last_child_order = current;
    }

    /// Reset the cached child_order to the current state without emitting events.
    /// Call after a `DocumentReset` event, since the layout engine will do a full
    /// rebuild anyway.
    pub fn reset_cached_child_order(&mut self) {
        self.last_child_order = self.main_frame_child_order();
    }

    /// Read the main frame's current child_order from the database.
    fn main_frame_child_order(&self) -> Vec<i64> {
        let frames = frontend::commands::document_commands::get_document_relationship(
            &self.ctx,
            &self.document_id,
            &frontend::document::dtos::DocumentRelationshipField::Frames,
        )
        .unwrap_or_default();

        let main_frame_id = match frames.first() {
            Some(&id) => id,
            None => return Vec::new(),
        };

        frontend::commands::frame_commands::get_frame(&self.ctx, &(main_frame_id as u64))
            .ok()
            .flatten()
            .map(|f| f.child_order)
            .unwrap_or_default()
    }

    /// Get or lazily build the cached plain text.
    pub fn plain_text(&mut self) -> Result<&str> {
        if self.plain_text_cache.is_none() {
            let dto = frontend::commands::document_io_commands::export_plain_text(&self.ctx)?;
            self.plain_text_cache = Some(dto.plain_text);
        }
        Ok(self.plain_text_cache.as_deref().unwrap())
    }

    /// Initialize the document: create Root → Document → Frame → Block.
    pub fn initialize(ctx: AppContext) -> Result<Self> {
        use frontend::block::dtos::CreateBlockDto;
        use frontend::commands::{
            block_commands, document_commands, frame_commands, root_commands, undo_redo_commands,
        };
        use frontend::document::dtos::CreateDocumentDto;
        use frontend::frame::dtos::CreateFrameDto;
        use frontend::root::dtos::CreateRootDto;

        let event_client = EventHubClient::new(&ctx.event_hub);
        event_client.start(ctx.shutdown_rx.clone());

        let stack_id = undo_redo_commands::create_new_stack(&ctx);

        // Create entity tree: Root → Document → Frame → Block
        let root = root_commands::create_orphan_root(&ctx, &CreateRootDto::default())?;
        let doc = document_commands::create_document(
            &ctx,
            Some(stack_id),
            &CreateDocumentDto::default(),
            root.id,
            -1,
        )?;
        let frame = frame_commands::create_frame(
            &ctx,
            Some(stack_id),
            &CreateFrameDto::default(),
            doc.id,
            -1,
        )?;
        let block = block_commands::create_block(
            &ctx,
            Some(stack_id),
            &CreateBlockDto::default(),
            frame.id,
            -1,
        )?;

        // Register the initial empty block in `block_offsets` so
        // subsequent rope mirrors (insert_text, insert_fragment, etc.)
        // can resolve its byte position. The generic create_block CRUD
        // path doesn't touch the rope.
        common::database::rope_helpers::rope_append_empty_block(
            ctx.db_context.get_store(),
            block.id,
        );

        // Fix child_order on the main frame — the generic create_block path
        // adds the block to the blocks junction table but does not update
        // child_order. We must set it so that flow() works correctly.
        let mut frame_update: frontend::frame::dtos::UpdateFrameDto = frame.into();
        frame_update.child_order = vec![block.id as i64];
        frame_commands::update_frame(&ctx, Some(stack_id), &frame_update)?;

        // Clear undo stack — initialization shouldn't be undoable
        undo_redo_commands::clear_stack(&ctx, stack_id);

        Ok(Self {
            ctx,
            event_client,
            stack_id,
            root_id: root.id,
            document_id: doc.id,
            modified: false,
            cursors: Vec::new(),
            pending_events: Vec::new(),
            callbacks: Vec::new(),
            callback_cursor: 0,
            poll_cursor: 0,
            resource_cache: HashMap::new(),
            plain_text_cache: None,
            last_block_count: 1, // new document starts with one block
            last_child_order: vec![block.id as i64],
            highlight: None,
            highlight_kind: crate::highlight::HighlighterKind::None,
            long_op_subscriptions: Vec::new(),
        })
    }
}

impl Drop for TextDocumentInner {
    fn drop(&mut self) {
        self.ctx.shutdown();
    }
}

/// Dispatch queued events outside the lock.
///
/// Call this after releasing the document mutex to avoid deadlocks
/// when callbacks call back into the document.
pub(crate) fn dispatch_queued_events(queued: QueuedEvents) {
    for (event, callbacks) in queued {
        for cb in &callbacks {
            cb(event.clone());
        }
    }
}

/// Shift an offset after an edit: offsets before the edit are unchanged,
/// offsets inside the removed range clamp to the edit point, offsets after
/// shift by the delta.
pub(crate) fn adjust_offset(offset: usize, edit_pos: usize, removed: usize, added: usize) -> usize {
    if offset <= edit_pos {
        offset
    } else if offset <= edit_pos + removed {
        edit_pos + added
    } else {
        offset - removed + added
    }
}

/// Refresh `BlockDto::document_position` in-place using the rope-derived
/// position from `BlockOffsetIndex`. No-op for documents containing
/// tables or unmirrored sub-frames — for those, the stored field
/// (maintained by use-case-side position-refresh loops, plus catch-up
/// refreshes when tables are inserted) is the authoritative source.
pub(crate) fn refresh_block_positions(
    dtos: &mut [frontend::block::dtos::BlockDto],
    store: &frontend::common::database::Store,
) {
    if !frontend::common::database::rope_helpers::rope_positions_match_flow(store) {
        return;
    }
    let offsets = store.block_offsets.read();
    let rope = store.rope.read();
    for dto in dtos.iter_mut() {
        if let Some((byte_start, _)) = offsets.range_of_block(dto.id) {
            dto.document_position = rope.byte_to_char(byte_start as usize) as i64;
        }
    }
}

/// Like [`refresh_block_positions`] but for a single DTO.
pub(crate) fn refresh_block_position(
    dto: &mut frontend::block::dtos::BlockDto,
    store: &frontend::common::database::Store,
) {
    if !frontend::common::database::rope_helpers::rope_positions_match_flow(store) {
        return;
    }
    let offsets = store.block_offsets.read();
    if let Some((byte_start, _)) = offsets.range_of_block(dto.id) {
        let rope = store.rope.read();
        dto.document_position = rope.byte_to_char(byte_start as usize) as i64;
    }
}
