//! Tests for the SyntaxHighlighter trait system.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use text_document::{
    Color, DocumentEvent, FlowElement, FlowElementSnapshot, FragmentContent, HighlightContext,
    HighlightFormat, MoveMode, SyntaxHighlighter, TextDocument, TextFormat, UnderlineStyle,
};

// ── Helpers ──────────────────────────────────────────────────────

fn new_doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

// Fully-merged display fragments (base + all highlights). Paint-only
// highlights now live in `BlockSnapshot::paint_highlights` rather than in
// `fragments()` (the shaping input), so these merge-result assertions go
// through `display_fragments()`, which reproduces the merged visual.
fn first_block_fragments(doc: &TextDocument) -> Vec<FragmentContent> {
    match &doc.flow()[0] {
        FlowElement::Block(b) => b.display_fragments(),
        _ => panic!("expected block"),
    }
}

fn first_block_paint_spans(doc: &TextDocument) -> Vec<text_document::PaintHighlightSpan> {
    match &doc.flow()[0] {
        FlowElement::Block(b) => b.snapshot().paint_highlights,
        _ => panic!("expected block"),
    }
}

fn first_block_id(doc: &TextDocument) -> usize {
    match &doc.flow()[0] {
        FlowElement::Block(b) => b.id(),
        _ => panic!("expected block"),
    }
}

/// A highlighter that colors all text with a fixed foreground color.
struct ColorAllHighlighter {
    color: Color,
}

impl SyntaxHighlighter for ColorAllHighlighter {
    fn highlight_block(&self, text: &str, ctx: &mut HighlightContext) {
        let len = text.chars().count();
        if len > 0 {
            ctx.set_format(
                0,
                len,
                HighlightFormat {
                    foreground_color: Some(self.color),
                    ..Default::default()
                },
            );
        }
    }
}

/// A highlighter that bolds a specific word wherever it appears.
struct WordBoldHighlighter {
    word: String,
}

impl SyntaxHighlighter for WordBoldHighlighter {
    fn highlight_block(&self, text: &str, ctx: &mut HighlightContext) {
        let word_chars: Vec<char> = self.word.chars().collect();
        let text_chars: Vec<char> = text.chars().collect();
        let word_len = word_chars.len();
        if word_len == 0 || text_chars.len() < word_len {
            return;
        }
        for i in 0..=(text_chars.len() - word_len) {
            if text_chars[i..i + word_len] == word_chars[..] {
                ctx.set_format(
                    i,
                    word_len,
                    HighlightFormat {
                        font_bold: Some(true),
                        ..Default::default()
                    },
                );
            }
        }
    }
}

/// A highlighter that handles multi-line `/* ... */` comments using block state.
/// State 0 = normal, state 1 = inside comment.
struct MultiLineCommentHighlighter;

impl SyntaxHighlighter for MultiLineCommentHighlighter {
    fn highlight_block(&self, text: &str, ctx: &mut HighlightContext) {
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut in_comment = ctx.previous_block_state() == 1;
        let mut i = 0;
        let green = Color::rgb(0, 128, 0);

        while i < len {
            if in_comment {
                // Look for */
                let start = i;
                while i < len {
                    if i + 1 < len && chars[i] == '*' && chars[i + 1] == '/' {
                        i += 2;
                        in_comment = false;
                        break;
                    }
                    i += 1;
                }
                ctx.set_format(
                    start,
                    i - start,
                    HighlightFormat {
                        foreground_color: Some(green),
                        ..Default::default()
                    },
                );
            } else {
                // Look for /*
                if i + 1 < len && chars[i] == '/' && chars[i + 1] == '*' {
                    in_comment = true;
                    // Don't advance — let the comment branch handle it next iteration.
                    // Actually, mark the opening and continue.
                    let start = i;
                    i += 2;
                    // Continue scanning for the closing */
                    while i < len {
                        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '/' {
                            i += 2;
                            in_comment = false;
                            break;
                        }
                        i += 1;
                    }
                    ctx.set_format(
                        start,
                        i - start,
                        HighlightFormat {
                            foreground_color: Some(green),
                            ..Default::default()
                        },
                    );
                } else {
                    i += 1;
                }
            }
        }

        ctx.set_current_block_state(if in_comment { 1 } else { 0 });
    }
}

/// A highlighter that counts how many times highlight_block is called.
struct CountingHighlighter {
    count: Arc<AtomicUsize>,
}

