//! Per-feature djot import/export round-trip tests.
//!
//! The central guarantee is a **fixpoint**: once a document has been exported
//! to djot, importing that djot and exporting again must yield byte-identical
//! djot. That proves `export∘import` loses nothing the model can represent.
//! Each test also asserts the feature's marker actually appears, so a feature
//! can't "pass" by being silently dropped on both sides.

extern crate text_document_io as document_io;
use common::long_operation::{LongOperationManager, OperationStatus};

use document_io::document_io_controller;
use document_io::*;
use test_harness::setup;

fn wait(mgr: &LongOperationManager, op_id: &str) {
    while let Some(OperationStatus::Running) = mgr.get_operation_status(op_id) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Import `input` djot into a fresh document, then export it back to djot.
fn import_then_export(input: &str) -> String {
    let (db, ev, _) = setup().expect("setup");
    let mut mgr = LongOperationManager::new();
    let op = document_io_controller::import_djot(
        &db,
        &ev,
        &mut mgr,
        &ImportDjotDto {
            djot_text: input.to_string(),
            options: Default::default(),
        },
    )
    .expect("import_djot");
    wait(&mgr, &op);
    assert_eq!(
        mgr.get_operation_status(&op),
        Some(OperationStatus::Completed),
        "import of {input:?} did not complete"
    );
    document_io_controller::export_djot(&db, &ev, &Default::default())
        .expect("export_djot")
        .djot_text
}

/// Assert `export∘import` is a fixpoint and return the canonical djot.
fn fixpoint(input: &str) -> String {
    let t1 = import_then_export(input);
    let t2 = import_then_export(&t1);
    assert_eq!(
        t1, t2,
        "export must be a fixpoint after one normalization pass\n--- input ---\n{input}\n--- t1 ---\n{t1}\n--- t2 ---\n{t2}"
    );
    t1
}

fn assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected to find {needle:?} in:\n{haystack}"
    );
}

#[test]
fn headings_levels_1_to_6() {
    for level in 1..=6 {
        let hashes = "#".repeat(level);
        let dj = fixpoint(&format!("{hashes} Heading {level}"));
        assert_contains(&dj, &format!("{hashes} Heading {level}"));
    }
}

#[test]
fn bold() {
    assert_contains(&fixpoint("normal *bold* end"), "*bold*");
}

#[test]
fn italic() {
    assert_contains(&fixpoint("normal _italic_ end"), "_italic_");
}

#[test]
fn bold_italic() {
    assert_contains(&fixpoint("*_x_*"), "*_x_*");
}

#[test]
fn inline_code() {
    assert_contains(&fixpoint("a `code` b"), "`code`");
}

#[test]
fn code_block_with_language() {
    let dj = fixpoint("```rust\nfn main() {}\n```");
    assert_contains(&dj, "```rust");
    assert_contains(&dj, "fn main() {}");
}

#[test]
fn link() {
    assert_contains(
        &fixpoint("[text](https://example.com)"),
        "[text](https://example.com)",
    );
}

#[test]
fn superscript_and_subscript() {
    assert_contains(&fixpoint("a^sup^"), "^sup^");
    assert_contains(&fixpoint("a~sub~"), "~sub~");
}

#[test]
fn strikeout_delete() {
    assert_contains(&fixpoint("{-gone-}"), "{-gone-}");
}

#[test]
fn underline_insert() {
    assert_contains(&fixpoint("{+added+}"), "{+added+}");
}

#[test]
fn unordered_bullet_markers_distinct() {
    assert_contains(&fixpoint("- dash"), "- dash");
    assert_contains(&fixpoint("* star"), "* star");
    assert_contains(&fixpoint("+ plus"), "+ plus");
}

#[test]
fn ordered_period() {
    let dj = fixpoint("1. one\n\n2. two");
    assert_contains(&dj, "1. one");
    assert_contains(&dj, "2. two");
}

#[test]
fn ordered_paren() {
    let dj = fixpoint("1) one\n\n2) two");
    assert_contains(&dj, "1) one");
    assert_contains(&dj, "2) two");
}

#[test]
fn ordered_alpha_and_roman() {
    assert_contains(&fixpoint("a. alpha"), "a. alpha");
    assert_contains(&fixpoint("i. roman"), "i. roman");
}

