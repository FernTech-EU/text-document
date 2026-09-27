// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Replacing a whole document through a cursor must cost time in proportion to its size.
//!
//! A host restores a past version of a text by selecting all of it and inserting the old
//! content as Djot, one undoable edit. That deletes every paragraph and inserts every one
//! again, and each half had per-paragraph work that grew with the document: owner lists
//! rewritten once per paragraph, the rope's offset index walked once per inserted
//! paragraph, and the deletion's position refresh searching the whole block list for every
//! paragraph. Restoring a version of an 8,000-paragraph chapter took seconds, where loading
//! the same text took milliseconds.
//!
//! `document_editing`'s `whole_document_scaling_tests` count the owner-list writes exactly.
//! The rope, its index and the refresh loop emit nothing to count, so this guard times the
//! whole edit instead, and runs in every build, the debug one CI tests with included.
//!
//! It never compares raw times. Each restore is divided by loading the same text with
//! `set_djot_sync`, timed right beside it (same core, clock and cache); that load is linear
//! and its own work is counted by `document_io`'s `import_scaling_tests`. A linear restore
//! stays a fixed multiple of the load as the text grows eight times; a restore with a
//! per-paragraph walk left in it grows with the text, eight times over for a full return to
//! the old behaviour. Each quotient is the median of several rounds, so a round a
//! preemption lands in cannot move it. Absolute times are printed for information only.
//!
//! Two of those walks are cheap per step, and at the sizes a debug run can afford they hide
//! under the linear work: with only the rope's per-block index update put back, the debug
//! guard measured a growth of 1.28, and with only the refresh loop's search put back, 1.05.
//! They cost seconds only in long texts (the rope's walk alone, 1.7 s to paste 16,000
//! paragraphs into a short document, against 0.15 s without it). The second guard therefore
//! runs in release builds only (`cargo test --release`), which CI does not do, at sizes where
//! each walk dominates: pasting a whole text into a short document, and one Backspace at the
//! end of a long one, 2,000 against 32,000 paragraphs, each over loading the same text. A
//! third, also release-only, deletes a whole text holding a small table every few pages,
//! over loading that text.

use std::time::{Duration, Instant};
use text_document::{SelectionType, TextDocument};

const PARA: &str = "The rain had not stopped since morning, and she walked along the quay \
                    counting the boats that would never leave the harbour again.";

fn djot(paragraphs: usize, tag: &str) -> String {
    (0..paragraphs)
        .map(|i| format!("{tag} {i}. {PARA}\n\n"))
        .collect()
}

/// Select all of a document of `paragraphs` paragraphs and insert as many others over it,
/// as one edit block: what a version restore does.
fn time_restore(paragraphs: usize) -> Duration {
    let doc = TextDocument::new();
    doc.set_djot_sync(&djot(paragraphs, "Current")).unwrap();
    let past = djot(paragraphs, "Past");
    let start = Instant::now();
    let cursor = doc.cursor();
    cursor.begin_edit_block();
    cursor.select(SelectionType::Document);
    cursor.insert_djot(&past).unwrap();
    cursor.end_edit_block();
    let elapsed = start.elapsed();
    assert_eq!(
        doc.block_count(),
        paragraphs,
        "the past text replaced the current one"
    );
    elapsed
}

/// Paste a whole text into a one-line document, as a split or a restore over a short text
/// does: the insertion alone, where the rope's per-block walk showed.
fn time_paste_into_a_short_document(paragraphs: usize) -> Duration {
    let doc = TextDocument::new();
    doc.set_djot_sync("Current text.\n").unwrap();
    let past = djot(paragraphs, "Past");
    let start = Instant::now();
    let cursor = doc.cursor();
    cursor.select(SelectionType::Document);
    cursor.insert_djot(&past).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(doc.block_count(), paragraphs, "the text was pasted");
    elapsed
}

/// One Backspace at the end of a document: each deletion refreshes the positions of every
/// block, where the refresh loop's search showed.
fn time_backspace(paragraphs: usize) -> Duration {
    let doc = TextDocument::new();
    doc.set_djot_sync(&djot(paragraphs, "Past")).unwrap();
    let cursor = doc.cursor_at(doc.character_count());
    let start = Instant::now();
    cursor.delete_previous_char().unwrap();
    start.elapsed()
}

/// A text of `paragraphs` paragraphs with a two-by-two table after every eleventh.
fn djot_with_tables(paragraphs: usize) -> String {
    (0..paragraphs)
        .map(|i| {
            if i % 11 == 5 {
                format!("Past {i}. {PARA}\n\n| a{i} | b |\n| c | d |\n\n")
            } else {
                format!("Past {i}. {PARA}\n\n")
            }
        })
        .collect()
}

