//! Ropey-backed storage backend.
//!
//! Holds character data in a single document-wide `ropey::Rope`,
//! with structural entities (Frames, Tables, Lists, Resources) in
//! `im::HashMap` tables and per-block character formatting in
//! `format_runs`. See the migration plan §1.5 for the relationship
//! inlining and §1.6 for the rope layout (block boundary `\n` +
//! U+FFFC table anchor).

use crate::database::block_offset_index::BlockOffsetIndex;
use crate::entities::*;
use crate::format_runs::{FootnoteRefAnchor, FormatRun, ImageAnchor};
use crate::snapshot::{StoreSnapshot, StoreSnapshotTrait};
use crate::types::EntityId;
use im::HashMap;
use parking_lot::{Mutex, RwLock};
use ropey::Rope;
use std::collections::HashMap as StdHashMap;
use std::collections::HashSet as StdHashSet;
use std::thread::ThreadId;

// ─────────────────────────────────────────────────────────────────────────────
// The Store
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct RopeStore {
    // ── Character content (shared across all blocks, including cells) ──
    pub rope: RwLock<Rope>,

    // ── Structural entity tables ──────────────────────────────────────
    pub roots: RwLock<HashMap<EntityId, Root>>,
    pub documents: RwLock<HashMap<EntityId, Document>>,
    pub frames: RwLock<HashMap<EntityId, Frame>>,
    pub blocks: RwLock<HashMap<EntityId, Block>>,
    pub lists: RwLock<HashMap<EntityId, List>>,
    pub resources: RwLock<HashMap<EntityId, Resource>>,
    pub tables: RwLock<HashMap<EntityId, Table>>,
    pub table_cells: RwLock<HashMap<EntityId, TableCell>>,

    // ── Per-block character formatting + image anchors ────────────────
    pub format_runs: RwLock<HashMap<EntityId, Vec<FormatRun>>>,
    pub block_images: RwLock<HashMap<EntityId, Vec<ImageAnchor>>>,
    /// Footnote references anchored in each block, byte-ordered.
    ///
    /// Beside `block_images` rather than merged with it: the two are written by
    /// different editing paths and read together only through
    /// `format_runs::block_anchors`, which is the one place their interleaving is
    /// decided.
    pub block_footnote_refs: RwLock<HashMap<EntityId, Vec<FootnoteRefAnchor>>>,
    /// Host-supplied markers by footnote label — what a reference *prints*.
    ///
    /// Ambient and presentation-only, like a syntax highlighter: never serialised,
    /// never part of the document, and consulted when a reference is rendered.
    ///
    /// It exists because the number is a fact about the **host's** manuscript, not
    /// about this document. Skribisto compiles one chapter into a document of its
    /// own, and that document's reading order would number the chapter's notes from
    /// one — disagreeing with the badge the writer sees in the editor, for the same
    /// note, at the same moment. Empty means "number them yourself", which is the
    /// right answer for a document that *is* the whole text.
    pub footnote_markers: RwLock<StdHashMap<String, String>>,

    // ── Document-wide block ordering (sorted by rope position) ────────
    pub block_offsets: RwLock<BlockOffsetIndex>,

    // ── ID counters ───────────────────────────────────────────────────
    // Never restored by undo (only by transaction rollback).
    pub counters: RwLock<StdHashMap<String, EntityId>>,

    // ── Savepoints (in-memory, transaction-scoped) ────────────────────
    savepoints: RwLock<StdHashMap<u64, RopeStoreSnapshot>>,
    next_savepoint_id: RwLock<u64>,

    // ── Transient edits (see `begin_transient_write`) ─────────────────
    transient: Mutex<TransientEdits>,
}

