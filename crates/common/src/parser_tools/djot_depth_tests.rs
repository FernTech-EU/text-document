//! The depth guard: what it refuses, what it lets through, and proof that its count is
//! the nesting `jotdown` builds.
//!
//! The parses that must survive run on a thread with the 2 MiB stack a spawned thread
//! gets, in the debug build the suite runs in. A stack overflow is not a panic, so a
//! regression there does not fail a test: it aborts the test binary, which is the
//! signal.

use super::*;
use crate::parser_tools::content_parser::{ParsedElement, parse_djot};
use crate::parser_tools::djot_options::DjotImportOptions;
use proptest::prelude::*;

/// The stack `std::thread::spawn` gives a thread by default.
const SPAWNED_THREAD_STACK: usize = 2 << 20;

/// Parse `djot` the way the importer does, on a thread with a spawned thread's stack.
fn parse_on_a_spawned_thread(djot: String) -> Vec<ParsedElement> {
    std::thread::Builder::new()
        .stack_size(SPAWNED_THREAD_STACK)
        .spawn(move || parse_djot(&djot, &DjotImportOptions::default()))
        .expect("spawn the parse thread")
        .join()
        .expect("the parse must not unwind")
}

/// Whether `elements` is the degraded parse of `djot`: one plain paragraph holding the
/// source verbatim.
fn is_raw(elements: &[ParsedElement], djot: &str) -> bool {
    match elements {
        [ParsedElement::Block(block)] => {
            block
                .spans
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>()
                == djot
        }
        _ => false,
    }
}

/// The most block containers `jotdown` itself nests any part of `djot` inside, read
/// from its events on a stack large enough for any document these tests build.
fn jotdown_depth(djot: &str) -> usize {
    let djot = djot.to_string();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            use jotdown::{Container as C, Event as E};
            fn counted(container: &C) -> bool {
                matches!(
                    container,
                    C::Blockquote
                        | C::ListItem
                        | C::TaskListItem { .. }
                        | C::DescriptionDetails
                        | C::Footnote { .. }
                        | C::Div { .. }
                        | C::Table
                )
            }
            let (mut depth, mut deepest) = (0usize, 0usize);
            for event in jotdown::Parser::new(&djot) {
                match event {
                    E::Start(container, _) if counted(&container) => {
                        depth += 1;
                        deepest = deepest.max(depth);
                    }
                    E::End(container) if counted(&container) => depth -= 1,
                    _ => {}
                }
            }
            deepest
        })
        .expect("spawn the measuring thread")
        .join()
        .expect("measuring must not unwind")
}

/// Every marker `jotdown` 0.10 opens a container with at the start of a line, as the
/// text that opens one more level when it is repeated on one line.
const ONE_LINE_MARKERS: [&str; 17] = [
    "- ", "* ", "+ ", "1. ", "1) ", "(1) ", "a. ", "B) ", "(c) ", "iv. ", "XII) ", "(ix) ",
    "- [ ] ", "* [x] ", "[^a]: ", ": ", "> ",
];

/// `levels` of `marker` on one line, then a word, which the last one holds.
fn one_line(marker: &str, levels: usize) -> String {
    format!("{}deep\n", marker.repeat(levels))
}

/// A list whose items each step in past the last by the width of their marker, with a
/// paragraph line at no indentation after each and a blank line before the next. The
/// paragraph line continues every item open above it, and each item strips at most its
/// marker's width from the lines it continues, so each item nests inside the last:
/// `levels` deep. `prefix` goes in front of every line, a quotation's `> ` for instance.
fn lazy_staircase(marker: &str, levels: usize, prefix: &str) -> String {
    let step = marker.trim_end().len();
    (0..levels)
        .map(|level| {
            format!(
                "{prefix}{}{marker}item {level}\n{prefix}lazy\n",
                " ".repeat(step * level)
            )
        })
        .collect::<Vec<_>>()
        .join(&format!("{}\n", prefix.trim_end()))
}

/// How deep the hostile prose below nests: past the 491 containers at which the
/// parser aborts a 2 MiB thread in a debug build.
const PAST_THE_PARSERS_LIMIT: usize = 700;

