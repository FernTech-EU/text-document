//! Feature tests for the DOCX exporter.
//!
//! Documents are built with the (well-tested) djot importer. Most tests export via the
//! file-less builder [`document_io_controller::build_docx_document`] and assert on the
//! resulting [`docx_rs::Docx`] structure directly — the exact builder used to write `.docx`
//! files, without touching the filesystem. The footnote tests are the exception: they run
//! the same builder through `docx-rs`'s `build()`/`pack()` into an in-memory zip
//! (`docx_bytes_from_djot`) and unzip it, because the fact they are checking — a footnote
//! reference resolving to a real body — is decided by that pack step, which the bare
//! in-memory struct never reaches.

extern crate text_document_io as document_io;

use common::long_operation::{LongOperationManager, OperationStatus};
use document_io::docx_rs::{
    AlignmentType, DocumentChild, Docx, HyperlinkData, Paragraph, ParagraphChild, RunChild,
    SpecialIndentType,
};
use document_io::{ExportDocxDto, ImportDjotDto, document_io_controller};
use test_harness::{EventHub, setup};

use std::sync::Arc;

/// A document touching every implemented feature, used by the on-disk test.
const RICH_DJOT: &str = "\
# Title

{alignment=center}
Centered intro with a [link](https://example.com).

- bullet one
- bullet two

1. first
2. second

- [x] done task
- [ ] pending task

> a quoted line
>
> > nested quote

```rust
let answer = 42;
```";

// --- harness ---------------------------------------------------------------

fn wait(mgr: &LongOperationManager, op_id: &str) {
    while let Some(OperationStatus::Running) = mgr.get_operation_status(op_id) {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// Import `djot` into a fresh document and return the built DOCX model.
fn docx_from_djot(djot: &str) -> Docx {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, djot);
    document_io_controller::build_docx_document(&db, &ExportDocxDto::default())
        .expect("build_docx_document")
}

/// Import `djot`, run the same builder `export_docx` uses, and pack it through
/// `docx-rs`'s `build()`/`pack()` into an in-memory zip — so the footnote tests can unzip the
/// real container instead of asserting on the bare [`Docx`] builder struct.
///
/// That distinction is the whole point of these tests: `docx-rs`'s pack step is where
/// footnote references get collected into a separate `word/footnotes.xml` part (see
/// `Docx::build`'s `collect_footnotes()`) and registered in `[Content_Types].xml` and the
/// document relationships — none of which the bare in-memory struct exercises. Asserting only
/// on the struct would be the false-confidence mistake the image work ran into: markup that
/// *looks* like a footnote reference, never proven to survive packing.
///
/// Packed in memory rather than via a shared temp-dir path: several of these tests run in
/// parallel, and Windows `SystemTime` resolution is coarse enough that
/// `pid + nanos`-named files collide — one test truncates another's zip mid-write
/// (`Invalid CDFH offset in EOCD`) or deletes it before the other can read it (`NotFound`).
/// The on-disk `export_docx` path is covered by `rich_document_packs_to_a_valid_docx_file`.
fn docx_bytes_from_djot(djot: &str) -> Vec<u8> {
    let docx = docx_from_djot(djot);
    let mut buf = std::io::Cursor::new(Vec::new());
    docx.build()
        .pack(&mut buf)
        .expect("docx-rs pack into memory");
    buf.into_inner()
}

/// Read one whole entry out of a packed `.docx`/zip container, by exact name.
fn read_zip_entry(bytes: &[u8], name: &str) -> String {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("packaged DOCX is a valid zip");
    let mut file = archive
        .by_name(name)
        .unwrap_or_else(|_| panic!("entry {name:?} present in the DOCX package"));
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut file, &mut contents).expect("entry is valid utf-8");
    contents
}

fn import_djot(db: &test_harness::DbContext, ev: &Arc<EventHub>, djot: &str) {
    let mut mgr = LongOperationManager::new();
    let op = document_io_controller::import_djot(
        db,
        ev,
        &mut mgr,
        &ImportDjotDto {
            djot_text: djot.to_string(),
            options: Default::default(),
        },
    )
    .expect("import_djot");
    wait(&mgr, &op);
    assert_eq!(
        mgr.get_operation_status(&op),
        Some(OperationStatus::Completed),
        "import of {djot:?} did not complete"
    );
}

// --- inspection helpers ----------------------------------------------------

fn paragraphs(docx: &Docx) -> Vec<&Paragraph> {
    docx.document
        .children
        .iter()
        .filter_map(|c| match c {
            DocumentChild::Paragraph(p) => Some(&**p),
            _ => None,
        })
        .collect()
}

fn collect_text(children: &[ParagraphChild], out: &mut String) {
    for child in children {
        match child {
            ParagraphChild::Run(run) => {
                for rc in &run.children {
                    if let RunChild::Text(t) = rc {
                        out.push_str(&t.text);
                    }
                }
            }
            ParagraphChild::Hyperlink(h) => collect_text(&h.children, out),
            _ => {}
        }
    }
}

fn para_text(p: &Paragraph) -> String {
    let mut s = String::new();
    collect_text(&p.children, &mut s);
    s
}

fn alignment(p: &Paragraph) -> Option<&str> {
    p.property.alignment.as_ref().map(|j| j.val.as_str())
}

fn numbering_id(p: &Paragraph) -> Option<usize> {
    p.property
        .numbering_property
        .as_ref()
        .and_then(|np| np.id.as_ref())
        .map(|id| id.id)
}

fn left_indent(p: &Paragraph) -> Option<i32> {
    p.property.indent.as_ref().and_then(|i| i.start)
}

/// The paragraph's first-line indent in twips, if it has one.
fn first_line_indent(p: &Paragraph) -> Option<i32> {
    match p.property.indent.as_ref()?.special_indent {
        Some(SpecialIndentType::FirstLine(v)) => Some(v),
        _ => None,
    }
}

/// The paragraph's space-above in twips, if it has one. `LineSpacing`'s fields
/// are private to docx-rs, but it derives `Serialize`, so read it back through
/// serde rather than reaching into the crate's internals.
fn space_before(p: &Paragraph) -> Option<u32> {
    let ls = p.property.line_spacing.as_ref()?;
    serde_json::to_value(ls)
        .ok()?
        .get("before")?
        .as_u64()
        .map(|v| v as u32)
}

/// First paragraph whose visible text contains `needle`.
fn para_containing<'a>(docx: &'a Docx, needle: &str) -> &'a Paragraph {
    paragraphs(docx)
        .into_iter()
        .find(|p| para_text(p).contains(needle))
        .unwrap_or_else(|| panic!("no paragraph containing {needle:?}"))
}

