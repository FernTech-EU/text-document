use serde::{Deserialize, Serialize};

use crate::entities::*;
use crate::format_runs::{InlineContent, InlineSegment};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentData {
    pub blocks: Vec<FragmentBlock>,
    /// Table fragments extracted from cell selections. Empty for text-only fragments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<FragmentTable>,
}

/// A table (or rectangular sub-region) captured from a cell selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentTable {
    pub rows: usize,
    pub columns: usize,
    pub cells: Vec<FragmentTableCell>,
    /// Index into the parent `FragmentData::blocks` at which this table
    /// should be inserted.  Blocks `[0..index)` come before the table,
    /// blocks `[index..]` come after.  Default `0` for backward compat.
    #[serde(default)]
    pub block_insert_index: usize,
    // ── Table-level formatting ────────────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_border: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_cell_spacing: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_cell_padding: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_width: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_alignment: Option<Alignment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub column_widths: Vec<i64>,
}

/// One cell within a [`FragmentTable`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentTableCell {
    pub row: usize,
    pub column: usize,
    pub row_span: usize,
    pub column_span: usize,
    pub blocks: Vec<FragmentBlock>,
    // ── Cell-level formatting ─────────────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_padding: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_border: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_vertical_alignment: Option<CellVerticalAlignment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_background_color: Option<String>,
}

/// One block of a fragment: its text, its runs and anchors, and its block formatting.
///
/// A field holding nothing (`None`, an empty list) is left out of the JSON a fragment is
/// carried as, and reads back as holding nothing. Written in full, every block and every run
/// of a manuscript's paragraph came to about 18 times its text, and an insertion refuses a
/// fragment past 64 MiB: putting back a whole text of some 26,000 paragraphs failed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentBlock {
    pub plain_text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<FragmentElement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading_level: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<FragmentList>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alignment: Option<Alignment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indent: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_indent: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<MarkerType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_margin: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom_margin: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_margin: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_margin: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tab_positions: Vec<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_breakable_lines: Option<bool>,
    /// Start this block on a new page. `#[serde(default)]` because a fragment
    /// copied by a build that predates the field carries no such key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_break_before: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<TextDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_code_block: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphenate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// One run of a fragment block: its content and its character formatting. As for
/// [`FragmentBlock`], a field holding nothing is left out of the JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentElement {
    pub content: InlineContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_font_point_size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_font_weight: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_font_bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_font_italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_font_underline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_font_overline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_font_strikeout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_letter_spacing: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_word_spacing: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_anchor_href: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fmt_anchor_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_is_anchor: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_tooltip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_underline_style: Option<UnderlineStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_vertical_alignment: Option<CharVerticalAlignment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentList {
    pub style: ListStyle,
    pub indent: i64,
    pub prefix: String,
    pub suffix: String,
}

impl FragmentElement {
    pub fn from_segment(seg: &InlineSegment) -> Self {
        FragmentElement {
            content: seg.content.clone(),
            fmt_font_family: seg.fmt_font_family.clone(),
            fmt_font_point_size: seg.fmt_font_point_size,
            fmt_font_weight: seg.fmt_font_weight,
            fmt_font_bold: seg.fmt_font_bold,
            fmt_font_italic: seg.fmt_font_italic,
            fmt_font_underline: seg.fmt_font_underline,
            fmt_font_overline: seg.fmt_font_overline,
            fmt_font_strikeout: seg.fmt_font_strikeout,
            fmt_letter_spacing: seg.fmt_letter_spacing,
            fmt_word_spacing: seg.fmt_word_spacing,
            fmt_anchor_href: seg.fmt_anchor_href.clone(),
            fmt_anchor_names: seg.fmt_anchor_names.clone(),
            fmt_is_anchor: seg.fmt_is_anchor,
            fmt_tooltip: seg.fmt_tooltip.clone(),
            fmt_underline_style: seg.fmt_underline_style.clone(),
            fmt_vertical_alignment: seg.fmt_vertical_alignment.clone(),
        }
    }
}

