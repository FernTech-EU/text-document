extern crate text_document_editing as document_editing;
use anyhow::Result;

use test_harness::{
    block_controller, export_text, get_block_ids, get_element_ids, inline_element_controller,
    setup_with_text,
};

use document_editing::document_editing_controller;
use document_editing::{InsertFragmentDto, InsertHtmlAtPositionDto, InsertMarkdownAtPositionDto};

// ─── Markdown tests ──────────────────────────────────────────────────

#[test]
fn test_insert_markdown_simple_paragraph() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_markdown_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertMarkdownAtPositionDto {
            position: 5,
            anchor: 5,
            markdown: "Hello **world**".to_string(),
        },
    )?;

    // Single plain paragraph merges inline — no new blocks
    assert_eq!(result.blocks_added, 0);

    // Verify the text was inserted
    let text = export_text(&db_context, &event_hub)?;
    assert!(text.contains("world"));

    // Verify bold formatting on the "world" span
    let block_ids = get_block_ids(&db_context)?;
    let mut found_bold = false;
    for block_id in &block_ids {
        let elem_ids = get_element_ids(&db_context, block_id)?;
        for elem_id in &elem_ids {
            let elem = inline_element_controller::get(&db_context, elem_id)?;
            if let Some(elem) = elem
                && let common::format_runs::InlineContent::Text(ref t) = elem.content
                && t == "world"
            {
                assert_eq!(elem.fmt_font_bold, Some(true));
                found_bold = true;
            }
        }
    }
    assert!(found_bold, "Should find a bold 'world' element");

    Ok(())
}

#[test]
fn test_insert_markdown_multiple_paragraphs() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Start")?;

    let result = document_editing_controller::insert_markdown_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertMarkdownAtPositionDto {
            position: 5,
            anchor: 5,
            markdown: "Para 1\n\nPara 2".to_string(),
        },
    )?;

    // Two parsed blocks: first merges into current, last becomes tail → 1 block added
    assert_eq!(
        result.blocks_added, 1,
        "Two-block merge should add 1 block (tail)"
    );

    let text = export_text(&db_context, &event_hub)?;
    assert!(text.contains("Para 1"));
    assert!(text.contains("Para 2"));

    Ok(())
}

#[test]
fn test_insert_markdown_heading() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Text")?;

    let _result = document_editing_controller::insert_markdown_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertMarkdownAtPositionDto {
            position: 4,
            anchor: 4,
            markdown: "# Title".to_string(),
        },
    )?;

    // Verify heading_level=1 on the inserted block
    let block_ids = get_block_ids(&db_context)?;
    let mut found_heading = false;
    for block_id in &block_ids {
        let block = block_controller::get(&db_context, block_id)?;
        if let Some(block) = block
            && block.fmt_heading_level == Some(1)
        {
            found_heading = true;
            break;
        }
    }
    assert!(found_heading, "Should find a block with heading_level=1");

    Ok(())
}

#[test]
fn test_insert_markdown_list() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Text")?;

    let _result = document_editing_controller::insert_markdown_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertMarkdownAtPositionDto {
            position: 4,
            anchor: 4,
            markdown: "- item1\n- item2".to_string(),
        },
    )?;

    // Verify list entities were created by checking block list relationships
    let block_ids = get_block_ids(&db_context)?;
    let mut list_blocks = 0;
    for block_id in &block_ids {
        let block = block_controller::get(&db_context, block_id)?;
        if let Some(block) = block
            && block.list.is_some()
        {
            list_blocks += 1;
        }
    }
    assert!(
        list_blocks >= 2,
        "Should have at least 2 blocks with list associations"
    );

    Ok(())
}

#[test]
fn test_insert_markdown_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let original_text = export_text(&db_context, &event_hub)?;

    let _result = document_editing_controller::insert_markdown_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertMarkdownAtPositionDto {
            position: 5,
            anchor: 5,
            markdown: "**bold text**".to_string(),
        },
    )?;

    let after_insert = export_text(&db_context, &event_hub)?;
    assert_ne!(after_insert, original_text);

    // Undo
    undo_redo_manager.undo(None)?;

    let after_undo = export_text(&db_context, &event_hub)?;
    assert_eq!(after_undo, original_text);

    Ok(())
}

// ─── HTML tests ──────────────────────────────────────────────────────

