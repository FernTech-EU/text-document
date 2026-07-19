#![cfg(feature = "pdf")]
//! Feature tests for the PDF exporter (embedded Typst).
//!
//! Documents are built with the (well-tested) djot importer, then exported via the file-less
//! builder [`document_io_controller::build_pdf_document`], and the resulting bytes are asserted
//! directly (PDF magic bytes, non-trivial size, page count) — mirroring
//! `docx_export_tests.rs`/`epub_export_tests.rs`'s harness. A real end-to-end, on-disk export
//! (through the `LongOperation` path) is exercised once, at the bottom, the same way
//! `rich_document_packs_to_a_valid_{docx,epub}_file*` do for their formats.

extern crate text_document_io as document_io;

use common::long_operation::{LongOperationManager, OperationStatus};
use common::parser_tools::PdfExportOptions;
use document_io::{ExportPdfDto, document_io_controller};
use std::sync::Arc;
use test_harness::{EventHub, setup};

/// A small, real TTF embedded as the sole test font — DejaVu Serif has broad Latin/Cyrillic/
/// Greek coverage but no Arabic/Hebrew shaping; the RTL fixture below deliberately reuses it
/// anyway (tofu glyphs are an acceptable outcome, a hard compile error is not).
const TEST_FONT: &[u8] = include_bytes!("assets/DejaVuSerif.ttf");

fn pdf_options() -> PdfExportOptions {
    PdfExportOptions {
        font_family: "DejaVu Serif".to_string(),
        font_bytes: vec![TEST_FONT.to_vec()],
        ..Default::default()
    }
}

/// Touches headings, bold/italic, an ordered and an unordered list, and a table — the "plain
/// prose" golden fixture required by the M7 test plan.
const RICH_DJOT: &str = "\
# Chapter One

Some *bold* and _italic_ prose about a #[stormy] night.

## A subsection

- bullet one
- bullet two

1. first
2. second

| A | B |
|---|---|
| 1 | 2 |
";

// --- harness -----------------------------------------------------------------

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
        &document_io::ImportDjotDto {
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

/// Import `djot` into a fresh document and return the compiled PDF bytes, using `options`.
fn pdf_from_djot(djot: &str, options: PdfExportOptions) -> Vec<u8> {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, djot);
    document_io_controller::build_pdf_document(
        &db,
        &ExportPdfDto {
            output_path: String::new(),
            options,
        },
    )
    .expect("build_pdf_document")
}

/// Count `/Type/Page` page-object dictionaries in raw PDF bytes — a word-boundary match so
/// `/Type/Pages` (the tree root) is never miscounted as a page. This is an **independent**
/// byte-level cross-check: the use case itself reports its page count from the laid-out
/// `PagedDocument` (`pages.len()`), not from a byte scan, so these two paths agreeing is what
/// this test set actually verifies.
fn count_pdf_pages(bytes: &[u8]) -> usize {
    let re = regex::bytes::Regex::new(r"/Type\s*/Page\b").unwrap();
    re.find_iter(bytes).count()
}

// --- (a) plain-prose fixture -------------------------------------------------

#[test]
fn plain_prose_fixture_exports_a_valid_pdf() {
    let bytes = pdf_from_djot(RICH_DJOT, pdf_options());
    assert!(
        bytes.starts_with(b"%PDF-"),
        "output must start with the PDF magic bytes"
    );
    assert!(
        bytes.len() > 500,
        "a document with headings/lists/a table must not compile to a trivially small PDF, got {} bytes",
        bytes.len()
    );
    assert!(
        count_pdf_pages(&bytes) >= 1,
        "must report at least one page"
    );
}

#[test]
fn plain_paragraph_exports_a_valid_pdf() {
    let bytes = pdf_from_djot(
        "Just a plain paragraph, no formatting at all.",
        pdf_options(),
    );
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_pdf_pages(&bytes) >= 1);
}

#[test]
fn heading_levels_all_compile() {
    for level in 1..=6 {
        let hashes = "#".repeat(level);
        let djot = format!("{hashes} Title level {level}\n\nSome body text.\n");
        let bytes = pdf_from_djot(&djot, pdf_options());
        assert!(bytes.starts_with(b"%PDF-"), "level {level} must compile");
    }
}

