//! Complex mixed scenarios -- tables, lists, frames, images interacting
//! with multiple formatting operations and undo chains.

extern crate text_document_formatting as document_formatting;

use anyhow::Result;
use common::format_runs::InlineContent;

use document_formatting::document_formatting_controller;
use document_formatting::{Alignment, CellVerticalAlignment, ListStyle, SetBlockFormatDto, SetFrameFormatDto, SetListFormatDto, SetTableCellFormatDto, SetTableFormatDto, SetTextFormatDto, TextDirection};

use test_harness::{
    block_controller, create_list, export_text, frame_controller, get_all_block_ids, get_block_ids,
    get_frame_id, get_sorted_cells, insert_frame, insert_image, insert_table, list_controller,
    setup_with_text, table_cell_controller, table_controller,
};

#[test]
fn test_mixed_list_block_and_text_formatting() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Heading\nItem A\nItem B\nItem C")?;

    // create_list clears the undo stack, so call it before undoable formatting ops
    let list_result = create_list(
        &db,
        &hub,
        &mut urm,
        8,
        28,
        common::entities::ListStyle::Decimal,
    )?;

    document_formatting_controller::set_block_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetBlockFormatDto {
            position: 0,
            anchor: 7,
            heading_level: Some(1),
            alignment: Some(Alignment::Center),
            ..Default::default()
        },
    )?;

    document_formatting_controller::set_list_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetListFormatDto {
            list_id: list_result.list_id as i64,
            style: Some(ListStyle::UpperAlpha),
            indent: Some(2),
            prefix: None,
            suffix: Some(".".into()),
        },
    )?;

    document_formatting_controller::set_text_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetTextFormatDto {
            position: 8,
            anchor: 28,
            font_bold: Some(true),
            ..Default::default()
        },
    )?;

    let block_ids = get_block_ids(&db)?;
    let heading_block = block_controller::get(&db, &block_ids[0])?.unwrap();
    assert_eq!(heading_block.fmt_heading_level, Some(1));
    assert_eq!(
        heading_block.fmt_alignment,
        Some(common::entities::Alignment::Center)
    );

    let list = list_controller::get(&db, &list_result.list_id)?.unwrap();
    assert_eq!(list.style, common::entities::ListStyle::UpperAlpha);
    assert_eq!(list.indent, 2);
    assert_eq!(list.suffix, ".");

    for (i, bid) in block_ids.iter().enumerate().take(4).skip(1) {
        let elements = test_harness::synth_block_elements(&db, *bid)?;
        for elem in &elements {
            if let InlineContent::Text(ref t) = elem.content
                && !t.is_empty()
            {
                assert_eq!(
                    elem.fmt_font_bold,
                    Some(true),
                    "Text '{}' in block {} should be bold",
                    t,
                    i
                );
            }
        }
    }

    // Undo the 3 formatting operations (set_text_format, set_list_format, set_block_format)
    urm.undo(None)?;
    urm.undo(None)?;
    urm.undo(None)?;

    let heading_block = block_controller::get(&db, &block_ids[0])?.unwrap();
    assert_eq!(heading_block.fmt_heading_level, None);
    assert_eq!(heading_block.fmt_alignment, None);
    Ok(())
}

#[test]
fn test_mixed_table_with_formatted_cells() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Report")?;
    let table_result = insert_table(&db, &hub, &mut urm, 6, 2, 3)?;
    let table_id = table_result.table_id;

    document_formatting_controller::set_table_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetTableFormatDto {
            table_id: table_id as i64,
            border: Some(1),
            cell_padding: Some(5),
            width: Some(500),
            alignment: Some(Alignment::Center),
            ..Default::default()
        },
    )?;

    let cells = get_sorted_cells(&db, &table_id)?;
    for c in cells.iter().take(3) {
        document_formatting_controller::set_table_cell_format(
            &db,
            &hub,
            &mut urm,
            None,
            &SetTableCellFormatDto {
                cell_id: c.id as i64,
                background_color: Some("#333333".into()),
                vertical_alignment: Some(CellVerticalAlignment::Middle),
                ..Default::default()
            },
        )?;
    }

    for c in cells.iter().take(3) {
        let cell = table_cell_controller::get(&db, &c.id)?.unwrap();
        assert_eq!(cell.fmt_background_color, Some("#333333".into()));
        assert_eq!(
            cell.fmt_vertical_alignment,
            Some(common::entities::CellVerticalAlignment::Middle)
        );
    }
    for c in cells.iter().take(6).skip(3) {
        let cell = table_cell_controller::get(&db, &c.id)?.unwrap();
        assert_eq!(cell.fmt_background_color, None);
    }

    let table = table_controller::get(&db, &table_id)?.unwrap();
    assert_eq!(table.fmt_border, Some(1));
    assert_eq!(table.fmt_cell_padding, Some(5));
    Ok(())
}

