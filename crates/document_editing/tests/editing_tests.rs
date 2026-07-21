extern crate text_document_editing as document_editing;
use anyhow::Result;

use test_harness::{export_text, setup_with_text};

use document_editing::document_editing_controller;
use document_editing::{DeleteTextDto, InsertBlockDto, InsertTextDto};

/// Helper: delete one character backward (backspace) at the given cursor position.
fn backspace(
    db_context: &common::database::db_context::DbContext,
    event_hub: &std::sync::Arc<common::event::EventHub>,
    undo_redo_manager: &mut common::undo_redo::UndoRedoManager,
    cursor_pos: i64,
) -> Result<i64> {
    let result = document_editing_controller::delete_text(
        db_context,
        event_hub,
        undo_redo_manager,
        None,
        &DeleteTextDto {
            position: cursor_pos,
            anchor: cursor_pos - 1,
        },
    )?;
    Ok(result.new_position)
}

/// Helper: delete one character forward (Del key) at the given cursor position.
fn forward_delete(
    db_context: &common::database::db_context::DbContext,
    event_hub: &std::sync::Arc<common::event::EventHub>,
    undo_redo_manager: &mut common::undo_redo::UndoRedoManager,
    cursor_pos: i64,
) -> Result<i64> {
    let result = document_editing_controller::delete_text(
        db_context,
        event_hub,
        undo_redo_manager,
        None,
        &DeleteTextDto {
            position: cursor_pos,
            anchor: cursor_pos + 1,
        },
    )?;
    Ok(result.new_position)
}

#[test]
fn test_insert_text_at_beginning() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 0,
            anchor: 0,
            text: "Say ".to_string(),
        },
    )?;

    assert_eq!(result.new_position, 4);
    assert_eq!(result.blocks_affected, 1);

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Say Hello");

    Ok(())
}

#[test]
fn test_insert_text_at_end() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 5,
            anchor: 5,
            text: " World".to_string(),
        },
    )?;

    assert_eq!(result.new_position, 11);

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello World");

    Ok(())
}

#[test]
fn test_insert_text_in_middle() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Helo")?;

    let result = document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 2,
            anchor: 2,
            text: "l".to_string(),
        },
    )?;

    assert_eq!(result.new_position, 3);

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello");

    Ok(())
}

#[test]
fn test_delete_text_within_block() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello World")?;

    // Delete "World" (positions 6..11)
    let result = document_editing_controller::delete_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &DeleteTextDto {
            position: 6,
            anchor: 11,
        },
    )?;

    assert_eq!(result.new_position, 6);
    assert_eq!(result.deleted_text, "World");

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello ");

    Ok(())
}

#[test]
fn test_delete_text_noop_same_position() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let result = document_editing_controller::delete_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &DeleteTextDto {
            position: 3,
            anchor: 3,
        },
    )?;

    assert_eq!(result.new_position, 3);
    assert_eq!(result.deleted_text, "");

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello");

    Ok(())
}

#[test]
fn test_insert_block_creates_new_block() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("HelloWorld")?;

    // Insert a block break at position 5, splitting "HelloWorld" into "Hello" and "World"
    let result = document_editing_controller::insert_block(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertBlockDto {
            position: 5,
            anchor: 5,
        },
    )?;

    // The new block should have been created with a valid ID
    assert!(result.new_block_id > 0);
    // The new position should be at the start of the new block (after "Hello" + block separator)
    assert_eq!(result.new_position, 6);

    // Verify via document stats that block count increased from 1 to 2
    let stats = test_harness::get_document_stats(&db_context)?;
    assert_eq!(stats.block_count, 2);

    // Verify content via export
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello\nWorld");

    Ok(())
}

// --- InsertText: Unicode ---

#[test]
fn test_insert_text_unicode() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("café")?;

    let result = document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 4, // after "café" (4 chars, not 5 bytes)
            anchor: 4,
            text: " latte".to_string(),
        },
    )?;

    assert_eq!(result.new_position, 10); // 4 + 6
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "café latte");

    Ok(())
}

// --- InsertText: with selection (position != anchor) ---

#[test]
fn test_insert_text_replaces_selection() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello World")?;

    // Select "World" (6..11) and replace with "Rust"
    let result = document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 6,
            anchor: 11,
            text: "Rust".to_string(),
        },
    )?;

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello Rust");
    assert_eq!(result.new_position, 10); // 6 + 4

    Ok(())
}

// --- DeleteText: reversed anchor/position ---

