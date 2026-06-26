//! DocumentFragment — format-agnostic rich text interchange type.

use crate::{InlineContent, ListStyle};
use frontend::common::parser_tools::content_parser::{ParsedElement, ParsedSpan};
use frontend::common::parser_tools::fragment_schema::{
    FragmentBlock, FragmentData, FragmentElement, FragmentTable, FragmentTableCell,
};

/// A piece of rich text that can be inserted into a [`TextDocument`](crate::TextDocument).
///
/// `DocumentFragment` is the clipboard/interchange type. It carries
/// blocks, per-character format runs, image anchors, and structural
/// metadata in a format-agnostic internal representation.
#[derive(Debug, Clone)]
pub struct DocumentFragment {
    data: String,
    plain_text: String,
}

impl DocumentFragment {
    /// Create an empty fragment.
    pub fn new() -> Self {
        Self {
            data: String::new(),
            plain_text: String::new(),
        }
    }

    /// Create a fragment from plain text.
    ///
    /// Builds valid fragment data so the fragment can be inserted via
    /// [`TextCursor::insert_fragment`](crate::TextCursor::insert_fragment).
    pub fn from_plain_text(text: &str) -> Self {
        let blocks: Vec<FragmentBlock> = text
            .split('\n')
            .map(|line| FragmentBlock {
                plain_text: line.to_string(),
                elements: vec![FragmentElement {
                    content: InlineContent::Text(line.to_string()),
                    fmt_font_family: None,
                    fmt_font_point_size: None,
                    fmt_font_weight: None,
                    fmt_font_bold: None,
                    fmt_font_italic: None,
                    fmt_font_underline: None,
                    fmt_font_overline: None,
                    fmt_font_strikeout: None,
                    fmt_letter_spacing: None,
                    fmt_word_spacing: None,
                    fmt_anchor_href: None,
                    fmt_anchor_names: vec![],
                    fmt_is_anchor: None,
                    fmt_tooltip: None,
                    fmt_underline_style: None,
                    fmt_vertical_alignment: None,
                }],
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
                tab_positions: vec![],
                line_height: None,
                non_breakable_lines: None,
                direction: None,
                background_color: None,
                is_code_block: None,
                code_language: None,
                hyphenate: None,
                language: None,
            })
            .collect();

        let data = serde_json::to_string(&FragmentData {
            blocks,
            tables: vec![],
        })
        .expect("fragment serialization should not fail");

        Self {
            data,
            plain_text: text.to_string(),
        }
    }

    /// Create a fragment from HTML.
    pub fn from_html(html: &str) -> Self {
        let parsed = frontend::common::parser_tools::content_parser::parse_html_elements(html);
        parsed_elements_to_fragment(parsed)
    }

    /// Create a fragment from Markdown.
    pub fn from_markdown(markdown: &str) -> Self {
        let parsed = frontend::common::parser_tools::content_parser::parse_markdown(markdown);
        parsed_elements_to_fragment(parsed)
    }

    /// Create a fragment from djot markup. Paste always uses the lossless
    /// default [`crate::DjotImportOptions`]; per-feature selection is exposed on
    /// the document-level import path (`TextDocument::set_djot_with_options`).
    pub fn from_djot(djot: &str) -> Self {
        let parsed = frontend::common::parser_tools::content_parser::parse_djot(
            djot,
            &frontend::common::parser_tools::DjotImportOptions::default(),
        );
        parsed_elements_to_fragment(parsed)
    }

    /// Create a fragment from an entire document.
    pub fn from_document(doc: &crate::TextDocument) -> crate::Result<Self> {
        let inner = doc.inner.lock();
        // Use i64::MAX as anchor to ensure the full document is captured.
        // Document positions include inter-block gaps, so character_count
        // alone would truncate the last block.
        let dto = frontend::document_inspection::ExtractFragmentDto {
            position: 0,
            anchor: i64::MAX,
        };
        let result =
            frontend::commands::document_inspection_commands::extract_fragment(&inner.ctx, &dto)?;
        Ok(Self::from_raw(result.fragment_data, result.plain_text))
    }

    /// Create a fragment from the serialized internal format.
    pub(crate) fn from_raw(data: String, plain_text: String) -> Self {
        Self { data, plain_text }
    }

    /// Export the fragment as plain text.
    pub fn to_plain_text(&self) -> &str {
        &self.plain_text
    }

