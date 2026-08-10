// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Prove `TextBlock::addressable_inline_pieces()` — the milestone M-T0 shared accessor —
//! reports offsets in exactly the character space `to_addressable_text()`, `find_all()`, and
//! `blocks().position()` already agree on (see `addressable_text_tests.rs` for that sibling
//! contract, and `TextDocument::to_addressable_text`'s doc comment for the bug class both
//! guard against: pairing an offset from one coordinate space with a string from another).
//!
//! This accessor exists because `common::format_runs::InlinePiece` — what
//! `merge_runs_and_anchors` already produces — carries no offsets at all: it is byte ranges
//! into a block's own `plain_text`, good for slicing that string and nothing else. A caller
//! reconstructing a document position by summing piece text lengths drifts the moment a
//! block holds a multi-byte character, or an inline image/footnote reference at all (their
//! `U+FFFC` sentinel is three bytes but one char). The tests below pin the three boundaries
//! that matter to the DOCX/ODT comment-export use case this accessor was built for: a
//! comment landing right after an inline image, right after a footnote reference, and inside
//! a block that follows a table (so the table's own two-character anchor+separator
//! contribution is already folded into the block's `position()`, and this accessor must not
//! re-shift or double-count it).

use text_document::{AddressablePiece, FindOptions, InlineContent, TextBlock, TextDocument};

fn doc_of(djot: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot(djot).unwrap().wait().unwrap();
    doc
}

/// The first block whose own text contains `needle`. Blocks are matched by content, not by
/// index: `doc.blocks()` also returns footnote-definition blocks and table-cell blocks, so
/// a hard-coded index into "the obvious paragraphs" is not guaranteed to survive a source
/// change.
fn block_containing(doc: &TextDocument, needle: &str) -> TextBlock {
    doc.blocks()
        .into_iter()
        .find(|b| b.text().contains(needle))
        .unwrap_or_else(|| panic!("no block contains {needle:?}"))
}

/// What `to_addressable_text()` must hold at `piece.start..piece.end` for one piece: its own
/// text for a text piece, one `U+FFFC` sentinel for an image or footnote-reference piece —
/// the same sentinel `to_plain_text()` omits and `to_addressable_text()` exists to keep.
fn expected_slice(content: &InlineContent) -> String {
    match content {
        InlineContent::Text(t) => t.clone(),
        InlineContent::Image { .. } | InlineContent::FootnoteRef { .. } => "\u{FFFC}".to_string(),
        InlineContent::Empty => String::new(),
    }
}

/// The general form of the contract every test below specializes: a block's pieces must
/// tile its own `[position(), position() + text().chars().count())` range with no gap, no
/// overlap, and every piece's claimed span must slice `to_addressable_text()` back to
/// exactly what the piece says it holds.
fn assert_pieces_tile_the_addressable_text(doc: &TextDocument, block: &TextBlock, label: &str) {
    let addressable = doc.to_addressable_text().unwrap();
    let chars: Vec<char> = addressable.chars().collect();
    let pieces = block.addressable_inline_pieces();

    let mut cursor = block.position();
    for (i, piece) in pieces.iter().enumerate() {
        assert_eq!(piece.start, cursor, "{label}: piece {i} gap or overlap");
        assert!(piece.start < piece.end, "{label}: piece {i} is empty");
        assert!(
            piece.end <= chars.len(),
            "{label}: piece {i} claims [{}, {}) past a {}-char addressable text",
            piece.start,
            piece.end,
            chars.len()
        );
        let slice: String = chars[piece.start..piece.end].iter().collect();
        assert_eq!(
            slice,
            expected_slice(&piece.content),
            "{label}: piece {i} ({:?}) does not match the addressable text at [{}, {})",
            piece.content,
            piece.start,
            piece.end
        );
        cursor = piece.end;
    }
    assert_eq!(
        cursor,
        block.position() + block.text().chars().count(),
        "{label}: pieces did not cover the whole block"
    );
}

/// A comment boundary landing immediately **after an inline image**: the piece right after
/// the image must start exactly where the image's one-character span ends, and that must be
/// the exact position `find_all` reports for the text starting right after the image.
#[test]
fn a_comment_boundary_right_after_an_inline_image_agrees_with_addressable_text_and_find_all() {
    let doc = doc_of("before ![alt](pic.png) after the picture");
    let block = block_containing(&doc, "after the picture");
    assert_pieces_tile_the_addressable_text(&doc, &block, "image doc");

    let pieces = block.addressable_inline_pieces();
    let image_idx = pieces
        .iter()
        .position(|p| matches!(p.content, InlineContent::Image { .. }))
        .expect("the paragraph has an image");
    let image: &AddressablePiece = &pieces[image_idx];
    assert_eq!(
        image.end - image.start,
        1,
        "an image occupies exactly one character of the addressable text"
    );

    // The boundary a comment landing "right after the image" must split at.
    let after = &pieces[image_idx + 1];
    assert_eq!(
        after.start, image.end,
        "the next piece must resume exactly where the image ends"
    );
    let InlineContent::Text(text) = &after.content else {
        panic!("expected text after the image, got {:?}", after.content);
    };
    assert!(text.starts_with(" after the picture"));

    // And that boundary must agree with `find_all`: searching for the text right after the
    // image returns a match starting exactly at the image's own end.
    let matches = doc
        .find_all(" after the picture", &FindOptions::default())
        .unwrap();
    let m = matches
        .first()
        .expect("the text after the image is findable");
    assert_eq!(
        m.position, image.end,
        "find_all's match position must agree with the piece boundary right after the image"
    );
}

