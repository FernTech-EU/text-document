extern crate text_document_editing as document_editing;
use anyhow::Result;

use test_harness::{export_text, get_block_ids, get_document_stats, setup_with_text};

use document_editing::document_editing_controller;
use document_editing::*;

// ═══════════════════════════════════════════════════════════════════
// InsertText error paths
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_insert_text_empty_string_is_noop() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_text(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 3,
            anchor: 3,
            text: "".to_string(),
        },
    )?;

    assert_eq!(result.new_position, 3);
    assert_eq!(export_text(&db, &hub)?, "Hello");

    Ok(())
}

#[test]
fn test_insert_text_at_beyond_document_end_clamps() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello")?;

    // Position way beyond document end (doc has 5 chars). Backend
    // contract is to clamp to the end and append.
    let result = document_editing_controller::insert_text(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 999,
            anchor: 999,
            text: "!".to_string(),
        },
    )?;

    // Text must be appended at the real end, not stored at a virtual offset.
    assert_eq!(export_text(&db, &hub)?, "Hello!");
    let stats = get_document_stats(&db)?;
    assert_eq!(stats.character_count, 6);
    // new_position must reflect the clamped insertion location (right
    // after the appended "!"), not `999 + 1`.
    assert_eq!(result.new_position, 6);

    Ok(())
}

#[test]
fn test_delete_text_empty_range_is_noop() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello")?;

    let result = document_editing_controller::delete_text(
        &db,
        &hub,
        &mut urm,
        None,
        &DeleteTextDto {
            position: 3,
            anchor: 3,
        },
    )?;

    assert_eq!(result.new_position, 3);
    assert_eq!(result.deleted_text, "");
    assert_eq!(export_text(&db, &hub)?, "Hello");

    Ok(())
}

#[test]
fn test_delete_all_text_leaves_one_block() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello\nWorld")?;

    // Select all: positions 0..12 (5 + 1 separator + 5 = 11, but max_pos = 11)
    document_editing_controller::delete_text(
        &db,
        &hub,
        &mut urm,
        None,
        &DeleteTextDto {
            position: 0,
            anchor: 11,
        },
    )?;

    let stats = get_document_stats(&db)?;
    assert!(
        stats.block_count >= 1,
        "Should have at least 1 block after deleting all"
    );
    assert_eq!(stats.character_count, 0);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
// InsertBlock error paths
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_insert_block_at_position_zero() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_block(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertBlockDto {
            position: 0,
            anchor: 0,
        },
    )?;

    assert!(result.new_block_id > 0);
    let stats = get_document_stats(&db)?;
    assert_eq!(stats.block_count, 2);

    // Undo should restore to 1 block
    urm.undo(None)?;
    let stats = get_document_stats(&db)?;
    assert_eq!(stats.block_count, 1);
    assert_eq!(export_text(&db, &hub)?, "Hello");

    Ok(())
}

#[test]
fn test_insert_block_in_empty_document() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    let result = document_editing_controller::insert_block(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertBlockDto {
            position: 0,
            anchor: 0,
        },
    )?;

    assert!(result.new_block_id > 0);
    let stats = get_document_stats(&db)?;
    assert_eq!(stats.block_count, 2);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
// Table operations — boundary conditions
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_insert_table_zero_dimensions_errors() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    let result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 0,
            anchor: 0,
            rows: 0,
            columns: 0,
        },
    );

    assert!(result.is_err(), "0x0 table should fail");

    Ok(())
}

#[test]
fn test_insert_table_negative_dimensions_errors() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    let result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 0,
            anchor: 0,
            rows: -1,
            columns: 2,
        },
    );

    assert!(result.is_err(), "Negative row count should fail");

    Ok(())
}

#[test]
fn test_remove_table_nonexistent_errors() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello")?;

    let result = document_editing_controller::remove_table(
        &db,
        &hub,
        &mut urm,
        None,
        &RemoveTableDto { table_id: 999999 },
    );

    assert!(result.is_err(), "Removing non-existent table should fail");

    Ok(())
}