impl SyntaxHighlighter for CountingHighlighter {
    fn highlight_block(&self, _text: &str, _ctx: &mut HighlightContext) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

/// A highlighter that stores user data (a counter) on each block and
/// records the counter value it observed on entry, so tests can assert
/// that user data persisted across rehighlights.
struct UserDataHighlighter {
    observations: Arc<Mutex<Vec<u32>>>,
}

impl SyntaxHighlighter for UserDataHighlighter {
    fn highlight_block(&self, _text: &str, ctx: &mut HighlightContext) {
        let count: u32 = ctx
            .user_data()
            .and_then(|d| d.downcast_ref::<u32>())
            .copied()
            .unwrap_or(0);
        self.observations.lock().unwrap().push(count);
        ctx.set_user_data(Box::new(count + 1));
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Color tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn color_rgb() {
    let c = Color::rgb(255, 0, 0);
    assert_eq!(c.red, 255);
    assert_eq!(c.green, 0);
    assert_eq!(c.blue, 0);
    assert_eq!(c.alpha, 255);
}

#[test]
fn color_rgba() {
    let c = Color::rgba(10, 20, 30, 128);
    assert_eq!(c.red, 10);
    assert_eq!(c.green, 20);
    assert_eq!(c.blue, 30);
    assert_eq!(c.alpha, 128);
}

#[test]
fn color_default() {
    let c = Color::default();
    assert_eq!(
        c,
        Color {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 0
        }
    );
}

#[test]
fn color_equality() {
    assert_eq!(Color::rgb(1, 2, 3), Color::rgb(1, 2, 3));
    assert_ne!(Color::rgb(1, 2, 3), Color::rgb(4, 5, 6));
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// HighlightFormat tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn highlight_format_default_all_none() {
    let fmt = HighlightFormat::default();
    assert_eq!(fmt.foreground_color, None);
    assert_eq!(fmt.background_color, None);
    assert_eq!(fmt.underline_color, None);
    assert_eq!(fmt.font_bold, None);
    assert_eq!(fmt.font_italic, None);
    assert_eq!(fmt.font_underline, None);
    assert_eq!(fmt.font_family, None);
    assert_eq!(fmt.tooltip, None);
}

#[test]
fn highlight_format_partial_set() {
    let fmt = HighlightFormat {
        foreground_color: Some(Color::rgb(255, 0, 0)),
        font_italic: Some(true),
        ..Default::default()
    };
    assert_eq!(fmt.foreground_color, Some(Color::rgb(255, 0, 0)));
    assert_eq!(fmt.font_italic, Some(true));
    assert_eq!(fmt.font_bold, None);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// HighlightContext tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn context_set_format_adds_spans() {
    let mut ctx = HighlightContext::new(1, -1, None);
    ctx.set_format(0, 5, HighlightFormat::default());
    ctx.set_format(5, 3, HighlightFormat::default());
    let (spans, _, _) = ctx.into_parts();
    assert_eq!(spans.len(), 2);
}

#[test]
fn context_zero_length_ignored() {
    let mut ctx = HighlightContext::new(1, -1, None);
    ctx.set_format(0, 0, HighlightFormat::default());
    let (spans, _, _) = ctx.into_parts();
    assert!(spans.is_empty());
}

#[test]
fn context_previous_block_state_default() {
    let ctx = HighlightContext::new(1, -1, None);
    assert_eq!(ctx.previous_block_state(), -1);
}

#[test]
fn context_set_and_get_block_state() {
    let mut ctx = HighlightContext::new(1, -1, None);
    assert_eq!(ctx.current_block_state(), -1);
    ctx.set_current_block_state(42);
    assert_eq!(ctx.current_block_state(), 42);
    let (_, state, _) = ctx.into_parts();
    assert_eq!(state, 42);
}

#[test]
fn context_user_data_roundtrip() {
    let mut ctx = HighlightContext::new(1, -1, None);
    ctx.set_user_data(Box::new(42u32));
    let val = ctx.user_data().unwrap().downcast_ref::<u32>().unwrap();
    assert_eq!(*val, 42);
}

#[test]
fn context_user_data_none_by_default() {
    let ctx = HighlightContext::new(1, -1, None);
    assert!(ctx.user_data().is_none());
}

#[test]
fn context_user_data_mut() {
    let mut ctx = HighlightContext::new(1, -1, None);
    ctx.set_user_data(Box::new(10u32));
    {
        let data = ctx.user_data_mut().unwrap().downcast_mut::<u32>().unwrap();
        *data = 20;
    }
    let val = ctx.user_data().unwrap().downcast_ref::<u32>().unwrap();
    assert_eq!(*val, 20);
}

#[test]
fn context_block_id() {
    let ctx = HighlightContext::new(99, -1, None);
    assert_eq!(ctx.block_id(), 99);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Fragment merge tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn highlight_full_block() {
    let doc = new_doc("Hello");
    let red = Color::rgb(255, 0, 0);
    let hl = Arc::new(ColorAllHighlighter { color: red });
    doc.set_syntax_highlighter(Some(hl));

    let frags = first_block_fragments(&doc);
    assert_eq!(frags.len(), 1);
    match &frags[0] {
        FragmentContent::Text { format, .. } => {
            assert_eq!(format.foreground_color, Some(red));
        }
        _ => panic!("expected text fragment"),
    }
}

#[test]
fn highlight_partial_splits_fragment() {
    let doc = new_doc("Hello world");
    // Highlight "llo w" (chars 2..7)
    struct PartialHighlighter;
    impl SyntaxHighlighter for PartialHighlighter {
        fn highlight_block(&self, _text: &str, ctx: &mut HighlightContext) {
            ctx.set_format(
                2,
                5,
                HighlightFormat {
                    foreground_color: Some(Color::rgb(255, 0, 0)),
                    ..Default::default()
                },
            );
        }
    }
    doc.set_syntax_highlighter(Some(Arc::new(PartialHighlighter)));

    let frags = first_block_fragments(&doc);
    // Should be split into 3: "He", "llo w", "orld"
    assert_eq!(frags.len(), 3);

    match &frags[0] {
        FragmentContent::Text {
            text,
            format,
            offset,
            length,
            ..
        } => {
            assert_eq!(text, "He");
            assert_eq!(*offset, 0);
            assert_eq!(*length, 2);
            assert_eq!(format.foreground_color, None);
        }
        _ => panic!("expected text"),
    }
    match &frags[1] {
        FragmentContent::Text {
            text,
            format,
            offset,
            length,
            ..
        } => {
            assert_eq!(text, "llo w");
            assert_eq!(*offset, 2);
            assert_eq!(*length, 5);
            assert_eq!(format.foreground_color, Some(Color::rgb(255, 0, 0)));
        }
        _ => panic!("expected text"),
    }
    match &frags[2] {
        FragmentContent::Text {
            text,
            format,
            offset,
            length,
            ..
        } => {
            assert_eq!(text, "orld");
            assert_eq!(*offset, 7);
            assert_eq!(*length, 4);
            assert_eq!(format.foreground_color, None);
        }
        _ => panic!("expected text"),
    }
}

#[test]
fn highlight_across_fragment_boundary() {
    // Create a document with two different format runs (via formatting)
    let doc = new_doc("AABB");
    let c = doc.cursor();
    c.set_position(0, MoveMode::MoveAnchor);
    c.set_position(2, MoveMode::KeepAnchor);
    c.set_char_format(&TextFormat {
        font_bold: Some(true),
        ..Default::default()
    })
    .unwrap();

    // Now highlight across the boundary: chars 1..3 ("AB")
    struct CrossBoundaryHighlighter;
    impl SyntaxHighlighter for CrossBoundaryHighlighter {
        fn highlight_block(&self, _text: &str, ctx: &mut HighlightContext) {
            ctx.set_format(
                1,
                2,
                HighlightFormat {
                    foreground_color: Some(Color::rgb(0, 0, 255)),
                    ..Default::default()
                },
            );
        }
    }
    doc.set_syntax_highlighter(Some(Arc::new(CrossBoundaryHighlighter)));

    let frags = first_block_fragments(&doc);
    // Original: [AA(bold), BB(normal)]
    // Highlight [1..3] crosses boundary
    // Result: A(bold,no-color), A(bold,blue), B(normal,blue), B(normal,no-color)
    assert!(frags.len() >= 3);

    // Find the blue fragments
    let blue_frags: Vec<_> = frags
        .iter()
        .filter(|f| match f {
            FragmentContent::Text { format, .. } => {
                format.foreground_color == Some(Color::rgb(0, 0, 255))
            }
            _ => false,
        })
        .collect();
    assert_eq!(blue_frags.len(), 2); // "A" (bold+blue) and "B" (normal+blue)
}

#[test]
fn highlight_multiple_non_overlapping() {
    let doc = new_doc("Hello world");
    struct TwoSpanHighlighter;
    impl SyntaxHighlighter for TwoSpanHighlighter {
        fn highlight_block(&self, _text: &str, ctx: &mut HighlightContext) {
            ctx.set_format(
                0,
                5,
                HighlightFormat {
                    foreground_color: Some(Color::rgb(255, 0, 0)),
                    ..Default::default()
                },
            );
            ctx.set_format(
                6,
                5,
                HighlightFormat {
                    foreground_color: Some(Color::rgb(0, 0, 255)),
                    ..Default::default()
                },
            );
        }
    }
    doc.set_syntax_highlighter(Some(Arc::new(TwoSpanHighlighter)));

    let frags = first_block_fragments(&doc);
    // "Hello" (red), " " (no color), "world" (blue)
    assert_eq!(frags.len(), 3);
    match &frags[0] {
        FragmentContent::Text { text, format, .. } => {
            assert_eq!(text, "Hello");
            assert_eq!(format.foreground_color, Some(Color::rgb(255, 0, 0)));
        }
        _ => panic!("expected text"),
    }
    match &frags[1] {
        FragmentContent::Text { text, format, .. } => {
            assert_eq!(text, " ");
            assert_eq!(format.foreground_color, None);
        }
        _ => panic!("expected text"),
    }
    match &frags[2] {
        FragmentContent::Text { text, format, .. } => {
            assert_eq!(text, "world");
            assert_eq!(format.foreground_color, Some(Color::rgb(0, 0, 255)));
        }
        _ => panic!("expected text"),
    }
}

#[test]
fn highlight_overlapping_last_wins() {
    let doc = new_doc("Hello");
    struct OverlapHighlighter;
    impl SyntaxHighlighter for OverlapHighlighter {
        fn highlight_block(&self, _text: &str, ctx: &mut HighlightContext) {
            // First span: entire text red
            ctx.set_format(
                0,
                5,
                HighlightFormat {
                    foreground_color: Some(Color::rgb(255, 0, 0)),
                    ..Default::default()
                },
            );
            // Second span: entire text blue — should win
            ctx.set_format(
                0,
                5,
                HighlightFormat {
                    foreground_color: Some(Color::rgb(0, 0, 255)),
                    ..Default::default()
                },
            );
        }
    }
    doc.set_syntax_highlighter(Some(Arc::new(OverlapHighlighter)));

    let frags = first_block_fragments(&doc);
    assert_eq!(frags.len(), 1);
    match &frags[0] {
        FragmentContent::Text { format, .. } => {
            assert_eq!(format.foreground_color, Some(Color::rgb(0, 0, 255)));
        }
        _ => panic!("expected text"),
    }
}

#[test]
fn highlight_empty_block() {
    let doc = new_doc("");
    let hl = Arc::new(ColorAllHighlighter {
        color: Color::rgb(255, 0, 0),
    });
    doc.set_syntax_highlighter(Some(hl));
    // Should not crash
    let frags = first_block_fragments(&doc);
    // Empty block has no text fragments
    assert!(
        frags.is_empty()
            || frags.iter().all(|f| match f {
                FragmentContent::Text { length, .. } => *length == 0,
                _ => true,
            })
    );
}

#[test]
fn highlight_image_fragment() {
    let doc = new_doc("AB");
    // Insert an image between A and B
    let c = doc.cursor_at(1);
    c.insert_image("test.png", 100, 100).unwrap();

    // Highlight the entire block (text + image)
    struct FullHighlighter;
    impl SyntaxHighlighter for FullHighlighter {
        fn highlight_block(&self, text: &str, ctx: &mut HighlightContext) {
            let len = text.chars().count();
            if len > 0 {
                ctx.set_format(
                    0,
                    len,
                    HighlightFormat {
                        foreground_color: Some(Color::rgb(255, 0, 0)),
                        ..Default::default()
                    },
                );
            }
        }
    }
    doc.set_syntax_highlighter(Some(Arc::new(FullHighlighter)));

    let frags = first_block_fragments(&doc);
    // Check that image fragment also gets the highlight color
    let image_frags: Vec<_> = frags
        .iter()
        .filter(|f| matches!(f, FragmentContent::Image { .. }))
        .collect();
    assert!(!image_frags.is_empty(), "expected image fragment");
    if let FragmentContent::Image { format, .. } = image_frags[0] {
        assert_eq!(format.foreground_color, Some(Color::rgb(255, 0, 0)));
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Shadow isolation tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn highlight_invisible_to_plain_text() {
    let doc = new_doc("Hello world");
    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter {
        color: Color::rgb(255, 0, 0),
    })));
    assert_eq!(doc.to_plain_text().unwrap(), "Hello world");
}

#[test]
fn highlight_invisible_to_entity_format() {
    let doc = new_doc("Hello");
    // Set real bold format
    let c = doc.cursor();
    c.set_position(0, MoveMode::MoveAnchor);
    c.set_position(5, MoveMode::KeepAnchor);
    c.set_char_format(&TextFormat {
        font_bold: Some(true),
        ..Default::default()
    })
    .unwrap();

    // Attach a highlighter that sets italic
    struct ItalicHighlighter;
    impl SyntaxHighlighter for ItalicHighlighter {
        fn highlight_block(&self, text: &str, ctx: &mut HighlightContext) {
            ctx.set_format(
                0,
                text.chars().count(),
                HighlightFormat {
                    font_italic: Some(true),
                    ..Default::default()
                },
            );
        }
    }
    doc.set_syntax_highlighter(Some(Arc::new(ItalicHighlighter)));

    // Read the entity format via cursor — should NOT have italic
    let read_c = doc.cursor_at(0);
    let entity_fmt = read_c.char_format().unwrap();
    assert_eq!(entity_fmt.font_bold, Some(true));
    // char_format reads from format_runs, not from merged fragments;
    // italic should NOT be there (it's shadow only)
    assert_ne!(entity_fmt.font_italic, Some(true));
}

#[test]
fn highlight_visible_in_snapshot() {
    let doc = new_doc("Hello");
    let red = Color::rgb(255, 0, 0);
    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter { color: red })));

