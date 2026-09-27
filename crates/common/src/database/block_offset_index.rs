//! Sorted index of marker (block or table-anchor) → rope byte range.
//!
//! Tracks the byte offset at which each marker starts in the global
//! rope, plus the rope's total byte length. A marker is either a
//! real `Block` or a `TableAnchor` — the U+FFFC sentinel that
//! occupies a line on its own in a parent frame's range to represent
//! an embedded table (plan §1.6). Each marker extends from its
//! `byte_start` to the next marker's `byte_start` (or to
//! `total_bytes` for the last entry).
//!
//! Invariants:
//! - `entries` is sorted by `byte_start` ascending.
//! - No two entries share the same `byte_start`.
//! - The last entry's `byte_start ≤ total_bytes`.
//! - Empty `entries` ⟺ no markers in the document.

use crate::types::EntityId;
use im::HashMap as ImHashMap;
use std::sync::Arc;

/// Discriminates real blocks from table-anchor sentinels in the
/// offset index. The wrapped `EntityId` is the corresponding entity's
/// id (a Block id or a Table id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OffsetMarker {
    Block(EntityId),
    TableAnchor(EntityId),
}

impl OffsetMarker {
    pub fn as_block(self) -> Option<EntityId> {
        match self {
            OffsetMarker::Block(id) => Some(id),
            _ => None,
        }
    }

    pub fn as_table_anchor(self) -> Option<EntityId> {
        match self {
            OffsetMarker::TableAnchor(id) => Some(id),
            _ => None,
        }
    }

