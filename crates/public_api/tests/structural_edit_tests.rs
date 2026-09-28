// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Structural edits keep the document's model whole.
//!
//! A document is held twice: as entities (frames listing blocks, tables listing cells, per
//! block the format runs and the anchors of its images and footnote references) and as one
//! rope with an offset index, which every position a caller deals in addresses. Each edit
//! has to change both the same way. When one side is left behind, nothing fails at the
//! edit: the next export slices a block's text at an offset the block no longer reaches and
//! panics, or a cursor lands in the wrong paragraph, or a save writes text in another
//! order than the editor shows. That is how restoring a version of a scene holding footnote
//! references crashed the next save, and how a paste holding a table left every position
//! after it wrong.
//!
//! [`check`] states the model: every block in exactly one frame and in the rope, the rope's
//! order the frames' order with every note's body after the main text, runs and anchors inside
//! their block's text and every anchor on a `U+FFFC` of its own, no image or note reference in
//! a code block and no code block in a table cell, the document naming exactly the tables and
//! lists the store holds, and every export (and a reload of the document's own Djot) reading
//! the same text, the same note references and the same footnote bodies. The random test drives
//! pastes of Djot, Markdown and HTML fragments holding footnotes, quotations (epigraphs, lists
//! in quotations, nested ones), code blocks, lists and tables (ragged ones among them, and one
//! too ragged to complete), copies of the document's own selections pasted back, deletions
//! (whole tables among them), Backspace and Delete anywhere and at block starts, typing,
//! formatted typing and replacement over ranges, paragraph breaks, images, footnote references,
//! table rows, columns, cells and whole tables, quotation, list and code-block changes (list
//! items moved a level in and out as an editor's Tab does among them), headings, italics,
//! replace-all, loads in place through every setter, and undo/redo over documents holding all
//! of those. Range ends favour the boundaries of blocks, table anchors and note bodies. It
//! checks the model after every step and, around each edit of a range, that another cursor past
//! the range still stands before the same text; that a text put back over everything reads as
//! the same text loaded through the setter for its syntax, when neither holds a note's body;
//! that a copy starting in the text holds none of a body's words and a paste changes no body
//! but the one it goes into; and that a load in place leaves no history and no list of the
//! content it replaced. Its size is bounded so it runs in every build; set
//! `STRUCTURAL_EDIT_SEEDS` (and `STRUCTURAL_EDIT_STEPS`) to run it longer, in a release build
//! for speed, and `STRUCTURAL_EDIT_FIRST_SEED` with `STRUCTURAL_EDIT_TRACE` to replay one
//! failing seed step by step.
//!
//! The tests after it are the defects that differential found, each named after what broke.

use common::database::block_offset_index::OffsetMarker;
use common::entities::{Block, Document, Frame, Table, TableCell};
use common::format_runs::{FootnoteRefAnchor, FormatRun, ImageAnchor, check_well_formed};
use std::collections::{HashMap, HashSet};
use text_document::{
    DocumentFragment, FlowElementSnapshot, MoveMode, MoveOperation, SelectionType, TextDocument,
    TextFormat,
};

const SENTINEL: char = '\u{FFFC}';

// ── The model ────────────────────────────────────────────────────────────────

/// What the frames and the rope index hold, in order: a block, or a table's anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Item {
    Block(u64),
    Anchor(u64),
}

/// Run `f`, turning a panic into an error naming `what`.
fn guarded<T>(what: &str, f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| {
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        format!("{what} panicked: {message}")
    })
}

/// A store table, copied out of its lock.
fn copied<'a, V: Clone + 'a>(table: impl IntoIterator<Item = (&'a u64, &'a V)>) -> HashMap<u64, V> {
    table
        .into_iter()
        .map(|(id, value)| (*id, value.clone()))
        .collect()
}

/// Every invariant below, the first disagreement as an error.
fn check(doc: &TextDocument) -> Result<(), String> {
    let djot = guarded("to_djot", || doc.to_djot())?.map_err(|e| format!("to_djot: {e}"))?;
    let plain = guarded("to_plain_text", || doc.to_plain_text())?
        .map_err(|e| format!("to_plain_text: {e}"))?;
    guarded("to_html", || doc.to_html())?.map_err(|e| format!("to_html: {e}"))?;
    guarded("to_markdown", || doc.to_markdown())?.map_err(|e| format!("to_markdown: {e}"))?;
    let addressable = guarded("to_addressable_text", || doc.to_addressable_text())?
        .map_err(|e| format!("to_addressable_text: {e}"))?;
    check_store(doc)?;
    check_flow(doc, &addressable)?;

    // A footnote reference is reported where its sentinel is.
    let chars: Vec<char> = addressable.chars().collect();
    for (at, label) in guarded("footnote_references", || doc.footnote_references())? {
        if chars.get(at) != Some(&SENTINEL) {
            return Err(format!(
                "footnote reference {label} reported at {at}, where the text holds {:?}",
                chars.get(at)
            ));
        }
    }

    // A code block is verbatim text: the save writes its characters and nothing else. No
    // edit may leave an image or a note's reference in one.
    if let Some(block) = code_block_holding_objects(doc) {
        return Err(format!(
            "code block {block} holds an image or a footnote reference, which the saved Djot \
             leaves out: {djot:?}"
        ));
    }

    // A pipe table writes a cell's paragraphs as one line of inline text: a code block in a
    // cell is one in the editor only. No edit may make one.
    if let Some(block) = code_block_in_a_cell(doc) {
        return Err(format!(
            "block {block} of a table cell is a code block, which the saved Djot leaves out: \
             {djot:?}"
        ));
    }

    // A table nested in a table cell is more than a pipe table can say: the save drops it.
    // No edit may build one.
    if table_in_a_cell(doc) {
        return Err(format!(
            "a table cell holds a table, which the saved Djot leaves out: {djot:?}"
        ));
    }

    // The document's own Djot reads back as the same words, in the same order. Djot has no
    // empty paragraph and a pipe table cell holds one line, so the comparison is word by
    // word.
    let reloaded = TextDocument::new();
    guarded("reload", || reloaded.set_djot_sync(&djot))?.map_err(|e| format!("reload: {e}"))?;
    let again = guarded("reloaded to_plain_text", || reloaded.to_plain_text())?
        .map_err(|e| e.to_string())?;
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    if words(&again) != words(&plain) {
        return Err(format!(
            "the reloaded Djot reads differently:\n djot {djot:?}\n plain {plain:?}\n again \
             {again:?}"
        ));
    }
    // And the same references to notes, in the same order.
    let labels = |doc: &TextDocument| -> Vec<String> {
        doc.footnote_references()
            .into_iter()
            .map(|(_, label)| label)
            .collect()
    };
    if labels(doc) != labels(&reloaded) {
        return Err(format!(
            "the reloaded Djot holds other note references:\n djot {djot:?}\n references {:?}\n \
             again {:?}",
            labels(doc),
            labels(&reloaded)
        ));
    }
    // The plain text leaves footnote bodies out, so they are compared on their own, note by
    // note.
    let (bodies, bodies_again) = (note_bodies(doc), note_bodies(&reloaded));
    if bodies != bodies_again {
        return Err(format!(
            "the reloaded Djot holds other notes:\n djot {djot:?}\n notes {bodies:?}\n again \
             {bodies_again:?}"
        ));
    }
    check_store(&reloaded).map_err(|e| format!("reloaded: {e}"))
}

/// Each footnote definition's body, by label: the words of every block it holds, at any
/// depth, in the order its frames list them. Bodies without a word are left out.
fn note_bodies(doc: &TextDocument) -> std::collections::BTreeMap<String, String> {
    let store = doc.rope_store_for_test();
    let rope = store.rope.read().to_string();
    let offsets = store.block_offsets.read().clone();
    let frames: HashMap<u64, Frame> = copied(store.frames.read().iter());
    let tables: HashMap<u64, Table> = copied(store.tables.read().iter());
    let cells: HashMap<u64, TableCell> = copied(store.table_cells.read().iter());
    let text_of = |block_id: u64| -> String {
        let Some((start, end)) = offsets.range_of_block(block_id) else {
            return String::new();
        };
        let (start, end) = (start as usize, (end as usize).min(rope.len()));
        rope.get(start..end).unwrap_or_default().to_string()
    };
    fn walk(
        frame_id: u64,
        frames: &HashMap<u64, Frame>,
        tables: &HashMap<u64, Table>,
        cells: &HashMap<u64, TableCell>,
        text_of: &dyn Fn(u64) -> String,
        out: &mut Vec<String>,
    ) {
        let Some(frame) = frames.get(&frame_id) else {
            return;
        };
        if let Some(table) = frame.table.and_then(|id| tables.get(&id)) {
            let mut table_cells: Vec<&TableCell> =
                table.cells.iter().filter_map(|id| cells.get(id)).collect();
            table_cells.sort_by_key(|cell| (cell.row, cell.column));
            for cell in table_cells {
                if let Some(cell_frame) = cell.cell_frame {
                    walk(cell_frame, frames, tables, cells, text_of, out);
                }
            }
            return;
        }
        for entry in &frame.child_order {
            if *entry > 0 {
                out.push(text_of(*entry as u64));
            } else if *entry < 0 {
                walk((-entry) as u64, frames, tables, cells, text_of, out);
            }
        }
    }
    let mut bodies = std::collections::BTreeMap::new();
    for frame in frames.values() {
        if let Some(label) = &frame.footnote_label {
            let mut texts = Vec::new();
            walk(frame.id, &frames, &tables, &cells, &text_of, &mut texts);
            let words = texts
                .join(" ")
                .replace(SENTINEL, " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            // A body with no text is no note at all: the writer leaves it out.
            if !words.is_empty() {
                bodies.insert(label.clone(), words);
            }
        }
    }
    bodies
}

/// Whether some table cell holds a table.
fn table_in_a_cell(doc: &TextDocument) -> bool {
    let store = doc.rope_store_for_test();
    let frames: HashMap<u64, Frame> = copied(store.frames.read().iter());
    let cells: HashMap<u64, TableCell> = copied(store.table_cells.read().iter());
    fn holds_table(frame_id: u64, frames: &HashMap<u64, Frame>, depth: usize) -> bool {
        let Some(frame) = frames.get(&frame_id) else {
            return false;
        };
        if depth > 0 && frame.table.is_some() {
            return true;
        }
        frame
            .child_order
            .iter()
            .any(|entry| *entry < 0 && holds_table((-entry) as u64, frames, depth + 1))
    }
    cells
        .values()
        .filter_map(|cell| cell.cell_frame)
        .any(|frame| holds_table(frame, &frames, 0))
}

/// A code block holding an image or a footnote reference, if any. A table cell's block is
/// left out: a pipe table writes a cell as inline text, whatever its block format says, so
/// the save keeps what it holds.
fn code_block_holding_objects(doc: &TextDocument) -> Option<u64> {
    let store = doc.rope_store_for_test();
    let blocks: HashMap<u64, Block> = copied(store.blocks.read().iter());
    let frames: HashMap<u64, Frame> = copied(store.frames.read().iter());
    let in_cells: HashSet<u64> = store
        .table_cells
        .read()
        .values()
        .filter_map(|cell| cell.cell_frame)
        .filter_map(|frame| frames.get(&frame))
        .flat_map(|frame| frame.blocks.iter().copied())
        .collect();
    let images = store.block_images.read();
    let notes = store.block_footnote_refs.read();
    blocks
        .values()
        .filter(|block| block.fmt_is_code_block == Some(true) && !in_cells.contains(&block.id))
        .map(|block| block.id)
        .find(|id| {
            images.get(id).is_some_and(|anchors| !anchors.is_empty())
                || notes.get(id).is_some_and(|anchors| !anchors.is_empty())
        })
}

/// A block of a table cell that is a code block, if any.
fn code_block_in_a_cell(doc: &TextDocument) -> Option<u64> {
    let store = doc.rope_store_for_test();
    let frames: HashMap<u64, Frame> = copied(store.frames.read().iter());
    let blocks = store.blocks.read();
    let cell_frames: Vec<u64> = store
        .table_cells
        .read()
        .values()
        .filter_map(|cell| cell.cell_frame)
        .collect();
    cell_frames
        .iter()
        .filter_map(|frame| frames.get(frame))
        .flat_map(|frame| frame.blocks.iter().copied())
        .find(|id| {
            blocks
                .get(id)
                .is_some_and(|block| block.fmt_is_code_block == Some(true))
        })
}

/// The label of the footnote whose body holds `position`, if one does.
fn note_body_at(doc: &TextDocument, position: usize) -> Option<String> {
    let store = doc.rope_store_for_test();
    let frames: HashMap<u64, Frame> = copied(store.frames.read().iter());
    let rope = store.rope.read();
    let offsets = store.block_offsets.read();
    for frame in frames.values() {
        let Some(label) = &frame.footnote_label else {
            continue;
        };
        let mut pending = vec![frame.id];
        while let Some(frame_id) = pending.pop() {
            let Some(frame) = frames.get(&frame_id) else {
                continue;
            };
            pending.extend(
                frame
                    .child_order
                    .iter()
                    .filter(|entry| **entry < 0)
                    .map(|entry| (-entry) as u64),
            );
            for block in &frame.blocks {
                let Some((start, end, has_successor)) =
                    offsets.range_with_successor(OffsetMarker::Block(*block))
                else {
                    continue;
                };
                let end = if has_successor && end > start {
                    end - 1
                } else {
                    end
                };
                let (start, end) = (
                    rope.byte_to_char(start as usize),
                    rope.byte_to_char(end as usize),
                );
                if (start..=end).contains(&position) {
                    return Some(label.clone());
                }
            }
        }
    }
    None
}

/// The flow a view lays out: block positions increasing, each block's text at its position
/// in the addressable text.
fn check_flow(doc: &TextDocument, addressable: &str) -> Result<(), String> {
    let chars: Vec<char> = addressable.chars().collect();
    let snapshot = guarded("snapshot_flow", || doc.snapshot_flow())?;
    fn walk(
        elements: &[FlowElementSnapshot],
        chars: &[char],
        last: &mut Option<usize>,
    ) -> Result<(), String> {
        let check_block = |block: &text_document::BlockSnapshot,
                           last: &mut Option<usize>|
         -> Result<(), String> {
            if let Some(before) = *last
                && block.position <= before
            {
                return Err(format!(
                    "block {} at {} comes after a block at {before}",
                    block.block_id, block.position
                ));
            }
            *last = Some(block.position);
            let length = block.text.chars().count();
            let there: String = chars
                .get(block.position..block.position + length)
                .map(|slice| slice.iter().collect())
                .unwrap_or_default();
            if there != block.text {
                return Err(format!(
                    "block {} at {} reads {:?}, the addressable text holds {there:?}",
                    block.block_id, block.position, block.text
                ));
            }
            Ok(())
        };
        for element in elements {
            match element {
                FlowElementSnapshot::Block(block) => check_block(block, last)?,
                FlowElementSnapshot::Table(table) => {
                    for cell in &table.cells {
                        for block in &cell.blocks {
                            check_block(block, last)?;
                        }
                    }
                }
                FlowElementSnapshot::Frame(frame) => walk(&frame.elements, chars, last)?,
            }
        }
        Ok(())
    }
    walk(&snapshot.elements, &chars, &mut None)
}

/// The entities and the rope, against each other.
fn check_store(doc: &TextDocument) -> Result<(), String> {
    let store = doc.rope_store_for_test();
    let rope = store.rope.read().to_string();
    let offsets = store.block_offsets.read().clone();
    let documents: Vec<Document> = store.documents.read().values().cloned().collect();
    let frames: HashMap<u64, Frame> = copied(store.frames.read().iter());
    let blocks: HashMap<u64, Block> = copied(store.blocks.read().iter());
    let tables: HashMap<u64, Table> = copied(store.tables.read().iter());
    let cells: HashMap<u64, TableCell> = copied(store.table_cells.read().iter());
    let runs: HashMap<u64, Vec<FormatRun>> = copied(store.format_runs.read().iter());
    let images: HashMap<u64, Vec<ImageAnchor>> = copied(store.block_images.read().iter());
    let notes: HashMap<u64, Vec<FootnoteRefAnchor>> =
        copied(store.block_footnote_refs.read().iter());

    let document = match documents.as_slice() {
        [document] => document.clone(),
        other => return Err(format!("{} documents", other.len())),
    };
    let main = *document.frames.first().ok_or("the document has no frame")?;

    // What the frames say, walked from the main frame and from each footnote definition.
    struct Walk<'a> {
        frames: &'a HashMap<u64, Frame>,
        tables: &'a HashMap<u64, Table>,
        cells: &'a HashMap<u64, TableCell>,
        seen: HashSet<u64>,
        owner: HashMap<u64, u64>,
    }
    impl Walk<'_> {
        fn frame(&mut self, frame_id: u64, out: &mut Vec<Item>) -> Result<(), String> {
            if !self.seen.insert(frame_id) {
                return Err(format!("frame {frame_id} is reached twice"));
            }
            let frame = self
                .frames
                .get(&frame_id)
                .ok_or(format!("frame {frame_id} is missing"))?;
            let listed: HashSet<u64> = frame
                .child_order
                .iter()
                .filter(|entry| **entry > 0)
                .map(|entry| *entry as u64)
                .collect();
            let held: HashSet<u64> = frame.blocks.iter().copied().collect();
            if listed != held {
                return Err(format!(
                    "frame {frame_id} orders blocks {listed:?} but holds {held:?}"
                ));
            }
            if let Some(table_id) = frame.table {
                let table = self
                    .tables
                    .get(&table_id)
                    .ok_or(format!("table {table_id} of frame {frame_id} is missing"))?;
                out.push(Item::Anchor(table_id));
                let mut table_cells = table
                    .cells
                    .iter()
                    .map(|id| self.cells.get(id).ok_or(format!("cell {id} is missing")))
                    .collect::<Result<Vec<_>, _>>()?;
                table_cells.sort_by_key(|cell| (cell.row, cell.column));
                for cell in table_cells {
                    let cell_frame = cell
                        .cell_frame
                        .ok_or(format!("cell {} has no frame", cell.id))?;
                    self.frame(cell_frame, out)?;
                }
                return Ok(());
            }
            for entry in &frame.child_order {
                if *entry > 0 {
                    if self.owner.insert(*entry as u64, frame_id).is_some() {
                        return Err(format!("block {entry} is listed twice"));
                    }
                    out.push(Item::Block(*entry as u64));
                } else if *entry < 0 {
                    self.frame((-entry) as u64, out)?;
                }
            }
            Ok(())
        }
    }
    let mut walk = Walk {
        frames: &frames,
        tables: &tables,
        cells: &cells,
        seen: HashSet::new(),
        owner: HashMap::new(),
    };
    let mut flow: Vec<Item> = Vec::new();
    walk.frame(main, &mut flow)?;
    let mut definitions: Vec<Vec<Item>> = Vec::new();
    for frame_id in &document.frames {
        if frames
            .get(frame_id)
            .is_some_and(|frame| frame.footnote_label.is_some())
        {
            let mut items = Vec::new();
            walk.frame(*frame_id, &mut items)?;
            definitions.push(items);
        }
    }
    let in_definitions: HashSet<Item> = definitions.iter().flatten().copied().collect();
    for frame_id in frames.keys() {
        if !walk.seen.contains(frame_id) {
            return Err(format!("frame {frame_id} is reached from nowhere"));
        }
    }
    for block_id in blocks.keys() {
        if !walk.owner.contains_key(block_id) {
            return Err(format!("block {block_id} is in no frame"));
        }
    }
    for (block_id, frame_id) in &walk.owner {
        if !blocks.contains_key(block_id) {
            return Err(format!(
                "frame {frame_id} lists block {block_id}, which is gone"
            ));
        }
    }
    for table_id in tables.keys() {
        let item = Item::Anchor(*table_id);
        if !flow.contains(&item) && !in_definitions.contains(&item) {
            return Err(format!("table {table_id} is in no flow"));
        }
    }
    // The document names the tables and lists the store holds: a load in place kept the
    // tables, cells and lists of the content it replaced out of the document's own lists.
    // (Edits merging paragraphs can leave a list without an item behind; a load in place
    // leaves none, which `reload_in_place` checks.)
    let stored_tables: HashSet<u64> = tables.keys().copied().collect();
    let named_tables: HashSet<u64> = document.tables.iter().copied().collect();
    if stored_tables != named_tables {
        return Err(format!(
            "the store holds tables {stored_tables:?}, the document names {named_tables:?}"
        ));
    }
    let stored_lists: HashSet<u64> = store.lists.read().keys().copied().collect();
    let named_lists: HashSet<u64> = document.lists.iter().copied().collect();
    if stored_lists != named_lists {
        return Err(format!(
            "the store holds lists {stored_lists:?}, the document names {named_lists:?}"
        ));
    }

    // What the rope says, through its index.
    let entries = offsets.entries.clone();
    if offsets.total_bytes() as usize != rope.len() {
        return Err(format!(
            "the index covers {} bytes, the rope holds {}",
            offsets.total_bytes(),
            rope.len()
        ));
    }
    for (i, (marker, _)) in entries.iter().enumerate() {
        if offsets.position_of(*marker) != Some(i) {
            return Err(format!("the index finds {marker:?} away from entry {i}"));
        }
    }
    let mut texts: HashMap<u64, String> = HashMap::new();
    let mut indexed: Vec<Item> = Vec::new();
    for (i, (marker, start)) in entries.iter().enumerate() {
        let start = *start as usize;
        let end = entries
            .get(i + 1)
            .map_or(rope.len(), |(_, next)| *next as usize);
        if start > end || !rope.is_char_boundary(start) || !rope.is_char_boundary(end) {
            return Err(format!("entry {i} {marker:?} spans {start}..{end}"));
        }
        let content_end = if i + 1 < entries.len() {
            if end == start || rope.as_bytes()[end - 1] != b'\n' {
                return Err(format!(
                    "entry {i} {marker:?} is not followed by a boundary"
                ));
            }
            end - 1
        } else {
            end
        };
        let text = &rope[start..content_end];
        match marker {
            OffsetMarker::Block(block_id) => {
                if !blocks.contains_key(block_id) {
                    return Err(format!("the rope holds block {block_id}, which is gone"));
                }
                texts.insert(*block_id, text.to_string());
                indexed.push(Item::Block(*block_id));
            }
            OffsetMarker::TableAnchor(table_id) => {
                if text != "\u{FFFC}" {
                    return Err(format!("table {table_id}'s anchor holds {text:?}"));
                }
                indexed.push(Item::Anchor(*table_id));
            }
        }
    }
    for block_id in blocks.keys() {
        if !texts.contains_key(block_id) {
            return Err(format!("block {block_id} is not in the rope"));
        }
    }
    let indexed_flow: Vec<Item> = indexed
        .iter()
        .copied()
        .filter(|item| !in_definitions.contains(item))
        .collect();
    if indexed_flow != flow {
        return Err(format!(
            "the rope holds {indexed_flow:?}, the frames order {flow:?}"
        ));
    }
    // The rope holds every note's body after the main text: the loads put them there, and
    // the caret's moves stop at the end of the main text on that ground. No edit may leave
    // a body before a paragraph of the text.
    if let Some(first_body) = indexed
        .iter()
        .position(|item| in_definitions.contains(item))
        && let Some(item) = indexed[first_body..]
            .iter()
            .find(|item| !in_definitions.contains(item))
    {
        return Err(format!(
            "the rope holds {item:?} of the main text after a note's body: {indexed:?}"
        ));
    }
    for items in &definitions {
        let wanted: HashSet<Item> = items.iter().copied().collect();
        let in_rope: Vec<Item> = indexed
            .iter()
            .copied()
            .filter(|item| wanted.contains(item))
            .collect();
        if &in_rope != items {
            return Err(format!(
                "the rope holds a footnote as {in_rope:?}, its frame orders {items:?}"
            ));
        }
    }

    // Each block's formatting and anchors, against its text.
    for block_id in runs.keys().chain(images.keys()).chain(notes.keys()) {
        if !blocks.contains_key(block_id) {
            return Err(format!(
                "formatting or anchors kept for removed block {block_id}"
            ));
        }
    }
    for (block_id, text) in &texts {
        if let Some(block_runs) = runs.get(block_id) {
            check_well_formed(block_runs, text.len())
                .map_err(|e| format!("block {block_id}'s runs over {text:?}: {e}"))?;
        }
        let mut anchored: Vec<u32> = images
            .get(block_id)
            .into_iter()
            .flatten()
            .map(|image| image.byte_offset)
            .chain(
                notes
                    .get(block_id)
                    .into_iter()
                    .flatten()
                    .map(|note| note.byte_offset),
            )
            .collect();
        anchored.sort_unstable();
        let sentinels: Vec<u32> = text
            .match_indices(SENTINEL)
            .map(|(at, _)| at as u32)
            .collect();
        if anchored != sentinels {
            return Err(format!(
                "block {block_id} anchors objects at {anchored:?}, its sentinels are at \
                 {sentinels:?} in {text:?}"
            ));
        }
    }
    Ok(())
}

