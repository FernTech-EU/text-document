extern crate text_document_inspection as document_inspection;
use anyhow::Result;
use common::parser_tools::fragment_schema::FragmentData;

use test_harness::{
    block_controller, export_text, get_block_ids, get_element_ids, inline_element_controller,
    setup_with_text,
};

use document_editing::InsertFragmentDto;
use document_editing::document_editing_controller;

use document_inspection::ExtractFragmentDto;
use document_inspection::document_inspection_controller;

// ─── Extract Fragment Tests ──────────────────────────────────────────

#[test]
fn test_extract_fragment_full_block() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello World")?;

    let result = document_inspection_controller::extract_fragment(
        &db_context,
        &event_hub,
        &ExtractFragmentDto {
            position: 0,
            anchor: 11,
        },
    )?;

    assert_eq!(result.plain_text, "Hello World");

    // Verify fragment_data is valid JSON
    let fragment: FragmentData = serde_json::from_str(&result.fragment_data)?;
    assert_eq!(fragment.blocks.len(), 1);
    assert_eq!(fragment.blocks[0].plain_text, "Hello World");
    assert!(!fragment.blocks[0].elements.is_empty());

    Ok(())
}

#[test]
fn test_extract_fragment_partial_block() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello World")?;

    let result = document_inspection_controller::extract_fragment(
        &db_context,
        &event_hub,
        &ExtractFragmentDto {
            position: 2,
            anchor: 7,
        },
    )?;

    assert_eq!(result.plain_text, "llo W");

    let fragment: FragmentData = serde_json::from_str(&result.fragment_data)?;
    assert_eq!(fragment.blocks.len(), 1);
    assert_eq!(fragment.blocks[0].plain_text, "llo W");

    Ok(())
}

#[test]
fn test_extract_fragment_cross_block() -> Result<()> {
    // "First\nSecond\nThird"
    // Block 0: "First" pos 0-4
    // Block 1: "Second" pos 6-11
    // Block 2: "Third" pos 13-17
    let (db_context, event_hub, _) = setup_with_text("First\nSecond\nThird")?;

    // "First" pos 0..5, "Second" pos 6..12, "Third" pos 13..18
    // Positions are half-open: anchor=18 means up to but not including position 18
    let result = document_inspection_controller::extract_fragment(
        &db_context,
        &event_hub,
        &ExtractFragmentDto {
            position: 0,
            anchor: 18,
        },
    )?;

    let fragment: FragmentData = serde_json::from_str(&result.fragment_data)?;
    assert_eq!(fragment.blocks.len(), 3);
    assert_eq!(fragment.blocks[0].plain_text, "First");
    assert_eq!(fragment.blocks[1].plain_text, "Second");
    assert_eq!(fragment.blocks[2].plain_text, "Third");

    // plain_text should join blocks with newline
    assert_eq!(result.plain_text, "First\nSecond\nThird");

    Ok(())
}

#[test]
fn test_extract_fragment_empty_range() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello World")?;

    let result = document_inspection_controller::extract_fragment(
        &db_context,
        &event_hub,
        &ExtractFragmentDto {
            position: 5,
            anchor: 5,
        },
    )?;

    assert_eq!(result.plain_text, "");

    let fragment: FragmentData = serde_json::from_str(&result.fragment_data)?;
    assert!(fragment.blocks.is_empty());

    Ok(())
}

// ─── Insert Fragment (via extract roundtrip) Tests ──────────────────

#[test]
fn test_insert_fragment_roundtrip() -> Result<()> {
    // Set up source document
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello World")?;

    // Extract "World" (positions 6-11)
    let extract_result = document_inspection_controller::extract_fragment(
        &db_context,
        &event_hub,
        &ExtractFragmentDto {
            position: 6,
            anchor: 11,
        },
    )?;

    assert_eq!(extract_result.plain_text, "World");

    // Now set up a second document (reuse same db)
    // Insert at end of current document
    let insert_result = document_editing_controller::insert_fragment(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFragmentDto {
            position: 11,
            anchor: 11,
            fragment_data: extract_result.fragment_data,
        },
    )?;

    // Partial-block fragment is inline-only — merges without adding blocks
    assert_eq!(insert_result.blocks_added, 0);

    let text = export_text(&db_context, &event_hub)?;
    assert!(
        text.contains("World"),
        "Inserted text should contain 'World', got: {}",
        text
    );

    Ok(())
}

