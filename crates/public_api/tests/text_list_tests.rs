use text_document::{ListFormat, ListStyle, MoveMode, TextDocument};

fn new_doc_with_list() -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text("Alpha\nBeta\nGamma").unwrap();
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(16, MoveMode::KeepAnchor); // select all
    cursor.create_list(ListStyle::Decimal).unwrap();
    doc
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// TextList basics
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn list_id_is_nonzero() {
    let doc = new_doc_with_list();
    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();
    assert!(list.id() > 0);
}

#[test]
fn list_style_matches() {
    let doc = new_doc_with_list();
    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();
    assert_eq!(list.style(), ListStyle::Decimal);
}

#[test]
fn list_count() {
    let doc = new_doc_with_list();
    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();
    assert_eq!(list.count(), 3);
}

#[test]
fn list_item_returns_correct_block() {
    let doc = new_doc_with_list();
    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();

    let item0 = list.item(0).unwrap();
    assert_eq!(item0.text(), "Alpha");

    let item1 = list.item(1).unwrap();
    assert_eq!(item1.text(), "Beta");

    let item2 = list.item(2).unwrap();
    assert_eq!(item2.text(), "Gamma");
}

#[test]
fn list_item_out_of_range() {
    let doc = new_doc_with_list();
    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();
    assert!(list.item(10).is_none());
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// item_marker()
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn item_marker_decimal() {
    let doc = new_doc_with_list();
    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();

    let m0 = list.item_marker(0);
    assert!(
        m0.contains('1'),
        "first decimal marker should contain '1', got: {m0}"
    );

    let m1 = list.item_marker(1);
    assert!(
        m1.contains('2'),
        "second decimal marker should contain '2', got: {m1}"
    );

    let m2 = list.item_marker(2);
    assert!(
        m2.contains('3'),
        "third decimal marker should contain '3', got: {m2}"
    );
}

#[test]
fn item_marker_disc() {
    let doc = TextDocument::new();
    doc.set_plain_text("A\nB").unwrap();
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(3, MoveMode::KeepAnchor);
    cursor.create_list(ListStyle::Disc).unwrap();

    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();
    let m = list.item_marker(0);
    assert!(
        m.contains('\u{2022}'),
        "disc marker should contain bullet, got: {m}"
    );
}

#[test]
fn item_marker_lower_alpha() {
    let doc = TextDocument::new();
    doc.set_plain_text("X\nY\nZ").unwrap();
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(5, MoveMode::KeepAnchor);
    cursor.create_list(ListStyle::LowerAlpha).unwrap();

    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();

    assert!(list.item_marker(0).contains('a'));
    assert!(list.item_marker(1).contains('b'));
    assert!(list.item_marker(2).contains('c'));
}

#[test]
fn item_marker_upper_roman() {
    let doc = TextDocument::new();
    doc.set_plain_text("X\nY\nZ\nW").unwrap();
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(7, MoveMode::KeepAnchor);
    cursor.create_list(ListStyle::UpperRoman).unwrap();

    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();

    assert!(
        list.item_marker(0).contains('I'),
        "got: {}",
        list.item_marker(0)
    );
    assert!(
        list.item_marker(1).contains("II"),
        "got: {}",
        list.item_marker(1)
    );
    assert!(
        list.item_marker(2).contains("III"),
        "got: {}",
        list.item_marker(2)
    );
    assert!(
        list.item_marker(3).contains("IV"),
        "got: {}",
        list.item_marker(3)
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// prefix / suffix
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn list_prefix_and_suffix() {
    let doc = new_doc_with_list();
    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();
    // prefix and suffix may be empty for default lists
    let _prefix = list.prefix();
    let _suffix = list.suffix();
    // just ensure they don't panic
}

#[test]
fn list_indent() {
    let doc = new_doc_with_list();
    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();
    let _indent = list.indent();
    // just ensure it doesn't panic
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// ListInfo in snapshot
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn snapshot_list_info_all_items() {
    let doc = new_doc_with_list();

    for i in 0..3 {
        let block = doc.block_by_number(i).unwrap();
        let snap = block.snapshot();
        assert!(snap.list_info.is_some(), "block {i} should have list_info");
        let info = snap.list_info.unwrap();
        assert_eq!(info.item_index, i);
        assert_eq!(info.style, ListStyle::Decimal);
    }
}

#[test]
fn snapshot_list_info_markers_sequential() {
    let doc = new_doc_with_list();

    let markers: Vec<String> = (0..3)
        .map(|i| {
            doc.block_by_number(i)
                .unwrap()
                .snapshot()
                .list_info
                .unwrap()
                .marker
        })
        .collect();

    assert!(markers[0].contains('1'));
    assert!(markers[1].contains('2'));
    assert!(markers[2].contains('3'));
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Clone
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn list_is_clone() {
    let doc = new_doc_with_list();
    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();
    let cloned = list.clone();
    assert_eq!(list.id(), cloned.id());
    assert_eq!(list.style(), cloned.style());
    assert_eq!(list.count(), cloned.count());
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// All blocks in list share the same list handle
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn all_blocks_share_same_list_id() {
    let doc = new_doc_with_list();
    let id0 = doc.block_by_number(0).unwrap().list().unwrap().id();
    let id1 = doc.block_by_number(1).unwrap().list().unwrap().id();
    let id2 = doc.block_by_number(2).unwrap().list().unwrap().id();
    assert_eq!(id0, id1);
    assert_eq!(id1, id2);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// TextList::format()
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn list_format_returns_all_props() {
    let doc = new_doc_with_list();
    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();
    let fmt = list.format();
    assert_eq!(fmt.style, Some(ListStyle::Decimal));
    assert_eq!(fmt.indent, Some(0));
    assert!(fmt.prefix.is_some());
    assert!(fmt.suffix.is_some());
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// TextCursor::current_list()
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn current_list_returns_some_when_in_list() {
    let doc = new_doc_with_list();
    let cursor = doc.cursor_at(0);
    let list = cursor.current_list();
    assert!(list.is_some());
    assert_eq!(list.unwrap().style(), ListStyle::Decimal);
}

#[test]
fn current_list_returns_none_when_not_in_list() {
    let doc = TextDocument::new();
    doc.set_plain_text("Hello world").unwrap();
    let cursor = doc.cursor_at(0);
    assert!(cursor.current_list().is_none());
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// set_list_format / set_current_list_format
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn set_list_format_changes_style() {
    let doc = new_doc_with_list();
    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();
    let list_id = list.id();

    let cursor = doc.cursor_at(0);
    cursor
        .set_list_format(
            list_id,
            &ListFormat {
                style: Some(ListStyle::Circle),
                ..Default::default()
            },
        )
        .unwrap();

    assert_eq!(list.style(), ListStyle::Circle);
}

#[test]
fn set_current_list_format_changes_indent() {
    let doc = new_doc_with_list();
    let cursor = doc.cursor_at(0);
    cursor
        .set_current_list_format(&ListFormat {
            indent: Some(2),
            ..Default::default()
        })
        .unwrap();

    let list = doc.block_by_number(0).unwrap().list().unwrap();
    assert_eq!(list.indent(), 2);
}

#[test]
fn set_list_format_is_undoable() {
    let doc = new_doc_with_list();
    let block = doc.block_by_number(0).unwrap();
    let list = block.list().unwrap();
    let list_id = list.id();

    assert_eq!(list.style(), ListStyle::Decimal);

    let cursor = doc.cursor_at(0);
    cursor
        .set_list_format(
            list_id,
            &ListFormat {
                style: Some(ListStyle::Square),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(list.style(), ListStyle::Square);

    doc.undo().unwrap();
    assert_eq!(list.style(), ListStyle::Decimal);

    doc.redo().unwrap();
    assert_eq!(list.style(), ListStyle::Square);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// add_block_to_list / add_current_block_to_list
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn add_block_to_list_explicit() {
    let doc = TextDocument::new();
    doc.set_plain_text("Alpha\nBeta\nGamma").unwrap();

    // Create list from first block only
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(5, MoveMode::KeepAnchor); // select "Alpha"
    cursor.create_list(ListStyle::Disc).unwrap();

    let list = doc.block_by_number(0).unwrap().list().unwrap();
    let list_id = list.id();
    assert_eq!(list.count(), 1);

    // Add second block explicitly
    let block1 = doc.block_by_number(1).unwrap();
    assert!(block1.list().is_none());

    cursor.add_block_to_list(block1.id(), list_id).unwrap();
    assert_eq!(list.count(), 2);
    assert!(doc.block_by_number(1).unwrap().list().is_some());
}

#[test]
fn add_current_block_to_list() {
    let doc = TextDocument::new();
    doc.set_plain_text("Alpha\nBeta\nGamma").unwrap();

    // Create list from first block only
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(5, MoveMode::KeepAnchor);
    cursor.create_list(ListStyle::Disc).unwrap();

    let list = doc.block_by_number(0).unwrap().list().unwrap();
    let list_id = list.id();
    assert_eq!(list.count(), 1);

    // Move cursor to second block and add it implicitly
    cursor.set_position(6, MoveMode::MoveAnchor); // inside "Beta"
    cursor.add_current_block_to_list(list_id).unwrap();
    assert_eq!(list.count(), 2);
}

#[test]
fn add_block_to_list_is_undoable() {
    let doc = TextDocument::new();
    doc.set_plain_text("Alpha\nBeta").unwrap();

    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(5, MoveMode::KeepAnchor);
    cursor.create_list(ListStyle::Disc).unwrap();

    let list = doc.block_by_number(0).unwrap().list().unwrap();
    let list_id = list.id();
    let block1 = doc.block_by_number(1).unwrap();

    cursor.add_block_to_list(block1.id(), list_id).unwrap();
    assert_eq!(list.count(), 2);

    doc.undo().unwrap();
    assert_eq!(list.count(), 1);
    assert!(doc.block_by_number(1).unwrap().list().is_none());

    doc.redo().unwrap();
    assert_eq!(list.count(), 2);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// remove_block_from_list / remove_current_block_from_list
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn remove_block_from_list_explicit() {
    let doc = new_doc_with_list();
    let list = doc.block_by_number(0).unwrap().list().unwrap();
    assert_eq!(list.count(), 3);

    let block1 = doc.block_by_number(1).unwrap();
    let cursor = doc.cursor();
    cursor.remove_block_from_list(block1.id()).unwrap();

    // Block 1 is no longer in the list
    assert!(doc.block_by_number(1).unwrap().list().is_none());
    assert_eq!(list.count(), 2);
    // Block still exists in the document
    assert_eq!(doc.block_by_number(1).unwrap().text(), "Beta");
}

#[test]
fn remove_current_block_from_list() {
    let doc = new_doc_with_list();
    let list = doc.block_by_number(0).unwrap().list().unwrap();
    assert_eq!(list.count(), 3);

    let cursor = doc.cursor_at(6); // inside "Beta"
    cursor.remove_current_block_from_list().unwrap();

    assert!(doc.block_by_number(1).unwrap().list().is_none());
    assert_eq!(list.count(), 2);
}

#[test]
fn remove_block_from_list_is_undoable() {
    let doc = new_doc_with_list();
    let list = doc.block_by_number(0).unwrap().list().unwrap();
    let block1 = doc.block_by_number(1).unwrap();

    let cursor = doc.cursor();
    cursor.remove_block_from_list(block1.id()).unwrap();
    assert_eq!(list.count(), 2);

    doc.undo().unwrap();
    assert_eq!(list.count(), 3);
    assert!(doc.block_by_number(1).unwrap().list().is_some());

    doc.redo().unwrap();
    assert_eq!(list.count(), 2);
}

#[test]
fn remove_last_block_auto_deletes_list() {
    let doc = TextDocument::new();
    doc.set_plain_text("Solo").unwrap();
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(4, MoveMode::KeepAnchor);
    cursor.create_list(ListStyle::Disc).unwrap();

    let block = doc.block_by_number(0).unwrap();
    assert!(block.list().is_some());

    cursor.remove_block_from_list(block.id()).unwrap();
    assert!(doc.block_by_number(0).unwrap().list().is_none());

    // Undo restores the list
    doc.undo().unwrap();
    assert!(doc.block_by_number(0).unwrap().list().is_some());
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// remove_list_item
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn remove_list_item_by_index() {
    let doc = new_doc_with_list();
    let list = doc.block_by_number(0).unwrap().list().unwrap();
    let list_id = list.id();
    assert_eq!(list.count(), 3);

    let cursor = doc.cursor();
    cursor.remove_list_item(list_id, 1).unwrap(); // remove "Beta"

    assert_eq!(list.count(), 2);
    // "Beta" block still exists but has no list
    assert!(doc.block_by_number(1).unwrap().list().is_none());
}

#[test]
fn remove_list_item_out_of_range_errors() {
    let doc = new_doc_with_list();
    let list = doc.block_by_number(0).unwrap().list().unwrap();
    let cursor = doc.cursor();
    assert!(cursor.remove_list_item(list.id(), 99).is_err());
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// The number an ordered list starts at
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

fn djot(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_sync(text).unwrap();
    doc
}

fn first_marker(doc: &TextDocument) -> String {
    doc.blocks()
        .iter()
        .find_map(|block| block.list().map(|list| list.item_marker(0)))
        .expect("a list")
}

/// A list written to start past 1 keeps its start through a load, the markers an editor
/// shows and every save, in Djot, Markdown and HTML. The model had nowhere to keep it: a
/// list starting at 3 came back numbered from 1 after a save and a reload.
#[test]
fn an_ordered_list_keeps_its_start_through_a_save_and_a_reload() {
    for (text, marker, saved) in [
        ("3. three\n4. four\n", "3.", "3. three\n\n4. four"),
        ("b. two\nc. three\n", "b.", "b. two\n\nc. three"),
        ("iii) three\niv) four\n", "iii)", "iii) three\n\niv) four"),
        ("0. zero\n1. one\n", "0.", "0. zero\n\n1. one"),
        (
            "1. a\n\n   3. x\n   4. y\n\n2. b\n",
            "1.",
            "1. a\n\n  3. x\n\n  4. y\n\n2. b",
        ),
    ] {
        let doc = djot(text);
        assert_eq!(first_marker(&doc), marker, "the marker of {text:?}");
        let saved_djot = doc.to_djot().unwrap();
        assert_eq!(saved_djot, saved, "{text:?} saved");
        assert_eq!(
            djot(&saved_djot).to_djot().unwrap(),
            saved,
            "{text:?} reloaded"
        );
    }

    let doc = djot("Before.\n\n3. three\n4. four\n");
    let items: Vec<String> = doc
        .blocks()
        .iter()
        .filter_map(|block| block.list_item_index().zip(block.list()))
        .map(|(index, list)| list.item_marker(index))
        .collect();
    assert_eq!(items, ["3.", "4."]);

    let markdown = TextDocument::new();
    markdown
        .set_markdown("7. seven\n8. eight\n")
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(markdown.to_markdown().unwrap(), "7. seven\n8. eight");
    assert!(markdown.to_djot().unwrap().starts_with("7. seven"));
    assert!(markdown.to_html().unwrap().contains("<ol start=\"7\">"));

    let html = TextDocument::new();
    html.set_html("<ol start=\"5\"><li>five</li><li>six</li></ol><p>p</p><ol><li>one</li></ol>")
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(html.to_djot().unwrap(), "5. five\n\n6. six\n\np\n\n1. one");
    assert!(
        html.to_html()
            .unwrap()
            .contains("<ol start=\"5\"><li>five</li><li>six</li></ol>")
    );
    let reloaded = TextDocument::new();
    reloaded
        .set_html(&html.to_html().unwrap())
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(reloaded.to_djot().unwrap(), html.to_djot().unwrap());
}

/// A copied list keeps its numbers wherever it is pasted, and a text cut and pasted back,
/// or put back as a version, keeps its lists' starts. The fragment carried no start: every
/// pasted list was numbered from 1.
#[test]
fn a_copied_list_keeps_its_numbers() {
    let source = djot("Intro.\n\n3. three\n4. four\n5. five\n\nOutro.\n");
    let original = source.to_djot().unwrap();
    let length = source.to_addressable_text().unwrap().chars().count();

    let whole = {
        let cursor = source.cursor();
        cursor.set_position(0, MoveMode::MoveAnchor);
        cursor.set_position(length, MoveMode::KeepAnchor);
        cursor.selection()
    };
    let pasted = TextDocument::new();
    pasted.cursor().insert_fragment(&whole).unwrap();
    assert_eq!(
        pasted.to_djot().unwrap(),
        original,
        "pasted into a new text"
    );
    assert!(whole.to_markdown().contains("3. three\n4. four"));
    assert!(whole.to_html().contains("<ol start=\"3\">"));

    // The items from the second on, numbered as they were.
    let from_four = {
        let text = source.to_addressable_text().unwrap();
        let at = |needle: &str| text[..text.find(needle).unwrap()].chars().count();
        let cursor = source.cursor();
        cursor.set_position(at("four"), MoveMode::MoveAnchor);
        cursor.set_position(at("Outro"), MoveMode::KeepAnchor);
        cursor.selection()
    };
    let doc = djot("Other.\n");
    doc.cursor_at(6).insert_fragment(&from_four).unwrap();
    assert!(
        doc.to_djot().unwrap().contains("4. four\n\n5. five"),
        "{:?}",
        doc.to_djot().unwrap()
    );

    // Cut everything and paste it back, and a version put back.
    let doc = djot(&original);
    let cursor = doc.cursor();
    cursor.select(text_document::SelectionType::Document);
    let cut = cursor.selection();
    cursor.remove_selected_text().unwrap();
    cursor.insert_fragment(&cut).unwrap();
    assert_eq!(doc.to_djot().unwrap(), original, "cut and pasted back");

    let doc = djot("Something else.\n");
    let cursor = doc.cursor();
    cursor.select(text_document::SelectionType::Document);
    cursor.insert_djot(&original).unwrap();
    assert_eq!(doc.to_djot().unwrap(), original, "put back as a version");

    let doc = djot("Something else.\n");
    let cursor = doc.cursor();
    cursor.select(text_document::SelectionType::Document);
    cursor
        .insert_markdown("Intro.\n\n3. three\n4. four\n5. five\n\nOutro.\n")
        .unwrap();
    assert_eq!(doc.to_djot().unwrap(), original, "put back as Markdown");
}
