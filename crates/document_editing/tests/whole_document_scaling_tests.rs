// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Replacing, pasting or deleting a whole document must cost work in proportion to its size.
//!
//! A host restores a past version of a text by selecting all of it and inserting the old
//! content, splits a scene by inserting the extracted half into a fresh document, and a
//! writer pastes or deletes whole chapters. Each of those used to create, or remove, every
//! block one at a time with its owner: every such call rewrites the owner's whole list,
//! fetches every entity it names to validate it, and announces the whole list in an event.
//! One call per paragraph made N paragraphs cost N(N+1)/2 of those, so restoring a version
//! of a long chapter froze the editor for seconds. The lists a paste creates, the frames a
//! deletion empties and the cells of a table it covers were handed over the same way.
//!
//! These guards count that work instead of timing it. Each owner-list write announces the
//! list it wrote in its event payload (`"Blocks:1,2,3"`), and nothing else on the direct
//! access channel carries a payload, so the ids those payloads carry are exactly the ids the
//! relationship writes had to rewrite and validate. Doubling the input must about double
//! that count; before the fix it quadrupled. The count is deterministic, so the guard cannot
//! flake on a loaded machine. Emptying a text that holds many small tables removed each
//! table's cells, cell frames, anchor and the table itself in calls of their own, each
//! rewriting the document's whole frame or table list: the same quadratic work, per table.
//!
//! The rope and its offset index emit no events, so a return to per-block work there cannot
//! be counted: `public_api`'s `perf_whole_document_replace` times it instead.

extern crate text_document_editing as document_editing;

use anyhow::Result;
use common::event::{Event, Origin};
use common::types::EntityId;
use document_editing::document_editing_controller as editing;
use document_editing::{DeleteTextDto, InsertFragmentDto, InsertTableDto, WrapBlocksInFrameDto};
use serde_json::{Value, json};
use test_harness::{DbContext, get_block_ids, setup_with_text};

const UNITS: usize = 200;

/// Linear work doubles with the input (2.0 plus small constants); the per-block owner writes
/// made it quadruple (3.9 and more at these sizes). The count is exact, so the bound needs no
/// allowance for noise, only for the constants.
const MAX_RATIO: f64 = 2.5;

/// Ids carried by the relationship writes among `events`. Each write announces the whole
/// list it wrote as `"Field:id,id,…"`; no other direct-access event has a payload.
fn relationship_ids_written(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event.origin, Origin::DirectAccess(_)))
        .filter_map(|event| event.data.as_deref())
        .filter_map(|data| data.split_once(':'))
        .map(|(_, ids)| ids.split(',').filter(|id| !id.is_empty()).count())
        .sum()
}

fn paragraph(i: usize) -> String {
    format!("Paragraph {i} of the chapter")
}

fn text_element(text: &str) -> Value {
    json!({
        "content": {"Text": text},
        "fmt_font_family": null, "fmt_font_point_size": null, "fmt_font_weight": null,
        "fmt_font_bold": null, "fmt_font_italic": null, "fmt_font_underline": null,
        "fmt_font_overline": null, "fmt_font_strikeout": null, "fmt_letter_spacing": null,
        "fmt_word_spacing": null, "fmt_anchor_href": null, "fmt_anchor_names": [],
        "fmt_is_anchor": null, "fmt_tooltip": null, "fmt_underline_style": null,
        "fmt_vertical_alignment": null
    })
}

/// A fragment block. A heading level keeps it from counting as inline-only, so the first and
/// last blocks of a fragment are blocks of their own rather than merged into their neighbours.
fn fragment_block(text: &str, list: Option<Value>) -> Value {
    json!({
        "plain_text": text,
        "elements": [text_element(text)],
        "heading_level": if list.is_none() { json!(1) } else { Value::Null },
        "list": list,
        "alignment": null, "indent": null, "text_indent": null, "marker": null,
        "top_margin": null, "bottom_margin": null, "left_margin": null, "right_margin": null,
        "tab_positions": []
    })
}

