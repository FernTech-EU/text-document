//! Tests for `insert_djot_at_position` — pasting djot markup at the cursor.

extern crate text_document_editing as document_editing;
use anyhow::Result;

use document_editing::InsertDjotAtPositionDto;
use document_editing::document_editing_controller;
use test_harness::{export_text, get_document_stats, setup_with_text};

#[test]
fn paste_djot_into_empty_document() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    let result = document_editing_controller::insert_djot_at_position(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertDjotAtPositionDto {
            position: 0,
            anchor: 0,
            djot: "- one\n- two\n- three".to_string(),
        },
    )?;

    let text = export_text(&db, &hub)?;
    assert!(text.contains("one"), "text: {text}");
    assert!(text.contains("three"), "text: {text}");
    assert!(result.blocks_added >= 1);
    Ok(())
}

#[test]
fn paste_djot_inline_into_existing_text() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello World")?;

    // Insert a bold fragment between "Hello" and " World".
    document_editing_controller::insert_djot_at_position(
        &db,
        &hub,
        &mut urm,
        None,
        &InsertDjotAtPositionDto {
            position: 5,
            anchor: 5,
            djot: "*bold*".to_string(),
        },
    )?;

    let text = export_text(&db, &hub)?;
    assert!(text.contains("Hello"), "text: {text}");
    assert!(text.contains("bold"), "text: {text}");
    assert!(text.contains("World"), "text: {text}");
    Ok(())
}

#[test]
fn paste_djot_is_undoable() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("base")?;
    let before = get_document_stats(&db)?.block_count;

    document_editing_controller::insert_djot_at_position(
        &db,
        &hub,
        &mut urm,
        Some(0),
        &InsertDjotAtPositionDto {
            position: 4,
            anchor: 4,
            djot: "\n\n# Heading\n\nmore".to_string(),
        },
    )?;
    assert!(get_document_stats(&db)?.block_count > before);

    urm.undo(Some(0))?;
    assert_eq!(get_document_stats(&db)?.block_count, before);
    let text = export_text(&db, &hub)?;
    assert_eq!(text, "base");
    Ok(())
}
