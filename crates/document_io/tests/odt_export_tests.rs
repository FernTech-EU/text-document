// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Cyril Jacquet

//! Feature tests for the ODT (OpenDocument Text) exporter.
//!
//! Documents are built with the (well-tested) djot importer, then exported via the file-less
//! builder [`document_io_controller::build_odt_document`], and the resulting bytes are read back
//! as a zip archive — mirroring `docx_export_tests.rs`/`epub_export_tests.rs`'s approach. Most
//! assertions are on the raw `content.xml`/`styles.xml` strings rather than through a typed
//! builder, because — unlike DOCX (`docx_rs::Docx`) and EPUB (`epub_builder`) — there is no
//! ODF-writing crate here to hand back a typed document; the writer's own output *is* the XML
//! text (see `crate::odt_render`'s module doc for why).
//!
//! [`rich_document_packs_to_a_valid_odt_file_soffice_can_convert`] is the one test that proves
//! more than well-formed XML: it writes a real `.odt` to disk and runs it through
//! `soffice --headless --convert-to pdf`, which fails outright on a file LibreOffice cannot
//! actually open — the same bar `docx_export_tests.rs`'s `rich_document_packs_to_a_valid_docx_file`
//! sets for DOCX (there, packing through `docx-rs`'s own `build()`/`pack()`/`read_docx` proves it;
//! here, with no such library, a real, independent reader has to be the judge).

extern crate text_document_io as document_io;

use common::long_operation::{LongOperationManager, OperationStatus};
use common::parser_tools::{ExportImage, ExportImages, OdtExportOptions};
use document_io::{ExportOdtDto, ImportDjotDto, ImportPlainTextDto, document_io_controller};
use std::io::{Cursor, Read};
use test_harness::{EventHub, setup};

use std::sync::Arc;

/// A document touching every M-T2a feature: a heading, a centered paragraph with a hyperlink and
/// a footnote reference, a bulleted list with one nested item, an ordered list, two task items, a
/// blockquote carrying an epigraph + its right-aligned attribution, an RTL paragraph, an inline
/// image, a fenced code block, and the footnote's own definition.
///
/// The scene-break/rule heuristic is deliberately tested separately (see
/// `a_scene_break_glyph_line_becomes_an_empty_rule_paragraph`), not from this document: djot has
/// its OWN native thematic-break syntax (`* * *` on its own line), which the djot *importer*
/// (`content_parser.rs`'s `E::ThematicBreak(_) => {}` arm) silently drops before it ever becomes
/// a `Block` at all — so it can never reach this writer to prove anything either way. The
/// scenario this writer's heuristic exists for is a `Block` whose plain text merely *looks like*
/// one of `skribisto_compiler::preset`'s literal scene-break glyphs (because that is what a
/// document assembled outside the djot importer — e.g. by Skribisto's own compiler — actually
/// contains), reached here via `import_plain_text`, which performs no markdown/djot
/// interpretation at all.
const RICH_DJOT: &str = "\
# Chapter One

{alignment=center}
Centered intro with a [link](https://example.com) and a note[^n1].

- bullet one
- bullet two

  - nested bullet

1. first
2. second

- [x] done task
- [ ] pending task

> {semantic_role=epigraph}
> All happy families are alike.
>
> {alignment=right}
> Tolstoy

{direction=rtl}
نص عربي هنا.

![A cat](cat.png)

```rust
let answer = 42;
```

More prose after the code block.

[^n1]: The note body.
";

// --- harness ---------------------------------------------------------------

fn wait(mgr: &LongOperationManager, op_id: &str) {
    while let Some(OperationStatus::Running) = mgr.get_operation_status(op_id) {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
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

/// A real 4×3 PNG — `image::load_from_memory` (which `build_image_frame` calls to size an image
/// with no explicit display dimensions) validates the bytes it is handed, so a placeholder would
/// fail the export rather than test the wiring. Mirrors `pdf_export_tests.rs::png_bytes`.
fn png_bytes() -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, 4, 3);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[0u8, 128, 255, 255].repeat(12))
            .unwrap();
    }
    buf
}