/// Prose the parser cannot survive on a spawned thread's stack, in every shape a
/// container can take.
fn past_the_parsers_limit() -> Vec<(&'static str, String)> {
    let levels = PAST_THE_PARSERS_LIMIT;
    let mixture: String = ["- ", "> ", "1. ", "[^a]: ", ": ", "(iv) ", "- [ ] "]
        .iter()
        .cycle()
        .take(levels)
        .copied()
        .collect();
    let descending = (0..levels)
        .map(|i| ":".repeat(levels + 2 - i))
        .collect::<Vec<_>>()
        .join("\n")
        + "\ndeep\n";
    vec![
        ("blockquotes on one line", one_line("> ", levels)),
        ("bullets on one line", one_line("- ", levels)),
        ("ordered items on one line", one_line("1. ", levels)),
        ("roman numerals in parentheses", one_line("(iv) ", levels)),
        ("task items on one line", one_line("- [ ] ", levels)),
        (
            "footnote definitions on one line",
            one_line("[^a]: ", levels),
        ),
        ("definition list items on one line", one_line(": ", levels)),
        ("a mixture on one line", format!("{mixture}deep\n")),
        (
            "a div opened inside a quote on every line",
            "> ::: note\n".repeat(levels),
        ),
        ("fences each one colon shorter", descending),
        (
            "divs a code fence keeps open",
            format!("::: a\n{}", "- item\n\n  ```x\n:::\n".repeat(levels)),
        ),
        (
            "list items stepping in between paragraph lines",
            lazy_staircase("- ", levels, ""),
        ),
        (
            "footnotes stepping in between paragraph lines",
            lazy_staircase("[^a]: ", levels, ""),
        ),
        (
            "quoted list items stepping in between paragraph lines",
            lazy_staircase("1. ", levels, "> "),
        ),
    ]
}

/// Every shape in [`past_the_parsers_limit`] is refused, and the real parser comes
/// back from it on a spawned thread's stack with the source verbatim.
///
/// Before the scan followed `jotdown`, only the first shape was refused. Every other
/// one was handed to the parser and aborted the test binary here.
#[test]
fn every_hostile_shape_is_refused_and_comes_back_verbatim() {
    for (shape, text) in past_the_parsers_limit() {
        assert!(is_too_deep(&text), "{shape} must be refused");
        let elements = parse_on_a_spawned_thread(text.clone());
        assert!(
            is_raw(&elements, &text),
            "{shape}: degrading may not lose prose"
        );
    }
}

#[test]
fn ordinary_prose_is_shallow() {
    for (text, depth) in [
        ("The ferry was late.\n\nShe waited.\n", 0),
        ("> He said it plainly.\n>\n> Then he left.\n", 1),
        ("- one\n\n  - two\n\n    - three\n", 3),
        ("::: note\nA note.\n:::\n", 1),
        ("> - a quoted list\n>\n>   - nested once\n", 3),
        ("1. First.\n\n   a. Inside.\n\n      i. Deeper.\n", 3),
        (
            "A note.[^1]\n\n[^1]: The note, which runs on.\n\n    A second paragraph.\n",
            1,
        ),
        ("- [ ] a task\n- [x] a done one\n", 1),
        ("Term\n\n: its definition\n", 1),
        ("| a | b |\n|---|---|\n| c | d |\n", 1),
        ("* * *\n\n- - - - - - - - - - - - - - - - - - - - - -\n", 0),
        ("", 0),
    ] {
        assert_eq!(nesting_depth(text), depth, "{text:?}");
        assert_eq!(jotdown_depth(text), depth, "the measure itself: {text:?}");
        assert!(!is_too_deep(text));
    }
}

/// Indentation in front of no marker opens nothing, however long it runs: a paragraph
/// whose first words sit behind hundreds of spaces or tabs is one paragraph. The
/// previous count read one level per two bytes of it, so a paragraph typed after 194
/// spaces, which the editor writes as typed, was shown to the writer as raw source.
#[test]
fn indentation_alone_opens_nothing_however_long() {
    for blank in [" ", "\t", " \t", "\r", "\u{c}"] {
        for text in [
            format!("{}Indented paragraph.\n", blank.repeat(194)),
            format!("{}item\n", blank.repeat(1_000)),
            format!(
                "First.\n\n{}Second.\n\n{}\n",
                blank.repeat(400),
                blank.repeat(700)
            ),
            format!(
                "```\n{}code keeps its indentation\n```\n",
                blank.repeat(900)
            ),
        ] {
            assert_eq!(nesting_depth(&text), 0, "{blank:?}: {text:.60?}");
            let elements = parse_on_a_spawned_thread(text.clone());
            assert!(!is_raw(&elements, &text), "{blank:?}: shown as raw source");
        }
    }
    // Continuing a container, it sits exactly as deep as the container.
    for (text, depth) in [
        (
            format!("- a list item\n\n{}continued far in\n", " ".repeat(900)),
            1,
        ),
        (
            format!("> a quotation\n>{}still in it\n", " ".repeat(900)),
            1,
        ),
    ] {
        assert_eq!(nesting_depth(&text), depth, "{text:.60?}");
    }
}

