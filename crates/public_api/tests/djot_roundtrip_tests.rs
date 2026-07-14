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
use text_document::{DjotImportOptions, FindOptions, TextDocument, djot_to_plain_text};

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
/// with an alphanumeric so emphasis/verbatim delimiters bind, no djot
/// metacharacters or newlines inside.
fn word() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9][a-zA-Z0-9 ]{0,10}[a-zA-Z0-9]".prop_map(|s| s.trim().to_string())
}

/// A safe URL (no characters that need link-destination escaping).
fn url() -> impl Strategy<Value = String> {
    "https://[a-z]{2,8}\\.example/[a-z0-9_-]{0,10}"
}

/// Plain text that may contain djot metacharacters in the interior to stress
/// the exporter's escaping. Starts with a letter so it isn't mistaken for a
/// block marker, contains no newlines. Backticks are excluded: a raw backtick
/// in source is a verbatim delimiter, so it represents *code*, not plain text —
/// the code path is exercised through [`Inline::Code`] instead.
fn plain_text() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9 .,!?*_~^]{0,24}".prop_map(|s| s.trim_end().to_string())
}

fn inline() -> impl Strategy<Value = Inline> {
    prop_oneof![
        plain_text().prop_map(Inline::Text),
        word().prop_map(Inline::Bold),
        word().prop_map(Inline::Italic),
        "[a-zA-Z0-9 ]{1,12}".prop_map(Inline::Code),
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
            "[a-zA-Z0-9 ;=()]{0,30}",
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
    "[a-zA-Z0-9][a-zA-Z0-9 ]{0,6}[a-zA-Z0-9]".prop_map(|s| s.trim().to_string())
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
