//! Complex mixed tests — tables, lists, frames, sub-frames, and blocks
//! interacting together in multi-step editing scenarios.

extern crate text_document_editing as document_editing;
use anyhow::Result;

use document_editing::document_editing_controller;
use document_editing::{
    AddBlockToListDto, CreateListDto, DeleteTextDto, InsertBlockDto, InsertFrameDto,
    InsertImageDto, InsertListDto, InsertMarkdownAtPositionDto, InsertTableColumnDto,
    InsertTableDto, InsertTableRowDto, InsertTextDto, ListStyle, MergeTableCellsDto,
    RemoveBlockFromListDto, RemoveTableColumnDto, RemoveTableDto, RemoveTableRowDto,
    SplitTableCellDto,
};

use test_harness::{
    DocumentRelationshipField, RootRelationshipField, block_controller, block_text_dto,
    document_controller, export_text, frame_controller, get_all_block_ids, get_block_ids,
    get_document_stats, get_element_ids, get_sorted_cells, get_table_ids,
    inline_element_controller, root_controller, setup_with_text, table_controller,
};

use test_harness::list_controller;

// ═══════════════════════════════════════════════════════════════════
// Complex mixed tests — tables, lists, frames, sub-frames, blocks
// ═══════════════════════════════════════════════════════════════════

/// Create a document with text, insert a table, add a list, then undo
/// all operations in reverse order.
#[test]
fn test_table_and_list_in_same_document() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Title\nContent")?;

    // Insert a table at the end
    let _table_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 13, // after "Content"
            anchor: 13,
            rows: 2,
            columns: 2,
        },
    )?;

    // Create a list on the "Content" block
    let list_result = document_editing_controller::create_list(
        &db,
        &hub,
        &mut urm,
        None,
        &CreateListDto {
            position: 6,
            anchor: 13,
            style: ListStyle::Disc,
        },
    )?;

    // Verify table exists and list exists
    assert_eq!(get_table_ids(&db)?.len(), 1);

    let block_ids = get_block_ids(&db)?;
    let content_block = block_ids
        .iter()
        .find_map(|id| {
            let b = block_controller::get(&db, id).ok()??;
            if block_text_dto(&db, &b) == "Content" {
                Some(b)
            } else {
                None
            }
        })
        .expect("Content block not found");
    assert_eq!(content_block.list, Some(list_result.list_id as u64));

    // Undo create_list
    urm.undo(None)?;
    let content_block = block_controller::get(&db, &content_block.id)?.unwrap();
    assert!(content_block.list.is_none());

    // Undo insert_table
    urm.undo(None)?;
    assert_eq!(get_table_ids(&db)?.len(), 0);

    let text = export_text(&db, &hub)?;
    assert_eq!(text, "Title\nContent");

    Ok(())
}

/// Insert a frame, then insert a table, then add list items.
/// Verify all structures coexist correctly.
#[test]
fn test_frame_with_table_and_list() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Before frame")?;

    // Insert a sub-frame at position 0
    let _frame_result = document_editing_controller::insert_frame(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertFrameDto {
            position: 0,
            anchor: 0,
        },
    )?;
    assert!(_frame_result.frame_id > 0);

    // After inserting frame, the document should have 2 frames
    let root_rels = root_controller::get_relationship(&db, &1, &RootRelationshipField::Document)?;
    let doc_id = root_rels[0];
    let frame_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Frames)?;
    assert_eq!(frame_ids.len(), 2);

    // Insert a 2x2 table at the end of the document
    let text = export_text(&db, &hub)?;
    let end_pos = text.len() as i64;
    let _table_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: end_pos,
            anchor: end_pos,
            rows: 2,
            columns: 2,
        },
    )?;

    assert_eq!(get_table_ids(&db)?.len(), 1);

    // Undo table
    urm.undo(None)?;
    assert_eq!(get_table_ids(&db)?.len(), 0);

    // Undo frame
    urm.undo(None)?;
    let frame_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Frames)?;
    assert_eq!(frame_ids.len(), 1);

    Ok(())
}

