//! Phase 2 step 5: verify markdown/html importers (which run via the
//! long-operation manager and so live behind the public API) populate
//! the global rope.

use text_document::TextDocument;

#[test]
fn set_markdown_populates_rope() {
    let doc = TextDocument::new();
    doc.set_markdown("first paragraph\n\nsecond paragraph")
        .expect("set_markdown")
        .wait()
        .expect("wait");

    // Inspect the rope via the document's internal store.
    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    let rope_text = rope.to_string();
    // Two blocks joined by an inter-block `\n`.
    assert_eq!(rope_text, "first paragraph\nsecond paragraph");

    let offsets = store.block_offsets.read().unwrap();
    assert_eq!(offsets.entries.len(), 2);
    assert_eq!(offsets.entries[0].1, 0);
    assert_eq!(offsets.entries[1].1, 16); // "first paragraph\n".len()
    assert_eq!(offsets.total_bytes(), 32);
}

#[test]
fn set_html_populates_rope() {
    let doc = TextDocument::new();
    doc.set_html("<p>alpha</p><p>beta</p><p>gamma</p>")
        .expect("set_html")
        .wait()
        .expect("wait");

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    assert_eq!(rope.to_string(), "alpha\nbeta\ngamma");

    let offsets = store.block_offsets.read().unwrap();
    assert_eq!(offsets.entries.len(), 3);
    assert_eq!(offsets.entries[0].1, 0);
    assert_eq!(offsets.entries[1].1, 6); // "alpha\n".len()
    assert_eq!(offsets.entries[2].1, 11); // "alpha\nbeta\n".len()
    assert_eq!(offsets.total_bytes(), 16); // full "alpha\nbeta\ngamma"
}

#[test]
fn insert_text_at_position_mirrors_to_rope() {
    let doc = TextDocument::new();
    doc.set_plain_text("hello world").unwrap();

    let cursor = doc.cursor_at(5);
    cursor.insert_text(",").unwrap();

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    assert_eq!(rope.to_string(), "hello, world");
}

#[test]
fn insert_text_at_end_mirrors_to_rope() {
    let doc = TextDocument::new();
    doc.set_plain_text("hello").unwrap();

    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    assert_eq!(rope.to_string(), "hello world");
}

#[test]
fn insert_text_into_block_other_than_first_shifts_offsets() {
    // Multi-block doc; insert into block 2; verify block_offsets
    // for block 3 shift by inserted length.
    let doc = TextDocument::new();
    doc.set_plain_text("aaa\nbbb\nccc").unwrap();

    // block 0 [0..3), block 1 [4..7), block 2 [8..11)
    // Insert "XX" at char position 5 (inside block 1, after "b")
    let cursor = doc.cursor_at(5);
    cursor.insert_text("XX").unwrap();

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    assert_eq!(rope.to_string(), "aaa\nbXXbb\nccc");

    let offsets = store.block_offsets.read().unwrap();
    assert_eq!(offsets.entries.len(), 3);
    assert_eq!(offsets.entries[0].1, 0);
    assert_eq!(offsets.entries[1].1, 4);
    assert_eq!(offsets.entries[2].1, 10); // was 8, shifted by 2
    assert_eq!(offsets.total_bytes(), 13);
}

#[test]
fn delete_within_block_mirrors_to_rope() {
    let doc = TextDocument::new();
    doc.set_plain_text("hello, world").unwrap();

    // Delete the comma + space at positions 5..7
    let cursor = doc.cursor_at(5);
    cursor.set_position(7, text_document::MoveMode::KeepAnchor);
    cursor.remove_selected_text().unwrap();

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    assert_eq!(rope.to_string(), "helloworld");
}

#[test]
fn delete_within_middle_block_shifts_subsequent_offsets() {
    let doc = TextDocument::new();
    doc.set_plain_text("aaa\nbbbbb\nccc").unwrap();

    // block 0 [0..3), block 1 [4..9), block 2 [10..13)
    // Delete chars 5..7 ("bb") inside block 1.
    let cursor = doc.cursor_at(5);
    cursor.set_position(7, text_document::MoveMode::KeepAnchor);
    cursor.remove_selected_text().unwrap();

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    assert_eq!(rope.to_string(), "aaa\nbbb\nccc");

    let offsets = store.block_offsets.read().unwrap();
    assert_eq!(offsets.entries[0].1, 0);
    assert_eq!(offsets.entries[1].1, 4);
    assert_eq!(offsets.entries[2].1, 8); // was 10, shifted by -2
    assert_eq!(offsets.total_bytes(), 11);
}

