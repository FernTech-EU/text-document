extern crate text_document_inspection as document_inspection;
use anyhow::Result;

use test_harness::setup_with_text;

use document_editing::document_editing_controller;
use document_editing::{InsertFrameDto, InsertTableDto, InsertTextDto};
use document_inspection::document_inspection_controller;
use document_inspection::{GetBlockAtPositionDto, GetTextAtPositionDto};

#[test]
fn test_get_document_stats_empty() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup_with_text("")?;

    let stats = document_inspection_controller::get_document_stats(&db_context, &event_hub)?;

    assert_eq!(stats.block_count, 1); // Even empty text creates one block
    assert_eq!(stats.character_count, 0);
    assert_eq!(stats.word_count, 0);
    assert_eq!(stats.frame_count, 1);
    assert_eq!(stats.image_count, 0);
    assert_eq!(stats.list_count, 0);

    Ok(())
}

#[test]
fn test_get_document_stats_after_import() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) =
        setup_with_text("Hello World\nSecond line\nThird line")?;

    let stats = document_inspection_controller::get_document_stats(&db_context, &event_hub)?;

    assert_eq!(stats.block_count, 3);
    // character_count = 11 + 11 + 10 = 32
    assert_eq!(stats.character_count, 32);
    // word_count: "Hello" "World" + "Second" "line" + "Third" "line" = 6
    assert_eq!(stats.word_count, 6);
    assert_eq!(stats.frame_count, 1);
    assert_eq!(stats.image_count, 0);
    assert_eq!(stats.list_count, 0);

    Ok(())
}

#[test]
fn test_get_text_at_position() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup_with_text("Hello World")?;

    let result = document_inspection_controller::get_text_at_position(
        &db_context,
        &event_hub,
        &GetTextAtPositionDto {
            position: 0,
            length: 5,
        },
    )?;

    assert_eq!(result.text, "Hello");
    assert!(result.block_id > 0);
    assert!(result.element_id > 0);

    Ok(())
}

#[test]
fn test_get_text_at_position_middle() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup_with_text("Hello World")?;

    let result = document_inspection_controller::get_text_at_position(
        &db_context,
        &event_hub,
        &GetTextAtPositionDto {
            position: 6,
            length: 5,
        },
    )?;

    assert_eq!(result.text, "World");

    Ok(())
}

#[test]
fn test_get_block_at_position_first_block() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup_with_text("First\nSecond\nThird")?;

    let result = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 0 },
    )?;

    assert_eq!(result.block_number, 0);
    assert_eq!(result.block_start, 0);
    assert_eq!(result.block_length, 5); // "First"
    assert!(result.block_id > 0);

    Ok(())
}

#[test]
fn test_get_block_at_position_second_block() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup_with_text("First\nSecond\nThird")?;

    // "First" is positions 0-4, block separator at 5, "Second" starts at 6
    let result = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 6 },
    )?;

    assert_eq!(result.block_number, 1);
    assert_eq!(result.block_start, 6);
    assert_eq!(result.block_length, 6); // "Second"

    Ok(())
}

#[test]
fn test_get_text_at_position_cross_block() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello\nWorld")?;

    // Block 0: "Hello" at pos 0, len 5
    // Block separator at pos 5
    // Block 1: "World" at pos 6, len 5
    // Starting at pos 3, length 6 should give "lo\nWor"
    let result = document_inspection_controller::get_text_at_position(
        &db_context,
        &event_hub,
        &GetTextAtPositionDto {
            position: 3,
            length: 6,
        },
    )?;

    assert_eq!(result.text, "lo\nWor");

    Ok(())
}

#[test]
fn test_get_text_at_position_end_of_document() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello")?;

    // Position at very end of document (position 5, length 0)
    let result = document_inspection_controller::get_text_at_position(
        &db_context,
        &event_hub,
        &GetTextAtPositionDto {
            position: 5,
            length: 0,
        },
    )?;

    assert_eq!(result.text, "");

    Ok(())
}

