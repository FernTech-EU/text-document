// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Regression guard for finding which frames own a set of blocks.
//!
//! Replacing a document's content removes its frames, and the removal hands every one of
//! their block ids to a single lookup: which frames name any of these blocks? The lookup
//! used to ask each frame's list about every id in turn, frames times ids, and a document
//! made of many one-block frames (footnotes, table cells, quotations) is exactly the case
//! where both are large. Reloading such a document in place, which a host does whenever
//! it re-reads an open text, was quadratic in its notes. The lookup now puts the ids in a
//! set first.
//!
//! This guard times the lookup. It runs in release builds only (`cargo test --release`),
//! where the clock sees the lookup rather than debug assertions and unoptimised iterators,
//! and CI runs no release tests, so CI never runs it. The guard CI does run is the unit test
//! `finding_owners_probes_each_listed_id_once` beside the lookup in `frame_table.rs`: it
//! counts the lookup's membership probes, which only the crate's own unit tests can see.
//!
//! A plain ratio of two sizes is not a stable measure: eight times the frames no longer
//! fit the same cache, and linear work alone measured anywhere from 8 to 30 times slower
//! across that step. So each lookup is divided by a reference pass with the same memory
//! traffic (walk every frame, copy its block list), timed right beside it on the same
//! core, clock and cache, and the guard compares that quotient at 1,000 and 64,000
//! frames. A linear lookup stays a fixed multiple of the walk: over twenty runs on a
//! loaded machine its quotient grew by 1.06 to 1.22, and the old lookup's, over eight, by
//! 7.2 to 11. The bound, 3, sits more than twice away from both, and each quotient is the
//! median of eleven rounds, so a preempted round cannot move it.

use common::database::db_context::DbContext;
use common::database::transactions::Transaction;
use common::direct_access::frame::FrameRelationshipField;
use common::direct_access::repository_factory;
use common::entities::{Block, Frame};
use common::types::EntityId;
use std::hint::black_box;
use std::time::{Duration, Instant};

/// A store of `frames` frames owning one block each, and the ids of those blocks.
fn one_block_frames(frames: u64) -> (DbContext, Vec<EntityId>) {
    let db = DbContext::new().expect("db context");
    let mut block_ids = Vec::with_capacity(frames as usize);
    {
        let store = db.get_store();
        let mut frame_map = store.frames.write();
        let mut block_map = store.blocks.write();
        for id in 1..=frames {
            block_map.insert(
                id,
                Block {
                    id,
                    ..Block::default()
                },
            );
            frame_map.insert(
                id,
                Frame {
                    id,
                    blocks: vec![id],
                    child_order: vec![id as i64],
                    ..Frame::default()
                },
            );
            block_ids.push(id);
        }
    }
    (db, block_ids)
}

/// The lookup's own memory traffic without its membership test: every frame, with a copy
/// of its block list. Linear by construction.
fn every_frame_with_its_blocks(db: &DbContext) -> Vec<(EntityId, Vec<EntityId>)> {
    let frames = db.get_store().frames.read();
    frames
        .iter()
        .map(|(id, frame)| (*id, frame.blocks.clone()))
        .collect()
}

fn timed<T>(pass: impl FnOnce() -> T) -> Duration {
    let start = Instant::now();
    black_box(pass());
    start.elapsed()
}

/// The lookup's time over the reference pass's, median over `rounds`.
///
/// Each round times the two back to back, so they run on the same core, at the same clock
/// and against the same cache: whatever those do to one they do to the other, and the
/// quotient keeps only what the lookup does beyond walking the frames. The median then
/// discards the rounds a preemption landed in.
fn relative_cost(frames: u64, rounds: usize) -> f64 {
    let (db, block_ids) = one_block_frames(frames);
    // The write repository, which is the one the removal cascade asks.
    let transaction = Transaction::begin_write_transaction(&db).expect("transaction");
    let repository =
        repository_factory::write::create_frame_repository(&transaction).expect("repository");
    let owners_of_every_block = || {
        let owners = repository
            .get_relationships_from_right_ids(
                &FrameRelationshipField::Blocks,
                black_box(&block_ids),
            )
            .expect("lookup");
        assert_eq!(owners.len(), block_ids.len(), "every frame owns a block");
        owners
    };

    black_box(owners_of_every_block());
    black_box(every_frame_with_its_blocks(&db));
    let mut quotients: Vec<f64> = (0..rounds)
        .map(|round| {
            let (lookup, reference) = if round % 2 == 0 {
                let reference = timed(|| every_frame_with_its_blocks(&db));
                (timed(owners_of_every_block), reference)
            } else {
                let lookup = timed(owners_of_every_block);
                (lookup, timed(|| every_frame_with_its_blocks(&db)))
            };
            lookup.as_nanos() as f64 / reference.as_nanos().max(1) as f64
        })
        .collect();
    quotients.sort_by(f64::total_cmp);
    quotients[rounds / 2]
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing guard, meaningful in release builds only: cargo test --release"
)]
fn finding_the_owners_of_every_block_scales_linearly_with_one_block_frames() {
    const SMALL: u64 = 1_000;
    const LARGE: u64 = 64 * SMALL;
    const ROUNDS: usize = 11;

    let small = relative_cost(SMALL, ROUNDS);
    let large = relative_cost(LARGE, ROUNDS);
    let growth = large / small;
    println!(
        "owner lookup over a plain walk of the frames: {small:.2}x at {SMALL} frames, \
         {large:.2}x at {LARGE}, growth {growth:.2}"
    );
    assert!(
        growth < 3.0,
        "finding the owners of every block among one-block frames cost {small:.2} times a \
         plain walk of the frames at {SMALL} frames and {large:.2} times at {LARGE}: it grew \
         {growth:.2} times with the input. A linear lookup stays a fixed multiple of the \
         walk; growing with the input means each frame is being checked against every id \
         again instead of against a set of them."
    );
}