#[test]
fn test_insert_html_simple() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let result = document_editing_controller::insert_html_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertHtmlAtPositionDto {
            position: 5,
            anchor: 5,
            html: "<p>Hello <b>world</b></p>".to_string(),
        },
    )?;

    // Single plain paragraph merges inline — no new blocks
    assert_eq!(result.blocks_added, 0);

    let text = export_text(&db_context, &event_hub)?;
    assert!(text.contains("world"));

    // Verify bold formatting
    let block_ids = get_block_ids(&db_context)?;
    let mut found_bold = false;
    for block_id in &block_ids {
        let elem_ids = get_element_ids(&db_context, block_id)?;
        for elem_id in &elem_ids {
            let elem = inline_element_controller::get(&db_context, elem_id)?;
            if let Some(elem) = elem
                && let common::format_runs::InlineContent::Text(ref t) = elem.content
                && t == "world"
            {
                assert_eq!(elem.fmt_font_bold, Some(true));
                found_bold = true;
            }
        }
    }
    assert!(found_bold, "Should find a bold 'world' element");

    Ok(())
}

#[test]
fn test_insert_html_multiple_paragraphs() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Start")?;

    let result = document_editing_controller::insert_html_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertHtmlAtPositionDto {
            position: 5,
            anchor: 5,
            html: "<p>A</p><p>B</p>".to_string(),
        },
    )?;

    // Two parsed blocks: first merges into current, last becomes tail → 1 block added
    assert_eq!(
        result.blocks_added, 1,
        "Two-block merge should add 1 block (tail)"
    );

    let text = export_text(&db_context, &event_hub)?;
    assert!(text.contains("A"));
    assert!(text.contains("B"));

    Ok(())
}

#[test]
fn test_insert_html_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let original_text = export_text(&db_context, &event_hub)?;

    let _result = document_editing_controller::insert_html_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertHtmlAtPositionDto {
            position: 5,
            anchor: 5,
            html: "<p><b>bold</b></p>".to_string(),
        },
    )?;

    let after_insert = export_text(&db_context, &event_hub)?;
    assert_ne!(after_insert, original_text);

    // Undo
    undo_redo_manager.undo(None)?;

    let after_undo = export_text(&db_context, &event_hub)?;
    assert_eq!(after_undo, original_text);

    Ok(())
}

// ─── Fragment tests ──────────────────────────────────────────────────

fn make_bold_fragment(text: &str) -> String {
    serde_json::json!({
        "blocks": [{
            "plain_text": text,
            "elements": [{
                "content": {"Text": text},
                "fmt_font_family": null,
                "fmt_font_point_size": null,
                "fmt_font_weight": null,
                "fmt_font_bold": true,
                "fmt_font_italic": null,
                "fmt_font_underline": null,
                "fmt_font_overline": null,
                "fmt_font_strikeout": null,
                "fmt_letter_spacing": null,
                "fmt_word_spacing": null,
                "fmt_anchor_href": null,
                "fmt_anchor_names": [],
                "fmt_is_anchor": null,
                "fmt_tooltip": null,
                "fmt_underline_style": null,
                "fmt_vertical_alignment": null
            }],
            "heading_level": null,
            "list": null,
            "alignment": null,
            "indent": null,
            "text_indent": null,
            "marker": null,
            "top_margin": null,
            "bottom_margin": null,
            "left_margin": null,
            "right_margin": null,
            "tab_positions": []
        }]
    })
    .to_string()
}

#[test]
fn test_insert_fragment_simple() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let fragment_json = make_bold_fragment("bold text");

    let result = document_editing_controller::insert_fragment(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFragmentDto {
            position: 5,
            anchor: 5,
            fragment_data: fragment_json,
        },
    )?;

    // Single inline-only fragment block merges inline — no new blocks
    assert_eq!(result.blocks_added, 0);

    let block_ids = get_block_ids(&db_context)?;
    let mut found_bold = false;
    for block_id in &block_ids {
        let elem_ids = get_element_ids(&db_context, block_id)?;
        for elem_id in &elem_ids {
            let elem = inline_element_controller::get(&db_context, elem_id)?.unwrap();
            if let common::format_runs::InlineContent::Text(ref t) = elem.content
                && t.contains("bold text")
                && elem.fmt_font_bold == Some(true)
            {
                found_bold = true;
            }
        }
    }
    assert!(found_bold, "Should find a bold 'bold text' element");

    Ok(())
}

