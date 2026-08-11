// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Cyril Jacquet

//! Low-level ODF (OpenDocument Format) plumbing shared by `use_cases::export_odt_uc`: XML text
//! encoding, automatic-style interning/dedup, the named/page-layout/master-page skeleton for
//! `styles.xml`, and final zip packaging.
//!
//! Kept separate from the walker (`export_odt_uc.rs`) for the same reason `html_render.rs` is
//! separate from `export_html_uc.rs`/`export_epub_uc.rs`: this half is "how do I spell an ODF
//! paragraph style / a run of three spaces / a valid package", the walker's half is "which block
//! comes next". Unlike `html_render.rs` this module currently has exactly one caller — there is
//! no second ODF-writing exporter yet — but the split still pays for itself: `export_odt_uc.rs`
//! is already long just from the block/frame/table/footnote tree walk, and mixing raw ODF XML
//! spelling into that would make both halves harder to read.
//!
//! ## Why hand-rolled XML, not a builder crate
//!
//! There is no maintained ODF-writing crate in the Rust ecosystem to lean on the way `docx-rs`
//! and `epub-builder` are leaned on for the other two writers (`open-document` v0.1.0 is a single
//! unstable release from 2019 — not a dependency). So every element here is a `format!` — string
//! concatenation into `String`, not a DOM tree — which is exactly what `html_render.rs` already
//! does for the same reason (its own docstring: no HTML builder crate would buy back the
//! escaping/space-run/style-dedup logic this module needs anyway). The one place that *is*
//! DOM-shaped is [`OdtStyleSheet`]'s dedup buckets, because "does this exact set of properties
//! already have a style name" is genuinely a lookup, not a walk.
//!
//! ## Unit convention: everything lands in points
//!
//! ODF lengths are strings with a unit suffix (`"12pt"`, `"2.5cm"`); this module always chooses
//! `pt`. [`common::parser_tools::OdtExportOptions`] carries twips/half-points (DOCX's units, kept
//! for cross-writer consistency at the *options* layer — see that module's doc comment); twips
//! convert to points losslessly (`/20.0`, since 1pt = 20 twips by definition) and half-points
//! trivially (`/2.0`), so `pt` is the one ODF unit that never needs a lossy px-derived rounding
//! step for anything coming from those options. Block-level spacing (`fmt_top_margin`,
//! `fmt_text_indent`, …) is in the document model's own logical-pixel unit (96 dpi, matching the
//! editor's layout engine and DOCX's own `px_to_twips`), so [`px_to_pt`] does the one lossy
//! conversion this module needs (1px = 0.75pt exactly at 96 dpi — not lossy at all, in fact,
//! since 96 and 72 share a clean ratio).

use common::types::EntityId;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// XML text encoding
// ---------------------------------------------------------------------------

/// Escape `&`, `<`, `>` and `"` for safe use in either XML text content or a quoted attribute
/// value. Always escaping the quote too (technically unnecessary in text content) means one
/// function serves both positions, which is simpler than tracking which callers need which
/// subset and getting it wrong once.
///
/// Also strips the handful of ASCII control characters XML 1.0 forbids outright (everything
/// below U+0020 except tab/LF/CR, which [`encode_run_text`] turns into real elements before this
/// function ever sees them). A user's prose should never contain them, but a corrupted paste or
/// a lossy round trip through another tool might, and an XML writer that lets one through
/// produces a file `roxmltree`/LibreOffice then refuse to parse at all — a whole manuscript lost
/// over one stray byte is a strictly worse failure than silently dropping that one byte.
pub(crate) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' => {}
            _ => out.push(c),
        }
    }
    out
}

