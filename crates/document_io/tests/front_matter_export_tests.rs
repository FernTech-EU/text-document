//! Front matter (`Block::fmt_front_matter`) in the DOCX and ODT writers: the leading run of
//! marked blocks gets pages with no running header and no page number, and the body after it
//! starts a fresh page sequence numbered from 1.
//!
//! The XML-shape tests pin what each writer emits. The rendering tests go further, through an
//! independent reader: LibreOffice converts each file to PDF and `pdftotext` reads it back page
//! by page, so a blank page or a header on the title page shows up as what a reader would see.
//! They are skipped, with a note, where `soffice` or `pdftotext` is not installed.

extern crate text_document_io as document_io;

use common::long_operation::{LongOperationManager, OperationStatus};
use common::parser_tools::{
    DocxExportOptions, DocxHeadingStyle, ExportImage, ExportImages, OdtExportOptions,
    OdtHeadingStyle,
};
use document_io::{ExportDocxDto, ExportOdtDto, ImportDjotDto, document_io_controller};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use test_harness::{EventHub, setup};

/// The shape Skribisto's compiler writes: a cover, a title page opening on a page of its own,
/// then two chapters that each start a page.
const BOOK: &str = "\
{alignment=center front_matter=true}
![The Lighthouse](cover.png)

{alignment=right page_break_before=true front_matter=true}
about 120,000 words

{alignment=center top_margin=300 front_matter=true}
# The Lighthouse

{alignment=center front_matter=true}
by Ada Vane

{page_break_before=true}
# Chapter One

The wind rose over the hills.

{page_break_before=true}
# Chapter Two

The keeper climbed the stairs.";

/// The same book with no front matter marked at all.
const BOOK_WITHOUT_FRONT_MATTER: &str = "\
# Chapter One

The wind rose over the hills.

{page_break_before=true}
# Chapter Two

The keeper climbed the stairs.";

const RUNNING_HEADER: &str = "Vane / LIGHTHOUSE";

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

/// A real 4×3 PNG, so the cover is an embedded picture rather than its alt text.
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

fn cover() -> ExportImages {
    ExportImages::from_iter([("cover.png", ExportImage::new(png_bytes(), "image/png"))])
}

/// A4, 1" margins, a running header with the page number: a manuscript.
fn docx_options() -> DocxExportOptions {
    DocxExportOptions {
        page_width_twips: Some(11906),
        page_height_twips: Some(16838),
        margin_top_twips: Some(1440),
        margin_bottom_twips: Some(1440),
        margin_left_twips: Some(1440),
        margin_right_twips: Some(1440),
        page_numbers: true,
        running_header: Some(RUNNING_HEADER.to_string()),
        images: cover(),
        ..Default::default()
    }
}

fn odt_options() -> OdtExportOptions {
    OdtExportOptions {
        page_width_twips: Some(11906),
        page_height_twips: Some(16838),
        margin_top_twips: Some(1440),
        margin_bottom_twips: Some(1440),
        margin_left_twips: Some(1440),
        margin_right_twips: Some(1440),
        page_numbers: true,
        running_header: Some(RUNNING_HEADER.to_string()),
        images: cover(),
        ..Default::default()
    }
}

fn docx_bytes(djot: &str, options: DocxExportOptions) -> Vec<u8> {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, djot);
    let docx = document_io_controller::build_docx_document(
        &db,
        &ExportDocxDto {
            output_path: String::new(),
            options,
        },
    )
    .expect("build_docx_document");
    let mut buf = std::io::Cursor::new(Vec::new());
    docx.build()
        .pack(&mut buf)
        .expect("docx-rs pack into memory");
    buf.into_inner()
}