    pub fn is_block(self) -> bool {
        matches!(self, OffsetMarker::Block(_))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockOffsetIndex {
    /// `(marker, byte_start)` pairs sorted by `byte_start` ascending.
    ///
    /// Wrapped in `Arc` so the per-edit snapshot path (which clones
    /// `BlockOffsetIndex` as part of `Store::snapshot`) does an O(1)
    /// pointer bump instead of an O(N) memcpy of the underlying Vec.
    /// Mutators in this `impl` block go through `Arc::make_mut`, which
    /// deep-clones the Vec only on the first write after the Arc has
    /// been shared (i.e., after a snapshot was taken). External
    /// callers see `Arc<Vec<...>>` which derefs transparently to
    /// `&[...]` for reads.
    pub entries: Arc<Vec<(OffsetMarker, u32)>>,

    /// Total byte length of the rope this index describes. The last
    /// entry extends from its `byte_start` to this value.
    pub total_bytes: u32,

    /// O(1)-average lookup from marker to its position in `entries`.
    /// Maintained eagerly by the `push` / `push_block` / `insert_at` /
    /// `remove_at` / `clear` methods on this type — direct mutation of
    /// `entries` from outside leaves this cache stale, so callers must
    /// go through those methods (or call `rebuild_marker_index` after
    /// mutating in bulk).
    ///
    /// Stored as `im::HashMap` so the snapshot-clone path stays O(1).
    /// `shift_after` does not move positions, only byte_starts — so
    /// this map is unaffected by shifts.
    marker_index: ImHashMap<OffsetMarker, usize>,

    /// Cached count of `OffsetMarker::TableAnchor` entries. Maintained
    /// by the same mutator methods that maintain `marker_index`. Used
    /// by `rope_helpers::rope_positions_match_flow` to gate the
    /// per-edit position-refresh loop in O(1) instead of an O(N) walk
    /// over `entries`.
    table_anchor_count: usize,
}

impl BlockOffsetIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total_bytes(&self) -> u32 {
        self.total_bytes
    }

    pub fn set_total_bytes(&mut self, total: u32) {
        self.total_bytes = total;
    }

    /// Number of `OffsetMarker::TableAnchor` entries currently indexed.
    /// O(1) via a maintained counter. `rope_positions_match_flow` subtracts
    /// it from `len()` to count the blocks the rope mirrors without walking
    /// the entries.
    pub fn table_anchor_count(&self) -> usize {
        self.table_anchor_count
    }

    /// Insert a marker at a given byte position. The caller is
    /// responsible for keeping the `byte_start` ordered relative to
    /// neighbours — this method does NOT re-sort.
    pub fn insert_at(&mut self, position: usize, marker: OffsetMarker, byte_start: u32) {
        Arc::make_mut(&mut self.entries).insert(position, (marker, byte_start));
        // All markers at positions ≥ `position` shifted up by 1.
        for (m, p) in self.marker_index.iter_mut() {
            if *p >= position && *m != marker {
                *p += 1;
            }
        }
        self.marker_index.insert(marker, position);
        if matches!(marker, OffsetMarker::TableAnchor(_)) {
            self.table_anchor_count += 1;
        }
    }

    /// Insert consecutive markers starting at `position`, in order. The
    /// result is exactly what calling [`insert_at`](Self::insert_at) once
    /// per marker at `position`, `position + 1`, … would leave, but the
    /// marker index is walked once for the whole run instead of once per
    /// marker: inserting a run of N markers one at a time costs N walks of
    /// every marker in the document. The caller keeps the `byte_start`s
    /// ordered, as for `insert_at`.
    pub fn insert_run_at(&mut self, position: usize, run: &[(OffsetMarker, u32)]) {
        if run.is_empty() {
            return;
        }
        Arc::make_mut(&mut self.entries).splice(position..position, run.iter().copied());
        // Every marker already at or past `position` moves up by the run's length.
        for (_, p) in self.marker_index.iter_mut() {
            if *p >= position {
                *p += run.len();
            }
        }
        for (offset, (marker, _)) in run.iter().enumerate() {
            self.marker_index.insert(*marker, position + offset);
            if matches!(marker, OffsetMarker::TableAnchor(_)) {
                self.table_anchor_count += 1;
            }
        }
    }

    /// Take bytes out after several entries at once: for each
    /// `(position, bytes)`, every entry past `position` moves back by
    /// `bytes`, and so does `total_bytes`. The result is exactly what calling
    /// [`shift_after`](Self::shift_after)`(entries[position].1 + 1, -bytes)`
    /// once per pair leaves, provided each entry at `position` starts before
    /// the entry after it, as an entry with content or a boundary after it
    /// does. The entries are walked once for the whole list instead of once
    /// per pair. `cuts` is in increasing `position` order.
    pub fn remove_bytes_after(&mut self, cuts: &[(usize, u32)]) {
        if cuts.is_empty() {
            return;
        }
        entries_rewritten(self.entries.len());
        let mut removed: u32 = 0;
        let mut pending = cuts.iter().peekable();
        for (position, (_, byte_start)) in Arc::make_mut(&mut self.entries).iter_mut().enumerate() {
            debug_assert!(*byte_start >= removed, "a cut past an entry's start");
            *byte_start = byte_start.saturating_sub(removed);
            while let Some(&&(cut_position, bytes)) = pending.peek() {
                if cut_position != position {
                    break;
                }
                removed = removed.saturating_add(bytes);
                pending.next();
            }
        }
        self.total_bytes = self.total_bytes.saturating_sub(removed);
    }

    /// Drop the entries `dropped` flags (one flag per entry, by position) and
    /// take the byte ranges `cuts` out of the text they index: every kept
    /// entry moves back by the bytes cut in front of it, and `total_bytes`
    /// by all of them. `cuts` is disjoint and in increasing order, and no cut
    /// straddles a kept entry's first byte: each ends at or before it, or
    /// starts at or after it. A cut starting exactly there is cut after the
    /// entry, which keeps its offset: that is how the trailing entries of the
    /// rope go when the entry before them is empty. One walk of the entries,
    /// however many are dropped: removing them one at a time walked the index
    /// once each.
    pub fn remove_entries(&mut self, dropped: &[bool], cuts: &[(u32, u32)]) {
        debug_assert_eq!(dropped.len(), self.entries.len());
        entries_rewritten(self.entries.len());
        let mut kept: Vec<(OffsetMarker, u32)> = Vec::with_capacity(self.entries.len());
        let mut pending = cuts.iter().peekable();
        let mut removed: u32 = 0;
        for (position, &(marker, byte_start)) in self.entries.iter().enumerate() {
            while let Some(&&(cut_start, cut_end)) = pending.peek() {
                if cut_end > byte_start {
                    break;
                }
                debug_assert!(cut_start <= cut_end, "a cut ends before it starts");
                removed += cut_end - cut_start;
                pending.next();
            }
            if !dropped.get(position).copied().unwrap_or(false) {
                kept.push((marker, byte_start - removed));
            }
        }
        let cut_total: u32 = cuts.iter().map(|(start, end)| end - start).sum();
        self.entries = Arc::new(kept);
        self.total_bytes = self.total_bytes.saturating_sub(cut_total);
        self.rebuild_marker_index();
    }

    /// Append a marker at the end (its `byte_start` must be ≥ the last
    /// entry's `byte_start`).
    pub fn push(&mut self, marker: OffsetMarker, byte_start: u32) {
        debug_assert!(
            self.entries
                .last()
                .map(|(_, bs)| byte_start >= *bs)
                .unwrap_or(true),
            "push must preserve ordering"
        );
        let position = self.entries.len();
        Arc::make_mut(&mut self.entries).push((marker, byte_start));
        self.marker_index.insert(marker, position);
        if matches!(marker, OffsetMarker::TableAnchor(_)) {
            self.table_anchor_count += 1;
        }
    }

    /// Convenience: register a block by id. Equivalent to
    /// `push(OffsetMarker::Block(id), byte_start)`.
    pub fn push_block(&mut self, block_id: EntityId, byte_start: u32) {
        self.push(OffsetMarker::Block(block_id), byte_start);
    }

    /// Remove the entry at the given position. Panics if out of bounds.
    pub fn remove_at(&mut self, position: usize) -> (OffsetMarker, u32) {
        let removed = Arc::make_mut(&mut self.entries).remove(position);
        self.marker_index.remove(&removed.0);
        // All markers at positions > `position` shifted down by 1.
        for (_, p) in self.marker_index.iter_mut() {
            if *p > position {
                *p -= 1;
            }
        }
        if matches!(removed.0, OffsetMarker::TableAnchor(_)) {
            self.table_anchor_count = self.table_anchor_count.saturating_sub(1);
        }
        removed
    }

    /// Remove a contiguous range of entries, equivalent to
    /// `entries.drain(start..=end_inclusive)` plus the matching
    /// marker_index maintenance. Returns the removed entries.
    pub fn drain_inclusive(
        &mut self,
        start: usize,
        end_inclusive: usize,
    ) -> Vec<(OffsetMarker, u32)> {
        let removed: Vec<_> = Arc::make_mut(&mut self.entries)
            .drain(start..=end_inclusive)
            .collect();
        let removed_anchors = removed
            .iter()
            .filter(|(m, _)| matches!(m, OffsetMarker::TableAnchor(_)))
            .count();
        self.table_anchor_count = self.table_anchor_count.saturating_sub(removed_anchors);
        for (m, _) in &removed {
            self.marker_index.remove(m);
        }
        let shift = removed.len();
        // All markers at positions > end_inclusive shift down by `shift`.
        for (_, p) in self.marker_index.iter_mut() {
            if *p > end_inclusive {
                *p -= shift;
            }
        }
        removed
    }

    /// Drop every entry and reset `total_bytes` to zero. Equivalent to
    /// `*self = Self::default()` but expressed as a method so callers
    /// don't need to depend on `Default`.
    pub fn clear(&mut self) {
        Arc::make_mut(&mut self.entries).clear();
        self.marker_index = ImHashMap::new();
        self.total_bytes = 0;
        self.table_anchor_count = 0;
    }

    /// Rebuild `marker_index` and `table_anchor_count` from `entries`.
    /// Use after bulk mutations that bypassed the maintenance methods
    /// (e.g. raw `entries.push` in test setup).
    pub fn rebuild_marker_index(&mut self) {
        let mut idx = ImHashMap::new();
        let mut anchors = 0usize;
        for (i, (m, _)) in self.entries.iter().enumerate() {
            idx.insert(*m, i);
            if matches!(m, OffsetMarker::TableAnchor(_)) {
                anchors += 1;
            }
        }
        self.marker_index = idx;
        self.table_anchor_count = anchors;
    }

    /// Byte range `(start, end)` of a marker. `end` is the next
    /// marker's `byte_start` (or `total_bytes` for the last entry).
    /// Returns `None` if the marker is not indexed.
    ///
    /// O(1) average via the `marker_index` map.
    pub fn range_of(&self, marker: OffsetMarker) -> Option<(u32, u32)> {
        let idx = *self.marker_index.get(&marker)?;
        let start = self.entries[idx].1;
        let end = self
            .entries
            .get(idx + 1)
            .map(|(_, bs)| *bs)
            .unwrap_or(self.total_bytes);
        Some((start, end))
    }

    /// Byte range for a block-id specifically. Convenience for the
    /// common case.
    pub fn range_of_block(&self, block_id: EntityId) -> Option<(u32, u32)> {
        self.range_of(OffsetMarker::Block(block_id))
    }

    /// Like `range_of`, but also reports whether the marker has a
    /// successor entry. Callers that need to strip the trailing
    /// inter-block `\n` boundary (e.g. `block_content_via_store`,
    /// `block_char_length`) use this to distinguish "end == next
    /// marker's byte_start, with a real `\n` between us" from "end
    /// == total_bytes, no separator after".
    ///
    /// O(1) average via `marker_index`.
    pub fn range_with_successor(&self, marker: OffsetMarker) -> Option<(u32, u32, bool)> {
        let idx = *self.marker_index.get(&marker)?;
        let start = self.entries[idx].1;
        let has_successor = idx + 1 < self.entries.len();
        let end = if has_successor {
            self.entries[idx + 1].1
        } else {
            self.total_bytes
        };
        Some((start, end, has_successor))
    }

    /// Position of `marker` in `entries`. O(1) average via the
    /// `marker_index` cache. Returns `None` if the marker is not
    /// indexed.
    pub fn position_of(&self, marker: OffsetMarker) -> Option<usize> {
        self.marker_index.get(&marker).copied()
    }

    /// Marker whose byte range covers `byte`. Returns `None` if the
    /// index is empty or `byte` falls past `total_bytes`.
    ///
    /// `byte == total_bytes` is treated as belonging to the last entry
    /// (this is the cursor-at-end-of-document case).
    pub fn marker_at_byte(&self, byte: u32) -> Option<OffsetMarker> {
        if self.entries.is_empty() {
            return None;
        }
        if byte > self.total_bytes {
            return None;
        }
        let idx = match self.entries.binary_search_by_key(&byte, |(_, bs)| *bs) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        Some(self.entries[idx].0)
    }

    /// Block id whose byte range covers `byte`, ignoring table-anchor
    /// markers. Returns `None` if no block covers the byte.
    pub fn block_at_byte(&self, byte: u32) -> Option<EntityId> {
        self.marker_at_byte(byte).and_then(|m| m.as_block())
    }

    /// Convert an absolute rope byte offset into
    /// `(marker, byte_in_marker)`. Returns `None` for offsets past the
    /// end or for an empty index.
    pub fn byte_to_marker_byte(&self, byte: u32) -> Option<(OffsetMarker, u32)> {
        let marker = self.marker_at_byte(byte)?;
        let (start, _) = self.range_of(marker)?;
        Some((marker, byte - start))
    }

    /// Block-only variant of `byte_to_marker_byte`.
    pub fn byte_to_block_byte(&self, byte: u32) -> Option<(EntityId, u32)> {
        let block_id = self.block_at_byte(byte)?;
        let (start, _) = self.range_of_block(block_id)?;
        Some((block_id, byte - start))
    }

    /// Shift every entry whose `byte_start ≥ threshold` by `delta`
    /// bytes, and adjust `total_bytes` by `delta`. Used after a rope
    /// insert (positive delta) or delete (negative delta) to keep the
    /// index in sync without a full rebuild.
    ///
    /// `entries` is sorted by `byte_start`, so the affected suffix is
    /// located via `partition_point` (O(log n)) and only that suffix
    /// is walked.
    pub fn shift_after(&mut self, threshold: u32, delta: i32) {
        let start = self.entries.partition_point(|(_, bs)| *bs < threshold);
        if start < self.entries.len() {
            entries_rewritten(self.entries.len() - start);
            for (_, bs) in Arc::make_mut(&mut self.entries)[start..].iter_mut() {
                *bs = apply_delta(*bs, delta);
            }
        }
        self.total_bytes = apply_delta(self.total_bytes, delta);
    }
}

/// Record `count` entries an offset shift rewrote. Unit tests count them, which is
/// how `rope_helpers`' `tests::clearing_blocks_together_walks_the_index_once`
/// guards `rope_clear_blocks`' cost in the debug builds CI tests with, where
/// timing it cannot.
fn entries_rewritten(count: usize) {
    #[cfg(test)]
    tests::ENTRIES_REWRITTEN.with(|rewritten| rewritten.set(rewritten.get() + count));
    #[cfg(not(test))]
    let _ = count;
}

fn apply_delta(value: u32, delta: i32) -> u32 {
    if delta >= 0 {
        value
            .checked_add(delta as u32)
            .expect("byte offset overflow")
    } else {
        let abs = (-delta) as u32;
        value
            .checked_sub(abs)
            .expect("byte offset would go negative")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;

    thread_local! {
        /// Entries the offset shifts rewrote on this thread (see [`super::entries_rewritten`]).
        pub(crate) static ENTRIES_REWRITTEN: Cell<usize> = const { Cell::new(0) };
    }
}
