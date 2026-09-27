// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Loading a document must cost work in proportion to its size, not to its square.
//!
//! Every importer used to create each block, frame, list, table and table cell *with* its
//! owner. That appends the child to the owner's list, and the append rewrites the whole
//! list, fetches every entity it names to validate it, and announces the whole list in an
//! event. One append per paragraph made N paragraphs cost N(N+1)/2 of those: 16,000
//! paragraphs took from 25 to 50 seconds to load in a release build, so a novel kept in
//! one document froze the editor on open. Replacing a document's content paid a second
//! quadratic bill on the way out, removing the old frames one at a time.
//!
//! These guards count that work instead of timing it. Each owner-list write announces the
//! list it wrote in its event payload (`"Blocks:1,2,3"`), and nothing else on the direct
//! access channel carries a payload, so the ids those payloads carry are exactly the ids
//! the relationship writes had to rewrite and validate. Doubling the input must about
//! double that count; before the fix it quadrupled. The count is deterministic, so the
//! guard cannot flake on a loaded machine.
//!
//! The last test pins what the importers now do differently: children are created without
//! an owner and handed over in one write per owner at the end, so it checks that every
//! one of them arrives, in creation order.

extern crate text_document_io as document_io;

use common::event::{Event, Origin};
use common::long_operation::{LongOperationManager, OperationStatus};
use common::parser_tools::DjotImportOptions;
use common::types::EntityId;
use document_io::document_io_controller as io;
use document_io::{ImportDjotDto, ImportHtmlDto, ImportMarkdownDto, ImportPlainTextDto};
use std::sync::Arc;
use test_harness::{DbContext, EventHub, setup};

#[derive(Clone, Copy, Debug)]
enum Importer {
    DjotSync,
    DjotLongOperation,
    Markdown,
    Html,
    PlainText,
}

/// What each unit of the generated source becomes.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// A paragraph in the root frame.
    Paragraphs,
    /// A paragraph, then a quoted paragraph: one blockquote frame per pair.
    Quotes,
    /// A paragraph, then a one-item list: one list per pair.
    Lists,
    /// A paragraph, then a two-by-two table: one table, four cells, five frames.
    Tables,
    /// A paragraph with a note: one footnote frame each.
    Footnotes,
}

const RICH_SHAPES: [Shape; 5] = [
    Shape::Paragraphs,
    Shape::Quotes,
    Shape::Lists,
    Shape::Tables,
    Shape::Footnotes,
];

/// HTML has no footnote syntax of its own, and plain text only has paragraphs.
fn shapes_of(importer: Importer) -> &'static [Shape] {
    match importer {
        Importer::DjotSync | Importer::DjotLongOperation | Importer::Markdown => &RICH_SHAPES,
        Importer::Html => &RICH_SHAPES[..4],
        Importer::PlainText => &RICH_SHAPES[..1],
    }
}

fn source(importer: Importer, shape: Shape, units: usize) -> String {
    let mut out = String::new();
    for i in 0..units {
        let unit = match (importer, shape) {
            (Importer::PlainText, _) => format!("Paragraph {i}\n"),
            (Importer::Html, Shape::Paragraphs) => format!("<p>Paragraph {i}</p>\n"),
            (Importer::Html, Shape::Quotes) => {
                format!("<p>Paragraph {i}</p><blockquote><p>Quoted {i}</p></blockquote>\n")
            }
            (Importer::Html, Shape::Lists) => {
                format!("<p>Paragraph {i}</p><ul><li>Item {i}</li></ul>\n")
            }
            (Importer::Html, Shape::Tables) => format!(
                "<p>Paragraph {i}</p><table><tr><td>a{i}</td><td>b</td></tr>\
                 <tr><td>c</td><td>d</td></tr></table>\n"
            ),
            (Importer::Html, Shape::Footnotes) => unreachable!("HTML has no footnotes"),
            (_, Shape::Paragraphs) => format!("Paragraph {i}\n\n"),
            (_, Shape::Quotes) => format!("Paragraph {i}\n\n> Quoted {i}\n\n"),
            (_, Shape::Lists) => format!("Paragraph {i}\n\n- Item {i}\n\n"),
            (Importer::Markdown, Shape::Tables) => {
                format!("Paragraph {i}\n\n| a{i} | b |\n|---|---|\n| c | d |\n\n")
            }
            (_, Shape::Tables) => format!("Paragraph {i}\n\n| a{i} | b |\n| c | d |\n\n"),
            (_, Shape::Footnotes) => format!("Paragraph {i}[^n{i}]\n\n[^n{i}]: Note {i}.\n\n"),
        };
        out.push_str(&unit);
    }
    out
}