fn hyperlink_paths(p: &Paragraph) -> Vec<String> {
    p.children
        .iter()
        .filter_map(|c| match c {
            ParagraphChild::Hyperlink(h) => match &h.link {
                HyperlinkData::External { path, .. } => Some(path.clone()),
                HyperlinkData::Anchor { anchor } => Some(anchor.clone()),
            },
            _ => None,
        })
        .collect()
}

/// Whether the paragraph carries `<w:pageBreakBefore/>`. Same serde route as
/// `space_before` — the property's fields are private to docx-rs.
fn page_break_before(p: &Paragraph) -> bool {
    serde_json::to_value(&p.property)
        .ok()
        .and_then(|v| v.get("pageBreakBefore").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

// --- pagination ------------------------------------------------------------

#[test]
fn a_flagged_block_carries_page_break_before() {
    let docx = docx_from_djot("First.\n\n{page_break_before=true}\nSecond.");
    assert!(!page_break_before(para_containing(&docx, "First")));
    assert!(page_break_before(para_containing(&docx, "Second")));
}

/// The flag is set in the common formatting section, before the heading/list/plain
/// dispatch, so applying a style afterwards cannot drop it.
#[test]
fn a_heading_keeps_its_page_break_alongside_its_style() {
    let docx = docx_from_djot("Body.\n\n{page_break_before=true}\n# Chapter Two");
    let p = para_containing(&docx, "Chapter Two");
    assert!(page_break_before(p));
    assert_eq!(
        p.property.style.as_ref().map(|s| s.val.as_str()),
        Some("Heading1"),
        "the heading style must still be applied"
    );
}

#[test]
fn an_unflagged_block_has_no_page_break() {
    let docx = docx_from_djot("Just prose.");
    assert!(!page_break_before(para_containing(&docx, "Just prose")));
}

/// A heading's own space-above must survive. It is how a title page drops its title a
/// third of the way down the page, and a heading never reaches `apply_body_style`, which
/// is where every other block's `fmt_top_margin` is applied.
#[test]
fn a_heading_keeps_its_own_space_above() {
    let docx = docx_from_djot("{top_margin=288}\n# A Title");
    let p = para_containing(&docx, "A Title");
    // 288 logical px = 3 inches = 4320 twips.
    assert_eq!(space_before(p), Some(4320));
}

// --- heading styles --------------------------------------------------------

/// Every `HeadingN` a paragraph can reference must be *defined* in the file. Referencing
/// an undefined style id is legal OOXML — the reader silently substitutes its own — which
/// is exactly how an export asking for a chapter title arrived as whatever Word had.
#[test]
fn the_heading_styles_referenced_are_actually_defined() {
    let docx =
        docx_from_djot("# One\n\n## Two\n\n### Three\n\n#### Four\n\n##### Five\n\n###### Six");
    let defined: Vec<&str> = docx
        .styles
        .styles
        .iter()
        .map(|s| s.style_id.as_str())
        .collect();
    for level in 1..=6 {
        let id = format!("Heading{level}");
        assert!(
            defined.contains(&id.as_str()),
            "{id} is referenced by a paragraph but never defined; defined: {defined:?}"
        );
    }
}

/// Not merely present: sized, so a level-1 heading is visibly a title rather than body
/// text, and carrying the outline level Word's navigation pane and TOC read.
#[test]
fn a_defined_heading_style_is_sized_and_outlined() {
    let docx = docx_from_djot("# One");
    let h1 = docx
        .styles
        .styles
        .iter()
        .find(|s| s.style_id == "Heading1")
        .expect("Heading1 defined");
    let json = serde_json::to_value(h1).expect("serialize");
    let size = json["runProperty"]["sz"].as_u64();
    assert_eq!(
        size,
        Some(43),
        "12 pt body x 1.8 rounds to 43 half-points, not {size:?}"
    );
    assert_eq!(json["paragraphProperty"]["outlineLvl"].as_u64(), Some(0));
    assert_eq!(json["paragraphProperty"]["keepNext"].as_bool(), Some(true));
    assert_eq!(
        json["paragraphProperty"]["lineSpacing"]["before"].as_u64(),
        Some(480)
    );
}

/// A caller that supplies its own ramp gets exactly that, not the default one.
#[test]
fn caller_supplied_heading_styles_win() {
    use common::parser_tools::{DocxExportOptions, DocxHeadingStyle};
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "# One");
    let docx = document_io_controller::build_docx_document(
        &db,
        &ExportDocxDto {
            options: DocxExportOptions {
                heading_styles: vec![DocxHeadingStyle {
                    size_half_points: Some(24),
                    bold: false,
                    alignment: Some(common::entities::Alignment::Center),
                    page_break_before: true,
                    ..DocxHeadingStyle::default()
                }],
                ..DocxExportOptions::default()
            },
            ..ExportDocxDto::default()
        },
    )
    .expect("build_docx_document");
    let h1 = docx
        .styles
        .styles
        .iter()
        .find(|s| s.style_id == "Heading1")
        .expect("Heading1 defined");
    let json = serde_json::to_value(h1).expect("serialize");
    assert_eq!(json["runProperty"]["sz"].as_u64(), Some(24));
    assert_eq!(
        json["paragraphProperty"]["pageBreakBefore"].as_bool(),
        Some(true)
    );
    // Shunn's chapter openers are body-sized and centred, not bold and enlarged —
    // the case that makes the ramp overridable rather than baked in.
    assert_ne!(json["runProperty"]["bold"].as_bool(), Some(true));
}

// --- alignment -------------------------------------------------------------

#[test]
fn alignment_center_maps_to_jc() {
    let docx = docx_from_djot("{alignment=center}\nCentered paragraph");
    let p = para_containing(&docx, "Centered paragraph");
    assert_eq!(alignment(p), Some("center"));
}

#[test]
fn alignment_all_variants_map() {
    for (attr, expected) in [
        ("left", AlignmentType::Left),
        ("right", AlignmentType::Right),
        ("center", AlignmentType::Center),
        ("justify", AlignmentType::Justified),
    ] {
        let marker = format!("aligned-{attr}");
        let docx = docx_from_djot(&format!("{{alignment={attr}}}\n{marker}"));
        let p = para_containing(&docx, &marker);
        let got = alignment(p).expect("alignment set");
        // docx-rs renders AlignmentType via Display; compare its string form.
        assert_eq!(got, expected.to_string(), "attr={attr}");
    }
}

#[test]
fn no_alignment_leaves_jc_unset() {
    let docx = docx_from_djot("Plain unaligned paragraph");
    let p = para_containing(&docx, "Plain unaligned paragraph");
    assert_eq!(alignment(p), None);
}

// --- hyperlinks ------------------------------------------------------------

#[test]
fn hyperlink_is_emitted_with_destination() {
    let docx = docx_from_djot("See [the site](https://example.com/page) now");
    let p = para_containing(&docx, "the site");
    let paths = hyperlink_paths(p);
    assert_eq!(paths.len(), 1, "exactly one hyperlink");
    assert!(
        paths[0].contains("example.com/page"),
        "href preserved, got {:?}",
        paths[0]
    );
    // The link's visible text is carried inside the hyperlink.
    assert!(para_text(p).contains("the site"));
}

#[test]
fn plain_text_has_no_hyperlink() {
    let docx = docx_from_djot("Just words, no link here");
    let p = para_containing(&docx, "Just words");
    assert!(hyperlink_paths(p).is_empty());
}

// --- code blocks -----------------------------------------------------------

#[test]
fn code_block_uses_monospace_font_and_preserves_text() {
    let docx = docx_from_djot("```rust\nlet x = 41 + 1;\n```");
    let p = para_containing(&docx, "let x = 41 + 1;");
    // Text is preserved verbatim, no markdown fences.
    assert!(!para_text(p).contains("```"));
    assert_eq!(para_text(p), "let x = 41 + 1;");
    // The monospace font is applied; RunFonts fields are private, so assert via
    // the serialized form.
    let json = docx.json();
    assert!(
        json.contains("Courier New"),
        "expected a Courier New run in the document"
    );
}

#[test]
fn code_block_inline_formatting_is_flattened() {
    // Even if the fenced content looks like emphasis, it stays literal.
    let docx = docx_from_djot("```\na * b * c\n```");
    let p = para_containing(&docx, "a * b * c");
    assert_eq!(para_text(p), "a * b * c");
}

// --- lists -----------------------------------------------------------------

#[test]
fn bullet_list_items_carry_numbering() {
    let docx = docx_from_djot("- first\n- second\n- third");
    let items: Vec<&Paragraph> = paragraphs(&docx)
        .into_iter()
        .filter(|p| numbering_id(p).is_some())
        .collect();
    assert_eq!(items.len(), 3, "all three bullets numbered");
    // All share one bullet list => one numbering instance.
    let ids: std::collections::HashSet<usize> =
        items.iter().filter_map(|p| numbering_id(p)).collect();
    assert_eq!(ids.len(), 1, "single bullet list => single numbering id");

    // The numbering definition exists and is a bullet format.
    let id = *ids.iter().next().unwrap();
    assert_numbering_format(&docx, id, "bullet");
}

#[test]
fn ordered_list_uses_decimal_format() {
    let docx = docx_from_djot("1. alpha\n2. beta");
    let id = numbering_id(para_containing(&docx, "alpha")).expect("numbered");
    assert_numbering_format(&docx, id, "decimal");
}

#[test]
fn two_separate_lists_get_independent_numbering() {
    // A paragraph between the lists splits them into two list instances.
    let docx = docx_from_djot("1. one\n2. two\n\nbreak\n\n1. uno\n2. dos");
    let first = numbering_id(para_containing(&docx, "one")).expect("first numbered");
    let second = numbering_id(para_containing(&docx, "uno")).expect("second numbered");
    assert_ne!(
        first, second,
        "distinct lists must use distinct numbering ids so counters restart"
    );
}

#[test]
fn task_items_render_checkbox_glyphs_without_numbering() {
    let docx = docx_from_djot("- [x] done\n- [ ] todo");
    let done = para_containing(&docx, "done");
    let todo = para_containing(&docx, "todo");
    assert!(para_text(done).contains('\u{2612}'), "checked glyph ☒");
    assert!(para_text(todo).contains('\u{2610}'), "unchecked glyph ☐");
    // Task items are indented but not auto-numbered.
    assert_eq!(numbering_id(done), None);
    assert!(left_indent(done).unwrap_or(0) > 0);
}

fn assert_numbering_format(docx: &Docx, numbering_id: usize, expected_format: &str) {
    let num = docx
        .numberings
        .numberings
        .iter()
        .find(|n| n.id == numbering_id)
        .unwrap_or_else(|| panic!("numbering {numbering_id} registered"));
    let abstract_num = docx
        .numberings
        .abstract_nums
        .iter()
        .find(|a| a.id == num.abstract_num_id)
        .expect("abstract numbering registered");
    let level0 = &abstract_num.levels[0];
    assert_eq!(
        level0.format.val, expected_format,
        "numbering {numbering_id} level-0 format"
    );
}

// --- blockquotes -----------------------------------------------------------

#[test]
fn blockquote_paragraph_is_indented() {
    let docx = docx_from_djot("> a quoted line");
    let p = para_containing(&docx, "a quoted line");
    assert_eq!(left_indent(p), Some(720), "one quote level => 720 twips");
}

#[test]
fn nested_blockquote_indents_deeper() {
    let docx = docx_from_djot("> outer quote\n>\n> > inner quote");
    let outer = para_containing(&docx, "outer quote");
    let inner = para_containing(&docx, "inner quote");
    assert_eq!(left_indent(outer), Some(720));
    assert_eq!(
        left_indent(inner),
        Some(1440),
        "two quote levels => 1440 twips"
    );
}

/// The indent says how far in; the **style name** says what it is.
///
/// An indent is a measurement and cannot be read back as a claim — verse, a pressed Tab
/// and a quotation all look the same to it — which is why a manuscript's quotations came
/// home from `.docx` as plain paragraphs. `Quote` is Word's own built-in id for this, so a
/// paragraph carrying it lands on the style a Word user already has, and it is what
/// `document_ingest::sources::docx::StyleTable::quoted_as` matches.
#[test]
fn blockquote_paragraph_carries_the_named_quote_style() {
    let docx = docx_from_djot("> a quoted line");
    let p = para_containing(&docx, "a quoted line");
    assert_eq!(
        p.property.style.as_ref().map(|s| s.val.as_str()),
        Some("Quote"),
        "an indented paragraph with no name cannot be read back as a quotation"
    );
}

/// Every level of a nested quotation is named, and the direct indent still wins over the
/// style's own — so a nested quote stays visibly deeper than the one containing it.
#[test]
fn a_nested_blockquote_is_named_at_every_level_and_keeps_its_deeper_indent() {
    let docx = docx_from_djot("> outer quote\n>\n> > inner quote");
    for (text, indent) in [("outer quote", 720), ("inner quote", 1440)] {
        let p = para_containing(&docx, text);
        assert_eq!(
            p.property.style.as_ref().map(|s| s.val.as_str()),
            Some("Quote"),
            "{text} is inside a quotation and must say so"
        );
        assert_eq!(left_indent(p), Some(indent), "{text} kept its own indent");
    }
}

/// The style is declared, not merely referenced.
///
/// `docx-rs` ships no built-in styles at all, so a `w:pStyle` naming one this file never
/// defines is a dangling reference the reader resolves from its own catalogue — the same
/// failure `heading_style`'s own doc records for a book title that "asked to be a title
/// and arrived as whatever Word had lying around".
#[test]
fn the_quote_style_is_declared_in_the_stylesheet() {
    let bytes = docx_bytes_from_djot("> a quoted line");
    let styles = read_zip_entry(&bytes, "word/styles.xml");
    assert!(
        styles.contains(r#"w:styleId="Quote""#),
        "the Quote style is referenced but never declared: {styles}"
    );
}

/// An epigraph keeps its own named style rather than being demoted to a plain quotation.
///
/// An epigraph lives inside a blockquote too, so a `quote_depth > 0` arm placed before the
/// epigraph one would claim every epigraph as an ordinary quote — and the importer would
/// lose the distinction between a chapter's opening quotation and one in its prose, which
/// is the whole difference between an `EpigraphText` and a paragraph of the manuscript.
#[test]
fn an_epigraph_keeps_its_own_style_rather_than_the_ordinary_quote_one() {
    let docx = docx_from_djot(
        "> {semantic_role=epigraph}\n> All happy families are alike.\n",
    );
    let p = para_containing(&docx, "All happy families are alike.");
    assert_eq!(
        p.property.style.as_ref().map(|s| s.val.as_str()),
        Some("Epigraph"),
        "an epigraph must not be flattened into the ordinary quote style"
    );
}

// --- headings & plain ------------------------------------------------------

#[test]
fn heading_levels_use_heading_styles() {
    for level in 1..=6 {
        let hashes = "#".repeat(level);
        let marker = format!("Title{level}");
        let docx = docx_from_djot(&format!("{hashes} {marker}"));
        let p = para_containing(&docx, &marker);
        let style = p.property.style.as_ref().map(|s| s.val.as_str());
        assert_eq!(style, Some(format!("Heading{level}").as_str()));
    }
}

#[test]
fn plain_paragraph_has_no_numbering_indent_or_style() {
    let docx = docx_from_djot("An ordinary paragraph");
    let p = para_containing(&docx, "An ordinary paragraph");
    assert_eq!(numbering_id(p), None);
    assert_eq!(left_indent(p), None);
    assert_eq!(p.property.style, None);
}

// --- inline marks still work ----------------------------------------------

// --- end-to-end pack/unpack ------------------------------------------------

#[test]
fn rich_document_packs_to_a_valid_docx_file() {
    use document_io::docx_rs::read_docx;

    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, RICH_DJOT);

    let dir = std::env::temp_dir();
    let path = dir.join(format!("docx_export_rich_{}.docx", std::process::id()));
    let path_str = path.to_string_lossy().to_string();

    let mut mgr = LongOperationManager::new();
    let op = document_io_controller::export_docx(
        &db,
        &ev,
        &mut mgr,
        &ExportDocxDto {
            output_path: path_str.clone(),
            options: Default::default(),
        },
    )
    .expect("export_docx");
    wait(&mgr, &op);
    assert_eq!(
        mgr.get_operation_status(&op),
        Some(OperationStatus::Completed),
        "export should complete"
    );

    // The packed file exists and is a structurally valid .docx that docx-rs can
    // read back (this exercises the numbering/hyperlink relationship wiring done
    // at `build()`/`pack()` time, which the in-memory builder skips).
    let bytes = std::fs::read(&path).expect("output file exists");
    let parsed = read_docx(&bytes).expect("packed docx must be readable");
    assert!(
        !parsed.document.children.is_empty(),
        "round-tripped document has content"
    );
    assert!(
        !parsed.numberings.numberings.is_empty(),
        "list numbering definitions survive the pack/unpack"
    );

    let _ = std::fs::remove_file(&path);
}

// --- footnotes (real packaged parts) ----------------------------------------
//
// `word/footnotes.xml` is written on every export, even with zero footnotes
// (`docx-rs` still emits an empty `<w:footnotes .../>` stub) — so "the part
// exists" proves nothing. These assert on its actual content, and on the
// matching `<w:footnoteReference w:id="…"/>` in `word/document.xml` sharing
// the SAME id as the `<w:footnote w:id="…">` that carries the note's body:
// two independently-true-looking facts that only together prove the
// reference really resolves to that body inside the real, packaged file.

const FOOTNOTE_DJOT: &str = "\
Prose with a note[^n1] in it.

[^n1]: The note body for Word.
";

/// Extract every `w:id="N"` a `<w:footnoteReference .../>` element carries, in document order.
fn footnote_reference_ids(document_xml: &str) -> Vec<&str> {
    document_xml
        .match_indices("<w:footnoteReference ")
        .filter_map(|(i, _)| {
            let tail = &document_xml[i..];
            let id_start = tail.find("w:id=\"")? + "w:id=\"".len();
            let id_end = id_start + tail[id_start..].find('"')?;
            Some(&tail[id_start..id_end])
        })
        .collect()
}

/// Extract every `w:id="N"` a `<w:footnote w:id="…">` element opens, in document order.
fn footnote_body_ids(footnotes_xml: &str) -> Vec<&str> {
    footnotes_xml
        .match_indices("<w:footnote ")
        .filter_map(|(i, _)| {
            let tail = &footnotes_xml[i..];
            let id_start = tail.find("w:id=\"")? + "w:id=\"".len();
            let id_end = id_start + tail[id_start..].find('"')?;
            Some(&tail[id_start..id_end])
        })
        .collect()
}

#[test]
fn docx_footnote_reference_resolves_to_a_real_body_in_footnotes_xml() {
    let bytes = docx_bytes_from_djot(FOOTNOTE_DJOT);

    let document_xml = read_zip_entry(&bytes, "word/document.xml");
    let footnotes_xml = read_zip_entry(&bytes, "word/footnotes.xml");

    let ref_ids = footnote_reference_ids(&document_xml);
    assert_eq!(
        ref_ids.len(),
        1,
        "expected exactly one footnote reference in document.xml, got {ref_ids:?}: {document_xml}"
    );

    let body_ids = footnote_body_ids(&footnotes_xml);
    assert!(
        body_ids.contains(&ref_ids[0]),
        "document.xml references footnote id {:?}, but footnotes.xml only defines {body_ids:?}: {footnotes_xml}",
        ref_ids[0]
    );

    // The real body text, in the real packaged part — not merely a reference to it.
    assert!(
        footnotes_xml.contains("The note body for Word."),
        "the note's body never reached footnotes.xml: {footnotes_xml}"
    );
}

#[test]
fn docx_note_body_is_not_also_rendered_as_prose() {
    // A definition is a detached top-level frame; without skipping it in the main walk
    // (`notes.is_definition`) it would render in the middle of the document as an ordinary
    // paragraph, in addition to becoming the footnote's real body.
    let bytes = docx_bytes_from_djot(FOOTNOTE_DJOT);
    let document_xml = read_zip_entry(&bytes, "word/document.xml");
    assert!(
        !document_xml.contains("The note body for Word."),
        "the note body must not appear inline in document.xml, only inside the footnote: {document_xml}"
    );
}

#[test]
fn docx_dangling_footnote_reference_still_produces_a_note() {
    // "[^solo]" here names no definition anywhere — the normal state for a host that owns
    // note bodies itself. The marker must not be silently dropped from the sentence: Word
    // still gets a real (if body-less) footnote, matching `build_run`'s own documented
    // choice ("dropping the run entirely would delete the marker from the sentence").
    let bytes = docx_bytes_from_djot("Text with a note[^solo] in it.\n");

    let document_xml = read_zip_entry(&bytes, "word/document.xml");
    let ref_ids = footnote_reference_ids(&document_xml);
    assert_eq!(
        ref_ids.len(),
        1,
        "the dangling reference must still produce a real footnote reference: {document_xml}"
    );

    let footnotes_xml = read_zip_entry(&bytes, "word/footnotes.xml");
    let body_ids = footnote_body_ids(&footnotes_xml);
    assert!(
        body_ids.contains(&ref_ids[0]),
        "the reference's id must resolve to a real (even if empty) footnote: {footnotes_xml}"
    );
}

const REPEAT_FOOTNOTE_DJOT: &str =
    "First[^n1] and second[^n1] citation.\n\n[^n1]: The note body for Word.\n";

/// Citing the same label twice must produce exactly ONE real
/// `<w:footnoteReference>`/`<w:footnote>` pair, not two — `docx-rs`'s
/// `collect_footnotes()` turns every reference it finds into its own
/// `<w:footnote>` entry, so a naive second `add_footnote_reference` call
/// would both duplicate the body and share the first one's `w:id`, which
/// OOXML does not allow two definitions to. `build_run`'s fix: only the
/// first citation opens a real footnote; a repeat prints a plain run
/// instead (proven end to end here, against the packaged files, not just
/// the in-memory `Docx` struct — the same reasoning `docx_footnote_
/// reference_resolves_to_a_real_body_in_footnotes_xml`'s own doc comment
/// gives for testing this way).
#[test]
fn docx_repeat_citation_reuses_one_footnote_not_two() {
    let bytes = docx_bytes_from_djot(REPEAT_FOOTNOTE_DJOT);

    let document_xml = read_zip_entry(&bytes, "word/document.xml");
    let footnotes_xml = read_zip_entry(&bytes, "word/footnotes.xml");

    let ref_ids = footnote_reference_ids(&document_xml);
    assert_eq!(
        ref_ids.len(),
        1,
        "only the FIRST citation may become a real <w:footnoteReference>: {document_xml}"
    );

    let body_ids = footnote_body_ids(&footnotes_xml);
    assert_eq!(
        body_ids.len(),
        1,
        "citing one label twice must define exactly one <w:footnote>: {footnotes_xml}"
    );
    assert!(
        body_ids.contains(&ref_ids[0]),
        "the one reference must resolve to the one body: {footnotes_xml}"
    );

    assert_eq!(
        footnotes_xml.matches("The note body for Word.").count(),
        1,
        "the note body must not be duplicated: {footnotes_xml}"
    );

    // The repeat citation still reads as a footnote mark (Word's built-in
    // "FootnoteReference" character style) even though it opens no second
    // note — one occurrence from the real reference's own run properties,
    // one from the repeat's plain, styled-only run.
    assert_eq!(
        document_xml
            .matches("w:rStyle w:val=\"FootnoteReference\"")
            .count(),
        2,
        "both the real reference and the repeat's plain marker must carry \
         the FootnoteReference character style: {document_xml}"
    );
}

#[test]
fn bold_run_is_marked_bold() {
    let docx = docx_from_djot("normal *bolded* normal");
    let p = para_containing(&docx, "bolded");
    let has_bold = p.children.iter().any(|c| match c {
        ParagraphChild::Run(r) => {
            r.run_property.bold.is_some()
                && r.children
                    .iter()
                    .any(|rc| matches!(rc, RunChild::Text(t) if t.text.contains("bolded")))
        }
        _ => false,
    });
    assert!(has_bold, "the 'bolded' run should be bold");
}

// --- Manuscript / RTL export options (M5) ----------------------------------

use common::parser_tools::DocxExportOptions;

fn docx_from_djot_with_options(djot: &str, options: DocxExportOptions) -> Docx {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, djot);
    document_io_controller::build_docx_document(
        &db,
        &ExportDocxDto {
            output_path: String::new(),
            options,
        },
    )
    .expect("build_docx_document")
}