/// Encode one run's literal text as ODF inline content: a run of two or more spaces becomes
/// `<text:s text:c="N"/>` (ODF's own construct for "a specific number of consecutive spaces",
/// mirrored from how `document_ingest::sources::odt::Walker::inline` reads it back — see that
/// module's `(Some(NS_TEXT), "s")` arm), a tab becomes `<text:tab/>`, and an embedded newline
/// (only ever seen inside a code block; see `render_code_block`) becomes `<text:line-break/>`.
/// Everything else passes through [`xml_escape`].
///
/// This matters for more than cosmetics: LibreOffice's own *rendering* collapses runs of
/// whitespace in ordinary text nodes the way a browser does, so a paragraph that skipped this
/// and emitted `"a    b"` as one literal text node would visibly show `"a b"` when opened — correct
/// XML, wrong document. `text:s` is what every real ODF writer (LibreOffice itself, Pandoc) uses
/// to say "no, I mean it, four spaces."
pub(crate) fn encode_run_text(text: &str) -> String {
    let mut out = String::new();
    let mut literal = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                flush_literal(&mut out, &mut literal);
                out.push_str("<text:line-break/>");
            }
            '\t' => {
                flush_literal(&mut out, &mut literal);
                out.push_str("<text:tab/>");
            }
            ' ' => {
                let mut count = 1usize;
                while chars.peek() == Some(&' ') {
                    count += 1;
                    chars.next();
                }
                if count == 1 {
                    literal.push(' ');
                } else {
                    flush_literal(&mut out, &mut literal);
                    out.push_str(&format!("<text:s text:c=\"{count}\"/>"));
                }
            }
            _ => literal.push(c),
        }
    }
    flush_literal(&mut out, &mut literal);
    out
}

fn flush_literal(out: &mut String, literal: &mut String) {
    if !literal.is_empty() {
        out.push_str(&xml_escape(literal));
        literal.clear();
    }
}

// ---------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------

/// Twips per logical pixel — the same constant `export_docx_uc::TWIPS_PER_PX` uses, kept in sync
/// so a `top_margin`/`text_indent` block attribute converts to the identical physical size in
/// both writers.
const TWIPS_PER_PX: f64 = 15.0;

/// A block's logical-pixel spacing, as ODF points.
pub(crate) fn px_to_pt(px: i64) -> f64 {
    (px as f64 * TWIPS_PER_PX) / 20.0
}

/// A DOCX-unit twips length (from [`common::parser_tools::OdtExportOptions`]), as ODF points.
pub(crate) fn twips_to_pt(twips: i32) -> f64 {
    twips as f64 / 20.0
}

/// A DOCX-unit half-points font size, as ODF points.
pub(crate) fn half_points_to_pt(half_points: usize) -> f64 {
    half_points as f64 / 2.0
}

