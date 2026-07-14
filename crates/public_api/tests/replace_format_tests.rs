//! What a replace does to the formatting it overwrites — end to end, through the real
//! document.
//!
//! There were 11 replace tests in this repo and **none** of them mentioned formatting.
//! The behaviour was emergent: a replace was a delete followed by an insert, so the
//! replacement inherited whatever run preceded it and every run *under* the range was
//! simply dropped. For a writer that means renaming a character whose name is styled —
//! `*Aurélien*` — silently loses the styling, across every occurrence, in one action,
//! with autosave writing the result to disk seconds later.
//!
//! These tests pin all four policies against real Djot, so the choice is visible in the
//! markup a writer would actually get back.

use text_document::{
    DjotExportOptions, DjotImportOptions, FindOptions, ReplaceFormatPolicy, ReplaceOptions,
    TextDocument,
};

/// Load `djot`, replace `query` with `replacement` under `policy`, and hand back the
/// re-exported Djot — i.e. exactly what would be written to the writer's file.
fn replace_and_export(
    djot: &str,
    query: &str,
    replacement: &str,
    policy: ReplaceFormatPolicy,
) -> String {
    let doc = TextDocument::new();
    doc.set_djot_with_options(djot, DjotImportOptions::default())
        .and_then(|op| op.wait())
        .expect("set_djot");

    let opts = ReplaceOptions::new(FindOptions::default()).with_format_policy(policy);
    let count = doc
        .replace_text(query, replacement, true, &opts)
        .expect("replace_text");
    assert!(count > 0, "the query must actually match");

    doc.to_djot_with_options(DjotExportOptions::default())
        .expect("to_djot")
        .trim()
        .to_string()
}

/// **The data loss, made visible.** The name is entirely inside a strong run, so the
/// replacement lands on a run that begins *exactly* where it does — and the historical
/// rule ("inherit the run that ends at or straddles the start") sees nothing to inherit.
/// The emphasis is gone from the writer's file.
///
/// This is not a bug being fixed; it is the shipped behaviour being *pinned*, so that
/// choosing it is now a decision rather than an accident.
#[test]
fn the_default_policy_drops_the_styling_it_overwrites() {
    let out = replace_and_export(
        "She called *Aurélien* into the trees.",
        "Aurélien",
        "Aurélian",
        ReplaceFormatPolicy::InheritPreceding,
    );
    assert_eq!(
        out, "She called Aurélian into the trees.",
        "the historical default loses the emphasis — pinned so it cannot change silently"
    );
}

/// The policy a character rename actually wants: the name was wholly styled, so the new
/// name is too.
#[test]
fn preserve_if_fully_covered_keeps_a_wholly_styled_name() {
    let out = replace_and_export(
        "She called *Aurélien* into the trees.",
        "Aurélien",
        "Aurélian",
        ReplaceFormatPolicy::PreserveIfFullyCovered,
    );
    assert_eq!(
        out, "She called *Aurélian* into the trees.",
        "a name that was entirely emphasised must stay emphasised across the rename"
    );
}

/// …but it refuses to *guess*. When the range is only partly styled, no single run
/// covers it, so it falls back to inheritance rather than inventing a format for text
/// the writer never styled that way.
///
/// Note the query is **plain text**: the search runs against the parsed prose, not the
/// Djot source, so markup never matches and never shifts an offset. Only the surname is
/// emphasised, and the replaced range spans both words.
#[test]
fn preserve_if_fully_covered_does_not_guess_on_a_partly_styled_range() {
    let out = replace_and_export(
        "She called Aurélien *Dubois* home.",
        "Aurélien Dubois",
        "Aurélien Duval",
        ReplaceFormatPolicy::PreserveIfFullyCovered,
    );
    assert_eq!(
        out, "She called Aurélien Duval home.",
        "partly styled → fall back, do not paint the whole replacement with a style that \
         only covered part of it"
    );
}

/// `PreserveNothing` means what it says, even when the range was wholly styled.
#[test]
fn preserve_nothing_strips_the_styling() {
    let out = replace_and_export(
        "She called *Aurélien* into the trees.",
        "Aurélien",
        "Aurélian",
        ReplaceFormatPolicy::PreserveNothing,
    );
    assert_eq!(out, "She called Aurélian into the trees.");
}

/// `KeepDominantRun` keeps the styling that covered most of what it replaced.
#[test]
fn keep_dominant_run_keeps_the_style_that_covered_most_of_the_range() {
    let out = replace_and_export(
        "She called *Aurélien* into the trees.",
        "Aurélien",
        "Aurélian",
        ReplaceFormatPolicy::KeepDominantRun,
    );
    assert_eq!(
        out, "She called *Aurélian* into the trees.",
        "the emphasis covered the whole name, so it dominates"
    );
}

/// Text *around* the replaced range must never be disturbed, whatever the policy. A
/// rename that reformatted the rest of the sentence would be far worse than one that
/// lost a style.
#[test]
fn no_policy_disturbs_the_formatting_around_the_replacement() {
    for policy in [
        ReplaceFormatPolicy::InheritPreceding,
        ReplaceFormatPolicy::PreserveIfFullyCovered,
        ReplaceFormatPolicy::KeepDominantRun,
        ReplaceFormatPolicy::PreserveNothing,
    ] {
        let out = replace_and_export(
            "A *bold* word, then Aurélien, then _emphasis_.",
            "Aurélien",
            "Aurélian",
            policy,
        );
        assert!(
            out.contains("*bold*"),
            "{policy:?} damaged the strong run before the replacement: {out:?}"
        );
        assert!(
            out.contains("_emphasis_"),
            "{policy:?} damaged the emphasis after the replacement: {out:?}"
        );
        assert!(
            out.contains("Aurélian"),
            "{policy:?} did not perform the replacement: {out:?}"
        );
    }
}

/// A replace is still undoable, and undo must restore the *formatting* too — not just
/// the characters. A rename that could not be taken back cleanly is the single most
/// destructive thing this API offers.
#[test]
fn undo_restores_the_formatting_a_replace_changed() {
    let source = "She called *Aurélien* into the trees.";
    let doc = TextDocument::new();
    doc.set_djot_with_options(source, DjotImportOptions::default())
        .and_then(|op| op.wait())
        .expect("set_djot");

    let opts = ReplaceOptions::new(FindOptions::default())
        .with_format_policy(ReplaceFormatPolicy::PreserveIfFullyCovered);
    doc.replace_text("Aurélien", "Aurélian", true, &opts)
        .expect("replace");
    assert_eq!(
        doc.to_djot_with_options(DjotExportOptions::default())
            .unwrap()
            .trim(),
        "She called *Aurélian* into the trees."
    );

    doc.undo().expect("undo");
    assert_eq!(
        doc.to_djot_with_options(DjotExportOptions::default())
            .unwrap()
            .trim(),
        source,
        "undo must restore the prose AND its emphasis, exactly"
    );
}