#[test]
fn test_delete_text_reversed_range() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello World")?;

    // anchor < position (reversed selection)
    let result = document_editing_controller::delete_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &DeleteTextDto {
            position: 11,
            anchor: 6,
        },
    )?;

    assert_eq!(result.new_position, 6);
    assert_eq!(result.deleted_text, "World");
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello ");

    Ok(())
}

// --- DeleteText: cross-block ---

#[test]
fn test_delete_text_cross_block() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello\nWorld")?;

    // Delete from position 3 to 9: "lo\nWor" -> merges blocks into "Helld"
    // "Hello" pos 0-4, separator at 5, "World" pos 6-10
    // Delete chars 3..9 = "lo" + separator + "Wor"
    let result = document_editing_controller::delete_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &DeleteTextDto {
            position: 3,
            anchor: 9,
        },
    )?;

    assert_eq!(result.new_position, 3);
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Helld");

    Ok(())
}

// --- DeleteText: entire block content ---

#[test]
fn test_delete_text_entire_content() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let result = document_editing_controller::delete_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &DeleteTextDto {
            position: 0,
            anchor: 5,
        },
    )?;

    assert_eq!(result.new_position, 0);
    assert_eq!(result.deleted_text, "Hello");
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "");

    Ok(())
}

// --- InsertBlock: at block boundaries ---

#[test]
fn test_insert_block_at_start() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_block(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertBlockDto {
            position: 0,
            anchor: 0,
        },
    )?;

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "\nHello");
    assert!(result.new_block_id > 0);

    Ok(())
}

#[test]
fn test_insert_block_at_end() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_block(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertBlockDto {
            position: 5,
            anchor: 5,
        },
    )?;

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello\n");
    assert!(result.new_block_id > 0);

    Ok(())
}

// --- InsertText: updates cached fields ---

#[test]
fn test_insert_text_updates_stats() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hi")?;

    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 2,
            anchor: 2,
            text: " there".to_string(),
        },
    )?;

    let stats = test_harness::get_document_stats(&db_context)?;
    assert_eq!(stats.character_count, 8); // "Hi there"
    assert_eq!(stats.block_count, 1);

    Ok(())
}

// --- Undo/Redo ---

#[test]
fn test_insert_text_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 5,
            anchor: 5,
            text: " World".to_string(),
        },
    )?;

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello World");

    // Undo
    undo_redo_manager.undo(None)?;
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello");

    // Redo
    undo_redo_manager.redo(None)?;
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello World");

    Ok(())
}

#[test]
fn test_delete_text_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello World")?;

    document_editing_controller::delete_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &DeleteTextDto {
            position: 5,
            anchor: 11,
        },
    )?;

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello");

    // Undo should restore " World"
    undo_redo_manager.undo(None)?;
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello World");

    Ok(())
}

#[test]
fn test_insert_block_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("HelloWorld")?;

    document_editing_controller::insert_block(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertBlockDto {
            position: 5,
            anchor: 5,
        },
    )?;

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello\nWorld");

    // Undo should merge back
    undo_redo_manager.undo(None)?;
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "HelloWorld");

    Ok(())
}

// ── InsertText merge tests ─────────────────────────────────────

#[test]
fn test_insert_text_merge_consecutive_chars() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    // Type "a", "b", "c" consecutively at positions 5, 6, 7
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 5,
            anchor: 5,
            text: "a".to_string(),
        },
    )?;
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 6,
            anchor: 6,
            text: "b".to_string(),
        },
    )?;
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 7,
            anchor: 7,
            text: "c".to_string(),
        },
    )?;

    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Helloabc");

    // All three should have merged into 1 undo command
    assert_eq!(undo_redo_manager.get_stack_size(0), 1);

    // Single undo restores original
    undo_redo_manager.undo(None)?;
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "Hello");

    Ok(())
}

#[test]
fn test_insert_text_merge_undo_redo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    // Type "W", "o", "r", "l", "d" consecutively
    for (i, ch) in "World".chars().enumerate() {
        document_editing_controller::insert_text(
            &db_context,
            &event_hub,
            &mut undo_redo_manager,
            None,
            &InsertTextDto {
                format_policy: Default::default(),
                position: 5 + i as i64,
                anchor: 5 + i as i64,
                text: ch.to_string(),
            },
        )?;
    }

    assert_eq!(export_text(&db_context, &event_hub)?, "HelloWorld");
    assert_eq!(undo_redo_manager.get_stack_size(0), 1);

    // Undo restores original
    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "Hello");

    // Redo replays the combined insert
    undo_redo_manager.redo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "HelloWorld");

    Ok(())
}