fn options_with_image() -> OdtExportOptions {
    OdtExportOptions {
        images: ExportImages::from_iter([("cat.png", ExportImage::new(png_bytes(), "image/png"))]),
        ..Default::default()
    }
}

/// Import `djot` into a fresh document and return the packaged ODT bytes.
fn odt_from_djot(djot: &str, options: OdtExportOptions) -> Vec<u8> {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, djot);
    document_io_controller::build_odt_document(
        &db,
        &ExportOdtDto {
            output_path: String::new(),
            options,
        },
    )
    .expect("build_odt_document")
}

/// Import `text` verbatim (one block per `\n`-separated line, no markdown/djot interpretation —
/// see `ImportPlainTextUseCase::execute`) and return the packaged ODT bytes. Used only for the
/// scene-break heuristic, which djot's own native thematic-break syntax makes untestable through
/// `odt_from_djot` — see `RICH_DJOT`'s doc comment.
fn odt_from_plain_text(text: &str, options: OdtExportOptions) -> Vec<u8> {
    let (db, ev, _) = setup().expect("setup");
    document_io_controller::import_plain_text(
        &db,
        &ev,
        &ImportPlainTextDto {
            plain_text: text.to_string(),
        },
    )
    .expect("import_plain_text");
    document_io_controller::build_odt_document(
        &db,
        &ExportOdtDto {
            output_path: String::new(),
            options,
        },
    )
    .expect("build_odt_document")
}

fn zip_entry_names(bytes: &[u8]) -> Vec<String> {
    let archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("packaged ODT is a valid zip");
    archive.file_names().map(|s| s.to_string()).collect()
}

fn read_zip_entry(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).expect("packaged ODT is a valid zip");
    let mut file = archive
        .by_name(name)
        .unwrap_or_else(|_| panic!("entry {name:?} present in the ODT package"));
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).expect("entry is readable");
    contents
}

fn read_zip_text(bytes: &[u8], name: &str) -> String {
    String::from_utf8(read_zip_entry(bytes, name)).expect("entry is valid utf-8")
}

fn content_xml(bytes: &[u8]) -> String {
    read_zip_text(bytes, "content.xml")
}

fn styles_xml(bytes: &[u8]) -> String {
    read_zip_text(bytes, "styles.xml")
}

// --- package structure -------------------------------------------------------

#[test]
fn odt_export_is_a_valid_zip_with_required_entries() {
    let bytes = odt_from_djot(RICH_DJOT, options_with_image());
    assert!(!bytes.is_empty(), "packaged ODT must be non-empty");

    let names = zip_entry_names(&bytes);
    for expected in [
        "mimetype",
        "META-INF/manifest.xml",
        "content.xml",
        "styles.xml",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "ODT must contain {expected}, got {names:?}"
        );
    }
}

#[test]
fn mimetype_is_stored_uncompressed_as_the_first_entry() {
    let bytes = odt_from_djot("Just prose.", OdtExportOptions::default());
    let names = zip_entry_names(&bytes);
    assert_eq!(
        names.first().map(String::as_str),
        Some("mimetype"),
        "mimetype must be the FIRST entry in the package — an ODF container requirement"
    );

    let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).expect("valid zip");
    let file = archive.by_name("mimetype").expect("mimetype entry");
    assert_eq!(
        file.compression(),
        zip::CompressionMethod::Stored,
        "mimetype must be stored, not deflated"
    );
    drop(file);

    let contents = read_zip_text(&bytes, "mimetype");
    assert_eq!(contents, "application/vnd.oasis.opendocument.text");
}

