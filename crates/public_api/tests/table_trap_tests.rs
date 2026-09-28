//! Tests for table-trap selection logic.
//!
//! When extending a selection across a table boundary, the cursor position
//! snaps to the table's edge on the far side, so the entire table is
//! enclosed ("trapped") by the selection range. This mirrors LibreOffice
//! behaviour.

use text_document::{FlowElementSnapshot, MoveMode, SelectionKind, TextDocument};

// ── Setup helpers ────────────────────────────────────────────────

/// "Before" block + 2x2 table (A/B/c/d) + "After" block.
fn doc_with_table() -> TextDocument {
    let doc = TextDocument::new();
    doc.set_markdown("Before\n\n| A | B |\n|---|---|\n| c | d |\n\nAfter")
        .unwrap()
        .wait()
        .unwrap();
    doc
}

/// Look up the first table's cell (0,0) block position from the snapshot.
fn first_table_cell_position(doc: &TextDocument) -> usize {
    let snap = doc.snapshot_flow();
    for el in &snap.elements {
        if let FlowElementSnapshot::Table(ts) = el {
            for cell in &ts.cells {
                if cell.row == 0
                    && cell.column == 0
                    && let Some(b) = cell.blocks.first()
                {
                    return b.position;
                }
            }
        }
    }
    panic!("no table cell (0,0) found");
}

/// Find the "After" block position (the first top-level block after the table).
fn after_block_position(doc: &TextDocument) -> usize {
    let snap = doc.snapshot_flow();
    let mut found_table = false;
    for el in &snap.elements {
        match el {
            FlowElementSnapshot::Table(_) => found_table = true,
            FlowElementSnapshot::Block(bs) if found_table => return bs.position,
            _ => {}
        }
    }
    panic!("no block found after table");
}

/// Find the "Before" block position and length.
fn before_block_info(doc: &TextDocument) -> (usize, usize) {
    let snap = doc.snapshot_flow();
    for el in &snap.elements {
        if let FlowElementSnapshot::Block(bs) = el {
            return (bs.position, bs.length);
        }
    }
    panic!("no block found before table");
}

// ── Forward snap: anchor before table, position inside table ────

/// Moved forward into a table, the position goes to the end of the table's last cell, the
/// last position a removal of the selection takes. It went to the start of the block after
/// the table, one position more than the removal takes, since the two paragraphs are not
/// joined.
#[test]
fn trap_snap_forward_position_lands_at_the_end_of_the_last_cell() {
    let doc = doc_with_table();
    let cell_pos = first_table_cell_position(&doc);
    let after_pos = after_block_position(&doc);

    // Anchor in "Before", extend selection into table cell
    let cursor = doc.cursor_at(2);
    cursor.set_position(cell_pos, MoveMode::KeepAnchor);

    assert_eq!(
        cursor.position(),
        after_pos - 1,
        "position should snap to the end of the table's last cell"
    );
    assert_eq!(cursor.anchor(), 2, "anchor should not move");
    let removed = cursor.remove_selected_text().unwrap();
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "Be\nAfter",
        "removed {removed:?}"
    );
}

#[test]
fn trap_snap_forward_selection_kind_is_mixed() {
    let doc = doc_with_table();
    let cell_pos = first_table_cell_position(&doc);

    let cursor = doc.cursor_at(2);
    cursor.set_position(cell_pos, MoveMode::KeepAnchor);

    match cursor.selection_kind() {
        SelectionKind::Mixed {
            text_before,
            text_after,
            ..
        } => {
            assert!(text_before, "should have text before table");
            assert!(!text_after, "should end with the table");
        }
        other => panic!("expected Mixed, got {:?}", other),
    }
}

#[test]
fn trap_snap_forward_selected_cells_covers_all() {
    let doc = doc_with_table();
    let cell_pos = first_table_cell_position(&doc);

    let cursor = doc.cursor_at(0);
    cursor.set_position(cell_pos, MoveMode::KeepAnchor);

    let cells = cursor.selected_cells();
    assert_eq!(cells.len(), 4, "2x2 table should have all 4 cells selected");
}

