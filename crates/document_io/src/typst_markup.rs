//! Store-based Typst markup rendering shared by the PDF exporter.
//!
//! Mirrors [`crate::html_render`]'s shape: the PDF use case
//! ([`crate::use_cases::export_pdf_uc`]) owns the document TRAVERSAL (Root→Document→Frame→Block,
//! `child_order` interleaving, cell-frame/blockquote handling — see
//! `export_html_uc.rs::render_frame_html` for the pattern this follows), while *this* module is a
//! plain function library over `&common::database::Store`/`&[Block]` that turns an already-fetched
//! run of blocks (or one table) into Typst markup text. It is not itself a use case, and it is not
//! reused by any other exporter (unlike `html_render`, which HTML and EPUB both need because they
//! share one output substrate) — Typst is a third, distinct substrate, so it gets its own
//! from-scratch emitter, per the same precedent that gives DOCX and LaTeX their own.
//!
//! **Escaping discipline**: [`escape_typst`] is applied to every literal character of author
//! prose that lands directly in markup (heading/paragraph/list-item text, table cell text, the
//! `title`/`author` document-metadata strings use a separate, narrower *string-literal* escape —
//! see [`escape_typst_string`] — since those are Typst *string* arguments, not markup content).
//! Code-block content uses [`escape_typst_string`] too (it becomes a `#raw("...")` **string**
//! argument, never markup) — never [`escape_typst`], which would corrupt the code's literal
//! characters (e.g. turning `a * b` into `a \* b`).

use anyhow::{Result, anyhow};
use common::database::Store;
use common::database::rope_helpers::block_content_via_store;
use common::entities::{
    Alignment, Block, CharVerticalAlignment, ListStyle, TableCell, TextDirection,
};
use common::format_runs::InlineContent;
use common::format_runs_query::inline_segments_for_block;
use common::parser_tools::PdfExportOptions;
use common::types::EntityId;

// ─────────────────────────── Escaping ───────────────────────────

/// Escapes literal author prose for safe embedding directly in Typst markup (never inside a
/// `#raw(...)`/string-literal argument — use [`escape_typst_string`] there instead).
///
/// Over-escapes some characters outside their strictly-dangerous context (every literal hyphen,
/// for instance) — the same trade-off `escape_latex` already makes for `&%$#_{}~^`. This is the
/// only rule that is unconditionally injection-proof without per-context parsing, which matters
/// here specifically because author prose must never be able to inject Typst code.
pub fn escape_typst(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '*' => out.push_str("\\*"),
            '_' => out.push_str("\\_"),
            '`' => out.push_str("\\`"),
            '#' => out.push_str("\\#"),
            '$' => out.push_str("\\$"),
            '<' => out.push_str("\\<"),
            '>' => out.push_str("\\>"),
            '@' => out.push_str("\\@"),
            '~' => out.push_str("\\~"),
            '[' => out.push_str("\\["),
            ']' => out.push_str("\\]"),
            '-' => out.push_str("\\-"),
            '/' => out.push_str("\\/"),
            '=' => out.push_str("\\="),
            '+' => out.push_str("\\+"),
            _ => out.push(ch),
        }
    }
    guard_leading_numbered_list(out)
}

/// Escapes the `.` in a leading `<digits>.` run (e.g. `"12. Go left"` -> `"12\. Go left"`) to
/// block numbered-list-marker parsing at the start of a line/content-block. Digits alone are
/// never special; only `digit+"."` at the very start is.
fn guard_leading_numbered_list(s: String) -> String {
    let digit_run_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    if digit_run_end > 0 && s[digit_run_end..].starts_with('.') {
        let mut out = String::with_capacity(s.len() + 1);
        out.push_str(&s[..digit_run_end]);
        out.push('\\');
        out.push_str(&s[digit_run_end..]);
        out
    } else {
        s
    }
}

/// Escapes a string for use as a Typst **string literal** argument (inside `"..."`, e.g.
/// `#raw("...")`, `#link("...")`, `#set text(font: "...")`, `#set document(title: "...")`) —
/// backslash and double-quote only. This is deliberately narrower than [`escape_typst`]: string
/// content is never parsed as markup, so `*`/`_`/`#`/etc. need no escaping there, and escaping
/// them would corrupt literal code-block text.
fn escape_typst_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

// ─────────────────────────── Preamble ───────────────────────────

