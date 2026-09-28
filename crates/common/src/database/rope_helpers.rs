//! Helpers for writing the global character rope from use cases.
//!
//! Each helper mutates `store.rope` and `store.block_offsets`
//! together so callers can stay oblivious to the underlying layout.
//! Read helpers (`block_content_via_store`) source content from the
//! rope and return empty when a block is not yet registered in the
//! offset index.

use crate::database::Store;
use crate::database::block_offset_index::OffsetMarker;
use crate::entities::Block;
use crate::format_runs::{
    FootnoteRefAnchor, FormatRun, FormatRunError, ImageAnchor, ReplaceFormatPolicy,
    check_well_formed, logical_offset_to_byte, shift_footnote_refs_for_delete,
    shift_footnote_refs_for_insert, shift_images_for_delete, shift_images_for_insert,
    shift_runs_for_replace,
};
use crate::types::EntityId;
use std::collections::HashMap;

/// Read a block's content from the global rope via `block_offsets`,
/// stripping the trailing `\n` boundary that `range_of` includes for
/// non-last entries. Returns an empty string if the block isn't
/// registered in the offset index (e.g. a freshly-created block that
/// hasn't been spliced into the rope yet — `setup_with_text` test
/// docs use this path).
pub fn block_content_via_store(block: &Block, store: &Store) -> String {
    let offsets = store.block_offsets.read();
    let marker = OffsetMarker::Block(block.id);
    let Some((bs, be, has_successor)) = offsets.range_with_successor(marker) else {
        return String::new();
    };
    // Drop the trailing inter-block boundary `\n` ONLY when this block
    // has a successor entry — that one byte is the boundary `\n` between
    // this block and the next. The last entry has no trailing boundary;
    // any final `\n` is real content.
    let content_end = if has_successor && be > bs { be - 1 } else { be };
    drop(offsets);
    let rope = store.rope.read();
    rope.byte_slice(bs as usize..content_end as usize)
        .to_string()
}

/// Logical character count of a block — what the old
/// `Block.text_length` field used to cache. Image anchors are stored as
/// `\u{FFFC}` (one char, three bytes) inside the rope content, so the
/// char count already covers them. Returns 0 for blocks not registered
/// in the offset index.
///
/// O(log n) via `ropey::Rope::byte_to_char` — does NOT materialize the
/// block's text into a String. Replaces the prior O(L) implementation
/// that counted chars by walking UTF-8 over a cloned slice.
pub fn block_char_length(block: &Block, store: &Store) -> i64 {
    let offsets = store.block_offsets.read();
    let marker = OffsetMarker::Block(block.id);
    let Some((bs, be, has_successor)) = offsets.range_with_successor(marker) else {
        return 0;
    };
    let content_end_bytes = if has_successor && be > bs { be - 1 } else { be };
    drop(offsets);
    let rope = store.rope.read();
    let char_start = rope.byte_to_char(bs as usize);
    let char_end = rope.byte_to_char(content_end_bytes as usize);
    (char_end - char_start) as i64
}

/// Return the absolute character position of a block's start in the
/// document, derived from the rope via `BlockOffsetIndex`.
///
/// O(log n): one `range_of_block` lookup + one `byte_to_char` conversion.
/// Falls back to `block.document_position` for blocks not registered in
/// the index — blocks under non-top-level frames (`insert_frame_uc` only
/// mirrors top-level frames). Table-cell blocks ARE registered: they are
/// mirrored inline into the rope in document order. The stored field is
/// set authoritatively by the non-flow paths and stays correct for them
/// across main-flow edits.
pub fn block_document_position(block: &Block, store: &Store) -> i64 {
    let offsets = store.block_offsets.read();
    let Some((byte_start, _)) = offsets.range_of_block(block.id) else {
        return block.document_position;
    };
    drop(offsets);
    let rope = store.rope.read();
    rope.byte_to_char(byte_start as usize) as i64
}

/// Bring a batch of `document_position` slots in step with the rope.
///
/// `slots` pairs each block's id with the field to overwrite. A block the offset index
/// holds gets its rope-derived start; one it does not hold keeps what it had. When the rope
/// is not the document's position space (see [`rope_positions_match_flow`]) nothing is
/// touched: there the stored field is maintained by the editing use cases and is the truth.
///
/// This is the reader's half of the bargain `insert_text_uc` and `delete_text_uc` struck
/// when they stopped shifting every later block on every keystroke: the stored field drifts
/// by exactly what was typed (or removed) since the last time something wrote it, so a use
/// case comparing a caller's position with it compares two different spaces. That is how a
/// cut at a paragraph's end came to carry the paragraph break and the head of the next
/// paragraph away while the deletion, which walks the rope, took only the selection — and
/// how bold, replace and "make a list" landed the same number of characters late.
pub fn refresh_positions_from_rope<'a>(
    store: &Store,
    slots: impl IntoIterator<Item = (EntityId, &'a mut i64)>,
) {
    if !rope_positions_match_flow(store) {
        return;
    }
    let offsets = store.block_offsets.read();
    let rope = store.rope.read();
    for (block_id, slot) in slots {
        if let Some((byte_start, _)) = offsets.range_of_block(block_id) {
            *slot = rope.byte_to_char(byte_start as usize) as i64;
        }
    }
}

/// [`refresh_positions_from_rope`] over block entities — what a use case holds after
/// `get_block_multi`. Call it before sorting by `document_position` or comparing that field
/// with a position the caller supplied.
pub fn refresh_block_positions(blocks: &mut [Block], store: &Store) {
    refresh_positions_from_rope(
        store,
        blocks.iter_mut().map(|b| (b.id, &mut b.document_position)),
    );
}

/// Whether the rope's char-position space matches the user-visible
/// flow positions that `Block.document_position` is computed against.
///
/// They match when every block is mirrored to the rope as a contiguous
/// run. They DON'T match when:
/// 1. The document contains tables — cell content sits at separate rope
///    byte ranges (plan §1.6), so the rope is missing the cells'
///    flow-position contribution.
/// 2. The document contains sub-frames whose blocks aren't mirrored —
///    `insert_frame_uc` only mirrors top-level frames.
///
/// When this returns `true`, readers can derive `document_position`
/// directly from the rope (O(log n)) and the use-case-side
/// position-refresh loops can be skipped. When it returns `false`,
/// readers must consult the maintained `Block.document_position`
/// stored field, and the loops are required to keep it correct.
pub fn rope_positions_match_flow(store: &Store) -> bool {
    let offsets = store.block_offsets.read();
    // The rope is authoritative as long as EVERY block is mirrored into it.
    // Tables no longer disqualify it: cell content is mirrored inline, in
    // document order, and the table itself occupies a 1-char anchor sentinel
    // — so the rope's char space matches the user-visible flow order. (The
    // flow snapshot derives its block positions from this same rope space,
    // so the two agree by construction.) Only count Block markers; the
    // TableAnchor sentinel entries are not blocks.
    // The index holds exactly two kinds of entry, so the blocks it mirrors are what is
    // left once the anchors are taken away — an O(1) answer for a check that sits on the
    // clamp of every caret move, insert and delete.
    let indexed_block_count = offsets.len() - offsets.table_anchor_count();
    drop(offsets);
    let total_block_count = store.blocks.read().len();
    indexed_block_count == total_block_count
}

/// Where the main text ends, as a position, when the rope holds anything after it, which
/// is the footnote bodies: the end of the main frame's last entry. `None` when nothing
/// follows the main text, or when the rope's positions are not the flow's.
///
/// No view shows a footnote's body, and the loads put every body after the main text: a
/// caret moved on from the end of the text went into the first note, and typing there
/// edited it unseen. The cursor's moves stop here.
///
/// Read down from the end of the main frame, so it costs the depth of the frames the text
/// ends in (and the cells of a table it ends with), whatever the length of the text and
/// however many notes follow it. Every forward move of a caret asks it; it gathered every
/// block of every body first, milliseconds a keystroke in a document of many notes.
pub fn main_text_end(store: &Store) -> Option<i64> {
    if !rope_positions_match_flow(store) {
        return None;
    }
    let main_frame = store
        .documents
        .read()
        .values()
        .next()
        .and_then(|document| document.frames.first().copied())?;
    let last = {
        let tables = store.tables.read();
        let cells = store.table_cells.read();
        let frames = store.frames.read();
        last_flow_entry(main_frame, &tables, &cells, &frames)?
    };
    let offsets = store.block_offsets.read();
    let last_of_text = offsets.position_of(last)?;
    looked_at(1);
    // The text of that entry ends at the boundary before whatever follows it.
    let next_start = offsets.entries.get(last_of_text + 1)?.1;
    drop(offsets);
    let end_byte = next_start.saturating_sub(1) as usize;
    Some(store.rope.read().byte_to_char(end_byte) as i64)
}

/// The last entry of the flow of `frame_id`, in reading order: its last block, or the last
/// entry of the quotation or the table it ends with. `None` when it holds nothing.
fn last_flow_entry(
    frame_id: EntityId,
    tables: &im::HashMap<EntityId, crate::entities::Table>,
    cells: &im::HashMap<EntityId, crate::entities::TableCell>,
    frames: &im::HashMap<EntityId, crate::entities::Frame>,
) -> Option<OffsetMarker> {
    // Each frame on the way down, with how many of its entries are still to look at: an
    // empty quotation at the end sends the walk back to the entry before it.
    let mut stack: Vec<(EntityId, usize)> =
        vec![(frame_id, frames.get(&frame_id)?.child_order.len())];
    let mut seen: std::collections::HashSet<EntityId> = std::collections::HashSet::new();
    seen.insert(frame_id);
    while let Some((id, remaining)) = stack.pop() {
        looked_at(1);
        let Some(entry) = remaining
            .checked_sub(1)
            .and_then(|at| frames.get(&id)?.child_order.get(at).copied())
        else {
            continue;
        };
        stack.push((id, remaining - 1));
        if entry > 0 {
            return Some(OffsetMarker::Block(entry as EntityId));
        }
        let sub_id = entry.unsigned_abs() as EntityId;
        let Some(sub) = frames.get(&sub_id) else {
            continue;
        };
        if !seen.insert(sub_id) {
            continue;
        }
        match sub.table {
            Some(table_id) => {
                let mut ordered: Vec<OffsetMarker> = Vec::new();
                table_reading_order(table_id, tables, cells, frames, &mut ordered);
                looked_at(ordered.len());
                return ordered.last().copied();
            }
            None => stack.push((sub_id, sub.child_order.len())),
        }
    }
    None
}

/// Record `count` frames or index entries [`main_text_end`] looked at. Unit tests count
/// them: it runs on every forward move of a caret, and a debug build cannot time it.
fn looked_at(count: usize) {
    #[cfg(test)]
    tests::LOOKED_AT.with(|looked| looked.set(looked.get() + count));
    #[cfg(not(test))]
    let _ = count;
}

/// Locate which block contains a given absolute char position in the
/// document, returning `(block_id, char_offset_in_block, block_char_start)`
/// in O(log n) using the rope + `BlockOffsetIndex` instead of an O(N)
/// linear walk of all blocks.
///
/// Replaces the per-keystroke hot path in editing use cases
/// (`find_block_at_position_sequential`) which fetched every block
/// + called `block_char_length` per block. For an N-block document
///   each editor keystroke now costs O(log n) lookups instead of O(N).
///
/// Returns `None` only when some block is unmirrored (a sub-frame
/// inserted with a parent — `insert_frame_uc` mirrors only top-level
/// frames), where the rope's char space diverges from flow order and
/// callers must fall back to the slow per-block walk. Tables are fine:
/// cell content is mirrored inline in document order and each table is a
/// 1-char anchor sentinel, so byte→block lookup resolves correctly.
///
/// `position` past the document end clamps to the last block's
/// end-of-content.
pub fn find_block_at_char_position(store: &Store, position: i64) -> Option<(EntityId, i64, i64)> {
    // Fast path is only valid when EVERY block in the document is
    // mirrored to the rope. Disqualifying cases:
    //
    // 1. Documents containing tables — cell content sits at separate
    //    rope byte ranges (plan §1.6) so byte→block lookup finds the
    //    wrong block for cursor positions inside cells.
    //
    // 2. Documents containing sub-frames inserted with a parent —
    //    `insert_frame_uc` currently only mirrors top-level frames to
    //    the rope (parent=None case), leaving sub-frame blocks
    //    unregistered in the offset index. Detect by comparing block
    //    counts: if the rope index has fewer Block markers than the
    //    store has Block entities, some are unmirrored.
    let offsets = store.block_offsets.read();
    // Valid as long as every block is mirrored to the rope. Tables are fine:
    // cell content is mirrored inline in document order and the table is a
    // 1-char anchor sentinel, so byte→block lookup resolves correctly for any
    // non-sentinel position. A position that lands exactly on the sentinel
    // resolves to a TableAnchor marker, where `as_block()` returns None and
    // the caller falls back to the slow walk. Only count Block markers.
    let indexed_block_count = offsets.entries.iter().filter(|(m, _)| m.is_block()).count();
    let total_block_count = store.blocks.read().len();
    if indexed_block_count != total_block_count {
        return None;
    }
    drop(offsets);

    let rope = store.rope.read();
    let total_chars = rope.len_chars() as i64;
    let pos_clamped = position.clamp(0, total_chars);
    let abs_byte = rope.char_to_byte(pos_clamped as usize);
    drop(rope);

    let offsets = store.block_offsets.read();
    let block_id = offsets.marker_at_byte(abs_byte as u32)?.as_block()?;
    let (bs, be, has_successor) = offsets.range_with_successor(OffsetMarker::Block(block_id))?;
    let content_end = if has_successor && be > bs { be - 1 } else { be };

    drop(offsets);

    let rope = store.rope.read();
    let block_char_start = rope.byte_to_char(bs as usize) as i64;
    // Clamp the cursor's byte to the block's content area (not into the
    // trailing `\n` boundary if any).
    let byte_for_char = std::cmp::min(abs_byte, content_end as usize);
    let abs_char = rope.byte_to_char(byte_for_char) as i64;
    let char_in_block = abs_char - block_char_start;

    // NOTE: separator semantics differ across use cases. Callers like
    // `get_block_at_position_uc` interpret "position at the end of a
    // non-empty block with a successor" as "belongs to the next
    // block"; callers like `insert_text_uc` want the previous block
    // with offset == block_char_len. This helper returns the
    // previous-block answer (the simpler semantic); use-case-specific
    // advance logic lives at the call site.
    let _ = has_successor;

    Some((block_id, char_in_block, block_char_start))
}

