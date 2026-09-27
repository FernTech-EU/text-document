// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! A list pasted into a document keeps every item, in its order, at most as deep as an
//! editor's own nesting gestures nest one.
//!
//! Text pasted from outside can hold a list nested far deeper than anyone builds by typing.
//! It kept any depth: two hundred levels of Markdown stayed two hundred levels, and the
//! document then saved as Djot nested past what a host that bounds nesting on load opens
//! again. The other two formats lost the list on the way in: Djot nested that deeply came
//! in as one paragraph of markup, and HTML kept the first eighty-five items and dropped the
//! rest. Each now lands with every item and word, the items below the deepest level side by
//! side at it (see `common::parser_tools::list_depth`).

use common::parser_tools::djot_depth::is_too_deep;
use common::parser_tools::list_depth::{MAX_LIST_INDENT, MAX_LIST_LEVELS};
use text_document::TextDocument;

/// How deeply the pasted lists nest.
const DEPTH: usize = 200;

/// The text of the item at `level`, several words long.
fn item(level: usize) -> String {
    format!("Item {level} of the list")
}

/// A Djot list nested [`DEPTH`] levels deep, written as Djot nests one: each item indented
/// under the one before it, a blank line between them.
fn deep_djot() -> String {
    (0..DEPTH)
        .map(|level| format!("{}- {}\n\n", "  ".repeat(level), item(level)))
        .collect()
}

/// The same list in Markdown, where a nested item needs no blank line.
fn deep_markdown() -> String {
    (0..DEPTH)
        .map(|level| format!("{}- {}\n", "  ".repeat(level), item(level)))
        .collect()
}

/// The same list in HTML, each list inside the item before it.
fn deep_html() -> String {
    let mut html: String = (0..DEPTH)
        .map(|level| format!("<ul><li>{}", item(level)))
        .collect();
    html.push_str(&"</li></ul>".repeat(DEPTH));
    html
}

fn document() -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_sync("Before the list.\n\nAfter the list.\n")
        .unwrap();
    doc
}

/// Paste `text` in `syntax` at the end of the first paragraph.
fn paste(doc: &TextDocument, syntax: &str, text: &str) {
    let cursor = doc.cursor_at("Before the list.".chars().count());
    match syntax {
        "djot" => cursor.insert_djot(text),
        "markdown" => cursor.insert_markdown(text),
        _ => cursor.insert_html(text),
    }
    .unwrap();
}

/// Each block holding one of the pasted items: its text and its list's level.
fn pasted_items(doc: &TextDocument) -> Vec<(String, Option<u8>)> {
    doc.blocks()
        .iter()
        .filter(|block| block.text().contains("Item "))
        .map(|block| (block.text(), block.list().map(|list| list.indent())))
        .collect()
}

fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every item is a list item, in its order, with its words: item `n` at level `n` down to
/// the deepest level, and every deeper item at that level beside it. The saved Djot is one a
/// parser follows, and reads back as the same items at the same levels.
#[track_caller]
fn assert_landed_at_the_deepest_level(doc: &TextDocument, syntax: &str) {
    let items = pasted_items(doc);
    let expected: Vec<(String, Option<u8>)> = (0..DEPTH)
        .map(|level| {
            let indent = (level as i64).min(MAX_LIST_INDENT) as u8;
            (item(level), Some(indent))
        })
        .collect();
    assert_eq!(items, expected, "the {syntax} list as pasted");
    assert_eq!(MAX_LIST_LEVELS, MAX_LIST_INDENT as usize + 1);

    let all_words: String = std::iter::once("Before the list.".to_string())
        .chain((0..DEPTH).map(item))
        .chain(std::iter::once("After the list.".to_string()))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(words(&doc.to_plain_text().unwrap()), all_words, "{syntax}");

    let djot = doc.to_djot().unwrap();
    assert!(
        !is_too_deep(&djot),
        "the {syntax} paste saves as Djot a parser refuses"
    );
    let reloaded = TextDocument::new();
    reloaded.set_djot_sync(&djot).unwrap();
    assert_eq!(
        pasted_items(&reloaded),
        expected,
        "the {syntax} list reloaded"
    );
}

#[test]
fn a_djot_list_nested_two_hundred_levels_lands_at_the_deepest_level() {
    let doc = document();
    paste(&doc, "djot", &deep_djot());
    assert_landed_at_the_deepest_level(&doc, "Djot");
}

#[test]
fn a_markdown_list_nested_two_hundred_levels_lands_at_the_deepest_level() {
    let doc = document();
    paste(&doc, "markdown", &deep_markdown());
    assert_landed_at_the_deepest_level(&doc, "Markdown");
}

#[test]
fn an_html_list_nested_two_hundred_levels_lands_at_the_deepest_level() {
    let doc = document();
    paste(&doc, "html", &deep_html());
    assert_landed_at_the_deepest_level(&doc, "HTML");
}

/// A copy of a document's own deep list pasted back lands at the deepest level too: the
/// limit is the paste's, whatever made the fragment.
#[test]
fn a_copied_list_pasted_back_lands_at_the_deepest_level() {
    let source = TextDocument::new();
    source
        .set_markdown(&deep_markdown())
        .unwrap()
        .wait()
        .unwrap();
    // Loading keeps the document as it is: the limit is on what is inserted.
    let loaded: Vec<Option<u8>> = pasted_items(&source)
        .into_iter()
        .map(|(_, level)| level)
        .collect();
    assert_eq!(loaded.last(), Some(&Some((DEPTH - 1) as u8)));

    let cursor = source.cursor();
    cursor.select(text_document::SelectionType::Document);
    let fragment = cursor.selection();
    let doc = document();
    doc.cursor_at("Before the list.".chars().count())
        .insert_fragment(&fragment)
        .unwrap();
    assert_landed_at_the_deepest_level(&doc, "copied");
}

/// A list pasted into a quotation lands in it, at the same levels.
#[test]
fn a_deep_list_pasted_into_a_quotation_lands_in_it_at_the_deepest_level() {
    let doc = TextDocument::new();
    doc.set_djot_sync("> Quoted.\n\nAfter the list.\n").unwrap();
    doc.cursor_at("Quoted.".chars().count())
        .insert_djot(&deep_djot())
        .unwrap();
    let items = pasted_items(&doc);
    assert_eq!(items.len(), DEPTH);
    assert!(
        items
            .iter()
            .all(|(_, level)| level.is_some_and(|level| i64::from(level) <= MAX_LIST_INDENT))
    );
    for block in doc.blocks() {
        if block.text().contains("Item ") {
            let quoted = doc.cursor_at(block.position()).blockquote_depth_at_cursor();
            assert_eq!(quoted, 1, "{:?} left the quotation", block.text());
        }
    }
    assert!(!is_too_deep(&doc.to_djot().unwrap()));
}
