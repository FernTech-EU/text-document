// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Snapshotting a document's flow must cost time in proportion to its size.
//!
//! An editor lays a document out from `snapshot_flow`, when the document opens and again
//! after edits. A block's list number, its table cell, what its footnote references print
//! and its default language are facts about the whole document, and the snapshot used to
//! work each of them out again for every block that needed it, a walk of the document per
//! paragraph: opening an 8,000-paragraph text took 0.3 s with one list in it, 2.4 s with a
//! small table every few pages, and a minute with a footnote on every paragraph.
//!
//! The unit test `a_flow_snapshot_reads_each_whole_document_fact_a_fixed_number_of_times`
//! (in `text_block.rs`) counts those reads exactly, in every build. This guard times the
//! snapshot itself, so it also catches a per-block walk that does not go through them.
//!
//! It never compares raw times. Each snapshot is divided by loading the same text with
//! `set_djot_sync`, timed right beside it (same core, clock and cache); that load is linear
//! and its own work is counted by `document_io`'s `import_scaling_tests`. A linear snapshot
//! stays a fixed multiple of the load as the text grows eight times; a snapshot with a
//! per-paragraph walk left in it grows with the text, about eight times over. Each quotient
//! is the median of several rounds, so a round a preemption lands in cannot move it.
//! Absolute times are printed for information only.

use std::collections::HashMap;
use std::time::{Duration, Instant};
use text_document::{FlowElementSnapshot, FlowSnapshot, FragmentContent, TextDocument};

const PARA: &str = "The rain had not stopped since morning, and she walked along the quay \
                    counting the boats that would never leave the harbour again.";

/// What a novel kept in one document holds besides its prose.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// A short list every few pages.
    Lists,
    /// A small table every few pages.
    Tables,
    /// A footnote every few paragraphs, numbered by the document itself.
    Notes,
    /// The same, numbered by the host, as Skribisto does.
    HostNumberedNotes,
}

fn djot(shape: Shape, paragraphs: usize) -> String {
    let mut text = String::new();
    for i in 0..paragraphs {
        match shape {
            Shape::Notes | Shape::HostNumberedNotes if i % 7 == 3 => {
                text.push_str(&format!("{PARA} ({i})[^n{i}]\n\n[^n{i}]: Note {i}.\n\n"));
            }
            _ => text.push_str(&format!("{PARA} ({i})\n\n")),
        }
        match shape {
            Shape::Lists if i % 11 == 5 => {
                text.push_str(&format!("- item {i}\n- item {i}, second\n\n"));
            }
            Shape::Tables if i % 11 == 5 => {
                text.push_str(&format!("| a{i} | b |\n| c | d |\n\n"));
            }
            _ => {}
        }
    }
    text
}

/// The markers a host numbering its notes itself would push.
fn host_markers(paragraphs: usize) -> HashMap<String, String> {
    (0..paragraphs)
        .filter(|i| i % 7 == 3)
        .enumerate()
        .map(|(k, i)| (format!("n{i}"), (k + 1).to_string()))
        .collect()
}

/// Load the text into a fresh document: the linear reference.
fn time_load(text: &str) -> Duration {
    let doc = TextDocument::new();
    let start = Instant::now();
    doc.set_djot_sync(text).unwrap();
    start.elapsed()
}

/// Snapshot the flow of a document holding the text.
fn time_snapshot(shape: Shape, paragraphs: usize, text: &str) -> Duration {
    let doc = TextDocument::new();
    doc.set_djot_sync(text).unwrap();
    if let Shape::HostNumberedNotes = shape {
        doc.set_footnote_markers(host_markers(paragraphs));
    }
    let start = Instant::now();
    let flow = doc.snapshot_flow();
    let elapsed = start.elapsed();
    assert!(
        flow.elements.len() >= paragraphs,
        "the snapshot covers the whole text"
    );
    check_shape(shape, &flow);
    elapsed
}