/// Convert an in-block char offset to an in-block byte offset using the
/// rope's index. Both inputs and outputs are relative to the start of
/// the block's content (NOT absolute rope positions). The char offset
/// is clamped to the block's logical length so callers don't need to
/// pre-validate.
///
/// O(log n) via `ropey::Rope::char_to_byte` — replaces the O(L)
/// "materialize block text + walk char_indices" pattern that
/// `set_text_format_uc` / `merge_text_format_uc` used. Returns
/// `(byte_offset_in_block, content_byte_len)` so callers can also
/// pass the content length to `debug_assert_well_formed` without
/// materializing the text.
///
/// Returns `(0, 0)` for blocks not registered in the offset index.
pub fn block_char_to_byte_in_block(
    store: &Store,
    block_id: EntityId,
    char_offset: usize,
) -> (u32, usize) {
    let offsets = store.block_offsets.read();
    let marker = OffsetMarker::Block(block_id);
    let Some((bs, be, has_successor)) = offsets.range_with_successor(marker) else {
        return (0, 0);
    };
    let content_end_bytes = if has_successor && be > bs { be - 1 } else { be };
    let content_byte_len = (content_end_bytes - bs) as usize;
    drop(offsets);

    let rope = store.rope.read();
    let block_char_start = rope.byte_to_char(bs as usize);
    let block_char_end = rope.byte_to_char(content_end_bytes as usize);
    let block_char_len = block_char_end - block_char_start;

    // Clamp char_offset to block's char length.
    let clamped = std::cmp::min(char_offset, block_char_len);
    let abs_byte = rope.char_to_byte(block_char_start + clamped);
    let byte_in_block = (abs_byte - bs as usize) as u32;
    (byte_in_block, content_byte_len)
}

/// Fast-path full-document plain text: returns `Some(rope.to_string())`
/// iff the document is in the canonical flat layout — no table anchors
/// in the offset index, single top-level frame. In that case the
/// rope's byte order is the same as the document-flow order, so one
/// `to_string()` allocation replaces the O(N) per-block walk +
/// per-block `Cow<str>` materialization that `build_full_text` /
/// `export_plain_text` would otherwise do.
///
/// Returns `None` for documents containing tables or nested frames —
/// those require the per-frame, per-block traversal because table
/// cell content lives in separate byte ranges later in the rope
/// (plan §1.6), not interleaved with the parent frame's bytes.
///
/// `top_frame_count` is the caller-known number of top-level frames
/// (typically obtained from `Document.frames.len()`). Callers
/// already have this value and pass it in to avoid a redundant uow
/// query.
pub fn rope_flat_text_if_simple(store: &Store, top_frame_count: usize) -> Option<String> {
    if top_frame_count != 1 {
        return None;
    }
    let offsets = store.block_offsets.read();
    let has_table = offsets
        .entries
        .iter()
        .any(|(m, _)| matches!(m, OffsetMarker::TableAnchor(_)));
    if has_table {
        return None;
    }
    drop(offsets);
    Some(store.rope.read().to_string())
}

/// Whole-document searchable text straight from the rope, valid whenever
/// the rope's char-position space matches the user-visible flow order
/// (`rope_positions_match_flow`).
///
/// Unlike `rope_flat_text_if_simple` this does NOT bail on tables or
/// multiple top-level frames: table-cell content is mirrored inline into
/// the rope in document order and each table occupies a 1-char anchor
/// sentinel, so the rope already contains all searchable text — including
/// cell text — at the same char offsets that match positions are reported
/// in. Returns `None` only when some block is unmirrored (e.g. a sub-frame
/// inserted with a parent), where the caller must fall back to the
/// per-frame, per-block traversal.
pub fn rope_full_text_if_flow_matches(store: &Store) -> Option<String> {
    rope_positions_match_flow(store).then(|| store.rope.read().to_string())
}

/// Reset the rope to empty and clear `block_offsets`. Called by
/// importers when they replace the entire document content.
pub fn rope_reset(store: &Store) {
    *store.rope.write() = ropey::Rope::new();
    *store.block_offsets.write() = crate::database::block_offset_index::BlockOffsetIndex::new();
}

/// Append `text` to the end of the rope and register `block_id` at
/// the byte position where the text starts. Returns that byte offset.
///
/// Callers are responsible for inserting an inter-block `\n`
/// (`rope_insert_block_boundary`) before each block AFTER the first
/// in a contiguous frame.
pub fn rope_append_block(store: &Store, block_id: EntityId, text: &str) -> u32 {
    let mut rope = store.rope.write();
    let byte_start = rope.len_bytes() as u32;
    let char_end = rope.len_chars();
    rope.insert(char_end, text);
    let new_total = rope.len_bytes() as u32;
    drop(rope);

    let mut offsets = store.block_offsets.write();
    offsets.push_block(block_id, byte_start);
    offsets.set_total_bytes(new_total);
    byte_start
}

/// Insert `text` as a new block at `byte_pos` in the rope, prepending
/// a `\n` boundary. Used by `insert_table_uc` to place cell blocks at
/// the end of their containing top-level frame's range (plan §1.6),
/// rather than always at rope end.
///
/// Total bytes inserted: `1 + text.len()`. The block's content
/// occupies `[byte_pos + 1, byte_pos + 1 + text.len())`. The block
/// entry is registered at `byte_pos + 1` in `block_offsets`.
///
/// Existing entries with `byte_start == byte_pos` (e.g. a previous
/// empty block whose end coincides with this insertion point) are
/// kept BEFORE the new entry in the Vec, since the inserted `\n`
/// boundary belongs after them. Entries strictly past `byte_pos`
/// shift forward by `(1 + text.len())` bytes.
///
/// When `byte_pos == total_bytes`, behaves like
/// `rope_insert_block_boundary` followed by `rope_append_block`.
pub fn rope_insert_block_at(store: &Store, byte_pos: u32, block_id: EntityId, text: &str) {
    let delta = (1 + text.len()) as i32;
    // Vec position: insert AFTER any entry at byte_pos itself
    // (those represent earlier empty blocks whose `\n` boundary
    // we are placing now). Only entries strictly past byte_pos
    // come after our new entry in the Vec.
    let new_entry_vec_pos = {
        let offsets = store.block_offsets.read();
        offsets
            .entries
            .iter()
            .position(|(_, bs)| *bs > byte_pos)
            .unwrap_or(offsets.entries.len())
    };
    {
        let mut rope = store.rope.write();
        let char_idx = rope.byte_to_char(byte_pos as usize);
        let mut combined = String::with_capacity(1 + text.len());
        combined.push('\n');
        combined.push_str(text);
        rope.insert(char_idx, &combined);
    }
    let mut offsets = store.block_offsets.write();
    // Shift entries strictly past byte_pos. Entries AT byte_pos
    // (the prior empty block) stay where they are — the new `\n`
    // is conceptually "after" them.
    offsets.shift_after(byte_pos + 1, delta);
    offsets.insert_at(
        new_entry_vec_pos,
        OffsetMarker::Block(block_id),
        byte_pos + 1,
    );
}

/// Walks up `frame.parent_frame` to find the top-level ancestor of
/// the given frame, then returns the end byte of that top-level
/// frame's current rope range.
///
/// Not where a table's cells belong. They follow the table's anchor, in
/// reading order (see [`rope_insert_run_after`] and
/// [`rope_place_new_cell_blocks`]); the end of the enclosing frame is that
/// place only when the table is the last thing in the frame, and cells put
/// there after a table followed by text left the rope out of flow order.
/// No editing use case calls this any more.
///
/// Reads `block_offsets`/`frames`/`tables`/`table_cells` directly, so
/// the result is fresh even when `Frame.byte_range` has not yet been
/// recomputed at commit time.
pub fn top_level_frame_end_byte(store: &Store, frame_id: EntityId) -> u32 {
    let top_id = {
        let frames = store.frames.read();
        let mut current = frame_id;
        loop {
            let Some(f) = frames.get(&current) else {
                return 0;
            };
            match f.parent_frame {
                None => break current,
                Some(p) => current = p,
            }
        }
    };
    let (_min, max) = compute_frame_byte_range_recursive(store, top_id);
    max
}

/// Append a new empty block to the end of the rope, separating it
/// from any prior content with a `\n` boundary (only if the rope is
/// already non-empty). Registers `block_id` at the resulting byte
/// position. Returns that byte position. Used when `insert_frame_uc`
/// creates a new top-level frame with a single empty block.
pub fn rope_append_empty_block(store: &Store, block_id: EntityId) -> u32 {
    let was_empty = store.rope.read().len_bytes() == 0;
    if !was_empty {
        rope_insert_block_boundary(store);
    }
    let pos = store.rope.read().len_bytes() as u32;
    let mut offsets = store.block_offsets.write();
    offsets.push_block(block_id, pos);
    offsets.set_total_bytes(pos);
    pos
}

/// Register `block_id` as a new empty block in front of every other entry:
/// at byte 0, followed by a boundary `\n` when anything comes after it.
/// Every other entry moves one byte on.
pub fn rope_insert_empty_block_first(store: &Store, block_id: EntityId) {
    let mut offsets = store.block_offsets.write();
    if !offsets.is_empty() {
        {
            let mut rope = store.rope.write();
            rope.insert(0, "\n");
        }
        offsets.shift_after(0, 1);
    }
    offsets.insert_at(0, OffsetMarker::Block(block_id), 0);
}

/// Append a single `\n` inter-block boundary character to the end of
/// the rope. Does NOT register a block — this is the sentinel between
/// two adjacent blocks within the same frame (plan §1.4).
pub fn rope_insert_block_boundary(store: &Store) {
    let mut rope = store.rope.write();
    let char_end = rope.len_chars();
    rope.insert(char_end, "\n");
    let new_total = rope.len_bytes() as u32;
    drop(rope);

    store.block_offsets.write().set_total_bytes(new_total);
}

/// Insert `text` at `byte_offset_in_block` inside the block identified
/// by `block_id`. Looks up the block's start in the rope via
/// `block_offsets.range_of()`, splices into the rope, and shifts
/// subsequent block offsets by the inserted byte length.
///
/// Silently no-ops if the block is not registered in the offset index
/// (this can happen for blocks whose content lives outside the global
/// rope, e.g. table cells until step 5.5).
pub fn rope_insert_in_block(
    store: &Store,
    block_id: EntityId,
    byte_offset_in_block: u32,
    text: &str,
) {
    let inserted_bytes = text.len() as u32;
    if inserted_bytes == 0 {
        return;
    }
    let block_byte_start = {
        let offsets = store.block_offsets.read();
        let Some((start, _end)) = offsets.range_of_block(block_id) else {
            return;
        };
        start
    };
    let rope_byte = block_byte_start + byte_offset_in_block;
    {
        let mut rope = store.rope.write();
        let char_idx = rope.byte_to_char(rope_byte as usize);
        rope.insert(char_idx, text);
    }
    // Shift entries past this block by inserted_bytes. Threshold
    // is one byte past block_byte_start so the current block's own
    // entry isn't moved.
    store
        .block_offsets
        .write()
        .shift_after(block_byte_start + 1, inserted_bytes as i32);
}

/// Split an existing block in the rope at `byte_offset_in_block`:
/// - inserts a `\n` inter-block boundary at the absolute byte position
///   `block_start + byte_offset_in_block` in the rope
/// - shifts entries past that position by +1 byte
/// - inserts a new entry for `new_block_id` at
///   `block_start + byte_offset_in_block + 1` (right after the newline),
///   placed immediately after the original block in the entries Vec
///
/// `byte_offset_in_block` may be 0 (split before first char of block,
/// i.e. insert empty block before this one) or equal to the block's
/// byte length (split after last char, i.e. insert empty block after).
pub fn rope_split_block(
    store: &Store,
    current_block_id: EntityId,
    byte_offset_in_block: u32,
    new_block_id: EntityId,
) {
    let current_marker = OffsetMarker::Block(current_block_id);
    let (block_start, current_idx) = {
        let offsets = store.block_offsets.read();
        let Some((start, _end)) = offsets.range_of(current_marker) else {
            return;
        };
        let idx = offsets
            .entries
            .iter()
            .position(|(m, _)| *m == current_marker)
            .unwrap();
        (start, idx)
    };
    let split_byte = block_start + byte_offset_in_block;

    // 1. Insert the `\n` boundary at the split point.
    {
        let mut rope = store.rope.write();
        let char_idx = rope.byte_to_char(split_byte as usize);
        rope.insert(char_idx, "\n");
    }

    // 2. Shift entries past the split (and total_bytes) by +1.
    //    Threshold > split_byte so the new entry we insert next
    //    isn't double-shifted.
    store.block_offsets.write().shift_after(split_byte + 1, 1);

    // 3. Register the new block at `split_byte + 1`, immediately
    //    after the original in the entries Vec.
    store.block_offsets.write().insert_at(
        current_idx + 1,
        OffsetMarker::Block(new_block_id),
        split_byte + 1,
    );
}