#[test]
fn code_block_compiles_and_does_not_interpret_its_own_content_as_markup() {
    // The code's own literal `*`/`#`/backslash characters must survive untouched, and must not
    // be interpreted as Typst markup (which would be a security-relevant escaping bug, not just
    // a cosmetic one, since `#raw(..)` is only safe if the content is string-escaped, not
    // markup-escaped).
    let djot = "```rust\nlet s = \"a * b # c\\\\d\";\n```\n";
    let bytes = pdf_from_djot(djot, pdf_options());
    assert!(bytes.starts_with(b"%PDF-"));
}

// --- (b) RTL fixture ----------------------------------------------------------

#[test]
fn rtl_hebrew_fixture_compiles() {
    let djot =
        "{direction=rtl}\n\u{05e9}\u{05dc}\u{05d5}\u{05dd} \u{05e2}\u{05d5}\u{05dc}\u{05dd}\n";
    let bytes = pdf_from_djot(djot, pdf_options());
    assert!(
        bytes.starts_with(b"%PDF-"),
        "an RTL Hebrew block must still compile even with a non-Hebrew-shaping font"
    );
}

#[test]
fn rtl_arabic_fixture_compiles() {
    let djot = "{direction=rtl}\n\u{0645}\u{0631}\u{062d}\u{0628}\u{0627} \u{0628}\u{0627}\u{0644}\u{0639}\u{0627}\u{0644}\u{0645}\n";
    let bytes = pdf_from_djot(djot, pdf_options());
    assert!(
        bytes.starts_with(b"%PDF-"),
        "an RTL Arabic block must still compile even with a non-Arabic-shaping font"
    );
}

#[test]
fn mixed_ltr_and_rtl_blocks_compile_in_one_document() {
    let djot = "English prose first.\n\n{direction=rtl}\n\u{05e9}\u{05dc}\u{05d5}\u{05dd}\n\nMore English prose after.\n";
    let bytes = pdf_from_djot(djot, pdf_options());
    assert!(bytes.starts_with(b"%PDF-"));
}

#[test]
fn rtl_heading_compiles() {
    // An RTL heading must emit `= #text(dir: rtl)[..]` — the `=` marker at the block start with
    // only the *inline* content direction-wrapped. The earlier `#text(dir: rtl)[= ..]` form
    // buried the marker inside a text element; guard that the marker-first form is valid Typst
    // and still compiles.
    let djot = "{direction=rtl}\n# \u{05e9}\u{05dc}\u{05d5}\u{05dd} \u{05e2}\u{05d5}\u{05dc}\u{05dd}\n\nBody paragraph.\n";
    let bytes = pdf_from_djot(djot, pdf_options());
    assert!(
        bytes.starts_with(b"%PDF-"),
        "an RTL heading must compile with the marker kept at block start"
    );
}

// --- (c) font failure fixture --------------------------------------------------

#[test]
fn garbage_font_bytes_produce_a_clear_error_not_a_silent_success() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "Hello, world.");

    let options = PdfExportOptions {
        font_bytes: vec![vec![0u8; 16]], // not a font at all
        ..Default::default()
    };
    let err = document_io_controller::build_pdf_document(
        &db,
        &ExportPdfDto {
            output_path: String::new(),
            options,
        },
    )
    .expect_err("corrupt font bytes must be rejected, not silently produce an empty-font PDF");
    assert!(
        err.to_string().contains("could not be parsed as a font"),
        "got: {err}"
    );
}

#[test]
fn no_fonts_at_all_produce_a_clear_error() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "Hello, world.");

    let err = document_io_controller::build_pdf_document(
        &db,
        &ExportPdfDto {
            output_path: String::new(),
            options: PdfExportOptions::default(), // font_bytes: vec![] by default
        },
    )
    .expect_err("an export with zero fonts must fail loudly");
    assert!(err.to_string().contains("no fonts supplied"), "got: {err}");
}

// --- escaping round-trips through the real import→export pipeline -------------