#[track_caller]
fn assert_model(doc: &TextDocument, after: &str) {
    if let Err(error) = check(doc) {
        panic!("after {after}: {error}");
    }
}

// ── Documents and edits ──────────────────────────────────────────────────────

fn paragraph(i: usize) -> String {
    format!("Paragraph {i} with *emphasis* and a [link](https://example.org/{i}) in it.")
}

/// The documents edits start from: prose, lists, nested quotations, tables, footnotes with
/// and without definitions, and all of those together.
fn corpus() -> Vec<(&'static str, String)> {
    let plain: String = (0..6).map(|i| format!("{}\n\n", paragraph(i))).collect();
    let lists: String = (0..3)
        .map(|i| {
            format!(
                "{}\n\n- item {i}a\n- item {i}b\n\n1. first {i}\n2. second {i}\n\n",
                paragraph(i)
            )
        })
        .collect();
    let quotes: String = (0..3)
        .map(|i| format!("{}\n\n> quoted {i}\n>\n> > nested {i}\n\n", paragraph(i)))
        .collect();
    let tables: String = (0..5)
        .map(|i| {
            // Tables of four cells, and one of a single cell, which a range can hold whole
            // from the end of the paragraph before it to the start of the one after it.
            let table = match i {
                1 => format!("| a{i} | b{i} |\n| c{i} | d{i} |\n\n"),
                3 => format!("| a{i} |\n\n"),
                _ => String::new(),
            };
            format!("{}\n\n{table}", paragraph(i))
        })
        .collect();
    let notes: String = (0..4)
        .map(|i| format!("Line {i} has a note[^n{i}] and more.\n\n[^n{i}]: Note {i}.\n\n"))
        .collect();
    let bare: String = (0..4)
        .map(|i| format!("Line {i} has a note[^b{i}] and *more*[^c{i}].\n\n"))
        .collect();
    let mixed = "# Title\n\nOpening *line* with `code`.\n\n> Quote one[^q]\n>\n> > Quote two\n\n\
                 1. first\n2. second\n\n| h1 | h2 |\n| c1[^t] | c2 |\n\nNoted[^a].\n\n\
                 [^a]: The note.\n\nLast line.\n"
        .to_string();
    // Code blocks, each followed by a paragraph holding a note's reference or an image, and
    // one in a quotation.
    let code: String = (0..3)
        .map(|i| {
            format!(
                "```\ncode {i}\n```\n\nNoted {i}[^k{i}] here ![pic](p{i}.png).\n\n> ```\n> quoted \
                 code {i}\n> ```\n\n{}\n\n",
                paragraph(i)
            )
        })
        .collect();
    // Quotations of every shape a text holds: an epigraph with its attribution set right, a
    // list and a table in a quotation, quotations nested three deep.
    let quotations = "> {semantic_role=epigraph}\n> The sea is not a place.\n>\n> \
                      {alignment=right}\n> Anon.\n\nChapter text.\n\n> - a\n> - b\n\n\
                      > | q1 | q2 |\n\n> > > deep\n> >\n> > less\n>\n> least\n\nEnd.\n"
        .to_string();
    // Rows of differing length, from Djot and from an HTML heading spanning two columns.
    let ragged = "Before.\n\n| a |\n| b | c |\n| d | e | f |\n\nAfter.\n".to_string();
    // Ordered lists starting past 1, in each numbering, quotations side by side, and tables
    // between paragraphs a selection runs out of.
    let starts = "Intro.\n\n3. three\n4. four\n\nMid.\n\nb) bee\nc) sea\n\n> a\n\n> b\n\n\
                  | t1 | t2 |\n| t3 | t4 |\n\niii. third\niv. fourth\n\n| solo |\n\nEnd.\n"
        .to_string();
    vec![
        ("prose", plain),
        ("lists", lists),
        ("quotes", quotes),
        ("tables", tables),
        ("notes", notes),
        ("bare notes", bare),
        ("mixed", mixed),
        ("code", code),
        ("quotations", quotations),
        ("ragged", ragged),
        ("list starts", starts),
    ]
}

/// What the edits paste, and in which syntax.
fn fragments() -> Vec<(&'static str, String)> {
    vec![
        ("djot", "inserted".into()),
        ("djot", "A plain paragraph.\n\nAnother *one*.\n".into()),
        ("djot", "See this[^f1] and that[^f2].\n".into()),
        ("djot", "Noted[^d1].\n\n[^d1]: A body.\n\nAfter.\n".into()),
        ("djot", "> quoted *a*\n>\n> > nested b\n".into()),
        ("djot", "- one\n- two[^l]\n\n1. x\n2. y\n".into()),
        ("djot", "a\n\n| x | y |\n| z | w |\n\nb".into()),
        ("djot", "| p | q |\n\nmid\n\n| r | s |\n| t | u |\n".into()),
        (
            "djot",
            "# H\n\ntext[^m] with [link](u)\n\n> q\n\n| t | u |\n\nend".into(),
        ),
        (
            "markdown",
            "Some *markdown*[^md]\n\n- a\n- b\n\n| x | y |\n|---|---|\n| 1 | 2 |\n\n[^md]: md \
             note\n"
                .into(),
        ),
        (
            "html",
            "<p>html <b>bold</b></p><blockquote><p>q</p><blockquote><p>qq</p></blockquote>\
             </blockquote><ol><li>one</li></ol><table><tr><td>c1</td><td>c2</td></tr></table>"
                .into(),
        ),
        // A table alone: pasted outside a table it goes in as a table, and with the caret
        // in a cell it fills the cells from there on when it fits.
        ("djot", "| only | table |\n".into()),
        ("djot", "| solo |\n".into()),
        ("djot", "| p *q* | r[^tq] |\n| s | t |\n".into()),
        (
            "html",
            "<table><tr><td>h1</td><td><b>h2</b></td></tr><tr><td>h3</td><td>h4</td></tr>\
             </table>"
                .into(),
        ),
        ("markdown", "| x | y |\n|---|---|\n| 1 | 2 |\n".into()),
        // Code blocks, alone and among paragraphs, and a `<pre>` holding an image.
        ("djot", "```\npasted code\n```\n".into()),
        (
            "djot",
            "before\n\n```rust\nfn f() {}\n```\n\nafter[^cb]".into(),
        ),
        ("markdown", "```\nmd code\n```\n\ntext\n".into()),
        (
            "html",
            "<pre>pre code <img src=\"p.png\" alt=\"pic\"></pre><p>p</p>".into(),
        ),
        (
            "html",
            "<pre>line one\nline two <img src=\"p.png\" alt=\"pic\"> tail\n\nline three</pre>\
             <p>after</p>"
                .into(),
        ),
        // A passage a web page sets as a `<pre>`, which a paste reads as its lines, and a
        // phrase copied from one, which goes into the paragraph at the caret.
        (
            "html",
            "<pre>verse one\n  verse two\n\nverse three</pre>".into(),
        ),
        ("html", "<span>a <b>pasted</b> phrase</span>".into()),
        // Quotations of the other shapes: an epigraph, a list in a quotation, one nested
        // deeper than the one before it.
        (
            "djot",
            "> {semantic_role=epigraph}\n> Epi.\n>\n> {alignment=right}\n> Who\n".into(),
        ),
        ("djot", "> - qa\n> - qb\n\nout".into()),
        ("djot", "> > deep first\n>\n> shallow\n".into()),
        // Tables whose rows differ in length.
        ("djot", "| r1 |\n| r2 | r3 |\n".into()),
        (
            "html",
            "<table><tr><th colspan=\"2\">Cast</th></tr><tr><td>Anna</td><td>widow</td></tr>\
             </table>"
                .into(),
        ),
        // A span closing a row, wider than any row reaches, and a table too ragged to
        // complete, which is read as its cells' paragraphs.
        (
            "html",
            "<table><tr><th colspan=\"900\">Title</th></tr><tr><td>one</td><td>two</td></tr>\
             </table>"
                .into(),
        ),
        ("djot", ragged_table(65, 64)),
        // Lists starting past 1, and quotations side by side.
        ("djot", "7. seven\n8. eight\n".into()),
        ("markdown", "5. five\n6. six\n\nafter\n".into()),
        (
            "html",
            "<ol start=\"4\"><li>four</li><li>five</li></ol><p>p</p>".into(),
        ),
        ("djot", "> one\n\n> two\n".into()),
    ]
}

/// A Djot table whose first row has `columns` cells and whose `rows` other rows have one
/// each: completing its rows would make it many times the cells it has.
fn ragged_table(columns: usize, rows: usize) -> String {
    let mut table = String::from("|");
    for column in 0..columns {
        table.push_str(&format!(" h{column} |"));
    }
    table.push('\n');
    for row in 0..rows {
        table.push_str(&format!("| r{row} |\n"));
    }
    table
}

fn load(djot: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_sync(djot).unwrap();
    doc
}

/// A document holding `text`, loaded through the setter for its `syntax`.
fn loaded_as(syntax: &str, text: &str) -> TextDocument {
    let doc = TextDocument::new();
    let loaded = match syntax {
        "djot" => doc.set_djot_sync(text).map(|_| ()),
        "markdown" => doc.set_markdown(text).and_then(|op| op.wait()).map(|_| ()),
        _ => doc.set_html(text).and_then(|op| op.wait()).map(|_| ()),
    };
    if let Err(error) = loaded {
        panic!("loading {syntax} {text:?}: {error}");
    }
    doc
}

/// Where `needle` starts in `doc`'s addressable text, as a character position: the space a
/// cursor addresses, where a byte index is off by two for every `U+FFFC` before it.
fn position_of(doc: &TextDocument, needle: &str) -> usize {
    let text = doc.to_addressable_text().unwrap();
    let byte = text.find(needle).expect("the text is in the document");
    text[..byte].chars().count()
}

fn corpus_document(name: &str) -> TextDocument {
    let (_, text) = corpus()
        .into_iter()
        .find(|(n, _)| *n == name)
        .expect("a document of the corpus");
    load(&text)
}

/// Length of the position space: the addressable text, every block and anchor with the
/// boundaries between them.
fn length(doc: &TextDocument) -> usize {
    doc.to_addressable_text().unwrap().chars().count()
}

fn select(doc: &TextDocument, from: usize, to: usize) -> text_document::TextCursor {
    let cursor = doc.cursor();
    cursor.set_position(from, MoveMode::MoveAnchor);
    cursor.set_position(to, MoveMode::KeepAnchor);
    cursor
}

/// Select all of `doc` and insert `djot` over it as one edit: a version restore.
fn restore(doc: &TextDocument, djot: &str) {
    let cursor = doc.cursor();
    cursor.begin_edit_block();
    cursor.select(SelectionType::Document);
    let result = cursor.insert_djot(djot);
    cursor.end_edit_block();
    result.unwrap();
}

fn paste(cursor: &text_document::TextCursor, syntax: &str, text: &str) {
    let _ = match syntax {
        "djot" => cursor.insert_djot(text),
        "markdown" => cursor.insert_markdown(text),
        _ => cursor.insert_html(text),
    };
}

/// A small deterministic generator: xorshift, seeded.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound.max(1) as u64) as usize
    }
}

/// The characters of `doc`'s addressable text.
fn characters(doc: &TextDocument) -> Vec<char> {
    doc.to_addressable_text()
        .map(|text| text.chars().collect())
        .unwrap_or_default()
}

/// A caret an edit returned stands right after the text it put in.
#[track_caller]
fn assert_caret_after(doc: &TextDocument, cursor: &text_document::TextCursor, inserted: &str) {
    let caret = cursor.position();
    let text = characters(doc);
    let length = inserted.chars().count();
    let before: String = text
        .get(caret.saturating_sub(length)..caret.min(text.len()))
        .map(|slice| slice.iter().collect())
        .unwrap_or_default();
    assert_eq!(
        before, inserted,
        "the caret at {caret} does not follow the text it put in"
    );
}

/// A cursor standing at or after everything an edit changed keeps the text after it: the
/// edit may move it, never across a character.
#[track_caller]
fn assert_other_cursor_kept(
    doc: &TextDocument,
    before: &[char],
    at: usize,
    other: &text_document::TextCursor,
    edit: &str,
) {
    let after = characters(doc);
    let moved_to = other.position();
    // Where a cursor stands is `cursor_at`'s answer, and the edit is what is checked here.
    let kept: String = before
        .get(at..)
        .map(|s| s.iter().collect())
        .unwrap_or_default();
    let now: String = after
        .get(moved_to..)
        .map(|s| s.iter().collect())
        .unwrap_or_default();
    assert_eq!(
        now,
        kept,
        "after {edit}, a cursor at {at}, moved to {moved_to}, no longer stands before the same \
         text: before {:?}, after {:?}",
        before.iter().collect::<String>(),
        after.iter().collect::<String>()
    );
}

/// Where edits go wrong most: the start and the end of every entry of the rope (each block,
/// and each table's anchor, whether in the main text, a table cell or a footnote's body),
/// and the positions on either side of them.
fn boundary_positions(doc: &TextDocument) -> Vec<usize> {
    let store = doc.rope_store_for_test();
    let rope = store.rope.read();
    let offsets = store.block_offsets.read();
    let total = rope.len_chars();
    let mut positions: Vec<usize> = Vec::new();
    for (i, (_, start)) in offsets.entries.iter().enumerate() {
        let start = rope.byte_to_char(*start as usize);
        let end = offsets.entries.get(i + 1).map_or(total, |(_, next)| {
            rope.byte_to_char(*next as usize).saturating_sub(1)
        });
        for position in [start, end] {
            positions.extend([position.saturating_sub(1), position, position + 1]);
        }
    }
    positions.retain(|position| *position <= total);
    positions.sort_unstable();
    positions.dedup();
    positions
}

/// A position in `doc`: one of its entries' boundaries half the time, any position otherwise.
fn random_position(doc: &TextDocument, n: usize, rng: &mut Rng) -> usize {
    if rng.below(2) == 0 {
        let boundaries = boundary_positions(doc);
        if !boundaries.is_empty() {
            return boundaries[rng.below(boundaries.len())];
        }
    }
    rng.below(n + 1)
}

/// Each table's extent as a selection holding it whole: from the end of the entry before its
/// anchor (or the document's start) to the start of the entry after its last cell (or the
/// document's end), what a writer selects by dragging across the table.
fn table_spans(doc: &TextDocument) -> Vec<(usize, usize)> {
    let store = doc.rope_store_for_test();
    let frames: HashMap<u64, Frame> = copied(store.frames.read().iter());
    let tables: Vec<Table> = store.tables.read().values().cloned().collect();
    let cells: HashMap<u64, TableCell> = copied(store.table_cells.read().iter());
    let rope = store.rope.read();
    let offsets = store.block_offsets.read();
    let total = rope.len_chars();
    let char_of = |index: usize| {
        offsets
            .entries
            .get(index)
            .map_or(total, |(_, byte)| rope.byte_to_char(*byte as usize))
    };
    let mut spans = Vec::new();
    for table in tables {
        let Some(anchor) = offsets.position_of(OffsetMarker::TableAnchor(table.id)) else {
            continue;
        };
        let last = table
            .cells
            .iter()
            .filter_map(|id| cells.get(id)?.cell_frame)
            .filter_map(|frame| frames.get(&frame))
            .flat_map(|frame| frame.blocks.iter())
            .filter_map(|block| offsets.position_of(OffsetMarker::Block(*block)))
            .max()
            .unwrap_or(anchor);
        spans.push((char_of(anchor).saturating_sub(1), char_of(last + 1)));
    }
    spans
}

/// A position at or after `to` that no edit of a range ending at `to` reaches: `to` itself,
/// or the start of a block after it, and never one inside a table cell, which a range ending
/// in the cell empties whole, nor a table's anchor, where typed text goes into the cell after. `past_its_block` leaves out `to` and the block after it, for a
/// paste: a table pasted into a paragraph goes in after the paragraph, after `to`.
fn position_past(
    doc: &TextDocument,
    to: usize,
    past_its_block: bool,
    rng: &mut Rng,
) -> Option<usize> {
    let store = doc.rope_store_for_test();
    let frames: HashMap<u64, Frame> = copied(store.frames.read().iter());
    let cells: Vec<TableCell> = store.table_cells.read().values().cloned().collect();
    let mut in_cells: HashSet<u64> = HashSet::new();
    fn blocks_under(frame_id: u64, frames: &HashMap<u64, Frame>, out: &mut HashSet<u64>) {
        if let Some(frame) = frames.get(&frame_id) {
            out.extend(frame.blocks.iter().copied());
            for entry in &frame.child_order {
                if *entry < 0 {
                    blocks_under((-entry) as u64, frames, out);
                }
            }
        }
    }
    for cell in &cells {
        if let Some(frame) = cell.cell_frame {
            blocks_under(frame, &frames, &mut in_cells);
        }
    }
    let rope = store.rope.read();
    let offsets = store.block_offsets.read();
    let total = rope.len_chars();
    let mut candidates: Vec<usize> = Vec::new();
    // Whether `to` stands inside the text of a block outside every table: a position on a
    // table's anchor, or on the boundary after it, stands for the table's first cell, where
    // text typed there goes; and a range ending at a block's end can take the boundary after
    // it with the block.
    let mut to_in_a_block = false;
    for (i, (marker, start)) in offsets.entries.iter().enumerate() {
        let start = rope.byte_to_char(*start as usize);
        let end = offsets
            .entries
            .get(i + 1)
            .map_or(total, |(_, next)| rope.byte_to_char(*next as usize));
        let in_a_cell = marker.as_block().is_some_and(|id| in_cells.contains(&id));
        let text_end = if i + 1 < offsets.entries.len() {
            end.saturating_sub(1)
        } else {
            end
        };
        if (start..end).contains(&to) || (to == total && end == total) {
            to_in_a_block = marker.is_block() && !in_a_cell && start < to && to < text_end;
        }
        if start > to && !in_a_cell && marker.is_block() {
            candidates.push(start);
        }
    }
    if past_its_block {
        // The paste lands in the block the deletion leaves at the range's start, which holds
        // what follows `to`: the block `to` is in, or the first one after a table `to` is in.
        if !candidates.is_empty() {
            candidates.remove(0);
        }
    } else if to_in_a_block {
        candidates.push(to);
    }
    (!candidates.is_empty()).then(|| candidates[rng.below(candidates.len())])
}

/// Run `edit` over the range `from..to`, and check after it that another cursor placed past
/// the range still stands before the text it stood before. A paste passes `past_its_block`
/// (see [`position_past`]).
fn with_other_cursor(
    doc: &TextDocument,
    rng: &mut Rng,
    (from, to): (usize, usize),
    past_its_block: bool,
    edit: impl FnOnce() -> (bool, String),
) -> String {
    let before = characters(doc);
    // Text typed at an empty selection goes in at `to` itself, in front of a cursor there.
    let past_its_block = past_its_block || from == to;
    let other = position_past(doc, to, past_its_block, rng).map(|at| (at, doc.cursor_at(at)));
    let (done, description) = edit();
    if let Some((at, other)) = other
        && done
    {
        let described = format!("{description}, another cursor at {at}");
        assert_other_cursor_kept(doc, &before, at, &other, &described);
        return described;
    }
    description
}

/// The range a selection from `from` to `to` covers: a selection reaching into a table from
/// outside it takes in the whole table.
fn selected(cursor: &text_document::TextCursor) -> (usize, usize) {
    (cursor.selection_start(), cursor.selection_end())
}

