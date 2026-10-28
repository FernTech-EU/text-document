//! Transient edits: changes to the document that no undo history records (see
//! [`TextCursor::insert_text_transient`]).

use std::ops::Range;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use frontend::commands::undo_redo_commands;

use crate::cursor::{Recording, TextCursor};
use crate::document::TextDocument;
use crate::events::{DocumentEvent, InsertionOrigin};
use crate::inner::{CursorData, QueuedEvents, TextDocumentInner, adjust_offset};
use crate::{DocumentError, ReplaceFormatPolicy, Result, TextFormat};

/// One change a transient edit made to the document's positions: from `at`, `removed`
/// positions replaced by `added`, in the positions the document had at the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Splice {
    pub at: usize,
    pub removed: usize,
    pub added: usize,
}

impl Splice {
    /// This change followed by `next`, as one change, when `next` touches what this one
    /// made (`[at, at + added]`, either edge included). `None` when it is elsewhere.
    ///
    /// Across one change, a position inside what it made goes to the end of what it
    /// replaced, and one at its start to its start. Across two that touch, which is what an
    /// input method makes when it takes a syllable out and then shows its candidate in its
    /// place, the edges cross: the candidate's start goes after the syllable (where text
    /// was only removed) and its end before it, so a range over the candidate held nothing.
    /// Composed, they are the one replacement the writer sees.
    fn then(self, next: Splice) -> Option<Splice> {
        let made_end = self.at + self.added;
        if next.at > made_end || next.at + next.removed < self.at {
            return None;
        }
        // What `next` removed before what this change made, after it, and inside it.
        let before = self.at.saturating_sub(next.at);
        let after = (next.at + next.removed).saturating_sub(made_end);
        let inside = next.removed.saturating_sub(before + after);
        Some(Splice {
            at: self.at.min(next.at),
            removed: before + self.removed + after,
            added: self.added.saturating_sub(inside) + next.added,
        })
    }
}

/// The part of the document the outstanding transient edits changed: outside it, the
/// document is as recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    /// Where it starts, the same position in the document as it is and as recorded.
    start: usize,
    /// Where it ends in the document as it is.
    end: usize,
    /// Where it ends in the document as recorded.
    recorded_end: usize,
}

/// Where a cursor stands: what a revert gives back to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stand {
    position: usize,
    anchor: usize,
    anchor_in_table: Option<usize>,
}

impl Stand {
    fn of(data: &CursorData) -> Self {
        Stand {
            position: data.position,
            anchor: data.anchor,
            anchor_in_table: data.anchor_in_table,
        }
    }

    /// Moved as an edit moves every cursor (see `TextDocumentInner::adjust_cursors`).
    fn adjusted(self, at: usize, removed: usize, added: usize) -> Self {
        Stand {
            position: adjust_offset(self.position, at, removed, added),
            anchor: adjust_offset(self.anchor, at, removed, added),
            anchor_in_table: self
                .anchor_in_table
                .map(|origin| adjust_offset(origin, at, removed, added)),
        }
    }

    /// Moved back across `splices`: a selection's two ends inwards (see [`RangeEnd`]), so
    /// it holds the same recorded text, a caret as a range's start.
    fn mapped_back(self, splices: &[Splice]) -> Self {
        let (low, high) = if self.position <= self.anchor {
            (&self.position, &self.anchor)
        } else {
            (&self.anchor, &self.position)
        };
        let low_back = map_back_through(splices, *low, RangeEnd::Start);
        let high_back = if high == low {
            low_back
        } else {
            map_back_through(splices, *high, RangeEnd::End)
        };
        let (position, anchor) = if self.position <= self.anchor {
            (low_back, high_back)
        } else {
            (high_back, low_back)
        };
        Stand {
            position,
            anchor,
            anchor_in_table: self
                .anchor_in_table
                .map(|origin| map_back_through(splices, origin, RangeEnd::Start)),
        }
    }
}

/// A cursor alive when the layer began: where it stood then, and where the transient edits
/// alone have taken it since.
///
/// A cursor that is still where they took it was moved by nothing else, and a revert puts
/// it back exactly where it stood. Only a cursor something else moved meanwhile (the one
/// making the edits, one the writer clicked elsewhere) has to be mapped back through the
/// changes, which cannot always tell where text came back: a caret where text was removed
/// stood before it or inside it, and the two look the same.
#[derive(Debug)]
struct Rider {
    cursor: Weak<Mutex<CursorData>>,
    stood: Stand,
    expected: Stand,
    /// What its selection was besides its ends, which every edit, a transient one included,
    /// clears on every cursor (see `TextDocumentInner::adjust_cursors`): a revert that puts
    /// it back where it stood puts these back too, the text being the one they were of.
    cells: Option<crate::flow::CellRange>,
    whole_text: bool,
}

