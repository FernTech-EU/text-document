//! The **one** implementation of "apply this set of edits to the document".
//!
//! `replace_text` (one replacement at every match) and `replace_ranges` (a different
//! replacement per range, chosen by the caller) are the same edit with different inputs.
//! They must not be two implementations: this crate has already been bitten once by three
//! independent walks over the document drifting apart, and a splice that drifts corrupts
//! prose rather than merely reading it wrong.
//!
//! So everything with a decision in it lives here — planning, splicing, rebasing — and each
//! use case is left with the unit-of-work plumbing it cannot share.
//!
//! ## The three rules a splice must not get wrong
//!
//! 1. **Descending.** Edits are applied last-first, so an earlier edit's length change
//!    cannot move the range a later one still has to address.
//! 2. **Single block.** A block is the unit an edit is applied to. A range straddling two
//!    of them is *refused and reported*, never half-applied.
//! 3. **Rebase.** Every block after a length-changing edit has its `document_position`
//!    shifted, and the document's `character_count` corrected. Skip this and document-wide
//!    addressing is silently wrong from the next edit onward — i.e. after any rename.

use anyhow::Result;
use common::database::Store;
use common::database::rope_helpers::{
    block_char_length, block_content_via_store, rope_delete_in_block, rope_insert_in_block,
};
use common::entities::Block;
use common::format_runs::{
    ReplaceFormatPolicy, check_well_formed, logical_offset_to_byte, shift_images_for_delete,
    shift_images_for_insert, shift_runs_for_replace,
};

/// One range the caller wants replaced, in **char** offsets into the document's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RangeSpec {
    pub position: usize,
    pub length: usize,
    pub replacement: String,
}

/// A range that survived planning, resolved to the block it lands in.
#[derive(Debug, Clone)]
pub(crate) struct PlannedEdit {
    pub block_idx: usize,
    /// Char offset of the edit **within its block**.
    pub block_offset: usize,
    pub length: usize,
    pub replacement: String,
}

/// The plan, plus an honest account of everything it refused.
#[derive(Debug, Default)]
pub(crate) struct Plan {
    /// Ascending by position. Callers apply them in **reverse**.
    pub edits: Vec<PlannedEdit>,
    pub skipped_cross_block: i64,
    pub skipped_overlapping: i64,
}

/// Resolve `specs` against the document's blocks, refusing what cannot be applied.
///
/// Refuses, rather than guesses:
/// - a range that straddles a block boundary (`skipped_cross_block`);
/// - a range that overlaps one already accepted (`skipped_overlapping`) — two edits to the
///   same characters cannot both be honoured, so the **earlier** range wins and the later
///   is reported. Silently applying one of them would rewrite text the caller never asked
///   about.
///
/// An empty range (`length == 0`) is a pure insertion and is legal.
pub(crate) fn plan(blocks: &[Block], specs: &[RangeSpec], store: &Store) -> Plan {
    let mut sorted: Vec<&RangeSpec> = specs.iter().collect();
    sorted.sort_by_key(|s| (s.position, s.length));

    let mut out = Plan::default();
    // The end of the last ACCEPTED range. Anything starting before this overlaps it.
    let mut accepted_end: Option<usize> = None;

    for spec in sorted {
        if let Some(end) = accepted_end
            && spec.position < end
        {
            out.skipped_overlapping += 1;
            continue;
        }

        match resolve_in_block(blocks, spec.position, spec.length, store) {
            Some((block_idx, block_offset)) => {
                accepted_end = Some(spec.position + spec.length);
                out.edits.push(PlannedEdit {
                    block_idx,
                    block_offset,
                    length: spec.length,
                    replacement: spec.replacement.clone(),
                });
            }
            None => out.skipped_cross_block += 1,
        }
    }
    out
}

/// The block containing `position`, and the char offset within it — but only if the whole
/// `[position, position + length)` range fits inside that one block.
fn resolve_in_block(
    blocks: &[Block],
    position: usize,
    length: usize,
    store: &Store,
) -> Option<(usize, usize)> {
    for (i, block) in blocks.iter().enumerate() {
        let start = block.document_position as usize;
        let len = block_char_length(block, store) as usize;
        let end = start + len;

        // `position == end` is a legal insertion point at the very end of a block, but only
        // for a zero-length range; a non-empty range starting there belongs to the next
        // block (or straddles, which is refused below).
        let inside = position >= start && (position < end || (length == 0 && position == end));
        if !inside {
            continue;
        }
        let offset = position - start;
        return (offset + length <= len).then_some((i, offset));
    }
    None
}