/// Build the `#set page/text/par/smartquote/heading` preamble (plus an optional
/// `#set document(title:, author:)` metadata line) from `options`. Returns an empty string when
/// `options.include_preamble` is `false` — mirrors `ExportLatexDto.include_preamble`, letting a
/// caller embed the emitted body inside a larger hand-authored Typst document.
pub fn typst_preamble(options: &PdfExportOptions) -> String {
    if !options.include_preamble {
        return String::new();
    }

    let mut out = String::new();

    // Document metadata — only emitted when at least one field is set, matching the struct doc's
    // "None => Typst/krilla defaults, no explicit /Title or /Author" contract.
    if options.title.is_some() || options.author.is_some() {
        let mut meta_args: Vec<String> = Vec::new();
        if let Some(title) = &options.title {
            meta_args.push(format!("title: \"{}\"", escape_typst_string(title)));
        }
        if let Some(author) = &options.author {
            meta_args.push(format!("author: \"{}\"", escape_typst_string(author)));
        }
        out.push_str(&format!("#set document({})\n", meta_args.join(", ")));
    }

    out.push_str(&format!(
        "#set page(\n  width: {w}mm, height: {h}mm,\n  margin: (top: {mt}mm, bottom: {mb}mm, left: {ml}mm, right: {mr}mm),\n)\n",
        w = options.page_width_mm,
        h = options.page_height_mm,
        mt = options.margin_top_mm,
        mb = options.margin_bottom_mm,
        ml = options.margin_left_mm,
        mr = options.margin_right_mm,
    ));

    let mut text_args: Vec<String> = Vec::new();
    if !options.font_family.is_empty() {
        text_args.push(format!(
            "font: \"{}\"",
            escape_typst_string(&options.font_family)
        ));
    }
    text_args.push(format!("size: {}pt", options.font_size_pt));
    if let Some(lang) = options.lang.as_deref().filter(|l| !l.is_empty()) {
        text_args.push(format!("lang: \"{}\"", escape_typst_string(lang)));
    }
    text_args.push(format!(
        "dir: {}",
        if options.base_rtl { "rtl" } else { "ltr" }
    ));
    out.push_str(&format!("#set text({})\n", text_args.join(", ")));

    let mut par_args: Vec<String> = vec![
        format!("justify: {}", options.justify),
        format!("leading: {}em", options.line_spacing),
    ];
    if let Some(indent) = options.first_line_indent_mm {
        par_args.push(format!("first-line-indent: {indent}mm"));
    }
    out.push_str(&format!("#set par({})\n", par_args.join(", ")));

    // `#set block(spacing: ..)` is document-wide (it governs the gap between ANY two blocks —
    // headings, lists, tables — not only body paragraphs), a coarser approximation than DOCX's
    // per-body-paragraph "space after" but the only document-level knob Typst offers without
    // wrapping every non-heading paragraph individually.
    if let Some(spacing) = options.paragraph_spacing_pt {
        out.push_str(&format!("#set block(spacing: {spacing}pt)\n"));
    }

    // Straight quotes in dialogue-heavy prose pass through `escape_typst` unescaped (it does not
    // touch `'`/`"`); disabling smart-quote substitution here, once, is what keeps them literal
    // instead of silently becoming curly quotes.
    out.push_str("#set smartquote(enabled: false)\n");
    // Skribisto composes its own, already-numbered heading text (see the heading-level mapping
    // below) — Typst must not additionally auto-number it.
    out.push_str("#set heading(numbering: none)\n");

    out
}

// ─────────────────────────── Block-level rendering ───────────────────────────