#[test]
fn test_insert_fragment_preserves_formatting() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Start")?;

    // Insert markdown with bold text
    document_editing_controller::insert_markdown_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &document_editing::InsertMarkdownAtPositionDto {
            position: 5,
            anchor: 5,
            markdown: "**bold text**".to_string(),
        },
    )?;

    // Find the position range containing "bold text"
    let text = export_text(&db_context, &event_hub)?;
    // text is now something like "Start\nbold text\n"
    // Find where "bold text" starts (convert byte offset to char position)
    let byte_offset = text.find("bold text").expect("should contain 'bold text'");
    let bold_start = text[..byte_offset].chars().count();
    let bold_end = bold_start + "bold text".chars().count();

    // Extract the bold fragment
    let extract_result = document_inspection_controller::extract_fragment(
        &db_context,
        &event_hub,
        &ExtractFragmentDto {
            position: bold_start as i64,
            anchor: bold_end as i64,
        },
    )?;

    assert_eq!(extract_result.plain_text, "bold text");

    // Verify the fragment contains bold formatting
    let fragment: FragmentData = serde_json::from_str(&extract_result.fragment_data)?;
    let has_bold = fragment
        .blocks
        .iter()
        .any(|b| b.elements.iter().any(|e| e.fmt_font_bold == Some(true)));
    assert!(
        has_bold,
        "Extracted fragment should contain bold formatting"
    );

    // Now insert this bold fragment at the end of the document
    let current_text = export_text(&db_context, &event_hub)?;
    let _end_pos = current_text.len();
    // Account for block separators in position calculation
    // Get last block position
    let block_ids = get_block_ids(&db_context)?;
    let last_block = block_controller::get(&db_context, block_ids.last().unwrap())?
        .expect("last block should exist");
    let last_entity: common::entities::Block = last_block.clone().into();
    let insert_pos = last_block.document_position
        + common::database::rope_helpers::block_char_length(&last_entity, db_context.get_store());

    document_editing_controller::insert_fragment(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFragmentDto {
            position: insert_pos,
            anchor: insert_pos,
            fragment_data: extract_result.fragment_data,
        },
    )?;

    // Verify bold formatting is preserved across the paste: count
    // occurrences of "bold text" that sit inside a bold-formatted
    // segment. Adjacent equal-format spans coalesce into one run /
    // one segment, so we count substring occurrences rather than
    // segment count.
    let block_ids = get_block_ids(&db_context)?;
    let mut bold_substring_count = 0;
    for block_id in &block_ids {
        let elem_ids = get_element_ids(&db_context, block_id)?;
        for elem_id in &elem_ids {
            let elem = inline_element_controller::get(&db_context, elem_id)?;
            if let Some(elem) = elem
                && let common::format_runs::InlineContent::Text(ref t) = elem.content
                && elem.fmt_font_bold == Some(true)
            {
                bold_substring_count += t.matches("bold text").count();
            }
        }
    }
    assert_eq!(
        bold_substring_count, 2,
        "Should have 2 bold-formatted 'bold text' occurrences \
         (original from markdown + pasted copy), got {}",
        bold_substring_count
    );

    Ok(())
}

#[test]
fn test_insert_fragment_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let original_text = export_text(&db_context, &event_hub)?;

    // Extract "Hello" and re-insert at end
    let extract_result = document_inspection_controller::extract_fragment(
        &db_context,
        &event_hub,
        &ExtractFragmentDto {
            position: 0,
            anchor: 5,
        },
    )?;

    document_editing_controller::insert_fragment(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFragmentDto {
            position: 5,
            anchor: 5,
            fragment_data: extract_result.fragment_data,
        },
    )?;

    let after_insert = export_text(&db_context, &event_hub)?;
    assert_ne!(
        after_insert, original_text,
        "Text should have changed after insert"
    );

    // Undo
    undo_redo_manager.undo(None)?;

    let after_undo = export_text(&db_context, &event_hub)?;
    assert_eq!(
        after_undo, original_text,
        "Text should be restored after undo"
    );

    Ok(())
}

