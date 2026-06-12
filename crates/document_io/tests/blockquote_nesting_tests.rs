//! Import/export tests for tables and lists nested inside blockquotes.
//!
//! Regression coverage for the blockquote-depth gap: tables used to lose
//! their blockquote context during import (the frame stack was driven only
//! by block elements), so `> | a | b |` landed in the root frame and a
//! table *after* a quote was swallowed into it.

extern crate text_document_io as document_io;
use anyhow::Result;
use common::long_operation::{LongOperationManager, OperationStatus};
use common::types::EntityId;

use test_harness::{frame_controller, get_frame_id, get_table_ids, setup};

use document_io::document_io_controller;
use document_io::*;

fn wait_for_long_operation(long_op_manager: &LongOperationManager, op_id: &str) {
    while let Some(OperationStatus::Running) = long_op_manager.get_operation_status(op_id) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn import_markdown(db: &test_harness::DbContext, ev: &std::sync::Arc<test_harness::EventHub>, md: &str) -> Result<()> {
    let mut long_op_manager = LongOperationManager::new();
    let op_id = document_io_controller::import_markdown(
        db,
        ev,
        &mut long_op_manager,
        &ImportMarkdownDto {
            markdown_text: md.to_string(),
        },
    )?;
    wait_for_long_operation(&long_op_manager, &op_id);
    assert_eq!(
        long_op_manager.get_operation_status(&op_id),
        Some(OperationStatus::Completed),
        "markdown import did not complete"
    );
    Ok(())
}

fn import_html(db: &test_harness::DbContext, ev: &std::sync::Arc<test_harness::EventHub>, html: &str) -> Result<()> {
    let mut long_op_manager = LongOperationManager::new();
    let op_id = document_io_controller::import_html(
        db,
        ev,
        &mut long_op_manager,
        &ImportHtmlDto {
            html_text: html.to_string(),
        },
    )?;
    wait_for_long_operation(&long_op_manager, &op_id);
    assert_eq!(
        long_op_manager.get_operation_status(&op_id),
        Some(OperationStatus::Completed),
        "html import did not complete"
    );
    Ok(())
}

/// Sub-frame ids referenced by `child_order` (negative entries), in order.
fn sub_frame_ids(frame: &test_harness::FrameDto) -> Vec<EntityId> {
    frame
        .child_order
        .iter()
        .filter(|&&e| e < 0)
        .map(|&e| (-e) as EntityId)
        .collect()
}

/// Block ids referenced by `child_order` (positive entries), in order.
fn block_entries(frame: &test_harness::FrameDto) -> Vec<EntityId> {
    frame
        .child_order
        .iter()
        .filter(|&&e| e > 0)
        .map(|&e| e as EntityId)
        .collect()
}

fn get_frame(db: &test_harness::DbContext, id: EntityId) -> test_harness::FrameDto {
    frame_controller::get(db, &id)
        .expect("frame_controller::get failed")
        .expect("frame not found")
}

// ─── Markdown import ────────────────────────────────────────────────

#[test]
fn test_md_import_table_first_in_blockquote() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_markdown(&db, &ev, "> | a | b |\n> |---|---|\n> | c | d |")?;

    let root = get_frame(&db, get_frame_id(&db)?);
    let root_subs = sub_frame_ids(&root);
    assert_eq!(
        root_subs.len(),
        1,
        "root should contain exactly the blockquote sub-frame, child_order: {:?}",
        root.child_order
    );
    assert!(block_entries(&root).is_empty(), "no loose blocks at root");

    let bq = get_frame(&db, root_subs[0]);
    assert_eq!(bq.fmt_is_blockquote, Some(true), "sub-frame is a blockquote");
    assert!(bq.table.is_none(), "blockquote frame is not a table anchor");

    let bq_subs = sub_frame_ids(&bq);
    assert_eq!(bq_subs.len(), 1, "blockquote contains the table anchor");
    let anchor = get_frame(&db, bq_subs[0]);
    assert!(anchor.table.is_some(), "anchor frame links to the table");
    assert_eq!(anchor.parent_frame, Some(bq.id));

    assert_eq!(get_table_ids(&db)?.len(), 1);
    Ok(())
}