/// Edits that no undo history holds, made over the content as it is recorded, and the
/// writes that are open while they begin.
#[derive(Debug, Default)]
struct TransientEdits {
    /// The store as it stood before the first outstanding transient edit, which a revert
    /// puts back. `None` while there are none.
    base: Option<RopeStoreSnapshot>,
    /// The thread inside a [`TransientWrite`]: while `base` is held, the only one a write
    /// transaction may be opened for.
    writer: Option<ThreadId>,
    /// The auto-savepoints of the writes open now: every write transaction, and every
    /// other write begun through [`RopeStore::check_write_allowed`]. A write discards its
    /// auto-savepoint as it ends, committed, rolled back or dropped, which is how the store
    /// sees it end.
    open_writes: StdHashSet<u64>,
    /// The threads that passed [`RopeStore::check_write_allowed`] and have still to take
    /// the auto-savepoint of the write they begin, which is the next one they take.
    beginning: StdHashSet<ThreadId>,
}

impl TransientEdits {
    /// Whether a write is open, or being begun on a thread other than `besides`.
    fn writes_open(&self, besides: Option<ThreadId>) -> bool {
        !self.open_writes.is_empty() || self.beginning.iter().any(|thread| Some(*thread) != besides)
    }
}

/// A transient write, or a revert of transient edits, was refused: what they need of the
/// store does not hold. See [`RopeStore::begin_transient_write`] and
/// [`RopeStore::revert_transient_edits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TransientWriteRefused {
    /// Another transient write is in progress, on this thread or another.
    #[error("a transient write is in progress: transient writes do not nest")]
    TransientWriteInProgress,
    /// A write is open (a write transaction, or another write begun through
    /// [`RopeStore::check_write_allowed`]): the first transient write's snapshot would be
    /// taken over it, and a revert would take what it then records away.
    #[error("a write is open on the store: a transient write begins once it ends")]
    WriteInProgress,
}

/// A write transaction was asked for while transient edits are outstanding, from outside
/// the transient write that makes them.
///
/// The recorded history names positions and blocks of the content as it is recorded.
/// A write landing over transient edits would be recorded against content that a revert
/// then takes away, and the revert, which puts the recorded content back as it was, would
/// take the write away with it. So the write is refused before it changes anything: revert
/// the transient edits first. `text_document` does so itself before every edit, undo and
/// redo; this is what stops a write begun some other way. Every write of this workspace
/// begins through [`RopeStore::check_write_allowed`]: a write transaction, and the streaming
/// appends, which write the rope directly. What the store cannot stop is code writing its
/// public fields without beginning a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "transient edits are outstanding: revert them before writing the document's recorded content"
)]
pub struct TransientEditsOutstanding;

