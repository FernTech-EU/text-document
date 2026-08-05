//! Image bytes reaching each export backend.
//!
//! Every one of these backends previously produced output that *looked* right
//! and was not: EPUB emitted `<img src>` for resources it never packaged, DOCX
//! wrote the literal text `[Image: name]`, PDF fell back to the filename as
//! prose, and HTML emitted no `alt` at all. These tests assert on the produced
//! artifact — unzipping the container where there is one — rather than on the
//! markup that references it, because the markup was never the part that was
//! wrong.

use std::io::Read;

use text_document::{
    EpubExportOptions, ExportImage, ExportImages, HtmlExportOptions, HtmlImageMode, ResourceType,
    TextDocument,
};

/// A real 4×3 PNG, encoded by the `png` crate so the bytes are a decodable
/// image rather than a placeholder the exporters would reject.
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

/// A document reading "before <image> after", with the image registered as a
/// document resource so the editor could paint it too.
fn doc_with_image() -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_sync("before ![a blue square](pic.png){width=64 height=48} after\n")
        .expect("import");
    doc.add_resource(ResourceType::Image, "pic.png", "image/png", &png_bytes())
        .expect("resource");
    doc
}

fn images() -> ExportImages {
    ExportImages::from_iter([("pic.png", ExportImage::new(png_bytes(), "image/png"))])
}

/// Read one entry out of a zip container, by suffix match on its name.
fn zip_entry(archive: &[u8], suffix: &str) -> Option<(String, Vec<u8>)> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive.to_vec())).ok()?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).ok()?;
        let name = f.name().to_string();
        if name.ends_with(suffix) {
            let mut bytes = Vec::new();
            f.read_to_end(&mut bytes).ok()?;
            return Some((name, bytes));
        }
    }
    None
}

fn zip_names(archive: &[u8]) -> Vec<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive.to_vec())).expect("zip");
    (0..zip.len())
        .map(|i| zip.by_index(i).expect("entry").name().to_string())
        .collect()
}

// ── HTML ────────────────────────────────────────────────────────────────

#[test]
fn html_references_the_src_by_default() {
    let html = doc_with_image().to_html().expect("html");
    assert!(html.contains("src=\"pic.png\""), "{html}");
    assert!(
        html.contains("alt=\"a blue square\""),
        "alt missing: {html}"
    );
    assert!(html.contains("width=\"64\""), "size missing: {html}");
}

#[test]
fn html_can_inline_the_bytes_as_a_data_uri() {
    let html = doc_with_image()
        .to_html_with_options(HtmlExportOptions {
            image_mode: HtmlImageMode::DataUri,
            images: images(),
        })
        .expect("html");
    assert!(
        html.contains("src=\"data:image/png;base64,"),
        "expected an inlined image: {html}"
    );
    // The base64 payload must be the real PNG, not a truncated placeholder.
    assert!(html.contains("iVBORw0KGgo"), "not a PNG payload: {html}");
}

#[test]
fn html_can_omit_images_and_keeps_their_description() {
    let html = doc_with_image()
        .to_html_with_options(HtmlExportOptions {
            image_mode: HtmlImageMode::Omit,
            images: ExportImages::new(),
        })
        .expect("html");
    assert!(!html.contains("<img"), "image should be gone: {html}");
    assert!(
        html.contains("a blue square"),
        "the description is the only fallback left: {html}"
    );
    assert!(html.contains("before") && html.contains("after"), "{html}");
}

#[test]
fn a_data_uri_export_with_no_bytes_falls_back_to_the_description() {
    // Asking to inline an image the caller never supplied must not emit a
    // `data:` URI with nothing in it.
    let html = doc_with_image()
        .to_html_with_options(HtmlExportOptions {
            image_mode: HtmlImageMode::DataUri,
            images: ExportImages::new(),
        })
        .expect("html");
    assert!(!html.contains("data:"), "{html}");
    assert!(html.contains("a blue square"), "{html}");
}

// ── LaTeX ───────────────────────────────────────────────────────────────

