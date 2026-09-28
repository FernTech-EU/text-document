//! Property-based "no loss" test for djot import/export.
//!
//! The document model is the canonical form, so the lossless guarantee is a
//! **fixpoint**: for any document built from the supported feature set,
//! exporting to djot and re-importing must reproduce the same document, and
//! re-exporting must yield byte-identical djot.
//!
//! Strategy: generate a constrained AST over the supported feature set, emit it
//! as djot with a deliberately "dumb" (non-canonical) emitter, then push it
//! through the public API twice:
//!
//! ```text
//!   seed ──set_djot──▶ doc1 ──to_djot──▶ t1 ──set_djot──▶ doc2 ──to_djot──▶ t2
//! ```
//!
//! The first import/export canonicalises whatever the dumb emitter produced;
//! the assertions then require `t1 == t2` (export is a one-pass fixpoint) and
//! that the two documents have identical observable content. If any supported
//! feature lost information on the round-trip, `t1 != t2` (or the plain text /
//! block count would diverge).

use common::parser_tools::content_parser::parse_html;
use common::parser_tools::djot_depth::{MAX_LEAF_LINES, longest_leaf_lines, nesting_depth};
use proptest::prelude::*;
use text_document::{
    CharVerticalAlignment, DjotImportOptions, FindOptions, FragmentContent, MoveMode, TextDocument,
    TextFormat, djot_to_plain_text, plain_text_to_djot,
};

// ── AST over the supported feature set ──────────────────────────

#[derive(Debug, Clone)]
enum Inline {
    Text(String),
    Bold(String),
    Italic(String),
    Code(String),
    Sup(String),
    Sub(String),
    Strike(String),
    Underline(String),
    Link(String, String),
    /// An image: its description and its source as the seed writes it, percent-encoded
    /// where the source holds a character the writer encodes (see [`image_source`]).
    Image(String, String),
}

/// Optional block-style attributes, emitted as a djot `{…}` block-attribute
/// line before a paragraph or heading. Mirrors the five model fields the
/// exporter round-trips; all-`None` emits nothing.
#[derive(Debug, Clone, Default)]
struct BlockStyle {
    alignment: Option<&'static str>,
    line_height: Option<i64>,
    direction: Option<&'static str>,
    non_breakable_lines: Option<bool>,
    background: Option<&'static str>,
}

impl BlockStyle {
    fn emit(&self) -> String {
        let mut pairs: Vec<String> = Vec::new();
        if let Some(a) = self.alignment {
            pairs.push(format!("alignment={a}"));
        }
        if let Some(lh) = self.line_height {
            pairs.push(format!("line_height={lh}"));
        }
        if let Some(d) = self.direction {
            pairs.push(format!("direction={d}"));
        }
        if let Some(n) = self.non_breakable_lines {
            pairs.push(format!("non_breakable_lines={n}"));
        }
        if let Some(bg) = self.background {
            pairs.push(format!("background_color=\"{bg}\""));
        }
        if pairs.is_empty() {
            String::new()
        } else {
            format!("{{{}}}\n", pairs.join(" "))
        }
    }
}

#[derive(Debug, Clone)]
enum Block {
    Para(BlockStyle, Vec<Inline>),
    Heading(BlockStyle, u8, String),
    Fenced(Option<String>, String),
    Bullet(Vec<String>),
    Ordered(Vec<String>),
    Task(Vec<(bool, String)>),
    Quote(String),
    /// A table: header row + body rows, all cells plain words.
    ///
    /// Tables were absent from this generator, and that absence hid a real bug: a table
    /// puts a `U+FFFC` anchor into the text the document searches, and the cheap
    /// `djot_to_plain_text` extractor was silently omitting it — so every offset after a
    /// table was short by two characters. The parity property below could not see that,
    /// because it never generated a table.
    Table(Vec<String>, Vec<Vec<String>>),
    /// A code block inside `depth` quotations, its lines as they are: some empty, some
    /// of whitespace alone, some of backticks, which the writer's fence has to outrun.
    QuotedCode(usize, Vec<String>),
    /// A footnote whose body holds a code block with indented lines, under a label
    /// whose length decides how much the parser takes off each continuation line.
    FootnoteCode(String, Vec<String>),
    /// A list nested three levels deep, each level opened by one of the wide markers
    /// (`- [ ]`, `iii.`, `(ii)`, `100.`) or a narrow one.
    Nested(Vec<&'static str>),
    /// A paragraph whose lines are joined by hard breaks, which the reader splits into
    /// one block a line, with a page break before it or not: only its first line keeps
    /// the page break.
    HardBreaks(bool, Vec<String>),
    /// A paragraph with whitespace at its edges, kept by empty attribute sets.
    EdgeWhitespace(String, String, String),
    /// A table whose first cell holds an image named with a `|` and a link whose
    /// destination holds one, both percent-encoded in the seed.
    CellDestinations(String, String),
}

// ── Dumb emitter: AST → djot text ───────────────────────────────

fn emit_inline(i: &Inline) -> String {
    match i {
        Inline::Text(s) => s.clone(),
        Inline::Bold(s) => format!("*{s}*"),
        Inline::Italic(s) => format!("_{s}_"),
        Inline::Code(s) => format!("`{s}`"),
        Inline::Sup(s) => format!("^{s}^"),
        Inline::Sub(s) => format!("~{s}~"),
        Inline::Strike(s) => format!("{{-{s}-}}"),
        Inline::Underline(s) => format!("{{+{s}+}}"),
        Inline::Link(t, u) => format!("[{t}]({u})"),
        Inline::Image(alt, src) => format!("![{alt}]({src}){{width=60 height=90}}"),
    }
}

fn emit_block(b: &Block) -> String {
    match b {
        Block::Para(style, inlines) => format!(
            "{}{}",
            style.emit(),
            inlines.iter().map(emit_inline).collect::<String>()
        ),
        Block::Heading(style, level, s) => {
            format!("{}{} {s}", style.emit(), "#".repeat(*level as usize))
        }
        Block::Fenced(lang, content) => {
            format!("```{}\n{content}\n```", lang.as_deref().unwrap_or(""))
        }
        // Lists are emitted "loose" (blank line between items): the canonical
        // form the exporter also produces.
        Block::Bullet(items) => items
            .iter()
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n\n"),
        Block::Ordered(items) => items
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {s}", i + 1))
            .collect::<Vec<_>>()
            .join("\n\n"),
        Block::Task(items) => items
            .iter()
            .map(|(checked, s)| format!("- [{}] {s}", if *checked { 'x' } else { ' ' }))
            .collect::<Vec<_>>()
            .join("\n\n"),
        Block::Quote(s) => format!("> {s}"),
        // No alignment markers in the separator row: column alignment is a documented
        // model limitation (normalised, not preserved on round-trip), and emitting it
        // would fail the fixpoint for a reason that has nothing to do with this test.
        Block::Table(header, rows) => {
            let mut out = format!("| {} |", header.join(" | "));
            out.push_str(&format!(
                "\n|{}|",
                header.iter().map(|_| " - ").collect::<Vec<_>>().join("|")
            ));
            for row in rows {
                out.push_str(&format!("\n| {} |", row.join(" | ")));
            }
            out
        }
        Block::QuotedCode(depth, lines) => {
            let prefix = "> ".repeat(*depth);
            let fence = "`".repeat(
                lines
                    .iter()
                    .map(|l| backtick_run(l))
                    .max()
                    .unwrap_or(0)
                    .max(2)
                    + 1,
            );
            let mut out = format!("{prefix}{fence}");
            for line in lines {
                out.push('\n');
                if line.trim().is_empty() {
                    out.push_str(prefix.trim_end());
                } else {
                    out.push_str(&prefix);
                }
                out.push_str(line);
            }
            out.push_str(&format!("\n{prefix}{fence}"));
            out
        }
        Block::FootnoteCode(label, lines) => {
            // Every line one column past `[^label]:`, where the fence stands, so the
            // parser takes the same off each; a line of whitespace alone it takes
            // nothing off but that column.
            let indent = " ".repeat(label.len() + 5);
            let fence = "`".repeat(
                lines
                    .iter()
                    .map(|l| backtick_run(l))
                    .max()
                    .unwrap_or(0)
                    .max(2)
                    + 1,
            );
            let mut out = format!("See[^{label}].\n\n[^{label}]: A note.\n\n{indent}{fence}");
            for line in lines {
                out.push('\n');
                if line.trim().is_empty() {
                    if !line.is_empty() {
                        out.push(' ');
                    }
                } else {
                    out.push_str(&indent);
                }
                out.push_str(line);
            }
            out.push_str(&format!("\n{indent}{fence}"));
            out
        }
        Block::Nested(markers) => {
            let mut out = String::new();
            let mut column = 0;
            for (level, marker) in markers.iter().enumerate() {
                if level > 0 {
                    out.push_str("\n\n");
                }
                out.push_str(&format!("{}{marker} level{level}", " ".repeat(column)));
                column += marker.chars().count() + 1;
            }
            out
        }
        Block::HardBreaks(page_break, lines) => {
            let attributes = if *page_break {
                "{page_break_before=true}\n"
            } else {
                ""
            };
            format!("{attributes}{}", lines.join("\\\n"))
        }
        Block::EdgeWhitespace(lead, text, trail) => format!("{{}}{lead}{text}{trail}{{}}"),
        Block::CellDestinations(src, href) => {
            format!("| ![p]({src}){{width=6 height=9}} [t]({href}) | kept |\n|---|---|\n| a | b |")
        }
    }
}

/// The longest run of backticks in `line`.
fn backtick_run(line: &str) -> usize {
    line.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

/// The seed for `blocks`, with every footnote definition after all of them.
///
/// A definition written between two paragraphs leaves the text the document searches
/// out of step with the text `djot_to_plain_text` extracts, which the property checks;
/// that is not what these seeds are about, so a definition is written where the writer
/// puts it.
fn emit(blocks: &[Block]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut definitions: Vec<String> = Vec::new();
    for block in blocks {
        let text = emit_block(block);
        match (block, text.split_once("\n\n")) {
            (Block::FootnoteCode(..), Some((reference, definition))) => {
                parts.push(reference.to_string());
                definitions.push(definition.to_string());
            }
            _ => parts.push(text),
        }
    }
    parts.extend(definitions);
    parts.join("\n\n")
}

// ── Strategies ──────────────────────────────────────────────────

/// A single "word"-ish run used as formatted-inline content: starts and ends
/// with an alphanumeric so emphasis/verbatim delimiters bind, no newlines
/// inside. The interior may hold the characters Djot turns into smart
/// punctuation or symbols (`'`, `"`, `-`, `.`, `:`): the seed's own parse is
/// free to rewrite them, and the exporter must then write whatever it read so
/// that it reads back unchanged.
fn word() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9][a-zA-Z0-9 :\"'.-]{0,10}[a-zA-Z0-9]".prop_map(|s| s.trim().to_string())
}

/// A URL. The seed writes it raw, so it holds only characters a raw
/// destination tolerates; the characters that need percent-encoding inside a
/// formatted link are covered by `a_link_destination_survives_any_character`.
fn url() -> impl Strategy<Value = String> {
    "https://[a-z]{2,8}\\.example/[a-z0-9_:'.~-]{0,10}"
}

/// Plain text that may contain djot metacharacters in the interior to stress
/// the exporter's escaping. Starts with a letter so it isn't mistaken for a
/// block marker, contains no newlines. Backticks are excluded: a raw backtick
/// in source is a verbatim delimiter, so it represents *code*, not plain text —
/// the code path is exercised through [`Inline::Code`] instead.
///
/// `:`, `"`, `'`, `-`, `.` and `=` are in the alphabet: they are where the
/// exporter used to disagree with the parser (a symbol, a curled quote, a dash).
/// So are a tab and a no-break space, which a paragraph keeps.
fn plain_text() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9 \t\u{a0}.,!?*_~^:\"'=-]{0,24}".prop_map(|s| s.trim_end().to_string())
}

