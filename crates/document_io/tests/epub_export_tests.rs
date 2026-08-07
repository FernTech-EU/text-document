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
    let bytes = epub_from_djot(
        "Just a plain paragraph, no headings at all.",
        EpubExportOptions {
            title: "My Book".to_string(),
            ..Default::default()
        },
    );
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

// --- footnotes -----------------------------------------------------------------
//
// These assert on the real, packaged `.xhtml` entries inside the zip — not on an
// in-memory HTML string, and not only on the referencing markup. That second part
// matters here specifically: `export_epub_uc` used to render a footnote's
// *reference* correctly (via the `html_render` module it shares with the plain-HTML
// backend) while never emitting the matching `<aside>` at all, because — unlike
// `export_html_uc`/`export_docx_uc` — it never skipped a note-body frame in its main
// walk and never appended a definition's rendering anywhere. A test asserting only
// on the `doc-noteref` marker would have kept passing throughout — the exact
// false-confidence shape the image work ran into, asserting on markup that referenced
// an artifact rather than on the artifact itself.

/// One note referenced twice — once from each of two chapters — plus one dangling
/// reference (no matching definition) in the second chapter.
const FOOTNOTE_DJOT: &str = "\
# Chapter One

Opening prose with a shared note[^shared] and one just for here[^only-one].

[^shared]: Shared note body.

[^only-one]: A note owned by chapter one alone.

# Chapter Two

More prose citing the shared note again[^shared], plus a dangling one[^gone].
";

