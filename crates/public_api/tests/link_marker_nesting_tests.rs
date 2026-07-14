//! A formatted link must survive being saved.
//!
//! `render_inline_segments` applied the link **outermost**, so a superscript link was written
//! `[^aa^](url)` — the `^` markers inside the brackets. But `[^…]` is djot's
//! **footnote-reference** syntax. The re-parse read it as a footnote, and the link was gone:
//!
//! ```text
//! typed     ^[aa](https://x/)^      a link, in superscript
//! saved     [^aa^](https://x/)      ← re-parses as a footnote reference
//! resaved   \(https://x/\)          ← the link text is simply GONE
//! ```
//!
//! That is silent, total data loss in the format a manuscript is *saved* in — one autosave
//! and the writer's link is a pair of escaped brackets. It survived because nothing had ever
//! written a link and a superscript over the same run; `djot_roundtrip_tests`' fixpoint
//! property found it, but only above the default 256 cases.
//!
//! The fix is the one nesting that collides with nothing: the link goes **innermost**, and
//! the marks wrap it.

use text_document::TextDocument;

fn to_djot(djot: &str) -> String {
    let doc = TextDocument::new();
    doc.set_djot(djot).unwrap().wait().unwrap();
    doc.to_djot().unwrap().trim().to_string()
}

/// The bug, stated as the thing that actually matters: **the link text must still be there.**
/// A fixpoint check alone would pass on a round-trip that stably lost it.
#[test]
fn a_superscript_link_is_not_eaten_by_the_footnote_syntax() {
    let once = to_djot("^[aa](https://x/)^");
    assert!(
        !once.starts_with("[^"),
        "`[^…]` is a footnote reference, not a link label: {once:?}"
    );

    let twice = to_djot(&once);
    assert_eq!(once, twice, "export must be a fixpoint");
    assert!(
        twice.contains("aa") && twice.contains("https://x/"),
        "the link's text AND destination must both survive a re-save: {twice:?}"
    );
}

/// The counterexample proptest actually shrank to, kept verbatim.
#[test]
fn the_shrunk_counterexample_round_trips() {
    let src = "a~~ ~~^?[aa](https://aa.example/)^0a^";
    let once = to_djot(src);
    let twice = to_djot(&once);
    assert_eq!(
        once, twice,
        "export not a fixpoint\n  t1={once:?}\n  t2={twice:?}"
    );
    assert!(
        twice.contains("aa.example"),
        "the link destination must survive: {twice:?}"
    );
}

/// Every other mark over a link. None of these collided before, and none may start doing so
/// now that the nesting flipped.
#[test]
fn every_mark_over_a_link_round_trips_with_its_text_intact() {
    for src in [
        "[aa](https://x/)",     // no marks at all
        "*[aa](https://x/)*",   // bold
        "_[aa](https://x/)_",   // italic
        "~[aa](https://x/)~",   // subscript
        "^[aa](https://x/)^",   // superscript — the broken one
        "{-[aa](https://x/)-}", // strikeout
        "{+[aa](https://x/)+}", // insert
        "*_[aa](https://x/)_*", // bold + italic
    ] {
        let once = to_djot(src);
        let twice = to_djot(&once);
        assert_eq!(once, twice, "not a fixpoint for {src:?}");
        assert!(
            twice.contains("aa") && twice.contains("https://x/"),
            "{src:?} lost its link on re-save: {twice:?}"
        );
    }
}

/// A superscript with no link, and a link with no superscript, must be untouched by the
/// reorder — the fix must not move markers that were already in the right place.
#[test]
fn marks_without_a_link_are_unchanged() {
    assert_eq!(to_djot("a^0a^"), "a^0a^");
    assert_eq!(to_djot("a~0a~"), "a~0a~");
    assert_eq!(to_djot("*bold*"), "*bold*");
    assert_eq!(to_djot("[aa](https://x/)"), "[aa](https://x/)");
}