#[derive(Clone, Copy, Debug)]
enum Pasted {
    /// `units` paragraphs: the block-only path.
    Paragraphs,
    /// `units` pairs of a paragraph and a one-item list: one new list per pair.
    Lists,
    /// `units` paragraphs with a two-by-two table halfway: the mixed path.
    ProseWithATable,
    /// `units` paragraphs with a two-by-two table after every tenth: the mixed path, with
    /// a table for every few pages of a restored text.
    ProseWithManyTables,
    /// `units / 10` two-by-two tables and nothing else: the table-only path.
    OnlyTables,
}

fn fragment(pasted: Pasted, units: usize) -> String {
    let mut blocks: Vec<Value> = Vec::new();
    let paragraphs = if let Pasted::OnlyTables = pasted {
        0
    } else {
        units
    };
    for i in 0..paragraphs {
        blocks.push(fragment_block(&paragraph(i), None));
        if let Pasted::Lists = pasted {
            let list = json!({"style": "Disc", "indent": 1, "prefix": "", "suffix": ""});
            blocks.push(fragment_block(&format!("Item {i}"), Some(list)));
        }
    }
    let cell = |row: usize, column: usize| {
        json!({
            "row": row, "column": column, "row_span": 1, "column_span": 1,
            "blocks": [fragment_block(&format!("cell {row} {column}"), None)]
        })
    };
    let table = |block_insert_index: usize| {
        json!({
            "rows": 2, "columns": 2, "block_insert_index": block_insert_index,
            "cells": [cell(0, 0), cell(0, 1), cell(1, 0), cell(1, 1)]
        })
    };
    let tables: Vec<Value> = match pasted {
        Pasted::ProseWithATable => vec![table(units / 2)],
        Pasted::ProseWithManyTables => (1..units / 10).map(|tenth| table(tenth * 10)).collect(),
        Pasted::OnlyTables => (0..units / 10).map(|_| table(0)).collect(),
        Pasted::Paragraphs | Pasted::Lists => Vec::new(),
    };
    json!({"blocks": blocks, "tables": tables}).to_string()
}

/// Characters the first `blocks` lines of `lines` take up, separators included: the
/// position of the next block's start.
fn position_after(lines: &[String], blocks: usize) -> i64 {
    lines[..blocks]
        .iter()
        .map(|line| line.chars().count() as i64 + 1)
        .sum()
}

/// The document's length in the position space a selection addresses: the rope's, which holds
/// every block's text and the separators between them. (`setup_with_text` leaves the
/// separators out of the document's own character count.)
fn document_length(db: &DbContext) -> i64 {
    db.get_store().rope.read().len_chars() as i64
}

fn block_count(db: &DbContext) -> usize {
    db.get_store().blocks.read().len()
}

#[derive(Clone, Copy, Debug)]
enum Edit {
    /// Paste into a one-paragraph document, after its text: a version restore, a split.
    PasteIntoAShortDocument(Pasted),
    /// Paste as many paragraphs again halfway into a document of `units` paragraphs.
    PasteHalfway,
    /// Select all of a document of `units` paragraphs and delete it.
    DeleteAll,
    /// The same, every paragraph followed by a quoted one in a quote frame of its own.
    DeleteAllWithQuotes,
    /// The same, with a table halfway: the deletion crosses its cells.
    DeleteAllAcrossATable,
    /// The same, with a small table after every tenth paragraph: the deletion removes each.
    DeleteAllAcrossManyTables,
}

