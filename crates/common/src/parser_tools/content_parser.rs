use crate::entities::{Alignment, ListStyle, MarkerType, SemanticRole, TextDirection};
use crate::parser_tools::djot_options::DjotImportOptions;

/// An inline image recovered by a parser, before it becomes an `ImageAnchor`.
///
/// `src` is whatever the source document pointed at — a relative path, a bare
/// name, a URL. Parsers do not resolve it; that is the embedding application's
/// job, and this crate never touches the filesystem.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedImage {
    pub src: String,
    pub alt: String,
    /// Display size in pixels, `0` when the source did not state one.
    ///
    /// Djot and HTML both carry these as attributes (`{width=800}`,
    /// `<img width=…>`); Markdown has no syntax for them, so a Markdown import
    /// leaves both zero and the caller supplies intrinsic dimensions.
    pub width: i64,
    pub height: i64,
}

/// A parsed inline span with formatting info.
///
/// A span carries *either* text or an image, never both: an image span's `text`
/// is empty and its description lives in [`ParsedImage::alt`], so alt text
/// cannot leak into the block's prose (and therefore cannot be counted as
/// manuscript words or matched by a search).
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
    /// Set when this span is an inline image rather than text.
    pub image: Option<ParsedImage>,
    /// Set when this span is a footnote *reference* rather than text — the
    /// label naming the note, never the number a reader sees.
    pub footnote_ref: Option<String>,
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
    /// A footnote definition: the label its references name, and the blocks
    /// making up its body.
    ///
    /// Separate from `Block` because a definition is not part of the flow it was
    /// written in — it belongs wherever the output format puts notes, which the
    /// importer expresses by giving it a detached frame of its own.
    FootnoteDefinition {
        label: String,
        blocks: Vec<ParsedBlock>,
    },
}

