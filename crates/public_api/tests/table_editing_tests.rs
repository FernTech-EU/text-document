//! Tests for editing inside table cells.
//!
//! These tests validate that the snapshot position system and the sequential
//! position computation (`find_block_at_position_sequential`) stay in sync
//! when text is inserted, deleted, or replaced inside table cells.

use text_document::{
    Alignment, BlockFormat, FlowElement, FlowElementSnapshot, MoveMode, TextDocument,
};

/// Create a document: "Before" + 2x2 table + "After"
/// Create a document: "Before" + 2x2 empty table + "After" using insert_table
fn doc_with_empty_table() -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text("Before").unwrap();
    let cursor = doc.cursor_at(6);
    cursor.insert_table(2, 2).unwrap();
    let end = doc.character_count();
    let cursor2 = doc.cursor_at(end);
    cursor2.insert_block().unwrap();
    cursor2.insert_text("After").unwrap();
    doc
}

/// Create a document: "Before" + 2x2 table + "After"
fn doc_with_table_and_text() -> TextDocument {
    let doc = TextDocument::new();
    doc.set_markdown("Before\n\n| A | B |\n|---|---|\n| c | d |\n\nAfter")
        .unwrap()
        .wait()
        .unwrap();
    doc
}

/// Collect all block (position, length, text) tuples from a snapshot, in order.
fn all_block_positions(doc: &TextDocument) -> Vec<(usize, usize, String)> {
    let snap = doc.snapshot_flow();
    let mut out = Vec::new();
    collect_from_elements(&snap.elements, &mut out);
    out
}

fn collect_from_elements(elements: &[FlowElementSnapshot], out: &mut Vec<(usize, usize, String)>) {
    for el in elements {
        match el {
            FlowElementSnapshot::Block(bs) => {
                out.push((bs.position, bs.length, bs.text.clone()));
            }
            FlowElementSnapshot::Table(ts) => {
                for cell in &ts.cells {
                    for block in &cell.blocks {
                        out.push((block.position, block.length, block.text.clone()));
                    }
                }
            }
            FlowElementSnapshot::Frame(fs) => {
                collect_from_elements(&fs.elements, out);
            }
        }
    }
}

/// Assert that no two blocks overlap and positions are monotonically increasing.
fn assert_no_overlaps(positions: &[(usize, usize, String)]) {
    let mut sorted = positions.to_vec();
    sorted.sort_by_key(|(pos, _, _)| *pos);
    for i in 1..sorted.len() {
        let (prev_pos, prev_len, ref prev_text) = sorted[i - 1];
        let (cur_pos, _, ref cur_text) = sorted[i];
        let prev_end = prev_pos + prev_len + 1;
        assert!(
            cur_pos >= prev_end,
            "Block {:?} at pos {} (end {}) overlaps with block {:?} at pos {}",
            prev_text,
            prev_pos,
            prev_end,
            cur_text,
            cur_pos
        );
    }
}