/// Render a slice of blocks (already fetched, in document order) as Typst markup, grouping
/// consecutive list items into one `#enum(..)`/`#list(..)` call and handling code blocks,
/// headings, and plain paragraphs. Mirrors the dispatch order `render_blocks_html` uses: code
/// block, then list membership, then heading/paragraph — and, additionally (Typst-specific, not
/// present in the HTML mapping), block-level line-height/direction/background/non-breakable/
/// alignment wraps around each heading/paragraph.
///
/// List items and code blocks intentionally do NOT receive those block-level wraps, mirroring
/// `render_blocks_html`'s own precedent (its list/code branches carry no `style_attr` either) —
/// only the "normal block" (heading/paragraph) branch does.
pub fn render_blocks_typst(store: &Store, blocks: &[Block], options: &PdfExportOptions) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;

    while i < blocks.len() {
        let block = &blocks[i];

        // --- Code block ---
        if block.fmt_is_code_block == Some(true) {
            let raw_text = raw_block_text(store, block);
            let lang_arg = block
                .fmt_code_language
                .as_deref()
                .filter(|l| !l.is_empty())
                .map(|l| format!(", lang: \"{}\"", escape_typst_string(l)))
                .unwrap_or_default();
            parts.push(format!(
                "#raw(\"{}\"{}, block: true)",
                escape_typst_string(&raw_text),
                lang_arg
            ));
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
            let mut items: Vec<String> = Vec::new();

            while i < blocks.len() {
                let b = &blocks[i];
                let b_is_listed = b
                    .list
                    .is_some_and(|list_id| store.lists.read().contains_key(&list_id));

                if b_is_listed {
                    let inline = render_inline_typst(store, b);
                    items.push(format!("[{inline}]"));
                    i += 1;
                } else {
                    break;
                }
            }

            let call = if is_ordered {
                format!(
                    "#enum(numbering: \"{}\")",
                    numbering_pattern(&list_entity.style)
                )
            } else {
                format!("#list(marker: [{}])", bullet_marker(&list_entity.style))
            };
            parts.push(format!("{call}{}", items.join("")));
        } else {
            // --- Normal block (paragraph / heading) ---
            let inline = render_inline_typst(store, block);

            // Skip a block with no inline content: an empty paragraph would inject a stray blank
            // (doubling the `\n\n` join) and an empty heading would emit a bare `= ` — a
            // titleless heading in the output. None of the wraps below mean anything without
            // content either.
            if inline.is_empty() {
                i += 1;
                continue;
            }

            // Per-block RTL wraps the *inline* content, not the whole block, so a heading keeps
            // its `= ` marker at the true block start — Typst only treats `=` as a heading there,
            // and `#text(dir: rtl)[= H]` would bury the marker inside a text element and lose the
            // heading. An RTL heading thus renders as `= #text(dir: rtl)[H]`.
            let inline = if block.fmt_direction == Some(TextDirection::RightToLeft) {
                format!("#text(dir: rtl)[{inline}]")
            } else {
                inline
            };

            let mut content = if let Some(level) = block.fmt_heading_level {
                let level = level.clamp(1, 6) as usize;
                format!("{} {}", "=".repeat(level), inline)
            } else {
                inline
            };

            if let Some(lh) = block.fmt_line_height {
                let ratio = lh as f64 / 1000.0;
                let leading_em = ratio * options.line_spacing as f64;
                content = format!("#[#set par(leading: {leading_em}em)\n{content}]");
            }
            // `fmt_text_indent` / `fmt_top_margin` are in the model's own unit —
            // logical (CSS) pixels at 96 dpi — so convert to Typst's physical
            // units: 96 px = 1 in = 25.4 mm = 72 pt. Each scopes an override of
            // the document-wide `#set par(first-line-indent:)` / block spacing,
            // which is how a scene break suppresses the following paragraph's
            // indent (`text_indent=0`) and opens a gap above it.
            if let Some(ti) = block.fmt_text_indent {
                let mm = ti as f64 * 25.4 / 96.0;
                content = format!("#[#set par(first-line-indent: {mm:.3}mm)\n{content}]");
            }
            if let Some(tm) = block.fmt_top_margin.filter(|&t| t > 0) {
                let pt = tm as f64 * 72.0 / 96.0;
                content = format!("#block(above: {pt:.2}pt)[{content}]");
            }
            if let Some(ref color) = block.fmt_background_color
                && !color.is_empty()
            {
                content = format!(
                    "#block(fill: rgb(\"{}\"), width: 100%)[{content}]",
                    escape_typst_string(color)
                );
            }
            if block.fmt_non_breakable_lines == Some(true) {
                content = format!("#block(breakable: false)[{content}]");
            }
            content = wrap_alignment(content, block.fmt_alignment.as_ref(), options.justify);

            parts.push(content);
            i += 1;
        }
    }

    parts.join("\n\n")
}