/// Which end of a range a position is, read with transient edits in place. It matters only
/// where an edit removed text and inserted none: there a revert puts text back at that very
/// position, and this says which side of it the position goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RangeEnd {
    /// A range's start, or a caret: after the text put back, before what followed it.
    Start,
    /// A range's end: before the text put back, after what preceded it.
    End,
}

/// `position` moved back across one transient change: the inverse of what it did.
fn unsplice(position: usize, splice: Splice, end: RangeEnd) -> usize {
    // The change replaced `removed` positions from `at` with `added`; back across it,
    // `added` positions from `at` become `removed` again.
    let Splice { at, removed, added } = splice;
    if position < at {
        position
    } else if position == at {
        // Text only removed, and put back here: a range starting here starts at what
        // followed the text, as it did before the removal.
        if added == 0 && end == RangeEnd::Start {
            at + removed
        } else {
            at
        }
    } else if position <= at + added {
        at + removed
    } else {
        position - added + removed
    }
}

/// `position` moved back across `splices`, the changes made, in order.
fn map_back_through(splices: &[Splice], position: usize, end: RangeEnd) -> usize {
    splices
        .iter()
        .rev()
        .fold(position, |p, splice| unsplice(p, *splice, end))
}

/// The outstanding transient edits (see the module documentation).
#[derive(Debug)]
pub(crate) struct TransientLayer {
    /// Every change of positions the layer made, in order, as each edit made it: what a
    /// panicking edit announces, and how an edit is seen to have changed anything.
    steps: Vec<Splice>,
    /// The same changes, each composed with the one before it when they touch (see
    /// [`Splice::then`]): what a revert maps the cursors back through, and every mapping
    /// of a position between the document as it is and as recorded.
    splices: Vec<Splice>,
    span: Option<Span>,
    /// Whether one of the edits changed the document away from the span the earlier ones
    /// had changed, so that the span holds recorded text between them.
    scattered: bool,
    riders: Vec<Rider>,
}

impl TransientLayer {
    /// A layer beginning while `cursors` are alive, each of which a revert gives back where
    /// it stands now unless something other than the transient edits moves it.
    pub(crate) fn over(cursors: &[Weak<Mutex<CursorData>>]) -> Self {
        let riders = cursors
            .iter()
            .filter_map(|weak| {
                let cursor = weak.upgrade()?;
                let data = cursor.lock();
                let stood = Stand::of(&data);
                Some(Rider {
                    cursor: weak.clone(),
                    stood,
                    expected: stood,
                    cells: data.cell_selection_override.clone(),
                    whole_text: data.whole_text_selected,
                })
            })
            .collect();
        TransientLayer {
            steps: Vec::new(),
            splices: Vec::new(),
            span: None,
            scattered: false,
            riders,
        }
    }

    /// A transient edit replaced `removed` positions from `at` with `added`.
    ///
    /// Called from `TextDocumentInner::adjust_cursors`, which every edit goes through to
    /// move the cursors: while a layer is outstanding, every edit is a transient one.
    pub(crate) fn record(&mut self, at: usize, removed: usize, added: usize) {
        if removed == 0 && added == 0 {
            return;
        }
        let splice = Splice { at, removed, added };
        self.steps.push(splice);
        match self.splices.last().and_then(|last| last.then(splice)) {
            Some(composed) => *self.splices.last_mut().expect("there is a last") = composed,
            None => self.splices.push(splice),
        }
        for rider in &mut self.riders {
            rider.expected = rider.expected.adjusted(at, removed, added);
        }
        self.span = Some(match self.span {
            None => Span {
                start: at,
                end: at + added,
                recorded_end: at + removed,
            },
            Some(span) => {
                if at > span.end || at + removed < span.start {
                    self.scattered = true;
                }
                let start = span.start.min(at);
                let end_before = span.end.max(at + removed);
                // The recorded text the span now covers: what it covered, and the unchanged
                // positions it grew over on either side.
                let recorded_len = (span.recorded_end - span.start)
                    + (span.start - start)
                    + (end_before - span.end);
                Span {
                    start,
                    end: end_before - removed + added,
                    recorded_end: start + recorded_len,
                }
            }
        });
    }

    /// Where a revert puts `cursor`, which stands at `now`: where it stood when the layer
    /// began if only the transient edits moved it, else `now` moved back across them.
    fn stand_after_revert(&self, cursor: &Weak<Mutex<CursorData>>, now: Stand) -> Stand {
        match self.carried(cursor, now) {
            Some(rider) => rider.stood,
            None => now.mapped_back(&self.splices),
        }
    }

