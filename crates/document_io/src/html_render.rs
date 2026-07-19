//! Store-based HTML rendering shared by the HTML and EPUB exporters.
//!
//! Each exporter's use case owns its own document TRAVERSAL — walking
//! Root→Document→Frame→Block through its own `QueryUnitOfWork`-backed uow
//! getters, since that walk differs per exporter (DOCX doesn't emit HTML at
//! all; EPUB additionally has to find chapter-heading boundaries in the
//! block stream). What the traversals arrive at — a contiguous run of
//! [`Block`]s in document order, or a table to render — is identical, so
//! *that* half is factored out here as free functions over
//! `&common::database::Store` rather than over a uow trait: a use case
//! passes `uow.store()` (already available via `QueryUnitOfWork::store()`
//! on every export uow) and gets an HTML fragment back.
//!
//! Table rendering additionally needs `Table`/`TableCell`/`Frame` data. It
//! reads those straight off the store's public entity maps (`store.tables`,
//! `store.table_cells`, `store.frames`, `store.blocks`) rather than through a
//! uow — the same "read the store's maps directly for a read-only structural
//! query" idiom `common::database::rope_helpers` already uses (see
//! `walk_frame_bounds`/`compute_frame_byte_range_recursive`). That keeps this
//! module free of any uow trait, so it can be called from both
//! `export_html_uc` and `export_epub_uc` without those two use cases sharing
//! a uow trait (each keeps its own, per the "a use case may not call another
//! use case" rule — and neither may reach into the other's uow).

use anyhow::{Result, anyhow};
use common::database::Store;
use common::database::rope_helpers::block_content_via_store;
use common::entities::{Alignment, Block, ListStyle, TableCell, TextDirection};
use common::format_runs::InlineContent;
use common::format_runs_query::inline_segments_for_block;
use common::types::EntityId;

/// Render a slice of blocks (already fetched, in document order) as HTML,
/// grouping consecutive list items into `<ul>`/`<ol>` and handling code
/// blocks, headings, and plain paragraphs. Mirrors the dispatch order used
/// by the DOCX/djot exporters: code block, then list membership, then
/// heading/paragraph.
pub fn render_blocks_html(store: &Store, blocks: &[Block]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;

    while i < blocks.len() {
        let block = &blocks[i];

        // --- Code block ---
        if block.fmt_is_code_block == Some(true) {
            let raw_text = block_plain_text(store, block);
            let escaped = escape_html(&raw_text);

            let code_open = if let Some(ref lang) = block.fmt_code_language {
                if !lang.is_empty() {
                    format!("<code class=\"language-{}\">", escape_html(lang))
                } else {
                    "<code>".to_string()
                }
            } else {
                "<code>".to_string()
            };

            parts.push(format!("<pre>{}{}</code></pre>", code_open, escaped));
            i += 1;
            continue;
        }

        // --- List items ---
        let list = block
            .list
            .and_then(|list_id| store.lists.read().get(&list_id).cloned());

        if let Some(list_entity) = list {
            let is_ordered = matches!(
                list_entity.style,
                ListStyle::Decimal
                    | ListStyle::LowerAlpha
                    | ListStyle::UpperAlpha
                    | ListStyle::LowerRoman
                    | ListStyle::UpperRoman
            );
            let list_tag = if is_ordered { "ol" } else { "ul" };
            let mut list_items = Vec::new();

            while i < blocks.len() {
                let b = &blocks[i];
                let b_is_listed = b
                    .list
                    .is_some_and(|list_id| store.lists.read().contains_key(&list_id));

                if b_is_listed {
                    let inline_html = render_inline_html(store, b);
                    list_items.push(format!("<li>{}</li>", inline_html));
                    i += 1;
                } else {
                    break;
                }
            }

            parts.push(format!(
                "<{}>{}</{}>",
                list_tag,
                list_items.join(""),
                list_tag
            ));
        } else {
            // --- Normal block (paragraph / heading) ---
            let inline_html = render_inline_html(store, block);

            let mut styles: Vec<String> = Vec::new();
            match block.fmt_alignment {
                Some(Alignment::Left) => styles.push("text-align: left".into()),
                Some(Alignment::Right) => styles.push("text-align: right".into()),
                Some(Alignment::Center) => styles.push("text-align: center".into()),
                Some(Alignment::Justify) => styles.push("text-align: justify".into()),
                None => {}
            }
            if let Some(lh) = block.fmt_line_height {
                styles.push(format!("line-height: {}", lh as f64 / 1000.0));
            }
            if block.fmt_non_breakable_lines == Some(true) {
                styles.push("white-space: pre".into());
            }
            if block.fmt_direction == Some(TextDirection::RightToLeft) {
                styles.push("direction: rtl".into());
            }
            if let Some(ref c) = block.fmt_background_color {
                styles.push(format!("background-color: {}", c));
            }
            // The model's unit for these two is the logical (CSS) pixel, so they
            // map straight across. A scene break emits `text_indent=0` on the
            // paragraph that follows it, which must win over any stylesheet
            // first-line indent — hence emitting it even when zero.
            if let Some(tm) = block.fmt_top_margin {
                styles.push(format!("margin-top: {tm}px"));
            }
            if let Some(ti) = block.fmt_text_indent {
                styles.push(format!("text-indent: {ti}px"));
            }
            let style_attr = if styles.is_empty() {
                String::new()
            } else {
                format!(" style=\"{}\"", styles.join("; "))
            };

            if let Some(level) = block.fmt_heading_level {
                let level = level.clamp(1, 6);
                parts.push(format!(
                    "<h{}{}>{}</h{}>",
                    level, style_attr, inline_html, level
                ));
            } else {
                parts.push(format!("<p{}>{}</p>", style_attr, inline_html));
            }
            i += 1;
        }
    }

    parts.join("")
}