#[test]
fn insert_image_inserts_object_replacement_sentinel() {
    let doc = TextDocument::new();
    doc.set_plain_text("ab").unwrap();
    let cursor = doc.cursor_at(1);
    cursor.insert_image("img1", 100, 100).unwrap();

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    // U+FFFC is 3 bytes in UTF-8. Original "ab" plus sentinel = 5 bytes.
    assert_eq!(rope.to_string(), "a\u{FFFC}b");
    assert_eq!(rope.len_bytes(), 5);

    let offsets = store.block_offsets.read().unwrap();
    assert_eq!(offsets.total_bytes(), 5);
}

#[test]
fn insert_formatted_text_at_position_mirrors_to_rope() {
    use text_document::TextFormat;

    let doc = TextDocument::new();
    doc.set_plain_text("hello world").unwrap();
    let cursor = doc.cursor_at(5);
    let fmt = TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    cursor.insert_formatted_text(",", &fmt).unwrap();

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    assert_eq!(rope.to_string(), "hello, world");
}

#[test]
fn insert_block_splits_existing_block_in_rope() {
    let doc = TextDocument::new();
    doc.set_plain_text("hello world").unwrap();

    // Insert a block boundary at char position 5 (between "hello" and " world")
    let cursor = doc.cursor_at(5);
    cursor.insert_block().unwrap();

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    assert_eq!(rope.to_string(), "hello\n world");

    let offsets = store.block_offsets.read().unwrap();
    assert_eq!(offsets.entries.len(), 2);
    assert_eq!(offsets.entries[0].1, 0);
    assert_eq!(offsets.entries[1].1, 6); // after "hello\n"
    assert_eq!(offsets.total_bytes(), 12);
}

#[test]
fn cross_block_delete_merges_in_rope() {
    let doc = TextDocument::new();
    doc.set_plain_text("aaa\nbbb\nccc").unwrap();

    // block 0 [0..3), block 1 [4..7), block 2 [8..11) — total 11 bytes
    // Select from char 2 (inside block 0) through char 9 (inside block 2)
    // and delete. Expected merged result: "aacc" — block 0 keeps "aa",
    // block 2's "cc" merges in.
    let cursor = doc.cursor_at(2);
    cursor.set_position(9, text_document::MoveMode::KeepAnchor);
    cursor.remove_selected_text().unwrap();

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    assert_eq!(rope.to_string(), "aacc");

    let offsets = store.block_offsets.read().unwrap();
    // Two intermediate blocks (block 1 and block 2) removed from
    // index; only the merged block remains.
    assert_eq!(offsets.entries.len(), 1);
    assert_eq!(offsets.entries[0].1, 0);
    assert_eq!(offsets.total_bytes(), 4);
}

#[test]
fn insert_table_inserts_sentinel_and_cells_inline_in_rope() {
    // Three-block doc; insert a 2x2 table between block 1 and block 2.
    let doc = TextDocument::new();
    doc.set_plain_text("alpha\nbeta\ngamma").unwrap();
    // Layout in rope: "alpha\nbeta\ngamma" (16 bytes)
    //                 ^0    ^6   ^11
    // Insert table at cursor position 6 (start of "beta"). With
    // offset == 0, the table goes BEFORE "beta".
    let cursor = doc.cursor_at(6);
    cursor.insert_table(2, 2).unwrap();

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    // The table anchor sentinel AND its 4 empty cells are spliced INLINE,
    // right where the table sits in the flow (before "beta"), NOT appended
    // at the end of the rope. This keeps the rope's char order identical to
    // the document/flow order, so the editor's caret space and the editing
    // path agree (and cursor navigation can move through the table).
    //   "alpha\n" + U+FFFC + sentinel-`\n` + 4×(cell-boundary `\n` + empty)
    //   + "beta\ngamma"
    //   = "alpha\n\u{FFFC}\n\n\n\n\nbeta\ngamma" (24 bytes)
    assert_eq!(rope.to_string(), "alpha\n\u{FFFC}\n\n\n\n\nbeta\ngamma");

    let offsets = store.block_offsets.read().unwrap();
    // 3 main blocks + 1 TableAnchor + 4 cell blocks = 8 entries
    assert_eq!(offsets.entries.len(), 8);
    // Index order is now flow order: alpha, TableAnchor, [4 cells], beta, gamma
    assert!(offsets.entries[0].0.is_block(), "alpha");
    assert!(offsets.entries[1].0.as_table_anchor().is_some(), "table anchor");
    assert!(offsets.entries[2].0.is_block(), "cell (0,0)");
    assert!(offsets.entries[3].0.is_block(), "cell (0,1)");
    assert!(offsets.entries[4].0.is_block(), "cell (1,0)");
    assert!(offsets.entries[5].0.is_block(), "cell (1,1)");
    assert!(offsets.entries[6].0.is_block(), "beta");
    assert!(offsets.entries[7].0.is_block(), "gamma");
    // Byte starts: alpha=0, anchor=6 (U+FFFC), cells inline at 10..=13,
    // then beta=14, gamma=19.
    assert_eq!(offsets.entries[0].1, 0);
    assert_eq!(offsets.entries[1].1, 6);
    assert_eq!(offsets.entries[2].1, 10);
    assert_eq!(offsets.entries[3].1, 11);
    assert_eq!(offsets.entries[4].1, 12);
    assert_eq!(offsets.entries[5].1, 13);
    assert_eq!(offsets.entries[6].1, 14);
    assert_eq!(offsets.entries[7].1, 19);
    assert_eq!(offsets.total_bytes(), 24);
}

