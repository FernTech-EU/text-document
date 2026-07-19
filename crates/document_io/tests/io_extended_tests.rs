extern crate text_document_io as document_io;
use anyhow::Result;
use common::long_operation::{LongOperationManager, OperationStatus};

use test_harness::{setup, setup_with_text};

use document_io::document_io_controller;
use document_io::*;

fn wait_for_long_operation(long_op_manager: &LongOperationManager, op_id: &str) {
    while let Some(OperationStatus::Running) = long_op_manager.get_operation_status(op_id) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

// ─── Import Markdown Tests ─────────────────────────────────────────

#[test]
fn test_import_markdown_simple() -> Result<()> {
    let (db_context, event_hub, _) = setup()?;
    let mut long_op_manager = LongOperationManager::new();

    let op_id = document_io_controller::import_markdown(
        &db_context,
        &event_hub,
        &mut long_op_manager,
        &ImportMarkdownDto {
            markdown_text: "Hello **world**".to_string(),
        },
    )?;

    wait_for_long_operation(&long_op_manager, &op_id);

    // Verify the operation completed successfully
    let status = long_op_manager.get_operation_status(&op_id);
    assert_eq!(status, Some(OperationStatus::Completed));

    // Verify plain text content
    let exported = document_io_controller::export_plain_text(&db_context, &event_hub)?;
    assert_eq!(exported.plain_text, "Hello world");

    Ok(())
}

#[test]
fn test_import_markdown_with_headings() -> Result<()> {
    let (db_context, event_hub, _) = setup()?;
    let mut long_op_manager = LongOperationManager::new();

    let op_id = document_io_controller::import_markdown(
        &db_context,
        &event_hub,
        &mut long_op_manager,
        &ImportMarkdownDto {
            markdown_text: "# Title\n\nContent".to_string(),
        },
    )?;

    wait_for_long_operation(&long_op_manager, &op_id);

    let status = long_op_manager.get_operation_status(&op_id);
    assert_eq!(status, Some(OperationStatus::Completed));

    let result = document_io_controller::get_import_markdown_result(&long_op_manager, &op_id)?;
    assert!(result.is_some());
    let result_dto = result.unwrap();
    assert_eq!(result_dto.block_count, 2); // heading + content

    Ok(())
}

// ─── Import HTML Tests ─────────────────────────────────────────────

#[test]
fn test_import_html_simple() -> Result<()> {
    let (db_context, event_hub, _) = setup()?;
    let mut long_op_manager = LongOperationManager::new();

    let op_id = document_io_controller::import_html(
        &db_context,
        &event_hub,
        &mut long_op_manager,
        &ImportHtmlDto {
            html_text: "<p>Hello <b>world</b></p>".to_string(),
        },
    )?;

    wait_for_long_operation(&long_op_manager, &op_id);

    let status = long_op_manager.get_operation_status(&op_id);
    assert_eq!(status, Some(OperationStatus::Completed));

    let exported = document_io_controller::export_plain_text(&db_context, &event_hub)?;
    assert_eq!(exported.plain_text, "Hello world");

    Ok(())
}

#[test]
fn test_import_html_with_list() -> Result<()> {
    let (db_context, event_hub, _) = setup()?;
    let mut long_op_manager = LongOperationManager::new();

    let op_id = document_io_controller::import_html(
        &db_context,
        &event_hub,
        &mut long_op_manager,
        &ImportHtmlDto {
            html_text: "<ul><li>a</li><li>b</li></ul>".to_string(),
        },
    )?;

    wait_for_long_operation(&long_op_manager, &op_id);

    let status = long_op_manager.get_operation_status(&op_id);
    assert_eq!(status, Some(OperationStatus::Completed));

    let result = document_io_controller::get_import_html_result(&long_op_manager, &op_id)?;
    assert!(result.is_some());
    let result_dto = result.unwrap();
    assert!(result_dto.block_count >= 2);

    Ok(())
}

// ─── Export Markdown Tests ──────────────────────────────────────────

#[test]
fn test_export_markdown_simple() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello World")?;

    let result = document_io_controller::export_markdown(&db_context, &event_hub)?;
    assert!(result.markdown_text.contains("Hello World"));

    Ok(())
}

#[test]
fn test_export_markdown_roundtrip() -> Result<()> {
    let (db_context, event_hub, _) = setup()?;
    let mut long_op_manager = LongOperationManager::new();

    // Import markdown
    let op_id = document_io_controller::import_markdown(
        &db_context,
        &event_hub,
        &mut long_op_manager,
        &ImportMarkdownDto {
            markdown_text: "# Title\n\nSome **bold** text".to_string(),
        },
    )?;

    wait_for_long_operation(&long_op_manager, &op_id);

    let status = long_op_manager.get_operation_status(&op_id);
    assert_eq!(status, Some(OperationStatus::Completed));

    // Export markdown
    let result = document_io_controller::export_markdown(&db_context, &event_hub)?;
    assert!(result.markdown_text.contains("# Title"));
    assert!(result.markdown_text.contains("**bold**"));

    Ok(())
}