/// [`rope_split_block`] on `current_block_id` at `byte_offset_in_block`,
/// then again at the end of each new block in turn, each new block
/// filled with its text by [`rope_insert_in_block`]: the rope and index
/// that loop leaves, built with one rope insert and one index update.
///
/// `blocks` pairs each new block's id with its text, in document order.
/// Whatever followed the split point in `current_block_id` ends up after
/// the last new block, as it does through the loop. No-op if
/// `current_block_id` is not in the index.
///
/// The loop walks every marker in the index once per new block, so
/// pasting or restoring a document of N paragraphs through it cost N
/// walks of N markers.
pub fn rope_split_block_into(
    store: &Store,
    current_block_id: EntityId,
    byte_offset_in_block: u32,
    blocks: &[(EntityId, &str)],
) {
    let current_marker = OffsetMarker::Block(current_block_id);
    let (block_start, current_idx) = {
        let offsets = store.block_offsets.read();
        let (Some((start, _end)), Some(idx)) = (
            offsets.range_of(current_marker),
            offsets.position_of(current_marker),
        ) else {
            return;
        };
        (start, idx)
    };
    rope_insert_marker_run(
        store,
        block_start + byte_offset_in_block,
        current_idx + 1,
        blocks
            .iter()
            .map(|(block_id, text)| (OffsetMarker::Block(*block_id), *text)),
    );
}

/// [`rope_insert_block_at`] for each of `blocks` in turn, the first at
/// `byte_pos` and each next one at the byte right after the block before
/// it: the rope and index that loop leaves, built with one rope insert
/// and one index update. Returns the byte right after the last block,
/// where that loop would insert next.
///
/// `blocks` pairs each new block's id with its text, in document order.
pub fn rope_insert_blocks_at(store: &Store, byte_pos: u32, blocks: &[(EntityId, &str)]) -> u32 {
    // Same placement rule as `rope_insert_block_at`: after every entry at
    // `byte_pos` itself, before every entry strictly past it.
    let vec_pos = {
        let offsets = store.block_offsets.read();
        offsets
            .entries
            .iter()
            .position(|(_, bs)| *bs > byte_pos)
            .unwrap_or(offsets.entries.len())
    };
    rope_insert_marker_run(
        store,
        byte_pos,
        vec_pos,
        blocks
            .iter()
            .map(|(block_id, text)| (OffsetMarker::Block(*block_id), *text)),
    )
}

/// Insert `run` into the rope right after the content of the entry
/// `after`, in order: each marker a `\n` boundary followed by its text (a
/// block's content, or `U+FFFC` for a table's anchor), registered in the
/// offset index right after `after`. Whatever followed `after` follows the
/// run. Returns `false`, leaving the rope alone, when `after` is not in the
/// index.
///
/// This is how a paste lays out what it inserts after the block at the
/// caret: its paragraphs, each table's anchor followed by the table's cells
/// in reading order, and the tail, in the order the frames list them, with
/// one rope insert and one index update for the whole paste.
pub fn rope_insert_run_after(
    store: &Store,
    after: OffsetMarker,
    run: &[(OffsetMarker, &str)],
) -> bool {
    let (content_end, position) = {
        let offsets = store.block_offsets.read();
        let (Some((start, end, has_successor)), Some(position)) = (
            offsets.range_with_successor(after),
            offsets.position_of(after),
        ) else {
            return false;
        };
        let content_end = if has_successor && end > start {
            end - 1
        } else {
            end
        };
        (content_end, position)
    };
    rope_insert_marker_run(store, content_end, position + 1, run.iter().copied());
    true
}

/// Insert `run` into the rope right in front of the entry `before`, in order: whatever
/// preceded `before` precedes the run, and `before` follows it. Returns `false`, leaving the
/// rope alone, when `before` is not in the index.
///
/// This is how a paste opening with a table at the start of a paragraph lays the table out
/// in front of that paragraph rather than after it (see `insert_fragment_uc`).
pub fn rope_insert_run_before(
    store: &Store,
    before: OffsetMarker,
    run: &[(OffsetMarker, &str)],
) -> bool {
    let previous = {
        let offsets = store.block_offsets.read();
        let Some(position) = offsets.position_of(before) else {
            return false;
        };
        position
            .checked_sub(1)
            .and_then(|at| offsets.entries.get(at))
            .map(|(marker, _)| *marker)
    };
    if let Some(previous) = previous {
        return rope_insert_run_after(store, previous, run);
    }
    if run.is_empty() {
        return true;
    }
    // `before` opens the rope: each marker of the run is its text followed by a boundary,
    // and everything already there moves up by the whole run.
    let mut inserted = String::new();
    let mut entries = Vec::with_capacity(run.len());
    for (marker, text) in run {
        entries.push((*marker, inserted.len() as u32));
        inserted.push_str(text);
        inserted.push('\n');
    }
    store.rope.write().insert(0, &inserted);
    let mut offsets = store.block_offsets.write();
    offsets.shift_after(0, inserted.len() as i32);
    offsets.insert_run_at(0, &entries);
    true
}

/// Insert a run of new markers at `byte_pos`, each a `\n` boundary
/// followed by its text, and register them in the offset index at
/// `vec_pos`, `vec_pos + 1`, …. Every entry strictly past `byte_pos`
/// shifts by the whole run. Returns the byte right after the run.
fn rope_insert_marker_run<'a>(
    store: &Store,
    byte_pos: u32,
    vec_pos: usize,
    markers: impl IntoIterator<Item = (OffsetMarker, &'a str)>,
) -> u32 {
    let mut inserted = String::new();
    let mut run = Vec::new();
    for (marker, text) in markers {
        inserted.push('\n');
        run.push((marker, byte_pos + inserted.len() as u32));
        inserted.push_str(text);
    }
    if run.is_empty() {
        return byte_pos;
    }
    {
        let mut rope = store.rope.write();
        let char_idx = rope.byte_to_char(byte_pos as usize);
        rope.insert(char_idx, &inserted);
    }
    let mut offsets = store.block_offsets.write();
    offsets.shift_after(byte_pos + 1, inserted.len() as i32);
    offsets.insert_run_at(vec_pos, &run);
    byte_pos + inserted.len() as u32
}

/// Mirror a table's new, empty cell blocks into the rope where reading order
/// puts them: each after the block before it in the table's row-major order,
/// the first cell's after the table's anchor. `new_blocks` are blocks the
/// table's cells list but the index does not hold yet; the others stay
/// where they are. Consecutive new blocks go in as one run.
///
/// The row, column and cell-split edits used to put their new cells at the
/// end of the table's enclosing top-level frame, which is where they belong
/// only when the table is the last thing in it: after any table followed by
/// text, the new cells sat after that text in the rope, out of flow order,
/// and every position past the table was wrong.
pub fn rope_place_new_cell_blocks(store: &Store, table_id: EntityId, new_blocks: &[EntityId]) {
    if new_blocks.is_empty() {
        return;
    }
    let new: std::collections::HashSet<EntityId> = new_blocks.iter().copied().collect();
    // Everything the table holds, in reading order: its anchor, then its
    // cells by row and column, each cell's blocks and whatever is nested in
    // it in its frame's order.
    let mut ordered: Vec<OffsetMarker> = Vec::new();
    {
        let tables = store.tables.read();
        let cells = store.table_cells.read();
        let frames = store.frames.read();
        table_reading_order(table_id, &tables, &cells, &frames, &mut ordered);
    }
    let mut after = OffsetMarker::TableAnchor(table_id);
    let mut run: Vec<(OffsetMarker, &str)> = Vec::new();
    for marker in ordered {
        if matches!(marker, OffsetMarker::Block(id) if new.contains(&id)) {
            run.push((marker, ""));
            continue;
        }
        if !run.is_empty() {
            rope_insert_run_after(store, after, &run);
            run.clear();
        }
        after = marker;
    }
    if !run.is_empty() {
        rope_insert_run_after(store, after, &run);
    }
}

/// Put `table_id`'s anchor and the blocks of its cells back in reading order in
/// the rope, when an edit of the table's rows left them in another.
///
/// Removing the first row of a cell that spans two rows keeps the cell at its
/// row, one row shorter, and moves the row below up beside it: a moved-up cell
/// of an earlier column now comes before the spanning cell in reading order,
/// while the rope still held the spanning cell's text first. Every position in
/// the table then addressed another cell than the one the frames put there, and
/// the text a caret walked read the cells in another order than the save.
///
/// The table's entries are rewritten where they stand, as one span: same texts,
/// same boundaries, same length, so nothing outside the table moves. Leaves the
/// rope alone when its entries are already in reading order, or are not all in
/// the index and next to each other.
pub fn rope_restore_table_reading_order(store: &Store, table_id: EntityId) {
    let mut ordered: Vec<OffsetMarker> = Vec::new();
    {
        let tables = store.tables.read();
        let cells = store.table_cells.read();
        let frames = store.frames.read();
        table_reading_order(table_id, &tables, &cells, &frames, &mut ordered);
    }
    let mut offsets = store.block_offsets.write();
    let positions: Option<Vec<usize>> = ordered
        .iter()
        .map(|marker| offsets.position_of(*marker))
        .collect();
    let Some(positions) = positions else {
        return;
    };
    if positions.windows(2).all(|pair| pair[0] < pair[1]) {
        return;
    }
    let Some(first) = positions.iter().min().copied() else {
        return;
    };
    let last = first + positions.len() - 1;
    let mut sorted = positions.clone();
    sorted.sort_unstable();
    if sorted
        .iter()
        .enumerate()
        .any(|(i, position)| *position != first + i)
    {
        return;
    }
    let total = offsets.total_bytes();
    let entries = offsets.entries.clone();
    // Where each entry's text ends: at the boundary before the next entry, or at
    // the end of the rope for the last one.
    let text_end = |index: usize| -> u32 {
        entries
            .get(index + 1)
            .map_or(total, |(_, next)| next.saturating_sub(1))
    };
    let span_start = entries[first].1;
    let span_end = text_end(last);
    let mut rope = store.rope.write();
    let char_of = |byte: u32| rope.byte_to_char(byte as usize);
    let text_of = |index: usize| -> String {
        let (start, end) = (entries[index].1, text_end(index));
        rope.slice(char_of(start)..char_of(end)).to_string()
    };
    let mut rebuilt = String::with_capacity((span_end - span_start) as usize);
    let mut run: Vec<(OffsetMarker, u32)> = Vec::with_capacity(positions.len());
    for (marker, position) in ordered.iter().zip(&positions) {
        if !run.is_empty() {
            rebuilt.push('\n');
        }
        run.push((*marker, span_start + rebuilt.len() as u32));
        rebuilt.push_str(&text_of(*position));
    }
    if rebuilt.len() as u32 != span_end - span_start {
        return;
    }
    let (char_start, char_end) = (char_of(span_start), char_of(span_end));
    rope.remove(char_start..char_end);
    rope.insert(char_start, &rebuilt);
    offsets.reorder_run(first, &run);
}

/// Append `table_id`'s anchor and everything in its cells to `out`, in
/// reading order.
fn table_reading_order(
    table_id: EntityId,
    tables: &im::HashMap<EntityId, crate::entities::Table>,
    cells: &im::HashMap<EntityId, crate::entities::TableCell>,
    frames: &im::HashMap<EntityId, crate::entities::Frame>,
    out: &mut Vec<OffsetMarker>,
) {
    out.push(OffsetMarker::TableAnchor(table_id));
    let Some(table) = tables.get(&table_id) else {
        return;
    };
    let mut table_cells: Vec<_> = table.cells.iter().filter_map(|id| cells.get(id)).collect();
    table_cells.sort_by_key(|cell| (cell.row, cell.column));
    for cell in table_cells {
        if let Some(frame) = cell.cell_frame {
            frame_reading_order(frame, tables, cells, frames, out);
        }
    }
}

/// Every block `table_id` holds, nested ones included, in reading order.
pub fn table_block_ids(store: &Store, table_id: EntityId) -> Vec<EntityId> {
    let mut ordered: Vec<OffsetMarker> = Vec::new();
    {
        let tables = store.tables.read();
        let cells = store.table_cells.read();
        let frames = store.frames.read();
        table_reading_order(table_id, &tables, &cells, &frames, &mut ordered);
    }
    ordered
        .into_iter()
        .filter_map(OffsetMarker::as_block)
        .collect()
}

/// Insert `run` into the rope right after everything `table_id` holds: its
/// anchor, its cells and whatever is nested in them, in reading order. See
/// [`rope_insert_run_after`]. Returns `false`, leaving the rope alone, when the
/// table's last entry is not in the index.
pub fn rope_insert_run_after_table(
    store: &Store,
    table_id: EntityId,
    run: &[(OffsetMarker, &str)],
) -> bool {
    let mut ordered: Vec<OffsetMarker> = Vec::new();
    {
        let tables = store.tables.read();
        let cells = store.table_cells.read();
        let frames = store.frames.read();
        table_reading_order(table_id, &tables, &cells, &frames, &mut ordered);
    }
    match ordered.last() {
        Some(last) => rope_insert_run_after(store, *last, run),
        None => false,
    }
}