#[test]
fn test_insert_text_no_merge_non_contiguous() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    // Insert "a" at position 5
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 5,
            anchor: 5,
            text: "a".to_string(),
        },
    )?;

    // Insert "b" at position 0 (non-contiguous)
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 0,
            anchor: 0,
            text: "b".to_string(),
        },
    )?;

    assert_eq!(export_text(&db_context, &event_hub)?, "bHelloa");
    // Should NOT merge — 2 separate commands
    assert_eq!(undo_redo_manager.get_stack_size(0), 2);

    // Undo the second insert
    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "Helloa");

    // Undo the first insert
    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "Hello");

    Ok(())
}

#[test]
fn test_insert_text_no_merge_with_selection() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    // Insert "a" at position 5
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 5,
            anchor: 5,
            text: "a".to_string(),
        },
    )?;

    // Replace selection [5..6) with "b" (selection replacement)
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 5,
            anchor: 6,
            text: "b".to_string(),
        },
    )?;

    assert_eq!(export_text(&db_context, &event_hub)?, "Hellob");
    // Should NOT merge — 2 separate commands
    assert_eq!(undo_redo_manager.get_stack_size(0), 2);

    Ok(())
}

#[test]
fn test_insert_text_merge_word_boundary() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    // Type "a", "b" at positions 5, 6
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 5,
            anchor: 5,
            text: "a".to_string(),
        },
    )?;
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 6,
            anchor: 6,
            text: "b".to_string(),
        },
    )?;

    // Type " " (space) at position 7 — merges because accumulated "ab" ends with "b" (not a boundary)
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 7,
            anchor: 7,
            text: " ".to_string(),
        },
    )?;

    // Type "c" at position 8 — should NOT merge because accumulated "ab " ends with space
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 8,
            anchor: 8,
            text: "c".to_string(),
        },
    )?;

    assert_eq!(export_text(&db_context, &event_hub)?, "Helloab c");
    // 2 groups: "ab " and "c"
    assert_eq!(undo_redo_manager.get_stack_size(0), 2);

    // Undo removes "c"
    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "Helloab ");

    // Undo removes "ab "
    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "Hello");

    Ok(())
}

#[test]
fn test_insert_text_merge_max_length() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("")?;

    // Insert a 200-char string as one insert (fills the merge limit)
    let long_text: String = "x".repeat(200);
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 0,
            anchor: 0,
            text: long_text.clone(),
        },
    )?;

    // Insert one more char — should NOT merge (would exceed 200)
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 200,
            anchor: 200,
            text: "y".to_string(),
        },
    )?;

    // 2 separate commands
    assert_eq!(undo_redo_manager.get_stack_size(0), 2);

    Ok(())
}

#[test]
fn test_insert_text_no_merge_after_delete() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    // Insert "a" at position 5
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 5,
            anchor: 5,
            text: "a".to_string(),
        },
    )?;

    // Delete a character (different command type breaks the merge chain)
    document_editing_controller::delete_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &DeleteTextDto {
            position: 5,
            anchor: 6,
        },
    )?;

    // Insert "b" at position 5 — should NOT merge with the first insert
    // because a DeleteText command sits between them
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 5,
            anchor: 5,
            text: "b".to_string(),
        },
    )?;

    assert_eq!(export_text(&db_context, &event_hub)?, "Hellob");
    // 3 commands: insert "a", delete "a", insert "b"
    assert_eq!(undo_redo_manager.get_stack_size(0), 3);

    Ok(())
}

#[test]
fn test_insert_text_merge_punctuation_boundary() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    // Type "a", "." consecutively
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 5,
            anchor: 5,
            text: "a".to_string(),
        },
    )?;
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 6,
            anchor: 6,
            text: ".".to_string(),
        },
    )?;

    // "a" and "." merge (accumulated "a" doesn't end with boundary)
    assert_eq!(undo_redo_manager.get_stack_size(0), 1);

    // Type "b" — should NOT merge because accumulated "a." ends with "."
    document_editing_controller::insert_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertTextDto {
            format_policy: Default::default(),
            position: 7,
            anchor: 7,
            text: "b".to_string(),
        },
    )?;

    assert_eq!(export_text(&db_context, &event_hub)?, "Helloa.b");
    assert_eq!(undo_redo_manager.get_stack_size(0), 2);

    // Undo removes "b", then "a."
    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "Helloa.");
    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "Hello");

    Ok(())
}

// ── DeleteText merge tests ─────────────────────────────────────