impl RopeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// O(1) snapshot of the entire store (rope is Arc-shared, all
    /// `im::HashMap`s are HAMT-shared; `BlockOffsetIndex` is a small
    /// `Vec` cloned outright).
    pub fn snapshot(&self) -> RopeStoreSnapshot {
        RopeStoreSnapshot {
            rope: self.rope.read().clone(),
            roots: self.roots.read().clone(),
            documents: self.documents.read().clone(),
            frames: self.frames.read().clone(),
            blocks: self.blocks.read().clone(),
            lists: self.lists.read().clone(),
            resources: self.resources.read().clone(),
            tables: self.tables.read().clone(),
            table_cells: self.table_cells.read().clone(),
            format_runs: self.format_runs.read().clone(),
            block_images: self.block_images.read().clone(),
            block_footnote_refs: self.block_footnote_refs.read().clone(),
            footnote_markers: self.footnote_markers.read().clone(),
            block_offsets: self.block_offsets.read().clone(),
            counters: self.counters.read().clone(),
        }
    }

    /// Restore from a snapshot. Overwrites counters too — used for
    /// transaction rollback (`Drop` of an uncommitted write txn).
    pub fn restore(&self, snap: &RopeStoreSnapshot) {
        *self.rope.write() = snap.rope.clone();
        *self.roots.write() = snap.roots.clone();
        *self.documents.write() = snap.documents.clone();
        *self.frames.write() = snap.frames.clone();
        *self.blocks.write() = snap.blocks.clone();
        *self.lists.write() = snap.lists.clone();
        *self.resources.write() = snap.resources.clone();
        *self.tables.write() = snap.tables.clone();
        *self.table_cells.write() = snap.table_cells.clone();
        *self.format_runs.write() = snap.format_runs.clone();
        *self.block_images.write() = snap.block_images.clone();
        *self.block_footnote_refs.write() = snap.block_footnote_refs.clone();
        *self.footnote_markers.write() = snap.footnote_markers.clone();
        *self.block_offsets.write() = snap.block_offsets.clone();
        *self.counters.write() = snap.counters.clone();
    }

    /// Restore everything *except* counters — used for undo, where IDs
    /// must remain monotonically increasing across undo/redo cycles.
    pub fn restore_without_counters(&self, snap: &RopeStoreSnapshot) {
        *self.rope.write() = snap.rope.clone();
        *self.roots.write() = snap.roots.clone();
        *self.documents.write() = snap.documents.clone();
        *self.frames.write() = snap.frames.clone();
        *self.blocks.write() = snap.blocks.clone();
        *self.lists.write() = snap.lists.clone();
        *self.resources.write() = snap.resources.clone();
        *self.tables.write() = snap.tables.clone();
        *self.table_cells.write() = snap.table_cells.clone();
        *self.format_runs.write() = snap.format_runs.clone();
        *self.block_images.write() = snap.block_images.clone();
        *self.block_footnote_refs.write() = snap.block_footnote_refs.clone();
        *self.footnote_markers.write() = snap.footnote_markers.clone();
        *self.block_offsets.write() = snap.block_offsets.clone();
        // counters intentionally not restored
    }

    pub fn create_savepoint(&self) -> u64 {
        let snap = self.snapshot();
        let mut id_counter = self.next_savepoint_id.write();
        let id = *id_counter;
        *id_counter += 1;
        drop(id_counter);
        self.savepoints.write().insert(id, snap);
        // The first savepoint a thread takes after `check_write_allowed` is the
        // auto-savepoint of the write it begins: the write is open until it is discarded.
        let mut state = self.transient.lock();
        if state.beginning.remove(&std::thread::current().id()) {
            state.open_writes.insert(id);
        }
        id
    }

    pub fn restore_savepoint(&self, savepoint_id: u64) {
        let snap = self
            .savepoints
            .read()
            .get(&savepoint_id)
            .expect("savepoint not found")
            .clone();
        self.restore(&snap);
    }

    pub fn discard_savepoint(&self, savepoint_id: u64) {
        self.savepoints.write().remove(&savepoint_id);
        self.transient.lock().open_writes.remove(&savepoint_id);
    }

    /// How many savepoints the store holds: one for each write transaction still open, and
    /// each one created explicitly and not discarded yet.
    pub fn savepoint_count(&self) -> usize {
        self.savepoints.read().len()
    }

    /// Get-and-increment counter for an entity type.
    pub(crate) fn next_id(&self, entity_name: &str) -> EntityId {
        let mut counters = self.counters.write();
        let counter = counters.entry(entity_name.to_string()).or_insert(1);
        let id = *counter;
        *counter += 1;
        id
    }

    /// Type-erased store snapshot (for the generic undo path).
    pub fn store_snapshot(&self) -> StoreSnapshot {
        StoreSnapshot::new(self.snapshot())
    }

    /// Restore from a type-erased store snapshot (undo semantic —
    /// counters preserved).
    pub fn restore_store_snapshot(&self, snap: &StoreSnapshot) {
        let s = snap
            .downcast_ref::<RopeStoreSnapshot>()
            .expect("StoreSnapshot must contain RopeStoreSnapshot");
        self.restore_without_counters(s);
    }

    // ── Transient edits ───────────────────────────────────────────────

    /// Begin a write that no undo history will hold, over the content as it is recorded.
    ///
    /// The first one takes a snapshot of the store, the **base**: the recorded content,
    /// which [`revert_transient_edits`](Self::revert_transient_edits) puts back. Every write
    /// made inside a [`TransientWrite`], up to that revert, is a transient edit over the
    /// same base, so the revert takes them all away at once and gives back the very content
    /// they were made over: the same text, formats and block identities, which is what the
    /// recorded history refers to.
    ///
    /// While the base is held, a write is begun only by the thread inside a
    /// `TransientWrite` (see [`check_write_allowed`](Self::check_write_allowed)).
    ///
    /// The write is kept only by [`TransientWrite::keep`]. Dropped without it, the store
    /// goes back to where it was when the write began, and the base is let go when this
    /// write is the one that took it, so a transient edit that failed partway or changed
    /// nothing leaves nothing outstanding.
    ///
    /// Refused, with nothing changed:
    ///
    /// * while another transient write is in progress, on this thread or another
    ///   ([`TransientWriteRefused::TransientWriteInProgress`]): they do not nest, since the
    ///   writer's permission is one thread's and each write goes back to its own starting
    ///   point;
    /// * while any other write is open ([`TransientWriteRefused::WriteInProgress`]): its
    ///   snapshot would hold half of that write, and a revert would take what the write
    ///   then records away.
    ///
    /// The snapshot is taken under the store's transient-edit lock, which every write takes
    /// as it begins, so no write begins or ends between the check and the snapshot.
    pub fn begin_transient_write(&self) -> Result<TransientWrite<'_>, TransientWriteRefused> {
        let mut state = self.transient.lock();
        if state.writer.is_some() {
            return Err(TransientWriteRefused::TransientWriteInProgress);
        }
        let thread = std::thread::current().id();
        if state.writes_open(Some(thread)) {
            return Err(TransientWriteRefused::WriteInProgress);
        }
        // This thread is not in the middle of beginning a write, since it is here: a check
        // it made without a write after it is dropped, and does not hold the store up.
        state.beginning.remove(&thread);
        let before = self.snapshot();
        let opens = state.base.is_none();
        // The first write's starting point is the base itself, and the base is what it
        // goes back to: one snapshot serves both.
        let before = if opens {
            state.base = Some(before);
            None
        } else {
            Some(before)
        };
        state.writer = Some(std::thread::current().id());
        Ok(TransientWrite {
            store: self,
            before,
            opens,
            kept: false,
        })
    }

    /// Whether transient edits are outstanding.
    pub fn has_transient_edits(&self) -> bool {
        self.transient.lock().base.is_some()
    }

    /// Put back the content the outstanding transient edits were made over, and end them.
    /// Returns whether there were any.
    ///
    /// Restores everything the base holds except the id counters, as an undo does (an id
    /// handed out stays spent), and the footnote markers, which the host sets and which are
    /// not part of the content.
    ///
    /// Refused, with nothing changed, while a transient write is in progress
    /// ([`TransientWriteRefused::TransientWriteInProgress`]), whose own end would put its
    /// starting point back over the revert, and while any other write is open
    /// ([`TransientWriteRefused::WriteInProgress`]), whose rollback would put back the
    /// transient content its savepoint holds.
    ///
    /// The content is put back under the store's transient-edit lock, before the base is let
    /// go: no write and no transient write begins until the store holds the recorded content
    /// again.
    pub fn revert_transient_edits(&self) -> Result<bool, TransientWriteRefused> {
        let mut state = self.transient.lock();
        if state.writer.is_some() {
            return Err(TransientWriteRefused::TransientWriteInProgress);
        }
        if state.writes_open(None) {
            return Err(TransientWriteRefused::WriteInProgress);
        }
        let Some(base) = state.base.as_ref() else {
            return Ok(false);
        };
        self.restore_content(base);
        state.base = None;
        Ok(true)
    }

    /// Begin a write, or refuse it while transient edits are outstanding unless the thread
    /// beginning it is inside a [`TransientWrite`] (see [`TransientEditsOutstanding`]).
    ///
    /// Every write calls it first and then takes its auto-savepoint
    /// ([`create_savepoint`](Self::create_savepoint)), which it discards as it ends: a write
    /// transaction does so as it begins and ends, and so does any other write to the store.
    /// The store counts the write open from that savepoint to its discard, and refuses a
    /// transient write and a revert meanwhile. A write ends at that discard: a transaction
    /// written through after its commit is not counted.
    ///
    /// It begins a write, so the next savepoint the thread takes is taken as the write's,
    /// and until it takes one, transient writes on other threads are refused. To ask
    /// whether a write would be allowed without beginning one, call
    /// [`is_write_allowed`](Self::is_write_allowed).
    pub fn check_write_allowed(&self) -> anyhow::Result<()> {
        let mut state = self.transient.lock();
        let thread = std::thread::current().id();
        if state.base.is_some() && state.writer != Some(thread) {
            return Err(TransientEditsOutstanding.into());
        }
        state.beginning.insert(thread);
        Ok(())
    }

    /// Whether a write begun now would be allowed (see
    /// [`check_write_allowed`](Self::check_write_allowed)), without beginning one.
    pub fn is_write_allowed(&self) -> bool {
        let state = self.transient.lock();
        state.base.is_none() || state.writer == Some(std::thread::current().id())
    }

    /// `snap`'s content, without its id counters or footnote markers.
    fn restore_content(&self, snap: &RopeStoreSnapshot) {
        let markers = self.footnote_markers.read().clone();
        self.restore_without_counters(snap);
        *self.footnote_markers.write() = markers;
    }
}