/// Insert multiple blocks, create different lists, add/remove blocks to lists.
#[test]
fn test_multiple_lists_with_add_remove() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("A\nB\nC\nD")?;
    // Layout: "A" at 0, "B" at 2, "C" at 4, "D" at 6

    // Create a bullet list on A
    let list1 = document_editing_controller::create_list(
        &db,
        &hub,
        &mut urm,
        None,
        &CreateListDto {
            position: 0,
            anchor: 0,
            style: ListStyle::Disc,
        },
    )?;

    // Create a numbered list on C
    let list2 = document_editing_controller::create_list(
        &db,
        &hub,
        &mut urm,
        None,
        &CreateListDto {
            position: 4,
            anchor: 4,
            style: ListStyle::Decimal,
        },
    )?;

    let block_ids = get_block_ids(&db)?;

    // Add B to list1
    document_editing_controller::add_block_to_list(
        &db,
        &hub,
        &mut urm,
        None,
        &AddBlockToListDto {
            block_id: block_ids[1] as i64,
            list_id: list1.list_id,
        },
    )?;

    // Add D to list2
    document_editing_controller::add_block_to_list(
        &db,
        &hub,
        &mut urm,
        None,
        &AddBlockToListDto {
            block_id: block_ids[3] as i64,
            list_id: list2.list_id,
        },
    )?;

    // Verify: A and B in list1, C and D in list2
    let a = block_controller::get(&db, &block_ids[0])?.unwrap();
    let b = block_controller::get(&db, &block_ids[1])?.unwrap();
    let c = block_controller::get(&db, &block_ids[2])?.unwrap();
    let d = block_controller::get(&db, &block_ids[3])?.unwrap();

    assert_eq!(a.list, Some(list1.list_id as u64));
    assert_eq!(b.list, Some(list1.list_id as u64));
    assert_eq!(c.list, Some(list2.list_id as u64));
    assert_eq!(d.list, Some(list2.list_id as u64));

    // Remove B from list1
    document_editing_controller::remove_block_from_list(
        &db,
        &hub,
        &mut urm,
        None,
        &RemoveBlockFromListDto {
            block_id: block_ids[1] as i64,
        },
    )?;

    let b = block_controller::get(&db, &block_ids[1])?.unwrap();
    assert!(b.list.is_none());

    // List1 still exists (A is still in it)
    assert!(list_controller::get(&db, &(list1.list_id as u64))?.is_some());

    Ok(())
}

/// Insert a table, then add text in a cell, then insert a list in normal content.
#[test]
fn test_table_then_list_operations() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello\nWorld")?;

    // Insert 2x2 table at the end
    let _table_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 11, // after "World"
            anchor: 11,
            rows: 2,
            columns: 2,
        },
    )?;

    // Create a list on "Hello"
    let list_result = document_editing_controller::create_list(
        &db,
        &hub,
        &mut urm,
        None,
        &CreateListDto {
            position: 0,
            anchor: 4,
            style: ListStyle::Disc,
        },
    )?;

    // Verify both exist
    assert_eq!(get_table_ids(&db)?.len(), 1);
    let hello_block = get_block_ids(&db)?
        .iter()
        .find_map(|id| {
            let b = block_controller::get(&db, id).ok()??;
            if block_text_dto(&db, &b) == "Hello" {
                Some(b)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(hello_block.list, Some(list_result.list_id as u64));

    // Undo list
    urm.undo(None)?;
    let hello_block = block_controller::get(&db, &hello_block.id)?.unwrap();
    assert!(hello_block.list.is_none());

    // Undo table
    urm.undo(None)?;
    assert_eq!(get_table_ids(&db)?.len(), 0);

    let text = export_text(&db, &hub)?;
    assert_eq!(text, "Hello\nWorld");

    Ok(())
}

/// Insert markdown with a list, then insert a table, then undo both.
#[test]
fn test_markdown_list_then_table() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    // Insert markdown with bullet points
    let md = "- Item one\n- Item two\n- Item three";
    let md_result = document_editing_controller::insert_markdown_at_position(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertMarkdownAtPositionDto {
            position: 0,
            anchor: 0,
            markdown: md.to_string(),
        },
    )?;

    let text_after_md = export_text(&db, &hub)?;
    assert!(text_after_md.contains("Item one"));

    let stats = get_document_stats(&db)?;
    let block_count_after_md = stats.block_count;

    // Insert a table after the list
    let end_pos = md_result.new_position;
    let _table_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: end_pos,
            anchor: end_pos,
            rows: 2,
            columns: 2,
        },
    )?;

    let stats = get_document_stats(&db)?;
    assert_eq!(stats.block_count, block_count_after_md + 4); // 4 cell blocks

    // Undo table
    urm.undo(None)?;
    let stats = get_document_stats(&db)?;
    assert_eq!(stats.block_count, block_count_after_md);
    assert_eq!(get_table_ids(&db)?.len(), 0);

    // Undo markdown
    urm.undo(None)?;
    let text = export_text(&db, &hub)?;
    assert_eq!(text, "");

    Ok(())
}

