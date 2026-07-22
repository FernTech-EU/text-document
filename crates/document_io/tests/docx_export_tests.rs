//! Feature tests for the DOCX exporter.
//!
//! Documents are built with the (well-tested) djot importer, then exported via
//! the file-less builder [`document_io_controller::build_docx_document`], and
//! the resulting [`docx_rs::Docx`] structure is asserted directly. This
//! exercises the exact builder used to write `.docx` files without touching the
//! filesystem.

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