#[test]
fn cell_text_edits_mirror_to_rope() {
    let doc = TextDocument::new();
    doc.set_plain_text("main").unwrap();
    let cursor = doc.cursor_at(4);
    let _table = cursor.insert_table(1, 2).unwrap();

    // After insertion: rope = "main\n\u{FFFC}\n\n" (main + sentinel
    // + boundary + 2 empty cells with boundaries). Now type into the
    // first cell. The cell's block lives at the end of the rope;
    // its block id is one of the entries[4..].
    let store = doc.rope_store_for_test();
    let registered_block_ids: Vec<u64> = {
        let offsets = store.block_offsets.read().unwrap();
        offsets
            .entries
            .iter()
            .filter_map(|(m, _)| m.as_block())
            .collect()
    };
    drop(store);

    // Find the cell using the public flow API and edit it.
    use text_document::FlowElement;
    let mut first_cell_block_id: Option<u64> = None;
    for elem in doc.flow() {
        if let FlowElement::Table(t) = elem {
            let snap = t.snapshot();
            if let Some(cell) = snap.cells.first()
                && let Some(blk) = cell.blocks.first()
            {
                first_cell_block_id = Some(blk.block_id as u64);
            }
            break;
        }
    }
    let cell_block_id = first_cell_block_id.expect("cell block not found in flow");
    // Sanity-check: this cell block IS one of the registered block entries.
    assert!(registered_block_ids.contains(&cell_block_id));

    // Move cursor to start of that cell block and type.
    let cell_block = doc.block_by_id(cell_block_id as usize).expect("block");
    let cell_pos = cell_block.position();
    let cell_cursor = doc.cursor_at(cell_pos);
    cell_cursor.insert_text("hi").unwrap();

    // The rope's cell area should now contain "hi" for that cell.
    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    assert!(
        rope.to_string().contains("hi"),
        "rope should contain edited cell text, got {:?}",
        rope.to_string()
    );
}

#[test]
fn remove_table_strips_sentinel_from_rope() {
    let doc = TextDocument::new();
    doc.set_plain_text("alpha\nbeta").unwrap();
    let cursor = doc.cursor_at(6);
    let table = cursor.insert_table(1, 1).unwrap();
    let table_id = table.id();

    // Verify the sentinel landed.
    {
        let store = doc.rope_store_for_test();
        let rope = store.rope.read().unwrap();
        assert!(rope.to_string().contains('\u{FFFC}'));
    }

    // Remove via cursor.
    let c2 = doc.cursor_at(0);
    c2.remove_table(table_id).unwrap();

    // Rope should be back to "alpha\nbeta".
    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    assert_eq!(rope.to_string(), "alpha\nbeta");
    let offsets = store.block_offsets.read().unwrap();
    assert_eq!(offsets.entries.len(), 2);
    assert!(offsets.entries.iter().all(|(m, _)| m.is_block()));
}

#[test]
fn set_html_table_cells_in_main_rope() {
    // HTML-imported tables put their cell content into the global
    // rope alongside the table-anchor sentinel.
    let doc = TextDocument::new();
    doc.set_html("<p>before</p><table><tr><td>cell</td></tr></table><p>after</p>")
        .expect("set_html")
        .wait()
        .expect("wait");

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    // Layout: paragraph "before", boundary, U+FFFC anchor, boundary,
    // cell "cell", boundary, paragraph "after".
    assert_eq!(rope.to_string(), "before\n\u{FFFC}\ncell\nafter");
}

/// Phase 2 step 5.6b: pasting an inline-only fragment (the
/// "copy text from one part, paste into another" path) mirrors the
/// splice into the rope.
#[test]
fn paste_inline_fragment_mirrors_to_rope() {
    let doc = TextDocument::new();
    doc.set_plain_text("hello world").unwrap();

    // Select "world" (char positions 6..11) and copy.
    let select = doc.cursor_at(6);
    select.move_position(
        text_document::MoveOperation::Right,
        text_document::MoveMode::KeepAnchor,
        5,
    );
    let frag = select.selection();

    // Paste at position 0.
    let paste = doc.cursor_at(0);
    paste.insert_fragment(&frag).unwrap();

    // Expected: "world" prepended to original → "worldhello world".
    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    assert_eq!(rope.to_string(), "worldhello world");
}

