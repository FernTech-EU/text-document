//! How deeply a Markdown or HTML text read into a document may nest its quotations.
//!
//! # The failure this prevents
//!
//! A quotation is a frame, and the writers, a copy and a document's other readers walk
//! frames one call deeper for each quotation. The Markdown and HTML readers kept every
//! level they read: a Markdown text nested 500 quotations deep loaded, and its first save,
//! export or copy aborted the process on the 2 MiB stack a spawned thread gets, in a debug
//! build. A stack overflow is not a panic: nothing catches it, and a host loses whatever
//! the writer had not saved. The Djot reader never let a text that deep in (see
//! [`super::djot_depth::MAX_NESTING_DEPTH`]).
//!
//! # The limit
//!
//! [`MAX_QUOTE_LEVELS`] is 64: where the quotation gestures of an editor built on this
//! crate stop (see [`super::list_depth`]), so a text read in nests no deeper than one
//! typed. With the list levels an insertion keeps, the deepest line a document can then
//! hold costs a Djot parser 80 levels, under the 128 its reader allows, so the document's
//! own Djot reloads as it is.
//!
//! # What happens to deeper blocks
//!
//! Every block is kept, in its order, with its text: a block quoted deeper than the limit
//! stands in the deepest quotation the limit allows, beside the blocks there. The
//! structure is flattened past level 64, never cut off. Djot has its own door, which
//! measures the text before its parser reads it.

use crate::parser_tools::content_parser::ParsedElement;

/// The most quotations a block read from Markdown or HTML stands in.
pub const MAX_QUOTE_LEVELS: u32 = 64;

/// Bring every block and table of `elements`, and every block of a footnote's body, within
/// [`MAX_QUOTE_LEVELS`] quotations. Of the quotations an element opens (its
/// `blockquote_opens`), those past the limit are gone, so it opens only the ones left: a
/// block deeper than the limit goes on in the deepest quotation kept, beside the blocks
/// there.
pub fn clamp_quote_depths(elements: &mut [ParsedElement]) {
    let clamp = |depth: &mut u32, opens: &mut u32| {
        if *depth > MAX_QUOTE_LEVELS {
            let cut = *depth - MAX_QUOTE_LEVELS;
            *opens = opens.saturating_sub(cut);
            *depth = MAX_QUOTE_LEVELS;
        }
    };
    for element in elements {
        match element {
            ParsedElement::Block(block) => {
                clamp(&mut block.blockquote_depth, &mut block.blockquote_opens)
            }
            ParsedElement::Table(table) => {
                clamp(&mut table.blockquote_depth, &mut table.blockquote_opens)
            }
            ParsedElement::FootnoteDefinition { blocks, .. } => {
                for block in blocks {
                    clamp(&mut block.blockquote_depth, &mut block.blockquote_opens);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser_tools::content_parser::ParsedBlock;

    fn block(depth: u32, opens: u32) -> ParsedElement {
        ParsedElement::Block(ParsedBlock {
            blockquote_depth: depth,
            blockquote_opens: opens,
            ..ParsedBlock::default()
        })
    }

    fn quoting(elements: &[ParsedElement]) -> Vec<(u32, u32)> {
        elements
            .iter()
            .filter_map(|element| match element {
                ParsedElement::Block(block) => {
                    Some((block.blockquote_depth, block.blockquote_opens))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_block_quoted_past_the_limit_goes_on_in_the_deepest_quotation_kept() {
        let mut elements = vec![
            block(1, 1),
            block(MAX_QUOTE_LEVELS, 0),
            block(MAX_QUOTE_LEVELS + 10, 10),
            block(MAX_QUOTE_LEVELS + 10, 12),
            block(MAX_QUOTE_LEVELS + 500, 0),
        ];
        clamp_quote_depths(&mut elements);
        assert_eq!(
            quoting(&elements),
            [
                (1, 1),
                (MAX_QUOTE_LEVELS, 0),
                (MAX_QUOTE_LEVELS, 0),
                (MAX_QUOTE_LEVELS, 2),
                (MAX_QUOTE_LEVELS, 0),
            ]
        );
    }
}