fn odt_bytes(djot: &str, options: OdtExportOptions) -> Vec<u8> {
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

fn zip_text(bytes: &[u8], name: &str) -> String {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("the package is a valid zip");
    let mut file = archive
        .by_name(name)
        .unwrap_or_else(|_| panic!("entry {name:?} present in the package"));
    let mut text = String::new();
    file.read_to_string(&mut text).expect("utf-8 entry");
    text
}

/// The `<w:p>…</w:p>` element of `document_xml` whose text holds `needle`.
fn docx_paragraph<'a>(document_xml: &'a str, needle: &str) -> &'a str {
    let at = document_xml
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} in the document: {document_xml}"));
    let start = document_xml[..at]
        .rfind("<w:p>")
        .or_else(|| document_xml[..at].rfind("<w:p "));
    let start = start.expect("an opening paragraph tag");
    let end = at
        + document_xml[at..]
            .find("</w:p>")
            .expect("a closing paragraph tag");
    &document_xml[start..end]
}

/// The document's own, final `w:sectPr`: the last one in the body, outside any paragraph.
fn docx_body_section(document_xml: &str) -> &str {
    let start = document_xml.rfind("<w:sectPr>").expect("a body sectPr");
    let end = start + document_xml[start..].find("</w:sectPr>").expect("closed");
    &document_xml[start..end]
}

/// The automatic style `content` declares under `name`.
fn odt_style<'a>(content: &'a str, name: &str) -> &'a str {
    let start = content
        .find(&format!("<style:style style:name=\"{name}\""))
        .unwrap_or_else(|| panic!("style {name} declared: {content}"));
    let end = start
        + content[start..]
            .find("</style:style>")
            .expect("a closed style element");
    &content[start..end]
}

/// The style name on the opening tag of the element that holds `needle`.
fn odt_style_of<'a>(content: &'a str, needle: &str) -> &'a str {
    let at = content
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} in the content: {content}"));
    let open = content[..at]
        .rfind("<text:")
        .expect("an opening text element");
    let tag = &content[open..at];
    let start = tag.find("text:style-name=\"").expect("a style name") + 17;
    let len = tag[start..].find('"').expect("closing quote");
    &tag[start..start + len]
}

// --- DOCX: XML shape ---------------------------------------------------------------

#[test]
fn docx_front_matter_ends_in_a_section_of_its_own_without_a_header() {
    let xml = zip_text(&docx_bytes(BOOK, docx_options()), "word/document.xml");

    let byline = docx_paragraph(&xml, "by Ada Vane");
    assert!(
        byline.contains("<w:sectPr>"),
        "the last front-matter paragraph closes the front matter's section: {byline}"
    );
    assert!(
        !byline.contains("headerReference"),
        "the front matter's section carries no header: {byline}"
    );
    assert!(
        !byline.contains("<w:type "),
        "a section with no type starts on the next page, the default: {byline}"
    );
    assert!(
        byline.contains("w:w=\"11906\"") && byline.contains("w:top=\"1440\""),
        "the front matter keeps the page's size and margins: {byline}"
    );
    assert_eq!(
        xml.matches("<w:sectPr>").count(),
        2,
        "exactly one section break, plus the body's own section: {xml}"
    );

    let body = docx_body_section(&xml);
    assert!(
        body.contains("headerReference"),
        "the body keeps the running header: {body}"
    );
    assert!(
        body.contains("<w:pgNumType w:start=\"1\""),
        "the body is numbered from 1: {body}"
    );
    assert!(!body.contains("<w:type "), "{body}");
}

#[test]
fn docx_body_drops_the_page_break_the_section_break_already_makes() {
    let xml = zip_text(&docx_bytes(BOOK, docx_options()), "word/document.xml");
    assert!(
        !docx_paragraph(&xml, "Chapter One").contains("pageBreakBefore"),
        "the first body paragraph must not break a second time: {xml}"
    );
    assert!(
        docx_paragraph(&xml, "Chapter Two").contains("pageBreakBefore"),
        "a later chapter keeps its own break"
    );
    assert!(
        docx_paragraph(&xml, "about 120,000 words").contains("pageBreakBefore"),
        "a page break inside the front matter is the front matter's own"
    );
}