/// The text holds what its shape says and nothing else, so each shape times one kind of
/// whole-document fact.
fn check_shape(shape: Shape, flow: &FlowSnapshot) {
    let blocks = || {
        flow.elements.iter().filter_map(|element| match element {
            FlowElementSnapshot::Block(block) => Some(block),
            _ => None,
        })
    };
    let list_items = blocks().filter(|b| b.list_info.is_some()).count();
    let tables = flow
        .elements
        .iter()
        .filter(|e| matches!(e, FlowElementSnapshot::Table(_)))
        .count();
    let notes = blocks()
        .flat_map(|b| b.fragments.iter())
        .filter(|f| matches!(f, FragmentContent::FootnoteReference { .. }))
        .count();
    let (want_lists, want_tables, want_notes) = match shape {
        Shape::Lists => (true, false, false),
        Shape::Tables => (false, true, false),
        Shape::Notes | Shape::HostNumberedNotes => (false, false, true),
    };
    assert_eq!(
        (list_items > 0, tables > 0, notes > 0),
        (want_lists, want_tables, want_notes),
        "{shape:?}: {list_items} list items, {tables} tables, {notes} note references"
    );
}

/// The snapshot's time over the load's, median over `rounds`, the two timed back to back
/// and in alternating order.
fn relative_cost(shape: Shape, paragraphs: usize, rounds: usize) -> (f64, Duration, Duration) {
    let text = djot(shape, paragraphs);
    let _ = (time_snapshot(shape, paragraphs, &text), time_load(&text));
    let mut rows: Vec<(f64, Duration, Duration)> = (0..rounds)
        .map(|round| {
            let (snapshot, load) = if round % 2 == 0 {
                let load = time_load(&text);
                (time_snapshot(shape, paragraphs, &text), load)
            } else {
                let snapshot = time_snapshot(shape, paragraphs, &text);
                (snapshot, time_load(&text))
            };
            (
                snapshot.as_nanos() as f64 / load.as_nanos().max(1) as f64,
                snapshot,
                load,
            )
        })
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows[rounds / 2]
}

/// How much more a snapshot costs relative to loading the text at `large` than at `small`
/// paragraphs. Prints both quotients.
fn growth(shape: Shape, small: usize, large: usize) -> f64 {
    const ROUNDS: usize = 5;
    let (at_small, small_snapshot, small_load) = relative_cost(shape, small, ROUNDS);
    let (at_large, large_snapshot, large_load) = relative_cost(shape, large, ROUNDS);
    let growth = at_large / at_small;
    println!(
        "{shape:?} snapshot over load: {at_small:.2}x at {small} paragraphs \
         ({small_snapshot:?} / {small_load:?}), {at_large:.2}x at {large} \
         ({large_snapshot:?} / {large_load:?}), growth {growth:.2}"
    );
    growth
}

#[test]
fn a_flow_snapshot_scales_linearly_with_lists_tables_and_notes() {
    let mut failures = Vec::new();
    for shape in [
        Shape::Lists,
        Shape::Tables,
        Shape::Notes,
        Shape::HostNumberedNotes,
    ] {
        let growth = growth(shape, 250, 2_000);
        if growth >= 3.0 {
            failures.push(format!("{shape:?} grew {growth:.2} times"));
        }
    }
    assert!(
        failures.is_empty(),
        "a flow snapshot grew relative to loading the same text, for eight times the text: \
         {}. A linear snapshot stays a fixed multiple of the load; growing with the text \
         means a whole-document fact (a list number, a table cell, a footnote number or \
         marker, the default language) is worked out again for each block that needs it.",
        failures.join(", ")
    );
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing guard for long texts, meaningful in release builds only: cargo test --release"
)]
fn a_flow_snapshot_of_a_long_text_with_host_numbered_notes_scales_linearly() {
    // A host that numbers its notes itself, as Skribisto does, pushes a marker for every
    // note, and the snapshot used to copy that whole map for each paragraph holding a
    // note, and the document's list of frames (one per note) for every paragraph. Each
    // copy is cheap, so the walk hides under the linear work at the sizes the test above
    // uses, and shows only in long texts: this guard compares 2,000 with 32,000
    // paragraphs, in release builds only (`cargo test --release`), which CI does not run.
    // The unit test `a_flow_snapshot_reads_each_whole_document_fact_a_fixed_number_of_times`
    // counts those copies exactly in every build.
    let growth = growth(Shape::HostNumberedNotes, 2_000, 32_000);
    assert!(
        growth < 3.0,
        "a flow snapshot of a text whose host numbers its notes grew {growth:.2} times \
         relative to loading it, for sixteen times the text: the host's markers or the \
         document's frame list are being copied again for each paragraph."
    );
}
