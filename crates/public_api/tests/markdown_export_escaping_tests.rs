//! Text exported as Markdown reads back as text.
//!
//! The Markdown writer escaped the characters that open emphasis, links and block
//! markers, but not a backtick, a `<` or a `&`. Typed text holding a pair of backticks
//! came back as code, a line of three of them opened a fence that ran to the end of the
//! file, `<http://a.b>` came back as a link with backslashes in its address, and
//! `Tom &amp; Jerry` came back as `Tom & Jerry`. A paragraph indented with a tab was
//! exported as a code block.
//!
//! What the writer wrote around text was no safer: a code block was fenced with three
//! backticks whatever it held, and an image's source, its description and a link's
//! address were written as they were, so a `)`, a space or a backslash in one cut it
//! short.

use proptest::prelude::*;
use std::time::{Duration, Instant};
use text_document::{BlockFormat, FragmentContent, MoveMode, TextDocument, TextFormat};

/// Typed text exported with `to_markdown` and read back with `set_markdown`.
fn markdown_round_trip(typed: &str) -> (String, String) {
    let doc = TextDocument::new();
    doc.set_plain_text(typed).unwrap();
    let markdown = doc.to_markdown().unwrap();
    let back = TextDocument::new();
    back.set_markdown(&markdown).unwrap().wait().unwrap();
    (markdown, back.to_plain_text().unwrap())
}