/// Create a table, merge cells, split them, add a row, remove a column —
/// and verify the document remains consistent at each step.
#[test]
fn test_table_merge_split_row_column_sequence() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Before\nAfter")?;

    // Insert a 3x3 table
    let table_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 7, // after "Before\n"
            anchor: 7,
            rows: 3,
            columns: 3,
        },
    )?;
    let table_id = table_result.table_id;

    // Merge top-left 2x2
    let merge_result = document_editing_controller::merge_table_cells(
        &db,
        &hub,
        &mut urm,
        None,
        &MergeTableCellsDto {
            table_id,
            start_row: 0,
            start_column: 0,
            end_row: 1,
            end_column: 1,
        },
    )?;

    // 9 - 3 = 6 cells
    assert_eq!(get_sorted_cells(&db, &(table_id as u64))?.len(), 6);

    // Split the merged cell back
    document_editing_controller::split_table_cell(
        &db,
        &hub,
        &mut urm,
        None,
        &SplitTableCellDto {
            cell_id: merge_result.merged_cell_id,
            split_rows: 2,
            split_columns: 2,
        },
    )?;

    // Back to 9 cells
    assert_eq!(get_sorted_cells(&db, &(table_id as u64))?.len(), 9);

    // Add a row
    document_editing_controller::insert_table_row(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableRowDto {
            table_id,
            row_index: 3,
        },
    )?;

    let table = table_controller::get(&db, &(table_id as u64))?.unwrap();
    assert_eq!(table.rows, 4);
    assert_eq!(get_sorted_cells(&db, &(table_id as u64))?.len(), 12);

    // Remove a column
    document_editing_controller::remove_table_column(
        &db,
        &hub,
        &mut urm,
        None,
        &RemoveTableColumnDto {
            table_id,
            column_index: 2,
        },
    )?;

    let table = table_controller::get(&db, &(table_id as u64))?.unwrap();
    assert_eq!(table.columns, 2);
    assert_eq!(get_sorted_cells(&db, &(table_id as u64))?.len(), 8);

    // Verify "Before" and "After" blocks still exist
    let text_blocks: Vec<_> = get_all_block_ids(&db)?
        .iter()
        .filter_map(|id| block_controller::get(&db, id).ok()?)
        .filter(|b| {
            let t = block_text_dto(&db, b);
            t == "Before" || t == "After"
        })
        .collect();
    assert_eq!(text_blocks.len(), 2);

    // Undo all 5 operations one by one
    for _ in 0..5 {
        urm.undo(None)?;
    }

    // Back to the original
    assert_eq!(get_table_ids(&db)?.len(), 0);
    let text = export_text(&db, &hub)?;
    assert_eq!(text, "Before\nAfter");

    Ok(())
}