// ─── Export HTML Tests ──────────────────────────────────────────────

#[test]
fn test_export_html_simple() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello World")?;

    let result = document_io_controller::export_html(&db_context, &event_hub)?;
    assert!(result.html_text.contains("<p>"));
    assert!(result.html_text.contains("Hello World"));
    assert!(result.html_text.contains("<html>"));
    assert!(result.html_text.contains("</html>"));

    Ok(())
}

#[test]
fn test_export_html_bold() -> Result<()> {
    let (db_context, event_hub, _) = setup()?;
    let mut long_op_manager = LongOperationManager::new();

    let op_id = document_io_controller::import_markdown(
        &db_context,
        &event_hub,
        &mut long_op_manager,
        &ImportMarkdownDto {
            markdown_text: "Hello **bold** text".to_string(),
        },
    )?;

    wait_for_long_operation(&long_op_manager, &op_id);

    let status = long_op_manager.get_operation_status(&op_id);
    assert_eq!(status, Some(OperationStatus::Completed));

    let result = document_io_controller::export_html(&db_context, &event_hub)?;
    assert!(result.html_text.contains("<strong>bold</strong>"));

    Ok(())
}

// ─── Export LaTeX Tests ─────────────────────────────────────────────

#[test]
fn test_export_latex_simple() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello World")?;

    let result = document_io_controller::export_latex(
        &db_context,
        &event_hub,
        &ExportLatexDto {
            document_class: String::new(),
            include_preamble: false,
        },
    )?;
    assert!(result.latex_text.contains("Hello World"));

    Ok(())
}

#[test]
fn test_export_latex_with_preamble() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Content")?;

    let result = document_io_controller::export_latex(
        &db_context,
        &event_hub,
        &ExportLatexDto {
            document_class: "article".to_string(),
            include_preamble: true,
        },
    )?;
    assert!(result.latex_text.contains("\\documentclass{article}"));
    assert!(result.latex_text.contains("\\begin{document}"));
    assert!(result.latex_text.contains("\\end{document}"));
    assert!(result.latex_text.contains("Content"));

    Ok(())
}

#[test]
fn test_export_latex_without_preamble() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Content")?;

    let result = document_io_controller::export_latex(
        &db_context,
        &event_hub,
        &ExportLatexDto {
            document_class: String::new(),
            include_preamble: false,
        },
    )?;
    assert!(!result.latex_text.contains("\\documentclass"));
    assert!(!result.latex_text.contains("\\begin{document}"));
    assert!(result.latex_text.contains("Content"));

    Ok(())
}

// ─── Export DOCX Tests ──────────────────────────────────────────────

#[test]
fn test_export_docx_simple() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello World")?;
    let mut long_op_manager = LongOperationManager::new();

    let temp_dir = std::env::temp_dir();
    let output_path = temp_dir
        .join("test_export_docx_simple.docx")
        .to_string_lossy()
        .to_string();

    let op_id = document_io_controller::export_docx(
        &db_context,
        &event_hub,
        &mut long_op_manager,
        &ExportDocxDto {
            output_path: output_path.clone(),
            options: Default::default(),
        },
    )?;

    wait_for_long_operation(&long_op_manager, &op_id);

    let status = long_op_manager.get_operation_status(&op_id);
    assert_eq!(status, Some(OperationStatus::Completed));

    // Verify file exists
    assert!(std::path::Path::new(&output_path).exists());

    // Verify result DTO
    let result = document_io_controller::get_export_docx_result(&long_op_manager, &op_id)?;
    assert!(result.is_some());
    let result_dto = result.unwrap();
    assert_eq!(result_dto.file_path, output_path);
    assert!(result_dto.paragraph_count >= 1);

    // Clean up
    let _ = std::fs::remove_file(&output_path);

    Ok(())
}

// ─── Additional edge case tests ───────────────────────────────────

#[test]
fn test_import_markdown_empty() -> Result<()> {
    let (db_context, event_hub, _) = setup()?;
    let mut long_op_manager = LongOperationManager::new();

    let op_id = document_io_controller::import_markdown(
        &db_context,
        &event_hub,
        &mut long_op_manager,
        &ImportMarkdownDto {
            markdown_text: "".to_string(),
        },
    )?;

    wait_for_long_operation(&long_op_manager, &op_id);

    let status = long_op_manager.get_operation_status(&op_id);
    assert_eq!(status, Some(OperationStatus::Completed));

    let exported = document_io_controller::export_plain_text(&db_context, &event_hub)?;
    assert!(exported.plain_text.is_empty() || exported.plain_text.trim().is_empty());

    Ok(())
}

