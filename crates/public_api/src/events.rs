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
/// Which channel some text arrived through.
///
/// A **fact about how the characters reached the document**, and nothing more.
/// It says who typed, pasted or dictated nothing at all: an application can only
/// report the channel it was called through, and the inference from a channel to
/// an author is not one any of this can make.
///
/// ## Why the default is `Unspecified` and not `Programmatic`
///
/// Every insertion method has a plain form and a `_with_origin` sibling. The
/// plain form reports [`Unspecified`](Self::Unspecified), which means *the
/// caller did not say* — deliberately **not** [`Programmatic`](Self::Programmatic),
/// which would assert that the application inserted the text itself. Those are
/// different claims, and a consumer counting them apart is entitled to know
/// which one it has.
///
/// The same distinction as an absent field versus a zero: an unspecified origin
/// is missing information, and a wrong one is a wrong fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InsertionOrigin {
    /// The caller did not say. The default for every plain insertion method.
    #[default]
    Unspecified,
    /// Entered a key at a time.
    Typed,
    /// Committed by an input method — the multi-keystroke path that produces
    /// one character. Kept apart from [`Typed`](Self::Typed) because a consumer
    /// counting keystrokes and one counting characters disagree here, and both
    /// are right.
    Composed,
    /// Pasted from a clipboard.
    Pasted,
    /// Dropped in from elsewhere.
    Dropped,
    /// Brought in by a document import.
    Imported,
    /// Re-applied by undo or redo.
    ///
    /// Hard-coded at those two paths, which never re-enter the insertion API —
    /// they snapshot and diff instead — so a replayed edit can never be counted
    /// twice under its original origin.
    Replayed,
    /// Inserted by the application: a template, an expansion, a substitution.
    Programmatic,
    /// Arrived through an accessibility channel: dictation, a braille display.
    ///
    /// **Never folded into [`Typed`](Self::Typed).** Some people write this way,
    /// and a record that erased the distinction would be reporting them as
    /// something they are not.
    Assistive,
}

impl InsertionOrigin {
    /// A stable lower-case token, for anything that has to write one down.
    ///
    /// Spelled out rather than derived from the variant name, so renaming a
    /// variant cannot silently change what a consumer persisted.
    pub fn token(self) -> &'static str {
        match self {
            InsertionOrigin::Unspecified => "unspecified",
            InsertionOrigin::Typed => "typed",
            InsertionOrigin::Composed => "composed",
            InsertionOrigin::Pasted => "pasted",
            InsertionOrigin::Dropped => "dropped",
            InsertionOrigin::Imported => "imported",
            InsertionOrigin::Replayed => "replayed",
            InsertionOrigin::Programmatic => "programmatic",
            InsertionOrigin::Assistive => "assistive",
        }
    }

    /// Every variant, for a consumer building a table over them.
    pub const ALL: [InsertionOrigin; 9] = [
        InsertionOrigin::Unspecified,
        InsertionOrigin::Typed,
        InsertionOrigin::Composed,
        InsertionOrigin::Pasted,
        InsertionOrigin::Dropped,
        InsertionOrigin::Imported,
        InsertionOrigin::Replayed,
        InsertionOrigin::Programmatic,
        InsertionOrigin::Assistive,
    ];
}

#[derive(Debug, Clone, PartialEq)]
pub enum DocumentEvent {
    /// Text content changed at a specific region.
    ///
    /// Emitted by every edit that changes what the document contains: the
    /// text-level ones (`insert_text`, `delete_char`, `delete_previous_char`,
    /// `remove_selected_text`, `insert_formatted_text`, `insert_block`,
    /// `insert_html`, `insert_markdown`, `insert_fragment`, `insert_image`), the
    /// streaming appends, `undo` and `redo`, and every **structural table edit**
    /// (`insert_table_row`, `insert_table_column`, `remove_table_row`,
    /// `remove_table_column`, `merge_table_cells`, `split_table_cell`,
    /// `remove_table`, and the cursor-relative wrappers over them).
    ///
    /// ⚠ The list above was wrong in both directions for a long time, and the
    /// half that mattered was the table edits: they emitted nothing at all, so a
    /// consumer holding offsets kept them across a row insert and a consumer
    /// caching on [`TextDocument::content_revision`] never reheated. Nothing
    /// errored and nothing looked wrong.
    ///
    /// ## Two things a consumer should know about the figures
    ///
    /// `chars_added` and `chars_removed` are a **net delta for the affected
    /// region**, not "characters this edit introduced": replacing a selection
    /// reports both, and a caller wanting to know how much text an edit brought
    /// in cannot get it from here.
    ///
    /// For `undo`, `redo` and the table edits the delta is computed as a diff
    /// over blocks joined by newlines, which is not the same string
    /// [`TextDocument::to_plain_text`] renders when the document contains a
    /// table. Consumers that shift offsets by these figures are consistent with
    /// each other; a consumer reconciling them against `to_plain_text` is not.
    ///
    /// [`TextDocument::content_revision`]: crate::TextDocument::content_revision
    /// [`TextDocument::to_plain_text`]: crate::TextDocument::to_plain_text
    ContentsChanged {
        position: usize,
        chars_removed: usize,
        chars_added: usize,
        blocks_affected: usize,
    },

    /// Text arrived, and this is where it came from.
    ///
    /// Emitted **alongside** [`ContentsChanged`](Self::ContentsChanged), never
    /// instead of it, and only when an insertion actually added characters.
    ///
    /// ## Why this is not a field on `ContentsChanged`
    ///
    /// Because `ContentsChanged` carries the wrong number for the question.
    /// Its `chars_added` is a **net delta for the affected region**: replacing a
    /// twelve-character selection with a four-character paste reports both a
    /// removal and an addition, and neither figure is "how much text this paste
    /// brought in". Attaching an origin to a net delta would produce an
    /// attribution that looks precise and is not.
    ///
    /// `chars_inserted` here is the other number: **what this insertion
    /// introduced**, which is the one a consumer attributing text to a channel
    /// actually wants.
    ///
    /// Keeping it a separate event is also what makes it additive — every
    /// existing consumer of `ContentsChanged` is untouched, and one that does
    /// not care about origins never has to mention this.
    TextInserted {
        position: usize,
        /// How many characters this insertion introduced. Never a net delta.
        chars_inserted: usize,
        origin: InsertionOrigin,
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
    /// `position` / `length` are document-absolute character offsets bounding
    /// the extent that changed, so a view may recolor just the blocks they
    /// cover rather than re-snapshotting the whole document.
    ///
    /// **A `length` of `0` means "unknown — assume the whole document"**, and
    /// is what the genuinely document-wide operations send: installing or
    /// retiring a highlighter, and a full rehighlight. `set_session_ranges`
    /// knows its own before/after ranges and reports their union exactly.
    /// A receiver that does not care may keep treating every one of these as
    /// whole-document; that is the safe reading of both cases.
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
