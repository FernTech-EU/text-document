//! Phase 2 step 5.6: verify document_search use cases mirror their
//! mutations to the global rope.

extern crate text_document_search as document_search;

use anyhow::Result;
use document_search::ReplaceTextDto;
use document_search::document_search_controller;
use test_harness::setup_with_imported_text;

/// `replace_text` mutates `block.plain_text` and must also mirror
/// the splice into the rope so the two stay in sync.
#[test]
fn replace_text_mirrors_to_rope() -> Result<()> {
    let (db_context, event_hub, mut urm) = setup_with_imported_text("hello world hello")?;

    let store = db_context.get_store();
    assert_eq!(store.rope.read().to_string(), "hello world hello");

    document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut urm,
        None,
        &ReplaceTextDto {
            query: "hello".to_string(),
            replacement: "HEY".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    // Rope reflects both replacements.
    assert_eq!(store.rope.read().to_string(), "HEY world HEY");

    Ok(())
}

/// Replace inside a multi-block doc must update the rope across the
/// affected block's byte range without disturbing neighbors.
#[test]
fn replace_text_in_one_block_of_many_preserves_other_blocks() -> Result<()> {
    let (db_context, event_hub, mut urm) = setup_with_imported_text("first\nfind me\nthird")?;

    let store = db_context.get_store();
    assert_eq!(store.rope.read().to_string(), "first\nfind me\nthird");

    document_search_controller::replace_text(
        &db_context,
        &event_hub,
        &mut urm,
        None,
        &ReplaceTextDto {
            query: "find me".to_string(),
            replacement: "FOUND".to_string(),
            case_sensitive: true,
            whole_word: false,
            diacritic_sensitive: false,
            language: String::new(),
            use_regex: false,
            replace_all: true,
            format_policy: Default::default(),
        },
    )?;

    assert_eq!(store.rope.read().to_string(), "first\nFOUND\nthird");

    Ok(())
}
