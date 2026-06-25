use crate::entities::{ListStyle, MarkerType, TextDirection};

/// A parsed inline span with formatting info
#[derive(Debug, Clone, Default)]
pub struct ParsedSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub code: bool,
    /// Superscript (djot `^x^`). Maps to `CharVerticalAlignment::SuperScript`.
    pub superscript: bool,
    /// Subscript (djot `~x~`). Maps to `CharVerticalAlignment::SubScript`.
    pub subscript: bool,
    pub link_href: Option<String>,
}

/// A parsed table cell containing inline spans.
#[derive(Debug, Clone)]
pub struct ParsedTableCell {
    pub spans: Vec<ParsedSpan>,
}

/// A parsed table extracted from markdown or HTML.
#[derive(Debug, Clone)]
pub struct ParsedTable {
    /// Number of header rows (typically 1 for markdown tables).
    pub header_rows: usize,
    /// All rows (header + body), each containing cells with their inline spans.
    pub rows: Vec<Vec<ParsedTableCell>>,
    /// Blockquote nesting depth at the point the table appeared
    /// (0 = not inside a blockquote), mirroring `ParsedBlock::blockquote_depth`.
    pub blockquote_depth: u32,
}

/// A parsed element: either a block or a table.
#[derive(Debug, Clone)]
pub enum ParsedElement {
    Block(ParsedBlock),
    Table(ParsedTable),
}

impl ParsedElement {
    /// Extract blocks, flattening tables into one block per cell.
    /// Use when table structure is not needed.
    pub fn flatten_to_blocks(elements: Vec<ParsedElement>) -> Vec<ParsedBlock> {
        let mut blocks = Vec::new();
        for elem in elements {
            match elem {
                ParsedElement::Block(b) => blocks.push(b),
                ParsedElement::Table(t) => {
                    for row in t.rows {
                        for cell in row {
                            blocks.push(ParsedBlock {
                                spans: cell.spans,
                                heading_level: None,
                                list_style: None,
                                list_indent: 0,
                                list_prefix: String::new(),
                                list_suffix: String::new(),
                                marker: None,
                                is_code_block: false,
                                code_language: None,
                                blockquote_depth: t.blockquote_depth,
                                line_height: None,
                                non_breakable_lines: None,
                                direction: None,
                                background_color: None,
                            });
                        }
                    }
                }
            }
        }
        if blocks.is_empty() {
            blocks.push(ParsedBlock {
                spans: vec![ParsedSpan {
                    text: String::new(),
                    ..Default::default()
                }],
                heading_level: None,
                list_style: None,
                list_indent: 0,
                list_prefix: String::new(),
                list_suffix: String::new(),
                marker: None,
                is_code_block: false,
                code_language: None,
                blockquote_depth: 0,
                line_height: None,
                non_breakable_lines: None,
                direction: None,
                background_color: None,
            });
        }
        blocks
    }
}

/// A parsed block (paragraph, heading, list item, code block)
#[derive(Debug, Clone)]
pub struct ParsedBlock {
    pub spans: Vec<ParsedSpan>,
    pub heading_level: Option<i64>,
    pub list_style: Option<ListStyle>,
    pub list_indent: u32,
    /// Ordered-list delimiter prefix (e.g. `"("` for djot `(1)` lists; empty
    /// otherwise). Stored on the `List` entity for round-trip fidelity.
    pub list_prefix: String,
    /// Ordered-list delimiter suffix (`"."` for `1.`, `")"` for `1)`/`(1)`;
    /// empty for unordered lists).
    pub list_suffix: String,
    /// Task-list checkbox marker (djot `- [ ]` / `- [x]`). Maps to
    /// `Block.fmt_marker`. `None` for non-task blocks.
    pub marker: Option<MarkerType>,
    pub is_code_block: bool,
    pub code_language: Option<String>,
    pub blockquote_depth: u32,
    pub line_height: Option<i64>,
    pub non_breakable_lines: Option<bool>,
    pub direction: Option<TextDirection>,
    pub background_color: Option<String>,
}

impl ParsedBlock {
    /// Returns `true` when this block carries no block-level formatting,
    /// meaning its content is purely inline.
    pub fn is_inline_only(&self) -> bool {
        self.heading_level.is_none()
            && self.list_style.is_none()
            && !self.is_code_block
            && self.blockquote_depth == 0
            && self.line_height.is_none()
            && self.non_breakable_lines.is_none()
            && self.direction.is_none()
            && self.background_color.is_none()
    }
}

// ─── Markdown parsing ────────────────────────────────────────────────

