//! What `insert_markdown_at_position`, `insert_html_at_position` and
//! `insert_djot_at_position` give the numbered lists they paste: the delimiter, on each of
//! the paths a paste takes (a single block, the first block over an empty one, the blocks
//! between, and the last).
//!
//! The text-document crate's own `insert_*` go through fragments and do not reach these use
//! cases, so they are tested here, where they are reached.

extern crate text_document_editing as document_editing;
use anyhow::Result;

use document_editing::document_editing_controller;
use document_editing::{
    InsertDjotAtPositionDto, InsertHtmlAtPositionDto, InsertMarkdownAtPositionDto,
};
use test_harness::{
    DbContext, EventHub, UpdateListDto, block_controller, block_text_dto, get_block_ids,
    list_controller, setup_with_text,
};

use common::undo_redo::UndoRedoManager;
use std::sync::Arc;

struct Item {
    text: String,
    /// The list id, prefix and suffix.
    list: Option<(u64, String, String)>,
}

fn items(db: &DbContext) -> Result<Vec<Item>> {
    let mut out = Vec::new();
    for id in get_block_ids(db)? {
        let block = block_controller::get(db, &id)?.expect("block");
        let list = match block.list {
            Some(list_id) => list_controller::get(db, &list_id)?
                .map(|l| (list_id, l.prefix.clone(), l.suffix.clone())),
            None => None,
        };
        out.push(Item {
            text: block_text_dto(db, &block),
            list,
        });
    }
    Ok(out)
}

fn list_items(db: &DbContext) -> Result<Vec<Item>> {
    Ok(items(db)?
        .into_iter()
        .filter(|i| i.list.is_some())
        .collect())
}

type Env = (DbContext, Arc<EventHub>, UndoRedoManager);

fn markdown(env: &mut Env, position: i64, text: &str) -> Result<()> {
    document_editing_controller::insert_markdown_at_position(
        &env.0,
        &env.1,
        &mut env.2,
        None,
        &InsertMarkdownAtPositionDto {
            position,
            anchor: position,
            markdown: text.to_string(),
        },
    )?;
    Ok(())
}

fn html(env: &mut Env, position: i64, text: &str) -> Result<()> {
    document_editing_controller::insert_html_at_position(
        &env.0,
        &env.1,
        &mut env.2,
        None,
        &InsertHtmlAtPositionDto {
            position,
            anchor: position,
            html: text.to_string(),
        },
    )?;
    Ok(())
}

fn djot(env: &mut Env, position: i64, text: &str) -> Result<()> {
    document_editing_controller::insert_djot_at_position(
        &env.0,
        &env.1,
        &mut env.2,
        None,
        &InsertDjotAtPositionDto {
            position,
            anchor: position,
            djot: text.to_string(),
        },
    )?;
    Ok(())
}

fn affixes(items: &[Item]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|i| {
            let (_, p, s) = i.list.clone().expect("a list item");
            (p, s)
        })
        .collect()
}

fn pair(prefix: &str, suffix: &str) -> (String, String) {
    (prefix.to_string(), suffix.to_string())
}

// ─── Delimiters ──────────────────────────────────────────────────────

#[test]
fn a_markdown_paste_keeps_the_delimiter_on_every_path() -> Result<()> {
    for (source, expected) in [("1) a\n2) b", ")"), ("1. a\n2. b", ".")] {
        // Over an empty block, and after text: the first block, the blocks between.
        let mut env = setup_with_text("")?;
        markdown(&mut env, 0, source)?;
        assert_eq!(
            affixes(&list_items(&env.0)?),
            vec![pair("", expected); 2],
            "{source:?} into an empty block"
        );
        let mut env = setup_with_text("x")?;
        markdown(&mut env, 1, &format!("\n\n{source}"))?;
        assert_eq!(
            affixes(&list_items(&env.0)?),
            vec![pair("", expected); 2],
            "{source:?} after text"
        );
    }
    // The single-block path.
    let mut env = setup_with_text("")?;
    markdown(&mut env, 0, "1) only")?;
    assert_eq!(affixes(&list_items(&env.0)?), vec![pair("", ")")]);
    Ok(())
}