#[test]
fn test_get_document_stats_unicode() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("café résumé")?;

    let stats = document_inspection_controller::get_document_stats(&db_context, &event_hub)?;

    // "café résumé" = 11 characters (not 13 bytes)
    assert_eq!(stats.character_count, 11);
    assert_eq!(stats.word_count, 2);

    Ok(())
}

#[test]
fn test_get_block_at_position_separator() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("abc\ndef")?;
    // "abc" at pos 0-2 (len 3), separator at pos 3, "def" at pos 4-6 (len 3)

    // Position 2 = last char of first block ('c'). Should be in first block.
    let result = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 2 },
    )?;
    assert_eq!(result.block_number, 0);
    assert_eq!(result.block_start, 0);

    // Position 3 = block separator between "abc" and "def". Should land in second block.
    let result = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 3 },
    )?;
    assert_eq!(result.block_number, 1);
    assert_eq!(result.block_start, 4);

    // Position 4 = first char of second block ('d'). Should be in second block.
    let result = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 4 },
    )?;
    assert_eq!(result.block_number, 1);
    assert_eq!(result.block_start, 4);

    Ok(())
}

#[test]
fn test_get_text_at_position_truncated_at_end() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello")?;

    // Request more text than available
    let result = document_inspection_controller::get_text_at_position(
        &db_context,
        &event_hub,
        &GetTextAtPositionDto {
            position: 3,
            length: 100,
        },
    )?;

    assert_eq!(result.text, "lo"); // only 2 chars remaining

    Ok(())
}

// ─── Edge cases: length + past-end ──────────────────────────────

#[test]
fn test_get_text_at_position_negative_length() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello")?;

    // Negative length triggers the explicit early-return branch.
    let result = document_inspection_controller::get_text_at_position(
        &db_context,
        &event_hub,
        &GetTextAtPositionDto {
            position: 0,
            length: -1,
        },
    )?;

    assert_eq!(result.text, "");
    assert_eq!(result.block_id, 0);
    assert_eq!(result.element_id, 0);

    Ok(())
}

#[test]
fn test_get_text_at_position_one_past_end_errors() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hi")?;

    // Max valid position is 2 (end of "Hi"). Position 3 is past the
    // valid range — should propagate an out-of-range error.
    let result = document_inspection_controller::get_text_at_position(
        &db_context,
        &event_hub,
        &GetTextAtPositionDto {
            position: 3,
            length: 1,
        },
    );

    assert!(result.is_err(), "position 3 on a 2-char doc should fail");

    Ok(())
}

// ─── Table & sub-frame traversal (exercises collect_block_ids) ───

#[test]
fn test_get_block_at_position_in_table_cells() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hi")?;

    // Insert 2x2 table after "Hi".
    document_editing_controller::insert_table(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTableDto {
            position: 2,
            anchor: 2,
            rows: 2,
            columns: 2,
        },
    )?;

    // Document layout after insert (rope-canonical positions). The table
    // occupies a 1-char U+FFFC anchor sentinel between "Hi" and its cells, so
    // the cells start past that sentinel rather than immediately after "Hi":
    //   "Hi" block      : start=0, len=2  (block_number 0)
    //   (table anchor sentinel gap)
    //   cell(0,0) block : start=5, len=0  (block_number 1)
    //   cell(0,1) block : start=6, len=0  (block_number 2)
    //   cell(1,0) block : start=7, len=0  (block_number 3)
    //   cell(1,1) block : start=8, len=0  (block_number 4)

    // The position at the end of "Hi" (its trailing boundary) resolves to "Hi"
    // itself — the table sentinel now sits between "Hi" and the first cell.
    let at_hi_end = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 2 },
    )?;
    assert_eq!(at_hi_end.block_number, 0);
    assert_eq!(at_hi_end.block_start, 0);
    assert_eq!(at_hi_end.block_length, 2);

    // Exact cell(0,0) position.
    let at_cell_00 = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 5 },
    )?;
    assert_eq!(at_cell_00.block_number, 1);
    assert_eq!(at_cell_00.block_start, 5);
    assert_eq!(at_cell_00.block_length, 0);

    // Exact cell(0,1) position.
    let at_cell_01 = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 6 },
    )?;
    assert_eq!(at_cell_01.block_number, 2);
    assert_eq!(at_cell_01.block_start, 6);

    // Exact cell(1,1) position — confirms row-major traversal (block_number is
    // the document block-order index, excluding the table-anchor sentinel).
    let at_cell_11 = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 8 },
    )?;
    assert_eq!(at_cell_11.block_number, 4);
    assert_eq!(at_cell_11.block_start, 8);

    // The four cells are distinct blocks.
    let at_cell_10 = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 7 },
    )?;
    let mut cell_ids = vec![
        at_cell_00.block_id,
        at_cell_01.block_id,
        at_cell_10.block_id,
        at_cell_11.block_id,
    ];
    cell_ids.sort();
    cell_ids.dedup();
    assert_eq!(cell_ids.len(), 4, "cell blocks must be distinct");

    Ok(())
}