#[test]
fn manifest_declares_the_odf_media_type_and_every_real_part() {
    let bytes = odt_from_djot(RICH_DJOT, options_with_image());
    let manifest = read_zip_text(&bytes, "META-INF/manifest.xml");
    assert!(manifest.contains("application/vnd.oasis.opendocument.text"));
    assert!(manifest.contains("manifest:full-path=\"content.xml\""));
    assert!(manifest.contains("manifest:full-path=\"styles.xml\""));
    assert!(
        manifest.contains("Pictures/img_001.png"),
        "the embedded image must be listed in the manifest: {manifest}"
    );
}

// --- headings / paragraphs / alignment --------------------------------------

#[test]
fn heading_becomes_a_text_h_with_explicit_outline_level() {
    let bytes = odt_from_djot(RICH_DJOT, OdtExportOptions::default());
    let content = content_xml(&bytes);
    assert!(
        content.contains(
            "<text:h text:style-name=\"Heading_1\" text:outline-level=\"1\">Chapter One</text:h>"
        ),
        "heading not found as expected in: {content}"
    );
}

#[test]
fn centered_paragraph_carries_the_alignment_and_a_hyperlink() {
    let bytes = odt_from_djot(RICH_DJOT, OdtExportOptions::default());
    let content = content_xml(&bytes);
    // The paragraph's automatic style must carry fo:text-align="center" somewhere in
    // content.xml's automatic-styles block.
    assert!(
        content.contains("fo:text-align=\"center\""),
        "centered alignment missing: {content}"
    );
    assert!(
        content.contains(
            "<text:a xlink:type=\"simple\" xlink:href=\"https://example.com\">link</text:a>"
        ),
        "hyperlink missing or malformed: {content}"
    );
}

// --- lists -------------------------------------------------------------------

#[test]
fn bullets_and_a_nested_bullet_produce_a_nested_text_list() {
    let bytes = odt_from_djot(RICH_DJOT, OdtExportOptions::default());
    let content = content_xml(&bytes);

    // Two top-level bullet items, the second of which opens a nested <text:list> before it
    // closes — i.e. the nested list's open tag appears after "bullet two" but before that
    // item's own closing tag.
    let outer_start = content
        .find("<text:list ")
        .expect("a top-level list must exist");
    let bullet_two = content
        .find("bullet two")
        .expect("bullet two must be present");
    let nested_list = content[bullet_two..]
        .find("<text:list ")
        .map(|i| i + bullet_two)
        .expect("a nested <text:list> must open after \"bullet two\"");
    let nested_bullet = content
        .find("nested bullet")
        .expect("nested bullet text must be present");
    assert!(outer_start < bullet_two);
    assert!(bullet_two < nested_list);
    assert!(nested_list < nested_bullet);

    // Exactly one bullet-style list-style is declared with a bullet char (•), used by both the
    // outer and nested list (uniform-per-level, see `odt_list_style_xml`'s doc comment).
    assert!(
        content.contains("text:bullet-char=\"\u{2022}\""),
        "no bullet glyph declared: {content}"
    );
}

#[test]
fn ordered_list_uses_a_decimal_number_format() {
    let bytes = odt_from_djot(RICH_DJOT, OdtExportOptions::default());
    let content = content_xml(&bytes);
    assert!(
        content.contains("style:num-format=\"1\""),
        "no decimal numbering format declared: {content}"
    );
    assert!(content.contains("first"));
    assert!(content.contains("second"));
}

#[test]
fn task_items_are_plain_paragraphs_with_a_checkbox_glyph_not_list_items() {
    let bytes = odt_from_djot(RICH_DJOT, OdtExportOptions::default());
    let content = content_xml(&bytes);
    assert!(
        content.contains("\u{2612}") && content.contains("done task"),
        "checked-task glyph missing: {content}"
    );
    assert!(
        content.contains("\u{2610}") && content.contains("pending task"),
        "unchecked-task glyph missing: {content}"
    );
    // Neither task line should sit inside a <text:list-item> — they are plain paragraphs.
    let done_pos = content.find("done task").unwrap();
    let preceding = &content[..done_pos];
    let last_list_item_open = preceding.rfind("<text:list-item>");
    let last_list_item_close = preceding.rfind("</text:list-item>");
    // If a <text:list-item> opened more recently than one closed, "done task" would be inside
    // it — assert that is not the case.
    assert!(
        last_list_item_close.unwrap_or(0) >= last_list_item_open.unwrap_or(0),
        "the checked task landed inside a <text:list-item>: {content}"
    );
}