pub fn parse_markdown(markdown: &str) -> Vec<ParsedElement> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(markdown, options);

    let mut elements: Vec<ParsedElement> = Vec::new();
    let mut current_spans: Vec<ParsedSpan> = Vec::new();
    let mut current_heading: Option<i64> = None;
    let mut current_list_style: Option<ListStyle> = None;
    let mut is_code_block = false;
    let mut code_language: Option<String> = None;
    let mut blockquote_depth: u32 = 0;
    let mut in_block = false;

    // Formatting state stack
    let mut bold = false;
    let mut italic = false;
    let mut strikeout = false;
    let mut link_href: Option<String> = None;

    // List style stack for nested lists (also tracks nesting depth)
    let mut list_stack: Vec<Option<ListStyle>> = Vec::new();
    let mut current_list_indent: u32 = 0;

    // Table tracking state
    let mut in_table = false;
    let mut in_table_head = false;
    let mut table_rows: Vec<Vec<ParsedTableCell>> = Vec::new();
    let mut current_row_cells: Vec<ParsedTableCell> = Vec::new();
    let mut current_cell_spans: Vec<ParsedSpan> = Vec::new();
    let mut table_header_rows: usize = 0;

    for event in parser {
        match event {
            Event::Start(Tag::Paragraph) => {
                in_block = true;
                current_heading = None;
                is_code_block = false;
            }
            Event::End(TagEnd::Paragraph) => {
                if !current_spans.is_empty() || in_block {
                    elements.push(ParsedElement::Block(ParsedBlock {
                        spans: std::mem::take(&mut current_spans),
                        heading_level: current_heading.take(),
                        list_style: current_list_style.clone(),
                        list_indent: current_list_indent,
                        list_prefix: String::new(),
                        list_suffix: String::new(),
                        marker: None,
                        is_code_block: false,
                        code_language: None,
                        blockquote_depth,
                        line_height: None,
                        non_breakable_lines: None,
                        direction: None,
                        background_color: None,
                    }));
                }
                in_block = false;
                current_list_style = None;
            }
            Event::Start(Tag::Heading { level, .. }) => {
                in_block = true;
                current_heading = Some(heading_level_to_i64(level));
                is_code_block = false;
            }
            Event::End(TagEnd::Heading(_)) => {
                elements.push(ParsedElement::Block(ParsedBlock {
                    spans: std::mem::take(&mut current_spans),
                    heading_level: current_heading.take(),
                    list_style: None,
                    list_indent: 0,
                    list_prefix: String::new(),
                    list_suffix: String::new(),
                    marker: None,
                    is_code_block: false,
                    code_language: None,
                    blockquote_depth,
                    line_height: None,
                    non_breakable_lines: None,
                    direction: None,
                    background_color: None,
                }));
                in_block = false;
            }
            Event::Start(Tag::List(ordered)) => {
                let style = if ordered.is_some() {
                    Some(ListStyle::Decimal)
                } else {
                    Some(ListStyle::Disc)
                };
                list_stack.push(style);
            }
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();
            }
            Event::Start(Tag::Item) => {
                // Flush any accumulated spans from the parent item before
                // starting a child item in a tight list
                if !current_spans.is_empty() {
                    elements.push(ParsedElement::Block(ParsedBlock {
                        spans: std::mem::take(&mut current_spans),
                        heading_level: None,
                        list_style: current_list_style.clone(),
                        list_indent: current_list_indent,
                        list_prefix: String::new(),
                        list_suffix: String::new(),
                        marker: None,
                        is_code_block: false,
                        code_language: None,
                        blockquote_depth,
                        line_height: None,
                        non_breakable_lines: None,
                        direction: None,
                        background_color: None,
                    }));
                }
                in_block = true;
                current_list_style = list_stack.last().cloned().flatten();
                current_list_indent = if list_stack.is_empty() {
                    0
                } else {
                    (list_stack.len() - 1) as u32
                };
            }
            Event::End(TagEnd::Item) => {
                // The paragraph inside the item will have already been flushed,
                // but if there was no inner paragraph (tight list), flush now.
                if !current_spans.is_empty() {
                    elements.push(ParsedElement::Block(ParsedBlock {
                        spans: std::mem::take(&mut current_spans),
                        heading_level: None,
                        list_style: current_list_style.clone(),
                        list_indent: current_list_indent,
                        list_prefix: String::new(),
                        list_suffix: String::new(),
                        marker: None,
                        is_code_block: false,
                        code_language: None,
                        blockquote_depth,
                        line_height: None,
                        non_breakable_lines: None,
                        direction: None,
                        background_color: None,
                    }));
                }
                in_block = false;
                current_list_style = None;
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                in_block = true;
                is_code_block = true;
                code_language = match &kind {
                    pulldown_cmark::CodeBlockKind::Fenced(lang) if !lang.is_empty() => {
                        Some(lang.to_string())
                    }
                    _ => None,
                };
            }
            Event::End(TagEnd::CodeBlock) => {
                // pulldown-cmark appends a trailing '\n' to code block text — strip it
                if let Some(last) = current_spans.last_mut()
                    && last.text.ends_with('\n')
                {
                    last.text.truncate(last.text.len() - 1);
                }
                elements.push(ParsedElement::Block(ParsedBlock {
                    spans: std::mem::take(&mut current_spans),
                    heading_level: None,
                    list_style: None,
                    list_indent: 0,
                    list_prefix: String::new(),
                    list_suffix: String::new(),
                    marker: None,
                    is_code_block: true,
                    code_language: code_language.take(),
                    blockquote_depth,
                    line_height: None,
                    non_breakable_lines: None,
                    direction: None,
                    background_color: None,
                }));
                in_block = false;
                is_code_block = false;
            }
            // ─── Table events ───────────────────────────────────────
            Event::Start(Tag::Table(_)) => {
                in_table = true;
                in_table_head = false;
                table_rows.clear();
                current_row_cells.clear();
                current_cell_spans.clear();
                table_header_rows = 0;
            }
            Event::End(TagEnd::Table) => {
                elements.push(ParsedElement::Table(ParsedTable {
                    header_rows: table_header_rows,
                    rows: std::mem::take(&mut table_rows),
                    blockquote_depth,
                }));
                in_table = false;
            }
            Event::Start(Tag::TableHead) => {
                in_table_head = true;
                current_row_cells.clear();
            }
            Event::End(TagEnd::TableHead) => {
                // Flush the header row
                table_rows.push(std::mem::take(&mut current_row_cells));
                table_header_rows += 1;
                in_table_head = false;
            }
            Event::Start(Tag::TableRow) => {
                current_row_cells.clear();
            }
            Event::End(TagEnd::TableRow) if !in_table_head => {
                // Body rows only — header row is flushed in End(TableHead)
                table_rows.push(std::mem::take(&mut current_row_cells));
            }
            Event::Start(Tag::TableCell) => {
                current_cell_spans.clear();
            }
            Event::End(TagEnd::TableCell) => {
                current_row_cells.push(ParsedTableCell {
                    spans: std::mem::take(&mut current_cell_spans),
                });
            }
            // ─── Inline formatting ──────────────────────────────────
            Event::Start(Tag::Emphasis) => {
                italic = true;
            }
            Event::End(TagEnd::Emphasis) => {
                italic = false;
            }
            Event::Start(Tag::Strong) => {
                bold = true;
            }
            Event::End(TagEnd::Strong) => {
                bold = false;
            }
            Event::Start(Tag::Strikethrough) => {
                strikeout = true;
            }
            Event::End(TagEnd::Strikethrough) => {
                strikeout = false;
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                link_href = Some(dest_url.to_string());
            }
            Event::End(TagEnd::Link) => {
                link_href = None;
            }
            Event::Text(text) => {
                let span = ParsedSpan {
                    text: text.to_string(),
                    bold,
                    italic,
                    underline: false,
                    strikeout,
                    code: is_code_block,
                    superscript: false,
                    subscript: false,
                    link_href: link_href.clone(),
                };
                if in_table {
                    current_cell_spans.push(span);
                } else {
                    if !in_block {
                        in_block = true;
                    }
                    current_spans.push(span);
                }
            }
            Event::Code(text) => {
                let span = ParsedSpan {
                    text: text.to_string(),
                    bold,
                    italic,
                    underline: false,
                    strikeout,
                    code: true,
                    superscript: false,
                    subscript: false,
                    link_href: link_href.clone(),
                };
                if in_table {
                    current_cell_spans.push(span);
                } else {
                    if !in_block {
                        in_block = true;
                    }
                    current_spans.push(span);
                }
            }
            Event::SoftBreak => {
                let span = ParsedSpan {
                    text: " ".to_string(),
                    bold,
                    italic,
                    underline: false,
                    strikeout,
                    code: false,
                    superscript: false,
                    subscript: false,
                    link_href: link_href.clone(),
                };
                if in_table {
                    current_cell_spans.push(span);
                } else {
                    current_spans.push(span);
                }
            }
            Event::HardBreak if !current_spans.is_empty() || in_block => {
                // Finalize current block
                elements.push(ParsedElement::Block(ParsedBlock {
                    spans: std::mem::take(&mut current_spans),
                    heading_level: current_heading.take(),
                    list_style: current_list_style.clone(),
                    list_indent: current_list_indent,
                    list_prefix: String::new(),
                    list_suffix: String::new(),
                    marker: None,
                    is_code_block,
                    code_language: code_language.clone(),
                    blockquote_depth,
                    line_height: None,
                    non_breakable_lines: None,
                    direction: None,
                    background_color: None,
                }));
            }
            Event::Start(Tag::BlockQuote(_)) => {
                blockquote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                blockquote_depth = blockquote_depth.saturating_sub(1);
            }
            _ => {}
        }
    }

    // Flush any remaining content
    if !current_spans.is_empty() {
        elements.push(ParsedElement::Block(ParsedBlock {
            spans: std::mem::take(&mut current_spans),
            heading_level: current_heading,
            list_style: current_list_style,
            list_indent: current_list_indent,
            list_prefix: String::new(),
            list_suffix: String::new(),
            marker: None,
            is_code_block,
            code_language: code_language.take(),
            blockquote_depth,
            line_height: None,
            non_breakable_lines: None,
            direction: None,
            background_color: None,
        }));
    }

    // If no elements were parsed, create a single empty paragraph
    if elements.is_empty() {
        elements.push(ParsedElement::Block(ParsedBlock {
            spans: vec![ParsedSpan {
                text: String::new(),
                ..Default::default()
            }],
            heading_level: None,
            list_style: None,
            list_indent: 0,
            list_prefix: String::new(),
            list_suffix: String::new(),
            marker: None,
            is_code_block: false,
            code_language: None,
            blockquote_depth: 0,
            line_height: None,
            non_breakable_lines: None,
            direction: None,
            background_color: None,
        }));
    }

    elements
}