/// Relationship ids written by `edit` at `units` units, and a check that it did its job.
fn work_of(edit: Edit, units: usize) -> Result<usize> {
    let lines: Vec<String> = match edit {
        Edit::PasteIntoAShortDocument(_) => vec!["Current text.".to_string()],
        Edit::DeleteAllWithQuotes => (0..units)
            .flat_map(|i| [paragraph(i), format!("Quoted {i}")])
            .collect(),
        _ => (0..units).map(paragraph).collect(),
    };
    let (db, ev, mut undo) = setup_with_text(&lines.join("\n"))?;

    match edit {
        Edit::DeleteAllWithQuotes => {
            let block_ids = get_block_ids(&db)?;
            for quoted in block_ids.iter().skip(1).step_by(2) {
                editing::wrap_blocks_in_frame(
                    &db,
                    &ev,
                    &mut undo,
                    None,
                    &WrapBlocksInFrameDto {
                        start_block_id: *quoted as i64,
                        end_block_id: *quoted as i64,
                        position: None,
                        top_margin: None,
                        bottom_margin: None,
                        left_margin: None,
                        right_margin: None,
                        padding: None,
                        border: None,
                        is_blockquote: Some(true),
                    },
                )?;
            }
            assert_eq!(
                db.get_store().frames.read().len(),
                units + 1,
                "one quote frame per unit, plus the root frame"
            );
        }
        Edit::DeleteAllAcrossATable => {
            let halfway = position_after(&lines, units / 2);
            editing::insert_table(
                &db,
                &ev,
                &mut undo,
                None,
                &InsertTableDto {
                    position: halfway,
                    anchor: halfway,
                    rows: 2,
                    columns: 2,
                },
            )?;
        }
        Edit::DeleteAllAcrossManyTables => {
            // From the last to the first, so a table leaves the positions before it alone.
            for tenth in (1..units / 10).rev() {
                let after = position_after(&lines, tenth * 10);
                editing::insert_table(
                    &db,
                    &ev,
                    &mut undo,
                    None,
                    &InsertTableDto {
                        position: after,
                        anchor: after,
                        rows: 2,
                        columns: 2,
                    },
                )?;
            }
            assert_eq!(
                db.get_store().tables.read().len(),
                units / 10 - 1,
                "a table after every tenth paragraph"
            );
        }
        _ => {}
    }

    // The event loop is not running in tests: events wait in the hub's one channel, the
    // setup's own included, so drain those first.
    let events = ev.subscribe_receiver();
    let _ = events.try_iter().count();
    let blocks_before = block_count(&db);
    match edit {
        Edit::PasteIntoAShortDocument(pasted) => {
            let end = position_after(&lines, 1) - 1;
            editing::insert_fragment(
                &db,
                &ev,
                &mut undo,
                None,
                &InsertFragmentDto {
                    position: end,
                    anchor: end,
                    fragment_data: fragment(pasted, units),
                },
            )?;
            let expected = match pasted {
                // Four cell blocks a table.
                Pasted::OnlyTables => 4 * (units / 10),
                _ => units,
            };
            assert!(
                block_count(&db) >= blocks_before + expected,
                "{edit:?}: the pasted blocks are in the document"
            );
        }
        Edit::PasteHalfway => {
            let halfway = position_after(&lines, units / 2);
            editing::insert_fragment(
                &db,
                &ev,
                &mut undo,
                None,
                &InsertFragmentDto {
                    position: halfway,
                    anchor: halfway,
                    fragment_data: fragment(Pasted::Paragraphs, units),
                },
            )?;
            assert!(
                block_count(&db) >= blocks_before + units,
                "{edit:?}: the pasted blocks are in the document"
            );
        }
        Edit::DeleteAll
        | Edit::DeleteAllWithQuotes
        | Edit::DeleteAllAcrossATable
        | Edit::DeleteAllAcrossManyTables => {
            editing::delete_text(
                &db,
                &ev,
                &mut undo,
                None,
                &DeleteTextDto {
                    position: 0,
                    anchor: document_length(&db),
                },
            )?;
            assert!(
                block_count(&db) <= 5,
                "{edit:?}: the deletion removed the paragraphs ({} blocks left of {blocks_before})",
                block_count(&db)
            );
            if let Edit::DeleteAllAcrossManyTables = edit {
                assert!(
                    db.get_store().tables.read().is_empty(),
                    "{edit:?}: the deletion removed every table"
                );
            }
        }
    }
    let written: Vec<Event> = events.try_iter().collect();
    Ok(relationship_ids_written(&written))
}

