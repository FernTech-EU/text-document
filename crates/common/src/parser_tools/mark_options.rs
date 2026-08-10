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

/// Bookmark names Word reserves for itself.
///
/// Legal by every syntactic rule above and unusable all the same: Word maintains these, so a
/// mark carrying one is not stored where it was put. `_GoBack` is the one that matters — Word
/// rewrites it to the last edit position on every save, so an identity parked there would be
/// silently relocated by the very application the file was sent to be edited in.
pub const RESERVED_BOOKMARK_NAMES: &[&str] = &["_GoBack", "_Toc", "_Ref", "_Hlk", "_MailAutoSig"];

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
    /// Which of several marks sharing this exact range comes first.
    ///
    /// Zero for every mark that does not share its range with another, which is almost all of
    /// them, so it can be ignored by any caller that never emits two.
    ///
    /// It exists because ties have to break the **same way** on both payloads. A comment and
    /// the mark carrying its identity are two objects the writers sort independently:
    /// [`DocumentComments`](super::comment_options::DocumentComments) breaks a tie on the
    /// comment's `uid`, and this on the mark's `name`. Where a name is derived from a uid by
    /// hashing — which is what a 40-character bookmark limit forces — the two orders are
    /// uncorrelated, so a document with two comments on the identical range can emit the
    /// annotations in one order and their marks in the other. A reader matching them by
    /// position then hands each comment the other's identity, and the editor's remark comes
    /// home on the wrong thread.
    ///
    /// The producer knows the intended order and nothing else can recover it, so the producer
    /// says: set this from the same sequence the comments are built in.
    ///
    /// `#[serde(default)]` because this field was added after the type shipped: a payload
    /// serialized before it exists has no `ordinal`, and reading it back must give 0 rather
    /// than fail. The default is also the correct answer for such a payload — it was written
    /// by a producer that stated no order.
    #[serde(default)]
    pub ordinal: u32,
}

impl DocumentMark {
    /// A point mark at `at`.
    pub fn point(at: u32, name: impl Into<String>) -> Self {
        Self {
            start: at,
            end: at,
            name: name.into(),
            ordinal: 0,
        }
    }

    /// A range mark over `[start, end)`.
    pub fn range(start: u32, end: u32, name: impl Into<String>) -> Self {
        Self {
            start,
            end,
            name: name.into(),
            ordinal: 0,
        }
    }

    /// This mark, ordered ahead of or behind others sharing its exact range.
    ///
    /// See [`ordinal`](Self::ordinal) — required only when two marks can span the same
    /// characters, which for a host carrying comment identity means two comments on one
    /// paragraph.
    pub fn with_ordinal(mut self, ordinal: u32) -> Self {
        self.ordinal = ordinal;
        self
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
        // Names Word owns. Syntactically fine and semantically taken: Word rewrites `_GoBack`
        // to wherever the last edit was, every save, so a mark of that name is not merely
        // unreliable — it is actively moved by the application the file was sent to. The rest
        // are its own field/TOC bookmarks. A host minting names from its own data will never
        // produce one, which is exactly why it would be missed if it ever did.
        if RESERVED_BOOKMARK_NAMES
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&self.name))
        {
            return Err(format!(
                "mark name '{}' is reserved by Word, which rewrites it on save",
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
                // Before the name, so a producer that states the order gets it. See
                // [`DocumentMark::ordinal`] — the name is the last resort, and for a hashed
                // name it is an arbitrary one.
                .then(a.ordinal.cmp(&b.ordinal))
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

    /// Syntactically perfect and unusable: Word maintains these itself.
    ///
    /// `_GoBack` is the one with teeth — Word moves it to the last edit position on every
    /// save, so an identity parked there is relocated by the very application the file was
    /// sent to. Nothing in this repo mints such a name today; that is the point of checking.
    /// Two marks over the identical range come back in the order the producer stated, not in
    /// the order their names happen to sort.
    ///
    /// This is what keeps a mark payload lined up with the comment payload it carries the
    /// identity of. The two are sorted independently and a hashed name sorts arbitrarily, so
    /// without this a document with two comments on one paragraph could emit the annotations
    /// in one order and their marks in the other — and a reader matching by position would
    /// hand each comment the other's identity.
    #[test]
    fn marks_sharing_a_range_come_back_in_the_order_the_producer_stated() {
        let mut marks = DocumentMarks::default();
        // `zzz` sorts after `aaa` by name, and is stated first.
        marks.insert(DocumentMark::range(4, 9, "zzz_first").with_ordinal(0));
        marks.insert(DocumentMark::range(4, 9, "aaa_second").with_ordinal(1));

        let order: Vec<&str> = marks
            .in_document_order()
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(order, vec!["zzz_first", "aaa_second"]);
    }

    /// A payload written before `ordinal` existed still reads back.
    ///
    /// The field was added to a type that had already shipped in a public crate, so a stored
    /// payload has no `ordinal` key. Without `#[serde(default)]` that is a hard
    /// `missing field` error rather than the 0 it should be.
    #[test]
    fn a_mark_serialized_before_the_ordinal_existed_still_deserializes() {
        let old = r#"{"start":4,"end":9,"name":"skrb_c0000000000000001"}"#;
        let mark: DocumentMark = serde_json::from_str(old).expect("an older payload still reads");
        assert_eq!(mark.ordinal, 0);
        assert_eq!(mark.start, 4);
        assert_eq!(mark.name, "skrb_c0000000000000001");
    }

    /// With no ordinal stated, the name still decides — so a producer that never emits two
    /// marks over one range needs to know nothing about any of this.
    #[test]
    fn marks_without_an_ordinal_still_fall_back_to_the_name() {
        let mut marks = DocumentMarks::default();
        marks.insert(DocumentMark::range(4, 9, "zzz"));
        marks.insert(DocumentMark::range(4, 9, "aaa"));

        let order: Vec<&str> = marks
            .in_document_order()
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(order, vec!["aaa", "zzz"]);
    }

    #[test]
    fn a_name_word_reserves_for_itself_is_refused() {
        for name in RESERVED_BOOKMARK_NAMES {
            let err = DocumentMark::point(0, *name)
                .validate()
                .expect_err("a reserved name must not validate");
            assert!(err.contains("reserved by Word"), "{name}: {err}");
        }
        // Case-insensitively, the way Word compares them.
        assert!(
            DocumentMark::point(0, "_goback")
                .validate()
                .unwrap_err()
                .contains("reserved")
        );
        // And a name that merely starts the same way is fine — the host's own names must not
        // be caught by a prefix rule that was never intended.
        assert!(DocumentMark::point(0, "_Toc_skrb_r00").validate().is_ok());
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