/// Insert a frame into a document that has a list, verify both coexist.
#[test]
fn test_frame_insertion_with_existing_list() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Line one\nLine two")?;

    // Create list on both lines
    let list_result = document_editing_controller::create_list(
        &db,
        &hub,
        &mut urm,
        None,
        &CreateListDto {
            position: 0,
            anchor: 16,
            style: ListStyle::Decimal,
        },
    )?;

    // Insert a frame
    let _frame_result = document_editing_controller::insert_frame(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertFrameDto {
            position: 0,
            anchor: 0,
        },
    )?;

    // Document should have 2 frames
    let root_rels = root_controller::get_relationship(&db, &1, &RootRelationshipField::Document)?;
    let doc_id = root_rels[0];
    let frame_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Frames)?;
    assert_eq!(frame_ids.len(), 2);

    // Undo frame
    urm.undo(None)?;
    let frame_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Frames)?;
    assert_eq!(frame_ids.len(), 1);

    // List should still be intact on the blocks
    let block_ids = get_block_ids(&db)?;
    for bid in &block_ids {
        let b = block_controller::get(&db, bid)?.unwrap();
        assert_eq!(b.list, Some(list_result.list_id as u64));
    }

    // Undo list
    urm.undo(None)?;
    for bid in &block_ids {
        let b = block_controller::get(&db, bid)?.unwrap();
        assert!(b.list.is_none());
    }

    Ok(())
}

/// Insert text, insert a block, insert a table, insert more text in a block,
/// undo all — a full editing session simulation.
#[test]
fn test_full_editing_session() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Start")?;

    // Insert text at end
    document_editing_controller::insert_text(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 5,
            anchor: 5,
            text: " here".to_string(),
        },
    )?;
    assert_eq!(export_text(&db, &hub)?, "Start here");

    // Insert a new block
    let block_result = document_editing_controller::insert_block(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertBlockDto {
            position: 10,
            anchor: 10,
        },
    )?;

    // Insert text in the new block
    document_editing_controller::insert_text(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: block_result.new_position,
            anchor: block_result.new_position,
            text: "Second line".to_string(),
        },
    )?;
    assert_eq!(export_text(&db, &hub)?, "Start here\nSecond line");

    // Insert 2x2 table at the end
    let text_len = export_text(&db, &hub)?.len() as i64;
    document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: text_len,
            anchor: text_len,
            rows: 2,
            columns: 2,
        },
    )?;

    assert_eq!(get_table_ids(&db)?.len(), 1);

    // Create a list on the first block
    document_editing_controller::create_list(
        &db,
        &hub,
        &mut urm,
        None,
        &CreateListDto {
            position: 0,
            anchor: 9,
            style: ListStyle::Disc,
        },
    )?;

    // 5 operations total. Undo all one by one.
    for _ in 0..5 {
        urm.undo(None)?;
    }

    let text = export_text(&db, &hub)?;
    assert_eq!(text, "Start");
    assert_eq!(get_table_ids(&db)?.len(), 0);
    assert_eq!(get_document_stats(&db)?.block_count, 1);

    Ok(())
}

/// Insert a table, add a row, insert text in a cell block to verify
/// all block positions remain valid (non-negative and unique).
#[test]
fn test_block_positions_consistent_with_table_and_text() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Para one\nPara two")?;

    // Insert 2x3 table between paragraphs
    document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 9, // after "Para one\n"
            anchor: 9,
            rows: 2,
            columns: 3,
        },
    )?;

    // Add a row
    let table_ids = get_table_ids(&db)?;
    document_editing_controller::insert_table_row(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableRowDto {
            table_id: table_ids[0] as i64,
            row_index: 2,
        },
    )?;

    // Verify all block positions are non-negative and unique
    let all_block_ids = get_all_block_ids(&db)?;
    let mut positions: Vec<i64> = all_block_ids
        .iter()
        .filter_map(|id| block_controller::get(&db, id).ok()?)
        .map(|b| b.document_position)
        .collect();
    positions.sort();

    let unique: std::collections::HashSet<i64> = positions.iter().copied().collect();
    assert_eq!(
        unique.len(),
        positions.len(),
        "Duplicate positions found: {:?}",
        positions
    );
    for pos in &positions {
        assert!(*pos >= 0, "Negative position found: {}", pos);
    }

    Ok(())
}