/// A block tagged `{direction=rtl}` must export with a paragraph-level `<w:bidi/>` — the one
/// bidi primitive docx-rs offers, and the fix for DOCX previously dropping direction entirely.
#[test]
fn rtl_block_exports_paragraph_bidi() {
    let docx = docx_from_djot("{direction=rtl}\nمرحبا بالعالم\n");
    let ps = paragraphs(&docx);
    assert_eq!(ps.len(), 1, "one paragraph");
    assert_eq!(
        ps[0].property.bidi,
        Some(true),
        "an RTL block gets paragraph bidi"
    );
}

/// A plain LTR block stays un-bidi (the default `to_docx` behaviour is untouched).
#[test]
fn ltr_block_has_no_bidi() {
    let docx = docx_from_djot("Hello world\n");
    let ps = paragraphs(&docx);
    assert_eq!(
        ps[0].property.bidi, None,
        "an LTR block is never marked bidi"
    );
}

/// Page geometry from the options lands on the document's section property, and the base font
/// size lands on the document defaults.
#[test]
fn options_apply_page_size_and_font_defaults() {
    let opts = DocxExportOptions {
        page_width_twips: Some(11906), // A4
        page_height_twips: Some(16838),
        font_family: Some("Courier New".to_string()),
        font_half_points: Some(24), // 12pt
        justify: true,
        first_line_indent_twips: Some(720),
        line_spacing_twips: Some(480),
        ..Default::default()
    };
    let docx = docx_from_djot_with_options("The wind rose over the hills.\n", opts);
    // PageSize's w/h fields are private; assert via the crate's own JSON serialization (the
    // same fallback the monospace-font test uses). 11906×16838 twips are the A4 dimensions.
    let json = docx.json();
    assert!(
        json.contains("11906") && json.contains("16838"),
        "A4 page size in section props"
    );

    // The single body paragraph is justified, spaced, and first-line indented.
    let ps = paragraphs(&docx);
    assert_eq!(
        alignment(ps[0]),
        Some("justified"),
        "justify → jc=justified"
    );
    assert!(ps[0].property.line_spacing.is_some(), "line spacing set");
    let ind = ps[0].property.indent.as_ref().expect("indent set");
    assert!(
        matches!(
            ind.special_indent,
            Some(document_io::docx_rs::SpecialIndentType::FirstLine(720))
        ),
        "first-line indent of 720 twips"
    );
}