impl FragmentBlock {
    /// Returns `true` when this block carries no block-level formatting,
    /// meaning its content is purely inline.
    ///
    /// The quotations a block stands in are not its own formatting: they are carried
    /// beside the fragment (see [`FragmentQuoting`]), and a quoted block is not inline
    /// only whatever this says.
    pub fn is_inline_only(&self) -> bool {
        self.heading_level.is_none()
            && self.list.is_none()
            && self.alignment.is_none()
            && self.indent.unwrap_or(0) == 0
            && self.text_indent.unwrap_or(0) == 0
            && self.marker.is_none()
            && self.top_margin.is_none()
            && self.bottom_margin.is_none()
            && self.left_margin.is_none()
            && self.right_margin.is_none()
            && self.line_height.is_none()
            && self.non_breakable_lines.is_none()
            && self.direction.is_none()
            && self.background_color.is_none()
            && self.is_code_block.is_none()
            && self.code_language.is_none()
            && self.hyphenate.is_none()
            && self.language.is_none()
    }
}

impl FragmentList {
    pub fn from_entity(list: &List) -> Self {
        FragmentList {
            style: list.style.clone(),
            indent: list.indent,
            prefix: list.prefix.clone(),
            suffix: list.suffix.clone(),
        }
    }

    /// The list entity this fragment list stands for, numbered from 1. A list starting past
    /// 1 takes its start from the [`FragmentListStarts`] the fragment carries (see
    /// [`to_entity_starting_at`](Self::to_entity_starting_at)).
    pub fn to_entity(&self) -> List {
        self.to_entity_starting_at(None)
    }

    /// As [`to_entity`](Self::to_entity), the list's first item numbered `start` (`None`
    /// for 1).
    pub fn to_entity_starting_at(&self, start: Option<i64>) -> List {
        List {
            id: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            style: self.style.clone(),
            indent: self.indent,
            prefix: self.prefix.clone(),
            suffix: self.suffix.clone(),
            start: start.filter(|start| *start != 1),
        }
    }
}

/// The number the ordered lists of a fragment start at, where it is not 1, by the index in
/// [`FragmentData::blocks`] of an item of the list: a copy records at the first item it
/// holds of a list the number that item wore, and a fragment parsed from Djot, Markdown or
/// HTML records at every item the start its list was written with. An insertion reads it
/// at the item it makes a new list for, and a writer at the item a list opens with. A copy
/// of a list starting at 3, or of its items from the fourth on, keeps its numbers, and a
/// text put back keeps its lists' starts: both were numbered from 1.
///
/// It travels beside [`FragmentData`] in the JSON an insertion takes, as [`FragmentQuoting`]
/// does and for the same reason: [`FragmentList`] is built field by field by earlier releases
/// of the other text-document crates, which a field added to it would stop compiling. A
/// reader that does not know it ignores it, and numbers the lists from 1.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FragmentListStarts {
    /// `(block index, start)`, in increasing order of the index.
    entries: Vec<(usize, i64)>,
}

impl FragmentListStarts {
    /// Whether every list of the fragment starts at 1.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Record that a list begins with the block at `index`, numbered `start`. A start of 1
    /// records nothing, and takes back what was recorded for that block.
    pub fn set(&mut self, index: usize, start: i64) {
        let at = self.entries.partition_point(|(entry, _)| *entry < index);
        let replaces = self
            .entries
            .get(at)
            .is_some_and(|(entry, _)| *entry == index);
        match (start == 1, replaces) {
            (true, true) => {
                self.entries.remove(at);
            }
            (true, false) => {}
            (false, true) => self.entries[at] = (index, start),
            (false, false) => self.entries.insert(at, (index, start)),
        }
    }

    /// The number a list beginning with the block at `index` starts at, when it is not 1.
    pub fn get(&self, index: usize) -> Option<i64> {
        self.entries
            .binary_search_by_key(&index, |(entry, _)| *entry)
            .ok()
            .and_then(|at| self.entries.get(at))
            .map(|(_, start)| *start)
    }
}

