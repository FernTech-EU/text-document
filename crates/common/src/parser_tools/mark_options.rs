// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Named marks a writer anchors into its output as **bookmarks** — the third export payload,
//! beside [`comments`](super::comment_options) and [`images`](super::image_options).
//!
//! # What this is for
//!
//! A host that exports a document for someone else to edit, and later reads the edited file
//! back, needs to know which part of the returned file corresponds to which part of its own
//! model. Nothing in DOCX or ODF carries that natively, and the obvious answer — a private
//! attribute on the host's own elements — does not survive: **both Word and LibreOffice discard
//! unknown-namespace attributes when they save.** Measured, not assumed: a file this crate wrote
//! with a `skrb:uid` on every `<office:annotation>` came back from LibreOffice 25.8 with the
//! attribute gone and its namespace declaration gone with it.
//!
//! A bookmark does survive, because it is not an extension. `text:bookmark` and
//! `w:bookmarkStart` are first-class in ODF and OOXML respectively, position-tracked as the
//! editor moves text around, invisible in both readers, and preserved by every writer that
//! claims to support either format. So identity travels as a bookmark, and this is the payload
//! that carries it.
//!
//! # Point marks and range marks
//!
//! A mark with `start == end` is a **point**: it names a position and nothing else, and is
//! written as a single self-closing element. A mark with `start < end` is a **range**, written
//! as a start/end pair bracketing exactly those characters. Both are useful and the difference
//! is not cosmetic — a point mark survives the text around it being rewritten wholesale, while
//! a range mark tells the reader precisely which characters it covered.
//!
//! # Names are the payload
//!
//! The name is the only thing that comes back, so it is where the host's identity has to live.
//! [`DocumentMark::validate`] enforces the intersection of what the two formats accept, which is
//! really just what *Word* accepts — 40 characters, ASCII alphanumerics and underscore, leading
//! letter. ODF is far more permissive, but a name legal in only one of the two would produce a
//! file that round-trips through one editor and loses its identity in the other, which is worse
//! than refusing it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Word's cap on the length of a bookmark name. Names longer than this are truncated or dropped
/// by Word without a diagnostic, which is exactly the failure this payload exists to avoid.
pub const MAX_BOOKMARK_NAME_LEN: usize = 40;

/// One named position or range in the document's addressable character space.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DocumentMark {
    /// `[start, end)` in the document's **addressable** character space — the same space
    /// [`DocumentComment::start`](super::comment_options::DocumentComment::start) uses, and the
    /// same one `TextDocument::to_addressable_text()` reports. `start == end` is a point mark.
    pub start: u32,
    pub end: u32,
    /// The bookmark name, which is the whole message. See [`validate`](Self::validate).
    pub name: String,
}

impl DocumentMark {
    /// A point mark at `at`.
    pub fn point(at: u32, name: impl Into<String>) -> Self {
        Self {
            start: at,
            end: at,
            name: name.into(),
        }
    }

    /// A range mark over `[start, end)`.
    pub fn range(start: u32, end: u32, name: impl Into<String>) -> Self {
        Self {
            start,
            end,
            name: name.into(),
        }
    }

    /// True when this mark names a position rather than a span.
    pub fn is_point(&self) -> bool {
        self.start == self.end
    }

    /// Check the name against the stricter of the two formats' rules, and the range against
    /// itself.
    ///
    /// Returns a description of the problem rather than a bool, because every caller of this
    /// wants to say what was wrong: these names are minted by the host from its own data, so a
    /// rejected one is a programming error and deserves to be reported as one — not silently
    /// dropped, which would leave an export that looks complete and cannot be read back.
    pub fn validate(&self) -> Result<(), String> {
        if self.end < self.start {
            return Err(format!(
                "mark '{}' ends ({}) before it starts ({})",
                self.name, self.end, self.start
            ));
        }
        if self.name.is_empty() {
            return Err("a mark with an empty name carries no identity".to_string());
        }
        if self.name.len() > MAX_BOOKMARK_NAME_LEN {
            return Err(format!(
                "mark name '{}' is {} characters; Word drops anything over {MAX_BOOKMARK_NAME_LEN}",
                self.name,
                self.name.len()
            ));
        }
        if !self
            .name
            .starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        {
            return Err(format!(
                "mark name '{}' must begin with a letter or underscore",
                self.name
            ));
        }
        if let Some(bad) = self
            .name
            .chars()
            .find(|c| !c.is_ascii_alphanumeric() && *c != '_')
        {
            return Err(format!(
                "mark name '{}' contains {bad:?}; only ASCII letters, digits and underscore \
                 survive both formats",
                self.name
            ));
        }
        Ok(())
    }
}