#[test]
fn test_mixed_sub_frame_with_list_and_formatting() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Main text")?;
    let frame_result = insert_frame(&db, &hub, &mut urm, 9)?;
    let sub_frame_id = frame_result.frame_id;

    document_formatting_controller::set_frame_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetFrameFormatDto {
            position: 0,
            anchor: 0,
            frame_id: sub_frame_id as i64,
            is_blockquote: Some(true),
            left_margin: Some(20),
            padding: Some(10),
            border: Some(1),
            ..Default::default()
        },
    )?;

    let sub_frame = frame_controller::get(&db, &sub_frame_id)?.unwrap();
    assert_eq!(sub_frame.fmt_is_blockquote, Some(true));
    assert_eq!(sub_frame.fmt_left_margin, Some(20));
    assert_eq!(sub_frame.fmt_padding, Some(10));
    assert_eq!(sub_frame.fmt_border, Some(1));

    let main_frame_id = get_frame_id(&db)?;
    let main_frame = frame_controller::get(&db, &main_frame_id)?.unwrap();
    assert_eq!(main_frame.fmt_is_blockquote, None);
    Ok(())
}

#[test]
fn test_mixed_full_document_formatting_undo_chain() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Title\nParagraph one\nParagraph two")?;

    document_formatting_controller::set_block_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetBlockFormatDto {
            position: 0,
            anchor: 5,
            heading_level: Some(1),
            ..Default::default()
        },
    )?;
    document_formatting_controller::set_text_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetTextFormatDto {
            position: 0,
            anchor: 5,
            font_bold: Some(true),
            font_family: Some("Helvetica".into()),
            ..Default::default()
        },
    )?;
    document_formatting_controller::set_text_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetTextFormatDto {
            position: 16,
            anchor: 19,
            font_italic: Some(true),
            ..Default::default()
        },
    )?;

    let frame_id = get_frame_id(&db)?;
    document_formatting_controller::set_frame_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetFrameFormatDto {
            position: 0,
            anchor: 0,
            frame_id: frame_id as i64,
            padding: Some(20),
            ..Default::default()
        },
    )?;

    let block_ids = get_block_ids(&db)?;
    assert_eq!(
        block_controller::get(&db, &block_ids[0])?
            .unwrap()
            .fmt_heading_level,
        Some(1)
    );
    assert_eq!(
        frame_controller::get(&db, &frame_id)?.unwrap().fmt_padding,
        Some(20)
    );

    urm.undo(None)?;
    assert_eq!(
        frame_controller::get(&db, &frame_id)?.unwrap().fmt_padding,
        None
    );
    urm.undo(None)?;
    urm.undo(None)?;
    let title_elems = test_harness::synth_block_elements(&db, block_ids[0])?;
    assert_eq!(title_elems[0].fmt_font_bold, None);
    urm.undo(None)?;
    assert_eq!(
        block_controller::get(&db, &block_ids[0])?
            .unwrap()
            .fmt_heading_level,
        None
    );

    urm.redo(None)?;
    urm.redo(None)?;
    urm.redo(None)?;
    urm.redo(None)?;
    assert_eq!(
        block_controller::get(&db, &block_ids[0])?
            .unwrap()
            .fmt_heading_level,
        Some(1)
    );
    assert_eq!(
        frame_controller::get(&db, &frame_id)?.unwrap().fmt_padding,
        Some(20)
    );
    Ok(())
}