/// Format a point value for an ODF length attribute: up to two decimal places, trailing zeros
/// (and a bare trailing `.`) trimmed, always with the unit suffix.
///
/// Fixed precision rather than `{}` on the raw `f64`: `px_to_pt`/`twips_to_pt` can produce
/// values like `18.749999999999996` from ordinary floating-point division, and ODF readers are
/// not obliged to be forgiving about how many digits a length carries.
pub(crate) fn fmt_pt(value: f64) -> String {
    let mut s = format!("{value:.2}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s.push_str("pt");
    s
}

/// One unit-step of left indentation for blockquote/list nesting, in points — the ODF analog of
/// `export_docx_uc::INDENT_STEP_TWIPS` (720 twips = 36pt = 0.5in), kept numerically identical so
/// a document exported to both formats nests to the same physical depth.
pub(crate) const INDENT_STEP_PT: f64 = 36.0;

// ---------------------------------------------------------------------------
// Automatic-style interning
// ---------------------------------------------------------------------------

/// One automatic style's content, split the way ODF's own `<style:style>` element is: attributes
/// that belong on the *opening tag itself* (today, only ever `style:parent-style-name="…"` — a
/// paragraph style's escape hatch back to a real, restylable named style) versus `inner`, the
/// child elements (`<style:paragraph-properties/>`, `<style:text-properties/>`, …). Keeping these
/// apart is not cosmetic: `style:parent-style-name` is invalid anywhere but the open tag's
/// attribute list, so a style body that concatenated it in with `inner` and let the caller
/// splice both between `<style:style …>` and `</style:style>` would silently write it out as
/// **text content** instead of an attribute — exactly the bug this type exists to make
/// unrepresentable.
#[derive(Clone, PartialEq, Eq, Hash)]
struct StyleBody {
    open_attrs: String,
    inner: String,
}

impl StyleBody {
    fn new(open_attrs: String, inner: String) -> Self {
        Self { open_attrs, inner }
    }

    /// A style with no open-tag attributes of its own (every family but paragraph styles).
    fn inner_only(inner: String) -> Self {
        Self {
            open_attrs: String::new(),
            inner,
        }
    }
}

/// One property-bucket's dedup table: `body` → assigned `style:name`. Two blocks that ask for
/// byte-identical formatting always get the same generated name, so a document with a thousand
/// plain paragraphs emits one automatic paragraph style, not a thousand — the same reason
/// `export_docx_uc::NumberingBuilder` dedups by `List` entity id rather than emitting one
/// `w:numId` per paragraph.
struct StyleBucket {
    prefix: &'static str,
    seen: HashMap<StyleBody, String>,
    /// `(name, body)` in first-seen order, so emission is deterministic across two exports of
    /// the same document (a `HashMap`'s iteration order is not).
    order: Vec<(String, StyleBody)>,
}

impl StyleBucket {
    fn new(prefix: &'static str) -> Self {
        Self {
            prefix,
            seen: HashMap::new(),
            order: Vec::new(),
        }
    }

    fn intern(&mut self, body: StyleBody) -> String {
        if let Some(name) = self.seen.get(&body) {
            return name.clone();
        }
        let name = format!("{}{}", self.prefix, self.order.len() + 1);
        self.seen.insert(body.clone(), name.clone());
        self.order.push((name.clone(), body));
        name
    }
}

/// Automatic-style dedup for one ODT export, plus the per-`List`-entity `<text:list-style>`
/// registry. Everything interned here is written into `content.xml`'s `<office:automatic-styles>`
/// (paragraph/text/table styles — the ones a paragraph or run references by `text:style-name`)
/// except the list styles, which content.xml's `<text:list>` elements reference by
/// `text:style-name` too but which this type keeps in their own list (ODF's schema allows
/// `<text:list-style>` inside `<office:automatic-styles>` alongside `<style:style>`, so both end
/// up in the same parent element in the end — [`Self::automatic_styles_xml`] concatenates them).
#[derive(Default)]
pub(crate) struct OdtStyleSheet {
    paragraph: Option<StyleBucket>,
    text: Option<StyleBucket>,
    table: Option<StyleBucket>,
    table_column: Option<StyleBucket>,
    table_cell: Option<StyleBucket>,
    /// `List` entity id → assigned list-style name, plus the list styles' own XML in
    /// first-registered order (mirrors `NumberingBuilder`).
    list_seen: HashMap<EntityId, String>,
    list_styles: Vec<(String, String)>,
}

impl OdtStyleSheet {
    fn paragraph_bucket(&mut self) -> &mut StyleBucket {
        self.paragraph.get_or_insert_with(|| StyleBucket::new("P"))
    }
    fn text_bucket(&mut self) -> &mut StyleBucket {
        self.text.get_or_insert_with(|| StyleBucket::new("T"))
    }
    fn table_bucket(&mut self) -> &mut StyleBucket {
        self.table.get_or_insert_with(|| StyleBucket::new("Tbl"))
    }
    fn table_column_bucket(&mut self) -> &mut StyleBucket {
        self.table_column
            .get_or_insert_with(|| StyleBucket::new("TblCol"))
    }
    fn table_cell_bucket(&mut self) -> &mut StyleBucket {
        self.table_cell
            .get_or_insert_with(|| StyleBucket::new("TblCell"))
    }

    /// Intern an automatic paragraph style descending from named style `parent` (which must
    /// already exist in `styles.xml`'s `<office:styles>` — see `named_styles_xml`).
    /// `para_attrs`/`text_attrs` are already-built XML attribute strings (possibly empty) for
    /// `<style:paragraph-properties>`/`<style:text-properties>`.
    ///
    /// When both are empty this returns `parent` directly rather than interning a
    /// property-less style — the shortcut that keeps a default-options export from declaring
    /// one automatic style per paragraph for paragraphs that carry no overrides at all.
    pub(crate) fn paragraph_style(
        &mut self,
        parent: &str,
        para_attrs: &str,
        text_attrs: &str,
    ) -> String {
        if para_attrs.is_empty() && text_attrs.is_empty() {
            return parent.to_string();
        }
        let mut inner = String::new();
        if !para_attrs.is_empty() {
            inner.push_str(&format!("<style:paragraph-properties {para_attrs}/>"));
        }
        if !text_attrs.is_empty() {
            inner.push_str(&format!("<style:text-properties {text_attrs}/>"));
        }
        self.paragraph_bucket().intern(StyleBody::new(
            format!("style:parent-style-name=\"{parent}\""),
            inner,
        ))
    }

    /// Intern an automatic **character** style (a `<text:span>` needs one for anything beyond
    /// plain text — ODF, like DOCX, carries no inline attributes directly on the run/span
    /// element itself). `attrs` is the already-built `<style:text-properties>` attribute string;
    /// empty is a caller bug (a plain run needs no span at all — see
    /// `export_odt_uc::build_run`), so this never shortcuts the way `paragraph_style` does.
    pub(crate) fn text_style(&mut self, attrs: &str) -> String {
        self.text_bucket().intern(StyleBody::inner_only(format!(
            "<style:text-properties {attrs}/>"
        )))
    }

    /// Intern an automatic table style.
    pub(crate) fn table_style(&mut self, attrs: &str) -> String {
        self.table_bucket().intern(StyleBody::inner_only(format!(
            "<style:table-properties {attrs}/>"
        )))
    }

    /// Intern an automatic table-column style.
    pub(crate) fn table_column_style(&mut self, attrs: &str) -> String {
        self.table_column_bucket()
            .intern(StyleBody::inner_only(format!(
                "<style:table-column-properties {attrs}/>"
            )))
    }

    /// Intern an automatic table-cell style.
    pub(crate) fn table_cell_style(&mut self, attrs: &str) -> String {
        self.table_cell_bucket()
            .intern(StyleBody::inner_only(format!(
                "<style:table-cell-properties {attrs}/>"
            )))
    }

    /// Return `list_id`'s list-style name, registering (via `build`) its `<text:list-style>` on
    /// first use — mirrors `NumberingBuilder::get_or_create`, including the "one List entity, one
    /// counter that restarts at the top of its own list" guarantee that gives.
    pub(crate) fn list_style(
        &mut self,
        list_id: EntityId,
        build: impl FnOnce(&str) -> String,
    ) -> String {
        if let Some(name) = self.list_seen.get(&list_id) {
            return name.clone();
        }
        let name = format!("L{}", self.list_styles.len() + 1);
        let xml = build(&name);
        self.list_styles.push((name.clone(), xml));
        self.list_seen.insert(list_id, name.clone());
        name
    }

    /// Every interned style, as the child content of `content.xml`'s
    /// `<office:automatic-styles>`.
    pub(crate) fn automatic_styles_xml(&self) -> String {
        let mut out = String::new();
        let families: [(&Option<StyleBucket>, &str); 5] = [
            (&self.paragraph, "paragraph"),
            (&self.text, "text"),
            (&self.table, "table"),
            (&self.table_column, "table-column"),
            (&self.table_cell, "table-cell"),
        ];
        for (bucket, family) in families {
            let Some(bucket) = bucket else { continue };
            for (name, body) in &bucket.order {
                let open_attrs = if body.open_attrs.is_empty() {
                    String::new()
                } else {
                    format!(" {}", body.open_attrs)
                };
                out.push_str(&format!(
                    "<style:style style:name=\"{name}\" style:family=\"{family}\"{open_attrs}>{}</style:style>",
                    body.inner
                ));
            }
        }
        for (_, xml) in &self.list_styles {
            out.push_str(xml);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// styles.xml skeleton: named styles, page layout, master page
// ---------------------------------------------------------------------------

/// The fixed named/common paragraph styles every export declares in `styles.xml`'s
/// `<office:styles>`, independent of the document's content: `Standard` (the universal parent —
/// LibreOffice's own reserved name for "the" default paragraph style, exactly as `docx-rs`'s
/// documents need `docx-rs`'s built-in default rather than a named style at all), `Heading_1`
/// through `Heading_6` (real, restylable named styles — same rationale as
/// `export_docx_uc::heading_style`'s doc comment: a heading's *level* comes from
/// `text:outline-level`, read back independently of style name, but what it *looks like* still
/// has to be said somewhere or it opens as plain body text), `Epigraph`/`EpigraphAttribution`
/// (mirrors the DOCX writer's identical pair), `Quote` (an ordinary blockquote's paragraphs),
/// `Rule` (the horizontal-rule/scene-break style —
/// see `export_odt_uc`'s module doc for why this exists and what it must look like for
/// `document_ingest::sources::odt::StyleTable::is_rule` to recognise it), and `Code_Block`
/// (monospace + shaded, mirrors `export_docx_uc::CODE_BLOCK_FILL`). `Header` is added only when
/// `options.page_numbers` is set, since it exists purely to hold the running-header paragraph.
pub(crate) fn named_styles_xml(
    options: &common::parser_tools::OdtExportOptions,
    heading_styles: &[common::parser_tools::OdtHeadingStyle],
) -> String {
    let mut out = String::new();

    // "Standard": the base every other style ultimately descends from, carrying the document's
    // base font/size when the caller supplied one.
    let mut standard_text_attrs = String::new();
    if let Some(family) = &options.font_family {
        standard_text_attrs.push_str(&format!(" style:font-name=\"{}\"", xml_escape(family)));
    }
    if let Some(hp) = options.font_half_points {
        standard_text_attrs.push_str(&format!(
            " fo:font-size=\"{}\"",
            fmt_pt(half_points_to_pt(hp))
        ));
    }
    out.push_str(&format!(
        "<style:style style:name=\"Standard\" style:family=\"paragraph\" style:class=\"text\">\
         <style:text-properties{standard_text_attrs}/></style:style>"
    ));
    // `style:default-style` covers every family that never gets an explicit `style:style` (so a
    // reader with no opinion of its own falls back to the same base font, not its own built-in).
    out.push_str(&format!(
        "<style:default-style style:family=\"paragraph\"><style:text-properties{standard_text_attrs}/></style:default-style>"
    ));

    for (i, h) in heading_styles.iter().enumerate() {
        let level = i + 1;
        let mut para_attrs = String::new();
        if let Some(before) = h.space_before_twips {
            para_attrs.push_str(&format!(
                " fo:margin-top=\"{}\"",
                fmt_pt(twips_to_pt(before))
            ));
        }
        if let Some(after) = h.space_after_twips {
            para_attrs.push_str(&format!(
                " fo:margin-bottom=\"{}\"",
                fmt_pt(twips_to_pt(after))
            ));
        }
        if h.keep_with_next {
            para_attrs.push_str(" fo:keep-with-next=\"always\"");
        }
        if h.page_break_before {
            para_attrs.push_str(" fo:break-before=\"page\"");
        }
        if let Some(a) = &h.alignment {
            para_attrs.push_str(&format!(" fo:text-align=\"{}\"", odf_align(a)));
        }
        let mut text_attrs = String::new();
        if let Some(size) = h.size_half_points {
            text_attrs.push_str(&format!(
                " fo:font-size=\"{}\"",
                fmt_pt(half_points_to_pt(size))
            ));
        }
        if h.bold {
            text_attrs.push_str(" fo:font-weight=\"bold\"");
        }
        if h.italic {
            text_attrs.push_str(" fo:font-style=\"italic\"");
        }
        out.push_str(&format!(
            "<style:style style:name=\"Heading_{level}\" style:family=\"paragraph\" \
             style:parent-style-name=\"Standard\" style:class=\"text\">\
             <style:paragraph-properties{para_attrs}/><style:text-properties{text_attrs}/></style:style>"
        ));
    }

    // Epigraph / EpigraphAttribution: mirrors `export_docx_uc`'s pair — italic quote body, its
    // right-aligned attribution line, both indented one step in from the body margin.
    //
    // `Quote` joins them for an *ordinary* blockquote, mirroring `export_docx_uc::QUOTE_STYLE_ID`
    // and carrying the same argument: an indent is a measurement and cannot say what it means,
    // so a quotation is named rather than merely inset — which is what lets
    // `document_ingest::sources::odt::StyleTable::is_quote` read one back as a quotation. Not
    // italic, unlike the epigraph: a quotation inside a scene is the writer's running text.
    out.push_str(&format!(
        "<style:style style:name=\"Epigraph\" style:family=\"paragraph\" \
         style:parent-style-name=\"Standard\" style:class=\"text\">\
         <style:paragraph-properties fo:margin-left=\"{indent}\"/>\
         <style:text-properties fo:font-style=\"italic\"/></style:style>\
         <style:style style:name=\"EpigraphAttribution\" style:family=\"paragraph\" \
         style:parent-style-name=\"Standard\" style:class=\"text\">\
         <style:paragraph-properties fo:margin-left=\"{indent}\" fo:text-align=\"right\"/>\
         </style:style>\
         <style:style style:name=\"Quote\" style:family=\"paragraph\" \
         style:parent-style-name=\"Standard\" style:class=\"text\">\
         <style:paragraph-properties fo:margin-left=\"{indent}\"/></style:style>",
        indent = fmt_pt(INDENT_STEP_PT)
    ));

    // Rule: the horizontal-rule/scene-break style. `document_ingest::sources::odt::StyleTable::
    // is_rule` recognises exactly this shape — a bottom border and nothing else — so every side
    // but the bottom is set to the literal string "none" rather than merely left unset, matching
    // its `sides_clear` check (`is_none_or(|v| v == "none")`, which an *absent* attribute also
    // satisfies, but an explicit "none" is unambiguous and survives a round trip through a tool
    // that fills in defaults).
    out.push_str(
        "<style:style style:name=\"Rule\" style:family=\"paragraph\" \
         style:parent-style-name=\"Standard\" style:class=\"text\">\
         <style:paragraph-properties fo:margin-top=\"12pt\" fo:margin-bottom=\"12pt\" \
         fo:border-top=\"none\" fo:border-left=\"none\" fo:border-right=\"none\" \
         fo:border-bottom=\"0.5pt solid #000000\" fo:padding=\"0pt\"/></style:style>",
    );

    // Code_Block: monospace + the same light-grey fill `export_docx_uc::CODE_BLOCK_FILL` uses,
    // kept together across a page break the way `Paragraph::keep_lines` does for DOCX.
    out.push_str(
        "<style:style style:name=\"Code_Block\" style:family=\"paragraph\" \
         style:parent-style-name=\"Standard\" style:class=\"text\">\
         <style:paragraph-properties fo:background-color=\"#F5F5F5\" fo:keep-together=\"always\"/>\
         <style:text-properties style:font-name=\"Courier New\" \
         style:font-name-complex=\"Courier New\"/></style:style>",
    );

    if options.page_numbers {
        out.push_str(
            "<style:style style:name=\"Header\" style:family=\"paragraph\" \
             style:parent-style-name=\"Standard\" style:class=\"extra\">\
             <style:paragraph-properties fo:text-align=\"right\"/></style:style>",
        );
    }

    out
}

/// `fo:text-align` for the model's alignment enum, physical (not logical `start`/`end`) so a
/// block that asked for `Left` stays visually left regardless of the paragraph's own writing
/// direction — the same physical-vs-logical choice `export_docx_uc::map_alignment` makes.
pub(crate) fn odf_align(alignment: &common::entities::Alignment) -> &'static str {
    use common::entities::Alignment;
    match alignment {
        Alignment::Left => "left",
        Alignment::Right => "right",
        Alignment::Center => "center",
        Alignment::Justify => "justify",
    }
}

/// The page-layout + master-page pair that goes in `styles.xml`'s `<office:automatic-styles>`
/// and `<office:master-styles>` respectively. A master page named exactly `"Standard"` is what
/// ODF uses as the document's first/only page when nothing in `content.xml` says otherwise — no
/// per-paragraph reference needed, which is why the walker never has to know this exists.
pub(crate) fn page_layout_and_master_page_xml(
    options: &common::parser_tools::OdtExportOptions,
) -> (String, String) {
    let mut layout_attrs = String::new();
    if let (Some(w), Some(h)) = (options.page_width_twips, options.page_height_twips) {
        layout_attrs.push_str(&format!(
            " fo:page-width=\"{}\" fo:page-height=\"{}\"",
            fmt_pt(twips_to_pt(w as i32)),
            fmt_pt(twips_to_pt(h as i32))
        ));
    }
    if let Some(m) = options.margin_top_twips {
        layout_attrs.push_str(&format!(" fo:margin-top=\"{}\"", fmt_pt(twips_to_pt(m))));
    }
    if let Some(m) = options.margin_bottom_twips {
        layout_attrs.push_str(&format!(" fo:margin-bottom=\"{}\"", fmt_pt(twips_to_pt(m))));
    }
    if let Some(m) = options.margin_left_twips {
        layout_attrs.push_str(&format!(" fo:margin-left=\"{}\"", fmt_pt(twips_to_pt(m))));
    }
    if let Some(m) = options.margin_right_twips {
        layout_attrs.push_str(&format!(" fo:margin-right=\"{}\"", fmt_pt(twips_to_pt(m))));
    }
    let page_layout = format!(
        "<style:page-layout style:name=\"PM1\"><style:page-layout-properties{layout_attrs} \
         style:print-orientation=\"portrait\"/></style:page-layout>"
    );

    let header = if options.page_numbers {
        let prefix = match &options.running_header {
            Some(text) if !text.trim().is_empty() => {
                xml_escape(format!("{}   ", text.trim()).as_str())
            }
            _ => String::new(),
        };
        format!(
            "<style:header><text:p text:style-name=\"Header\">{prefix}\
             <text:page-number>1</text:page-number></text:p></style:header>"
        )
    } else {
        String::new()
    };
    let master_page = format!(
        "<style:master-page style:name=\"Standard\" style:page-layout-name=\"PM1\">{header}</style:master-page>"
    );

    (page_layout, master_page)
}

// ---------------------------------------------------------------------------
// Package assembly
// ---------------------------------------------------------------------------

/// XML namespace declarations shared by `content.xml`'s `<office:document-content>` and
/// `styles.xml`'s `<office:document-styles>` root elements. Every prefix this module (or the
/// walker) ever writes must be declared here — an undeclared prefix is invalid XML, which
/// `roxmltree`/LibreOffice reject outright rather than degrading gracefully.
///
/// `dc` and `loext` exist for `office:annotation` (comment threads — `dc:creator`/`dc:date`,
/// `loext:resolved`/`loext:parent-name`; see `export_odt_uc`'s M-T2b module-doc section).
/// `skrb` is this crate's own private extension, declared here for the same reason: a comment's
/// stable `uid` needs an XML carrier ODF's own vocabulary has no slot for, and every prefix a
/// writer uses must be declared on some ancestor element or the document does not parse —
/// declaring it unconditionally in the one place every root element already shares is simpler
/// than adding it only when `styles.paragraph`/… options add it (an unused namespace declaration
/// costs a few bytes and is not an error; the `docx` writer's `w15:done`/`w:initials`/`skrb:uid`
/// patch uses the identical URI, `urn:ferntech:text-document:comment:1`, so the two writers'
/// private extension data is recognisably the same vocabulary to a reader that understands
/// either).
const NAMESPACES: &str = "xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" \
     xmlns:style=\"urn:oasis:names:tc:opendocument:xmlns:style:1.0\" \
     xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\" \
     xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\" \
     xmlns:draw=\"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0\" \
     xmlns:fo=\"urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0\" \
     xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
     xmlns:svg=\"urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0\" \
     xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
     xmlns:loext=\"urn:org:documentfoundation:names:experimental:office:xmlns:loext:1.0\" \
     xmlns:skrb=\"urn:ferntech:text-document:comment:1\"";

/// Assemble `content.xml`: `<office:automatic-styles>` from `styles`, then `body_xml` (already
/// fully rendered paragraphs/headings/lists/tables) inside `<office:body><office:text>`.
pub(crate) fn content_xml(styles: &OdtStyleSheet, body_xml: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <office:document-content {NAMESPACES} office:version=\"1.3\">\
         <office:automatic-styles>{}</office:automatic-styles>\
         <office:body><office:text>{body_xml}</office:text></office:body>\
         </office:document-content>",
        styles.automatic_styles_xml()
    )
}

/// Assemble `styles.xml`: named styles in `<office:styles>`, the page layout in
/// `<office:automatic-styles>`, the master page in `<office:master-styles>`.
pub(crate) fn styles_xml(
    options: &common::parser_tools::OdtExportOptions,
    heading_styles: &[common::parser_tools::OdtHeadingStyle],
) -> String {
    let (page_layout, master_page) = page_layout_and_master_page_xml(options);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <office:document-styles {NAMESPACES} office:version=\"1.3\">\
         <office:styles>{}</office:styles>\
         <office:automatic-styles>{page_layout}</office:automatic-styles>\
         <office:master-styles>{master_page}</office:master-styles>\
         </office:document-styles>",
        named_styles_xml(options, heading_styles)
    )
}

/// `META-INF/manifest.xml`: the root entry (required, carries the package's own media type) plus
/// one entry per real part. `image_paths` are the in-package hrefs (`Pictures/imgNNN.ext`)
/// already written into the zip by [`package_odt`]'s caller.
fn manifest_xml(image_entries: &[(String, String)]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <manifest:manifest xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\" \
         manifest:version=\"1.3\">\
         <manifest:file-entry manifest:full-path=\"/\" manifest:version=\"1.3\" \
         manifest:media-type=\"application/vnd.oasis.opendocument.text\"/>\
         <manifest:file-entry manifest:full-path=\"content.xml\" manifest:media-type=\"text/xml\"/>\
         <manifest:file-entry manifest:full-path=\"styles.xml\" manifest:media-type=\"text/xml\"/>",
    );
    for (href, media_type) in image_entries {
        out.push_str(&format!(
            "<manifest:file-entry manifest:full-path=\"{}\" manifest:media-type=\"{}\"/>",
            xml_escape(href),
            xml_escape(media_type)
        ));
    }
    out.push_str("</manifest:manifest>");
    out
}