#[test]
fn test_extract_insert_with_list() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Start")?;

    // Insert markdown with list
    document_editing_controller::insert_markdown_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &document_editing::InsertMarkdownAtPositionDto {
            position: 5,
            anchor: 5,
            markdown: "- item1\n- item2".to_string(),
        },
    )?;

    // Find blocks with lists
    let block_ids = get_block_ids(&db_context)?;
    let mut list_block_positions: Vec<(i64, i64)> = Vec::new();
    let store = db_context.get_store();
    for block_id in &block_ids {
        let block = block_controller::get(&db_context, block_id)?;
        if let Some(block) = block
            && block.list.is_some()
        {
            let entity: common::entities::Block = block.clone().into();
            list_block_positions.push((
                block.document_position,
                common::database::rope_helpers::block_char_length(&entity, store),
            ));
        }
    }
    // "- item1\n- item2" produces exactly 2 list blocks
    assert_eq!(
        list_block_positions.len(),
        2,
        "Should have exactly 2 list blocks"
    );

    // Extract the range covering the list items.
    // End position must extend past the last block's text to cross the
    // paragraph break (Word paragraph-mark rule: block formatting is only
    // captured when the selection crosses the gap).
    let first_pos = list_block_positions[0].0;
    let last = list_block_positions.last().unwrap();
    let end_pos = last.0 + last.1 + 1; // +1 to cross the paragraph break

    let extract_result = document_inspection_controller::extract_fragment(
        &db_context,
        &event_hub,
        &ExtractFragmentDto {
            position: first_pos,
            anchor: end_pos,
        },
    )?;

    // Verify the fragment contains list data
    let fragment: FragmentData = serde_json::from_str(&extract_result.fragment_data)?;
    let has_list = fragment.blocks.iter().any(|b| b.list.is_some());
    assert!(has_list, "Extracted fragment should contain list data");

    // Insert the fragment at the very end
    let all_block_ids = get_block_ids(&db_context)?;
    let last_block =
        block_controller::get(&db_context, all_block_ids.last().unwrap())?.expect("last block");
    let last_entity: common::entities::Block = last_block.clone().into();
    let insert_pos = last_block.document_position
        + common::database::rope_helpers::block_char_length(&last_entity, db_context.get_store());

    document_editing_controller::insert_fragment(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFragmentDto {
            position: insert_pos,
            anchor: insert_pos,
            fragment_data: extract_result.fragment_data,
        },
    )?;

    // Verify that the inserted blocks also have list associations
    let final_block_ids = get_block_ids(&db_context)?;
    let mut final_list_count = 0;
    for block_id in &final_block_ids {
        let block = block_controller::get(&db_context, block_id)?;
        if let Some(block) = block
            && block.list.is_some()
        {
            final_list_count += 1;
        }
    }
    // Should have more list blocks now (original + inserted copies)
    assert!(
        final_list_count >= list_block_positions.len() * 2,
        "Should have at least {} list blocks (original + copies), got {}",
        list_block_positions.len() * 2,
        final_list_count
    );

    Ok(())
}

#[test]
fn test_insert_empty_fragment_should_error() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let empty_fragment = serde_json::json!({ "blocks": [] }).to_string();

    let result = document_editing_controller::insert_fragment(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFragmentDto {
            position: 0,
            anchor: 0,
            fragment_data: empty_fragment,
        },
    );

    assert!(result.is_err(), "Inserting an empty fragment should fail");

    // Document should be unchanged
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello");

    Ok(())
}

#[test]
fn test_extract_insert_fragment_with_image() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Before")?;

    // Insert an image into the document
    use document_editing::InsertImageDto;
    document_editing_controller::insert_image(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertImageDto {
            position: 6,
            anchor: 6,
            image_name: "photo.png".to_string(),
            alt: String::new(),
            width: 200,
            height: 100,
            quality: 100,
        },
    )?;

    // Extract fragment containing the image (position 5..7 spans text end + image)
    let extract_result = document_inspection_controller::extract_fragment(
        &db_context,
        &event_hub,
        &ExtractFragmentDto {
            position: 5,
            anchor: 7,
        },
    )?;

    // Verify fragment_data contains image
    let fragment: common::parser_tools::fragment_schema::FragmentData =
        serde_json::from_str(&extract_result.fragment_data)?;
    let has_image = fragment.blocks.iter().any(|b| {
        b.elements
            .iter()
            .any(|e| matches!(e.content, common::format_runs::InlineContent::Image { .. }))
    });
    assert!(has_image, "Fragment should contain an image element");

    // Insert the extracted fragment elsewhere
    let result = document_editing_controller::insert_fragment(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFragmentDto {
            position: 0,
            anchor: 0,
            fragment_data: extract_result.fragment_data,
        },
    )?;

    // Partial-block image fragment is inline-only — merges without adding blocks
    assert_eq!(result.blocks_added, 0);

    Ok(())
}

