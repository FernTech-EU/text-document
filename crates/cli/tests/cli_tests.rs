use std::fs;
use std::process::Command;

fn text_document_bin() -> Command {
    Command::new(std::env::var("CARGO_BIN_EXE_text-document").expect("set by cargo test"))
}

fn tmp_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("text-document-cli-tests");
    fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

// ── Convert ──────────────────────────────────────────────────────

#[test]
fn convert_txt_to_md() {
    let input = tmp_path("convert_input.txt");
    let output = tmp_path("convert_output.md");
    fs::write(&input, "Hello world").unwrap();

    let status = text_document_bin()
        .args(["convert", input.to_str().unwrap(), output.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    assert!(content.contains("Hello world"));
}

#[test]
fn convert_txt_to_html() {
    let input = tmp_path("convert_html_input.txt");
    let output = tmp_path("convert_html_output.html");
    fs::write(&input, "Hello HTML").unwrap();

    let status = text_document_bin()
        .args(["convert", input.to_str().unwrap(), output.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    assert!(content.contains("Hello HTML"));
}

#[test]
fn convert_txt_to_latex() {
    let input = tmp_path("convert_latex_input.txt");
    let output = tmp_path("convert_latex_output.tex");
    fs::write(&input, "Hello LaTeX").unwrap();

    let status = text_document_bin()
        .args(["convert", input.to_str().unwrap(), output.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    assert!(content.contains("Hello LaTeX"));
}

#[test]
fn convert_txt_to_latex_with_preamble() {
    let input = tmp_path("convert_preamble_input.txt");
    let output = tmp_path("convert_preamble_output.tex");
    fs::write(&input, "With preamble").unwrap();

    let status = text_document_bin()
        .args([
            "convert",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "--preamble",
            "--document-class",
            "report",
        ])
        .status()
        .unwrap();
    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    assert!(content.contains("With preamble"));
    assert!(content.contains("\\documentclass"));
}

#[test]
fn convert_md_to_txt() {
    let input = tmp_path("convert_md_input.md");
    let output = tmp_path("convert_md_output.txt");
    fs::write(&input, "# Heading\n\nParagraph text").unwrap();

    let status = text_document_bin()
        .args(["convert", input.to_str().unwrap(), output.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    assert!(content.contains("Paragraph text"));
}

#[test]
fn convert_html_to_txt() {
    let input = tmp_path("convert_html_to_txt_input.html");
    let output = tmp_path("convert_html_to_txt_output.txt");
    fs::write(&input, "<p>Hello from HTML</p>").unwrap();

    let status = text_document_bin()
        .args(["convert", input.to_str().unwrap(), output.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    assert!(content.contains("Hello from HTML"));
}

#[test]
fn convert_nonexistent_file_fails() {
    let output = tmp_path("convert_fail_output.txt");
    let status = text_document_bin()
        .args(["convert", "/nonexistent/file.txt", output.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(!status.success());
}

// ── Stats ────────────────────────────────────────────────────────

#[test]
fn stats_plain_text() {
    let input = tmp_path("stats_input.txt");
    fs::write(&input, "Hello world").unwrap();

    let output = text_document_bin()
        .args(["stats", input.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Characters:"));
    assert!(stdout.contains("Words:"));
    assert!(stdout.contains("Blocks:"));
}

#[test]
fn stats_nonexistent_file_fails() {
    let status = text_document_bin()
        .args(["stats", "/nonexistent/file.txt"])
        .status()
        .unwrap();
    assert!(!status.success());
}

// ── Find ─────────────────────────────────────────────────────────

#[test]
fn find_text() {
    let input = tmp_path("find_input.txt");
    fs::write(&input, "The quick brown fox jumps over the lazy dog").unwrap();

    let output = text_document_bin()
        .args(["find", input.to_str().unwrap(), "fox"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("fox"));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("1 match(es) found"));
}

#[test]
fn find_no_match() {
    let input = tmp_path("find_nomatch_input.txt");
    fs::write(&input, "Hello world").unwrap();

    let output = text_document_bin()
        .args(["find", input.to_str().unwrap(), "xyz"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no matches found"));
}

#[test]
fn find_case_sensitive() {
    let input = tmp_path("find_case_input.txt");
    fs::write(&input, "Hello HELLO hello").unwrap();

    // Case-insensitive (default) should find all
    let output = text_document_bin()
        .args(["find", input.to_str().unwrap(), "hello"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("3 match(es) found"));

    // Case-sensitive should find only "hello"
    let output = text_document_bin()
        .args(["find", input.to_str().unwrap(), "hello", "-c"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("1 match(es) found"));
}

// ── Replace ──────────────────────────────────────────────────────

#[test]
fn replace_text() {
    let input = tmp_path("replace_input.txt");
    let output_file = tmp_path("replace_output.txt");
    fs::write(&input, "Hello world").unwrap();

    let output = text_document_bin()
        .args([
            "replace",
            input.to_str().unwrap(),
            "world",
            "Rust",
            "-o",
            output_file.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());

    let content = fs::read_to_string(&output_file).unwrap();
    assert_eq!(content, "Hello Rust");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("1 replacement(s)"));
}

#[test]
fn replace_no_match() {
    let input = tmp_path("replace_nomatch_input.txt");
    fs::write(&input, "Hello world").unwrap();

    let output = text_document_bin()
        .args(["replace", input.to_str().unwrap(), "xyz", "abc"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no matches found"));
}

#[test]
fn replace_in_place() {
    let input = tmp_path("replace_inplace_input.txt");
    fs::write(&input, "foo bar foo").unwrap();

    let output = text_document_bin()
        .args(["replace", input.to_str().unwrap(), "foo", "baz"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let content = fs::read_to_string(&input).unwrap();
    assert_eq!(content, "baz bar baz");
}

// ── Cat ──────────────────────────────────────────────────────────

#[test]
fn cat_plain() {
    let input = tmp_path("cat_input.txt");
    fs::write(&input, "Cat content").unwrap();

    let output = text_document_bin()
        .args(["cat", input.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "Cat content");
}

#[test]
fn cat_markdown_format() {
    let input = tmp_path("cat_md_input.txt");
    fs::write(&input, "Some text").unwrap();

    let output = text_document_bin()
        .args(["cat", input.to_str().unwrap(), "-f", "markdown"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Some text"));
}

#[test]
fn cat_html_format() {
    let input = tmp_path("cat_html_input.txt");
    fs::write(&input, "HTML output").unwrap();

    let output = text_document_bin()
        .args(["cat", input.to_str().unwrap(), "-f", "html"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("HTML output"));
}

#[test]
fn cat_latex_format() {
    let input = tmp_path("cat_latex_input.txt");
    fs::write(&input, "LaTeX output").unwrap();

    let output = text_document_bin()
        .args(["cat", input.to_str().unwrap(), "-f", "latex"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("LaTeX output"));
}

#[test]
fn cat_nonexistent_file_fails() {
    let status = text_document_bin()
        .args(["cat", "/nonexistent/file.txt"])
        .status()
        .unwrap();
    assert!(!status.success());
}

// ── No subcommand ────────────────────────────────────────────────

#[test]
fn no_subcommand_shows_help() {
    let output = text_document_bin().output().unwrap();
    // clap exits with non-zero when no subcommand provided
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage"));
}
