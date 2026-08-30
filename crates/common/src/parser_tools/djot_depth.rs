//! A ceiling on how deeply nested a Djot document may be before it is parsed.
//!
//! # The failure this prevents
//!
//! `jotdown` descends once per nested block container and has no depth limit of
//! its own. A few kilobytes of prose — on the order of two thousand nested
//! blockquote markers — exhausts the stack.
//!
//! That is not a panic. **A stack overflow aborts the process**: it cannot be
//! caught by `catch_unwind`, a panic hook does not run, and every unsaved
//! document in every window of the embedding application dies with it. So it
//! cannot be handled by the caller after the fact; it has to be refused before
//! `jotdown` is handed the text at all.
//!
//! The input is not always the author's own. A `.skrib` bundle is mailed,
//! shared on a drive and restored from someone else's backup; an imported
//! `.docx` comes from an editor. Any of those can carry prose this crate then
//! parses.
//!
//! # Why a scan rather than a limit inside the parser
//!
//! A depth limit belongs in the recursive descent itself, and this is not that.
//! `jotdown` is an external crate and its recursion is not reachable from here,
//! so what this module does instead is bound the *input*: nesting cannot exceed
//! the number of nesting markers the text actually contains, so counting them is
//! a conservative upper bound on how deep the parser can go.
//!
//! It deliberately over-estimates. Every construct counted here *may* open a
//! container and some will not — a `>` inside a code fence is prose. Over-counting
//! is the safe direction: it can only flag a document that was closer to the
//! ceiling than it looked, and the ceiling sits two orders of magnitude above
//! anything a person writes.
//!
//! # What callers do with it
//!
//! [`parse_djot`](super::content_parser::parse_djot) keeps its signature and
//! **degrades rather than refusing**: over-deep input comes back as a single
//! plain paragraph holding the source verbatim. Nothing is lost — the text is
//! all still there — it is simply not given a structure, which is the honest
//! answer for a document whose structure cannot be computed without ending the
//! process.
//!
//! # The limit
//!
//! [`MAX_NESTING_DEPTH`] is 96. For scale, a blockquote inside a list inside a
//! footnote inside a div is 4; CommonMark's own reference implementations cap
//! list nesting far below this. No real document reaches 96, and 96 is far below
//! the ~2000 that overflows a debug build.

/// The most nested block containers a document may declare before
/// [`is_too_deep`] reports it.
pub const MAX_NESTING_DEPTH: usize = 96;

/// A conservative upper bound on the block nesting `text` can produce.
///
/// Counts, per line: the run of blockquote markers opening it, the number of
/// `:::` div fences currently open, and one level per two columns of leading
/// indentation (the coarsest list-nesting unit Djot admits).
pub fn nesting_depth(text: &str) -> usize {
    let mut open_divs = 0usize;
    let mut deepest = 0usize;

    for line in text.lines() {
        let trimmed = line.trim_start();

        if let Some(rest) = trimmed.strip_prefix(":::") {
            // A **closing** fence is colons and nothing else. Djot lets an outer
            // fence be longer than three so a div can nest inside another, so
            // stripping `:::` leaves `":"` on a `::::` line — reading any
            // non-empty remainder as a class would count a closing `::::` as a
            // second opener, and the count would never come back down.
            let rest = rest.trim();
            if rest.is_empty() || rest.chars().all(|c| c == ':') {
                open_divs = open_divs.saturating_sub(1);
            } else {
                open_divs += 1;
                deepest = deepest.max(open_divs);
            }
            continue;
        }

        let indent = line.len() - trimmed.len();
        let mut quotes = 0usize;
        for ch in trimmed.chars() {
            match ch {
                '>' => quotes += 1,
                ' ' | '\t' => {}
                _ => break,
            }
        }

        deepest = deepest.max(quotes + open_divs + indent / 2);
    }

    deepest
}

/// Whether `text` nests deeply enough to risk exhausting the stack.
pub fn is_too_deep(text: &str) -> bool {
    nesting_depth(text) > MAX_NESTING_DEPTH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_prose_is_shallow() {
        for text in [
            "The ferry was late.\n\nShe waited.\n",
            "> He said it plainly.\n>\n> Then he left.\n",
            "- one\n  - two\n    - three\n",
            "::: note\nA note.\n:::\n",
            "> - a quoted list\n>   - nested once\n",
            "",
        ] {
            assert!(!is_too_deep(text), "should accept: {text:?}");
        }
    }

    /// The shape measured aborting the process: a blockquote marker run is the
    /// cheapest way to reach the recursion.
    #[test]
    fn a_deep_blockquote_run_is_flagged() {
        assert!(is_too_deep(&format!("{}deep\n", ">".repeat(2_000))));
    }

    #[test]
    fn deeply_stacked_divs_are_flagged() {
        assert!(is_too_deep(&"::: a\n".repeat(500)));
    }

    #[test]
    fn runaway_indentation_is_flagged() {
        assert!(is_too_deep(&format!("{}item\n", " ".repeat(1_000))));
    }

    /// Sibling divs close each other, so a long document made of many of them
    /// has the nesting of one — not of all of them.
    #[test]
    fn sibling_divs_do_not_accumulate() {
        assert!(!is_too_deep(&"::: note\nbody\n:::\n".repeat(500)));
    }

    #[test]
    fn a_longer_closing_fence_closes_rather_than_opens() {
        let text = ":::: outer\n::: inner\nbody\n:::\n::::\n".repeat(200);
        assert!(!is_too_deep(&text), "nested fences must not accumulate");
    }

    /// The one that matters: run the **real** parser on input that used to
    /// abort the process, and check it comes back.
    ///
    /// A stack overflow is not a panic, so this test cannot be written with
    /// `#[should_panic]` or `catch_unwind` — if the guard regresses, the test
    /// binary dies and takes the whole run with it. That is the intended
    /// signal, and it is why the assertion is about the *content* coming back
    /// whole rather than merely about not crashing.
    #[test]
    fn the_real_parser_survives_input_that_used_to_abort_the_process() {
        use crate::parser_tools::content_parser::{ParsedElement, parse_djot};
        use crate::parser_tools::djot_options::DjotImportOptions;

        let hostile = format!("{}deep\n", ">".repeat(4_000));
        let elements = parse_djot(&hostile, &DjotImportOptions::default());

        assert_eq!(elements.len(), 1, "degrades to a single block");
        let ParsedElement::Block(block) = &elements[0] else {
            panic!("expected a plain paragraph");
        };
        let text: String = block.spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            text, hostile,
            "the source must come back verbatim — degrading may not lose prose"
        );
    }

    /// And a document just under the ceiling still parses normally, so the
    /// guard is not quietly flattening real prose.
    #[test]
    fn prose_below_the_ceiling_still_gets_its_structure() {
        use crate::parser_tools::content_parser::parse_djot;
        use crate::parser_tools::djot_options::DjotImportOptions;

        let ok = "> > > a quoted quote\n\nand a paragraph\n";
        let elements = parse_djot(ok, &DjotImportOptions::default());
        assert!(
            elements.len() > 1,
            "a two-block document must still parse as two blocks: {elements:#?}"
        );
    }
}
