//! Document event types and subscription handle.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::inner::{CallbackEntry, TextDocumentInner};

/// Events emitted by a [`TextDocument`](crate::TextDocument).
///
/// Subscribe via [`TextDocument::on_change`](crate::TextDocument::on_change) (callback-based)
/// or poll via [`TextDocument::poll_events`](crate::TextDocument::poll_events) (frame-loop).
///
/// These events carry enough information for a UI to do incremental updates —
/// repaint only the affected region, not the entire document.
#[derive(Debug, Clone, PartialEq)]
pub enum DocumentEvent {
    /// Text content changed at a specific region.
    ///
    /// Emitted by: `insert_text`, `delete_char`, `delete_previous_char`,
    /// `remove_selected_text`, `insert_formatted_text`, `insert_block`,
    /// `insert_html`, `insert_markdown`, `insert_fragment`, `insert_image`.
    ContentsChanged {
        position: usize,
        chars_removed: usize,
        chars_added: usize,
        blocks_affected: usize,
    },

    /// Formatting changed without text content change.
    FormatChanged {
        position: usize,
        length: usize,
        /// Distinguishes block-level changes (relayout needed) from
        /// character-level changes (reshaping only).
        kind: crate::flow::FormatChangeKind,
    },

    /// Only paint-level highlight attributes changed (colors, underline
    /// decorations) on a paint-only highlighter. The shaping input
    /// (`fragments`) is unchanged, so the layout engine can recolor the
    /// cached layout without reshaping or reflowing.
    ///
    /// `position` / `length` are document-absolute character offsets. For
    /// whole-document rehighlights both are 0; for single-block
    /// rehighlights they bound the affected block.
    HighlightPaintChanged { position: usize, length: usize },

    /// Block count changed. Carries the new count.
    BlockCountChanged(usize),

    /// Flow elements were inserted at the given index in the main
    /// frame's `child_order`.
    ///
    /// This is a performance optimization — the layout engine can
    /// update incrementally instead of re-querying
    /// [`TextDocument::flow()`](crate::TextDocument::flow).
    FlowElementsInserted { flow_index: usize, count: usize },

    /// Flow elements were removed starting at the given index in the
    /// main frame's `child_order`.
    FlowElementsRemoved { flow_index: usize, count: usize },

    /// The document was completely replaced (import, clear).
    DocumentReset,

    /// Undo/redo was performed or availability changed.
    UndoRedoChanged { can_undo: bool, can_redo: bool },

    /// The modified flag changed.
    ModificationChanged(bool),

    /// A long operation progressed.
    LongOperationProgress {
        operation_id: String,
        percent: f64,
        message: String,
    },

    /// A long operation completed or failed.
    LongOperationFinished {
        operation_id: String,
        success: bool,
        error: Option<String>,
    },
}

/// Handle to a document event subscription.
///
/// Events are delivered as long as this handle is alive.
/// Drop it to unsubscribe. No explicit unsubscribe method needed.
pub struct Subscription {
    alive: Arc<AtomicBool>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.alive
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Register a callback with the document inner, returning a Subscription handle.
pub(crate) fn subscribe_inner<F>(inner: &mut TextDocumentInner, callback: F) -> Subscription
where
    F: Fn(DocumentEvent) + Send + Sync + 'static,
{
    let alive = Arc::new(AtomicBool::new(true));
    inner.callbacks.push(CallbackEntry {
        alive: Arc::downgrade(&alive),
        callback: Arc::new(callback),
    });
    Subscription { alive }
}