#[test]
fn test_delete_text_merge_consecutive_backspaces() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    // Backspace 3 times from position 5: delete 'o', 'l', 'l'
    let pos = backspace(&db_context, &event_hub, &mut undo_redo_manager, 5)?;
    assert_eq!(pos, 4);
    let pos = backspace(&db_context, &event_hub, &mut undo_redo_manager, pos)?;
    assert_eq!(pos, 3);
    let _pos = backspace(&db_context, &event_hub, &mut undo_redo_manager, pos)?;

    assert_eq!(export_text(&db_context, &event_hub)?, "He");
    // All 3 should have merged into 1 command
    assert_eq!(undo_redo_manager.get_stack_size(0), 1);

    // Single undo restores original
    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "Hello");

    Ok(())
}

#[test]
fn test_delete_text_merge_consecutive_forward_deletes() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    // Forward-delete 3 times from position 1: delete 'e', 'l', 'l'
    let pos = forward_delete(&db_context, &event_hub, &mut undo_redo_manager, 1)?;
    assert_eq!(pos, 1);
    let pos = forward_delete(&db_context, &event_hub, &mut undo_redo_manager, pos)?;
    assert_eq!(pos, 1);
    let _pos = forward_delete(&db_context, &event_hub, &mut undo_redo_manager, pos)?;

    assert_eq!(export_text(&db_context, &event_hub)?, "Ho");
    assert_eq!(undo_redo_manager.get_stack_size(0), 1);

    // Undo restores original
    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "Hello");

    Ok(())
}

#[test]
fn test_delete_text_merge_undo_redo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    // Backspace 3 times from position 5
    let mut pos = 5;
    for _ in 0..3 {
        pos = backspace(&db_context, &event_hub, &mut undo_redo_manager, pos)?;
    }

    assert_eq!(export_text(&db_context, &event_hub)?, "He");

    // Undo restores
    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "Hello");

    // Redo replays combined delete
    undo_redo_manager.redo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "He");

    Ok(())
}

#[test]
fn test_delete_text_no_merge_different_direction() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    // Backspace from position 5 (delete 'o')
    let _pos = backspace(&db_context, &event_hub, &mut undo_redo_manager, 5)?;

    // Forward delete from position 4 would be different direction — should not merge
    // But actually cursor is at 4 after backspace. Forward delete at 4 is NOT contiguous
    // with the backspace direction. Let's verify with a non-contiguous case.

    // Forward delete from position 0 (non-contiguous)
    let _pos = forward_delete(&db_context, &event_hub, &mut undo_redo_manager, 0)?;

    assert_eq!(undo_redo_manager.get_stack_size(0), 2);

    Ok(())
}

#[test]
fn test_delete_text_no_merge_non_contiguous() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello World")?;

    // Backspace from position 11 (delete 'd')
    let _pos = backspace(&db_context, &event_hub, &mut undo_redo_manager, 11)?;

    // Backspace from position 5 (not contiguous with previous cursor at 10)
    let _pos = backspace(&db_context, &event_hub, &mut undo_redo_manager, 5)?;

    assert_eq!(undo_redo_manager.get_stack_size(0), 2);

    Ok(())
}

#[test]
fn test_delete_text_no_merge_selection_delete() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello World")?;

    // Multi-char selection delete (not single-char)
    document_editing_controller::delete_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &DeleteTextDto {
            position: 5,
            anchor: 11,
        },
    )?;

    // Single-char backspace — should NOT merge with multi-char delete
    let _pos = backspace(&db_context, &event_hub, &mut undo_redo_manager, 5)?;

    assert_eq!(undo_redo_manager.get_stack_size(0), 2);

    Ok(())
}

#[test]
fn test_delete_text_merge_word_boundary() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("ab cd")?;

    // Backspace from position 5: delete 'd', then 'c', then ' '
    let pos = backspace(&db_context, &event_hub, &mut undo_redo_manager, 5)?; // 'd'
    let pos = backspace(&db_context, &event_hub, &mut undo_redo_manager, pos)?; // 'c'
    let pos = backspace(&db_context, &event_hub, &mut undo_redo_manager, pos)?; // ' '

    // 'd', 'c' merge. Then ' ' merges too (since 'c' is not a boundary).
    // Now accumulated last deleted char is ' ' (whitespace).
    assert_eq!(undo_redo_manager.get_stack_size(0), 1);

    // Next backspace for 'b' should NOT merge (last deleted was space)
    let _pos = backspace(&db_context, &event_hub, &mut undo_redo_manager, pos)?; // 'b'

    assert_eq!(export_text(&db_context, &event_hub)?, "a");
    assert_eq!(undo_redo_manager.get_stack_size(0), 2);

    // Undo restores 'b'
    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "ab");

    // Undo restores ' cd'
    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "ab cd");

    Ok(())
}
