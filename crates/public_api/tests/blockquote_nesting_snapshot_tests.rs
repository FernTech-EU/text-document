//! Snapshot tests for tables and lists nested inside blockquotes.
//!
//! Verifies the public flow-snapshot API exposes nested structure the way
//! a typesetting consumer (e.g. text-typeset) expects: a blockquote
//! `FrameSnapshot` whose `elements` contain `Table` / list-info blocks.

use text_document::{FlowElementSnapshot, TextDocument};

fn doc_with_markdown(md: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_markdown(md).unwrap().wait().unwrap();
    doc
}

#[test]
fn snapshot_table_in_blockquote_is_nested_in_frame() {
    let doc = doc_with_markdown("> | a | b |\n> |---|---|\n> | c | d |");
    let snap = doc.snapshot_flow();

    assert_eq!(snap.elements.len(), 1, "one top-level element");
    let frame = match &snap.elements[0] {
        FlowElementSnapshot::Frame(f) => f,
        other => panic!("expected a Frame snapshot, got {other:?}"),
    };
    assert_eq!(frame.format.is_blockquote, Some(true));

    let tables: Vec<_> = frame
        .elements
        .iter()
        .filter(|e| matches!(e, FlowElementSnapshot::Table(_)))
        .collect();
    assert_eq!(tables.len(), 1, "frame elements: {:?}", frame.elements);

    if let FlowElementSnapshot::Table(t) = tables[0] {
        assert_eq!(t.rows, 2);
        assert_eq!(t.columns, 2);
        let texts: Vec<&str> = t.cells.iter().flat_map(|c| &c.blocks).map(|b| b.text.as_str()).collect();
        assert!(texts.contains(&"a") && texts.contains(&"d"), "cell texts: {texts:?}");
    }
}

#[test]
fn snapshot_table_after_blockquote_stays_top_level() {
    let doc = doc_with_markdown("> Para\n\n| a | b |\n|---|---|\n| c | d |");
    let snap = doc.snapshot_flow();

    assert_eq!(snap.elements.len(), 2, "elements: {:?}", snap.elements);
    assert!(matches!(&snap.elements[0], FlowElementSnapshot::Frame(f) if f.format.is_blockquote == Some(true)));
    assert!(matches!(&snap.elements[1], FlowElementSnapshot::Table(_)));

    // And nothing leaked into the quote.
    if let FlowElementSnapshot::Frame(f) = &snap.elements[0] {
        assert!(
            f.elements.iter().all(|e| matches!(e, FlowElementSnapshot::Block(_))),
            "frame elements: {:?}",
            f.elements
        );
    }
}

#[test]
fn snapshot_list_in_blockquote_carries_list_info() {
    let doc = doc_with_markdown("> - item1\n> - item2");
    let snap = doc.snapshot_flow();

    assert_eq!(snap.elements.len(), 1);
    let frame = match &snap.elements[0] {
        FlowElementSnapshot::Frame(f) => f,
        other => panic!("expected a Frame snapshot, got {other:?}"),
    };
    assert_eq!(frame.format.is_blockquote, Some(true));

    let blocks: Vec<_> = frame
        .elements
        .iter()
        .filter_map(|e| match e {
            FlowElementSnapshot::Block(b) => Some(b),
            _ => None,
        })
        .collect();
    assert_eq!(blocks.len(), 2, "frame elements: {:?}", frame.elements);
    for (i, b) in blocks.iter().enumerate() {
        let info = b
            .list_info
            .as_ref()
            .unwrap_or_else(|| panic!("block {} ({:?}) missing list_info", i, b.text));
        assert_eq!(info.item_index, i, "item index for {:?}", b.text);
        assert!(!info.marker.is_empty(), "marker rendered for {:?}", b.text);
    }
}

#[test]
fn snapshot_block_table_block_order_preserved_in_blockquote() {
    let doc = doc_with_markdown(
        "> First paragraph\n>\n> | a | b |\n> |---|---|\n> | c | d |\n>\n> Last paragraph",
    );
    let snap = doc.snapshot_flow();

    assert_eq!(snap.elements.len(), 1);
    let frame = match &snap.elements[0] {
        FlowElementSnapshot::Frame(f) => f,
        other => panic!("expected a Frame snapshot, got {other:?}"),
    };

    let kinds: Vec<&str> = frame
        .elements
        .iter()
        .map(|e| match e {
            FlowElementSnapshot::Block(_) => "block",
            FlowElementSnapshot::Table(_) => "table",
            FlowElementSnapshot::Frame(_) => "frame",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["block", "table", "block"],
        "interleaved order must survive the snapshot"
    );
}
