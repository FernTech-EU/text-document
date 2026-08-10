//! Store-aware readers that synthesize inline content views from
//! per-block `format_runs` + `block_images`. The canonical entry
//! point is [`inline_segments_for_block`], which returns the
//! `Vec<InlineSegment>` view used by export, fragments, cursor, and
//! tests.

use crate::database::Store;
use crate::database::rope_helpers::block_document_position;
use crate::entities::Block;
use crate::format_runs::{
    AddressableInlinePiece, FootnoteRefAnchor, FormatRun, ImageAnchor, InlineSegment,
    addressable_inline_pieces, block_anchors, inline_segments_view,
};
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

/// Fetch the footnote references anchored in a block.
pub fn get_block_footnote_refs(store: &Store, block_id: EntityId) -> Vec<FootnoteRefAnchor> {
    store
        .block_footnote_refs
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
    let notes = get_block_footnote_refs(store, block_id);
    inline_segments_view(block_plain_text, &runs, &images, &notes)
}

/// The block's inline pieces (text runs, images, footnote references), addressed in the
/// document's own **addressable character space** — the same space `TextDocument::
/// to_addressable_text()`, `find_all` match positions, and a block's `document_position` all
/// share. See [`crate::format_runs::AddressableInlinePiece`]'s doc comment for the hazard
/// this closes: `FormatRun`/`ImageAnchor` offsets are UTF-8 *bytes* local to this one block,
/// document-wide offsets are *characters*, and a caller bridging the two by hand (or not at
/// all) is exactly how a comment anchored right after a table landed two characters off
/// (see `TextDocument::to_addressable_text`'s doc comment for that incident).
///
/// This is the accessor a writer splitting a comment's character range across runs, images
/// and footnote references — the DOCX and ODT exporters, in particular — should reach for
/// instead of re-deriving `InlinePiece`'s byte offsets into document space by hand: one
/// definition of the weave, shared the same way [`inline_segments_for_block`] already shares
/// it for the byte-space view.
///
/// Takes the whole [`Block`] entity rather than just its id (unlike
/// [`inline_segments_for_block`]): landing in document-wide space needs the block's own
/// `document_position`, and [`block_document_position`]'s fallback path — for a block not yet
/// mirrored into the rope — reads `block.document_position` directly, so the id alone isn't
/// enough. Every existing caller of `inline_segments_for_block` already has the full entity in
/// scope (it's where `block_plain_text` came from), so this costs nothing extra to call.
pub fn addressable_inline_pieces_for_block(
    store: &Store,
    block: &Block,
    block_plain_text: &str,
) -> Vec<AddressableInlinePiece> {
    let runs = get_format_runs(store, block.id);
    let images = get_block_images(store, block.id);
    let notes = get_block_footnote_refs(store, block.id);
    let anchors = block_anchors(&images, &notes);
    // `chars ≤ bytes`, and the rope's own byte length is already `u32`-bounded
    // (`BlockOffsetIndex::total_bytes`) everywhere else in this crate — a document large
    // enough to overflow this cast would already have overflowed the rope it lives in.
    let base_char_offset = block_document_position(block, store) as u32;
    addressable_inline_pieces(block_plain_text, &runs, &anchors, base_char_offset)
}