/// One random edit; returns what it did, for the failure message. Carets are checked as it
/// goes: after typing or an inline paste, the caret follows the new text; after a deletion,
/// a replacement or a paste over a range, another cursor past the change still stands before
/// the same text.
fn random_edit(doc: &TextDocument, rng: &mut Rng, fragments: &[(&str, String)]) -> String {
    let n = length(doc);
    let (a, b) = (random_position(doc, n, rng), random_position(doc, n, rng));
    let (from, to) = (a.min(b), a.max(b));
    let pick_fragment = |rng: &mut Rng| {
        let (syntax, text) = &fragments[rng.below(fragments.len())];
        (*syntax, text.clone())
    };
    match rng.below(42) {
        0 | 1 => {
            let (syntax, text) = pick_fragment(rng);
            let cursor = doc.cursor_at(from);
            paste(&cursor, syntax, &text);
            if text == "inserted" {
                assert_caret_after(doc, &cursor, "inserted");
            }
            format!("paste {syntax} {text:?} at {from}")
        }
        2 | 3 => {
            let (syntax, text) = pick_fragment(rng);
            let cursor = select(doc, from, to);
            with_other_cursor(doc, rng, selected(&cursor), true, || {
                let done = match syntax {
                    "djot" => cursor.insert_djot(&text),
                    "markdown" => cursor.insert_markdown(&text),
                    _ => cursor.insert_html(&text),
                }
                .is_ok();
                (done, format!("paste {syntax} {text:?} over {from}..{to}"))
            })
        }
        4 => {
            // Half the time, a whole table, from the paragraph before it to the one after it.
            let spans = table_spans(doc);
            let (from, to) = if !spans.is_empty() && rng.below(2) == 0 {
                spans[rng.below(spans.len())]
            } else {
                (from, to)
            };
            let cursor = select(doc, from, to);
            with_other_cursor(doc, rng, selected(&cursor), false, || {
                let done = cursor.remove_selected_text().is_ok();
                (done, format!("delete {from}..{to}"))
            })
        }
        5 => {
            let starts: Vec<usize> = doc
                .blocks()
                .iter()
                .map(|block| block.position())
                .filter(|position| *position > 0)
                .collect();
            if starts.is_empty() {
                return "nothing to join".into();
            }
            let at = starts[rng.below(starts.len())];
            let before = characters(doc);
            let other_at = at + rng.below(n + 1 - at.min(n));
            let other = doc.cursor_at(other_at);
            let other_at = other.position();
            let edit = format!("backspace at the block start {at}, another cursor at {other_at}");
            if doc.cursor_at(at).delete_previous_char().is_ok() {
                assert_other_cursor_kept(doc, &before, other_at, &other, &edit);
            }
            edit
        }
        6 => {
            let (syntax, text) = pick_fragment(rng);
            // A text put back reads as the same text loaded, as long as neither holds a
            // footnote definition: a fragment carries no note's body, and the bodies of the
            // text it replaces stay.
            let comparable = !text.contains("]:") && !holds_definitions(doc);
            let cursor = doc.cursor();
            cursor.begin_edit_block();
            cursor.select(SelectionType::Document);
            paste(&cursor, syntax, &text);
            cursor.end_edit_block();
            let edit = format!("restore {syntax} {text:?}");
            if comparable {
                let restored = doc.to_djot().unwrap_or_default();
                // Djot and Markdown are whole texts: put back, they read as the same text
                // loaded. HTML is a paste: several paragraphs or a table read as pasted into
                // an empty text, and a phrase goes into the paragraph the removal leaves, as
                // typing does, which this leaves to its own test.
                let expected = if syntax == "html" {
                    let pasted = TextDocument::new();
                    paste(&pasted.cursor(), syntax, &text);
                    (pasted.blocks().len() > 1 || pasted.stats().table_count > 0)
                        .then(|| pasted.to_djot().unwrap_or_default())
                } else {
                    Some(loaded_as(syntax, &text).to_djot().unwrap_or_default())
                };
                if let Some(expected) = expected {
                    assert_eq!(
                        restored, expected,
                        "{edit}: the text put back reads otherwise loaded"
                    );
                }
            }
            edit
        }
        7 => {
            let cursor = doc.cursor_at(from);
            if cursor.insert_text("typed").is_ok() {
                assert_caret_after(doc, &cursor, "typed");
            }
            format!("type at {from}")
        }
        8 => {
            let _ = doc.cursor_at(from).insert_block();
            format!("break the paragraph at {from}")
        }
        9 => {
            let before = characters(doc);
            let other_at = from + rng.below(n + 1 - from);
            let other = doc.cursor_at(other_at);
            let other_at = other.position();
            let edit = format!("backspace at {from}, another cursor at {other_at}");
            if doc.cursor_at(from).delete_previous_char().is_ok() {
                assert_other_cursor_kept(doc, &before, other_at, &other, &edit);
            }
            edit
        }
        10 => {
            let before = characters(doc);
            let other_at = (from + 1 + rng.below(n + 1 - from)).min(n);
            let other = doc.cursor_at(other_at);
            let other_at = other.position();
            let edit = format!("delete forward at {from}, another cursor at {other_at}");
            if doc.cursor_at(from).delete_char().is_ok() && from < n {
                assert_other_cursor_kept(doc, &before, other_at, &other, &edit);
            }
            edit
        }
        11 => {
            let cursor = select(doc, from, to);
            with_other_cursor(doc, rng, selected(&cursor), false, || {
                let done = cursor.insert_text("over").is_ok();
                (done, format!("type over {from}..{to}"))
            })
        }
        12 => {
            let _ = doc.cursor_at(from).insert_footnote_reference("z");
            format!("insert a footnote reference at {from}")
        }
        13 => {
            let italic = TextFormat {
                font_italic: Some(true),
                ..Default::default()
            };
            let _ = select(doc, from, to).merge_char_format(&italic);
            format!("italic over {from}..{to}")
        }
        14 => {
            let _ = doc.cursor_at(from).insert_table(2, 2);
            format!("insert a table at {from}")
        }
        15 => {
            let cursor = doc.cursor_at(from);
            let _ = if rng.below(2) == 0 {
                cursor.insert_row_below()
            } else {
                cursor.insert_row_above()
            };
            format!("insert a row at {from}")
        }
        16 => {
            let cursor = doc.cursor_at(from);
            let _ = if rng.below(2) == 0 {
                cursor.insert_column_before()
            } else {
                cursor.insert_column_after()
            };
            format!("insert a column at {from}")
        }
        17 => {
            let _ = doc.cursor_at(from).remove_current_row();
            format!("remove the row at {from}")
        }
        18 => {
            let _ = doc.cursor_at(from).remove_current_column();
            format!("remove the column at {from}")
        }
        19 => {
            let _ = doc.cursor_at(from).split_current_cell(1, 2);
            format!("split the cell at {from}")
        }
        20 => {
            let _ = doc.cursor_at(from).remove_current_table();
            format!("remove the table at {from}")
        }
        21 => {
            let cursor = select(doc, from, to);
            let _ = match rng.below(3) {
                0 => cursor.toggle_blockquote(),
                1 => cursor.wrap_selection_in_blockquote(),
                _ => cursor.insert_blockquote(),
            };
            format!("quote {from}..{to}")
        }
        22 => {
            let cursor = doc.cursor_at(from);
            let _ = match rng.below(3) {
                0 => cursor.unwrap_current_block_from_blockquote(),
                1 => cursor.increase_blockquote_depth(),
                _ => cursor.decrease_blockquote_depth(),
            };
            format!("change the quotation depth at {from}")
        }
        23 => {
            let cursor = select(doc, from, to);
            match rng.below(5) {
                0 => {
                    let _ = cursor.create_list(text_document::ListStyle::Disc);
                    format!("list {from}..{to}")
                }
                1 => {
                    let _ = cursor.remove_current_block_from_list();
                    format!("take the item at {from} out of its list")
                }
                2 => {
                    let _ = doc
                        .cursor_at(from)
                        .insert_list(text_document::ListStyle::Decimal);
                    format!("insert a list item at {from}")
                }
                way => {
                    let deeper = way == 3;
                    let edit = format!(
                        "move the item at {from} one level {}",
                        if deeper { "in" } else { "out" }
                    );
                    let was_listed = doc.cursor_at(from).current_list().is_some();
                    if let Err(error) = nest_list_item(doc, from, deeper) {
                        panic!("{edit}: {error}");
                    }
                    assert_eq!(
                        doc.cursor_at(from).current_list().is_some(),
                        was_listed,
                        "{edit}: the item left its list"
                    );
                    edit
                }
            }
        }
        24 => {
            let cursor = doc.cursor_at(from);
            if cursor.insert_image("image.png", "alt", 4, 4).is_ok() {
                assert_caret_after(doc, &cursor, "\u{FFFC}");
            }
            format!("insert an image at {from}")
        }
        25 => {
            let words = ["note", "a", "e", "Paragraph", "x", "t"];
            let word = words[rng.below(words.len())];
            let replacement = ["X", "", "longer text"][rng.below(3)];
            let options = text_document::ReplaceOptions::new(text_document::FindOptions::default());
            let edit = format!("replace every {word:?} by {replacement:?}");
            // Every match `find_all` reports is the text replaced, in the text it searched:
            // after a table, the matches were placed two characters early for each table
            // before them, and other characters were rewritten, an image with them.
            let before = doc.to_addressable_text().unwrap_or_default();
            let matches = doc.find_all(word, &options.find).unwrap_or_default();
            if let Ok(replaced) = doc.replace_text(word, replacement, true, &options) {
                let chars: Vec<char> = before.chars().collect();
                let mut expected = String::new();
                let mut at = 0;
                for found in &matches {
                    expected.extend(&chars[at..found.position]);
                    expected.push_str(replacement);
                    at = found.position + found.length;
                }
                expected.extend(&chars[at..]);
                assert_eq!(
                    replaced,
                    matches.len(),
                    "{edit}: not every match was replaced"
                );
                assert_eq!(
                    doc.to_addressable_text().unwrap_or_default(),
                    expected,
                    "{edit}: other text than the matches changed"
                );
            }
            edit
        }
        26 => with_other_cursor(doc, rng, (from, to), false, || {
            let done = doc
                .cursor()
                .replace(from, to, "swap", Default::default())
                .is_ok();
            (done, format!("replace {from}..{to}"))
        }),
        27 => {
            let bold = TextFormat {
                font_bold: Some(true),
                ..Default::default()
            };
            let cursor = select(doc, from, to);
            with_other_cursor(doc, rng, selected(&cursor), false, || {
                let done = cursor.insert_formatted_text("bold", &bold).is_ok();
                (done, format!("type bold over {from}..{to}"))
            })
        }
        28 => {
            let heading = text_document::BlockFormat {
                heading_level: Some(2),
                ..Default::default()
            };
            let _ = select(doc, from, to).set_block_format(&heading);
            format!("heading over {from}..{to}")
        }
        29 => {
            let cursor = doc.cursor_at(from);
            let Some(table) = cursor.current_table() else {
                return format!("no table at {from}");
            };
            let (row, column) = (rng.below(2), rng.below(2));
            let (end_row, end_column) = (row + rng.below(2), column + rng.below(2));
            cursor.select_cell_range(table.id(), row, column, end_row, end_column);
            let _ = if rng.below(2) == 0 {
                cursor.merge_selected_cells()
            } else {
                cursor.remove_selected_text().map(|_| ())
            };
            format!("cells {row},{column}..{end_row},{end_column} of the table at {from}")
        }
        30 | 31 => {
            // Copy a range and paste it, at a caret or over a range: the clipboard's path.
            let fragment = select(doc, from, to).selection();
            let (x, y) = (random_position(doc, n, rng), random_position(doc, n, rng));
            let (x, y) = (x.min(y), x.max(y));
            let edit = format!("copy {from}..{to} and paste it over {x}..{y}");
            // A copy of a range starting in the text holds the text's words only, never a
            // note's body.
            if note_body_at(doc, from).is_none() {
                let text = doc
                    .to_plain_text()
                    .unwrap_or_default()
                    .replace(SENTINEL, " ");
                let copied = fragment.to_plain_text().replace(SENTINEL, " ");
                if let Some(word) = copied.split_whitespace().find(|word| !text.contains(word)) {
                    panic!("{edit}: the copy holds {word:?}, which the text does not: {copied:?}");
                }
            }
            // The paste edits the tree it starts in: no other note's body changes.
            let pasted_into = note_body_at(doc, x);
            let bodies_before = note_bodies(doc);
            let _ = select(doc, x, y).insert_fragment(&fragment);
            let bodies_after = note_bodies(doc);
            let others = |bodies: &std::collections::BTreeMap<String, String>| {
                bodies
                    .iter()
                    .filter(|(label, _)| Some(*label) != pasted_into.as_ref())
                    .map(|(label, body)| (label.clone(), body.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                others(&bodies_after),
                others(&bodies_before),
                "{edit}: a note's body changed"
            );
            edit
        }
        32 => {
            let cursor = select(doc, from, to);
            let _ = cursor.insert_block();
            format!("break the paragraph over {from}..{to}")
        }
        33 => {
            let cursor = select(doc, from, to);
            let _ = cursor.insert_image("image.png", "alt", 4, 4);
            format!("insert an image over {from}..{to}")
        }
        35 => reload_in_place(doc, rng, fragments),
        36 => {
            let code = rng.below(2) == 0;
            let format = text_document::BlockFormat {
                is_code_block: Some(code),
                ..Default::default()
            };
            let _ = select(doc, from, to).set_block_format(&format);
            format!("code block {code} over {from}..{to}")
        }
        34 if doc.can_redo() => {
            let _ = doc.redo();
            "redo".into()
        }
        37 => {
            // Select all, cut and paste back at the same caret, as one edit: the text is as
            // it was, and so is the text a save writes. A copy held a quoted paragraph once
            // for each quotation around it, one of a text opening with a quoted table held
            // nothing, and the paste put every paragraph into the quotation the first one
            // stood in; two quotations side by side came back as one, an ordered list
            // numbered from 1, and a text opening with a table with an empty paragraph
            // before it.
            let edit = "cut everything and paste it back".to_string();
            let Ok((djot, text)) = seen(doc) else {
                return edit;
            };
            let hidden_list_items = holds_list_items_written_otherwise(doc);
            let cursor = doc.cursor();
            cursor.select(SelectionType::Document);
            let cut = cursor.selection();
            if cut.is_empty() {
                return edit;
            }
            cursor.begin_edit_block();
            let pasted = cursor
                .remove_selected_text()
                .and_then(|_| cursor.insert_fragment(&cut));
            cursor.end_edit_block();
            if let Err(error) = pasted {
                panic!("{edit}: {error}");
            }
            let (djot_after, text_after) =
                seen(doc).unwrap_or_else(|error| panic!("{edit}: {error}"));
            assert_eq!(text_after, text, "{edit}: the text changed");
            // A list item the save writes as a heading or a code block leaves no marker, and
            // the paste groups the items around it into lists by their style, as a reload
            // groups written items: the items after it can then count from another number.
            // Every other list keeps its numbers.
            if hidden_list_items {
                assert_eq!(
                    without_list_numbers(&djot_after),
                    without_list_numbers(&djot),
                    "{edit}: the saved text changed"
                );
            } else {
                assert_eq!(djot_after, djot, "{edit}: the saved text changed");
            }
            edit
        }
        38 | 39 => table_edge_selection(doc, rng),
        _ => {
            let _ = doc.undo();
            "undo".into()
        }
    }
}

/// Whether `doc` holds a list item its Djot writes as a heading or a code block, with no
/// marker: an editor's heading or code-block format over a list item.
fn holds_list_items_written_otherwise(doc: &TextDocument) -> bool {
    doc.rope_store_for_test()
        .blocks
        .read()
        .values()
        .any(|block| {
            block.list.is_some()
                && (block.fmt_heading_level.is_some() || block.fmt_is_code_block == Some(true))
        })
}

/// `djot` with the number of every ordered list item's marker left out.
fn without_list_numbers(djot: &str) -> String {
    djot.lines()
        .map(|line| {
            let body = line.trim_start_matches(['>', ' ']);
            let lead = &line[..line.len() - body.len()];
            let digits = body.chars().take_while(char::is_ascii_digit).count();
            match body[digits..].chars().next() {
                Some('.' | ')') if digits > 0 => format!("{lead}#{}", &body[digits..]),
                _ => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The characters of `text` a selection or a removal accounts for, sorted: its words, without
/// the whitespace and the anchors a removal joins or a copy leaves out.
fn counted_characters(text: &str) -> Vec<char> {
    let mut characters: Vec<char> = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != SENTINEL)
        .collect();
    characters.sort_unstable();
    characters
}

/// A paragraph's text, as the positions it starts and ends at.
type Span = (usize, usize);

/// The text of the paragraphs right before and right after the table `table_id` in the frame
/// that holds it, when the entry beside the table is a paragraph.
fn paragraphs_beside_table(doc: &TextDocument, table_id: u64) -> (Option<Span>, Option<Span>) {
    let store = doc.rope_store_for_test();
    let frames: HashMap<u64, Frame> = copied(store.frames.read().iter());
    let Some(anchor_frame) = frames
        .iter()
        .find(|(_, frame)| frame.table == Some(table_id))
        .map(|(id, _)| *id)
    else {
        return (None, None);
    };
    let Some(holder) = frames
        .values()
        .find(|frame| frame.child_order.contains(&-(anchor_frame as i64)))
    else {
        return (None, None);
    };
    let Some(at) = holder
        .child_order
        .iter()
        .position(|entry| *entry == -(anchor_frame as i64))
    else {
        return (None, None);
    };
    let rope = store.rope.read();
    let offsets = store.block_offsets.read();
    let total = rope.len_chars();
    let extent = |entry: Option<&i64>| -> Option<Span> {
        let block = u64::try_from(*entry?).ok().filter(|id| *id > 0)?;
        let index = offsets.position_of(OffsetMarker::Block(block))?;
        let start = rope.byte_to_char(offsets.entries.get(index)?.1 as usize);
        let end = offsets
            .entries
            .get(index + 1)
            .map_or(total, |(_, next)| rope.byte_to_char(*next as usize) - 1);
        Some((start, end))
    };
    (
        at.checked_sub(1)
            .and_then(|before| extent(holder.child_order.get(before))),
        extent(holder.child_order.get(at + 1)),
    )
}

/// A selection with one end in a table's cells and the other outside the table, made as a
/// drag or Shift and the arrows make it, from either end: it shows the whole table selected,
/// and a copy, a cut, typing over it and a drag of it elsewhere all take that, the table
/// whole with the text beside it. Cut and pasted back when its outer end stands in a
/// paragraph beside the table, it gives the text back exactly; anywhere else, a removal
/// takes exactly what the copy holds, and a drag moves it without a word lost.
fn table_edge_selection(doc: &TextDocument, rng: &mut Rng) -> String {
    let store = doc.rope_store_for_test();
    let extents = common::database::rope_helpers::table_extents(&store);
    drop(store);
    if extents.is_empty() {
        return "no table to select from".into();
    }
    let extent = extents[rng.below(extents.len())];
    let (anchor, end) = (extent.anchor as usize, extent.end as usize);
    let first_cell = anchor + 2;
    if first_cell > end {
        return "a table with no cell".into();
    }
    let n = length(doc);
    let inside = first_cell + rng.below(end - first_cell + 1);
    // Where the outer end may stand for the cut pasted back to give the text back exactly:
    // in the paragraph before the table past its start, or in the one after it before its
    // end. From the start of the paragraph before, or to the end of the one after, the
    // paragraph goes whole, and the paste puts its text into the paragraph at the caret,
    // with that paragraph's format, as a paste of a plain paragraph does.
    let (before, after) = paragraphs_beside_table(doc, extent.table_id);
    let beside: Vec<Span> = before
        .filter(|(start, end)| start < end)
        .map(|(start, end)| (start + 1, end))
        .into_iter()
        .chain(after.map(|(start, end)| (start, end.saturating_sub(1).max(start))))
        .collect();
    let outside = if !beside.is_empty() && rng.below(2) == 0 {
        let (start, stop) = beside[rng.below(beside.len())];
        Some((start + rng.below(stop - start + 1), true))
    } else {
        // Anywhere before the table's anchor or past its last cell.
        let candidates = anchor + n.saturating_sub(end);
        (candidates > 0).then(|| {
            let pick = rng.below(candidates);
            (
                if pick < anchor {
                    pick
                } else {
                    end + 1 + (pick - anchor)
                },
                false,
            )
        })
    };
    let Some((outside, beside_the_table)) = outside else {
        return "nothing outside the table".into();
    };
    let outside = outside.min(n);
    let (from, to) = if rng.below(2) == 0 {
        (inside, outside)
    } else {
        (outside, inside)
    };
    let cursor = doc.cursor_at(from);
    cursor.set_position(to, MoveMode::KeepAnchor);
    let (lo, hi) = (cursor.selection_start(), cursor.selection_end());
    let what = format!("a selection from {from} to {to} across the table at {anchor}..{end}");
    assert!(
        lo <= anchor && hi >= end,
        "{what}: the selection {lo}..{hi} does not hold the whole table"
    );
    let before_text = doc.to_addressable_text().unwrap_or_default();
    let (djot, _) = seen(doc).unwrap_or_default();
    let copied = cursor.selection();
    let copied_characters = counted_characters(copied.to_plain_text());
    let all_characters = counted_characters(&before_text);
    let edit = match rng.below(4) {
        0 => {
            cursor.begin_edit_block();
            let done = cursor
                .remove_selected_text()
                .and_then(|_| cursor.insert_fragment(&copied));
            cursor.end_edit_block();
            if let Err(error) = done {
                panic!("{what}, cut and pasted back: {error}");
            }
            if beside_the_table {
                let (djot_after, text_after) = seen(doc).unwrap_or_default();
                assert_eq!(text_after, before_text, "{what}, cut and pasted back");
                assert_eq!(
                    djot_after, djot,
                    "{what}, cut and pasted back: the saved text"
                );
            }
            "cut and pasted back"
        }
        1 => {
            cursor.remove_selected_text().unwrap_or_default();
            let mut kept_and_cut =
                counted_characters(&doc.to_addressable_text().unwrap_or_default());
            kept_and_cut.extend(&copied_characters);
            kept_and_cut.sort_unstable();
            assert_eq!(
                kept_and_cut,
                all_characters,
                "{what}, removed: the removal took other than the copy holds, copied {:?}",
                copied.to_plain_text()
            );
            "removed"
        }
        2 => {
            if cursor.insert_text("typed").is_ok() {
                let mut kept = counted_characters(&doc.to_addressable_text().unwrap_or_default());
                kept.extend(&copied_characters);
                kept.sort_unstable();
                let mut expected = all_characters.clone();
                expected.extend("typed".chars());
                expected.sort_unstable();
                assert_eq!(kept, expected, "{what}, typed over");
            }
            "typed over"
        }
        _ => {
            // An editor's drag of the selection: selected again by its ends, removed, and
            // put in where it is dropped, outside every table. It is selected again as it was
            // made, from its anchor: from its start to its end instead, an end standing on the
            // anchor of the next table is a moving end that takes that table in (as Shift and
            // the Right arrow onto it do), which the copy dragged does not hold.
            let store = doc.rope_store_for_test();
            let tables = common::database::rope_helpers::table_extents(&store);
            drop(store);
            let drops: Vec<usize> = (0..=n)
                .filter(|at| *at < lo || *at > hi)
                .filter(|at| {
                    tables
                        .iter()
                        .all(|t| (*at as i64) < t.anchor || (*at as i64) > t.end + 1)
                })
                .collect();
            if drops.is_empty() {
                return format!("{what}: nowhere to drop it");
            }
            let drop_at = drops[rng.below(drops.len())];
            // One edit, as an editor makes a drag one step of its history.
            let (anchor_end, moving_end) = (cursor.anchor(), cursor.position());
            cursor.begin_edit_block();
            cursor.set_position(anchor_end, MoveMode::MoveAnchor);
            cursor.set_position(moving_end, MoveMode::KeepAnchor);
            assert_eq!(
                (cursor.selection_start(), cursor.selection_end()),
                (lo, hi),
                "{what}: selected again"
            );
            let _ = cursor.remove_selected_text();
            let target = if drop_at > hi {
                drop_at - (hi - lo)
            } else {
                drop_at
            };
            cursor.set_position(target.min(length(doc)), MoveMode::MoveAnchor);
            let dropped = cursor.insert_fragment(&copied);
            cursor.end_edit_block();
            if dropped.is_ok() {
                assert_eq!(
                    counted_characters(&doc.to_addressable_text().unwrap_or_default()),
                    all_characters,
                    "{what}, dragged to {drop_at}"
                );
            }
            "dragged"
        }
    };
    format!("{what}, {edit}")
}

/// Move the list item at `at` one level in or out, as an editor's Tab and Shift+Tab do: take
/// it out of its list, then make a list of it at the new level, as one edit. Nothing to do
/// outside a list, or out of the top level.
fn nest_list_item(doc: &TextDocument, at: usize, deeper: bool) -> text_document::Result<()> {
    let cursor = doc.cursor_at(at);
    let Some(list) = cursor.current_list() else {
        return Ok(());
    };
    let (style, level) = (list.style(), list.indent());
    let target = match (deeper, level) {
        (true, _) => level.saturating_add(1),
        (false, 0) => return Ok(()),
        (false, _) => level - 1,
    };
    cursor.begin_edit_block();
    let result = cursor
        .remove_current_block_from_list()
        .and_then(|()| cursor.create_list(style))
        .and_then(|()| {
            cursor.set_current_list_format(&text_document::ListFormat {
                indent: Some(target),
                ..Default::default()
            })
        });
    cursor.end_edit_block();
    result
}

/// Whether `doc` holds a footnote definition.
fn holds_definitions(doc: &TextDocument) -> bool {
    doc.rope_store_for_test()
        .frames
        .read()
        .values()
        .any(|frame| frame.footnote_label.is_some())
}

/// Load a text over the document, as a host reloads a scene another command rewrote: the
/// document's own Djot, or a fragment in its syntax, through the setter for it. Nothing of
/// the content replaced stays: no table, cell or list outside the new content, and no
/// history to undo into it.
fn reload_in_place(doc: &TextDocument, rng: &mut Rng, fragments: &[(&str, String)]) -> String {
    let (syntax, text) = if rng.below(3) == 0 {
        ("djot", doc.to_djot().unwrap_or_default())
    } else {
        let (syntax, text) = &fragments[rng.below(fragments.len())];
        (*syntax, text.clone())
    };
    let loaded = match syntax {
        "djot" if rng.below(2) == 0 => doc.set_djot_sync(&text).map(|_| ()),
        "djot" => doc.set_djot(&text).and_then(|op| op.wait()).map(|_| ()),
        "markdown" => doc.set_markdown(&text).and_then(|op| op.wait()).map(|_| ()),
        _ => doc.set_html(&text).and_then(|op| op.wait()).map(|_| ()),
    };
    let edit = format!("reload {syntax} {text:?}");
    if let Err(error) = loaded {
        panic!("{edit}: {error}");
    }
    assert!(
        !doc.can_undo() && !doc.can_redo(),
        "{edit}: the history of the replaced content survived"
    );
    let store = doc.rope_store_for_test();
    let used_lists: HashSet<u64> = store
        .blocks
        .read()
        .values()
        .filter_map(|block| block.list)
        .collect();
    let stored_lists: HashSet<u64> = store.lists.read().keys().copied().collect();
    assert_eq!(
        stored_lists, used_lists,
        "{edit}: lists of the replaced content stayed"
    );
    edit
}

fn env_count(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// The seeds a random test runs: `STRUCTURAL_EDIT_SEEDS` of them (`default` when unset),
/// from `STRUCTURAL_EDIT_FIRST_SEED` on, to replay one failure.
fn seed_range(default: u64) -> std::ops::Range<u64> {
    let first = env_count("STRUCTURAL_EDIT_FIRST_SEED", 0);
    first..first + env_count("STRUCTURAL_EDIT_SEEDS", default)
}

/// With `STRUCTURAL_EDIT_TRACE` set, each edit and the text it leaves, for replaying one.
fn trace(doc: &TextDocument, edit: &str) {
    if std::env::var_os("STRUCTURAL_EDIT_TRACE").is_some() {
        eprintln!(
            "{edit}\n    {:?}\n    {:?}",
            doc.to_addressable_text(),
            doc.to_djot()
        );
    }
}

// ── The differential ─────────────────────────────────────────────────────────

/// Random structural edits over every document of the corpus, the model checked after each.
#[test]
fn random_structural_edits_keep_the_model() {
    let steps = env_count("STRUCTURAL_EDIT_STEPS", 12) as usize;
    let corpus = corpus();
    let fragments = fragments();
    for (name, text) in &corpus {
        assert_model(&load(text), &format!("loading {name}"));
    }
    for seed in seed_range(200) {
        let mut rng = Rng::new(seed);
        let (name, text) = &corpus[rng.below(corpus.len())];
        let doc = load(text);
        let mut done: Vec<String> = Vec::new();
        for _ in 0..steps {
            let edit = guarded("the edit", || random_edit(&doc, &mut rng, &fragments))
                .unwrap_or_else(|panic| panic!("seed {seed} on {name}: {done:?} then {panic}"));
            trace(&doc, &edit);
            done.push(edit);
            if let Err(error) = check(&doc) {
                panic!("seed {seed} on {name}, after {done:?}: {error}");
            }
        }
    }
}

/// What a writer sees of a document, for comparing two states of it.
fn seen(doc: &TextDocument) -> Result<(String, String), String> {
    Ok((
        guarded("to_djot", || doc.to_djot())?.map_err(|e| e.to_string())?,
        guarded("to_addressable_text", || doc.to_addressable_text())?.map_err(|e| e.to_string())?,
    ))
}

/// Each random edit, undone, gives back the document as it was, and redone, the document as
/// the edit left it. The history is cleared before each edit so that one Undo takes back that
/// edit and nothing else.
#[test]
fn random_structural_edits_undo_and_redo_exactly() {
    let steps = env_count("STRUCTURAL_EDIT_STEPS", 8) as usize;
    let corpus = corpus();
    let fragments = fragments();
    for seed in seed_range(120) {
        let mut rng = Rng::new(seed ^ 0x5EED);
        let (name, text) = &corpus[rng.below(corpus.len())];
        let doc = load(text);
        let mut done: Vec<String> = Vec::new();
        for _ in 0..steps {
            doc.clear_undo_redo();
            let before = seen(&doc).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
            let edit = guarded("the edit", || random_edit(&doc, &mut rng, &fragments))
                .unwrap_or_else(|panic| panic!("seed {seed} on {name}: {done:?} then {panic}"));
            trace(&doc, &edit);
            done.push(edit);
            let after =
                seen(&doc).unwrap_or_else(|e| panic!("seed {seed} on {name}, after {done:?}: {e}"));
            if !doc.can_undo() {
                continue;
            }
            guarded("undo", || doc.undo())
                .and_then(|r| r.map_err(|e| e.to_string()))
                .unwrap_or_else(|e| panic!("seed {seed} on {name}, undoing {done:?}: {e}"));
            let undone = seen(&doc).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
            assert_eq!(undone, before, "seed {seed} on {name}: undoing {done:?}");
            guarded("redo", || doc.redo())
                .and_then(|r| r.map_err(|e| e.to_string()))
                .unwrap_or_else(|e| panic!("seed {seed} on {name}, redoing {done:?}: {e}"));
            let redone = seen(&doc).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
            assert_eq!(redone, after, "seed {seed} on {name}: redoing {done:?}");
            if let Err(error) = check(&doc) {
                panic!("seed {seed} on {name}, after redoing {done:?}: {error}");
            }
        }
    }
}

// ── Footnote references ──────────────────────────────────────────────────────

/// Restoring a version (select all, insert its Djot) over a scene whose notes have bodies
/// left the first reference's anchor behind past its block's end and the first note's body
/// out of the rope: the next export sliced past the text and panicked.
#[test]
fn restoring_a_text_over_footnote_definitions_exports_without_panicking() {
    let doc = load(
        "Line one has a note[^n0].\n\n[^n0]: Note zero.\n\nLine two has a note[^n1].\n\n\
         [^n1]: Note one.\n",
    );
    restore(&doc, "A plain paragraph.\n\nAnother paragraph.\n");
    assert_model(&doc, "the restore");
    assert!(
        doc.to_plain_text()
            .unwrap()
            .starts_with("A plain paragraph.\nAnother paragraph.")
    );
}

/// Skribisto keeps note bodies in its own store, so a scene holds references alone. Putting
/// a version back over one kept the references of the deleted text on the blocks that took
/// their place, at offsets past their end: saving the scene (which serialises it with
/// `to_djot`) panicked.
#[test]
fn restoring_a_text_over_bare_footnote_references_exports_without_panicking() {
    let doc = load(
        &(0..10)
            .map(|i| format!("Line {i} has a note[^n{i}].\n\n"))
            .collect::<String>(),
    );
    let past: String = (0..30).map(|i| format!("{}\n\n", paragraph(i))).collect();
    restore(&doc, &past);
    assert_model(&doc, "the restore");
    assert!(doc.footnote_references().is_empty(), "the references went");
    assert!(!doc.to_djot().unwrap().contains("[^"), "none is saved");
}

/// Typing, breaking a paragraph and replacing text in front of a reference moved the
/// reference's text but not its anchor: the save put `[^a]` inside the word and a bare
/// `U+FFFC` where the reference was, and a paragraph break made the next export panic.
#[test]
fn edits_before_a_footnote_reference_move_it_with_its_text() {
    let doc = load("Hello[^a] world.\n");
    doc.cursor_at(0).insert_text("XXXX").unwrap();
    assert_eq!(doc.to_djot().unwrap(), "XXXXHello[^a] world.");
    assert_eq!(doc.footnote_references(), vec![(9, "a".to_string())]);

    let doc = load("Hello[^a] world.\n");
    doc.cursor_at(2).insert_block().unwrap();
    assert_eq!(doc.to_djot().unwrap(), "He\n\nllo[^a] world.");
    assert_model(&doc, "a paragraph break before the reference");

    let doc = load("Hello[^a] world.\n");
    doc.replace_text("Hello", "Goodbye", true, &Default::default())
        .unwrap();
    assert_eq!(doc.to_djot().unwrap(), "Goodbye[^a] world.");

    let doc = load("Hello[^a] world.\n");
    select(&doc, 0, 3).remove_selected_text().unwrap();
    assert_eq!(doc.to_djot().unwrap(), "lo[^a] world.");
    assert_model(&doc, "a deletion before the reference");
}

/// Joining two paragraphs with Backspace dropped the second one's references, leaving
/// their `U+FFFC` in the text as a bare character; deleting a range over a reference left
/// its anchor behind, past the text.
#[test]
fn joining_or_deleting_over_a_footnote_reference_keeps_the_anchors_true() {
    let doc = load("First para.\n\nHello[^a] world.\n");
    doc.cursor_at(12).delete_previous_char().unwrap();
    assert_eq!(doc.to_djot().unwrap(), "First para.Hello[^a] world.");
    assert_model(&doc, "the join");

    let doc = load("Hello[^a] world.\n");
    select(&doc, 3, 8).remove_selected_text().unwrap();
    assert_eq!(doc.to_djot().unwrap(), "Helorld.");
    assert!(doc.footnote_references().is_empty());
    assert_model(&doc, "the deletion over the reference");
}

/// Where a reference is reported was computed from a block position the edits leave stale:
/// after typing in an earlier paragraph, every later reference was reported short of its
/// sentinel.
#[test]
fn footnote_references_are_reported_where_they_are_after_typing() {
    let doc = load("One.\n\nTwo has a note[^a].\n");
    doc.cursor_at(0).insert_text("Typed. ").unwrap();
    let text: Vec<char> = doc.to_addressable_text().unwrap().chars().collect();
    let references = doc.footnote_references();
    assert_eq!(references.len(), 1);
    assert_eq!(text[references[0].0], SENTINEL, "{references:?}");
}

/// A footnote's body sits in the rope where it was written. Emptying the main text around
/// it reset the whole rope, taking the body's text with it.
#[test]
fn deleting_the_whole_text_before_a_note_body_keeps_the_body() {
    let doc = load("First line.\n\n| a | b |\n\nSee[^n].\n\n[^n]: The body.\n");
    let body = position_of(&doc, "The body");
    select(&doc, 0, body + 1).remove_selected_text().unwrap();
    assert_model(&doc, "the deletion");
    assert!(
        doc.to_djot().unwrap().contains("he body."),
        "{:?}",
        doc.to_djot()
    );
}

/// A range can start in a note's body, since the rope holds it. Typing over one that ran
/// from a body into the paragraph after resolved both ends to one block, the end before the
/// start, and panicked on the subtraction.
#[test]
fn replacing_a_selection_from_a_note_body_does_not_panic() {
    let doc = load("Noted[^a].\n\n[^a]: The note.\n\nLast line.\n");
    let body = position_of(&doc, "note.");
    let end = length(&doc) - 2;
    select(&doc, body, end).insert_text("over").unwrap();
    assert_model(&doc, "typing over the range");
}

/// A table pasted into a note's body has cell frames of its own, with no parent. The
/// plain-text export took them for prose and printed the cells in the middle of the text.
#[test]
fn a_table_pasted_into_a_note_body_stays_out_of_the_prose() {
    let doc = load("Noted[^a] here.\n\n[^a]: The note.\n\nLast line.\n");
    let body = position_of(&doc, "note.");
    doc.cursor_at(body)
        .insert_djot("a\n\n| x | y |\n\nb")
        .unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Noted￼ here.\nLast line.");
    assert_model(&doc, "the paste");
}

// ── Quotations ───────────────────────────────────────────────────────────────

/// The deletion recounted block positions from the main frame's own paragraphs, leaving
/// every quotation's out of the count: it wrote the paragraphs after a quotation back at
/// the wrong positions, and a paste over a quarter of the text left formatting runs past
/// the ends of their blocks. The next export panicked.
#[test]
fn pasting_over_a_quarter_of_nested_quotes_exports_without_panicking() {
    let quotes: String = (0..8)
        .map(|i| format!("{}\n\n> quoted {i}\n>\n> > nested {i}\n\n", paragraph(i)))
        .collect();
    let plain: String = (0..30).map(|i| format!("{}\n\n", paragraph(i))).collect();
    let doc = load(&quotes);
    let n = length(&doc);
    let cursor = select(&doc, n / 4, n / 2);
    cursor.insert_djot(&plain).unwrap();
    assert_model(&doc, "the paste over a quarter");
    cursor
        .insert_markdown("Some *markdown*\n\n- a\n- b\n\n| x | y |\n|---|---|\n| 1 | 2 |\n")
        .unwrap();
    cursor
        .insert_html("<p>html <b>bold</b></p><blockquote><p>q</p></blockquote>")
        .unwrap();
    assert_model(&doc, "the Markdown and HTML pastes after it");
}

/// The ranges the differential deleted in the footnote and quotation documents. Each left
/// blocks out of the rope, anchors past their text, or paragraphs in the wrong order.
#[test]
fn deleting_ranges_in_footnote_and_quote_documents_keeps_the_model() {
    let notes: String = (0..10)
        .map(|i| format!("Line {i} has a note[^n{i}].\n\n[^n{i}]: Note {i}.\n\n"))
        .collect();
    let quotes: String = (0..8)
        .map(|i| format!("{}\n\n> quoted {i}\n>\n> > nested {i}\n\n", paragraph(i)))
        .collect();
    for (text, ranges) in [
        (
            &notes,
            vec![(0, 190), (63, 126), (1, 189), (0, 95), (38, 78)],
        ),
        (&quotes, vec![(236, 472), (0, 15), (40, 200)]),
    ] {
        for (from, to) in ranges {
            let doc = load(text);
            let to = to.min(length(&doc));
            select(&doc, from, to).remove_selected_text().unwrap();
            assert_model(&doc, &format!("deleting {from}..{to}"));
            doc.undo().unwrap();
            assert_model(&doc, &format!("undoing the deletion of {from}..{to}"));
            doc.redo().unwrap();
            assert_model(&doc, &format!("redoing the deletion of {from}..{to}"));
        }
    }
}

/// Backspace at a block start joins two blocks. In a document of nested quotations the
/// recounted positions put the blocks out of order, the range's end resolved before its
/// start, and `delete_text` panicked slicing the blocks between them.
#[test]
fn backspace_at_block_starts_in_nested_quotes_does_not_panic() {
    // The differential's own sequence: from the end, every tenth block start. The third
    // Backspace panicked with "slice index starts at 4 but ends at 2".
    let quotes: String = (0..8)
        .map(|i| format!("{}\n\n> quoted {i}\n>\n> > nested {i}\n\n", paragraph(i)))
        .collect();
    let doc = load(&quotes);
    let starts: Vec<usize> = doc.blocks().iter().map(|block| block.position()).collect();
    for &at in starts.iter().rev().step_by(10) {
        if at > 0 {
            doc.cursor_at(at).delete_previous_char().unwrap();
            assert_model(&doc, &format!("Backspace at {at}"));
        }
    }
    // And at every block start, from the end, in the smaller quotation documents.
    for name in ["quotes", "mixed"] {
        let doc = corpus_document(name);
        let starts: Vec<usize> = doc.blocks().iter().map(|block| block.position()).collect();
        for &at in starts.iter().rev() {
            if at > 0 {
                doc.cursor_at(at).delete_previous_char().unwrap();
                assert_model(&doc, &format!("Backspace at {at} in {name}"));
            }
        }
    }
}

/// A paste into a paragraph after a quotation was listed in the frame at the paragraph's
/// index among all the document's blocks, further down the frame than the rope put it.
#[test]
fn a_paste_after_a_quotation_lands_where_the_caret_is() {
    let doc = load("Before.\n\n> Q1.\n>\n> Q2.\n\nAfter one.\n\nAfter two.\n");
    let at = position_of(&doc, "After one.") + "After one.".len();
    doc.cursor_at(at)
        .insert_djot("# Pasted\n\n# Twice\n")
        .unwrap();
    assert_model(&doc, "the paste");
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "Before.\nQ1.\nQ2.\nAfter one.\nPasted\nTwice\nAfter two."
    );
}

/// Putting back a version of a text selects all of it and inserts the version's Djot. The
/// fragment a Djot, Markdown or HTML text becomes carried no quotation depth, and dropped the
/// code-block flag and the alignment: every quotation and code block of the restored text came
/// back as a plain paragraph, an epigraph lost its quotation and its alignment, and the next
/// save kept it so. A text put back reads as the same text loaded.
#[test]
fn putting_back_a_version_keeps_its_quotations_and_code_blocks() {
    for text in [
        "First.\n\n> A quotation.\n\nLast.",
        "Para.\n\n> quoted",
        "> - a\n> - b",
        "```\ncode\n```",
        "```rust\nfn main() {}\n```\n\nAfter.",
        "> {semantic_role=epigraph}\n> The sea is not a place; it is a going.\n>\n> \
         {alignment=right}\n> Anon., *Tidewater*\n\nChapter text.",
        "> quoted *a*\n>\n> > nested b\n\nAfter.",
        "> > Deep first.\n>\n> Shallow.\n\nOut.",
        "Before.\n\n> In the quote.\n>\n> | a | b |\n\nAfter.",
        "{alignment=center}\nCentered.\n\n> Quoted.",
        "> Quote one.\n\nBetween.\n\n> Quote two.",
    ] {
        let expected = load(text).to_djot().unwrap();
        let doc = load("Now.");
        restore(&doc, text);
        assert_model(&doc, &format!("restoring {text:?}"));
        assert_eq!(doc.to_djot().unwrap(), expected, "restoring {text:?}");
    }

    // The same holds for Markdown, and for HTML but its `<pre>`, which a paste reads as
    // paragraphs (see `a_preformatted_passage_pasted_as_html_goes_in_as_its_lines`).
    let doc = load("Now.");
    let cursor = doc.cursor();
    cursor.select(SelectionType::Document);
    cursor
        .insert_markdown("Para.\n\n> quoted\n\n```\ncode\n```\n")
        .unwrap();
    assert_eq!(
        doc.to_djot().unwrap(),
        "Para.\n\n> quoted\n\n```\ncode\n```"
    );
    let doc = load("Now.");
    let cursor = doc.cursor();
    cursor.select(SelectionType::Document);
    cursor
        .insert_html("<p>Para.</p><blockquote><p>quoted</p></blockquote><pre>code</pre>")
        .unwrap();
    assert_eq!(doc.to_djot().unwrap(), "Para.\n\n> quoted\n\ncode");
}

/// Deleting everything leaves one empty paragraph, which keeps the formatting of the first
/// one deleted and stays in its quotations. A version put back over everything merged its first
/// paragraph into it: it came back a heading, a code block, a list item or an epigraph it
/// never was. Pasted into an empty text, a text keeps its own formatting, and one Undo gives
/// back the empty paragraph as it was.
#[test]
fn putting_back_a_version_over_any_text_reads_as_the_version_loaded() {
    let versions = [
        "First.\n\n> A quotation.\n\nLast.",
        "See this[^f1] and that.",
        "| p | q |\n\nafter",
    ];
    for current in [
        "# Title\n\nText.",
        "```\ncode\n```\n\nText.",
        "> {semantic_role=epigraph}\n> Epi.\n\nText.",
        "> > Deep.\n\nText.",
        "- item\n- two",
        "| solo |",
        "|  |",
    ] {
        for version in versions {
            let doc = load(current);
            restore(&doc, version);
            assert_model(&doc, &format!("putting {version:?} back over {current:?}"));
            assert_eq!(
                doc.to_djot().unwrap(),
                load(version).to_djot().unwrap(),
                "putting {version:?} back over {current:?}"
            );
        }
    }

    // Removing a table a quotation held alone leaves the quotation, holding nothing, which
    // no export shows. The empty text around it is still empty.
    let doc = load("# Title\n\n> | a | b |\n");
    doc.cursor_at(position_of(&doc, "a"))
        .remove_current_table()
        .unwrap();
    assert_eq!(doc.to_djot().unwrap(), "# Title");
    restore(&doc, "Plain.");
    assert_model(&doc, "a text put back over a quotation holding nothing");
    assert_eq!(doc.to_djot().unwrap(), "Plain.");

    let doc = load("# Heading\n\nText.\n");
    let cursor = select(&doc, 0, length(&doc));
    cursor.begin_edit_block();
    cursor.insert_djot("plain").unwrap();
    cursor.end_edit_block();
    assert_eq!(doc.to_djot().unwrap(), "plain");
    doc.undo().unwrap();
    assert_eq!(
        doc.to_djot().unwrap(),
        "# Heading\n\nText.",
        "one Undo gives the text back"
    );
}

/// Pasting at a caret in an empty paragraph is an edit of that paragraph: a fragment of one
/// plain paragraph takes its formatting, as typed text does. A heading line, a code block, a
/// centred line, a list item or a quotation the writer made of an empty line lost its
/// formatting to a paste, because every paste into an empty text first made its paragraph
/// plain, for the sake of a text put back over everything. Only a paste over a selection of
/// the whole text replaces the text with the pasted one, formatting and all: a selection
/// from its start to its end, or `select(SelectionType::Document)`, which in an empty text
/// selects nothing and still means the whole text.
#[test]
fn pasting_into_an_empty_formatted_line_keeps_its_format() {
    let heading = text_document::BlockFormat {
        heading_level: Some(1),
        ..Default::default()
    };
    let code = text_document::BlockFormat {
        is_code_block: Some(true),
        ..Default::default()
    };
    let centred = text_document::BlockFormat {
        alignment: Some(text_document::Alignment::Center),
        ..Default::default()
    };
    for (name, format, expected) in [
        ("a heading", &heading, "# Pasted title"),
        ("a code block", &code, "```\nPasted title\n```"),
        (
            "a centred line",
            &centred,
            "{alignment=center}\nPasted title",
        ),
    ] {
        for syntax in ["djot", "markdown", "html"] {
            let doc = TextDocument::new();
            doc.cursor().set_block_format(format).unwrap();
            paste(&doc.cursor(), syntax, "Pasted title");
            assert_eq!(
                doc.to_djot().unwrap(),
                expected,
                "{syntax} pasted into {name}"
            );
        }
        let doc = TextDocument::new();
        doc.cursor().set_block_format(format).unwrap();
        doc.cursor().insert_text("Pasted title").unwrap();
        assert_eq!(doc.to_djot().unwrap(), expected, "typed into {name}");
    }

    let doc = TextDocument::new();
    doc.cursor()
        .create_list(text_document::ListStyle::Disc)
        .unwrap();
    doc.cursor().insert_djot("milk").unwrap();
    assert_eq!(doc.to_djot().unwrap(), "- milk");

    let doc = TextDocument::new();
    doc.cursor().insert_blockquote().unwrap();
    doc.cursor().insert_djot("A quote").unwrap();
    assert_model(&doc, "the paste into the empty quotation");
    assert_eq!(doc.to_djot().unwrap(), "> A quote");

    // Deleting everything, then pasting at the caret: the empty paragraph keeps the first
    // paragraph's heading, as it does for typing.
    let doc = load("# Title\n\nText.\n");
    select(&doc, 0, length(&doc))
        .remove_selected_text()
        .unwrap();
    doc.cursor().insert_djot("New").unwrap();
    assert_eq!(doc.to_djot().unwrap(), "# New");
    // Pasting over the whole text: the text is the one pasted.
    let doc = load("# Title\n\nText.\n");
    select(&doc, 0, length(&doc)).insert_djot("New").unwrap();
    assert_eq!(doc.to_djot().unwrap(), "New");
    // Deleting everything, then putting a version back over all of the empty text, as a
    // host does it: the text is the version.
    let doc = load("# Title\n\nText.\n");
    select(&doc, 0, length(&doc))
        .remove_selected_text()
        .unwrap();
    restore(&doc, "New");
    assert_eq!(doc.to_djot().unwrap(), "New");
    for (name, format) in [("a heading", &heading), ("a code block", &code)] {
        let doc = TextDocument::new();
        doc.cursor().set_block_format(format).unwrap();
        restore(&doc, "Put back.\n\n> Quoted.");
        assert_model(&doc, &format!("a text put back over {name}"));
        assert_eq!(
            doc.to_djot().unwrap(),
            "Put back.\n\n> Quoted.",
            "a text put back over {name}"
        );
    }
    // Moving the caret after selecting everything is a caret again.
    let doc = TextDocument::new();
    doc.cursor().set_block_format(&heading).unwrap();
    let cursor = doc.cursor();
    cursor.select(SelectionType::Document);
    cursor.set_position(0, MoveMode::MoveAnchor);
    cursor.insert_djot("Kept").unwrap();
    assert_eq!(doc.to_djot().unwrap(), "# Kept");
}

/// A cut of all of a text leaves its cursor standing for the whole of the empty text, so the
/// paste that follows gives the text back as it was. Any edit made between the two ends
/// that: a heading, a list item or a quotation the writer makes of the emptied line, by this
/// cursor or another, or a text a load puts in, leaves the paste doing what it does in any
/// text that is one empty formatted line: a phrase goes into the line and keeps its
/// format, and a text of its own structure replaces it. The mark outlived every edit that
/// moved no text, so the two differed.
#[test]
fn an_edit_after_a_cut_of_everything_is_kept_by_the_paste() {
    type Edit = fn(&TextDocument, &text_document::TextCursor);
    fn heading() -> text_document::BlockFormat {
        text_document::BlockFormat {
            heading_level: Some(1),
            ..Default::default()
        }
    }
    let edits: [(&str, Edit); 6] = [
        ("a heading", |_, cursor| {
            cursor.set_block_format(&heading()).unwrap();
        }),
        ("a list item", |_, cursor| {
            cursor.create_list(text_document::ListStyle::Disc).unwrap();
        }),
        ("a quotation", |_, cursor| {
            cursor.insert_blockquote().unwrap();
        }),
        ("a heading another cursor makes", |doc, _| {
            doc.cursor().set_block_format(&heading()).unwrap();
        }),
        ("a heading loaded as Djot", |doc, _| {
            doc.set_djot_sync("#\n").unwrap();
        }),
        ("a heading loaded as HTML", |doc, _| {
            doc.set_html("<h1></h1>").unwrap().wait().unwrap();
        }),
    ];
    let source = load("One.\n\nTwo.\n");
    let copied = select(&source, 0, length(&source)).selection();
    for (name, edit) in edits {
        // The same edit of a new text's empty line, and the paste into it.
        let fresh = TextDocument::new();
        let cursor = fresh.cursor();
        edit(&fresh, &cursor);
        cursor.insert_fragment(&copied).unwrap();
        let expected = fresh.to_djot().unwrap();

        let doc = load("Alpha.\n\nBeta.\n");
        let cursor = select(&doc, 0, length(&doc));
        cursor.remove_selected_text().unwrap();
        edit(&doc, &cursor);
        cursor.insert_fragment(&copied).unwrap();
        assert_model(&doc, &format!("{name}, then a paste"));
        assert_eq!(
            doc.to_djot().unwrap(),
            expected,
            "{name} made of the line a cut of everything left, then a paste"
        );
    }
}

/// Cut everything, paste it back, undo the paste and paste again: the second paste gives
/// the text back as the first did, and so does a paste after the cut undone and redone. It
/// went into the line the undo leaves, which keeps the formatting and the quotation of the
/// first paragraph cut, and a scene opening with an epigraph came back quoted from end to
/// end: the mark saying that line stands for the whole text was the cursor's, and ended with
/// the first paste, where no undo or redo gives it back. A text pasted into a text that is
/// one empty paragraph now keeps its own formatting whatever made the text empty.
#[test]
fn pasting_again_after_undoing_the_paste_of_a_cut_of_everything_changes_nothing() {
    let text = "> {semantic_role=epigraph}\n> The sea.\n\n# Title\n\nAlpha.\n";
    let doc = load(text);
    let original = doc.to_djot().unwrap();
    let cursor = select(&doc, 0, length(&doc));
    let cut = cursor.selection();
    cursor.remove_selected_text().unwrap();
    cursor.insert_fragment(&cut).unwrap();
    assert_eq!(doc.to_djot().unwrap(), original, "the first paste");
    doc.undo().unwrap();
    cursor.insert_fragment(&cut).unwrap();
    assert_eq!(
        doc.to_djot().unwrap(),
        original,
        "a second paste after undoing the first"
    );

    let doc = load(text);
    let cursor = select(&doc, 0, length(&doc));
    let cut = cursor.selection();
    cursor.remove_selected_text().unwrap();
    doc.undo().unwrap();
    doc.redo().unwrap();
    cursor.insert_fragment(&cut).unwrap();
    assert_eq!(
        doc.to_djot().unwrap(),
        original,
        "a paste after the cut undone and redone"
    );
}

/// A text of its own structure pasted into a text that is one empty paragraph keeps its own
/// formatting, whatever made the text empty: a new document whose line the writer made a
/// heading or a quotation, a removal of everything by another cursor than the one pasting,
/// an undo back to an empty text. A phrase goes into the paragraph and keeps its format, as
/// typing does. The text's first paragraph went into the empty line's heading, and all of
/// it into the empty line's quotation, unless the cursor pasting had just cut everything.
#[test]
fn a_text_pasted_into_an_empty_text_keeps_its_formatting_however_it_came_to_be_empty() {
    let text = load("Plain.\n\n> Quoted.\n");
    let copied = select(&text, 0, length(&text)).selection();
    let expected = text.to_djot().unwrap();
    /// What emptied the text, how to make that text, and the format its empty line keeps.
    type Emptied = (&'static str, fn() -> TextDocument, &'static str);
    let emptied: [Emptied; 4] = [
        (
            "a new document with a heading line",
            || {
                let doc = TextDocument::new();
                doc.cursor()
                    .set_block_format(&text_document::BlockFormat {
                        heading_level: Some(2),
                        ..Default::default()
                    })
                    .unwrap();
                doc
            },
            "## ",
        ),
        (
            "a new document with a quotation line",
            || {
                let doc = TextDocument::new();
                doc.cursor().insert_blockquote().unwrap();
                doc
            },
            "> ",
        ),
        (
            "a text emptied by another cursor",
            || {
                let doc = load("## Title\n\nText.\n");
                select(&doc, 0, length(&doc))
                    .remove_selected_text()
                    .unwrap();
                doc
            },
            "## ",
        ),
        (
            "an undo back to an empty text",
            || {
                let doc = load("> Title\n\nText.\n");
                select(&doc, 0, length(&doc))
                    .remove_selected_text()
                    .unwrap();
                doc.cursor().insert_text("x").unwrap();
                doc.undo().unwrap();
                doc
            },
            "> ",
        ),
    ];
    for (name, make, format) in emptied {
        let doc = make();
        doc.cursor().insert_fragment(&copied).unwrap();
        assert_model(&doc, name);
        assert_eq!(
            doc.to_djot().unwrap(),
            expected,
            "a text pasted into {name}"
        );

        let doc = make();
        doc.cursor().insert_djot("Phrase").unwrap();
        assert_eq!(
            doc.to_djot().unwrap(),
            format!("{format}Phrase"),
            "a phrase pasted into {name}"
        );

        let doc = make();
        doc.cursor().insert_text("Typed").unwrap();
        assert_eq!(
            doc.to_djot().unwrap(),
            format!("{format}Typed"),
            "typing into {name}"
        );
    }
}

/// Selecting all of a text and pasting a phrase over it, a few words copied from the text or
/// from a web page, is an edit of the text's first paragraph, as typing over everything is:
/// the phrase takes that paragraph's direction, alignment, heading level and quotation. Every
/// paste over all of a text made that paragraph plain first, for the sake of a version put
/// back: a right-to-left scene came back left to right, a quoted letter unquoted and a
/// centred line uncentred. A whole text (Djot or Markdown a host inserts, or a whole
/// document) and a fragment with a structure of its own still replace the text with theirs.
#[test]
fn pasting_a_phrase_over_the_whole_text_keeps_its_first_paragraph_as_typing_does() {
    let rtl = text_document::BlockFormat {
        direction: Some(text_document::TextDirection::RightToLeft),
        ..Default::default()
    };
    for (name, text) in [
        (
            "right to left",
            "\u{645}\u{631}\u{62d}\u{628}\u{627} \u{628}\u{627}\u{644}\u{639}\u{627}\u{644}\u{645}.",
        ),
        (
            "centred",
            "{alignment=center}\nA centred line here.\n\nMore.",
        ),
        ("quoted", "> A letter, here.\n>\n> Signed.\n\nAfter."),
        ("a heading", "# A title here\n\nText."),
    ] {
        let source = load(text);
        if name == "right to left" {
            source.cursor_at(1).set_block_format(&rtl).unwrap();
        }
        let original = source.to_djot().unwrap();
        let phrase = select(&source, 2, 5).selection();
        let words = phrase.to_plain_text().to_string();

        let typed = load(&original);
        let cursor = typed.cursor();
        cursor.select(SelectionType::Document);
        cursor.insert_text(&words).unwrap();
        let typed = typed.to_djot().unwrap();

        let pasted = load(&original);
        let cursor = pasted.cursor();
        cursor.select(SelectionType::Document);
        cursor.insert_fragment(&phrase).unwrap();
        assert_model(&pasted, &format!("a phrase pasted over {name}"));
        assert_eq!(
            pasted.to_djot().unwrap(),
            typed,
            "a phrase copied from {name} and pasted over all of it"
        );
        let html = load(&original);
        let cursor = html.cursor();
        cursor.select(SelectionType::Document);
        cursor
            .insert_html(&format!("<span>{words}</span>"))
            .unwrap();
        assert_eq!(
            html.to_djot().unwrap(),
            typed,
            "a phrase pasted as HTML over all of {name}"
        );
    }

    // A whole text replaces the text, a phrase as much as a longer one.
    for (syntax, text) in [("djot", "New"), ("markdown", "New")] {
        let doc = load("# Title\n\nText.\n");
        paste(&select(&doc, 0, length(&doc)), syntax, text);
        assert_eq!(doc.to_djot().unwrap(), "New", "{syntax} over all of a text");
    }
    let whole = DocumentFragment::from_document(&load("Plain.")).unwrap();
    let doc = load("> # Title\n\nText.\n");
    select(&doc, 0, length(&doc))
        .insert_fragment(&whole)
        .unwrap();
    assert_eq!(
        doc.to_djot().unwrap(),
        "Plain.",
        "a whole document over all of a text"
    );

    // A fragment with a structure of its own replaces the text too: two paragraphs copied,
    // and one heading pasted as HTML.
    let copied = {
        let source = load("First.\n\nSecond.\n");
        let cursor = source.cursor();
        cursor.select(SelectionType::Document);
        cursor.selection()
    };
    let doc = load("# Title\n\n> Text.\n");
    select(&doc, 0, length(&doc))
        .insert_fragment(&copied)
        .unwrap();
    assert_model(&doc, "two paragraphs pasted over all of a text");
    assert_eq!(doc.to_djot().unwrap(), "First.\n\nSecond.");
    let doc = load("> A letter.\n\nText.\n");
    select(&doc, 0, length(&doc))
        .insert_html("<h2>Heading</h2>")
        .unwrap();
    assert_model(&doc, "a heading pasted over all of a text");
    assert_eq!(doc.to_djot().unwrap(), "## Heading");
}

/// A quotation pasted into a paragraph goes in as a quotation, between the two halves of the
/// paragraph; pasted into a quotation it is not quoted twice; pasted into a table cell or a
/// note's body, which hold paragraphs only, it goes in as paragraphs.
#[test]
fn a_pasted_quotation_stays_a_quotation_where_one_can_stand() {
    let doc = load("Before after.\n");
    doc.cursor_at(7).insert_djot("> quoted").unwrap();
    assert_model(&doc, "the paste into a paragraph");
    assert_eq!(doc.to_djot().unwrap(), "Before {}\n\n> quoted\n\nafter.");

    let doc = load("> One two.\n\nOut.\n");
    doc.cursor_at(3).insert_djot("> in\n\nplain").unwrap();
    assert_model(&doc, "the paste into a quotation");
    assert_eq!(
        doc.to_djot().unwrap(),
        "> One\n>\n> in\n>\n> plain two.\n\nOut."
    );

    let doc = load("> One.\n\nOut.\n");
    doc.cursor_at(3).insert_djot("> > nested\n").unwrap();
    assert_model(&doc, "a nested quotation pasted into a quotation");
    assert_eq!(
        doc.to_djot().unwrap(),
        "> One\n>\n> > nested\n>\n> .\n\nOut."
    );

    let doc = load("Start.\n\n| abc | d |\n\nEnd.\n");
    doc.cursor_at(position_of(&doc, "bc"))
        .insert_djot("x\n\n> q\n\ny")
        .unwrap();
    assert_model(&doc, "the paste into a cell");

    let doc = load("Noted[^a] here.\n\n[^a]: The note.\n");
    doc.cursor_at(position_of(&doc, "note."))
        .insert_djot("x\n\n> q\n\ny")
        .unwrap();
    assert_model(&doc, "the paste into a note");
}

/// A paragraph of its own format (a quoted paragraph, a heading, a code block) pasted at
/// the end of a paragraph goes in after it, and nothing follows it: the paste left an empty
/// paragraph after it, which a paste of several paragraphs never did, and a save dropped.
/// The caret stands at the end of what was pasted.
#[test]
fn a_formatted_paragraph_pasted_at_the_end_of_a_paragraph_leaves_no_empty_one_after_it() {
    for (fragment, words) in [
        ("> Quoted.", "Quoted."),
        ("# Title", "Title"),
        ("```\ncode\n```", "code"),
    ] {
        for (text, rest) in [("Some text.\n", ""), ("Some text.\n\nMore.\n", "\n\nMore.")] {
            let doc = load(text);
            let cursor = doc.cursor_at("Some text.".len());
            cursor.insert_djot(fragment).unwrap();
            assert_model(&doc, &format!("{fragment:?} pasted at the end of {text:?}"));
            let blocks: Vec<String> = doc.blocks().iter().map(|block| block.text()).collect();
            assert!(
                !blocks.iter().any(String::is_empty),
                "{fragment:?} pasted at the end of {text:?}: an empty paragraph in {blocks:?}"
            );
            assert_eq!(
                doc.to_djot().unwrap(),
                format!("Some text.\n\n{fragment}{rest}"),
                "{fragment:?} into {text:?}"
            );
            assert_caret_after(&doc, &cursor, words);
        }
    }
    // The same passage copied from a text.
    let source = load("> Quoted.\n\nPlain.\n");
    let copied = select(&source, 0, position_of(&source, "Plain.")).selection();
    let doc = load("Some text.\n");
    doc.cursor_at("Some text.".len())
        .insert_fragment(&copied)
        .unwrap();
    assert_eq!(doc.to_djot().unwrap(), "Some text.\n\n> Quoted.");
    assert_eq!(doc.blocks().len(), 2);
}

/// A paste opening with a paragraph of its own format, at the start of a paragraph, puts
/// that paragraph in the first one's place, and the paragraph's own text after the paste
/// keeps its format: its heading level, list, quotation or alignment. It was made a plain
/// paragraph: a heading cut with the table after it and pasted back at the start of the
/// next heading lost that heading's level. (The structural differential's seed 29.)
#[test]
fn the_rest_of_a_paragraph_a_formatted_paste_goes_into_keeps_its_format() {
    let text = "## One\n\n| c1 | c2 |\n|---|---|\n\n## Two\n";
    let doc = load(text);
    let original = doc.to_djot().unwrap();
    let cursor = doc.cursor_at(position_of(&doc, "c1"));
    cursor.set_position(position_of(&doc, "One"), MoveMode::KeepAnchor);
    let cut = cursor.selection();
    cursor.remove_selected_text().unwrap();
    assert_eq!(doc.to_djot().unwrap(), "## Two");
    cursor.insert_fragment(&cut).unwrap();
    assert_model(&doc, "cut and pasted back");
    assert_eq!(doc.to_djot().unwrap(), original, "cut and pasted back");

    for (into, expected) in [
        ("## Title\n", "# Pasted\n\n| t |\n|---|\n\n## Title"),
        ("- item\n", "# Pasted\n\n| t |\n|---|\n\n- item"),
        ("> quoted\n", "> # Pasted\n>\n> | t |\n> |---|\n>\n> quoted"),
    ] {
        let doc = load(into);
        doc.cursor_at(0).insert_djot("# Pasted\n\n| t |\n").unwrap();
        assert_model(&doc, into);
        assert_eq!(
            doc.to_djot().unwrap(),
            expected,
            "pasted at the start of {into:?}"
        );
    }
}

/// A selection across a quotation copies each of its paragraphs once. Every frame of the
/// document was read, each quotation's once for itself and once more for every frame it is
/// nested in: a paragraph of a quotation nested in another was copied three times.
#[test]
fn copying_across_quotations_copies_each_paragraph_once_and_keeps_them_quoted() {
    let text = "Para.\n\n> quoted\n>\n> > deeper\n\n| a | b |\n\nEnd.\n";
    let doc = load(text);
    let cursor = doc.cursor();
    cursor.select(SelectionType::Document);
    let copied = cursor.selection();
    assert_eq!(copied.to_plain_text(), "Para.\nquoted\ndeeper\na\nb\nEnd.");
    let pasted = load("");
    pasted.cursor().insert_fragment(&copied).unwrap();
    assert_model(&pasted, "the paste of the copy");
    assert_eq!(pasted.to_djot().unwrap(), load(text).to_djot().unwrap());
}

// ── Code blocks ──────────────────────────────────────────────────────────────

/// The labels of `doc`'s footnote references, in order, and those of a reload of its Djot.
fn references_and_reloaded(doc: &TextDocument) -> (Vec<String>, Vec<String>) {
    let labels = |doc: &TextDocument| -> Vec<String> {
        doc.footnote_references()
            .into_iter()
            .map(|(_, label)| label)
            .collect()
    };
    (labels(doc), labels(&load(&doc.to_djot().unwrap())))
}

/// A code block is verbatim text: the save writes its characters and nothing else. Joining a
/// paragraph holding a note's reference or an image into one kept them in the model, so the
/// editor showed them, while the save dropped them: the note lost its place in the text at
/// the next reopening. Backspace and Delete there join nothing now, and a range across them
/// takes the text it covers and joins nothing, and says which text it took.
#[test]
fn nothing_joins_a_note_reference_or_an_image_into_a_code_block() {
    let text = "```\ncode\n```\n\nNoted[^a] here ![pic](p.png).\n";
    let original = load(text).to_djot().unwrap();
    let doc = load(text);
    doc.cursor_at(position_of(&doc, "Noted"))
        .delete_previous_char()
        .unwrap();
    assert_model(&doc, "Backspace after the code block");
    assert_eq!(doc.to_djot().unwrap(), original);

    let doc = load(text);
    doc.cursor_at("code".len()).delete_char().unwrap();
    assert_model(&doc, "Delete at the end of the code block");
    assert_eq!(doc.to_djot().unwrap(), original);

    let doc = load(text);
    let removed = select(&doc, 2, position_of(&doc, "ted"))
        .remove_selected_text()
        .unwrap();
    assert_model(&doc, "a deletion from the code block into the paragraph");
    assert_eq!(removed, "de\nNo", "the text the deletion took");
    assert_eq!(
        doc.to_djot().unwrap(),
        "```\nco\n```\n\nted[^a] here ![pic](p.png)."
    );
    let (references, reloaded) = references_and_reloaded(&doc);
    assert_eq!(references, vec!["a".to_string()]);
    assert_eq!(references, reloaded);

    // Text alone still joins.
    let doc = load("```\ncode\n```\n\nplain words\n");
    doc.cursor_at(position_of(&doc, "plain"))
        .delete_previous_char()
        .unwrap();
    assert_model(&doc, "Backspace joining text into the code block");
    assert_eq!(doc.to_djot().unwrap(), "```\ncodeplain words\n```");
}

/// An image, a note's reference, or a paste carrying either, goes into no code block: the
/// insertion is refused and the text left as it was. Making a paragraph that holds one a
/// code block is refused the same way.
#[test]
fn a_code_block_takes_no_image_and_no_note_reference() {
    let text = "```\ncode\n```\n\nPlain[^a].\n";
    let original = load(text).to_djot().unwrap();

    let doc = load(text);
    assert!(doc.cursor_at(2).insert_image("p.png", "pic", 4, 4).is_err());
    assert!(doc.cursor_at(2).insert_footnote_reference("z").is_err());
    assert!(doc.cursor_at(2).insert_djot("see[^y] this").is_err());
    assert!(
        doc.cursor_at(2)
            .insert_djot("one\n\ntwo ![pic](p.png)")
            .is_err()
    );
    assert_model(&doc, "the refused insertions");
    assert_eq!(doc.to_djot().unwrap(), original);
    // Text still goes in, and so does a paragraph of its own.
    doc.cursor_at(2).insert_djot("XY").unwrap();
    assert_eq!(doc.to_djot().unwrap(), "```\ncoXYde\n```\n\nPlain[^a].");

    let doc = load(text);
    let code = text_document::BlockFormat {
        is_code_block: Some(true),
        ..Default::default()
    };
    assert!(
        select(&doc, position_of(&doc, "Plain"), length(&doc))
            .set_block_format(&code)
            .is_err()
    );
    assert_eq!(doc.to_djot().unwrap(), original);

    // A `<pre>` holding an image is read as a paragraph, the image kept.
    let doc = TextDocument::new();
    doc.set_html("<pre>code <img src=\"p.png\" alt=\"pic\"></pre>")
        .unwrap()
        .wait()
        .unwrap();
    assert_model(&doc, "the HTML load");
    assert!(
        doc.to_djot().unwrap().contains("![pic](p.png)"),
        "{:?}",
        doc.to_djot()
    );
}

/// A `<pre>` holding an image is read as paragraphs, and a paragraph holds no line break: its
/// lines were kept in one paragraph, which the editor showed on several lines and the save
/// wrote as one, so they ran together at the next reopening. Each line is a paragraph of
/// its own, loaded or pasted, an empty line an empty paragraph, as a preformatted paragraph
/// is split (see `split_block_at_line_breaks`), and the saved text reads back the same, but
/// for the empty paragraph, which a reload keeps no block for.
#[test]
fn a_pre_holding_an_image_keeps_its_lines() {
    let html = "<pre>line one\nline two <img src=\"p.png\" alt=\"pic\"> tail\n\nline three\n</pre>\
                <p>After.</p>";
    let lines = [
        "line one",
        "line two \u{FFFC} tail",
        "",
        "line three",
        "After.",
    ];
    let doc = TextDocument::new();
    doc.set_html(html).unwrap().wait().unwrap();
    assert_model(&doc, "the HTML load");
    assert_eq!(doc.to_addressable_text().unwrap(), lines.join("\n"));
    let reloaded = load(&doc.to_djot().unwrap());
    let written: Vec<&str> = lines.iter().copied().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        reloaded.to_addressable_text().unwrap(),
        written.join("\n"),
        "the saved text reads back line for line"
    );

    let doc = load("Before.\n");
    doc.cursor_at("Before.".len()).insert_html(html).unwrap();
    assert_model(&doc, "the HTML paste");
    assert_eq!(
        doc.to_addressable_text().unwrap(),
        format!("Before.{}", lines.join("\n"))
    );
}

/// A passage pasted from a web page as a `<pre>`, a poem set line by line, went in as a code
/// block once fragments carried code blocks for the sake of a version put back: a code block
/// keeps no emphasis at the next save and takes no footnote reference, and a host with no
/// command to make it prose again left the writer with the passage set as code. Pasted as
/// HTML, a `<pre>` goes in as paragraphs, one for each line, an empty line an empty paragraph
/// and each line keeping its spaces; loaded as HTML, and pasted as Djot or Markdown, it stays
/// a code block.
#[test]
fn a_preformatted_passage_pasted_as_html_goes_in_as_its_lines() {
    let verses: String = (0..4).map(|i| format!("verse {i} of the poem\n")).collect();
    let html = format!("<pre>{verses}\n  the last verse</pre>");
    let lines: Vec<String> = (0..4)
        .map(|i| format!("verse {i} of the poem"))
        .chain(["".to_string(), "  the last verse".to_string()])
        .collect();

    let doc = TextDocument::new();
    doc.cursor_at(0).insert_html(&html).unwrap();
    assert_model(&doc, "the passage pasted");
    assert_eq!(doc.to_addressable_text().unwrap(), lines.join("\n"));
    let cursor = doc.cursor_at(0);
    cursor.move_position(MoveOperation::End, MoveMode::KeepAnchor, 1);
    cursor
        .merge_char_format(&TextFormat {
            font_italic: Some(true),
            ..Default::default()
        })
        .unwrap();
    // Emphasis cannot open on a space, so the writer puts the kept spaces in front of it,
    // behind the `{}` that keeps them.
    let emphasised: Vec<String> = lines
        .iter()
        .map(|line| match line.strip_prefix("  ") {
            Some(rest) => format!("{{}}  _{rest}_"),
            None if line.is_empty() => String::new(),
            None => format!("_{line}_"),
        })
        .collect();
    assert_eq!(
        doc.to_djot().unwrap(),
        emphasised.join("\n\n"),
        "the emphasis is saved"
    );
    doc.cursor_at("verse 0".len())
        .insert_footnote_reference("n1")
        .unwrap();
    let (references, reloaded) = references_and_reloaded(&doc);
    assert_eq!(references, ["n1"]);
    assert_eq!(reloaded, ["n1"], "the reference is saved");

    // Into prose, the lines go in as pasted paragraphs do, and into a quotation too.
    let doc = load("She read aloud: and then stopped.\n");
    doc.cursor_at("She read aloud: ".len())
        .insert_html("<pre>Two roads diverged\nin a yellow wood</pre>")
        .unwrap();
    assert_model(&doc, "the passage pasted into prose");
    assert_eq!(
        doc.to_djot().unwrap(),
        "She read aloud: Two roads diverged\n\nin a yellow woodand then stopped."
    );
    let doc = load("Before.\n");
    doc.cursor_at("Before.".len())
        .insert_html("<blockquote><pre>one\ntwo</pre></blockquote>")
        .unwrap();
    assert_model(&doc, "the passage pasted from a quotation");
    assert_eq!(doc.to_djot().unwrap(), "Before.\n\n> one\n>\n> two");

    // Loaded as HTML, and pasted as Djot or Markdown, it stays a code block.
    let doc = TextDocument::new();
    doc.set_html("<pre>line one\nline two</pre>")
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(doc.to_djot().unwrap(), "```\nline one\nline two\n```");
    for (syntax, text) in [
        ("djot", "```\nline one\nline two\n```\n"),
        ("markdown", "```\nline one\nline two\n```\n"),
    ] {
        let doc = TextDocument::new();
        paste(&doc.cursor(), syntax, text);
        assert_eq!(
            doc.to_djot().unwrap(),
            "```\nline one\nline two\n```",
            "{syntax} pasted"
        );
    }
}

/// A pipe table writes a cell's paragraphs as one line of inline text, whatever their
/// format. A code block pasted into a cell stayed one in the editor, lost at the next save,
/// and refused an image the save would have kept; making a cell's paragraph a code block did
/// the same. A cell's paragraph is never a code block: the paste goes in as a paragraph, and
/// the other formats asked for alongside the code block still apply.
#[test]
fn a_table_cell_holds_no_code_block() {
    let doc = load("| a | b |\n");
    doc.cursor_at(position_of(&doc, "a"))
        .insert_djot("```rust\ncode\n```\n\nnext")
        .unwrap();
    assert_model(&doc, "the paste into a cell");
    assert!(code_block_in_a_cell(&doc).is_none(), "{:?}", doc.to_djot());
    assert!(
        doc.cursor_at(position_of(&doc, "code"))
            .insert_image("p.png", "pic", 4, 4)
            .is_ok()
    );
    assert_model(&doc, "the image in the cell");
    assert_eq!(
        load(&doc.to_djot().unwrap()).to_djot().unwrap(),
        doc.to_djot().unwrap()
    );

    let doc = load("Before.\n\n| a | b |\n\nAfter.\n");
    let format = text_document::BlockFormat {
        is_code_block: Some(true),
        alignment: Some(text_document::Alignment::Center),
        ..Default::default()
    };
    select(&doc, 0, length(&doc))
        .set_block_format(&format)
        .unwrap();
    assert_model(&doc, "a code block over the paragraphs and the cells");
    assert!(code_block_in_a_cell(&doc).is_none());
    let cell = doc.block_format_at(position_of(&doc, "a")).unwrap();
    assert_eq!(cell.alignment, Some(text_document::Alignment::Center));
    assert_eq!(
        doc.block_format_at(0).unwrap().is_code_block,
        Some(true),
        "the paragraph outside the table is one"
    );
}

// ── Tables ───────────────────────────────────────────────────────────────────

/// A pasted table's cells went into the rope at the end of the enclosing frame, after
/// everything that followed the paste: the cells and the paragraphs after the table were
/// out of flow order, and every position past the paste pointed at the wrong text.
#[test]
fn pasting_a_table_mid_document_keeps_the_rope_in_flow_order() {
    let doc = load("one\n\ntwo\n\nthree\n");
    doc.cursor_at(3).insert_djot("a\n\n| x | y |\n\nb").unwrap();
    assert_model(&doc, "the paste");
    let positions: Vec<(String, usize)> = doc
        .snapshot_flow()
        .elements
        .iter()
        .flat_map(|element| match element {
            FlowElementSnapshot::Block(block) => vec![(block.text.clone(), block.position)],
            FlowElementSnapshot::Table(table) => table
                .cells
                .iter()
                .flat_map(|cell| cell.blocks.iter())
                .map(|block| (block.text.clone(), block.position))
                .collect(),
            FlowElementSnapshot::Frame(_) => Vec::new(),
        })
        .collect();
    assert_eq!(
        positions,
        vec![
            ("onea".to_string(), 0),
            ("x".to_string(), 7),
            ("y".to_string(), 9),
            ("b".to_string(), 11),
            ("two".to_string(), 13),
            ("three".to_string(), 17),
        ]
    );
    doc.cursor_at(13).insert_text("TWO ").unwrap();
    assert!(doc.to_plain_text().unwrap().contains("TWO two"));
}

/// A deletion crossing a table removed the paragraphs and the table it covered from the
/// entities only: their text stayed in the rope, under index entries naming blocks that
/// were gone, and search still found the deleted words.
#[test]
fn deleting_across_a_table_takes_the_deleted_text_out_of_the_rope() {
    let doc = load("Keep.\n\nGone one.\n\n| a | b |\n\nGone two.\n\nKept.\n");
    let from = position_of(&doc, "Gone one");
    let to = position_of(&doc, "Kept");
    select(&doc, from, to).remove_selected_text().unwrap();
    assert_model(&doc, "the deletion");
    assert!(
        !doc.to_addressable_text().unwrap().contains("Gone"),
        "{:?}",
        doc.to_addressable_text()
    );
}

/// A paragraph break in a table cell looked for the cell's owner among the main frame's
/// quotations only, found none, and appended the new paragraph to the end of the document.
#[test]
fn a_paragraph_break_in_a_table_cell_stays_in_the_cell() {
    let doc = load("Before.\n\n| ab | c |\n\nAfter.\n");
    let at = position_of(&doc, "ab") + 1;
    doc.cursor_at(at).insert_block().unwrap();
    assert_model(&doc, "the break");
    assert_eq!(doc.to_plain_text().unwrap(), "Before.\na\nb\nc\nAfter.");
}

/// A position on a table's anchor, or on the boundary after it, belongs to no block. It
/// was resolved against stored positions, whose space leaves the anchor out, so the
/// boundary after the anchor stood one character into the first cell; resolved against the
/// rope's, it fell back to the document's last block. A caret there stands at the start of
/// the table's first cell.
#[test]
fn edits_at_a_table_anchor_land_in_its_first_cell() {
    let text = "Before.\n\n| a | b |\n\nAfter.\n";
    let anchor = load(text)
        .to_addressable_text()
        .unwrap()
        .chars()
        .position(|c| c == SENTINEL)
        .unwrap();
    for at in [anchor, anchor + 1] {
        let doc = load(text);
        doc.cursor_at(at).insert_text("X").unwrap();
        assert_model(&doc, &format!("typing at {at}"));
        assert_eq!(
            doc.to_addressable_text().unwrap(),
            "Before.\n\u{FFFC}\nXa\nb\nAfter."
        );

        let doc = load(text);
        doc.cursor_at(at).insert_djot("pasted").unwrap();
        assert_model(&doc, &format!("pasting at {at}"));
        assert_eq!(
            doc.to_addressable_text().unwrap(),
            "Before.\n\u{FFFC}\npasteda\nb\nAfter."
        );
    }
}

/// A caret on a table's anchor, or on the boundary after it, is read as every edit reads it,
/// at the start of the table's first cell, by what a cursor reports of it (its block, its
/// list, its cell) as by what an edit changes there. The lookup of the caret's block walked
/// the blocks without the anchor's two positions: past the anchor it answered the second
/// paragraph of the first cell, so a list item made there read back as out of its list,
/// and moving it a level in failed ("cursor is not inside a list").
#[test]
fn a_caret_on_a_table_anchor_reads_the_first_cell() {
    let doc = load("Before.\n\n| a | b |\n\nAfter.\n");
    let anchor = position_of(&doc, "\u{FFFC}");
    let first_cell = position_of(&doc, "a\nb");
    // A second paragraph in the first cell, a list item.
    doc.cursor_at(first_cell + 1)
        .insert_list(text_document::ListStyle::Decimal)
        .unwrap();
    assert_model(&doc, "the list item in the cell");
    let item = doc.cursor_at(first_cell + 2);
    assert!(item.current_list().is_some(), "{:?}", doc.to_djot());
    let first_block = doc.block_at_caret(first_cell).unwrap();
    for at in [anchor, anchor + 1] {
        let cursor = doc.cursor_at(at);
        assert_eq!(
            doc.block_at_caret(at).unwrap(),
            first_block,
            "the caret at {at} on the anchor"
        );
        assert!(cursor.current_list().is_none(), "the caret at {at}");
        let cell = cursor
            .current_table_cell()
            .expect("the caret stands in a cell");
        assert_eq!((cell.row, cell.column), (0, 0), "the caret at {at}");
        // Making a list there makes one of the first paragraph, which then reads as listed.
        let doc = load(&doc.to_djot().unwrap());
        let cursor = doc.cursor_at(at);
        cursor.create_list(text_document::ListStyle::Disc).unwrap();
        assert!(cursor.current_list().is_some(), "the list made at {at}");
        cursor
            .set_current_list_format(&text_document::ListFormat {
                indent: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_model(&doc, &format!("the list made at {at}"));
    }
}

/// Backspace or Delete across the boundary between a table and the text around it joins
/// nothing: a cell cannot take a paragraph in, nor leave its table. Backspace at the start
/// of the paragraph after a table panicked in `delete_text` (its end resolved before its
/// start); at the start of the first cell it emptied both cells and took a letter of the
/// paragraph after the table; Delete at the end of the paragraph before a table emptied the
/// first cell.
#[test]
fn backspace_and_delete_next_to_a_table_leave_it_whole() {
    let text = "Before.\n\n| a | b |\n\nAfter.\n";
    let unchanged = "Before.\n\u{FFFC}\na\nb\nAfter.";
    let doc = load(text);
    doc.cursor_at(position_of(&doc, "After."))
        .delete_previous_char()
        .unwrap();
    assert_model(
        &doc,
        "Backspace at the start of the paragraph after the table",
    );
    assert_eq!(doc.to_addressable_text().unwrap(), unchanged);

    let doc = load(text);
    doc.cursor_at(position_of(&doc, "a\nb"))
        .delete_previous_char()
        .unwrap();
    assert_model(&doc, "Backspace at the start of the first cell");
    assert_eq!(doc.to_addressable_text().unwrap(), unchanged);

    let doc = load(text);
    doc.cursor_at("Before.".len()).delete_char().unwrap();
    assert_model(&doc, "Delete at the end of the paragraph before the table");
    assert_eq!(doc.to_addressable_text().unwrap(), unchanged);
}

/// The exports put a table cell's paragraphs in the order of their stored positions,
/// which a deletion earlier in the text leaves ahead of the rope: after a paragraph break
/// in a cell, the saved Djot wrote the cell's two paragraphs the other way round.
#[test]
fn a_table_cell_keeps_its_paragraph_order_in_the_saved_text() {
    let doc = load("A long paragraph to delete from.\n\n| t | uvw |\n\nAfter.\n");
    select(&doc, 0, 20).remove_selected_text().unwrap();
    doc.cursor_at(position_of(&doc, "uvw") + 1)
        .insert_block()
        .unwrap();
    assert_model(&doc, "the break in the cell");
    assert!(
        doc.to_djot().unwrap().contains("| t | u vw |"),
        "{:?}",
        doc.to_djot()
    );
}

/// Inserting a row or a column put the new cells at the end of the table's enclosing frame,
/// after the text that follows the table: out of flow order, as the pasted tables were.
#[test]
fn inserting_a_row_or_a_column_keeps_the_rope_in_flow_order() {
    let text = "one\n\n| a | b |\n| c | d |\n\ntwo\n";
    for (what, edit) in [
        (
            "a row below",
            (|cursor: &text_document::TextCursor| cursor.insert_row_below())
                as fn(&text_document::TextCursor) -> text_document::Result<()>,
        ),
        ("a row above", |cursor| cursor.insert_row_above()),
        ("a column after", |cursor| cursor.insert_column_after()),
        ("a column before", |cursor| cursor.insert_column_before()),
    ] {
        let doc = load(text);
        edit(&doc.cursor_at(position_of(&doc, "a"))).unwrap();
        assert_model(&doc, &format!("inserting {what}"));
        doc.undo().unwrap();
        assert_model(&doc, &format!("undoing {what}"));
    }
}

/// A table alone, pasted with the caret in a table cell, fills the cells from there on when
/// it fits. That wrote the pasted runs over the cells' old text and never put the pasted text
/// in the rope: the words were lost, and the next save sliced a cell past its end and
/// panicked.
#[test]
fn pasting_a_table_into_a_table_cell_keeps_the_pasted_text_in_the_save() {
    let doc = load("Start.\n\n| abc | d |\n\nEnd.\n");
    let cursor = doc.cursor_at(position_of(&doc, "bc"));
    cursor.insert_djot("| xyz *bold* | w |\n").unwrap();
    assert_model(&doc, "the paste into the cell");
    assert_eq!(
        doc.to_djot().unwrap(),
        "Start.\n\n| xyz *bold* | w |\n|---|---|\n\nEnd."
    );
    assert_eq!(
        cursor.position(),
        position_of(&doc, "w") + 1,
        "after the text"
    );

    // On a table's anchor, or on the boundary after it, the caret stands in the first cell.
    let text = "Before[^a].\n\n| a | b |\n| c | d |\n";
    let anchor = position_of(&load(text), ".\n\u{FFFC}") + 2;
    for at in [anchor, anchor + 1] {
        let doc = load(text);
        doc.cursor_at(at).insert_djot("| x | y |\n").unwrap();
        assert_model(&doc, &format!("the paste at {at}"));
        assert_eq!(
            doc.to_djot().unwrap(),
            "Before[^a].\n\n| x | y |\n|---|---|\n| c | d |"
        );
    }
}

/// Text holding a table, pasted into a table cell, went in as a table nested in the cell. The
/// saved Djot writes a cell's own paragraphs only, so the pasted table's words showed in the
/// editor and were gone after a reload. They now go in as paragraphs of the cell.
#[test]
fn pasting_a_table_into_a_cell_keeps_every_word_in_the_save() {
    let doc = load("Start.\n\n| abc | d |\n\nEnd.\n");
    doc.cursor_at(position_of(&doc, "bc"))
        .insert_djot("x\n\n| p | q |\n\ny")
        .unwrap();
    assert_model(&doc, "the paste");
    assert_eq!(
        doc.to_addressable_text().unwrap(),
        "Start.\n\u{FFFC}\nax\np\nq\nybc\nd\nEnd."
    );

    // A copied table pasted where it does not fit.
    let doc = load("Intro.\n\n| a1 | b1 |\n| a2 | b2 |\n\nOutro.\n");
    let table = doc
        .cursor_at(position_of(&doc, "a1"))
        .current_table()
        .unwrap()
        .id();
    let cursor = doc.cursor();
    cursor.select_cell_range(table, 0, 0, 1, 1);
    let copied = cursor.selection();
    doc.cursor_at(position_of(&doc, "b2"))
        .insert_fragment(&copied)
        .unwrap();
    assert_model(&doc, "the paste of the copied table");
    let saved = load(&doc.to_djot().unwrap()).to_plain_text().unwrap();
    for word in ["a1", "b1", "a2"] {
        assert_eq!(saved.matches(word).count(), 2, "{word} in {saved:?}");
    }
}

/// A cell of several paragraphs, copied and pasted as a table, listed only its first
/// paragraph in its frame's order: every walk of the frames missed the others.
#[test]
fn a_pasted_cell_of_several_paragraphs_keeps_them_all() {
    let source = load("Start.\n\n| ab | c |\n\nEnd.\n");
    source
        .cursor_at(position_of(&source, "b\nc"))
        .insert_block()
        .unwrap();
    let everything = select(&source, 0, length(&source)).selection();
    let table = source
        .cursor_at(position_of(&source, "c\nEnd"))
        .current_table()
        .unwrap()
        .id();
    let cursor = source.cursor();
    cursor.select_cell_range(table, 0, 0, 0, 1);
    let cells = cursor.selection();

    let doc = load("One.\n\nTwo.\n");
    doc.cursor_at(2).insert_fragment(&everything).unwrap();
    assert_model(&doc, "the paste of text holding the table");

    let doc = load("One.\n\nTwo.\n");
    doc.cursor_at(2).insert_fragment(&cells).unwrap();
    assert_model(&doc, "the paste of the cells alone");
    assert_eq!(
        doc.to_addressable_text().unwrap(),
        "One.\n\u{FFFC}\na\nb\nc\nTwo."
    );

    let doc = load("One.\n\n| x | y |\n\nTwo.\n");
    doc.cursor_at(position_of(&doc, "x"))
        .insert_fragment(&cells)
        .unwrap();
    assert_model(&doc, "the paste over the cells of a table");
    assert_eq!(
        doc.to_djot().unwrap(),
        "One.\n\n| a b | c |\n|---|---|\n\nTwo."
    );
}

/// Backspace next to a table joins nothing and removes nothing, yet the cursor reported one
/// character removed: every other cursor on the document moved back by one, so the next
/// keystroke there landed a character early, and listeners were told of a change that never
/// happened.
#[test]
fn backspace_next_to_a_table_moves_no_other_cursor() {
    let doc = load("Before.\n\n| a | b |\n\nAfter.\n");
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen_events = events.clone();
    let _subscription = doc.on_change(move |event| {
        if let Ok(mut list) = seen_events.lock() {
            list.push(format!("{event:?}"));
        }
    });
    let after = position_of(&doc, "After.");
    let other = doc.cursor_at(after + 4);
    doc.cursor_at(after).delete_previous_char().unwrap();
    assert_eq!(other.position(), after + 4, "the other cursor stays");
    let announced = events.lock().unwrap().clone();
    assert!(
        !announced.iter().any(|e| e.starts_with("ContentsChanged")),
        "{announced:?}"
    );
    other.insert_text("X").unwrap();
    assert_eq!(
        doc.to_addressable_text().unwrap(),
        "Before.\n\u{FFFC}\na\nb\nAfteXr."
    );

    // Delete at the end of the paragraph before the table, likewise.
    let doc = load("Before.\n\n| a | b |\n\nAfter.\n");
    let other = doc.cursor_at(position_of(&doc, "After."));
    let at = other.position();
    doc.cursor_at("Before.".len()).delete_char().unwrap();
    assert_eq!(other.position(), at);
}

/// A paste of one line at a table's anchor goes into the first cell, but its caret was
/// counted from the anchor: the next paste went in front of this one.
#[test]
fn two_pastes_at_a_table_anchor_land_in_order() {
    let text = "Before.\n\n| a | b |\n\nAfter.\n";
    let anchor = position_of(&load(text), "\u{FFFC}");
    for at in [anchor, anchor + 1] {
        let doc = load(text);
        let cursor = doc.cursor_at(at);
        cursor.insert_djot("p").unwrap();
        cursor.insert_djot("q").unwrap();
        assert_eq!(
            doc.to_addressable_text().unwrap(),
            "Before.\n\u{FFFC}\npqa\nb\nAfter.",
            "pasting at {at}"
        );
        assert_caret_after(&doc, &cursor, "pq");
    }
}

/// Format, Insert table, with the caret in a cell puts the new table after the table holding
/// the cell. That table was found through the cell frame's parent, which a pasted table's
/// cells do not carry, so the new table nested in the cell, where the save dropped it; and
/// the new table went into the rope after the caret's cell rather than after the table, out
/// of flow order whenever that cell was not the table's last.
#[test]
fn inserting_a_table_from_a_cell_puts_it_after_that_table() {
    let expected = "Before.\n\u{FFFC}\nc1\nc2\n\u{FFFC}\n\nEnd.";

    // A pasted table: its cell frames have no parent.
    let doc = load("Before.\n\nEnd.\n");
    doc.cursor_at("Before.".len())
        .insert_html("<table><tr><td>c1</td><td>c2</td></tr></table>")
        .unwrap();
    doc.cursor_at(position_of(&doc, "c1"))
        .insert_table(1, 1)
        .unwrap();
    assert_model(&doc, "a table inserted from a pasted table's cell");
    assert_eq!(doc.to_addressable_text().unwrap(), expected);

    // A loaded table, from its first cell and from its anchor.
    let text = "Before.\n\n| c1 | c2 |\n\nEnd.\n";
    for at in [
        position_of(&load(text), "c1"),
        position_of(&load(text), "\u{FFFC}"),
    ] {
        let doc = load(text);
        doc.cursor_at(at).insert_table(1, 1).unwrap();
        assert_model(&doc, &format!("a table inserted at {at}"));
        assert_eq!(doc.to_addressable_text().unwrap(), expected);
    }
}

/// A table pasted alone put its cells in the rope at the end of the enclosing frame, after the
/// text that followed the paste, as the tables of mixed pastes did.
#[test]
fn pasting_a_table_alone_mid_document_keeps_the_rope_in_flow_order() {
    for (syntax, table) in [
        ("djot", "| x | y |\n| z | w |\n"),
        (
            "html",
            "<table><tr><td>x</td><td>y</td></tr><tr><td>z</td><td>w</td></tr></table>",
        ),
        ("markdown", "| x | y |\n|---|---|\n| z | w |\n"),
    ] {
        let doc = load("one\n\ntwo\n\nthree\n");
        paste(&doc.cursor_at(3), syntax, table);
        assert_model(&doc, &format!("the {syntax} paste"));
        assert_eq!(
            doc.to_addressable_text().unwrap(),
            "one\n\u{FFFC}\nx\ny\nz\nw\ntwo\nthree",
            "{syntax}"
        );

        let doc = load("Before.\n\n> Quoted one.\n>\n> Quoted two.\n\nAfter.\n");
        paste(&doc.cursor_at(position_of(&doc, " one.")), syntax, table);
        assert_model(&doc, &format!("the {syntax} paste into a quotation"));
        assert_eq!(
            doc.to_addressable_text().unwrap(),
            "Before.\nQuoted one.\n\u{FFFC}\nx\ny\nz\nw\nQuoted two.\nAfter.",
            "{syntax}"
        );
    }
}

/// A text opening with a table, pasted over everything, put back as a version, cut and
/// pasted back, or pasted into an empty document, reads as the text loaded: the table first.
/// The paste split the empty paragraph at the caret and put the table after its empty first
/// half, so the live text opened with an empty paragraph no save keeps, and a caret or a
/// search counted a line the text does not have.
#[test]
fn a_text_opening_with_a_table_replaces_everything_with_nothing_before_the_table() {
    for text in [
        "| a | b |\n| c | d |\n\nAfter.\n",
        "| a | b |\n",
        "| a |\n\n| b |\n\nEnd.\n",
        "> | q1 | q2 |\n\nAfter.\n",
        "| a | b |\n\n- one\n- two\n",
    ] {
        let source = load(text);
        let expected = source.to_addressable_text().unwrap();
        let expected_djot = source.to_djot().unwrap();
        let whole = select(&source, 0, length(&source)).selection();
        let check = |doc: &TextDocument, what: &str| {
            assert_model(doc, &format!("{what} of {text:?}"));
            assert_eq!(
                doc.to_addressable_text().unwrap(),
                expected,
                "{what} of {text:?}"
            );
            assert_eq!(doc.to_djot().unwrap(), expected_djot, "{what} of {text:?}");
        };

        let doc = load("Some.\n\nText.\n");
        let cursor = doc.cursor();
        cursor.select(SelectionType::Document);
        cursor.insert_fragment(&whole).unwrap();
        check(&doc, "select all and paste");

        let doc = load("# A heading\n\nText.\n");
        restore(&doc, text);
        check(&doc, "a version put back");

        let doc = load(text);
        let cursor = doc.cursor();
        cursor.select(SelectionType::Document);
        let cut = cursor.selection();
        cursor.remove_selected_text().unwrap();
        cursor.insert_fragment(&cut).unwrap();
        check(&doc, "cut everything and paste it back");

        let doc = TextDocument::new();
        doc.cursor().insert_fragment(&whole).unwrap();
        check(&doc, "a paste into an empty document");
    }
}

/// A table pasted with the caret at the start of a paragraph goes in front of the paragraph,
/// which keeps its text and format; an empty paragraph stays after it. It went in after the
/// paragraph wherever the caret stood in it.
#[test]
fn a_table_pasted_at_the_start_of_a_paragraph_goes_in_front_of_it() {
    for (fragment, expected) in [
        ("| x | y |\n", "one\n\u{FFFC}\nx\ny\ntwo\nthree"),
        ("| x | y |\n\nmore", "one\n\u{FFFC}\nx\ny\nmoretwo\nthree"),
    ] {
        let doc = load("one\n\n## two\n\nthree\n");
        paste(&doc.cursor_at(position_of(&doc, "two")), "djot", fragment);
        assert_model(&doc, &format!("{fragment:?} pasted"));
        assert_eq!(doc.to_addressable_text().unwrap(), expected, "{fragment:?}");
        assert!(
            doc.to_djot().unwrap().contains("## "),
            "{fragment:?}: the heading keeps its level: {:?}",
            doc.to_djot().unwrap()
        );
    }

    // An empty line stays after a table pasted on it, as in a word processor.
    let doc = load("one\n\nthree\n");
    doc.cursor_at(3).insert_block().unwrap();
    assert_eq!(doc.to_addressable_text().unwrap(), "one\n\nthree");
    paste(&doc.cursor_at(4), "djot", "| x | y |\n");
    assert_model(&doc, "a table pasted into an empty paragraph");
    assert_eq!(
        doc.to_addressable_text().unwrap(),
        "one\n\u{FFFC}\nx\ny\n\nthree"
    );
    doc.undo().unwrap();
    assert_eq!(doc.to_addressable_text().unwrap(), "one\n\nthree");
}

/// A selection over a whole table of one cell, from the end of the paragraph before it to
/// the start of the paragraph after it, takes text from that one cell only. It went through
/// the join, which removed the cell's blocks and left the table itself behind: an anchor in
/// the frames with nothing in the rope, which the next save wrote as an empty table.
#[test]
fn deleting_a_selection_over_a_table_of_one_cell_removes_the_table() {
    let doc = load("Start.\n\n| abf |\n\nEnd.\n");
    let to = position_of(&doc, "End.");
    select(&doc, "Start.".len(), to)
        .remove_selected_text()
        .unwrap();
    assert_model(&doc, "the deletion");
    assert_eq!(doc.to_djot().unwrap(), "Start.\n\nEnd.");
}

/// A selection from a table's anchor to the end of its last cell holds the whole table, as
/// one from the paragraph before it does. Its start moved into the first cell, so over a table
/// of one cell it read as a range inside that cell: select all over a text that is one such
/// table emptied the cell and kept the table, and a text put back over everything went into
/// the cell.
#[test]
fn selecting_all_of_a_text_that_is_a_table_of_one_cell_removes_the_table() {
    let doc = load("| solo |\n");
    let cursor = doc.cursor();
    cursor.select(SelectionType::Document);
    cursor.remove_selected_text().unwrap();
    assert_model(&doc, "the deletion");
    assert_eq!(doc.to_addressable_text().unwrap(), "");

    let doc = load("| solo |\n");
    restore(&doc, "See this[^f1].");
    assert_model(&doc, "the restore");
    assert_eq!(doc.to_djot().unwrap(), "See this[^f1].");

    // The cell empty: the range's start, moved past the anchor, meets its end.
    let doc = load("|  |\n");
    restore(&doc, "> quoted");
    assert_model(&doc, "the restore over an empty cell");
    assert_eq!(doc.to_djot().unwrap(), "> quoted");
}

/// Removing a row, a column or a table removes its cells' blocks through the block store,
/// which kept their footnote references: readers walking the references still counted them,
/// and reported and numbered notes whose text was gone.
#[test]
fn removing_a_row_drops_the_footnote_references_of_its_cells() {
    let doc = load("Before[^a].\n\n| c1[^t] | c2 |\n| d1 | d2 |\n\nAfter.\n");
    doc.cursor_at(position_of(&doc, "c1"))
        .remove_current_row()
        .unwrap();
    assert_model(&doc, "removing the row");
    let labels: Vec<String> = doc
        .footnote_references()
        .into_iter()
        .map(|(_, label)| label)
        .collect();
    assert_eq!(labels, vec!["a".to_string()]);
}

/// Notes are numbered in reading order. A paragraph break writes the new paragraph's position
/// from the rope, while the paragraphs after it keep the stored position typing left behind:
/// numbering by the stored field put the later note first.
#[test]
fn notes_are_numbered_in_reading_order_after_typing() {
    let doc = load("Hello[^x] world.\n\nNext[^y].\n\nLast[^z].\n");
    doc.cursor_at(0).insert_text(&"X".repeat(30)).unwrap();
    doc.cursor_at(35).insert_block().unwrap();
    let html = doc.to_html().unwrap();
    for (label, number) in [("x", 1), ("y", 2), ("z", 3)] {
        assert!(
            html.contains(&format!("id=\"fnref-{label}\"><sup>{number}</sup>")),
            "{label} is not note {number}: {html}"
        );
    }
}

/// A frame whose `child_order` is empty has its paragraphs written in the order of their
/// positions. No loader builds such a frame today, so the test empties one through the store.
/// The writer sorted by the stored field, which typing leaves behind, and wrote the two
/// halves of a broken paragraph around the paragraph after them.
#[test]
fn a_frame_without_an_order_keeps_its_paragraph_order_in_the_saved_text() {
    let doc = load("One.\n\nTwobar.\n\nThree.\n");
    doc.cursor_at(0).insert_text(&"X".repeat(30)).unwrap();
    doc.cursor_at(position_of(&doc, "bar."))
        .insert_block()
        .unwrap();
    let store = doc.rope_store_for_test();
    let main = store.documents.read().values().next().unwrap().frames[0];
    {
        let mut frames = store.frames.write();
        let mut frame = frames.get(&main).cloned().unwrap();
        frame.child_order.clear();
        frames.insert(main, frame);
    }
    assert_eq!(
        doc.to_djot().unwrap(),
        format!("{}One.\n\nTwo\n\nbar.\n\nThree.", "X".repeat(30))
    );
}

// ── Footnote bodies ──────────────────────────────────────────────────────────

/// No view shows a footnote's body, and the rope used to hold it where the definition was
/// written, between the paragraphs around it. Backspace at the start of the paragraph after a
/// body, and Delete at the end of the paragraph before one, joined nothing there, so a writer
/// could not join the two paragraphs they saw; the Right arrow from the end of the first went
/// into the hidden body, and typing there edited the note unseen. A load now puts every body
/// after the main text: the two paragraphs join as any two do, and the note keeps its body.
#[test]
fn a_note_body_written_between_paragraphs_stays_out_of_their_way() {
    let text = "One[^a].\n\n[^a]: Note text.\n\nTwo.\n";
    let doc = load(text);
    assert_eq!(
        doc.to_addressable_text().unwrap(),
        "One\u{FFFC}.\nTwo.\nNote text."
    );
    assert_model(&doc, "the load");

    let doc = load(text);
    doc.cursor_at(position_of(&doc, "Two"))
        .delete_previous_char()
        .unwrap();
    assert_model(&doc, "Backspace at the start of the second paragraph");
    assert_eq!(doc.to_djot().unwrap(), "One[^a].Two.\n\n[^a]: Note text.");

    let doc = load(text);
    doc.cursor_at(position_of(&doc, ".\nTwo") + 1)
        .delete_char()
        .unwrap();
    assert_model(&doc, "Delete at the end of the first paragraph");
    assert_eq!(doc.to_djot().unwrap(), "One[^a].Two.\n\n[^a]: Note text.");

    // The Right arrow goes from one paragraph to the other, and stops at the end of the text.
    let doc = load(text);
    let cursor = doc.cursor_at(position_of(&doc, ".\nTwo") + 1);
    cursor.move_position(text_document::MoveOperation::Right, MoveMode::MoveAnchor, 1);
    assert_eq!(cursor.position(), position_of(&doc, "Two"));
    let end_of_text = position_of(&doc, "\nNote");
    let cursor = doc.cursor_at(end_of_text);
    cursor.move_position(text_document::MoveOperation::Right, MoveMode::MoveAnchor, 1);
    assert_eq!(
        cursor.position(),
        end_of_text,
        "the Right arrow went into the note"
    );
    cursor.insert_text("X").unwrap();
    assert_eq!(
        doc.to_djot().unwrap(),
        "One[^a].\n\nTwo.X\n\n[^a]: Note text."
    );
}

/// The last paragraph and a note's body are still two texts, not two paragraphs of one: Delete
/// at the end of the text, and Backspace at the start of the body, join nothing. And the arrows
/// and the jumps to the end of the text stop at its end.
#[test]
fn the_edge_between_the_text_and_the_notes_joins_nothing() {
    let text = "Line one has a note[^n0].\n\n[^n0]: Note zero.\n\nLine two here.\n";
    let original = load(text).to_djot().unwrap();
    let doc = load(text);
    doc.cursor_at(position_of(&doc, ".\nNote") + 1)
        .delete_char()
        .unwrap();
    assert_model(&doc, "Delete before the body");
    assert_eq!(doc.to_djot().unwrap(), original);

    let doc = load(text);
    doc.cursor_at(position_of(&doc, "Note zero"))
        .delete_previous_char()
        .unwrap();
    assert_model(&doc, "Backspace at the start of the body");
    assert_eq!(doc.to_djot().unwrap(), original);

    let doc = load(text);
    let end_of_text = position_of(&doc, ".\nNote") + 1;
    for operation in [
        text_document::MoveOperation::End,
        text_document::MoveOperation::NextCharacter,
        text_document::MoveOperation::NextBlock,
        text_document::MoveOperation::NextWord,
        text_document::MoveOperation::Down,
    ] {
        let cursor = doc.cursor_at(position_of(&doc, "here"));
        cursor.move_position(operation, MoveMode::KeepAnchor, 3);
        assert!(
            cursor.position() <= end_of_text,
            "{operation:?} went into the note, to {}",
            cursor.position()
        );
    }
}

/// A selection runs from where it starts: in the text it copies and deletes text only, in a
/// note's body that body only. Copying or cutting a range that ran across a body carried the
/// body's text as a paragraph, and pasting put the note's text into the prose; cutting took the
/// body away, so select all, cut and paste left every note without its body.
#[test]
fn copying_or_cutting_across_a_note_body_leaves_the_body_where_it_is() {
    let text = "Intro[^a] text.\n\n[^a]: Body a.\n\nMiddle[^b].\n\n[^b]: Body b.\n\nEnd.\n";
    let original = load(text).to_djot().unwrap();

    let doc = load(text);
    let cursor = doc.cursor();
    cursor.select(SelectionType::Document);
    let copied = cursor.selection();
    assert!(
        !copied.to_plain_text().contains("Body"),
        "{:?}",
        copied.to_plain_text()
    );
    cursor.insert_fragment(&copied).unwrap();
    assert_model(&doc, "select all, copy and paste");
    assert_eq!(doc.to_djot().unwrap(), original);

    let doc = load(text);
    let cursor = doc.cursor();
    cursor.select(SelectionType::Document);
    let cut = cursor.selection();
    cursor.remove_selected_text().unwrap();
    assert_model(&doc, "select all and cut");
    assert_eq!(
        note_bodies(&doc).len(),
        2,
        "the cut took the notes' bodies: {:?}",
        doc.to_djot()
    );
    doc.cursor().insert_fragment(&cut).unwrap();
    assert_model(&doc, "the paste after the cut");
    assert_eq!(doc.to_djot().unwrap(), original);

    // From a paragraph to the end, from the text's side: a note referenced before the range
    // keeps its body.
    let doc = load("A[^a] first.\n\nB second.\n\nC third.\n\n[^a]: Body a.\n");
    let cursor = doc.cursor_at(position_of(&doc, "\nC third"));
    cursor.move_position(text_document::MoveOperation::End, MoveMode::KeepAnchor, 1);
    cursor.set_position(length(&doc), MoveMode::KeepAnchor);
    let cut = cursor.selection();
    assert_eq!(cut.to_plain_text(), "\nC third.");
    cursor.remove_selected_text().unwrap();
    assert_model(&doc, "cutting to the end");
    assert_eq!(
        doc.to_djot().unwrap(),
        "A[^a] first.\n\nB second.\n\n[^a]: Body a."
    );

    // A selection inside a body copies from that body.
    let doc = load("One[^a].\n\n[^a]: Note text.\n\nTwo.\n");
    let body = position_of(&doc, "Note text");
    assert_eq!(
        select(&doc, body, body + 4).selection().to_plain_text(),
        "Note"
    );
    assert_eq!(
        select(&doc, 0, length(&doc)).selection().to_plain_text(),
        "One\u{FFFC}.\nTwo."
    );
}

/// A cursor is placed where it is asked, and a selection reads the text it covers, after a
/// table as before one. Both went through a lookup that counted positions without the two
/// characters a table's anchor takes: a caret put inside the last characters of a paragraph
/// after a table snapped back, so typing there landed early, and `selected_text` returned
/// text from further on.
#[test]
fn carets_and_selections_after_a_table_stay_where_they_are_put() {
    let doc = load("Before.\n\n| a | b |\n\nAfter words.\n");
    let end = length(&doc);
    for at in end - 3..=end {
        assert_eq!(doc.cursor_at(at).position(), at, "a cursor made at {at}");
        let cursor = doc.cursor();
        cursor.set_position(at, MoveMode::MoveAnchor);
        assert_eq!(cursor.position(), at, "a cursor moved to {at}");
    }
    let words = position_of(&doc, "words");
    assert_eq!(
        select(&doc, words, words + 5).selected_text().unwrap(),
        "words"
    );
    assert_eq!(doc.text_at(words, 5).unwrap(), "words");
    let cursor = doc.cursor_at(end - 1);
    cursor.insert_text("X").unwrap();
    assert!(doc.to_addressable_text().unwrap().ends_with("wordsX."));
}

/// Backspace at the start of a table cell joins nothing. When the cell before it ended with
/// an empty paragraph, that paragraph counted as touched by the range, and the deletion
/// emptied the whole cell before, text and all.
#[test]
fn backspace_at_the_start_of_a_cell_keeps_the_cell_before() {
    let doc = load("Intro.\n\n| x | y |\n| 1 | 2 |\n");
    doc.cursor_at(position_of(&doc, "y") + 1)
        .insert_block()
        .unwrap();
    let before = doc.to_addressable_text().unwrap();
    assert_eq!(before, "Intro.\n\u{FFFC}\nx\ny\n\n1\n2");
    let one = position_of(&doc, "1");
    let other = doc.cursor_at(one + 1);
    doc.cursor_at(one).delete_previous_char().unwrap();
    assert_model(&doc, "Backspace at the start of the cell");
    assert_eq!(doc.to_addressable_text().unwrap(), before);
    assert_eq!(other.position(), one + 1);

    // A selection from the table's anchor onward still takes an empty first cell.
    let doc = load("|  | b |\n\nAfter.\n");
    assert_eq!(doc.to_addressable_text().unwrap(), "\u{FFFC}\n\nb\nAfter.");
    select(&doc, 0, length(&doc))
        .remove_selected_text()
        .unwrap();
    assert_model(&doc, "deleting everything");
    assert_eq!(doc.to_addressable_text().unwrap(), "");
}

/// A cell whose first paragraph is empty keeps its other paragraphs when Backspace is
/// pressed at its start: the empty paragraph counted as touched, and the deletion emptied the
/// whole cell. A selection holding a whole table whose last cell is empty still removes the
/// table.
#[test]
fn backspace_at_an_empty_first_paragraph_of_a_cell_keeps_the_cell() {
    let doc = load("Intro.\n\n| x | y |\n| 1 | h4 |\n");
    doc.cursor_at(position_of(&doc, "h4"))
        .insert_block()
        .unwrap();
    let before = doc.to_addressable_text().unwrap();
    assert_eq!(before, "Intro.\n\u{FFFC}\nx\ny\n1\n\nh4");
    let empty = position_of(&doc, "\n\nh4") + 1;
    doc.cursor_at(empty).delete_previous_char().unwrap();
    assert_model(&doc, "Backspace at the empty first paragraph of a cell");
    assert_eq!(doc.to_addressable_text().unwrap(), before);

    let doc = load("Intro.\n\n| x |  |\n");
    assert_eq!(doc.to_addressable_text().unwrap(), "Intro.\n\u{FFFC}\nx\n");
    select(&doc, 0, length(&doc))
        .remove_selected_text()
        .unwrap();
    assert_model(&doc, "deleting everything");
    assert_eq!(doc.to_addressable_text().unwrap(), "");
}

/// Backspace at the start of the paragraph after a table whose cells are empty takes nothing:
/// the table's text spans no position, and a rule asking only whether the range held that
/// span removed the table.
#[test]
fn backspace_after_a_table_of_empty_cells_keeps_the_table() {
    let doc = load("|  |\n\ned\n");
    let before = doc.to_addressable_text().unwrap();
    assert_eq!(before, "\u{FFFC}\n\ned");
    let other = doc.cursor_at(4);
    doc.cursor_at(3).delete_previous_char().unwrap();
    assert_model(&doc, "Backspace after the table");
    assert_eq!(doc.to_addressable_text().unwrap(), before);
    assert_eq!(other.position(), 4);
}

// ── Removing a table ─────────────────────────────────────────────────────────

/// A table pasted into an empty paragraph follows that paragraph in the rope, as it does in
/// the frame. Removing it (Format, Remove table) removed its cells first, which left its anchor
/// as the last entry right behind the empty paragraph, then cut the anchor and moved every
/// entry from the start of the cut back by its length: the empty paragraph starts there, so it
/// moved back into the paragraph before it. The next save split that paragraph and lost a
/// letter ("Hello w" and "rld").
#[test]
fn removing_a_table_pasted_into_an_empty_paragraph_keeps_the_paragraph_before_it() {
    for (syntax, table) in [
        ("html", "<table><tr><td>a</td><td>b</td></tr></table>"),
        ("djot", "| only | table |\n"),
    ] {
        let doc = load("Hello world\n");
        doc.cursor_at(11).insert_block().unwrap();
        paste(&doc.cursor_at(12), syntax, table);
        assert_model(&doc, &format!("the {syntax} paste"));
        doc.cursor_at(position_of(&doc, "\u{FFFC}") + 2)
            .remove_current_table()
            .unwrap();
        assert_model(&doc, &format!("removing the {syntax} table"));
        assert_eq!(doc.to_addressable_text().unwrap(), "Hello world\n");
        assert_eq!(doc.to_djot().unwrap().trim_end(), "Hello world");
        doc.cursor_at(0).insert_text("A").unwrap();
        assert_eq!(doc.to_addressable_text().unwrap(), "AHello world\n");
        assert_model(&doc, "typing after the removal");
    }
}

/// A document that is only a table (loaded so, restored so, or an empty document a table was
/// pasted into) has nothing but the table in the rope. Removing it cut four bytes from a rope
/// of three and panicked, or moved the empty paragraph in front of the table below zero. The
/// document now keeps one empty paragraph, where the caret can stand.
#[test]
fn removing_the_table_of_a_document_that_is_only_a_table_leaves_an_empty_paragraph() {
    let loaded = load("| only | table |\n");
    let restored = load("Hello\n");
    restore(&restored, "| only | table |\n");
    let pasted = TextDocument::new();
    pasted
        .cursor_at(0)
        .insert_html("<table><tr><td>a</td><td>b</td></tr></table>")
        .unwrap();
    for (how, doc) in [
        ("loaded", loaded),
        ("restored", restored),
        ("pasted", pasted),
    ] {
        assert_model(&doc, how);
        let before = doc.to_djot().unwrap();
        doc.cursor_at(position_of(&doc, "\u{FFFC}") + 2)
            .remove_current_table()
            .unwrap();
        assert_model(&doc, &format!("removing the {how} table"));
        assert_eq!(doc.to_addressable_text().unwrap(), "", "{how}");
        doc.cursor_at(0).insert_text("x").unwrap();
        assert_eq!(doc.to_djot().unwrap(), "x", "{how}");
        assert_model(&doc, &format!("typing after removing the {how} table"));
        doc.undo().unwrap();
        doc.undo().unwrap();
        assert_eq!(doc.to_djot().unwrap(), before, "{how}: undoing the removal");
        assert_model(&doc, &format!("undoing the removal of the {how} table"));
    }
}

// ── Other cursors ────────────────────────────────────────────────────────────

/// Typing over a selection that crosses a table deletes what the selection covers, which is
/// not the selection's length: a range holding a whole table takes it, and a range into a
/// table empties the cells it touches. The cursor moved every other cursor back by the
/// selection's length all the same, and said so in its event, so the next keystroke in another
/// view of the document landed away from its caret. The same held for replacing a range and for
/// typing formatted text over one.
#[test]
fn typing_over_a_selection_across_a_table_moves_other_cursors_by_what_went() {
    let text = "Before.\n\n| ab | cd |\n\nAfter.\n";
    let at = |needle: &str| position_of(&load(text), needle);
    let ranges = [
        (
            "from the paragraph's end into the first cell",
            at("\n\u{FFFC}"),
            at("b\ncd"),
        ),
        ("from a cell to after the table", at("b\ncd"), at("ter.")),
        ("across two cells", at("b\ncd"), at("d\nAfter")),
        ("over the whole table", at("\n\u{FFFC}"), at("After.")),
        (
            "from the anchor into the second cell",
            at("\u{FFFC}"),
            at("d\nAfter"),
        ),
    ];
    type Edit = fn(&TextDocument, usize, usize);
    let edits: [(&str, Edit); 3] = [
        ("typing", |doc, from, to| {
            select(doc, from, to).insert_text("X").unwrap();
        }),
        ("replacing", |doc, from, to| {
            doc.cursor()
                .replace(from, to, "X", Default::default())
                .unwrap();
        }),
        ("typing bold", |doc, from, to| {
            let bold = TextFormat {
                font_bold: Some(true),
                ..Default::default()
            };
            select(doc, from, to)
                .insert_formatted_text("X", &bold)
                .unwrap();
        }),
    ];
    for (edit_name, edit) in edits {
        for (range_name, from, to) in ranges {
            let doc = load(text);
            let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(usize, usize)>::new()));
            let seen_events = events.clone();
            let _subscription = doc.on_change(move |event| {
                if let text_document::DocumentEvent::ContentsChanged {
                    chars_removed,
                    chars_added,
                    ..
                } = event
                    && let Ok(mut list) = seen_events.lock()
                {
                    list.push((chars_removed, chars_added));
                }
            });
            let before = characters(&doc);
            let other_at = position_of(&doc, "ter.");
            let other = doc.cursor_at(other_at);
            edit(&doc, from, to);
            let what = format!("{edit_name} {range_name} ({from}..{to})");
            assert_model(&doc, &what);
            assert_other_cursor_kept(&doc, &before, other_at, &other, &what);
            let after = characters(&doc);
            let announced = events.lock().unwrap().clone();
            let net: isize = announced
                .iter()
                .map(|(removed, added)| *added as isize - *removed as isize)
                .sum();
            assert_eq!(
                net,
                after.len() as isize - before.len() as isize,
                "{what}: the events announce {announced:?}"
            );
        }
    }
}

/// A table pasted into a paragraph goes in after the paragraph. The cursor counted it from the
/// caret all the same, so a cursor further along that paragraph moved past the table, and one
/// after the paragraph did not move at all when the pasted table was the whole paste, whose
/// caret stays in front of the table. A table pasted into a table's cells replaces what they
/// held from the start of the caret's cell, which was counted as text added at the caret.
#[test]
fn pasting_a_table_into_a_paragraph_moves_other_cursors_by_where_it_went() {
    let doc = load("one two\n\nthree\n");
    let before = characters(&doc);
    let in_the_rest = doc.cursor_at(position_of(&doc, "two"));
    let three = position_of(&doc, "three");
    let after_it = doc.cursor_at(three);
    doc.cursor_at(3).insert_djot("| x | y |\n").unwrap();
    assert_model(&doc, "the paste");
    assert_eq!(
        doc.to_addressable_text().unwrap(),
        "one two\n\u{FFFC}\nx\ny\nthree"
    );
    assert_eq!(in_the_rest.position(), position_of(&doc, "two"));
    assert_other_cursor_kept(&doc, &before, three, &after_it, "the paste");

    // A table pasted into a table cell fills the cells from the start of the caret's cell,
    // which lies before the caret: what it replaced was counted from the caret on.
    let doc = load("Intro.\n\n| abc def | g |\n\nAfter.\n");
    let before = characters(&doc);
    let after = position_of(&doc, "After.");
    let other = doc.cursor_at(after);
    doc.cursor_at(position_of(&doc, "def"))
        .insert_djot("| x | y |\n")
        .unwrap();
    assert_model(&doc, "the paste into the cells");
    assert_eq!(
        doc.to_addressable_text().unwrap(),
        "Intro.\n\u{FFFC}\nx\ny\nAfter."
    );
    assert_other_cursor_kept(&doc, &before, after, &other, "the paste into the cells");
}

// ── Undo ─────────────────────────────────────────────────────────────────────

/// Backspace or Delete next to a table, or between two of its cells, removes nothing, yet it
/// went on the undo stack: the Edit menu offered an Undo that changed nothing, and the writer
/// had to undo twice to take back the last real edit.
#[test]
fn backspace_or_delete_that_removes_nothing_leaves_no_undo_entry() {
    let doc = load("Before.\n\n| ab | cd |\n\nAfter.\n");
    assert!(!doc.can_undo());
    let text = doc.to_addressable_text().unwrap();
    doc.cursor_at(position_of(&doc, "After."))
        .delete_previous_char()
        .unwrap();
    doc.cursor_at("Before.".len()).delete_char().unwrap();
    doc.cursor_at(position_of(&doc, "cd"))
        .delete_previous_char()
        .unwrap();
    doc.cursor_at(position_of(&doc, "ab"))
        .delete_previous_char()
        .unwrap();
    assert_eq!(doc.to_addressable_text().unwrap(), text);
    assert!(
        !doc.can_undo(),
        "an edit that removed nothing is on the undo stack"
    );

    doc.cursor_at(0).insert_text("X").unwrap();
    doc.cursor_at(position_of(&doc, "After."))
        .delete_previous_char()
        .unwrap();
    doc.undo().unwrap();
    assert_eq!(doc.to_addressable_text().unwrap(), text);
    assert!(!doc.can_undo());
}

/// A selection of nothing but a table's anchor copies an empty fragment. Pasting it over a
/// selection deleted the selection, then failed to read the fragment and returned early,
/// leaving the edit's undo group open: the deletion stayed, the cached plain text still showed
/// the old text, and every later edit joined the open group, so none of them could be undone
/// on its own. The paste still fails, and now leaves the document and its history as they were.
#[test]
fn pasting_an_empty_fragment_over_a_selection_changes_nothing() {
    let doc = load("Para.\n\n| x | y |\n\nEnd.\n");
    let anchor = position_of(&doc, "\u{FFFC}");
    let copied = select(&doc, anchor, anchor + 2).selection();
    assert!(copied.is_empty());
    let (djot, plain) = (doc.to_djot().unwrap(), doc.to_plain_text().unwrap());
    for fragment in [copied, text_document::DocumentFragment::new()] {
        assert!(select(&doc, 0, 4).insert_fragment(&fragment).is_err());
    }
    assert_eq!(doc.to_djot().unwrap(), djot);
    assert_eq!(doc.to_plain_text().unwrap(), plain);
    assert!(!doc.can_undo());
    assert_model(&doc, "the empty paste");

    // The history is whole: two edits undo one at a time.
    doc.cursor_at(0).insert_text("A").unwrap();
    doc.cursor_at(length(&doc)).insert_block().unwrap();
    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), format!("A{plain}"));
    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), plain);
}

