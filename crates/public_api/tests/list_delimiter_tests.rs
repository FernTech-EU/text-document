// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! An ordered list paints and exports its delimiter: `1.` or `1)`, not a bare `1`.

use text_document::{DocumentFragment, ListFormat, ListStyle, MoveMode, TextDocument};

/// The marker the first item of the list at block `block` paints.
fn marker(doc: &TextDocument, block: usize) -> String {
    let b = doc.block_by_number(block).expect("block");
    let list = b.list().expect("list");
    // The position of the block among the items of its list.
    let mut index = 0;
    for earlier in 0..block {
        if let Some(l) = doc.block_by_number(earlier).and_then(|b| b.list())
            && l.id() == list.id()
        {
            index += 1;
        }
    }
    list.item_marker(index)
}

fn from_markdown(md: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_markdown(md).unwrap().wait().unwrap();
    doc
}

fn list_doc(style: ListStyle) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text("Alpha\nBeta").unwrap();
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(10, MoveMode::KeepAnchor);
    cursor.create_list(style).unwrap();
    doc
}

#[test]
fn markdown_import_paints_the_delimiter_it_was_written_with() {
    assert_eq!(marker(&from_markdown("1. a"), 0), "1.");
    assert_eq!(marker(&from_markdown("1) a"), 0), "1)");
    assert_eq!(marker(&from_markdown("3) a\n4) b"), 1), "4)");
    assert_eq!(marker(&from_markdown("- a"), 0), "\u{2022}");
}

#[test]
fn markdown_paste_paints_the_delimiter_it_was_written_with() {
    for (md, expected) in [("1. a\n2. b", "1."), ("1) a\n2) b", "1)")] {
        let doc = TextDocument::new();
        doc.set_plain_text("x").unwrap();
        let cursor = doc.cursor();
        cursor.set_position(1, MoveMode::MoveAnchor);
        cursor.insert_markdown(&format!("\n\n{md}")).unwrap();
        let list_block = (0..doc.block_count())
            .find(|n| doc.block_by_number(*n).unwrap().list().is_some())
            .expect("a list item");
        assert_eq!(marker(&doc, list_block), expected, "{md:?}");
    }
    // The single-block paste takes the same path as a middle block.
    let doc = TextDocument::new();
    doc.set_plain_text("").unwrap();
    doc.cursor().insert_markdown("1) only").unwrap();
    assert_eq!(marker(&doc, 0), "1)");
}

#[test]
fn html_import_and_paste_paint_a_period() {
    let doc = TextDocument::new();
    doc.set_html("<ol><li>a</li><li>b</li></ol>")
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(marker(&doc, 0), "1.");
    assert_eq!(marker(&doc, 1), "2.");

    let doc = TextDocument::new();
    doc.set_plain_text("x").unwrap();
    doc.cursor().insert_html("<ol><li>a</li></ol>").unwrap();
    let list_block = (0..doc.block_count())
        .find(|n| doc.block_by_number(*n).unwrap().list().is_some())
        .expect("a list item");
    assert_eq!(marker(&doc, list_block), "1.");

    let doc = TextDocument::new();
    doc.set_html("<ul><li>a</li></ul>").unwrap().wait().unwrap();
    assert_eq!(marker(&doc, 0), "\u{2022}");
}

#[test]
fn djot_paste_keeps_the_delimiter_import_keeps() {
    for (djot, expected) in [("1) a", "1)"), ("(1) a", "(1)"), ("1. a", "1.")] {
        let imported = TextDocument::new();
        imported.set_djot_sync(djot).unwrap();
        assert_eq!(marker(&imported, 0), expected, "import {djot:?}");

        let pasted = TextDocument::new();
        pasted.set_plain_text("").unwrap();
        pasted.cursor().insert_djot(djot).unwrap();
        assert_eq!(marker(&pasted, 0), expected, "paste {djot:?}");
    }
}

#[test]
fn a_list_made_in_the_editor_wears_a_period() {
    assert_eq!(marker(&list_doc(ListStyle::Decimal), 0), "1.");
    assert_eq!(marker(&list_doc(ListStyle::LowerAlpha), 0), "a.");
    assert_eq!(marker(&list_doc(ListStyle::Disc), 0), "\u{2022}");

    let doc = TextDocument::new();
    doc.set_plain_text("x").unwrap();
    let cursor = doc.cursor();
    cursor.insert_list(ListStyle::Decimal).unwrap();
    let list_block = (0..doc.block_count())
        .find(|n| doc.block_by_number(*n).unwrap().list().is_some())
        .expect("a list item");
    assert_eq!(marker(&doc, list_block), "1.");
}