/// The outermost table whose cells hold the frame `frame_id`, at any depth:
/// the table, its anchor frame, and the frame whose `child_order` lists that
/// anchor frame. `None` when `frame_id` is in no table cell.
///
/// Read from the tables' cell lists and the frames' `child_order`, never from
/// `Frame.parent_frame`: a table pasted into the document has cell frames with
/// no parent, so a walk up the parents took a caret in such a cell for one
/// outside any table.
pub fn outermost_table_around(
    store: &Store,
    frame_id: EntityId,
) -> Option<(EntityId, EntityId, EntityId)> {
    let tables = store.tables.read();
    let cells = store.table_cells.read();
    let frames = store.frames.read();
    let table_of_cell_frame = |frame: EntityId| {
        let (cell_id, _) = cells
            .iter()
            .find(|(_, cell)| cell.cell_frame == Some(frame))?;
        tables
            .iter()
            .find(|(_, table)| table.cells.contains(cell_id))
            .map(|(table_id, _)| *table_id)
    };
    let frame_listing = |frame: EntityId| {
        let entry = -(frame as i64);
        frames
            .iter()
            .find(|(_, candidate)| candidate.child_order.contains(&entry))
            .map(|(id, _)| *id)
    };
    let mut found = None;
    let mut current = frame_id;
    // Each step moves one frame out; a well-formed tree ends at a root in at
    // most as many steps as there are frames.
    for _ in 0..=frames.len() {
        let next = match table_of_cell_frame(current) {
            Some(table_id) => {
                let Some((anchor_id, _)) = frames
                    .iter()
                    .find(|(_, frame)| frame.table == Some(table_id))
                else {
                    break;
                };
                let Some(parent) = frame_listing(*anchor_id) else {
                    break;
                };
                found = Some((table_id, *anchor_id, parent));
                parent
            }
            None => match frame_listing(current) {
                Some(parent) => parent,
                None => break,
            },
        };
        current = next;
    }
    found
}

/// Append a frame's blocks and everything nested in it to `out`, in the
/// frame's `child_order`.
fn frame_reading_order(
    frame_id: EntityId,
    tables: &im::HashMap<EntityId, crate::entities::Table>,
    cells: &im::HashMap<EntityId, crate::entities::TableCell>,
    frames: &im::HashMap<EntityId, crate::entities::Frame>,
    out: &mut Vec<OffsetMarker>,
) {
    let Some(frame) = frames.get(&frame_id) else {
        return;
    };
    for entry in &frame.child_order {
        if *entry > 0 {
            out.push(OffsetMarker::Block(*entry as EntityId));
        } else if *entry < 0 {
            let sub_id = (-*entry) as EntityId;
            match frames.get(&sub_id).and_then(|sub| sub.table) {
                Some(nested) => table_reading_order(nested, tables, cells, frames, out),
                None => frame_reading_order(sub_id, tables, cells, frames, out),
            }
        }
    }
}

/// Merge `start_block` and `end_block` by deleting the rope range
/// `[start_block.start + byte_so .. end_block.start + byte_eo)` — i.e.
/// the suffix of `start_block`, every block between (and their
/// boundary newlines), and the prefix of `end_block`. Removes the
/// index entries for every block strictly between `start_block` and
/// `end_block` (inclusive of `end_block` itself); the surviving
/// content lives in `start_block`. Shifts any blocks past `end_block`
/// by the negative delta.
///
/// No-op if `start_block` is not in the index. Skipped for any
/// intermediate block id whose range is missing from the index (e.g.
/// table cells until step 5.5e).
pub fn rope_merge_block_range(
    store: &Store,
    start_block_id: EntityId,
    byte_so_in_start: u32,
    end_block_id: EntityId,
    byte_eo_in_end: u32,
) {
    let start_marker = OffsetMarker::Block(start_block_id);
    let end_marker = OffsetMarker::Block(end_block_id);
    let (start_block_byte, end_block_byte, start_idx, end_idx) = {
        let offsets = store.block_offsets.read();
        let Some((sb, _)) = offsets.range_of(start_marker) else {
            return;
        };
        let Some((eb, _)) = offsets.range_of(end_marker) else {
            return;
        };
        let si = offsets
            .entries
            .iter()
            .position(|(m, _)| *m == start_marker)
            .unwrap();
        let ei = offsets
            .entries
            .iter()
            .position(|(m, _)| *m == end_marker)
            .unwrap();
        (sb, eb, si, ei)
    };
    if end_idx <= start_idx {
        return;
    }

    let delete_start = start_block_byte + byte_so_in_start;
    let delete_end = end_block_byte + byte_eo_in_end;
    if delete_end <= delete_start {
        return;
    }
    let deleted_bytes = delete_end - delete_start;

    // 1. Remove the rope range.
    {
        let mut rope = store.rope.write();
        let char_start = rope.byte_to_char(delete_start as usize);
        let char_end = rope.byte_to_char(delete_end as usize);
        rope.remove(char_start..char_end);
    }

    // 2. Remove block_offsets entries for [start_idx+1 ..= end_idx].
    {
        let mut offsets = store.block_offsets.write();
        offsets.drain_inclusive(start_idx + 1, end_idx);
    }

    // 3. Shift any remaining entries past the deletion by -deleted_bytes.
    //    Threshold > delete_start because start_block's own entry
    //    sits at delete_start - byte_so_in_start (≤ delete_start)
    //    and must not move.
    store
        .block_offsets
        .write()
        .shift_after(delete_start + 1, -(deleted_bytes as i32));
}

/// Insert a U+FFFC OBJECT REPLACEMENT CHARACTER sentinel in the rope
/// at the table-anchor position, registering a `TableAnchor(table_id)`
/// marker in the offset index (plan §1.6).
///
/// `target_block_id` is the block in the parent frame that the table
/// is adjacent to. `after` controls whether the table goes BEFORE
/// (`after = false`) or AFTER the target block.
///
/// The 3-byte sentinel is paired with an inter-marker `\n`:
/// - `after = false`: inserts `\u{FFFC}\n` at `target.byte_start`
/// - `after = true`, target is NOT the last entry: inserts
///   `\u{FFFC}\n` at `target.byte_end` (between target's trailing
///   `\n` and the next entry)
/// - `after = true`, target IS the last entry: inserts `\n\u{FFFC}`
///   at `target.byte_end` (rope now ends with the sentinel)
///
/// NOTE: cell-internal content is not yet routed through the rope —
/// the rope reflects table *presence* (3-byte sentinel) only.
/// Routing cell content is deferred to plan §1.6's `Frame.byte_range`
/// model.
///
/// No-op if `target_block_id` is not in the index.
pub fn rope_insert_table_anchor(
    store: &Store,
    table_id: EntityId,
    target_block_id: EntityId,
    after: bool,
) {
    const SENTINEL: &str = "\u{FFFC}"; // 3 bytes
    const SENTINEL_BYTES: u32 = 3;

    let (insert_pos, target_idx, target_is_last) = {
        let offsets = store.block_offsets.read();
        let target_marker = OffsetMarker::Block(target_block_id);
        let Some((start, end)) = offsets.range_of(target_marker) else {
            return;
        };
        let idx = offsets
            .entries
            .iter()
            .position(|(m, _)| *m == target_marker)
            .unwrap();
        let is_last = idx + 1 == offsets.entries.len();
        let pos = if after { end } else { start };
        (pos, idx, is_last)
    };

    // Insertion strategy:
    let (rope_inserted, marker_byte_start, new_entry_pos, shift_threshold, shift_delta) = if !after
    {
        // Before target: "\u{FFFC}\n" at target.byte_start
        ("\u{FFFC}\n", insert_pos, target_idx, insert_pos, 4i32)
    } else if !target_is_last {
        // After target, with following entries: "\u{FFFC}\n"
        // at target.byte_end. The TableAnchor sits where the
        // next block USED to start; that following entry
        // shifts by 4.
        ("\u{FFFC}\n", insert_pos, target_idx + 1, insert_pos, 4i32)
    } else {
        // After target which is last: "\n\u{FFFC}" appended.
        // TableAnchor's byte_start sits 1 past the original
        // total (after the new `\n`).
        (
            "\n\u{FFFC}",
            insert_pos + 1,
            target_idx + 1,
            insert_pos,
            4i32,
        )
    };

    // 1. Splice the literal bytes into the rope.
    {
        let mut rope = store.rope.write();
        let char_idx = rope.byte_to_char(insert_pos as usize);
        rope.insert(char_idx, rope_inserted);
    }

    // 2. Shift entries past the insertion point. Use shift_after
    //    BEFORE inserting our new entry so we don't double-shift.
    store
        .block_offsets
        .write()
        .shift_after(shift_threshold, shift_delta);

    // 3. Register the TableAnchor at the resolved byte position
    //    and the resolved Vec position.
    store.block_offsets.write().insert_at(
        new_entry_pos,
        OffsetMarker::TableAnchor(table_id),
        marker_byte_start,
    );

    // Note: SENTINEL_BYTES is part of `shift_delta` (3 for the
    // sentinel + 1 for the `\n`).
    let _ = SENTINEL;
    let _ = SENTINEL_BYTES;
}

/// Append a U+FFFC table-anchor sentinel at the end of the rope and
/// register a `TableAnchor(table_id)` marker.
///
/// Used by import paths (`import_djot_uc`, `import_html_uc`,
/// `import_markdown_uc`) that process the document linearly and append
/// entities as they encounter them, rather than inserting relative to
/// an existing target block.
///
/// # `needs_boundary`
///
/// True when something has already been emitted, so the sentinel needs a
/// `\n` in front of it rather than running into the previous entry.
///
/// It is the **caller's** flag, not `rope.len_bytes() == 0`, and the
/// difference is not academic. Those two answers diverge on exactly one
/// document: one whose first block is *empty* — an empty code fence, say.
/// The rope is then still zero bytes long even though a block has been
/// emitted, so an emptiness check skips the boundary that block is owed.
/// Its `block_offsets` entry and the anchor's then both point at byte 0:
/// two entities claiming one offset, in the offset index every edit
/// resolves through.
///
/// Every other element gets its boundary from the importer's own
/// positional flag (`rope_insert_block_boundary` after the first block).
/// Deriving the same fact a second way, from the rope's byte length, is
/// what let the two disagree.
pub fn rope_append_table_anchor(store: &Store, table_id: EntityId, needs_boundary: bool) {
    let (anchor_byte_start, new_total) = {
        let mut rope = store.rope.write();
        let char_end = rope.len_chars();
        let to_insert = if needs_boundary {
            "\n\u{FFFC}"
        } else {
            "\u{FFFC}"
        };
        rope.insert(char_end, to_insert);
        let new_total = rope.len_bytes() as u32;
        // Sentinel is 3 bytes; if a `\n` was prepended that's 1 byte
        // before the sentinel.
        let anchor_byte_start = new_total - 3;
        (anchor_byte_start, new_total)
    };

    let mut offsets = store.block_offsets.write();
    offsets.push(OffsetMarker::TableAnchor(table_id), anchor_byte_start);
    offsets.set_total_bytes(new_total);
}

/// Remove a TableAnchor sentinel from the rope, undoing the effect
/// of `rope_insert_table_anchor`. Looks up the anchor's byte range
/// (always 3 bytes for the U+FFFC plus 1 byte of inter-marker `\n`
/// either before or after, depending on what's adjacent), removes
/// those 4 bytes from the rope, drops the entry, shifts trailing
/// entries by -4.
///
/// No-op if no TableAnchor for `table_id` exists.
pub fn rope_remove_table_anchor(store: &Store, table_id: EntityId) {
    let anchor_marker = OffsetMarker::TableAnchor(table_id);
    let (anchor_byte_start, anchor_idx, anchor_is_last, has_predecessor) = {
        let offsets = store.block_offsets.read();
        let Some((start, _end)) = offsets.range_of(anchor_marker) else {
            return;
        };
        let idx = offsets
            .entries
            .iter()
            .position(|(m, _)| *m == anchor_marker)
            .unwrap();
        let is_last = idx + 1 == offsets.entries.len();
        let has_pred = idx > 0;
        (start, idx, is_last, has_pred)
    };

    // Symmetric to insert_table_anchor. The bytes to remove are:
    // - if anchor is last: [byte_start - 1 .. byte_start + 3) — the
    //   preceding `\n` + the 3-byte sentinel
    // - if anchor is the only entry: [byte_start .. byte_start + 3),
    //   the sentinel alone, with no boundary on either side
    // - otherwise: [byte_start .. byte_start + 4) — the sentinel
    //   + the following `\n`
    let (remove_start, remove_end) = match (anchor_is_last, has_predecessor) {
        (true, true) => (anchor_byte_start - 1, anchor_byte_start + 3),
        (true, false) => (anchor_byte_start, anchor_byte_start + 3),
        (false, _) => (anchor_byte_start, anchor_byte_start + 4),
    };

    {
        let mut rope = store.rope.write();
        let char_start = rope.byte_to_char(remove_start as usize);
        let char_end = rope.byte_to_char(remove_end as usize);
        rope.remove(char_start..char_end);
    }
    {
        let mut offsets = store.block_offsets.write();
        offsets.remove_at(anchor_idx);
    }
    // Shift the entries past the removed range only, as `rope_remove_block`
    // does. An empty entry right before a last anchor starts where the cut
    // starts, at the boundary it loses; shifting from there moved it back
    // into the entry before it, or below zero.
    store
        .block_offsets
        .write()
        .shift_after(remove_end, -((remove_end - remove_start) as i32));
}