// ── Footnote bodies, images, formatted text ──────────────────────────────────

/// A footnote's body holds paragraphs: the Djot reader keeps nothing else of a definition. A
/// table pasted into a body was written into the saved text as a table and was gone after the
/// next load, words and all. It goes in as paragraphs, as it does in a table cell.
#[test]
fn a_table_pasted_into_a_note_body_is_kept_by_a_reload() {
    for (syntax, table) in [
        ("djot", "a\n\n| x | y |\n\nb"),
        ("djot", "| x | y |\n"),
        ("html", "<table><tr><td>x</td><td>y</td></tr></table>"),
    ] {
        let doc = load("Noted[^a] here.\n\n[^a]: The note.\n\nLast line.\n");
        paste(&doc.cursor_at(position_of(&doc, "note.")), syntax, table);
        assert_model(&doc, &format!("the {syntax} paste {table:?}"));
        let reloaded = load(&doc.to_djot().unwrap());
        let body = note_bodies(&reloaded).remove("a").unwrap_or_default();
        assert!(
            body.contains('x') && body.contains('y'),
            "{syntax} {table:?}: the reloaded note reads {body:?}"
        );
        assert_eq!(doc.to_plain_text().unwrap(), "Noted￼ here.\nLast line.");
    }
}

/// Enter after an image put the caret one position late for each image in front of the split:
/// each image is one character of the text it counted, and it added the images again. The
/// next keystroke landed after the first letter of the new paragraph.
#[test]
fn enter_after_an_image_puts_the_caret_at_the_start_of_the_new_paragraph() {
    let doc = load("abcdef\n");
    doc.cursor_at(1)
        .insert_image("image.png", "alt", 4, 4)
        .unwrap();
    doc.cursor_at(3)
        .insert_image("image.png", "alt", 4, 4)
        .unwrap();
    assert_eq!(doc.to_addressable_text().unwrap(), "a\u{FFFC}b\u{FFFC}cdef");
    let cursor = doc.cursor_at(5);
    cursor.insert_block().unwrap();
    assert_eq!(cursor.position(), 6);
    cursor.insert_text("Z").unwrap();
    assert_eq!(
        doc.to_addressable_text().unwrap(),
        "a\u{FFFC}b\u{FFFC}c\nZdef"
    );
    assert_model(&doc, "typing after Enter");
}