#[test]
fn test_insert_fragment_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello")?;

    let original_text = export_text(&db_context, &event_hub)?;

    let fragment_json = make_bold_fragment("bold text");

    let _result = document_editing_controller::insert_fragment(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertFragmentDto {
            position: 5,
            anchor: 5,
            fragment_data: fragment_json,
        },
    )?;

    let after_insert = export_text(&db_context, &event_hub)?;
    assert_ne!(after_insert, original_text);

    undo_redo_manager.undo(None)?;

    let after_undo = export_text(&db_context, &event_hub)?;
    assert_eq!(after_undo, original_text);

    Ok(())
}

// --- Additional coverage ---

#[test]
fn test_insert_markdown_code_block() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Before")?;

    let result = document_editing_controller::insert_markdown_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertMarkdownAtPositionDto {
            position: 6,
            anchor: 6,
            markdown: "```\nfn main() {}\n```".to_string(),
        },
    )?;

    assert!(result.blocks_added >= 1);

    let block_ids = get_block_ids(&db_context)?;
    let mut found_code = false;
    for block_id in &block_ids {
        let elem_ids = get_element_ids(&db_context, block_id)?;
        for elem_id in &elem_ids {
            let elem = inline_element_controller::get(&db_context, elem_id)?.unwrap();
            if let common::format_runs::InlineContent::Text(ref t) = elem.content
                && t.contains("fn main")
            {
                assert_eq!(elem.fmt_font_family, Some("monospace".to_string()));
                found_code = true;
            }
        }
    }
    assert!(found_code, "Should find a code-formatted element");

    Ok(())
}

#[test]
fn test_insert_markdown_nested_bold_italic() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Start")?;

    let result = document_editing_controller::insert_markdown_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertMarkdownAtPositionDto {
            position: 5,
            anchor: 5,
            markdown: "***bold italic***".to_string(),
        },
    )?;

    // Single inline paragraph merges — no new blocks
    assert_eq!(result.blocks_added, 0);

    let block_ids = get_block_ids(&db_context)?;
    let mut found_bi = false;
    for block_id in &block_ids {
        let elem_ids = get_element_ids(&db_context, block_id)?;
        for elem_id in &elem_ids {
            let elem = inline_element_controller::get(&db_context, elem_id)?.unwrap();
            if elem.fmt_font_bold == Some(true) && elem.fmt_font_italic == Some(true) {
                found_bi = true;
            }
        }
    }
    assert!(found_bi, "Should find an element with both bold and italic");

    Ok(())
}

#[test]
fn test_insert_html_link() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("See")?;

    document_editing_controller::insert_html_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertHtmlAtPositionDto {
            position: 3,
            anchor: 3,
            html: "<p><a href=\"http://example.com\">link</a></p>".to_string(),
        },
    )?;

    let block_ids = get_block_ids(&db_context)?;
    let mut found_link = false;
    for block_id in &block_ids {
        let elem_ids = get_element_ids(&db_context, block_id)?;
        for elem_id in &elem_ids {
            let elem = inline_element_controller::get(&db_context, elem_id)?.unwrap();
            if elem.fmt_anchor_href == Some("http://example.com".to_string()) {
                found_link = true;
            }
        }
    }
    assert!(found_link, "Should find an element with link href");

    Ok(())
}

// ─── Inline merge tests ─────────────────────────────────────────────