    /// `cursor`'s rider, when the cursor stands at `now` and only the transient edits moved
    /// it there: one a revert puts back exactly as it was.
    fn carried(&self, cursor: &Weak<Mutex<CursorData>>, now: Stand) -> Option<&Rider> {
        self.riders
            .iter()
            .find(|rider| Weak::ptr_eq(&rider.cursor, cursor) && rider.expected == now)
    }
}

/// The transient edits a revert took away: how to map a position read in the document as
/// it was, with them, into the document as it is now.
#[derive(Debug, Default)]
pub(crate) struct Reverted {
    splices: Vec<Splice>,
}

impl Reverted {
    /// The range `[start, end)` (either order), read with the transient edits in place, in
    /// the recorded document: its start moved back as a start, its end as an end, so that it
    /// holds the recorded text it held. A range that held only transient text can come back
    /// empty.
    pub(crate) fn map_range(&self, start: usize, end: usize) -> (usize, usize) {
        map_range_through(&self.splices, start, end)
    }
}

/// [`Reverted::map_range`] across `splices`.
fn map_range_through(splices: &[Splice], start: usize, end: usize) -> (usize, usize) {
    let (low, high) = (start.min(end), start.max(end));
    if low == high {
        let at = map_back_through(splices, low, RangeEnd::Start);
        return (at, at);
    }
    let low = map_back_through(splices, low, RangeEnd::Start);
    let high = map_back_through(splices, high, RangeEnd::End);
    (low, high.max(low))
}

/// [`Reverted::map_range`] of an optional revert: the range, ordered, when there was none.
pub(crate) fn map_range_back(
    reverted: &Option<Reverted>,
    start: usize,
    end: usize,
) -> (usize, usize) {
    match reverted {
        Some(reverted) => reverted.map_range(start, end),
        None => (start.min(end), start.max(end)),
    }
}

/// What [`settle`] did before a recorded write: the transient edits it reverted, if any,
/// with the events that announce it still queued.
///
/// The events go out with the write's own, in order, when the write dispatches them: no
/// callback runs between the revert and the write. A callback that wrote then, from the
/// revert's own announcement, would write before the write it made way for: an undo would
/// have taken back the callback's edit instead of the writer's, and a callback putting a
/// composition back would have had the write refused. Whatever is still queued when this
/// is dropped (a write that returned before dispatching anything, or failed) goes out then,
/// so it is held until the write has run and the document is unlocked.
#[must_use = "hold it until the write has run: the revert's events go out with the write's"]
pub(crate) struct Settled {
    /// The document, when a revert happened and its events are still to go out.
    doc: Option<Arc<Mutex<TextDocumentInner>>>,
}

impl Drop for Settled {
    fn drop(&mut self) {
        if let Some(doc) = self.doc.take() {
            let queued = doc.lock().take_queued_events();
            crate::inner::dispatch_queued_events(queued);
        }
    }
}

impl TextDocumentInner {
    /// Revert the outstanding transient edits: the store back to the recorded content, every
    /// cursor mapped back, the change announced. `None` when there were none.
    ///
    /// The events are queued, not dispatched: the caller dispatches them with its own.
    pub(crate) fn revert_transient_edits(&mut self) -> Option<Reverted> {
        let layer = self.transient.take()?;
        // The store refuses a revert only inside a transient write or over an open write.
        // This document's transient writes hold its lock as this does, its recorded writes
        // end before they let it go, and the one write that outlives a call, a load in
        // place, is never open with a layer: it cannot happen, and if it did, the layer and
        // the store are left as they are, in step.
        let Ok(reverted) = self.ctx.db_context.get_store().revert_transient_edits() else {
            self.transient = Some(layer);
            debug_assert!(
                false,
                "the store refused a revert under the document's lock"
            );
            return None;
        };
        debug_assert!(
            reverted || layer.steps.is_empty(),
            "the document held transient edits that its store did not"
        );
        self.prune_dead_cursors();
        for weak in &self.cursors {
            let Some(cursor) = weak.upgrade() else {
                continue;
            };
            let mut data = cursor.lock();
            let now = Stand::of(&data);
            let back = layer.stand_after_revert(weak, now);
            data.position = back.position;
            data.anchor = back.anchor;
            data.anchor_in_table = back.anchor_in_table;
            // A cursor put back as it was gets back the cells or the whole text it selected;
            // any other selection is a new one, of positions alone.
            let carried = layer.carried(weak, now);
            data.cell_selection_override = carried.and_then(|rider| rider.cells.clone());
            data.whole_text_selected = carried.is_some_and(|rider| rider.whole_text);
        }
        self.invalidate_text_cache();
        if let Some(span) = layer.span {
            let blocks_affected = self.blocks_between(span.start, span.recorded_end);
            if blocks_affected > 1 {
                self.rehighlight_all();
            } else {
                self.rehighlight_affected(span.start);
            }
            self.queue_event(DocumentEvent::ContentsChanged {
                position: span.start,
                chars_removed: span.end - span.start,
                chars_added: span.recorded_end - span.start,
                blocks_affected,
            });
        }
        self.check_block_count_changed();
        self.check_flow_changed();
        Some(Reverted {
            splices: layer.splices,
        })
    }