impl ParsedElement {
    /// Extract blocks, flattening tables into one block per cell.
    /// Use when table structure is not needed.
    pub fn flatten_to_blocks(elements: Vec<ParsedElement>) -> Vec<ParsedBlock> {
        let mut blocks = Vec::new();
        for elem in elements {
            match elem {
                ParsedElement::Block(b) => blocks.push(b),
                // A definition is not part of the flow it was written in — it
                // belongs wherever the format puts notes. Flattening it in
                // would splice a note's body into the prose at the point the
                // definition happened to be typed.
                ParsedElement::FootnoteDefinition { .. } => {}
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
                                page_break_before: None,
                                direction: None,
                                background_color: None,
                                alignment: None,
                                top_margin: None,
                                text_indent: None,
                                semantic_role: None,
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
                page_break_before: None,
                direction: None,
                background_color: None,
                alignment: None,
                top_margin: None,
                text_indent: None,
                semantic_role: None,
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
    /// Start this block on a new page (djot `{page_break_before=true}`). Maps to
    /// `Block.fmt_page_break_before`. `None` when absent.
    pub page_break_before: Option<bool>,
    pub direction: Option<TextDirection>,
    pub background_color: Option<String>,
    /// Paragraph alignment (djot `{alignment=left|right|center|justify}`). Maps
    /// to `Block.fmt_alignment`. `None` when no alignment attribute is present.
    pub alignment: Option<Alignment>,
    /// This block's own space-above (djot `{top_margin=<int>}`). Maps to
    /// `Block.fmt_top_margin` and overrides the document-wide paragraph
    /// spacing for this block alone. `None` when absent.
    pub top_margin: Option<i64>,
    /// This block's own first-line indent (djot `{text_indent=<int>}`). Maps to
    /// `Block.fmt_text_indent` and overrides the document-wide first-line
    /// indent for this block alone. `None` when absent.
    pub text_indent: Option<i64>,
    /// The enclosing blockquote's semantic role (djot `{semantic_role=epigraph}`),
    /// written on the quote's first block because block attributes are the only channel
    /// djot offers. The importer lifts it onto the `Frame`, where it belongs.
    pub semantic_role: Option<SemanticRole>,
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
            && self.page_break_before.is_none()
            && self.direction.is_none()
            && self.background_color.is_none()
            && self.alignment.is_none()
            && self.top_margin.is_none()
            && self.text_indent.is_none()
    }
}

// ─── Markdown parsing ────────────────────────────────────────────────

/// Labels referenced as `[^label]` in `markdown` with no `[^label]: ...`
/// definition anywhere in the document.
///
/// pulldown-cmark's default footnote mode (`Options::ENABLE_FOOTNOTES`, i.e.
/// GitHub's syntax) only emits `Event::FootnoteReference` for a label some
/// `Tag::FootnoteDefinition` actually defines (verified against 0.13) — an
/// undefined reference silently decomposes into three ordinary `Text` events
/// (`"["`, `"^label"`, `"]"`) instead, indistinguishable from a reader having
/// typed literal brackets. `Options::ENABLE_OLD_FOOTNOTES` recognises it, but
/// as `parse_markdown`'s doc comment explains, trades away multi-paragraph
/// definition bodies to get there. Neither flag alone gives both, so
/// `parse_markdown` hands pulldown-cmark a synthetic empty definition for
/// every such label instead — real enough for it to recognise the reference,
/// thrown away again before `elements` is returned.
///
/// Finding "every such label" is two parts: which labels pulldown-cmark's own
/// block scanner (a first pass, at the caller's `options`) already recognises
/// as truly defined, and which `[^…]`-shaped spans exist in the raw text at
/// all. The second part is a plain scan, not a parser — it can over-match
/// (inside a code span, inside a fenced block, a coincidental `[^x]:` in the
/// middle of a sentence that real block parsing would never treat as a
/// definition), but over-matching only ever produces an unused synthetic
/// definition: pulldown-cmark's real, correct parse of the augmented text is
/// what decides whether any span actually becomes a reference, and
/// `parse_markdown` drops every synthesized definition regardless of whether
/// that happened.
fn dangling_footnote_labels(
    markdown: &str,
    options: pulldown_cmark::Options,
) -> std::collections::BTreeSet<String> {
    use pulldown_cmark::{Event, Parser, Tag};

    let mut defined: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for event in Parser::new_ext(markdown, options) {
        if let Event::Start(Tag::FootnoteDefinition(label)) = event {
            defined.insert(label.to_string());
        }
    }

    let mut referenced: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let bytes = markdown.as_bytes();
    let mut search_from = 0usize;
    while let Some(rel) = markdown[search_from..].find("[^") {
        let open = search_from + rel;
        let label_start = open + 2;
        let Some(close_rel) = markdown[label_start..].find(']') else {
            break;
        };
        let close = label_start + close_rel;
        let label = &markdown[label_start..close];
        // `]:` immediately after is definition syntax, not a reference —
        // pulldown-cmark's own scan above already supplied the ground truth
        // for which labels are truly defined; this only needs to avoid
        // counting a definition's own label as a "reference" candidate.
        let looks_like_definition = bytes.get(close + 1) == Some(&b':');
        if !label.is_empty() && !looks_like_definition && !label.chars().any(char::is_whitespace) {
            referenced.insert(label.to_string());
        }
        search_from = close + 1;
    }

    referenced.difference(&defined).cloned().collect()
}

pub fn parse_markdown(markdown: &str) -> Vec<ParsedElement> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    // ENABLE_FOOTNOTES is required for pulldown-cmark to parse `[^label]`
    // references and `[^label]: body` definitions at all — without it both
    // fall through as plain link-reference-shaped text. Deliberately NOT
    // `ENABLE_OLD_FOOTNOTES` too: that variant makes a reference with no
    // definition survive (see `dangling_footnote_labels`), but in exchange
    // breaks multi-paragraph definition bodies — a blank line before an
    // indented continuation escapes the definition and becomes a sibling
    // code block, which is exactly the shape this crate's own Markdown
    // exporter writes for a note with more than one paragraph. Getting both
    // is `dangling_footnote_labels`'s job, not an `Options` flag's.
    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;

    // See `dangling_footnote_labels`: pulldown-cmark's GFM-style footnotes
    // only recognise `[^label]` as a reference when some definition exists
    // for it anywhere in the document, so an undefined one — the normal
    // state for a host that owns note bodies itself — has to be given a
    // throwaway definition before parsing, or it silently becomes the
    // literal text "[^label]" instead of surviving as a reference.
    let dangling = dangling_footnote_labels(markdown, options);
    let augmented_owner;
    let source: &str = if dangling.is_empty() {
        markdown
    } else {
        augmented_owner = dangling
            .iter()
            .fold(markdown.to_string(), |mut acc, label| {
                acc.push_str("\n\n[^");
                acc.push_str(label);
                acc.push_str("]:\n");
                acc
            });
        &augmented_owner
    };
    let parser = Parser::new_ext(source, options);

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
    // Set between an image's Start and End; its alt text arrives as Text events.
    let mut pending_image: Option<ParsedImage> = None;

    // The label and element index of the footnote definition currently open,
    // if any. Mirrors `parse_djot`'s `footnote_open`: a definition's body is
    // ordinary block content, parsed by the same machinery as everything
    // else, then lifted back out at `End` into its own top-level element so
    // it never becomes part of the flow it was written in. CommonMark
    // footnote definitions cannot nest, so one slot suffices.
    let mut footnote_open: Option<(String, usize)> = None;

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
                        page_break_before: None,
                        direction: None,
                        background_color: None,
                        alignment: None,
                        top_margin: None,
                        text_indent: None,
                        semantic_role: None,
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
                    page_break_before: None,
                    direction: None,
                    background_color: None,
                    alignment: None,
                    top_margin: None,
                    text_indent: None,
                    semantic_role: None,
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
                        page_break_before: None,
                        direction: None,
                        background_color: None,
                        alignment: None,
                        top_margin: None,
                        text_indent: None,
                        semantic_role: None,
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
                        page_break_before: None,
                        direction: None,
                        background_color: None,
                        alignment: None,
                        top_margin: None,
                        text_indent: None,
                        semantic_role: None,
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
                    page_break_before: None,
                    direction: None,
                    background_color: None,
                    alignment: None,
                    top_margin: None,
                    text_indent: None,
                    semantic_role: None,
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
            // Markdown has no syntax for display size, so width/height stay 0
            // and the caller supplies the image's intrinsic dimensions.
            Event::Start(Tag::Image { dest_url, .. }) => {
                pending_image = Some(ParsedImage {
                    src: dest_url.to_string(),
                    alt: String::new(),
                    width: 0,
                    height: 0,
                });
            }
            Event::End(TagEnd::Image) => {
                if let Some(image) = pending_image.take() {
                    let span = ParsedSpan {
                        text: String::new(),
                        bold,
                        italic,
                        underline: false,
                        strikeout,
                        code: false,
                        superscript: false,
                        subscript: false,
                        link_href: link_href.clone(),
                        image: Some(image),
                        footnote_ref: None,
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
            }
            Event::Text(text) => {
                // Inside an image this is its alt text, which pulldown-cmark
                // emits as an ordinary Text event. Without this guard it fell
                // through and landed in the paragraph as prose — the image was
                // dropped and its description silently became manuscript text.
                if let Some(img) = pending_image.as_mut() {
                    img.alt.push_str(&text);
                    continue;
                }
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
                    image: None,
                    footnote_ref: None,
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
                    image: None,
                    footnote_ref: None,
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
                    image: None,
                    footnote_ref: None,
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
                    page_break_before: None,
                    direction: None,
                    background_color: None,
                    alignment: None,
                    top_margin: None,
                    text_indent: None,
                    semantic_role: None,
                }));
            }
            Event::Start(Tag::BlockQuote(_)) => {
                blockquote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                blockquote_depth = blockquote_depth.saturating_sub(1);
            }
            // ── Footnote definitions ──
            //
            // `[^label]: body` opens a block-level container; its body is
            // ordinary paragraphs/lists/etc., pushed onto `elements` by the
            // ordinary machinery above. `End` drains everything pushed since
            // `Start` back out into one `FootnoteDefinition`, exactly as
            // `parse_djot` does for the same `[^label]: body` syntax.
            // Flushing any spans left open by an interrupted block first
            // matches every other container boundary in this parser
            // (List/Item/BlockQuote).
            Event::Start(Tag::FootnoteDefinition(label)) => {
                if !current_spans.is_empty() {
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
                        page_break_before: None,
                        direction: None,
                        background_color: None,
                        alignment: None,
                        top_margin: None,
                        text_indent: None,
                        semantic_role: None,
                    }));
                }
                footnote_open = Some((label.to_string(), elements.len()));
            }
            Event::End(TagEnd::FootnoteDefinition) => {
                if !current_spans.is_empty() {
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
                        page_break_before: None,
                        direction: None,
                        background_color: None,
                        alignment: None,
                        top_margin: None,
                        text_indent: None,
                        semantic_role: None,
                    }));
                }
                if let Some((label, start)) = footnote_open.take() {
                    let blocks: Vec<ParsedBlock> = elements
                        .drain(start..)
                        .filter_map(|e| match e {
                            ParsedElement::Block(b) => Some(b),
                            // A table inside a footnote is not representable
                            // as note content, same limitation as djot.
                            _ => None,
                        })
                        .collect();
                    elements.push(ParsedElement::FootnoteDefinition { label, blocks });
                }
            }
            // A footnote reference. pulldown-cmark emits this whether or not
            // a matching `[^label]:` definition exists anywhere in the
            // document — a dangling reference (the normal state for a host
            // that owns note bodies itself) must survive just the same,
            // mirroring `parse_djot`'s `E::FootnoteReference` handling.
            Event::FootnoteReference(label) => {
                let span = ParsedSpan {
                    text: String::new(),
                    bold,
                    italic,
                    underline: false,
                    strikeout,
                    code: false,
                    superscript: false,
                    subscript: false,
                    link_href: link_href.clone(),
                    image: None,
                    footnote_ref: Some(label.to_string()),
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
            page_break_before: None,
            direction: None,
            background_color: None,
            alignment: None,
            top_margin: None,
            text_indent: None,
            semantic_role: None,
        }));
    }

    // Drop the throwaway definitions `dangling_footnote_labels` asked for —
    // they exist only to make pulldown-cmark recognise the reference as
    // real, never to become a note the document owns. `elements` cannot
    // become empty from this: a label reached `dangling` only via a
    // reference actually present in `markdown`, so its surrounding block
    // survives even with the synthetic definition gone.
    if !dangling.is_empty() {
        elements.retain(
            |e| !matches!(e, ParsedElement::FootnoteDefinition { label, .. } if dangling.contains(label)),
        );
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
            page_break_before: None,
            direction: None,
            background_color: None,
            alignment: None,
            top_margin: None,
            text_indent: None,
            semantic_role: None,
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
    page_break_before: Option<bool>,
    direction: Option<TextDirection>,
    background_color: Option<String>,
}