#[test]
fn test_insert_html_single_paragraph_merges_inline() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello world")?;

    let block_ids_before = get_block_ids(&db_context)?;
    let block_count_before = block_ids_before.len();

    let result = document_editing_controller::insert_html_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertHtmlAtPositionDto {
            position: 5,
            anchor: 5,
            // The paragraph's leading space is layout, not text: a whitespace
            // run at a block edge collapses away, so the fragment carries
            // exactly "beautiful" into the merge.
            html: "<p> <b>beautiful</b></p>".to_string(),
        },
    )?;

    // Single-paragraph inline HTML should NOT create new blocks
    assert_eq!(
        result.blocks_added, 0,
        "Single inline block should not add blocks"
    );

    let block_ids_after = get_block_ids(&db_context)?;
    assert_eq!(
        block_ids_after.len(),
        block_count_before,
        "Block count should remain the same for inline merge"
    );

    // Verify text is merged
    let text = export_text(&db_context, &event_hub)?;
    assert!(
        text.contains("Hellobeautiful world"),
        "Text should be merged inline, got: {}",
        text
    );

    // Verify bold formatting on "beautiful"
    let mut found_bold = false;
    for block_id in &block_ids_after {
        let elem_ids = get_element_ids(&db_context, block_id)?;
        for elem_id in &elem_ids {
            let elem = inline_element_controller::get(&db_context, elem_id)?;
            if let Some(elem) = elem
                && let common::format_runs::InlineContent::Text(ref t) = elem.content
                && t == "beautiful"
            {
                assert_eq!(elem.fmt_font_bold, Some(true));
                found_bold = true;
            }
        }
    }
    assert!(found_bold, "Should find a bold 'beautiful' element");

    Ok(())
}

#[test]
fn test_insert_html_single_paragraph_merges_inline_undo() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello world")?;

    let original_text = export_text(&db_context, &event_hub)?;

    document_editing_controller::insert_html_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertHtmlAtPositionDto {
            position: 5,
            anchor: 5,
            html: "<b>bold</b>".to_string(),
        },
    )?;

    let after_insert = export_text(&db_context, &event_hub)?;
    assert_ne!(after_insert, original_text);

    // Undo should restore original state
    undo_redo_manager.undo(None)?;
    let after_undo = export_text(&db_context, &event_hub)?;
    assert_eq!(after_undo, original_text);

    Ok(())
}

#[test]
fn test_insert_markdown_single_paragraph_merges_inline() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("Hello world")?;

    let block_ids_before = get_block_ids(&db_context)?;
    let block_count_before = block_ids_before.len();

    let result = document_editing_controller::insert_markdown_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertMarkdownAtPositionDto {
            position: 6, // After "Hello " (past the space)
            anchor: 6,
            markdown: "**beautiful** ".to_string(),
        },
    )?;

    assert_eq!(
        result.blocks_added, 0,
        "Single inline block should not add blocks"
    );

    let block_ids_after = get_block_ids(&db_context)?;
    assert_eq!(block_ids_after.len(), block_count_before);

    let text = export_text(&db_context, &event_hub)?;
    assert!(
        text.contains("beautiful"),
        "Text should be merged inline, got: {}",
        text
    );

    Ok(())
}

// ─── Multi-block merge tests ────────────────────────────────────────

#[test]
fn test_insert_html_multi_block_merges_first_and_last() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("HelloWorld")?;

    let block_ids_before = get_block_ids(&db_context)?;
    assert_eq!(block_ids_before.len(), 1);

    // Insert 3 paragraphs at position 5 (between "Hello" and "World")
    let result = document_editing_controller::insert_html_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertHtmlAtPositionDto {
            position: 5,
            anchor: 5,
            html: "<p>A</p><p>B</p><p>C</p>".to_string(),
        },
    )?;

    // 3 parsed blocks: first merges into current, 1 middle block, last→tail = 2 added
    assert_eq!(result.blocks_added, 2);

    let text = export_text(&db_context, &event_hub)?;
    // Expected: "HelloA" / "B" / "CWorld"
    assert_eq!(
        text, "HelloA\nB\nCWorld",
        "Expected merged first/last with middle blocks, got: {}",
        text
    );

    let block_ids_after = get_block_ids(&db_context)?;
    assert_eq!(block_ids_after.len(), 3, "Should have 3 blocks total");

    Ok(())
}

#[test]
fn test_insert_html_two_blocks_no_middle() -> Result<()> {
    let (db_context, event_hub, mut undo_redo_manager) = setup_with_text("HelloWorld")?;

    let result = document_editing_controller::insert_html_at_position(
        &db_context,
        &event_hub,
        &mut undo_redo_manager,
        None,
        &InsertHtmlAtPositionDto {
            position: 5,
            anchor: 5,
            html: "<p>A</p><p>B</p>".to_string(),
        },
    )?;

    assert_eq!(result.blocks_added, 1);

    let text = export_text(&db_context, &event_hub)?;
    // Expected: "HelloA" / "BWorld"
    assert_eq!(
        text, "HelloA\nBWorld",
        "Expected two-block merge, got: {}",
        text
    );

    Ok(())
}