/// Select all of a document holding many small tables and delete it: every cell is
/// emptied, and every table removed.
fn time_delete_all_with_tables(paragraphs: usize) -> Duration {
    let doc = TextDocument::new();
    doc.set_djot_sync(&djot_with_tables(paragraphs)).unwrap();
    let cursor = doc.cursor();
    let start = Instant::now();
    cursor.select(SelectionType::Document);
    cursor.remove_selected_text().unwrap();
    let elapsed = start.elapsed();
    assert_eq!(doc.block_count(), 1, "the whole text was deleted");
    elapsed
}

/// Select all of a document holding many small tables and insert another such text over
/// it, as one edit: a version restore of a text with a table every few pages.
fn time_restore_with_tables(paragraphs: usize) -> Duration {
    let doc = TextDocument::new();
    doc.set_djot_sync(&djot_with_tables(paragraphs).replace("Past", "Current"))
        .unwrap();
    let past = djot_with_tables(paragraphs);
    let start = Instant::now();
    let cursor = doc.cursor();
    cursor.begin_edit_block();
    cursor.select(SelectionType::Document);
    cursor.insert_djot(&past).unwrap();
    cursor.end_edit_block();
    start.elapsed()
}

/// Paste a text holding many small tables into the middle of a two-paragraph document: the
/// paste's tables are followed by text, where their cells used to go to the end of it.
fn time_paste_with_tables_mid_document(paragraphs: usize) -> Duration {
    let doc = TextDocument::new();
    doc.set_djot_sync("One line.\n\nTwo lines.\n").unwrap();
    let past = djot_with_tables(paragraphs);
    let start = Instant::now();
    doc.cursor_at(4).insert_djot(&past).unwrap();
    start.elapsed()
}

/// Load the text `time_delete_all_with_tables` deletes: its linear reference.
fn time_load_with_tables(paragraphs: usize) -> Duration {
    let doc = TextDocument::new();
    let text = djot_with_tables(paragraphs);
    let start = Instant::now();
    doc.set_djot_sync(&text).unwrap();
    start.elapsed()
}

/// Load the same amount of text into a fresh document: the linear reference.
fn time_load(paragraphs: usize) -> Duration {
    let doc = TextDocument::new();
    let past = djot(paragraphs, "Past");
    let start = Instant::now();
    doc.set_djot_sync(&past).unwrap();
    start.elapsed()
}

/// `edit`'s time over `load`'s, median over `rounds`, the two timed back to back and in
/// alternating order.
fn relative_cost(
    edit: fn(usize) -> Duration,
    load: fn(usize) -> Duration,
    paragraphs: usize,
    rounds: usize,
) -> (f64, Duration, Duration) {
    let _ = (edit(paragraphs), load(paragraphs));
    let mut rows: Vec<(f64, Duration, Duration)> = (0..rounds)
        .map(|round| {
            let (edited, load) = if round % 2 == 0 {
                let load = load(paragraphs);
                (edit(paragraphs), load)
            } else {
                let edited = edit(paragraphs);
                (edited, load(paragraphs))
            };
            (
                edited.as_nanos() as f64 / load.as_nanos().max(1) as f64,
                edited,
                load,
            )
        })
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows[rounds / 2]
}

/// How much more `edit` costs relative to loading the text at `large` than at `small`
/// paragraphs. Prints both quotients.
fn growth(label: &str, edit: fn(usize) -> Duration, small: usize, large: usize) -> f64 {
    growth_over(label, edit, time_load, small, large)
}

/// [`growth`] over a load of the caller's choosing: one of the same text as `edit` works on.
fn growth_over(
    label: &str,
    edit: fn(usize) -> Duration,
    load: fn(usize) -> Duration,
    small: usize,
    large: usize,
) -> f64 {
    const ROUNDS: usize = 5;
    let (at_small, small_edit, small_load) = relative_cost(edit, load, small, ROUNDS);
    let (at_large, large_edit, large_load) = relative_cost(edit, load, large, ROUNDS);
    let growth = at_large / at_small;
    println!(
        "{label} over load: {at_small:.2}x at {small} paragraphs ({small_edit:?} / \
         {small_load:?}), {at_large:.2}x at {large} ({large_edit:?} / {large_load:?}), \
         growth {growth:.2}"
    );
    growth
}

