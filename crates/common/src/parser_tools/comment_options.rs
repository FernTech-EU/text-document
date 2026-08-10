// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Comment payload shared by every writer that can anchor a margin note into its output —
//! today DOCX ([`super::docx_options::DocxExportOptions::comments`]), and ODT once its own
//! writer reaches this crate. One definition here, reused by both, is the whole point: a
//! DOCX-only `struct Comment` in `document_io` and a parallel ODT-only one elsewhere would let
//! the two drift the moment a field's meaning changed in one but not the other.
//!
//! # The character range this crate deals in
//!
//! [`DocumentComment::start`]/[`DocumentComment::end`] are `[start, end)` in the document's
//! own **addressable character space** — the same space `TextDocument::to_addressable_text()`,
//! `find_all` match positions, and a block's `document_position` all share (see
//! [`crate::format_runs::AddressableInlinePiece`]'s doc comment for the full contract). A
//! writer splitting a run at a comment boundary must resolve that boundary against
//! [`crate::format_runs_query::addressable_inline_pieces_for_block`]'s own `start`/`end`
//! fields — never against `FormatRun`'s block-local UTF-8 *byte* offsets, which agree with
//! this crate's char offsets only by coincidence on the first line of the first block.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One reply in a comment's thread.
///
/// A reply carries no range of its own: in every writer this crate feeds, a reply anchors to
/// the exact same span as the comment it answers — precisely how Word and LibreOffice both
/// render a reply thread (one highlighted range in the body, several bubbles stacked in the
/// margin), and precisely what lets a writer treat "open this thread's range" and "open each
/// reply's own range" as the same operation repeated once per reply.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CommentReply {
    /// Durable identifier, stable across a save/reload round trip the way `BinderItem::uid`
    /// is in the app this crate was built for — never a store id or a document position,
    /// both of which are free to be re-minted the moment the host reloads.
    pub uid: String,
    pub author: String,
    #[serde(default)]
    pub author_initials: String,
    /// ISO-8601 (e.g. `"2026-08-09T12:00:00Z"`).
    pub date: String,
    /// Djot source. A writer that cannot embed rich text (a plain-text export, say) is free
    /// to reduce it to its plain reading; every rich writer this crate ships today renders at
    /// least bold, italic and paragraph/line breaks — see each writer's own body renderer.
    pub body: String,
}

/// One comment thread: an opening note anchored to a character range, plus its flat list of
/// replies.
///
/// Flat, not a tree: neither DOCX nor ODT's comment model nests a reply under another reply
/// (Word's own UI does not offer it either), so a second level of nesting would have nowhere
/// to go in the output format — the model does not pretend to support what no consumer of it
/// can render.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DocumentComment {
    /// `[start, end)` in the document's addressable character space. `start == end` anchors
    /// the comment to a single insertion point rather than a highlighted run — both DOCX and
    /// ODT accept an empty range there.
    pub start: u32,
    pub end: u32,
    /// Durable identifier — see [`CommentReply::uid`].
    pub uid: String,
    pub author: String,
    #[serde(default)]
    pub author_initials: String,
    /// ISO-8601 (e.g. `"2026-08-09T12:00:00Z"`).
    pub date: String,
    /// Whether the thread is marked resolved (DOCX `w15:done`; ODT's own resolved marker on
    /// the annotation). Resolving a thread does not delete it: the opening note and its full
    /// reply history are still written out either way, just flagged.
    #[serde(default)]
    pub resolved: bool,
    /// Djot source — see [`CommentReply::body`].
    pub body: String,
    #[serde(default)]
    pub replies: Vec<CommentReply>,
}

/// Every comment thread supplied to one export, keyed by [`DocumentComment::uid`].
///
/// A `BTreeMap`, not a `HashMap`, for the same reason [`super::image_options::ExportImages`]
/// is one: two exports of the same document must be byte-comparable, and a randomised
/// iteration order would quietly break that. Keying by `uid` rather than storing a bare `Vec`
/// also makes "does this document already carry a thread with this id" an O(log n) lookup
/// instead of a linear scan — relevant because a caller re-exporting after an edit typically
/// hands over its whole current comment set again, not just what changed since last time.
///
/// Iteration order is uid order, which is *not* the order a writer needs to open and close
/// ranges in as it walks the document front to back — use
/// [`in_document_order`](Self::in_document_order) for that.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DocumentComments(BTreeMap<String, DocumentComment>);

