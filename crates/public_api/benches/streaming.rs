// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Streaming-buffer timing harness: what a log/console view actually pays.
//!
//! Such a view does exactly three things in a loop — append a line at the end,
//! ask whether it is over its scrollback cap, and evict the oldest lines — so
//! each is measured against document size. Anything that scales with the whole
//! document turns `tail -f` into a stall.
//!
//! * `append_line` — the naive consumer path: locate the end, then
//!   `insert_block` + `insert_text`.
//! * `append_line_held_cursor` — the same insert with the cursor created once,
//!   outside the timing, to separate the insert's own cost from the cost of
//!   *finding* the end.
//! * `block_count` — the cap check. Documented O(1); measured here because the
//!   eviction loop calls it every line.
//! * `truncate_front` — evicting the oldest lines.
//!
//! Deliberately harness-free (`harness = false`, plain `main`) rather than
//! criterion, which sizes its own iteration counts: here that would either grow
//! the document under test without bound or spend the budget on untimed
//! rebuilds. A fixed, declared workload is reproducible and honest.
//!
//! Run with `cargo bench --bench streaming`.

use std::io::Write;
use std::time::{Duration, Instant};

use text_document::{MoveMode, TextDocument};

/// Print a row and push it out immediately.
///
/// Rust block-buffers stdout when it is not a terminal, so a `cargo bench`
/// whose output is piped or captured would otherwise show nothing at all until
/// the very end — unhelpful for a harness whose slowest row takes minutes.
macro_rules! row {
    ($($arg:tt)*) => {{
        println!($($arg)*);
        let _ = std::io::stdout().flush();
    }};
}

/// A representative log line.
const LINE: &str = "2026-07-16T12:00:00Z INFO  worker: processed batch id=1234 in 42ms";

/// Document sizes to probe. The scaling across these is the whole point.
const SIZES: [usize; 3] = [1_000, 10_000, 100_000];

/// Lines appended per timed repetition.
const APPEND_BATCH: usize = 20;

/// Repetitions per measurement; the median is reported.
const REPS: usize = 3;

fn make_doc(lines: usize) -> TextDocument {
    let text: String = (0..lines).map(|_| LINE).collect::<Vec<_>>().join("\n");
    let doc = TextDocument::new();
    doc.set_plain_text(&text).unwrap();
    doc
}

fn median(mut xs: Vec<Duration>) -> Duration {
    xs.sort_unstable();
    xs[xs.len() / 2]
}

fn main() {
    row!();
    row!("text-document — what a streaming log view pays per line");
    row!("{:-<92}", "");
    row!(
        "{:>9}  {:>16}  {:>18}  {:>14}  {:>16}",
        "N",
        "append_line",
        "append (held cur)",
        "block_count",
        "truncate_front"
    );
    row!(
        "{:>9}  {:>16}  {:>18}  {:>14}  {:>16}",
        "",
        "(per line)",
        "(per line)",
        "(per call)",
        "(20 lines)"
    );
    row!("{:-<92}", "");

    for n in SIZES {
        // ── append_line: the whole naive path, end-lookup included ──
        let append = median(
            (0..REPS)
                .map(|_| {
                    let doc = make_doc(n);
                    let start = Instant::now();
                    for _ in 0..APPEND_BATCH {
                        let cursor = doc.cursor_at(doc.character_count());
                        cursor.insert_block().unwrap();
                        cursor.insert_text(LINE).unwrap();
                    }
                    start.elapsed()
                })
                .collect(),
        ) / APPEND_BATCH as u32;

        // ── append with the end already located (cursor built untimed) ──
        let append_held = median(
            (0..REPS)
                .map(|_| {
                    let doc = make_doc(n);
                    let cursor = doc.cursor_at(doc.character_count());
                    let start = Instant::now();
                    for _ in 0..APPEND_BATCH {
                        cursor.insert_block().unwrap();
                        cursor.insert_text(LINE).unwrap();
                    }
                    start.elapsed()
                })
                .collect(),
        ) / APPEND_BATCH as u32;

        // ── block_count: the cap check, called once per appended line ──
        let count = median(
            (0..REPS)
                .map(|_| {
                    let doc = make_doc(n);
                    let start = Instant::now();
                    for _ in 0..APPEND_BATCH {
                        std::hint::black_box(doc.block_count());
                    }
                    start.elapsed()
                })
                .collect(),
        ) / APPEND_BATCH as u32;

        // ── truncate_front: evict the oldest 20 lines ──
        let truncate = median(
            (0..REPS)
                .map(|_| {
                    let doc = make_doc(n);
                    // Start of the block just past the ones being evicted.
                    let cut = doc.block_by_number(APPEND_BATCH).unwrap().position();
                    let cursor = doc.cursor_at(0);
                    let start = Instant::now();
                    cursor.set_position(cut, MoveMode::KeepAnchor);
                    cursor.remove_selected_text().unwrap();
                    start.elapsed()
                })
                .collect(),
        );

        row!(
            "{:>9}  {:>16}  {:>18}  {:>14}  {:>16}",
            n,
            format!("{append:?}"),
            format!("{append_held:?}"),
            format!("{count:?}"),
            format!("{truncate:?}")
        );
    }

    row!("{:-<92}", "");
    row!("append_line       = cursor_at(character_count()) + insert_block + insert_text");
    row!("append (held cur) = insert_block + insert_text, end located once beforehand");
    row!("block_count       = the scrollback-cap check a viewer runs every line");
    row!("truncate_front    = select [0, block 20) + remove_selected_text");
    row!();
}
