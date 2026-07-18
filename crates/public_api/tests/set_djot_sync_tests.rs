//! `set_djot_sync` — the synchronous document-load path.
//!
//! [`TextDocument::set_djot`] starts a long operation (a spawned thread) which
//! the caller then blocks on. For *loading* content that round trip is pure
//! overhead, and it does not shrink with the input: an empty document costs the
//! same thread spawn and hand-off as a full one, so loading N documents in a loop
//! paid it N times.
//!
//! These tests pin the two properties that make the sync path a safe drop-in for
//! a loader: it produces exactly the same document as the async path, and it
//! carries no fixed per-call latency.

use std::time::{Duration, Instant};

use text_document::{DocumentEvent, TextDocument};

/// A spread of inputs: empty (the case the old path was slowest at, relatively),
/// plain, and each of the block shapes a manuscript actually uses.
const SAMPLES: &[&str] = &[
    "",
    "hello",
    "# A heading\n\nA paragraph with *emphasis* and `code`.",
    "- one\n- two\n- three",
    "> a quote\n\nand a trailing paragraph",
];

/// Load `src` the async way: start the operation, block for its completion.
fn load_async(src: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot(src).unwrap().wait().unwrap();
    doc
}

/// Load `src` synchronously, on this thread.
fn load_sync(src: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_sync(src).unwrap();
    doc
}

/// Same parser, same document. This equivalence is what lets a loader swap the
/// async call for the sync one without changing what the user sees.
#[test]
fn sync_and_async_loads_produce_the_same_document() {
    for src in SAMPLES {
        let via_async = load_async(src).to_djot().unwrap();
        let via_sync = load_sync(src).to_djot().unwrap();
        assert_eq!(via_sync, via_async, "divergence for input {src:?}");
    }
}

/// Both paths report the same block count — the sync one returns it directly
/// instead of through an operation result.
#[test]
fn sync_reports_the_same_block_count_as_async() {
    for src in SAMPLES {
        let sync_doc = TextDocument::new();
        let sync_count = sync_doc.set_djot_sync(src).unwrap().block_count;

        let async_doc = TextDocument::new();
        let async_count = async_doc.set_djot(src).unwrap().wait().unwrap().block_count;

        assert_eq!(
            sync_count, async_count,
            "block_count divergence for {src:?}"
        );
    }
}

/// A sync load still resets the document, so a bound view refreshes exactly as it
/// did on the async path. Without this the editor would keep showing the old text.
#[test]
fn sync_load_emits_document_reset() {
    let doc = TextDocument::new();
    doc.poll_events(); // drain setup events

    doc.set_djot_sync("# Title\n\nBody").unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::DocumentReset)),
        "expected DocumentReset, got: {events:?}"
    );
}

/// The regression this path exists to prevent.
///
/// `Operation::wait` used to re-check the result on a 50 ms timer, so every load
/// — however trivial — cost ~50 ms of sleeping. Loading a book's worth of empty
/// scenes in a loop therefore burned seconds of pure latency with no work being
/// done. The sync path has no such floor.
#[test]
fn many_empty_loads_have_no_fixed_latency_floor() {
    const LOADS: usize = 40;
    let doc = TextDocument::new();

    let started = Instant::now();
    for _ in 0..LOADS {
        doc.set_djot_sync("").unwrap();
    }
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(500),
        "{LOADS} empty loads took {elapsed:?}; the old polling path would have \
         spent ~{}ms of that asleep",
        LOADS * 50
    );
}