#[test]
fn changing_a_lists_style_takes_the_affixes_of_the_new_kind() {
    let set = |doc: &TextDocument, style| {
        let cursor = doc.cursor();
        cursor.set_position(0, MoveMode::MoveAnchor);
        cursor
            .set_current_list_format(&ListFormat {
                style: Some(style),
                ..ListFormat::default()
            })
            .unwrap();
    };

    // Numbered to bullet: no "•." left behind.
    let doc = list_doc(ListStyle::Decimal);
    set(&doc, ListStyle::Disc);
    assert_eq!(marker(&doc, 0), "\u{2022}");

    // A Djot `(1)` list to a bullet loses its parentheses too.
    let doc = TextDocument::new();
    doc.set_djot_sync("(1) a").unwrap();
    set(&doc, ListStyle::Disc);
    assert_eq!(marker(&doc, 0), "\u{2022}");

    // Bullet to numbered gains the period.
    let doc = list_doc(ListStyle::Disc);
    set(&doc, ListStyle::Decimal);
    assert_eq!(marker(&doc, 0), "1.");

    // Between two numbered styles the delimiter stays.
    let doc = from_markdown("1) a");
    set(&doc, ListStyle::LowerAlpha);
    assert_eq!(marker(&doc, 0), "a)");

    // An explicit empty suffix is kept.
    let doc = list_doc(ListStyle::Disc);
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor
        .set_current_list_format(&ListFormat {
            style: Some(ListStyle::Decimal),
            suffix: Some(String::new()),
            ..ListFormat::default()
        })
        .unwrap();
    assert_eq!(marker(&doc, 0), "1");
}

#[test]
fn markdown_export_writes_the_delimiter() {
    for (md, expected) in [("1) a\n2) b", "1) a\n2) b"), ("1. a\n2. b", "1. a\n2. b")] {
        let doc = from_markdown(md);
        assert_eq!(doc.to_markdown().unwrap().trim_end(), expected);
    }
    // A Djot list read as `1)` writes `1)` to Markdown.
    let doc = TextDocument::new();
    doc.set_djot_sync("1) a").unwrap();
    assert_eq!(doc.to_markdown().unwrap().trim_end(), "1) a");
    // A list with no suffix stored (an older document) writes the period.
    let doc = list_doc(ListStyle::Decimal);
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor
        .set_current_list_format(&ListFormat {
            suffix: Some(String::new()),
            ..ListFormat::default()
        })
        .unwrap();
    assert_eq!(doc.to_markdown().unwrap().trim_end(), "1. Alpha\n2. Beta");
}

#[test]
fn two_lists_with_different_delimiters_stay_two_lists() {
    let doc = from_markdown("1. a\n1) b");
    let first = doc.block_by_number(0).unwrap().list().unwrap();
    let second = doc.block_by_number(1).unwrap().list().unwrap();
    assert_ne!(first.id(), second.id());
    // The delimiter alone ends the first list, so no blank line stands between them, and
    // the text reads back as the same two lists.
    let out = doc.to_markdown().unwrap();
    assert_eq!(out.trim_end(), "1. a\n1) b");
    let again = from_markdown(&out);
    assert_ne!(
        again.block_by_number(0).unwrap().list().unwrap().id(),
        again.block_by_number(1).unwrap().list().unwrap().id()
    );
}

#[test]
fn a_fragment_writes_the_delimiter_in_markdown() {
    assert_eq!(
        DocumentFragment::from_markdown("1) a")
            .to_markdown()
            .trim_end(),
        "1) a"
    );
    assert_eq!(
        DocumentFragment::from_markdown("1. a")
            .to_markdown()
            .trim_end(),
        "1. a"
    );
    assert_eq!(
        DocumentFragment::from_html("<ol><li>a</li></ol>")
            .to_markdown()
            .trim_end(),
        "1. a"
    );
}

#[test]
fn a_mixed_delimiter_document_round_trips() {
    let md = "1. a\n2. b\n\ntext\n\n1) c\n2) d\n";
    let doc = from_markdown(md);
    let out = doc.to_markdown().unwrap();
    let again = from_markdown(&out);
    assert_eq!(again.to_markdown().unwrap(), out);
    assert!(out.contains("1. a") && out.contains("2. b"), "{out:?}");
    assert!(out.contains("1) c") && out.contains("2) d"), "{out:?}");
}