#[test]
fn latex_emits_includegraphics_with_a_size_and_declares_graphicx() {
    let tex = doc_with_image().to_latex("article", true).expect("latex");
    assert!(
        tex.contains("\\includegraphics["),
        "no sized include: {tex}"
    );
    assert!(tex.contains("pic.png"), "{tex}");
    // 64 logical px at 96 dpi = 48 big points.
    assert!(tex.contains("width=48bp"), "wrong unit or size: {tex}");
    assert!(
        tex.contains("\\usepackage{graphicx}"),
        "graphicx must be declared or the document will not compile: {tex}"
    );
}

#[test]
fn latex_can_omit_images() {
    // `\includegraphics` is resolved by the LaTeX compiler against the
    // filesystem, so an export whose caller will not place the files beside the
    // `.tex` has to leave the command out or the build fails outright.
    let tex = doc_with_image()
        .to_latex_with_options("article", true, true)
        .expect("latex");
    assert!(!tex.contains("\\includegraphics"), "{tex}");
    assert!(tex.contains("before") && tex.contains("after"), "{tex}");
}

// ── Markdown and Djot ───────────────────────────────────────────────────

#[test]
fn markdown_and_djot_reference_their_images_by_default() {
    let doc = doc_with_image();
    let md = doc.to_markdown().expect("markdown");
    assert!(md.contains("![a blue square](pic.png)"), "{md}");
    let dj = doc.to_djot().expect("djot");
    assert!(dj.contains("![a blue square](pic.png)"), "{dj}");
    // Djot has attribute syntax, so unlike Markdown it keeps the display size.
    assert!(dj.contains("width=64"), "{dj}");
}

#[test]
fn markdown_can_omit_images_without_losing_the_surrounding_prose() {
    let md = doc_with_image()
        .to_markdown_with(text_document::MarkdownExportOptions {
            omit_images: true,
            ..Default::default()
        })
        .expect("markdown");
    assert!(!md.contains("pic.png"), "{md}");
    // Not the alt text either: Markdown cannot mark a description as standing in
    // for a picture, so it would read as a sentence the author never wrote.
    assert!(!md.contains("a blue square"), "{md}");
    assert!(md.contains("before") && md.contains("after"), "{md}");
}

#[test]
fn djot_can_omit_images_without_losing_the_surrounding_prose() {
    let dj = doc_with_image()
        .to_djot_with_options(text_document::DjotExportOptions {
            omit_images: true,
            ..Default::default()
        })
        .expect("djot");
    assert!(!dj.contains("pic.png"), "{dj}");
    assert!(dj.contains("before") && dj.contains("after"), "{dj}");
}

#[test]
fn dropping_every_optional_attribute_still_keeps_the_images() {
    // `DjotExportOptions::none()` means "no optional block attributes". An image
    // is content, not styling, and must survive that setting — the field sits in
    // the same struct, so this is the mistake worth pinning.
    let dj = doc_with_image()
        .to_djot_with_options(text_document::DjotExportOptions::none())
        .expect("djot");
    assert!(
        dj.contains("pic.png"),
        "images are not a block attribute: {dj}"
    );
}

// ── DOCX ────────────────────────────────────────────────────────────────

#[test]
fn docx_embeds_a_real_media_part() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("out.docx");
    let doc = doc_with_image();
    doc.to_docx_with_options(
        path.to_str().unwrap(),
        text_document::DocxExportOptions {
            images: images(),
            ..Default::default()
        },
    )
    .expect("start")
    .wait()
    .expect("docx");

    let bytes = std::fs::read(&path).expect("read docx");
    let names = zip_names(&bytes);
    assert!(
        names.iter().any(|n| n.starts_with("word/media/")),
        "no media part was written: {names:?}"
    );

    let (_, doc_xml) = zip_entry(&bytes, "word/document.xml").expect("document.xml");
    let xml = String::from_utf8_lossy(&doc_xml);
    assert!(
        xml.contains("<w:drawing>"),
        "the image must be a drawing, not text"
    );
    assert!(
        !xml.contains("[Image:"),
        "the old bracketed-filename placeholder is still being emitted"
    );
}

#[test]
fn docx_without_bytes_degrades_to_the_description() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("out.docx");
    doc_with_image()
        .to_docx(path.to_str().unwrap())
        .expect("start")
        .wait()
        .expect("docx");

    let bytes = std::fs::read(&path).expect("read docx");
    let (_, doc_xml) = zip_entry(&bytes, "word/document.xml").expect("document.xml");
    let xml = String::from_utf8_lossy(&doc_xml);
    assert!(xml.contains("a blue square"), "description lost: {xml}");
    assert!(!xml.contains("[Image:"), "{xml}");
}