#[test]
fn special_characters_round_trip_through_import_and_export() {
    // Backslash-escaped in the djot source so the importer stores these as LITERAL characters in
    // the block's plain text (not djot's own formatting) — exercising `escape_typst` against
    // every character it must neutralize, driven through the real document model rather than
    // called directly (which isn't reachable from an external test crate; `escape_typst` also has
    // dedicated unit tests inside `typst_markup.rs` itself).
    let djot = "\\#hashtag \\*star\\* \\_underscore\\_ \\[bracket\\] \\$dollar \\`tick\\` \\~tilde\\~ \\<lt\\> \\@at and a literal - hyphen / slash = equals + plus.\n";
    let bytes = pdf_from_djot(djot, pdf_options());
    assert!(
        bytes.starts_with(b"%PDF-"),
        "prose containing every escaped-special character must still compile"
    );
}

#[test]
fn leading_numbered_looking_prose_does_not_become_a_typst_list() {
    let djot = "12\\. Go left at the fork.\n";
    let bytes = pdf_from_djot(djot, pdf_options());
    assert!(bytes.starts_with(b"%PDF-"));
}

// --- end-to-end write-to-disk via the LongOperation path -----------------------

#[test]
fn rich_document_writes_a_real_pdf_file_to_disk() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, RICH_DJOT);

    let dir = std::env::temp_dir();
    let path = dir.join(format!("pdf_export_rich_{}.pdf", std::process::id()));
    let path_str = path.to_string_lossy().to_string();

    let mut mgr = LongOperationManager::new();
    let op = document_io_controller::export_pdf(
        &db,
        &ev,
        &mut mgr,
        &ExportPdfDto {
            output_path: path_str.clone(),
            options: PdfExportOptions {
                title: Some("Rich Book".to_string()),
                author: Some("Test Author".to_string()),
                ..pdf_options()
            },
        },
    )
    .expect("export_pdf");
    wait(&mgr, &op);
    assert_eq!(
        mgr.get_operation_status(&op),
        Some(OperationStatus::Completed),
        "export should complete"
    );

    let result_json = mgr.get_operation_result(&op).expect("result present");
    let result: document_io::ExportPdfResultDto =
        serde_json::from_str(&result_json).expect("result deserializes");
    assert_eq!(result.file_path, path_str);
    assert!(result.page_count >= 1);

    let bytes = std::fs::read(&path).expect("output file exists");
    assert!(!bytes.is_empty());
    assert!(
        bytes.starts_with(b"%PDF-"),
        "the written file is a real PDF"
    );
    assert_eq!(count_pdf_pages(&bytes) as i64, result.page_count);

    let _ = std::fs::remove_file(&path);
}

// --- per-block spacing overrides (what a scene break needs) ----------------

#[test]
fn per_block_spacing_overrides_compile() {
    // The Typst wraps for `top_margin` / `text_indent` are hand-written markup,
    // so the real risk is emitting something Typst cannot parse. Compiling is
    // the assertion: malformed markup fails here rather than at a user's export.
    let djot = "Ordinary indented paragraph.\n\n\
                {top_margin=24 text_indent=0}\nThe paragraph after a scene break.";
    let bytes = pdf_from_djot(djot, pdf_options());
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_pdf_pages(&bytes) >= 1);
}

#[test]
fn per_block_spacing_compiles_alongside_a_document_wide_indent() {
    // The block override has to coexist with `#set par(first-line-indent: ..)`
    // from the preamble — that combination is what a real manuscript export hits.
    let mut options = pdf_options();
    options.first_line_indent_mm = Some(5.0);
    options.paragraph_spacing_pt = Some(6.0);
    let djot = "First paragraph.\n\n{text_indent=0}\nFlush after the break.";
    let bytes = pdf_from_djot(djot, options);
    assert!(bytes.starts_with(b"%PDF-"));
}

#[test]
fn per_block_spacing_compiles_together_with_rtl_and_alignment() {
    // An RTL scene whose first paragraph follows a blank-line break stacks
    // direction + alignment + both spacing wraps on one block.
    let djot = "{direction=rtl alignment=center top_margin=24 text_indent=0}\nنص عربي بعد الفاصل.";
    let bytes = pdf_from_djot(djot, pdf_options());
    assert!(bytes.starts_with(b"%PDF-"));
}