/// Wrap `content` per `alignment`, relative to the document's base `doc_justify` default —
/// `None` inherits the document default untouched; every other case scopes a `#set
/// par(justify: ..)` override only when it would actually differ from that default (an
/// already-justified document doesn't need a redundant override for a `Justify` block, and vice
/// versa), then (for `Left`/`Right`/`Center`) wraps the result in `#align(..)`.
fn wrap_alignment(content: String, alignment: Option<&Alignment>, doc_justify: bool) -> String {
    match alignment {
        None => content,
        Some(Alignment::Justify) => {
            if doc_justify {
                content
            } else {
                scoped_justify(true, content)
            }
        }
        Some(side @ (Alignment::Left | Alignment::Right | Alignment::Center)) => {
            let content = if doc_justify {
                scoped_justify(false, content)
            } else {
                content
            };
            let align_arg = match side {
                Alignment::Left => "left",
                Alignment::Right => "right",
                Alignment::Center => "center",
                Alignment::Justify => unreachable!(),
            };
            format!("#align({align_arg})[{content}]")
        }
    }
}

/// Scope a `#set par(justify: ..)` override to exactly `content`, via a content-block set-rule
/// (`#[#set par(justify: ..)\n...]`) — the Typst idiom for a local style override that must not
/// leak to sibling blocks.
fn scoped_justify(justify: bool, content: String) -> String {
    format!("#[#set par(justify: {justify})\n{content}]")
}

/// Typst `enum` numbering pattern for an ordered [`ListStyle`]. `Disc`/`Circle`/`Square` never
/// reach here (guarded by the `is_ordered` check at the call site).
fn numbering_pattern(style: &ListStyle) -> &'static str {
    match style {
        ListStyle::Decimal => "1.",
        ListStyle::LowerAlpha => "a.",
        ListStyle::UpperAlpha => "A.",
        ListStyle::LowerRoman => "i.",
        ListStyle::UpperRoman => "I.",
        ListStyle::Disc | ListStyle::Circle | ListStyle::Square => "1.",
    }
}

/// Typst `list` marker glyph for an unordered [`ListStyle`], matching the glyphs the DOCX
/// exporter's numbering builder already uses for the same styles.
fn bullet_marker(style: &ListStyle) -> &'static str {
    match style {
        ListStyle::Disc => "\u{2022}",   // •
        ListStyle::Circle => "\u{25CB}", // ○
        ListStyle::Square => "\u{25AA}", // ▪
        _ => "\u{2022}",
    }
}

/// Render one block's inline content (text runs + images) as Typst markup, applying character
/// formatting (monospace/bold/italic/underline/strike/super/sub/hyperlink). Nesting order
/// (innermost to outermost) mirrors `render_inline_latex`: monospace, bold, italic, underline,
/// strike, vertical alignment, then hyperlink outermost.
pub fn render_inline_typst(store: &Store, block: &Block) -> String {
    let block_text = block_content_via_store(block, store);
    let elements = inline_segments_for_block(store, block.id, &block_text);

    let mut out = String::new();

    for elem in &elements {
        let is_monospace = elem.fmt_font_family.as_deref() == Some("monospace");

        let text = match &elem.content {
            InlineContent::Text(t) => {
                if is_monospace {
                    format!("#raw(\"{}\")", escape_typst_string(t))
                } else {
                    escape_typst(t)
                }
            }
            InlineContent::Image { name, .. } => {
                // No embedded-image support in the PDF exporter yet (Typst would need the image
                // bytes registered as a virtual file); fall back to the image's name as literal
                // text, exactly as `render_raw_text`-style fallbacks do elsewhere for a
                // plain-text rendering of an unsupported inline kind.
                escape_typst(name)
            }
            InlineContent::Empty => String::new(),
        };

        if text.is_empty() {
            continue;
        }

        let mut formatted = text;

        if !is_monospace {
            if elem.fmt_font_bold == Some(true) {
                formatted = format!("*{formatted}*");
            }
            if elem.fmt_font_italic == Some(true) {
                formatted = format!("_{formatted}_");
            }
            if elem.fmt_font_underline == Some(true) {
                formatted = format!("#underline[{formatted}]");
            }
            if elem.fmt_font_strikeout == Some(true) {
                formatted = format!("#strike[{formatted}]");
            }
            match elem.fmt_vertical_alignment {
                Some(CharVerticalAlignment::SuperScript) => {
                    formatted = format!("#super[{formatted}]");
                }
                Some(CharVerticalAlignment::SubScript) => {
                    formatted = format!("#sub[{formatted}]");
                }
                _ => {}
            }
        }
        if let Some(ref href) = elem.fmt_anchor_href
            && !href.is_empty()
        {
            formatted = format!("#link(\"{}\")[{formatted}]", escape_typst_string(href));
        }

        out.push_str(&formatted);
    }

    out
}

