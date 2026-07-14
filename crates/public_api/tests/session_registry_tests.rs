//! The highlight **session registry** — several highlight layers on one document, and a
//! per-view mask selecting which ones each view renders.
//!
//! Before this, a document held exactly one highlighter, wholesale-replaced. Now it holds any
//! number of sessions — a syntax highlighter, a spell-checker, one find session per view —
//! merged in registration order, and a [`HighlightMask`] lets two panes over the same document
//! show different find highlighting. These tests pin the properties that machinery must have;
//! the single-highlighter behaviour is covered (unchanged) by `highlight_tests.rs`.

use std::sync::{Arc, Mutex};

use text_document::{
    Color, FlowElement, FlowElementSnapshot, HighlightContext, HighlightFormat, HighlightMask,
    PaintHighlightSpan, RangeHighlight, SyntaxHighlighter, TextDocument,
};

fn new_doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

/// Paint spans of the first block, under a mask.
fn masked_paint_spans(doc: &TextDocument, mask: &HighlightMask) -> Vec<PaintHighlightSpan> {
    match &doc.snapshot_flow_masked(mask).elements[0] {
        FlowElementSnapshot::Block(b) => b.paint_highlights.clone(),
        _ => panic!("expected a block"),
    }
}

fn first_block_id(doc: &TextDocument) -> usize {
    match &doc.flow()[0] {
        FlowElement::Block(b) => b.id(),
        _ => panic!("expected a block"),
    }
}

const RED: Color = Color {
    red: 255,
    green: 0,
    blue: 0,
    alpha: 255,
};
const BLUE: Color = Color {
    red: 0,
    green: 0,
    blue: 255,
    alpha: 255,
};
const GREEN: Color = Color {
    red: 0,
    green: 255,
    blue: 0,
    alpha: 255,
};

/// Colours the whole block with a fixed foreground.
struct ColorAll(Color);
impl SyntaxHighlighter for ColorAll {
    fn highlight_block(&self, text: &str, ctx: &mut HighlightContext) {
        let len = text.chars().count();
        if len > 0 {
            ctx.set_format(
                0,
                len,
                HighlightFormat {
                    foreground_color: Some(self.0),
                    ..Default::default()
                },
            );
        }
    }
}

fn bg(color: Color) -> HighlightFormat {
    HighlightFormat {
        background_color: Some(color),
        ..Default::default()
    }
}

/// **Two syntax sessions coexist and compose.** `add_syntax_session` adds; it does not
/// replace. One paints the foreground, another the background, and a block shows both.
#[test]
fn two_syntax_sessions_compose_by_field() {
    let doc = new_doc("hello world");

    // Session A: red foreground everywhere.
    doc.add_syntax_session(Arc::new(ColorAll(RED)));
    // Session B: a background highlighter over the whole block.
    struct BgAll(Color);
    impl SyntaxHighlighter for BgAll {
        fn highlight_block(&self, text: &str, ctx: &mut HighlightContext) {
            let len = text.chars().count();
            if len > 0 {
                ctx.set_format(0, len, bg(self.0));
            }
        }
    }
    doc.add_syntax_session(Arc::new(BgAll(BLUE)));

    let spans = masked_paint_spans(&doc, &HighlightMask::all());
    assert!(!spans.is_empty(), "both sessions should paint");
    // Every painted char carries BOTH the red foreground (session A) and the blue background
    // (session B) — different fields, so neither overwrites the other.
    assert!(spans.iter().all(|s| s.foreground_color == Some(RED)));
    assert!(spans.iter().all(|s| s.background_color == Some(BLUE)));
}

/// **Registration order is precedence.** When two sessions set the *same* field on the same
/// characters, the later-registered session wins.
#[test]
fn a_later_session_wins_the_same_field() {
    let doc = new_doc("hello");
    doc.add_syntax_session(Arc::new(ColorAll(RED)));
    doc.add_syntax_session(Arc::new(ColorAll(BLUE))); // registered later → wins

    let spans = masked_paint_spans(&doc, &HighlightMask::all());
    assert!(!spans.is_empty());
    assert!(
        spans.iter().all(|s| s.foreground_color == Some(BLUE)),
        "the later session's foreground must win"
    );
}