    /// How many blocks `[start, end)` touches, at least one.
    ///
    /// Its last position is `end - 1`: the block at `end` itself starts after a separator
    /// the range ends before, which a block lookup counts as that next block's.
    fn blocks_between(&self, start: usize, end: usize) -> usize {
        let number = |position: usize| {
            frontend::commands::document_inspection_commands::get_block_at_position(
                &self.ctx,
                &frontend::document_inspection::GetBlockAtPositionDto {
                    position: position as i64,
                },
            )
            .ok()
            .map(|info| info.block_number)
        };
        match (number(start), number(end.saturating_sub(1).max(start))) {
            (Some(first), Some(last)) if last > first => (last - first) as usize + 1,
            _ => 1,
        }
    }

    /// After a panic inside a transient edit that had made its change: announce the change,
    /// unless the edit had already, and what it did to the paragraphs and the flow. A panic
    /// in a syntax highlighter comes before the edit announces anything, and the document
    /// would show text no listener had heard of.
    fn announce_kept_changes(&mut self, changes_before: usize, events_before: usize) {
        let announced = self
            .pending_events
            .get(events_before..)
            .is_some_and(|events| {
                events
                    .iter()
                    .any(|event| matches!(event, DocumentEvent::ContentsChanged { .. }))
            });
        let kept: Vec<Splice> = self
            .transient
            .as_ref()
            .map(|layer| layer.steps[changes_before..].to_vec())
            .unwrap_or_default();
        self.invalidate_text_cache();
        if !announced {
            for splice in &kept {
                let blocks_affected = self.blocks_between(splice.at, splice.at + splice.added);
                self.queue_event(DocumentEvent::ContentsChanged {
                    position: splice.at,
                    chars_removed: splice.removed,
                    chars_added: splice.added,
                    blocks_affected,
                });
            }
        }
        if !kept.is_empty() {
            self.check_block_count_changed();
            self.check_flow_changed();
        }
    }

    /// Whether a load in place this document started is still writing its content.
    fn is_loading_in_place(&self) -> bool {
        if self.history_resets.is_empty() {
            return false;
        }
        let operations = self.ctx.long_operation_manager.lock();
        self.history_resets
            .keys()
            .any(|id| operations.is_operation_finished(id) == Some(false))
    }
}

/// Revert `doc`'s outstanding transient edits, if any, before a recorded write.
///
/// What a recorded edit calls first, before it reads a cursor, so that it acts on the
/// recorded text at the cursor's place in it. Hold what it returns until the write has run
/// (see [`Settled`]).
pub(crate) fn settle(doc: &Arc<Mutex<TextDocumentInner>>) -> Settled {
    let reverted = doc.lock().revert_transient_edits().is_some();
    Settled {
        doc: reverted.then(|| Arc::clone(doc)),
    }
}

impl TextCursor {
    /// Revert the document's outstanding transient edits before an edit reads this cursor
    /// (see [`settle`]).
    pub(crate) fn settle_transient_edits(&self) -> Settled {
        settle(&self.doc)
    }