/// Render one block's inline content (text runs + images) as HTML, applying
/// character formatting (monospace/bold/italic/underline/strike/hyperlink).
pub fn render_inline_html(store: &Store, block: &Block) -> String {
    let block_text = block_content_via_store(block, store);
    let elements = inline_segments_for_block(store, block.id, &block_text);

    let mut html = String::new();

    for elem in &elements {
        let text = match &elem.content {
            InlineContent::Text(t) => escape_html(t),
            InlineContent::Image {
                name,
                width,
                height,
                ..
            } => {
                format!(
                    "<img src=\"{}\" width=\"{}\" height=\"{}\" />",
                    escape_html(name),
                    width,
                    height
                )
            }
            InlineContent::Empty => String::new(),
        };

        if text.is_empty() {
            continue;
        }

        // Check if this is an image tag (already formatted)
        if text.starts_with("<img ") {
            html.push_str(&text);
            continue;
        }

        let mut formatted = text;

        if elem.fmt_font_family.as_deref() == Some("monospace") {
            formatted = format!("<code>{}</code>", formatted);
        }
        if elem.fmt_font_bold == Some(true) {
            formatted = format!("<strong>{}</strong>", formatted);
        }
        if elem.fmt_font_italic == Some(true) {
            formatted = format!("<em>{}</em>", formatted);
        }
        if elem.fmt_font_underline == Some(true) {
            formatted = format!("<u>{}</u>", formatted);
        }
        if elem.fmt_font_strikeout == Some(true) {
            formatted = format!("<s>{}</s>", formatted);
        }
        if let Some(ref href) = elem.fmt_anchor_href {
            formatted = format!("<a href=\"{}\">{}</a>", escape_html(href), formatted);
        }

        html.push_str(&formatted);
    }

    html
}

