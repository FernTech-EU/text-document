//! Store-aware readers that synthesize inline content views from
//! per-block `format_runs` + `block_images`. The canonical entry
//! point is [`inline_segments_for_block`], which returns the
//! `Vec<InlineSegment>` view used by export, fragments, cursor, and
//! tests.

use crate::database::Store;
use crate::format_runs::{FormatRun, ImageAnchor, InlineSegment, inline_segments_view};
use crate::types::EntityId;

/// Fetch the format runs for a block. Returns an empty Vec if the block
/// has no runs (treated the same as a missing entry).
pub fn get_format_runs(store: &Store, block_id: EntityId) -> Vec<FormatRun> {
    store
        .format_runs
        .read()
        .get(&block_id)
        .cloned()
        .unwrap_or_default()
}

/// Fetch the image anchors for a block.
pub fn get_block_images(store: &Store, block_id: EntityId) -> Vec<ImageAnchor> {
    store
        .block_images
        .read()
        .get(&block_id)
        .cloned()
        .unwrap_or_default()
}

/// Synthesize the `Vec<InlineSegment>` view for a block from its
/// format_runs and block_images. Callers must pass the block's
/// `plain_text` (which they already have in scope from a prior
/// `get_block` call) — this avoids re-locking the blocks table.
///
/// This is the Phase 1.14b-and-forward reader function. Returns segments
/// in document order.
pub fn inline_segments_for_block(
    store: &Store,
    block_id: EntityId,
    block_plain_text: &str,
) -> Vec<InlineSegment> {
    let runs = get_format_runs(store, block_id);
    let images = get_block_images(store, block_id);
    inline_segments_view(block_plain_text, &runs, &images)
}
