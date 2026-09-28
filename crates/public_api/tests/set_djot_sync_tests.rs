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

// ── Undo history after a load in place ──────────────────────────────────────

/// A document a writer has typed in, pressed Enter in and typed in again: its undo stack
/// holds entries that restore the whole text as it was before each of them.
fn edited_document() -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_sync("The lamp went out in the hall.\n\nShe waited.")
        .unwrap();
    let end = doc.character_count();
    let cursor = doc.cursor_at(end);
    cursor.insert_text(" Then the door opened").unwrap();
    cursor.insert_block().unwrap();
    cursor.insert_text("Nobody came in").unwrap();
    assert!(doc.can_undo(), "the edits are in the history");
    doc
}

const RELOADED: &str = "The lantern went out in the hall.\n\nShe waited alone.";

/// Every setter that replaces the whole document says it clears the undo history. Only
/// `set_plain_text` did: after a load in place, Undo brought back the text from before the
/// load, and the next save wrote that stale text over what the host had just loaded.
#[test]
fn every_load_in_place_clears_the_undo_history() {
    type Load = fn(&TextDocument);
    let loads: [(&str, Load); 5] = [
        ("set_djot_sync", |doc| {
            doc.set_djot_sync(RELOADED).unwrap();
        }),
        ("set_djot", |doc| {
            doc.set_djot(RELOADED).unwrap().wait().unwrap();
        }),
        ("set_markdown", |doc| {
            doc.set_markdown(RELOADED).unwrap().wait().unwrap();
        }),
        ("set_html", |doc| {
            doc.set_html("<p>The lantern went out in the hall.</p><p>She waited alone.</p>")
                .unwrap()
                .wait()
                .unwrap();
        }),
        ("set_plain_text", |doc| {
            doc.set_plain_text("The lantern went out in the hall.\nShe waited alone.")
                .unwrap();
        }),
    ];
    for (name, load) in loads {
        let doc = edited_document();
        load(&doc);
        let loaded = doc.to_plain_text().unwrap();
        assert_eq!(
            loaded, "The lantern went out in the hall.\nShe waited alone.",
            "{name}"
        );
        assert!(!doc.can_undo(), "{name}: the history survived the load");
        assert!(
            !doc.can_redo(),
            "{name}: the redo history survived the load"
        );
        let _ = doc.undo();
        assert_eq!(
            doc.to_plain_text().unwrap(),
            loaded,
            "{name}: Undo after the load changed the loaded text"
        );
    }
}

/// A synchronous load tells a bound view that there is nothing left to undo, as
/// `set_plain_text` does, so an Edit menu greys its Undo out.
#[test]
fn a_sync_load_announces_the_cleared_history() {
    let doc = edited_document();
    doc.poll_events();
    doc.set_djot_sync(RELOADED).unwrap();
    let events = doc.poll_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            DocumentEvent::UndoRedoChanged {
                can_undo: false,
                can_redo: false
            }
        )),
        "expected UndoRedoChanged with nothing to undo, got: {events:?}"
    );
}

/// The history of an asynchronous load is cleared once, when its content is in: by whichever
/// of `wait` and the operation's completion comes first. An edit made right after `wait`
/// returns keeps its entry when the completion is handled after it.
#[test]
fn an_edit_after_an_async_load_keeps_its_undo_entry() {
    for _ in 0..20 {
        let doc = edited_document();
        doc.poll_events();
        doc.set_djot(RELOADED).unwrap().wait().unwrap();
        doc.cursor_at(0).insert_text("Then ").unwrap();
        // Wait for the operation's completion to be handled.
        let started = Instant::now();
        loop {
            let finished = doc
                .poll_events()
                .iter()
                .any(|e| matches!(e, DocumentEvent::LongOperationFinished { .. }));
            if finished {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the load's completion never arrived"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            doc.can_undo(),
            "the edit made after the load lost its entry"
        );
        doc.undo().unwrap();
        assert_eq!(
            doc.to_plain_text().unwrap(),
            "The lantern went out in the hall.\nShe waited alone."
        );
        assert!(!doc.can_undo(), "nothing from before the load came back");
    }
}

/// A host that never reads the result, and learns of the load's end from the document's
/// events, finds the history cleared and is told so.
#[test]
fn an_async_load_nobody_waits_for_clears_the_history_at_its_completion() {
    let doc = edited_document();
    doc.poll_events();
    drop(doc.set_djot(RELOADED).unwrap());
    let started = Instant::now();
    let mut seen = Vec::new();
    while !seen
        .iter()
        .any(|e| matches!(e, DocumentEvent::LongOperationFinished { .. }))
    {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the load's completion never arrived"
        );
        std::thread::sleep(Duration::from_millis(1));
        seen.extend(doc.poll_events());
    }
    assert!(!doc.can_undo(), "the history survived the load");
    assert!(
        seen.iter().any(|e| matches!(
            e,
            DocumentEvent::UndoRedoChanged {
                can_undo: false,
                can_redo: false
            }
        )),
        "expected UndoRedoChanged with nothing to undo, got: {seen:?}"
    );
}