    /// Export the fragment as HTML.
    pub fn to_html(&self) -> String {
        if self.data.is_empty() {
            return String::from("<html><head><meta charset=\"utf-8\"></head><body></body></html>");
        }

        let fragment_data: FragmentData = match serde_json::from_str(&self.data) {
            Ok(d) => d,
            Err(_) => {
                return String::from(
                    "<html><head><meta charset=\"utf-8\"></head><body></body></html>",
                );
            }
        };

        let mut body = String::new();
        let blocks = &fragment_data.blocks;

        // Single inline-only block with no tables: emit inline HTML without block wrapper
        if blocks.len() == 1 && blocks[0].is_inline_only() && fragment_data.tables.is_empty() {
            push_inline_html(&mut body, &blocks[0].elements);
            return format!(
                "<html><head><meta charset=\"utf-8\"></head><body>{}</body></html>",
                body
            );
        }

        // Sort tables by block_insert_index so we can interleave them
        let mut sorted_tables: Vec<&FragmentTable> = fragment_data.tables.iter().collect();
        sorted_tables.sort_by_key(|t| t.block_insert_index);
        let mut table_cursor = 0;

        let mut i = 0;

        while i < blocks.len() {
            // Insert any tables whose block_insert_index == i
            while table_cursor < sorted_tables.len()
                && sorted_tables[table_cursor].block_insert_index <= i
            {
                push_table_html(&mut body, sorted_tables[table_cursor]);
                table_cursor += 1;
            }

            let block = &blocks[i];

            if let Some(ref list) = block.list {
                let is_ordered = is_ordered_list_style(&list.style);
                let list_tag = if is_ordered { "ol" } else { "ul" };
                body.push('<');
                body.push_str(list_tag);
                body.push('>');

                while i < blocks.len() {
                    let b = &blocks[i];
                    match &b.list {
                        Some(l) if is_ordered_list_style(&l.style) == is_ordered => {
                            body.push_str("<li>");
                            push_inline_html(&mut body, &b.elements);
                            body.push_str("</li>");
                            i += 1;
                        }
                        _ => break,
                    }
                }

                body.push_str("</");
                body.push_str(list_tag);
                body.push('>');
            } else if let Some(level) = block.heading_level {
                let n = level.clamp(1, 6);
                body.push_str(&format!("<h{}>", n));
                push_inline_html(&mut body, &block.elements);
                body.push_str(&format!("</h{}>", n));
                i += 1;
            } else {
                // Emit block-level formatting as inline styles (ISSUE-19)
                let style = block_style_attr(block);
                if style.is_empty() {
                    body.push_str("<p>");
                } else {
                    body.push_str(&format!("<p style=\"{}\">", style));
                }
                push_inline_html(&mut body, &block.elements);
                body.push_str("</p>");
                i += 1;
            }
        }

        // Emit any remaining tables after all blocks
        while table_cursor < sorted_tables.len() {
            push_table_html(&mut body, sorted_tables[table_cursor]);
            table_cursor += 1;
        }

        format!(
            "<html><head><meta charset=\"utf-8\"></head><body>{}</body></html>",
            body
        )
    }

    /// Export the fragment as Markdown.
    pub fn to_markdown(&self) -> String {
        if self.data.is_empty() {
            return String::new();
        }

        let fragment_data: FragmentData = match serde_json::from_str(&self.data) {
            Ok(d) => d,
            Err(_) => return String::new(),
        };

        // (rendered_text, is_list_item) — used for join logic
        let mut parts: Vec<(String, bool)> = Vec::new();
        let mut prev_was_list = false;
        let mut list_counter: u32 = 0;

        // Sort tables by block_insert_index for interleaving
        let mut sorted_tables: Vec<&FragmentTable> = fragment_data.tables.iter().collect();
        sorted_tables.sort_by_key(|t| t.block_insert_index);
        let mut table_cursor = 0;

        for (blk_idx, block) in fragment_data.blocks.iter().enumerate() {
            // Insert tables before this block index
            while table_cursor < sorted_tables.len()
                && sorted_tables[table_cursor].block_insert_index <= blk_idx
            {
                parts.push((render_table_markdown(sorted_tables[table_cursor]), false));
                prev_was_list = false;
                list_counter = 0;
                table_cursor += 1;
            }

            let inline_text = render_inline_markdown(&block.elements);
            let is_list = block.list.is_some();

            let indent_prefix = match block.indent {
                Some(n) if n > 0 => "  ".repeat(n as usize),
                _ => String::new(),
            };

            if let Some(level) = block.heading_level {
                let n = level.clamp(1, 6) as usize;
                let prefix = "#".repeat(n);
                parts.push((format!("{} {}", prefix, inline_text), false));
                prev_was_list = false;
                list_counter = 0;
            } else if let Some(ref list) = block.list {
                let is_ordered = is_ordered_list_style(&list.style);
                if !prev_was_list {
                    list_counter = 0;
                }
                if is_ordered {
                    list_counter += 1;
                    parts.push((
                        format!("{}{}. {}", indent_prefix, list_counter, inline_text),
                        true,
                    ));
                } else {
                    parts.push((format!("{}- {}", indent_prefix, inline_text), true));
                }
                prev_was_list = true;
            } else {
                if indent_prefix.is_empty() {
                    parts.push((inline_text, false));
                } else {
                    parts.push((format!("{}{}", indent_prefix, inline_text), false));
                }
                prev_was_list = false;
                list_counter = 0;
            }

            if !is_list {
                prev_was_list = false;
            }
        }

        // Emit remaining tables after all blocks
        while table_cursor < sorted_tables.len() {
            parts.push((render_table_markdown(sorted_tables[table_cursor]), false));
            table_cursor += 1;
        }

        // Join: list items with \n, others with \n\n
        let mut result = String::new();
        for (idx, (text, is_list)) in parts.iter().enumerate() {
            if idx > 0 {
                let (_, prev_is_list) = &parts[idx - 1];
                if *prev_is_list && *is_list {
                    result.push('\n');
                } else {
                    result.push_str("\n\n");
                }
            }
            result.push_str(text);
        }

        result
    }

