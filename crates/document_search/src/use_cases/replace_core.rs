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

use anyhow::{Result, anyhow};
use common::database::Store;
use common::database::rope_helpers::{BlockReplacement, block_char_length, replace_in_blocks};
use common::entities::Block;
use common::format_runs::ReplaceFormatPolicy;
use common::types::EntityId;
use std::collections::HashMap;

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

    // Each block's extent, measured once, in the order of their starts: the ranges come in
    // order too, so one sweep places them all. Placing each range by walking the blocks from
    // the first cost a walk of the document per range, and Replace All of a word found in
    // every paragraph grew with the square of the text's length.
    let mut extents: Vec<Extent> = blocks
        .iter()
        .enumerate()
        .map(|(block_idx, block)| Extent {
            start: block.document_position.max(0) as usize,
            len: block_char_length(block, store).max(0) as usize,
            block_idx,
        })
        .collect();
    extents.sort_by_key(|extent| (extent.start, extent.block_idx));
    let mut sweep = 0;

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

        match resolve_in_block(&extents, &mut sweep, spec.position, spec.length) {
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

/// Where a block's text lies: `[start, start + len)`, and the block's index in the planner's
/// input.
#[derive(Debug, Clone, Copy)]
struct Extent {
    start: usize,
    len: usize,
    block_idx: usize,
}

/// The block containing `position`, and the char offset within it — but only if the whole
/// `[position, position + length)` range fits inside that one block.
///
/// `extents` is in the order of their starts, and the ranges are placed in the order of
/// theirs: `sweep` is the first extent a range from here on can lie in, and moves past each
/// extent ending before `position`, which no later range reaches either.
fn resolve_in_block(
    extents: &[Extent],
    sweep: &mut usize,
    position: usize,
    length: usize,
) -> Option<(usize, usize)> {
    while let Some(extent) = extents.get(*sweep)
        && extent.start + extent.len < position
    {
        blocks_examined(1);
        *sweep += 1;
    }
    for extent in extents.get(*sweep..).unwrap_or_default() {
        blocks_examined(1);
        if extent.start > position {
            break;
        }
        let end = extent.start + extent.len;
        // `position == end` is a legal insertion point at the very end of a block, but only
        // for a zero-length range; a non-empty range starting there belongs to the next
        // block (or straddles, which is refused below).
        let inside = position < end || (length == 0 && position == end);
        if !inside {
            continue;
        }
        let offset = position - extent.start;
        return (offset + length <= extent.len).then_some((extent.block_idx, offset));
    }
    None
}

/// Make every edit of `plan` in the store, in one pass: each block's formatting, anchors and
/// text, the rope and its index (see [`replace_in_blocks`]). Returns the blocks the edits
/// changed, with a bumped `updated_at`, for the caller to persist, and the net char delta each
/// of them absorbed, for [`rebase_positions`].
///
/// The edits were made one at a time, and each moved every entry of the rope's index after
/// it: a walk of the document per replacement.
///
/// `policy` decides what the replacement wears where it overwrites formatted prose — see
/// [`ReplaceFormatPolicy`]. The default reproduces the historical behaviour exactly.
pub(crate) fn apply_plan(
    store: &Store,
    blocks: &[Block],
    plan: &Plan,
    policy: ReplaceFormatPolicy,
) -> Result<(Vec<Block>, HashMap<EntityId, i64>)> {
    let mut edits: Vec<BlockReplacement<'_>> = Vec::with_capacity(plan.edits.len());
    let mut delta_by_block_id: HashMap<EntityId, i64> = HashMap::new();
    let mut changed: Vec<Block> = Vec::new();
    for edit in &plan.edits {
        let block = blocks
            .get(edit.block_idx)
            .ok_or_else(|| anyhow!("Block {} not found", edit.block_idx))?;
        edits.push(BlockReplacement {
            block,
            char_start: edit.block_offset as i64,
            char_end: (edit.block_offset + edit.length) as i64,
            replacement: &edit.replacement,
        });
        let delta = delta_by_block_id.entry(block.id).or_insert_with(|| {
            let mut updated = block.clone();
            updated.updated_at = chrono::Utc::now();
            changed.push(updated);
            0
        });
        *delta += char_delta(edit);
    }
    replace_in_blocks(store, &edits, policy)?;
    Ok((changed, delta_by_block_id))
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
    delta_by_block_id: &HashMap<EntityId, i64>,
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

/// Record `count` blocks the planner looked at to place a range. Unit tests count them: the
/// planner's cost is what made Replace All grow with the square of a text's length, and a
/// debug build cannot time it.
fn blocks_examined(count: usize) {
    #[cfg(test)]
    tests::BLOCKS_EXAMINED.with(|examined| examined.set(examined.get() + count));
    #[cfg(not(test))]
    let _ = count;
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::database::rope_helpers::{rope_append_block, rope_insert_block_boundary};
    use std::cell::Cell;

    thread_local! {
        /// Blocks the planner looked at on this thread (see [`super::blocks_examined`]).
        pub(super) static BLOCKS_EXAMINED: Cell<usize> = const { Cell::new(0) };
    }

    /// A document of `paragraphs` paragraphs, "Paragraph {i} text.", and one range per
    /// paragraph over its first word: Replace All of a word every paragraph starts with.
    fn document(paragraphs: u64) -> (Store, Vec<Block>, Vec<RangeSpec>) {
        let store = Store::new();
        let mut blocks = Vec::new();
        let mut specs = Vec::new();
        let mut position = 0;
        for id in 1..=paragraphs {
            if id > 1 {
                rope_insert_block_boundary(&store);
            }
            let text = format!("Paragraph {id} text.");
            rope_append_block(&store, id, &text);
            blocks.push(Block {
                id,
                document_position: position as i64,
                ..Block::default()
            });
            specs.push(RangeSpec {
                position,
                length: "Paragraph".len(),
                replacement: "N".to_string(),
            });
            position += text.chars().count() + 1;
        }
        (store, blocks, specs)
    }

    fn examined_planning(paragraphs: u64) -> usize {
        let (store, blocks, specs) = document(paragraphs);
        BLOCKS_EXAMINED.with(|examined| examined.set(0));
        let plan = plan(&blocks, &specs, &store);
        assert_eq!(
            plan.edits.len(),
            paragraphs as usize,
            "every range is placed"
        );
        assert_eq!(plan.skipped_cross_block, 0);
        for (edit, block) in plan.edits.iter().zip(0..) {
            assert_eq!((edit.block_idx, edit.block_offset), (block, 0));
        }
        BLOCKS_EXAMINED.with(Cell::get)
    }

    /// Replace All placed each match by walking the document's blocks from the first:
    /// with a match in every paragraph, a walk of half the document per match, so replacing
    /// a word throughout a long text grew with the square of its length. Placing every match
    /// looks at each block a bounded number of times.
    #[test]
    fn placing_a_match_in_every_paragraph_looks_at_each_block_a_few_times() {
        let small = examined_planning(1_000);
        let large = examined_planning(2_000);
        let ratio = large as f64 / small as f64;
        assert!(
            ratio < 2.5,
            "doubling the matches from 1,000 to 2,000 made the planner look at {small} then \
             {large} blocks ({ratio:.2} times): it is walking the document once per match again"
        );
        assert!(
            large <= 3 * 2_000,
            "{large} blocks looked at for 2,000 matches"
        );
    }

    /// A range is placed as before: in the block holding it, an insertion at a block's end
    /// in that block, and a range across a boundary refused.
    #[test]
    fn ranges_are_placed_in_the_block_that_holds_them() {
        let (store, blocks, _) = document(3);
        // "Paragraph 1 text." is 17 characters: the first block ends at 17, the second
        // starts at 18.
        let specs = vec![
            RangeSpec {
                position: 17,
                length: 0,
                replacement: "!".into(),
            },
            RangeSpec {
                position: 17,
                length: 2,
                replacement: "x".into(),
            },
            RangeSpec {
                position: 20,
                length: 3,
                replacement: "y".into(),
            },
            RangeSpec {
                position: 36,
                length: 0,
                replacement: "z".into(),
            },
        ];
        let plan = plan(&blocks, &specs, &store);
        let placed: Vec<(usize, usize)> = plan
            .edits
            .iter()
            .map(|edit| (edit.block_idx, edit.block_offset))
            .collect();
        assert_eq!(placed, vec![(0, 17), (1, 2), (2, 0)]);
        assert_eq!(plan.skipped_cross_block, 1);
    }
}