    /// Run `edit` as a transient edit, written to the store inside a transient write.
    ///
    /// The write is kept when the edit changed the document's positions, which it logs into
    /// the layer as it moves the cursors (see `TransientLayer::record`), and undone when it
    /// did not: the store then goes back to where the edit found it, and the layer is let go
    /// when the edit opened it. So the store and the layer stay in step whatever happens:
    /// an error before the change was made (a refused selection, a failed half of a
    /// replacement) leaves the document as it was, and so does a panic there; a panic after
    /// it (in a syntax highlighter, which runs after the cursors move) keeps the change, as
    /// a transient edit the layer holds and a revert takes away.
    fn edit_transiently<R>(
        &self,
        edit: impl FnOnce(&mut TextDocumentInner) -> Result<(R, QueuedEvents)>,
    ) -> Result<R> {
        let (value, queued) = {
            let mut inner = self.doc.lock();
            if inner.is_loading_in_place() {
                return Err(DocumentError::TransientEdits(
                    crate::TransientRefusal::LoadInPlace,
                ));
            }
            let store = Arc::clone(inner.ctx.db_context.get_store());
            let write = store
                .begin_transient_write()
                .map_err(|refused| DocumentError::TransientEdits(refused.into()))?;
            let opened = write.opens();
            if opened {
                let layer = TransientLayer::over(&inner.cursors);
                inner.transient = Some(layer);
            }
            let changes =
                |inner: &TextDocumentInner| inner.transient.as_ref().map_or(0, |l| l.steps.len());
            let changes_before = changes(&inner);
            let events_before = inner.pending_events.len();
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| edit(&mut inner)));
            if changes(&inner) > changes_before {
                write.keep();
                if outcome.is_err() {
                    inner.announce_kept_changes(changes_before, events_before);
                }
            } else {
                drop(write);
                if opened {
                    inner.transient = None;
                }
                inner.invalidate_text_cache();
            }
            match outcome {
                Ok(Ok(done)) => done,
                Ok(Err(error)) => return Err(error),
                Err(panic) => {
                    // The change the edit kept goes out now, to the callbacks as well as to
                    // `poll_events`: the host may catch the panic and edit nothing more. A
                    // callback's own panic is dropped, so the edit's is the one resumed.
                    let queued = inner.take_queued_events();
                    drop(inner);
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        crate::inner::dispatch_queued_events(queued)
                    }));
                    std::panic::resume_unwind(panic);
                }
            }
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(value)
    }

    /// Insert plain text at the cursor, replacing the selection if any, **without recording
    /// it** in the undo history.
    ///
    /// For an input method's composition, shown in the text while it is being made, or any
    /// tentative text a host shows and then either commits or takes back. The text goes in
    /// as [`insert_text`](Self::insert_text) puts it, with the format of the text before it,
    /// and the cursor is left after it. The document announces the change as it does every
    /// edit ([`DocumentEvent::ContentsChanged`], and the block and flow events when they
    /// apply), so views lay it out and draw it; it announces no
    /// [`DocumentEvent::TextInserted`], since nothing has arrived yet, no
    /// [`DocumentEvent::UndoRedoChanged`], since the history has not moved, and leaves
    /// [`is_modified`](crate::TextDocument::is_modified) as it was.
    ///
    /// Nothing is pushed on the undo stack, the redo stack is kept, and the next recorded
    /// edit coalesces with the one before the transient edits as it would have without
    /// them. To replace the text a composition showed with its next state, select it and
    /// call this again: [`TextDocument::transient_range`] says where it is.
    ///
    /// The edit joins the document's outstanding transient edits, which a revert, a commit
    /// or any recorded write ends (see the [module documentation](crate::transient)).
    ///
    /// Refused with [`DocumentError::TransientEdits`], the document left as it was, while a
    /// load in place ([`set_djot`](crate::TextDocument::set_djot) and its siblings) is still
    /// writing the document, and when the selection crosses a table's cells or holds part
    /// of a table and text outside it.
    pub fn insert_text_transient(&self, text: &str) -> Result<()> {
        self.edit_transiently(|inner| {
            let (pos, anchor) = self.read_cursor();
            let queued = self.insert_text_locked(
                inner,
                pos,
                anchor,
                text,
                ReplaceFormatPolicy::default(),
                InsertionOrigin::Unspecified,
                Recording::Transient,
            )?;
            Ok(((), queued))
        })
    }

    /// As [`insert_text_transient`](Self::insert_text_transient), with a character format,
    /// as [`insert_formatted_text`](Self::insert_formatted_text) inserts it. Refused as
    /// `insert_text_transient` is.
    pub fn insert_formatted_text_transient(&self, text: &str, format: &TextFormat) -> Result<()> {
        self.edit_transiently(|inner| {
            let (pos, anchor) = self.read_cursor();
            let queued = self.insert_formatted_text_locked(
                inner,
                pos,
                anchor,
                text,
                format,
                InsertionOrigin::Unspecified,
                Recording::Transient,
            )?;
            Ok(((), queued))
        })
    }

    /// Remove the selected text **without recording it** in the undo history. Returns the
    /// removed text; nothing happens without a selection.
    ///
    /// As [`remove_selected_text`](Self::remove_selected_text) removes it, announced and
    /// refused as [`insert_text_transient`](Self::insert_text_transient) is, and recorded no
    /// more. The removed text may be recorded text: an input method that converts a
    /// committed syllable (Korean Hanja conversion, a reconversion in Japanese) takes it out
    /// transiently, shows its candidates in its place, and the commit then records the
    /// whole replacement as one step, which an undo takes back to the syllable.
    pub fn remove_selected_text_transient(&self) -> Result<String> {
        self.edit_transiently(|inner| {
            let (pos, anchor) = self.read_cursor();
            if pos == anchor {
                return Ok((String::new(), inner.take_queued_events()));
            }
            self.remove_locked(inner, pos, anchor, Recording::Transient)
        })
    }

    /// Commit the outstanding transient edits as `text`, recorded as **one undo step of its
    /// own**.
    ///
    /// The transient edits are reverted, and the text they changed in the recorded document
    /// (see [`TextDocument::transient_range`]) is replaced with `text`: after a composition
    /// inserted transiently, `text` goes in where it was shown, over the text the
    /// composition took the place of; after a transient removal of recorded text (a
    /// syllable an input method converts), it replaces that text, so an undo gives it back.
    /// The cursor is left after `text`.
    ///
    /// The step is closed on both sides, as
    /// [`break_undo_merge`](crate::TextDocument::break_undo_merge) closes one: it does not
    /// coalesce with the typing before the composition, and what is typed after it starts a
    /// step of its own, so an undo takes back exactly the committed text. Inside an open
    /// edit block, the commit joins the block instead. A replaced range within one block
    /// entirely in one format keeps it ([`ReplaceFormatPolicy::PreserveIfFullyCovered`]);
    /// inserted text, and text replacing a range across blocks, takes the format of the text
    /// before it, as typed text does.
    ///
    /// With no transient edits outstanding, `text` replaces the cursor's selection, or goes
    /// in at the cursor, as the same one closed step. An empty `text` records the removal
    /// of the recorded text the transient edits had replaced (or of the selection, with
    /// none outstanding); when there is none it records nothing and, like a cancel
    /// ([`TextDocument::revert_transient_edits`]), closes no step.
    ///
    /// On an error from the recorded half, the transient edits are reverted all the same and
    /// nothing is recorded, but the step before them is closed already: the typing on either
    /// side of the failed commit no longer coalesces.
    ///
    /// Reports [`InsertionOrigin::Unspecified`], as
    /// [`insert_text`](Self::insert_text) does. Use
    /// [`commit_transient_text_with_origin`](Self::commit_transient_text_with_origin) to say,
    /// [`InsertionOrigin::Composed`] for an input method.
    ///
    /// Refused with [`DocumentError::TransientEdits`], the transient edits left as they
    /// were, when they changed the document in more than one place: the recorded text
    /// between them would be replaced too.
    pub fn commit_transient_text(&self, text: &str) -> Result<()> {
        self.commit_transient_text_with_origin(text, InsertionOrigin::Unspecified)
    }

    /// As [`commit_transient_text`](Self::commit_transient_text), saying which channel the
    /// text came through (see [`insert_text_with_origin`](Self::insert_text_with_origin)).
    pub fn commit_transient_text_with_origin(
        &self,
        text: &str,
        origin: InsertionOrigin,
    ) -> Result<()> {
        let (result, rest) = {
            let mut inner = self.doc.lock();
            let range = match inner.transient.as_ref() {
                Some(layer) if layer.scattered => {
                    return Err(DocumentError::TransientEdits(
                        crate::TransientRefusal::CommitInSeveralPlaces,
                    ));
                }
                Some(TransientLayer {
                    span: Some(span), ..
                }) => Some((span.start, span.recorded_end)),
                _ => None,
            };
            inner.revert_transient_edits();
            // Read after the revert, which moved it back into the recorded text.
            let (start, end) = range.unwrap_or_else(|| self.read_cursor());
            // Nothing to record is a cancel, which closes no step: the typing on either side
            // coalesces as it would have.
            let records = !text.is_empty() || start != end;
            let stack = Some(inner.stack_id);
            if records {
                undo_redo_commands::seal_head(&inner.ctx, stack);
            }
            let result = if !text.is_empty() {
                self.insert_text_locked(
                    &mut inner,
                    start,
                    end,
                    text,
                    ReplaceFormatPolicy::PreserveIfFullyCovered,
                    origin,
                    Recording::History,
                )
            } else if start != end {
                self.remove_locked(&mut inner, start, end, Recording::History)
                    .map(|(_, queued)| queued)
            } else {
                Ok(Vec::new())
            };
            if records {
                undo_redo_commands::seal_head(&inner.ctx, stack);
            }
            // What is still queued: the revert's events when the recorded half did not take
            // them with its own.
            (result, inner.take_queued_events())
        };
        match result {
            Ok(mut queued) => {
                queued.extend(rest);
                crate::inner::dispatch_queued_events(queued);
                Ok(())
            }
            Err(error) => {
                crate::inner::dispatch_queued_events(rest);
                Err(error)
            }
        }
    }
}