/// Find the snapshot position of the first cell (0,0) block.
fn cell_block_position(doc: &TextDocument, row: usize, col: usize) -> Option<(usize, usize)> {
    let snap = doc.snapshot_flow();
    for el in &snap.elements {
        if let FlowElementSnapshot::Table(ts) = el {
            for cell in &ts.cells {
                if cell.row == row
                    && cell.column == col
                    && let Some(b) = cell.blocks.first()
                {
                    return Some((b.position, b.length));
                }
            }
        }
    }
    None
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Position consistency
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn positions_no_overlap_fresh_table() {
    let doc = doc_with_table_and_text();
    let positions = all_block_positions(&doc);
    assert_no_overlaps(&positions);
}

#[test]
fn positions_no_overlap_after_insert_in_first_cell() {
    let doc = doc_with_table_and_text();
    let (pos, len) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    let cursor = doc.cursor_at(pos + len);
    cursor.insert_text("X").unwrap();
    let positions = all_block_positions(&doc);
    assert_no_overlaps(&positions);
}

#[test]
fn positions_no_overlap_after_insert_in_last_cell() {
    let doc = doc_with_table_and_text();
    let (pos, len) = cell_block_position(&doc, 1, 1).expect("cell (1,1)");
    let cursor = doc.cursor_at(pos + len);
    cursor.insert_text("Z").unwrap();
    let positions = all_block_positions(&doc);
    assert_no_overlaps(&positions);
}

#[test]
fn positions_no_overlap_after_multiple_inserts() {
    let doc = doc_with_table_and_text();

    // Type in cell (0,0)
    let (pos, len) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    let cursor = doc.cursor_at(pos + len);
    cursor.insert_text("Hello").unwrap();

    // Type in cell (1,1)
    let (pos2, len2) = cell_block_position(&doc, 1, 1).expect("cell (1,1)");
    let cursor2 = doc.cursor_at(pos2 + len2);
    cursor2.insert_text("World").unwrap();

    let positions = all_block_positions(&doc);
    assert_no_overlaps(&positions);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Text appears in the correct cell
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn insert_text_appears_in_cell() {
    let doc = doc_with_table_and_text();
    let (pos, len) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    let cursor = doc.cursor_at(pos + len);
    cursor.insert_text("X").unwrap();

    let snap = doc.snapshot_flow();
    let cell_text: Option<&str> = snap.elements.iter().find_map(|el| {
        if let FlowElementSnapshot::Table(ts) = el {
            ts.cells
                .iter()
                .find(|c| c.row == 0 && c.column == 0)
                .and_then(|c| c.blocks.first())
                .map(|b| b.text.as_str())
        } else {
            None
        }
    });

    let text = cell_text.expect("cell (0,0) should have a block");
    assert!(
        text.contains('X'),
        "cell (0,0) text should contain 'X', got {:?}",
        text
    );
}

#[test]
fn after_block_position_shifts_when_cell_grows() {
    let doc = doc_with_table_and_text();
    let positions_before = all_block_positions(&doc);
    let after_pos_before = positions_before
        .iter()
        .find(|(_, _, t)| t == "After")
        .map(|(p, _, _)| *p)
        .expect("should find 'After'");

    let (pos, len) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    let cursor = doc.cursor_at(pos + len);
    cursor.insert_text("XYZ").unwrap();

    let positions_after = all_block_positions(&doc);
    let after_pos_after = positions_after
        .iter()
        .find(|(_, _, t)| t == "After")
        .map(|(p, _, _)| *p)
        .expect("should find 'After'");

    assert_eq!(
        after_pos_after,
        after_pos_before + 3,
        "'After' position should shift by 3 chars"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Cursor positioning
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn cursor_stays_in_cell_after_insert() {
    let doc = doc_with_table_and_text();
    let (pos, len) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    let cursor = doc.cursor_at(pos + len);
    cursor.insert_text("X").unwrap();

    let cursor_pos = cursor.position();
    let (cell_pos, cell_len) = cell_block_position(&doc, 0, 0).expect("cell (0,0) after edit");

    assert!(
        cursor_pos >= cell_pos && cursor_pos <= cell_pos + cell_len,
        "cursor at {} should be within cell (0,0) range [{}, {}]",
        cursor_pos,
        cell_pos,
        cell_pos + cell_len
    );
}

#[test]
fn consecutive_inserts_in_same_cell() {
    let doc = doc_with_table_and_text();
    let (pos, len) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    let cursor = doc.cursor_at(pos + len);

    // Type three characters
    cursor.insert_text("a").unwrap();
    cursor.insert_text("b").unwrap();
    cursor.insert_text("c").unwrap();

    // All text should be in cell (0,0)
    let snap = doc.snapshot_flow();
    let cell_text: Option<&str> = snap.elements.iter().find_map(|el| {
        if let FlowElementSnapshot::Table(ts) = el {
            ts.cells
                .iter()
                .find(|c| c.row == 0 && c.column == 0)
                .and_then(|c| c.blocks.first())
                .map(|b| b.text.as_str())
        } else {
            None
        }
    });

    let text = cell_text.expect("cell (0,0) should have a block");
    assert!(
        text.contains("abc"),
        "cell (0,0) should contain 'abc', got {:?}",
        text
    );

    // Positions should still be valid
    assert_no_overlaps(&all_block_positions(&doc));
}

#[test]
fn delete_in_cell_keeps_positions_valid() {
    let doc = doc_with_table_and_text();
    let (pos, len) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");

    // Only delete if cell has content
    if len > 0 {
        let cursor = doc.cursor_at(pos + len);
        cursor.delete_previous_char().unwrap();
        assert_no_overlaps(&all_block_positions(&doc));
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Undo/redo with table edits
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn snapshot_block_at_position_finds_cell_blocks() {
    let doc = doc_with_table_and_text();

    // Get cell (0,0) position from full snapshot
    let (cell_pos, _cell_len) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");

    // snapshot_block_at_position should find this cell block
    let snap = doc
        .snapshot_block_at_position(cell_pos)
        .expect("should find block at cell position");
    assert_eq!(
        snap.position, cell_pos,
        "snapshot position should match cell position"
    );
    assert!(
        snap.table_cell.is_some(),
        "block should have table_cell context"
    );
}

#[test]
fn snapshot_block_at_position_finds_cell_after_edit() {
    let doc = doc_with_table_and_text();
    let (cell_pos, cell_len) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");

    // Insert text
    let cursor = doc.cursor_at(cell_pos + cell_len);
    cursor.insert_text("XYZ").unwrap();

    // snapshot_block_at_position should still find the edited cell block
    let snap = doc
        .snapshot_block_at_position(cell_pos)
        .expect("should find block at cell position after edit");
    assert!(
        snap.text.contains("XYZ"),
        "snapshot text should contain inserted text, got {:?}",
        snap.text
    );
}

#[test]
fn insert_in_empty_cell_positions_stay_valid() {
    let doc = doc_with_empty_table();
    let positions = all_block_positions(&doc);
    assert_no_overlaps(&positions);

    // Find an empty cell and type in it
    if let Some((pos, len)) = cell_block_position(&doc, 0, 0) {
        assert_eq!(len, 0, "empty table cell should have length 0");
        let cursor = doc.cursor_at(pos);
        cursor.insert_text("Hello").unwrap();
        assert_no_overlaps(&all_block_positions(&doc));
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// `cursor.insert_table(rows, cols)` round-trip
//
// Symptoms reported against the formatting toolbar's "Insert Table"
// button: after typing in cells of a 3×3 inserted table, cells
// (2,0)..(2,2) become unreachable, and typing in cell (1,1) can
// spill into the following block. Markdown-imported tables work fine
// (covered by the older tests above) — these tests pin the
// programmatic insertion path.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

fn inserted_3x3_with_surrounding_text() -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text("Before").unwrap();
    let cursor = doc.cursor_at(6);
    cursor.insert_table(3, 3).unwrap();
    // Position immediately past the table's last cell, derived from the
    // snapshot's actual position space rather than hard-coded arithmetic.
    // (The rope-canonical model represents the table itself as a 1-char
    // U+FFFC anchor sentinel, so "6 + 1 separator + 9 cells" no longer
    // names the spot just past the table — the last cell's reported
    // position + 1 does, independent of the sentinel footprint.)
    let (last_cell_pos, last_cell_len) =
        cell_block_position(&doc, 2, 2).expect("3x3 has a (2,2) cell");
    let after_table = last_cell_pos + last_cell_len + 1;
    let cursor2 = doc.cursor_at(after_table);
    cursor2.insert_block().unwrap();
    cursor2.insert_text("After").unwrap();
    doc
}

#[test]
fn inserted_3x3_all_nine_cells_addressable() {
    let doc = inserted_3x3_with_surrounding_text();
    // Cell positions should be strictly monotonic in row-major order
    // and match the snapshot's `running_pos` walk (one boundary
    // between each empty cell).
    let mut prev: Option<usize> = None;
    for r in 0..3 {
        for c in 0..3 {
            let pos = cell_block_position(&doc, r, c);
            assert!(
                pos.is_some(),
                "cell ({r},{c}) of a fresh 3x3 insert_table should be addressable, snapshot returned None"
            );
            let (p, l) = pos.unwrap();
            assert_eq!(
                l, 0,
                "fresh cell ({r},{c}) should be empty (len 0), got len {l}"
            );
            if let Some(prev_p) = prev {
                assert_eq!(
                    p,
                    prev_p + 1,
                    "cell ({r},{c}) at pos {p} should be one boundary past previous cell at {prev_p}"
                );
            }
            prev = Some(p);
        }
    }
}

#[test]
fn inserted_3x3_typing_in_each_cell_lands_in_that_cell() {
    let doc = inserted_3x3_with_surrounding_text();
    // Type one distinct character per cell, in a non-row-major order so
    // a position-arithmetic bug that gets the first row right is still
    // exposed.
    let plan = [
        (2, 2, 'a'),
        (1, 1, 'b'),
        (0, 0, 'c'),
        (2, 0, 'd'),
        (0, 2, 'e'),
        (1, 0, 'f'),
        (0, 1, 'g'),
        (2, 1, 'h'),
        (1, 2, 'i'),
    ];
    for (r, c, ch) in plan {
        let (pos, len) = cell_block_position(&doc, r, c)
            .unwrap_or_else(|| panic!("cell ({r},{c}) should be reachable before typing '{ch}'"));
        let cursor = doc.cursor_at(pos + len);
        cursor.insert_text(&ch.to_string()).unwrap();
        // After typing, the snapshot should show the character in that cell.
        let snap = doc.snapshot_flow();
        let cell_text = snap.elements.iter().find_map(|el| {
            if let FlowElementSnapshot::Table(ts) = el {
                ts.cells
                    .iter()
                    .find(|cc| cc.row == r && cc.column == c)
                    .and_then(|cc| cc.blocks.first())
                    .map(|b| b.text.clone())
            } else {
                None
            }
        });
        let text = cell_text.unwrap_or_default();
        assert!(
            text.contains(ch),
            "cell ({r},{c}) should contain '{ch}' after typing, got {text:?}; full doc positions: {:?}",
            all_block_positions(&doc)
        );
    }
    // After all typing, no overlaps; "After" still exists.
    let positions = all_block_positions(&doc);
    assert_no_overlaps(&positions);
    assert!(
        positions.iter().any(|(_, _, t)| t == "After"),
        "post-table 'After' block should survive nine in-cell inserts; got {positions:?}"
    );
}

/// Find the snapshot position of a named block.
fn block_position_by_text(doc: &TextDocument, text: &str) -> Option<usize> {
    all_block_positions(doc)
        .into_iter()
        .find(|(_, _, t)| t == text)
        .map(|(p, _, _)| p)
}

/// Snapshot the text inside cell (r, c).
fn cell_text(doc: &TextDocument, row: usize, col: usize) -> Option<String> {
    let snap = doc.snapshot_flow();
    snap.elements.iter().find_map(|el| {
        if let FlowElementSnapshot::Table(ts) = el {
            ts.cells
                .iter()
                .find(|cc| cc.row == row && cc.column == col)
                .and_then(|cc| cc.blocks.first())
                .map(|b| b.text.clone())
        } else {
            None
        }
    })
}

#[test]
fn inserted_2x2_at_start_of_block_lands_before_it() {
    // Cursor at the START of an existing block (offset 0) places
    // the table *before* that block in the flow. The cells should
    // span a contiguous run of positions starting at the cursor's
    // position; the displaced block should sit past the table.
    let doc = TextDocument::new();
    doc.set_plain_text("Hello").unwrap();
    let cursor = doc.cursor_at(0);
    cursor.insert_table(2, 2).unwrap();

    let positions = all_block_positions(&doc);
    let hello_pos =
        block_position_by_text(&doc, "Hello").expect("'Hello' block should survive insertion");
    let cell_positions: Vec<usize> = positions
        .iter()
        .filter_map(|(p, _, t)| if t.is_empty() { Some(*p) } else { None })
        .collect();
    assert_eq!(
        cell_positions.len(),
        4,
        "2x2 table should yield 4 empty cell blocks; got {cell_positions:?}"
    );
    // The table sits at the document start: its cells are the earliest
    // content (only the table's own 1-char anchor sentinel precedes them),
    // contiguous, each one boundary past the previous. We assert "earliest
    // content" via the minimum block position rather than a literal 0,
    // because in the rope-canonical model the sentinel occupies position 0
    // and the cells follow it.
    let min_pos = positions.iter().map(|(p, _, _)| *p).min().unwrap();
    assert_eq!(
        cell_positions[0], min_pos,
        "the table (cells) should be the first content in the document; positions={positions:?}"
    );
    for w in cell_positions.windows(2) {
        assert_eq!(
            w[1],
            w[0] + 1,
            "cells should be contiguous: got {cell_positions:?}"
        );
    }
    // 'Hello' should appear after the table.
    assert!(
        hello_pos > *cell_positions.last().unwrap(),
        "'Hello' should sit after the table; got hello_pos={hello_pos}, cells={cell_positions:?}"
    );
    // Typing in cell (0,0) should land in cell (0,0).
    let (p00, l00) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    let c = doc.cursor_at(p00 + l00);
    c.insert_text("X").unwrap();
    assert_eq!(
        cell_text(&doc, 0, 0).as_deref(),
        Some("X"),
        "typing in cell (0,0) of a before-the-block insertion should land in (0,0); positions: {:?}",
        all_block_positions(&doc)
    );
    // 'Hello' content untouched.
    assert!(
        block_position_by_text(&doc, "Hello").is_some(),
        "'Hello' content must survive a same-line insertion"
    );
}

#[test]
fn inserted_2x2_in_empty_document_starts_at_zero() {
    // Empty doc still has an implicit empty block (TextDocument::new()
    // creates it). insert_table at offset 0 produces a contiguous run of
    // cells at the document start, with the implicit empty block displaced
    // past the table. In the rope-canonical model the table's 1-char anchor
    // sentinel occupies the very first position, so the cells start just
    // past it (not at a literal 0) — assert ordering/contiguity instead.
    let doc = TextDocument::new();
    let cursor = doc.cursor_at(0);
    cursor.insert_table(2, 2).unwrap();

    let positions = all_block_positions(&doc);
    let cell_positions: Vec<usize> = positions
        .iter()
        .take(4) // first four entries are the cells in row-major order
        .map(|(p, _, _)| *p)
        .collect();
    let min_pos = positions.iter().map(|(p, _, _)| *p).min().unwrap();
    assert_eq!(
        cell_positions[0], min_pos,
        "the table's cells should be the first content in the empty doc; positions: {positions:?}"
    );
    for w in cell_positions.windows(2) {
        assert_eq!(
            w[1],
            w[0] + 1,
            "cells should be contiguous; positions: {positions:?}"
        );
    }
    // The implicit empty block is displaced to after the table's cells.
    assert!(
        positions.len() >= 5 && positions[4].0 > cell_positions[3],
        "the implicit empty block should sit after the table; positions: {positions:?}"
    );
    // Typing in cell (0,0) should land in cell (0,0).
    let (p00, l00) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    let c = doc.cursor_at(p00 + l00);
    c.insert_text("Y").unwrap();
    assert_eq!(
        cell_text(&doc, 0, 0).as_deref(),
        Some("Y"),
        "typing in cell (0,0) of an empty-doc insertion should land in (0,0); positions: {:?}",
        all_block_positions(&doc)
    );
}

/// For every block in the document, assert `document_position` ==
/// snapshot position. The two MUST stay in lock-step or cursor
/// lookups drift from what the user sees.
fn assert_doc_pos_matches_snapshot(doc: &TextDocument, label: &str) {
    let snapshot_positions = all_block_positions(doc);
    let raw_doc_positions = {
        let mut out: Vec<(usize, String)> = Vec::new();
        fn walk(el: FlowElement, out: &mut Vec<(usize, String)>) {
            match el {
                FlowElement::Block(b) => out.push((b.position(), b.text())),
                FlowElement::Table(t) => {
                    for r in 0..t.rows() {
                        for c in 0..t.columns() {
                            if let Some(cell) = t.cell(r, c) {
                                for b in cell.blocks() {
                                    out.push((b.position(), b.text()));
                                }
                            }
                        }
                    }
                }
                FlowElement::Frame(f) => {
                    for el in f.flow() {
                        walk(el, out);
                    }
                }
            }
        }
        for el in doc.flow() {
            walk(el, &mut out);
        }
        out
    };
    assert_eq!(
        raw_doc_positions.len(),
        snapshot_positions.len(),
        "[{label}] block count mismatch: raw={raw_doc_positions:?}, snap={snapshot_positions:?}"
    );
    for (idx, ((raw_pos, raw_text), (snap_pos, _, snap_text))) in raw_doc_positions
        .iter()
        .zip(snapshot_positions.iter())
        .enumerate()
    {
        assert_eq!(
            raw_text, snap_text,
            "[{label}] block #{idx} text mismatch: raw={raw_text:?}, snap={snap_text:?}"
        );
        assert_eq!(
            *raw_pos, *snap_pos,
            "[{label}] block #{idx} ({raw_text:?}): document_position={raw_pos} ≠ snapshot position={snap_pos}. \
             They must agree so cursor lookups (sorted by document_position) match the rendered snapshot. \
             Full raw={raw_doc_positions:?}, snap={snapshot_positions:?}"
        );
    }
}

#[test]
fn doc_pos_matches_snapshot_for_multi_block_then_insert_table_anywhere() {
    // Mimics the real editor scenario: a document with several
    // paragraphs of varying length. Click into each paragraph at
    // various offsets (start, middle, end), insert a 3x3 table, and
    // assert the snapshot positions and document_position values stay
    // in lock-step for every block — including the cells just
    // inserted and every block after them.
    let make_doc = || {
        let doc = TextDocument::new();
        doc.set_markdown(
            "First paragraph here.\n\
             \n\
             Second paragraph, a bit longer than the first to make things interesting.\n\
             \n\
             Third — short.\n\
             \n\
             Fourth paragraph that's also somewhat long, with words and stuff to stretch it out.",
        )
        .expect("parse markdown")
        .wait()
        .expect("import");
        doc
    };

    // Probe a handful of cursor positions across the doc: start of
    // each paragraph, middle of each, and end of each.
    let probes: Vec<(usize, &str)> = {
        let doc = make_doc();
        let mut probes = Vec::new();
        let positions = all_block_positions(&doc);
        for (p, l, t) in positions {
            if t.is_empty() {
                continue;
            }
            probes.push((p, "start"));
            if l >= 4 {
                probes.push((p + l / 2, "middle"));
            }
            probes.push((p + l, "end"));
        }
        probes
    };

    for (pos, label_where) in probes {
        let doc = make_doc();
        let cursor = doc.cursor_at(pos);
        cursor.insert_table(3, 3).unwrap();
        assert_doc_pos_matches_snapshot(
            &doc,
            &format!("insert_table(3,3) at pos {pos} ({label_where} of block)"),
        );
    }
}

#[test]
fn doc_pos_stays_consistent_after_typing_then_insert_table() {
    // Mirror what a real user does: load a multi-block document, do
    // a few edits, then insert a table. The invariant
    // `document_position == snapshot position` must hold after each
    // step.
    let doc = TextDocument::new();
    doc.set_markdown(
        "First paragraph.\n\
         \n\
         Second paragraph is a bit longer.\n\
         \n\
         Third has even more content for variety.\n\
         \n\
         Fourth and final paragraph here.",
    )
    .expect("parse")
    .wait()
    .expect("import");
    assert_doc_pos_matches_snapshot(&doc, "after markdown import");

    // Type some characters into the second paragraph at a known
    // position (somewhere in the middle).
    let positions = all_block_positions(&doc);
    let second = positions
        .iter()
        .find(|(_, _, t)| t.contains("Second"))
        .map(|(p, l, _)| (*p, *l))
        .expect("second paragraph");
    let cursor = doc.cursor_at(second.0 + 5);
    cursor.insert_text("XYZ").unwrap();
    assert_doc_pos_matches_snapshot(&doc, "after typing 'XYZ' in second paragraph");

    // Type into the LAST paragraph too.
    let positions2 = all_block_positions(&doc);
    let fourth = positions2
        .iter()
        .find(|(_, _, t)| t.contains("Fourth"))
        .map(|(p, l, _)| (*p, *l))
        .expect("fourth paragraph");
    let cursor2 = doc.cursor_at(fourth.0 + fourth.1);
    cursor2.insert_text(" Edited.").unwrap();
    assert_doc_pos_matches_snapshot(&doc, "after typing in fourth paragraph");

    // Now insert a 3x3 table mid-third paragraph.
    let positions3 = all_block_positions(&doc);
    let third = positions3
        .iter()
        .find(|(_, _, t)| t.contains("Third"))
        .map(|(p, l, _)| (*p, *l))
        .expect("third paragraph");
    let cursor3 = doc.cursor_at(third.0 + 4);
    cursor3.insert_table(3, 3).unwrap();
    assert_doc_pos_matches_snapshot(
        &doc,
        "after insert_table(3,3) mid-third paragraph following edits",
    );
}

#[test]
fn inserted_2x2_deep_in_long_block_document_position_matches_snapshot() {
    // The user-reported regression: when the cursor sits deep inside
    // a long host block, the table is placed *after* the host block
    // in child_order, but the cells' `document_position` field was
    // being set to `insert_pos + 1, insert_pos + 2, …` — which is
    // inside the host block's range when `insert_pos` is well shy of
    // the host's end. Subsequent operations that route by
    // `document_position` (insert_text, find_block_at_position, etc.)
    // then route into the wrong block. Visually the table appears in
    // the right place (the snapshot walks child_order, ignoring
    // document_position), but cursor lookups land elsewhere.
    let doc = TextDocument::new();
    let long_text = "abcdefghijklmnopqrstuvwxyz"; // 26 chars
    doc.set_plain_text(long_text).unwrap();
    let cursor = doc.cursor_at(5); // 5 chars in
    cursor.insert_table(2, 2).unwrap();

    // Compare snapshot positions (what the user sees) to
    // document_position (what cursor lookups use). They must agree.
    let snapshot_positions = all_block_positions(&doc);
    eprintln!("snapshot positions after insert_table(2,2) at offset 5: {snapshot_positions:?}");
    let raw_doc_positions = {
        // Pull all blocks' document_position via the API and pair
        // them with their text content. Order by id for determinism.
        let mut out: Vec<(usize, String)> = Vec::new();
        for el in doc.flow() {
            match el {
                FlowElement::Block(b) => out.push((b.position(), b.text())),
                FlowElement::Table(t) => {
                    for r in 0..t.rows() {
                        for c in 0..t.columns() {
                            if let Some(cell) = t.cell(r, c)
                                && let Some(b) = cell.blocks().first()
                            {
                                out.push((b.position(), b.text()));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        out
    };
    eprintln!("document_position values:                                  {raw_doc_positions:?}");

    // Both iterations walk the same flow in the same order, so compare
    // positionally. (Empty cells share text "" so a `find`-by-text
    // lookup collapses them all to the first match.)
    assert_eq!(
        raw_doc_positions.len(),
        snapshot_positions.len(),
        "block count must match: raw={raw_doc_positions:?}, snap={snapshot_positions:?}"
    );
    for (idx, ((raw_pos, raw_text), (snap_pos, _, snap_text))) in raw_doc_positions
        .iter()
        .zip(snapshot_positions.iter())
        .enumerate()
    {
        assert_eq!(
            raw_text, snap_text,
            "block #{idx} text mismatch: raw={raw_text:?}, snap={snap_text:?}"
        );
        assert_eq!(
            *raw_pos, *snap_pos,
            "block #{idx} ({raw_text:?}): document_position={raw_pos} but snapshot position={snap_pos}. \
             The two must agree, otherwise cursor lookups (sorted by document_position) drift from \
             the snapshot rendered for the user."
        );
    }
}

#[test]
fn inserted_2x2_deep_in_long_block_lands_after_block() {
    // The user-reported regression: when the cursor sits deep inside
    // a long host block, the table is placed *after* the host block
    // (per `child_order_insert_idx = found_idx + 1`), so the cells'
    // `document_position` field must match the snapshot's running_pos
    // walk — which places them at `host_end + 1`, NOT
    // `cursor_pos + 1`. A previous revision used `insert_pos + 1`,
    // so the deeper the cursor sat in the block (= the larger
    // `host.length - offset`), the further the cells' document_position
    // diverged from the snapshot — making subsequent typing route into
    // the wrong cell or back into the host block.
    let doc = TextDocument::new();
    let long_text = "abcdefghijklmnopqrstuvwxyz"; // 26 chars, one block
    doc.set_plain_text(long_text).unwrap();
    // Cursor 5 chars in (well shy of the end at offset 26).
    let cursor = doc.cursor_at(5);
    cursor.insert_table(2, 2).unwrap();

    let positions = all_block_positions(&doc);
    let host_pos = positions
        .iter()
        .find(|(_, _, t)| t == long_text)
        .map(|(p, _, _)| *p)
        .expect("host block survives");
    let host_len = positions
        .iter()
        .find(|(_, _, t)| t == long_text)
        .map(|(_, l, _)| *l)
        .unwrap();
    let cell_positions: Vec<usize> = positions
        .iter()
        .filter_map(|(p, _, t)| if t.is_empty() { Some(*p) } else { None })
        .collect();
    assert_eq!(cell_positions.len(), 4, "2x2 → 4 cells");
    // Cells must start strictly PAST the host block's end, not at the
    // cursor's offset inside it. The original regression placed cells at
    // `insert_pos + 1` (= 6, well inside the 26-char host), so asserting
    // `cell_positions[0] > host_pos + host_len` catches it independently of
    // the cursor offset. (We no longer assert the exact `host_end + 1`: the
    // rope-canonical model inserts the table's 1-char anchor sentinel
    // between the host and its cells, a constant footprint that does not
    // depend on the cursor offset — which is what this test actually guards.)
    assert!(
        cell_positions[0] > host_pos + host_len,
        "cells should start past the host block's end, not inside it \
         (host_pos={host_pos}, host_len={host_len}, cell_positions={cell_positions:?})"
    );
    for w in cell_positions.windows(2) {
        assert_eq!(w[1], w[0] + 1, "cells contiguous: {cell_positions:?}");
    }
    // Typing in cell (0,0) must land in cell (0,0), regardless of how
    // deep the cursor was in the host block.
    let (p00, l00) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    let c = doc.cursor_at(p00 + l00);
    c.insert_text("Q").unwrap();
    assert_eq!(
        cell_text(&doc, 0, 0).as_deref(),
        Some("Q"),
        "deep-in-block insertion: typing in cell (0,0) lands in (0,0); positions: {:?}",
        all_block_positions(&doc)
    );
}

#[test]
fn inserted_2x2_in_middle_of_block_lands_after_it() {
    // Cursor in the MIDDLE of a block (offset > 0) places the table
    // *after* that block in the flow. The cells should start one
    // boundary past the host block's end.
    let doc = TextDocument::new();
    doc.set_plain_text("Hello world").unwrap();
    let cursor = doc.cursor_at(5); // between "Hello" and " world", inside the block
    cursor.insert_table(2, 2).unwrap();

    let positions = all_block_positions(&doc);
    let host_pos = block_position_by_text(&doc, "Hello world")
        .expect("host block should survive insertion as a single block");
    let cell_positions: Vec<usize> = positions
        .iter()
        .filter_map(|(p, _, t)| if t.is_empty() { Some(*p) } else { None })
        .collect();
    assert_eq!(cell_positions.len(), 4);
    // Cells should follow the host block (not precede it).
    assert!(
        cell_positions[0] > host_pos,
        "cells should be after 'Hello world' (host_pos={host_pos}, cells={cell_positions:?})"
    );
    for w in cell_positions.windows(2) {
        assert_eq!(w[1], w[0] + 1, "cells contiguous: {cell_positions:?}");
    }
    // Typing in cell (0,0) lands in cell (0,0).
    let (p00, l00) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    let c = doc.cursor_at(p00 + l00);
    c.insert_text("Z").unwrap();
    assert_eq!(
        cell_text(&doc, 0, 0).as_deref(),
        Some("Z"),
        "typing in cell (0,0) of an after-the-block insertion should land in (0,0)"
    );
}

#[test]
fn inserted_3x3_does_not_leak_into_following_block() {
    let doc = inserted_3x3_with_surrounding_text();
    // Type a lot in cell (1,1) — enough that a "this cell only owns one
    // position" bug would push the input into the cell after it (or out
    // of the table entirely).
    let (p, l) = cell_block_position(&doc, 1, 1).expect("cell (1,1) addressable");
    let cursor = doc.cursor_at(p + l);
    let burst = "the quick brown fox";
    cursor.insert_text(burst).unwrap();

    let snap = doc.snapshot_flow();
    // Check the cell got the entire burst (no spill).
    let cell_text = snap.elements.iter().find_map(|el| {
        if let FlowElementSnapshot::Table(ts) = el {
            ts.cells
                .iter()
                .find(|c| c.row == 1 && c.column == 1)
                .and_then(|c| c.blocks.first())
                .map(|b| b.text.clone())
        } else {
            None
        }
    });
    assert_eq!(
        cell_text.as_deref(),
        Some(burst),
        "cell (1,1) should hold the entire burst; full doc positions: {:?}",
        all_block_positions(&doc)
    );

    // The other 8 cells should still be empty (no spill into them).
    for r in 0..3 {
        for c in 0..3 {
            if r == 1 && c == 1 {
                continue;
            }
            let other_text = snap.elements.iter().find_map(|el| {
                if let FlowElementSnapshot::Table(ts) = el {
                    ts.cells
                        .iter()
                        .find(|cc| cc.row == r && cc.column == c)
                        .and_then(|cc| cc.blocks.first())
                        .map(|b| b.text.clone())
                } else {
                    None
                }
            });
            assert_eq!(
                other_text.as_deref(),
                Some(""),
                "cell ({r},{c}) should be empty (no spill from typing in (1,1)), got {other_text:?}"
            );
        }
    }

    // "After" must still exist and not contain any of the burst.
    let after_block = all_block_positions(&doc)
        .into_iter()
        .find(|(_, _, t)| t == "After");
    assert!(
        after_block.is_some(),
        "'After' must survive typing in cell (1,1)"
    );
}

#[test]
fn undo_insert_in_cell_restores_positions() {
    let doc = doc_with_table_and_text();
    let positions_before = all_block_positions(&doc);

    let (pos, len) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    let cursor = doc.cursor_at(pos + len);
    cursor.insert_text("XYZ").unwrap();

    doc.undo().unwrap();

    let positions_after = all_block_positions(&doc);
    assert_eq!(
        positions_before.len(),
        positions_after.len(),
        "block count should match after undo"
    );
    for (before, after) in positions_before.iter().zip(positions_after.iter()) {
        assert_eq!(
            before.0, after.0,
            "position mismatch after undo: {:?} vs {:?}",
            before, after
        );
        assert_eq!(
            before.2, after.2,
            "text mismatch after undo: {:?} vs {:?}",
            before, after
        );
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Markdown importer: per-element consistency of document_position
// against the snapshot's running positions. If any of these fail,
// it means the importer leaves Block.document_position out of sync
// with where the snapshot walker places the block.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn import_blockquote_single_level() {
    let doc = TextDocument::new();
    doc.set_markdown("Para A\n\n> Quoted line one.\n>\n> Quoted line two.\n\nPara B")
        .unwrap()
        .wait()
        .unwrap();
    assert_doc_pos_matches_snapshot(&doc, "blockquote single level");
}

#[test]
fn import_blockquote_nested_three_levels() {
    let doc = TextDocument::new();
    doc.set_markdown(
        "Para A\n\
         \n\
         > Level 1.\n\
         >\n\
         > > Level 2.\n\
         >\n\
         > > > Level 3.\n\
         \n\
         Para B",
    )
    .unwrap()
    .wait()
    .unwrap();
    assert_doc_pos_matches_snapshot(&doc, "blockquote nested 3 levels");
}

#[test]
fn import_fenced_code_block() {
    let doc = TextDocument::new();
    doc.set_markdown(
        "Para A\n\
         \n\
         ```rust\n\
         fn hi() {}\n\
         ```\n\
         \n\
         Para B",
    )
    .unwrap()
    .wait()
    .unwrap();
    assert_doc_pos_matches_snapshot(&doc, "fenced code block");
}

#[test]
fn import_gfm_table_then_paragraph() {
    let doc = TextDocument::new();
    doc.set_markdown(
        "Para A\n\
         \n\
         | A | B |\n\
         |---|---|\n\
         | c | d |\n\
         \n\
         Para B",
    )
    .unwrap()
    .wait()
    .unwrap();
    assert_doc_pos_matches_snapshot(&doc, "GFM table");
}

#[test]
fn import_nested_unordered_list() {
    let doc = TextDocument::new();
    doc.set_markdown(
        "- Item 0\n\
         - Item 1\n  - Nested 1.1\n  - Nested 1.2\n    - Deeper 1.2.1\n- Item 2",
    )
    .unwrap()
    .wait()
    .unwrap();
    assert_doc_pos_matches_snapshot(&doc, "nested unordered list");
}

#[test]
fn import_full_rich_text_editor_sample_structure() {
    // Mirrors the rich_text_editor example's SAMPLE constant
    // structurally — every element class the importer touches in
    // that demo. If document_position drifts from snapshot positions
    // here, every block past the drift point is unreachable from
    // cursor lookups (which sort by document_position).
    let doc = TextDocument::new();
    doc.set_markdown(
        "# Title\n\
         \n\
         Para A with **bold** and *italic* and `code`.\n\
         \n\
         ## Heading 2\n\
         \n\
         Para B short.\n\
         \n\
         ### Heading 3\n\
         \n\
         - List item 0\n\
         - List item 1\n  - Nested 1.1\n  - Nested 1.2\n    - Deeper 1.2.1\n\
         - List item 2\n\
         \n\
         1. Ordered 0\n\
         2. Ordered 1\n   1. Nested 1.1\n      1. Deeper 1.1.1\n\
         3. Ordered 2\n\
         \n\
         > Quoted level 1.\n\
         >\n\
         > > Quoted level 2.\n\
         > >\n\
         > > > Quoted level 3.\n\
         \n\
         ```rust\n\
         fn hi() {}\n\
         ```\n\
         \n\
         ```python\n\
         def hi(): pass\n\
         ```\n\
         \n\
         | A | B | C |\n\
         |---|---|---|\n\
         | 1 | 2 | 3 |\n\
         | 4 | 5 | 6 |\n\
         \n\
         Final paragraph.",
    )
    .unwrap()
    .wait()
    .unwrap();
    assert_doc_pos_matches_snapshot(&doc, "full rich_text_editor sample structure");
}

/// Mimics the rich_text_editor example's apply_default_margins:
/// walks every block, sets per-kind top/bottom margins via
/// cursor.set_block_format. Reproducible because we read
/// block.position() and set_position(...) just like the example.
fn apply_default_margins_test_clone(doc: &TextDocument) {
    for block in doc.blocks() {
        let heading_level = block.block_format().heading_level;
        let (top, bottom) = match heading_level {
            Some(1) => (24, 12),
            Some(2) => (20, 10),
            Some(3) => (16, 8),
            Some(4) => (12, 6),
            Some(_) => (10, 4),
            None => (4, 4),
        };
        let cursor = doc.cursor_at(block.position());
        cursor.set_position(block.position(), MoveMode::MoveAnchor);
        let _ = cursor.set_block_format(&BlockFormat {
            top_margin: Some(top),
            bottom_margin: Some(bottom),
            ..BlockFormat::default()
        });
    }
}

fn apply_alignment_demos_test_clone(doc: &TextDocument) {
    for block in doc.blocks() {
        let text = block.text();
        let trimmed = text.trim_start();
        let (alignment, text_indent) = if trimmed.starts_with("[Center]") {
            (Some(Alignment::Center), None)
        } else if trimmed.starts_with("[Right]") {
            (Some(Alignment::Right), None)
        } else if trimmed.starts_with("[Justify]") {
            (Some(Alignment::Justify), None)
        } else if trimmed.starts_with("[Indent]") {
            (None, Some(32))
        } else {
            continue;
        };
        let prior = block.block_format();
        let cursor = doc.cursor_at(block.position());
        cursor.set_position(block.position(), MoveMode::MoveAnchor);
        let _ = cursor.set_block_format(&BlockFormat {
            alignment,
            text_indent,
            top_margin: prior.top_margin,
            bottom_margin: prior.bottom_margin,
            ..BlockFormat::default()
        });
    }
}

#[test]
fn apply_default_margins_keeps_positions_consistent() {
    // The rich_text_editor demo runs this immediately after
    // set_markdown.wait(). If set_block_format leaks any drift,
    // it shows up here.
    let doc = TextDocument::new();
    doc.set_markdown(
        "# Title\n\
         \n\
         Para A.\n\
         \n\
         ## H2\n\
         \n\
         - List item 0\n  - Nested 1\n\
         \n\
         > Quoted line.\n\
         \n\
         ```rust\n\
         fn hi() {}\n\
         ```\n\
         \n\
         | A | B |\n\
         |---|---|\n\
         | c | d |\n\
         \n\
         Final.",
    )
    .unwrap()
    .wait()
    .unwrap();
    assert_doc_pos_matches_snapshot(&doc, "before apply_default_margins");
    apply_default_margins_test_clone(&doc);
    assert_doc_pos_matches_snapshot(&doc, "after apply_default_margins");
}

#[test]
fn insert_table_from_inside_cell_lands_after_containing_table() {
    // User-reported: clicking inside a table cell and pressing the
    // toolbar's "Insert Table" creates a strange invisible block and
    // misplaces the new table. Desired: when the cursor is inside any
    // cell of an existing table, the new table goes IMMEDIATELY AFTER
    // that table, in the table's parent frame's flow.
    let doc = TextDocument::new();
    doc.set_markdown(
        "Para A.\n\
         \n\
         | A | B | C |\n\
         |---|---|---|\n\
         | 1 | 2 | 3 |\n\
         | 4 | 5 | 6 |\n\
         \n\
         Para B.",
    )
    .unwrap()
    .wait()
    .unwrap();
    assert_doc_pos_matches_snapshot(&doc, "before insert (cell scenario)");
    let pre_top_level = doc.flow().len();

    // Place cursor inside the "5" cell (row 2, col 1 of the imported
    // table). Walk the snapshot to find its position.
    let target = all_block_positions(&doc)
        .into_iter()
        .find(|(_, _, t)| t == "5")
        .expect("'5' cell exists");
    let cursor = doc.cursor_at(target.0);
    cursor.set_position(target.0, MoveMode::MoveAnchor);
    cursor.insert_table(2, 2).expect("insert");

    assert_doc_pos_matches_snapshot(&doc, "after insert from inside cell");

    // The new table must NOT be nested inside the cell. It must appear
    // as a sibling of the imported table, immediately after it, in the
    // document's top-level flow.
    let flow = doc.flow();
    assert_eq!(
        flow.len(),
        pre_top_level + 1,
        "expected exactly +1 top-level flow element (the new table). \
         If +0 the new table was nested inside the cell instead of \
         hoisted out; if >+1 the parent frame got split."
    );

    // Verify the order: [ParaA, imported_table, new_table, ParaB].
    let kinds: Vec<&'static str> = flow
        .iter()
        .map(|el| match el {
            FlowElement::Block(_) => "block",
            FlowElement::Table(_) => "table",
            FlowElement::Frame(_) => "frame",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["block", "table", "table", "block"],
        "expected new table between imported table and 'Para B', got {kinds:?}"
    );
}

#[test]
fn insert_table_after_imported_gfm_table_lands_at_doc_end() {
    // Reproduction of the user-reported "table inserted N blocks before
    // the cursor" drift. The parent frame here contains:
    //   child_order: [+ParaA_blk, -imported_table_anchor, +Final_blk]
    // The OLD insert_table_uc computed the insertion index by walking
    // the frame's `blocks` list (which holds only [+ParaA, +Final]),
    // so Final landed at blocks_idx=1, while in child_order Final is
    // at idx=2. The new anchor went into child_order at idx=2 (before
    // Final) instead of idx=3 (after Final).
    //
    // Fix: walk `child_order` directly to locate the target block.
    let doc = TextDocument::new();
    doc.set_markdown(
        "Para A.\n\
         \n\
         | A | B | C |\n\
         |---|---|---|\n\
         | 1 | 2 | 3 |\n\
         | 4 | 5 | 6 |\n\
         \n\
         Final paragraph here.",
    )
    .unwrap()
    .wait()
    .unwrap();
    assert_doc_pos_matches_snapshot(&doc, "after import (small)");

    let pre = all_block_positions(&doc);
    let pre_top_level = doc.flow().len();
    let (last_pos, last_len, _) = *pre.last().unwrap();
    let end_pos = last_pos + last_len;
    let cursor = doc.cursor_at(end_pos);
    cursor.set_position(end_pos, MoveMode::MoveAnchor);
    cursor.insert_table(3, 3).expect("insert");

    assert_doc_pos_matches_snapshot(&doc, "after insert_table at end (small)");

    // The new table must be the LAST top-level flow element.
    let flow = doc.flow();
    assert_eq!(
        flow.len(),
        pre_top_level + 1,
        "expected exactly +1 top-level element (the new table)"
    );
    assert!(
        matches!(flow.last(), Some(FlowElement::Table(_))),
        "new table must land at the very end of the document, not between earlier elements"
    );
}

#[test]
fn rich_text_editor_demo_end_to_end_insert_table_at_end() {
    // Exact reproduction of the user's failing scenario:
    //   1. Load the SAMPLE markdown (mirroring the demo).
    //   2. apply_default_margins, apply_alignment_demos (the demo does both).
    //   3. Place cursor at character_count() (end of doc, in the last block).
    //   4. Insert a 3x3 table.
    //   5. Assert document_position == snapshot position for every block
    //      (including the new cells), AND assert the new table is the
    //      LAST top-level flow element (not "4 blocks before the cursor").
    let doc = TextDocument::new();
    doc.set_markdown(
        "# Title\n\
         \n\
         Para A with **bold** and *italic* and `code`.\n\
         \n\
         ## Heading 2\n\
         \n\
         Para B short.\n\
         \n\
         ### Heading 3\n\
         \n\
         - List item 0\n\
         - List item 1\n  - Nested 1.1\n  - Nested 1.2\n    - Deeper 1.2.1\n\
         - List item 2\n\
         \n\
         1. Ordered 0\n\
         2. Ordered 1\n   1. Nested 1.1\n      1. Deeper 1.1.1\n\
         3. Ordered 2\n\
         \n\
         > Quoted level 1.\n\
         >\n\
         > > Quoted level 2.\n\
         > >\n\
         > > > Quoted level 3.\n\
         \n\
         ```rust\n\
         fn hi() {}\n\
         ```\n\
         \n\
         | A | B | C |\n\
         |---|---|---|\n\
         | 1 | 2 | 3 |\n\
         | 4 | 5 | 6 |\n\
         \n\
         Final paragraph here.",
    )
    .unwrap()
    .wait()
    .unwrap();

    assert_doc_pos_matches_snapshot(&doc, "after import");
    apply_default_margins_test_clone(&doc);
    assert_doc_pos_matches_snapshot(&doc, "after apply_default_margins");
    apply_alignment_demos_test_clone(&doc);
    assert_doc_pos_matches_snapshot(&doc, "after apply_alignment_demos");

    // The user's failing scenario: click at the end of the visible
    // text — that's the end of the LAST snapshot block, NOT
    // character_count() (which sums block char lengths and excludes
    // per-block boundary separators that the snapshot does count).
    let pre_snap = all_block_positions(&doc);
    let (last_pos, last_len, _) = *pre_snap.last().expect("doc non-empty");
    let end_pos = last_pos + last_len;
    let cursor = doc.cursor_at(end_pos);
    cursor.set_position(end_pos, MoveMode::MoveAnchor);

    // Capture top-level flow text BEFORE insertion so we can verify
    // the new table really lands at the END (not 4 blocks before).
    let pre_top_level_count = doc.flow().len();

    cursor
        .insert_table(3, 3)
        .expect("insert_table at end of doc");

    assert_doc_pos_matches_snapshot(&doc, "after insert_table at end-of-doc");

    // The new table should be the last (or second-to-last, depending on
    // whether the host block stays at the very end) top-level flow
    // element. Concretely: when the cursor is at end-of-doc inside the
    // "Final paragraph here." block (offset > 0), insert_table picks
    // after=true, so the table goes AFTER the final paragraph — meaning
    // the table is now the last top-level flow element.
    let flow = doc.flow();
    assert!(
        flow.len() == pre_top_level_count + 1,
        "expected exactly +1 top-level element (the table), got {} → {}",
        pre_top_level_count,
        flow.len()
    );
    match flow.last().expect("non-empty flow") {
        FlowElement::Table(_) => {} // expected
        other => panic!(
            "expected the LAST top-level flow element to be the new Table; got {:?}. \
             User reported: 'table is inserted 4 blocks before the cursor' — this assertion \
             captures that regression.",
            std::mem::discriminant(other)
        ),
    }
}
