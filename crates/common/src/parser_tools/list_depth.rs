//! How deeply a list inserted into a document may nest.
//!
//! # The failure this prevents
//!
//! A paste keeps the nesting of the lists it carries. Text copied from outside
//! (a web page, another program, a file somebody sent) can hold a list nested
//! far deeper than any editing gesture builds, and nothing bounded it on the
//! way in: pasted, a list two hundred levels deep stayed two hundred levels
//! deep. The document then saves as Djot whose nesting a host that bounds it
//! on load refuses (a Djot parser recurses once per list level), so the writer
//! could no longer open a document they had only pasted into.
//!
//! # The limit
//!
//! [`MAX_LIST_LEVELS`] is 16: a top-level item and fifteen levels below it,
//! list indents 0 through [`MAX_LIST_INDENT`]. That is where the nesting
//! gestures of an editor built on this crate stop (Tab and `indent` in
//! teksilo's rich-text editor), so a paste reaches no deeper than typing does.
//! Word processors stop list nesting at nine (Word) or ten (LibreOffice)
//! levels, so 16 keeps every list they produce as it is. With the 64
//! quotation levels those gestures stop at, the deepest line a document can
//! then hold costs a Djot parser 80 levels, under the 128 that
//! [`super::djot_depth::MAX_NESTING_DEPTH`] allows.
//!
//! # What happens to deeper items
//!
//! Every item is kept, in its order, with its text. An item nested deeper than
//! the limit is placed at the deepest level, beside the items there: the
//! structure is flattened below level 16, never cut off.
//!
//! The limit is applied where content is inserted (a paste, an insertion of
//! Djot, HTML or Markdown), not where a document is loaded: a Djot or Markdown
//! document holding deeper lists opens with them as they are. Two readers
//! flatten a list on their own, because they cannot follow it any deeper: the
//! HTML reader, past this depth, and the Djot reader, for a text nested too
//! deeply to parse at all (see `super::djot_depth::flatten_deep_indentation`).

use crate::parser_tools::content_parser::ParsedBlock;
use crate::parser_tools::fragment_schema::{FragmentBlock, FragmentData};

/// The most list levels an insertion leaves in a document: a top-level item is
/// level 1, and its list indent is 0.
pub const MAX_LIST_LEVELS: usize = 16;

/// The deepest list indent an insertion leaves in a document, counted from 0:
/// [`MAX_LIST_LEVELS`] levels.
pub const MAX_LIST_INDENT: i64 = MAX_LIST_LEVELS as i64 - 1;

/// `indent` brought within the levels an insertion may leave: at most
/// [`MAX_LIST_INDENT`], and never below the top level.
pub fn clamp_list_indent(indent: i64) -> i64 {
    indent.clamp(0, MAX_LIST_INDENT)
}

/// Bring every list item of `blocks` within [`MAX_LIST_LEVELS`].
pub fn clamp_parsed_list_indents(blocks: &mut [ParsedBlock]) {
    for block in blocks {
        block.list_indent = clamp_list_indent(i64::from(block.list_indent)) as u32;
    }
}

/// Bring every list item of `fragment` within [`MAX_LIST_LEVELS`]: its blocks
/// and the blocks of its table cells.
pub fn clamp_fragment_list_indents(fragment: &mut FragmentData) {
    let clamp = |block: &mut FragmentBlock| {
        if let Some(list) = block.list.as_mut() {
            list.indent = clamp_list_indent(list.indent);
        }
    };
    fragment.blocks.iter_mut().for_each(clamp);
    for table in &mut fragment.tables {
        for cell in &mut table.cells {
            cell.blocks.iter_mut().for_each(clamp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::ListStyle;
    use crate::parser_tools::fragment_schema::{FragmentList, FragmentTable, FragmentTableCell};

    fn item(indent: i64) -> FragmentBlock {
        FragmentBlock {
            plain_text: String::new(),
            elements: vec![],
            heading_level: None,
            list: Some(FragmentList {
                style: ListStyle::Disc,
                indent,
                prefix: String::new(),
                suffix: String::new(),
            }),
            alignment: None,
            indent: None,
            text_indent: None,
            marker: None,
            top_margin: None,
            bottom_margin: None,
            left_margin: None,
            right_margin: None,
            tab_positions: vec![],
            line_height: None,
            non_breakable_lines: None,
            page_break_before: None,
            direction: None,
            background_color: None,
            is_code_block: None,
            code_language: None,
            hyphenate: None,
            language: None,
        }
    }

    fn indents(blocks: &[FragmentBlock]) -> Vec<i64> {
        blocks
            .iter()
            .filter_map(|block| block.list.as_ref().map(|list| list.indent))
            .collect()
    }

    #[test]
    fn indents_within_the_limit_are_kept() {
        for indent in 0..=MAX_LIST_INDENT {
            assert_eq!(clamp_list_indent(indent), indent);
        }
    }

    #[test]
    fn deeper_indents_land_at_the_deepest_level() {
        assert_eq!(clamp_list_indent(MAX_LIST_INDENT + 1), MAX_LIST_INDENT);
        assert_eq!(clamp_list_indent(i64::MAX), MAX_LIST_INDENT);
        assert_eq!(clamp_list_indent(-3), 0);
    }

    #[test]
    fn a_fragment_is_clamped_in_its_blocks_and_its_cells() {
        let mut fragment = FragmentData {
            blocks: vec![item(0), item(15), item(16), item(199)],
            tables: vec![FragmentTable {
                rows: 1,
                columns: 1,
                cells: vec![FragmentTableCell {
                    row: 0,
                    column: 0,
                    row_span: 1,
                    column_span: 1,
                    blocks: vec![item(40)],
                    fmt_padding: None,
                    fmt_border: None,
                    fmt_vertical_alignment: None,
                    fmt_background_color: None,
                }],
                block_insert_index: 0,
                fmt_border: None,
                fmt_cell_spacing: None,
                fmt_cell_padding: None,
                fmt_width: None,
                fmt_alignment: None,
                column_widths: vec![],
            }],
        };
        clamp_fragment_list_indents(&mut fragment);
        assert_eq!(indents(&fragment.blocks), vec![0, 15, 15, 15]);
        assert_eq!(indents(&fragment.tables[0].cells[0].blocks), vec![15]);
    }

    #[test]
    fn parsed_blocks_are_clamped() {
        let mut blocks = vec![
            ParsedBlock {
                list_indent: 3,
                ..ParsedBlock::default()
            },
            ParsedBlock {
                list_indent: 300,
                ..ParsedBlock::default()
            },
        ];
        clamp_parsed_list_indents(&mut blocks);
        let indents: Vec<u32> = blocks.iter().map(|block| block.list_indent).collect();
        assert_eq!(indents, vec![3, 15]);
    }
}