/// Apply one edit to a block, in the store, and hand back the block to persist.
///
/// Deliberately takes no unit of work: everything here is store-level (the block's text
/// lives in the rope, its formatting in `format_runs`, its images in `block_images`), so
/// the caller's UoW only has to persist the returned `Block`.
///
/// `policy` decides what the replacement wears where it overwrites formatted prose — see
/// [`ReplaceFormatPolicy`]. The default reproduces the historical behaviour exactly.
pub(crate) fn apply_in_block(
    store: &Store,
    block: &Block,
    char_start: usize,
    char_end: usize,
    replacement: &str,
    policy: ReplaceFormatPolicy,
) -> Result<Block> {
    let images_before = store
        .block_images
        .read()
        .get(&block.id)
        .cloned()
        .unwrap_or_default();
    let block_text = block_content_via_store(block, store);

    let byte_start = logical_offset_to_byte(&block_text, &images_before, char_start as i64);
    let byte_end = logical_offset_to_byte(&block_text, &images_before, char_end as i64);

    let mut new_plain = String::with_capacity(
        block_text.len() - (byte_end - byte_start) as usize + replacement.len(),
    );
    new_plain.push_str(&block_text[..byte_start as usize]);
    new_plain.push_str(replacement);
    new_plain.push_str(&block_text[byte_end as usize..]);

    let inserted_byte_len = replacement.len() as u32;

    // Format runs under an explicit policy, and then CHECK the result rather than assert it:
    // `debug_assert_well_formed` is compiled out of release, so a malformed run list produced
    // in a shipped build went entirely undetected — and autosave wrote it to the writer's
    // file seconds later. A replace that would corrupt a block's formatting fails loudly.
    {
        let mut runs_map = store.format_runs.write();
        let runs = runs_map.entry(block.id).or_default();
        shift_runs_for_replace(runs, byte_start, byte_end, inserted_byte_len, policy)?;
        check_well_formed(runs, new_plain.len())?;
    }
    {
        let mut images_map = store.block_images.write();
        let images = images_map.entry(block.id).or_default();
        shift_images_for_delete(images, byte_start, byte_end);
        shift_images_for_insert(images, byte_start, inserted_byte_len);
    }

    // Mirror the in-block splice into the global rope.
    rope_delete_in_block(store, block.id, byte_start, byte_end);
    rope_insert_in_block(store, block.id, byte_start, replacement);

    let mut updated = block.clone();
    updated.updated_at = chrono::Utc::now();
    Ok(updated)
}

/// Re-derive every block's `document_position` after a set of length-changing edits, and
/// report the document's total character delta.
///
/// Without this, document-wide addressing is silently wrong from the next edit onward —
/// which, for a rename, means the *following* rename lands in the wrong place.
///
/// `blocks_in_order` must be the document's blocks sorted by `document_position` **as they
/// stood before the edits**; `delta_by_block_id` is the net char delta each block absorbed.
/// Returns only the blocks whose position actually moved.
pub(crate) fn rebase_positions(
    blocks_in_order: &[Block],
    delta_by_block_id: &std::collections::HashMap<common::types::EntityId, i64>,
) -> (Vec<Block>, i64) {
    let mut to_update = Vec::new();
    let mut cumulative: i64 = 0;

    for block in blocks_in_order {
        if cumulative != 0 {
            let mut moved = block.clone();
            moved.document_position += cumulative;
            moved.updated_at = chrono::Utc::now();
            to_update.push(moved);
        }
        // A block's own edits shift everything AFTER it, not itself.
        cumulative += delta_by_block_id.get(&block.id).copied().unwrap_or(0);
    }
    (to_update, cumulative)
}

/// Char delta of one edit: how much longer (or shorter) the block got.
pub(crate) fn char_delta(edit: &PlannedEdit) -> i64 {
    edit.replacement.chars().count() as i64 - edit.length as i64
}

/// What a splice actually did — including everything it refused.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Applied {
    pub replacements_count: i64,
    pub skipped_cross_block: i64,
    pub skipped_overlapping: i64,
}