/// Formatted text is typed where the caret is, after a table or a footnote's body as before
/// them. Its position was counted over the main text's paragraphs alone, one per character and
/// one per boundary: the text landed two characters early for each table in front of it, and
/// past the last paragraph of the main text both ends of a selection resolved to that
/// paragraph's end, so nothing was replaced and the text went there instead.
#[test]
fn formatted_text_lands_at_the_caret_after_a_table_or_a_note_body() {
    let bold = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    let doc = load("Before.\n\n| a | b |\n\nAfter words.\n");
    let cursor = doc.cursor_at(position_of(&doc, "words"));
    cursor.insert_formatted_text("X", &bold).unwrap();
    assert_eq!(
        doc.to_addressable_text().unwrap(),
        "Before.\n\u{FFFC}\na\nb\nAfter Xwords."
    );
    assert_caret_after(&doc, &cursor, "X");
    assert_model(&doc, "typing bold after the table");

    let doc = load("Noted[^a] here.\n\n[^a]: The note.\n\nLast line.\n");
    let body = position_of(&doc, "note.");
    select(&doc, body, body + 4)
        .insert_formatted_text("text", &bold)
        .unwrap();
    assert!(
        note_bodies(&doc)["a"].contains("The text."),
        "{:?}",
        note_bodies(&doc)
    );
    assert_model(&doc, "typing bold over a word of a note");
}