// --- blockquote / epigraph ----------------------------------------------------

#[test]
fn epigraph_blockquote_uses_the_named_epigraph_styles() {
    let bytes = odt_from_djot(RICH_DJOT, OdtExportOptions::default());
    let content = content_xml(&bytes);
    assert!(
        content.contains("text:style-name=\"Epigraph\"") || {
            // An automatic style parented off "Epigraph" is also acceptable if quote-depth
            // margins were layered on — but with no options overrides this document should
            // reference the named style directly.
            content.contains("style:parent-style-name=\"Epigraph\"")
        },
        "no epigraph-styled paragraph: {content}"
    );
    assert!(
        content.contains("text:style-name=\"EpigraphAttribution\"")
            || content.contains("style:parent-style-name=\"EpigraphAttribution\""),
        "no epigraph-attribution-styled paragraph: {content}"
    );
    assert!(content.contains("All happy families"));
    assert!(content.contains("Tolstoy"));
}

// --- RTL -----------------------------------------------------------------------

#[test]
fn rtl_block_sets_writing_mode_rl_tb() {
    let bytes = odt_from_djot(RICH_DJOT, OdtExportOptions::default());
    let content = content_xml(&bytes);
    assert!(
        content.contains("style:writing-mode=\"rl-tb\""),
        "no RTL paragraph style found: {content}"
    );
}

// --- code block ------------------------------------------------------------

#[test]
fn code_block_uses_the_monospace_code_style() {
    let bytes = odt_from_djot(RICH_DJOT, OdtExportOptions::default());
    let content = content_xml(&bytes);
    assert!(
        content.contains("text:style-name=\"Code_Block\""),
        "no code-block-styled paragraph: {content}"
    );
    assert!(content.contains("let answer = 42;"));
    let styles = styles_xml(&bytes);
    assert!(styles.contains("Courier New"));
}

// --- scene break / horizontal rule -------------------------------------------

#[test]
fn a_scene_break_glyph_line_becomes_an_empty_rule_paragraph() {
    // "* * *" is `skribisto_compiler::preset`'s own default minor-break glyph; imported as raw
    // plain text (one block per line, no djot interpretation), it reaches the writer exactly the
    // way a compiled-from-the-model manuscript would.
    let bytes = odt_from_plain_text("Before.\n* * *\nAfter.", OdtExportOptions::default());
    let content = content_xml(&bytes);
    assert!(
        content.contains("<text:p text:style-name=\"Rule\"/>"),
        "scene break did not become an empty Rule paragraph: {content}"
    );
    // The glyph itself ("* * *") must NOT appear as literal text anywhere in the body.
    assert!(
        !content.contains("* * *"),
        "the literal glyph leaked into the body instead of becoming a Rule paragraph: {content}"
    );
    assert!(content.contains("Before."));
    assert!(content.contains("After."));
}

/// Every other preset glyph `skribisto_compiler::preset` offers must be recognised too — the
/// whole reason `looks_like_rule_glyph` matches a *shape* rather than a fixed string (see this
/// module's and `export_odt_uc`'s doc comments).
#[test]
fn every_known_scene_break_preset_glyph_is_recognised() {
    for glyph in [
        "#", "# # #", "*", "***", ". . .", "\u{FF0A}", "\u{25C7}", "###", "+++", "-",
    ] {
        let bytes = odt_from_plain_text(glyph, OdtExportOptions::default());
        let content = content_xml(&bytes);
        assert!(
            content.contains("<text:p text:style-name=\"Rule\"/>"),
            "glyph {glyph:?} was not recognised as a scene break: {content}"
        );
    }
}

