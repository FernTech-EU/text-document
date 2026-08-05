use text_document::{
    BlockFormat, FlowElement, FragmentContent, ListStyle, MoveMode, TextBlock, TextDocument,
    TextFormat,
};

fn new_doc_with_text(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

fn first_block(doc: &TextDocument) -> TextBlock {
    match &doc.flow()[0] {
        FlowElement::Block(b) => b.clone(),
        _ => panic!("expected block"),
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Content methods: text(), length(), is_empty(), is_valid()
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn text_returns_block_content() {
    let doc = new_doc_with_text("Hello world");
    let block = first_block(&doc);
    assert_eq!(block.text(), "Hello world");
}

#[test]
fn text_empty_doc_block() {
    let doc = TextDocument::new();
    let block = first_block(&doc);
    assert_eq!(block.text(), "");
}

#[test]
fn length_returns_char_count() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    assert_eq!(block.length(), 5);
}

#[test]
fn length_unicode() {
    let doc = new_doc_with_text("café");
    let block = first_block(&doc);
    assert_eq!(block.length(), 4);
}

#[test]
fn is_empty_on_empty_block() {
    let doc = TextDocument::new();
    let block = first_block(&doc);
    assert!(block.is_empty());
}

#[test]
fn is_empty_on_non_empty_block() {
    let doc = new_doc_with_text("Hi");
    let block = first_block(&doc);
    assert!(!block.is_empty());
}

#[test]
fn is_valid_for_existing_block() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    assert!(block.is_valid());
}

#[test]
fn block_by_id_none_for_bogus_id() {
    let doc = new_doc_with_text("Hello");
    assert!(doc.block_by_id(999999).is_none());
}

#[test]
fn is_valid_false_after_block_deleted() {
    let doc = new_doc_with_text("First\nSecond");
    let block1 = doc.block_by_number(1).unwrap();
    assert!(block1.is_valid());
    // Replace the document — the old block entity is gone
    doc.set_plain_text("Only one block").unwrap();
    assert!(
        !block1.is_valid(),
        "block handle should be invalid after document reset"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Identity: id(), position(), block_number()
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn id_is_stable() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    let id1 = block.id();
    let id2 = block.id();
    assert_eq!(id1, id2);
}

#[test]
fn position_first_block_is_zero() {
    let doc = new_doc_with_text("Hello\nWorld");
    let block = first_block(&doc);
    assert_eq!(block.position(), 0);
}

#[test]
fn position_second_block() {
    let doc = new_doc_with_text("Hello\nWorld");
    let block = doc.block_by_number(1).unwrap();
    // "Hello" (5 chars) + block separator (1) = position 6
    assert_eq!(block.position(), 6);
}

#[test]
fn block_number_first() {
    let doc = new_doc_with_text("A\nB\nC");
    let block = first_block(&doc);
    assert_eq!(block.block_number(), 0);
}

#[test]
fn block_number_second() {
    let doc = new_doc_with_text("A\nB\nC");
    let block = doc.block_by_number(1).unwrap();
    assert_eq!(block.block_number(), 1);
}

#[test]
fn block_number_third() {
    let doc = new_doc_with_text("A\nB\nC");
    let block = doc.block_by_number(2).unwrap();
    assert_eq!(block.block_number(), 2);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// next() and previous()
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn next_from_first_block() {
    let doc = new_doc_with_text("A\nB\nC");
    let block = doc.block_by_number(0).unwrap();
    let next = block.next().unwrap();
    assert_eq!(next.text(), "B");
}

#[test]
fn next_from_middle_block() {
    let doc = new_doc_with_text("A\nB\nC");
    let block = doc.block_by_number(1).unwrap();
    let next = block.next().unwrap();
    assert_eq!(next.text(), "C");
}

#[test]
fn next_from_last_block_is_none() {
    let doc = new_doc_with_text("A\nB\nC");
    let block = doc.block_by_number(2).unwrap();
    assert!(block.next().is_none());
}

#[test]
fn previous_from_last_block() {
    let doc = new_doc_with_text("A\nB\nC");
    let block = doc.block_by_number(2).unwrap();
    let prev = block.previous().unwrap();
    assert_eq!(prev.text(), "B");
}

#[test]
fn previous_from_middle_block() {
    let doc = new_doc_with_text("A\nB\nC");
    let block = doc.block_by_number(1).unwrap();
    let prev = block.previous().unwrap();
    assert_eq!(prev.text(), "A");
}

#[test]
fn previous_from_first_block_is_none() {
    let doc = new_doc_with_text("A\nB\nC");
    let block = doc.block_by_number(0).unwrap();
    assert!(block.previous().is_none());
}

#[test]
fn next_and_previous_none_for_single_block() {
    let doc = new_doc_with_text("Only one block");
    let block = first_block(&doc);
    assert!(block.next().is_none());
    assert!(block.previous().is_none());
}

#[test]
fn next_previous_roundtrip() {
    let doc = new_doc_with_text("A\nB\nC");
    let block = doc.block_by_number(1).unwrap();
    let next = block.next().unwrap();
    let back = next.previous().unwrap();
    assert_eq!(back.id(), block.id());
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Structural context: frame(), table_cell()
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn frame_returns_valid_handle() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    let frame = block.frame();
    assert!(frame.id() > 0);
}

#[test]
fn table_cell_returns_none_for_regular_block() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    assert!(block.table_cell().is_none());
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Formatting: block_format(), char_format_at()
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn block_format_default() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    let fmt = block.block_format();
    // Default format has no alignment set
    assert_eq!(fmt.alignment, None);
}

#[test]
fn block_format_after_set() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(0);
    cursor.set_position(5, MoveMode::KeepAnchor);
    let fmt = BlockFormat {
        alignment: Some(text_document::Alignment::Center),
        ..Default::default()
    };
    cursor.set_block_format(&fmt).unwrap();

    let block = first_block(&doc);
    assert_eq!(
        block.block_format().alignment,
        Some(text_document::Alignment::Center)
    );
}

#[test]
fn char_format_at_default() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    // Position 0 should have a format (even if all fields are None/default)
    let fmt = block.char_format_at(0);
    assert!(fmt.is_some());
}