impl DocumentComments {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a comment, keyed by its own `uid`. A second insert under the same uid
    /// replaces the first — the caller is expected to hand over its current state on every
    /// export, not maintain an append-only log through this type.
    pub fn insert(&mut self, comment: DocumentComment) -> &mut Self {
        self.0.insert(comment.uid.clone(), comment);
        self
    }

    pub fn get(&self, uid: &str) -> Option<&DocumentComment> {
        self.0.get(uid)
    }

    pub fn iter(&self) -> impl Iterator<Item = &DocumentComment> {
        self.0.values()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Every comment, sorted by `(start, end, uid)` — the order a writer walking the document
    /// text front-to-back needs to open and close ranges in. `uid` breaks an exact
    /// `(start, end)` tie deterministically (two threads anchored to the identical span)
    /// rather than leaving it to whatever order the `BTreeMap`'s own key (`uid` again, but
    /// unsorted by position) happened to produce.
    pub fn in_document_order(&self) -> Vec<&DocumentComment> {
        let mut out: Vec<&DocumentComment> = self.0.values().collect();
        out.sort_by(|a, b| {
            a.start
                .cmp(&b.start)
                .then(a.end.cmp(&b.end))
                .then(a.uid.cmp(&b.uid))
        });
        out
    }
}

impl FromIterator<DocumentComment> for DocumentComments {
    fn from_iter<I: IntoIterator<Item = DocumentComment>>(iter: I) -> Self {
        let mut out = Self::default();
        for c in iter {
            out.insert(c);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(uid: &str, start: u32, end: u32) -> DocumentComment {
        DocumentComment {
            start,
            end,
            uid: uid.to_string(),
            author: "Author".to_string(),
            author_initials: "AU".to_string(),
            date: "2026-01-01T00:00:00Z".to_string(),
            resolved: false,
            body: "Body".to_string(),
            replies: vec![],
        }
    }

    #[test]
    fn insert_keys_by_uid_and_replaces_on_reinsert() {
        let mut comments = DocumentComments::new();
        comments.insert(comment("a", 0, 5));
        comments.insert(comment("a", 10, 20));
        assert_eq!(comments.len(), 1);
        assert_eq!(comments.get("a").unwrap().start, 10);
    }

    #[test]
    fn document_order_sorts_by_start_then_end_then_uid() {
        let comments: DocumentComments =
            [comment("z", 5, 10), comment("a", 5, 10), comment("m", 0, 3)]
                .into_iter()
                .collect();
        let ordered: Vec<&str> = comments
            .in_document_order()
            .into_iter()
            .map(|c| c.uid.as_str())
            .collect();
        assert_eq!(ordered, vec!["m", "a", "z"]);
    }

    #[test]
    fn iteration_order_is_stable_across_builds() {
        let build = || -> DocumentComments {
            ["z", "a", "m"]
                .into_iter()
                .map(|uid| comment(uid, 0, 1))
                .collect()
        };
        let first: Vec<String> = build().iter().map(|c| c.uid.clone()).collect();
        let second: Vec<String> = build().iter().map(|c| c.uid.clone()).collect();
        assert_eq!(first, second);
        assert_eq!(first, vec!["a", "m", "z"]);
    }

    #[test]
    fn empty_range_and_replies_round_trip_through_json() {
        let mut c = comment("root", 4, 4);
        c.replies.push(CommentReply {
            uid: "reply-1".to_string(),
            author: "Editor".to_string(),
            author_initials: "ED".to_string(),
            date: "2026-02-02T00:00:00Z".to_string(),
            body: "*Fixed.*".to_string(),
        });
        let json = serde_json::to_string(&c).expect("serialize");
        let back: DocumentComment = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, c);
        assert_eq!(back.start, back.end);
        assert_eq!(back.replies.len(), 1);
    }
}