#[test]
fn an_ordinary_short_line_is_not_mistaken_for_a_scene_break() {
    // A single real word is not "one non-alphanumeric character repeated" and must render as
    // ordinary text, not as a Rule paragraph.
    let bytes = odt_from_djot("Ok.", OdtExportOptions::default());
    let content = content_xml(&bytes);
    assert!(!content.contains("text:style-name=\"Rule\""));
    assert!(content.contains("Ok."));
}

#[test]
fn a_formatted_or_linked_glyph_line_is_not_mistaken_for_a_scene_break() {
    // Real content that merely happens to be short and symbol-heavy — e.g. an em dash the
    // writer bolded for emphasis — must never be silently replaced by an empty Rule paragraph.
    let bytes = odt_from_djot("{alignment=center}\n**—**", OdtExportOptions::default());
    let content = content_xml(&bytes);
    assert!(!content.contains("text:style-name=\"Rule\""));
    assert!(content.contains("\u{2014}"));
}

/// The `styles.xml` "Rule" style must be exactly a bottom border with every other side
/// explicitly `"none"` — the shape `document_ingest::sources::odt::StyleTable::is_rule` detects.
/// See `export_odt_uc`'s module doc comment for why nothing may ever be layered on top of it.
#[test]
fn the_rule_style_is_a_bottom_border_only_with_other_sides_explicitly_none() {
    let bytes = odt_from_djot(RICH_DJOT, OdtExportOptions::default());
    let styles = styles_xml(&bytes);
    let start = styles
        .find("style:name=\"Rule\"")
        .expect("Rule style must be declared");
    let props_start = styles[start..]
        .find("<style:paragraph-properties")
        .map(|i| i + start)
        .expect("Rule style must carry paragraph-properties");
    let props_end = styles[props_start..]
        .find("/>")
        .map(|i| i + props_start)
        .expect("self-closing paragraph-properties element");
    let props = &styles[props_start..props_end];
    assert!(props.contains("fo:border-bottom=\"0.5pt solid #000000\""));
    assert!(props.contains("fo:border-top=\"none\""));
    assert!(props.contains("fo:border-left=\"none\""));
    assert!(props.contains("fo:border-right=\"none\""));
}

// --- footnotes -----------------------------------------------------------------

#[test]
fn footnote_reference_becomes_a_real_text_note_with_a_matching_body() {
    let bytes = odt_from_djot(RICH_DJOT, OdtExportOptions::default());
    let content = content_xml(&bytes);
    assert!(
        content.contains("<text:note text:id=\"ftn1\" text:note-class=\"footnote\">"),
        "no real footnote found: {content}"
    );
    assert!(content.contains("<text:note-citation>1</text:note-citation>"));
    assert!(
        content.contains("The note body."),
        "note body text missing: {content}"
    );
}

#[test]
fn a_repeated_citation_of_the_same_label_does_not_duplicate_the_note() {
    let djot = "\
First citation[^n].\n\nSecond citation[^n] too.\n\n[^n]: The one true body.\n";
    let bytes = odt_from_djot(djot, OdtExportOptions::default());
    let content = content_xml(&bytes);
    assert_eq!(
        content.matches("<text:note ").count(),
        1,
        "a repeated citation must not open a second real note: {content}"
    );
    assert_eq!(
        content.matches("The one true body.").count(),
        1,
        "the note body text must appear exactly once: {content}"
    );
    // The second citation still shows the SAME marker, as plain superscript text.
    assert!(
        content.contains("style:text-position=\"super"),
        "repeated citation must use a superscript run: {content}"
    );
}

// --- images --------------------------------------------------------------------