fn inline() -> impl Strategy<Value = Inline> {
    prop_oneof![
        plain_text().prop_map(Inline::Text),
        word().prop_map(Inline::Bold),
        word().prop_map(Inline::Italic),
        // Code is verbatim, so the characters the parser rewrites in text must come back
        // untouched here. No backtick: the seed writes the span with a single one.
        "[a-zA-Z0-9 :\"'.-]{1,12}".prop_map(Inline::Code),
        word().prop_map(Inline::Sup),
        word().prop_map(Inline::Sub),
        word().prop_map(Inline::Strike),
        word().prop_map(Inline::Underline),
        (word(), url()).prop_map(|(t, u)| Inline::Link(t, u)),
        (word(), image_source()).prop_map(|(alt, src)| Inline::Image(alt, src)),
    ]
}

/// Image sources as the seed writes them: the characters a destination cannot hold
/// raw percent-encoded as the writer encodes them, and `%` escapes of other characters
/// that are part of the name as it is.
fn image_source() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            4 => "[a-z0-9./_ :-]{1,4}",
            1 => prop::sample::select(vec![
                "%28", "%29", "%60", "%7B", "%7D", "%3C", "%3E", "%5C", "%7C", "%2A",
                "%5E", "%7E", "%25", "%20", "%2F", "%2525", "%",
            ]).prop_map(str::to_string),
        ],
        1..6,
    )
    .prop_map(|pieces| pieces.concat())
    .prop_filter("a source", |s| {
        !s.trim().is_empty() && !s.starts_with(' ') && !s.ends_with(' ')
    })
}

/// A line of code, as a code block holds it: any of the characters prose holds,
/// leading spaces, or a run of backticks long enough to close a short fence.
fn code_line() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[a-zA-Z0-9 ;=():>\"'.-]{0,16}",
        1 => "[ ]{0,4}`{1,5}[a-z]{0,3}",
        1 => "[ ]{1,3}",
        1 => Just(String::new()),
    ]
}

/// The markers a nested list level opens with, wide and narrow.
const NESTED_MARKERS: &[&str] = &["-", "- [ ]", "- [x]", "1.", "iii.", "(ii)", "100.", "a)"];

fn block_style() -> impl Strategy<Value = BlockStyle> {
    (
        prop::option::of(prop_oneof![
            Just("left"),
            Just("right"),
            Just("center"),
            Just("justify")
        ]),
        prop::option::of(1i64..3000),
        prop::option::of(prop_oneof![Just("ltr"), Just("rtl")]),
        prop::option::of(any::<bool>()),
        prop::option::of(prop_oneof![
            Just("#ff0000"),
            Just("#00ff00"),
            Just("yellow")
        ]),
    )
        .prop_map(
            |(alignment, line_height, direction, non_breakable_lines, background)| BlockStyle {
                alignment,
                line_height,
                direction,
                non_breakable_lines,
                background,
            },
        )
}

fn block() -> impl Strategy<Value = Block> {
    prop_oneof![
        (block_style(), prop::collection::vec(inline(), 1..5)).prop_map(|(s, i)| Block::Para(s, i)),
        (block_style(), 1u8..=6, word()).prop_map(|(st, l, s)| Block::Heading(st, l, s)),
        (
            prop::option::of(prop_oneof![
                Just("rust".to_string()),
                Just("py".to_string())
            ]),
            "[a-zA-Z0-9 ;=():\"'.-]{0,30}",
        )
            .prop_map(|(lang, c)| Block::Fenced(lang, c)),
        prop::collection::vec(word(), 1..4).prop_map(Block::Bullet),
        prop::collection::vec(word(), 1..4).prop_map(Block::Ordered),
        prop::collection::vec((any::<bool>(), word()), 1..4).prop_map(Block::Task),
        word().prop_map(Block::Quote),
        // 1..3 columns, 1..3 body rows. `cell()` excludes `|` so it cannot break the row
        // syntax it lives in.
        (1usize..4)
            .prop_flat_map(|cols| {
                (
                    prop::collection::vec(cell(), cols..=cols),
                    prop::collection::vec(prop::collection::vec(cell(), cols..=cols), 1..3),
                )
            })
            .prop_map(|(header, rows)| Block::Table(header, rows)),
        (1usize..4, prop::collection::vec(code_line(), 1..5))
            .prop_map(|(depth, lines)| Block::QuotedCode(depth, lines)),
        ("[a-z0-9-]{1,12}", prop::collection::vec(code_line(), 1..5))
            .prop_map(|(label, lines)| Block::FootnoteCode(label, lines)),
        prop::collection::vec(prop::sample::select(NESTED_MARKERS), 3..=3).prop_map(Block::Nested),
        (any::<bool>(), prop::collection::vec(plain_text(), 2..4))
            .prop_map(|(page_break, lines)| Block::HardBreaks(page_break, lines)),
        ("[ \t]{0,3}", plain_text(), "[ \t]{0,3}")
            .prop_map(|(lead, text, trail)| Block::EdgeWhitespace(lead, text, trail)),
        (image_source(), url().prop_map(|u| format!("{u}%7Cx")))
            .prop_map(|(src, href)| Block::CellDestinations(src, href)),
    ]
}

/// A table cell: a plain word with no `|` (which would break the row syntax) and no
/// leading/trailing space (which the parser trims).
fn cell() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9][a-zA-Z0-9 :\"'.-]{0,6}[a-zA-Z0-9]".prop_map(|s| s.trim().to_string())
}

// ── The fixpoint property ───────────────────────────────────────

fn set_djot(doc: &TextDocument, src: &str) {
    doc.set_djot(src).unwrap().wait().unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn djot_roundtrip_is_a_fixpoint(blocks in prop::collection::vec(block(), 1..6)) {
        let seed = emit(&blocks);

        let doc1 = TextDocument::new();
        set_djot(&doc1, &seed);
        let t1 = doc1.to_djot().unwrap();

        let doc2 = TextDocument::new();
        set_djot(&doc2, &t1);
        let t2 = doc2.to_djot().unwrap();

        // Export is a one-pass fixpoint: nothing the model represents is lost
        // when re-serialising and re-parsing.
        prop_assert_eq!(&t1, &t2, "export not a fixpoint\nseed={:?}\nt1={:?}\nt2={:?}", seed, t1, t2);

        // Observable content is identical across the round-trip.
        prop_assert_eq!(
            doc1.to_plain_text().unwrap(),
            doc2.to_plain_text().unwrap(),
            "plain text diverged"
        );
        prop_assert_eq!(doc1.block_count(), doc2.block_count(), "block count diverged");

        // ── The cheap extractor must BE the text the document searches ──────────
        //
        // `djot_to_plain_text` stops at the parse: it never creates a Block entity, never
        // touches the rope, never writes a format run. That is what makes a project-wide
        // search viable at all — importing thousands of scenes on every keystroke is not a
        // slow feature, it is a frozen app.
        //
        // But a second, *cheaper* definition of "the text" is only safe if it is not a
        // second definition. If it drifts by so much as one separator, an occurrence count
        // taken from it disagrees with what a replace re-derives inside the real document,
        // and the replace's "the text moved under me, skip this field" guard starts firing
        // on perfectly good rows.
        //
        // The check is behavioural rather than a string compare, because it pins the thing
        // that actually matters: search the document for the *whole* extracted string. If
        // the extractor and the document's search text are identical, that matches exactly
        // once, at offset 0, spanning everything. Any divergence — a lost separator, a
        // reordered block — and it does not match at all.
        //
        // NB this deliberately does NOT compare against `to_plain_text()`. That export
        // walks frames, so it orders a blockquote's prose differently from the way search
        // does (`"> a0\n\na"` exports as `"a\na0"` but is *searched* as `"a0\na"`). The
        // authority here is whatever `find_all` sees, because that is what a replace edits.
        let extracted = djot_to_plain_text(&t1, &DjotImportOptions::default());
        if !extracted.is_empty() {
            let whole = doc1.find_all(&extracted, &FindOptions::default()).unwrap();
            prop_assert_eq!(
                whole.len(),
                1,
                "the extracted text is not the text the document searches\n\
                 t1={:?}\nextracted={:?}",
                t1,
                extracted
            );
            prop_assert_eq!(whole[0].position, 0);
            prop_assert_eq!(whole[0].length, extracted.chars().count());
        }
    }
}

// ── What a writer typed survives a save and a reload ────────────
//
// The fixpoint property above starts from Djot, so it cannot see text the *first*
// parse rewrites: a seed `10:30:45` loses its `:30:` symbol on the way in, and the
// damaged document then round-trips perfectly. An editor starts from typed text
// instead, saves it with `to_djot` and reopens it with `set_djot`, and that is
// where `10:30:45` became `1045`, `I. The beginning` became a list item without
// its "I.", and every straight quote curled into an English one, all at the first
// save and for good.

/// Every ASCII punctuation mark, the letters that can open an ordered-list marker
/// (single letters and roman numerals of both cases, plus a few that are neither),
/// digits, and the spaces a manuscript holds: space, tab and no-break space.
const TYPED_ALPHABET: &str = concat!(
    "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~",
    "aAbBcCdDeEiIlLmMsSvVxXzZ",
    "0123456789",
    " \t\u{a0}",
);

/// Fragments the parser used to rewrite, so that a generated paragraph meets them
/// far more often than random characters would assemble them.
const TYPED_FRAGMENTS: &[&str] = &[
    "--",
    "---",
    "...",
    "..",
    "::",
    ":a:",
    ":+1:",
    "10:30:45",
    "std::vector",
    "I. ",
    "iv.\t",
    "A.",
    "mix. ",
    "Mild. ",
    "(c) ",
    "B) ",
    "1. ",
    "* * *",
    "- - -",
    "***",
    ":::",
    "'",
    "\"",
    "a=b",
    "[^1]",
    "\\",
    "https://example.com:8080/a_b",
    "e. e. ",
    "<!-- x -->",
    "# ",
    "> ",
    "| a |",
    "{-x=y-}",
];

fn typed_piece() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop::sample::select(TYPED_ALPHABET.chars().collect::<Vec<_>>())
            .prop_map(String::from),
        1 => prop::sample::select(TYPED_FRAGMENTS).prop_map(str::to_string),
    ]
}

/// One typed paragraph, spaces and tabs at its edges included: the parser drops them
/// from a paragraph, and the writer keeps them with an empty attribute set.
fn typed_paragraph() -> impl Strategy<Value = String> {
    prop::collection::vec(typed_piece(), 1..16)
        .prop_map(|pieces| pieces.concat())
        .prop_filter("a paragraph holds text", |s| !s.is_empty())
}