#[test]
fn text_that_reads_as_markup_comes_back_as_text() {
    let mut failures = Vec::new();
    for typed in [
        "Tom &amp; Jerry",
        "&copy; 2026 and &#169; and &#x1F600;",
        "Use `ls` now",
        "a ``b`` c",
        "```",
        "~~~",
        "<http://a.b>",
        "x <div> y",
        "5 < 6 > 4",
        "`Tis the season. `Hello,' she said.",
        "\tShe opened the door.",
        "    four spaces",
        "  two spaces",
        // Left as they are, and still text.
        "AT&T",
        "&#12345678; is not a reference",
        "a & b",
    ] {
        let (markdown, back) = markdown_round_trip(typed);
        if back != typed {
            failures.push(format!(
                "typed {typed:?}, exported {markdown:?}, read back {back:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // A `&` that opens no reference is not escaped.
    assert_eq!(markdown_round_trip("AT&T").0, "AT&T");
}

/// A fence of three backticks ran on to the end of the export: every paragraph after it
/// came back as code, its escapes shown as backslashes.
#[test]
fn a_line_of_backticks_does_not_swallow_what_follows() {
    let (markdown, back) =
        markdown_round_trip("Chapter one.\n```\nShe said: stop. It was 3 p.m.\nThe end.");
    assert_eq!(
        back, "Chapter one.\n```\nShe said: stop. It was 3 p.m.\nThe end.",
        "exported {markdown:?}"
    );
}

/// Code holding backticks is written with a fence longer than any run inside it, and
/// with a space inside each fence where the code opens or ends with a backtick or a
/// space at both ends, which the reader strips again.
#[test]
fn inline_code_keeps_its_backticks_and_spaces() {
    let code = TextFormat {
        font_family: Some("monospace".to_string()),
        ..Default::default()
    };
    for text in ["a`b", "``x``", "`", " a ", "a ` b", "x"] {
        let doc = TextDocument::new();
        let cursor = doc.cursor();
        cursor.insert_text("see ").unwrap();
        cursor.insert_formatted_text(text, &code).unwrap();
        cursor
            .insert_formatted_text(" end", &TextFormat::default())
            .unwrap();
        let markdown = doc.to_markdown().unwrap();
        let back = TextDocument::new();
        back.set_markdown(&markdown).unwrap().wait().unwrap();
        assert_eq!(
            back.to_plain_text().unwrap(),
            format!("see {text} end"),
            "exported {markdown:?}"
        );
    }
}

/// `doc` exported with `to_markdown` and read back with `set_markdown`.
fn exported_and_read_back(doc: &TextDocument) -> (String, TextDocument) {
    let markdown = doc.to_markdown().unwrap();
    let back = TextDocument::new();
    back.set_markdown(&markdown).unwrap().wait().unwrap();
    (markdown, back)
}

/// Each code block of `doc`: its text and its language.
fn code_blocks(doc: &TextDocument) -> Vec<(String, Option<String>)> {
    doc.blocks()
        .iter()
        .filter(|block| block.block_format().is_code_block == Some(true))
        .map(|block| (block.text(), block.block_format().code_language))
        .collect()
}

/// A document holding one code block of `code` in `language`.
fn code_block_of(code: &str, language: Option<&str>) -> TextDocument {
    let doc = TextDocument::new();
    let cursor = doc.cursor();
    cursor.insert_text(code).unwrap();
    cursor
        .set_block_format(&BlockFormat {
            is_code_block: Some(true),
            code_language: language.map(str::to_string),
            ..Default::default()
        })
        .unwrap();
    doc
}

/// A code block was fenced with three backticks whatever it held, so a line of three in
/// the code closed it: the lines after it came back as prose, and the fence meant to
/// close the block opened one that ran to the end of the export.
#[test]
fn a_code_block_holding_a_fence_keeps_its_lines() {
    for (djot, code) in [
        ("````\na\n```\nb\n````\n\nAfter.", "a\n```\nb"),
        ("``````\n`````\n``````\n\nAfter.", "`````"),
        ("> ````\n> a\n> ```\n> b\n> ````\n\nAfter.", "a\n```\nb"),
        ("> > ````\n> > ```\n> > ````\n\nAfter.", "```"),
        ("> ```\n> a\n>\n> ```\n\nAfter.", "a\n"),
    ] {
        let doc = TextDocument::new();
        doc.set_djot_sync(djot).unwrap();
        assert_eq!(code_blocks(&doc), [(code.to_string(), None)], "{djot:?}");
        let (markdown, back) = exported_and_read_back(&doc);
        assert_eq!(
            code_blocks(&back),
            [(code.to_string(), None)],
            "exported {markdown:?}"
        );
        assert_eq!(
            back.to_plain_text().unwrap().lines().last(),
            Some("After."),
            "exported {markdown:?}"
        );
    }
    // A language holding a backtick cannot follow a fence of backticks.
    for (code, language) in [("a\n```\nb", "a`b"), ("~~~\nx", "x`")] {
        let doc = code_block_of(code, Some(language));
        let (markdown, back) = exported_and_read_back(&doc);
        assert_eq!(
            code_blocks(&back),
            [(code.to_string(), Some(language.to_string()))],
            "exported {markdown:?}"
        );
    }
}

/// Image sources and link addresses that were written as they were, so that a `)`
/// ended one early, a space or a trailing backslash kept it from being read at all, a
/// backslash before punctuation was dropped and a character reference was decoded.
const ODD_DESTINATIONS: &[&str] = &[
    "a).png",
    "photo (1).png",
    "https://ex.com/wiki/File:Gull_(bird).jpg",
    "((a)).png",
    "a b.png",
    "a\\",
    "a\\.png",
    "C:\\Users\\Anna\\.config",
    "\\\\server\\share",
    "<a>.png",
    "a|b.png",
    "a&amp;b.png",
    "AT&T.png",
    "a`b.png",
    "a\tb.png",
    "a\u{a0}b.png",
    "*a*_b_[c]",
    "a%20b.png",
];

/// Each image of `doc`: its source and its description.
fn images(doc: &TextDocument) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for block in doc.blocks() {
        for fragment in block.fragments() {
            if let FragmentContent::Image { name, alt, .. } = fragment {
                out.push((name, alt));
            }
        }
    }
    out
}

/// The address of each run of a link in `doc`.
fn hrefs(doc: &TextDocument) -> Vec<String> {
    let mut out = Vec::new();
    for block in doc.blocks() {
        for fragment in block.fragments() {
            if let FragmentContent::Text { format, .. } = fragment
                && let Some(href) = format.anchor_href
            {
                out.push(href);
            }
        }
    }
    out
}

/// A paragraph holding an image of `source` described as `alt`, then a link to `href`.
fn paragraph_with_image_and_link(source: &str, alt: &str, href: &str) -> TextDocument {
    let doc = TextDocument::new();
    let cursor = doc.cursor();
    cursor.insert_text("See ").unwrap();
    cursor.insert_image(source, alt, 60, 90).unwrap();
    cursor.insert_text(" and `the link` here.").unwrap();
    let link = doc.cursor_at(10);
    link.set_position(18, MoveMode::KeepAnchor);
    link.merge_char_format(&TextFormat {
        anchor_href: Some(href.to_string()),
        ..Default::default()
    })
    .unwrap();
    doc
}

/// An image's source is the key its bytes are kept under and a link's address is where
/// it goes, so both come back as written, in a paragraph and in a table cell, and so
/// does an image's description.
#[test]
fn an_image_or_a_link_keeps_its_destination_whatever_it_holds() {
    for destination in ODD_DESTINATIONS {
        let doc = paragraph_with_image_and_link(destination, "a gull", destination);
        let before = doc.to_plain_text().unwrap();
        let (markdown, back) = exported_and_read_back(&doc);
        assert_eq!(
            images(&back),
            [(destination.to_string(), "a gull".to_string())],
            "exported {markdown:?}"
        );
        assert_eq!(
            hrefs(&back),
            [destination.to_string()],
            "exported {markdown:?}"
        );
        assert_eq!(
            back.to_plain_text().unwrap(),
            before,
            "exported {markdown:?}"
        );

        let doc = TextDocument::new();
        let table = doc.cursor().insert_table(2, 2).unwrap();
        let first = table.cell(1, 0).unwrap().blocks()[0].position();
        doc.cursor_at(first)
            .insert_image(destination, "p", 60, 90)
            .unwrap();
        let second = table.cell(1, 1).unwrap().blocks()[0].position();
        doc.cursor_at(second).insert_text("kept").unwrap();
        let (markdown, back) = exported_and_read_back(&doc);
        assert_eq!(
            images(&back),
            [(destination.to_string(), "p".to_string())],
            "in a cell, exported {markdown:?}"
        );
        assert!(
            back.to_plain_text().unwrap().contains("kept"),
            "in a cell, exported {markdown:?}"
        );
    }
    for alt in ["a]b", "*a* _b_", "x\\", "`c`", "&amp;", "<b>", "[x](y)"] {
        let doc = paragraph_with_image_and_link("x.png", alt, "https://example.com");
        let (markdown, back) = exported_and_read_back(&doc);
        assert_eq!(
            images(&back),
            [("x.png".to_string(), alt.to_string())],
            "exported {markdown:?}"
        );
    }
    // A plain destination is written as it always was.
    let doc = paragraph_with_image_and_link("x.png", "a", "https://ex.com/a_(b)");
    let markdown = doc.to_markdown().unwrap();
    assert!(markdown.contains("](x.png)"), "{markdown:?}");
    assert!(markdown.contains("](https://ex.com/a_(b))"), "{markdown:?}");
}

/// A document holding one paragraph of `unit` written `units` times.
fn paragraph_of(unit: &str, units: usize) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(&unit.repeat(units)).unwrap();
    doc
}

/// How long `doc` takes to export as Markdown.
fn time_export(doc: &TextDocument, length: usize) -> Duration {
    let start = Instant::now();
    let markdown = doc.to_markdown().unwrap();
    let elapsed = start.elapsed();
    assert!(markdown.len() >= length, "the whole text was exported");
    elapsed
}

/// How many times each paragraph is exported. Only the fastest export counts: whatever
/// else the machine runs can only slow one down, never speed it up, so the fastest of
/// several is the export's own cost.
const EXPORTS: usize = 7;

/// A paragraph of `a&` over the same paragraph of `a+` (the same length, and every `+`
/// escaped): the fastest of [`EXPORTS`] exports of each, taken in alternating order.
fn ampersands_over_plain_text(units: usize) -> f64 {
    let ampersands = paragraph_of("a&", units);
    let plain = paragraph_of("a+", units);
    let length = 2 * units;
    let mut fastest_ampersands = Duration::MAX;
    let mut fastest_plain = Duration::MAX;
    for _ in 0..EXPORTS {
        fastest_ampersands = fastest_ampersands.min(time_export(&ampersands, length));
        fastest_plain = fastest_plain.min(time_export(&plain, length));
    }
    fastest_ampersands.as_secs_f64() / fastest_plain.as_secs_f64().max(1e-9)
}

/// To tell whether a `&` opens a character reference, the writer looked for the next `;`
/// in the whole rest of the text, so a paragraph of many `&` and no `;` cost time in the
/// square of its length: in a debug build, 320,000 characters of `a&` took 300 times as
/// long to export as the same length of `a+`. It looks only as far as a reference can
/// reach now.
///
/// The unit test beside the scan (`export_markdown_uc`'s
/// `each_ampersand_is_scanned_no_further_than_a_reference_reaches`) counts the bytes it
/// looks at, in every run. This one times the whole export, and only on demand (`cargo
/// test --release -- --ignored`): a time depends on whatever else the machine runs, and
/// under 48 copies of this binary at once the growth below reached 3.4 where the fix alone
/// gives 0.9 to 1.4, too near a bound for a shared CI runner.
///
/// It never compares raw times: each export is divided by the export of the same length of
/// text with no `&`, and the quotient has to stay about the same for sixty-four times the
/// text. Each side is the fastest of [`EXPORTS`] exports. With the scan to the end of the
/// text: a growth of 44.
#[test]
#[ignore = "times the export; the scan's work is counted in export_markdown_uc's unit test"]
fn a_paragraph_of_many_ampersands_exports_in_time_proportional_to_its_length() {
    let small = ampersands_over_plain_text(5_000);
    let large = ampersands_over_plain_text(320_000);
    let growth = large / small;
    println!("ampersands over plain text: {small:.2}x at 5,000, {large:.2}x at 320,000");
    assert!(
        growth < 8.0,
        "exporting a paragraph of many `&` grew {growth:.2} times relative to the same \
         length of plain text, for sixty-four times the text: each `&` is looking for a \
         `;` past the longest character reference again"
    );
}

/// Printable ASCII, the spaces a manuscript holds, and fragments that are markup to a
/// Markdown reader.
fn typed_piece() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop::sample::select(
            "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~aAbB09 \u{a0}".chars().collect::<Vec<_>>()
        )
        .prop_map(String::from),
        1 => prop::sample::select(vec![
            "&amp;", "&#169;", "&copy;", "```", "~~~", "<http://a.b>", "<div>", "`code`",
            "**", "__", "[a](b)", "![a](b)", "# ", "> ", "- ", "1. ", "    ", "\t",
        ])
        .prop_map(str::to_string),
    ]
}

