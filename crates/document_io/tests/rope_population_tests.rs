//! Phase 2 step 5: verify use cases populate the global rope.

extern crate text_document_io as document_io;

use anyhow::Result;
use document_io::ImportPlainTextDto;
use document_io::document_io_controller;
use test_harness::setup;

#[test]
fn import_plain_text_single_line_populates_rope() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup()?;

    document_io_controller::import_plain_text(
        &db_context,
        &event_hub,
        &ImportPlainTextDto {
            plain_text: "Hello World".to_string(),
        },
    )?;

    let store = db_context.get_store();
    let rope = store.rope.read();
    assert_eq!(rope.to_string(), "Hello World");
    assert_eq!(rope.len_bytes(), 11);

    let offsets = store.block_offsets.read();
    assert_eq!(offsets.entries.len(), 1);
    assert_eq!(offsets.entries[0].1, 0);
    assert_eq!(offsets.total_bytes(), 11);

    Ok(())
}

#[test]
fn import_plain_text_multi_line_inserts_block_boundaries() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup()?;

    document_io_controller::import_plain_text(
        &db_context,
        &event_hub,
        &ImportPlainTextDto {
            plain_text: "first\nsecond\nthird".to_string(),
        },
    )?;

    let store = db_context.get_store();
    let rope = store.rope.read();
    // Three blocks joined by inter-block newlines.
    assert_eq!(rope.to_string(), "first\nsecond\nthird");

    let offsets = store.block_offsets.read();
    assert_eq!(offsets.entries.len(), 3);
    // byte_starts: 0, then after "first\n" = 6, then after "second\n" = 13
    assert_eq!(offsets.entries[0].1, 0);
    assert_eq!(offsets.entries[1].1, 6);
    assert_eq!(offsets.entries[2].1, 13);
    assert_eq!(offsets.total_bytes(), 18);

    // Verify range_of works: block 0 covers [0..6), block 1 [6..13), block 2 [13..18).
    let (b0, b1, b2) = (
        offsets.entries[0].0,
        offsets.entries[1].0,
        offsets.entries[2].0,
    );
    assert_eq!(offsets.range_of(b0), Some((0, 6)));
    assert_eq!(offsets.range_of(b1), Some((6, 13)));
    assert_eq!(offsets.range_of(b2), Some((13, 18)));
    // The first entries should all be real blocks (no table anchors).
    assert!(b0.is_block() && b1.is_block() && b2.is_block());

    Ok(())
}

#[test]
fn second_import_resets_rope() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup()?;

    document_io_controller::import_plain_text(
        &db_context,
        &event_hub,
        &ImportPlainTextDto {
            plain_text: "first import".to_string(),
        },
    )?;
    document_io_controller::import_plain_text(
        &db_context,
        &event_hub,
        &ImportPlainTextDto {
            plain_text: "second import".to_string(),
        },
    )?;

    let store = db_context.get_store();
    let rope = store.rope.read();
    assert_eq!(rope.to_string(), "second import");

    let offsets = store.block_offsets.read();
    assert_eq!(offsets.entries.len(), 1);
    assert_eq!(offsets.total_bytes(), 13);

    Ok(())
}

#[test]
fn import_unicode_text_uses_byte_offsets() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup()?;

    // 4 chars, 6 bytes ("café" — é is 2 bytes in UTF-8).
    document_io_controller::import_plain_text(
        &db_context,
        &event_hub,
        &ImportPlainTextDto {
            plain_text: "café\nbar".to_string(),
        },
    )?;

    let store = db_context.get_store();
    let rope = store.rope.read();
    assert_eq!(rope.to_string(), "café\nbar");
    assert_eq!(rope.len_bytes(), 9); // "café"=5 + "\n"=1 + "bar"=3
    assert_eq!(rope.len_chars(), 8); // 4 + 1 + 3

    let offsets = store.block_offsets.read();
    assert_eq!(offsets.entries.len(), 2);
    assert_eq!(offsets.entries[0].1, 0);
    // Second block starts after "café\n" = 5 + 1 = 6 BYTES (not chars).
    assert_eq!(offsets.entries[1].1, 6);
    assert_eq!(offsets.total_bytes(), 9);

    Ok(())
}

/// Plan §1.6 maintenance: after `import_plain_text`, the root
/// frame's `byte_range` must cover the entire imported content.
#[test]
fn import_plain_text_sets_root_frame_byte_range() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup()?;

    document_io_controller::import_plain_text(
        &db_context,
        &event_hub,
        &ImportPlainTextDto {
            plain_text: "first\nsecond\nthird".to_string(),
        },
    )?;

    let store = db_context.get_store();
    let total = store.rope.read().len_bytes() as u32;
    assert_eq!(total, 18);

    // Exactly one top-level frame; find it.
    let frames = store.frames.read();
    let top_level: Vec<_> = frames
        .values()
        .filter(|f| f.parent_frame.is_none())
        .collect();
    assert_eq!(top_level.len(), 1);
    assert_eq!(top_level[0].byte_range, (0, 18));

    Ok(())
}

/// After import, the root frame's byte_range covers the entire
/// rope including any trailing inter-block boundaries (since
/// BlockOffsetIndex.range_of extends each entry to the next).
#[test]
fn import_plain_text_byte_range_matches_total_bytes() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup()?;

    document_io_controller::import_plain_text(
        &db_context,
        &event_hub,
        &ImportPlainTextDto {
            plain_text: "a\nb\nc".to_string(),
        },
    )?;

    let store = db_context.get_store();
    let total = store.rope.read().len_bytes() as u32;
    let frames = store.frames.read();
    let top = frames
        .values()
        .find(|f| f.parent_frame.is_none())
        .expect("top-level frame");
    // Three blocks at byte_starts 0, 2, 4; range_of(last) extends to
    // total_bytes (5). Root frame covers (0, 5).
    assert_eq!(top.byte_range, (0, total));

    Ok(())
}

/// Re-import must reset the byte_range to cover the new content
/// (not the union of old + new).
#[test]
fn second_import_resets_root_frame_byte_range() -> Result<()> {
    let (db_context, event_hub, _undo_redo_manager) = setup()?;

    document_io_controller::import_plain_text(
        &db_context,
        &event_hub,
        &ImportPlainTextDto {
            plain_text: "first import that is longer".to_string(),
        },
    )?;
    document_io_controller::import_plain_text(
        &db_context,
        &event_hub,
        &ImportPlainTextDto {
            plain_text: "short".to_string(),
        },
    )?;

    let store = db_context.get_store();
    let frames = store.frames.read();
    let top_level: Vec<_> = frames
        .values()
        .filter(|f| f.parent_frame.is_none())
        .collect();
    assert_eq!(top_level.len(), 1);
    assert_eq!(top_level[0].byte_range, (0, 5));

    Ok(())
}
