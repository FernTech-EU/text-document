// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Turn arbitrary plain text into Djot that renders it back verbatim.
//!
//! The inverse of [`djot_to_plain_text`](super::djot_to_plain_text), and the counterpart
//! it had been missing. The Djot exporter has always needed this — it cannot write a
//! paragraph containing `*` without the re-parse reading emphasis that the writer never
//! typed — but the two halves lived privately inside `export_djot_uc`, so anything *else*
//! holding plain text destined to become Djot had to reinvent them.
//!
//! That reinvention is the failure this module exists to prevent. A host app promoting a
//! stored plain-text field to a Djot one (a comment body, say) has to escape the values
//! already on disk, and a second, slightly-different escaper would disagree with the
//! exporter about exactly the awkward strings — a paragraph opening `- ` , a title with
//! `[brackets]`, prose about `snake_case` — while agreeing on everything easy enough to
//! notice in review.
//!
//! Two levels, because Djot has two:
//!
//! * [`escape_djot_inline`] neutralises the characters that can start *inline* markup
//!   anywhere in a line.
//! * [`guard_djot_block_start`] neutralises the markers that mean something only at the
//!   **start of a line** — a leading `#` is a heading, a leading `- ` a list item, and no
//!   amount of inline escaping reaches them.
//!
//! [`plain_text_to_djot`] composes both over every line, which is what a caller
//! converting a whole stored string wants.

/// Backslash-escape every character that can trigger Djot *inline* markup, so arbitrary
/// text survives a re-parse verbatim.
///
/// jotdown turns `\x` into an `Escape` event followed by the literal character, so
/// **over-escaping is always round-trip-safe** — which is why this takes the whole
/// punctuation set rather than trying to be clever about which occurrences are actually
/// syntactic. Being clever there means tracking Djot's inline state machine, and being
/// wrong about it silently rewrites the writer's text.
///
/// Block-start markers (`#`, `>`, `-`, …) are *not* covered here — they are only
/// meaningful at the start of a line, and escaping them mid-sentence would litter
/// ordinary prose with backslashes. Use [`guard_djot_block_start`] for those.
pub fn escape_djot_inline(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '*' | '_' | '`' | '~' | '^' | '[' | ']' | '(' | ')' | '{' | '}' | '|' | '<' => {
                result.push('\\');
                result.push(c);
            }
            _ => result.push(c),
        }
    }
    result
}

/// Neutralise a line's leading characters so they are not parsed as a block-construct
/// marker.
///
/// Covers the block-only markers (`#`, `>`, `-`, `+`, `:`) and the ordered-list forms
/// `<digits>.` and `<digits>)`. Inline specials are [`escape_djot_inline`]'s job.
///
/// For the ordered-list case the **delimiter** is escaped rather than the digit: a
/// backslash before a digit is a literal backslash in Djot, so escaping `1` in `1.` would
/// add a visible `\` and still leave the list marker intact — wrong twice over.
pub fn guard_djot_block_start(s: &str) -> String {
    let Some(first) = s.chars().next() else {
        return s.to_string();
    };
    if matches!(first, '#' | '>' | '-' | '+' | ':') {
        return format!("\\{s}");
    }
    if first.is_ascii_digit() {
        let rest = s.trim_start_matches(|c: char| c.is_ascii_digit());
        if rest.starts_with('.') || rest.starts_with(')') {
            let digits_len = s.len() - rest.len();
            return format!("{}\\{}", &s[..digits_len], &s[digits_len..]);
        }
    }
    s.to_string()
}

/// Convert a whole plain-text string into Djot that parses back to exactly that text.
///
/// Escapes inline markup everywhere and guards each line's own start, since a line
/// beginning `- ` is a list item wherever it sits in the string, not only in the first.
///
/// # Each line becomes its own paragraph, and that is forced, not chosen
///
/// A single newline *inside* a Djot paragraph is a soft break, and
/// [`djot_to_plain_text`](super::djot_to_plain_text) collapses it to a space — so
/// emitting the lines as one paragraph loses every line ending. Blocks, meanwhile, are
/// joined by exactly one `\n` when read back. One paragraph per line is therefore the
/// only shape whose round trip is the identity, and it is also what the text it will
/// meet already means: a `.docx`/`.odt` comment body is assembled by joining its
/// paragraphs with `\n`, so each newline in such a string *is* a paragraph boundary.
///
/// # The contract, stated exactly
///
/// `djot_to_plain_text(plain_text_to_djot(s)) == s` for every `s` that contains **no
/// blank line** — i.e. no two consecutive newlines, and no leading or trailing one.
///
/// That restriction is not a gap left open; it is the shape of the target. `djot_to_plain_text`
/// never emits two consecutive newlines, because blocks are joined by exactly one — so no
/// string containing a blank line is in the image of the parse, and none can be recovered by
/// any encoding. Blank lines in the input collapse, which for the paragraph-joined text this
/// serves is a no-op. Use [`needs_djot_escaping`] to find values a conversion would alter.
pub fn plain_text_to_djot(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for line in s.split('\n') {
        if line.is_empty() {
            continue;
        }
        if !first {
            out.push_str("\n\n");
        }
        first = false;
        out.push_str(&guard_djot_block_start(&escape_djot_inline(line)));
    }
    out
}

/// Whether [`plain_text_to_djot`] would rewrite `s` at all.
///
/// For a caller migrating a stored field from plain text to Djot: a value this returns
/// `false` for is *already* legal Djot meaning exactly itself, so it can be left
/// byte-identical on disk and stays readable by an older build. Only the values this
/// returns `true` for force a rewrite — which is the distinction a format-version floor
/// should be gated on, rather than stamping every project that merely *has* comments.
///
/// Note this asks whether the **stored bytes** change, not whether meaning survives. For
/// that, see [`djot_round_trip_is_lossy`] — the two are independent, and a migration
/// generally wants both.
pub fn needs_djot_escaping(s: &str) -> bool {
    plain_text_to_djot(s) != s
}