/// Remove every one of `markers` (blocks and table anchors alike) from the
/// rope, each with one boundary `\n`, and drop their entries from the index:
/// the rope and index that [`rope_remove_block`] and
/// [`rope_remove_table_anchor`] leave, called for each marker, with one walk
/// of the index for all of them. What is left is the kept entries' contents
/// joined by boundaries, in their order. Markers not in the index are
/// skipped.
///
/// A deletion that removes entities has to take their text out of the rope
/// too, or the rope keeps text no block owns: the offset index then names
/// blocks that are gone, every position after them is off by their length,
/// and search and the addressable text still find the deleted words.
pub fn rope_remove_markers(store: &Store, markers: &[OffsetMarker]) {
    let mut offsets = store.block_offsets.write();
    let count = offsets.len();
    let mut dropped = vec![false; count];
    let mut any = false;
    for marker in markers {
        if let Some(position) = offsets.position_of(*marker) {
            dropped[position] = true;
            any = true;
        }
    }
    if !any {
        return;
    }
    let total = offsets.total_bytes();
    // One cut per run of consecutive dropped entries: their contents and the
    // boundary after each when a kept entry follows the run, or else the
    // boundary before the run and everything to the end of the rope.
    let mut cuts: Vec<(u32, u32)> = Vec::new();
    let mut position = 0;
    while position < count {
        if !dropped[position] {
            position += 1;
            continue;
        }
        let run_start = position;
        while position < count && dropped[position] {
            position += 1;
        }
        let first_byte = offsets.entries[run_start].1;
        let cut = if position < count {
            (first_byte, offsets.entries[position].1)
        } else if run_start > 0 {
            (first_byte.saturating_sub(1), total)
        } else {
            (0, total)
        };
        if cut.1 > cut.0 {
            cuts.push(cut);
        }
    }
    {
        // Last cut first, so the byte offsets of the others still hold.
        let mut rope = store.rope.write();
        for &(start, end) in cuts.iter().rev() {
            let char_start = rope.byte_to_char(start as usize);
            let char_end = rope.byte_to_char(end as usize);
            rope.remove(char_start..char_end);
        }
    }
    offsets.remove_entries(&dropped, &cuts);
}

/// What an edit changed between two states of the rope, as positions: the
/// span of `before` from the first returned position to the second was
/// replaced by the span of `after` from the first to the third.
///
/// `from` is where the edit was asked to go in, and the span starts there or
/// after it when the text in front of `from` is unchanged: the ends are
/// matched first, so text that repeats the text in front of it counts as
/// going in at the earliest place from `from` on, then the starts. An
/// insertion goes in where it is asked, with two exceptions this finds: a
/// table pasted into a paragraph goes in after the paragraph, so what of the
/// paragraph followed the caret stays in front of it; and a table pasted into
/// a table cell fills the cells from the caret's own, replacing what they held
/// from the start of the caret's cell, which can lie before `from`. Counting
/// either as text put in at the caret moved every cursor standing in the rest
/// of that paragraph, or in those cells, to the wrong place.
pub fn changed_span(
    before: &ropey::Rope,
    after: &ropey::Rope,
    from: usize,
) -> (usize, usize, usize) {
    let (old_len, new_len) = (before.len_chars(), after.len_chars());
    let shorter = old_len.min(new_len);
    let from = from.min(shorter);
    // The common start, as far as `from`.
    let mut prefix = 0;
    {
        let mut old_chars = before.chars();
        let mut new_chars = after.chars();
        while prefix < from {
            match (old_chars.next(), new_chars.next()) {
                (Some(old), Some(new)) if old == new => prefix += 1,
                _ => break,
            }
        }
    }
    // The common end, reaching back no further than the common start.
    let mut suffix = 0;
    {
        let reach = shorter - prefix;
        let mut old_chars = before.chars_at(old_len);
        let mut new_chars = after.chars_at(new_len);
        while suffix < reach {
            match (old_chars.prev(), new_chars.prev()) {
                (Some(old), Some(new)) if old == new => suffix += 1,
                _ => break,
            }
        }
    }
    let (old_end, new_end) = (old_len - suffix, new_len - suffix);
    // When nothing in front of `from` changed, the rest of the common start,
    // from `from` on, up to what the end left.
    if prefix == from {
        let mut old_chars = before.chars_at(from);
        let mut new_chars = after.chars_at(from);
        while prefix < old_end.min(new_end) {
            match (old_chars.next(), new_chars.next()) {
                (Some(old), Some(new)) if old == new => prefix += 1,
                _ => break,
            }
        }
    }
    (prefix, old_end, new_end)
}

/// Where a position belongs when it falls on a table's anchor: the `U+FFFC`
/// a table occupies in the rope, or the boundary after it. Those two
/// characters stand for the table as a whole and belong to no block.
/// `forward`, the position moves to the start of the table's first cell:
/// where a caret on the table types, and where a range starting on it
/// starts. Otherwise it moves back to the end of the entry before the table,
/// where a range ending on it ends. Any other position, and every position of
/// a document whose rope is not its position space, is returned as is.
///
/// Resolved as a block position, the anchor matched no block: the edit fell
/// back to the document's last block, so text typed or pasted there landed at
/// the end of the document and a deletion ending there ran to it.
///
/// Tables can follow each other with nothing between them in the rope: a table
/// nested at the start of a cell's frame has its anchor right after the anchor
/// of the table holding that cell. The walk goes past every anchor of such a
/// chain, so the result is never an anchor position itself: forward, it is the
/// first block after the chain (the innermost table's first cell); backward, it
/// is the end of the entry before the chain, or the document's start when the
/// chain opens it.
pub fn snap_off_table_anchor(store: &Store, position: i64, forward: bool) -> i64 {
    if !rope_positions_match_flow(store) {
        return position;
    }
    let rope = store.rope.read();
    let clamped = position.clamp(0, rope.len_chars() as i64);
    let byte = rope.char_to_byte(clamped as usize) as u32;
    let offsets = store.block_offsets.read();
    let Some(marker @ OffsetMarker::TableAnchor(_)) = offsets.marker_at_byte(byte) else {
        return position;
    };
    let Some(index) = offsets.position_of(marker) else {
        return position;
    };
    let is_anchor = |i: usize| {
        offsets
            .entries
            .get(i)
            .is_some_and(|(entry, _)| !entry.is_block())
    };
    let target = if forward {
        let mut next = index + 1;
        while is_anchor(next) {
            next += 1;
        }
        offsets
            .entries
            .get(next)
            .map_or_else(|| offsets.total_bytes(), |(_, start)| *start)
    } else {
        let mut first = index;
        while first > 0 && is_anchor(first - 1) {
            first -= 1;
        }
        // The boundary in front of the chain's first anchor closes the entry
        // before it: a caret there stands at that entry's end.
        offsets
            .entries
            .get(first)
            .map_or(0, |(_, start)| start.saturating_sub(1))
    };
    rope.byte_to_char(target as usize) as i64
}

/// Where `table_id`'s anchor stands in the rope's position space, when the
/// rope is the document's position space and holds the anchor.
pub fn table_anchor_position(store: &Store, table_id: EntityId) -> Option<i64> {
    if !rope_positions_match_flow(store) {
        return None;
    }
    let (byte, _) = store
        .block_offsets
        .read()
        .range_of(OffsetMarker::TableAnchor(table_id))?;
    Some(store.rope.read().byte_to_char(byte as usize) as i64)
}

/// Whether a table's anchor lies in the positions `[start, end)`: whether a
/// range over them covers the start of a table. `false` for a document whose
/// rope is not its position space.
pub fn range_covers_table_anchor(store: &Store, start: i64, end: i64) -> bool {
    if start >= end || !rope_positions_match_flow(store) {
        return false;
    }
    let rope = store.rope.read();
    let total = rope.len_chars() as i64;
    let byte_of = |position: i64| rope.char_to_byte(position.clamp(0, total) as usize) as u32;
    let (from, to) = (byte_of(start), byte_of(end));
    let offsets = store.block_offsets.read();
    // The entries are in byte order: only those starting inside the range are
    // looked at, which for a Backspace or a Delete is one or two.
    let first = offsets.entries.partition_point(|(_, byte)| *byte < from);
    offsets.entries[first..]
        .iter()
        .take_while(|(_, byte)| *byte < to)
        .any(|(marker, _)| !marker.is_block())
}

/// Whether the range from `requested_start` (on or before a table's anchor, which the
/// range's `start` was moved past, see [`snap_off_table_anchor`]) to `end` holds a whole
/// table: the anchor and every block of its cells.
pub fn holds_a_table_from_its_anchor(
    store: &Store,
    requested_start: i64,
    start: i64,
    end: i64,
) -> bool {
    if requested_start >= start {
        return false;
    }
    let table_ids: Vec<EntityId> = store.tables.read().keys().copied().collect();
    table_ids.into_iter().any(|table_id| {
        let Some(anchor) = table_anchor_position(store, table_id) else {
            return false;
        };
        if anchor < requested_start || anchor >= start {
            return false;
        }
        let table_end = {
            let offsets = store.block_offsets.read();
            let rope = store.rope.read();
            table_block_ids(store, table_id)
                .iter()
                .filter_map(|block_id| offsets.range_with_successor(OffsetMarker::Block(*block_id)))
                .map(|(block_start, block_end, has_successor)| {
                    let text_end = if has_successor && block_end > block_start {
                        block_end - 1
                    } else {
                        block_end
                    };
                    rope.byte_to_char(text_end as usize) as i64
                })
                .max()
        };
        table_end.is_some_and(|table_end| end >= table_end)
    })
}

/// Whether a range over the positions `[start, end)` holds a whole table, as a deletion of
/// it takes one: with its ends moved off any table's anchor (see [`snap_off_table_anchor`]),
/// it covers a table's anchor, or it starts on a table's anchor and reaches the end of that
/// table's last cell.
///
/// A copy reads a range as the deletion does. It took a table whole whenever the range held
/// the table's anchor, where the deletion takes it only then: a selection from a table's
/// anchor into its first cell, which the Right arrow and Shift+Right from the paragraph
/// before the table make, copied the whole table and cut the characters selected, and the
/// paste of the cut put the table in a second time.
pub fn range_holds_a_whole_table(store: &Store, start: i64, end: i64) -> bool {
    let snapped_start = snap_off_table_anchor(store, start, true);
    let snapped_end = snap_off_table_anchor(store, end, false);
    range_covers_table_anchor(store, snapped_start, snapped_end)
        || holds_a_table_from_its_anchor(store, start, snapped_start, snapped_end)
}

/// Where a table lies in the rope's position space: the position of its anchor, and where
/// the text of the last entry it holds ends (its last cell's last paragraph, or a table
/// nested there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableExtent {
    pub table_id: EntityId,
    pub anchor: i64,
    pub end: i64,
}

impl TableExtent {
    /// Whether a range starting at `start` starts in the table: in one of its cells, or on
    /// the boundary right after its anchor, which stands for its first cell. A start on the
    /// anchor itself is the table's start (see [`holds_a_table_from_its_anchor`]).
    fn holds_start(&self, start: i64) -> bool {
        self.anchor < start && start <= self.end
    }

    /// Whether a range ending at `end` ends in the table: in one of its cells. An end on the
    /// anchor, or on the boundary after it, ends before the table (see
    /// [`snap_off_table_anchor`]).
    fn holds_end(&self, store: &Store, end: i64) -> bool {
        let end = snap_off_table_anchor(store, end, false);
        self.anchor < end && end <= self.end
    }
}

/// Every table's [`TableExtent`], in reading order, when the rope is the document's position
/// space. Empty otherwise, and for a document holding no table.
pub fn table_extents(store: &Store) -> Vec<TableExtent> {
    if !rope_positions_match_flow(store) {
        return Vec::new();
    }
    let table_ids: Vec<EntityId> = store.tables.read().keys().copied().collect();
    let mut extents: Vec<TableExtent> = table_ids
        .into_iter()
        .filter_map(|table_id| extent_of(store, table_id))
        .collect();
    // In reading order, whatever order the store keeps the tables in.
    extents.sort_unstable_by_key(|extent| extent.anchor);
    extents
}

/// The [`TableExtent`] of `table_id`, when the rope is the document's position space and
/// holds its anchor.
pub fn table_extent(store: &Store, table_id: EntityId) -> Option<TableExtent> {
    if !rope_positions_match_flow(store) {
        return None;
    }
    extent_of(store, table_id)
}

fn extent_of(store: &Store, table_id: EntityId) -> Option<TableExtent> {
    let blocks = table_block_ids(store, table_id);
    let offsets = store.block_offsets.read();
    let anchor_index = offsets.position_of(OffsetMarker::TableAnchor(table_id))?;
    let last_index = blocks
        .iter()
        .filter_map(|block_id| offsets.position_of(OffsetMarker::Block(*block_id)))
        .max()
        .unwrap_or(anchor_index)
        .max(anchor_index);
    let rope = store.rope.read();
    let char_of = |byte: u32| rope.byte_to_char(byte as usize) as i64;
    let (_, anchor_byte) = offsets.entries.get(anchor_index)?;
    // The text of the last entry ends at the boundary before whatever follows it, or at the
    // end of the rope.
    let end = offsets
        .entries
        .get(last_index + 1)
        .map_or(rope.len_chars() as i64, |(_, next)| {
            char_of(next.saturating_sub(1))
        });
    Some(TableExtent {
        table_id,
        anchor: char_of(*anchor_byte),
        end,
    })
}

