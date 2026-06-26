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
use text_document::TextDocument;

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

#[derive(Debug, Clone)]
enum Block {
    Para(Vec<Inline>),
    Heading(u8, String),
    CodeBlock(Option<String>, String),
    Bullet(Vec<String>),
    Ordered(Vec<String>),
    Task(Vec<(bool, String)>),
    Quote(String),
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
        Block::Para(inlines) => inlines.iter().map(emit_inline).collect::<String>(),
        Block::Heading(level, s) => format!("{} {s}", "#".repeat(*level as usize)),
        Block::CodeBlock(lang, content) => {
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
    }
}

fn emit(blocks: &[Block]) -> String {
    blocks.iter().map(emit_block).collect::<Vec<_>>().join("\n\n")
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

fn block() -> impl Strategy<Value = Block> {
    prop_oneof![
        prop::collection::vec(inline(), 1..5).prop_map(Block::Para),
        (1u8..=6, word()).prop_map(|(l, s)| Block::Heading(l, s)),
        (
            prop::option::of(prop_oneof![Just("rust".to_string()), Just("py".to_string())]),
            "[a-zA-Z0-9 ;=()]{0,30}",
        )
            .prop_map(|(lang, c)| Block::CodeBlock(lang, c)),
        prop::collection::vec(word(), 1..4).prop_map(Block::Bullet),
        prop::collection::vec(word(), 1..4).prop_map(Block::Ordered),
        prop::collection::vec((any::<bool>(), word()), 1..4).prop_map(Block::Task),
        word().prop_map(Block::Quote),
    ]
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
    }
}