fn wait_for(manager: &LongOperationManager, op_id: &str) {
    while let Some(OperationStatus::Running) = manager.get_operation_status(op_id) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        manager.get_operation_status(op_id),
        Some(OperationStatus::Completed),
        "the import did not complete"
    );
}

fn import(db: &DbContext, ev: &Arc<EventHub>, importer: Importer, text: &str) {
    let djot = || ImportDjotDto {
        djot_text: text.to_string(),
        options: DjotImportOptions::default(),
    };
    let mut manager = LongOperationManager::new();
    match importer {
        Importer::DjotSync => {
            io::import_djot_sync(db, ev, &djot()).expect("djot import");
        }
        Importer::DjotLongOperation => {
            let op = io::import_djot(db, ev, &mut manager, &djot()).expect("djot import");
            wait_for(&manager, &op);
        }
        Importer::Markdown => {
            let dto = ImportMarkdownDto {
                markdown_text: text.to_string(),
            };
            let op = io::import_markdown(db, ev, &mut manager, &dto).expect("markdown import");
            wait_for(&manager, &op);
        }
        Importer::Html => {
            let dto = ImportHtmlDto {
                html_text: text.to_string(),
            };
            let op = io::import_html(db, ev, &mut manager, &dto).expect("html import");
            wait_for(&manager, &op);
        }
        Importer::PlainText => {
            let dto = ImportPlainTextDto {
                plain_text: text.to_string(),
            };
            io::import_plain_text(db, ev, &dto).expect("plain text import");
        }
    }
}

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

/// Relationship ids written while importing `units` of `shape`, into a fresh document or,
/// with `reload`, over a document already holding the same content.
fn work_of(importer: Importer, shape: Shape, units: usize, reload: bool) -> usize {
    let (db, ev, _) = setup().expect("setup");
    // The event loop is not running in tests: events wait in the hub's channel.
    let events = ev.subscribe_receiver();
    let text = source(importer, shape, units);
    if reload {
        import(&db, &ev, importer, &text);
    }
    let _ = events.try_iter().count();
    import(&db, &ev, importer, &text);
    let written: Vec<Event> = events.try_iter().collect();
    relationship_ids_written(&written)
}

const UNITS: usize = 200;

/// Linear work doubles with the input (2.0 plus small constants); the quadratic append
/// made it quadruple (3.99 for the paragraphs at these sizes). The count is exact, so the
/// bound needs no allowance for noise, only for the constants.
const MAX_RATIO: f64 = 2.5;

/// Every shape is measured before anything is asserted, so a failure names all the
/// shapes that regressed, not only the first.
fn assert_linear(importer: Importer, reload: bool) {
    let label = if reload { " (re-import)" } else { "" };
    let mut regressed: Vec<String> = Vec::new();
    for &shape in shapes_of(importer) {
        let small = work_of(importer, shape, UNITS, reload);
        let large = work_of(importer, shape, 2 * UNITS, reload);
        assert!(
            small > 0,
            "{importer:?} {shape:?}{label}: no relationship write seen"
        );
        let ratio = large as f64 / small as f64;
        let line =
            format!("{importer:?} {shape:?}{label}: {small} -> {large} ids written ({ratio:.2}x)");
        println!("{line}");
        if ratio > MAX_RATIO {
            regressed.push(line);
        }
    }
    assert!(
        regressed.is_empty(),
        "doubling the input from {UNITS} to {} units more than doubled the ids the relationship \
         writes had to rewrite:\n  {}\nLinear work doubles; a ratio near 4 means a loop is \
         appending children to their owner one at a time again (a create with an owner, or \
         one frame removed at a time).",
        2 * UNITS,
        regressed.join("\n  "),
    );
}

#[test]
fn djot_import_writes_owner_lists_in_linear_work() {
    assert_linear(Importer::DjotSync, false);
}

#[test]
fn djot_long_operation_import_writes_owner_lists_in_linear_work() {
    assert_linear(Importer::DjotLongOperation, false);
}

#[test]
fn markdown_import_writes_owner_lists_in_linear_work() {
    assert_linear(Importer::Markdown, false);
}

#[test]
fn html_import_writes_owner_lists_in_linear_work() {
    assert_linear(Importer::Html, false);
}

#[test]
fn plain_text_import_writes_owner_lists_in_linear_work() {
    assert_linear(Importer::PlainText, false);
}

#[test]
fn reimport_over_a_loaded_document_writes_owner_lists_in_linear_work() {
    for importer in [
        Importer::DjotSync,
        Importer::Markdown,
        Importer::Html,
        Importer::PlainText,
    ] {
        assert_linear(importer, true);
    }
}

// ─── Deferred ownership is complete ─────────────────────────────────