#[test]
fn image_is_embedded_under_pictures_and_referenced_as_char_anchored() {
    let bytes = odt_from_djot(RICH_DJOT, options_with_image());
    let content = content_xml(&bytes);
    assert!(
        content.contains("<draw:frame ") && content.contains("text:anchor-type=\"as-char\""),
        "no inline draw:frame found: {content}"
    );
    assert!(
        content.contains("xlink:href=\"Pictures/img_001.png\""),
        "image href missing or wrong: {content}"
    );
    assert!(
        content.contains("<svg:title>A cat</svg:title>"),
        "alt text not carried as svg:title: {content}"
    );

    let embedded = read_zip_entry(&bytes, "Pictures/img_001.png");
    assert_eq!(
        embedded,
        png_bytes(),
        "embedded image bytes must be the original bytes, unmodified (no transcoding, unlike DOCX)"
    );
}

#[test]
fn an_image_with_no_bytes_supplied_falls_back_to_its_alt_text() {
    // No `options.images` entry for "cat.png" at all — the export must not fail, and must
    // degrade to the alt text rather than a dangling reference.
    let bytes = odt_from_djot("![A cat](cat.png)", OdtExportOptions::default());
    let content = content_xml(&bytes);
    assert!(!content.contains("<draw:frame"));
    assert!(content.contains("A cat"));
}

// --- tables --------------------------------------------------------------------
//
// The djot importer has no table syntax (`RICH_DJOT` never contains one), so these tests build
// a table structurally instead, through `text-document`'s own (`public_api`) high-level API —
// `TextCursor::insert_table` + `TextTable::cell` + a cursor positioned via `snapshot_flow()`'s
// reported cell position — rather than the raw entity layer. That is a deliberate choice, not
// just convenience: `common::entities::Block::document_position` (a per-frame ordering index)
// and `InsertTextDto::position`/`::anchor` (a document-wide *addressable character offset* — the
// same space `snapshot_flow()` reports in) turn out NOT to share a value space, so hand-deriving
// the former and handing it to the latter silently mis-targets the insertion; `public_api`'s
// `text_table_tests.rs::new_doc_with_text_and_table` establishes the correct recipe, followed
// here. See `document_io/Cargo.toml`'s comment on the resulting dev-only dependency cycle.
//
// A real file-writing `to_odt_with_options` + `Operation::wait()` round trip is used here
// instead of the file-less `build_odt_document` every other test in this file calls, because
// `text-document`'s own API has no file-less byte-returning export — only a real operation that
// writes to a path.

/// Export `doc` to a temporary `.odt` file and return its bytes, cleaning up afterwards.
fn odt_bytes_from_document(
    doc: &text_document::TextDocument,
    options: OdtExportOptions,
) -> Vec<u8> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let path =
        std::env::temp_dir().join(format!("odt_table_test_{}_{nanos}.odt", std::process::id()));
    doc.to_odt_with_options(&path.to_string_lossy(), options)
        .expect("to_odt_with_options")
        .wait()
        .expect("odt export completes");
    let bytes = std::fs::read(&path).expect("read exported odt");
    let _ = std::fs::remove_file(&path);
    bytes
}

/// `(row, column)` → the block position `snapshot_flow()` reports for that cell right now — the
/// correct, document-wide addressable-character-space value `TextCursor::cursor_at`/
/// `insert_text` expect. Read in one pass, before any cell is typed into: typing into one cell
/// shifts the recorded position of every block after it, so gathering every position up front
/// and then applying edits in **descending** position order (as every caller below does) means
/// an edit can only ever invalidate positions that have already been consumed.
fn all_cell_positions(doc: &text_document::TextDocument) -> Vec<((usize, usize), usize)> {
    let snapshot = doc.snapshot_flow();
    let table = snapshot
        .elements
        .iter()
        .find_map(|e| match e {
            text_document::FlowElementSnapshot::Table(t) => Some(t),
            _ => None,
        })
        .expect("a table must exist in the flow");
    table
        .cells
        .iter()
        .map(|cell| {
            let position = cell
                .blocks
                .first()
                .map(|b| b.position)
                .expect("a cell always has at least one block");
            ((cell.row, cell.column), position)
        })
        .collect()
}