#[test]
fn epub_footnote_reference_and_its_aside_share_one_chapter_file() {
    let bytes = epub_from_djot(FOOTNOTE_DJOT, EpubExportOptions::default());
    let ch1 = read_zip_entry(&bytes, "OEBPS/chapter_001.xhtml");

    // The reference: reading-system idiom (`epub:type`/`role` pair — `epub:type`
    // alone reaches no assistive technology), numbered 1 (first reference in
    // reading order).
    assert!(
        ch1.contains(r#"epub:type="noteref" role="doc-noteref""#),
        "no noteref marker in chapter one: {ch1}"
    );
    assert!(
        ch1.contains("id=\"fnref-shared\""),
        "reference anchor missing its id: {ch1}"
    );
    assert!(ch1.contains("<sup>1</sup>"), "wrong/missing marker: {ch1}");

    // The aside, in the SAME file — an EPUB fragment identifier does not resolve
    // across spine files, so a reference and its note have to share a document.
    assert!(
        ch1.contains(r#"epub:type="footnote" role="doc-footnote""#),
        "no footnote aside in chapter one: {ch1}"
    );
    assert!(
        ch1.contains("id=\"fn-shared\""),
        "aside is missing its id: {ch1}"
    );
    assert!(
        ch1.contains("Shared note body."),
        "the note's body never rendered: {ch1}"
    );
    // The back-link returns to the reference in the same document.
    assert!(
        ch1.contains(r##"href="#fnref-shared" role="doc-backlink""##),
        "aside has no back-link to its reference: {ch1}"
    );

    // A second, distinct note in the same chapter gets its own pair too.
    assert!(ch1.contains("<sup>2</sup>"), "second marker missing: {ch1}");
    assert!(
        ch1.contains("id=\"fn-only-one\"") && ch1.contains("A note owned by chapter one alone."),
        "second note's aside missing: {ch1}"
    );
}

#[test]
fn epub_note_referenced_from_two_chapters_gets_an_aside_in_each() {
    let bytes = epub_from_djot(FOOTNOTE_DJOT, EpubExportOptions::default());
    let ch1 = read_zip_entry(&bytes, "OEBPS/chapter_001.xhtml");
    let ch2 = read_zip_entry(&bytes, "OEBPS/chapter_002.xhtml");

    // Chapter two cites the SAME label ("shared") a second time — it must carry
    // its own copy of the aside rather than link back to chapter one's, which a
    // reading system may not resolve as a same-page popup across files.
    assert!(
        ch2.contains(r#"epub:type="noteref" role="doc-noteref""#),
        "no noteref marker in chapter two: {ch2}"
    );
    assert!(
        ch2.contains("id=\"fnref-shared\""),
        "chapter two's own reference anchor is missing: {ch2}"
    );
    // One note referenced twice keeps one number.
    assert!(
        ch2.contains("<sup>1</sup>"),
        "the shared note must keep the same number everywhere: {ch2}"
    );
    assert!(
        ch2.contains(r#"epub:type="footnote" role="doc-footnote""#)
            && ch2.contains("id=\"fn-shared\"")
            && ch2.contains("Shared note body."),
        "chapter two must carry its own copy of the shared note's aside: {ch2}"
    );

    // And chapter one's own copy is still there, independently.
    assert!(ch1.contains("id=\"fn-shared\"") && ch1.contains("Shared note body."));

    // The note not referenced from chapter two must not leak an aside into it.
    assert!(
        !ch2.contains("id=\"fn-only-one\""),
        "chapter two must not carry an aside for a note it never cites: {ch2}"
    );
}

const REPEAT_IN_ONE_CHAPTER_DJOT: &str =
    "First[^n1] and second[^n1] citation, in the SAME chapter.\n\n[^n1]: The shared body.\n";

/// One label cited TWICE from the SAME chapter — unlike `FOOTNOTE_DJOT`'s cross-chapter case
/// above (which legitimately duplicates the aside once per *file*, since an EPUB fragment
/// identifier can't reach across spine files), citing a label twice within one file must NOT
/// duplicate the aside: `chapter.footnote_labels` is a deduplicated set and the aside loop
/// walks it once per label, so both citations must point at the one copy. Proven against the
/// real, packaged `.xhtml`, not merely the referencing markup — same reasoning
/// `epub_footnote_reference_and_its_aside_share_one_chapter_file`'s own module doc gives.
#[test]
fn epub_repeat_citation_in_one_chapter_reuses_one_aside_not_two() {
    let bytes = epub_from_djot(REPEAT_IN_ONE_CHAPTER_DJOT, EpubExportOptions::default());
    let ch1 = read_zip_entry(&bytes, "OEBPS/chapter_001.xhtml");

    // TWO markers, both citing the label...
    assert_eq!(
        ch1.matches(r#"epub:type="noteref" role="doc-noteref""#)
            .count(),
        2,
        "both citations must render their own noteref marker: {ch1}"
    );
    assert_eq!(
        ch1.matches("id=\"fnref-n1\"").count(),
        2,
        "both citations must anchor as fnref-n1: {ch1}"
    );
    // ...both printing the SAME number: one note, one number, cited twice.
    assert_eq!(
        ch1.matches("<sup>1</sup>").count(),
        2,
        "both citations must carry the same marker number: {ch1}"
    );

    // ...but exactly ONE aside (one note body) for that label in the file.
    assert_eq!(
        ch1.matches(r#"epub:type="footnote" role="doc-footnote""#)
            .count(),
        1,
        "one label cited twice in one chapter must produce exactly one aside: {ch1}"
    );
    assert_eq!(
        ch1.matches("id=\"fn-n1\"").count(),
        1,
        "the aside must not be duplicated: {ch1}"
    );
    assert_eq!(
        ch1.matches("The shared body.").count(),
        1,
        "the note's body text must not be duplicated: {ch1}"
    );
}

#[test]
fn epub_dangling_footnote_reference_survives_with_no_aside_anywhere() {
    // "[^gone]" in FOOTNOTE_DJOT names no definition — the normal state for a host
    // that owns note bodies itself. The reference must still render (with SOME
    // marker — which number it gets is not the point of this test), and no aside
    // must appear anywhere in the package for a body that does not exist.
    let bytes = epub_from_djot(FOOTNOTE_DJOT, EpubExportOptions::default());
    let ch2 = read_zip_entry(&bytes, "OEBPS/chapter_002.xhtml");

    assert!(
        ch2.contains("id=\"fnref-gone\""),
        "the dangling reference must still render: {ch2}"
    );
    assert!(
        !ch2.contains("id=\"fn-gone\""),
        "no aside can exist for a note with no body: {ch2}"
    );

    let names = zip_entry_names(&bytes);
    for name in names.iter().filter(|n| n.ends_with(".xhtml")) {
        let xhtml = read_zip_entry(&bytes, name);
        assert!(
            !xhtml.contains("id=\"fn-gone\""),
            "a dangling reference produced an aside somewhere ({name}): {xhtml}"
        );
    }
}

#[test]
fn epub_note_body_is_not_rendered_inline_as_ordinary_prose() {
    // A definition is a detached top-level frame; without skipping it in the main
    // walk (`notes.is_definition`) it renders in the middle of whichever chapter it
    // was typed in, in addition to (not instead of) its aside.
    let bytes = epub_from_djot(FOOTNOTE_DJOT, EpubExportOptions::default());
    let ch1 = read_zip_entry(&bytes, "OEBPS/chapter_001.xhtml");

    // "Shared note body." appears exactly once in chapter one: inside its aside,
    // never also inline as a stray paragraph at the point the definition was typed.
    assert_eq!(
        ch1.matches("Shared note body.").count(),
        1,
        "the note body must appear exactly once (in its aside), not also inline: {ch1}"
    );
}

const NESTED_FOOTNOTE_DJOT: &str = "\
# Chapter One

Opening prose citing an outer note[^outer].

[^outer]: This note itself cites another[^inner].

[^inner]: Detail that only the outer note points at.
";

/// "[^inner]" is cited only from inside "[^outer]"'s own body — the ONE case
/// `footnotes.rs` documents as refused rather than numbered (see
/// `Footnotes::is_nested_reference`'s doc): it never gets a number, never
/// appears in `in_print_order`, and — since `export_epub_uc`'s per-chapter
/// aside pass shares `html_render::render_inline_html` with the plain-HTML
/// writer — must never be linked either. Checks the whole package, not just
/// one chapter file: nothing should ever attempt an aside for it anywhere.
#[test]
fn epub_nested_citation_does_not_dangle_anywhere_in_the_package() {
    let bytes = epub_from_djot(NESTED_FOOTNOTE_DJOT, EpubExportOptions::default());
    let ch1 = read_zip_entry(&bytes, "OEBPS/chapter_001.xhtml");

    // The outer note is fully resolved and renders exactly as usual.
    assert!(
        ch1.contains("id=\"fnref-outer\"") && ch1.contains("id=\"fn-outer\""),
        "the resolved outer note must render its normal reference+aside pair: {ch1}"
    );

    let names = zip_entry_names(&bytes);
    for name in names.iter().filter(|n| n.ends_with(".xhtml")) {
        let xhtml = read_zip_entry(&bytes, name);
        assert!(
            !xhtml.contains("href=\"#fn-inner\""),
            "a nested citation must not carry a dangling href ({name}): {xhtml}"
        );
        assert!(
            !xhtml.contains("id=\"fn-inner\""),
            "no aside may exist for a note nothing numbers ({name}): {xhtml}"
        );
        assert!(
            !xhtml.contains("Detail that only the outer note points at"),
            "a nested note's body must never be emitted anywhere ({name}): {xhtml}"
        );
    }
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
            images: Default::default(),
            cover: None,
        },
    );
    let opf = content_opf(&bytes);
    assert!(
        opf.contains("<dc:title>The Lighthouse</dc:title>"),
        "title in OPF, got:\n{opf}"
    );
    assert!(opf.contains("Ann Vane"), "author in OPF, got:\n{opf}");
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
                images: Default::default(),
                cover: None,
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
    let archive =
        zip::ZipArchive::new(Cursor::new(&bytes[..])).expect("packed epub must be a valid zip");
    assert!(archive.file_names().any(|n| n == "mimetype"));

    let _ = std::fs::remove_file(&path);
}
