//! Fragment-paste tests with the cursor inside a blockquote.
//!
//! Regression coverage: `insert_table_fragment` used to hard-wire the
//! root frame as the anchor's parent, so a table pasted at a cursor
//! inside a blockquote escaped the quote.

extern crate text_document_editing as document_editing;
use anyhow::Result;
use common::types::EntityId;

use test_harness::{
    DbContext, FrameDto, frame_controller, get_block_ids, get_frame_id, setup_with_text,
};

use document_editing::document_editing_controller;
use document_editing::{InsertFragmentDto, WrapBlocksInFrameDto};

fn get_frame(db: &DbContext, id: EntityId) -> FrameDto {
    frame_controller::get(db, &id)
        .expect("frame_controller::get failed")
        .expect("frame not found")
}

fn sub_frame_ids(frame: &FrameDto) -> Vec<EntityId> {
    frame
        .child_order
        .iter()
        .filter(|&&e| e < 0)
        .map(|&e| (-e) as EntityId)
        .collect()
}

/// Minimal one-cell-per-position 1x2 table fragment (tables-only payload).
fn make_table_fragment() -> String {
    let cell = |row: usize, column: usize, text: &str| {
        serde_json::json!({
            "row": row,
            "column": column,
            "row_span": 1,
            "column_span": 1,
            "blocks": [{
                "plain_text": text,
                "elements": [],
                "heading_level": null,
                "list": null,
                "alignment": null,
                "indent": null,
                "text_indent": null,
                "marker": null,
                "top_margin": null,
                "bottom_margin": null,
                "left_margin": null,
                "right_margin": null,
                "tab_positions": []
            }]
        })
    };
    serde_json::json!({
        "blocks": [],
        "tables": [{
            "rows": 1,
            "columns": 2,
            "cells": [cell(0, 0, "c1"), cell(0, 1, "c2")]
        }]
    })
    .to_string()
}

/// Wrap the whole (single-block) document in a blockquote frame and
/// return the blockquote frame's id.
fn wrap_document_in_blockquote(
    db: &DbContext,
    ev: &std::sync::Arc<test_harness::EventHub>,
    undo: &mut common::undo_redo::UndoRedoManager,
) -> Result<EntityId> {
    let block_ids = get_block_ids(db)?;
    let first = *block_ids.first().expect("document has a block") as i64;
    let last = *block_ids.last().expect("document has a block") as i64;
    let result = document_editing_controller::wrap_blocks_in_frame(
        db,
        ev,
        undo,
        None,
        &WrapBlocksInFrameDto {
            start_block_id: first,
            end_block_id: last,
            position: None,
            top_margin: None,
            bottom_margin: None,
            left_margin: None,
            right_margin: None,
            padding: None,
            border: None,
            is_blockquote: Some(true),
        },
    )?;
    Ok(result.new_frame_id as EntityId)
}

#[test]
fn test_paste_table_inside_blockquote_anchors_to_quote_frame() -> Result<()> {
    let (db, ev, mut undo) = setup_with_text("Hello world")?;
    let bq_id = wrap_document_in_blockquote(&db, &ev, &mut undo)?;

    // Cursor inside the quoted paragraph.
    document_editing_controller::insert_fragment(
        &db,
        &ev,
        &mut undo,
        None,
        &InsertFragmentDto {
            position: 5,
            anchor: 5,
            fragment_data: make_table_fragment(),
        },
    )?;

    let bq = get_frame(&db, bq_id);
    assert_eq!(bq.fmt_is_blockquote, Some(true));
    let anchors = sub_frame_ids(&bq);
    assert_eq!(
        anchors.len(),
        1,
        "table anchor must land in the blockquote frame, bq child_order: {:?}",
        bq.child_order
    );
    let anchor = get_frame(&db, anchors[0]);
    assert!(anchor.table.is_some(), "anchor links to the table");
    assert_eq!(anchor.parent_frame, Some(bq_id));

    // And the root frame holds only the blockquote — no stray anchor.
    let root = get_frame(&db, get_frame_id(&db)?);
    let root_subs = sub_frame_ids(&root);
    assert_eq!(
        root_subs,
        vec![bq_id],
        "root child_order: {:?}",
        root.child_order
    );
    Ok(())
}

#[test]
fn test_paste_table_outside_blockquote_still_anchors_to_root() -> Result<()> {
    let (db, ev, mut undo) = setup_with_text("Hello world")?;

    document_editing_controller::insert_fragment(
        &db,
        &ev,
        &mut undo,
        None,
        &InsertFragmentDto {
            position: 5,
            anchor: 5,
            fragment_data: make_table_fragment(),
        },
    )?;

    let root = get_frame(&db, get_frame_id(&db)?);
    let anchors = sub_frame_ids(&root);
    assert_eq!(anchors.len(), 1, "root child_order: {:?}", root.child_order);
    let anchor = get_frame(&db, anchors[0]);
    assert!(anchor.table.is_some());
    assert_eq!(anchor.parent_frame, Some(root.id));
    Ok(())
}

#[test]
fn test_paste_table_inside_blockquote_undo_restores() -> Result<()> {
    let (db, ev, mut undo) = setup_with_text("Hello world")?;
    let bq_id = wrap_document_in_blockquote(&db, &ev, &mut undo)?;
    let child_order_before = get_frame(&db, bq_id).child_order.clone();

    document_editing_controller::insert_fragment(
        &db,
        &ev,
        &mut undo,
        None,
        &InsertFragmentDto {
            position: 5,
            anchor: 5,
            fragment_data: make_table_fragment(),
        },
    )?;
    assert_ne!(get_frame(&db, bq_id).child_order, child_order_before);

    undo.undo(None)?;
    assert_eq!(
        get_frame(&db, bq_id).child_order,
        child_order_before,
        "undo must restore the blockquote's child_order"
    );
    Ok(())
}
