//! Finding the full reach of one hyperlink.
//!
//! A link is a character format, not an object: it has no identity and no
//! boundary of its own, only a stretch of runs that happen to agree about
//! `anchor_href`. So "the link under the caret" is a question about *extent*,
//! and answering it is the one piece of link handling with no existing API.
//!
//! ## Why coalescing is the whole job
//!
//! Format runs split on **any** field difference, so bolding one word inside a
//! link cuts that link into three runs carrying the same destination. Reporting
//! only the piece under the caret would silently truncate the link to whichever
//! third the caret happened to land in — and an "Edit link" built on that
//! answer would rewrite a third of the writer's text. The extent therefore
//! walks outward from the piece under the caret and absorbs every neighbour
//! that agrees on the destination.
//!
//! Two links that merely sit next to each other stay separate, because they
//! disagree about `anchor_href` — which is also why the comparison is on the
//! destination and not on `is_anchor`.

use crate::flow::AddressablePiece;
use crate::text_block::TextBlock;
use frontend::common::format_runs::InlineContent;

/// One hyperlink's full reach, in the document's addressable character space.
///
/// `start`/`end` are document-relative — the same space `TextCursor::position`,
/// `select_range` and `find_all` matches use — so a caller can select the
/// extent without converting anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkExtent {
    /// `[start, end)` in the document's addressable character space.
    pub start: usize,
    pub end: usize,
    /// The link's destination.
    pub href: String,
    /// The text the link covers — what a writer sees as the link's name.
    pub text: String,
}

/// The destination a piece carries, if it is link text.
///
/// Images and footnote references are skipped even when they sit inside a
/// link's range: they occupy a `U+FFFC` sentinel rather than text, and letting
/// one join the extent would put a sentinel character into `text`, which the
/// caller then writes back as literal prose.
fn href_of(piece: &AddressablePiece) -> Option<&str> {
    match piece.content {
        InlineContent::Text(_) => piece.format.anchor_href.as_deref(),
        _ => None,
    }
}

/// The link covering `position`, or `None` if there is no link there.
///
/// `position` is document-relative. A caret sitting on either edge of a link
/// counts as inside it, matching the caret semantics
/// [`crate::inner::block_at_caret_dto`] applies one level up: a caret at the
/// end of a link is still in it, exactly as a caret at the end of a paragraph
/// is still in that paragraph.
pub(crate) fn link_extent_at(block: &TextBlock, position: usize) -> Option<LinkExtent> {
    let pieces = block.addressable_inline_pieces();

    // The piece under the caret. Inclusive of both edges, so a caret between
    // two pieces prefers the one it closes rather than the one it opens —
    // `rposition` picks the later candidate only when the earlier one does not
    // reach the caret at all.
    let hit = pieces
        .iter()
        .rposition(|p| p.start <= position && position <= p.end)?;

    // A caret on a boundary can touch a plain piece and a link piece at once.
    // Prefer the link: the writer who clicked the end of a link means that
    // link, and there is no competing interpretation — a plain run has nothing
    // to offer this query.
    let anchor = [hit, hit.saturating_sub(1), (hit + 1).min(pieces.len())]
        .into_iter()
        .filter(|&i| i < pieces.len())
        .find(|&i| {
            href_of(&pieces[i]).is_some()
                && pieces[i].start <= position
                && position <= pieces[i].end
        })?;

    let href = href_of(&pieces[anchor])?.to_string();

    // Walk outward while the destination holds. This is the coalescing the
    // module doc describes: bold inside a link splits the runs, and every one
    // of those splinters belongs to the same link.
    let mut first = anchor;
    while first > 0 && href_of(&pieces[first - 1]) == Some(href.as_str()) {
        first -= 1;
    }
    let mut last = anchor;
    while last + 1 < pieces.len() && href_of(&pieces[last + 1]) == Some(href.as_str()) {
        last += 1;
    }

    let text = pieces[first..=last]
        .iter()
        .filter_map(|p| match &p.content {
            InlineContent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<String>();

    Some(LinkExtent {
        start: pieces[first].start,
        end: pieces[last].end,
        href,
        text,
    })
}