#[test]
fn char_format_at_with_bold() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor_at(0);
    cursor.set_position(5, MoveMode::KeepAnchor);
    let fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    cursor.set_char_format(&fmt).unwrap();

    let block = first_block(&doc);
    let fmt_at_0 = block.char_format_at(0).unwrap();
    assert_eq!(fmt_at_0.font_bold, Some(true));

    // Character at position 6 (in " world") should NOT be bold
    let fmt_at_6 = block
        .char_format_at(6)
        .expect("position 6 is within 'Hello world'");
    assert_ne!(fmt_at_6.font_bold, Some(true));
}

#[test]
fn char_format_at_out_of_range() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    assert!(block.char_format_at(999).is_none());
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Fragments
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn fragments_single_run() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    let frags = block.fragments();
    // A plain text block should have at least one text fragment
    assert!(!frags.is_empty());
    if let FragmentContent::Text {
        text,
        offset,
        length,
        ..
    } = &frags[0]
    {
        assert_eq!(text, "Hello");
        assert_eq!(*offset, 0);
        assert_eq!(*length, 5);
    } else {
        panic!("expected Text fragment");
    }
}

#[test]
fn fragments_unicode_offsets() {
    let doc = new_doc_with_text("café");
    let block = first_block(&doc);
    let frags = block.fragments();
    assert!(!frags.is_empty());
    if let FragmentContent::Text {
        text,
        offset,
        length,
        ..
    } = &frags[0]
    {
        assert_eq!(text, "café");
        assert_eq!(*offset, 0);
        assert_eq!(*length, 4); // 4 characters, not 5 bytes
    } else {
        panic!("expected Text fragment");
    }
}

#[test]
fn fragments_multiple_runs_after_formatting() {
    let doc = new_doc_with_text("Hello world");
    // Bold the first word
    let cursor = doc.cursor_at(0);
    cursor.set_position(5, MoveMode::KeepAnchor);
    let fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    cursor.set_char_format(&fmt).unwrap();

    let block = first_block(&doc);
    let frags = block.fragments();
    // Should have at least 2 fragments: bold "Hello" and regular " world"
    assert!(
        frags.len() >= 2,
        "expected at least 2 fragments, got {}",
        frags.len()
    );
}

#[test]
fn fragments_offsets_are_sequential() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor_at(0);
    cursor.set_position(5, MoveMode::KeepAnchor);
    let fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    cursor.set_char_format(&fmt).unwrap();

    let block = first_block(&doc);
    let frags = block.fragments();

    let mut expected_offset = 0;
    for frag in &frags {
        match frag {
            FragmentContent::Text { offset, length, .. } => {
                assert_eq!(*offset, expected_offset);
                expected_offset += length;
            }
            // Both objects cost exactly one character.
            FragmentContent::Image { offset, .. }
            | FragmentContent::FootnoteReference { offset, .. } => {
                assert_eq!(*offset, expected_offset);
                expected_offset += 1;
            }
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// List membership: list(), list_item_index()
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn list_returns_none_for_non_list_block() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    assert!(block.list().is_none());
}

#[test]
fn list_item_index_returns_none_for_non_list_block() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    assert!(block.list_item_index().is_none());
}