/// Insert list item read the main text's own paragraphs alone and left its new paragraph out
/// of the rope. With the caret in a quotation, a table cell or a note's body, it either put the
/// item after a paragraph of the main text or failed with "Position N is on no block", and the
/// next export after any insertion could slice a block past its end. The item goes right after
/// the caret's paragraph, in the caret's frame and in the rope.
#[test]
fn inserting_a_list_item_puts_it_after_the_caret_in_its_own_frame() {
    let doc = load("Alpha beta gamma.\n\n> Quoted line one.\n>\n> Quoted two.\n\nOmega end.\n");
    let cursor = doc.cursor_at(position_of(&doc, "two."));
    cursor
        .insert_list(text_document::ListStyle::Decimal)
        .unwrap();
    assert_model(&doc, "the list item in the quotation");
    cursor.insert_text("item").unwrap();
    assert_model(&doc, "typing in the list item");
    assert_eq!(
        doc.to_djot().unwrap(),
        "Alpha beta gamma.\n\n> Quoted line one.\n>\n> Quoted two.\n>\n> 1. item\n\nOmega end."
    );

    let doc = load("Noted[^a].\n\n[^a]: The note.\n\nLast.\n");
    doc.cursor_at(position_of(&doc, "note."))
        .insert_list(text_document::ListStyle::Decimal)
        .unwrap();
    assert_model(&doc, "the list item in a note's body");
    assert_eq!(doc.to_plain_text().unwrap(), "Noted￼.\nLast.");
}