#[test]
fn a_djot_paste_keeps_the_delimiter_on_every_path() -> Result<()> {
    for (source, expected) in [
        ("1) a\n2) b", pair("", ")")),
        ("(1) a\n(2) b", pair("(", ")")),
    ] {
        let mut env = setup_with_text("")?;
        djot(&mut env, 0, source)?;
        assert_eq!(
            affixes(&list_items(&env.0)?),
            vec![expected.clone(); 2],
            "{source:?}"
        );
        let mut env = setup_with_text("x")?;
        djot(&mut env, 1, &format!("\n\n{source}"))?;
        assert_eq!(
            affixes(&list_items(&env.0)?),
            vec![expected.clone(); 2],
            "{source:?} after text"
        );
    }
    let mut env = setup_with_text("")?;
    djot(&mut env, 0, "(1) only")?;
    assert_eq!(affixes(&list_items(&env.0)?), vec![pair("(", ")")]);
    Ok(())
}

#[test]
fn an_html_paste_gives_a_numbered_list_a_period_on_every_path() -> Result<()> {
    let two = "<ol><li>a</li><li>b</li></ol>";
    let mut env = setup_with_text("")?;
    html(&mut env, 0, two)?;
    assert_eq!(
        affixes(&list_items(&env.0)?),
        vec![pair("", "."); 2],
        "head over an empty block"
    );
    let mut env = setup_with_text("x")?;
    html(&mut env, 1, two)?;
    assert_eq!(
        affixes(&list_items(&env.0)?),
        vec![pair("", "."); 2],
        "after text"
    );
    let mut env = setup_with_text("")?;
    html(&mut env, 0, "<ol><li>only</li></ol>")?;
    assert_eq!(
        affixes(&list_items(&env.0)?),
        vec![pair("", ".")],
        "a single item"
    );
    let mut env = setup_with_text("")?;
    html(&mut env, 0, "<ul><li>a</li><li>b</li></ul>")?;
    assert_eq!(
        affixes(&list_items(&env.0)?),
        vec![pair("", ""); 2],
        "a bullet list"
    );
    Ok(())
}

/// HTML says nothing of a delimiter: items pasted into a numbered list of the same style join
/// it and wear its delimiter, whatever it is.
#[test]
fn html_pasted_into_a_numbered_list_joins_it_and_takes_its_delimiter() -> Result<()> {
    for (setup, expected) in [
        (("markdown", "1) a\n2) b"), pair("", ")")),
        (("djot", "(1) a\n(2) b"), pair("(", ")")),
        (("markdown", "1. a\n2. b"), pair("", ".")),
        // An older list stored with no delimiter.
        (("no delimiter", "1. a\n2. b"), pair("", "")),
    ] {
        let mut env = setup_with_text("")?;
        match setup.0 {
            "djot" => djot(&mut env, 0, setup.1)?,
            _ => markdown(&mut env, 0, setup.1)?,
        }
        if setup.0 == "no delimiter" {
            for id in get_block_ids(&env.0)? {
                let block = block_controller::get(&env.0, &id)?.unwrap();
                let Some(list_id) = block.list else {
                    continue;
                };
                let list = list_controller::get(&env.0, &list_id)?.unwrap();
                list_controller::update(
                    &env.0,
                    &env.1,
                    &mut env.2,
                    None,
                    &UpdateListDto {
                        id: list.id,
                        created_at: list.created_at,
                        updated_at: list.updated_at,
                        style: list.style,
                        indent: list.indent,
                        prefix: list.prefix,
                        suffix: String::new(),
                        start: list.start,
                    },
                )?;
                break;
            }
        }
        // The caret at the end of the first item (an empty block comes before the list).
        html(&mut env, 2, "<ol><li>x</li><li>y</li></ol>")?;
        let found = list_items(&env.0)?;
        let texts: Vec<_> = found.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["a", "x", "y", "b"], "{setup:?}");
        let ids: Vec<_> = found.iter().map(|i| i.list.clone().unwrap().0).collect();
        assert!(
            ids.iter().all(|id| *id == ids[0]),
            "{setup:?}: one list, got {ids:?}"
        );
        assert_eq!(affixes(&found), vec![expected.clone(); 4], "{setup:?}");
    }
    Ok(())
}

/// A bullet list next to a pasted numbered one is not joined.
#[test]
fn html_pasted_next_to_a_bullet_list_stays_its_own_list() -> Result<()> {
    let mut env = setup_with_text("")?;
    markdown(&mut env, 0, "- a\n- b")?;
    html(&mut env, 2, "<ol><li>x</li><li>y</li></ol>")?;
    let found = list_items(&env.0)?;
    let numbered: Vec<_> = found
        .iter()
        .filter(|i| i.list.clone().unwrap().2 == ".")
        .collect();
    assert_eq!(numbered.len(), 2);
    Ok(())
}