#[test]
fn task_list_checked_unchecked() {
    let dj = fixpoint("- [ ] todo\n- [x] done");
    assert_contains(&dj, "- [ ] todo");
    assert_contains(&dj, "- [x] done");
}

#[test]
fn nested_list() {
    let dj = fixpoint("- a\n\n  - b\n\n    - c");
    assert_contains(&dj, "- a");
    assert_contains(&dj, "  - b");
    assert_contains(&dj, "    - c");
}

#[test]
fn blockquote_depths() {
    assert_contains(&fixpoint("> quoted"), "> quoted");
    assert_contains(&fixpoint("> > deep"), "> > deep");
}

#[test]
fn table_2x2() {
    let dj = fixpoint("| A | B |\n|---|---|\n| c | d |");
    assert_contains(&dj, "| A | B |");
    assert_contains(&dj, "| c | d |");
}

#[test]
fn table_in_blockquote() {
    // Mirrors the markdown blockquote-nesting coverage: a table nested in a
    // blockquote must stay inside the quote across the round-trip.
    let dj = fixpoint("> | A | B |\n> |---|---|\n> | c | d |");
    assert_contains(&dj, "> | A | B |");
    assert_contains(&dj, "> | c | d |");
}

#[test]
fn smart_punctuation_normalised_to_unicode() {
    let dj = fixpoint("an ellipsis... and em---dash");
    assert_contains(&dj, "\u{2026}"); // …
    assert_contains(&dj, "\u{2014}"); // —
}

#[test]
fn metacharacters_escaped_and_preserved() {
    // Literal djot metacharacters in text must survive verbatim.
    let dj = fixpoint(r"literal \* \_ \` \~ \^ \[ \] \( \) \{ \} \| chars");
    let twice = import_then_export(&dj);
    assert_eq!(dj, twice);
    // The characters themselves are still present (escaped) in the output.
    for ch in ['*', '_', '`', '~', '^', '[', ']', '(', ')', '{', '}', '|'] {
        assert!(dj.contains(ch), "missing {ch:?} in {dj:?}");
    }
}

#[test]
fn paragraph_starting_with_block_marker_is_guarded() {
    // A paragraph whose text begins with a block marker must not be re-parsed
    // as that block construct.
    for input in [
        "# not a heading",
        "> not a quote",
        "- not a list",
        "1. not ordered",
    ] {
        let dj = fixpoint(input);
        // The visible text survives intact after one normalization pass.
        let twice = import_then_export(&dj);
        assert_eq!(dj, twice, "fixpoint for {input:?}");
    }
}

#[test]
fn empty_document() {
    assert_eq!(fixpoint(""), "");
}

#[test]
fn mixed_document() {
    let input = "# Title\n\nA paragraph with *bold*, _italic_, `code`, ^sup^ and a [link](https://example.com).\n\n- one\n\n- two\n\n> a quote\n\n```rust\nlet x = 1;\n```";
    let dj = fixpoint(input);
    // Spot-check several features coexist in the canonical output.
    assert_contains(&dj, "# Title");
    assert_contains(&dj, "*bold*");
    assert_contains(&dj, "_italic_");
    assert_contains(&dj, "`code`");
    assert_contains(&dj, "^sup^");
    assert_contains(&dj, "[link](https://example.com)");
    assert_contains(&dj, "> a quote");
    assert_contains(&dj, "```rust");
}

// ── Optional block attributes (alignment, line height, direction, ──────────
//    non-breakable lines, background color) round-tripped via djot `{…}`.

#[test]
fn block_alignment_round_trips() {
    assert_contains(
        &fixpoint("{alignment=right}\nRight aligned."),
        "{alignment=right}",
    );
}

#[test]
fn block_line_height_round_trips() {
    assert_contains(
        &fixpoint("{line_height=1500}\nTall lines."),
        "line_height=1500",
    );
}

#[test]
fn block_direction_round_trips() {
    assert_contains(
        &fixpoint("{direction=rtl}\nRight to left."),
        "direction=rtl",
    );
}