#[test]
fn test_export_latex_escapes_special_chars() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Price: $100 & 50% off #1")?;

    let result = document_io_controller::export_latex(
        &db_context,
        &event_hub,
        &ExportLatexDto {
            document_class: String::new(),
            include_preamble: false,
        },
    )?;

    assert!(result.latex_text.contains("\\$"));
    assert!(result.latex_text.contains("\\&"));
    assert!(result.latex_text.contains("\\%"));
    assert!(result.latex_text.contains("\\#"));

    Ok(())
}

#[test]
fn test_export_html_escapes_special_chars() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("1 < 2 & 3 > 0")?;

    let result = document_io_controller::export_html(&db_context, &event_hub)?;

    assert!(result.html_text.contains("&lt;"));
    assert!(result.html_text.contains("&amp;"));
    assert!(result.html_text.contains("&gt;"));

    Ok(())
}

#[test]
fn test_export_markdown_multiline() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Line one\nLine two\nLine three")?;

    let result = document_io_controller::export_markdown(&db_context, &event_hub)?;

    assert!(result.markdown_text.contains("Line one"));
    assert!(result.markdown_text.contains("Line two"));
    assert!(result.markdown_text.contains("Line three"));

    Ok(())
}

#[test]
fn test_export_html_multiline() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Para one\nPara two")?;

    let result = document_io_controller::export_html(&db_context, &event_hub)?;

    // Each block should be wrapped in <p> tags
    let count = result.html_text.matches("<p>").count();
    assert!(count >= 2, "Expected at least 2 <p> tags, got {}", count);

    Ok(())
}

#[test]
fn test_import_html_empty() -> Result<()> {
    let (db_context, event_hub, _) = setup()?;
    let mut long_op_manager = LongOperationManager::new();

    let op_id = document_io_controller::import_html(
        &db_context,
        &event_hub,
        &mut long_op_manager,
        &ImportHtmlDto {
            html_text: "".to_string(),
        },
    )?;

    wait_for_long_operation(&long_op_manager, &op_id);

    let status = long_op_manager.get_operation_status(&op_id);
    assert_eq!(status, Some(OperationStatus::Completed));

    Ok(())
}

#[test]
fn test_export_latex_heading() -> Result<()> {
    let (db_context, event_hub, _) = setup()?;
    let mut long_op_manager = LongOperationManager::new();

    let op_id = document_io_controller::import_markdown(
        &db_context,
        &event_hub,
        &mut long_op_manager,
        &ImportMarkdownDto {
            markdown_text: "# Main Title\n\n## Subtitle\n\nBody text".to_string(),
        },
    )?;

    wait_for_long_operation(&long_op_manager, &op_id);

    let result = document_io_controller::export_latex(
        &db_context,
        &event_hub,
        &ExportLatexDto {
            document_class: "article".to_string(),
            include_preamble: true,
        },
    )?;

    assert!(result.latex_text.contains("\\section{Main Title}"));
    assert!(result.latex_text.contains("\\subsection{Subtitle}"));
    assert!(result.latex_text.contains("Body text"));

    Ok(())
}

// ─── Per-block spacing overrides (what a scene break needs) ─────────

/// Import `djot` into a fresh document and return the exported HTML.
fn html_from_djot(djot: &str) -> Result<String> {
    let (db_context, event_hub, _) = setup()?;
    let mut mgr = LongOperationManager::new();
    let op_id = document_io_controller::import_djot(
        &db_context,
        &event_hub,
        &mut mgr,
        &ImportDjotDto {
            djot_text: djot.to_string(),
            options: Default::default(),
        },
    )?;
    wait_for_long_operation(&mgr, &op_id);
    Ok(document_io_controller::export_html(&db_context, &event_hub)?.html_text)
}

#[test]
fn test_export_html_emits_per_block_spacing() -> Result<()> {
    // The model's unit for these is the logical (CSS) pixel, so they map across
    // unchanged. EPUB shares this renderer, so it is covered by the same code.
    let html = html_from_djot("{top_margin=24 text_indent=0}\nAfter the break.")?;
    assert!(
        html.contains("margin-top: 24px"),
        "expected margin-top in:\n{html}"
    );
    assert!(
        html.contains("text-indent: 0px"),
        "expected text-indent in:\n{html}"
    );
    Ok(())
}

#[test]
fn test_export_html_omits_spacing_when_absent() -> Result<()> {
    // A paragraph with no override must not gain the properties at all, or it
    // would defeat any stylesheet the ebook applies.
    let html = html_from_djot("An ordinary paragraph.")?;
    assert!(!html.contains("margin-top"), "unexpected margin-top:\n{html}");
    assert!(!html.contains("text-indent"), "unexpected text-indent:\n{html}");
    Ok(())
}