/// Parse relevant CSS properties from an inline style string.
/// Handles: line-height, white-space, break-before/page-break-before, direction,
/// background-color.
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
                // CSS3 `break-before` and its CSS2 predecessor `page-break-before` mean
                // the same thing; both are read because both are written (browsers still
                // want the legacy spelling, and so do most EPUB engines).
                "break-before" | "page-break-before" => {
                    result.page_break_before = match val.to_ascii_lowercase().as_str() {
                        "page" | "always" | "left" | "right" | "recto" | "verso" => Some(true),
                        "avoid" | "auto" => Some(false),
                        _ => None,
                    };
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

/// Build an inline-image span from an `<img>` element, if it has a usable
/// source.
///
/// `<img>` was matched by none of the HTML walker's three tag dispatches, so it
/// fell into their wildcard arms and was dropped whole — silently, and unlike
/// Markdown not even leaving its alt text behind. That is the path a browser or
/// Word paste travels.
fn html_img_span(el: &scraper::node::Element, link_href: Option<String>) -> Option<ParsedSpan> {
    let src = el.attr("src")?;
    if src.is_empty() {
        return None;
    }
    let dim = |name: &str| -> i64 {
        el.attr(name)
            .and_then(|v| v.trim().trim_end_matches("px").parse::<i64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(0)
    };
    Some(ParsedSpan {
        text: String::new(),
        link_href,
        image: Some(ParsedImage {
            src: src.to_string(),
            alt: el.attr("alt").unwrap_or_default().to_string(),
            width: dim("width"),
            height: dim("height"),
        }),
        ..Default::default()
    })
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
                            image: None,
                            footnote_ref: None,
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
                        "img" => {
                            if let Some(span) = html_img_span(el, new_state.link_href.clone()) {
                                spans.push(span);
                            }
                            continue;
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
                        page_break_before: None,
                        direction: None,
                        background_color: None,
                        alignment: None,
                        top_margin: None,
                        text_indent: None,
                        semantic_role: None,
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
                            page_break_before: css.page_break_before,
                            direction: css.direction,
                            background_color: css.background_color,
                            alignment: None,
                            top_margin: None,
                            text_indent: None,
                            semantic_role: None,
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
                            image: None,
                            footnote_ref: None,
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
                        page_break_before: None,
                        direction: None,
                        background_color: None,
                        alignment: None,
                        top_margin: None,
                        text_indent: None,
                        semantic_role: None,
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
                            image: None,
                            footnote_ref: None,
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
                        "img" => {
                            if let Some(span) = html_img_span(el, new_state.link_href.clone()) {
                                spans.push(span);
                            }
                            continue;
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
            page_break_before: None,
            direction: None,
            background_color: None,
            alignment: None,
            top_margin: None,
            text_indent: None,
            semantic_role: None,
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
            page_break_before: None,
            direction: None,
            background_color: None,
            alignment: None,
            top_margin: None,
            text_indent: None,
            semantic_role: None,
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
pub fn format_runs_from_spans(spans: &[ParsedSpan], is_code_block: bool) -> ParsedInline {
    use crate::format_runs::{
        CharacterFormat, FootnoteRefAnchor, FormatRun, ImageAnchor, coalesce_in_place,
    };

    let mut plain_text = String::new();
    let mut runs: Vec<FormatRun> = Vec::new();
    let mut images: Vec<ImageAnchor> = Vec::new();
    let mut footnote_refs: Vec<FootnoteRefAnchor> = Vec::new();
    let default = CharacterFormat::default();

    for span in spans {
        let byte_start = plain_text.len() as u32;

        if let Some(label) = &span.footnote_ref {
            // A reference occupies one U+FFFC, exactly as an image does, so
            // every downstream offset treats the two alike.
            plain_text.push('\u{FFFC}');
            // Raised, always — whatever the surrounding run is doing.
            //
            // A footnote marker is superscript by definition, in every
            // typographic tradition and in every reader that renders djot. The
            // ambient `superscript` flag the span carries is the *prose's*, and
            // prose is not superscript, so taking it verbatim sets a note's
            // number on the baseline in the middle of a sentence — which reads
            // as a stray digit the writer typed rather than as a reference.
            //
            // It is set here, on the anchor, rather than at render time so that
            // every consumer agrees: the editor raises it, the exporters that
            // carry character formatting carry it, and the djot writer knows to
            // emit `[^label]` *without* wrapping it in `^…^` (see
            // `a_reference_is_not_wrapped_in_superscript_markers`).
            let mut format = character_format_from_span(span, is_code_block);
            format.vertical_alignment = Some(crate::entities::CharVerticalAlignment::SuperScript);
            footnote_refs.push(FootnoteRefAnchor {
                byte_offset: byte_start,
                label: label.clone(),
                format,
            });
            continue;
        }

        if let Some(image) = &span.image {
            // An image occupies one U+FFFC in the text, exactly as
            // `insert_image` mirrors into the rope, so every downstream offset
            // calculation treats a parsed image and an inserted one alike.
            plain_text.push('\u{FFFC}');
            images.push(ImageAnchor {
                byte_offset: byte_start,
                name: image.src.clone(),
                alt: image.alt.clone(),
                width: image.width,
                height: image.height,
                quality: 100,
                format: character_format_from_span(span, is_code_block),
            });
            continue;
        }

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
    ParsedInline {
        plain_text,
        runs,
        images,
        footnote_refs,
    }
}

/// The three parallel things a block stores, as recovered from parsed spans.
///
/// Returned as a struct rather than a tuple because it grew a third member
/// (images) after nine call sites already destructured a pair — and every one
/// of those sites has to decide what to do with images, so a silent
/// tuple-arity change would have been the wrong kind of easy.
#[derive(Debug, Clone, Default)]
pub struct ParsedInline {
    pub plain_text: String,
    pub runs: Vec<crate::format_runs::FormatRun>,
    pub images: Vec<crate::format_runs::ImageAnchor>,
    pub footnote_refs: Vec<crate::format_runs::FootnoteRefAnchor>,
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

/// Optional block-level style attributes carried on a djot block through its
/// `{key=value}` block attributes. All `None` when the block has no such
/// attributes (or they were filtered out by [`DjotImportOptions`]).
#[derive(Debug, Clone, Default)]
struct DjotBlockStyle {
    alignment: Option<Alignment>,
    line_height: Option<i64>,
    non_breakable_lines: Option<bool>,
    page_break_before: Option<bool>,
    direction: Option<TextDirection>,
    background_color: Option<String>,
    top_margin: Option<i64>,
    text_indent: Option<i64>,
    semantic_role: Option<SemanticRole>,
}

impl DjotBlockStyle {
    /// Overlay the `Some` fields of `other` onto `self`, leaving `self`'s
    /// existing values for any field `other` does not set. Used to combine a
    /// heading's enclosing-`Section` attributes with any on the heading itself.
    fn merge_from(&mut self, other: DjotBlockStyle) {
        if other.alignment.is_some() {
            self.alignment = other.alignment;
        }
        if other.line_height.is_some() {
            self.line_height = other.line_height;
        }
        if other.non_breakable_lines.is_some() {
            self.non_breakable_lines = other.non_breakable_lines;
        }
        if other.page_break_before.is_some() {
            self.page_break_before = other.page_break_before;
        }
        if other.direction.is_some() {
            self.direction = other.direction;
        }
        if other.background_color.is_some() {
            self.background_color = other.background_color;
        }
        if other.top_margin.is_some() {
            self.top_margin = other.top_margin;
        }
        if other.text_indent.is_some() {
            self.text_indent = other.text_indent;
        }
        if other.semantic_role.is_some() {
            self.semantic_role = other.semantic_role.clone();
        }
    }
}

/// Read the round-tripped block-style attributes off a djot block's
/// [`jotdown::Attributes`], honouring the import [`DjotImportOptions`]. Keys are
/// the model field names (`alignment`, `line_height`, `direction`,
/// `non_breakable_lines`, `page_break_before`, `background_color`, `top_margin`,
/// `text_indent`, `semantic_role`); unrecognised values are ignored.
fn block_attrs_to_style(attrs: &jotdown::Attributes, opts: &DjotImportOptions) -> DjotBlockStyle {
    let mut style = DjotBlockStyle::default();

    if opts.alignment
        && let Some(v) = attrs.get_value("alignment")
    {
        style.alignment = match v.to_string().as_str() {
            "left" => Some(Alignment::Left),
            "right" => Some(Alignment::Right),
            "center" => Some(Alignment::Center),
            "justify" => Some(Alignment::Justify),
            _ => None,
        };
    }
    if opts.line_height
        && let Some(v) = attrs.get_value("line_height")
    {
        style.line_height = v.to_string().parse::<i64>().ok();
    }
    if opts.direction
        && let Some(v) = attrs.get_value("direction")
    {
        style.direction = match v.to_string().as_str() {
            "ltr" => Some(TextDirection::LeftToRight),
            "rtl" => Some(TextDirection::RightToLeft),
            _ => None,
        };
    }
    if opts.non_breakable_lines
        && let Some(v) = attrs.get_value("non_breakable_lines")
    {
        style.non_breakable_lines = match v.to_string().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
    }
    if opts.page_break_before
        && let Some(v) = attrs.get_value("page_break_before")
    {
        style.page_break_before = match v.to_string().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
    }
    if opts.background_color
        && let Some(v) = attrs.get_value("background_color")
    {
        style.background_color = Some(v.to_string());
    }
    if opts.top_margin
        && let Some(v) = attrs.get_value("top_margin")
    {
        style.top_margin = v.to_string().parse::<i64>().ok();
    }
    if opts.text_indent
        && let Some(v) = attrs.get_value("text_indent")
    {
        style.text_indent = v.to_string().parse::<i64>().ok();
    }
    if opts.semantic_role
        && let Some(v) = attrs.get_value("semantic_role")
    {
        style.semantic_role = match v.to_string().as_str() {
            "epigraph" => Some(SemanticRole::Epigraph),
            // An unknown role is dropped, not guessed at — the same way an unknown
            // alignment value above is. A future role read by an older build then
            // degrades to a plain blockquote, which is what it looks like anyway.
            _ => None,
        };
    }

    style
}

/// Push a finished block into `elements`, applying the djot block-level fields
/// plus any round-tripped block-style attributes carried in `style`.
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
    style: DjotBlockStyle,
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
        line_height: style.line_height,
        non_breakable_lines: style.non_breakable_lines,
        page_break_before: style.page_break_before,
        direction: style.direction,
        background_color: style.background_color,
        alignment: style.alignment,
        top_margin: style.top_margin,
        text_indent: style.text_indent,
        semantic_role: style.semantic_role.clone(),
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
/// Standalone paragraphs and headings additionally carry the optional
/// block-style attributes selected by `options` — paragraph alignment, line
/// height, text direction, non-breakable lines and background color — read from
/// djot `{key=value}` block attributes (see [`DjotImportOptions`]). List items,
/// code blocks and table cells normalise their block styling away.
///
/// Known model limitations (normalised, not preserved on round-trip):
/// ordered-list start number, table column alignment, and list tight/loose.
pub fn parse_djot(djot: &str, options: &DjotImportOptions) -> Vec<ParsedElement> {
    use jotdown::{Container as C, Event as E, ListKind, Parser};

    let mut elements: Vec<ParsedElement> = Vec::new();
    let mut current_spans: Vec<ParsedSpan> = Vec::new();
    let mut current_heading: Option<i64> = None;
    let mut is_code_block = false;
    let mut code_language: Option<String> = None;
    let mut blockquote_depth: u32 = 0;
    // Block-style attributes captured from a standalone paragraph/heading's djot
    // `{…}` block attributes, consumed when that block is flushed.
    let mut pending_style = DjotBlockStyle::default();

    // Inline formatting state.
    let mut bold = false;
    let mut italic = false;
    let mut underline = false;
    let mut strikeout = false;
    let mut code = false;
    let mut superscript = false;
    let mut subscript = false;
    let mut link_href: Option<String> = None;
    // Set between an image's Start and End. Its alt text arrives as ordinary
    // `Str` events in between, so it has to be captured rather than emitted.
    let mut pending_image: Option<ParsedImage> = None;

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

    // The label and element index of the footnote definition currently open,
    // if any. Djot has no footnote inside a footnote, so one slot is enough
    // where the dropped containers need a depth counter.
    let mut footnote_open: Option<(String, usize)> = None;

    // Push one inline span carrying the current formatting state into the
    // active sink (table cell or block). A macro (not a closure) to avoid
    // borrowing `current_spans`/`current_cell_spans` across the formatting
    // state reads.
    macro_rules! push_text {
        ($t:expr) => {{
            // Alt text belongs to the image, not to the paragraph. While an
            // image is open every text event is diverted into its description,
            // which is what keeps a photo's caption out of the manuscript's
            // word count and out of the search corpus.
            if let Some(img) = pending_image.as_mut() {
                img.alt.push_str(($t).as_ref());
            } else {
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
                    image: None,
                    footnote_ref: None,
                };
                if in_table_cell {
                    current_cell_spans.push(sp);
                } else {
                    current_spans.push(sp);
                }
            }
        }};
    }

    // Push a completed inline image span into the active sink.
    macro_rules! push_image {
        ($img:expr) => {{
            let sp = ParsedSpan {
                text: String::new(),
                bold,
                italic,
                underline,
                strikeout,
                code,
                superscript,
                subscript,
                link_href: link_href.clone(),
                image: Some($img),
                footnote_ref: None,
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
                    DjotBlockStyle::default(),
                );
            }
            let (style, prefix, suffix) = list_stack.last().cloned().unwrap_or((
                ListStyle::Disc,
                String::new(),
                String::new(),
            ));
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
            E::Start(C::Section { .. }, attrs) => {
                // A heading's block attributes attach to its enclosing Section,
                // not the heading itself; capture them for the heading's flush.
                if list_stack.is_empty() {
                    pending_style.merge_from(block_attrs_to_style(&attrs, options));
                }
            }
            E::End(C::Section { .. }) => {}
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
                        DjotBlockStyle::default(),
                    );
                }
                cur_list_style = None;
                cur_marker = None;
            }

            // ── Headings, paragraphs, code blocks ──
            E::Start(C::Heading { level, .. }, attrs) => {
                current_heading = Some(level as i64);
                // The block-style attributes live on the enclosing Section;
                // merge any placed directly on the heading without clearing them.
                pending_style.merge_from(block_attrs_to_style(&attrs, options));
            }
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
                    std::mem::take(&mut pending_style),
                );
            }
            E::Start(C::Paragraph, attrs) => {
                current_heading = None;
                // Block attributes only apply to standalone paragraphs;
                // list-item paragraphs normalise their styling away (matching
                // the exporter).
                pending_style = if list_stack.is_empty() {
                    block_attrs_to_style(&attrs, options)
                } else {
                    DjotBlockStyle::default()
                };
            }
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
                        std::mem::take(&mut pending_style),
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
                    DjotBlockStyle::default(),
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
            // Inline images. Djot writes display size as inline attributes
            // (`![alt](src){width=800 height=600}`), which jotdown hands over
            // on the `Start` event — verified against jotdown 0.10, including
            // quoted values and images mid-sentence.
            E::Start(C::Image(src, _), attrs) => {
                let attr_num = |key: &str| -> i64 {
                    attrs
                        .get_value(key)
                        .map(|v| v.to_string())
                        .and_then(|v| v.trim().parse::<i64>().ok())
                        .filter(|n| *n > 0)
                        .unwrap_or(0)
                };
                pending_image = Some(ParsedImage {
                    src: src.to_string(),
                    alt: String::new(),
                    width: attr_num("width"),
                    height: attr_num("height"),
                });
            }
            E::End(C::Image(..)) => {
                if let Some(img) = pending_image.take() {
                    push_image!(img);
                }
            }

            // ── Footnote definitions ──
            //
            // The body is ordinary block content, so it is parsed by the
            // ordinary machinery: flush whatever inline run is open, note where
            // this definition's blocks start, and let them accumulate. `End`
            // lifts them back out. Nesting cannot occur — djot has no footnote
            // inside a footnote — so one mark suffices where the dropped
            // containers below need a depth counter.
            E::Start(C::Footnote { label }, _) => {
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
                        DjotBlockStyle::default(),
                    );
                }
                footnote_open = Some((label.to_string(), elements.len()));
            }
            E::End(C::Footnote { .. }) => {
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
                        DjotBlockStyle::default(),
                    );
                }
                if let Some((label, start)) = footnote_open.take() {
                    let blocks: Vec<ParsedBlock> = elements
                        .drain(start..)
                        .filter_map(|e| match e {
                            ParsedElement::Block(b) => Some(b),
                            // A table inside a footnote is not representable as
                            // note content; its cells would have to become
                            // blocks and lose their structure either way.
                            _ => None,
                        })
                        .collect();
                    elements.push(ParsedElement::FootnoteDefinition { label, blocks });
                }
            }

            // ── Unrepresentable containers: drop the entire subtree ──
            E::Start(
                C::Math { .. }
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
                        pending_style.clone(),
                    );
                }
            }
            // A footnote reference. jotdown emits this purely syntactically —
            // it never checks that a matching `[^label]:` exists anywhere — so a
            // reference whose definition lives outside this document (the normal
            // state for a host that owns note bodies itself) arrives here just
            // the same, and must survive.
            E::FootnoteReference(label) => {
                let sp = ParsedSpan {
                    text: String::new(),
                    bold,
                    italic,
                    underline,
                    strikeout,
                    code,
                    superscript,
                    subscript,
                    link_href: link_href.clone(),
                    image: None,
                    footnote_ref: Some(label.to_string()),
                };
                if in_table_cell {
                    current_cell_spans.push(sp);
                } else {
                    current_spans.push(sp);
                }
            }
            // Symbols, escapes, blanklines, thematic breaks and dangling block
            // attributes carry no representable content.
            E::Symbol(_) => {}
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
            std::mem::take(&mut pending_style),
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
            DjotBlockStyle::default(),
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
                // Definitions carry no blockquote nesting of their own; this
                // helper exists for the nesting assertions and never sees one.
                ParsedElement::FootnoteDefinition { .. } => (false, 0),
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
        ParsedElement::flatten_to_blocks(parse_djot(d, &DjotImportOptions::default()))
    }

    fn first_span_with(b: &ParsedBlock, pred: impl Fn(&ParsedSpan) -> bool) -> &ParsedSpan {
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
        let els = parse_djot("> quoted", &DjotImportOptions::default());
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
        let els = parse_djot(
            "| a | b |\n|---|---|\n| c | d |",
            &DjotImportOptions::default(),
        );
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
        assert_eq!(
            b[0].spans
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "para1"
        );
        assert_eq!(
            b[1].spans
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "para2"
        );

        // Fenced div is unwrapped: its content survives, the fence does not.
        let d = blocks(":::\ninside\n:::");
        let joined: String = d
            .iter()
            .flat_map(|b| b.spans.iter())
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(joined, "inside");

        // Inline math content is dropped, surrounding text kept.
        let m = blocks("before $`E=mc^2` after");
        let joined: String = m
            .iter()
            .flat_map(|b| b.spans.iter())
            .map(|s| s.text.as_str())
            .collect();
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

    #[test]
    fn block_attributes_parse_into_block() {
        let b = blocks(
            "{alignment=center line_height=1500 direction=rtl non_breakable_lines=true background_color=\"#ff0000\"}\nhello",
        );
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].alignment, Some(Alignment::Center));
        assert_eq!(b[0].line_height, Some(1500));
        assert_eq!(b[0].direction, Some(TextDirection::RightToLeft));
        assert_eq!(b[0].non_breakable_lines, Some(true));
        assert_eq!(b[0].background_color, Some("#ff0000".to_string()));
    }

    #[test]
    fn spacing_block_attributes_parse_into_block() {
        // `top_margin` / `text_indent` let one block override the document-wide
        // paragraph spacing and first-line indent — what a scene break needs for
        // the paragraph that follows it.
        let b = blocks("{top_margin=24 text_indent=0}\nhello");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].top_margin, Some(24));
        assert_eq!(b[0].text_indent, Some(0));
    }

    #[test]
    fn a_zero_text_indent_is_distinct_from_an_absent_one() {
        // The whole point of the attribute: `Some(0)` means "explicitly no
        // indent", which must not collapse to `None` ("use the document
        // default") — otherwise a scene break could not suppress the indent.
        let explicit = blocks("{text_indent=0}\nhello");
        let absent = blocks("hello");
        assert_eq!(explicit[0].text_indent, Some(0));
        assert_eq!(absent[0].text_indent, None);
        assert!(!explicit[0].is_inline_only());
        assert!(absent[0].is_inline_only());
    }

    #[test]
    fn spacing_block_attributes_respect_import_options() {
        let src = "{top_margin=24 text_indent=0}\nhello";
        let b = ParsedElement::flatten_to_blocks(parse_djot(src, &DjotImportOptions::none()));
        assert_eq!(b[0].top_margin, None);
        assert_eq!(b[0].text_indent, None);
    }

    #[test]
    fn block_attributes_on_heading() {
        let b = blocks("{alignment=right}\n# Title");
        assert_eq!(b[0].heading_level, Some(1));
        assert_eq!(b[0].alignment, Some(Alignment::Right));
    }

    #[test]
    fn block_attributes_respect_import_options() {
        // With every optional attribute disabled, the `{…}` block attributes are
        // parsed and discarded — only the core paragraph survives.
        let src = "{alignment=center line_height=1500}\nhello";
        let b = ParsedElement::flatten_to_blocks(parse_djot(src, &DjotImportOptions::none()));
        assert_eq!(b[0].alignment, None);
        assert_eq!(b[0].line_height, None);
        assert_eq!(
            b[0].spans
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "hello"
        );
    }

    #[test]
    fn list_item_block_attributes_are_dropped() {
        // Block attributes only bind to standalone paragraphs/headings; a list
        // item normalises them away (symmetric with the exporter).
        let b = blocks("{alignment=center}\n- item");
        assert!(b.iter().all(|blk| blk.alignment.is_none()));
    }

    #[test]
    fn unknown_alignment_value_is_ignored() {
        let b = blocks("{alignment=sideways}\nhello");
        assert_eq!(b[0].alignment, None);
    }
}

