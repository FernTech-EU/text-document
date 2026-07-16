//! Feature tests for the EPUB 3 exporter.
//!
//! Documents are built with the (well-tested) djot importer, then exported via the file-less
//! builder [`document_io_controller::build_epub_document`], and the resulting bytes are read
//! back as a zip archive (the `zip` crate — the same crate `epub-builder` itself packages
//! with). This exercises the exact builder used to write `.epub` files without touching the
//! filesystem, mirroring `docx_export_tests.rs`'s approach.

extern crate text_document_io as document_io;

use common::long_operation::{LongOperationManager, OperationStatus};
use common::parser_tools::EpubExportOptions;
use document_io::{ExportEpubDto, document_io_controller};
use std::io::{Cursor, Read};
use test_harness::{EventHub, setup};

use std::sync::Arc;

/// Three level-1 chapter headings (one has a level-2 subsection) — no content precedes the
/// first heading, so there is no front-matter chapter and the chapter count is exactly the
/// number of level-1 headings.
const HEADINGS_DJOT: &str = "\
# Chapter One

Some opening prose for chapter one.

## A subsection

More text under the subsection.

# Chapter Two

Prose for chapter two.

# Chapter Three

Final chapter prose.
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

/// Import `djot` into a fresh document and return the packaged EPUB bytes.
fn epub_from_djot(djot: &str, options: EpubExportOptions) -> Vec<u8> {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, djot);
    document_io_controller::build_epub_document(
        &db,
        &ExportEpubDto {
            output_path: String::new(),
            options,
        },
    )
    .expect("build_epub_document")
}

fn zip_entry_names(bytes: &[u8]) -> Vec<String> {
    let archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("packaged EPUB is a valid zip");
    archive.file_names().map(|s| s.to_string()).collect()
}

fn read_zip_entry(bytes: &[u8], name: &str) -> String {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).expect("packaged EPUB is a valid zip");
    let mut file = archive
        .by_name(name)
        .unwrap_or_else(|_| panic!("entry {name:?} present in the EPUB package"));
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .expect("entry is valid utf-8");
    contents
}

fn content_opf(bytes: &[u8]) -> String {
    read_zip_entry(bytes, "OEBPS/content.opf")
}

// --- package structure -------------------------------------------------------

#[test]
fn epub_export_is_a_valid_zip_with_required_entries() {
    let bytes = epub_from_djot(HEADINGS_DJOT, EpubExportOptions::default());
    assert!(!bytes.is_empty(), "packaged EPUB must be non-empty");

    let names = zip_entry_names(&bytes);
    assert!(
        names.iter().any(|n| n == "mimetype"),
        "EPUB must contain a mimetype entry, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "META-INF/container.xml"),
        "EPUB must contain META-INF/container.xml, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "OEBPS/content.opf"),
        "EPUB must contain OEBPS/content.opf, got {names:?}"
    );

    let mimetype = read_zip_entry(&bytes, "mimetype");
    assert_eq!(mimetype, "application/epub+zip");
}

#[test]
fn epub_splits_into_one_chapter_per_top_level_heading() {
    let bytes = epub_from_djot(HEADINGS_DJOT, EpubExportOptions::default());
    let names = zip_entry_names(&bytes);

    let chapters: Vec<&String> = names
        .iter()
        .filter(|n| n.starts_with("OEBPS/chapter_") && n.ends_with(".xhtml"))
        .collect();

    // Three level-1 headings ("Chapter One/Two/Three"); the level-2 subsection stays inline
    // inside "Chapter One" rather than starting a chapter of its own.
    assert_eq!(
        chapters.len(),
        3,
        "expected one chapter per top-level heading, got {names:?}"
    );

    let ch1 = read_zip_entry(&bytes, "OEBPS/chapter_001.xhtml");
    assert!(ch1.contains("Chapter One"));
    assert!(
        ch1.contains("A subsection"),
        "the level-2 subsection heading stays inside chapter one, got:\n{ch1}"
    );
    assert!(ch1.contains("Some opening prose"));

    let ch2 = read_zip_entry(&bytes, "OEBPS/chapter_002.xhtml");
    assert!(ch2.contains("Chapter Two"));
    assert!(ch2.contains("Prose for chapter two"));

    let ch3 = read_zip_entry(&bytes, "OEBPS/chapter_003.xhtml");
    assert!(ch3.contains("Chapter Three"));
    assert!(ch3.contains("Final chapter prose"));
}