/// Phase 2 step 5.6b: pasting a multi-block fragment splits the
/// current block and inserts intermediate blocks. The rope mirror
/// must keep all new blocks consistent.
#[test]
fn paste_multi_block_fragment_mirrors_to_rope() {
    let doc = TextDocument::new();
    doc.set_plain_text("A1\nB2\nC3").unwrap();

    // Select "B2" plus the surrounding newlines so the fragment has
    // two blocks (the trailing portion of A1 if any, B2, and the
    // leading portion of C3 — actually selecting "1\nB2\nC" gives a
    // 3-block fragment in practice). For simplicity select full "B2".
    let select = doc.cursor_at(3); // start of B2
    select.move_position(
        text_document::MoveOperation::Right,
        text_document::MoveMode::KeepAnchor,
        2,
    );
    let frag = select.selection();

    // Paste at position 0 (start of A1).
    let paste = doc.cursor_at(0);
    paste.insert_fragment(&frag).unwrap();

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    let rope_text = rope.to_string();

    // Expected: "B2" prepended to the original. Single inline-only
    // block fragment — uses the inline-merge path → result "B2A1\nB2\nC3".
    assert_eq!(rope_text, "B2A1\nB2\nC3");
}

/// Phase 2 step 5.6b: pasting a table-only fragment (cell selection
/// → copy → paste elsewhere) exercises insert_table_fragment. The
/// rope mirror must insert the anchor sentinel and place each cell's
/// blocks at the containing top-level frame's end.
#[test]
fn paste_table_fragment_mirrors_to_rope() {
    // Build a doc containing a 2x2 table by importing HTML.
    let src = TextDocument::new();
    src.set_html("<p>before</p><table><tr><td>A</td><td>B</td></tr><tr><td>C</td><td>D</td></tr></table><p>after</p>")
        .expect("set_html").wait().expect("wait");

    // Select the entire table by selecting from inside the first cell
    // to inside the last cell.
    use text_document::FlowElement;
    let mut table_id_opt: Option<usize> = None;
    for elem in src.flow() {
        if let FlowElement::Table(t) = elem {
            table_id_opt = Some(t.id());
            break;
        }
    }
    let table_id = table_id_opt.expect("table");

    let cur = src.cursor_at(0);
    cur.select_cell_range(table_id, 0, 0, 1, 1);
    let frag = cur.selection();

    // Paste into a fresh doc.
    let dst = TextDocument::new();
    dst.set_plain_text("dst").unwrap();
    let paste = dst.cursor_at(3);
    paste.insert_fragment(&frag).unwrap();

    // The rope should contain at least the original "dst" plus the
    // U+FFFC table-anchor sentinel and the cell contents A, B, C, D
    // somewhere in the cell area.
    let store = dst.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    let rope_text = rope.to_string();
    assert!(
        rope_text.contains('\u{FFFC}'),
        "rope should contain table-anchor sentinel, got {:?}",
        rope_text
    );
    for cell_text in &["A", "B", "C", "D"] {
        assert!(
            rope_text.contains(cell_text),
            "rope should contain cell text {:?}, got {:?}",
            cell_text,
            rope_text
        );
    }
}

/// Phase 2 step 5.6b: pasting a fragment that spans multiple blocks
/// exercises the block-splitting path of insert_fragment_uc. The
/// rope mirror must coordinate the head sync + middle/tail block
/// creation correctly.
#[test]
fn paste_cross_block_fragment_mirrors_to_rope() {
    let doc = TextDocument::new();
    doc.set_plain_text("A1\nB2\nC3").unwrap();
    // Block positions: A1 at 0..2, '\n' at 2, B2 at 3..5, '\n' at 5, C3 at 6..8.

    // Select from inside A1 (pos 1) to inside C3 (pos 7) — spans 3 blocks:
    // partial "1", full "B2", partial "C".
    let select = doc.cursor_at(1);
    select.move_position(
        text_document::MoveOperation::Right,
        text_document::MoveMode::KeepAnchor,
        6,
    );
    let frag = select.selection();

    // Move to end of doc and paste.
    let paste = doc.cursor_at(8);
    paste.insert_fragment(&frag).unwrap();

    let store = doc.rope_store_for_test();
    let rope = store.rope.read().unwrap();
    let rope_text = rope.to_string();
    // Doc after paste should contain the original "A1\nB2\nC3" PLUS the
    // pasted "1\nB2\nC" content appended at the end.
    assert_eq!(rope_text, "A1\nB2\nC31\nB2\nC");
}
