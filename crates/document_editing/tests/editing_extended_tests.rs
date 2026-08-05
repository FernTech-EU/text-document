extern crate text_document_editing as document_editing;
use anyhow::Result;

use common::types::EntityId;
use test_harness::{
    DocumentRelationshipField, RootRelationshipField, block_controller, document_controller,
    export_text, frame_controller, get_block_ids, get_document_stats, get_first_block_element_ids,
    inline_element_controller, root_controller, setup_with_text,
};

use document_editing::document_editing_controller;
use document_editing::*;

// --- InsertFormattedText tests ---

#[test]
fn test_insert_formatted_text() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_formatted_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFormattedTextDto {
            position: 5,
            anchor: 5,
            text: " World".to_string(),
            font_family: "Arial".to_string(),
            font_point_size: 12,
            font_bold: true,
            font_italic: false,
            font_underline: false,
            font_strikeout: false,
        },
    )?;

    assert_eq!(result.new_position, 11);

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello World");

    // Verify the new element has formatting
    let elem_ids = get_first_block_element_ids(&db_context)?;
    let mut found_bold = false;
    for elem_id in &elem_ids {
        let elem =
            inline_element_controller::get(&db_context, elem_id)?.expect("Element not found");
        if elem.fmt_font_bold == Some(true) {
            found_bold = true;
            assert_eq!(elem.fmt_font_family, Some("Arial".to_string()));
            assert_eq!(elem.fmt_font_point_size, Some(12));
        }
    }
    assert!(found_bold, "Should find an element with bold formatting");

    Ok(())
}

#[test]
fn test_insert_formatted_text_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    document_editing_controller::insert_formatted_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFormattedTextDto {
            position: 5,
            anchor: 5,
            text: " Bold".to_string(),
            font_family: "Arial".to_string(),
            font_point_size: 14,
            font_bold: true,
            font_italic: false,
            font_underline: false,
            font_strikeout: false,
        },
    )?;

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello Bold");

    // Undo
    undo_redo_manager.undo(None)?;
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello");

    Ok(())
}

// --- InsertImage tests ---

#[test]
fn test_insert_image() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_image(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertImageDto {
            position: 5,
            anchor: 5,
            image_name: "test.png".to_string(),
            alt: String::new(),
            width: 100,
            height: 50,
            quality: 100,
        },
    )?;

    // Image occupies 1 character position
    assert_eq!(result.new_position, 6);
    assert!(result.element_id > 0);

    // Verify document stats show increased character count
    let stats = test_harness::get_document_stats(&db_context)?;
    // Original "Hello" = 5 chars + 1 image = 6
    assert_eq!(stats.character_count, 6);

    Ok(())
}

#[test]
fn test_insert_image_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    document_editing_controller::insert_image(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertImageDto {
            position: 3,
            anchor: 3,
            image_name: "photo.jpg".to_string(),
            alt: String::new(),
            width: 200,
            height: 150,
            quality: 100,
        },
    )?;

    let stats = test_harness::get_document_stats(&db_context)?;
    assert_eq!(stats.character_count, 6); // 5 + 1 image

    // Undo
    undo_redo_manager.undo(None)?;
    let stats = test_harness::get_document_stats(&db_context)?;
    assert_eq!(stats.character_count, 5); // back to original

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello");

    Ok(())
}

// --- CreateList tests ---

#[test]
fn test_create_list() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Item one")?;

    let result = document_editing_controller::create_list(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &CreateListDto {
            position: 0,
            anchor: 8,
            style: ListStyle::Disc,
        },
    )?;

    assert!(result.list_id > 0);

    // Verify the block now has a list reference
    let block_ids = get_block_ids(&db_context)?;
    let block = block_controller::get(&db_context, &block_ids[0])?.expect("Block not found");
    assert!(block.list.is_some(), "Block should have a list reference");
    assert_eq!(block.list.unwrap() as i64, result.list_id);

    Ok(())
}