fn heading_level_to_i64(level: pulldown_cmark::HeadingLevel) -> i64 {
    use pulldown_cmark::HeadingLevel;
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

// ─── HTML parsing ────────────────────────────────────────────────────

use scraper::Node;

/// Parsed CSS block-level styles from an inline `style` attribute.
#[derive(Debug, Clone, Default)]
struct BlockStyles {
    line_height: Option<i64>,
    non_breakable_lines: Option<bool>,
    direction: Option<TextDirection>,
    background_color: Option<String>,
}

/// Parse relevant CSS properties from an inline style string.
/// Handles: line-height, white-space, direction, background-color.
fn parse_block_styles(style: &str) -> BlockStyles {
    let mut result = BlockStyles::default();
    for part in style.split(';') {
        let part = part.trim();
        if let Some((prop, val)) = part.split_once(':') {
            let prop = prop.trim().to_ascii_lowercase();
            let val = val.trim();
            match prop.as_str() {
                "line-height" => {
                    // Try parsing as a plain number (multiplier)
                    if let Ok(v) = val.parse::<f64>() {
                        result.line_height = Some((v * 1000.0) as i64);
                    }
                }
                "white-space" if val == "pre" || val == "nowrap" || val == "pre-wrap" => {
                    result.non_breakable_lines = Some(true);
                }
                "direction" => {
                    if val.eq_ignore_ascii_case("rtl") {
                        result.direction = Some(TextDirection::RightToLeft);
                    } else if val.eq_ignore_ascii_case("ltr") {
                        result.direction = Some(TextDirection::LeftToRight);
                    }
                }
                "background-color" | "background" => {
                    result.background_color = Some(val.to_string());
                }
                _ => {}
            }
        }
    }
    result
}

pub fn parse_html(html: &str) -> Vec<ParsedBlock> {
    ParsedElement::flatten_to_blocks(parse_html_elements(html))
}

pub fn parse_html_elements(html: &str) -> Vec<ParsedElement> {
    use scraper::Html;

    let fragment = Html::parse_fragment(html);
    let mut elements: Vec<ParsedElement> = Vec::new();

    // Walk the DOM tree starting from the root
    let root = fragment.root_element();

    #[derive(Clone, Default)]
    struct FmtState {
        bold: bool,
        italic: bool,
        underline: bool,
        strikeout: bool,
        code: bool,
        link_href: Option<String>,
    }

    const MAX_RECURSION_DEPTH: usize = 256;

    /// Collect inline spans from a `<td>` or `<th>` cell element.
    fn collect_cell_spans(
        node: ego_tree::NodeRef<Node>,
        state: &FmtState,
        spans: &mut Vec<ParsedSpan>,
        depth: usize,
    ) {
        if depth > MAX_RECURSION_DEPTH {
            return;
        }
        for child in node.children() {
            match child.value() {
                Node::Text(text) => {
                    let t = text.text.to_string();
                    if !t.is_empty() {
                        spans.push(ParsedSpan {
                            text: t,
                            bold: state.bold,
                            italic: state.italic,
                            underline: state.underline,
                            strikeout: state.strikeout,
                            code: state.code,
                            superscript: false,
                            subscript: false,
                            link_href: state.link_href.clone(),
                        });
                    }
                }
                Node::Element(el) => {
                    let tag = el.name();
                    let mut new_state = state.clone();
                    match tag {
                        "b" | "strong" => new_state.bold = true,
                        "i" | "em" => new_state.italic = true,
                        "u" | "ins" => new_state.underline = true,
                        "s" | "del" | "strike" => new_state.strikeout = true,
                        "code" => new_state.code = true,
                        "a" => {
                            if let Some(href) = el.attr("href") {
                                new_state.link_href = Some(href.to_string());
                            }
                        }
                        _ => {}
                    }
                    collect_cell_spans(child, &new_state, spans, depth + 1);
                }
                _ => {}
            }
        }
    }

    /// Parse a `<table>` element into a ParsedTable.
    fn parse_table_element(table_node: ego_tree::NodeRef<Node>) -> ParsedTable {
        let mut rows: Vec<Vec<ParsedTableCell>> = Vec::new();
        let mut header_rows: usize = 0;

        fn collect_rows(
            node: ego_tree::NodeRef<Node>,
            rows: &mut Vec<Vec<ParsedTableCell>>,
            header_rows: &mut usize,
            in_thead: bool,
        ) {
            for child in node.children() {
                if let Node::Element(el) = child.value() {
                    match el.name() {
                        "thead" => collect_rows(child, rows, header_rows, true),
                        "tbody" | "tfoot" => collect_rows(child, rows, header_rows, false),
                        "tr" => {
                            let mut cells: Vec<ParsedTableCell> = Vec::new();
                            for td in child.children() {
                                if let Node::Element(td_el) = td.value()
                                    && matches!(td_el.name(), "td" | "th")
                                {
                                    let mut spans = Vec::new();
                                    let state = FmtState::default();
                                    collect_cell_spans(td, &state, &mut spans, 0);
                                    if spans.is_empty() {
                                        spans.push(ParsedSpan::default());
                                    }
                                    cells.push(ParsedTableCell { spans });
                                }
                            }
                            if !cells.is_empty() {
                                rows.push(cells);
                                if in_thead {
                                    *header_rows += 1;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        collect_rows(table_node, &mut rows, &mut header_rows, false);

        // Tables without explicit <thead> but with <th> cells: treat first row as header
        if header_rows == 0 && !rows.is_empty() {
            header_rows = 1;
        }

        ParsedTable {
            header_rows,
            rows,
            // The caller (`walk_node`) sets the real depth — this helper has
            // no visibility into the surrounding blockquote nesting.
            blockquote_depth: 0,
        }
    }

    fn walk_node(
        node: ego_tree::NodeRef<Node>,
        state: &FmtState,
        elements: &mut Vec<ParsedElement>,
        current_list_style: &Option<ListStyle>,
        blockquote_depth: u32,
        list_depth: u32,
        depth: usize,
    ) {
        if depth > MAX_RECURSION_DEPTH {
            return;
        }
        match node.value() {
            Node::Element(el) => {
                let tag = el.name();
                let mut new_state = state.clone();
                let mut new_list_style = current_list_style.clone();
                let mut bq_depth = blockquote_depth;
                let mut new_list_depth = list_depth;

                // Determine if this is a block-level element
                let is_block_tag = matches!(
                    tag,
                    "p" | "div"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "li"
                        | "pre"
                        | "br"
                        | "blockquote"
                        | "body"
                        | "html"
                );

                // Update formatting state
                match tag {
                    "b" | "strong" => new_state.bold = true,
                    "i" | "em" => new_state.italic = true,
                    "u" | "ins" => new_state.underline = true,
                    "s" | "del" | "strike" => new_state.strikeout = true,
                    "code" => new_state.code = true,
                    "a" => {
                        if let Some(href) = el.attr("href") {
                            new_state.link_href = Some(href.to_string());
                        }
                    }
                    "ul" => {
                        new_list_style = Some(ListStyle::Disc);
                        new_list_depth = list_depth + 1;
                    }
                    "ol" => {
                        new_list_style = Some(ListStyle::Decimal);
                        new_list_depth = list_depth + 1;
                    }
                    "blockquote" => {
                        bq_depth += 1;
                    }
                    _ => {}
                }

                // Determine heading level
                let heading_level = match tag {
                    "h1" => Some(1),
                    "h2" => Some(2),
                    "h3" => Some(3),
                    "h4" => Some(4),
                    "h5" => Some(5),
                    "h6" => Some(6),
                    _ => None,
                };

                let is_code_block = tag == "pre";

                // Extract code language from <pre><code class="language-xxx">
                let code_language = if is_code_block {
                    node.children().find_map(|child| {
                        if let Node::Element(cel) = child.value()
                            && cel.name() == "code"
                            && let Some(cls) = cel.attr("class")
                        {
                            return cls
                                .split_whitespace()
                                .find_map(|c| c.strip_prefix("language-"))
                                .map(|l| l.to_string());
                        }
                        None
                    })
                } else {
                    None
                };

                // Extract CSS styles from block-level elements
                let css = if is_block_tag {
                    el.attr("style").map(parse_block_styles).unwrap_or_default()
                } else {
                    BlockStyles::default()
                };

                if tag == "table" {
                    // Parse table structure into a ParsedTable
                    let mut parsed_table = parse_table_element(node);
                    if !parsed_table.rows.is_empty() {
                        parsed_table.blockquote_depth = bq_depth;
                        elements.push(ParsedElement::Table(parsed_table));
                    }
                    return;
                }

                if tag == "br" {
                    // <br> creates a new block
                    elements.push(ParsedElement::Block(ParsedBlock {
                        spans: vec![ParsedSpan {
                            text: String::new(),
                            ..Default::default()
                        }],
                        heading_level: None,
                        list_style: None,
                        list_indent: 0,
                        list_prefix: String::new(),
                        list_suffix: String::new(),
                        marker: None,
                        is_code_block: false,
                        code_language: None,
                        blockquote_depth: bq_depth,
                        line_height: None,
                        non_breakable_lines: None,
                        direction: None,
                        background_color: None,
                    }));
                    return;
                }

                if tag == "blockquote" {
                    // Blockquote is a container — recurse into children with increased depth
                    for child in node.children() {
                        walk_node(
                            child,
                            &new_state,
                            elements,
                            &new_list_style,
                            bq_depth,
                            new_list_depth,
                            depth + 1,
                        );
                    }
                } else if is_block_tag && tag != "br" {
                    // Start collecting spans for a new block.
                    // Use a temporary buffer so that nested block-level
                    // elements (e.g. sub-lists inside <li>) are collected
                    // separately and appended *after* the parent block.
                    let mut spans: Vec<ParsedSpan> = Vec::new();
                    let mut nested_elements: Vec<ParsedElement> = Vec::new();
                    collect_inline_spans(
                        node,
                        &new_state,
                        &mut spans,
                        &new_list_style,
                        &mut nested_elements,
                        bq_depth,
                        new_list_depth,
                        depth + 1,
                    );

                    let list_style_for_block = if tag == "li" {
                        new_list_style.clone()
                    } else {
                        None
                    };

                    let list_indent_for_block = if tag == "li" {
                        new_list_depth.saturating_sub(1)
                    } else {
                        0
                    };

                    if !spans.is_empty() || heading_level.is_some() {
                        elements.push(ParsedElement::Block(ParsedBlock {
                            spans,
                            heading_level,
                            list_style: list_style_for_block,
                            list_indent: list_indent_for_block,
                            list_prefix: String::new(),
                            list_suffix: String::new(),
                            marker: None,
                            is_code_block,
                            code_language,
                            blockquote_depth: bq_depth,
                            line_height: css.line_height,
                            non_breakable_lines: css.non_breakable_lines,
                            direction: css.direction,
                            background_color: css.background_color,
                        }));
                    }
                    // Append nested block elements after the parent block
                    elements.append(&mut nested_elements);
                } else if matches!(tag, "ul" | "ol" | "thead" | "tbody" | "tr") {
                    // Container elements: recurse into children
                    for child in node.children() {
                        walk_node(
                            child,
                            &new_state,
                            elements,
                            &new_list_style,
                            bq_depth,
                            new_list_depth,
                            depth + 1,
                        );
                    }
                } else {
                    // Inline element or unknown: recurse
                    for child in node.children() {
                        walk_node(
                            child,
                            &new_state,
                            elements,
                            current_list_style,
                            bq_depth,
                            list_depth,
                            depth + 1,
                        );
                    }
                }
            }
            Node::Text(text) => {
                let t = text.text.to_string();
                let trimmed = t.trim();
                if !trimmed.is_empty() {
                    // Bare text not in a block — create a paragraph
                    elements.push(ParsedElement::Block(ParsedBlock {
                        spans: vec![ParsedSpan {
                            text: trimmed.to_string(),
                            bold: state.bold,
                            italic: state.italic,
                            underline: state.underline,
                            strikeout: state.strikeout,
                            code: state.code,
                            superscript: false,
                            subscript: false,
                            link_href: state.link_href.clone(),
                        }],
                        heading_level: None,
                        list_style: None,
                        list_indent: 0,
                        list_prefix: String::new(),
                        list_suffix: String::new(),
                        marker: None,
                        is_code_block: false,
                        code_language: None,
                        blockquote_depth,
                        line_height: None,
                        non_breakable_lines: None,
                        direction: None,
                        background_color: None,
                    }));
                }
            }
            _ => {
                // Document, Comment, etc. — recurse children
                for child in node.children() {
                    walk_node(
                        child,
                        state,
                        elements,
                        current_list_style,
                        blockquote_depth,
                        list_depth,
                        depth + 1,
                    );
                }
            }
        }
    }

    /// Collect inline spans from a block-level element's children.
    /// If a nested block-level element is encountered, it is flushed as a
    /// separate block.
    #[allow(clippy::too_many_arguments)]
    fn collect_inline_spans(
        node: ego_tree::NodeRef<Node>,
        state: &FmtState,
        spans: &mut Vec<ParsedSpan>,
        current_list_style: &Option<ListStyle>,
        elements: &mut Vec<ParsedElement>,
        blockquote_depth: u32,
        list_depth: u32,
        depth: usize,
    ) {
        if depth > MAX_RECURSION_DEPTH {
            return;
        }
        for child in node.children() {
            match child.value() {
                Node::Text(text) => {
                    let t = text.text.to_string();
                    if !t.is_empty() {
                        spans.push(ParsedSpan {
                            text: t,
                            bold: state.bold,
                            italic: state.italic,
                            underline: state.underline,
                            strikeout: state.strikeout,
                            code: state.code,
                            superscript: false,
                            subscript: false,
                            link_href: state.link_href.clone(),
                        });
                    }
                }
                Node::Element(el) => {
                    let tag = el.name();
                    let mut new_state = state.clone();

                    match tag {
                        "b" | "strong" => new_state.bold = true,
                        "i" | "em" => new_state.italic = true,
                        "u" | "ins" => new_state.underline = true,
                        "s" | "del" | "strike" => new_state.strikeout = true,
                        "code" => new_state.code = true,
                        "a" => {
                            if let Some(href) = el.attr("href") {
                                new_state.link_href = Some(href.to_string());
                            }
                        }
                        _ => {}
                    }

                    // Check for nested block elements
                    let nested_block = matches!(
                        tag,
                        "p" | "div"
                            | "h1"
                            | "h2"
                            | "h3"
                            | "h4"
                            | "h5"
                            | "h6"
                            | "li"
                            | "pre"
                            | "blockquote"
                            | "ul"
                            | "ol"
                    );

                    if tag == "br" {
                        // br within a block: treat as splitting into new block
                        // For simplicity, just add a newline to current span
                        spans.push(ParsedSpan {
                            text: String::new(),
                            ..Default::default()
                        });
                    } else if nested_block || tag == "table" {
                        // Flush as separate element
                        walk_node(
                            child,
                            &new_state,
                            elements,
                            current_list_style,
                            blockquote_depth,
                            list_depth,
                            depth + 1,
                        );
                    } else {
                        // Inline element: recurse
                        collect_inline_spans(
                            child,
                            &new_state,
                            spans,
                            current_list_style,
                            elements,
                            blockquote_depth,
                            list_depth,
                            depth + 1,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    let initial_state = FmtState::default();
    // Treat the root element as a block-level container so that
    // top-level inline elements (e.g. `<b>Bold</b> <em>Italic</em>`)
    // are grouped into a single block instead of becoming separate blocks.
    let mut root_spans: Vec<ParsedSpan> = Vec::new();
    collect_inline_spans(
        *root,
        &initial_state,
        &mut root_spans,
        &None,
        &mut elements,
        0,
        0,
        0,
    );
    if !root_spans.is_empty() {
        elements.push(ParsedElement::Block(ParsedBlock {
            spans: root_spans,
            heading_level: None,
            list_style: None,
            list_indent: 0,
            list_prefix: String::new(),
            list_suffix: String::new(),
            marker: None,
            is_code_block: false,
            code_language: None,
            blockquote_depth: 0,
            line_height: None,
            non_breakable_lines: None,
            direction: None,
            background_color: None,
        }));
    }

    // If no elements were parsed, create a single empty paragraph
    if elements.is_empty() {
        elements.push(ParsedElement::Block(ParsedBlock {
            spans: vec![ParsedSpan {
                text: String::new(),
                ..Default::default()
            }],
            heading_level: None,
            list_style: None,
            list_indent: 0,
            list_prefix: String::new(),
            list_suffix: String::new(),
            marker: None,
            is_code_block: false,
            code_language: None,
            blockquote_depth: 0,
            line_height: None,
            non_breakable_lines: None,
            direction: None,
            background_color: None,
        }));
    }

    elements
}

/// Convert a `ParsedSpan` (parser output) into the `CharacterFormat` used by
/// `FormatRun`. `is_code_block` forces `monospace` as the font family for
/// every span inside a code block.
pub fn character_format_from_span(
    span: &ParsedSpan,
    is_code_block: bool,
) -> crate::format_runs::CharacterFormat {
    use crate::entities::CharVerticalAlignment;
    crate::format_runs::CharacterFormat {
        font_bold: if span.bold { Some(true) } else { None },
        font_italic: if span.italic { Some(true) } else { None },
        font_underline: if span.underline { Some(true) } else { None },
        font_strikeout: if span.strikeout { Some(true) } else { None },
        font_family: if span.code || is_code_block {
            Some("monospace".to_string())
        } else {
            None
        },
        anchor_href: span.link_href.clone(),
        is_anchor: if span.link_href.is_some() {
            Some(true)
        } else {
            None
        },
        vertical_alignment: if span.superscript {
            Some(CharVerticalAlignment::SuperScript)
        } else if span.subscript {
            Some(CharVerticalAlignment::SubScript)
        } else {
            None
        },
        ..Default::default()
    }
}

/// Translate a slice of parsed spans into `(plain_text, format_runs)`.
///
/// One non-default span yields one `FormatRun`; spans with empty
/// `CharacterFormat` (no decoration, no link, no code) emit no run, since an
/// absent run means "inherit default formatting" in the new model. Adjacent
/// runs with identical formats are coalesced via `coalesce_in_place` so the
/// resulting vector satisfies `debug_assert_well_formed`.
///
/// Returns the concatenated `plain_text` of all spans and a sorted,
/// non-overlapping, coalesced `Vec<FormatRun>`. Both safe to feed straight
/// into the store under the dual-write bridge.
pub fn format_runs_from_spans(
    spans: &[ParsedSpan],
    is_code_block: bool,
) -> (String, Vec<crate::format_runs::FormatRun>) {
    use crate::format_runs::{CharacterFormat, FormatRun, coalesce_in_place};

    let mut plain_text = String::new();
    let mut runs: Vec<FormatRun> = Vec::new();
    let default = CharacterFormat::default();

    for span in spans {
        let byte_start = plain_text.len() as u32;
        plain_text.push_str(&span.text);
        let byte_end = plain_text.len() as u32;
        if byte_start == byte_end {
            continue;
        }
        let format = character_format_from_span(span, is_code_block);
        if format == default {
            continue;
        }
        runs.push(FormatRun {
            byte_start,
            byte_end,
            format,
        });
    }
    coalesce_in_place(&mut runs);
    (plain_text, runs)
}

// ─── Djot parsing ────────────────────────────────────────────────────

/// Map a jotdown unordered/task bullet marker to a model `ListStyle`.
///
/// The mapping is a stable bijection (`-`↔Disc, `*`↔Circle, `+`↔Square) so the
/// djot exporter can recover the exact bullet character for a lossless
/// round-trip.
fn djot_bullet_style(b: jotdown::ListBulletType) -> ListStyle {
    use jotdown::ListBulletType as B;
    match b {
        B::Dash => ListStyle::Disc,
        B::Star => ListStyle::Circle,
        B::Plus => ListStyle::Square,
    }
}

/// Map a jotdown ordered-list numbering scheme to a model `ListStyle`.
fn djot_ordered_style(n: jotdown::OrderedListNumbering) -> ListStyle {
    use jotdown::OrderedListNumbering as N;
    match n {
        N::Decimal => ListStyle::Decimal,
        N::AlphaLower => ListStyle::LowerAlpha,
        N::AlphaUpper => ListStyle::UpperAlpha,
        N::RomanLower => ListStyle::LowerRoman,
        N::RomanUpper => ListStyle::UpperRoman,
    }
}

/// Map a jotdown ordered-list delimiter to the `(prefix, suffix)` affixes
/// stored on the `List` entity (`1.` → `("", ".")`, `1)` → `("", ")")`,
/// `(1)` → `("(", ")")`).
fn djot_ordered_affixes(style: jotdown::OrderedListStyle) -> (String, String) {
    use jotdown::OrderedListStyle as S;
    match style {
        S::Period => (String::new(), ".".to_string()),
        S::Paren => (String::new(), ")".to_string()),
        S::ParenParen => ("(".to_string(), ")".to_string()),
    }
}

/// Push a finished block into `elements` (only the block-level fields djot uses
/// are set; CSS-derived fields stay `None`).
#[allow(clippy::too_many_arguments)]
fn djot_push_block(
    elements: &mut Vec<ParsedElement>,
    spans: Vec<ParsedSpan>,
    heading_level: Option<i64>,
    list_style: Option<ListStyle>,
    list_indent: u32,
    list_prefix: String,
    list_suffix: String,
    marker: Option<MarkerType>,
    is_code_block: bool,
    code_language: Option<String>,
    blockquote_depth: u32,
) {
    elements.push(ParsedElement::Block(ParsedBlock {
        spans,
        heading_level,
        list_style,
        list_indent,
        list_prefix,
        list_suffix,
        marker,
        is_code_block,
        code_language,
        blockquote_depth,
        line_height: None,
        non_breakable_lines: None,
        direction: None,
        background_color: None,
    }));
}

/// Parse djot source into the shared [`ParsedElement`] intermediate, mirroring
/// [`parse_markdown`]. Uses the [`jotdown`] pull parser.
///
/// Constructs the document model cannot represent are dropped, and their text
/// content is discarded so it never leaks into the document: footnotes, math,
/// fenced divs, raw blocks/inline, thematic breaks, description lists,
/// captions, symbols, link-reference definitions, and highlight/`mark`. Inline
/// images keep their alt text as plain text (the image itself is not modelled),
/// matching the Markdown importer. Smart-punctuation events are normalised to
/// their canonical Unicode characters so the model→djot→model round-trip is a
/// fixpoint.
///
/// Known model limitations (normalised, not preserved on round-trip):
/// ordered-list start number, table column alignment, and list tight/loose.
pub fn parse_djot(djot: &str) -> Vec<ParsedElement> {
    use jotdown::{Container as C, Event as E, ListKind, Parser};

    let mut elements: Vec<ParsedElement> = Vec::new();
    let mut current_spans: Vec<ParsedSpan> = Vec::new();
    let mut current_heading: Option<i64> = None;
    let mut is_code_block = false;
    let mut code_language: Option<String> = None;
    let mut blockquote_depth: u32 = 0;

    // Inline formatting state.
    let mut bold = false;
    let mut italic = false;
    let mut underline = false;
    let mut strikeout = false;
    let mut code = false;
    let mut superscript = false;
    let mut subscript = false;
    let mut link_href: Option<String> = None;

    // List nesting: each entry is (style, prefix, suffix); depth = indent + 1.
    let mut list_stack: Vec<(ListStyle, String, String)> = Vec::new();
    // Context applied to the next flushed block while inside a list item.
    let mut cur_list_style: Option<ListStyle> = None;
    let mut cur_list_prefix = String::new();
    let mut cur_list_suffix = String::new();
    let mut cur_list_indent: u32 = 0;
    let mut cur_marker: Option<MarkerType> = None;

    // Table accumulation.
    let mut in_table_cell = false;
    let mut table_rows: Vec<Vec<ParsedTableCell>> = Vec::new();
    let mut current_row: Vec<ParsedTableCell> = Vec::new();
    let mut current_cell_spans: Vec<ParsedSpan> = Vec::new();
    let mut table_header_rows: usize = 0;
    let mut row_is_head = false;

    // Subtree-skip depth for unrepresentable containers (their entire content
    // is dropped). Incremented on the dropped container's `Start` and on every
    // nested `Start`; decremented on every `End`.
    let mut skip_depth: u32 = 0;

    // Push one inline span carrying the current formatting state into the
    // active sink (table cell or block). A macro (not a closure) to avoid
    // borrowing `current_spans`/`current_cell_spans` across the formatting
    // state reads.
    macro_rules! push_text {
        ($t:expr) => {{
            let sp = ParsedSpan {
                text: ($t).to_string(),
                bold,
                italic,
                underline,
                strikeout,
                code,
                superscript,
                subscript,
                link_href: link_href.clone(),
            };
            if in_table_cell {
                current_cell_spans.push(sp);
            } else {
                current_spans.push(sp);
            }
        }};
    }

    // Enter a list item, flushing any unterminated inline content first and
    // capturing the list context + task marker for the item's block.
    macro_rules! enter_item {
        ($marker:expr) => {{
            if !current_spans.is_empty() {
                djot_push_block(
                    &mut elements,
                    std::mem::take(&mut current_spans),
                    None,
                    cur_list_style.clone(),
                    cur_list_indent,
                    cur_list_prefix.clone(),
                    cur_list_suffix.clone(),
                    cur_marker.clone(),
                    false,
                    None,
                    blockquote_depth,
                );
            }
            let (style, prefix, suffix) = list_stack
                .last()
                .cloned()
                .unwrap_or((ListStyle::Disc, String::new(), String::new()));
            cur_list_style = Some(style);
            cur_list_prefix = prefix;
            cur_list_suffix = suffix;
            cur_list_indent = list_stack.len().saturating_sub(1) as u32;
            cur_marker = $marker;
        }};
    }

    for event in Parser::new(djot) {
        if skip_depth > 0 {
            match event {
                E::Start(..) => skip_depth += 1,
                E::End(_) => skip_depth -= 1,
                _ => {}
            }
            continue;
        }

        match event {
            // ── Transparent wrappers (unwrap, keep content) ──
            E::Start(C::Document, _) | E::End(C::Document) => {}
            E::Start(C::Section { .. }, _) | E::End(C::Section { .. }) => {}
            E::Start(C::Div { .. }, _) | E::End(C::Div { .. }) => {}

            // ── Blockquote ──
            E::Start(C::Blockquote, _) => blockquote_depth += 1,
            E::End(C::Blockquote) => blockquote_depth = blockquote_depth.saturating_sub(1),

            // ── Lists ──
            E::Start(C::List { kind, .. }, _) => {
                let (style, prefix, suffix) = match kind {
                    ListKind::Unordered(b) | ListKind::Task(b) => {
                        (djot_bullet_style(b), String::new(), String::new())
                    }
                    ListKind::Ordered {
                        numbering, style, ..
                    } => {
                        let (p, s) = djot_ordered_affixes(style);
                        (djot_ordered_style(numbering), p, s)
                    }
                };
                list_stack.push((style, prefix, suffix));
            }
            E::End(C::List { .. }) => {
                list_stack.pop();
                cur_list_style = None;
                cur_marker = None;
            }
            E::Start(C::ListItem, _) => enter_item!(None),
            E::Start(C::TaskListItem { checked }, _) => enter_item!(Some(if checked {
                MarkerType::Checked
            } else {
                MarkerType::Unchecked
            })),
            E::End(C::ListItem) | E::End(C::TaskListItem { .. }) => {
                // Tight item without a wrapping paragraph (defensive flush).
                if !current_spans.is_empty() {
                    djot_push_block(
                        &mut elements,
                        std::mem::take(&mut current_spans),
                        None,
                        cur_list_style.clone(),
                        cur_list_indent,
                        cur_list_prefix.clone(),
                        cur_list_suffix.clone(),
                        cur_marker.clone(),
                        false,
                        None,
                        blockquote_depth,
                    );
                }
                cur_list_style = None;
                cur_marker = None;
            }

            // ── Headings, paragraphs, code blocks ──
            E::Start(C::Heading { level, .. }, _) => current_heading = Some(level as i64),
            E::End(C::Heading { .. }) => {
                djot_push_block(
                    &mut elements,
                    std::mem::take(&mut current_spans),
                    current_heading.take(),
                    None,
                    0,
                    String::new(),
                    String::new(),
                    None,
                    false,
                    None,
                    blockquote_depth,
                );
            }
            E::Start(C::Paragraph, _) => current_heading = None,
            E::End(C::Paragraph) => {
                if !current_spans.is_empty() {
                    djot_push_block(
                        &mut elements,
                        std::mem::take(&mut current_spans),
                        None,
                        cur_list_style.clone(),
                        cur_list_indent,
                        cur_list_prefix.clone(),
                        cur_list_suffix.clone(),
                        cur_marker.clone(),
                        false,
                        None,
                        blockquote_depth,
                    );
                }
                cur_list_style = None;
                cur_marker = None;
            }
            E::Start(C::CodeBlock { language }, _) => {
                is_code_block = true;
                code_language = if language.is_empty() {
                    None
                } else {
                    Some(language.to_string())
                };
            }
            E::End(C::CodeBlock { .. }) => {
                // Strip the single trailing newline jotdown appends.
                if let Some(last) = current_spans.last_mut()
                    && last.text.ends_with('\n')
                {
                    last.text.pop();
                }
                djot_push_block(
                    &mut elements,
                    std::mem::take(&mut current_spans),
                    None,
                    None,
                    0,
                    String::new(),
                    String::new(),
                    None,
                    true,
                    code_language.take(),
                    blockquote_depth,
                );
                is_code_block = false;
            }

            // ── Tables ──
            E::Start(C::Table, _) => {
                table_rows.clear();
                current_row.clear();
                current_cell_spans.clear();
                table_header_rows = 0;
            }
            E::End(C::Table) => {
                elements.push(ParsedElement::Table(ParsedTable {
                    header_rows: table_header_rows,
                    rows: std::mem::take(&mut table_rows),
                    blockquote_depth,
                }));
            }
            E::Start(C::TableRow { head }, _) => {
                row_is_head = head;
                current_row.clear();
            }
            E::End(C::TableRow { .. }) => {
                if row_is_head {
                    table_header_rows += 1;
                }
                table_rows.push(std::mem::take(&mut current_row));
            }
            E::Start(C::TableCell { .. }, _) => {
                in_table_cell = true;
                current_cell_spans.clear();
            }
            E::End(C::TableCell { .. }) => {
                in_table_cell = false;
                current_row.push(ParsedTableCell {
                    spans: std::mem::take(&mut current_cell_spans),
                });
            }

            // ── Inline formatting ──
            E::Start(C::Strong, _) => bold = true,
            E::End(C::Strong) => bold = false,
            E::Start(C::Emphasis, _) => italic = true,
            E::End(C::Emphasis) => italic = false,
            E::Start(C::Verbatim, _) => code = true,
            E::End(C::Verbatim) => code = false,
            E::Start(C::Superscript, _) => superscript = true,
            E::End(C::Superscript) => superscript = false,
            E::Start(C::Subscript, _) => subscript = true,
            E::End(C::Subscript) => subscript = false,
            E::Start(C::Insert, _) => underline = true,
            E::End(C::Insert) => underline = false,
            E::Start(C::Delete, _) => strikeout = true,
            E::End(C::Delete) => strikeout = false,
            // Highlight/mark and bare spans have no model field — keep the text.
            E::Start(C::Mark, _) | E::End(C::Mark) => {}
            E::Start(C::Span, _) | E::End(C::Span) => {}
            E::Start(C::Link(dst, _), _) => link_href = Some(dst.to_string()),
            E::End(C::Link(..)) => link_href = None,
            // Inline images: keep alt text as plain text (image not modelled).
            E::Start(C::Image(..), _) | E::End(C::Image(..)) => {}

            // ── Unrepresentable containers: drop the entire subtree ──
            E::Start(
                C::Footnote { .. }
                | C::Math { .. }
                | C::RawBlock { .. }
                | C::RawInline { .. }
                | C::DescriptionList
                | C::DescriptionDetails
                | C::DescriptionTerm
                | C::Caption
                | C::LinkDefinition { .. },
                _,
            ) => skip_depth = 1,

            // ── Text + atoms ──
            E::Str(s) => push_text!(s.as_ref()),
            E::Softbreak => push_text!(" "),
            E::LeftSingleQuote => push_text!("\u{2018}"),
            E::RightSingleQuote => push_text!("\u{2019}"),
            E::LeftDoubleQuote => push_text!("\u{201C}"),
            E::RightDoubleQuote => push_text!("\u{201D}"),
            E::Ellipsis => push_text!("\u{2026}"),
            E::EnDash => push_text!("\u{2013}"),
            E::EmDash => push_text!("\u{2014}"),
            E::NonBreakingSpace => push_text!("\u{00A0}"),
            E::Hardbreak => {
                if in_table_cell {
                    push_text!(" ");
                } else if !current_spans.is_empty() {
                    // Mirrors the Markdown importer: a hard break splits the
                    // paragraph into a new block.
                    djot_push_block(
                        &mut elements,
                        std::mem::take(&mut current_spans),
                        None,
                        cur_list_style.clone(),
                        cur_list_indent,
                        cur_list_prefix.clone(),
                        cur_list_suffix.clone(),
                        cur_marker.clone(),
                        is_code_block,
                        code_language.clone(),
                        blockquote_depth,
                    );
                }
            }
            // Symbols, footnote refs, escapes, blanklines, thematic breaks and
            // dangling block attributes carry no representable content.
            E::Symbol(_) | E::FootnoteReference(_) => {}
            E::Escape | E::Blankline => {}
            E::ThematicBreak(_) | E::Attributes(_) => {}

            // Ends of dropped containers (never reached at skip_depth 0) and any
            // future variants.
            _ => {}
        }
    }

    // Flush any trailing inline content (defensive — Document End closes blocks).
    if !current_spans.is_empty() {
        djot_push_block(
            &mut elements,
            std::mem::take(&mut current_spans),
            current_heading.take(),
            cur_list_style.clone(),
            cur_list_indent,
            cur_list_prefix.clone(),
            cur_list_suffix.clone(),
            cur_marker.clone(),
            is_code_block,
            code_language.take(),
            blockquote_depth,
        );
    }

    // An empty document still yields a single empty paragraph (matches
    // `parse_markdown`).
    if elements.is_empty() {
        djot_push_block(
            &mut elements,
            vec![ParsedSpan {
                text: String::new(),
                ..Default::default()
            }],
            None,
            None,
            0,
            String::new(),
            String::new(),
            None,
            false,
            None,
            0,
        );
    }

    elements
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: flatten parse_markdown output to blocks for tests that don't care about tables.
    fn parse_markdown_blocks(md: &str) -> Vec<ParsedBlock> {
        ParsedElement::flatten_to_blocks(parse_markdown(md))
    }

    #[test]
    fn test_parse_markdown_simple_paragraph() {
        let blocks = parse_markdown_blocks("Hello **world**");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].spans.len() >= 2);
        // "Hello " is plain, "world" is bold
        let plain_span = blocks[0]
            .spans
            .iter()
            .find(|s| s.text.contains("Hello"))
            .unwrap();
        assert!(!plain_span.bold);
        let bold_span = blocks[0].spans.iter().find(|s| s.text == "world").unwrap();
        assert!(bold_span.bold);
    }

    #[test]
    fn test_parse_markdown_heading() {
        let blocks = parse_markdown_blocks("# Title");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].heading_level, Some(1));
        assert_eq!(blocks[0].spans[0].text, "Title");
    }

    #[test]
    fn test_parse_markdown_list() {
        let blocks = parse_markdown_blocks("- item1\n- item2");
        assert!(blocks.len() >= 2);
        assert_eq!(blocks[0].list_style, Some(ListStyle::Disc));
        assert_eq!(blocks[1].list_style, Some(ListStyle::Disc));
    }

    /// Helper: extract (is_table, blockquote_depth) per element for nesting assertions.
    fn element_depths(elements: &[ParsedElement]) -> Vec<(bool, u32)> {
        elements
            .iter()
            .map(|e| match e {
                ParsedElement::Block(b) => (false, b.blockquote_depth),
                ParsedElement::Table(t) => (true, t.blockquote_depth),
            })
            .collect()
    }

    #[test]
    fn test_parse_markdown_table_in_blockquote_records_depth() {
        let elements = parse_markdown("> | a | b |\n> |---|---|\n> | c | d |");
        assert_eq!(element_depths(&elements), vec![(true, 1)]);
    }

    #[test]
    fn test_parse_markdown_text_then_table_in_blockquote() {
        let elements = parse_markdown("> Para\n>\n> | a | b |\n> |---|---|\n> | c | d |");
        assert_eq!(element_depths(&elements), vec![(false, 1), (true, 1)]);
    }

    #[test]
    fn test_parse_markdown_table_after_blockquote_closes() {
        let elements = parse_markdown("> Para\n\n| a | b |\n|---|---|\n| c | d |");
        assert_eq!(element_depths(&elements), vec![(false, 1), (true, 0)]);
    }

    #[test]
    fn test_parse_markdown_table_in_nested_blockquote() {
        let elements = parse_markdown(">> | a | b |\n>> |---|---|\n>> | c | d |");
        assert_eq!(element_depths(&elements), vec![(true, 2)]);
    }

    #[test]
    fn test_parse_markdown_list_in_blockquote_records_depth() {
        let elements = parse_markdown("> - item1\n> - item2");
        let depths = element_depths(&elements);
        assert_eq!(depths, vec![(false, 1), (false, 1)]);
        for e in &elements {
            if let ParsedElement::Block(b) = e {
                assert_eq!(b.list_style, Some(ListStyle::Disc));
            }
        }
    }

    #[test]
    fn test_parse_html_table_in_blockquote_records_depth() {
        let elements = parse_html_elements(
            "<blockquote><table><tr><th>A</th></tr><tr><td>x</td></tr></table></blockquote>",
        );
        assert_eq!(element_depths(&elements), vec![(true, 1)]);
    }

    #[test]
    fn test_parse_html_table_after_blockquote() {
        let elements = parse_html_elements(
            "<blockquote><p>Para</p></blockquote><table><tr><td>X</td></tr></table>",
        );
        let depths = element_depths(&elements);
        // The blockquote paragraph carries depth 1; the table is outside (depth 0).
        assert!(depths.contains(&(false, 1)), "depths: {depths:?}");
        assert!(depths.contains(&(true, 0)), "depths: {depths:?}");
    }

    #[test]
    fn test_flatten_to_blocks_propagates_blockquote_depth() {
        let elements = parse_markdown("> | a | b |\n> |---|---|\n> | c | d |");
        let blocks = ParsedElement::flatten_to_blocks(elements);
        assert!(!blocks.is_empty());
        for b in &blocks {
            assert_eq!(b.blockquote_depth, 1);
        }
    }

    #[test]
    fn test_parse_html_simple() {
        let blocks = parse_html("<p>Hello <b>world</b></p>");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].spans.len() >= 2);
        let bold_span = blocks[0].spans.iter().find(|s| s.text == "world").unwrap();
        assert!(bold_span.bold);
    }

    #[test]
    fn test_parse_html_multiple_paragraphs() {
        let blocks = parse_html("<p>A</p><p>B</p>");
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn test_parse_html_heading() {
        let blocks = parse_html("<h2>Subtitle</h2>");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].heading_level, Some(2));
    }

    #[test]
    fn test_parse_html_list() {
        let blocks = parse_html("<ul><li>one</li><li>two</li></ul>");
        assert!(blocks.len() >= 2);
        assert_eq!(blocks[0].list_style, Some(ListStyle::Disc));
    }

    #[test]
    fn test_parse_markdown_code_block() {
        let blocks = parse_markdown_blocks("```\nfn main() {}\n```");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].is_code_block);
        assert!(blocks[0].spans[0].code);
        // pulldown-cmark appends a trailing \n to code block text — verify it's stripped
        let text: String = blocks[0].spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            text, "fn main() {}",
            "code block text should not have trailing newline"
        );
    }

    #[test]
    fn test_parse_markdown_nested_formatting() {
        let blocks = parse_markdown_blocks("***bold italic***");
        assert_eq!(blocks.len(), 1);
        let span = &blocks[0].spans[0];
        assert!(span.bold);
        assert!(span.italic);
    }

    #[test]
    fn test_parse_markdown_link() {
        let blocks = parse_markdown_blocks("[click](http://example.com)");
        assert_eq!(blocks.len(), 1);
        let span = &blocks[0].spans[0];
        assert_eq!(span.text, "click");
        assert_eq!(span.link_href, Some("http://example.com".to_string()));
    }

    #[test]
    fn test_parse_markdown_empty() {
        let blocks = parse_markdown_blocks("");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].spans[0].text.is_empty());
    }

    #[test]
    fn test_parse_html_empty() {
        let blocks = parse_html("");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].spans[0].text.is_empty());
    }

    #[test]
    fn test_parse_html_nested_formatting() {
        let blocks = parse_html("<p><b><i>bold italic</i></b></p>");
        assert_eq!(blocks.len(), 1);
        let span = &blocks[0].spans[0];
        assert!(span.bold);
        assert!(span.italic);
    }

    #[test]
    fn test_parse_html_link() {
        let blocks = parse_html("<p><a href=\"http://example.com\">click</a></p>");
        assert_eq!(blocks.len(), 1);
        let span = &blocks[0].spans[0];
        assert_eq!(span.text, "click");
        assert_eq!(span.link_href, Some("http://example.com".to_string()));
    }

    #[test]
    fn test_parse_html_ordered_list() {
        let blocks = parse_html("<ol><li>first</li><li>second</li></ol>");
        assert!(blocks.len() >= 2);
        assert_eq!(blocks[0].list_style, Some(ListStyle::Decimal));
    }

    #[test]
    fn test_parse_markdown_ordered_list() {
        let blocks = parse_markdown_blocks("1. first\n2. second");
        assert!(blocks.len() >= 2);
        assert_eq!(blocks[0].list_style, Some(ListStyle::Decimal));
    }

    #[test]
    fn test_parse_html_blockquote_nested() {
        let blocks = parse_html("<p>before</p><blockquote>quoted</blockquote><p>after</p>");
        assert!(blocks.len() >= 3);
    }

    #[test]
    fn test_parse_block_styles_line_height() {
        let styles = parse_block_styles("line-height: 1.5");
        assert_eq!(styles.line_height, Some(1500));
    }

    #[test]
    fn test_parse_block_styles_direction_rtl() {
        let styles = parse_block_styles("direction: rtl");
        assert_eq!(styles.direction, Some(TextDirection::RightToLeft));
    }

    #[test]
    fn test_parse_block_styles_background_color() {
        let styles = parse_block_styles("background-color: #ff0000");
        assert_eq!(styles.background_color, Some("#ff0000".to_string()));
    }

    #[test]
    fn test_parse_block_styles_white_space_pre() {
        let styles = parse_block_styles("white-space: pre");
        assert_eq!(styles.non_breakable_lines, Some(true));
    }

    #[test]
    fn test_parse_block_styles_multiple() {
        let styles = parse_block_styles("line-height: 2.0; direction: rtl; background-color: blue");
        assert_eq!(styles.line_height, Some(2000));
        assert_eq!(styles.direction, Some(TextDirection::RightToLeft));
        assert_eq!(styles.background_color, Some("blue".to_string()));
    }

    #[test]
    fn test_parse_html_block_styles_extracted() {
        let blocks = parse_html(
            r#"<p style="line-height: 1.5; direction: rtl; background-color: #ccc">text</p>"#,
        );
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].line_height, Some(1500));
        assert_eq!(blocks[0].direction, Some(TextDirection::RightToLeft));
        assert_eq!(blocks[0].background_color, Some("#ccc".to_string()));
    }

    #[test]
    fn test_parse_html_white_space_pre() {
        let blocks = parse_html(r#"<p style="white-space: pre">code</p>"#);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].non_breakable_lines, Some(true));
    }

    #[test]
    fn test_parse_html_no_styles_returns_none() {
        let blocks = parse_html("<p>plain</p>");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].line_height, None);
        assert_eq!(blocks[0].direction, None);
        assert_eq!(blocks[0].background_color, None);
        assert_eq!(blocks[0].non_breakable_lines, None);
    }

    #[test]
    fn test_parse_markdown_nested_list_indent() {
        let md = "- top\n  - nested\n    - deep";
        let blocks = parse_markdown_blocks(md);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].list_style, Some(ListStyle::Disc));
        assert_eq!(blocks[0].list_indent, 0);
        assert_eq!(blocks[1].list_style, Some(ListStyle::Disc));
        assert_eq!(blocks[1].list_indent, 1);
        assert_eq!(blocks[2].list_style, Some(ListStyle::Disc));
        assert_eq!(blocks[2].list_indent, 2);
    }

    #[test]
    fn test_parse_markdown_nested_ordered_list_indent() {
        let md = "1. first\n   1. nested\n   2. nested2";
        let blocks = parse_markdown_blocks(md);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].list_indent, 0);
        assert_eq!(blocks[1].list_indent, 1);
        assert_eq!(blocks[2].list_indent, 1);
    }

    #[test]
    fn test_parse_html_nested_list_indent() {
        let html = "<ul><li>top</li><ul><li>nested</li></ul></ul>";
        let blocks = parse_html(html);
        assert!(blocks.len() >= 2);
        assert_eq!(blocks[0].list_indent, 0);
        assert_eq!(blocks[1].list_indent, 1);
    }

    #[test]
    fn test_parse_markdown_table() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |";
        let elements = parse_markdown(md);
        assert_eq!(elements.len(), 1);
        match &elements[0] {
            ParsedElement::Table(table) => {
                assert_eq!(table.header_rows, 1);
                assert_eq!(table.rows.len(), 2); // 1 header + 1 body
                // Header row
                assert_eq!(table.rows[0].len(), 2);
                assert_eq!(table.rows[0][0].spans[0].text, "A");
                assert_eq!(table.rows[0][1].spans[0].text, "B");
                // Body row
                assert_eq!(table.rows[1].len(), 2);
                assert_eq!(table.rows[1][0].spans[0].text, "1");
                assert_eq!(table.rows[1][1].spans[0].text, "2");
            }
            _ => panic!("Expected ParsedElement::Table"),
        }
    }

    #[test]
    fn test_parse_markdown_table_with_formatting() {
        let md = "| **bold** | `code` | *italic* |\n|---|---|---|\n| ~~strike~~ | plain | [link](http://x.com) |";
        let elements = parse_markdown(md);
        assert_eq!(elements.len(), 1);
        match &elements[0] {
            ParsedElement::Table(table) => {
                assert_eq!(table.rows.len(), 2);
                // Header: bold cell
                assert!(table.rows[0][0].spans[0].bold);
                // Header: code cell
                assert!(table.rows[0][1].spans[0].code);
                // Header: italic cell
                assert!(table.rows[0][2].spans[0].italic);
                // Body: strikeout cell
                assert!(table.rows[1][0].spans[0].strikeout);
                // Body: link cell
                assert_eq!(
                    table.rows[1][2].spans[0].link_href,
                    Some("http://x.com".to_string())
                );
            }
            _ => panic!("Expected ParsedElement::Table"),
        }
    }

    #[test]
    fn test_parse_markdown_mixed_content_with_table() {
        let md = "Before\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\nAfter";
        let elements = parse_markdown(md);
        assert_eq!(elements.len(), 3);
        assert!(matches!(&elements[0], ParsedElement::Block(_)));
        assert!(matches!(&elements[1], ParsedElement::Table(_)));
        assert!(matches!(&elements[2], ParsedElement::Block(_)));
    }
}