/// The block's text with all inline formatting stripped — just the
/// concatenated literal text of its format runs (image segments
/// contribute nothing). Used for code blocks (whose content must not carry
/// inline marks) and by the EPUB exporter to lift a heading's visible text
/// as a chapter/TOC title.
pub fn block_plain_text(store: &Store, block: &Block) -> String {
    let block_text = block_content_via_store(block, store);
    let elements = inline_segments_for_block(store, block.id, &block_text);

    let mut raw_text = String::new();
    for elem in &elements {
        if let InlineContent::Text(t) = &elem.content {
            raw_text.push_str(t);
        }
    }
    raw_text
}

/// Render the table `table_id` as an HTML `<table>`, including its cells'
/// content. Reads `Table`/`TableCell`/`Frame`/`Block` straight off the
/// store's public entity maps — no transaction/uow needed for a read (see
/// the module doc comment).
pub fn render_table_html(store: &Store, table_id: EntityId) -> Result<String> {
    let table = store
        .tables
        .read()
        .get(&table_id)
        .cloned()
        .ok_or_else(|| anyhow!("Table not found"))?;

    let mut cells: Vec<TableCell> = table
        .cells
        .iter()
        .filter_map(|cid| store.table_cells.read().get(cid).cloned())
        .collect();
    cells.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));

    // Build a grid to track which cells are covered by spans
    let rows = table.rows as usize;
    let cols = table.columns as usize;
    let mut covered = vec![vec![false; cols]; rows];

    let mut html = String::from("<table");
    if let Some(border) = table.fmt_border {
        html.push_str(&format!(" border=\"{}\"", border));
    }
    html.push('>');

    for r in 0..rows {
        html.push_str("<tr>");
        for c in 0..cols {
            if covered[r][c] {
                continue;
            }

            // Find the cell at this position
            let cell = cells
                .iter()
                .find(|cell| cell.row == r as i64 && cell.column == c as i64);

            if let Some(cell) = cell {
                let mut td = String::from("<td");
                if cell.row_span > 1 {
                    td.push_str(&format!(" rowspan=\"{}\"", cell.row_span));
                }
                if cell.column_span > 1 {
                    td.push_str(&format!(" colspan=\"{}\"", cell.column_span));
                }
                td.push('>');

                // Render cell content from the cell's frame
                if let Some(cf_id) = cell.cell_frame {
                    let block_ids = store
                        .frames
                        .read()
                        .get(&cf_id)
                        .map(|f| f.blocks.clone())
                        .unwrap_or_default();
                    let blocks: Vec<Block> = block_ids
                        .iter()
                        .filter_map(|bid| store.blocks.read().get(bid).cloned())
                        .collect();

                    let mut cell_parts: Vec<String> = Vec::new();
                    for block in &blocks {
                        let inline_html = render_inline_html(store, block);
                        if !inline_html.is_empty() {
                            cell_parts.push(inline_html);
                        }
                    }
                    td.push_str(&cell_parts.join("<br/>"));
                }

                td.push_str("</td>");
                html.push_str(&td);

                // Mark spanned cells as covered
                for sr in 0..cell.row_span as usize {
                    for sc in 0..cell.column_span as usize {
                        if sr == 0 && sc == 0 {
                            continue;
                        }
                        if r + sr < rows && c + sc < cols {
                            covered[r + sr][c + sc] = true;
                        }
                    }
                }
            } else {
                html.push_str("<td></td>");
            }
        }
        html.push_str("</tr>");
    }

    html.push_str("</table>");
    Ok(html)
}

/// Escape `&`, `<`, `>`, `"`, `'` and a literal CR for safe inclusion in HTML
/// text content (`&#13;` rather than a raw CR — see the note on
/// idempotency below).
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
        // A raw CR in text content is normalised to LF by the HTML5 input
        // preprocessor on re-import (CR-from-`&#xD;` survives, literal CR
        // does not), which breaks serialiser idempotency. Emit it as a
        // numeric reference so it round-trips losslessly.
        .replace('\r', "&#13;")
}