/// A lone marker behind any amount of indentation opens exactly one level: the
/// indentation opens nothing, and no item encloses it.
#[test]
fn a_lone_indented_marker_opens_one_level_however_far_it_is_indented() {
    for block in [
        "- item",
        "1. item",
        "[^a]: item",
        ": item",
        "> item",
        "| a |",
    ] {
        for indent in [0, 1, MAX_NESTING_DEPTH, 4 * MAX_NESTING_DEPTH, 4_000] {
            let text = format!("{}{block}\n", " ".repeat(indent));
            assert_eq!(nesting_depth(&text), 1, "{block:?} behind {indent} spaces");
        }
    }
}

/// One line of any container marker, repeated, nests one level per marker: accepted up
/// to the ceiling, parsed from a spawned thread's stack as structure there, and
/// refused one past it. The previous count read only `>` on a line, so every other
/// marker went through at any depth and aborted the process.
#[test]
fn every_container_a_line_can_open_is_counted_to_the_ceiling() {
    for marker in ONE_LINE_MARKERS {
        let text = one_line(marker, MAX_NESTING_DEPTH);
        assert_eq!(nesting_depth(&text), MAX_NESTING_DEPTH, "{marker:?}");
        assert!(!is_too_deep(&text), "{marker:?} at the ceiling must pass");
        let elements = parse_on_a_spawned_thread(text.clone());
        assert!(!is_raw(&elements, &text), "{marker:?}: shown as raw source");

        let past = format!("Before.\n\n{}", one_line(marker, MAX_NESTING_DEPTH + 1));
        assert!(is_too_deep(&past), "{marker:?} past the ceiling");
    }
}

/// A link definition holds no blocks, so a run of them on one line is one leaf, not a
/// nesting: `jotdown` reads what follows the first as its destination.
#[test]
fn a_run_of_link_definitions_is_one_leaf() {
    let text = one_line("[link]: ", 4 * MAX_NESTING_DEPTH);
    assert_eq!(nesting_depth(&text), 0);
    assert_eq!(jotdown_depth(&text), 0);
}

/// The hole the previous estimate in the application had: an item continued by a
/// paragraph line at no indentation, then a blank line, then the next item one column
/// further in. Each item nests inside the last, so the count is the number of steps,
/// however little indentation each one has.
#[test]
fn a_list_continued_by_paragraph_lines_is_counted_at_its_real_depth() {
    for (marker, prefix) in [("- ", ""), ("[^a]: ", ""), ("1. ", "> "), ("- ", "> > ")] {
        for levels in [1, 2, 10, 50, MAX_NESTING_DEPTH] {
            let text = lazy_staircase(marker, levels, prefix);
            let quotes = prefix.matches('>').count();
            assert_eq!(
                nesting_depth(&text),
                levels + quotes,
                "{marker:?} {prefix:?} x{levels}"
            );
            assert_eq!(jotdown_depth(&text), levels + quotes, "the measure itself");
        }
    }
}

