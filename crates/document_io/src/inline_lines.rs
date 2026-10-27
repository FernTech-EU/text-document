//! A block's inline segments cut into pieces at a character its text runs hold, for the
//! writers that set a block down a line at a time.

use std::borrow::Cow;

use common::format_runs::{InlineContent, InlineSegment};

/// Split a block's segments at each `separator` a text run holds: one entry per piece,
/// the separator itself in neither. A block without one is borrowed whole.
///
/// A run holding separators is copied once, and each piece of it is that copy with the
/// piece's text: copying the whole run for every piece copied its whole text once a
/// line, and saving a block of 64,000 lines took seconds.
pub(crate) fn split_segments_at(
    elements: &[InlineSegment],
    separator: char,
) -> Vec<Cow<'_, [InlineSegment]>> {
    let breaks = |elem: &InlineSegment| matches!(&elem.content, InlineContent::Text(t) if t.contains(separator));
    if !elements.iter().any(breaks) {
        return vec![Cow::Borrowed(elements)];
    }
    let mut lines: Vec<Cow<'_, [InlineSegment]>> = Vec::new();
    let mut current: Vec<InlineSegment> = Vec::new();
    for elem in elements {
        match &elem.content {
            InlineContent::Text(t) if t.contains(separator) => {
                let format = InlineSegment {
                    content: InlineContent::Empty,
                    ..elem.clone()
                };
                for (i, piece) in t.split(separator).enumerate() {
                    if i > 0 {
                        lines.push(Cow::Owned(std::mem::take(&mut current)));
                    }
                    if !piece.is_empty() {
                        current.push(InlineSegment {
                            content: InlineContent::Text(piece.to_string()),
                            ..format.clone()
                        });
                    }
                }
            }
            _ => current.push(elem.clone()),
        }
    }
    lines.push(Cow::Owned(current));
    lines
}
