// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Task-list checkboxes survive a Markdown or HTML import, a paste, and a save.

use text_document::{DocumentFragment, MarkerType, MoveMode, TextDocument};

fn from_markdown(md: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_markdown(md).unwrap().wait().unwrap();
    doc
}

/// The box of every block, in order.
fn markers(doc: &TextDocument) -> Vec<Option<MarkerType>> {
    (0..doc.block_count())
        .map(|n| doc.block_by_number(n).unwrap().block_format().marker)
        .collect()
}

const CHECKED: Option<MarkerType> = Some(MarkerType::Checked);
const UNCHECKED: Option<MarkerType> = Some(MarkerType::Unchecked);

#[test]
fn a_markdown_import_keeps_the_boxes() {
    let doc = from_markdown("- [x] done\n- [ ] todo\n- plain\n");
    assert_eq!(markers(&doc), [CHECKED, UNCHECKED, None]);
    assert_eq!(doc.block_by_number(0).unwrap().text(), "done");
}

#[test]
fn a_markdown_paste_keeps_the_boxes_in_the_middle_and_alone() {
    let doc = TextDocument::new();
    doc.set_plain_text("x").unwrap();
    let cursor = doc.cursor();
    cursor.set_position(1, MoveMode::MoveAnchor);
    cursor.insert_markdown("\n\n- [x] a\n- [ ] b\n").unwrap();
    let found: Vec<_> = markers(&doc).into_iter().flatten().collect();
    assert_eq!(found, [MarkerType::Checked, MarkerType::Unchecked]);

    let doc = TextDocument::new();
    doc.set_plain_text("").unwrap();
    doc.cursor().insert_markdown("- [x] only").unwrap();
    assert_eq!(markers(&doc), [CHECKED]);
}

#[test]
fn a_djot_paste_keeps_the_boxes() {
    let doc = TextDocument::new();
    doc.set_plain_text("").unwrap();
    doc.cursor().insert_djot("- [x] a\n- [ ] b\n").unwrap();
    assert_eq!(markers(&doc), [CHECKED, UNCHECKED]);
}

#[test]
fn an_html_import_and_paste_keep_the_boxes() {
    let html =
        r#"<ul><li><input type="checkbox" checked> a</li><li><input type="checkbox"> b</li></ul>"#;
    let doc = TextDocument::new();
    doc.set_html(html).unwrap().wait().unwrap();
    assert_eq!(markers(&doc), [CHECKED, UNCHECKED]);
    assert_eq!(doc.block_by_number(0).unwrap().text(), "a");

    // Over an empty block: the head is overwritten.
    let doc = TextDocument::new();
    doc.set_plain_text("").unwrap();
    doc.cursor().insert_html(html).unwrap();
    assert_eq!(markers(&doc), [CHECKED, UNCHECKED]);

    // After text: the items are middle blocks.
    let doc = TextDocument::new();
    doc.set_plain_text("x").unwrap();
    let cursor = doc.cursor();
    cursor.set_position(1, MoveMode::MoveAnchor);
    cursor.insert_html(html).unwrap();
    let found: Vec<_> = markers(&doc).into_iter().flatten().collect();
    assert_eq!(found, [MarkerType::Checked, MarkerType::Unchecked]);
}

#[test]
fn a_markdown_round_trip_is_a_fixpoint() {
    for md in [
        "- [x] done\n- [ ] todo\n- plain\n",
        "1. [x] a\n2. [ ] b\n",
        "> - [x] quoted\n> - [ ] too\n",
        // The nested item is still a list item, not an indented code block.
        "- [x] a\n  - [ ] nested\n  - [x] nested too\n- [ ] b\n",
    ] {
        let first = from_markdown(md);
        let out = first.to_markdown().unwrap();
        assert_eq!(out, md.trim_end(), "written");
        let second = from_markdown(&out);
        assert_eq!(markers(&second), markers(&first), "{md:?}");
        assert_eq!(second.to_markdown().unwrap(), out);
    }
}

#[test]
fn a_djot_task_list_reaches_markdown() {
    let doc = TextDocument::new();
    doc.set_djot_sync("- [x] a\n- [ ] b\n").unwrap();
    assert_eq!(doc.to_markdown().unwrap(), "- [x] a\n- [ ] b");
}

#[test]
fn an_html_round_trip_is_a_fixpoint() {
    let doc = from_markdown("- [x] done\n- [ ] todo\n- plain\n");
    let html = doc.to_html().unwrap();
    assert!(html.contains(r#"<input type="checkbox" disabled="disabled" checked="checked"/>"#));
    let again = TextDocument::new();
    again.set_html(&html).unwrap().wait().unwrap();
    assert_eq!(markers(&again), [CHECKED, UNCHECKED, None]);
    assert_eq!(again.block_by_number(0).unwrap().text(), "done");
    assert_eq!(again.to_html().unwrap(), html);
}

#[test]
fn a_fragment_writes_the_boxes_in_markdown_and_html() {
    let fragment = DocumentFragment::from_markdown("- [x] a\n1. [ ] b\n");
    assert_eq!(fragment.to_markdown().trim_end(), "- [x] a\n1. [ ] b");
    let html = fragment.to_html();
    assert!(
        html.contains(
            r#"<li><input type="checkbox" disabled="disabled" checked="checked"/> a</li>"#
        )
    );
    let back = DocumentFragment::from_html(&html);
    assert_eq!(back.to_markdown().trim_end(), "- [x] a\n1. [ ] b");
}

/// EPUB chapters are XHTML: the checkbox must be well-formed XML, with quoted attribute
/// values and a closed tag.
#[cfg(feature = "epub")]
#[test]
fn an_epub_chapter_holds_a_well_formed_checkbox() {
    use std::io::Read;
    let doc = from_markdown("- [x] done\n- [ ] todo\n");
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("out.epub");
    doc.to_epub(path.to_str().unwrap())
        .expect("start")
        .wait()
        .expect("epub");
    let bytes = std::fs::read(&path).expect("read");
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip");
    let mut chapters = String::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        if f.name().expect("entry name").ends_with(".xhtml") {
            let mut text = String::new();
            f.read_to_string(&mut text).unwrap();
            chapters.push_str(&text);
        }
    }
    assert!(
        chapters.contains(r#"<input type="checkbox" disabled="disabled" checked="checked"/>"#),
        "{chapters}"
    );
    assert!(chapters.contains(r#"<input type="checkbox" disabled="disabled"/>"#));
    // No minimised attribute and no unclosed input anywhere.
    for tag in chapters
        .match_indices("<input")
        .map(|(i, _)| &chapters[i..])
    {
        let end = tag.find('>').expect("a closed tag");
        assert!(tag[..end].ends_with('/'), "unclosed: {}", &tag[..=end]);
        assert!(!tag[..end].contains(" disabled ") && !tag[..end].contains(" checked "));
    }
}
