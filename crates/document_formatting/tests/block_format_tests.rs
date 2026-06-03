//! SetBlockFormat — all fields, lists, tables, sub-frames, clamping, undo/redo

extern crate text_document_formatting as document_formatting;

use anyhow::Result;

use document_formatting::document_formatting_controller;
use document_formatting::{Alignment, MarkerType, SetBlockFormatDto};

use test_harness::{
    FrameRelationshipField, block_controller, create_list, frame_controller, get_block_ids,
    get_sorted_cells, insert_table, setup_with_text,
};

#[test]
fn test_set_block_format_all_fields() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello World")?;

    document_formatting_controller::set_block_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetBlockFormatDto {
            position: 0,
            anchor: 5,
            alignment: Some(Alignment::Justify),
            heading_level: Some(3),
            indent: Some(2),
            marker: Some(MarkerType::Checked),
            line_height: Some(150),
            non_breakable_lines: Some(true),
            direction: Some(common::entities::TextDirection::RightToLeft),
            background_color: Some("#ff0000".into()),
            is_code_block: Some(true),
            code_language: Some("rust".into()),
            hyphenate: Some(true),
            language: Some("en".into()),
            top_margin: Some(10),
            bottom_margin: Some(20),
            left_margin: Some(30),
            right_margin: Some(40),
            text_indent: Some(15),
        },
    )?;

    let block_ids = get_block_ids(&db)?;
    let block = block_controller::get(&db, &block_ids[0])?.unwrap();

    assert_eq!(
        block.fmt_alignment,
        Some(common::entities::Alignment::Justify)
    );
    assert_eq!(block.fmt_heading_level, Some(3));
    assert_eq!(block.fmt_indent, Some(2));
    assert_eq!(
        block.fmt_marker,
        Some(common::entities::MarkerType::Checked)
    );
    assert_eq!(block.fmt_line_height, Some(150));
    assert_eq!(block.fmt_non_breakable_lines, Some(true));
    assert_eq!(
        block.fmt_direction,
        Some(common::entities::TextDirection::RightToLeft)
    );
    assert_eq!(block.fmt_background_color, Some("#ff0000".into()));
    assert_eq!(block.fmt_is_code_block, Some(true));
    assert_eq!(block.fmt_code_language, Some("rust".into()));
    assert_eq!(block.fmt_hyphenate, Some(true));
    assert_eq!(block.fmt_language, Some("en".into()));
    assert_eq!(block.fmt_top_margin, Some(10));
    assert_eq!(block.fmt_bottom_margin, Some(20));
    assert_eq!(block.fmt_left_margin, Some(30));
    assert_eq!(block.fmt_right_margin, Some(40));
    assert_eq!(block.fmt_text_indent, Some(15));
    Ok(())
}

#[test]
fn test_set_block_format_heading_level_clamping() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Title")?;

    // Heading level > 6 should be clamped to 6
    document_formatting_controller::set_block_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetBlockFormatDto {
            position: 0,
            anchor: 5,
            heading_level: Some(99),
            ..Default::default()
        },
    )?;

    let block_ids = get_block_ids(&db)?;
    let block = block_controller::get(&db, &block_ids[0])?.unwrap();
    assert_eq!(
        block.fmt_heading_level,
        Some(6),
        "Heading level should be clamped to 6"
    );

    // Heading level < 0 should be clamped to 0
    document_formatting_controller::set_block_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetBlockFormatDto {
            position: 0,
            anchor: 5,
            heading_level: Some(-5),
            ..Default::default()
        },
    )?;

    let block = block_controller::get(&db, &block_ids[0])?.unwrap();
    assert_eq!(
        block.fmt_heading_level,
        Some(0),
        "Heading level should be clamped to 0"
    );
    Ok(())
}