/// A frame inserted after a paragraph follows it in the frames. Its empty paragraph went to
/// the end of the rope instead: the rope's lookup of the paragraph at the caret ran after the
/// new paragraph had been created and was not in the rope yet, so it always failed. Every
/// position after the caret then named the wrong text, and a deletion of the whole document
/// stopped short of the new paragraph.
#[test]
fn a_frame_inserted_after_a_paragraph_follows_it_in_the_rope() {
    let doc = load("One.\n\nTwo.\n\nThree.\n");
    doc.cursor_at(2).insert_frame().unwrap();
    assert_model(&doc, "inserting a frame");
    assert_eq!(doc.to_addressable_text().unwrap(), "One.\n\nTwo.\nThree.");
    select(&doc, 0, length(&doc))
        .remove_selected_text()
        .unwrap();
    assert_model(&doc, "deleting everything");
    assert_eq!(doc.to_addressable_text().unwrap(), "");
}

/// A range from the start of a table's first cell to past the table takes the whole table,
/// as a selection with one end in a table and the other outside it does. When a cell held two
/// paragraphs, the deletion removed the second one before it looked at the tables; the rope,
/// still holding it, was then no longer the position space, so the table's anchor had no
/// position and the table was taken to start at its first cell. That removal is what the
/// range asks for now, and the model has to come out of it whole.
#[test]
fn a_range_from_the_first_cell_on_takes_the_table_when_a_cell_holds_two_paragraphs() {
    let doc = load("Intro.\n\n| x | y |\n| 1 | 2 |\n\nAfter words.\n");
    doc.cursor_at(position_of(&doc, "2\n"))
        .insert_block()
        .unwrap();
    assert_eq!(
        doc.to_addressable_text().unwrap(),
        "Intro.\n\u{FFFC}\nx\ny\n1\n\n2\nAfter words."
    );
    let from = position_of(&doc, "\u{FFFC}") + 1;
    let to = position_of(&doc, "words");
    doc.cursor()
        .replace(from, to, "X", Default::default())
        .unwrap();
    assert_model(&doc, "replacing from the first cell to past the table");
    assert_eq!(doc.to_addressable_text().unwrap(), "Intro.\nXwords.");
}