/// Test that removing a table from a document that also has lists
/// doesn't corrupt the list relationships.
#[test]
fn test_remove_table_preserves_lists() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Item A\nItem B")?;

    // Create a list
    let list_result = document_editing_controller::create_list(
        &db,
        &hub,
        &mut urm,
        None,
        &CreateListDto {
            position: 0,
            anchor: 13,
            style: ListStyle::Disc,
        },
    )?;

    // Insert table
    let table_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 13,
            anchor: 13,
            rows: 2,
            columns: 2,
        },
    )?;

    // Remove the table
    document_editing_controller::remove_table(
        &db,
        &hub,
        &mut urm,
        None,
        &RemoveTableDto {
            table_id: table_result.table_id,
        },
    )?;

    assert_eq!(get_table_ids(&db)?.len(), 0);

    // Verify list is still intact
    let block_ids = get_block_ids(&db)?;
    for bid in &block_ids {
        let b = block_controller::get(&db, bid)?.unwrap();
        let t = block_text_dto(&db, &b);
        assert_eq!(
            b.list,
            Some(list_result.list_id as u64),
            "Block '{}' should still be in the list",
            t
        );
    }

    Ok(())
}

/// Test split_table_cell undo/redo preserves document position consistency.
#[test]
fn test_split_undo_redo_positions_consistent() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Before\nAfter")?;

    let table_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 7,
            anchor: 7,
            rows: 2,
            columns: 2,
        },
    )?;
    let table_id = table_result.table_id;

    // Merge all cells
    let merge_result = document_editing_controller::merge_table_cells(
        &db,
        &hub,
        &mut urm,
        None,
        &MergeTableCellsDto {
            table_id,
            start_row: 0,
            start_column: 0,
            end_row: 1,
            end_column: 1,
        },
    )?;

    // Split back to 2x2
    document_editing_controller::split_table_cell(
        &db,
        &hub,
        &mut urm,
        None,
        &SplitTableCellDto {
            cell_id: merge_result.merged_cell_id,
            split_rows: 2,
            split_columns: 2,
        },
    )?;

    // Verify positions are consistent
    let check_positions = |db: &common::database::db_context::DbContext| -> Result<()> {
        let all_ids = get_all_block_ids(db)?;
        let mut positions: Vec<i64> = all_ids
            .iter()
            .filter_map(|id| block_controller::get(db, id).ok()?)
            .map(|b| b.document_position)
            .collect();
        positions.sort();
        let unique: std::collections::HashSet<i64> = positions.iter().copied().collect();
        assert_eq!(unique.len(), positions.len(), "Duplicates: {:?}", positions);
        for p in &positions {
            assert!(*p >= 0, "Negative: {}", p);
        }
        Ok(())
    };

    check_positions(&db)?;

    // Undo split
    urm.undo(None)?;
    check_positions(&db)?;

    // Redo split
    urm.redo(None)?;
    check_positions(&db)?;

    Ok(())
}