/// **Each syntax session cascades independently.** A highlighter that carries block state
/// across blocks (multiline-comment style) must not have its state timeline corrupted by
/// another session sharing the document. This is the single most dangerous way a naive
/// registry breaks: one shared cascade.
#[test]
fn per_session_state_cascades_do_not_interfere() {
    // Records, per block, the `previous_block_state` it was handed. A correct per-session
    // cascade hands block N the state block N-1 set, for THIS session only.
    struct StateTracker {
        seen_prev: Mutex<Vec<i64>>,
    }
    impl SyntaxHighlighter for StateTracker {
        fn highlight_block(&self, _text: &str, ctx: &mut HighlightContext) {
            self.seen_prev
                .lock()
                .unwrap()
                .push(ctx.previous_block_state());
            // Advance the state monotonically so each block's `previous` is the prior block's.
            ctx.set_current_block_state(ctx.previous_block_state() + 1);
        }
    }

    let doc = new_doc("a\n\nb\n\nc");
    let a = Arc::new(StateTracker {
        seen_prev: Mutex::new(Vec::new()),
    });
    let b = Arc::new(StateTracker {
        seen_prev: Mutex::new(Vec::new()),
    });
    doc.add_syntax_session(a.clone());
    doc.add_syntax_session(b.clone());
    // The `add`s each trigger a rehighlight; clear and do ONE clean pass to observe.
    a.seen_prev.lock().unwrap().clear();
    b.seen_prev.lock().unwrap().clear();
    doc.rehighlight();

    let a_seen = a.seen_prev.lock().unwrap().clone();
    let b_seen = b.seen_prev.lock().unwrap().clone();

    // The isolation property, two ways:
    // 1. The two sessions saw the IDENTICAL sequence. A shared timeline would have handed
    //    session B block-0's `previous` as session A's final state, not −1.
    assert_eq!(
        a_seen, b_seen,
        "the two cascades must be independent and identical"
    );
    // 2. Each is a clean cascade: starts at −1, each block's `previous` is the prior block's
    //    `current` (monotonic +1 by construction), with no interleaving.
    assert_eq!(a_seen.first(), Some(&-1), "the cascade starts clean at −1");
    assert!(
        a_seen.windows(2).all(|w| w[1] == w[0] + 1),
        "each block sees exactly the previous block's state — no other session's: {a_seen:?}"
    );
}

/// **A range session highlights absolute offsets, sliced to the right block.** The offsets are
/// in the document's char space (the space `find_all` reports in); the document slices them to
/// per-block spans.
#[test]
fn a_range_session_highlights_absolute_offsets() {
    let doc = new_doc("hello world");
    let find = doc.add_range_session();
    // Highlight "world" — chars 6..11 in the single block.
    assert!(doc.set_session_ranges(
        find,
        vec![RangeHighlight {
            start: 6,
            length: 5,
            format: bg(GREEN),
        }]
    ));

    let spans = masked_paint_spans(&doc, &HighlightMask::all());
    assert_eq!(spans.len(), 1, "one contiguous highlighted range");
    assert_eq!(spans[0].start, 6);
    assert_eq!(spans[0].length, 5);
    assert_eq!(spans[0].background_color, Some(GREEN));
}

/// A range that spans block boundaries is **sliced**: each block gets only its own part, at
/// block-relative offsets. Covering the whole document with one absolute range must land a
/// span in every text-bearing block, each starting at block-relative 0.
#[test]
fn a_range_session_slices_across_blocks() {
    let doc = new_doc("hello\n\nworld\n\nagain");
    let s = doc.add_range_session();
    // One absolute range that covers everything (length far past the end is clamped per block).
    doc.set_session_ranges(
        s,
        vec![RangeHighlight {
            start: 0,
            length: 10_000,
            format: bg(GREEN),
        }],
    );

    let flow = doc.snapshot_flow_masked(&HighlightMask::all());
    let painted: Vec<&PaintHighlightSpan> = flow
        .elements
        .iter()
        .filter_map(|e| match e {
            FlowElementSnapshot::Block(b) if !b.paint_highlights.is_empty() => {
                Some(&b.paint_highlights[0])
            }
            _ => None,
        })
        .collect();

    assert!(
        painted.len() >= 2,
        "the one absolute range must have sliced into several blocks, got {}",
        painted.len()
    );
    for p in painted {
        assert_eq!(
            p.start, 0,
            "each block's slice is BLOCK-relative — a block after the first must not carry an \
             absolute offset"
        );
        assert!(p.length > 0);
        assert_eq!(p.background_color, Some(GREEN));
    }
}