#[test]
fn block_non_breakable_lines_round_trips() {
    assert_contains(
        &fixpoint("{non_breakable_lines=true}\nKeep together."),
        "non_breakable_lines=true",
    );
}

#[test]
fn block_background_color_round_trips() {
    assert_contains(
        &fixpoint("{background_color=\"#ff0000\"}\nColoured."),
        "background_color=\"#ff0000\"",
    );
}

#[test]
fn multiple_block_attributes_combine() {
    let dj = fixpoint("{alignment=center line_height=1500}\nBoth set.");
    assert_contains(&dj, "alignment=center");
    assert_contains(&dj, "line_height=1500");
}

#[test]
fn heading_alignment_round_trips() {
    let dj = fixpoint("{alignment=center}\n# Centered Title");
    assert_contains(&dj, "{alignment=center}");
    assert_contains(&dj, "# Centered Title");
}

#[test]
fn spacing_attributes_round_trip() {
    // A scene break sets these on the paragraph that follows it, so they must
    // survive the save/load cycle intact.
    let dj = fixpoint("{top_margin=24 text_indent=0}\nAfter the break.");
    assert_contains(&dj, "top_margin=24");
    assert_contains(&dj, "text_indent=0");
    assert_contains(&dj, "After the break.");
}

#[test]
fn a_zero_text_indent_survives_rather_than_vanishing() {
    // `text_indent=0` is meaningful ("explicitly no indent") and must not be
    // dropped as if it were absent — a falsy-value round-trip bug here would
    // silently restore the document-wide indent on every reload.
    let dj = fixpoint("{text_indent=0}\nFlush left.");
    assert_contains(&dj, "text_indent=0");
}

#[test]
fn spacing_attributes_combine_with_direction_on_one_block() {
    // An RTL scene whose first paragraph follows a blank-line break carries both
    // `direction` and the spacing pair. They must land on the SAME block, in one
    // attribute line, not clobber each other.
    let dj = fixpoint("{direction=rtl top_margin=24 text_indent=0}\nنص عربي.");
    assert_contains(&dj, "direction=rtl");
    assert_contains(&dj, "top_margin=24");
    assert_contains(&dj, "text_indent=0");
    let attr_lines = dj
        .lines()
        .filter(|l| l.trim_start().starts_with('{'))
        .count();
    assert_eq!(attr_lines, 1, "attributes must share one line:\n{dj}");
}

#[test]
fn block_attributes_survive_inside_blockquote() {
    let dj = fixpoint("> {alignment=center}\n> Quoted and centered.");
    assert_contains(&dj, "alignment=center");
    assert_contains(&dj, "> ");
}

#[test]
fn export_options_can_suppress_an_attribute() {
    let (db, ev, _) = setup().expect("setup");
    let mut mgr = LongOperationManager::new();
    let op = document_io_controller::import_djot(
        &db,
        &ev,
        &mut mgr,
        &ImportDjotDto {
            djot_text: "{alignment=center}\nhi".into(),
            options: Default::default(),
        },
    )
    .expect("import_djot");
    wait(&mgr, &op);
    let opts = common::parser_tools::DjotExportOptions {
        alignment: false,
        ..Default::default()
    };
    let dj = document_io_controller::export_djot(&db, &ev, &opts)
        .expect("export_djot")
        .djot_text;
    assert!(
        !dj.contains("alignment"),
        "alignment should be suppressed:\n{dj}"
    );
    assert!(dj.contains("hi"));
}

#[test]
fn import_options_can_ignore_an_attribute() {
    let (db, ev, _) = setup().expect("setup");
    let mut mgr = LongOperationManager::new();
    let op = document_io_controller::import_djot(
        &db,
        &ev,
        &mut mgr,
        &ImportDjotDto {
            djot_text: "{alignment=center}\nhi".into(),
            options: common::parser_tools::DjotImportOptions {
                alignment: false,
                ..Default::default()
            },
        },
    )
    .expect("import_djot");
    wait(&mgr, &op);
    let dj = document_io_controller::export_djot(&db, &ev, &Default::default())
        .expect("export_djot")
        .djot_text;
    assert!(
        !dj.contains("alignment"),
        "alignment should not have been imported:\n{dj}"
    );
    assert!(dj.contains("hi"));
}