/// Genuine nesting by indentation is counted per step, not per byte: one item a line,
/// a blank line between, each indented one column past the last.
#[test]
fn a_list_nested_by_one_space_a_level_is_counted_per_step() {
    let nested = |levels: usize| {
        (0..levels)
            .map(|level| format!("{}- level {level}\n", " ".repeat(level)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(nesting_depth(&nested(MAX_NESTING_DEPTH)), MAX_NESTING_DEPTH);
    assert!(is_too_deep(&nested(MAX_NESTING_DEPTH + 1)));
    // Without the blank lines, each item is a line of the first one's paragraph.
    let folded = nested(MAX_NESTING_DEPTH + 1).replace("\n\n", "\n");
    assert_eq!(nesting_depth(&folded), 1);
}

/// Sibling divs close each other, so a long document made of many of them has the
/// nesting of one.
#[test]
fn sibling_divs_do_not_accumulate() {
    assert_eq!(nesting_depth(&"::: note\nbody\n:::\n".repeat(500)), 1);
}

/// Djot lets an outer div use a longer fence so another can nest inside it. A `::::`
/// line carrying no class closes rather than opens.
#[test]
fn a_longer_closing_fence_closes_rather_than_opens() {
    let text = ":::: outer\n::: inner\nbody\n:::\n::::\n".repeat(200);
    assert_eq!(nesting_depth(&text), 2);
}

/// A div that saw a code fence open ignores every fence until one closes it
/// (`jotdown`'s `nested_raw`), even when the code block itself ended with the list item
/// it sat in, so the bare fence after it opens a div inside the one before.
#[test]
fn a_fence_the_div_reads_as_code_does_not_close_it() {
    let rounds = |n: usize| format!("::: a\n{}", "- item\n\n  ```x\n:::\n".repeat(n));
    assert_eq!(nesting_depth(&rounds(10)), jotdown_depth(&rounds(10)));
    assert!(is_too_deep(&rounds(MAX_NESTING_DEPTH)));
    let closed = "::: a\n```\ncode\n```\n:::\n".repeat(500);
    assert_eq!(nesting_depth(&closed), 1);
}

/// `jotdown` strips a div's indentation from each line of its content except the
/// first, which it hands on as it found it. So an item on that first line keeps the
/// div's indentation as its own, and the line after the blank below closes it rather
/// than nesting in it.
#[test]
fn a_div_leaves_its_first_line_as_it_found_it() {
    let text = "  ::: d\n  - a\n\n   - b\n  :::\n";
    assert_eq!(jotdown_depth(text), 2, "the measure itself");
    assert_eq!(nesting_depth(text), 2);
}

/// Only ASCII whitespace indents a line or ends a marker. A paragraph opening with
/// no-break spaces, ideographic spaces or narrow no-break spaces is a paragraph whose
/// text starts with them, and so is one where markers follow them.
#[test]
fn unicode_spaces_at_a_line_start_are_text_not_indentation() {
    for space in ['\u{A0}', '\u{3000}', '\u{202F}', '\u{2003}'] {
        let spaces = space.to_string().repeat(300);
        for text in [
            format!("{spaces}A title set in the middle of the page.\n"),
            format!("{space}{}x\n", ">".repeat(300)),
            format!("{space}{}x\n", "- ".repeat(300)),
        ] {
            assert_eq!(nesting_depth(&text), 0, "{space:?}");
        }
    }
}

/// Text that only looks like a marker opens nothing: a word ending in a full stop, a
/// bullet not followed by a space, a colon inside a word, a thematic break, a number
/// longer than a list marker may be.
#[test]
fn text_that_only_looks_like_a_marker_is_not_counted() {
    for text in [
        format!("{}\n", "Mr. ".repeat(300)),
        format!("{}\n", "-x ".repeat(300)),
        format!("{}\n", "a:b ".repeat(300)),
        format!("{}\n", "- ".repeat(300)),
        format!("{}\n", "* - ".repeat(300)),
        format!("{}\n", "[a] ".repeat(300)),
        format!("{}\n", "abc. ".repeat(300)),
        format!("{}\n", "12345678901234567890. ".repeat(300)),
        format!("{}\n", "-\t".repeat(300)),
        // A `>` is a blockquote only before whitespace: a run of them is a word.
        format!("{}deep\n", ">".repeat(4_000)),
    ] {
        assert_eq!(nesting_depth(&text), 0, "{text:.40}");
        assert_eq!(jotdown_depth(&text), 0, "the measure itself: {text:.40}");
    }
}

/// The scan stops at the ceiling rather than reading on: refusing a document costs no
/// more than reading it to its first line past the ceiling, however much more it holds.
#[test]
fn a_refusal_stops_at_the_first_line_past_the_ceiling() {
    let text = format!(
        "{}{}",
        one_line("> ", MAX_NESTING_DEPTH + 1),
        one_line("- ", 100_000)
    );
    assert!(is_too_deep(&text));
    assert_eq!(
        deepest(&text, MAX_NESTING_DEPTH),
        MAX_NESTING_DEPTH + 1,
        "the scan stopped on the first line"
    );
}

/// The one that matters for the fallback: the source comes back verbatim.
#[test]
fn the_real_parser_survives_input_that_used_to_abort_the_process() {
    let hostile = one_line("> ", 4_000);
    let elements = parse_djot(&hostile, &DjotImportOptions::default());
    assert!(is_raw(&elements, &hostile));
}

// ── The count is the nesting jotdown builds ─────────────────────────────────────────

/// What a generated line may open with: whitespace, container markers, and text that
/// only looks like one.
fn line_start() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "",
        " ",
        "  ",
        "   ",
        "    ",
        "\t",
        " \t",
        "\u{c}",
        "\r",
        "> ",
        ">",
        ">\t",
        ">\u{c}",
        "- ",
        "* ",
        "+ ",
        "1. ",
        "2) ",
        "(iv) ",
        "a. ",
        "B) ",
        "(c) ",
        "- [ ] ",
        "* [x] ",
        ": ",
        "[^a]: ",
        "[^n]:",
        "[l]: ",
        "-",
        "*",
        "+",
        ">x",
        "-\t",
        "(1",
        "12345678901234567890. ",
    ])
}