    let block = match &doc.flow()[0] {
        FlowElement::Block(b) => b.clone(),
        _ => panic!("expected block"),
    };
    let snap = block.snapshot();
    // Paint-only highlighter: colors ride in `paint_highlights`, not merged
    // into `fragments` (which stay base for stable shaping).
    assert_eq!(snap.paint_highlights.len(), 1);
    assert_eq!(snap.paint_highlights[0].start, 0);
    assert_eq!(snap.paint_highlights[0].length, 5);
    assert_eq!(snap.paint_highlights[0].foreground_color, Some(red));
}

#[test]
fn highlight_visible_in_flow_snapshot() {
    let doc = new_doc("Hello");
    let red = Color::rgb(255, 0, 0);
    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter { color: red })));

    let snap = doc.snapshot_flow();
    match &snap.elements[0] {
        FlowElementSnapshot::Block(bs) => {
            assert_eq!(bs.paint_highlights.len(), 1);
            assert_eq!(bs.paint_highlights[0].foreground_color, Some(red));
        }
        _ => panic!("expected block snapshot"),
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// set_syntax_highlighter / rehighlight tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn set_highlighter_triggers_full_rehighlight() {
    let doc = new_doc("Hello");
    let red = Color::rgb(255, 0, 0);
    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter { color: red })));

    let frags = first_block_fragments(&doc);
    match &frags[0] {
        FragmentContent::Text { format, .. } => {
            assert_eq!(format.foreground_color, Some(red));
        }
        _ => panic!("expected text"),
    }
}