#[test]
fn epub_chapter_xhtml_is_well_formed() {
    let bytes = epub_from_djot(HEADINGS_DJOT, EpubExportOptions::default());
    let ch1 = read_zip_entry(&bytes, "OEBPS/chapter_001.xhtml");
    assert!(ch1.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
    assert!(ch1.contains("<!DOCTYPE html>"));
    assert!(ch1.contains("xmlns=\"http://www.w3.org/1999/xhtml\""));
    assert!(ch1.contains("<title>Chapter One</title>"));
    assert!(ch1.contains("<h1>Chapter One</h1>"));
}

#[test]
fn epub_with_no_headings_is_a_single_chapter() {
    let bytes = epub_from_djot("Just a plain paragraph, no headings at all.", EpubExportOptions {
        title: "My Book".to_string(),
        ..Default::default()
    });
    let names = zip_entry_names(&bytes);
    let chapters: Vec<&String> = names
        .iter()
        .filter(|n| n.starts_with("OEBPS/chapter_") && n.ends_with(".xhtml"))
        .collect();
    assert_eq!(chapters.len(), 1, "a headingless document is one chapter");

    let ch1 = read_zip_entry(&bytes, "OEBPS/chapter_001.xhtml");
    // Untitled front matter takes the book title.
    assert!(ch1.contains("<title>My Book</title>"));
    assert!(ch1.contains("Just a plain paragraph"));
}

#[test]
fn epub_front_matter_chapter_precedes_the_first_heading() {
    let djot = "\
A short introduction before any chapter heading.

# Chapter One

Chapter one prose.
";
    let bytes = epub_from_djot(
        djot,
        EpubExportOptions {
            title: "The Book".to_string(),
            ..Default::default()
        },
    );
    let names = zip_entry_names(&bytes);
    let chapters: Vec<&String> = names
        .iter()
        .filter(|n| n.starts_with("OEBPS/chapter_") && n.ends_with(".xhtml"))
        .collect();
    assert_eq!(
        chapters.len(),
        2,
        "front matter + one chapter heading, got {names:?}"
    );

    let front = read_zip_entry(&bytes, "OEBPS/chapter_001.xhtml");
    assert!(front.contains("A short introduction"));
    assert!(
        front.contains("<title>The Book</title>"),
        "front matter takes the book title"
    );

    let ch1 = read_zip_entry(&bytes, "OEBPS/chapter_002.xhtml");
    assert!(ch1.contains("Chapter One"));
    assert!(ch1.contains("Chapter one prose"));
}

// --- metadata ----------------------------------------------------------------

#[test]
fn epub_metadata_title_author_lang_land_in_the_opf() {
    let bytes = epub_from_djot(
        HEADINGS_DJOT,
        EpubExportOptions {
            title: "The Lighthouse".to_string(),
            author: "Ann Vane".to_string(),
            language: "fr".to_string(),
            rtl: false,
        },
    );
    let opf = content_opf(&bytes);
    assert!(
        opf.contains("<dc:title>The Lighthouse</dc:title>"),
        "title in OPF, got:\n{opf}"
    );
    assert!(
        opf.contains("Ann Vane"),
        "author in OPF, got:\n{opf}"
    );
    assert!(opf.contains(">fr<"), "language in OPF, got:\n{opf}");

    // The generator meta tag lives in the nav document, not content.opf.
    let nav = read_zip_entry(&bytes, "OEBPS/nav.xhtml");
    assert!(
        nav.contains("Skribisto"),
        "generator in nav.xhtml, got:\n{nav}"
    );
}

#[test]
fn epub_default_language_falls_back_to_en() {
    let bytes = epub_from_djot(HEADINGS_DJOT, EpubExportOptions::default());
    let opf = content_opf(&bytes);
    assert!(
        opf.contains(">en<"),
        "blank language option defaults to en, got:\n{opf}"
    );
}

#[test]
fn epub_rtl_option_sets_page_progression_direction() {
    let bytes = epub_from_djot(
        HEADINGS_DJOT,
        EpubExportOptions {
            rtl: true,
            ..Default::default()
        },
    );
    let opf = content_opf(&bytes);
    assert!(
        opf.contains("page-progression-direction=\"rtl\""),
        "RTL option must set page-progression-direction=rtl in the OPF spine, got:\n{opf}"
    );

    let ch1 = read_zip_entry(&bytes, "OEBPS/chapter_001.xhtml");
    assert!(
        ch1.contains("dir=\"rtl\""),
        "RTL option must also mark each chapter's <html> as dir=rtl, got:\n{ch1}"
    );
}

#[test]
fn epub_ltr_is_the_default_direction() {
    let bytes = epub_from_djot(HEADINGS_DJOT, EpubExportOptions::default());
    let opf = content_opf(&bytes);
    assert!(
        !opf.contains("page-progression-direction=\"rtl\""),
        "default direction is LTR, got:\n{opf}"
    );
    let ch1 = read_zip_entry(&bytes, "OEBPS/chapter_001.xhtml");
    assert!(!ch1.contains("dir=\"rtl\""));
}

// --- end-to-end pack/write to disk -------------------------------------------

#[test]
fn rich_document_packs_to_a_valid_epub_file_on_disk() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, HEADINGS_DJOT);

    let dir = std::env::temp_dir();
    let path = dir.join(format!("epub_export_rich_{}.epub", std::process::id()));
    let path_str = path.to_string_lossy().to_string();

    let mut mgr = LongOperationManager::new();
    let op = document_io_controller::export_epub(
        &db,
        &ev,
        &mut mgr,
        &ExportEpubDto {
            output_path: path_str.clone(),
            options: EpubExportOptions {
                title: "Rich Book".to_string(),
                author: "Test Author".to_string(),
                language: "en".to_string(),
                rtl: false,
            },
        },
    )
    .expect("export_epub");
    wait(&mgr, &op);
    assert_eq!(
        mgr.get_operation_status(&op),
        Some(OperationStatus::Completed),
        "export should complete"
    );

    let result_json = mgr.get_operation_result(&op).expect("result present");
    let result: document_io::ExportEpubResultDto =
        serde_json::from_str(&result_json).expect("result deserializes");
    assert_eq!(result.file_path, path_str);
    assert_eq!(result.chapter_count, 3);

    let bytes = std::fs::read(&path).expect("output file exists");
    assert!(!bytes.is_empty());
    let archive = zip::ZipArchive::new(Cursor::new(&bytes[..])).expect("packed epub must be a valid zip");
    assert!(archive.file_names().any(|n| n == "mimetype"));

    let _ = std::fs::remove_file(&path);
}