#[test]
fn test_create_list_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Item one")?;

    document_editing_controller::create_list(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &CreateListDto {
            position: 0,
            anchor: 8,
            style: ListStyle::Decimal,
        },
    )?;

    // Verify block has list
    let block_ids = get_block_ids(&db_context)?;
    let block = block_controller::get(&db_context, &block_ids[0])?.expect("Block not found");
    assert!(block.list.is_some());

    // Undo
    undo_redo_manager.undo(None)?;

    let block_ids = get_block_ids(&db_context)?;
    let block = block_controller::get(&db_context, &block_ids[0])?.expect("Block not found");
    assert!(
        block.list.is_none(),
        "Block should not have a list after undo"
    );

    Ok(())
}

// --- InsertList tests ---

#[test]
fn test_insert_list() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_list(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertListDto {
            position: 5,
            anchor: 5,
            style: ListStyle::Disc,
        },
    )?;

    assert!(result.list_id > 0);

    // Verify block count increased
    let stats = test_harness::get_document_stats(&db_context)?;
    assert_eq!(stats.block_count, 2);

    Ok(())
}

// --- InsertFrame tests ---

#[test]
fn test_insert_frame() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_frame(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFrameDto {
            position: 0,
            anchor: 0,
        },
    )?;

    assert!(result.frame_id > 0);

    // Verify the document now has 2 frames
    let root = root_controller::get(&db_context, &1)?.expect("Root not found");
    let doc_ids =
        root_controller::get_relationship(&db_context, &root.id, &RootRelationshipField::Document)?;
    let frame_ids = document_controller::get_relationship(
        &db_context,
        &doc_ids[0],
        &DocumentRelationshipField::Frames,
    )?;
    assert_eq!(frame_ids.len(), 2);

    // Verify the new frame has parent_frame set to the root frame
    let new_frame = frame_controller::get(&db_context, &(result.frame_id as EntityId))?
        .expect("New frame not found");
    let root_frame_id = frame_ids[0]; // first frame is the root frame
    assert_eq!(
        new_frame.parent_frame,
        Some(root_frame_id),
        "New frame should have the root frame as parent"
    );

    // Verify the parent frame's child_order contains the new frame (as negative ID)
    let parent_frame =
        frame_controller::get(&db_context, &root_frame_id)?.expect("Parent frame not found");
    assert!(
        parent_frame.child_order.contains(&-(result.frame_id)),
        "Parent frame's child_order should contain -(new_frame_id)"
    );

    Ok(())
}

#[test]
fn test_insert_frame_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    document_editing_controller::insert_frame(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFrameDto {
            position: 0,
            anchor: 0,
        },
    )?;

    // Verify 2 frames
    let root = root_controller::get(&db_context, &1)?.expect("Root not found");
    let doc_ids =
        root_controller::get_relationship(&db_context, &root.id, &RootRelationshipField::Document)?;
    let frame_ids = document_controller::get_relationship(
        &db_context,
        &doc_ids[0],
        &DocumentRelationshipField::Frames,
    )?;
    assert_eq!(frame_ids.len(), 2);

    // Undo
    undo_redo_manager.undo(None)?;

    let frame_ids = document_controller::get_relationship(
        &db_context,
        &doc_ids[0],
        &DocumentRelationshipField::Frames,
    )?;
    assert_eq!(frame_ids.len(), 1, "Should be back to 1 frame after undo");

    // Text should still be intact
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello");

    Ok(())
}

// --- InsertFormattedText selection-replacement tests ---

#[test]
fn test_insert_formatted_text_replaces_selection() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello World")?;

    // Replace "World" (positions 6..11) with bold "Earth"
    let result = document_editing_controller::insert_formatted_text(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertFormattedTextDto {
            position: 6,
            anchor: 11,
            text: "Earth".to_string(),
            font_family: "Arial".to_string(),
            font_point_size: 12,
            font_bold: true,
            font_italic: false,
            font_underline: false,
            font_strikeout: false,
        },
    )?;

    assert_eq!(result.new_position, 11); // 6 + len("Earth")
    let text = export_text(&db, &hub)?;
    assert_eq!(text, "Hello Earth");

    // Verify bold formatting on the new element
    let elem_ids = get_first_block_element_ids(&db)?;
    let mut found_bold_earth = false;
    for elem_id in &elem_ids {
        let elem = inline_element_controller::get(&db, elem_id)?.unwrap();
        if let common::format_runs::InlineContent::Text(ref t) = elem.content
            && t.contains("Earth")
            && elem.fmt_font_bold == Some(true)
        {
            found_bold_earth = true;
        }
    }
    assert!(found_bold_earth, "Should find bold 'Earth' element");

    Ok(())
}