#[test]
fn remove_highlighter_clears_highlights() {
    let doc = new_doc("Hello");
    let red = Color::rgb(255, 0, 0);
    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter { color: red })));

    // Verify highlight is there
    let frags = first_block_fragments(&doc);
    match &frags[0] {
        FragmentContent::Text { format, .. } => {
            assert_eq!(format.foreground_color, Some(red));
        }
        _ => panic!("expected text"),
    }

    // Remove highlighter
    doc.set_syntax_highlighter(None);

    // Verify highlight is gone
    let frags = first_block_fragments(&doc);
    match &frags[0] {
        FragmentContent::Text { format, .. } => {
            assert_eq!(format.foreground_color, None);
        }
        _ => panic!("expected text"),
    }
}

#[test]
fn replace_highlighter() {
    let doc = new_doc("Hello");
    let red = Color::rgb(255, 0, 0);
    let blue = Color::rgb(0, 0, 255);

    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter { color: red })));
    let frags = first_block_fragments(&doc);
    match &frags[0] {
        FragmentContent::Text { format, .. } => {
            assert_eq!(format.foreground_color, Some(red));
        }
        _ => panic!("expected text"),
    }

    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter { color: blue })));
    let frags = first_block_fragments(&doc);
    match &frags[0] {
        FragmentContent::Text { format, .. } => {
            assert_eq!(format.foreground_color, Some(blue));
        }
        _ => panic!("expected text"),
    }
}