#[test]
fn test_mixed_table_and_list_same_document_formatting() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Header\nBullet A\nBullet B")?;
    let list_result = create_list(
        &db,
        &hub,
        &mut urm,
        7,
        24,
        common::entities::ListStyle::Disc,
    )?;
    let table_result = insert_table(&db, &hub, &mut urm, 24, 2, 2)?;

    let table_id = table_result.table_id;
    let list_id = list_result.list_id;

    document_formatting_controller::set_list_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetListFormatDto {
            list_id: list_id as i64,
            style: Some(ListStyle::Circle),
            indent: Some(1),
            prefix: Some("- ".into()),
            suffix: None,
        },
    )?;
    document_formatting_controller::set_table_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetTableFormatDto {
            table_id: table_id as i64,
            border: Some(2),
            alignment: Some(Alignment::Left),
            ..Default::default()
        },
    )?;
    document_formatting_controller::set_block_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetBlockFormatDto {
            position: 0,
            anchor: 6,
            heading_level: Some(2),
            alignment: Some(Alignment::Center),
            ..Default::default()
        },
    )?;

    assert_eq!(
        list_controller::get(&db, &list_id)?.unwrap().style,
        common::entities::ListStyle::Circle
    );
    assert_eq!(list_controller::get(&db, &list_id)?.unwrap().prefix, "- ");
    assert_eq!(
        table_controller::get(&db, &table_id)?.unwrap().fmt_border,
        Some(2)
    );
    assert_eq!(
        block_controller::get(&db, &get_block_ids(&db)?[0])?
            .unwrap()
            .fmt_heading_level,
        Some(2)
    );

    for _ in 0..3 {
        urm.undo(None)?;
    }
    assert_eq!(
        table_controller::get(&db, &table_id)?.unwrap().fmt_border,
        None,
        "Table format should be undone"
    );
    assert_eq!(
        list_controller::get(&db, &list_id)?.unwrap().style,
        common::entities::ListStyle::Disc,
        "List style should be reverted"
    );
    Ok(())
}

#[test]
fn test_mixed_text_format_with_image_and_list() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Start\nList item")?;
    insert_image(&db, &hub, &mut urm, 3, "icon.png", 16, 16)?;
    create_list(
        &db,
        &hub,
        &mut urm,
        7,
        16,
        common::entities::ListStyle::Disc,
    )?;

    document_formatting_controller::set_text_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetTextFormatDto {
            position: 0,
            anchor: 6,
            font_bold: Some(true),
            ..Default::default()
        },
    )?;

    let block_ids = get_block_ids(&db)?;
    let elements = test_harness::synth_block_elements(&db, block_ids[0])?;
    let mut text_bold_count = 0;
    let mut image_bold = false;
    for elem in &elements {
        if elem.fmt_font_bold == Some(true) {
            match &elem.content {
                InlineContent::Text(_) => text_bold_count += 1,
                InlineContent::Image { .. } => image_bold = true,
                _ => {}
            }
        }
    }
    assert!(
        text_bold_count > 0,
        "At least one text element should be bold"
    );
    assert!(image_bold, "Image should be bold when fully in range");

    let text = export_text(&db, &hub)?;
    assert!(text.starts_with("Sta"), "Text should start with 'Sta'");
    Ok(())
}

#[test]
fn test_format_blocks_across_multiple_frames() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Frame 1 content")?;
    insert_frame(&db, &hub, &mut urm, 15)?;

    let all_block_ids = get_all_block_ids(&db)?;
    assert!(
        all_block_ids.len() >= 2,
        "Should have blocks in both frames"
    );

    let mut min_pos = i64::MAX;
    let mut max_end = 0;
    let store = db.get_store();
    for bid in &all_block_ids {
        let b = block_controller::get(&db, bid)?.unwrap();
        min_pos = min_pos.min(b.document_position);
        let entity: common::entities::Block = b.clone().into();
        max_end = max_end.max(
            b.document_position + common::database::rope_helpers::block_char_length(&entity, store),
        );
    }

    document_formatting_controller::set_block_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetBlockFormatDto {
            position: min_pos,
            anchor: max_end,
            direction: Some(TextDirection::RightToLeft),
            clear_direction: false,
            ..Default::default()
        },
    )?;

    for bid in &all_block_ids {
        let b = block_controller::get(&db, bid)?.unwrap();
        assert_eq!(
            b.fmt_direction,
            Some(common::entities::TextDirection::RightToLeft),
            "Block at position {} should have RTL direction",
            b.document_position
        );
    }
    Ok(())
}
