//! `snapshot_flow_masked_no_paint` is the accessibility path's snapshot. It must
//! produce **byte-identical fragments** to the full masked snapshot — the AT tree
//! reads those fragments and would report different text runs if they diverged —
//! differing only in `paint_highlights`, which it leaves empty (the AT walk never
//! reads the paint overlay, so recomputing it there was pure waste).

use text_document::{
    Color, FlowElementSnapshot, HighlightFormat, HighlightMask, RangeHighlight, TextDocument,
    UnderlineStyle,
};

#[test]
fn no_paint_snapshot_keeps_fragments_and_drops_the_overlay() {
    let doc = TextDocument::new();
    doc.set_plain_text("The quikc brown fox\n\njumpd over the lazy dog")
        .unwrap();

    let session = doc.add_range_session();
    let fmt = HighlightFormat {
        underline_style: Some(UnderlineStyle::SpellCheckUnderline),
        underline_color: Some(Color { red: 220, green: 50, blue: 50, alpha: 255 }),
        ..Default::default()
    };
    // Two misspelled words, in different blocks, so both blocks carry an overlay.
    doc.set_session_ranges(
        session,
        vec![
            RangeHighlight { start: 4, length: 5, format: fmt.clone() }, // "quikc"
            RangeHighlight { start: 21, length: 5, format: fmt.clone() }, // "jumpd"
        ],
    );

    let all = HighlightMask::all();
    let full = doc.snapshot_flow_masked(&all);
    let no_paint = doc.snapshot_flow_masked_no_paint(&all);

    assert_eq!(
        full.elements.len(),
        no_paint.elements.len(),
        "the two snapshots must have the same structure"
    );

    let mut full_overlay_seen = false;
    for (a, b) in full.elements.iter().zip(no_paint.elements.iter()) {
        if let (FlowElementSnapshot::Block(fa), FlowElementSnapshot::Block(fb)) = (a, b) {
            assert_eq!(fa.text, fb.text, "block text must match");
            assert_eq!(fa.position, fb.position, "block position must match");
            assert_eq!(
                fa.fragments, fb.fragments,
                "fragments must be byte-identical — the AT tree reads these"
            );
            assert!(
                fb.paint_highlights.is_empty(),
                "the no-paint snapshot must carry no paint overlay"
            );
            full_overlay_seen |= !fa.paint_highlights.is_empty();
        }
    }

    assert!(
        full_overlay_seen,
        "the full snapshot must actually carry a paint overlay, or this proves nothing"
    );
}