#[test]
fn test_md_import_text_then_table_in_blockquote() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_markdown(&db, &ev, "> Some text\n>\n> | a | b |\n> |---|---|\n> | c | d |")?;

    let root = get_frame(&db, get_frame_id(&db)?);
    let root_subs = sub_frame_ids(&root);
    assert_eq!(root_subs.len(), 1);

    let bq = get_frame(&db, root_subs[0]);
    assert_eq!(bq.fmt_is_blockquote, Some(true));
    assert_eq!(block_entries(&bq).len(), 1, "one paragraph block in quote");
    let bq_subs = sub_frame_ids(&bq);
    assert_eq!(bq_subs.len(), 1, "one table anchor in quote");
    // Document order: block first, then the anchor.
    assert!(bq.child_order[0] > 0 && bq.child_order[1] < 0);
    Ok(())
}

#[test]
fn test_md_import_table_after_blockquote_closes() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_markdown(&db, &ev, "> Para\n\n| a | b |\n|---|---|\n| c | d |")?;

    let root = get_frame(&db, get_frame_id(&db)?);
    let root_subs = sub_frame_ids(&root);
    assert_eq!(
        root_subs.len(),
        2,
        "root holds the blockquote AND the table anchor, child_order: {:?}",
        root.child_order
    );

    let bq = get_frame(&db, root_subs[0]);
    assert_eq!(bq.fmt_is_blockquote, Some(true));
    assert!(
        sub_frame_ids(&bq).is_empty(),
        "the table must NOT be swallowed into the blockquote"
    );
    assert_eq!(block_entries(&bq).len(), 1);

    let anchor = get_frame(&db, root_subs[1]);
    assert!(anchor.table.is_some(), "second sub-frame is the table anchor");
    assert_eq!(anchor.parent_frame, Some(root.id));
    Ok(())
}

#[test]
fn test_md_import_table_in_nested_blockquote() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_markdown(&db, &ev, ">> | a | b |\n>> |---|---|\n>> | c | d |")?;

    let root = get_frame(&db, get_frame_id(&db)?);
    let depth1_ids = sub_frame_ids(&root);
    assert_eq!(depth1_ids.len(), 1);
    let depth1 = get_frame(&db, depth1_ids[0]);
    assert_eq!(depth1.fmt_is_blockquote, Some(true));

    let depth2_ids = sub_frame_ids(&depth1);
    assert_eq!(depth2_ids.len(), 1);
    let depth2 = get_frame(&db, depth2_ids[0]);
    assert_eq!(depth2.fmt_is_blockquote, Some(true));

    let anchor_ids = sub_frame_ids(&depth2);
    assert_eq!(anchor_ids.len(), 1);
    assert!(get_frame(&db, anchor_ids[0]).table.is_some());
    Ok(())
}

#[test]
fn test_md_import_list_in_blockquote() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_markdown(&db, &ev, "> - item1\n> - item2")?;

    let root = get_frame(&db, get_frame_id(&db)?);
    let root_subs = sub_frame_ids(&root);
    assert_eq!(root_subs.len(), 1);

    let bq = get_frame(&db, root_subs[0]);
    assert_eq!(bq.fmt_is_blockquote, Some(true));
    let items = block_entries(&bq);
    assert_eq!(items.len(), 2, "both list items live in the quote");

    // Both blocks must belong to the same list entity.
    let mut list_ids = Vec::new();
    for bid in &items {
        let rel = test_harness::block_controller::get_relationship(
            &db,
            bid,
            &test_harness::BlockRelationshipField::List,
        )?;
        assert_eq!(rel.len(), 1, "list item block has a List relationship");
        list_ids.push(rel[0]);
    }
    assert_eq!(list_ids[0], list_ids[1], "items grouped into one list");
    Ok(())
}

#[test]
fn test_md_import_mixed_list_and_table_in_blockquote() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_markdown(
        &db,
        &ev,
        "> - item1\n>\n> | a | b |\n> |---|---|\n> | c | d |",
    )?;

    let root = get_frame(&db, get_frame_id(&db)?);
    let root_subs = sub_frame_ids(&root);
    assert_eq!(root_subs.len(), 1);

    let bq = get_frame(&db, root_subs[0]);
    assert_eq!(bq.fmt_is_blockquote, Some(true));
    assert_eq!(block_entries(&bq).len(), 1, "list item block in quote");
    let anchors = sub_frame_ids(&bq);
    assert_eq!(anchors.len(), 1, "table anchor in quote");
    assert!(get_frame(&db, anchors[0]).table.is_some());
    // Document order preserved: block before table.
    assert!(bq.child_order[0] > 0 && bq.child_order[1] < 0);
    Ok(())
}

// ─── HTML import ────────────────────────────────────────────────────