#[test]
fn rehighlight_refreshes_all_blocks() {
    let doc = new_doc("Hello\nworld");
    let count = Arc::new(AtomicUsize::new(0));
    doc.set_syntax_highlighter(Some(Arc::new(CountingHighlighter {
        count: count.clone(),
    })));
    let initial = count.load(Ordering::SeqCst);
    assert!(initial >= 2); // at least 2 blocks

    count.store(0, Ordering::SeqCst);
    doc.rehighlight();
    assert!(count.load(Ordering::SeqCst) >= 2);
}

#[test]
fn rehighlight_block_refreshes_one_and_cascades() {
    let doc = new_doc("Hello\nworld");
    let count = Arc::new(AtomicUsize::new(0));
    doc.set_syntax_highlighter(Some(Arc::new(CountingHighlighter {
        count: count.clone(),
    })));

    count.store(0, Ordering::SeqCst);
    let bid = first_block_id(&doc);
    doc.rehighlight_block(bid);
    // Should highlight at least the first block, and cascade stops when state
    // stabilizes (state is always -1 for CountingHighlighter, so it highlights
    // the first block then stops at the second since state didn't change).
    assert!(count.load(Ordering::SeqCst) >= 1);
}

#[test]
fn rehighlight_without_highlighter_is_noop() {
    let doc = new_doc("Hello");
    // Should not crash
    doc.rehighlight();
    doc.rehighlight_block(0);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Auto re-highlighting tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn insert_text_triggers_rehighlight() {
    let doc = new_doc("Hello");
    doc.set_syntax_highlighter(Some(Arc::new(WordBoldHighlighter {
        word: "Hola".into(),
    })));

    // No bold yet — "Hola" not in "Hello"
    let frags = first_block_fragments(&doc);
    for f in &frags {
        if let FragmentContent::Text { format, .. } = f {
            assert_ne!(format.font_bold, Some(true));
        }
    }

    // Insert "Hola" — triggers rehighlight
    let c = doc.cursor_at(5);
    c.insert_text(" Hola").unwrap();

    let frags = first_block_fragments(&doc);
    let has_bold = frags.iter().any(|f| match f {
        FragmentContent::Text { format, .. } => format.font_bold == Some(true),
        _ => false,
    });
    assert!(has_bold, "expected bold fragment after inserting 'Hola'");
}

#[test]
fn delete_text_triggers_rehighlight() {
    let doc = new_doc("Hello Hola");
    doc.set_syntax_highlighter(Some(Arc::new(WordBoldHighlighter {
        word: "Hola".into(),
    })));

    // "Hola" is present → should be bold
    let frags = first_block_fragments(&doc);
    let has_bold = frags.iter().any(|f| match f {
        FragmentContent::Text { format, .. } => format.font_bold == Some(true),
        _ => false,
    });
    assert!(has_bold);

    // Delete "Hola" (chars 6..10)
    let c = doc.cursor();
    c.set_position(6, MoveMode::MoveAnchor);
    c.set_position(10, MoveMode::KeepAnchor);
    c.remove_selected_text().unwrap();

    // No more bold
    let frags = first_block_fragments(&doc);
    let has_bold = frags.iter().any(|f| match f {
        FragmentContent::Text { format, .. } => format.font_bold == Some(true),
        _ => false,
    });
    assert!(!has_bold, "expected no bold after deleting 'Hola'");
}

#[test]
fn undo_triggers_rehighlight() {
    let doc = new_doc("Hello");
    doc.set_syntax_highlighter(Some(Arc::new(WordBoldHighlighter {
        word: "Hola".into(),
    })));

    let c = doc.cursor_at(5);
    c.insert_text(" Hola").unwrap();

    // Bold present
    let has_bold = first_block_fragments(&doc).iter().any(|f| match f {
        FragmentContent::Text { format, .. } => format.font_bold == Some(true),
        _ => false,
    });
    assert!(has_bold);

    // Undo
    doc.undo().unwrap();

    // Bold gone
    let has_bold = first_block_fragments(&doc).iter().any(|f| match f {
        FragmentContent::Text { format, .. } => format.font_bold == Some(true),
        _ => false,
    });
    assert!(!has_bold, "expected no bold after undo");
}

#[test]
fn redo_triggers_rehighlight() {
    let doc = new_doc("Hello");
    doc.set_syntax_highlighter(Some(Arc::new(WordBoldHighlighter {
        word: "Hola".into(),
    })));

    let c = doc.cursor_at(5);
    c.insert_text(" Hola").unwrap();
    doc.undo().unwrap();
    doc.redo().unwrap();

    let has_bold = first_block_fragments(&doc).iter().any(|f| match f {
        FragmentContent::Text { format, .. } => format.font_bold == Some(true),
        _ => false,
    });
    assert!(has_bold, "expected bold after redo");
}

#[test]
fn set_plain_text_triggers_rehighlight() {
    let doc = new_doc("Hello");
    doc.set_syntax_highlighter(Some(Arc::new(WordBoldHighlighter {
        word: "Hola".into(),
    })));

    // Replace entire text
    doc.set_plain_text("Say Hola").unwrap();

    let frags = first_block_fragments(&doc);
    let has_bold = frags.iter().any(|f| match f {
        FragmentContent::Text { format, .. } => format.font_bold == Some(true),
        _ => false,
    });
    assert!(has_bold, "expected bold after set_plain_text with 'Hola'");
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Multi-block cascade tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn state_change_cascades_to_next_block() {
    // "/* start" on line 1 opens a comment → state=1
    // "middle" on line 2 should be highlighted as comment
    let doc = new_doc("/* start\nmiddle\nend */\nnormal");
    doc.set_syntax_highlighter(Some(Arc::new(MultiLineCommentHighlighter)));

    let flow = doc.flow();
    let green = Color::rgb(0, 128, 0);

    // Block 0: "/* start" — fully green
    if let FlowElement::Block(b) = &flow[0] {
        let frags = b.display_fragments();
        assert!(frags.iter().all(|f| match f {
            FragmentContent::Text { format, .. } => format.foreground_color == Some(green),
            _ => true,
        }));
    }

    // Block 1: "middle" — fully green (inside comment)
    if let FlowElement::Block(b) = &flow[1] {
        let frags = b.display_fragments();
        assert!(frags.iter().all(|f| match f {
            FragmentContent::Text { format, .. } => format.foreground_color == Some(green),
            _ => true,
        }));
    }

    // Block 3: "normal" — not green
    if let FlowElement::Block(b) = &flow[3] {
        let frags = b.display_fragments();
        assert!(frags.iter().all(|f| match f {
            FragmentContent::Text { format, .. } => format.foreground_color != Some(green),
            _ => true,
        }));
    }
}

#[test]
fn stable_state_stops_cascade() {
    let doc = new_doc("normal\nalso normal\nstill normal");
    let count = Arc::new(AtomicUsize::new(0));

    struct StableStateHighlighter {
        count: Arc<AtomicUsize>,
    }
    impl SyntaxHighlighter for StableStateHighlighter {
        fn highlight_block(&self, _text: &str, ctx: &mut HighlightContext) {
            self.count.fetch_add(1, Ordering::SeqCst);
            ctx.set_current_block_state(0); // Always stable
        }
    }

    doc.set_syntax_highlighter(Some(Arc::new(StableStateHighlighter {
        count: count.clone(),
    })));

    let initial = count.load(Ordering::SeqCst);
    assert_eq!(initial, 3); // Full rehighlight hits all 3 blocks

    // Edit first block — should rehighlight block 0, then stop at block 1
    // since state didn't change.
    count.store(0, Ordering::SeqCst);
    let c = doc.cursor_at(0);
    c.insert_text("X").unwrap();

    let after_edit = count.load(Ordering::SeqCst);
    // Block 0 is always rehighlighted. Block 1 is rehighlighted to check
    // if state changed (it didn't), so cascade stops.
    assert!(
        after_edit <= 2,
        "cascade should stop early, got {after_edit} calls"
    );
}

#[test]
fn cascade_through_multiple_blocks() {
    // Opening /* on first block cascades through all remaining blocks.
    let doc = new_doc("/*\nline2\nline3\nline4");
    doc.set_syntax_highlighter(Some(Arc::new(MultiLineCommentHighlighter)));

    let green = Color::rgb(0, 128, 0);
    let flow = doc.flow();

    for (i, element) in flow.iter().enumerate() {
        if let FlowElement::Block(b) = element {
            let frags = b.display_fragments();
            let all_green = frags.iter().all(|f| match f {
                FragmentContent::Text { format, .. } => format.foreground_color == Some(green),
                _ => true,
            });
            assert!(all_green, "block {i} should be green (inside comment)");
        }
    }
}

#[test]
fn cascade_terminates_at_document_end() {
    // Comment never closes — cascade reaches end of document without crash.
    let doc = new_doc("/*\nstill open");
    doc.set_syntax_highlighter(Some(Arc::new(MultiLineCommentHighlighter)));

    let green = Color::rgb(0, 128, 0);
    let flow = doc.flow();

    for element in &flow {
        if let FlowElement::Block(b) = element {
            let frags = b.display_fragments();
            assert!(frags.iter().all(|f| match f {
                FragmentContent::Text { format, .. } => format.foreground_color == Some(green),
                _ => true,
            }));
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// User data persistence tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn user_data_persists_across_rehighlights() {
    let doc = new_doc("Hello\nworld");
    let observations = Arc::new(Mutex::new(Vec::new()));
    doc.set_syntax_highlighter(Some(Arc::new(UserDataHighlighter {
        observations: observations.clone(),
    })));

    // Initial highlight pass: no user data exists yet, so every block
    // observed on entry must see 0.
    let initial_obs = observations.lock().unwrap().clone();
    assert!(
        !initial_obs.is_empty(),
        "highlighter should have been invoked on initial attach"
    );
    assert!(
        initial_obs.iter().all(|&v| v == 0),
        "initial pass must see no stored user data, got {:?}",
        initial_obs
    );
    let initial_calls = initial_obs.len();

    // Edit-triggered incremental rehighlight must read back the value
    // stored on the previous pass. This is the actual persistence
    // contract — full `rehighlight()` intentionally resets state.
    let c = doc.cursor_at(0);
    c.insert_text("X").unwrap();
    let after_edit = observations.lock().unwrap().clone();
    let new_obs: Vec<u32> = after_edit[initial_calls..].to_vec();
    assert!(
        !new_obs.is_empty(),
        "edit should have re-invoked the highlighter"
    );
    assert!(
        new_obs.iter().all(|&v| v >= 1),
        "incremental rehighlight must read back the previously-stored counter (>= 1), got {:?}",
        new_obs
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Integration test: multiline comment highlighter
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn multiline_comment_full_integration() {
    let doc = new_doc("int x;\n/* comment\nstill comment */\nint y;");
    doc.set_syntax_highlighter(Some(Arc::new(MultiLineCommentHighlighter)));

    let green = Color::rgb(0, 128, 0);
    let flow = doc.flow();

    // Block 0: "int x;" — no highlight
    if let FlowElement::Block(b) = &flow[0] {
        let frags = b.display_fragments();
        assert!(
            frags.iter().all(|f| match f {
                FragmentContent::Text { format, .. } => format.foreground_color != Some(green),
                _ => true,
            }),
            "block 0 should not be green"
        );
    }

    // Block 1: "/* comment" — green
    if let FlowElement::Block(b) = &flow[1] {
        let frags = b.display_fragments();
        assert!(
            frags.iter().all(|f| match f {
                FragmentContent::Text { format, .. } => format.foreground_color == Some(green),
                _ => true,
            }),
            "block 1 should be green"
        );
    }

    // Block 2: "still comment */" — green (closes at end)
    if let FlowElement::Block(b) = &flow[2] {
        let frags = b.display_fragments();
        assert!(
            frags.iter().all(|f| match f {
                FragmentContent::Text { format, .. } => format.foreground_color == Some(green),
                _ => true,
            }),
            "block 2 should be green"
        );
    }

    // Block 3: "int y;" — no highlight
    if let FlowElement::Block(b) = &flow[3] {
        let frags = b.display_fragments();
        assert!(
            frags.iter().all(|f| match f {
                FragmentContent::Text { format, .. } => format.foreground_color != Some(green),
                _ => true,
            }),
            "block 3 should not be green"
        );
    }

    // Now edit: remove the closing */ from block 2 to cascade the comment
    // into block 3.
    // Block 2 text is "still comment */" — remove " */" (last 3 chars)
    if let FlowElement::Block(b) = &flow[2] {
        let pos = b.position();
        let len = b.length();
        let c = doc.cursor();
        c.set_position(pos + len - 3, MoveMode::MoveAnchor);
        c.set_position(pos + len, MoveMode::KeepAnchor);
        c.remove_selected_text().unwrap();
    }

    // Now block 3 should be green (comment cascaded)
    let flow = doc.flow();
    if let FlowElement::Block(b) = &flow[3] {
        let frags = b.display_fragments();
        assert!(
            frags.iter().all(|f| match f {
                FragmentContent::Text { format, .. } => format.foreground_color == Some(green),
                _ => true,
            }),
            "block 3 should be green after removing closing */"
        );
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Highlight with underline style (spellcheck use case)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn spellcheck_underline_highlight() {
    let doc = new_doc("Hello wrold");

    struct SpellcheckHighlighter;
    impl SyntaxHighlighter for SpellcheckHighlighter {
        fn highlight_block(&self, text: &str, ctx: &mut HighlightContext) {
            // Pretend "wrold" at offset 6 is misspelled
            if let Some(pos) = text.find("wrold") {
                let char_pos = text[..pos].chars().count();
                ctx.set_format(
                    char_pos,
                    5,
                    HighlightFormat {
                        underline_style: Some(UnderlineStyle::SpellCheckUnderline),
                        underline_color: Some(Color::rgb(255, 0, 0)),
                        ..Default::default()
                    },
                );
            }
        }
    }

    doc.set_syntax_highlighter(Some(Arc::new(SpellcheckHighlighter)));

    let frags = first_block_fragments(&doc);
    let spellcheck_frag = frags.iter().find(|f| match f {
        FragmentContent::Text { format, .. } => {
            format.underline_style == Some(UnderlineStyle::SpellCheckUnderline)
        }
        _ => false,
    });
    assert!(
        spellcheck_frag.is_some(),
        "expected spellcheck underline on 'wrold'"
    );

    if let Some(FragmentContent::Text { text, format, .. }) = spellcheck_frag {
        assert_eq!(text, "wrold");
        assert_eq!(format.underline_color, Some(Color::rgb(255, 0, 0)));
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Paint-only fast path: classification, snapshot split, events
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// A metric-affecting highlighter (bolds the whole block).
struct BoldAllHighlighter;
impl SyntaxHighlighter for BoldAllHighlighter {
    fn highlight_block(&self, text: &str, ctx: &mut HighlightContext) {
        let len = text.chars().count();
        if len > 0 {
            ctx.set_format(
                0,
                len,
                HighlightFormat {
                    font_bold: Some(true),
                    ..Default::default()
                },
            );
        }
    }
}

/// Base (shaping-input) fragments of the first block.
fn first_block_base_fragments(doc: &TextDocument) -> Vec<FragmentContent> {
    match &doc.flow()[0] {
        FlowElement::Block(b) => b.fragments(),
        _ => panic!("expected block"),
    }
}

/// Drain pending events, returning only highlight-relevant ones.
fn drain_highlight_events(doc: &TextDocument) -> Vec<DocumentEvent> {
    doc.poll_events()
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                DocumentEvent::HighlightPaintChanged { .. } | DocumentEvent::FormatChanged { .. }
            )
        })
        .collect()
}

#[test]
fn paint_only_keeps_fragments_base_and_fills_paint_spans() {
    let doc = new_doc("Hello");
    let red = Color::rgb(255, 0, 0);
    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter { color: red })));

    // Shaping input stays base — no color merged.
    let base = first_block_base_fragments(&doc);
    assert_eq!(base.len(), 1);
    match &base[0] {
        FragmentContent::Text { format, .. } => assert_eq!(format.foreground_color, None),
        _ => panic!("expected text"),
    }
    // The color lives in the paint overlay.
    let spans = first_block_paint_spans(&doc);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].start, 0);
    assert_eq!(spans[0].length, 5);
    assert_eq!(spans[0].foreground_color, Some(red));
}

#[test]
fn metric_highlighter_merges_fragments_and_has_no_paint_spans() {
    let doc = new_doc("Hello");
    doc.set_syntax_highlighter(Some(Arc::new(BoldAllHighlighter)));

    // Bold IS metric: merged into the shaping input.
    let base = first_block_base_fragments(&doc);
    match &base[0] {
        FragmentContent::Text { format, .. } => assert_eq!(format.font_bold, Some(true)),
        _ => panic!("expected text"),
    }
    // No separate paint overlay for a metric highlighter.
    assert!(first_block_paint_spans(&doc).is_empty());
}

#[test]
fn no_highlighter_has_empty_paint_spans() {
    let doc = new_doc("Hello");
    assert!(first_block_paint_spans(&doc).is_empty());
}

#[test]
fn paint_only_overlapping_last_wins_in_paint_spans() {
    let doc = new_doc("Hello");
    struct OverlapHighlighter;
    impl SyntaxHighlighter for OverlapHighlighter {
        fn highlight_block(&self, _text: &str, ctx: &mut HighlightContext) {
            ctx.set_format(
                0,
                5,
                HighlightFormat {
                    foreground_color: Some(Color::rgb(255, 0, 0)),
                    ..Default::default()
                },
            );
            ctx.set_format(
                0,
                5,
                HighlightFormat {
                    foreground_color: Some(Color::rgb(0, 0, 255)),
                    ..Default::default()
                },
            );
        }
    }
    doc.set_syntax_highlighter(Some(Arc::new(OverlapHighlighter)));
    let spans = first_block_paint_spans(&doc);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].foreground_color, Some(Color::rgb(0, 0, 255)));
}

