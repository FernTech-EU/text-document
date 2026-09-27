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
    }
}

fn emit(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(emit_block)
        .collect::<Vec<_>>()
        .join("\n\n")
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
    ]
}

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

/// One typed paragraph. Its edges are trimmed of spaces and tabs because Djot drops
/// them when it reads a paragraph, which no escaping can change.
fn typed_paragraph() -> impl Strategy<Value = String> {
    prop::collection::vec(typed_piece(), 1..16)
        .prop_map(|pieces| pieces.concat().trim_matches([' ', '\t']).to_string())
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
/// whitespace itself is dropped with the paragraph's other edges, which no
/// escaping can prevent; the marker and the rest of the text must stay.
#[test]
fn a_marker_behind_leading_whitespace_stays_text() {
    let mut failures = Vec::new();
    for (typed, expected) in [
        ("\t- (void)someMethod", "- (void)someMethod"),
        (" \t1.\tIn the main menu", "1.\tIn the main menu"),
        ("\t## Section 2", "## Section 2"),
        ("   I. Indented", "I. Indented"),
        ("\t:::", ":::"),
        ("  > quoted", "> quoted"),
    ] {
        let (saved, back, resaved) = save_and_reload_plain(typed);
        if back != expected || resaved.trim_start() != resaved {
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
            Piece::Image => vec![('\u{FFFC}', None)],
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

/// A backslash in a destination is an escape to the lexer only before ASCII
/// punctuation or whitespace, or at the end, where it would escape the closing `)`.
/// Anywhere else it reads back as written and must be kept as written: `%5C` is a
/// different link, since a URL parser reads a `\` in a web or file address as `/`.
#[test]
fn a_backslash_in_a_link_destination_is_encoded_only_where_it_would_escape() {
    for href in [
        "file:///C:\\Users\\me\\notes.txt",
        "http://example.com/a\\b",
    ] {
        let doc = document_of_runs(&[("t", link(href))]);
        let saved = doc.to_djot().unwrap();
        assert!(saved.contains(href), "{href:?} saved as {saved:?}");
        let reopened = TextDocument::new();
        set_djot(&reopened, &saved);
        assert_eq!(
            hrefs(&reopened),
            vec![href.to_string()],
            "saved as {saved:?}"
        );
        assert_survives_save_and_reload(&doc, href);
    }
    for (href, written) in [
        ("http://example.com/a\\", "a%5C)"),
        ("http://example.com/a\\(b", "a%5C%28b"),
        ("http://example.com/a\\ b", "a%5C b"),
        ("http://example.com/a\\.b", "a%5C.b"),
        // The second backslash is before a letter again.
        ("http://example.com/a\\\\b", "a%5C\\b"),
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
    ]
}

/// Remove the spaces and tabs at the paragraph's two edges, which Djot drops when it
/// reads a paragraph, taking runs left empty with them. An image at an edge stops it.
fn trim_paragraph_edges(mut pieces: Vec<Piece>) -> Vec<Piece> {
    let is_edge = |c: char| c == ' ' || c == '\t';
    while let Some(Piece::Run(text, _)) = pieces.first_mut() {
        *text = text.trim_start_matches(is_edge).to_string();
        if !text.is_empty() {
            break;
        }
        pieces.remove(0);
    }
    while let Some(Piece::Run(text, _)) = pieces.last_mut() {
        *text = text.trim_end_matches(is_edge).to_string();
        if !text.is_empty() {
            break;
        }
        pieces.pop();
    }
    pieces
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Text and per-character style of a paragraph built from formatted runs and
    /// images, the way the editor holds it, survive a save and reload. This is where
    /// escaping meets the exporter's own markup and the boundaries between runs.
    #[test]
    fn styled_runs_survive_save_and_reload(pieces in prop::collection::vec(styled_piece(), 1..6)) {
        let pieces = trim_paragraph_edges(pieces);
        prop_assume!(!pieces.is_empty());
        let doc = document_of_pieces(&pieces);
        let before = styled_chars(&doc);
        let saved = doc.to_djot().unwrap();
        let reopened = TextDocument::new();
        set_djot(&reopened, &saved);
        let after = styled_chars(&reopened);
        let plain = |v: &[(char, Option<VisibleStyle>)]| v.iter().map(|(c, _)| *c).collect::<String>();
        prop_assert_eq!(plain(&after), plain(&before), "text changed; saved as {:?}", saved);
        prop_assert_eq!(after, before, "a style changed; saved as {:?}", saved);
    }
}
