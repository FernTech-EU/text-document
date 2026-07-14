use anyhow::Result;
use text_document::{
    BlockFormat, Color, FindOptions, ListStyle, MoveMode, MoveOperation, TextDocument, TextFormat,
};

const SEED: &str = include_str!("benchmark_seed.txt");

const MD_BLURB: &str = "\
# Section

A paragraph with **bold**, *italic*, and `code`.

- item one
- item two
- item three

```rust
fn demo() -> i32 { 42 }
```

> A short blockquote for good measure.

Another paragraph after the list, ending the section.
";

const HTML_BLURB: &str = "\
<h2>Subsection</h2>\
<p>Intro paragraph with <b>bold</b>, <i>italic</i>, and <code>code</code>.</p>\
<ul><li>alpha</li><li>beta</li><li>gamma</li></ul>\
<p>Closing paragraph.</p>";

pub fn run_benchmark_test() -> Result<()> {
    let doc = TextDocument::new();

    // 1. Seed with a substantial body of plain text.
    doc.set_plain_text(SEED)?;

    let cursor = doc.cursor();
    let base_format = cursor.char_format()?;

    // 2. Mass insertion of formatted text with rotating formats.
    let bold = TextFormat {
        font_bold: Some(true),
        ..base_format.clone()
    };
    let italic = TextFormat {
        font_italic: Some(true),
        foreground_color: Some(Color::rgb(30, 100, 200)),
        ..base_format.clone()
    };
    let underline = TextFormat {
        font_underline: Some(true),
        ..base_format.clone()
    };

    for i in 0..2000 {
        let fmt = match i % 4 {
            0 => &base_format,
            1 => &bold,
            2 => &italic,
            _ => &underline,
        };
        cursor.insert_formatted_text(
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ",
            fmt,
        )?;
    }

    // 3. Insert block separators and heading-formatted blocks.
    let heading = BlockFormat {
        heading_level: Some(2),
        ..Default::default()
    };
    for i in 0..50 {
        cursor.insert_block()?;
        cursor.set_block_format(&heading)?;
        cursor.insert_text(&format!("Heading number {i}"))?;
        cursor.insert_block()?;
        cursor.set_block_format(&BlockFormat::default())?;
        cursor.insert_text("Body paragraph under the heading with some filler.")?;
    }

    // 4. Inline Markdown and HTML insertions exercise parsers end-to-end.
    for _ in 0..30 {
        cursor.insert_markdown(MD_BLURB)?;
    }
    for _ in 0..30 {
        cursor.insert_html(HTML_BLURB)?;
    }

    // 5. Lists and nested structures.
    for _ in 0..20 {
        cursor.insert_block()?;
        cursor.create_list(ListStyle::Disc)?;
        for i in 0..10 {
            cursor.insert_text(&format!("List item {i}"))?;
            cursor.insert_block()?;
        }
    }

    // 6. Tables — a handful of small tables scattered through the doc.
    for _ in 0..10 {
        cursor.insert_block()?;
        let table = cursor.insert_table(4, 3)?;
        let _ = table;
        cursor.insert_text("Cell text")?;
    }

    // 7. Navigation pass — exercise MoveOperation traversals.
    cursor.set_position(0, MoveMode::MoveAnchor);
    for _ in 0..500 {
        cursor.move_position(MoveOperation::NextWord, MoveMode::MoveAnchor, 1);
    }
    for _ in 0..200 {
        cursor.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 1);
    }

    // 8. Find / find_all / replace.
    let find_opts = FindOptions::default();
    for _ in 0..20 {
        let _ = doc.find_all("Lorem", &find_opts)?;
    }
    doc.replace_text(
        "Lorem",
        "LOREM",
        true,
        &text_document::ReplaceOptions::new(find_opts.clone()),
    )?;
    doc.replace_text(
        "LOREM",
        "Lorem",
        true,
        &text_document::ReplaceOptions::new(find_opts.clone()),
    )?;

    // 9. Undo / redo churn — stacks touch many entities.
    for _ in 0..30 {
        doc.undo()?;
    }
    for _ in 0..30 {
        doc.redo()?;
    }

    // 10. Exports stress the read-only traversal paths.
    for _ in 0..5 {
        let _ = doc.to_plain_text()?;
        let _ = doc.to_markdown()?;
        let _ = doc.to_html()?;
    }
    let _ = doc.to_latex("article", true)?;

    Ok(())
}