#[test]
fn docx_without_front_matter_is_one_section_as_before() {
    let xml = zip_text(
        &docx_bytes(BOOK_WITHOUT_FRONT_MATTER, docx_options()),
        "word/document.xml",
    );
    assert_eq!(xml.matches("<w:sectPr>").count(), 1, "{xml}");
    let body = docx_body_section(&xml);
    assert!(body.contains("headerReference"));
    assert!(!body.contains("pgNumType"), "no numbering restart: {body}");
}

#[test]
fn docx_a_document_that_is_all_front_matter_has_no_header() {
    let bytes = docx_bytes(
        "{front_matter=true}\n# The Lighthouse\n\n{front_matter=true}\nby Ada Vane",
        docx_options(),
    );
    let xml = zip_text(&bytes, "word/document.xml");
    assert_eq!(
        xml.matches("<w:sectPr>").count(),
        1,
        "no section break: {xml}"
    );
    assert!(
        !xml.contains("headerReference"),
        "no page carries the running header: {xml}"
    );
}

#[test]
fn docx_a_marked_block_after_the_body_starts_is_body() {
    // Only the leading run is front matter: a marked block further down changes nothing.
    let xml = zip_text(
        &docx_bytes(
            "# Chapter One\n\n{front_matter=true}\nNot a title page.",
            docx_options(),
        ),
        "word/document.xml",
    );
    assert_eq!(xml.matches("<w:sectPr>").count(), 1, "{xml}");
}

// --- ODT: XML shape ----------------------------------------------------------------

#[test]
fn odt_front_matter_opens_on_a_master_page_without_a_header() {
    let bytes = odt_bytes(BOOK, odt_options());
    let content = zip_text(&bytes, "content.xml");
    let styles = zip_text(&bytes, "styles.xml");

    let cover_style = odt_style_of(&content, "<draw:frame");
    assert!(
        odt_style(&content, cover_style).contains("style:master-page-name=\"Front_Matter\""),
        "the first front-matter paragraph enters the front matter's master page: {content}"
    );
    assert_eq!(
        content.matches("style:master-page-name=").count(),
        2,
        "exactly two switches, into the front matter and out of it: {content}"
    );

    let front = styles
        .find("<style:master-page style:name=\"Front_Matter\"")
        .map(|at| &styles[at..at + styles[at..].find("/>").expect("self-closing")])
        .unwrap_or_else(|| panic!("the front matter's master page is declared: {styles}"));
    assert!(
        front.contains("style:page-layout-name=\"PM1\""),
        "the same page layout as the body: {front}"
    );
    assert!(
        styles.contains("<style:master-page style:name=\"Standard\" style:page-layout-name=\"PM1\"><style:header>"),
        "the body's master page keeps its header: {styles}"
    );
}

#[test]
fn odt_body_returns_to_the_standard_master_page_numbered_from_one() {
    let content = zip_text(&odt_bytes(BOOK, odt_options()), "content.xml");
    let chapter = odt_style(&content, odt_style_of(&content, "Chapter One"));
    assert!(
        chapter.contains("style:master-page-name=\"Standard\""),
        "{chapter}"
    );
    assert!(chapter.contains("style:page-number=\"1\""), "{chapter}");
    assert!(
        !chapter.contains("fo:break-before=\"page\""),
        "the master page already starts the page: {chapter}"
    );

    let second = odt_style(&content, odt_style_of(&content, "Chapter Two"));
    assert!(
        second.contains("fo:break-before=\"page\"") && !second.contains("master-page-name"),
        "a later chapter keeps its own break and its page sequence: {second}"
    );
}

#[test]
fn odt_without_front_matter_names_no_master_page() {
    let bytes = odt_bytes(BOOK_WITHOUT_FRONT_MATTER, odt_options());
    assert!(!zip_text(&bytes, "content.xml").contains("master-page-name"));
    assert!(!zip_text(&bytes, "styles.xml").contains("Front_Matter"));
}