    /// Returns true if the fragment contains no text or elements.
    pub fn is_empty(&self) -> bool {
        self.plain_text.is_empty()
    }

    /// Returns the serialized internal representation.
    pub(crate) fn raw_data(&self) -> &str {
        &self.data
    }
}

impl Default for DocumentFragment {
    fn default() -> Self {
        Self::new()
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Shared helpers (used by both to_html and to_markdown)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

fn is_ordered_list_style(style: &ListStyle) -> bool {
    matches!(
        style,
        ListStyle::Decimal
            | ListStyle::LowerAlpha
            | ListStyle::UpperAlpha
            | ListStyle::LowerRoman
            | ListStyle::UpperRoman
    )
}

// ── HTML helpers ────────────────────────────────────────────────

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            // A raw CR in text content is normalised to LF by the HTML5 input
            // preprocessor on re-import (CR-from-`&#xD;` survives, literal CR
            // does not), which breaks serialiser idempotency. Emit it as a
            // numeric reference so it round-trips losslessly.
            '\r' => out.push_str("&#13;"),
            _ => out.push(c),
        }
    }
    out
}

/// Build a CSS `style` attribute value from block-level formatting (ISSUE-19).
fn block_style_attr(block: &FragmentBlock) -> String {
    use crate::Alignment;

    let mut parts = Vec::new();
    if let Some(ref alignment) = block.alignment {
        let value = match alignment {
            Alignment::Left => "left",
            Alignment::Right => "right",
            Alignment::Center => "center",
            Alignment::Justify => "justify",
        };
        parts.push(format!("text-align: {}", value));
    }
    if let Some(n) = block.indent
        && n > 0
    {
        parts.push(format!("margin-left: {}em", n));
    }
    if let Some(px) = block.text_indent
        && px != 0
    {
        parts.push(format!("text-indent: {}px", px));
    }
    if let Some(px) = block.top_margin {
        parts.push(format!("margin-top: {}px", px));
    }
    if let Some(px) = block.bottom_margin {
        parts.push(format!("margin-bottom: {}px", px));
    }
    if let Some(px) = block.left_margin {
        parts.push(format!("margin-left: {}px", px));
    }
    if let Some(px) = block.right_margin {
        parts.push(format!("margin-right: {}px", px));
    }
    parts.join("; ")
}

fn push_inline_html(out: &mut String, elements: &[FragmentElement]) {
    for elem in elements {
        let text = match &elem.content {
            InlineContent::Text(t) => escape_html(t),
            InlineContent::Image {
                name,
                width,
                height,
                ..
            } => {
                format!(
                    "<img src=\"{}\" width=\"{}\" height=\"{}\">",
                    escape_html(name),
                    width,
                    height
                )
            }
            InlineContent::Empty => String::new(),
        };

        let is_monospace = elem
            .fmt_font_family
            .as_deref()
            .is_some_and(|f| f == "monospace");
        let is_bold = elem.fmt_font_bold.unwrap_or(false);
        let is_italic = elem.fmt_font_italic.unwrap_or(false);
        let is_underline = elem.fmt_font_underline.unwrap_or(false);
        let is_strikeout = elem.fmt_font_strikeout.unwrap_or(false);
        let is_anchor = elem.fmt_is_anchor.unwrap_or(false);

        let mut result = text;

        if is_monospace {
            result = format!("<code>{}</code>", result);
        }
        if is_bold {
            result = format!("<strong>{}</strong>", result);
        }
        if is_italic {
            result = format!("<em>{}</em>", result);
        }
        if is_underline {
            result = format!("<u>{}</u>", result);
        }
        if is_strikeout {
            result = format!("<s>{}</s>", result);
        }
        if is_anchor && let Some(ref href) = elem.fmt_anchor_href {
            result = format!("<a href=\"{}\">{}</a>", escape_html(href), result);
        }

        out.push_str(&result);
    }
}