#[test]
fn test_extract_fragment_multiple_formats() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    // Apply bold to "Hel" and italic to "lo"
    use document_formatting::document_formatting_controller;
    use document_formatting::{CharVerticalAlignment, SetTextFormatDto, UnderlineStyle};

    document_formatting_controller::set_text_format(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &SetTextFormatDto {
            position: 0,
            anchor: 3,
            font_family: Some("Arial".into()),
            font_point_size: Some(12),
            font_weight: Some(700),
            font_bold: Some(true),
            font_italic: Some(false),
            font_underline: Some(false),
            font_overline: Some(false),
            font_strikeout: Some(false),
            letter_spacing: Some(0),
            word_spacing: Some(0),
            underline_style: Some(UnderlineStyle::NoUnderline),
            vertical_alignment: Some(CharVerticalAlignment::Normal),
        },
    )?;

    document_formatting_controller::set_text_format(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &SetTextFormatDto {
            position: 3,
            anchor: 5,
            font_family: Some("Times".into()),
            font_point_size: Some(14),
            font_weight: Some(400),
            font_bold: Some(false),
            font_italic: Some(true),
            font_underline: Some(false),
            font_overline: Some(false),
            font_strikeout: Some(false),
            letter_spacing: Some(0),
            word_spacing: Some(0),
            underline_style: Some(UnderlineStyle::NoUnderline),
            vertical_alignment: Some(CharVerticalAlignment::Normal),
        },
    )?;

    // Extract the full block
    let extract_result = document_inspection_controller::extract_fragment(
        &db_context,
        &event_hub,
        &ExtractFragmentDto {
            position: 0,
            anchor: 5,
        },
    )?;

    // Verify fragment has multiple elements with different formats
    let fragment: common::parser_tools::fragment_schema::FragmentData =
        serde_json::from_str(&extract_result.fragment_data)?;
    assert!(!fragment.blocks.is_empty());
    let block = &fragment.blocks[0];

    let bold_count = block
        .elements
        .iter()
        .filter(|e| e.fmt_font_bold == Some(true))
        .count();
    let italic_count = block
        .elements
        .iter()
        .filter(|e| e.fmt_font_italic == Some(true))
        .count();

    // "Hel" is bold (1 element), "lo" is italic (1 element)
    assert_eq!(bold_count, 1, "Should have exactly one bold element");
    assert_eq!(italic_count, 1, "Should have exactly one italic element");

    // Roundtrip: insert elsewhere and verify formats preserved
    let (db2, eh2, mut urm2) = setup_with_text("Target")?;

    document_editing_controller::insert_fragment(
        &db2,
        &eh2,
        &mut urm2,
        None,
        &InsertFragmentDto {
            position: 6,
            anchor: 6,
            fragment_data: extract_result.fragment_data,
        },
    )?;

    // Verify bold and italic elements exist in target
    let block_ids = get_block_ids(&db2)?;
    let mut found_bold = false;
    let mut found_italic = false;
    for block_id in &block_ids {
        let elem_ids = get_element_ids(&db2, block_id)?;
        for elem_id in &elem_ids {
            let elem = inline_element_controller::get(&db2, elem_id)?.unwrap();
            if elem.fmt_font_bold == Some(true) {
                found_bold = true;
            }
            if elem.fmt_font_italic == Some(true) {
                found_italic = true;
            }
        }
    }
    assert!(
        found_bold,
        "Target should have bold element after fragment insert"
    );
    assert!(
        found_italic,
        "Target should have italic element after fragment insert"
    );

    Ok(())
}