/// The block's text with all inline formatting stripped — just the concatenated literal text of
/// its format runs (image segments contribute nothing). Used for code blocks, whose content must
/// go through [`escape_typst_string`] (a string-literal escape) rather than [`escape_typst`]
/// (markup escape), and must never carry inline marks.
fn raw_block_text(store: &Store, block: &Block) -> String {
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

// ─────────────────────────── Table rendering ───────────────────────────

/// Render the table `table_id` as a Typst `#table(..)`, including its cells' content. Reads
/// `Table`/`TableCell`/`Frame`/`Block` straight off the store's public entity maps — no
/// transaction/uow needed for a read (see `html_render`'s module doc for the same idiom).
///
/// Ports `render_table_latex`'s full covered-grid occupancy tracking rather than a naive flat
/// cell list: a position covered by a previous cell's row/column span emits **nothing** (not
/// even an empty filler) — which is also exactly what Typst's own `table` auto-placement expects
/// of a flat cell list (verified against the pinned Typst 0.15: its auto-placement already skips
/// positions covered by an explicit `rowspan`/`colspan`, so emitting a filler there would shift
/// every subsequent cell by one position). A position that is NOT covered but has no `TableCell`
/// entity at all (a genuine gap in the model, not a span) still gets an empty `[]` filler, since
/// Typst's auto-placement has no way to know a gap was intentional.
pub fn render_table_typst(store: &Store, table_id: EntityId) -> Result<String> {
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

    let rows = table.rows as usize;
    let cols = table.columns as usize;
    let mut covered = vec![vec![false; cols]; rows];

    let mut items: Vec<String> = Vec::new();

    for r in 0..rows {
        let mut c = 0;
        while c < cols {
            if covered[r][c] {
                c += 1;
                continue;
            }

            let cell = cells
                .iter()
                .find(|cell| cell.row == r as i64 && cell.column == c as i64);

            if let Some(cell) = cell {
                let content = if let Some(cf_id) = cell.cell_frame {
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
                        let inline = render_inline_typst(store, block);
                        if !inline.is_empty() {
                            cell_parts.push(inline);
                        }
                    }
                    cell_parts.join("#linebreak()")
                } else {
                    String::new()
                };

                // Spans are `i64`; clamp to >= 1 before any `as usize` so a malformed (0 or
                // negative) span can never wrap to a huge `usize` and blow up the coverage grid.
                let row_span = cell.row_span.max(1) as usize;
                let col_span = cell.column_span.max(1) as usize;

                if row_span > 1 || col_span > 1 {
                    let mut span_args: Vec<String> = Vec::new();
                    if col_span > 1 {
                        span_args.push(format!("colspan: {col_span}"));
                    }
                    if row_span > 1 {
                        span_args.push(format!("rowspan: {row_span}"));
                    }
                    items.push(format!("table.cell({})[{content}]", span_args.join(", ")));
                } else {
                    items.push(format!("[{content}]"));
                }

                // Mark spanned cells (both directions) as covered — same bookkeeping as
                // `render_table_latex`/`render_table_docx`.
                for sr in 0..row_span {
                    for sc in 0..col_span {
                        if sr == 0 && sc == 0 {
                            continue;
                        }
                        if r + sr < rows && c + sc < cols {
                            covered[r + sr][c + sc] = true;
                        }
                    }
                }

                c += col_span;
            } else {
                // A genuine gap (no entity at this uncovered position) — Typst's auto-placement
                // still needs *something* here, unlike a span-covered position.
                items.push("[]".to_string());
                c += 1;
            }
        }
    }

    Ok(format!(
        "#table(\n  columns: {cols},\n  {}\n)",
        items.join(",\n  ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FONT: &[u8] = include_bytes!("../tests/assets/DejaVuSerif.ttf");

    // ─── escape_typst ───────────────────────────────────────────

    #[test]
    fn escapes_every_special_character() {
        let cases: &[(char, &str)] = &[
            ('\\', "\\\\"),
            ('*', "\\*"),
            ('_', "\\_"),
            ('`', "\\`"),
            ('#', "\\#"),
            ('$', "\\$"),
            ('<', "\\<"),
            ('>', "\\>"),
            ('@', "\\@"),
            ('~', "\\~"),
            ('[', "\\["),
            (']', "\\]"),
            ('-', "\\-"),
            ('/', "\\/"),
            ('=', "\\="),
            ('+', "\\+"),
        ];
        for (ch, expected) in cases {
            let input = format!("a{ch}b");
            let want = format!("a{expected}b");
            assert_eq!(escape_typst(&input), want, "escaping {ch:?}");
        }
    }

    #[test]
    fn plain_ascii_and_unicode_prose_is_left_untouched() {
        assert_eq!(escape_typst("Hello, world!"), "Hello, world!");
        assert_eq!(escape_typst("héllo wörld — café"), "héllo wörld — café");
        assert_eq!(escape_typst("مرحبا بالعالم"), "مرحبا بالعالم");
    }

    #[test]
    fn straight_quotes_are_never_escaped() {
        // Smart-quote substitution is disabled once in the preamble instead — see
        // `typst_preamble`'s doc comment.
        assert_eq!(
            escape_typst("She said \"hello\" and 'goodbye'."),
            "She said \"hello\" and 'goodbye'."
        );
    }

    #[test]
    fn leading_numbered_list_guard_escapes_only_the_first_period() {
        assert_eq!(escape_typst("12. Go left"), "12\\. Go left");
        assert_eq!(escape_typst("1. one"), "1\\. one");
        assert_eq!(escape_typst("123.456 more"), "123\\.456 more");
    }

    #[test]
    fn leading_numbered_list_guard_does_not_fire_without_a_period() {
        assert_eq!(escape_typst("123 apples"), "123 apples");
    }

    #[test]
    fn leading_numbered_list_guard_does_not_fire_mid_string() {
        // Only a digit run at the very start of the (already-escaped) string is guarded.
        assert_eq!(escape_typst("see step 12. now"), "see step 12. now");
    }

    #[test]
    fn leading_numbered_list_guard_does_not_fire_on_pure_digits() {
        assert_eq!(escape_typst("2024"), "2024");
    }

    /// Author prose containing every dangerous character must compile as inert Typst markup —
    /// i.e. `escape_typst`'s output is never interpreted as code, math, emphasis, a list marker,
    /// or a label, however adversarial the input. This is the security property the whole
    /// escaper exists for.
    #[test]
    fn escaped_adversarial_prose_compiles_without_being_interpreted_as_markup() {
        let adversarial = "#set text(font: \"Comic Sans\") *bold* _italic_ $x^2$ [label] <ref> @cite ~nbsp~ `code` 12. item -dash- /slash/ +plus+ =eq=";
        let escaped = escape_typst(adversarial);
        let options = PdfExportOptions {
            font_bytes: vec![TEST_FONT.to_vec()],
            ..Default::default()
        };
        let markup = format!("{}{escaped}\n", typst_preamble(&options));
        let (pdf, _pages) =
            crate::typst_compile::compile_typst_pdf(&markup, vec![TEST_FONT.to_vec()])
                .expect("adversarial-but-escaped prose must compile as plain text");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    // ─── typst_preamble ───────────────────────────────────────────

    #[test]
    fn empty_preamble_when_include_preamble_is_false() {
        let options = PdfExportOptions {
            include_preamble: false,
            ..Default::default()
        };
        assert_eq!(typst_preamble(&options), "");
    }

    #[test]
    fn default_preamble_compiles() {
        let options = PdfExportOptions {
            font_bytes: vec![TEST_FONT.to_vec()],
            ..Default::default()
        };
        let markup = format!("{}Hello, world.\n", typst_preamble(&options));
        let (pdf, _pages) =
            crate::typst_compile::compile_typst_pdf(&markup, vec![TEST_FONT.to_vec()])
                .expect("default preamble must compile");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn preamble_with_title_author_lang_and_first_line_indent_compiles() {
        let options = PdfExportOptions {
            font_bytes: vec![TEST_FONT.to_vec()],
            title: Some("A \"Quoted\" Title".to_string()),
            author: Some("Jane \\ Doe".to_string()),
            lang: Some("fr".to_string()),
            first_line_indent_mm: Some(5.0),
            paragraph_spacing_pt: Some(6.0),
            base_rtl: true,
            ..Default::default()
        };
        let markup = format!("{}Bonjour le monde.\n", typst_preamble(&options));
        let (pdf, _pages) =
            crate::typst_compile::compile_typst_pdf(&markup, vec![TEST_FONT.to_vec()])
                .expect("preamble with metadata must compile");
        assert!(pdf.starts_with(b"%PDF-"));
    }
}