// ─── Cheap plain-text extraction ─────────────────────────────────────

/// The `U+FFFC OBJECT REPLACEMENT CHARACTER` that stands for a table in the document's
/// text.
///
/// A table is not prose, but it *occupies a position* in the flow: the import mirrors this
/// single sentinel into the rope where the table sits
/// (`rope_helpers::rope_append_table_anchor`), then the cells as ordinary blocks. Anything
/// reconstructing the text a search runs against has to reproduce it, or every offset after
/// the first table is short by the two characters (the sentinel and its separator) that the
/// document really holds there.
pub const TABLE_ANCHOR: &str = "\u{FFFC}";

/// The prose of a Djot document, with no entities, no store, and no threads.
///
/// [`parse_djot`] and [`ParsedElement::flatten_to_blocks`] were both already `pub`;
/// nothing chained them. This does, and that is the whole trick: it stops at the
/// *parse*, where a full import goes on to create a `Block` entity per paragraph, list
/// item and table cell, mirror each into the rope, and write its format runs.
///
/// # Why a project-wide search needs this
///
/// A host app searching a manuscript must ask "does this scene contain that word" of
/// **thousands** of Djot rows, on every keystroke. Doing that by importing each one into
/// a document is not a slow feature, it is a frozen app.
///
/// And searching the Djot *source* instead — the tempting shortcut — is simply wrong:
/// the source is markup. `http` matches inside a link's URL, `*` matches an emphasis
/// marker, and an occurrence count taken from the source does not agree with what a
/// replace re-derives inside the parsed document. Where a replace guards itself with
/// "the text moved under me, skip this field", a count taken from markup makes that
/// guard fire on perfectly good rows.
///
/// # The contract
///
/// The result is **byte-identical to the text the document searches** for the same Djot:
/// each block's spans concatenated, blocks joined by a single `\n`, and a table announced
/// by its [`TABLE_ANCHOR`] sentinel — exactly the string the import mirrors into the rope
/// and that `build_full_text_via_store` recomposes. So an offset found here is an offset
/// the document agrees with.
///
/// A property in `djot_roundtrip_tests` pins that across the whole generated feature set;
/// without it this would be a second, silently-diverging definition of "the text".
///
/// ⚠ It is **not** the same as `TextDocument::to_plain_text()`, which walks frames and
/// therefore orders a blockquote's prose differently (`"> a0\n\na"` exports as `"a\na0"`
/// but is *searched* as `"a0\na"`). The authority is what a search sees, because that is
/// what a replace edits. See `claude_reviews/text-document-plain-text-ordering.md`.
/// One span's contribution to the addressable text.
///
/// An inline object carries no prose but **does** occupy one `U+FFFC` in the
/// document (`format_runs_from_spans` mirrors it there), so it has to occupy one
/// here too. Leaving it out makes this string shorter than the document it
/// claims to be byte-identical to, and every offset past the object — every
/// search hit, every comment anchor — lands a character early.
fn span_prose(span: &ParsedSpan, out: &mut String) {
    if span.image.is_some() || span.footnote_ref.is_some() {
        out.push('\u{FFFC}');
        return;
    }
    out.push_str(&span.text);
}