impl TextDocument {
    /// Revert the outstanding transient edits before a recorded write (see [`settle`]).
    pub(crate) fn settle_transient_edits(&self) -> Settled {
        settle(&self.inner)
    }

    /// Whether transient edits are outstanding (see
    /// [`TextCursor::insert_text_transient`]): the document shows text its history does not
    /// hold, which a revert takes away.
    pub fn has_transient_edits(&self) -> bool {
        self.inner.lock().transient.is_some()
    }

    /// Where the outstanding transient edits are, in the document as it is: the smallest
    /// range outside of which the document is as recorded. `None` when there are none.
    ///
    /// After a composition inserted transiently, the composition's text, which the next
    /// update selects to replace. After a transient removal and nothing else, an empty
    /// range where the text was. The range is the one
    /// [`TextCursor::commit_transient_text`] replaces, read in the document with the
    /// transient edits in place.
    pub fn transient_range(&self) -> Option<Range<usize>> {
        let inner = self.inner.lock();
        let span = inner.transient.as_ref()?.span?;
        Some(span.start..span.end)
    }

    /// Revert the outstanding transient edits, recording nothing: the document is back to
    /// its recorded content, as it was before the first of them, and the undo and redo
    /// histories are as they were. Returns whether there were any.
    ///
    /// What cancelling a composition calls. Every cursor goes back into the recorded text,
    /// as the [module documentation](crate::transient#positions) says: the cursor that showed
    /// a composition, left after it, lands after the text the composition took the place of.
    /// The change is
    /// announced as an edit's is ([`DocumentEvent::ContentsChanged`] over the range that
    /// changed, and the block and flow events when they apply), with nothing about the
    /// history.
    pub fn revert_transient_edits(&self) -> bool {
        let queued = {
            let mut inner = self.inner.lock();
            if inner.revert_transient_edits().is_none() {
                return false;
            }
            inner.take_queued_events()
        };
        crate::inner::dispatch_queued_events(queued);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(splices: &[(usize, usize, usize)]) -> TransientLayer {
        let mut layer = TransientLayer::over(&[]);
        for &(at, removed, added) in splices {
            layer.record(at, removed, added);
        }
        layer
    }

    #[test]
    fn a_composition_replaced_at_every_update_is_one_span_over_nothing_recorded() {
        // "n" at 3, then "ni", "nih", "nihao" over it.
        let layer = layer(&[(3, 0, 1), (3, 1, 2), (3, 2, 3), (3, 3, 5)]);
        assert_eq!(
            layer.span,
            Some(Span {
                start: 3,
                end: 8,
                recorded_end: 3
            })
        );
        assert!(!layer.scattered);
    }

    #[test]
    fn a_removed_syllable_shown_converted_is_one_span_over_the_syllable() {
        let layer = layer(&[(7, 1, 0), (7, 0, 1)]);
        assert_eq!(
            layer.span,
            Some(Span {
                start: 7,
                end: 8,
                recorded_end: 8
            })
        );
        assert!(!layer.scattered);
    }

    #[test]
    fn an_edit_touching_the_span_from_before_extends_it() {
        // A composition at 5..8, then the two recorded characters before it removed.
        let layer = layer(&[(5, 0, 3), (3, 2, 0)]);
        assert_eq!(
            layer.span,
            Some(Span {
                start: 3,
                end: 6,
                recorded_end: 5
            })
        );
        assert!(!layer.scattered);
    }

    #[test]
    fn an_edit_away_from_the_span_scatters_the_layer() {
        let layer = layer(&[(3, 0, 2), (10, 0, 1)]);
        assert!(layer.scattered);
        // The span still bounds both, recorded text between them included.
        assert_eq!(
            layer.span,
            Some(Span {
                start: 3,
                end: 11,
                recorded_end: 8
            })
        );
    }

    #[test]
    fn a_removal_then_an_insertion_in_its_place_is_one_replacement() {
        // "ab한": the syllable taken out, then its candidate shown in its place.
        let layer = layer(&[(2, 1, 0), (2, 0, 1)]);
        assert_eq!(
            layer.splices,
            [Splice {
                at: 2,
                removed: 1,
                added: 1
            }]
        );
        assert_eq!(layer.steps.len(), 2);
        let reverted = Reverted {
            splices: layer.splices.clone(),
        };
        assert_eq!(
            reverted.map_range(2, 3),
            (2, 3),
            "the candidate is the syllable"
        );
    }

    #[test]
    fn changes_that_touch_compose_and_others_do_not() {
        let composed = |a: (usize, usize, usize), b: (usize, usize, usize)| {
            let splice = |(at, removed, added)| Splice { at, removed, added };
            splice(a)
                .then(splice(b))
                .map(|s| (s.at, s.removed, s.added))
        };
        // A composition replaced by its next state.
        assert_eq!(composed((3, 0, 1), (3, 1, 2)), Some((3, 0, 2)));
        // Recorded text before it taken out up to it.
        assert_eq!(composed((5, 0, 3), (3, 2, 0)), Some((3, 2, 3)));
        // Its last character and two recorded ones after it taken out.
        assert_eq!(composed((2, 0, 2), (3, 3, 0)), Some((2, 2, 1)));
        // Recorded text right after it taken out.
        assert_eq!(composed((2, 0, 2), (4, 2, 0)), Some((2, 2, 2)));
        // Elsewhere.
        assert_eq!(composed((2, 0, 2), (5, 0, 1)), None);
        assert_eq!(composed((5, 0, 2), (1, 2, 0)), None);
    }

    #[test]
    fn a_change_of_nothing_is_not_recorded() {
        let layer = layer(&[(4, 0, 0)]);
        assert!(layer.splices.is_empty());
        assert_eq!(layer.span, None);
    }

    #[test]
    fn mapping_back_undoes_every_change_in_reverse() {
        let reverted = Reverted {
            splices: vec![
                Splice {
                    at: 3,
                    removed: 0,
                    added: 1,
                },
                Splice {
                    at: 3,
                    removed: 1,
                    added: 5,
                },
            ],
        };
        // Before the composition, inside it, at its end, and after it.
        for end in [RangeEnd::Start, RangeEnd::End] {
            let map = |position| map_back_through(&reverted.splices, position, end);
            assert_eq!(map(2), 2);
            assert_eq!(map(5), 3);
            assert_eq!(map(8), 3);
            assert_eq!(map(12), 7);
        }
    }

    #[test]
    fn a_range_starting_where_text_was_removed_starts_after_it_again() {
        // "hello" with "ll" (2..4) removed: "heo".
        let reverted = Reverted {
            splices: vec![Splice {
                at: 2,
                removed: 2,
                added: 0,
            }],
        };
        // The "o", live 2..3, is recorded 4..5; "he", live 0..2, is recorded 0..2.
        assert_eq!(reverted.map_range(2, 3), (4, 5));
        assert_eq!(reverted.map_range(0, 2), (0, 2));
        // A range across the removal holds what was removed as well.
        assert_eq!(reverted.map_range(1, 3), (1, 5));
    }

    /// A recorded edit with a selection that finds transient edits made since its revert
    /// (another thread's, between the revert and its lock) is refused, and leaves an edit
    /// block it is part of whole.
    #[test]
    fn a_recorded_edit_finding_a_layer_keeps_the_edit_block_whole() {
        let doc = TextDocument::new();
        doc.set_plain_text("hello world").unwrap();
        doc.clear_undo_redo();
        let cursor = doc.cursor_at(0);
        cursor.begin_edit_block();
        cursor.insert_text("A").unwrap();
        // The other thread's transient edit, after this edit's revert.
        doc.cursor_at(3).insert_text_transient("x").unwrap();
        let refused = {
            let mut inner = doc.inner.lock();
            cursor.insert_text_locked(
                &mut inner,
                7,
                12,
                "b",
                ReplaceFormatPolicy::default(),
                InsertionOrigin::Unspecified,
                Recording::History,
            )
        };
        assert!(matches!(
            refused,
            Err(DocumentError::TransientEdits(
                crate::TransientRefusal::LayerOutstanding
            ))
        ));
        cursor.end_edit_block();
        doc.revert_transient_edits();
        assert_eq!(doc.to_plain_text().unwrap(), "Ahello world");
        doc.undo().unwrap();
        assert_eq!(doc.to_plain_text().unwrap(), "hello world");
        assert!(!doc.can_undo());
    }

    /// A transient edit while a write is open on the store (the one this crate leaves open
    /// across its calls is a load in place, refused earlier) is the store's refusal, with
    /// nothing changed, and is made once the write has ended.
    #[test]
    fn a_transient_edit_over_an_open_write_is_refused_by_the_store() {
        let doc = TextDocument::new();
        doc.set_plain_text("abc").unwrap();
        let db = doc.inner.lock().ctx.db_context.clone();
        let transaction =
            common::database::transactions::Transaction::begin_write_transaction(&db).unwrap();
        let refused = doc.cursor_at(1).insert_text_transient("n");
        assert!(matches!(
            refused,
            Err(DocumentError::TransientEdits(
                crate::TransientRefusal::WriteInProgress
            ))
        ));
        assert!(!doc.has_transient_edits());
        assert_eq!(doc.to_plain_text().unwrap(), "abc");
        drop(transaction);
        doc.cursor_at(1).insert_text_transient("n").unwrap();
        assert_eq!(doc.to_plain_text().unwrap(), "anbc");
    }

    #[test]
    fn a_range_that_held_only_transient_text_comes_back_empty() {
        let reverted = Reverted {
            splices: vec![Splice {
                at: 5,
                removed: 0,
                added: 3,
            }],
        };
        assert_eq!(reverted.map_range(5, 8), (5, 5));
        assert_eq!(reverted.map_range(6, 7), (5, 5));
    }
}