/// What a generated line may end with, after its markers.
fn line_end() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "",
        "",
        "words",
        "x y",
        ":::",
        "::: c",
        "::::",
        ":::: c",
        ":::c d",
        "```",
        "```x",
        "`` x",
        "~~~",
        "~~~~",
        "| a |",
        "|a|b|",
        "| a \\|",
        "^ caption",
        "# h",
        "## h",
        "#x",
        "{.c}",
        "{#i k=v}",
        "{.c} x",
        "{% note %}",
        "---",
        "***",
        "- * -",
        "-",
        ":",
        "[x]",
        "10:30",
        "Mr. x",
        "1.",
        "(a)",
        "[^a]:",
        ">",
        "  ",
        "\t",
    ])
}

/// A line: a few starts, then an end.
fn generated_line() -> impl Strategy<Value = String> {
    (prop::collection::vec(line_start(), 0..6), line_end())
        .prop_map(|(starts, end)| starts.concat() + end)
}

/// A list stepping in `step` columns a level, each item followed by `between`.
fn staircase() -> impl Strategy<Value = String> {
    let marker = prop::sample::select(vec![
        "- ", "1. ", "(iv) ", "- [ ] ", "[^a]: ", ": ", "> - ", "- > ", "> ", "::: c\n",
    ]);
    let between = prop::sample::select(vec![
        "\n",
        "\n\n",
        "\nlazy\n",
        "\nlazy\n\n",
        "\n\n  more\n\n",
        "\n>\n",
        "\n> lazy\n>\n",
        "\n# h\n",
        "\n\n```\n",
        "\n:::\n",
        "\n| a |\n\n",
        "\r\n\r\n",
    ]);
    (marker, between, 0usize..7, 1usize..60).prop_map(|(marker, between, step, levels)| {
        (0..levels)
            .map(|level| format!("{}{marker}level {level}", " ".repeat(step * level)))
            .collect::<Vec<_>>()
            .join(between)
    })
}

/// A fence, indented or not, around lines that open containers at an indentation of
/// their own, some after a blank line.
fn fenced_block() -> impl Strategy<Value = String> {
    let fence = prop::sample::select(vec![":::", "::: c", "::::", "```", "~~~"]);
    let start = prop::sample::select(vec!["- ", "1. ", "> ", "[^a]: ", ": ", ""]);
    let content = prop::collection::vec((0usize..7, start, any::<bool>()), 1..8);
    (0usize..4, fence, content).prop_map(|(indent, fence, content)| {
        let fence_line = format!("{}{fence}", " ".repeat(indent));
        let mut lines = vec![fence_line.clone()];
        for (spaces, start, blank_before) in content {
            if blank_before {
                lines.push(String::new());
            }
            lines.push(format!("{}{start}x", " ".repeat(spaces)));
        }
        lines.push(fence_line);
        lines.join("\n")
    })
}

/// A document of generated lines, staircases and fenced blocks, ending with a line
/// break or not.
fn generated_document() -> impl Strategy<Value = String> {
    let group = prop_oneof![
        6 => generated_line(),
        1 => staircase(),
        1 => fenced_block(),
    ];
    (prop::collection::vec(group, 1..40), any::<bool>()).prop_map(|(groups, closed)| {
        let mut text = groups.join("\n");
        if closed {
            text.push('\n');
        }
        text
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2_048, ..ProptestConfig::default() })]

    /// The count is exactly the nesting `jotdown` builds: every line opening, continuing
    /// or closing containers the way the block parser does. An estimate would have to be
    /// wrong on one side, and both sides have already cost a writer something.
    #[test]
    fn the_count_is_the_nesting_jotdown_builds(text in generated_document()) {
        prop_assert_eq!(nesting_depth(&text), jotdown_depth(&text), "{:?}", text);
    }

    /// Whatever the guard lets through parses from a spawned thread's stack, and what it
    /// refuses comes back as its source, verbatim. The assertion for the first is the
    /// parse coming back at all: a guard that let a document past the parser's limit
    /// through would abort the test binary here.
    #[test]
    fn whatever_the_guard_accepts_parses_from_a_spawned_thread(
        starts in prop::collection::vec(line_start(), 1..4),
        times in 1usize..(4 * MAX_NESTING_DEPTH),
        end in line_end(),
    ) {
        let text = format!("{}{end}\n", starts.concat().repeat(times));
        let elements = parse_on_a_spawned_thread(text.clone());
        if is_too_deep(&text) {
            prop_assert!(is_raw(&elements, &text), "{:?}", text);
        }
    }
}
