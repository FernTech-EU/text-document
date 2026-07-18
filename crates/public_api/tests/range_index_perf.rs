//! Performance regression guard for the per-block range index (B2-M1).
//!
//! Before the index, resolving a block's highlight spans scanned the whole range vector, so a
//! snapshot was O(blocks × ranges) and the "incremental" single-block path leaked O(total
//! ranges). On a Lorem-Ipsum-dense document — one flagged range per word, tens of thousands of
//! them — that pinned a core. The index makes both O(ranges-in-block).
//!
//! The gates are **ratios** (loaded ÷ zero-range baseline), measured back-to-back in one
//! process, so machine speed cancels and they are stable on any CI runner. That is exactly the
//! property that would catch a regression to the old behaviour (whose ratios were ≈3.8× for a
//! full snapshot and ≈6× for one keystroke): with the index the range work is a rounding error
//! next to the base snapshot cost, so both ratios sit near 1. Absolute times are printed for
//! information but never asserted (they would flake on a shared runner).

use std::time::Instant;

use text_document::{Color, HighlightFormat, HighlightMask, RangeHighlight, TextDocument};

const PARAGRAPHS: usize = 298; // Scene 1 of the reported project
const WORD: &str = "lorem "; // 6 chars incl. the trailing space
const WORDS_PER_PARA: usize = 47;

fn big_doc() -> TextDocument {
    let para = WORD.repeat(WORDS_PER_PARA);
    let text = vec![para.trim_end(); PARAGRAPHS].join("\n\n");
    let doc = TextDocument::new();
    doc.set_plain_text(&text).unwrap();
    doc
}

/// One flagged range per word across the whole document — the Lorem-Ipsum-vs-English case.
fn one_range_per_word(doc: &TextDocument) -> Vec<RangeHighlight> {
    let fmt = HighlightFormat {
        background_color: Some(Color {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 255,
        }),
        ..Default::default()
    };
    let mut out = Vec::new();
    for e in &doc.snapshot_flow_masked(&HighlightMask::all()).elements {
        if let text_document::FlowElementSnapshot::Block(b) = e {
            let base = b.position;
            let mut off = 0usize;
            for word in b.text.split(' ') {
                let len = word.chars().count();
                if len > 0 {
                    out.push(RangeHighlight {
                        start: base + off,
                        length: len,
                        format: fmt.clone(),
                    });
                }
                off += len + 1; // + the space
            }
        }
    }
    out
}

fn time_avg(iters: usize, mut f: impl FnMut()) -> f64 {
    f(); // warm
    let t = Instant::now();
    for _ in 0..iters {
        f();
    }
    t.elapsed().as_secs_f64() * 1000.0 / iters as f64
}

#[test]
fn a_full_snapshot_does_not_scale_with_document_range_count() {
    let doc = big_doc();
    let session = doc.add_range_session();
    let ranges = one_range_per_word(&doc);
    assert!(
        ranges.len() > 10_000,
        "expected a dense document, got {} ranges",
        ranges.len()
    );

    // Baseline: no ranges.
    doc.set_session_ranges(session, Vec::new());
    let base = time_avg(20, || {
        std::hint::black_box(doc.snapshot_flow());
    });

    // Loaded: one range per word.
    doc.set_session_ranges(session, ranges.clone());
    let loaded = time_avg(20, || {
        std::hint::black_box(doc.snapshot_flow());
    });

    let ratio = loaded / base;
    println!(
        "snapshot_flow: base={base:.2}ms  loaded({} ranges)={loaded:.2}ms  ratio={ratio:.2}×",
        ranges.len()
    );
    assert!(
        ratio < 2.0,
        "a full snapshot must not scale with the document's range count (was ≈3.8× before the \
         index); got {ratio:.2}× ({loaded:.2}ms vs {base:.2}ms baseline)"
    );
}

#[test]
fn one_keystroke_does_not_leak_the_whole_documents_range_count() {
    let doc = big_doc();
    let session = doc.add_range_session();
    let ranges = one_range_per_word(&doc);

    // A block in the middle — the block a keystroke in the thick of the document would relayout.
    let mid = match &doc.snapshot_flow_masked(&HighlightMask::all()).elements[PARAGRAPHS] {
        text_document::FlowElementSnapshot::Block(b) => b.position + 3,
        _ => 3,
    };

    doc.set_session_ranges(session, Vec::new());
    let base = time_avg(200, || {
        std::hint::black_box(doc.snapshot_block_at_position(mid));
    });

    doc.set_session_ranges(session, ranges.clone());
    let loaded = time_avg(200, || {
        std::hint::black_box(doc.snapshot_block_at_position(mid));
    });

    let ratio = loaded / base.max(f64::MIN_POSITIVE);
    println!(
        "one block: base={base:.4}ms  loaded={loaded:.4}ms  ratio={ratio:.2}×  ({} ranges)",
        ranges.len()
    );
    assert!(
        ratio < 2.5,
        "the single-block path must stay O(ranges-in-block), not leak O(total ranges) (was ≈6× \
         before the index); got {ratio:.2}×"
    );
}

#[test]
fn building_the_index_at_push_is_cheaper_than_a_snapshot() {
    // The index is rebuilt on every push, and pushes cluster on word-boundary keystrokes — the
    // latency-sensitive moment. Guard that the build is not itself a new stall: one push
    // (including the O(blocks) position scan + the bucketing of ~14k ranges) must cost less than
    // a single full snapshot of the same document, so it never dominates a frame.
    let doc = big_doc();
    let session = doc.add_range_session();
    let ranges = one_range_per_word(&doc);

    doc.set_session_ranges(session, ranges.clone());
    let snapshot = time_avg(20, || {
        std::hint::black_box(doc.snapshot_flow());
    });

    let push = time_avg(20, || {
        doc.set_session_ranges(session, ranges.clone());
    });

    println!(
        "index build: push={push:.2}ms  snapshot={snapshot:.2}ms  ({} ranges)",
        ranges.len()
    );
    assert!(
        push < snapshot * 2.0,
        "building the index on a push must not dwarf a snapshot; push={push:.2}ms vs \
         snapshot={snapshot:.2}ms"
    );
}
