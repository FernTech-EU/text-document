// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! The run helpers must leave exactly the rope and offset index their per-block loops leave.
//!
//! `rope_split_block_into` stands in for `rope_split_block` + `rope_insert_in_block` once
//! per new block, and `rope_insert_blocks_at` for `rope_insert_block_at` once per new block.
//! The loops walk every marker in the index once per block, which made pasting or restoring
//! a document of N paragraphs cost N walks of N markers; the run helpers do one rope insert
//! and one index update. `rope_clear_blocks` likewise stands in for
//! `rope_replace_block_content(.., "")` once per block, which emptying the cells of many
//! tables ran once per cell. These properties replay both on the same store and compare the
//! rope text and the whole index, marker positions and table-anchor count included.

use common::database::Store;
use common::database::block_offset_index::{BlockOffsetIndex, OffsetMarker};
use common::database::rope_helpers::{
    block_content_via_store, rope_append_block, rope_append_table_anchor, rope_clear_blocks,
    rope_insert_block_at, rope_insert_block_boundary, rope_insert_blocks_at, rope_insert_in_block,
    rope_replace_block_content, rope_split_block, rope_split_block_into,
};
use common::entities::Block;
use common::types::EntityId;
use proptest::prelude::*;

/// What a store is built from: each existing entry is a block with this text, or (`None`)
/// a table anchor.
type Layout = Vec<Option<String>>;

/// Existing block ids start here; new blocks are numbered after them.
const FIRST_ID: EntityId = 1;
const FIRST_NEW_ID: EntityId = 10_000;

/// Short texts with multi-byte characters, and empty ones, which the loops treat specially.
fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(vec!["a", "bc", "é", "日本", " ", "\u{202F}"]),
        0..4,
    )
    .prop_map(|parts| parts.concat())
}

fn layout() -> impl Strategy<Value = Layout> {
    prop::collection::vec(
        prop_oneof![4 => text().prop_map(Some), 1 => Just(None)],
        1..7,
    )
    // Blocks carry the positions the tests split and insert at: keep at least one.
    .prop_filter("at least one block", |entries| {
        entries.iter().any(Option::is_some)
    })
}

fn new_blocks() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(text(), 0..6)
}

/// A store holding `layout` the way the importers lay a document out: entries separated by
/// a `\n` boundary. Returns the ids of its blocks, in order.
fn build(layout: &Layout) -> (Store, Vec<EntityId>) {
    let store = Store::new();
    let mut block_ids = Vec::new();
    for (i, entry) in layout.iter().enumerate() {
        let id = FIRST_ID + i as EntityId;
        match entry {
            Some(text) => {
                if i > 0 {
                    rope_insert_block_boundary(&store);
                }
                rope_append_block(&store, id, text);
                block_ids.push(id);
            }
            None => rope_append_table_anchor(&store, id, i > 0),
        }
    }
    (store, block_ids)
}

fn state(store: &Store) -> (String, BlockOffsetIndex) {
    (
        store.rope.read().to_string(),
        store.block_offsets.read().clone(),
    )
}

fn content(store: &Store, block_id: EntityId) -> String {
    block_content_via_store(
        &Block {
            id: block_id,
            ..Block::default()
        },
        store,
    )
}

/// The char boundaries of `text`, as byte offsets, end included.
fn boundaries(text: &str) -> Vec<u32> {
    text.char_indices()
        .map(|(i, _)| i as u32)
        .chain(std::iter::once(text.len() as u32))
        .collect()
}

fn with_ids(texts: &[String]) -> Vec<(EntityId, &str)> {
    texts
        .iter()
        .enumerate()
        .map(|(i, text)| (FIRST_NEW_ID + i as EntityId, text.as_str()))
        .collect()
}

