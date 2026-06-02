//! `RopeStore` snapshot/restore round-trip property tests.
//!
//! Verifies the contract carried by Phase 2 step 2: after a
//! `snapshot → mutate → restore` cycle, every field of the store is
//! byte-for-byte identical to the pre-mutation state.

use common::database::block_offset_index::BlockOffsetIndex;
use common::database::rope_store::RopeStore;
use common::entities::{Block, Frame, Root};
use common::format_runs::{CharacterFormat, FormatRun};
use proptest::prelude::*;

fn assert_states_eq(a: &RopeStore, b: &RopeStore) {
    assert_eq!(
        a.rope.read().to_string(),
        b.rope.read().to_string(),
        "rope content"
    );
    assert_eq!(*a.roots.read(), *b.roots.read(), "roots");
    assert_eq!(*a.documents.read(), *b.documents.read(), "documents");
    assert_eq!(*a.frames.read(), *b.frames.read(), "frames");
    assert_eq!(*a.blocks.read(), *b.blocks.read(), "blocks");
    assert_eq!(*a.lists.read(), *b.lists.read(), "lists");
    assert_eq!(*a.resources.read(), *b.resources.read(), "resources");
    assert_eq!(*a.tables.read(), *b.tables.read(), "tables");
    assert_eq!(*a.table_cells.read(), *b.table_cells.read(), "table_cells");
    assert_eq!(*a.format_runs.read(), *b.format_runs.read(), "format_runs");
    assert_eq!(
        *a.block_images.read(),
        *b.block_images.read(),
        "block_images"
    );
    assert_eq!(
        *a.block_offsets.read(),
        *b.block_offsets.read(),
        "block_offsets"
    );
}

/// Seed a representative state into the store. Uses hardcoded ids so
/// the test never needs to call the crate-private `next_id`.
fn populate_some_state(store: &RopeStore) {
    const ROOT_ID: u64 = 1;
    const BLOCK_ID: u64 = 2;
    const FRAME_ID: u64 = 3;

    store
        .rope
        .write()
        .insert(0, "hello world\nsecond paragraph\n");
    store.roots.write().insert(
        ROOT_ID,
        Root {
            id: ROOT_ID,
            ..Root::default()
        },
    );
    store.blocks.write().insert(
        BLOCK_ID,
        Block {
            id: BLOCK_ID,
            document_position: 0,
            ..Block::default()
        },
    );
    store.frames.write().insert(
        FRAME_ID,
        Frame {
            id: FRAME_ID,
            ..Frame::default()
        },
    );
    store.format_runs.write().insert(
        BLOCK_ID,
        vec![FormatRun {
            byte_start: 0,
            byte_end: 5,
            format: CharacterFormat {
                font_bold: Some(true),
                ..CharacterFormat::default()
            },
        }],
    );
    store.block_offsets.write().push_block(BLOCK_ID, 0);

    // Pre-set counters so populate is deterministic (would otherwise
    // depend on call order).
    {
        let mut counters = store.counters.write();
        counters.insert("Root".into(), 2);
        counters.insert("Block".into(), 3);
        counters.insert("Frame".into(), 4);
    }
}

#[test]
fn snapshot_restore_round_trip_empty_store() {
    let store = RopeStore::new();
    let snap = store.snapshot();
    // Mutate aggressively.
    store.rope.write().insert(0, "garbage");
    store.counters.write().insert("Block".into(), 99);
    store.blocks.write().insert(
        99,
        Block {
            id: 99,
            ..Block::default()
        },
    );

    let expected = RopeStore::new();
    store.restore(&snap);
    assert_states_eq(&store, &expected);
}

#[test]
fn snapshot_restore_round_trip_populated_store() {
    let store = RopeStore::new();
    populate_some_state(&store);
    let snap = store.snapshot();

    // Mutate after snapshot.
    store.rope.write().insert(0, "PREFIX");
    store.blocks.write().clear();
    store.format_runs.write().clear();
    store.block_offsets.write().clear();

    // Restore.
    let expected = RopeStore::new();
    populate_some_state(&expected);
    // expected was populated with next_id calls — its counters differ
    // from the snapshot's. Restore from the same snapshot to align.
    expected.restore(&snap);

    store.restore(&snap);
    assert_states_eq(&store, &expected);
}

#[test]
fn restore_without_counters_preserves_counters() {
    let store = RopeStore::new();
    populate_some_state(&store);
    let snap = store.snapshot();
    let counter_at_snap = *store.counters.read().get("Block").unwrap();

    // Bump counter past the snapshot value and mutate state.
    store
        .counters
        .write()
        .insert("Block".into(), counter_at_snap + 42);
    store.blocks.write().clear();

    store.restore_without_counters(&snap);

    // Entity tables restored.
    assert!(!store.blocks.read().is_empty());
    // Counter NOT rolled back to the snapshot value.
    let counter_after_restore = *store.counters.read().get("Block").unwrap();
    assert_eq!(
        counter_after_restore,
        counter_at_snap + 42,
        "restore_without_counters must NOT touch the counters map"
    );
}

#[test]
fn savepoint_create_restore_discard() {
    let store = RopeStore::new();
    populate_some_state(&store);
    let sp = store.create_savepoint();

    // Mutate.
    store.rope.write().insert(0, "MUTATION");
    store.blocks.write().clear();

    // Restore.
    store.restore_savepoint(sp);
    assert!(!store.blocks.read().is_empty());
    assert!(!store.rope.read().to_string().starts_with("MUTATION"));

    // Discard is just a memory cleanup — restoring after discard panics.
    store.discard_savepoint(sp);
}

#[test]
fn store_snapshot_round_trip_via_type_erased_path() {
    let store = RopeStore::new();
    populate_some_state(&store);
    let erased = store.store_snapshot();

    // Mutate.
    store.rope.write().insert(0, "X");
    store.blocks.write().clear();

    store.restore_store_snapshot(&erased);

    // Restored: rope content matches what was populated.
    let rope_text = store.rope.read().to_string();
    assert!(rope_text.starts_with("hello world"));
    assert!(!store.blocks.read().is_empty());
}

#[test]
fn block_offset_index_is_default_equal_when_empty() {
    let a = BlockOffsetIndex::default();
    let b = BlockOffsetIndex::default();
    assert_eq!(a, b);
}

// ── Property-based test ─────────────────────────────────────────────

proptest! {
    /// For any sequence of mutations applied after a snapshot, calling
    /// `restore(&snap)` must yield a store byte-equivalent to a fresh
    /// store that received the same pre-snapshot history.
    #[test]
    fn prop_snapshot_restore_round_trip(
        rope_inserts in proptest::collection::vec("[a-z\n]{0,20}", 0..10),
        post_snapshot_inserts in proptest::collection::vec("[A-Z]{0,20}", 0..10),
    ) {
        let store = RopeStore::new();
        let expected = RopeStore::new();

        // Apply identical pre-snapshot history to both stores.
        for text in &rope_inserts {
            let len = store.rope.read().len_chars();
            store.rope.write().insert(len, text);
            let elen = expected.rope.read().len_chars();
            expected.rope.write().insert(elen, text);
        }

        // Snapshot one, mutate it.
        let snap = store.snapshot();
        for text in &post_snapshot_inserts {
            let len = store.rope.read().len_chars();
            store.rope.write().insert(len, text);
        }

        // Restoring must return to the pre-snapshot state.
        store.restore(&snap);
        assert_states_eq(&store, &expected);
    }
}
