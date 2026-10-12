//! What becomes of a block's front-matter mark (`{front_matter=true}`) when HTML is pasted
//! at its very start and the paste takes the block over.
//!
//! The pasted block replaces the head's whole block formatting there (heading level, line
//! height, page break, direction, background), and the front-matter mark is block formatting
//! like those: the pasted HTML carries none, so the head keeps none. It used to keep the old
//! block's, so pasting a chapter heading in front of a title-page line made the heading
//! front matter.

use text_document::TextDocument;

fn document(djot: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_sync(djot).expect("set_djot_sync");
    doc
}

/// The attribute line written above the line holding `needle`, or `""` when there is none.
fn attrs_above(djot: &str, needle: &str) -> String {
    let lines: Vec<&str> = djot.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} missing from {djot}"));
    match at.checked_sub(1).map(|i| lines[i]) {
        Some(line) if line.starts_with('{') => line.to_string(),
        _ => String::new(),
    }
}

#[test]
fn several_pasted_blocks_taking_over_a_front_matter_block_do_not_inherit_the_mark() {
    let doc = document("{front_matter=true}\nOn the title page.");
    doc.cursor_at(0)
        .insert_html("<h1>Chapter</h1><p>Body.</p>")
        .expect("insert_html");
    let djot = doc.to_djot().expect("to_djot");
    assert!(
        !attrs_above(&djot, "Chapter").contains("front_matter"),
        "the pasted heading took the old block's mark: {djot}"
    );
}

#[test]
fn one_pasted_block_taking_over_a_front_matter_block_does_not_inherit_the_mark() {
    let doc = document("{front_matter=true}\nOn the title page.");
    doc.cursor_at(0)
        .insert_html("<h1>Chapter</h1>")
        .expect("insert_html");
    let djot = doc.to_djot().expect("to_djot");
    assert!(
        !attrs_above(&djot, "Chapter").contains("front_matter"),
        "the pasted heading took the old block's mark: {djot}"
    );
}
