extern crate text_document_search as document_search;
use anyhow::Result;

use test_harness::{export_text, setup_with_text};

use document_search::ReplaceTextDto;
use document_search::document_search_controller;

#[test]
fn test_replace_single() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("hello world hello")?;

    let result = document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &ReplaceTextDto {
            query: "hello".to_string(),
            replacement: "hi".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            replace_all: false,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 1);
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "hi world hello");

    Ok(())
}

#[test]
fn test_replace_all() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("hello world hello")?;

    let result = document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &ReplaceTextDto {
            query: "hello".to_string(),
            replacement: "hi".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 2);
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "hi world hi");

    Ok(())
}

#[test]
fn test_replace_case_insensitive() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello HELLO hello")?;

    let result = document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &ReplaceTextDto {
            query: "hello".to_string(),
            replacement: "hi".to_string(),
            case_sensitive: false,
            whole_word: false,
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 3);
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "hi hi hi");

    Ok(())
}

#[test]
fn test_replace_regex() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("abc 123 def 456")?;

    let result = document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &ReplaceTextDto {
            query: r"\d+".to_string(),
            replacement: "NUM".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: true,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 2);
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "abc NUM def NUM");

    Ok(())
}

#[test]
fn test_replace_not_found() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("hello world")?;

    let result = document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &ReplaceTextDto {
            query: "xyz".to_string(),
            replacement: "abc".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 0);
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "hello world");

    Ok(())
}

#[test]
fn test_replace_empty_query() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("hello world")?;

    let result = document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &ReplaceTextDto {
            query: "".to_string(),
            replacement: "abc".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 0);
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "hello world");

    Ok(())
}

#[test]
fn test_replace_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("hello world hello")?;

    let result = document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &ReplaceTextDto {
            query: "hello".to_string(),
            replacement: "hi".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 2);
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "hi world hi");

    // Undo
    undo_redo_manager.undo(None)?;
    let text_after_undo = export_text(&db_context, &event_hub)?;
    assert_eq!(text_after_undo, "hello world hello");

    Ok(())
}

#[test]
fn test_replace_all_across_blocks() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) =
        setup_with_text("hello there\nhello again\nhello end")?;

    let result = document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &ReplaceTextDto {
            query: "hello".to_string(),
            replacement: "hi".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 3);
    let text = export_text(&db_context, &event_hub)?;
    assert_eq!(text, "hi there\nhi again\nhi end");

    Ok(())
}

#[test]
fn test_replace_redo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("hello world")?;

    document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &ReplaceTextDto {
            query: "hello".to_string(),
            replacement: "hi".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(export_text(&db_context, &event_hub)?, "hi world");

    undo_redo_manager.undo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "hello world");

    undo_redo_manager.redo(None)?;
    assert_eq!(export_text(&db_context, &event_hub)?, "hi world");

    Ok(())
}

#[test]
fn test_replace_with_empty_deletes() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("hello beautiful world")?;

    let result = document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &ReplaceTextDto {
            query: "beautiful ".to_string(),
            replacement: "".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 1);
    assert_eq!(export_text(&db_context, &event_hub)?, "hello world");

    Ok(())
}

#[test]
fn test_replace_with_longer_text() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("a b a")?;

    let result = document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &ReplaceTextDto {
            query: "a".to_string(),
            replacement: "xxxx".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 2);
    assert_eq!(export_text(&db_context, &event_hub)?, "xxxx b xxxx");

    Ok(())
}