/// A write in progress that no undo history will hold. See
/// [`RopeStore::begin_transient_write`].
#[derive(Debug)]
pub struct TransientWrite<'a> {
    store: &'a RopeStore,
    /// The store as the write found it, when that is not the base (see `opens`): put back
    /// unless the write is kept.
    before: Option<RopeStoreSnapshot>,
    /// Whether this write took the base, which is then also where it started.
    opens: bool,
    kept: bool,
}

impl TransientWrite<'_> {
    /// Whether this is the first of the outstanding transient edits.
    pub fn opens(&self) -> bool {
        self.opens
    }

    /// Keep what the write changed, as a transient edit over the base.
    pub fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for TransientWrite<'_> {
    fn drop(&mut self) {
        let mut state = self.store.transient.lock();
        // The one transient write in progress (see `begin_transient_write`).
        state.writer = None;
        if self.kept {
            return;
        }
        // Not kept: back to where the write started, which for the first one is the base,
        // let go with it. Put back under the lock, as a revert does, so that nothing begins
        // over the content being put back. The writes the transient write opened end inside
        // it: one still open would put its savepoint back over this afterwards.
        debug_assert!(
            !state.writes_open(None),
            "a transient write ended with a write it opened still open"
        );
        if self.opens {
            if let Some(base) = state.base.as_ref() {
                self.store.restore_content(base);
            }
            state.base = None;
        } else if let Some(before) = self.before.take() {
            self.store.restore_content(&before);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Snapshot
// ─────────────────────────────────────────────────────────────────────────────

/// O(1)-clone snapshot. `Rope::clone()` shares the Arc-d B+ tree root;
/// every `im::HashMap::clone()` is HAMT-structural.
#[derive(Debug, Clone, Default)]
pub struct RopeStoreSnapshot {
    pub(crate) rope: Rope,
    pub(crate) roots: HashMap<EntityId, Root>,
    pub(crate) documents: HashMap<EntityId, Document>,
    pub(crate) frames: HashMap<EntityId, Frame>,
    pub(crate) blocks: HashMap<EntityId, Block>,
    pub(crate) lists: HashMap<EntityId, List>,
    pub(crate) resources: HashMap<EntityId, Resource>,
    pub(crate) tables: HashMap<EntityId, Table>,
    pub(crate) table_cells: HashMap<EntityId, TableCell>,
    pub(crate) format_runs: HashMap<EntityId, Vec<FormatRun>>,
    pub(crate) block_images: HashMap<EntityId, Vec<ImageAnchor>>,
    pub(crate) block_footnote_refs: HashMap<EntityId, Vec<FootnoteRefAnchor>>,
    pub(crate) footnote_markers: StdHashMap<String, String>,
    pub(crate) block_offsets: BlockOffsetIndex,
    pub(crate) counters: StdHashMap<String, EntityId>,
}

impl StoreSnapshotTrait for RopeStoreSnapshot {
    fn clone_box(&self) -> Box<dyn StoreSnapshotTrait> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
