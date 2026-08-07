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
use common::parser_tools::image_options::{ExportImages, base64_encode};
use common::types::EntityId;

/// How an image is represented in rendered HTML.
///
/// The same renderer serves the HTML exporter (whose output is a standalone
/// string the caller places somewhere) and the EPUB exporter (whose output is a
/// chapter inside a package that also carries the image files). Those need
/// different `src` values for the same document, so the choice is a parameter
/// rather than a constant.
#[derive(Debug, Clone, Copy, Default)]
pub enum HtmlImagePolicy<'a> {
    /// Emit `src` exactly as the document stores it.
    #[default]
    Reference,
    /// Replace `src` via a map of document-src → output-href. Used by the EPUB
    /// exporter, where every image is repackaged under its own name.
    Rewrite(&'a std::collections::BTreeMap<String, String>),
    /// Inline the bytes as a `data:` URI, producing a self-contained document.
    DataUri(&'a ExportImages),
    /// Emit no `<img>` at all, leaving only the alt text as the visible content.
    Omit,
}

impl HtmlImagePolicy<'_> {
    /// Resolve one image's `src`, or `None` when it should not be emitted.
    fn resolve(&self, name: &str) -> Option<String> {
        match self {
            Self::Reference => Some(name.to_string()),
            // An unmapped image keeps its original src rather than vanishing:
            // a reference that might resolve beats a picture silently deleted.
            Self::Rewrite(map) => Some(map.get(name).cloned().unwrap_or_else(|| name.to_string())),
            Self::DataUri(images) => images.get(name).map(|img| {
                format!(
                    "data:{};base64,{}",
                    img.mime_type,
                    base64_encode(&img.bytes)
                )
            }),
            Self::Omit => None,
        }
    }
}

/// Render a slice of blocks (already fetched, in document order) as HTML,
/// grouping consecutive list items into `<ul>`/`<ol>` and handling code
/// blocks, headings, and plain paragraphs. Mirrors the dispatch order used
/// by the DOCX/djot exporters: code block, then list membership, then
/// heading/paragraph.
pub fn render_blocks_html(
    store: &Store,
    blocks: &[Block],
    images: HtmlImagePolicy<'_>,
    notes: &crate::footnotes::Footnotes,
) -> String {
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
                    let inline_html = render_inline_html(store, b, images, notes);
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
            let inline_html = render_inline_html(store, block, images, notes);

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
            // Both spellings, because neither alone reaches everything: `break-before`
            // is the CSS3 property, `page-break-before` its CSS2 predecessor, and
            // EPUB reading systems and browser print engines are split between them.
            // Inert on screen, load-bearing on paper.
            if block.fmt_page_break_before == Some(true) {
                styles.push("break-before: page".into());
                styles.push("page-break-before: always".into());
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
/// Render one block's inline content.
///
/// Takes the document's resolved footnotes rather than defaulting them: a
/// reference has to print its number, and a convenience overload that quietly
/// passed none would render every marker as a raw label with nothing to say it
/// was wrong.
pub fn render_inline_html(
    store: &Store,
    block: &Block,
    images: HtmlImagePolicy<'_>,
    notes: &crate::footnotes::Footnotes,
) -> String {
    let block_text = block_content_via_store(block, store);
    let elements = inline_segments_for_block(store, block.id, &block_text);

    let mut html = String::new();

    for elem in &elements {
        let text = match &elem.content {
            InlineContent::Text(t) => escape_html(t),
            // A footnote reference.
            //
            // Both the `epub:type` and the DPUB-ARIA role: `epub:type` alone
            // reaches no assistive technology, the same pairing the epigraph
            // work settled on. Reading systems render this pair as a pop-up,
            // which is a reflowable book's own idiom for a footnote — there is
            // no page bottom to put one at.
            //
            // Unless the label is cited only from inside another note's own
            // body (`Footnotes::is_nested_reference` — see its doc, and
            // `footnotes.rs`'s module doc for why that citation is refused
            // rather than numbered): no writer ever gives that label a number
            // or an aside, so a live link here would carry an `href` to a
            // `#fn-…` id nothing emits. An ordinary **dangling** reference (no
            // definition anywhere) is not this — it keeps the full noteref
            // treatment exactly as before.
            InlineContent::FootnoteRef { label } => {
                let marker = escape_html(&notes.marker(label));
                if notes.is_nested_reference(label) {
                    html.push_str(&format!("<sup>{marker}</sup>"));
                } else {
                    let id = escape_html(label);
                    html.push_str(&format!(
                        "<a epub:type=\"noteref\" role=\"doc-noteref\" href=\"#fn-{id}\" \
                         id=\"fnref-{id}\"><sup>{marker}</sup></a>"
                    ));
                }
                continue;
            }
            InlineContent::Image {
                name,
                alt,
                width,
                height,
                ..
            } => match images.resolve(name) {
                Some(src) => {
                    // `alt` is required markup, not decoration: an <img> without
                    // it is inaccessible, and EPUB validators flag it. Emit it
                    // even when empty — an explicit empty alt is the correct way
                    // to say "decorative", whereas a missing attribute says
                    // nothing at all.
                    let mut tag = format!(
                        "<img src=\"{}\" alt=\"{}\"",
                        escape_html(&src),
                        escape_html(alt)
                    );
                    if *width > 0 {
                        tag.push_str(&format!(" width=\"{width}\""));
                    }
                    if *height > 0 {
                        tag.push_str(&format!(" height=\"{height}\""));
                    }
                    tag.push_str(" />");
                    tag
                }
                // Dropped image: the description is all that is left to show,
                // and showing it beats leaving a silent hole in the prose.
                None => escape_html(alt),
            },
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
pub fn render_table_html(
    store: &Store,
    table_id: EntityId,
    images: HtmlImagePolicy<'_>,
    notes: &crate::footnotes::Footnotes,
) -> Result<String> {
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
                        let inline_html = render_inline_html(store, block, images, notes);
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