#[test]
fn test_get_text_at_position_spans_into_table_cell() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hi")?;

    document_editing_controller::insert_table(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTableDto {
            position: 2,
            anchor: 2,
            rows: 2,
            columns: 2,
        },
    )?;

    // Type "ab" into cell(0,0) at its starting position (3).
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            position: 3,
            anchor: 3,
            text: "ab".to_string(),
        },
    )?;

    // Now cell(0,0) has text_length=2 → later blocks shift by 2.
    // Running positions:
    //   "Hi"        : 0..2
    //   cell(0,0) ab: 3..5
    //   cell(0,1)   : 6..6
    //   cell(1,0)   : 7..7
    //   cell(1,1)   : 8..8

    // Read "i" + separator + "ab" crossing into the table's first cell —
    // exercises the cell traversal in collect_block_ids.
    let result = document_inspection_controller::get_text_at_position(
        &db_context,
        &event_hub,
        &GetTextAtPositionDto {
            position: 1,
            length: 4,
        },
    )?;
    assert_eq!(result.text, "i\nab");

    // Also verify typed text is readable purely within the cell.
    let in_cell = document_inspection_controller::get_text_at_position(
        &db_context,
        &event_hub,
        &GetTextAtPositionDto {
            position: 3,
            length: 2,
        },
    )?;
    assert_eq!(in_cell.text, "ab");

    Ok(())
}

#[test]
fn test_get_block_at_position_in_sub_frame() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Start")?;

    // Insert a sub-frame at position 5 (end of "Start"). This creates
    // a nested frame containing one empty block.
    document_editing_controller::insert_frame(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFrameDto {
            position: 5,
            anchor: 5,
        },
    )?;

    // Running positions after the sub-frame's empty block is appended
    // at index 1 of the root frame's child_order:
    //   "Start" block          : start=0, len=5
    //   sub-frame empty block  : start=6, len=0

    // Position 5 is the separator → folds into the sub-frame's block.
    let at_sep = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 5 },
    )?;
    assert_eq!(at_sep.block_number, 1);
    assert_eq!(at_sep.block_start, 6);
    assert_eq!(at_sep.block_length, 0);

    // Position 6 is the exact start of the empty sub-frame block.
    let at_sub = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 6 },
    )?;
    assert_eq!(at_sub.block_number, 1);
    assert_eq!(at_sub.block_start, 6);
    assert_eq!(at_sub.block_id, at_sep.block_id);

    // The root "Start" block and the sub-frame's block are distinct.
    let at_root = document_inspection_controller::get_block_at_position(
        &db_context,
        &event_hub,
        &GetBlockAtPositionDto { position: 0 },
    )?;
    assert_eq!(at_root.block_number, 0);
    assert_ne!(at_root.block_id, at_sub.block_id);

    Ok(())
}