/// Test: insert Markdown list, then add a table after the list, verify coexistence.
#[test]
fn test_markdown_list_coexists_with_table() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    // Insert markdown with a bullet list
    let md_result = document_editing_controller::insert_markdown_at_position(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertMarkdownAtPositionDto {
            position: 0,
            anchor: 0,
            markdown: "- Apple\n- Banana".to_string(),
        },
    )?;

    let text = export_text(&db, &hub)?;
    assert!(text.contains("Apple"));
    assert!(text.contains("Banana"));

    // Insert table
    let _table_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: md_result.new_position,
            anchor: md_result.new_position,
            rows: 2,
            columns: 2,
        },
    )?;

    // Both should coexist
    assert_eq!(get_table_ids(&db)?.len(), 1);
    let stats = get_document_stats(&db)?;
    assert!(stats.block_count >= 3); // at least 2 list items + empty first block rewritten + 4 cells

    // Undo table then markdown
    urm.undo(None)?;
    urm.undo(None)?;

    let text = export_text(&db, &hub)?;
    assert_eq!(text, "");

    Ok(())
}

/// Test: multi-step operations on a document with sub-frame, table, and list.
/// Insert text → insert frame → insert table inside → add list outside → undo all.
#[test]
fn test_complex_frame_table_list_undo_sequence() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Doc start\nDoc end")?;

    // Step 1: Insert frame
    let frame_result = document_editing_controller::insert_frame(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertFrameDto {
            position: 9, // in the middle of "Doc start"
            anchor: 9,
        },
    )?;
    assert!(frame_result.frame_id > 0);

    // Step 2: Insert table at the end
    let text = export_text(&db, &hub)?;
    let end_pos = text.len() as i64;
    let _table_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: end_pos,
            anchor: end_pos,
            rows: 2,
            columns: 2,
        },
    )?;
    assert_eq!(get_table_ids(&db)?.len(), 1);

    // Step 3: Create a numbered list on "Doc start"
    let _list_result = document_editing_controller::create_list(
        &db,
        &hub,
        &mut urm,
        None,
        &CreateListDto {
            position: 0,
            anchor: 8,
            style: ListStyle::Decimal,
        },
    )?;

    // Verify: 2 frames, 1 table, list on first block
    let root_rels = root_controller::get_relationship(&db, &1, &RootRelationshipField::Document)?;
    let doc_id = root_rels[0];
    let frame_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Frames)?;
    assert!(frame_ids.len() >= 2);
    assert_eq!(get_table_ids(&db)?.len(), 1);

    // Undo all 3 operations
    urm.undo(None)?; // undo list
    urm.undo(None)?; // undo table
    urm.undo(None)?; // undo frame

    let frame_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Frames)?;
    assert_eq!(frame_ids.len(), 1);
    assert_eq!(get_table_ids(&db)?.len(), 0);

    let text = export_text(&db, &hub)?;
    assert_eq!(text, "Doc start\nDoc end");

    Ok(())
}