#[test]
fn odt_a_marked_block_after_the_body_starts_is_body() {
    // The body switch lands on the first unmarked paragraph, and a marked block later in the
    // flow opens nothing.
    let content = zip_text(
        &odt_bytes(
            "{front_matter=true}\nCover.\n\nBody.\n\n{front_matter=true}\nLater.",
            odt_options(),
        ),
        "content.xml",
    );
    assert_eq!(
        content.matches("style:master-page-name=").count(),
        2,
        "{content}"
    );
    assert!(
        odt_style(&content, odt_style_of(&content, ">Body.<"))
            .contains("style:master-page-name=\"Standard\""),
        "{content}"
    );
}

/// A table opening the body carries the switch itself, on its table style, and nothing inside
/// its cells opens a page sequence of its own. The Djot reader has no table syntax, so the
/// table is inserted through the document API.
#[test]
fn odt_a_table_opening_the_body_switches_and_its_cells_do_not() {
    let doc = text_document::TextDocument::new();
    doc.set_djot("{front_matter=true}\nCover.")
        .expect("set_djot")
        .wait()
        .expect("import completes");
    doc.cursor_at(doc.character_count())
        .insert_table(1, 1)
        .expect("insert_table");

    let path = scratch_dir("odt_table").join("table.odt");
    doc.to_odt_with_options(&path.to_string_lossy(), odt_options())
        .expect("to_odt_with_options")
        .wait()
        .expect("odt export completes");
    let bytes = std::fs::read(&path).expect("read exported odt");
    let _ = std::fs::remove_dir_all(path.parent().expect("scratch dir"));
    let content = zip_text(&bytes, "content.xml");

    let table_at = content.find("<table:table ").expect("a table");
    let tag = &content[table_at..table_at + content[table_at..].find('>').expect("tag end")];
    let start = tag.find("table:style-name=\"").expect("a table style") + 18;
    let name = &tag[start..start + tag[start..].find('"').expect("quote")];
    let style = odt_style(&content, name);
    assert!(
        style.contains("style:master-page-name=\"Standard\"")
            && style.contains("style:page-number=\"1\""),
        "the table opens the body: {style}"
    );
    assert_eq!(
        content.matches("style:master-page-name=").count(),
        2,
        "the cover and the table, nothing in the cell: {content}"
    );
}