/// The editor's cycle: typed text saved with `to_djot`, reopened in a fresh document
/// with `set_djot`. Returns the saved Djot, the reopened text, and the reopened
/// document saved again.
fn save_and_reload_plain(typed: &str) -> (String, String, String) {
    let doc = TextDocument::new();
    doc.set_plain_text(typed).unwrap();
    let saved = doc.to_djot().unwrap();
    let reopened = TextDocument::new();
    set_djot(&reopened, &saved);
    (
        saved,
        reopened.to_plain_text().unwrap(),
        reopened.to_djot().unwrap(),
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn typed_plain_text_survives_save_and_reload(
        paragraphs in prop::collection::vec(typed_paragraph(), 1..4)
    ) {
        let typed = paragraphs.join("\n");
        let (saved, back, resaved) = save_and_reload_plain(&typed);
        prop_assert_eq!(&back, &typed, "typed text changed; saved as {:?}", saved);
        prop_assert_eq!(&resaved, &saved, "the second save differs from the first");
    }
}

/// The containers a line of text can sit in, as Djot opens them. Only the text after
/// the marker varies, so each seed means exactly one block of that kind.
const CONTAINER_MARKERS: &[&str] = &["- ", "* ", "1. ", "a) ", "- [ ] ", "> ", "# ", "### "];

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// A list item's text is parsed as blocks of its own, so the exporter has to guard
    /// its start the way it guards a paragraph's. The seed is written with
    /// `plain_text_to_djot`, which also checks that escaper inside each container.
    #[test]
    fn typed_text_inside_a_container_survives_save_and_reload(
        marker in prop::sample::select(CONTAINER_MARKERS),
        typed in typed_paragraph(),
    ) {
        let seed = format!("{marker}{}", plain_text_to_djot(&typed));
        let doc = TextDocument::new();
        set_djot(&doc, &seed);
        prop_assert_eq!(doc.to_plain_text().unwrap(), typed.clone(), "seed {:?}", seed);
        prop_assert_eq!(doc.block_count(), 1, "seed {:?}", seed);

        let saved = doc.to_djot().unwrap();
        let reopened = TextDocument::new();
        set_djot(&reopened, &saved);
        prop_assert_eq!(reopened.to_plain_text().unwrap(), typed, "saved as {:?}", saved);
        prop_assert_eq!(reopened.block_count(), 1, "saved as {:?}", saved);
        prop_assert_eq!(reopened.to_djot().unwrap(), saved, "the second save differs");
    }
}

/// Named cases, from typed text and from the corpus of real projects measured
/// for the Scrivener importer's design (D8-6, D5.24, D1-11, D1-3). Each one
/// changed at the first save and reload before the escaping was completed; the
/// last group never did, and guards against an escape that goes too far.
const TYPED_CASES: &[(&str, &str)] = &[
    // A symbol `:name:` was dropped whole.
    ("time with seconds", "We met at 10:30:45 sharp."),
    ("three-part number", "1:2:3"),
    ("log timestamp", "2015-06-26 13:49:02,239 WARN"),
    ("time range", "Open 9:20pm-10:00pm daily."),
    ("emoji alias", "a :smile: b"),
    ("one-letter symbol", "a :b: c"),
    ("symbol after a port", "http://example.com:8080:"),
    // `::` is an empty symbol.
    ("double colon", "Use std::vector and a::b."),
    ("double colon between digits", "ratio 2::1 here"),
    ("Scrivener placeholder", "<$Scr_Ps::0>Text<!$Scr_Ps::0>"),
    ("div fence with a class", "::: warning"),
    ("bare div fence", ":::"),
    // Straight quotes curled into English ones.
    ("French straight quotes", "Il dit \"bonjour\" en entrant."),
    ("English straight quotes", "He said \"hi\" and 'bye'."),
    ("apostrophes", "It's Anna's book, don't."),
    ("feet and inches", "He is 5'10\" tall."),
    ("quoted paragraph", "\"Quoted\""),
    // Runs of hyphens and full stops became dashes and an ellipsis.
    ("two hyphens", "Pages 10--20 only."),
    ("three hyphens", "He paused---then left."),
    ("spaced two hyphens", "a -- b"),
    ("four hyphens", "----"),
    ("hyphen separator", "---"),
    ("second paragraph separator", "one\n---\ntwo"),
    ("HTML comment", "<!-- TODO -->"),
    ("ellipsis", "Wait... what?"),
    ("leading ellipsis", "...and then"),
    ("trailing ellipsis", "Wait..."),
    ("two full stops", "a..b"),
    // A letter or roman numeral and a delimiter opened a list and lost the marker.
    ("roman initial", "I. The beginning of it all."),
    ("roman numeral", "IV. Fourth part"),
    ("lower-case roman", "i. roman"),
    ("word made of roman numerals", "mix. then stir"),
    ("another roman word", "vivid. colours"),
    ("capital initial", "A. Capital"),
    ("initial and surname", "A. Smith said so."),
    ("letter initial", "a. Primary"),
    ("x marks", "x. marks the spot"),
    ("initials", "J. P. Morrison"),
    ("lower-case initials", "e. e. cummings"),
    ("letter alone", "A."),
    ("letter and tab", "A.\tIntroduction"),
    ("roman and tab", "iv.\tFourth"),
    (
        "roman, tab, bracket",
        "I.\t[The Manuscripts of the Kebra Nagast]",
    ),
    ("initial in a second paragraph", "intro\nS. Russell's view"),
    // Shapes that were right already, kept right.
    ("letter and parenthesis", "B) plan"),
    ("letter in parentheses", "(c) third"),
    ("digit in parentheses", "(1) fourth"),
    ("digit and tab", "1.\tIn the main menu"),
    ("key and value", "x=5"),
    ("formula", "E=mc2"),
    (
        "pipe table typed as text",
        "intro\n| a | b |\n| --- | --- |",
    ),
    ("smileys", ":-) and :)"),
    ("time", "At 10:30 exactly."),
    ("URL with a port", "https://example.com:8080/a_b_c"),
    ("French spaced colon", "Il dit : oui ; non !"),
    ("double space", "One.  Two."),
    ("letter and parenthesis, lower case", "a) first we eat."),
    ("year", "1984. That was the year."),
    ("abbreviation", "Mr. Smith"),
    ("capitalised word of roman numerals", "Mild. Very mild."),
    ("decimal", "3.14 is pi"),
    ("hash", "# not a heading"),
    ("greater-than", "> not a quote"),
    ("plus", "+ plus sign start"),
    ("dialogue dash", "- a dialogue dash line"),
    ("spaced stars", "* * *"),
    ("stars", "***"),
    ("spaced hyphens", "- - -"),
    ("lone hash", "#"),
    ("inner tab", "a\tb"),
    ("no-break space", "a\u{a0}: b"),
    ("no-break space at the edges", "\u{a0}a\u{a0}"),
    // The parser dropped a paragraph's edge whitespace.
    ("tab-indented paragraph", "\tShe opened the door."),
    ("trailing spaces", "End of line.  "),
    ("spaces at both ends", "  both  "),
    ("whitespace alone", " \t "),
    ("form feed at an edge", "\u{c}page\u{c}"),
    ("tab before a marker", "\t- (void)someMethod"),
];