// ── Reverse snap: anchor after table, position inside table ─────

/// Moved back into a table, the position goes to the table's anchor, its start, so the
/// selection holds the table and none of the paragraph before it. It went to the end of that
/// paragraph, and a removal of the selection joined the paragraph to what was left of the one
/// the selection started in.
#[test]
fn trap_snap_reverse_position_lands_at_the_tables_anchor() {
    let doc = doc_with_table();
    let cell_pos = first_table_cell_position(&doc);
    let (before_pos, before_len) = before_block_info(&doc);
    let before_end = before_pos + before_len;

    // Anchor in "After", extend selection backwards into table
    let after_pos = after_block_position(&doc);
    let cursor = doc.cursor_at(after_pos + 3);
    cursor.set_position(cell_pos, MoveMode::KeepAnchor);

    assert_eq!(
        cursor.position(),
        before_end + 1,
        "position should snap to the table's anchor"
    );
    cursor.remove_selected_text().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "Before\ner");
}

#[test]
fn trap_snap_reverse_selection_kind_is_mixed() {
    let doc = doc_with_table();
    let cell_pos = first_table_cell_position(&doc);
    let after_pos = after_block_position(&doc);

    let cursor = doc.cursor_at(after_pos + 3);
    cursor.set_position(cell_pos, MoveMode::KeepAnchor);

    match cursor.selection_kind() {
        SelectionKind::Mixed { .. } => {}
        other => panic!("expected Mixed, got {:?}", other),
    }
}

// ── No snap when table is first or last ─────────────────────────

#[test]
fn no_snap_when_table_is_first() {
    // Create document with table at the very start (no preceding block)
    let doc = TextDocument::new();
    let cursor0 = doc.cursor();
    cursor0.insert_table(2, 2).unwrap();
    let end = doc.character_count();
    let cursor_end = doc.cursor_at(end);
    cursor_end.insert_block().unwrap();
    cursor_end.insert_text("After").unwrap();

    let cell_pos = first_table_cell_position(&doc);
    let after_pos = after_block_position(&doc);

    // Anchor in "After", try to extend into table
    let cursor = doc.cursor_at(after_pos + 2);
    cursor.set_position(cell_pos, MoveMode::KeepAnchor);

    // No block before table, so no snap; position stays as resolved
    let pos = cursor.position();
    assert!(
        pos <= cell_pos,
        "no snap should happen when table is first: position {} should be <= cell_pos {}",
        pos,
        cell_pos
    );
}

// ── MoveAnchor does not snap ────────────────────────────────────

#[test]
fn move_anchor_no_snap() {
    let doc = doc_with_table();
    let cell_pos = first_table_cell_position(&doc);

    // Plain click (MoveAnchor) should not snap
    let cursor = doc.cursor_at(0);
    cursor.set_position(cell_pos, MoveMode::MoveAnchor);

    assert_eq!(
        cursor.position(),
        cell_pos,
        "MoveAnchor should place cursor inside table without snapping"
    );
    assert_eq!(cursor.anchor(), cell_pos);
}

// ── Snap from move_position (shift+arrow across table) ──────────

#[test]
fn move_position_keep_anchor_triggers_snap() {
    let doc = doc_with_table();
    let (before_pos, before_len) = before_block_info(&doc);
    let before_end = before_pos + before_len;

    // Place cursor at end of "Before" block
    let cursor = doc.cursor_at(before_end);

    // Move right with KeepAnchor - the next position is inside the table,
    // which should trigger the snap to the end of the table.
    let after_pos = after_block_position(&doc);
    cursor.set_position(before_end + 1, MoveMode::KeepAnchor);

    assert_eq!(
        cursor.position(),
        after_pos - 1,
        "shift-right from end of block before table should snap to the end of the table"
    );
}