#[test]
fn test_insert_formatted_text_replaces_selection_undo() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello World")?;

    document_editing_controller::insert_formatted_text(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertFormattedTextDto {
            position: 6,
            anchor: 11,
            text: "Earth".to_string(),
            font_family: "".to_string(),
            font_point_size: 0,
            font_bold: false,
            font_italic: false,
            font_underline: false,
            font_strikeout: false,
        },
    )?;

    assert_eq!(export_text(&db, &hub)?, "Hello Earth");

    urm.undo(None)?;
    assert_eq!(export_text(&db, &hub)?, "Hello World");

    urm.redo(None)?;
    assert_eq!(export_text(&db, &hub)?, "Hello Earth");

    Ok(())
}

#[test]
fn test_insert_formatted_text_cross_block_selection_errors() -> Result<()> {
    // Cross-block selection replacement is not supported (same as insert_text)
    let (db, hub, mut urm) = setup_with_text("Hello\nWorld")?;

    let result = document_editing_controller::insert_formatted_text(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertFormattedTextDto {
            position: 3,
            anchor: 9,
            text: "XY".to_string(),
            font_family: "".to_string(),
            font_point_size: 0,
            font_bold: true,
            font_italic: false,
            font_underline: false,
            font_strikeout: false,
        },
    );

    assert!(result.is_err(), "Cross-block selection should fail");
    // Document should be unchanged
    assert_eq!(export_text(&db, &hub)?, "Hello\nWorld");

    Ok(())
}

// --- InsertImage edge cases ---

#[test]
fn test_insert_image_at_position_zero() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_image(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertImageDto {
            position: 0,
            anchor: 0,
            image_name: "start.png".to_string(),
            alt: String::new(),
            width: 50,
            height: 50,
            quality: 100,
        },
    )?;

    assert_eq!(result.new_position, 1);
    let stats = get_document_stats(&db)?;
    assert_eq!(stats.character_count, 6); // 1 image + 5 chars

    // Verify the image exists somewhere in the block's synthesized elements.
    let elements = test_harness::synth_first_block_elements(&db)?;
    let has_image = elements
        .iter()
        .any(|e| matches!(e.content, common::format_runs::InlineContent::Image { .. }));
    assert!(has_image, "Block should contain an image element");

    Ok(())
}

#[test]
fn test_insert_image_with_selection_errors() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello World")?;

    // Image insertion does not support selection replacement
    let result = document_editing_controller::insert_image(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertImageDto {
            position: 6,
            anchor: 11,
            image_name: "replaced.png".to_string(),
            alt: String::new(),
            width: 100,
            height: 100,
            quality: 100,
        },
    );

    assert!(result.is_err(), "Image insert with selection should fail");
    // Document should be unchanged
    assert_eq!(export_text(&db, &hub)?, "Hello World");

    Ok(())
}

// --- InsertFrame edge cases ---

#[test]
fn test_insert_frame_at_end_of_document() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_frame(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertFrameDto {
            position: 5,
            anchor: 5,
        },
    )?;

    assert!(result.frame_id > 0);

    // Verify 2 frames exist
    let root = root_controller::get(&db, &1)?.unwrap();
    let doc_ids =
        root_controller::get_relationship(&db, &root.id, &RootRelationshipField::Document)?;
    let frame_ids = document_controller::get_relationship(
        &db,
        &doc_ids[0],
        &DocumentRelationshipField::Frames,
    )?;
    assert_eq!(frame_ids.len(), 2);

    // Undo, redo
    urm.undo(None)?;
    let frame_ids = document_controller::get_relationship(
        &db,
        &doc_ids[0],
        &DocumentRelationshipField::Frames,
    )?;
    assert_eq!(frame_ids.len(), 1);

    urm.redo(None)?;
    let frame_ids = document_controller::get_relationship(
        &db,
        &doc_ids[0],
        &DocumentRelationshipField::Frames,
    )?;
    assert_eq!(frame_ids.len(), 2);

    Ok(())
}