/// Every edit is measured before anything is asserted, so a failure names all the edits that
/// regressed, not only the first.
fn assert_linear(edits: &[Edit]) -> Result<()> {
    let mut regressed: Vec<String> = Vec::new();
    for &edit in edits {
        let small = work_of(edit, UNITS)?;
        let large = work_of(edit, 2 * UNITS)?;
        assert!(small > 0, "{edit:?}: no relationship write seen");
        let ratio = large as f64 / small as f64;
        let line = format!("{edit:?}: {small} -> {large} ids written ({ratio:.2}x)");
        println!("{line}");
        if ratio > MAX_RATIO {
            regressed.push(line);
        }
    }
    assert!(
        regressed.is_empty(),
        "doubling the input from {UNITS} to {} units more than doubled the ids the relationship \
         writes had to rewrite:\n  {}\nLinear work doubles; a ratio near 4 means a loop is \
         creating or removing children with their owner one at a time again.",
        2 * UNITS,
        regressed.join("\n  "),
    );
    Ok(())
}

#[test]
fn pasting_a_whole_document_writes_owner_lists_in_linear_work() -> Result<()> {
    assert_linear(&[
        Edit::PasteIntoAShortDocument(Pasted::Paragraphs),
        Edit::PasteIntoAShortDocument(Pasted::Lists),
        Edit::PasteIntoAShortDocument(Pasted::ProseWithATable),
        Edit::PasteHalfway,
    ])
}

/// Every table a paste creates, and the anchor frame and the cell frames each table needs,
/// used to be created with the document as its owner, each creation rewriting the
/// document's whole table or frame list: restoring or pasting a text holding a small table
/// every few pages was quadratic in its tables (8,000 paragraphs with 727 tables took 5.6 s
/// to restore and 9.3 s to paste).
#[test]
fn pasting_many_small_tables_writes_owner_lists_in_linear_work() -> Result<()> {
    assert_linear(&[
        Edit::PasteIntoAShortDocument(Pasted::ProseWithManyTables),
        Edit::PasteIntoAShortDocument(Pasted::OnlyTables),
    ])
}

#[test]
fn deleting_a_whole_document_writes_owner_lists_in_linear_work() -> Result<()> {
    assert_linear(&[
        Edit::DeleteAll,
        Edit::DeleteAllWithQuotes,
        Edit::DeleteAllAcrossATable,
        Edit::DeleteAllAcrossManyTables,
    ])
}

/// The pasted blocks reach their frame in document order, where the per-block creation put
/// them: right after the block the paste splits, before the blocks that followed it.
#[test]
fn pasted_blocks_land_in_their_frame_in_document_order() -> Result<()> {
    let lines: Vec<String> = (0..6).map(paragraph).collect();
    let (db, ev, mut undo) = setup_with_text(&lines.join("\n"))?;
    let before = get_block_ids(&db)?;
    // At the end of the third paragraph's text: the paste neither splits nor overwrites it.
    let halfway = position_after(&lines, 3) - 1;
    editing::insert_fragment(
        &db,
        &ev,
        &mut undo,
        None,
        &InsertFragmentDto {
            position: halfway,
            anchor: halfway,
            fragment_data: fragment(Pasted::Paragraphs, 4),
        },
    )?;

    let store = db.get_store();
    let frames = store.frames.read();
    let root = frames
        .values()
        .find(|frame| frame.blocks.contains(&before[0]))
        .expect("the root frame");
    let listed: Vec<EntityId> = root
        .child_order
        .iter()
        .filter(|&&entry| entry > 0)
        .map(|&entry| entry as EntityId)
        .collect();
    assert_eq!(root.blocks, listed, "blocks and child order agree");
    assert_eq!(
        &root.blocks[..3],
        &before[..3],
        "the blocks before the paste"
    );
    assert_eq!(
        &root.blocks[root.blocks.len() - 3..],
        &before[3..],
        "the blocks after the paste"
    );
    let pasted = &root.blocks[3..root.blocks.len() - 3];
    let mut in_creation_order = pasted.to_vec();
    in_creation_order.sort_unstable();
    assert_eq!(pasted, in_creation_order.as_slice(), "pasted in order");
    assert!(
        pasted.iter().all(|id| !before.contains(id)),
        "only new blocks between"
    );
    Ok(())
}
