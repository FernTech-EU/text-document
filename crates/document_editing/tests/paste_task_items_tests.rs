//! What `insert_markdown_at_position`, `insert_html_at_position` and
//! `insert_djot_at_position` give the task items they paste: the checkbox, on each of the
//! paths a paste takes (a single block, the first block over an empty one, the blocks
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
    DbContext, EventHub, block_controller, get_block_ids, list_controller, setup_with_text,
};

use common::entities::MarkerType;
use common::undo_redo::UndoRedoManager;
use std::sync::Arc;

struct Item {
    /// The list id, prefix and suffix.
    list: Option<(u64, String, String)>,
    marker: Option<MarkerType>,
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
            list,
            marker: block.fmt_marker.clone(),
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

// ─── Task items ──────────────────────────────────────────────────────

fn markers(items: &[Item]) -> Vec<Option<MarkerType>> {
    items.iter().map(|i| i.marker.clone()).collect()
}

const C: Option<MarkerType> = Some(MarkerType::Checked);
const U: Option<MarkerType> = Some(MarkerType::Unchecked);

#[test]
fn a_markdown_paste_keeps_the_task_boxes_on_every_path() -> Result<()> {
    let mut env = setup_with_text("")?;
    markdown(&mut env, 0, "- [x] a\n- [ ] b")?;
    assert_eq!(markers(&list_items(&env.0)?), [C, U], "into an empty block");
    let mut env = setup_with_text("x")?;
    markdown(&mut env, 1, "\n\n- [x] a\n- [ ] b\n\ntail")?;
    assert_eq!(markers(&list_items(&env.0)?), [C, U], "after text");
    let mut env = setup_with_text("")?;
    markdown(&mut env, 0, "- [x] only")?;
    assert_eq!(markers(&list_items(&env.0)?), [C], "a single item");
    Ok(())
}

#[test]
fn a_djot_paste_keeps_the_task_boxes_on_every_path() -> Result<()> {
    let mut env = setup_with_text("")?;
    djot(&mut env, 0, "- [x] a\n- [ ] b")?;
    assert_eq!(markers(&list_items(&env.0)?), [C, U], "into an empty block");
    let mut env = setup_with_text("x")?;
    djot(&mut env, 1, "\n\n- [x] a\n- [ ] b\n\ntail")?;
    assert_eq!(markers(&list_items(&env.0)?), [C, U], "after text");
    let mut env = setup_with_text("")?;
    djot(&mut env, 0, "- [x] only")?;
    assert_eq!(markers(&list_items(&env.0)?), [C], "a single item");
    Ok(())
}

#[test]
fn an_html_paste_keeps_the_task_boxes_on_every_path() -> Result<()> {
    let two =
        r#"<ul><li><input type="checkbox" checked> a</li><li><input type="checkbox"> b</li></ul>"#;
    let mut env = setup_with_text("")?;
    html(&mut env, 0, two)?;
    assert_eq!(
        markers(&list_items(&env.0)?),
        [C, U],
        "head over an empty block"
    );
    let mut env = setup_with_text("x")?;
    html(&mut env, 1, two)?;
    assert_eq!(markers(&list_items(&env.0)?), [C, U], "after text");
    let mut env = setup_with_text("")?;
    html(
        &mut env,
        0,
        r#"<ul><li><input type="checkbox" checked> only</li></ul>"#,
    )?;
    assert_eq!(markers(&list_items(&env.0)?), [C], "a single item");
    // Nested: the parent and the child each wear their own.
    let mut env = setup_with_text("")?;
    html(
        &mut env,
        0,
        r#"<ul><li><input type="checkbox" checked> a<ul><li><input type="checkbox"> b</li></ul></li></ul>"#,
    )?;
    assert_eq!(markers(&list_items(&env.0)?), [C, U], "nested");
    Ok(())
}