#[cfg(test)]
mod djot_tests {
    use super::*;
    use crate::entities::MarkerType;

    fn blocks(d: &str) -> Vec<ParsedBlock> {
        ParsedElement::flatten_to_blocks(parse_djot(d))
    }

    fn first_span_with<'a>(b: &'a ParsedBlock, pred: impl Fn(&ParsedSpan) -> bool) -> &'a ParsedSpan {
        b.spans.iter().find(|s| pred(s)).expect("span not found")
    }

    #[test]
    fn paragraph_bold_italic() {
        let b = blocks("normal *bold* _italic_");
        assert_eq!(b.len(), 1);
        assert!(first_span_with(&b[0], |s| s.text == "bold").bold);
        assert!(first_span_with(&b[0], |s| s.text == "italic").italic);
    }

    #[test]
    fn heading_levels() {
        assert_eq!(blocks("# H1")[0].heading_level, Some(1));
        assert_eq!(blocks("### H3")[0].heading_level, Some(3));
        assert_eq!(blocks("###### H6")[0].heading_level, Some(6));
    }

    #[test]
    fn unordered_bullet_styles_are_distinct() {
        assert_eq!(blocks("- a")[0].list_style, Some(ListStyle::Disc));
        assert_eq!(blocks("* a")[0].list_style, Some(ListStyle::Circle));
        assert_eq!(blocks("+ a")[0].list_style, Some(ListStyle::Square));
    }

    #[test]
    fn ordered_delimiters() {
        let period = blocks("1. a");
        assert_eq!(period[0].list_style, Some(ListStyle::Decimal));
        assert_eq!(period[0].list_prefix, "");
        assert_eq!(period[0].list_suffix, ".");

        let paren = blocks("1) a");
        assert_eq!(paren[0].list_suffix, ")");
        assert_eq!(paren[0].list_prefix, "");

        let paren_paren = blocks("(1) a");
        assert_eq!(paren_paren[0].list_prefix, "(");
        assert_eq!(paren_paren[0].list_suffix, ")");
    }

    #[test]
    fn task_list_markers() {
        let b = blocks("- [ ] a\n- [x] b");
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].marker, Some(MarkerType::Unchecked));
        assert_eq!(b[1].marker, Some(MarkerType::Checked));
    }

    #[test]
    fn code_block_with_language() {
        let b = blocks("```rust\nfn main() {}\n```");
        assert_eq!(b.len(), 1);
        assert!(b[0].is_code_block);
        assert_eq!(b[0].code_language.as_deref(), Some("rust"));
        let text: String = b[0].spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "fn main() {}");
    }

    #[test]
    fn link_href() {
        let b = blocks("[text](http://example.com)");
        let s = first_span_with(&b[0], |s| s.text == "text");
        assert_eq!(s.link_href.as_deref(), Some("http://example.com"));
    }

    #[test]
    fn superscript_subscript() {
        assert!(first_span_with(&blocks("a^b^")[0], |s| s.text == "b").superscript);
        assert!(first_span_with(&blocks("a~b~")[0], |s| s.text == "b").subscript);
    }

    #[test]
    fn delete_insert_verbatim() {
        assert!(first_span_with(&blocks("{-x-}")[0], |s| s.text == "x").strikeout);
        assert!(first_span_with(&blocks("{+x+}")[0], |s| s.text == "x").underline);
        assert!(first_span_with(&blocks("`x`")[0], |s| s.text == "x").code);
    }

    #[test]
    fn blockquote_depth() {
        let els = parse_djot("> quoted");
        match &els[0] {
            ParsedElement::Block(b) => assert_eq!(b.blockquote_depth, 1),
            _ => panic!("expected block"),
        }
    }

    #[test]
    fn nested_list_indent() {
        // Djot nests a sub-list only when a blank line separates it from the
        // parent item and it is indented to the parent's content column
        // (2 spaces per level). Without the blank line the markers fold into
        // the paragraph as lazy continuation.
        let b = blocks("- a\n\n  - b\n\n    - c");
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].list_indent, 0);
        assert_eq!(b[1].list_indent, 1);
        assert_eq!(b[2].list_indent, 2);
    }

    #[test]
    fn table_parsed_as_table() {
        let els = parse_djot("| a | b |\n|---|---|\n| c | d |");
        assert_eq!(els.len(), 1);
        match &els[0] {
            ParsedElement::Table(t) => {
                assert_eq!(t.header_rows, 1);
                assert_eq!(t.rows.len(), 2);
                assert_eq!(t.rows[0][0].spans[0].text, "a");
                assert_eq!(t.rows[1][1].spans[0].text, "d");
            }
            _ => panic!("expected table"),
        }
    }

    #[test]
    fn smart_punctuation_normalised_to_unicode() {
        let text: String = blocks("a... b---c")[0]
            .spans
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert!(text.contains('\u{2026}'), "ellipsis: {text:?}");
        assert!(text.contains('\u{2014}'), "em dash: {text:?}");
    }

    #[test]
    fn unrepresentable_constructs_dropped_without_leaking_text() {
        // Thematic break between two paragraphs: no extra block, no stray text.
        let b = blocks("para1\n\n---\n\npara2");
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].spans.iter().map(|s| s.text.as_str()).collect::<String>(), "para1");
        assert_eq!(b[1].spans.iter().map(|s| s.text.as_str()).collect::<String>(), "para2");

        // Fenced div is unwrapped: its content survives, the fence does not.
        let d = blocks(":::\ninside\n:::");
        let joined: String = d.iter().flat_map(|b| b.spans.iter()).map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "inside");

        // Inline math content is dropped, surrounding text kept.
        let m = blocks("before $`E=mc^2` after");
        let joined: String = m.iter().flat_map(|b| b.spans.iter()).map(|s| s.text.as_str()).collect();
        assert!(joined.contains("before"), "{joined:?}");
        assert!(joined.contains("after"), "{joined:?}");
        assert!(!joined.contains("E=mc"), "math leaked: {joined:?}");
    }

    #[test]
    fn empty_document_yields_one_empty_block() {
        let b = blocks("");
        assert_eq!(b.len(), 1);
        assert!(b[0].spans.iter().all(|s| s.text.is_empty()));
    }
}