/// Where the blocks and tables of a fragment stood in quotations, counted from the text
/// they were taken from: `0` for a paragraph of the text itself, `2` for one of a
/// quotation nested in another, with the role of the innermost quotation (an epigraph).
///
/// An insertion puts each block and table that deep, and never less deep than the caret
/// already is: a quotation pasted into a paragraph goes in as a quotation, one pasted into
/// a quotation is not quoted twice. A fragment carried no depth, and every quotation of a
/// text put back by a select all and a paste of its Djot came back as plain paragraphs.
///
/// It travels beside [`FragmentData`] in the JSON an insertion takes (see
/// [`fragment_to_json`]), not in [`FragmentBlock`] or [`FragmentTable`]: every field of
/// those two is public and earlier releases of the other text-document crates build them
/// field by field, so a field added to either stops those releases from compiling against
/// this crate. A reader that does not know it ignores it, and reads the fragment unquoted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FragmentQuoting {
    /// The quoted blocks, by their index in [`FragmentData::blocks`], in increasing order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blocks: Vec<QuotedEntry>,
    /// The quoted tables, by their index in [`FragmentData::tables`], in increasing order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tables: Vec<QuotedEntry>,
}

/// One quoted block or table of a [`FragmentQuoting`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct QuotedEntry {
    index: usize,
    depth: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<SemanticRole>,
    /// How many of its quotations, the innermost, open at it (see
    /// [`FragmentQuoting::set_block_opens`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    opens: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl FragmentQuoting {
    /// Whether no block and no table stands in a quotation.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty() && self.tables.is_empty()
    }

    /// Record that the block at `index` stands `depth` quotations deep, the innermost with
    /// `role`. A depth of `0` records nothing.
    pub fn set_block(&mut self, index: usize, depth: u32, role: Option<SemanticRole>) {
        set_entry(&mut self.blocks, index, depth, role);
    }

    /// Record that the table at `index` stands `depth` quotations deep. A depth of `0`
    /// records nothing.
    pub fn set_table(&mut self, index: usize, depth: u32) {
        set_entry(&mut self.tables, index, depth, None);
    }

    /// How many quotations the block at `index` stands in.
    pub fn block_depth(&self, index: usize) -> u32 {
        find_entry(&self.blocks, index).map_or(0, |entry| entry.depth)
    }

    /// The role of the innermost quotation the block at `index` stands in.
    pub fn block_role(&self, index: usize) -> Option<&SemanticRole> {
        find_entry(&self.blocks, index).and_then(|entry| entry.role.as_ref())
    }

    /// How many quotations the table at `index` stands in.
    pub fn table_depth(&self, index: usize) -> u32 {
        find_entry(&self.tables, index).map_or(0, |entry| entry.depth)
    }

    /// Record that the innermost `opens` of the quotations the block at `index` stands in
    /// (see [`set_block`](Self::set_block), which comes first) open at it: they are not the
    /// quotations of the block or table before it, however deep those stand. Two quotations
    /// one after the other were pasted as one, since the depth alone does not tell them
    /// apart.
    pub fn set_block_opens(&mut self, index: usize, opens: u32) {
        set_opens(&mut self.blocks, index, opens);
    }

    /// As [`set_block_opens`](Self::set_block_opens), for the table at `index`.
    pub fn set_table_opens(&mut self, index: usize, opens: u32) {
        set_opens(&mut self.tables, index, opens);
    }

    /// How many of the quotations the block at `index` stands in open at it.
    pub fn block_opens(&self, index: usize) -> u32 {
        find_entry(&self.blocks, index).map_or(0, |entry| entry.opens)
    }

    /// How many of the quotations the table at `index` stands in open at it.
    pub fn table_opens(&self, index: usize) -> u32 {
        find_entry(&self.tables, index).map_or(0, |entry| entry.opens)
    }
}

fn set_opens(entries: &mut [QuotedEntry], index: usize, opens: u32) {
    if let Ok(at) = entries.binary_search_by_key(&index, |entry| entry.index)
        && let Some(entry) = entries.get_mut(at)
    {
        entry.opens = opens.min(entry.depth);
    }
}