/// Test position consistency after a long sequence of mixed operations:
/// insert table → add row → add column → merge → split → remove row
#[test]
fn test_complex_table_operation_sequence_positions() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Alpha\nBeta\nGamma")?;

    // Insert 2x2 table after "Alpha"
    let table_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 6,
            anchor: 6,
            rows: 2,
            columns: 2,
        },
    )?;
    let table_id = table_result.table_id;

    // Add a row
    document_editing_controller::insert_table_row(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableRowDto {
            table_id,
            row_index: 2,
        },
    )?;

    // Add a column
    document_editing_controller::insert_table_column(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableColumnDto {
            table_id,
            column_index: 2,
        },
    )?;

    let table = table_controller::get(&db, &(table_id as u64))?.unwrap();
    assert_eq!(table.rows, 3);
    assert_eq!(table.columns, 3);
    assert_eq!(get_sorted_cells(&db, &(table_id as u64))?.len(), 9);

    // Merge top-left 2x2
    let merge_result = document_editing_controller::merge_table_cells(
        &db,
        &hub,
        &mut urm,
        None,
        &MergeTableCellsDto {
            table_id,
            start_row: 0,
            start_column: 0,
            end_row: 1,
            end_column: 1,
        },
    )?;

    assert_eq!(get_sorted_cells(&db, &(table_id as u64))?.len(), 6);

    // Split the merged cell
    document_editing_controller::split_table_cell(
        &db,
        &hub,
        &mut urm,
        None,
        &SplitTableCellDto {
            cell_id: merge_result.merged_cell_id,
            split_rows: 2,
            split_columns: 2,
        },
    )?;
    assert_eq!(get_sorted_cells(&db, &(table_id as u64))?.len(), 9);

    // Remove a row
    document_editing_controller::remove_table_row(
        &db,
        &hub,
        &mut urm,
        None,
        &RemoveTableRowDto {
            table_id,
            row_index: 0,
        },
    )?;

    let table = table_controller::get(&db, &(table_id as u64))?.unwrap();
    assert_eq!(table.rows, 2);
    assert_eq!(get_sorted_cells(&db, &(table_id as u64))?.len(), 6);

    // Verify all block positions are consistent
    let all_ids = get_all_block_ids(&db)?;
    let mut positions: Vec<i64> = all_ids
        .iter()
        .filter_map(|id| block_controller::get(&db, id).ok()?)
        .map(|b| b.document_position)
        .collect();
    positions.sort();
    let unique: std::collections::HashSet<i64> = positions.iter().copied().collect();
    assert_eq!(
        unique.len(),
        positions.len(),
        "Duplicate positions: {:?}",
        positions
    );
    for p in &positions {
        assert!(*p >= 0, "Negative position: {}", p);
    }

    // Verify text blocks are still accessible
    let text_blocks: Vec<_> = all_ids
        .iter()
        .filter_map(|id| block_controller::get(&db, id).ok()?)
        .map(|b| {
            let t = block_text_dto(&db, &b);
            (b, t)
        })
        .filter(|(_b, t)| !t.is_empty())
        .collect();
    let texts: Vec<&str> = text_blocks.iter().map(|(_b, t)| t.as_str()).collect();
    assert!(
        texts.contains(&"Alpha"),
        "Alpha should still exist, got: {:?}",
        texts
    );

    Ok(())
}

/// Verify that inserting a table into a document with an existing frame
/// doesn't corrupt sub-frame parent relationships.
#[test]
fn test_insert_table_preserves_frame_relationships() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello world")?;

    // Insert a sub-frame
    let frame_result = document_editing_controller::insert_frame(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertFrameDto {
            position: 5,
            anchor: 5,
        },
    )?;

    let root_rels = root_controller::get_relationship(&db, &1, &RootRelationshipField::Document)?;
    let doc_id = root_rels[0];
    let frame_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Frames)?;
    assert_eq!(frame_ids.len(), 2);

    // Verify sub-frame has parent
    let sub_frame =
        frame_controller::get(&db, &(frame_result.frame_id as u64))?.expect("Sub-frame not found");
    let parent_id = sub_frame
        .parent_frame
        .expect("Sub-frame should have parent");

    // Insert a table
    let text = export_text(&db, &hub)?;
    let end = text.len() as i64;
    document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: end,
            anchor: end,
            rows: 2,
            columns: 2,
        },
    )?;

    // Sub-frame parent relationship should be preserved
    let sub_frame = frame_controller::get(&db, &(frame_result.frame_id as u64))?.unwrap();
    assert_eq!(sub_frame.parent_frame, Some(parent_id));

    Ok(())
}