#[test]
fn paint_highlight_invisible_to_char_format() {
    let doc = new_doc("Hello");
    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter {
        color: Color::rgb(255, 0, 0),
    })));
    // char_format reads stored formatting, never the highlight overlay.
    let fmt = doc.cursor_at(0).char_format().unwrap();
    assert_eq!(fmt.foreground_color, None);
}

#[test]
fn set_paint_only_emits_highlight_paint_changed() {
    let doc = new_doc("Hello");
    let _ = doc.poll_events(); // drain construction events
    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter {
        color: Color::rgb(255, 0, 0),
    })));
    let events = drain_highlight_events(&doc);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        DocumentEvent::HighlightPaintChanged { .. }
    ));
}

#[test]
fn set_metric_emits_format_changed() {
    let doc = new_doc("Hello");
    let _ = doc.poll_events();
    doc.set_syntax_highlighter(Some(Arc::new(BoldAllHighlighter)));
    let events = drain_highlight_events(&doc);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], DocumentEvent::FormatChanged { .. }));
}

#[test]
fn remove_paint_only_emits_highlight_paint_changed() {
    let doc = new_doc("Hello");
    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter {
        color: Color::rgb(255, 0, 0),
    })));
    let _ = doc.poll_events();
    doc.set_syntax_highlighter(None);
    let events = drain_highlight_events(&doc);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        DocumentEvent::HighlightPaintChanged { .. }
    ));
}

