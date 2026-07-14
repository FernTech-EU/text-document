extern crate text_document_search as document_search;
use anyhow::Result;

use test_harness::setup_with_text;

use document_search::document_search_controller;
use document_search::{FindAllDto, FindTextDto, ReplaceTextDto};

// ═══════════════════════════════════════════════════════════════════
// Find on empty document
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_find_text_in_empty_document() -> Result<()> {
    let (db, hub, _urm) = setup_with_text("")?;

    let result = document_search_controller::find_text(
        &db,
        &hub,
        &FindTextDto {
            query: "anything".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            search_backward: false,
            start_position: 0,
        },
    )?;

    assert!(!result.found);

    Ok(())
}

#[test]
fn test_find_all_in_empty_document() -> Result<()> {
    let (db, hub, _urm) = setup_with_text("")?;

    let result = document_search_controller::find_all(
        &db,
        &hub,
        &FindAllDto {
            query: "anything".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
        },
    )?;

    assert_eq!(result.positions.len(), 0);
    assert_eq!(result.count, 0);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
// Empty query
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_find_empty_query() -> Result<()> {
    let (db, hub, _urm) = setup_with_text("Hello World")?;

    let result = document_search_controller::find_text(
        &db,
        &hub,
        &FindTextDto {
            query: "".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            search_backward: false,
            start_position: 0,
        },
    )?;

    // Empty query should either not find or error — but not crash
    // Finding nothing is the most sensible behavior
    assert!(!result.found, "Empty query should not find anything");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
// Invalid regex
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_find_invalid_regex() -> Result<()> {
    let (db, hub, _urm) = setup_with_text("Hello World")?;

    let result = document_search_controller::find_text(
        &db,
        &hub,
        &FindTextDto {
            query: "[invalid".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: true,
            search_backward: false,
            start_position: 0,
        },
    );

    // Should error, not crash
    assert!(
        result.is_err(),
        "Invalid regex should produce an error, not panic"
    );

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
// Start position out of range
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_find_start_position_beyond_document() -> Result<()> {
    let (db, hub, _urm) = setup_with_text("Hello")?;

    let result = document_search_controller::find_text(
        &db,
        &hub,
        &FindTextDto {
            query: "Hello".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            search_backward: false,
            start_position: 999,
        },
    )?;

    // Starting beyond document should not find the text
    assert!(!result.found);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
// Replace edge cases
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_replace_no_matches() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello World")?;

    let result = document_search_controller::replace_text(
        &db,
        &hub,
        &mut urm,
        None,
        &ReplaceTextDto {
            query: "NotFound".to_string(),
            replacement: "Replaced".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 0);

    Ok(())
}

#[test]
fn test_replace_with_empty_string() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Hello World")?;

    let result = document_search_controller::replace_text(
        &db,
        &hub,
        &mut urm,
        None,
        &ReplaceTextDto {
            query: " World".to_string(),
            replacement: "".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            replace_all: false,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 1);

    Ok(())
}

#[test]
fn test_replace_in_empty_document() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("")?;

    let result = document_search_controller::replace_text(
        &db,
        &hub,
        &mut urm,
        None,
        &ReplaceTextDto {
            query: "Hello".to_string(),
            replacement: "World".to_string(),
            case_sensitive: true,
            whole_word: false,
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(result.replacements_count, 0);

    Ok(())
}
