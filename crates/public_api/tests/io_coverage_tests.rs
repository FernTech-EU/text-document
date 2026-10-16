//! Tests that exercise import/export paths with richer content.

use text_document::{
    BlockFormat, ListStyle, SelectionType, TextDirection, TextDocument, TextFormat,
};

fn new_doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

// ── HTML export with formatting ─────────────────────────────────

#[test]
fn html_export_with_bold_text() {
    let doc = new_doc("Hello world");
    let c = doc.cursor();
    c.set_position(5, text_document::MoveMode::KeepAnchor);
    let fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    c.set_char_format(&fmt).unwrap();
    let html = doc.to_html().unwrap();
    assert!(html.contains("Hello"));
    assert!(html.contains("world"));
}

#[test]
fn html_export_with_italic_and_underline() {
    let doc = new_doc("Styled text here");
    let c = doc.cursor();
    c.set_position(6, text_document::MoveMode::KeepAnchor);
    let fmt = TextFormat {
        font_italic: Some(true),
        font_underline: Some(true),
        ..Default::default()
    };
    c.set_char_format(&fmt).unwrap();
    let html = doc.to_html().unwrap();
    assert!(html.contains("Styled"));
}

#[test]
fn html_export_multiblock() {
    let doc = new_doc("First paragraph\nSecond paragraph\nThird paragraph");
    let html = doc.to_html().unwrap();
    assert!(html.contains("First paragraph"));
    assert!(html.contains("Second paragraph"));
    assert!(html.contains("Third paragraph"));
}

#[test]
fn html_export_with_heading() {
    let doc = TextDocument::new();
    let op = doc
        .set_markdown("# Heading 1\n\nSome text\n\n## Heading 2\n\nMore text")
        .unwrap();
    op.wait().unwrap();
    let html = doc.to_html().unwrap();
    assert!(html.contains("Heading 1"));
    assert!(html.contains("Heading 2"));
}

#[test]
fn html_export_with_list() {
    let doc = new_doc("Item A\nItem B\nItem C");
    let c = doc.cursor();
    c.select(SelectionType::Document);
    c.create_list(ListStyle::Disc).unwrap();
    let html = doc.to_html().unwrap();
    assert!(html.contains("Item A"));
}

// ── HTML export with new block format fields ────────────────────

#[test]
fn html_export_block_format_line_height() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.set_block_format(&BlockFormat {
        line_height: Some(1.5),
        ..Default::default()
    })
    .unwrap();
    let html = doc.to_html().unwrap();
    assert!(html.contains("line-height: 1.5"), "HTML: {}", html);
}

#[test]
fn html_export_block_format_direction_rtl() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.set_block_format(&BlockFormat {
        direction: Some(TextDirection::RightToLeft),
        ..Default::default()
    })
    .unwrap();
    let html = doc.to_html().unwrap();
    assert!(html.contains("direction: rtl"), "HTML: {}", html);
}

#[test]
fn html_export_block_format_background_color() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.set_block_format(&BlockFormat {
        background_color: Some("#ff0000".to_string()),
        ..Default::default()
    })
    .unwrap();
    let html = doc.to_html().unwrap();
    assert!(html.contains("background-color: #ff0000"), "HTML: {}", html);
}

#[test]
fn html_export_block_format_non_breakable() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    c.set_block_format(&BlockFormat {
        non_breakable_lines: Some(true),
        ..Default::default()
    })
    .unwrap();
    let html = doc.to_html().unwrap();
    assert!(html.contains("white-space: pre"), "HTML: {}", html);
}

#[test]
fn html_import_block_format_styles() {
    let doc = TextDocument::new();
    let op = doc
        .set_html(
            r#"<html><body><p style="line-height: 2.0; direction: rtl; background-color: blue">Styled</p></body></html>"#,
        )
        .unwrap();
    op.wait().unwrap();
    let fmt = doc.block_format_at(0).unwrap();
    assert_eq!(fmt.line_height, Some(2.0));
    assert_eq!(fmt.direction, Some(TextDirection::RightToLeft));
    assert_eq!(fmt.background_color, Some("blue".to_string()));
}

// ── LaTeX export with formatting ────────────────────────────────

#[test]
fn latex_export_multiblock() {
    let doc = new_doc("First\nSecond\nThird");
    let latex = doc.to_latex("article", false).unwrap();
    assert!(latex.contains("First"));
    assert!(latex.contains("Second"));
    assert!(latex.contains("Third"));
}

#[test]
fn latex_export_with_bold() {
    let doc = new_doc("Bold text");
    let c = doc.cursor();
    c.set_position(4, text_document::MoveMode::KeepAnchor);
    let fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    c.set_char_format(&fmt).unwrap();
    let latex = doc.to_latex("article", false).unwrap();
    assert!(latex.contains("Bold"));
}

#[test]
fn latex_export_with_heading() {
    let doc = TextDocument::new();
    let op = doc
        .set_markdown("# Main Title\n\nContent here\n\n## Subsection\n\nMore content")
        .unwrap();
    op.wait().unwrap();
    let latex = doc.to_latex("article", true).unwrap();
    assert!(latex.contains("Main Title"));
    assert!(latex.contains("Content here"));
    assert!(latex.contains("\\documentclass"));
}

#[test]
fn latex_export_with_list() {
    let doc = new_doc("Item 1\nItem 2");
    let c = doc.cursor();
    c.select(SelectionType::Document);
    c.create_list(ListStyle::Decimal).unwrap();
    let latex = doc.to_latex("article", false).unwrap();
    assert!(latex.contains("Item 1"));
}