#[test]
fn test_set_block_format_on_list_items() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("First\nSecond\nThird")?;

    // Create list spanning all blocks
    let list_result = create_list(
        &db,
        &hub,
        &mut urm,
        0,
        18,
        common::entities::ListStyle::Decimal,
    )?;

    // Format only the second list item (position 6..12 = "Second")
    document_formatting_controller::set_block_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetBlockFormatDto {
            position: 6,
            anchor: 12,
            alignment: Some(Alignment::Center),
            indent: Some(2),
            marker: Some(MarkerType::Unchecked),
            ..Default::default()
        },
    )?;

    let block_ids = get_block_ids(&db)?;

    // First block should not be formatted
    let block_0 = block_controller::get(&db, &block_ids[0])?.unwrap();
    assert_eq!(block_0.fmt_alignment, None);
    assert_eq!(block_0.list, Some(list_result.list_id));

    // Second block should be formatted
    let block_1 = block_controller::get(&db, &block_ids[1])?.unwrap();
    assert_eq!(
        block_1.fmt_alignment,
        Some(common::entities::Alignment::Center)
    );
    assert_eq!(block_1.fmt_indent, Some(2));
    assert_eq!(
        block_1.fmt_marker,
        Some(common::entities::MarkerType::Unchecked)
    );
    assert_eq!(block_1.list, Some(list_result.list_id));

    // Third block should not be formatted
    let block_2 = block_controller::get(&db, &block_ids[2])?.unwrap();
    assert_eq!(block_2.fmt_alignment, None);
    Ok(())
}

#[test]
fn test_set_block_format_in_table_cell() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Before")?;

    let table_result = insert_table(&db, &hub, &mut urm, 6, 2, 2)?;

    let cells = get_sorted_cells(&db, &table_result.table_id)?;
    let cell_0_frame = cells[0].cell_frame.unwrap();
    let cell_block_ids =
        frame_controller::get_relationship(&db, &cell_0_frame, &FrameRelationshipField::Blocks)?;
    let cell_block = block_controller::get(&db, &cell_block_ids[0])?.unwrap();
    let cell_pos = cell_block.document_position;

    // Format the cell's block as a code block
    document_formatting_controller::set_block_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetBlockFormatDto {
            position: cell_pos,
            anchor: cell_pos,
            is_code_block: Some(true),
            code_language: Some("python".into()),
            hyphenate: Some(true),
            language: Some("fr".into()),
            background_color: Some("#1e1e1e".into()),
            ..Default::default()
        },
    )?;

    let cell_block_after = block_controller::get(&db, &cell_block_ids[0])?.unwrap();
    assert_eq!(cell_block_after.fmt_is_code_block, Some(true));
    assert_eq!(cell_block_after.fmt_code_language, Some("python".into()));
    assert_eq!(cell_block_after.fmt_hyphenate, Some(true));
    assert_eq!(cell_block_after.fmt_language, Some("fr".into()));
    assert_eq!(
        cell_block_after.fmt_background_color,
        Some("#1e1e1e".into())
    );

    // Verify "Before" block is unaffected
    let main_block_ids = get_block_ids(&db)?;
    let main_block = block_controller::get(&db, &main_block_ids[0])?.unwrap();
    assert_eq!(main_block.fmt_is_code_block, None);
    Ok(())
}

#[test]
fn test_set_block_format_undo_redo() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Block A\nBlock B")?;

    document_formatting_controller::set_block_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetBlockFormatDto {
            position: 0,
            anchor: 15,
            alignment: Some(Alignment::Right),
            heading_level: Some(2),
            ..Default::default()
        },
    )?;

    let block_ids = get_block_ids(&db)?;

    // Verify applied
    for bid in &block_ids {
        let b = block_controller::get(&db, bid)?.unwrap();
        assert_eq!(b.fmt_alignment, Some(common::entities::Alignment::Right));
    }

    // Undo
    urm.undo(None)?;
    for bid in &block_ids {
        let b = block_controller::get(&db, bid)?.unwrap();
        assert_eq!(b.fmt_alignment, None);
    }

    // Redo
    urm.redo(None)?;
    for bid in &block_ids {
        let b = block_controller::get(&db, bid)?.unwrap();
        assert_eq!(b.fmt_alignment, Some(common::entities::Alignment::Right));
    }
    Ok(())
}

#[test]
fn test_set_block_format_preserves_unset_fields() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Content")?;

    // First, set heading
    document_formatting_controller::set_block_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetBlockFormatDto {
            position: 0,
            anchor: 7,
            heading_level: Some(1),
            ..Default::default()
        },
    )?;

    // Then set alignment without heading — heading should be preserved
    document_formatting_controller::set_block_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetBlockFormatDto {
            position: 0,
            anchor: 7,
            alignment: Some(Alignment::Center),
            ..Default::default()
        },
    )?;

    let block_ids = get_block_ids(&db)?;
    let block = block_controller::get(&db, &block_ids[0])?.unwrap();
    assert_eq!(
        block.fmt_heading_level,
        Some(1),
        "Heading should be preserved"
    );
    assert_eq!(
        block.fmt_alignment,
        Some(common::entities::Alignment::Center),
        "Alignment should be set"
    );
    Ok(())
}