/// The range `[start, end)` widened to hold whole every table it has one end in and the
/// other end outside of: a start in a table's cells with an end past the table moves back to
/// the table's anchor, and an end in a table's cells with a start before the table moves on
/// to the end of the table's last cell. The same is done for the tables around those, so a
/// range from a table nested in a cell to past the outer table holds the outer one whole.
///
/// This is the one rule a copy and a removal share for a range crossing a table's edge, as
/// a range holding a table's anchor already is (see [`range_holds_a_whole_table`]): the
/// whole table goes with the text beside it, as Word and LibreOffice take a table a
/// selection runs out of. A copy of such a range held the whole table while the removal
/// emptied the cells it met and kept the grid, so a cut pasted back put the table in twice;
/// and a removal from a table's last cell to the end of the text kept the paragraph after
/// the table. A range with both ends in one table, or both outside every table, is left as
/// it is.
pub fn take_whole_tables(store: &Store, start: i64, end: i64) -> (i64, i64) {
    if start >= end || store.tables.read().is_empty() {
        return (start, end);
    }
    let extents = table_extents(store);
    let (mut start, mut end) = (start, end);
    // Each widening moves an end out to a table's edge, never back: at most one pass per
    // table, and one more to see nothing moves.
    for _ in 0..=extents.len() {
        let mut moved = false;
        for extent in &extents {
            if extent.holds_start(start) && end > extent.end {
                start = extent.anchor;
                moved = true;
            }
            if start < extent.anchor && extent.holds_end(store, end) && end < extent.end {
                end = extent.end;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    (start, end)
}

/// Remove a registered block from the rope: drops its content bytes
/// plus one boundary `\n` (the one after, if the block has a
/// successor; the one before, if it's the last entry), removes the
/// entry from the index, and shifts trailing entries by the negative
/// byte delta.
///
/// No-op if `block_id` is not in the index. No-op for the special
/// case of a single-block document being asked to remove its sole
/// block (we'd produce an empty rope but the block itself is being
/// cascaded by the caller).
pub fn rope_remove_block(store: &Store, block_id: EntityId) {
    let block_marker = OffsetMarker::Block(block_id);
    let (block_start, block_end, idx, is_last, has_pred) = {
        let offsets = store.block_offsets.read();
        let Some((start, end)) = offsets.range_of(block_marker) else {
            return;
        };
        let Some(idx) = offsets.position_of(block_marker) else {
            return;
        };
        let is_last = idx + 1 == offsets.entries.len();
        let has_pred = idx > 0;
        (start, end, idx, is_last, has_pred)
    };

    // Determine the byte range to delete:
    // - if there's a successor: [block_start..block_end) — the
    //   block's content INCLUDING its trailing boundary `\n`
    //   (which is the byte at block_end - 1)
    // - if last and has predecessor: [block_start - 1..block_end)
    //   — also delete the LEADING boundary `\n` that the previous
    //   entry placed before us
    // - if last and no predecessor (sole entry): just delete
    //   [block_start..block_end) (no boundary `\n` exists)
    let (remove_start, remove_end) = if is_last && has_pred {
        (block_start.saturating_sub(1), block_end)
    } else {
        (block_start, block_end)
    };
    if remove_end <= remove_start {
        // Drop the entry only; nothing to remove from the rope.
        store.block_offsets.write().remove_at(idx);
        return;
    }
    let deleted_bytes = remove_end - remove_start;

    {
        let mut rope = store.rope.write();
        let char_start = rope.byte_to_char(remove_start as usize);
        let char_end = rope.byte_to_char(remove_end as usize);
        rope.remove(char_start..char_end);
    }
    store.block_offsets.write().remove_at(idx);
    // Shift entries STRICTLY PAST the removed range. Using `remove_end` as
    // the threshold (rather than `remove_start`) keeps an empty predecessor
    // whose byte_start equals `remove_start` (the leading boundary `\n` we
    // just deleted) in place — its content position is unchanged, only its
    // trailing boundary is gone. `total_bytes` decreases by `deleted_bytes`
    // regardless of threshold.
    store
        .block_offsets
        .write()
        .shift_after(remove_end, -(deleted_bytes as i32));
}

/// Replace the entire content of a registered block in the rope with
/// `new_text`. Preserves the block's `byte_start` and its trailing
/// boundary `\n` (if any); subsequent entries shift by the net
/// length delta.
///
/// Used by use cases that compute a block's final content as a string
/// and want to push that content to the rope in one shot — e.g. the
/// block-splitting branches of `insert_html_at_position_uc` and
/// `insert_markdown_at_position_uc`, where each affected block
/// (head, tail, mid-replacement) gets a single new value.
///
/// No-op if `block_id` is not in the index.
pub fn rope_replace_block_content(store: &Store, block_id: EntityId, new_text: &str) {
    let (block_byte_start, content_bytes) = {
        let offsets = store.block_offsets.read();
        let Some((start, end, has_successor)) =
            offsets.range_with_successor(OffsetMarker::Block(block_id))
        else {
            return;
        };
        // `range_of` extends to the next entry's `byte_start` (or to
        // `total_bytes`). If there's a following entry, the byte at
        // `end - 1` is the inter-block boundary `\n` that belongs to
        // the boundary between this block and the next, not to this
        // block's content.
        //
        // Whether there is one is the index's to say. Asking whether the
        // range stops short of the rope's end got it wrong before an empty
        // last block, which starts at the rope's end: the boundary before it
        // was taken for content and replaced, and the two blocks were left
        // at one offset.
        let content_bytes = if has_successor && end > start {
            end - start - 1
        } else {
            end - start
        };
        (start, content_bytes)
    };

    let new_bytes = new_text.len() as u32;
    if content_bytes == 0 && new_bytes == 0 {
        return;
    }
    let delta = new_bytes as i32 - content_bytes as i32;

    // Splice [block_byte_start..block_byte_start + content_bytes)
    // with `new_text`.
    {
        let mut rope = store.rope.write();
        let char_start = rope.byte_to_char(block_byte_start as usize);
        if content_bytes > 0 {
            let char_end = rope.byte_to_char((block_byte_start + content_bytes) as usize);
            rope.remove(char_start..char_end);
        }
        if new_bytes > 0 {
            rope.insert(char_start, new_text);
        }
    }

    if delta != 0 {
        // Shift entries that sit strictly past this block's start
        // (i.e. the trailing boundary and everything after).
        store
            .block_offsets
            .write()
            .shift_after(block_byte_start + 1, delta);
    }
}

/// [`rope_replace_block_content`] with an empty text for each of
/// `block_ids` in turn: the rope and index that loop leaves, with one walk
/// of the index instead of one per block.
///
/// Each one-block clear shifts every entry after the block, so emptying the
/// cells of many tables one block at a time walked the index once per cell.
/// When the blocks come in index order, as a deletion meets them, each
/// block's content is exactly what it holds before any of them is cleared:
/// clearing an earlier block moves this one and the entry after it back by
/// the same amount. Out of that order the blocks are cleared one at a time,
/// as that loop does.
pub fn rope_clear_blocks(store: &Store, block_ids: &[EntityId]) {
    // `(index position, byte start, content bytes)` of each block with content.
    let mut cuts: Vec<(usize, u32, u32)> = Vec::with_capacity(block_ids.len());
    {
        let offsets = store.block_offsets.read();
        let mut previous: Option<usize> = None;
        let mut in_index_order = true;
        for &block_id in block_ids {
            let marker = OffsetMarker::Block(block_id);
            let (Some((start, end, has_successor)), Some(position)) = (
                offsets.range_with_successor(marker),
                offsets.position_of(marker),
            ) else {
                // Not in the index: the one-block clear leaves it alone too.
                continue;
            };
            if previous.is_some_and(|before| position <= before) {
                in_index_order = false;
                break;
            }
            previous = Some(position);
            // What `rope_replace_block_content` counts as the block's content.
            let content = if has_successor && end > start {
                end - start - 1
            } else {
                end - start
            };
            if content > 0 {
                cuts.push((position, start, content));
            }
        }
        if !in_index_order {
            drop(offsets);
            for &block_id in block_ids {
                rope_replace_block_content(store, block_id, "");
            }
            return;
        }
    }
    if cuts.is_empty() {
        return;
    }
    {
        // Last cut first, so the byte offsets of the others still hold.
        let mut rope = store.rope.write();
        for &(_, start, content) in cuts.iter().rev() {
            let char_start = rope.byte_to_char(start as usize);
            let char_end = rope.byte_to_char((start + content) as usize);
            rope.remove(char_start..char_end);
        }
    }
    let shifts: Vec<(usize, u32)> = cuts
        .iter()
        .map(|&(position, _, content)| (position, content))
        .collect();
    store.block_offsets.write().remove_bytes_after(&shifts);
}

/// Delete bytes `[byte_start_in_block..byte_end_in_block)` from inside
/// the block identified by `block_id`. Shifts subsequent block offsets
/// by the deleted byte length. No-op for blocks not in the index.
pub fn rope_delete_in_block(
    store: &Store,
    block_id: EntityId,
    byte_start_in_block: u32,
    byte_end_in_block: u32,
) {
    if byte_end_in_block <= byte_start_in_block {
        return;
    }
    let deleted_bytes = byte_end_in_block - byte_start_in_block;
    let block_byte_start = {
        let offsets = store.block_offsets.read();
        let Some((start, _end)) = offsets.range_of_block(block_id) else {
            return;
        };
        start
    };
    let rope_byte_start = block_byte_start + byte_start_in_block;
    let rope_byte_end = block_byte_start + byte_end_in_block;
    {
        let mut rope = store.rope.write();
        let char_start = rope.byte_to_char(rope_byte_start as usize);
        let char_end = rope.byte_to_char(rope_byte_end as usize);
        rope.remove(char_start..char_end);
    }
    store
        .block_offsets
        .write()
        .shift_after(block_byte_start + 1, -(deleted_bytes as i32));
}

/// Recompute `Frame.byte_range` for every frame in `store.frames`
/// based on current `block_offsets` and the frame tree structure.
/// Plan §1.6 invariant: each frame's byte_range is the (min_start,
/// max_end) over all its descendant blocks, sub-frames, and table
/// anchors+cells.
///
/// Call this after any mutation that affects rope byte positions.
/// O(F + B) where F = frames, B = blocks in the document.
pub fn recompute_all_frame_byte_ranges(store: &Store) {
    let frame_ids: Vec<EntityId> = {
        let frames = store.frames.read();
        frames.keys().copied().collect()
    };
    for fid in frame_ids {
        let new_range = compute_frame_byte_range_recursive(store, fid);
        let mut frames = store.frames.write();
        if let Some(f) = frames.get(&fid).cloned()
            && f.byte_range != new_range
        {
            let mut updated = f;
            updated.byte_range = new_range;
            frames.insert(fid, updated);
        }
    }
}

fn compute_frame_byte_range_recursive(store: &Store, frame_id: EntityId) -> (u32, u32) {
    let mut bounds: Option<(u32, u32)> = None;
    walk_frame_bounds(store, frame_id, &mut bounds);
    bounds.unwrap_or((0, 0))
}

fn walk_frame_bounds(store: &Store, frame_id: EntityId, bounds: &mut Option<(u32, u32)>) {
    fn merge(bounds: &mut Option<(u32, u32)>, s: u32, e: u32) {
        *bounds = Some(match *bounds {
            None => (s, e),
            Some((min, max)) => (min.min(s), max.max(e)),
        });
    }

    let (blocks, child_order, table_id) = {
        let frames = store.frames.read();
        let Some(f) = frames.get(&frame_id) else {
            return;
        };
        (f.blocks.clone(), f.child_order.clone(), f.table)
    };

    {
        let offsets = store.block_offsets.read();
        for bid in &blocks {
            if let Some((s, e)) = offsets.range_of_block(*bid) {
                merge(bounds, s, e);
            }
        }
        if let Some(tid) = table_id
            && let Some((s, e)) = offsets.range_of(OffsetMarker::TableAnchor(tid))
        {
            merge(bounds, s, e);
        }
    }

    for entry in &child_order {
        if *entry < 0 {
            walk_frame_bounds(store, (-*entry) as EntityId, bounds);
        }
    }

    if let Some(tid) = table_id {
        let cell_ids: Vec<EntityId> = {
            let tables = store.tables.read();
            tables
                .get(&tid)
                .map(|t| t.cells.clone())
                .unwrap_or_default()
        };
        for cell_id in &cell_ids {
            let cell_frame_id = {
                let cells = store.table_cells.read();
                cells.get(cell_id).and_then(|c| c.cell_frame)
            };
            if let Some(cfid) = cell_frame_id {
                walk_frame_bounds(store, cfid, bounds);
            }
        }
    }
}

/// Replace `[char_start..char_end)` inside `block` with `replacement`, choosing what the
/// replacement wears where it overwrites formatted text — see [`ReplaceFormatPolicy`].
/// Mutates the block's format runs, image anchors, footnote references and the global rope
/// consistently in one step, and returns the updated `Block` (with a bumped `updated_at`) for
/// the caller to persist via its own unit of work.
///
/// The single shared implementation of "replace a char range inside one block" — originally
/// written for the project-wide replace path (`document_search::replace_core::apply_in_block`)
/// and moved here so `document_editing`'s interactive selection-replace path can offer the
/// same format-policy choice instead of being permanently pinned to
/// [`ReplaceFormatPolicy::InheritPreceding`]. See `ReplaceFormatPolicy`'s own doc comment for
/// the failure mode a second, independently-drifting copy of this would risk: a replace used
/// to be an unannounced delete + insert, which silently dropped formatting. It is
/// [`replace_in_blocks`] with one replacement, so the two cannot drift either.
///
/// Deliberately takes no unit of work — everything here is store-level (the block's text
/// lives in the rope, its formatting in `format_runs`, its images in `block_images`), so the
/// caller's UoW only has to persist the returned `Block`.
///
/// Returns `Err` rather than corrupting the block if the replace would violate the format-run
/// invariants (see [`FormatRunError`]) — the caller should refuse the edit and propagate this
/// rather than swallow it.
pub fn replace_in_block(
    store: &Store,
    block: &Block,
    char_start: i64,
    char_end: i64,
    replacement: &str,
    policy: ReplaceFormatPolicy,
) -> Result<Block, FormatRunError> {
    replace_in_blocks(
        store,
        &[BlockReplacement {
            block,
            char_start,
            char_end,
            replacement,
        }],
        policy,
    )?;
    let mut updated = block.clone();
    updated.updated_at = chrono::Utc::now();
    Ok(updated)
}

/// One replacement [`replace_in_blocks`] makes: `[char_start..char_end)` of `block`'s text, in
/// the block's own positions (an image or a footnote reference counts one), replaced by
/// `replacement`.
#[derive(Debug, Clone, Copy)]
pub struct BlockReplacement<'a> {
    pub block: &'a Block,
    pub char_start: i64,
    pub char_end: i64,
    pub replacement: &'a str,
}

/// Make every replacement of `edits` in one pass over the document: each block's format runs,
/// image anchors and footnote references, the rope, and one walk of the offset index for all
/// of them. The replacements of one block must not overlap; they may come in any order.
///
/// Replace All used to make its replacements one at a time, and each moved every index entry
/// after it: one match per paragraph cost a walk of the index per match, so replacing a word
/// throughout a long text grew with the square of its length and froze the editor for
/// seconds. The rope takes each splice in logarithmic time, from the last back, so the
/// offsets of the others still hold; the index then moves each entry once, by what the
/// replacements before it added or took.
///
/// Nothing is written until every block's new state is known to be well formed: an `Err`
/// leaves the store as it was.
pub fn replace_in_blocks(
    store: &Store,
    edits: &[BlockReplacement<'_>],
    policy: ReplaceFormatPolicy,
) -> Result<(), FormatRunError> {
    // The replacements of each block, blocks in the order they first appear.
    let mut order: Vec<&Block> = Vec::new();
    let mut of_block: HashMap<EntityId, Vec<&BlockReplacement<'_>>> = HashMap::new();
    for edit in edits {
        let group = of_block.entry(edit.block.id).or_default();
        if group.is_empty() {
            order.push(edit.block);
        }
        group.push(edit);
    }

    struct NewState<'a> {
        block_id: EntityId,
        runs: Vec<FormatRun>,
        images: Vec<ImageAnchor>,
        notes: Option<Vec<FootnoteRefAnchor>>,
        /// `(byte_start, byte_end, replacement)` in the block's text, ascending.
        splices: Vec<(u32, u32, &'a str)>,
    }
    let mut states: Vec<NewState<'_>> = Vec::with_capacity(order.len());
    for block in order {
        let group = of_block.remove(&block.id).unwrap_or_default();
        let images_before = store
            .block_images
            .read()
            .get(&block.id)
            .cloned()
            .unwrap_or_default();
        let block_text = block_content_via_store(block, store);
        let mut splices: Vec<(u32, u32, &str)> = Vec::with_capacity(group.len());
        for edit in group {
            let byte_start = logical_offset_to_byte(&block_text, &images_before, edit.char_start);
            let byte_end = logical_offset_to_byte(&block_text, &images_before, edit.char_end);
            if byte_end < byte_start {
                // A range whose end comes before its start names no text to replace;
                // measuring it overflowed.
                return Err(FormatRunError::ReversedRange {
                    start: byte_start,
                    end: byte_end,
                });
            }
            splices.push((byte_start, byte_end, edit.replacement));
        }
        splices.sort_by_key(|(start, end, _)| (*start, *end));
        if let Some(pair) = splices.windows(2).find(|pair| pair[0].1 > pair[1].0) {
            // Two replacements of the same text: the second would splice bytes the first
            // already changed.
            return Err(FormatRunError::ReversedRange {
                start: pair[0].1,
                end: pair[1].0,
            });
        }

        let mut runs = store
            .format_runs
            .read()
            .get(&block.id)
            .cloned()
            .unwrap_or_default();
        let mut images = images_before;
        let mut notes = store.block_footnote_refs.read().get(&block.id).cloned();
        let mut new_len = block_text.len();
        // From the last back, so each splice's offsets are still the text's own.
        for &(byte_start, byte_end, replacement) in splices.iter().rev() {
            let inserted = replacement.len() as u32;
            // Format runs under an explicit policy, and then CHECK the result rather than
            // assert it: `debug_assert_well_formed` is compiled out of release, so a
            // malformed run list produced in a shipped build went entirely undetected, and
            // autosave wrote it to the writer's file seconds later. A replace that would
            // corrupt a block's formatting fails loudly.
            shift_runs_for_replace(&mut runs, byte_start, byte_end, inserted, policy)?;
            shift_images_for_delete(&mut images, byte_start, byte_end);
            shift_images_for_insert(&mut images, byte_start, inserted);
            if let Some(notes) = notes.as_mut() {
                shift_footnote_refs_for_delete(notes, byte_start, byte_end);
                shift_footnote_refs_for_insert(notes, byte_start, inserted);
            }
            new_len = new_len - (byte_end - byte_start) as usize + replacement.len();
        }
        check_well_formed(&runs, new_len)?;
        states.push(NewState {
            block_id: block.id,
            runs,
            images,
            notes,
            splices,
        });
    }

    // Every block's new state is sound: write it.
    let mut rope_splices: Vec<(u32, u32, &str)> = Vec::new();
    let mut shifts: Vec<(u32, i32)> = Vec::new();
    {
        let offsets = store.block_offsets.read();
        let mut runs_map = store.format_runs.write();
        let mut images_map = store.block_images.write();
        let mut notes_map = store.block_footnote_refs.write();
        for state in states {
            runs_map.insert(state.block_id, state.runs);
            images_map.insert(state.block_id, state.images);
            if let Some(notes) = state.notes {
                notes_map.insert(state.block_id, notes);
            }
            // A block the index does not hold has no text in the rope to change.
            let Some((block_start, _)) = offsets.range_of_block(state.block_id) else {
                continue;
            };
            let mut delta: i64 = 0;
            for (byte_start, byte_end, replacement) in state.splices {
                rope_splices.push((
                    block_start + byte_start,
                    block_start + byte_end,
                    replacement,
                ));
                delta += replacement.len() as i64 - (byte_end - byte_start) as i64;
            }
            // One byte past the block's start, so the block's own entry stays where it is.
            if delta != 0 {
                shifts.push((block_start + 1, delta as i32));
            }
        }
    }
    rope_splices.sort_by_key(|(start, end, _)| (*start, *end));
    {
        let mut rope = store.rope.write();
        for &(byte_start, byte_end, replacement) in rope_splices.iter().rev() {
            let char_start = rope.byte_to_char(byte_start as usize);
            if byte_end > byte_start {
                let char_end = rope.byte_to_char(byte_end as usize);
                rope.remove(char_start..char_end);
            }
            if !replacement.is_empty() {
                rope.insert(char_start, replacement);
            }
        }
    }
    shifts.sort_by_key(|(threshold, _)| *threshold);
    store.block_offsets.write().shift_after_each(&shifts);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::block_offset_index::tests::ENTRIES_REWRITTEN;
    use crate::entities::{Document, Frame};
    use std::cell::Cell;

    thread_local! {
        /// What `main_text_end` looked at on this thread (see [`super::looked_at`]).
        pub(super) static LOOKED_AT: Cell<usize> = const { Cell::new(0) };
    }

    /// A text of `paragraphs` paragraphs followed by `notes` footnote bodies of one
    /// paragraph each, laid out as the loads lay one out: the bodies after the main text.
    /// Returns the store and where the main text ends.
    fn text_with_notes(paragraphs: u64, notes: u64) -> (Store, i64) {
        let store = Store::new();
        let main: EntityId = 1;
        let mut frames = vec![Frame {
            id: main,
            ..Frame::default()
        }];
        let mut end = 0;
        let mut next_block: EntityId = 1;
        for i in 0..paragraphs {
            if next_block > 1 {
                rope_insert_block_boundary(&store);
            }
            let text = format!("Paragraph {i}.");
            end = store.rope.read().len_chars() as i64 + text.chars().count() as i64;
            rope_append_block(&store, next_block, &text);
            frames[0].blocks.push(next_block);
            frames[0].child_order.push(next_block as i64);
            next_block += 1;
        }
        for i in 0..notes {
            rope_insert_block_boundary(&store);
            rope_append_block(&store, next_block, &format!("Note {i}."));
            frames.push(Frame {
                id: main + 1 + i,
                blocks: vec![next_block],
                child_order: vec![next_block as i64],
                footnote_label: Some(format!("n{i}")),
                ..Frame::default()
            });
            next_block += 1;
        }
        {
            let mut blocks = store.blocks.write();
            for id in 1..next_block {
                blocks.insert(
                    id,
                    Block {
                        id,
                        ..Block::default()
                    },
                );
            }
        }
        store.documents.write().insert(
            1,
            Document {
                id: 1,
                frames: frames.iter().map(|frame| frame.id).collect(),
                ..Document::default()
            },
        );
        let mut stored = store.frames.write();
        for frame in frames {
            stored.insert(frame.id, frame);
        }
        drop(stored);
        (store, end)
    }

    /// Every forward move of a caret asks where the main text ends. It gathered every block
    /// of every note's body and walked the rope index back over them: a document of
    /// thousands of notes spent milliseconds on each arrow key. What it looks at must not
    /// grow with the notes, nor with the text.
    #[test]
    fn the_end_of_the_main_text_costs_the_same_however_many_notes_follow() {
        let mut looked = Vec::new();
        for (paragraphs, notes) in [(50, 100), (50, 2_000), (1_000, 2_000)] {
            let (store, end) = text_with_notes(paragraphs, notes);
            LOOKED_AT.with(|looked| looked.set(0));
            assert_eq!(
                main_text_end(&store),
                Some(end),
                "{paragraphs} paragraphs, {notes} notes"
            );
            looked.push(LOOKED_AT.with(Cell::get));
        }
        assert!(
            looked.windows(2).all(|pair| pair[0] == pair[1]),
            "the end of the main text looked at {looked:?} frames and entries for 100 and \
             2,000 notes, then 1,000 paragraphs"
        );

        // Nothing follows a text without notes, and its end needs no stop.
        let (store, _) = text_with_notes(20, 0);
        assert_eq!(main_text_end(&store), None);
    }

    /// A store of `blocks` blocks of a few characters each, the way the importers lay a
    /// document out: separated by a `\n` boundary. Returns the ids of its blocks, in order.
    fn blocks(blocks: u64) -> (Store, Vec<EntityId>) {
        let store = Store::new();
        let ids: Vec<EntityId> = (1..=blocks).collect();
        for &id in &ids {
            if id > 1 {
                rope_insert_block_boundary(&store);
            }
            rope_append_block(&store, id, &format!("cell {id}"));
        }
        (store, ids)
    }

    /// Emptying the cells of every table a deletion covers clears a block per cell, and a
    /// one-block clear rewrites every index entry after the block: clearing a quarter of
    /// the blocks one at a time rewrites about an eighth of the index squared. Clearing
    /// them together must rewrite each entry once.
    #[test]
    fn clearing_blocks_together_walks_the_index_once() {
        const BLOCKS: u64 = 1_000;
        let (one_by_one, ids) = blocks(BLOCKS);
        let (together, _) = blocks(BLOCKS);
        let cleared: Vec<EntityId> = ids.iter().copied().step_by(4).collect();

        ENTRIES_REWRITTEN.with(|rewritten| rewritten.set(0));
        for &id in &cleared {
            rope_replace_block_content(&one_by_one, id, "");
        }
        let block_by_block = ENTRIES_REWRITTEN.with(Cell::get);

        ENTRIES_REWRITTEN.with(|rewritten| rewritten.set(0));
        rope_clear_blocks(&together, &cleared);
        let in_one_walk = ENTRIES_REWRITTEN.with(Cell::get);

        assert_eq!(
            *together.rope.read(),
            *one_by_one.rope.read(),
            "the same text is left"
        );
        assert_eq!(
            *together.block_offsets.read(),
            *one_by_one.block_offsets.read(),
            "the same index is left"
        );
        assert!(
            block_by_block > 100 * BLOCKS as usize,
            "clearing block by block rewrote {block_by_block} entries"
        );
        assert_eq!(
            in_one_walk,
            BLOCKS as usize,
            "clearing {} blocks together rewrote {in_one_walk} index entries, where one walk \
             of the {BLOCKS} entries is enough",
            cleared.len()
        );
    }

    /// Replace All replaced one match at a time, and each replacement moved every index
    /// entry after it: with a match in every paragraph, the whole index once per match. All
    /// of them together must move each entry once, and leave what one at a time leaves.
    #[test]
    fn replacing_in_many_blocks_walks_the_index_once() {
        const BLOCKS: u64 = 1_000;
        let (one_by_one, ids) = blocks(BLOCKS);
        let (together, _) = blocks(BLOCKS);
        let entities: Vec<Block> = ids
            .iter()
            .map(|&id| Block {
                id,
                ..Block::default()
            })
            .collect();
        // In each block, "cell {id}": "ce" becomes "k", and "!?" goes in at the end: each
        // block grows by one byte, so every entry after it moves.
        let edits: Vec<BlockReplacement<'_>> = entities
            .iter()
            .flat_map(|block| {
                let end = format!("cell {}", block.id).chars().count() as i64;
                [
                    BlockReplacement {
                        block,
                        char_start: 0,
                        char_end: 2,
                        replacement: "k",
                    },
                    BlockReplacement {
                        block,
                        char_start: end,
                        char_end: end,
                        replacement: "!?",
                    },
                ]
            })
            .collect();

        ENTRIES_REWRITTEN.with(|rewritten| rewritten.set(0));
        for edit in edits.iter().rev() {
            replace_in_block(
                &one_by_one,
                edit.block,
                edit.char_start,
                edit.char_end,
                edit.replacement,
                ReplaceFormatPolicy::default(),
            )
            .unwrap();
        }
        let edit_by_edit = ENTRIES_REWRITTEN.with(Cell::get);

        ENTRIES_REWRITTEN.with(|rewritten| rewritten.set(0));
        replace_in_blocks(&together, &edits, ReplaceFormatPolicy::default()).unwrap();
        let in_one_walk = ENTRIES_REWRITTEN.with(Cell::get);

        assert_eq!(
            *together.rope.read(),
            *one_by_one.rope.read(),
            "the same text is left"
        );
        assert_eq!(
            *together.block_offsets.read(),
            *one_by_one.block_offsets.read(),
            "the same index is left"
        );
        assert!(
            together
                .rope
                .read()
                .to_string()
                .starts_with("kll 1!?\nkll 2!?\n")
        );
        assert!(
            edit_by_edit > 100 * BLOCKS as usize,
            "replacing edit by edit rewrote {edit_by_edit} entries"
        );
        assert!(
            in_one_walk <= BLOCKS as usize,
            "{} replacements together rewrote {in_one_walk} index entries, where one walk of \
             the {BLOCKS} entries is enough",
            edits.len()
        );
    }

    /// Two replacements of the same characters cannot both be made: the whole call is
    /// refused and nothing is written.
    #[test]
    fn overlapping_replacements_change_nothing() {
        let store = blocks_holding(&["alpha", "beta"]);
        let block = Block {
            id: 1,
            ..Block::default()
        };
        let edits = [
            BlockReplacement {
                block: &block,
                char_start: 0,
                char_end: 3,
                replacement: "x",
            },
            BlockReplacement {
                block: &block,
                char_start: 2,
                char_end: 4,
                replacement: "y",
            },
        ];
        assert!(replace_in_blocks(&store, &edits, ReplaceFormatPolicy::default()).is_err());
        assert_eq!(store.rope.read().to_string(), "alpha\nbeta");
    }

    /// A store holding one block per text, in order, separated by `\n` boundaries.
    fn blocks_holding(texts: &[&str]) -> Store {
        let store = Store::new();
        for (i, text) in texts.iter().enumerate() {
            if i > 0 {
                rope_insert_block_boundary(&store);
            }
            rope_append_block(&store, i as EntityId + 1, text);
        }
        store
    }

    /// A block followed by an empty last block has a boundary after it, though its range
    /// runs to the end of the rope: the empty block starts there. Taking a range that ends
    /// at the rope's end for the last one counted that boundary as the block's content, so
    /// replacing or clearing the block took the boundary out and left the two blocks at one
    /// offset, where every later read of either sliced the wrong bytes.
    #[test]
    fn emptying_the_block_before_an_empty_last_block_keeps_the_boundary() {
        for together in [false, true] {
            let store = blocks_holding(&["text", ""]);
            if together {
                rope_clear_blocks(&store, &[1]);
            } else {
                rope_replace_block_content(&store, 1, "");
            }
            assert_eq!(store.rope.read().to_string(), "\n", "together: {together}");
            let offsets = store.block_offsets.read();
            assert_eq!(
                *offsets.entries,
                vec![(OffsetMarker::Block(1), 0), (OffsetMarker::Block(2), 1)],
                "together: {together}"
            );
            assert_eq!(offsets.total_bytes(), 1);
        }
    }

    /// A table nested at the start of a cell puts its anchor right after the anchor of the
    /// table holding the cell, and a position on either stands for the tables, not for a
    /// block. Moving one entry off the anchor landed on the other anchor: forward, a caret
    /// meant for the first cell stood on the inner table's anchor, and backward, a range end
    /// meant to stop before the tables stood on the outer anchor's boundary. Resolved as
    /// block positions, both fell back to the document's last block, where a deletion ran to
    /// and a paste landed.
    #[test]
    fn a_position_on_a_chain_of_anchors_snaps_past_the_whole_chain() {
        let with_anchors = |texts: &[&str], anchors: &[usize]| {
            let store = blocks_holding(texts);
            {
                let mut offsets = store.block_offsets.write();
                let mut entries = (*offsets.entries).clone();
                for (i, entry) in entries.iter_mut().enumerate() {
                    if anchors.contains(&i) {
                        entry.0 = OffsetMarker::TableAnchor(100 + i as EntityId);
                    }
                }
                offsets.entries = std::sync::Arc::new(entries);
                offsets.rebuild_marker_index();
            }
            {
                // The rope is the position space only when it mirrors every block.
                let mut blocks = store.blocks.write();
                for id in 1..=texts.len() as EntityId {
                    if !anchors.contains(&(id as usize - 1)) {
                        blocks.insert(
                            id,
                            Block {
                                id,
                                ..Block::default()
                            },
                        );
                    }
                }
            }
            store
        };

        // "Before." 0..7, its boundary 7, the outer anchor 8 and its boundary 9, the inner
        // anchor 10 and its boundary 11, then the inner table's cell at 12.
        let store = with_anchors(
            &[
                "Before.", "\u{FFFC}", "\u{FFFC}", "inner", "outer", "After.",
            ],
            &[1, 2],
        );
        for on_chain in 8..=11 {
            assert_eq!(
                snap_off_table_anchor(&store, on_chain, true),
                12,
                "from {on_chain}"
            );
            assert_eq!(
                snap_off_table_anchor(&store, on_chain, false),
                7,
                "from {on_chain}"
            );
        }
        for off_chain in [0, 6, 7, 12, 17] {
            assert_eq!(snap_off_table_anchor(&store, off_chain, true), off_chain);
            assert_eq!(snap_off_table_anchor(&store, off_chain, false), off_chain);
        }

        // A chain opening the document: back to its start, forward to the first cell.
        let store = with_anchors(&["\u{FFFC}", "\u{FFFC}", "cell"], &[0, 1]);
        for on_chain in 0..=3 {
            assert_eq!(
                snap_off_table_anchor(&store, on_chain, true),
                4,
                "from {on_chain}"
            );
            assert_eq!(
                snap_off_table_anchor(&store, on_chain, false),
                0,
                "from {on_chain}"
            );
        }
    }

    /// Whether a range covers a table's anchor, for ranges around, onto and past one.
    #[test]
    fn a_range_covers_a_table_when_it_holds_its_anchor() {
        let store = blocks_holding(&["Before.", "\u{FFFC}", "cell", "After."]);
        {
            let mut offsets = store.block_offsets.write();
            let mut entries = (*offsets.entries).clone();
            entries[1].0 = OffsetMarker::TableAnchor(40);
            offsets.entries = std::sync::Arc::new(entries);
            offsets.rebuild_marker_index();
        }
        {
            let mut blocks = store.blocks.write();
            for id in [1, 3, 4] {
                blocks.insert(
                    id,
                    Block {
                        id,
                        ..Block::default()
                    },
                );
            }
        }
        // The anchor is at 8, the cell at 10..14, "After." from 15.
        for (start, end, covers) in [
            (0, 8, false),
            (0, 9, true),
            (7, 9, true),
            (8, 9, true),
            (9, 20, false),
            (7, 15, true),
            (14, 15, false),
            (9, 9, false),
        ] {
            assert_eq!(
                range_covers_table_anchor(&store, start, end),
                covers,
                "{start}..{end}"
            );
        }
    }

    /// A rope laid out from `entries`, `None` standing for a table's anchor: the entries'
    /// contents joined by boundaries. Entry `i` is `Block(i + 1)`, or `TableAnchor(i + 1)`.
    fn layout_of(entries: &[Option<&str>]) -> (Store, Vec<OffsetMarker>) {
        let texts: Vec<&str> = entries
            .iter()
            .map(|entry| entry.unwrap_or("\u{FFFC}"))
            .collect();
        let store = blocks_holding(&texts);
        let markers: Vec<OffsetMarker> = entries
            .iter()
            .enumerate()
            .map(|(i, entry)| match entry {
                Some(_) => OffsetMarker::Block(i as EntityId + 1),
                None => OffsetMarker::TableAnchor(i as EntityId + 1),
            })
            .collect();
        {
            let mut offsets = store.block_offsets.write();
            let mut indexed = (*offsets.entries).clone();
            for (slot, marker) in indexed.iter_mut().zip(&markers) {
                slot.0 = *marker;
            }
            offsets.entries = std::sync::Arc::new(indexed);
            offsets.rebuild_marker_index();
        }
        (store, markers)
    }

    /// Removing a set of entries leaves the kept entries' contents joined by boundaries, each
    /// entry indexed where it starts in that text, whether they are removed in one pass or
    /// one at a time, for every subset of each layout. The layouts hold empty blocks next to
    /// table anchors: an empty block right before a last anchor starts where the anchor's
    /// cut starts, which moved it back into the entry before it, or below zero; and a table
    /// alone had four bytes cut from a rope of three.
    #[test]
    fn removing_markers_together_matches_removing_them_one_at_a_time() {
        let layouts: [&[Option<&str>]; 7] = [
            &[
                Some("one"),
                Some(""),
                Some("three"),
                None,
                Some("five"),
                Some(""),
            ],
            &[Some("Hello"), Some(""), None],
            &[Some(""), None],
            &[None],
            &[None, Some("")],
            &[Some("x"), None, Some(""), None, Some("")],
            &[Some(""), None, Some("a"), Some("b"), None],
        ];
        for entries in layouts {
            for subset in 1u32..(1 << entries.len()) {
                let (_, markers) = layout_of(entries);
                let chosen: Vec<OffsetMarker> = markers
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| subset & (1 << i) != 0)
                    .map(|(_, marker)| *marker)
                    .collect();
                // What is left: the kept contents joined by boundaries.
                let mut expected_text = String::new();
                let mut expected_entries: Vec<(OffsetMarker, u32)> = Vec::new();
                for (entry, marker) in entries.iter().zip(&markers) {
                    if chosen.contains(marker) {
                        continue;
                    }
                    if !expected_entries.is_empty() {
                        expected_text.push('\n');
                    }
                    expected_entries.push((*marker, expected_text.len() as u32));
                    expected_text.push_str(entry.unwrap_or("\u{FFFC}"));
                }

                let (one_by_one, _) = layout_of(entries);
                for marker in &chosen {
                    match marker {
                        OffsetMarker::Block(id) => rope_remove_block(&one_by_one, *id),
                        OffsetMarker::TableAnchor(id) => rope_remove_table_anchor(&one_by_one, *id),
                    }
                }
                let (together, _) = layout_of(entries);
                rope_remove_markers(&together, &chosen);
                for (how, store) in [("one by one", &one_by_one), ("together", &together)] {
                    assert_eq!(
                        store.rope.read().to_string(),
                        expected_text,
                        "{how}, removing {chosen:?} from {entries:?}"
                    );
                    let offsets = store.block_offsets.read();
                    assert_eq!(
                        *offsets.entries, expected_entries,
                        "{how}, removing {chosen:?} from {entries:?}"
                    );
                    assert_eq!(
                        offsets.total_bytes() as usize,
                        expected_text.len(),
                        "{how}, removing {chosen:?} from {entries:?}"
                    );
                }
            }
        }
    }

    /// The span an edit changed, from the two ropes around it and where it was asked to go
    /// in: at the caret, at the earliest place from the caret on when the new text repeats
    /// the text in front of it, after the rest of the paragraph when the edit put its text
    /// there, and from before the caret when the edit replaced text there.
    #[test]
    fn a_changed_span_is_found_where_the_edit_went() {
        let span = |before: &str, after: &str, from: usize| {
            changed_span(
                &ropey::Rope::from_str(before),
                &ropey::Rope::from_str(after),
                from,
            )
        };
        // Typed at the caret.
        assert_eq!(span("Hello world", "Hello big world", 6), (6, 6, 10));
        // The same word typed in front of itself: at the caret, not after the old word.
        assert_eq!(span("X inserted Y", "X insertedinserted Y", 2), (2, 2, 10));
        // A table pasted at 3 goes in after the paragraph: from its end, a boundary and the
        // table, in front of the paragraph's own boundary.
        assert_eq!(
            span("one two\nthree", "one two\n\u{FFFC}\nx\ny\nthree", 3),
            (7, 7, 13)
        );
        // Cells filled from the start of the caret's cell, which is before the caret.
        assert_eq!(span("a\nbcd\ne", "a\nx\ny\ne", 4), (2, 5, 5));
        // Nothing changed: an empty span at the caret.
        assert_eq!(span("same", "same", 2), (2, 2, 2));
    }
}