fn set_entry(entries: &mut Vec<QuotedEntry>, index: usize, depth: u32, role: Option<SemanticRole>) {
    let at = entries.partition_point(|entry| entry.index < index);
    let replaces = entries.get(at).is_some_and(|entry| entry.index == index);
    if depth == 0 {
        if replaces {
            entries.remove(at);
        }
        return;
    }
    let entry = QuotedEntry {
        index,
        depth,
        role,
        opens: 0,
    };
    if replaces {
        entries[at] = entry;
    } else {
        entries.insert(at, entry);
    }
}

fn find_entry(entries: &[QuotedEntry], index: usize) -> Option<&QuotedEntry> {
    entries
        .binary_search_by_key(&index, |entry| entry.index)
        .ok()
        .and_then(|at| entries.get(at))
}

/// A fragment as an insertion reads it back from its JSON (see [`fragment_from_json`]).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CarriedFragment {
    /// The blocks and tables.
    pub data: FragmentData,
    /// Where they stood in quotations.
    pub quoting: FragmentQuoting,
    /// Whether the insertion replaces a selection its caller removed first, one that held
    /// the whole of a text (see [`replacing_the_text`]).
    pub replaces_text: bool,
    /// Whether the fragment is a whole text, written in a syntax a text is saved in (Djot,
    /// Markdown) or taken from a whole document, rather than a passage copied from a text
    /// or from another application (see [`whole_text_to_json`]).
    pub whole_text: bool,
    /// The numbers its ordered lists start at.
    pub list_starts: FragmentListStarts,
}

/// The JSON a fragment is written as for an insertion.
#[derive(Serialize)]
struct CarriedRef<'a> {
    #[serde(skip_serializing_if = "is_false")]
    replaces_text: bool,
    #[serde(skip_serializing_if = "is_false")]
    whole_text: bool,
    blocks: &'a [FragmentBlock],
    #[serde(skip_serializing_if = "no_tables")]
    tables: &'a [FragmentTable],
    #[serde(skip_serializing_if = "no_quoting")]
    quoting: &'a FragmentQuoting,
    #[serde(skip_serializing_if = "no_list_starts")]
    list_starts: &'a FragmentListStarts,
}