// --- Rendering through LibreOffice -------------------------------------------------

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Convert `file` to PDF with LibreOffice and return the text of each page, as `pdftotext
/// -layout` lays it out. `None` when either tool is missing.
fn rendered_pages(file: &Path) -> Option<Vec<String>> {
    let (Some(soffice), Some(pdftotext)) = (which("soffice"), which("pdftotext")) else {
        eprintln!("soffice or pdftotext not found on PATH; skipping the rendering check");
        return None;
    };
    let dir = file.parent().expect("a parent directory");
    let output = std::process::Command::new(soffice)
        .arg("--headless")
        .arg("--norestore")
        .arg(format!(
            "-env:UserInstallation=file://{}",
            dir.join("lo_profile").display()
        ))
        .arg("--convert-to")
        .arg("pdf")
        .arg("--outdir")
        .arg(dir)
        .arg(file)
        .output()
        .expect("run soffice");
    assert!(
        output.status.success(),
        "soffice failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pdf = file.with_extension("pdf");
    let text = std::process::Command::new(pdftotext)
        .arg("-layout")
        .arg(&pdf)
        .arg("-")
        .output()
        .expect("run pdftotext");
    assert!(text.status.success(), "pdftotext failed");
    let text = String::from_utf8_lossy(&text.stdout).into_owned();
    let mut pages: Vec<String> = text.split('\u{c}').map(str::to_string).collect();
    // pdftotext ends the last page with a form feed too.
    if pages.last().is_some_and(|p| p.trim().is_empty()) {
        pages.pop();
    }
    Some(pages)
}

/// A scratch directory of its own per test, so parallel conversions never share a profile.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "front_matter_{name}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Four pages: a cover and a title page with no header and no number, then two chapters
/// numbered 1 and 2 under the running header, and no blank page anywhere.
fn assert_book_pages(pages: &[String], format: &str) {
    assert_eq!(
        pages.len(),
        4,
        "{format}: cover, title page, two chapters, nothing blank: {pages:#?}"
    );
    let has_header = |page: &str| page.contains(RUNNING_HEADER);
    assert!(
        !has_header(&pages[0]) && pages[0].trim().is_empty(),
        "{format}: the cover holds its picture and nothing else: {:?}",
        pages[0]
    );
    assert!(
        pages[1].contains("by Ada Vane") && pages[1].contains("about 120,000 words"),
        "{format}: the title page: {:?}",
        pages[1]
    );
    assert!(
        !has_header(&pages[1]),
        "{format}: no running header on the title page: {:?}",
        pages[1]
    );
    for (page, (chapter, number)) in pages[2..]
        .iter()
        .zip([("Chapter One", "1"), ("Chapter Two", "2")])
    {
        assert!(page.contains(chapter), "{format}: {chapter} page: {page:?}");
        let header_line = page
            .lines()
            .find(|line| has_header(line))
            .unwrap_or_else(|| panic!("{format}: the running header on {chapter}: {page:?}"));
        assert_eq!(
            header_line.split_whitespace().last(),
            Some(number),
            "{format}: {chapter} is page {number} of the body: {header_line:?}"
        );
    }
}

#[test]
fn docx_renders_front_matter_without_furniture_and_numbers_the_body_from_one() {
    let dir = scratch_dir("docx");
    let file = dir.join("book.docx");
    std::fs::write(&file, docx_bytes(BOOK, docx_options())).expect("write docx");
    if let Some(pages) = rendered_pages(&file) {
        assert_book_pages(&pages, "DOCX");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn odt_renders_front_matter_without_furniture_and_numbers_the_body_from_one() {
    let dir = scratch_dir("odt");
    let file = dir.join("book.odt");
    std::fs::write(&file, odt_bytes(BOOK, odt_options())).expect("write odt");
    if let Some(pages) = rendered_pages(&file) {
        assert_book_pages(&pages, "ODT");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A heading level whose style opens a page, under the section or master-page switch that
/// already opens one: still no blank page.
#[test]
fn a_heading_style_that_opens_a_page_does_not_add_a_blank_one() {
    let docx_dir = scratch_dir("docx_style_break");
    let docx_file = docx_dir.join("book.docx");
    let docx = DocxExportOptions {
        heading_styles: vec![DocxHeadingStyle {
            page_break_before: true,
            ..Default::default()
        }],
        ..docx_options()
    };
    // The front matter holds no heading here, or the style would split the title page.
    let djot = BOOK.replace("# The Lighthouse", "The Lighthouse");
    std::fs::write(&docx_file, docx_bytes(&djot, docx)).expect("write docx");
    if let Some(pages) = rendered_pages(&docx_file) {
        assert_book_pages(&pages, "DOCX, heading style breaking");
    }
    let _ = std::fs::remove_dir_all(&docx_dir);

    let odt_dir = scratch_dir("odt_style_break");
    let odt_file = odt_dir.join("book.odt");
    let odt = OdtExportOptions {
        heading_styles: vec![OdtHeadingStyle {
            page_break_before: true,
            ..Default::default()
        }],
        ..odt_options()
    };
    std::fs::write(&odt_file, odt_bytes(&djot, odt)).expect("write odt");
    if let Some(pages) = rendered_pages(&odt_file) {
        assert_book_pages(&pages, "ODT, heading style breaking");
    }
    let _ = std::fs::remove_dir_all(&odt_dir);
}