fn block_prose(block: &ParsedBlock) -> String {
    let mut prose = String::new();
    for span in &block.spans {
        span_prose(span, &mut prose);
    }
    prose
}

fn cell_prose(cell: &ParsedTableCell) -> String {
    let mut prose = String::new();
    for span in &cell.spans {
        span_prose(span, &mut prose);
    }
    prose
}

pub fn djot_to_plain_text(djot: &str, options: &DjotImportOptions) -> String {
    // Deliberately NOT `ParsedElement::flatten_to_blocks`: that helper drops a table's
    // anchor and yields only its cells, which would silently shift every offset in a
    // document containing a table by the two characters the document actually holds there.
    let elements = parse_djot(djot, options);

    // Sized from the source: the prose is always shorter than the markup that carries it,
    // so this allocates once and never grows.
    let mut out = String::with_capacity(djot.len());

    // The separator is decided by "is this the first block", NOT by "is the output still
    // empty". An EMPTY block (an empty code fence, say) is still a block: the document
    // holds an empty line for it, and an emptiness test would swallow both the block and
    // its separator, shifting every offset after it by one.
    let mut first = true;
    let push = |text: &str, out: &mut String, first: &mut bool| {
        if *first {
            *first = false;
        } else {
            out.push('\n');
        }
        out.push_str(text);
    };

    for element in &elements {
        match element {
            ParsedElement::Block(block) => {
                push(&block_prose(block), &mut out, &mut first);
            }
            // A note's body is **out of flow**: it is not laid out where its
            // definition was written, it is not part of a copied fragment, and
            // the document does not count its characters. So it is not part of
            // the addressable text either — which is the one thing that has to
            // stay true here, since `character_count()` and this string are
            // compared directly.
            //
            // Its blocks do live in the rope, because they have to live
            // somewhere; they are simply not addressable prose.
            ParsedElement::FootnoteDefinition { .. } => {}
            ParsedElement::Table(table) => {
                // The import mirrors a table into the rope as a lone `U+FFFC` sentinel
                // followed by its cells, one per block (`rope_append_table_anchor`). The
                // sentinel occupies a real position in the text the document searches, so
                // it has to occupy one here too — otherwise every offset after a table is
                // short by two characters, and a snippet taken from this string would be
                // sliced in the wrong place.
                push(TABLE_ANCHOR, &mut out, &mut first);
                for row in &table.rows {
                    for cell in row {
                        push(&cell_prose(cell), &mut out, &mut first);
                    }
                }
            }
        }
    }
    out
}