/// Whether converting `s` to Djot and reading it back would **lose text**.
///
/// Distinct from [`needs_djot_escaping`], and not derivable from it: the escape is a pure
/// string transform, while this runs the real parse. Two shapes are outside the image of
/// any Djot parse, so no encoding can recover them and this reports both:
///
/// * a **blank line** — blocks are joined by exactly one `\n` on the way back, so two
///   consecutive newlines never come out;
/// * **trailing whitespace** on a line — Djot strips it.
///
/// A migration should report the values this flags rather than rewrite them silently: the
/// text is the writer's, and quietly dropping a blank line out of someone's remark is the
/// same class of failure as quietly moving their comment.
pub fn djot_round_trip_is_lossy(s: &str) -> bool {
    use crate::parser_tools::djot_options::DjotImportOptions;
    crate::parser_tools::content_parser::djot_to_plain_text(
        &plain_text_to_djot(s),
        &DjotImportOptions::default(),
    ) != s
}

#[cfg(test)]
mod tests {
    use super::super::content_parser::djot_to_plain_text;
    use super::*;
    use crate::parser_tools::djot_options::DjotImportOptions;

    /// The contract, over the strings that actually break naive escaping.
    #[test]
    fn escaped_plain_text_parses_back_to_itself() {
        for original in [
            "plain prose, nothing special",
            "a *starred* word",
            "snake_case and more_snake_case",
            "code `backticks` here",
            "brackets [like this] and (parens)",
            "a title: The Lighthouse [Revised]",
            "# not a heading",
            "- not a list item",
            "1. not an ordered list",
            "12) also not an ordered list",
            "> not a quote",
            "+ not a list",
            ": not a definition",
            "a backslash \\ alone",
            "tilde ~sub~ and caret ^sup^",
            "braces {attr} and a pipe | here",
            "an angle <bracket>",
            "line one\nline two",
            "- leading marker\nand a second line",
            "1. first\n2. second\n3. third",
            "unicode — em dash, ellipsis …, quotes “ ”",
        ] {
            let djot = plain_text_to_djot(original);
            let round_tripped = djot_to_plain_text(&djot, &DjotImportOptions::default());
            assert_eq!(
                round_tripped, *original,
                "escaping {original:?} produced {djot:?}, which parsed back as \
                 {round_tripped:?} — the escape is not round-trip safe"
            );
        }
    }

    /// Text with nothing syntactic must be left byte-identical, or migrating a stored
    /// field would rewrite every ordinary value for no reason.
    #[test]
    fn ordinary_prose_is_left_untouched() {
        for plain in [
            "Just an ordinary remark.",
            "Two sentences. Both ordinary!",
            "A question? Yes.",
            "",
        ] {
            assert_eq!(plain_text_to_djot(plain), plain);
            assert!(!needs_djot_escaping(plain), "{plain:?} needs no escaping");
        }
    }

    #[test]
    fn text_with_markup_characters_is_reported_as_needing_escaping() {
        for plain in ["a *star*", "# heading-ish", "1. listish", "under_score"] {
            assert!(needs_djot_escaping(plain), "{plain:?} must need escaping");
        }
    }

    /// The documented restriction, asserted rather than left implicit: a blank line
    /// cannot survive, because `djot_to_plain_text` joins blocks with exactly one `\n`
    /// and so never emits two in a row. A caller that needs to know beforehand has
    /// [`needs_djot_escaping`].
    #[test]
    fn a_blank_line_collapses_because_no_djot_can_produce_one() {
        let round_tripped =
            djot_to_plain_text(&plain_text_to_djot("a\n\nb"), &DjotImportOptions::default());
        assert_eq!(round_tripped, "a\nb");
    }

    /// The two predicates answer different questions and neither implies the other —
    /// which is exactly why both exist. `"a\n\nb"` escapes to itself byte-for-byte (the
    /// blank line is dropped and the paragraph join puts it back), so a pure string
    /// comparison sees no change while the round trip genuinely loses a line.
    #[test]
    fn lossiness_is_not_detectable_by_string_comparison_alone() {
        assert!(
            !needs_djot_escaping("a\n\nb"),
            "the escape happens to reproduce the input byte-for-byte here"
        );
        assert!(
            djot_round_trip_is_lossy("a\n\nb"),
            "…but the round trip still loses the blank line, and a migration must be able \
             to see that"
        );
    }

    /// Djot strips trailing whitespace, so it is outside the image of any parse too.
    #[test]
    fn trailing_whitespace_is_reported_as_lossy() {
        assert!(djot_round_trip_is_lossy("trailing spaces are content   "));
        assert!(!djot_round_trip_is_lossy("no trailing space"));
    }

    /// Multi-line text is the shape a `.docx`/`.odt` comment body actually arrives in —
    /// its paragraphs joined with `\n` by the scanners. It must survive exactly.
    #[test]
    fn a_multi_paragraph_comment_body_round_trips() {
        let body = "First paragraph of the note.\nA second one, with *emphasis* typed literally.";
        let djot = plain_text_to_djot(body);
        assert_eq!(
            djot_to_plain_text(&djot, &DjotImportOptions::default()),
            body
        );
    }

    /// The ordered-list guard must escape the delimiter, never the digit — a backslash
    /// before a digit is a literal backslash in Djot.
    #[test]
    fn an_ordered_list_guard_escapes_the_delimiter_not_the_digit() {
        assert_eq!(guard_djot_block_start("1. text"), "1\\. text");
        assert_eq!(guard_djot_block_start("42) text"), "42\\) text");
    }
}
