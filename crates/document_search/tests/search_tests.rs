extern crate text_document_search as document_search;
use anyhow::Result;

use test_harness::setup_with_text;

use document_search::document_search_controller;
use document_search::{FindAllDto, FindTextDto};

#[test]
fn test_find_text_simple() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup_with_text("Hello World")?;

    let result = document_search_controller::find_text(
        &db_context,
        &event_hub,
        &FindTextDto {
            query: "World".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
            search_backward: false,
            start_position: 0,
        },
    )?;

    assert!(result.found);
    assert_eq!(result.position, 6);
    assert_eq!(result.length, 5);

    Ok(())
}

#[test]
fn test_find_text_not_found() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup_with_text("Hello World")?;

    let result = document_search_controller::find_text(
        &db_context,
        &event_hub,
        &FindTextDto {
            query: "Foo".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
            search_backward: false,
            start_position: 0,
        },
    )?;

    assert!(!result.found);

    Ok(())
}

#[test]
fn test_find_text_case_insensitive() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup_with_text("Hello World")?;

    // Case-sensitive search should not find "hello"
    let result_sensitive = document_search_controller::find_text(
        &db_context,
        &event_hub,
        &FindTextDto {
            query: "hello".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
            search_backward: false,
            start_position: 0,
        },
    )?;
    assert!(!result_sensitive.found);

    // Case-insensitive search should find "hello"
    let result_insensitive = document_search_controller::find_text(
        &db_context,
        &event_hub,
        &FindTextDto {
            query: "hello".to_string(),
            case_sensitive: false,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
            search_backward: false,
            start_position: 0,
        },
    )?;
    assert!(result_insensitive.found);
    assert_eq!(result_insensitive.position, 0);
    assert_eq!(result_insensitive.length, 5);

    Ok(())
}

#[test]
fn test_find_text_backward() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup_with_text("abc abc abc")?;

    // Search backward from position 10, should find the second "abc" at position 4
    let result = document_search_controller::find_text(
        &db_context,
        &event_hub,
        &FindTextDto {
            query: "abc".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
            search_backward: true,
            start_position: 8,
        },
    )?;

    assert!(result.found);
    assert_eq!(result.position, 4);

    Ok(())
}

#[test]
fn test_find_all_multiple_matches() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) =
        setup_with_text("the cat and the dog and the bird")?;

    let result = document_search_controller::find_all(
        &db_context,
        &event_hub,
        &FindAllDto {
            query: "the".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
        },
    )?;

    assert_eq!(result.count, 3);
    assert_eq!(result.positions, vec![0, 12, 24]);
    assert_eq!(result.lengths, vec![3, 3, 3]);

    Ok(())
}

#[test]
fn test_find_all_no_matches() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup_with_text("Hello World")?;

    let result = document_search_controller::find_all(
        &db_context,
        &event_hub,
        &FindAllDto {
            query: "xyz".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
        },
    )?;

    assert_eq!(result.count, 0);
    assert!(result.positions.is_empty());

    Ok(())
}

#[test]
fn test_find_all_across_blocks() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup_with_text("hello there\nhello again")?;

    let result = document_search_controller::find_all(
        &db_context,
        &event_hub,
        &FindAllDto {
            query: "hello".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
        },
    )?;

    assert_eq!(result.count, 2);
    // "hello there\nhello again" -> positions 0 and 12
    assert_eq!(result.positions, vec![0, 12]);

    Ok(())
}

#[test]
fn test_find_text_regex() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("abc 123 def 456")?;

    // Find a sequence of digits using regex
    let result = document_search_controller::find_text(
        &db_context,
        &event_hub,
        &FindTextDto {
            query: r"\d+".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: true,
            search_backward: false,
            start_position: 0,
        },
    )?;

    assert!(result.found);
    assert_eq!(result.position, 4); // "123" starts at char 4
    assert_eq!(result.length, 3);

    Ok(())
}

#[test]
fn test_find_all_regex() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("cat bat hat mat")?;

    // Find all words ending in "at" with regex
    let result = document_search_controller::find_all(
        &db_context,
        &event_hub,
        &FindAllDto {
            query: r"[a-z]at".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: true,
        },
    )?;

    assert_eq!(result.count, 4);
    assert_eq!(result.positions, vec![0, 4, 8, 12]);
    assert_eq!(result.lengths, vec![3, 3, 3, 3]);

    Ok(())
}

#[test]
fn test_find_text_unicode() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("café résumé naïve")?;

    // Find an accented word
    let result = document_search_controller::find_text(
        &db_context,
        &event_hub,
        &FindTextDto {
            query: "résumé".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
            search_backward: false,
            start_position: 0,
        },
    )?;

    assert!(result.found);
    assert_eq!(result.position, 5); // "café " is 5 chars, so "résumé" starts at char 5
    assert_eq!(result.length, 6); // "résumé" is 6 chars

    Ok(())
}

#[test]
fn test_find_text_whole_word_unicode() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("café caféine")?;

    // Whole word search for "café" — should only match the first one
    let result = document_search_controller::find_all(
        &db_context,
        &event_hub,
        &FindAllDto {
            query: "café".to_string(),
            case_sensitive: true,
            whole_word: true,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
        },
    )?;

    assert_eq!(result.count, 1);
    assert_eq!(result.positions, vec![0]);

    Ok(())
}

#[test]
fn test_find_text_start_position_skips() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("abc abc abc")?;

    // Search from position 1 should skip first "abc"
    let result = document_search_controller::find_text(
        &db_context,
        &event_hub,
        &FindTextDto {
            query: "abc".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
            search_backward: false,
            start_position: 1,
        },
    )?;

    assert!(result.found);
    assert_eq!(result.position, 4); // second "abc"

    Ok(())
}

#[test]
fn test_find_text_whole_word_ascii() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("catfish cat concatenate")?;

    let result = document_search_controller::find_all(
        &db_context,
        &event_hub,
        &FindAllDto {
            query: "cat".to_string(),
            case_sensitive: true,
            whole_word: true,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
        },
    )?;

    assert_eq!(result.count, 1);
    assert_eq!(result.positions, vec![8]); // only standalone "cat"

    Ok(())
}

#[test]
fn test_find_text_regex_case_insensitive() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello HELLO hello")?;

    let result = document_search_controller::find_all(
        &db_context,
        &event_hub,
        &FindAllDto {
            query: "hello".to_string(),
            case_sensitive: false,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: true,
        },
    )?;

    assert_eq!(result.count, 3);

    Ok(())
}

#[test]
fn test_find_text_empty_query() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello")?;

    let result = document_search_controller::find_text(
        &db_context,
        &event_hub,
        &FindTextDto {
            query: "".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
            search_backward: false,
            start_position: 0,
        },
    )?;

    assert!(!result.found);

    Ok(())
}

#[test]
fn test_find_all_empty_query() -> Result<()> {
    let (db_context, event_hub, _) = setup_with_text("Hello")?;

    let result = document_search_controller::find_all(
        &db_context,
        &event_hub,
        &FindAllDto {
            query: "".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
        },
    )?;

    assert_eq!(result.count, 0);

    Ok(())
}