// ── Lists in quotations, table cells and notes ───────────────────────────────

/// A block as a list shows it: its text, its list's style and level when it is a list item,
/// and how many quotations hold it.
type ListedBlock = (String, Option<(text_document::ListStyle, u8)>, usize);

fn listed_blocks(doc: &TextDocument) -> Vec<ListedBlock> {
    doc.blocks()
        .iter()
        .map(|block| {
            let quoted = doc.cursor_at(block.position()).blockquote_depth_at_cursor();
            let list = block.list().map(|list| (list.style(), list.indent()));
            (block.text(), list, quoted)
        })
        .collect()
}

/// The block of `doc` reading `text`, as [`listed_blocks`] shows it.
#[track_caller]
fn listed(doc: &TextDocument, text: &str) -> ListedBlock {
    listed_blocks(doc)
        .into_iter()
        .find(|(block, _, _)| block == text)
        .unwrap_or_else(|| panic!("no block reads {text:?}"))
}

/// The lists no block is an item of.
fn lists_without_items(doc: &TextDocument) -> Vec<u64> {
    let store = doc.rope_store_for_test();
    let used: HashSet<u64> = store
        .blocks
        .read()
        .values()
        .filter_map(|block| block.list)
        .collect();
    let mut unused: Vec<u64> = store
        .lists
        .read()
        .keys()
        .copied()
        .filter(|id| !used.contains(id))
        .collect();
    unused.sort_unstable();
    unused
}

/// The model holds, and every list has an item.
#[track_caller]
fn assert_lists_whole(doc: &TextDocument, after: &str) {
    assert_model(doc, after);
    assert_eq!(
        lists_without_items(doc),
        Vec::<u64>::new(),
        "after {after}: lists left without an item"
    );
}

/// Move the list item at `at` to `indent` as an editor's Tab and Shift+Tab do: take it out
/// of its list, then make a list of it at the new level, as one edit.
#[track_caller]
fn move_list_item(doc: &TextDocument, at: usize, indent: u8) {
    let cursor = doc.cursor_at(at);
    let style = cursor.current_list().expect("a list item").style();
    cursor.begin_edit_block();
    cursor.remove_current_block_from_list().unwrap();
    cursor.create_list(style).unwrap();
    cursor
        .set_current_list_format(&text_document::ListFormat {
            indent: Some(indent),
            ..Default::default()
        })
        .unwrap();
    cursor.end_edit_block();
}

fn reload_html(html: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_html(html).unwrap().wait().unwrap();
    doc
}

fn reload_markdown(markdown: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_markdown(markdown).unwrap().wait().unwrap();
    doc
}

/// Saved as Djot, HTML and Markdown and read back, each block comes back with its text, its
/// list and level, and its quotation, and saving the Djot again writes the same text.
#[track_caller]
fn assert_lists_round_trip(doc: &TextDocument, after: &str) {
    let blocks = listed_blocks(doc);
    let djot = doc.to_djot().unwrap();
    let back = load(&djot);
    assert_eq!(listed_blocks(&back), blocks, "after {after}, Djot {djot:?}");
    assert_eq!(back.to_djot().unwrap(), djot, "after {after}, Djot again");
    let html = doc.to_html().unwrap();
    assert_eq!(
        listed_blocks(&reload_html(&html)),
        blocks,
        "after {after}, HTML {html:?}"
    );
    let markdown = doc.to_markdown().unwrap();
    assert_eq!(
        listed_blocks(&reload_markdown(&markdown)),
        blocks,
        "after {after}, Markdown {markdown:?}"
    );
}

/// The words of `doc`, in order.
fn words(doc: &TextDocument) -> String {
    doc.to_plain_text()
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Saved as Djot, HTML and Markdown and read back, `doc` reads the same words in the same
/// order. A table cell of Djot and Markdown holds one line, and the HTML reader takes each
/// cell as one paragraph, so a list in a cell comes back as its words.
#[track_caller]
fn assert_cell_words_round_trip(doc: &TextDocument, after: &str) {
    let expected = words(doc);
    let djot = doc.to_djot().unwrap();
    assert_eq!(
        words(&load(&djot)),
        expected,
        "after {after}, Djot {djot:?}"
    );
    let html = doc.to_html().unwrap();
    assert_eq!(
        words(&reload_html(&html)),
        expected,
        "after {after}, HTML {html:?}"
    );
    let markdown = doc.to_markdown().unwrap();
    assert_eq!(
        words(&reload_markdown(&markdown)),
        expected,
        "after {after}, Markdown {markdown:?}"
    );
}

/// Make a list of the paragraphs "One." and "Two." in `doc`, move "Two." one level in and
/// back out, then take it out of the list, checking the list, the item's level, that no list
/// is left without an item, and (through `round_trip`) what an export and a reload keep.
fn make_move_and_remove_a_list_item(
    doc: &TextDocument,
    quoted: usize,
    round_trip: fn(&TextDocument, &str),
) {
    use text_document::ListStyle::Decimal;
    let (one, two) = (position_of(doc, "One."), position_of(doc, "Two."));
    select(doc, one, two + 1).create_list(Decimal).unwrap();
    assert_lists_whole(doc, "making the list");
    assert_eq!(
        listed(doc, "One."),
        ("One.".into(), Some((Decimal, 0)), quoted)
    );
    assert_eq!(
        listed(doc, "Two."),
        ("Two.".into(), Some((Decimal, 0)), quoted)
    );
    assert_eq!(
        listed(doc, "Three.").1,
        None,
        "the paragraph after the selection"
    );
    round_trip(doc, "making the list");

    move_list_item(doc, position_of(doc, "Two."), 1);
    assert_lists_whole(doc, "moving the item one level in");
    assert_eq!(
        listed(doc, "One."),
        ("One.".into(), Some((Decimal, 0)), quoted)
    );
    assert_eq!(
        listed(doc, "Two."),
        ("Two.".into(), Some((Decimal, 1)), quoted)
    );
    round_trip(doc, "moving the item one level in");

    move_list_item(doc, position_of(doc, "Two."), 0);
    assert_lists_whole(doc, "moving the item back out");
    assert_eq!(
        listed(doc, "Two."),
        ("Two.".into(), Some((Decimal, 0)), quoted)
    );
    round_trip(doc, "moving the item back out");

    doc.cursor_at(position_of(doc, "Two."))
        .remove_current_block_from_list()
        .unwrap();
    assert_lists_whole(doc, "taking the item out of the list");
    assert_eq!(
        listed(doc, "One."),
        ("One.".into(), Some((Decimal, 0)), quoted)
    );
    assert_eq!(listed(doc, "Two."), ("Two.".into(), None, quoted));
    round_trip(doc, "taking the item out of the list");

    // Each step is one edit, and undoing them all gives back the paragraphs.
    for _ in 0..4 {
        doc.undo().unwrap();
    }
    assert_lists_whole(doc, "undoing every step");
    assert_eq!(listed(doc, "One.").1, None);
    assert_eq!(listed(doc, "Two.").1, None);
}

/// Making a list in a quotation found no paragraph: it read the main text's own paragraphs
/// alone, so it created a list holding nothing and left the paragraph as it was. Moving an
/// item one level in or out there (take it out of its list, make a list of it at the new
/// level, which is what an editor's Tab does) then took the item out of its list for good.
/// And a list that did survive in a quotation lost its levels at the next save: the Djot
/// writer closed the quotation between two of its blocks, the HTML writer put every item in
/// one flat list, and the Markdown writer put an ordered sub-list under its parent's number.
#[test]
fn a_list_item_in_a_quotation_is_made_moved_and_taken_out() {
    let doc = load("Alpha.\n\n> One.\n>\n> Two.\n>\n> Three.\n\nOmega.\n");
    make_move_and_remove_a_list_item(&doc, 1, assert_lists_round_trip);
}

#[test]
fn a_list_item_in_a_nested_quotation_is_made_moved_and_taken_out() {
    let doc = load("Alpha.\n\n> Outer.\n>\n> > One.\n> >\n> > Two.\n> >\n> > Three.\n\nOmega.\n");
    make_move_and_remove_a_list_item(&doc, 2, assert_lists_round_trip);
}

/// A table cell as a quotation: the list was never made. The words of the cell also ran
/// together through HTML, which writes a cell's two paragraphs as `One.<br/>Two.`: the
/// reader took the line break for nothing.
#[test]
fn a_list_item_in_a_table_cell_is_made_moved_and_taken_out() {
    let doc = load("Alpha.\n\n| One. | x |\n| y | z |\n\nOmega.\n");
    let cursor = doc.cursor_at(position_of(&doc, "One.") + 4);
    cursor.insert_block().unwrap();
    cursor.insert_text("Two.").unwrap();
    cursor.insert_block().unwrap();
    cursor.insert_text("Three.").unwrap();
    assert_model(&doc, "writing three paragraphs in a cell");
    make_move_and_remove_a_list_item(&doc, 0, assert_cell_words_round_trip);
    // The cell holds its three paragraphs still, in their order.
    let cell = doc
        .cursor_at(position_of(&doc, "One."))
        .current_table_cell()
        .expect("the paragraph is in a cell");
    let texts: Vec<String> = doc
        .blocks()
        .iter()
        .filter(|block| {
            block
                .table_cell()
                .is_some_and(|other| (other.row, other.column) == (cell.row, cell.column))
        })
        .map(|block| block.text())
        .collect();
    assert_eq!(texts, ["One.", "Two.", "Three."]);
}

/// A note's body as a quotation, and a list the reader dropped: a list item of a note came
/// back from a save as a plain paragraph, and one nested in it came back one level up, its
/// continuation lines indented less than the note's first line. HTML has no notes to read
/// back, so the note is compared in Djot and Markdown.
#[test]
fn a_list_item_in_a_note_is_made_moved_and_taken_out() {
    fn note_round_trip(doc: &TextDocument, after: &str) {
        let note = |doc: &TextDocument| -> Vec<ListedBlock> {
            ["One.", "Two.", "Three."]
                .into_iter()
                .map(|text| listed(doc, text))
                .collect()
        };
        let expected = note(doc);
        let djot = doc.to_djot().unwrap();
        let back = load(&djot);
        assert_eq!(note(&back), expected, "after {after}, Djot {djot:?}");
        assert_eq!(back.to_djot().unwrap(), djot, "after {after}, Djot again");
        let markdown = doc.to_markdown().unwrap();
        assert_eq!(
            note(&reload_markdown(&markdown)),
            expected,
            "after {after}, Markdown {markdown:?}"
        );
    }
    let doc = load("Noted[^a].\n\n[^a]: One.\n\n    Two.\n\n    Three.\n\nLast.\n");
    make_move_and_remove_a_list_item(&doc, 0, note_round_trip);
}

/// A selection running from the main text through a quotation makes a list in each frame it
/// crosses: a list runs on inside one frame only, in the model as in every format it is
/// written to. It used to make one list of the main text's paragraphs across the quotation,
/// which every format reads back as two, and leave the quotation's paragraph out.
#[test]
fn a_list_made_across_a_quotation_is_a_list_in_each_frame() {
    use text_document::ListStyle::Decimal;
    let doc = load("One.\n\n> Two.\n\nThree.\n\nAfter.\n");
    select(&doc, position_of(&doc, "One."), position_of(&doc, "Three."))
        .create_list(Decimal)
        .unwrap();
    assert_lists_whole(&doc, "making the lists");
    assert_eq!(listed(&doc, "One.").1, Some((Decimal, 0)));
    assert_eq!(listed(&doc, "Two."), ("Two.".into(), Some((Decimal, 0)), 1));
    assert_eq!(listed(&doc, "Three.").1, Some((Decimal, 0)));
    assert_eq!(listed(&doc, "After.").1, None);
    let list_of = |text: &str| {
        doc.block_at_position(position_of(&doc, text))
            .and_then(|block| block.list())
            .map(|list| list.id())
    };
    assert_ne!(list_of("One."), list_of("Three."));
    assert_ne!(list_of("One."), list_of("Two."));
    assert_lists_round_trip(&doc, "making the lists");
}

/// Making a list of the items of another list moves them into it, and the list they all left
/// goes, as it does when its items leave it one by one. It was left behind without an item.
#[test]
fn a_list_whose_items_all_join_a_new_list_is_removed() {
    use text_document::ListStyle::{Decimal, Disc};
    let doc = load("Intro.\n\n- One.\n- Two.\n\n> - Three.\n\nEnd.\n");
    select(&doc, position_of(&doc, "One."), position_of(&doc, "Two."))
        .create_list(Decimal)
        .unwrap();
    assert_lists_whole(&doc, "making a list of a whole list");
    assert_eq!(listed(&doc, "One.").1, Some((Decimal, 0)));
    assert_eq!(listed(&doc, "Two.").1, Some((Decimal, 0)));
    assert_eq!(listed(&doc, "Three.").1, Some((Disc, 0)));
    doc.undo().unwrap();
    assert_lists_whole(&doc, "undoing it");
    assert_eq!(listed(&doc, "One.").1, Some((Disc, 0)));
}

/// Inserting a list item works in a table cell as it does in a quotation and a note (see
/// `inserting_a_list_item_puts_it_after_the_caret_in_its_own_frame`).
#[test]
fn inserting_a_list_item_in_a_table_cell_keeps_it_in_the_cell() {
    let doc = load("Alpha.\n\n| One. | x |\n| y | z |\n\nOmega.\n");
    let cursor = doc.cursor_at(position_of(&doc, "One.") + 4);
    cursor
        .insert_list(text_document::ListStyle::Decimal)
        .unwrap();
    cursor.insert_text("Item.").unwrap();
    assert_lists_whole(&doc, "inserting a list item in a cell");
    assert_eq!(
        listed(&doc, "Item.").1,
        Some((text_document::ListStyle::Decimal, 0))
    );
    let cell = doc
        .cursor_at(position_of(&doc, "Item."))
        .current_table_cell()
        .expect("the item is in a cell");
    assert_eq!((cell.row, cell.column), (0, 0));
    assert_cell_words_round_trip(&doc, "inserting a list item in a cell");
}

/// Moving an item a level in or out with the caret at the end of its text moves that item. The
/// list helpers acting on the current block read the block at the character index, which at
/// the end of a paragraph is the next one: the item after it left its list, and the item at
/// the caret went into a new list at the level the other one had.
#[test]
fn moving_a_list_item_from_the_end_of_its_text_moves_that_item() {
    use text_document::ListStyle::Decimal;
    for (source, quoted) in [
        ("1. zero\n2. first\n3. second\n\nAfter.\n", 0),
        ("> 1. zero\n>\n> 2. first\n>\n> 3. second\n\nAfter.\n", 1),
    ] {
        let doc = load(source);
        let end = position_of(&doc, "first") + "first".len();
        assert_eq!(
            doc.cursor_at(end).current_list().map(|list| list.indent()),
            Some(0)
        );
        nest_list_item(&doc, end, true).unwrap();
        assert_lists_whole(&doc, "moving the item one level in");
        assert_eq!(
            listed(&doc, "zero"),
            ("zero".into(), Some((Decimal, 0)), quoted)
        );
        assert_eq!(
            listed(&doc, "first"),
            ("first".into(), Some((Decimal, 1)), quoted)
        );
        assert_eq!(
            listed(&doc, "second"),
            ("second".into(), Some((Decimal, 0)), quoted)
        );
        nest_list_item(&doc, end, false).unwrap();
        assert_lists_whole(&doc, "moving it back out");
        assert_eq!(listed(&doc, "first").1, Some((Decimal, 0)));
        assert_eq!(listed(&doc, "second").1, Some((Decimal, 0)));
    }
}

/// Moving a list item a level in, undone and redone, is moved again. The redo ran each step
/// of the edit again, and making the item's new list again made it under a new id: setting
/// that list's level, the next step, named the list the undo had taken away, and the redo
/// failed halfway, the item at the top level.
#[test]
fn moving_a_list_item_is_undone_and_redone() {
    use text_document::ListStyle::Decimal;
    for (source, quoted) in [
        ("1. zero\n2. first\n3. second\n", 0),
        ("> 1. zero\n>\n> 2. first\n>\n> 3. second\n", 1),
    ] {
        let doc = load(source);
        move_list_item(&doc, position_of(&doc, "first"), 1);
        let moved = listed_blocks(&doc);
        assert_eq!(
            listed(&doc, "first"),
            ("first".into(), Some((Decimal, 1)), quoted)
        );
        doc.undo().unwrap();
        assert_eq!(
            listed(&doc, "first"),
            ("first".into(), Some((Decimal, 0)), quoted)
        );
        doc.redo().unwrap();
        assert_eq!(listed_blocks(&doc), moved);
        assert_lists_whole(&doc, "redoing the move");
        // And the same again, with the document's history carried on past it.
        doc.undo().unwrap();
        doc.redo().unwrap();
        assert_eq!(listed_blocks(&doc), moved);
        assert_lists_whole(&doc, "redoing the move a second time");
    }
}