#[test]
fn list_returns_handle_after_create_list() {
    let doc = new_doc_with_text("Item one\nItem two");
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(17, MoveMode::KeepAnchor); // select all
    cursor.create_list(ListStyle::Decimal).unwrap();

    let block = first_block(&doc);
    let list = block.list();
    assert!(list.is_some(), "block should belong to a list");
}

#[test]
fn list_item_index_after_create_list() {
    let doc = new_doc_with_text("Item one\nItem two");
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(17, MoveMode::KeepAnchor);
    cursor.create_list(ListStyle::Decimal).unwrap();

    let block0 = doc.block_by_number(0).unwrap();
    assert_eq!(block0.list_item_index(), Some(0));

    let block1 = doc.block_by_number(1).unwrap();
    assert_eq!(block1.list_item_index(), Some(1));
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Snapshot
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn snapshot_captures_all_fields() {
    let doc = new_doc_with_text("Hello world");
    let block = first_block(&doc);
    let snap = block.snapshot();

    assert_eq!(snap.block_id, block.id());
    assert_eq!(snap.text, "Hello world");
    assert_eq!(snap.position, 0);
    assert_eq!(snap.length, 11);
    assert!(!snap.fragments.is_empty());
    assert!(snap.list_info.is_none());
}

#[test]
fn snapshot_captures_list_info() {
    let doc = new_doc_with_text("Item one\nItem two");
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(17, MoveMode::KeepAnchor);
    cursor.create_list(ListStyle::Disc).unwrap();

    let block = doc.block_by_number(0).unwrap();
    let snap = block.snapshot();
    assert!(snap.list_info.is_some());
    let info = snap.list_info.unwrap();
    assert_eq!(info.style, ListStyle::Disc);
    assert_eq!(info.item_index, 0);
    assert!(!info.marker.is_empty()); // should be "•" or similar
}

#[test]
fn snapshot_second_list_item() {
    let doc = new_doc_with_text("One\nTwo\nThree");
    let cursor = doc.cursor();
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.set_position(13, MoveMode::KeepAnchor);
    cursor.create_list(ListStyle::Decimal).unwrap();

    let block2 = doc.block_by_number(2).unwrap();
    let snap = block2.snapshot();
    let info = snap.list_info.unwrap();
    assert_eq!(info.item_index, 2);
    // Decimal marker for item 3 (1-based) should contain "3"
    assert!(
        info.marker.contains('3'),
        "marker should contain '3', got: {}",
        info.marker
    );
}

#[test]
fn snapshot_block_format() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(0);
    let fmt = BlockFormat {
        heading_level: Some(2),
        ..Default::default()
    };
    cursor.set_block_format(&fmt).unwrap();

    let block = first_block(&doc);
    let snap = block.snapshot();
    assert_eq!(snap.block_format.heading_level, Some(2));
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Snapshot parent context
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn snapshot_has_parent_frame_id() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    let snap = block.snapshot();
    assert!(
        snap.parent_frame_id.is_some(),
        "snapshot should have parent_frame_id"
    );
    // The parent frame should match the block's frame
    assert_eq!(snap.parent_frame_id.unwrap(), block.frame().id());
}

#[test]
fn snapshot_non_table_block_has_no_table_cell() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    let snap = block.snapshot();
    assert!(
        snap.table_cell.is_none(),
        "non-table block should have no table_cell context"
    );
}

#[test]
fn snapshot_table_cell_block_has_table_cell_context() {
    let doc = new_doc_with_text("Before");
    let cursor = doc.cursor_at(6);
    cursor.insert_table(2, 2).unwrap();

    // Find the table in the flow
    let flow = doc.flow();
    let table = flow
        .iter()
        .find_map(|e| match e {
            FlowElement::Table(t) => Some(t.clone()),
            _ => None,
        })
        .expect("should have a table");

    // Get a cell's block
    let cell = table.cell(0, 0).expect("cell (0,0) should exist");
    let cell_blocks = cell.blocks();
    assert!(
        !cell_blocks.is_empty(),
        "cell should have at least one block"
    );

    let snap = cell_blocks[0].snapshot();
    assert!(
        snap.table_cell.is_some(),
        "block inside table cell should have table_cell context"
    );
    let ctx = snap.table_cell.unwrap();
    assert_eq!(ctx.table_id, table.id());
    assert_eq!(ctx.row, 0);
    assert_eq!(ctx.column, 0);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Clone / Send / Sync
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn text_block_is_clone() {
    let doc = new_doc_with_text("Hello");
    let block = first_block(&doc);
    let cloned = block.clone();
    assert_eq!(block.id(), cloned.id());
    assert_eq!(block.text(), cloned.text());
}