/// A comment boundary landing immediately **after a footnote reference** — the same
/// boundary check as the image test above, for the sibling sentinel-occupying anchor kind.
#[test]
fn a_comment_boundary_right_after_a_footnote_reference_agrees_with_addressable_text_and_find_all() {
    let doc = doc_of("A claim.[^n] settles it.\n\n[^n]: The note body.");
    let block = block_containing(&doc, "settles it.");
    assert_pieces_tile_the_addressable_text(&doc, &block, "footnote doc");

    let pieces = block.addressable_inline_pieces();
    let note_idx = pieces
        .iter()
        .position(|p| matches!(p.content, InlineContent::FootnoteRef { .. }))
        .expect("the paragraph has a footnote reference");
    let note: &AddressablePiece = &pieces[note_idx];
    assert_eq!(
        note.end - note.start,
        1,
        "a footnote reference occupies exactly one character of the addressable text"
    );

    let after = &pieces[note_idx + 1];
    assert_eq!(
        after.start, note.end,
        "the next piece must resume exactly where the footnote reference ends"
    );
    let InlineContent::Text(text) = &after.content else {
        panic!(
            "expected text after the footnote reference, got {:?}",
            after.content
        );
    };
    assert!(text.starts_with(" settles it."));

    let matches = doc
        .find_all(" settles it.", &FindOptions::default())
        .unwrap();
    let m = matches
        .first()
        .expect("the text after the footnote reference is findable");
    assert_eq!(
        m.position, note.end,
        "find_all's match position must agree with the piece boundary right after the \
         footnote reference"
    );
}

/// A block **inside a document containing a table** — the table's own `U+FFFC` anchor (plus
/// its `\n` separator) shifts every later block's `position()` by two characters, and this
/// accessor must carry that shift through rather than re-deriving (and getting wrong) its
/// own. Reusing the exact reproduction from `addressable_text_tests.rs`
/// (`prose_after_a_table_resolves_where_its_block_says_it_does`), which is where this bug
/// class was first caught downstream.
#[test]
fn a_block_after_a_table_agrees_with_addressable_text_and_find_all() {
    let doc = doc_of("intro\n\n| a | b |\n| - | - |\n| c | d |\n\nthe salt-bleached door");
    let block = block_containing(&doc, "salt-bleached");
    assert_pieces_tile_the_addressable_text(&doc, &block, "table doc");

    let pieces = block.addressable_inline_pieces();
    assert_eq!(pieces.len(), 1, "a plain paragraph is a single text piece");
    let piece = &pieces[0];
    let InlineContent::Text(text) = &piece.content else {
        panic!("expected text, got {:?}", piece.content);
    };
    assert_eq!(text, "the salt-bleached door");
    assert_eq!(
        piece.start,
        block.position(),
        "the block's one piece must start exactly at the block's own (table-shifted) position"
    );

    let matches = doc
        .find_all("salt-bleached", &FindOptions::default())
        .unwrap();
    let m = matches.first().expect("findable");
    assert_eq!(
        m.position,
        piece.start + "the ".chars().count(),
        "find_all's match position, offset from the piece's table-shifted start, must land \
         exactly on \"salt-bleached\""
    );
}

/// Broad regression net: whatever the construct (plain paragraphs, an image, a footnote
/// reference, a table, several images in one paragraph, the empty document), every block's
/// pieces must tile the addressable text with no gap, overlap, or content mismatch.
#[test]
fn every_block_in_every_construct_tiles_the_addressable_text() {
    const BATTERY: &[&str] = &[
        "First paragraph.\n\nSecond paragraph.\n\nThird.",
        "before ![alt](pic.png) after",
        "A claim.[^n]\n\n[^n]: The note body.\n\nAfter.",
        "intro\n\n| a | b |\n| - | - |\n| c | d |\n\nafter",
        "a ![p1](p1.png) b ![p2](p2.png) c",
        "",
    ];
    for src in BATTERY {
        let doc = doc_of(src);
        for block in doc.blocks() {
            assert_pieces_tile_the_addressable_text(&doc, &block, &format!("{src:?}"));
        }
    }
}