/// A line of code: printable ASCII, and runs of the characters that fence a block.
fn code_line() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            3 => prop::sample::select(
                "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~aA09 \t".chars().collect::<Vec<_>>()
            )
            .prop_map(String::from),
            1 => prop::sample::select(vec!["```", "````", "~~~", "~~~~", "    ", "> ", "- "])
                .prop_map(str::to_string),
        ],
        0..6,
    )
    .prop_map(|pieces| pieces.concat())
}

/// A destination: printable ASCII, spaces, a tab, a no-break space and the pieces
/// that end one or are read inside it.
fn destination() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            3 => prop::sample::select(
                "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~aA09 \t\u{a0}".chars().collect::<Vec<_>>()
            )
            .prop_map(String::from),
            1 => prop::sample::select(vec!["&amp;", "&#169;", "%20", "%28", "(a)", "\\\\"])
                .prop_map(str::to_string),
        ],
        1..10,
    )
    .prop_map(|pieces| pieces.concat())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Whatever a writer types, exported as Markdown and read back, is what was typed,
    /// its lines' ends aside: a reader drops the whitespace a paragraph ends with.
    #[test]
    fn typed_text_survives_a_markdown_round_trip(
        pieces in prop::collection::vec(typed_piece(), 1..12)
    ) {
        let typed = pieces.concat();
        let typed = typed.trim_end_matches([' ', '\t']);
        prop_assume!(!typed.trim().is_empty());
        // The Markdown reader's own scan for footnote references reads an escaped
        // `\[^` as one and adds a definition the text then shows: a reader's matter,
        // not the writer's.
        prop_assume!(!typed.contains("[^"));
        let (markdown, back) = markdown_round_trip(typed);
        prop_assert_eq!(&back, typed, "exported {:?}", markdown);
    }

    /// A code block comes back with every line it held and its language, whatever runs
    /// of backticks or tildes it holds.
    #[test]
    fn a_code_block_survives_a_markdown_round_trip(
        lines in prop::collection::vec(code_line(), 1..6),
        language in prop::sample::select(vec![None, Some("rust"), Some("a`b"), Some("c++")]),
    ) {
        let code = lines.join("\n");
        let doc = code_block_of(&code, language);
        let (markdown, back) = exported_and_read_back(&doc);
        prop_assert_eq!(
            code_blocks(&back),
            vec![(code.clone(), language.map(str::to_string))],
            "exported {:?}",
            markdown
        );
    }

    /// An image's source and a link's address come back exactly as written.
    #[test]
    fn a_destination_survives_a_markdown_round_trip(
        source in destination(),
        href in destination(),
    ) {
        let doc = paragraph_with_image_and_link(&source, "a gull", &href);
        let (markdown, back) = exported_and_read_back(&doc);
        prop_assert_eq!(
            images(&back),
            vec![(source.clone(), "a gull".to_string())],
            "exported {:?}",
            markdown
        );
        prop_assert_eq!(hrefs(&back), vec![href.clone()], "exported {:?}", markdown);
    }
}