/// The items of an HTML list pasted into a numbered list join it and wear its delimiter,
/// whatever it is: HTML has none to say.
#[test]
fn html_pasted_into_a_numbered_list_joins_it_and_takes_its_delimiter() {
    let html = "<ol><li>x</li><li>y</li></ol>";
    for (source, markers) in [
        ("1) a\n2) b", ["1)", "2)", "3)", "4)"]),
        ("1. a\n2. b", ["1.", "2.", "3.", "4."]),
    ] {
        let doc = from_markdown(source);
        doc.cursor_at(1).insert_html(html).unwrap();
        let ids: Vec<_> = (0..doc.block_count())
            .map(|n| doc.block_by_number(n).unwrap().list().map(|l| l.id()))
            .collect();
        assert!(
            ids.iter().all(|id| *id == ids[0] && id.is_some()),
            "{source:?}: {ids:?}"
        );
        let found: Vec<_> = (0..doc.block_count()).map(|n| marker(&doc, n)).collect();
        assert_eq!(found, markers, "{source:?}");
    }

    // A Djot `(1)` list, and an older list stored with no delimiter.
    let doc = TextDocument::new();
    doc.set_djot_sync("(1) a\n(2) b").unwrap();
    doc.cursor_at(1).insert_html(html).unwrap();
    let found: Vec<_> = (0..doc.block_count()).map(|n| marker(&doc, n)).collect();
    assert_eq!(found, ["(1)", "(2)", "(3)", "(4)"]);

    let doc = list_doc(ListStyle::Decimal);
    let cursor = doc.cursor_at(0);
    cursor
        .set_current_list_format(&ListFormat {
            suffix: Some(String::new()),
            ..ListFormat::default()
        })
        .unwrap();
    doc.cursor_at(5).insert_html(html).unwrap();
    let ids: Vec<_> = (0..doc.block_count())
        .map(|n| doc.block_by_number(n).unwrap().list().map(|l| l.id()))
        .collect();
    assert!(
        ids.iter().all(|id| *id == ids[0] && id.is_some()),
        "{ids:?}"
    );
    let found: Vec<_> = (0..doc.block_count()).map(|n| marker(&doc, n)).collect();
    assert_eq!(found, ["1", "2", "3", "4"]);
}

/// A list that is not the caret's, or of another style, is as it was: the pasted list keeps
/// the default delimiter.
#[test]
fn html_pasted_next_to_a_list_of_another_style_keeps_the_default_delimiter() {
    let doc = TextDocument::new();
    doc.set_djot_sync("- a\n- b").unwrap();
    doc.cursor_at(1)
        .insert_html("<ol><li>x</li><li>y</li></ol>")
        .unwrap();
    let markers: Vec<_> = (0..doc.block_count()).map(|n| marker(&doc, n)).collect();
    assert!(markers.contains(&"1.".to_string()), "{markers:?}");
}

/// Two items pasted from HTML take the head-overwrite path into an empty block and the
/// middle-block path after text; both keep the default delimiter.
#[test]
fn a_multi_item_html_paste_paints_a_period() {
    let html = "<ol><li>a</li><li>b</li></ol>";
    let doc = TextDocument::new();
    doc.set_plain_text("").unwrap();
    doc.cursor().insert_html(html).unwrap();
    assert_eq!(
        (marker(&doc, 0), marker(&doc, 1)),
        ("1.".into(), "2.".into())
    );

    let doc = TextDocument::new();
    doc.set_plain_text("x").unwrap();
    doc.cursor_at(1).insert_html(html).unwrap();
    let first_item = (0..doc.block_count())
        .find(|n| doc.block_by_number(*n).unwrap().list().is_some())
        .expect("a list item");
    assert_eq!(marker(&doc, first_item), "1.");
    assert_eq!(marker(&doc, first_item + 1), "2.");
}

/// A multi-item Djot paste keeps the delimiter, in an empty block and after text.
#[test]
fn a_multi_item_djot_paste_keeps_the_delimiter() {
    for djot in ["1) a\n2) b\n", "(1) a\n(2) b\n"] {
        let expected = if djot.starts_with('(') {
            ["(1)", "(2)"]
        } else {
            ["1)", "2)"]
        };
        let doc = TextDocument::new();
        doc.set_plain_text("").unwrap();
        doc.cursor().insert_djot(djot).unwrap();
        assert_eq!(
            (marker(&doc, 0), marker(&doc, 1)),
            (expected[0].into(), expected[1].into())
        );

        let doc = TextDocument::new();
        doc.set_plain_text("x").unwrap();
        doc.cursor_at(1).insert_djot(&format!("\n{djot}")).unwrap();
        let first_item = (0..doc.block_count())
            .find(|n| doc.block_by_number(*n).unwrap().list().is_some())
            .expect("a list item");
        assert_eq!(marker(&doc, first_item), expected[0]);
    }
}

/// The change of a list from numbered to bullet is one undo step and its redo makes the same
/// change.
#[test]
fn the_crossing_style_change_undoes_and_redoes() {
    let doc = list_doc(ListStyle::Decimal);
    assert_eq!(marker(&doc, 0), "1.");
    let cursor = doc.cursor_at(0);
    cursor
        .set_current_list_format(&ListFormat {
            style: Some(ListStyle::Disc),
            ..ListFormat::default()
        })
        .unwrap();
    assert_eq!(marker(&doc, 0), "\u{2022}");
    doc.undo().unwrap();
    assert_eq!(marker(&doc, 0), "1.");
    doc.redo().unwrap();
    assert_eq!(marker(&doc, 0), "\u{2022}");
}
