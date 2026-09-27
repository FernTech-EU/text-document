use text_document::TextDocument;

// ── Markdown import (Operation) ──────────────────────────────────

#[test]
fn markdown_import_wait() {
    let doc = TextDocument::new();
    let op = doc.set_markdown("# Hello\n\nWorld").unwrap();
    let result = op.wait().unwrap();
    assert!(result.block_count >= 2);
    let text = doc.to_plain_text().unwrap();
    assert!(text.contains("Hello"));
    assert!(text.contains("World"));
}

#[test]
fn markdown_import_operation_id() {
    let doc = TextDocument::new();
    let op = doc.set_markdown("Some text").unwrap();
    assert!(!op.id().is_empty());
    op.wait().unwrap();
}

#[test]
fn markdown_import_is_done_after_wait() {
    let doc = TextDocument::new();
    let op = doc.set_markdown("Test").unwrap();
    // Before waiting, it may or may not be done
    let _id = op.id().to_string();
    let result = op.wait().unwrap();
    assert!(result.block_count >= 1);
}

#[test]
fn markdown_import_try_result() {
    let doc = TextDocument::new();
    let mut op = doc.set_markdown("Quick").unwrap();
    // Poll until done
    loop {
        if let Some(result) = op.try_result() {
            let r = result.unwrap();
            assert!(r.block_count >= 1);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn markdown_import_progress() {
    let doc = TextDocument::new();
    let op = doc
        .set_markdown("# Title\n\nParagraph 1\n\nParagraph 2")
        .unwrap();
    // Progress is Some(..) only if the op is still running; in either case
    // the reported percentage must be in [0.0, 100.0].
    if let Some((percent, _message)) = op.progress() {
        assert!(
            (0.0..=100.0).contains(&percent),
            "progress percentage out of range: {percent}"
        );
    }
    op.wait().unwrap();
}

// ── HTML import (Operation) ──────────────────────────────────────

#[test]
fn html_import_wait() {
    let doc = TextDocument::new();
    let op = doc.set_html("<p>Hello</p><p>World</p>").unwrap();
    let result = op.wait().unwrap();
    assert!(result.block_count >= 2);
    let text = doc.to_plain_text().unwrap();
    assert!(text.contains("Hello"));
    assert!(text.contains("World"));
}

#[test]
fn html_import_operation_id() {
    let doc = TextDocument::new();
    let op = doc.set_html("<p>Test</p>").unwrap();
    assert!(!op.id().is_empty());
    op.wait().unwrap();
}

#[test]
fn html_import_is_done() {
    let doc = TextDocument::new();
    let op = doc.set_html("<p>Check</p>").unwrap();
    op.wait().unwrap();
    // After wait, the operation is consumed, so we can verify the doc
    assert!(doc.character_count() > 0);
}

// ── DOCX export (Operation) ──────────────────────────────────────

#[test]
fn docx_export_wait() {
    let doc = TextDocument::new();
    doc.set_plain_text("DOCX content").unwrap();

    let tmp = std::env::temp_dir().join("test_export.docx");
    let op = doc.to_docx(tmp.to_str().unwrap()).unwrap();
    let result = op.wait().unwrap();
    assert!(result.paragraph_count >= 1);
    assert!(!result.file_path.is_empty());
    // Clean up
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn docx_export_operation_id() {
    let doc = TextDocument::new();
    doc.set_plain_text("Test").unwrap();

    let tmp = std::env::temp_dir().join("test_export_id.docx");
    let op = doc.to_docx(tmp.to_str().unwrap()).unwrap();
    assert!(!op.id().is_empty());
    op.wait().unwrap();
    let _ = std::fs::remove_file(&tmp);
}

// ── Operation cancel ─────────────────────────────────────────────

#[test]
fn cancel_operation() {
    let doc = TextDocument::new();
    let op = doc.set_markdown("# Some markdown\n\nWith content").unwrap();
    // Cancel is a no-op if already finished, shouldn't panic
    op.cancel();
    // The operation may or may not have completed before cancel
}

// ── Markdown export ──────────────────────────────────────────────

#[test]
fn markdown_roundtrip() {
    let doc = TextDocument::new();
    let op = doc.set_markdown("# Title\n\nParagraph text").unwrap();
    op.wait().unwrap();
    let md = doc.to_markdown().unwrap();
    assert!(md.contains("Title"));
    assert!(md.contains("Paragraph text"));
}

// ── HTML export ──────────────────────────────────────────────────

#[test]
fn html_roundtrip() {
    let doc = TextDocument::new();
    let op = doc.set_html("<h1>Title</h1><p>Body</p>").unwrap();
    op.wait().unwrap();
    let html = doc.to_html().unwrap();
    assert!(html.contains("Title"));
    assert!(html.contains("Body"));
}

// ── LaTeX export ─────────────────────────────────────────────────

#[test]
fn latex_export() {
    let doc = TextDocument::new();
    doc.set_plain_text("LaTeX content").unwrap();
    let latex = doc.to_latex("article", false).unwrap();
    assert!(latex.contains("LaTeX content"));
}

#[test]
fn latex_export_with_preamble() {
    let doc = TextDocument::new();
    doc.set_plain_text("Preamble test").unwrap();
    let latex = doc.to_latex("report", true).unwrap();
    assert!(latex.contains("\\documentclass"));
    assert!(latex.contains("Preamble test"));
}

// ── Reading while an import runs ─────────────────────────────────

/// An import writes into the store in place, so a read taken while it runs sees the
/// document partly replaced, or empty. `to_plain_text` used to cache that read, and the
/// import's completion invalidated nothing, so the half-imported (or empty) text went on
/// being returned after the import had finished.
#[test]
fn plain_text_read_while_an_import_runs_is_not_kept_after_it() {
    const PARAGRAPHS: usize = 4_000;
    let lines: Vec<String> = (0..PARAGRAPHS)
        .map(|i| format!("Paragraph {i} of the imported text."))
        .collect();
    let djot: String = lines.iter().map(|line| format!("{line}\n\n")).collect();
    let html: String = lines.iter().map(|line| format!("<p>{line}</p>")).collect();
    let expected = {
        let fresh = TextDocument::new();
        fresh.set_djot_sync(&djot).unwrap();
        fresh.to_plain_text().unwrap()
    };

    type Start = fn(&TextDocument, &str, &str) -> Box<dyn FnOnce() + Send>;
    let imports: [(&str, Start); 3] = [
        ("set_djot", |doc, djot, _| {
            let op = doc.set_djot(djot).unwrap();
            Box::new(move || {
                op.wait().unwrap();
            })
        }),
        ("set_markdown", |doc, djot, _| {
            let op = doc.set_markdown(djot).unwrap();
            Box::new(move || {
                op.wait().unwrap();
            })
        }),
        ("set_html", |doc, _, html| {
            let op = doc.set_html(html).unwrap();
            Box::new(move || {
                op.wait().unwrap();
            })
        }),
    ];
    for (label, start) in imports {
        let doc = TextDocument::new();
        doc.set_djot_sync("The text before the import.\n").unwrap();
        // The text a read sees before the import starts: a cache holding it would be as
        // stale as one holding a half-imported text.
        assert!(doc.to_plain_text().unwrap().contains("before the import"));

        let wait = start(&doc, &djot, &html);
        // Read at once, while the import is writing: thousands of paragraphs take far
        // longer to import than the few microseconds before this read.
        let during = doc.to_plain_text().unwrap_or_default();
        wait();

        assert_ne!(
            during, expected,
            "{label}: the read was meant to happen while the import ran"
        );
        assert_eq!(
            doc.to_plain_text().unwrap(),
            expected,
            "{label}: the text read after the import is the imported text"
        );
    }
}