/// Every mark supplied to one export, keyed by name.
///
/// A `BTreeMap` for the same reason [`DocumentComments`](super::comment_options::DocumentComments)
/// is one: two exports of the same document must be byte-comparable, and a randomised iteration
/// order would quietly break that. Keying by name also makes the uniqueness the formats require
/// structural — a bookmark name may appear only once in a document, and a `Vec` would let a
/// caller supply the same name twice and produce a file neither editor can open cleanly.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DocumentMarks(BTreeMap<String, DocumentMark>);

impl DocumentMarks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a mark, keyed by its own name. A second insert under the same name replaces the
    /// first.
    pub fn insert(&mut self, mark: DocumentMark) -> &mut Self {
        self.0.insert(mark.name.clone(), mark);
        self
    }

    pub fn get(&self, name: &str) -> Option<&DocumentMark> {
        self.0.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &DocumentMark> {
        self.0.values()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Every mark, sorted by `(start, end, name)` — the order a writer walking the document
    /// front-to-back opens and closes them in. `name` breaks an exact positional tie
    /// deterministically.
    pub fn in_document_order(&self) -> Vec<&DocumentMark> {
        let mut out: Vec<&DocumentMark> = self.0.values().collect();
        out.sort_by(|a, b| {
            a.start
                .cmp(&b.start)
                .then(a.end.cmp(&b.end))
                .then(a.name.cmp(&b.name))
        });
        out
    }

    /// Validate every mark, reporting all the problems rather than the first — a caller fixing
    /// a name-generation bug wants the whole list, not one round trip per offender.
    pub fn validate(&self) -> Result<(), String> {
        let problems: Vec<String> = self.0.values().filter_map(|m| m.validate().err()).collect();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_mark_is_its_own_start_and_end() {
        let m = DocumentMark::point(12, "skrb_r0000000000000001_aaaaaaaaaaaa");
        assert!(m.is_point());
        assert_eq!((m.start, m.end), (12, 12));
        assert_eq!(m.validate(), Ok(()));
    }

    #[test]
    fn a_range_mark_spans_characters() {
        let m = DocumentMark::range(4, 9, "skrb_c000000000000c001");
        assert!(!m.is_point());
        assert_eq!(m.validate(), Ok(()));
    }

    /// Every rejection is Word's rule, not ODF's — see the module doc for why the stricter of
    /// the two is the one that applies.
    #[test]
    fn a_name_word_would_mangle_is_refused_with_a_reason() {
        let too_long = DocumentMark::point(0, "a".repeat(MAX_BOOKMARK_NAME_LEN + 1));
        assert!(too_long.validate().unwrap_err().contains("41 characters"));

        let hyphenated = DocumentMark::point(0, "skrb-row-1");
        assert!(hyphenated.validate().unwrap_err().contains("only ASCII"));

        let leading_digit = DocumentMark::point(0, "1row");
        assert!(
            leading_digit
                .validate()
                .unwrap_err()
                .contains("begin with a letter")
        );

        assert!(
            DocumentMark::point(0, "")
                .validate()
                .unwrap_err()
                .contains("empty name")
        );
    }

    #[test]
    fn an_inverted_range_is_refused() {
        let m = DocumentMark::range(9, 4, "skrb_c000000000000c001");
        assert!(m.validate().unwrap_err().contains("ends (4) before"));
    }

    #[test]
    fn marks_come_back_in_document_order_not_name_order() {
        let mut marks = DocumentMarks::new();
        marks.insert(DocumentMark::point(30, "zzz_later"));
        marks.insert(DocumentMark::point(10, "aaa_earlier"));
        marks.insert(DocumentMark::range(10, 20, "mmm_same_start"));

        let order: Vec<&str> = marks
            .in_document_order()
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(order, ["aaa_earlier", "mmm_same_start", "zzz_later"]);
    }

    #[test]
    fn one_name_can_only_be_registered_once() {
        let mut marks = DocumentMarks::new();
        marks.insert(DocumentMark::point(
            10,
            "skrb_r0000000000000001_aaaaaaaaaaaa",
        ));
        marks.insert(DocumentMark::point(
            99,
            "skrb_r0000000000000001_aaaaaaaaaaaa",
        ));
        assert_eq!(marks.len(), 1, "a name is a key, not a label");
        assert_eq!(
            marks
                .get("skrb_r0000000000000001_aaaaaaaaaaaa")
                .map(|m| m.start),
            Some(99),
            "the later registration wins"
        );
    }

    #[test]
    fn validation_reports_every_offender_at_once() {
        let mut marks = DocumentMarks::new();
        marks.insert(DocumentMark::point(0, "1bad"));
        marks.insert(DocumentMark::point(0, "also-bad"));
        marks.insert(DocumentMark::point(0, "fine_one"));
        let err = marks.validate().unwrap_err();
        assert!(err.contains("1bad"), "{err}");
        assert!(err.contains("also-bad"), "{err}");
        assert!(!err.contains("fine_one"), "{err}");
    }
}