proptest! {
    #[test]
    fn splitting_into_a_run_matches_splitting_block_by_block(
        layout in layout(),
        pick_block in any::<prop::sample::Index>(),
        pick_offset in any::<prop::sample::Index>(),
        texts in new_blocks(),
    ) {
        let (by_block, block_ids) = build(&layout);
        let (as_run, _) = build(&layout);
        let current = *pick_block.get(&block_ids);
        let offset = *pick_offset.get(&boundaries(&content(&by_block, current)));
        let blocks = with_ids(&texts);

        let mut previous = current;
        let mut split_at = offset;
        for (id, text) in &blocks {
            rope_split_block(&by_block, previous, split_at, *id);
            rope_insert_in_block(&by_block, *id, 0, text);
            previous = *id;
            split_at = text.len() as u32;
        }
        rope_split_block_into(&as_run, current, offset, &blocks);

        prop_assert_eq!(state(&as_run), state(&by_block));
    }

    #[test]
    fn inserting_a_run_matches_inserting_block_by_block(
        layout in layout(),
        pick_block in any::<prop::sample::Index>(),
        at_rope_end in any::<bool>(),
        texts in new_blocks(),
    ) {
        let (by_block, block_ids) = build(&layout);
        let (as_run, _) = build(&layout);
        // Where the importers insert: right after a block's content, or at the rope's end.
        let start = if at_rope_end {
            by_block.block_offsets.read().total_bytes()
        } else {
            let block = *pick_block.get(&block_ids);
            let (block_start, _) = by_block
                .block_offsets
                .read()
                .range_of(OffsetMarker::Block(block))
                .expect("a block of the layout is indexed");
            block_start + content(&by_block, block).len() as u32
        };
        let blocks = with_ids(&texts);

        let mut next = start;
        for (id, text) in &blocks {
            rope_insert_block_at(&by_block, next, *id, text);
            next += 1 + text.len() as u32;
        }
        let returned = rope_insert_blocks_at(&as_run, start, &blocks);

        prop_assert_eq!(returned, next, "the byte after the run");
        prop_assert_eq!(state(&as_run), state(&by_block));
    }

    #[test]
    fn clearing_blocks_together_matches_clearing_them_one_by_one(
        layout in layout(),
        picks in prop::collection::vec(any::<prop::sample::Index>(), 0..8),
        in_index_order in any::<bool>(),
        with_a_stranger in any::<bool>(),
    ) {
        let (one_by_one, block_ids) = build(&layout);
        let (together, _) = build(&layout);
        // Any blocks, repeats included; in the order a deletion meets them (index order,
        // one pass) or in any order (block by block).
        let mut cleared: Vec<EntityId> = picks.iter().map(|pick| *pick.get(&block_ids)).collect();
        if in_index_order {
            cleared.sort_unstable();
            cleared.dedup();
        }
        if with_a_stranger {
            // A block the index does not hold, which both leave alone.
            cleared.insert(cleared.len() / 2, FIRST_NEW_ID);
        }

        for &id in &cleared {
            rope_replace_block_content(&one_by_one, id, "");
        }
        rope_clear_blocks(&together, &cleared);

        prop_assert_eq!(state(&together), state(&one_by_one));
    }
}

#[test]
fn clearing_blocks_empties_each_and_keeps_its_boundaries() {
    let (store, _) = build(&vec![
        Some("one".to_string()),
        None,
        Some("two".to_string()),
        Some("".to_string()),
        Some("three".to_string()),
    ]);
    rope_clear_blocks(&store, &[FIRST_ID, FIRST_ID + 2, FIRST_ID + 4]);

    assert_eq!(store.rope.read().to_string(), "\n\u{FFFC}\n\n\n");
    for id in [FIRST_ID, FIRST_ID + 2, FIRST_ID + 3, FIRST_ID + 4] {
        assert_eq!(content(&store, id), "", "block {id} is empty");
    }
}

/// Out of index order, one clear can change what a later one measures: once the last
/// block is empty, the boundary before it counts as the content of the block before. The
/// batch must then replay the clears one at a time rather than measure them all first.
#[test]
fn clearing_blocks_out_of_index_order_replays_them_one_at_a_time() {
    let layout = vec![Some("ab".to_string()), Some("cd".to_string())];
    let (one_by_one, _) = build(&layout);
    let (together, _) = build(&layout);
    for id in [FIRST_ID + 1, FIRST_ID] {
        rope_replace_block_content(&one_by_one, id, "");
    }
    rope_clear_blocks(&together, &[FIRST_ID + 1, FIRST_ID]);

    assert_eq!(state(&together), state(&one_by_one));
    assert_eq!(
        together.rope.read().to_string(),
        "",
        "the boundary went with the first block, as it does one clear at a time"
    );
}

#[test]
fn a_split_in_the_middle_of_a_block_carries_its_rest_past_the_run() {
    let (store, _) = build(&vec![
        Some("head|tail".to_string()),
        Some("next".to_string()),
    ]);
    rope_split_block_into(
        &store,
        FIRST_ID,
        5,
        &[(FIRST_NEW_ID, "one"), (FIRST_NEW_ID + 1, "")],
    );

    assert_eq!(store.rope.read().to_string(), "head|\none\ntail\nnext");
    assert_eq!(content(&store, FIRST_ID), "head|");
    assert_eq!(content(&store, FIRST_NEW_ID), "one");
    assert_eq!(content(&store, FIRST_NEW_ID + 1), "tail");
    assert_eq!(content(&store, FIRST_ID + 1), "next");
}

#[test]
fn a_block_missing_from_the_index_is_left_alone() {
    let (store, _) = build(&vec![Some("only".to_string())]);
    let before = state(&store);
    rope_split_block_into(&store, 999, 0, &[(FIRST_NEW_ID, "new")]);
    assert_eq!(state(&store), before);
}