#[test]
fn table_2x2_renders_with_the_correct_grid_and_cell_text() {
    let doc = text_document::TextDocument::new();
    doc.cursor().insert_table(2, 2).expect("insert_table");

    let mut positions = all_cell_positions(&doc);
    positions.sort_by_key(|&(_, pos)| std::cmp::Reverse(pos));
    for ((row, col), pos) in positions {
        let text = match (row, col) {
            (0, 0) => "R0C0",
            (0, 1) => "R0C1",
            (1, 0) => "R1C0",
            (1, 1) => "R1C1",
            _ => panic!("unexpected cell ({row}, {col}) in a 2x2 table"),
        };
        doc.cursor_at(pos).insert_text(text).expect("insert_text");
    }

    let content = content_xml(&odt_bytes_from_document(&doc, OdtExportOptions::default()));

    assert_eq!(content.matches("<table:table ").count(), 1, "{content}");
    assert_eq!(content.matches("<table:table-row>").count(), 2, "{content}");
    // 4 real cells, no covered ones — no spans in this table.
    assert_eq!(content.matches("<table:table-cell").count(), 4, "{content}");
    assert_eq!(
        content.matches("<table:covered-table-cell").count(),
        0,
        "{content}"
    );
    for text in ["R0C0", "R0C1", "R1C0", "R1C1"] {
        assert!(content.contains(text), "missing {text:?}: {content}");
    }
}

/// Proves the grid-coverage logic `render_table_odt` documents: ODF's table model (unlike
/// OOXML's) requires an explicit element at every row/column position, so a column span must
/// leave behind an explicit `<table:covered-table-cell/>`, not merely omit an element the way a
/// DOCX column span does.
#[test]
fn a_merged_table_cell_emits_a_column_span_and_a_covered_cell() {
    let doc = text_document::TextDocument::new();
    let table = doc.cursor().insert_table(2, 2).expect("insert_table");

    // Merge row 1's two cells into one BEFORE typing: merging after typing would cascade-delete
    // the frame/block (and therefore the text) of every absorbed cell but the surviving one.
    doc.cursor()
        .merge_table_cells(table.id(), 1, 0, 1, 1)
        .expect("merge_table_cells");

    let mut positions = all_cell_positions(&doc);
    assert_eq!(positions.len(), 3, "the merge must leave exactly 3 cells");
    positions.sort_by_key(|&(_, pos)| std::cmp::Reverse(pos));
    for ((row, col), pos) in positions {
        let text = match (row, col) {
            (0, 0) => "R0C0",
            (0, 1) => "R0C1",
            (1, 0) => "Spanning",
            _ => panic!("unexpected surviving cell ({row}, {col})"),
        };
        doc.cursor_at(pos).insert_text(text).expect("insert_text");
    }

    let content = content_xml(&odt_bytes_from_document(&doc, OdtExportOptions::default()));

    assert!(
        content.contains("table:number-columns-spanned=\"2\""),
        "no column span attribute: {content}"
    );
    assert!(
        content.contains("<table:covered-table-cell/>"),
        "no covered-cell placeholder for the span: {content}"
    );
    assert!(content.contains("Spanning"));
    assert!(content.contains("R0C0"));
    assert!(content.contains("R0C1"));
}

// --- heading ramp / options --------------------------------------------------