/// Emit an HTML `<table>` for a `FragmentTable`.
fn push_table_html(out: &mut String, table: &FragmentTable) {
    out.push_str("<table>");
    for row in 0..table.rows {
        out.push_str("<tr>");
        for col in 0..table.columns {
            if let Some(cell) = table.cells.iter().find(|c| c.row == row && c.column == col) {
                out.push_str("<td");
                if cell.row_span > 1 {
                    out.push_str(&format!(" rowspan=\"{}\"", cell.row_span));
                }
                if cell.column_span > 1 {
                    out.push_str(&format!(" colspan=\"{}\"", cell.column_span));
                }
                out.push('>');
                for (i, block) in cell.blocks.iter().enumerate() {
                    if i > 0 {
                        out.push_str("<br>");
                    }
                    push_inline_html(out, &block.elements);
                }
                out.push_str("</td>");
            }
            // Skip positions covered by spans — the HTML renderer handles them.
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
}

// ── Markdown helpers ────────────────────────────────────────────

fn escape_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '+'
                | '-'
                | '.'
                | '!'
                | '|'
                | '~'
                | '<'
                | '>'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn render_inline_markdown(elements: &[FragmentElement]) -> String {
    let mut out = String::new();
    for elem in elements {
        let raw_text = match &elem.content {
            InlineContent::Text(t) => t.clone(),
            InlineContent::Image { name, .. } => format!("![{}]({})", name, name),
            InlineContent::Empty => String::new(),
        };

        let is_monospace = elem
            .fmt_font_family
            .as_deref()
            .is_some_and(|f| f == "monospace");
        let is_bold = elem.fmt_font_bold.unwrap_or(false);
        let is_italic = elem.fmt_font_italic.unwrap_or(false);
        let is_strikeout = elem.fmt_font_strikeout.unwrap_or(false);
        let is_anchor = elem.fmt_is_anchor.unwrap_or(false);

        if is_monospace {
            out.push('`');
            out.push_str(&raw_text);
            out.push('`');
        } else {
            let mut text = escape_markdown(&raw_text);
            if is_bold && is_italic {
                text = format!("***{}***", text);
            } else if is_bold {
                text = format!("**{}**", text);
            } else if is_italic {
                text = format!("*{}*", text);
            }
            if is_strikeout {
                text = format!("~~{}~~", text);
            }
            if is_anchor {
                let href = elem.fmt_anchor_href.as_deref().unwrap_or("");
                out.push_str(&format!("[{}]({})", text, href));
            } else {
                out.push_str(&text);
            }
        }
    }
    out
}

/// Render a `FragmentTable` as a pipe-delimited Markdown table.
fn render_table_markdown(table: &FragmentTable) -> String {
    let mut rows: Vec<Vec<String>> = vec![vec![String::new(); table.columns]; table.rows];

    for cell in &table.cells {
        let text: String = cell
            .blocks
            .iter()
            .map(|b| render_inline_markdown(&b.elements))
            .collect::<Vec<_>>()
            .join(" ");
        if cell.row < table.rows && cell.column < table.columns {
            rows[cell.row][cell.column] = text;
        }
    }

    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        out.push_str("| ");
        out.push_str(&row.join(" | "));
        out.push_str(" |");
        if i == 0 {
            // Header separator
            out.push('\n');
            out.push('|');
            for _ in 0..table.columns {
                out.push_str(" --- |");
            }
        }
        if i + 1 < rows.len() {
            out.push('\n');
        }
    }
    out
}

// ── Fragment construction from parsed content ───────────────────

/// Convert parsed blocks (from HTML or Markdown parser) into a `DocumentFragment`.
/// Convert a `ParsedSpan` to a `FragmentElement`.
fn span_to_fragment_element(span: &ParsedSpan) -> FragmentElement {
    let content = InlineContent::Text(span.text.clone());
    let fmt_font_family = if span.code {
        Some("monospace".into())
    } else {
        None
    };
    let fmt_font_bold = if span.bold { Some(true) } else { None };
    let fmt_font_italic = if span.italic { Some(true) } else { None };
    let fmt_font_underline = if span.underline { Some(true) } else { None };
    let fmt_font_strikeout = if span.strikeout { Some(true) } else { None };
    let (fmt_anchor_href, fmt_is_anchor) = if let Some(ref href) = span.link_href {
        (Some(href.clone()), Some(true))
    } else {
        (None, None)
    };

    FragmentElement {
        content,
        fmt_font_family,
        fmt_font_point_size: None,
        fmt_font_weight: None,
        fmt_font_bold,
        fmt_font_italic,
        fmt_font_underline,
        fmt_font_overline: None,
        fmt_font_strikeout,
        fmt_letter_spacing: None,
        fmt_word_spacing: None,
        fmt_anchor_href,
        fmt_anchor_names: vec![],
        fmt_is_anchor,
        fmt_tooltip: None,
        fmt_underline_style: None,
        fmt_vertical_alignment: None,
    }
}

/// Convert parsed elements (blocks + tables) into a `DocumentFragment`,
/// preserving table structure as `FragmentTable` entries.
fn parsed_elements_to_fragment(parsed: Vec<ParsedElement>) -> DocumentFragment {
    use frontend::common::parser_tools::fragment_schema::FragmentList;

    let mut blocks: Vec<FragmentBlock> = Vec::new();
    let mut tables: Vec<FragmentTable> = Vec::new();

    for elem in parsed {
        match elem {
            ParsedElement::Block(pb) => {
                let elements: Vec<FragmentElement> =
                    pb.spans.iter().map(span_to_fragment_element).collect();
                let plain_text: String = pb.spans.iter().map(|s| s.text.as_str()).collect();
                let list = pb.list_style.map(|style| FragmentList {
                    style,
                    indent: pb.list_indent as i64,
                    prefix: String::new(),
                    suffix: String::new(),
                });

                blocks.push(FragmentBlock {
                    plain_text,
                    elements,
                    heading_level: pb.heading_level,
                    list,
                    alignment: None,
                    indent: None,
                    text_indent: None,
                    marker: None,
                    top_margin: None,
                    bottom_margin: None,
                    left_margin: None,
                    right_margin: None,
                    tab_positions: vec![],
                    line_height: pb.line_height,
                    non_breakable_lines: pb.non_breakable_lines,
                    direction: pb.direction,
                    background_color: pb.background_color,
                    is_code_block: None,
                    code_language: None,
                    hyphenate: None,
                    language: None,
                });
            }
            ParsedElement::Table(pt) => {
                let block_insert_index = blocks.len();
                let num_columns = pt.rows.iter().map(|r| r.len()).max().unwrap_or(0);
                let num_rows = pt.rows.len();

                let mut frag_cells: Vec<FragmentTableCell> = Vec::new();
                for (row_idx, row) in pt.rows.iter().enumerate() {
                    for (col_idx, cell) in row.iter().enumerate() {
                        let cell_elements: Vec<FragmentElement> =
                            cell.spans.iter().map(span_to_fragment_element).collect();
                        let cell_text: String =
                            cell.spans.iter().map(|s| s.text.as_str()).collect();

                        frag_cells.push(FragmentTableCell {
                            row: row_idx,
                            column: col_idx,
                            row_span: 1,
                            column_span: 1,
                            blocks: vec![FragmentBlock {
                                plain_text: cell_text,
                                elements: cell_elements,
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
                                tab_positions: vec![],
                                line_height: None,
                                non_breakable_lines: None,
                                direction: None,
                                background_color: None,
                                is_code_block: None,
                                code_language: None,
                                hyphenate: None,
                                language: None,
                            }],
                            fmt_padding: None,
                            fmt_border: None,
                            fmt_vertical_alignment: None,
                            fmt_background_color: None,
                        });
                    }
                }

                tables.push(FragmentTable {
                    rows: num_rows,
                    columns: num_columns,
                    cells: frag_cells,
                    block_insert_index,
                    fmt_border: None,
                    fmt_cell_spacing: None,
                    fmt_cell_padding: None,
                    fmt_width: None,
                    fmt_alignment: None,
                    column_widths: vec![],
                });
            }
        }
    }

    let data = serde_json::to_string(&FragmentData { blocks, tables })
        .expect("fragment serialization should not fail");

    let plain_text = parsed_plain_text_from_data(&data);

    DocumentFragment { data, plain_text }
}

/// Extract plain text from serialized fragment data.
fn parsed_plain_text_from_data(data: &str) -> String {
    let fragment_data: FragmentData = match serde_json::from_str(data) {
        Ok(d) => d,
        Err(_) => return String::new(),
    };

    fragment_data
        .blocks
        .iter()
        .map(|b| b.plain_text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