/// A page-numbered running header is attached when requested.
#[test]
fn page_numbers_attach_a_header() {
    let opts = DocxExportOptions {
        page_numbers: true,
        running_header: Some("Vane / THE LIGHTHOUSE".to_string()),
        ..Default::default()
    };
    let docx = docx_from_djot_with_options("Prose.\n", opts);
    assert!(
        docx.document_rels.header_count > 0,
        "a header relationship was registered"
    );
}

// --- per-block spacing overrides (what a scene break needs) ----------------

#[test]
fn a_blocks_own_text_indent_overrides_the_document_wide_one() {
    // A scene break suppresses the indent on the paragraph that follows it by
    // setting `text_indent=0`; every other paragraph keeps the preset's indent.
    let options = DocxExportOptions {
        first_line_indent_twips: Some(720),
        ..Default::default()
    };
    let docx = docx_from_djot_with_options(
        "Indented paragraph.\n\n{text_indent=0}\nFlush paragraph.",
        options,
    );
    assert_eq!(
        first_line_indent(para_containing(&docx, "Indented paragraph.")),
        Some(720),
        "an ordinary paragraph keeps the document-wide first-line indent"
    );
    assert_eq!(
        first_line_indent(para_containing(&docx, "Flush paragraph.")),
        None,
        "text_indent=0 must suppress the indent, not inherit it"
    );
}

#[test]
fn a_blocks_own_top_margin_becomes_space_before() {
    // 24 logical px × 15 twips/px = 360 twips.
    let docx = docx_from_djot_with_options(
        "Before.\n\n{top_margin=24}\nAfter.",
        DocxExportOptions::default(),
    );
    assert_eq!(space_before(para_containing(&docx, "After.")), Some(360));
    assert_eq!(
        space_before(para_containing(&docx, "Before.")),
        None,
        "a paragraph without the attribute gets no space-above"
    );
}