#[test]
fn typed_text_that_the_parser_used_to_rewrite_survives_save_and_reload() {
    let mut failures = Vec::new();
    for (name, typed) in TYPED_CASES {
        let (saved, back, resaved) = save_and_reload_plain(typed);
        if back != *typed || resaved != saved {
            failures.push(format!(
                "{name}: typed {typed:?}, saved {saved:?}, reopened {back:?}, resaved {resaved:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A marker behind leading spaces or tabs is still a marker to the parser. The
/// whitespace, the marker and the rest of the text all stay: the parser drops a
/// paragraph's edge whitespace, and an empty attribute set in front of it keeps it.
#[test]
fn a_marker_behind_leading_whitespace_stays_text() {
    let mut failures = Vec::new();
    for typed in [
        "\t- (void)someMethod",
        " \t1.\tIn the main menu",
        "\t## Section 2",
        "   I. Indented",
        "\t:::",
        "  > quoted",
    ] {
        let (saved, back, resaved) = save_and_reload_plain(typed);
        if back != typed || resaved != saved || !saved.starts_with("{}") {
            failures.push(format!(
                "typed {typed:?}, saved {saved:?}, reopened {back:?}, resaved {resaved:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ── Shapes only the exporter writes ─────────────────────────────

/// What Djot can say about one character, and so what a save and reload must keep.
#[derive(Debug, Clone, PartialEq)]
struct VisibleStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    raised: Option<CharVerticalAlignment>,
    code: bool,
    /// Percent-decoded: a save may percent-encode a destination once.
    href: Option<String>,
}

impl VisibleStyle {
    fn of(format: &TextFormat) -> Self {
        VisibleStyle {
            bold: format.font_bold == Some(true),
            italic: format.font_italic == Some(true),
            underline: format.font_underline == Some(true),
            strike: format.font_strikeout == Some(true),
            raised: format
                .vertical_alignment
                .clone()
                .filter(|v| *v != CharVerticalAlignment::Normal),
            code: format.font_family.as_deref() == Some("monospace"),
            href: format.anchor_href.as_deref().map(percent_decode),
        }
    }
}

/// Undo a destination's percent-encoding (the test hrefs hold no `%` of their own).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

/// Every character of the document with its visible style. Spaces and tabs carry
/// `None`: the exporter writes them outside the marks on purpose, since Djot's
/// delimiters cannot sit against whitespace.
fn styled_chars(doc: &TextDocument) -> Vec<(char, Option<VisibleStyle>)> {
    let mut out = Vec::new();
    for (i, block) in doc.blocks().iter().enumerate() {
        if i > 0 {
            out.push(('\n', None));
        }
        for fragment in block.fragments() {
            match fragment {
                FragmentContent::Text { text, format, .. } => {
                    let style = VisibleStyle::of(&format);
                    for c in text.chars() {
                        let visible = !matches!(c, ' ' | '\t');
                        out.push((c, visible.then(|| style.clone())));
                    }
                }
                _ => out.push(('\u{FFFC}', None)),
            }
        }
    }
    out
}

/// A document with one paragraph made of `runs`, every field of each run's format
/// applied. See [`document_of_pieces`].
fn document_of_runs(runs: &[(&str, TextFormat)]) -> TextDocument {
    let pieces: Vec<Piece> = runs
        .iter()
        .map(|(text, format)| Piece::Run(text.to_string(), format.clone()))
        .collect();
    document_of_pieces(&pieces)
}

/// One piece of a paragraph, inserted the way the editor inserts it.
#[derive(Debug, Clone)]
enum Piece {
    /// Text in one format.
    Run(String, TextFormat),
    /// An image, with a display size as the editor always gives one. Djot writes the
    /// size as an attribute set, which the parser continues with a `{` written after it.
    Image,
    /// An image under a name the writer has to encode (see [`ODD_IMAGE_NAMES`]).
    NamedImage(&'static str),
}

/// A document with one paragraph made of `pieces`, every field of each run's format
/// applied.
///
/// `insert_formatted_text` applies the font fields only (family, size, bold, italic,
/// underline, strikeout) and drops the link and the vertical alignment, so those two
/// are merged onto each run's range once all the text is in; merging them only then
/// also keeps a run from inheriting its neighbour's as it is typed. The document is
/// checked against `pieces` before it is returned, so no test built on it can pass
/// without the formatting it names.
fn document_of_pieces(pieces: &[Piece]) -> TextDocument {
    let doc = TextDocument::new();
    let cursor = doc.cursor();
    let mut ranges = Vec::with_capacity(pieces.len());
    for piece in pieces {
        let start = cursor.position();
        match piece {
            Piece::Run(text, format) => {
                cursor.insert_formatted_text(text, format).unwrap();
                ranges.push((start, cursor.position(), format));
            }
            Piece::Image => cursor
                .insert_image("assets/plate.png", "plate", 600, 900)
                .unwrap(),
            Piece::NamedImage(name) => cursor.insert_image(name, "plate", 600, 900).unwrap(),
        }
    }
    for &(start, end, format) in &ranges {
        if format.anchor_href.is_none() && format.vertical_alignment.is_none() {
            continue;
        }
        let selection = doc.cursor_at(start);
        selection.set_position(end, MoveMode::KeepAnchor);
        selection
            .merge_char_format(&TextFormat {
                anchor_href: format.anchor_href.clone(),
                vertical_alignment: format.vertical_alignment.clone(),
                ..Default::default()
            })
            .unwrap();
    }
    let expected: Vec<(char, Option<VisibleStyle>)> = pieces
        .iter()
        .flat_map(|piece| match piece {
            Piece::Run(text, format) => {
                let style = VisibleStyle::of(format);
                text.chars()
                    .map(|c| (c, (!matches!(c, ' ' | '\t')).then(|| style.clone())))
                    .collect::<Vec<_>>()
            }
            Piece::Image | Piece::NamedImage(_) => vec![('\u{FFFC}', None)],
        })
        .collect();
    assert_eq!(
        styled_chars(&doc),
        expected,
        "the document does not hold the pieces it was built from"
    );
    doc
}

/// Save `doc`, reopen it, and check the text and every character's visible style
/// survived, and that saving the reopened document changes nothing further. The
/// error names what changed.
fn check_save_and_reload(doc: &TextDocument) -> Result<(), String> {
    let before = styled_chars(doc);
    let saved = doc.to_djot().unwrap();
    let reopened = TextDocument::new();
    set_djot(&reopened, &saved);
    let after = styled_chars(&reopened);
    let text = |v: &[(char, Option<VisibleStyle>)]| v.iter().map(|(c, _)| *c).collect::<String>();
    if text(&after) != text(&before) {
        return Err(format!(
            "the text changed from {:?} to {:?}; saved as {saved:?}",
            text(&before),
            text(&after)
        ));
    }
    if after != before {
        return Err(format!("a style changed; saved as {saved:?}"));
    }
    let resaved = reopened.to_djot().unwrap();
    let again = TextDocument::new();
    set_djot(&again, &resaved);
    if again.to_djot().unwrap() != resaved {
        return Err(format!("{resaved:?} does not save stably"));
    }
    Ok(())
}

fn assert_survives_save_and_reload(doc: &TextDocument, what: &str) {
    if let Err(e) = check_save_and_reload(doc) {
        panic!("{what}: {e}");
    }
}

fn bold() -> TextFormat {
    TextFormat {
        font_bold: Some(true),
        ..Default::default()
    }
}

fn struck() -> TextFormat {
    TextFormat {
        font_strikeout: Some(true),
        ..Default::default()
    }
}

fn sized(points: u32) -> TextFormat {
    TextFormat {
        font_point_size: Some(points),
        ..Default::default()
    }
}

/// An item's text is parsed as blocks of its own, so `- 1. x` is a nested list and
/// `- # x` a heading inside the item. The exporter guarded a paragraph's start and
/// not an item's.
#[test]
fn a_list_item_whose_text_looks_like_a_marker_stays_one_item() {
    let doc = TextDocument::new();
    set_djot(
        &doc,
        "- 1\\. not a sublist\n\n- \\# hash item\n\n- He said \\\"hi\\\" at 10\\:30\\:45\n\n\
         - I\\. roman\n\n- \\> quote\n\n- [ ] 2\\. a step",
    );
    let typed =
        "1. not a sublist\n# hash item\nHe said \"hi\" at 10:30:45\nI. roman\n> quote\n2. a step";
    assert_eq!(doc.to_plain_text().unwrap(), typed);

    let saved = doc.to_djot().unwrap();
    let reopened = TextDocument::new();
    set_djot(&reopened, &saved);
    assert_eq!(
        reopened.to_plain_text().unwrap(),
        typed,
        "saved as {saved:?}"
    );
    let blocks = reopened.blocks();
    assert_eq!(blocks.len(), 6, "saved as {saved:?}");
    for block in &blocks {
        let list = block.list();
        assert!(
            list.as_ref().is_some_and(|l| l.indent() == 0),
            "{:?} is no longer a top-level item; saved as {saved:?}",
            block.text()
        );
        assert_eq!(
            block.block_format().heading_level,
            None,
            "saved as {saved:?}"
        );
    }
    assert_survives_save_and_reload(&doc, "list items");
}

/// `{-debug=true-}` is a deletion to the inline parser and, because `-` may start an
/// attribute key, a block attribute to the block parser, which reads it first and
/// deletes the paragraph.
#[test]
fn a_struck_paragraph_shaped_like_an_attribute_keeps_its_text() {
    for text in ["debug=true", "a=b c=d", "x:y=z"] {
        let doc = document_of_runs(&[(text, struck())]);
        assert_survives_save_and_reload(&doc, text);
    }
}

/// An image carries its size as an attribute set, and the parser reads a `{` straight
/// after an attribute set as more attributes for the image: a struck `x=5` written there
/// as `{-x=5-}` became the attribute `-x="5-"`, and the text was gone at the reload.
#[test]
fn a_struck_run_after_an_image_keeps_its_text() {
    for text in ["x=5", "=C", "a=b c=d", "debug=true", "x:y=z"] {
        let doc = document_of_pieces(&[
            Piece::Run("See".to_string(), TextFormat::default()),
            Piece::Image,
            Piece::Run(text.to_string(), struck()),
            Piece::Run(" then more.".to_string(), TextFormat::default()),
        ]);
        assert_survives_save_and_reload(&doc, text);
    }
    // A struck run that is not attribute-shaped, and one after a space, are written as
    // they always were.
    for (pieces, written) in [
        (
            vec![Piece::Image, Piece::Run("gone".to_string(), struck())],
            "{-gone-}",
        ),
        (
            vec![
                Piece::Image,
                Piece::Run(" ".to_string(), TextFormat::default()),
                Piece::Run("x=5".to_string(), struck()),
            ],
            " {-x=5-}",
        ),
    ] {
        let doc = document_of_pieces(&pieces);
        let saved = doc.to_djot().unwrap();
        assert!(saved.ends_with(written), "saved as {saved:?}");
        assert_survives_save_and_reload(&doc, written);
    }
}

/// A bold `-` is written `*-*`, which is also a thematic break.
#[test]
fn a_bold_dash_paragraph_is_not_a_thematic_break() {
    for text in ["-", "- -", "- - -"] {
        let doc = document_of_runs(&[(text, bold())]);
        assert_survives_save_and_reload(&doc, text);
    }
}

/// A paragraph opening with a footnote reference followed by a colon is written
/// `[^1]: …`, which is a footnote *definition*.
#[test]
fn a_paragraph_opening_with_a_footnote_reference_and_a_colon_stays_a_paragraph() {
    let doc = TextDocument::new();
    set_djot(&doc, "[^1]\\: as the note says.\n\n[^1]: The note.");
    let before = doc.to_plain_text().unwrap();
    assert_eq!(doc.footnote_references().len(), 1);

    let saved = doc.to_djot().unwrap();
    let reopened = TextDocument::new();
    set_djot(&reopened, &saved);
    assert_eq!(
        reopened.to_plain_text().unwrap(),
        before,
        "saved as {saved:?}"
    );
    assert_eq!(
        reopened.footnote_references().len(),
        1,
        "the reference is gone; saved as {saved:?}"
    );
}

/// Two runs Djot cannot tell apart (they differ in size only) are written side by
/// side with nothing between them, so each must be escaped knowing its neighbour:
/// `wrote-` and `-with` are harmless alone and an en dash together.
#[test]
fn runs_djot_cannot_tell_apart_are_escaped_as_one_line() {
    for (first, second) in [
        ("wrote-", "-with"),
        ("10:3", "0:45"),
        ("std:", ":vector"),
        (":smi", "le:"),
        ("Wait.", ".. what"),
        ("a", "--b"),
    ] {
        let doc = document_of_runs(&[(first, sized(12)), (second, sized(16))]);
        let fragments = doc.blocks()[0].fragments().len();
        assert_eq!(fragments, 2, "the two runs must really be separate");
        assert_survives_save_and_reload(&doc, &format!("{first:?} + {second:?}"));
    }
}

fn code() -> TextFormat {
    TextFormat {
        font_family: Some("monospace".to_string()),
        ..Default::default()
    }
}

/// jotdown reads the token after a code span's closing fence while still in verbatim
/// mode, so an escape there escaped nothing (`` `a`\" `` curled the quote) and a
/// backtick there lengthened the fence (two code runs side by side came back as one
/// with the fences as text).
#[test]
fn what_follows_a_code_span_keeps_its_meaning() {
    let cases: Vec<Vec<(&str, TextFormat)>> = vec![
        vec![("a", code()), ("\"quoted\"", TextFormat::default())],
        vec![("a", code()), ("_not italic_", TextFormat::default())],
        vec![("a", code()), ("\\", TextFormat::default())],
        vec![
            ("a", code()),
            (
                "b",
                TextFormat {
                    font_point_size: Some(16),
                    ..code()
                },
            ),
        ],
    ];
    for runs in &cases {
        let doc = document_of_runs(runs);
        assert_survives_save_and_reload(&doc, &format!("{runs:?}"));
    }
}

/// Djot drops whitespace between a code span's fence and a backtick, so code that
/// holds a space next to a backtick lost it. The space is written outside the span.
#[test]
fn a_code_span_keeps_a_space_next_to_a_backtick() {
    for text in [" `x", "x` ", " ` ", "`x`", "a `b` c"] {
        let doc = document_of_runs(&[
            ("see ", TextFormat::default()),
            (text, code()),
            (" end", TextFormat::default()),
        ]);
        assert_survives_save_and_reload(&doc, text);
    }
}

/// Every wrapper a link can sit in, as the exporter writes it.
fn link_wrappers() -> Vec<(&'static str, TextFormat)> {
    let with = |f: fn(&mut TextFormat)| {
        let mut format = TextFormat::default();
        f(&mut format);
        format
    };
    vec![
        ("plain", TextFormat::default()),
        ("bold", with(|f| f.font_bold = Some(true))),
        ("italic", with(|f| f.font_italic = Some(true))),
        (
            "bold italic",
            with(|f| {
                f.font_bold = Some(true);
                f.font_italic = Some(true);
            }),
        ),
        ("underlined", with(|f| f.font_underline = Some(true))),
        ("struck", with(|f| f.font_strikeout = Some(true))),
        (
            "superscript",
            with(|f| f.vertical_alignment = Some(CharVerticalAlignment::SuperScript)),
        ),
        (
            "subscript",
            with(|f| f.vertical_alignment = Some(CharVerticalAlignment::SubScript)),
        ),
    ]
}

/// A destination is the raw source between `(` and `)`, but the inline lexer still
/// runs over it: a `)` ends it early, a backtick opens verbatim, and inside a bold
/// link a `*` closes the bold and the link comes back as literal brackets. The
/// `<…>` form the exporter used for some of them is not Djot: the brackets became
/// part of the URL and were encoded again at every save.
///
/// Each link is followed by text holding the characters that close something (`]`,
/// `)`, `}`, `>`), since a construct the destination opens reads on past its end: a
/// `[^` in it is a footnote reference that runs to the next `]` of the paragraph.
#[test]
fn a_link_destination_survives_any_character() {
    let mut hrefs = Vec::new();
    for c in (0x21u8..0x7f)
        .map(char::from)
        .filter(|c| c.is_ascii_punctuation())
    {
        hrefs.push(format!("https://example.com/a{c}b"));
        hrefs.push(format!("https://example.com/a{c}"));
        hrefs.push(format!("https://example.com/a{c}{c}b{c}"));
    }
    hrefs.extend(
        [
            "https://example.com/[^a",
            "https://example.com/a[^",
            "https://example.com/[^]",
            "http://[::1]:8080/[^note",
        ]
        .map(str::to_string),
    );
    let mut failures = Vec::new();
    for href in &hrefs {
        for (wrapper, base) in link_wrappers() {
            for text in ["text", href.as_str()] {
                let format = TextFormat {
                    anchor_href: Some(href.clone()),
                    ..base.clone()
                };
                let doc = document_of_runs(&[
                    (text, format),
                    (" and b] c) d} e> f", TextFormat::default()),
                ]);
                let saved = doc.to_djot().unwrap();
                if !saved.contains("](") {
                    failures.push(format!("{wrapper} link {href:?} saved as {saved:?}"));
                }
                if let Err(e) = check_save_and_reload(&doc) {
                    failures.push(format!("{wrapper} link {href:?} with text {text:?}: {e}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Only the caret of a `[^` is encoded: the `[` of a host address is legal as it is.
#[test]
fn a_footnote_shaped_destination_is_broken_at_its_caret() {
    for (href, written) in [
        ("https://example.com/[^a", "https://example.com/[%5Ea"),
        ("http://[::1]:8080/a", "http://[::1]:8080/a"),
        ("http://[::1]:8080/[^b", "http://[::1]:8080/[%5Eb"),
    ] {
        let doc = document_of_runs(&[("t", link(href)), (" and b] c", TextFormat::default())]);
        let saved = doc.to_djot().unwrap();
        assert!(
            saved.contains(&format!("]({written})")),
            "{href:?} saved as {saved:?}"
        );
        assert_survives_save_and_reload(&doc, href);
    }
}

/// A URL used as its own link text is prose to the escaper: its `--`, `:b:` and
/// `'` were rewritten like any other text.
#[test]
fn a_link_whose_text_is_its_url_survives() {
    for url in [
        "http://example.com/a--b",
        "http://example.com/a:b:c",
        "http://example.com/it's",
        "http://example.com/wiki/Foo_(bar)",
        "http://example.com/a...b",
    ] {
        for (wrapper, base) in link_wrappers() {
            let format = TextFormat {
                anchor_href: Some(url.to_string()),
                ..base
            };
            let doc = document_of_runs(&[("see ", TextFormat::default()), (url, format)]);
            let saved = doc.to_djot().unwrap();
            assert!(saved.contains("]("), "{wrapper} {url}: saved as {saved:?}");
            assert_survives_save_and_reload(&doc, &format!("{wrapper} {url}"));
        }
    }
}

/// A backslash in a destination is kept as written. The lexer reads one before
/// punctuation as an escape, but the destination is taken from its source bytes, and a
/// character that would act there is encoded whatever precedes it. `%5C` is a different
/// link, since a URL parser reads a `\` in a web or file address as `/`, and 1.12.2
/// wrote these exactly: an earlier rule encoded every backslash before punctuation, and
/// `C:\Users\Anna\.config` came back as `C:\Users\Anna%5C.config` for good.
///
/// Only the last of an odd run of backslashes ending the destination is encoded, since
/// it would escape the closing `)`.
#[test]
fn a_backslash_in_a_link_destination_is_kept_as_written() {
    for href in [
        "file:///C:\\Users\\me\\notes.txt",
        "http://example.com/a\\b",
        "file:///C:\\Users\\Anna\\Documents\\_Research\\map.pdf",
        "file:///C:\\Users\\Anna\\.config\\app.ini",
        "\\\\server\\share\\notes.txt",
        "http://example.com/a\\.b",
        "http://example.com/a\\ b",
        "http://example.com/a\\\\b",
        "http://example.com/a\\\\",
        "http://example.com/a\\*b",
        "http://example.com/a\\]b",
        "http://example.com/a\\[^b",
    ] {
        let doc = document_of_runs(&[("t", link(href)), (" and b] c) d", TextFormat::default())]);
        let saved = doc.to_djot().unwrap();
        let reopened = TextDocument::new();
        set_djot(&reopened, &saved);
        assert_eq!(
            hrefs(&reopened),
            vec![href.replace("[^", "[%5E")],
            "saved as {saved:?}"
        );
        assert_survives_save_and_reload(&doc, href);
    }
    for (href, written) in [
        ("http://example.com/a\\", "a%5C)"),
        ("http://example.com/a\\\\\\", "a\\\\%5C)"),
    ] {
        let doc = document_of_runs(&[("t", link(href))]);
        let saved = doc.to_djot().unwrap();
        assert!(saved.contains(written), "{href:?} saved as {saved:?}");
        assert_survives_save_and_reload(&doc, href);
    }
}

/// The destinations of every link in `doc`, as stored, in order.
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

fn link(href: &str) -> TextFormat {
    TextFormat {
        anchor_href: Some(href.to_string()),
        ..Default::default()
    }
}

/// `![` opens an image, so a link written straight after a `!` came back as an image
/// placeholder, losing the `!` and the link text, whether or not a code span came
/// just before the `!`.
#[test]
fn a_link_after_an_exclamation_mark_stays_a_link() {
    let cases: Vec<Vec<(&str, TextFormat)>> = vec![
        vec![
            ("Yahoo!", TextFormat::default()),
            ("the site", link("https://yahoo.com")),
        ],
        vec![
            ("!", TextFormat::default()),
            ("start", link("https://example.com")),
        ],
        vec![
            ("a", code()),
            ("!", TextFormat::default()),
            ("b", link("https://example.com")),
        ],
    ];
    for runs in &cases {
        let doc = document_of_runs(runs);
        assert_survives_save_and_reload(&doc, &format!("{runs:?}"));
    }
}

/// A footnote reference written straight after a `!` read `![^1]`, the start of an
/// image, and came back as the literal text `[^1]`, the note losing its anchor. (The
/// last seed separates the two with `{}`: straight after a code span a backslash
/// escapes nothing.)
#[test]
fn a_footnote_reference_after_an_exclamation_mark_stays_a_reference() {
    for seed in [
        "It was over\\![^1] she said.\n\n[^1]: The note.",
        "\\![^1]\n\n[^1]: The note.",
        "`code`!{}[^1]\n\n[^1]: The note.",
    ] {
        let doc = TextDocument::new();
        set_djot(&doc, seed);
        assert_eq!(doc.footnote_references().len(), 1, "seed {seed:?}");
        let before = doc.to_plain_text().unwrap();

        let saved = doc.to_djot().unwrap();
        let reopened = TextDocument::new();
        set_djot(&reopened, &saved);
        assert_eq!(
            reopened.to_plain_text().unwrap(),
            before,
            "saved as {saved:?}"
        );
        assert_eq!(
            reopened.footnote_references().len(),
            1,
            "the reference is gone; saved as {saved:?}"
        );
        assert_eq!(
            reopened.to_djot().unwrap(),
            saved,
            "the second save differs"
        );
    }
}

/// A `$` or `$$` right before a code span's opening fence makes the span inline or
/// display math, and the `$` and the code text both disappeared at the reload.
#[test]
fn a_dollar_sign_before_a_code_span_is_not_read_as_math() {
    let cases: Vec<Vec<(&str, TextFormat)>> = vec![
        vec![
            ("cost $", TextFormat::default()),
            ("5", code()),
            (" more", TextFormat::default()),
        ],
        vec![("cost $$", TextFormat::default()), ("5", code())],
        vec![("$", TextFormat::default()), ("x", code())],
        vec![("a", code()), ("$", TextFormat::default()), ("b", code())],
        vec![("a\\$", TextFormat::default()), ("b", code())],
    ];
    for runs in &cases {
        let doc = document_of_runs(runs);
        assert_survives_save_and_reload(&doc, &format!("{runs:?}"));
    }
}

/// A table row whose cells each read `-`, `:-` or `-:` is a separator row to the
/// parser, which drops it: a row of placeholders vanished, and, as the first row, took
/// the header row with it.
#[test]
fn a_table_row_of_dashes_keeps_its_text() {
    for seed in [
        "| a | b |\n|---|---|\n| x | y |\n| \\- | \\- |",
        "| \\- | \\- |\n|---|---|\n| x | y |",
        "| a | b |\n|---|---|\n| \\:- | -\\: |",
        "| a | b | c |\n|---|---|---|\n| \\- | \\:- | -\\: |",
        "| a |\n|---|\n| \\- |",
    ] {
        let doc = TextDocument::new();
        set_djot(&doc, seed);
        let before = doc.to_plain_text().unwrap();
        assert!(before.contains('-'), "seed {seed:?} read as {before:?}");

        let saved = doc.to_djot().unwrap();
        let reopened = TextDocument::new();
        set_djot(&reopened, &saved);
        assert_eq!(
            reopened.to_plain_text().unwrap(),
            before,
            "saved as {saved:?}"
        );
        assert_eq!(
            reopened.to_djot().unwrap(),
            saved,
            "the second save differs"
        );
    }
}

/// A paragraph opening with a link whose text is code holding `]:` reads as a
/// definition to the block parser. The usual guard, escaping the colon, would land
/// inside the code span, where a backslash is text.
#[test]
fn a_definition_shape_inside_a_code_span_keeps_its_text() {
    let code_link = TextFormat {
        anchor_href: Some("https://example.com".to_string()),
        ..code()
    };
    for runs in [
        vec![("]: x tail", code_link.clone())],
        vec![
            ("]: x", code_link.clone()),
            (" and more", TextFormat::default()),
        ],
    ] {
        let doc = document_of_runs(&runs);
        assert_survives_save_and_reload(&doc, &format!("{runs:?}"));
    }
}

// ── Styled runs, the shape the editor actually saves ────────────

/// Destinations with characters that need encoding in some wrapper or other.
const STYLED_HREFS: &[&str] = &[
    "https://example.com/a_b",
    "https://example.com/wiki/Foo_(bar)",
    "https://example.com/a*b^c~d",
    "https://example.com/path with space",
    "https://example.com/a\\b`c{d}e<f>",
    "mailto:someone@example.com",
    "file:///C:\\Users\\Anna\\.config\\app.ini",
    "https://fonts.googleapis.com/css?family=Lora|Inter",
    "https://example.com/a\\",
];

#[derive(Debug, Clone)]
struct StyledRun {
    text: String,
    format: TextFormat,
}

fn styled_run() -> impl Strategy<Value = StyledRun> {
    (
        prop::collection::vec(typed_piece(), 1..6).prop_map(|p| p.concat()),
        (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()),
        prop_oneof![
            4 => Just(None),
            1 => Just(Some(CharVerticalAlignment::SuperScript)),
            1 => Just(Some(CharVerticalAlignment::SubScript)),
        ],
        prop::bool::weighted(0.1),
        prop::option::of(prop_oneof![Just(10u32), Just(16u32)]),
        prop::option::weighted(0.2, prop::sample::select(STYLED_HREFS)),
    )
        .prop_map(
            |(text, (bold, italic, underline, strike), raised, code, size, href)| StyledRun {
                text,
                format: TextFormat {
                    font_bold: bold.then_some(true),
                    font_italic: italic.then_some(true),
                    font_underline: underline.then_some(true),
                    font_strikeout: strike.then_some(true),
                    vertical_alignment: raised,
                    font_family: code.then(|| "monospace".to_string()),
                    font_point_size: size,
                    anchor_href: href.map(str::to_string),
                    ..Default::default()
                },
            },
        )
}

/// A styled run, or now and then an image: the exporter writes an image's size as an
/// attribute set, which reaches out to whatever is written straight after it.
fn styled_piece() -> impl Strategy<Value = Piece> {
    prop_oneof![
        8 => styled_run().prop_map(|r| Piece::Run(r.text, r.format)),
        1 => Just(Piece::Image),
        1 => prop::sample::select(ODD_IMAGE_NAMES).prop_map(Piece::NamedImage),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Text and per-character style of a paragraph built from formatted runs and
    /// images, the way the editor holds it, survive a save and reload. This is where
    /// escaping meets the exporter's own markup and the boundaries between runs.
    #[test]
    fn styled_runs_survive_save_and_reload(pieces in prop::collection::vec(styled_piece(), 1..6)) {
        let doc = document_of_pieces(&pieces);
        let before = styled_chars(&doc);
        let saved = doc.to_djot().unwrap();
        let reopened = TextDocument::new();
        set_djot(&reopened, &saved);
        let after = styled_chars(&reopened);
        let plain = |v: &[(char, Option<VisibleStyle>)]| v.iter().map(|(c, _)| *c).collect::<String>();
        prop_assert_eq!(plain(&after), plain(&before), "text changed; saved as {:?}", saved);
        prop_assert_eq!(after, before, "a style changed; saved as {:?}", saved);
        prop_assert_eq!(image_names(&reopened), image_names(&doc), "saved as {:?}", saved);
        // Runs Djot cannot tell apart (a size) are one run once reloaded, so it is the
        // save after the first that has to settle.
        let resaved = reopened.to_djot().unwrap();
        let again = TextDocument::new();
        set_djot(&again, &resaved);
        prop_assert_eq!(again.to_djot().unwrap(), resaved, "the save does not settle");
    }

    /// The same runs and images in a table cell, where a `|` in a destination splits
    /// the row, beside a cell whose text has to keep its place.
    #[test]
    fn styled_runs_in_a_table_cell_survive_save_and_reload(
        pieces in prop::collection::vec(styled_piece(), 1..5)
    ) {
        let doc = TextDocument::new();
        let table = doc.cursor().insert_table(1, 2).unwrap();
        let second = table.cell(0, 1).unwrap().blocks()[0].position();
        doc.cursor_at(second).insert_text("kept").unwrap();
        let start = table.cell(0, 0).unwrap().blocks()[0].position();
        let cursor = doc.cursor_at(start);
        let mut links = Vec::new();
        for piece in &pieces {
            let from = cursor.position();
            match piece {
                Piece::Run(text, format) => {
                    cursor.insert_formatted_text(text, format).unwrap();
                    if format.anchor_href.is_some() || format.vertical_alignment.is_some() {
                        links.push((from, cursor.position(), format.clone()));
                    }
                }
                Piece::Image => cursor.insert_image("assets/plate.png", "p", 60, 90).unwrap(),
                Piece::NamedImage(name) => cursor.insert_image(name, "p", 60, 90).unwrap(),
            }
        }
        for (from, to, format) in links {
            let selection = doc.cursor_at(from);
            selection.set_position(to, MoveMode::KeepAnchor);
            selection
                .merge_char_format(&TextFormat {
                    anchor_href: format.anchor_href.clone(),
                    vertical_alignment: format.vertical_alignment.clone(),
                    ..Default::default()
                })
                .unwrap();
        }
        let saved = doc.to_djot().unwrap();
        let reopened = TextDocument::new();
        set_djot(&reopened, &saved);
        prop_assert_eq!(image_names(&reopened), image_names(&doc), "saved as {:?}", saved);
        // A link on whitespace alone is written as the whitespace, as in a paragraph.
        prop_assert_eq!(
            visible_hrefs(&reopened),
            visible_hrefs(&doc),
            "saved as {:?}", saved
        );
        let cells: Vec<String> = reopened
            .blocks()
            .iter()
            .filter(|b| b.table_cell().is_some())
            .map(|b| b.text())
            .collect();
        prop_assert_eq!(cells.last().map(String::as_str), Some("kept"), "saved as {:?}", saved);
        prop_assert_eq!(cells.len(), 2, "saved as {:?}", saved);
        let resaved = reopened.to_djot().unwrap();
        let again = TextDocument::new();
        set_djot(&again, &resaved);
        prop_assert_eq!(again.to_djot().unwrap(), resaved, "the save does not settle");
    }

    /// A block holding line breaks, however they got there, saves with each line as
    /// text: nothing nests, every line that holds something comes back as a block of
    /// its own in the same list, heading or quotation, each saved one line long, and
    /// the first save is already a fixpoint. A page break stays on the first line. The
    /// same text pasted as preformatted HTML is stored one block a line.
    #[test]
    fn a_block_holding_line_breaks_saves_its_lines_as_text(
        lines in prop::collection::vec(prop_oneof![3 => typed_paragraph(), 1 => Just(String::new())], 2..5),
        container in prop::sample::select(&["", "- ", "# ", "> ", "1. "][..]),
        page_break in any::<bool>(),
    ) {
        let text = lines.join("\n");
        let doc = TextDocument::new();
        // Block attributes go on a paragraph or a heading, in a quotation or not; a list
        // item takes none.
        let takes_attributes = matches!(container, "" | "# " | "> ");
        let page_break = page_break && takes_attributes;
        let attributes = if page_break {
            format!("{}{{page_break_before=true}}\n", container.trim_start_matches("# "))
        } else {
            String::new()
        };
        set_djot(&doc, &format!("{attributes}{container}x"));
        let block = doc.blocks()[0].clone();
        let shape = block_shapes(&doc)[0].clone();
        doc.cursor_at(block.position() + 1).insert_text(&text).unwrap();
        let (saved, reopened, stable) = save_reopen(&doc);
        prop_assert_eq!(nesting_depth(&saved), usize::from(!container.is_empty() && container != "# "), "saved as {:?}", saved);
        prop_assert_eq!(longest_leaf_lines(&saved), 1, "saved as {:?}", saved);
        let expected: Vec<_> = non_empty_lines(&format!("x{text}"))
            .into_iter()
            .map(|line| (line, shape.1, shape.2, shape.3))
            .collect();
        prop_assert_eq!(block_shapes(&reopened), expected, "saved as {:?}", saved);
        let breaks: Vec<Option<bool>> = reopened
            .blocks()
            .iter()
            .map(|block| block.block_format().page_break_before)
            .collect();
        let expected_breaks: Vec<Option<bool>> = (0..breaks.len())
            .map(|i| (page_break && i == 0).then_some(true))
            .collect();
        prop_assert_eq!(breaks, expected_breaks, "saved as {:?}", saved);
        prop_assert_eq!(reopened.to_djot().unwrap(), saved.clone(), "the first save is not the fixpoint");
        prop_assert!(stable, "{:?} does not save stably", saved);

        let html: String = text
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let pasted = TextDocument::new();
        pasted.cursor().insert_html(&format!("<pre>{html}</pre>")).unwrap();
        prop_assert!(block_texts(&pasted).iter().all(|t| !t.contains('\n')));
        let (saved, reopened, stable) = save_reopen(&pasted);
        prop_assert_eq!(nesting_depth(&saved), 0, "saved as {:?}", saved);
        prop_assert_eq!(
            block_texts(&reopened).into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>(),
            non_empty_lines(&text),
            "saved as {:?}", saved
        );
        prop_assert!(stable, "{:?} does not save stably", saved);
    }
}

// ── Shapes the 1.12.3 review found ──────────────────────────────

/// The text of every block of `doc`, in order.
fn block_texts(doc: &TextDocument) -> Vec<String> {
    doc.blocks().iter().map(|block| block.text()).collect()
}

/// What a reload shows of each block: its text, its list indent if it is a list item,
/// its heading level, and how many quotations it sits in.
fn block_shapes(doc: &TextDocument) -> Vec<(String, Option<u8>, Option<u8>, usize)> {
    let cursor = doc.cursor();
    doc.blocks()
        .iter()
        .map(|block| {
            cursor.set_position(block.position(), MoveMode::MoveAnchor);
            (
                block.text(),
                block.list().map(|list| list.indent()),
                block.block_format().heading_level,
                cursor.blockquote_depth_at_cursor(),
            )
        })
        .collect()
}

/// Save `doc`, reopen the Djot in a new document, and return the saved Djot, the
/// reopened document and whether saving it again changes nothing further.
fn save_reopen(doc: &TextDocument) -> (String, TextDocument, bool) {
    let saved = doc.to_djot().unwrap();
    let reopened = TextDocument::new();
    set_djot(&reopened, &saved);
    let resaved = reopened.to_djot().unwrap();
    let again = TextDocument::new();
    set_djot(&again, &resaved);
    let stable = again.to_djot().unwrap() == resaved;
    (saved, reopened, stable)
}

/// The lines of `text` that hold something: what a block holding line breaks reads back
/// as, one block a line. An empty line is an empty paragraph, which the reader keeps
/// no block for.
fn non_empty_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// A block's text can hold a line break (inserted with one, or kept by a paste), and each
/// was written as it was: a blank line ended the paragraph, and the lines after it were
/// read back as the lists, headings and quotations their markers spelled, as deep as
/// they went. Every line is now saved as a paragraph of its own, one line long and
/// guarded as any paragraph is, so each comes back as text, nothing nests, and the first
/// save is already the one every later save writes.
#[test]
fn a_line_break_inside_a_block_is_saved_as_a_block_a_line() {
    let deep = format!("x\n\n{}deep", "> ".repeat(97));
    for typed in [
        "Steps:\n\n- one\n- two",
        "On Monday she wrote:\n> > > nested reply\n> > more",
        "a\n# not a heading\n1. not a list\n: not a definition\n| not | a row |",
        "trailing break\n",
        "\nleading break",
        "  indented\n\tline\nends with spaces  ",
        "one\n   \ntwo",
        deep.as_str(),
    ] {
        let doc = TextDocument::new();
        doc.cursor().insert_text(typed).unwrap();
        assert_eq!(doc.blocks().len(), 1, "{typed:?} is one block");
        let (saved, reopened, stable) = save_reopen(&doc);
        assert_eq!(
            nesting_depth(&saved),
            0,
            "{typed:?} saved as {saved:?}, which nests"
        );
        assert_eq!(
            longest_leaf_lines(&saved),
            1,
            "{typed:?} saved as {saved:?}"
        );
        assert_eq!(reopened.to_djot().unwrap(), saved, "{typed:?}");
        assert_eq!(
            block_shapes(&reopened),
            non_empty_lines(typed)
                .into_iter()
                .map(|line| (line, None, None, 0))
                .collect::<Vec<_>>(),
            "{typed:?} saved as {saved:?}"
        );
        assert!(stable, "{typed:?} does not save stably");
    }
}

/// A line break in a list item, a heading, a quotation, a table cell, and inside a
/// formatted or code run: each line keeps the block's own format, the run's format, and
/// the container it is in.
#[test]
fn a_line_break_keeps_the_block_and_run_it_is_in() {
    let doc = TextDocument::new();
    set_djot(
        &doc,
        "- item\n\n# Heading\n\n> quoted\n\nplain\n\n| cell | b |\n|---|---|\n| c | d |",
    );
    // The end of block `index`, read afresh: every insertion moves the blocks after it.
    let end_of = |index: usize| {
        let block = doc.blocks()[index].clone();
        doc.cursor_at(block.position() + block.text().chars().count())
    };
    end_of(4).insert_text("\nsecond line of the cell").unwrap();
    // A bold run and a code run each holding a break, the code run closing its line.
    let cursor = end_of(3);
    cursor
        .insert_formatted_text(" bold\n- line", &bold())
        .unwrap();
    cursor
        .insert_formatted_text("co`de\nmore code", &code())
        .unwrap();
    cursor
        .insert_formatted_text("\ntail", &TextFormat::default())
        .unwrap();
    end_of(2)
        .insert_text("\n> second line of the quotation")
        .unwrap();
    end_of(1)
        .insert_text("\n# second line of the heading")
        .unwrap();
    end_of(0)
        .insert_text("\n- second line of the item")
        .unwrap();

    let (saved, reopened, stable) = save_reopen(&doc);
    assert_eq!(nesting_depth(&saved), 1, "saved as {saved:?}");
    assert_eq!(longest_leaf_lines(&saved), 1, "saved as {saved:?}");
    assert_eq!(reopened.to_djot().unwrap(), saved);
    assert!(stable, "{saved:?} does not save stably");
    let expected: Vec<(String, Option<u8>, Option<u8>, usize)> = vec![
        ("item".into(), Some(0), None, 0),
        ("- second line of the item".into(), Some(0), None, 0),
        ("Heading".into(), None, Some(1), 0),
        ("# second line of the heading".into(), None, Some(1), 0),
        ("quoted".into(), None, None, 1),
        ("> second line of the quotation".into(), None, None, 1),
        ("plain bold".into(), None, None, 0),
        ("- lineco`de".into(), None, None, 0),
        ("more code".into(), None, None, 0),
        ("tail".into(), None, None, 0),
    ];
    let shapes = block_shapes(&reopened);
    assert_eq!(
        &shapes[..expected.len()],
        &expected[..],
        "saved as {saved:?}"
    );
    // The cell's two lines share its one line of Djot.
    assert!(
        reopened
            .to_plain_text()
            .unwrap()
            .contains("cell second line of the cell"),
        "saved as {saved:?}"
    );
    // Each run keeps its format on both of its lines.
    let styles = styled_chars(&reopened);
    let text: String = styles.iter().map(|(c, _)| *c).collect();
    let style_of = |needle: &str| {
        let at = text.find(needle).map(|at| text[..at].chars().count());
        at.and_then(|at| styles[at].1.clone())
    };
    let is = |needle: &str, f: fn(&VisibleStyle) -> bool| style_of(needle).is_some_and(|s| f(&s));
    assert!(is("bold", |s| s.bold), "saved as {saved:?}");
    assert!(is("- line", |s| s.bold), "saved as {saved:?}");
    assert!(is("co`de", |s| s.code), "saved as {saved:?}");
    assert!(is("more code", |s| s.code), "saved as {saved:?}");
    assert!(is("tail", |s| !s.code && !s.bold), "saved as {saved:?}");
}

/// A paste of preformatted text (`<pre>`, or `white-space: pre` or `pre-wrap`) kept its
/// line breaks inside one paragraph: the paste turns a code block into prose, and a
/// preformatted paragraph was never split. The editor showed the lines; the saved Djot
/// read them back as structure, a line of 97 `> ` as a quotation 97 deep. Each line is
/// now its own block, as a plain-text paste gives.
#[test]
fn pasting_preformatted_html_stores_one_block_a_line() {
    let quoted = format!("{}deep", "> ".repeat(97));
    let cases: Vec<(String, Vec<&str>)> = vec![
        (
            "<pre>Steps:\n\n- one\n- two</pre>".to_string(),
            vec!["Before.Steps:", "", "- one", "- two"],
        ),
        (
            "<pre><code>a\n```\nb</code></pre>".to_string(),
            vec!["Before.a", "```", "b"],
        ),
        (
            "<div style=\"white-space: pre-wrap\">a\n\n- b</div>".to_string(),
            vec!["Before.", "a", "", "- b"],
        ),
        (
            "<p style=\"white-space: pre\">one\n  two\n</p>".to_string(),
            vec!["Before.", "one", "  two"],
        ),
        (
            format!("<pre>x\n\n{}deep</pre>", "&gt; ".repeat(97)),
            vec!["Before.x", "", quoted.as_str()],
        ),
    ];
    for (html, lines) in cases {
        let doc = TextDocument::new();
        doc.set_plain_text("Before.").unwrap();
        doc.cursor_at(7).insert_html(&html).unwrap();
        let texts = block_texts(&doc);
        assert!(
            texts.len() >= lines.len() && texts[..lines.len()] == lines[..],
            "pasted {html:?} as {texts:?}"
        );
        assert!(texts.iter().all(|t| !t.contains('\n')), "{texts:?}");
        let (saved, reopened, stable) = save_reopen(&doc);
        assert_eq!(nesting_depth(&saved), 0, "saved as {saved:?}");
        assert!(stable, "{saved:?} does not save stably");
        for (text, list, heading, quotes) in block_shapes(&reopened) {
            assert_eq!(
                (list, heading, quotes),
                (None, None, 0),
                "{text:?} in {saved:?}"
            );
        }
    }
    // The same text loaded as a document is split the same way, a code block aside.
    let doc = TextDocument::new();
    doc.set_html("<div style=\"white-space: pre-wrap\">a\n\n- b</div><pre>c\n- d</pre>")
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(block_texts(&doc), ["a", "", "- b", "c\n- d"]);
}

/// A table row is split into cells at every `|` outside a code span, a link's
/// destination included, so a link whose address held one lost its cell, and the row's
/// last cell dropped out of the table at the next save. It is encoded as `%7C` there,
/// the same address; elsewhere it is kept as it is. An image's name is encoded the same
/// way and decoded again, since the name is the key its bytes are kept under.
#[test]
fn a_pipe_in_a_destination_keeps_its_table_cell() {
    let doc = TextDocument::new();
    let table = doc.cursor().insert_table(2, 2).unwrap();
    let put = |row: usize, col: usize, text: &str, format: Option<TextFormat>| {
        let position = table.cell(row, col).unwrap().blocks()[0].position();
        let cursor = doc.cursor_at(position);
        match format {
            Some(format) => {
                cursor.insert_formatted_text(text, &format).unwrap();
                let selection = doc.cursor_at(position);
                selection.set_position(position + text.chars().count(), MoveMode::KeepAnchor);
                selection.merge_char_format(&format).unwrap();
            }
            None => cursor.insert_text(text).unwrap(),
        }
    };
    let href = "https://fonts.googleapis.com/css?family=Lora|Inter";
    put(0, 0, "Font", None);
    put(0, 1, "Note", None);
    put(1, 1, "kept", None);
    put(1, 0, "stylesheet", Some(link(href)));
    let image_position = table.cell(0, 1).unwrap().blocks()[0].position();
    doc.cursor_at(image_position)
        .insert_image("plates/a|b (1).png", "plate", 60, 90)
        .unwrap();
    // The empty paragraph a table is inserted with is no block to the reader.
    let before = non_empty_lines(&doc.to_plain_text().unwrap());
    let (saved, reopened, stable) = save_reopen(&doc);
    assert_eq!(
        non_empty_lines(&reopened.to_plain_text().unwrap()),
        before,
        "saved as {saved:?}"
    );
    assert!(stable, "{saved:?} does not save stably");
    assert_eq!(
        hrefs(&reopened),
        vec!["https://fonts.googleapis.com/css?family=Lora%7CInter".to_string()],
        "saved as {saved:?}"
    );
    assert_eq!(
        image_names(&reopened),
        ["plates/a|b (1).png"],
        "saved as {saved:?}"
    );

    // Outside a table the `|` is harmless, and kept.
    let doc = document_of_runs(&[("t", link(href))]);
    let (saved, reopened, _) = save_reopen(&doc);
    assert_eq!(
        hrefs(&reopened),
        vec![href.to_string()],
        "saved as {saved:?}"
    );
}

/// The destinations of every link in `doc` on text holding more than whitespace,
/// percent-decoded, in order.
fn visible_hrefs(doc: &TextDocument) -> Vec<String> {
    let mut out = Vec::new();
    for block in doc.blocks() {
        for fragment in block.fragments() {
            if let FragmentContent::Text { text, format, .. } = fragment
                && let Some(href) = format.anchor_href
                && !text.trim().is_empty()
            {
                out.push(percent_decode(&href));
            }
        }
    }
    out
}

/// The names of every image in `doc`, in order.
fn image_names(doc: &TextDocument) -> Vec<String> {
    let mut out = Vec::new();
    for block in doc.blocks() {
        for fragment in block.fragments() {
            if let FragmentContent::Image { name, .. } = fragment {
                out.push(name);
            }
        }
    }
    out
}

/// Image names the writer used to put out raw, so that a `)` ended the reference, a
/// trailing `\` escaped its closing `)`, and the rest came back as prose.
const ODD_IMAGE_NAMES: &[&str] = &[
    "a).png",
    "photo (1).png",
    "https://ex.com/wiki/File:Gull_(bird).jpg",
    "a\\",
    "a\\\\b\\\\",
    "a`b.png",
    "{x}.png",
    "<a>.png",
    "a b.png",
    "100%25.png",
    "a%28b.png",
    "%%29",
    "a%2Fb.png",
    "a%20b.png",
    "x*y_z^w~v.png",
    "[^1].png",
];

/// An image's name is the key its bytes are kept under, so it has to come back as it
/// was written, whatever it holds, in a paragraph, inside a mark and in a table cell.
#[test]
fn an_image_keeps_its_name_whatever_it_holds() {
    for name in ODD_IMAGE_NAMES {
        for wrapper in [TextFormat::default(), bold(), struck()] {
            let doc = TextDocument::new();
            let cursor = doc.cursor();
            cursor.insert_text("See ").unwrap();
            cursor.insert_image(name, "a gull", 600, 900).unwrap();
            cursor.insert_text(" here.").unwrap();
            let selection = doc.cursor_at(4);
            selection.set_position(5, MoveMode::KeepAnchor);
            selection.merge_char_format(&wrapper).unwrap();
            let before = doc.to_plain_text().unwrap();
            let (saved, reopened, stable) = save_reopen(&doc);
            assert_eq!(image_names(&reopened), [*name], "saved as {saved:?}");
            assert_eq!(
                reopened.to_plain_text().unwrap(),
                before,
                "saved as {saved:?}"
            );
            assert!(stable, "{saved:?} does not save stably");
        }
        let doc = TextDocument::new();
        let table = doc.cursor().insert_table(1, 2).unwrap();
        let position = table.cell(0, 0).unwrap().blocks()[0].position();
        doc.cursor_at(position)
            .insert_image(name, "p", 60, 90)
            .unwrap();
        let second = table.cell(0, 1).unwrap().blocks()[0].position();
        doc.cursor_at(second).insert_text("kept").unwrap();
        let (saved, reopened, stable) = save_reopen(&doc);
        assert_eq!(
            image_names(&reopened),
            [*name],
            "in a cell, saved as {saved:?}"
        );
        assert!(
            reopened.to_plain_text().unwrap().contains("kept"),
            "in a cell, saved as {saved:?}"
        );
        assert!(stable, "{saved:?} does not save stably");
    }
    // A name holding none of the escapes reads exactly as it always has, `%20` and all.
    let doc = TextDocument::new();
    set_djot(&doc, "![x](a%20b%2F%29c.png)");
    assert_eq!(image_names(&doc), ["a%20b%2F)c.png"]);
}

/// The parser takes up to the width of a list item's marker off each line it continues
/// the item on. Nested two spaces a level, the item two levels under a marker four or
/// more wide (a task's `- [ ]`, `iii.`, `(ii)`, the hundredth item) landed in the same
/// column as the one between them, came back one level up, and renumbered the list.
#[test]
fn a_list_nested_under_a_wide_marker_keeps_its_levels() {
    let hundred: String = (1..=100).map(|n| format!("{n}. item\n\n")).collect();
    for (seed, indents) in [
        ("- [ ] a\n\n      - b\n\n            - c", vec![0, 1, 2]),
        (
            "i. x\n\nii. y\n\niii. a\n\n     - b\n\n       - c",
            vec![0, 0, 0, 1, 2],
        ),
        (
            "(i) x\n\n(ii) a\n\n     1. b\n\n        1. c\n\n     2. e\n\n     3. d",
            vec![0, 0, 1, 2, 1, 1],
        ),
        (
            "- a\n\n  - [ ] b\n\n        - c\n\n          - d",
            vec![0, 1, 2, 3],
        ),
        (
            "- [x] a\n\n      - [ ] b\n\n            - [ ] c\n\n                  - d",
            vec![0, 1, 2, 3],
        ),
    ] {
        let doc = TextDocument::new();
        set_djot(&doc, seed);
        let before: Vec<Option<u8>> = doc
            .blocks()
            .iter()
            .map(|b| b.list().map(|l| l.indent()))
            .collect();
        assert_eq!(
            before,
            indents.iter().map(|i| Some(*i)).collect::<Vec<_>>(),
            "seed {seed:?}"
        );
        let (saved, reopened, stable) = save_reopen(&doc);
        assert_eq!(
            block_shapes(&reopened),
            block_shapes(&doc),
            "saved as {saved:?}"
        );
        assert!(stable, "{saved:?} does not save stably");
    }
    let seed = format!("{hundred}   - sub\n\n     - subsub");
    let doc = TextDocument::new();
    set_djot(&doc, &seed);
    let (saved, reopened, stable) = save_reopen(&doc);
    assert_eq!(
        block_shapes(&reopened),
        block_shapes(&doc),
        "saved as {saved:?}"
    );
    assert!(stable);
    let last: Vec<Option<u8>> = reopened.blocks()[98..]
        .iter()
        .map(|b| b.list().map(|l| l.indent()))
        .collect();
    assert_eq!(
        last,
        [Some(0), Some(0), Some(1), Some(2)],
        "saved as {saved:?}"
    );
}

/// A paragraph's leading tab and trailing spaces were dropped at every save: the parser
/// strips a paragraph's edge whitespace. An empty attribute set before and after them
/// shows nothing and keeps them, in a paragraph, a list item, a heading and a table cell.
#[test]
fn edge_whitespace_survives_save_and_reload_in_every_block() {
    for seed in [
        "{}\tShe opened the door.",
        "End of line.  {}",
        "{}  {}",
        "- {}\titem  {}",
        "# {}\theading {}",
        "> {}\tquoted  {}",
        "| {}\ta | b  {} |\n|---|---|\n| {} c{} | d |",
        "{}\t- not an item\n\n{}  1. not a list",
    ] {
        let doc = TextDocument::new();
        set_djot(&doc, seed);
        let before = block_shapes(&doc);
        let (saved, reopened, stable) = save_reopen(&doc);
        assert_eq!(
            block_shapes(&reopened),
            before,
            "{seed:?} saved as {saved:?}"
        );
        assert!(stable, "{saved:?} does not save stably");
    }
    let doc = TextDocument::new();
    set_djot(&doc, "{}\tShe opened.\n\nEnd.  {}");
    assert_eq!(block_texts(&doc), ["\tShe opened.", "End.  "]);
}

/// The code blocks of `doc`, their text in order.
fn code_blocks(doc: &TextDocument) -> Vec<String> {
    doc.blocks()
        .iter()
        .filter(|b| b.block_format().is_code_block == Some(true))
        .map(|b| b.text())
        .collect()
}

/// Load each seed, check it holds the one code block `code`, save and reload it, and
/// check the reloaded document holds the same, and saves stably.
fn assert_code_blocks_survive(cases: &[(&str, &str)]) {
    for (seed, code) in cases {
        let doc = TextDocument::new();
        set_djot(&doc, seed);
        assert_eq!(code_blocks(&doc), [*code], "seed {seed:?}");
        let (saved, reopened, stable) = save_reopen(&doc);
        assert_eq!(
            code_blocks(&reopened),
            [*code],
            "{seed:?} saved as {saved:?}"
        );
        assert!(stable, "{saved:?} does not save stably");
    }
}

/// A code block was fenced with three backticks whatever it held, so a line of three
/// backticks in it closed it early, and the rest came back as prose and a new, unclosed
/// block. And a language of more than one word, a Markdown info string such as `rust
/// ignore`, made the whole block and every line of it one inline code span.
#[test]
fn a_code_block_holding_a_fence_keeps_its_lines() {
    assert_code_blocks_survive(&[
        ("````\na\n```\nb\n````", "a\n```\nb"),
        (
            "`````md\nBefore\n````\ncode\n```\nAfter\n`````",
            "Before\n````\ncode\n```\nAfter",
        ),
        ("> ````\n> ```\n> ````", "```"),
    ]);
    let doc = TextDocument::new();
    doc.set_markdown("```rust ignore\nfn main() {}\n```\n")
        .unwrap()
        .wait()
        .unwrap();
    let (saved, reopened, stable) = save_reopen(&doc);
    let blocks = reopened.blocks();
    assert_eq!(blocks.len(), 1, "saved as {saved:?}");
    assert_eq!(
        blocks[0].block_format().is_code_block,
        Some(true),
        "saved as {saved:?}"
    );
    assert_eq!(
        blocks[0].block_format().code_language.as_deref(),
        Some("rust")
    );
    assert_eq!(blocks[0].text(), "fn main() {}");
    assert!(stable);
}

/// In a quotation, an empty line of a code block was written as `> `, and the parser
/// takes only the `>` from a line of whitespace: the line came back as a space, and
/// gained one more at every save.
#[test]
fn a_blank_line_in_a_quoted_code_block_stays_as_it_is() {
    assert_code_blocks_survive(&[
        ("> ```\n> ```", ""),
        ("> ```\n>\n>  \n> x\n>\n> ```", "\n  \nx\n"),
        ("> > ```\n> >\n> >   \n> > ```", "\n   "),
    ]);
}

/// A footnote's continuation lines were indented four spaces, or as far as the note's
/// text for a line indented already, and the parser takes up to the width of
/// `[^label]:` off each: a code block's indented lines gained a column at every save
/// against its fence, or, under four spaces, lost as many as the label was long.
#[test]
fn a_code_block_in_a_footnote_keeps_its_indentation() {
    assert_code_blocks_survive(&[
        (
            "Text[^note-7f3a].\n\n[^note-7f3a]: A note.\n\n              ```\n              fn x() {\n                  y\n              }\n              ```",
            "fn x() {\n    y\n}",
        ),
        (
            "Text[^1].\n\n[^1]: ```\n      fn x() {\n          y\n      }\n      ```",
            "fn x() {\n    y\n}",
        ),
        (
            "Text[^ab].\n\n[^ab]: A note.\n\n       ```\n         a\n   \n       ```",
            "  a\n  ",
        ),
    ]);
}

/// A code span keeps its line breaks through the parser, and the model holds none in a
/// block: a code span written over two lines loads as a block a line, each keeping the
/// code format, and saves as it loads. Kept in one block, it saved as a hard break and
/// reloaded as two blocks, never settling.
#[test]
fn a_code_span_over_two_lines_loads_as_a_block_a_line() {
    let doc = TextDocument::new();
    set_djot(&doc, "a `b\nc` d\n\n- `e\nf`");
    assert_eq!(
        block_shapes(&doc),
        [
            ("a b".to_string(), None, None, 0),
            ("c d".to_string(), None, None, 0),
            ("e".to_string(), Some(0), None, 0),
            ("f".to_string(), Some(0), None, 0),
        ]
    );
    let styles = styled_chars(&doc);
    let code: String = styles
        .iter()
        .filter(|(_, style)| style.as_ref().is_some_and(|s| s.code))
        .map(|(c, _)| *c)
        .collect();
    assert_eq!(code, "bcef");
    let saved = doc.to_djot().unwrap();
    let reopened = TextDocument::new();
    set_djot(&reopened, &saved);
    assert_eq!(reopened.to_djot().unwrap(), saved);
    assert_eq!(
        block_shapes(&reopened),
        block_shapes(&doc),
        "saved as {saved:?}"
    );
}

/// A block holding more line breaks than the parser reads in one paragraph
/// (`MAX_LEAF_LINES`) was saved as one paragraph of hard breaks, which the reader set
/// down as its source lines: every line came back ending in a backslash and showing its
/// escapes, and the next save kept them for good. Each line is saved as a paragraph of
/// its own now, one line long however many the block holds.
#[test]
fn a_block_holding_more_lines_than_a_paragraph_may_hold_reloads_as_its_lines() {
    let lines: Vec<String> = (0..=MAX_LEAF_LINES)
        .map(|i| {
            if i % 1000 == 7 {
                format!("- item {i}")
            } else {
                format!("line {i}")
            }
        })
        .collect();
    let doc = TextDocument::new();
    doc.cursor().insert_text(&lines.join("\n")).unwrap();
    assert_eq!(doc.blocks().len(), 1, "the text is one block");
    let saved = doc.to_djot().unwrap();
    assert_eq!(
        longest_leaf_lines(&saved),
        1,
        "a paragraph of the save runs over several lines"
    );
    let reopened = TextDocument::new();
    set_djot(&reopened, &saved);
    let texts = block_texts(&reopened);
    let changed = texts
        .iter()
        .zip(&lines)
        .filter(|(reloaded, line)| reloaded != line)
        .count();
    assert_eq!(texts.len(), lines.len(), "one block a line");
    assert_eq!(
        changed, 0,
        "lines changed, the first reading {:?}",
        texts[0]
    );
    assert_eq!(
        reopened.to_djot().unwrap(),
        saved,
        "the save does not settle"
    );
}

/// A page break belongs where its block starts. A block holding line breaks comes back
/// one block a line, and each line took the page break with it: through a save and a
/// reload (the reader gave every line after a hard break the paragraph's attributes),
/// through a paste of preformatted text and through `set_html`, so an export put every
/// line on a page of its own. Only the first line starts a new page now.
#[test]
fn a_page_break_before_a_block_holding_line_breaks_stays_on_its_first_line() {
    let breaks = |doc: &TextDocument| -> Vec<(String, Option<bool>)> {
        doc.blocks()
            .iter()
            .map(|block| (block.text(), block.block_format().page_break_before))
            .collect()
    };
    let expected = |lines: &[&str]| -> Vec<(String, Option<bool>)> {
        lines
            .iter()
            .enumerate()
            .map(|(i, line)| (line.to_string(), (i == 0).then_some(true)))
            .collect()
    };
    let chapter = ["Chapter start.", "second line", "third line"];

    // Inserted with its line breaks, then saved and reopened.
    for container in ["", "# "] {
        let doc = TextDocument::new();
        set_djot(
            &doc,
            &format!("{{page_break_before=true}}\n{container}Chapter start."),
        );
        let block = doc.blocks()[0].clone();
        doc.cursor_at(block.position() + block.text().chars().count())
            .insert_text("\nsecond line\nthird line")
            .unwrap();
        let (saved, reopened, stable) = save_reopen(&doc);
        assert_eq!(breaks(&reopened), expected(&chapter), "saved as {saved:?}");
        assert_eq!(
            saved.matches("page_break_before").count(),
            1,
            "saved as {saved:?}"
        );
        assert!(stable, "{saved:?} does not save stably");
    }

    // Written with hard breaks.
    for container in ["", "# "] {
        let doc = TextDocument::new();
        set_djot(
            &doc,
            &format!(
                "{{page_break_before=true}}\n{container}Chapter start.\\\nsecond line\\\nthird line"
            ),
        );
        assert_eq!(breaks(&doc), expected(&chapter), "{container:?}");
    }

    // Pasted, and read by the HTML importer, as preformatted HTML. A paste's first and
    // last blocks join the paragraph it lands in and take its format, so the passage is
    // pasted between two paragraphs of its own. `set_html` keeps no page break, so the
    // importer is asked what it read.
    let passage =
        "<p style=\"white-space: pre-wrap; page-break-before: always\">one\ntwo\nthree</p>";
    let pasted = TextDocument::new();
    pasted
        .cursor()
        .insert_html(&format!("<p>before</p>{passage}<p>after</p>"))
        .unwrap();
    let mut lines = expected(&["one", "two", "three"]);
    lines.insert(0, ("before".to_string(), None));
    lines.push(("after".to_string(), None));
    assert_eq!(breaks(&pasted), lines, "pasted");
    let read: Vec<(String, Option<bool>)> = parse_html(passage)
        .into_iter()
        .map(|block| {
            let text = block.spans.iter().map(|span| span.text.as_str()).collect();
            (text, block.page_break_before)
        })
        .collect();
    assert_eq!(read, expected(&["one", "two", "three"]), "read");
}

/// Older versions wrote an image's source as it was, so a source saved then can hold
/// the escapes the writer now uses: a web address naming `100%.png` holds `%25`, and
/// one with an encoded backslash `%5C`. Decoded, the first read as an address no server
/// knows and the second as another path. The reader decodes `%25` only before another
/// of its escapes and `%5C` only at the end, the two places the writer needs them, so
/// those sources read as they were written, and save as they were read.
#[test]
fn an_image_source_an_older_version_saved_reads_as_it_was_written() {
    for src in [
        "https://example.com/100%25.png",
        "https://example.com/a%5Cb.png",
        "a%25%25b.png",
        "%25",
        "a%5C%5Cb.png",
        "50%25%20off.png",
    ] {
        let doc = TextDocument::new();
        set_djot(&doc, &format!("See ![a picture]({src}) here."));
        assert_eq!(image_names(&doc), [src], "{src:?} read as another name");
        let (saved, reopened, stable) = save_reopen(&doc);
        assert_eq!(image_names(&reopened), [src], "saved as {saved:?}");
        assert!(stable, "{saved:?} does not save stably");
    }
}