/// Package `content_xml`/`styles_xml` plus every embedded image into a complete `.odt` file's
/// bytes.
///
/// `mimetype` **must** be the first entry, stored (not deflated) — an ODF (and EPUB) container
/// requirement: a reader is allowed to detect the format by reading exactly the first N bytes of
/// the zip's local-file-header-plus-data for that one entry without inflating anything, and a
/// compressed or reordered `mimetype` breaks that shortcut. This is the ODF spelling of the same
/// rule `export_epub_uc`'s module doc points at for EPUB (`epub-builder` enforces it there; here
/// there is no library doing it for us, so it is enforced by hand: `mimetype` is written before
/// anything else, with `CompressionMethod::Stored`).
pub(crate) fn package_odt(
    content_xml: &str,
    styles_xml: &str,
    images: &[(String, Vec<u8>, String)],
) -> anyhow::Result<Vec<u8>> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let mut buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buf);

    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("mimetype", stored)?;
    zip.write_all(b"application/vnd.oasis.opendocument.text")?;

    let deflated =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    zip.add_directory("META-INF/", deflated)?;
    let image_entries: Vec<(String, String)> = images
        .iter()
        .map(|(href, _, media_type)| (href.clone(), media_type.clone()))
        .collect();
    zip.start_file("META-INF/manifest.xml", deflated)?;
    zip.write_all(manifest_xml(&image_entries).as_bytes())?;

    zip.start_file("content.xml", deflated)?;
    zip.write_all(content_xml.as_bytes())?;

    zip.start_file("styles.xml", deflated)?;
    zip.write_all(styles_xml.as_bytes())?;

    if !images.is_empty() {
        zip.add_directory("Pictures/", deflated)?;
        for (href, bytes, _) in images {
            zip.start_file(href, deflated)?;
            zip.write_all(bytes)?;
        }
    }

    zip.finish()?;
    Ok(buf.into_inner())
}