#[test]
fn latex_export_document_class_variants() {
    let doc = new_doc("Content");
    for class in &["article", "report", "book", "letter"] {
        let latex = doc.to_latex(class, true).unwrap();
        assert!(latex.contains(&format!("\\documentclass{{{class}}}")));
    }
}

// ── Markdown export with formatting ─────────────────────────────

#[test]
fn markdown_export_multiblock() {
    let doc = new_doc("Line A\nLine B\nLine C");
    let md = doc.to_markdown().unwrap();
    assert!(md.contains("Line A"));
    assert!(md.contains("Line B"));
    assert!(md.contains("Line C"));
}

#[test]
fn markdown_export_with_heading() {
    let doc = TextDocument::new();
    let op = doc.set_markdown("# Title\n\nBody text").unwrap();
    op.wait().unwrap();
    let md = doc.to_markdown().unwrap();
    assert!(md.contains("Title"));
    assert!(md.contains("Body text"));
}

// ── HTML import with rich content ───────────────────────────────

#[test]
fn html_import_with_formatting() {
    let doc = TextDocument::new();
    let op = doc
        .set_html("<p><b>Bold</b> and <i>italic</i> text</p>")
        .unwrap();
    op.wait().unwrap();
    let text = doc.to_plain_text().unwrap();
    assert!(text.contains("Bold"));
    assert!(text.contains("italic"));
}

#[test]
fn html_import_with_list() {
    let doc = TextDocument::new();
    let op = doc
        .set_html("<ul><li>Item 1</li><li>Item 2</li></ul>")
        .unwrap();
    op.wait().unwrap();
    let text = doc.to_plain_text().unwrap();
    assert!(text.contains("Item 1"));
    assert!(text.contains("Item 2"));
}

#[test]
fn html_import_with_headings() {
    let doc = TextDocument::new();
    let op = doc
        .set_html("<h1>Title</h1><p>Paragraph</p><h2>Subtitle</h2>")
        .unwrap();
    op.wait().unwrap();
    let text = doc.to_plain_text().unwrap();
    assert!(text.contains("Title"));
    assert!(text.contains("Paragraph"));
}

// ── Markdown import with rich content ───────────────────────────

#[test]
fn markdown_import_with_formatting() {
    let doc = TextDocument::new();
    let op = doc.set_markdown("**Bold** and *italic* text").unwrap();
    op.wait().unwrap();
    let text = doc.to_plain_text().unwrap();
    assert!(text.contains("Bold"));
    assert!(text.contains("italic"));
}

#[test]
fn markdown_import_with_list() {
    let doc = TextDocument::new();
    let op = doc.set_markdown("- Item A\n- Item B\n- Item C").unwrap();
    op.wait().unwrap();
    let text = doc.to_plain_text().unwrap();
    assert!(text.contains("Item A"));
    assert!(text.contains("Item B"));
}

#[test]
fn markdown_import_with_headings_and_paragraphs() {
    let doc = TextDocument::new();
    let op = doc
        .set_markdown("# H1\n\nPara 1\n\n## H2\n\nPara 2\n\n### H3\n\nPara 3")
        .unwrap();
    op.wait().unwrap();
    let text = doc.to_plain_text().unwrap();
    assert!(text.contains("H1"));
    assert!(text.contains("Para 1"));
    assert!(text.contains("H2"));
    assert!(text.contains("Para 3"));
}

// ── Cross-format roundtrips ─────────────────────────────────────

#[test]
fn markdown_to_html_roundtrip() {
    let doc = TextDocument::new();
    let op = doc.set_markdown("# Title\n\nHello **world**").unwrap();
    op.wait().unwrap();
    let html = doc.to_html().unwrap();
    assert!(html.contains("Title"));
    assert!(html.contains("world"));
}

#[test]
fn html_to_markdown_roundtrip() {
    let doc = TextDocument::new();
    let op = doc.set_html("<h1>Title</h1><p>Content</p>").unwrap();
    op.wait().unwrap();
    let md = doc.to_markdown().unwrap();
    assert!(md.contains("Title"));
    assert!(md.contains("Content"));
}

#[test]
fn markdown_to_latex_roundtrip() {
    let doc = TextDocument::new();
    let op = doc.set_markdown("# Title\n\nParagraph text").unwrap();
    op.wait().unwrap();
    let latex = doc.to_latex("article", true).unwrap();
    assert!(latex.contains("Title"));
    assert!(latex.contains("Paragraph text"));
}

// ── DOCX export with content ────────────────────────────────────

#[cfg(feature = "docx")]
#[test]
fn docx_export_multiblock() {
    let doc = new_doc("Para 1\nPara 2\nPara 3");
    let tmp = std::env::temp_dir().join("test_docx_multiblock.docx");
    let op = doc.to_docx(tmp.to_str().unwrap()).unwrap();
    let result = op.wait().unwrap();
    assert!(result.paragraph_count >= 3);
    let _ = std::fs::remove_file(&tmp);
}

#[cfg(feature = "docx")]
#[test]
fn docx_export_with_formatting() {
    let doc = new_doc("Bold text");
    let c = doc.cursor();
    c.set_position(4, text_document::MoveMode::KeepAnchor);
    let fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    c.set_char_format(&fmt).unwrap();
    let tmp = std::env::temp_dir().join("test_docx_fmt.docx");
    let op = doc.to_docx(tmp.to_str().unwrap()).unwrap();
    let result = op.wait().unwrap();
    assert!(result.paragraph_count >= 1);
    let _ = std::fs::remove_file(&tmp);
}