#[test]
fn test_insert_frame_in_middle_of_text() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello World")?;

    let result = document_editing_controller::insert_frame(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertFrameDto {
            position: 5,
            anchor: 5,
        },
    )?;

    assert!(result.frame_id > 0);

    // Text should still contain both parts
    let text = export_text(&db, &hub)?;
    assert!(
        text.contains("Hello"),
        "Should still contain 'Hello', got: {}",
        text
    );
    assert!(
        text.contains("World"),
        "Should still contain 'World', got: {}",
        text
    );

    Ok(())
}

// --- InsertList edge cases ---

#[test]
fn test_insert_list_at_end_of_multiblock_document() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Line one\nLine two")?;

    // Insert list at end of document (after "Line two", pos = 8 + 1 + 8 = 17)
    let result = document_editing_controller::insert_list(
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

    assert!(result.list_id > 0);
    let stats = get_document_stats(&db)?;
    assert_eq!(stats.block_count, 3); // 2 original + 1 list block

    // Undo
    urm.undo(None)?;
    let stats = get_document_stats(&db)?;
    assert_eq!(stats.block_count, 2);
    assert_eq!(export_text(&db, &hub)?, "Line one\nLine two");

    Ok(())
}

#[test]
fn test_create_list_spanning_multiple_blocks() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("First\nSecond\nThird")?;

    // Select all text: "First" (0..5), sep, "Second" (6..12), sep, "Third" (13..18)
    let result = document_editing_controller::create_list(
        &db,
        &hub,
        &mut urm,
        None,
        &CreateListDto {
            position: 0,
            anchor: 18,
            style: ListStyle::Disc,
        },
    )?;

    assert!(result.list_id > 0);

    // All 3 blocks should be in the list
    let block_ids = get_block_ids(&db)?;
    assert_eq!(block_ids.len(), 3);
    for block_id in &block_ids {
        let block = block_controller::get(&db, block_id)?.unwrap();
        assert!(
            block.list.is_some(),
            "Block {} should have a list reference",
            block_id
        );
        assert_eq!(block.list.unwrap() as i64, result.list_id);
    }

    // Undo — all blocks should lose list reference
    urm.undo(None)?;
    for block_id in &block_ids {
        let block = block_controller::get(&db, block_id)?.unwrap();
        assert!(
            block.list.is_none(),
            "Block {} should have no list after undo",
            block_id
        );
    }

    Ok(())
}

// --- Combined operation tests ---

#[test]
fn test_insert_formatted_text_into_empty_document() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    let result = document_editing_controller::insert_formatted_text(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertFormattedTextDto {
            position: 0,
            anchor: 0,
            text: "Bold start".to_string(),
            font_family: "Courier".to_string(),
            font_point_size: 10,
            font_bold: true,
            font_italic: false,
            font_underline: false,
            font_strikeout: false,
        },
    )?;

    assert_eq!(result.new_position, 10);
    assert_eq!(export_text(&db, &hub)?, "Bold start");

    let stats = get_document_stats(&db)?;
    assert_eq!(stats.character_count, 10);
    assert_eq!(stats.block_count, 1);

    Ok(())
}

#[test]
fn test_insert_image_cross_block_selection_errors() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello\nWorld")?;

    // Image insertion does not support selection replacement
    let result = document_editing_controller::insert_image(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertImageDto {
            position: 3,
            anchor: 9,
            image_name: "bridge.png".to_string(),
            alt: String::new(),
            width: 200,
            height: 100,
            quality: 100,
        },
    );

    assert!(result.is_err(), "Image insert with selection should fail");
    assert_eq!(export_text(&db, &hub)?, "Hello\nWorld");

    Ok(())
}