/// **The mask is per view.** Two snapshots of one document under different masks show
/// different highlighting — which is how two panes carry different find sessions.
#[test]
fn two_masks_over_one_document_differ() {
    let doc = new_doc("hello world");
    let syntax = doc.add_syntax_session(Arc::new(ColorAll(RED)));
    let find_a = doc.add_range_session();
    let find_b = doc.add_range_session();
    doc.set_session_ranges(
        find_a,
        vec![RangeHighlight {
            start: 0,
            length: 5,
            format: bg(GREEN),
        }],
    );
    doc.set_session_ranges(
        find_b,
        vec![RangeHighlight {
            start: 6,
            length: 5,
            format: bg(BLUE),
        }],
    );

    // Pane A: syntax + its own find (green over "hello"), NOT pane B's.
    let a = masked_paint_spans(&doc, &HighlightMask::only([syntax, find_a]));
    assert!(a.iter().any(|s| s.background_color == Some(GREEN)));
    assert!(
        a.iter().all(|s| s.background_color != Some(BLUE)),
        "pane A must not see pane B's find highlighting"
    );

    // Pane B: syntax + its own find (blue over "world"), NOT pane A's.
    let b = masked_paint_spans(&doc, &HighlightMask::only([syntax, find_b]));
    assert!(b.iter().any(|s| s.background_color == Some(BLUE)));
    assert!(b.iter().all(|s| s.background_color != Some(GREEN)));

    // The empty mask shows nothing at all — the read-only-preview fast path.
    assert!(masked_paint_spans(&doc, &HighlightMask::none()).is_empty());
}

/// A mask that names only the syntax session hides an active find session — even though the
/// find session exists and has ranges.
#[test]
fn a_mask_hides_the_sessions_it_omits() {
    let doc = new_doc("hello world");
    let syntax = doc.add_syntax_session(Arc::new(ColorAll(RED)));
    let find = doc.add_range_session();
    doc.set_session_ranges(
        find,
        vec![RangeHighlight {
            start: 0,
            length: 5,
            format: bg(GREEN),
        }],
    );

    let only_syntax = masked_paint_spans(&doc, &HighlightMask::only([syntax]));
    assert!(
        only_syntax
            .iter()
            .all(|s| s.background_color != Some(GREEN))
    );
    assert!(only_syntax.iter().any(|s| s.foreground_color == Some(RED)));
}

/// `set_syntax_highlighter` keeps its "replace the syntax highlighter" contract — and must
/// **not** disturb a range (find/spell) session.
#[test]
fn set_syntax_highlighter_replaces_syntax_but_spares_range_sessions() {
    let doc = new_doc("hello world");
    let find = doc.add_range_session();
    doc.set_session_ranges(
        find,
        vec![RangeHighlight {
            start: 0,
            length: 5,
            format: bg(GREEN),
        }],
    );

    doc.set_syntax_highlighter(Some(Arc::new(ColorAll(RED))));
    doc.set_syntax_highlighter(Some(Arc::new(ColorAll(BLUE)))); // replaces the first

    let spans = masked_paint_spans(&doc, &HighlightMask::all());
    // Exactly one syntax foreground survives (the second), and the find highlight is intact.
    assert!(
        spans.iter().all(|s| s.foreground_color != Some(RED)),
        "the replaced syntax highlighter must be gone"
    );
    assert!(spans.iter().any(|s| s.foreground_color == Some(BLUE)));
    assert!(
        spans.iter().any(|s| s.background_color == Some(GREEN)),
        "the range session must survive set_syntax_highlighter"
    );

    // …and removing the syntax highlighter still spares the range session.
    doc.set_syntax_highlighter(None);
    let spans = masked_paint_spans(&doc, &HighlightMask::all());
    assert!(spans.iter().all(|s| s.foreground_color.is_none()));
    assert!(spans.iter().any(|s| s.background_color == Some(GREEN)));
}

/// `remove_session` retires exactly one session and leaves the rest.
#[test]
fn remove_session_retires_one_layer() {
    let doc = new_doc("hello");
    let a = doc.add_range_session();
    let b = doc.add_range_session();
    doc.set_session_ranges(
        a,
        vec![RangeHighlight {
            start: 0,
            length: 2,
            format: bg(GREEN),
        }],
    );
    doc.set_session_ranges(
        b,
        vec![RangeHighlight {
            start: 3,
            length: 2,
            format: bg(BLUE),
        }],
    );

    assert!(doc.remove_session(a));
    assert!(!doc.remove_session(a), "already gone");

    let spans = masked_paint_spans(&doc, &HighlightMask::all());
    assert!(spans.iter().all(|s| s.background_color != Some(GREEN)));
    assert!(spans.iter().any(|s| s.background_color == Some(BLUE)));
}

/// Setting ranges on a non-range (syntax) session is a caller error, reported — not a silent
/// no-op that leaves the writer wondering why nothing highlighted.
#[test]
fn set_session_ranges_rejects_a_syntax_session() {
    let doc = new_doc("hello");
    let syntax = doc.add_syntax_session(Arc::new(ColorAll(RED)));
    assert!(
        !doc.set_session_ranges(syntax, vec![]),
        "a syntax session cannot take ranges"
    );
    // …and an unknown id is likewise false, never a panic.
    assert!(!doc.set_session_ranges(text_document::SessionId(9999), vec![]));
    let _ = first_block_id(&doc);
}