#[test]
fn default_heading_ramp_scales_off_the_body_size() {
    let options = OdtExportOptions {
        font_half_points: Some(24), // 12pt body
        ..Default::default()
    };
    let bytes = odt_from_djot("# A Title", options);
    let styles = styles_xml(&bytes);
    // Level 1 of the default ramp is 1.80x the body size, computed in half-points and rounded
    // there (matching `DocxHeadingStyle::default_ramp`'s identical arithmetic): 24 * 1.80 =
    // 43.2 half-points, rounds to 43, i.e. 21.5pt — not the 21.6pt a direct point-space
    // multiplication would give.
    assert!(
        styles.contains("fo:font-size=\"21.5pt\""),
        "heading ramp size not applied: {styles}"
    );
    assert!(styles.contains("fo:font-weight=\"bold\""));
}

#[test]
fn page_numbers_option_writes_a_header_with_a_page_number_field() {
    let options = OdtExportOptions {
        page_numbers: true,
        running_header: Some("Author / TITLE".to_string()),
        ..Default::default()
    };
    let bytes = odt_from_djot("Body.", options);
    let styles = styles_xml(&bytes);
    assert!(styles.contains("<style:header>"));
    assert!(styles.contains("<text:page-number>"));
    assert!(styles.contains("Author / TITLE"));
}

// --- end-to-end pack + LibreOffice validation -------------------------------

/// Writes a real `.odt` file to disk via the actual `export_odt` long operation (not the
/// file-less builder every other test in this file uses) and proves it with an independent
/// reader: LibreOffice itself, headless, converting it to PDF. A `soffice` failure here means
/// the file is not just malformed XML but something LibreOffice genuinely cannot open — the same
/// bar `docx_export_tests.rs::rich_document_packs_to_a_valid_docx_file` sets via `read_docx`,
/// applied through an external, independent tool since no ODF-reading crate lives in this repo.
///
/// `-env:UserInstallation=file://…` points LibreOffice at a private, per-test profile directory
/// rather than the invoking user's real one — required so this test cannot collide with (or wait
/// on a lock held by) an actual running LibreOffice session, and so parallel test runs never
/// share one profile.
#[test]
fn rich_document_packs_to_a_valid_odt_file_soffice_can_convert() {
    let soffice = which_soffice();
    let Some(soffice) = soffice else {
        eprintln!("soffice not found on PATH; skipping LibreOffice validation");
        return;
    };

    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, RICH_DJOT);

    let dir = std::env::temp_dir().join(format!("odt_export_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let odt_path = dir.join("rich.odt");
    let profile_dir = dir.join("lo_profile");

    let mut mgr = LongOperationManager::new();
    let op = document_io_controller::export_odt(
        &db,
        &ev,
        &mut mgr,
        &ExportOdtDto {
            output_path: odt_path.to_string_lossy().to_string(),
            options: options_with_image(),
        },
    )
    .expect("export_odt");
    wait(&mgr, &op);
    assert_eq!(
        mgr.get_operation_status(&op),
        Some(OperationStatus::Completed),
        "export should complete"
    );
    assert!(odt_path.exists(), "the .odt file must exist on disk");

    let output = std::process::Command::new(&soffice)
        .arg("--headless")
        .arg("--norestore")
        .arg(format!(
            "-env:UserInstallation=file://{}",
            profile_dir.display()
        ))
        .arg("--convert-to")
        .arg("pdf")
        .arg("--outdir")
        .arg(&dir)
        .arg(&odt_path)
        .output()
        .expect("failed to run soffice");

    assert!(
        output.status.success(),
        "soffice --convert-to pdf failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let pdf_path = dir.join("rich.pdf");
    let pdf_bytes = std::fs::read(&pdf_path).unwrap_or_else(|e| {
        panic!(
            "soffice reported success but produced no PDF at {pdf_path:?}: {e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert!(
        pdf_bytes.starts_with(b"%PDF-"),
        "LibreOffice's own output must be a real PDF"
    );
    assert!(
        pdf_bytes.len() > 1000,
        "the converted PDF is suspiciously small ({} bytes) — LibreOffice likely rendered an \
         empty or near-empty page",
        pdf_bytes.len()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn which_soffice() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("soffice");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