// ── EPUB ────────────────────────────────────────────────────────────────

#[test]
fn epub_packages_the_image_and_points_at_it() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("out.epub");
    doc_with_image()
        .to_epub_with_options(
            path.to_str().unwrap(),
            EpubExportOptions {
                title: "Test".into(),
                language: "en".into(),
                images: images(),
                ..Default::default()
            },
        )
        .expect("start")
        .wait()
        .expect("epub");

    let bytes = std::fs::read(&path).expect("read epub");
    let names = zip_names(&bytes);
    let packaged = names
        .iter()
        .find(|n| n.contains("images/img_001.png"))
        .unwrap_or_else(|| panic!("image was not packaged: {names:?}"));

    // The bytes in the package must be the image we supplied, not a stub.
    let (_, stored) = zip_entry(&bytes, "images/img_001.png").expect("stored image");
    assert_eq!(stored, png_bytes(), "packaged bytes differ from the source");

    // And a chapter must actually reference the packaged href — an <img src>
    // pointing at a file that was never written is what this used to produce.
    let (_, chapter) = zip_entry(&bytes, ".xhtml").expect("a chapter");
    let xhtml = String::from_utf8_lossy(&chapter);
    assert!(
        xhtml.contains("images/img_001.png"),
        "chapter does not reference the packaged image ({packaged}): {xhtml}"
    );
}

#[test]
fn epub_without_bytes_degrades_to_the_description() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("out.epub");
    doc_with_image()
        .to_epub(path.to_str().unwrap())
        .expect("start")
        .wait()
        .expect("epub");

    let bytes = std::fs::read(&path).expect("read epub");
    let names = zip_names(&bytes);
    assert!(
        !names.iter().any(|n| n.contains("images/")),
        "nothing should be packaged when no bytes were supplied: {names:?}"
    );
}

// ── EPUB cover ──────────────────────────────────────────────────────────

#[test]
fn epub_marks_a_cover_in_the_manifest_and_gives_it_a_page() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("out.epub");
    doc_with_image()
        .to_epub_with_options(
            path.to_str().unwrap(),
            EpubExportOptions {
                title: "Test".into(),
                language: "en".into(),
                cover: Some(ExportImage::new(png_bytes(), "image/png")),
                ..Default::default()
            },
        )
        .expect("start")
        .wait()
        .expect("epub");

    let bytes = std::fs::read(&path).expect("read epub");
    let names = zip_names(&bytes);

    // The bytes have to actually be in the package…
    let (_, stored) = zip_entry(&bytes, "cover.png")
        .unwrap_or_else(|| panic!("cover missing from the package: {names:?}"));
    assert_eq!(
        stored,
        png_bytes(),
        "packaged cover differs from the source"
    );

    // …the manifest has to say it is the cover, or a reader shows a blank
    // rectangle on its shelf…
    let (_, opf) = zip_entry(&bytes, ".opf").expect("content.opf");
    let opf = String::from_utf8_lossy(&opf);
    assert!(
        opf.contains("cover-image"),
        "cover is not marked in the manifest: {opf}"
    );

    // …and there has to be a page, because `add_cover_image` alone generates
    // none and a book read straight through would open on chapter one.
    let (_, page) = zip_entry(&bytes, "cover.xhtml").expect("cover page");
    let page = String::from_utf8_lossy(&page);
    assert!(
        page.contains("cover.png"),
        "cover page shows nothing: {page}"
    );
    assert!(page.contains("alt="), "cover image has no alt text: {page}");
}

#[test]
fn an_epub_without_a_cover_gains_no_cover_files() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("out.epub");
    doc_with_image()
        .to_epub(path.to_str().unwrap())
        .expect("start")
        .wait()
        .expect("epub");
    let names = zip_names(&std::fs::read(&path).expect("read epub"));
    assert!(
        !names.iter().any(|n| n.contains("cover")),
        "a book with no cover should carry no cover entries: {names:?}"
    );
}