/// Build a complex document with every entity type (table, frame, sub-frame,
/// lists, blocks, image), select the entire range, delete, and verify that
/// nothing is left.
#[test]
fn test_delete_all_complex_document_leaves_nothing() -> Result<()> {
    // 3 blocks: "Line one" (pos 0), "Line two" (pos 9), "Line three" (pos 18)
    let (db, hub, mut urm) = setup_with_text("Line one\nLine two\nLine three")?;

    // 1. Insert a 2x2 table at the end
    let text = export_text(&db, &hub)?;
    let end = text.len() as i64;
    let _table_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: end,
            anchor: end,
            rows: 2,
            columns: 2,
        },
    )?;

    // 2. Insert a sub-frame at position 0
    let _frame_result = document_editing_controller::insert_frame(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertFrameDto {
            position: 0,
            anchor: 0,
        },
    )?;

    // 3. Create a bullet list on "Line one"
    let _list1 = document_editing_controller::create_list(
        &db,
        &hub,
        &mut urm,
        None,
        &CreateListDto {
            position: 0,
            anchor: 8,
            style: ListStyle::Disc,
        },
    )?;

    // 4. Insert a numbered list after "Line two" (creates a new block + list)
    let _list2 = document_editing_controller::insert_list(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertListDto {
            position: 17,
            anchor: 17,
            style: ListStyle::Decimal,
        },
    )?;

    // 5. Insert an image in "Line one"
    let _image = document_editing_controller::insert_image(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertImageDto {
            position: 4,
            anchor: 4,
            image_name: "photo.png".to_string(),
            width: 200,
            height: 150,
        },
    )?;

    // --- Pre-delete: verify all entity types exist ---
    let doc_id = root_controller::get_relationship(&db, &1, &RootRelationshipField::Document)?[0];

    let table_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Tables)?;
    assert_eq!(table_ids.len(), 1, "Should have 1 table");

    let frame_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Frames)?;
    assert!(frame_ids.len() > 1, "Should have multiple frames");

    let list_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Lists)?;
    assert_eq!(list_ids.len(), 2, "Should have 2 lists");

    let stats = get_document_stats(&db)?;
    assert!(stats.character_count > 0, "Should have content");
    assert!(
        stats.block_count > 3,
        "Should have more than initial 3 blocks"
    );

    // --- Compute document end position across all frames ---
    let all_bids = get_all_block_ids(&db)?;
    let mut max_pos = 0i64;
    let store = db.get_store();
    for bid in &all_bids {
        if let Some(b) = block_controller::get(&db, bid)? {
            let entity: common::entities::Block = b.clone().into();
            let block_end = b.document_position
                + common::database::rope_helpers::block_char_length(&entity, store);
            if block_end > max_pos {
                max_pos = block_end;
            }
        }
    }
    assert!(max_pos > 0, "Document should have content before delete");

    // --- Select all and delete ---
    document_editing_controller::delete_text(
        &db,
        &hub,
        &mut urm,
        None,
        &DeleteTextDto {
            position: 0,
            anchor: max_pos,
        },
    )?;

    // --- Post-delete: verify nothing is left ---
    let stats = get_document_stats(&db)?;
    assert_eq!(stats.character_count, 0, "All text should be deleted");
    assert_eq!(stats.block_count, 1, "Only one empty block should remain");

    // Tables must be removed
    let table_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Tables)?;
    assert_eq!(table_ids.len(), 0, "All tables should be removed");

    // Only root frame should remain
    let frame_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Frames)?;
    assert_eq!(frame_ids.len(), 1, "Only root frame should remain");

    // Lists should be cleaned up
    let list_ids =
        document_controller::get_relationship(&db, &doc_id, &DocumentRelationshipField::Lists)?;
    assert_eq!(list_ids.len(), 0, "All lists should be removed");

    // No image anchors should remain
    let remaining_bids = get_all_block_ids(&db)?;
    for bid in &remaining_bids {
        let elem_ids = get_element_ids(&db, bid)?;
        for eid in &elem_ids {
            let elem = inline_element_controller::get(&db, eid)?.expect("Element should exist");
            assert!(
                !matches!(
                    elem.content,
                    common::format_runs::InlineContent::Image { .. }
                ),
                "No image elements should remain"
            );
        }
    }

    // Remaining block should be empty
    for bid in &remaining_bids {
        let b = block_controller::get(&db, bid)?.expect("Block should exist");
        assert_eq!(
            block_text_dto(&db, &b),
            "",
            "Remaining block should have no text"
        );
    }

    Ok(())
}