#[test]
fn test_merge_cells_invalid_range_errors() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    let insert_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 0,
            anchor: 0,
            rows: 2,
            columns: 2,
        },
    )?;

    // Try to merge with start > end
    let result = document_editing_controller::merge_table_cells(
        &db,
        &hub,
        &mut urm,
        None,
        &MergeTableCellsDto {
            table_id: insert_result.table_id,
            start_row: 1,
            start_column: 1,
            end_row: 0,
            end_column: 0,
        },
    );

    assert!(result.is_err(), "Inverted merge range should fail");

    Ok(())
}

#[test]
fn test_merge_cells_out_of_bounds_errors() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    let insert_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 0,
            anchor: 0,
            rows: 2,
            columns: 2,
        },
    )?;

    // Try to merge beyond table bounds
    let result = document_editing_controller::merge_table_cells(
        &db,
        &hub,
        &mut urm,
        None,
        &MergeTableCellsDto {
            table_id: insert_result.table_id,
            start_row: 0,
            start_column: 0,
            end_row: 5,
            end_column: 5,
        },
    );

    assert!(result.is_err(), "Out-of-bounds merge should fail");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
// Row/column operations — boundary conditions
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_remove_last_row_errors_or_removes_table() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    let insert_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 0,
            anchor: 0,
            rows: 1,
            columns: 2,
        },
    )?;

    // Removing the only row should either error or remove the entire table
    let result = document_editing_controller::remove_table_row(
        &db,
        &hub,
        &mut urm,
        None,
        &RemoveTableRowDto {
            table_id: insert_result.table_id,
            row_index: 0,
        },
    );

    // Either an error or row_count=0 is acceptable — but no panic
    if let Ok(r) = result {
        assert_eq!(r.new_row_count, 0);
    }

    Ok(())
}

#[test]
fn test_remove_last_column_errors_or_removes_table() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    let insert_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 0,
            anchor: 0,
            rows: 2,
            columns: 1,
        },
    )?;

    let result = document_editing_controller::remove_table_column(
        &db,
        &hub,
        &mut urm,
        None,
        &RemoveTableColumnDto {
            table_id: insert_result.table_id,
            column_index: 0,
        },
    );

    if let Ok(r) = result {
        assert_eq!(r.new_column_count, 0);
    }

    Ok(())
}

#[test]
fn test_remove_row_out_of_bounds_errors() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    let insert_result = document_editing_controller::insert_table(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertTableDto {
            position: 0,
            anchor: 0,
            rows: 2,
            columns: 2,
        },
    )?;

    let result = document_editing_controller::remove_table_row(
        &db,
        &hub,
        &mut urm,
        None,
        &RemoveTableRowDto {
            table_id: insert_result.table_id,
            row_index: 99,
        },
    );

    assert!(result.is_err(), "Removing out-of-bounds row should fail");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
// List operations — boundary conditions
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_add_block_to_nonexistent_list_errors() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello")?;

    let block_ids = get_block_ids(&db)?;

    let result = document_editing_controller::add_block_to_list(
        &db,
        &hub,
        &mut urm,
        None,
        &AddBlockToListDto {
            block_id: block_ids[0] as i64,
            list_id: 999999,
        },
    );

    assert!(
        result.is_err(),
        "Adding block to non-existent list should fail"
    );

    Ok(())
}

#[test]
fn test_remove_block_from_list_when_not_in_list() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello")?;

    let block_ids = get_block_ids(&db)?;

    let result = document_editing_controller::remove_block_from_list(
        &db,
        &hub,
        &mut urm,
        None,
        &RemoveBlockFromListDto {
            block_id: block_ids[0] as i64,
        },
    );

    // Should either error or be a no-op
    if let Ok(()) = result {
        // No-op is fine — block wasn't in a list
        assert_eq!(export_text(&db, &hub)?, "Hello");
    }

    Ok(())
}