const DJOT_MIXED: &str = "# Title\n\nOpening *line*.\n\n\
> Quote one\n>\n> > Quote two\n> >\n> > - x\n> > - y\n> >\n> > | q | r |\n> > | s | t |\n>\n> Back.\n\n\
1. first\n2. second\n\n\
| h1 | h2 |\n| c1 | c2 |\n\n\
Noted[^a] twice[^b].\n\n[^a]: First.\n\n[^b]: Second, two paragraphs.\n\n    Continued.\n\n\
Last line.\n";

const MARKDOWN_MIXED: &str = "# Title\n\nOpening *line*.\n\n\
> Quote one\n>\n> > Quote two\n> >\n> > - x\n> > - y\n>\n> Back.\n\n\
1. first\n2. second\n\n\
| h1 | h2 |\n|----|----|\n| c1 | c2 |\n\n\
Noted[^a].\n\n[^a]: The note.\n\nLast line.\n";

const HTML_MIXED: &str = "<h1>Title</h1><p>Opening <i>line</i>.</p>\
<blockquote><p>Quote one</p><blockquote><p>Quote two</p><ul><li>x</li><li>y</li></ul>\
</blockquote><p>Back.</p></blockquote>\
<ol><li>first</li><li>second</li></ol>\
<table><tr><td>c1</td><td>c2</td></tr><tr><td>c3</td><td>c4</td></tr></table><p>Last line.</p>";

fn sorted(ids: impl Iterator<Item = EntityId>) -> Vec<EntityId> {
    let mut ids: Vec<EntityId> = ids.collect();
    ids.sort_unstable();
    ids
}

/// Every frame, list, table, cell and block an import creates reaches its owner's list,
/// once, in creation order (ids are handed out in creation order).
fn assert_every_child_owned(db: &DbContext, what: &str) {
    let store = db.get_store();
    let documents = store.documents.read();
    assert_eq!(documents.len(), 1, "{what}: one document");
    let document = documents.values().next().expect("the document");

    let frames = store.frames.read();
    let lists = store.lists.read();
    let tables = store.tables.read();
    let cells = store.table_cells.read();
    let blocks = store.blocks.read();

    assert_eq!(
        document.frames,
        sorted(frames.keys().copied()),
        "{what}: document frames"
    );
    assert_eq!(
        document.lists,
        sorted(lists.keys().copied()),
        "{what}: document lists"
    );
    assert_eq!(
        document.tables,
        sorted(tables.keys().copied()),
        "{what}: document tables"
    );

    let mut owned_cells: Vec<EntityId> = Vec::new();
    for table in tables.values() {
        let mut in_order = table.cells.clone();
        in_order.sort_unstable();
        assert_eq!(table.cells, in_order, "{what}: table {} cells", table.id);
        assert_eq!(
            table.cells.len() as i64,
            table.rows * table.columns,
            "{what}: table {} holds every cell",
            table.id
        );
        owned_cells.extend(&table.cells);
    }
    owned_cells.sort_unstable();
    assert_eq!(
        owned_cells,
        sorted(cells.keys().copied()),
        "{what}: every cell owned"
    );

    let mut owned_blocks: Vec<EntityId> = Vec::new();
    for frame in frames.values() {
        let listed: Vec<EntityId> = frame
            .child_order
            .iter()
            .filter(|&&entry| entry > 0)
            .map(|&entry| entry as EntityId)
            .collect();
        assert_eq!(
            frame.blocks, listed,
            "{what}: frame {} owns the blocks its child order lists",
            frame.id
        );
        owned_blocks.extend(&frame.blocks);
    }
    owned_blocks.sort_unstable();
    assert_eq!(
        owned_blocks,
        sorted(blocks.keys().copied()),
        "{what}: every block owned once"
    );

    drop((documents, frames, lists, tables, cells, blocks));
    assert!(
        common::database::rope_helpers::rope_positions_match_flow(store),
        "{what}: every block is mirrored in the rope"
    );
}

#[test]
fn every_created_entity_reaches_its_owner_in_creation_order() {
    let cases: [(Importer, &str); 4] = [
        (Importer::DjotSync, DJOT_MIXED),
        (Importer::DjotLongOperation, DJOT_MIXED),
        (Importer::Markdown, MARKDOWN_MIXED),
        (Importer::Html, HTML_MIXED),
    ];
    for (importer, text) in cases {
        let (db, ev, _) = setup().expect("setup");
        import(&db, &ev, importer, text);
        assert_every_child_owned(&db, &format!("{importer:?}"));

        // Lists and tables outlive a re-import, frames do not: the second import appends
        // its lists and tables to the ones the document already holds.
        import(&db, &ev, importer, text);
        assert_every_child_owned(&db, &format!("{importer:?} re-imported"));
    }

    let (db, ev, _) = setup().expect("setup");
    import(&db, &ev, Importer::PlainText, "one\ntwo\n\nfour");
    assert_every_child_owned(&db, "PlainText");
}