#[test]
fn restoring_a_whole_document_scales_linearly() {
    let growth = growth("restore", time_restore, 250, 2_000);
    assert!(
        growth < 3.0,
        "replacing a whole document grew {growth:.2} times relative to loading it, for eight \
         times the text. A linear restore stays a fixed multiple of the load; growing with \
         the text means a per-paragraph walk of the document is back in the delete or the \
         insert (owner lists written once per block, the rope's offset index updated once \
         per inserted block, or the deletion's position refresh searching the block list for \
         every paragraph)."
    );
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing guard for long texts, meaningful in release builds only: cargo test --release"
)]
fn long_texts_paste_and_delete_without_a_walk_per_paragraph() {
    // Measured on the fix over three runs: 0.76 to 0.97 for the paste, and 1.48 to 1.67 for
    // the Backspace, whose own work grows a little faster than the load's once the document
    // outgrows the caches. With the rope's per-block walk put back, the paste grew 36.95
    // times; with the refresh loop's search put back, the Backspace grew 7.99 times.
    let paste = growth(
        "paste into a short document",
        time_paste_into_a_short_document,
        2_000,
        32_000,
    );
    let backspace = growth("backspace", time_backspace, 2_000, 32_000);
    assert!(
        paste < 3.0,
        "pasting a whole text into a short document grew {paste:.2} times relative to \
         loading it, for sixteen times the text: the rope's offset index is being updated \
         once per pasted paragraph again (see rope_split_block_into and rope_insert_blocks_at)."
    );
    assert!(
        backspace < 3.5,
        "one Backspace at the end of a document grew {backspace:.2} times relative to loading \
         it, for sixteen times the text: the deletion's position refresh is searching the \
         block list for every paragraph again."
    );
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing guard for long texts, meaningful in release builds only: cargo test --release"
)]
fn deleting_a_long_text_with_many_small_tables_scales_linearly() {
    // Deleting a whole text removes every table it covers, and each table used to go in
    // calls of its own, each rewriting the document's whole frame or table list and
    // searching every table for the owners of its cells; every cell emptied on the way
    // walked the rope's offset index once more. Deleting 32,000 paragraphs with a small
    // table every few pages took 31 s. `whole_document_scaling_tests` counts the list
    // rewrites exactly, and unit tests beside `get_relationships_from_right_ids` (tables)
    // and `rope_clear_blocks` count the other two, all in every build; this guard times the
    // whole deletion, at 2,000 against 32,000 paragraphs, in release builds only
    // (`cargo test --release`), which CI does not run. Measured on the fix: growth 1.54; on
    // the code before it, 26.6. The index walks alone are cheap per step and hide under the
    // linear work at these sizes (growth 1.70 with only them put back): their unit test is
    // their guard.
    let growth = growth_over(
        "delete all with tables",
        time_delete_all_with_tables,
        time_load_with_tables,
        2_000,
        32_000,
    );
    assert!(
        growth < 3.0,
        "deleting a whole text holding many small tables grew {growth:.2} times relative to \
         loading it, for sixteen times the text: the deletion is doing work per table that \
         grows with the document again (removing each table in calls of its own, or looking \
         for the owners of the removed cells table by table)."
    );
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing guard for long texts, meaningful in release builds only: cargo test --release"
)]
fn restoring_or_pasting_a_text_with_many_small_tables_scales_linearly() {
    // A paste put each table's cells at the end of the frame it landed in, finding that end
    // by walking the whole frame once per cell, and created every table and every frame
    // with the document as its owner, rewriting the document's whole table or frame list
    // each time: restoring 8,000 paragraphs with 727 small tables took 5.6 s, and pasting
    // them 9.3 s. `whole_document_scaling_tests` counts the list rewrites exactly in every
    // build; this guard times the whole edit, at 2,000 against 16,000 paragraphs, in release
    // builds only (`cargo test --release`). Measured on the fix: growth 1.22 for the restore
    // and 0.98 for the paste; on the code before it, 11.55 and 12.83 (18.7 s and 16.8 s at
    // 16,000 paragraphs).
    let restore = growth_over(
        "restore with tables",
        time_restore_with_tables,
        time_load_with_tables,
        2_000,
        16_000,
    );
    let paste = growth_over(
        "paste with tables mid document",
        time_paste_with_tables_mid_document,
        time_load_with_tables,
        2_000,
        16_000,
    );
    assert!(
        restore < 3.0 && paste < 3.0,
        "restoring (growth {restore:.2}) or pasting (growth {paste:.2}) a text holding many \
         small tables grew relative to loading it, for eight times the text: a paste is doing \
         work per table that grows with the document again (walking the frame to place each \
         table's cells, or creating each table and frame with the document as its owner)."
    );
}