#[test]
fn remove_metric_emits_format_changed() {
    let doc = new_doc("Hello");
    doc.set_syntax_highlighter(Some(Arc::new(BoldAllHighlighter)));
    let _ = doc.poll_events();
    doc.set_syntax_highlighter(None);
    let events = drain_highlight_events(&doc);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], DocumentEvent::FormatChanged { .. }));
}

#[test]
fn paint_to_metric_emits_format_changed() {
    let doc = new_doc("Hello");
    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter {
        color: Color::rgb(255, 0, 0),
    })));
    let _ = doc.poll_events();
    doc.set_syntax_highlighter(Some(Arc::new(BoldAllHighlighter)));
    let events = drain_highlight_events(&doc);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], DocumentEvent::FormatChanged { .. }));
}

#[test]
fn metric_to_paint_emits_format_changed() {
    let doc = new_doc("Hello");
    doc.set_syntax_highlighter(Some(Arc::new(BoldAllHighlighter)));
    let _ = doc.poll_events();
    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter {
        color: Color::rgb(255, 0, 0),
    })));
    let events = drain_highlight_events(&doc);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], DocumentEvent::FormatChanged { .. }));
}

#[test]
fn rehighlight_paint_only_emits_highlight_paint_changed() {
    let doc = new_doc("Hello");
    doc.set_syntax_highlighter(Some(Arc::new(ColorAllHighlighter {
        color: Color::rgb(255, 0, 0),
    })));
    let _ = doc.poll_events();
    doc.rehighlight();
    let events = drain_highlight_events(&doc);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        DocumentEvent::HighlightPaintChanged { .. }
    ));
}