#[test]
fn test_html_import_table_in_blockquote() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_html(
        &db,
        &ev,
        "<blockquote><table><tr><th>H</th></tr><tr><td>C</td></tr></table></blockquote>",
    )?;

    let root = get_frame(&db, get_frame_id(&db)?);
    let root_subs = sub_frame_ids(&root);
    assert_eq!(root_subs.len(), 1, "child_order: {:?}", root.child_order);

    let bq = get_frame(&db, root_subs[0]);
    assert_eq!(bq.fmt_is_blockquote, Some(true));
    let anchors = sub_frame_ids(&bq);
    assert_eq!(anchors.len(), 1);
    let anchor = get_frame(&db, anchors[0]);
    assert!(anchor.table.is_some());
    assert_eq!(anchor.parent_frame, Some(bq.id));
    Ok(())
}

#[test]
fn test_html_import_table_after_blockquote() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_html(
        &db,
        &ev,
        "<blockquote><p>Para</p></blockquote><table><tr><td>X</td></tr></table>",
    )?;

    let root = get_frame(&db, get_frame_id(&db)?);
    let root_subs = sub_frame_ids(&root);
    assert_eq!(root_subs.len(), 2, "child_order: {:?}", root.child_order);

    let bq = get_frame(&db, root_subs[0]);
    assert_eq!(bq.fmt_is_blockquote, Some(true));
    assert!(sub_frame_ids(&bq).is_empty(), "table stays outside the quote");

    let anchor = get_frame(&db, root_subs[1]);
    assert!(anchor.table.is_some());
    assert_eq!(anchor.parent_frame, Some(root.id));
    Ok(())
}

#[test]
fn test_html_import_backfills_cell_frame_parent() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_html(&db, &ev, "<table><tr><td>A</td><td>B</td></tr></table>")?;

    let table_ids = get_table_ids(&db)?;
    assert_eq!(table_ids.len(), 1);
    let cells = test_harness::get_sorted_cells(&db, &table_ids[0])?;
    assert_eq!(cells.len(), 2);
    for cell in &cells {
        let cf_id = cell.cell_frame.expect("cell has a frame");
        let cf = get_frame(&db, cf_id);
        let parent = cf.parent_frame.expect("cell frame parent_frame is set");
        let anchor = get_frame(&db, parent);
        assert_eq!(
            anchor.table,
            Some(table_ids[0]),
            "cell frame parent is the table's anchor frame"
        );
    }
    Ok(())
}

#[test]
fn test_html_import_list_in_blockquote() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_html(
        &db,
        &ev,
        "<blockquote><ul><li>one</li><li>two</li></ul></blockquote>",
    )?;

    let root = get_frame(&db, get_frame_id(&db)?);
    let root_subs = sub_frame_ids(&root);
    assert_eq!(root_subs.len(), 1);
    let bq = get_frame(&db, root_subs[0]);
    assert_eq!(bq.fmt_is_blockquote, Some(true));
    assert_eq!(block_entries(&bq).len(), 2);
    Ok(())
}

// ─── Export round-trips ─────────────────────────────────────────────

#[test]
fn test_md_roundtrip_table_in_blockquote() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_markdown(&db, &ev, "> | a | b |\n> |---|---|\n> | c | d |")?;

    let result = document_io_controller::export_markdown(&db, &ev)?;
    let quoted_table_lines = result
        .markdown_text
        .lines()
        .filter(|l| l.trim_start().starts_with("> |"))
        .count();
    assert!(
        quoted_table_lines >= 3,
        "exported table rows must keep the quote prefix, got:\n{}",
        result.markdown_text
    );
    Ok(())
}

#[test]
fn test_md_roundtrip_list_in_blockquote() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_markdown(&db, &ev, "> - item1\n> - item2")?;

    let result = document_io_controller::export_markdown(&db, &ev)?;
    let quoted_items = result
        .markdown_text
        .lines()
        .filter(|l| l.trim_start().starts_with("> -"))
        .count();
    assert_eq!(
        quoted_items, 2,
        "exported list items must keep the quote prefix, got:\n{}",
        result.markdown_text
    );
    Ok(())
}

#[test]
fn test_html_roundtrip_table_in_blockquote() -> Result<()> {
    let (db, ev, _) = setup()?;
    import_markdown(&db, &ev, "> | a | b |\n> |---|---|\n> | c | d |")?;

    let result = document_io_controller::export_html(&db, &ev)?;
    let html = result.html_text;
    let bq_start = html.find("<blockquote>").expect("blockquote in export");
    let bq_end = html.find("</blockquote>").expect("blockquote closes");
    let table_pos = html.find("<table").expect("table in export");
    assert!(
        bq_start < table_pos && table_pos < bq_end,
        "table must be inside the blockquote, got:\n{html}"
    );
    Ok(())
}