/// The JSON a fragment is read from for an insertion. Every key but `blocks` may be absent,
/// and an unknown one is ignored, as [`FragmentData`] reads it.
#[derive(Deserialize)]
struct Carried {
    #[serde(default)]
    replaces_text: bool,
    #[serde(default)]
    whole_text: bool,
    blocks: Vec<FragmentBlock>,
    #[serde(default)]
    tables: Vec<FragmentTable>,
    #[serde(default)]
    quoting: FragmentQuoting,
    #[serde(default)]
    list_starts: FragmentListStarts,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn no_tables(tables: &&[FragmentTable]) -> bool {
    tables.is_empty()
}

fn no_quoting(quoting: &&FragmentQuoting) -> bool {
    quoting.is_empty()
}

fn no_list_starts(list_starts: &&FragmentListStarts) -> bool {
    list_starts.is_empty()
}

/// Write `data` and its `quoting` as the JSON an insertion takes. With nothing quoted it is
/// exactly the JSON of `data` alone, and every reader of [`FragmentData`] reads it.
pub fn fragment_to_json(
    data: &FragmentData,
    quoting: &FragmentQuoting,
) -> serde_json::Result<String> {
    carried_to_json(data, quoting, &FragmentListStarts::default(), false)
}

/// As [`fragment_to_json`], or [`whole_text_to_json`] when `whole_text`, with the numbers the
/// fragment's ordered lists start at.
pub fn fragment_with_list_starts_to_json(
    data: &FragmentData,
    quoting: &FragmentQuoting,
    list_starts: &FragmentListStarts,
    whole_text: bool,
) -> serde_json::Result<String> {
    carried_to_json(data, quoting, list_starts, whole_text)
}

/// As [`fragment_to_json`], for a fragment that is a whole text: parsed from a syntax a text
/// is saved in (Djot, Markdown), as a host puts back a past version of a text by selecting
/// all of it and inserting the version.
///
/// Inserted over a selection holding the whole of a text, a whole text replaces it and
/// reads as the same text loaded, even when it is a single plain paragraph. Any other
/// fragment of a single plain paragraph, a phrase copied from a text or pasted from another
/// application, goes into the paragraph the removal leaves as typed text does, and keeps
/// that paragraph's formatting.
pub fn whole_text_to_json(
    data: &FragmentData,
    quoting: &FragmentQuoting,
) -> serde_json::Result<String> {
    carried_to_json(data, quoting, &FragmentListStarts::default(), true)
}

fn carried_to_json(
    data: &FragmentData,
    quoting: &FragmentQuoting,
    list_starts: &FragmentListStarts,
    whole_text: bool,
) -> serde_json::Result<String> {
    serde_json::to_string(&CarriedRef {
        replaces_text: false,
        whole_text,
        blocks: &data.blocks,
        tables: &data.tables,
        quoting,
        list_starts,
    })
}

/// Read a fragment written by [`fragment_to_json`] or [`whole_text_to_json`], or as JSON of
/// a [`FragmentData`] alone.
pub fn fragment_from_json(json: &str) -> serde_json::Result<CarriedFragment> {
    let carried: Carried = serde_json::from_str(json)?;
    Ok(CarriedFragment {
        data: FragmentData {
            blocks: carried.blocks,
            tables: carried.tables,
        },
        quoting: carried.quoting,
        replaces_text: carried.replaces_text,
        whole_text: carried.whole_text,
        list_starts: carried.list_starts,
    })
}

/// The mark [`replacing_the_text`] puts in front of a fragment's keys.
const REPLACES_TEXT_MARK: &str = "\"replaces_text\":true";

/// The mark [`as_a_whole_text`] puts in front of a fragment's keys, and
/// [`whole_text_to_json`] writes.
const WHOLE_TEXT_MARK: &str = "\"whole_text\":true";

/// `json`, a fragment's JSON, marked as replacing a whole text: the caller removed a
/// selection holding all of a text before inserting it. An insertion into the one empty
/// paragraph such a removal leaves then gives it the fragment's own formatting, where it
/// otherwise keeps the formatting the paragraph has (see the insertion's use case), when the
/// fragment is a whole text (see [`whole_text_to_json`]) or more than a phrase: several
/// paragraphs, a table, a quotation, or a paragraph of its own format.
///
/// The mark is spliced in front of the other keys: the JSON is not parsed again, which for
/// a whole book put back over itself is most of the insertion's reading. JSON that is not
/// an object is returned as it is, for the insertion to refuse.
pub fn replacing_the_text(json: &str) -> String {
    marked(json, REPLACES_TEXT_MARK)
}

/// `json`, a fragment's JSON, marked as a whole text, as [`whole_text_to_json`] writes it:
/// for a fragment made from a whole document. Spliced in as [`replacing_the_text`] is.
pub fn as_a_whole_text(json: &str) -> String {
    marked(json, WHOLE_TEXT_MARK)
}

/// `json` with `mark` in front of its keys, where the marks stand, unless it holds it there
/// already.
fn marked(json: &str, mark: &str) -> String {
    let Some(rest) = json.strip_prefix('{') else {
        return json.to_string();
    };
    // The marks lead the object, in whichever order they were put in.
    let mut leading = rest;
    while let Some(present) = [REPLACES_TEXT_MARK, WHOLE_TEXT_MARK]
        .into_iter()
        .find(|present| leading.starts_with(present))
    {
        if present == mark {
            return json.to_string();
        }
        let after = &leading[present.len()..];
        leading = after.strip_prefix(',').unwrap_or(after);
    }
    let separator = if rest.trim_start().starts_with('}') {
        ""
    } else {
        ","
    };
    let mut marked = String::with_capacity(json.len() + mark.len() + 1);
    marked.push('{');
    marked.push_str(mark);
    marked.push_str(separator);
    marked.push_str(rest);
    marked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(text: &str) -> FragmentBlock {
        FragmentBlock {
            plain_text: text.to_string(),
            elements: Vec::new(),
            heading_level: None,
            list: None,
            alignment: None,
            indent: None,
            text_indent: None,
            marker: None,
            top_margin: None,
            bottom_margin: None,
            left_margin: None,
            right_margin: None,
            tab_positions: Vec::new(),
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

    fn data() -> FragmentData {
        FragmentData {
            blocks: vec![block("One"), block("Two"), block("Three")],
            tables: Vec::new(),
        }
    }

    /// Nothing quoted, the carried JSON is the fragment's own JSON: a reader of
    /// `FragmentData` alone reads it, and the carrier cannot drift from it unseen.
    #[test]
    fn an_unquoted_fragment_is_written_as_its_data_alone() {
        let data = data();
        let carried = fragment_to_json(&data, &FragmentQuoting::default()).unwrap();
        assert_eq!(carried, serde_json::to_string(&data).unwrap());
    }

    /// The quotations come back from the JSON, and a reader of `FragmentData` alone, as
    /// an earlier release is, still reads the blocks.
    #[test]
    fn the_quotations_travel_beside_the_data() {
        let data = data();
        let mut quoting = FragmentQuoting::default();
        quoting.set_block(2, 1, None);
        quoting.set_block(1, 2, Some(SemanticRole::Epigraph));
        quoting.set_block(0, 0, None);
        let json = fragment_to_json(&data, &quoting).unwrap();
        let back = fragment_from_json(&json).unwrap();
        assert_eq!(back.quoting, quoting);
        assert_eq!(back.quoting.block_depth(0), 0);
        assert_eq!(back.quoting.block_depth(1), 2);
        assert_eq!(back.quoting.block_role(1), Some(&SemanticRole::Epigraph));
        assert_eq!(back.quoting.block_depth(2), 1);
        assert!(!back.replaces_text);
        let alone: FragmentData = serde_json::from_str(&json).unwrap();
        assert_eq!(alone.blocks.len(), 3);
    }

    /// The mark of a whole text replaced reads back, whatever the fragment holds, and is
    /// put in once.
    #[test]
    fn a_fragment_marked_as_replacing_a_text_reads_back_marked() {
        let json = fragment_to_json(&data(), &FragmentQuoting::default()).unwrap();
        let marked = replacing_the_text(&json);
        assert!(fragment_from_json(&marked).unwrap().replaces_text);
        assert_eq!(replacing_the_text(&marked), marked);
        let alone: FragmentData = serde_json::from_str(&marked).unwrap();
        assert_eq!(alone.blocks.len(), 3);
        assert!(
            fragment_from_json(&replacing_the_text("{\"blocks\":[]}"))
                .is_ok_and(|f| f.replaces_text)
        );
        assert_eq!(replacing_the_text("not json"), "not json");
    }

    /// A whole text reads back as one, written so or marked afterwards, beside the mark of a
    /// whole text replaced and in either order; each mark goes in once, and a reader of
    /// `FragmentData` alone still reads the blocks.
    #[test]
    fn a_whole_text_reads_back_as_one_beside_the_mark_of_a_text_replaced() {
        let quoting = FragmentQuoting::default();
        let plain = fragment_to_json(&data(), &quoting).unwrap();
        assert!(!fragment_from_json(&plain).unwrap().whole_text);

        let written = whole_text_to_json(&data(), &quoting).unwrap();
        let spliced = as_a_whole_text(&plain);
        for whole in [&written, &spliced] {
            let back = fragment_from_json(whole).unwrap();
            assert!(back.whole_text && !back.replaces_text, "{whole}");
            let replacing = replacing_the_text(whole);
            let back = fragment_from_json(&replacing).unwrap();
            assert!(back.whole_text && back.replaces_text, "{replacing}");
            assert_eq!(as_a_whole_text(&replacing), replacing);
            assert_eq!(replacing_the_text(&replacing), replacing);
            let alone: FragmentData = serde_json::from_str(&replacing).unwrap();
            assert_eq!(alone.blocks.len(), 3);
        }
        let replacing_first = as_a_whole_text(&replacing_the_text(&plain));
        let back = fragment_from_json(&replacing_first).unwrap();
        assert!(back.whole_text && back.replaces_text, "{replacing_first}");
        assert_eq!(as_a_whole_text(&replacing_first), replacing_first);
        assert_eq!(replacing_the_text(&replacing_first), replacing_first);
        assert_eq!(as_a_whole_text("not json"), "not json");
    }
}
