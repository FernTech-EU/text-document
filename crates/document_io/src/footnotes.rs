//! What every writer needs to know about a document's footnotes.
//!
//! Three questions, and each of them has one right answer that must not be
//! worked out twice:
//!
//! * **Which frames are notes rather than prose.** A definition is a detached
//!   top-level frame, and every exporter's outer loop walks *all* top-level
//!   frames — so without a skip-set each one renders note bodies inline, in the
//!   middle of the chapter, at the point the definition happened to be typed.
//! * **What number a reference prints.** Not stored anywhere: it is a fact about
//!   document order, so it is derived here, once, from the order references
//!   actually appear.
//! * **Where a note's body is**, given the label a reference carries.
//!
//! Built by reading the store directly, the way `block_content_via_store`
//! already does, so a writer needs no extra fetches and no change to its unit of
//! work — which is what keeps this from being re-derived per writer. The
//! `cell_frame_ids` computation next door is written out seven times; this is
//! deliberately written once.
//!
//! **A reference cited only from inside another note's own body is refused, not
//! numbered.** [`Footnotes::build`]'s numbering walk already excludes definition
//! frames (see its own doc for why: numbering by where the *definition* sits
//! would be wrong). That means such a label never gets a number, never appears
//! in [`Footnotes::in_print_order`], and no writer ever renders its aside/body —
//! so a writer that still linked the reference (an HTML `href`, a Markdown
//! `[^label]`) would point at a target nothing emits.
//! [`Footnotes::is_nested_reference`] is how a writer tells this case apart from
//! an ordinary **dangling** reference (no definition anywhere — the normal
//! state for a host that owns note bodies itself, see `FootnoteRefAnchor`'s doc
//! in `common::format_runs`): a dangling label still gets a number here (the
//! main-flow walk does not care whether a definition exists) and keeps
//! whatever a writer already does for it; only the nested case newly degrades
//! to a bare, unlinked marker. LaTeX, Typst and DOCX need no such check — their
//! own "no body" fallback (`\footnotemark`, a bare raised label, an empty
//! native footnote) already covers both cases without ever emitting a dangling
//! target.

use std::collections::{HashMap, HashSet};

use common::database::Store;
use common::types::EntityId;

/// A document's footnotes, resolved.
#[derive(Debug, Default, Clone)]
pub struct Footnotes {
    /// Label → the number a reader sees, 1-based, in document order.
    numbers: HashMap<String, usize>,
    /// Label → the frame holding that note's body.
    definitions: HashMap<String, EntityId>,
    /// Every definition frame, for the outer walk to skip.
    definition_frames: HashSet<EntityId>,
    /// What the host says each label prints, when it has an opinion.
    overrides: HashMap<String, String>,
}

impl Footnotes {
    /// Resolve every footnote in the document.
    ///
    /// Numbering follows the order references appear in the prose — blocks by
    /// `document_position`, and within a block by byte offset — **not** the
    /// order definitions were written. A writer who put their notes at the
    /// bottom of the file still gets 1, 2, 3 in reading order.
    ///
    /// Definition frames are excluded from that walk. Their bodies are prose
    /// too, and a note that itself cites another note would otherwise number the
    /// inner reference by where the *definition* sits rather than by where the
    /// note is read.
    pub fn build(store: &Store) -> Self {
        let frames = store.frames.read();
        let mut definitions: HashMap<String, EntityId> = HashMap::new();
        let mut definition_frames: HashSet<EntityId> = HashSet::new();
        let mut definition_blocks: HashSet<EntityId> = HashSet::new();

        for frame in frames.values() {
            let Some(label) = &frame.footnote_label else {
                continue;
            };
            definition_frames.insert(frame.id);
            // Last one wins, deterministically: a duplicate label is malformed
            // input, and silently keeping the first would depend on hash order.
            definitions.insert(label.clone(), frame.id);
            for child in &frame.child_order {
                if *child > 0 {
                    definition_blocks.insert(*child as EntityId);
                }
            }
        }
        drop(frames);

        // Blocks in reading order, definitions left out.
        let mut ordered: Vec<(i64, EntityId)> = store
            .blocks
            .read()
            .values()
            .filter(|b| !definition_blocks.contains(&b.id))
            .map(|b| (b.document_position, b.id))
            .collect();
        ordered.sort_unstable();

        let refs = store.block_footnote_refs.read();
        let mut numbers: HashMap<String, usize> = HashMap::new();
        let mut next = 1usize;
        for (_, block_id) in ordered {
            let Some(anchors) = refs.get(&block_id) else {
                continue;
            };
            let mut in_block: Vec<_> = anchors.iter().collect();
            in_block.sort_by_key(|a| a.byte_offset);
            for anchor in in_block {
                // A label referenced twice keeps one number — it is one note.
                numbers.entry(anchor.label.clone()).or_insert_with(|| {
                    let n = next;
                    next += 1;
                    n
                });
            }
        }

        Self {
            numbers,
            definitions,
            definition_frames,
            overrides: store.footnote_markers.read().clone(),
        }
    }

    /// Is this frame a note's body, and therefore not part of the flow?
    pub fn is_definition(&self, frame_id: EntityId) -> bool {
        self.definition_frames.contains(&frame_id)
    }

    /// Is `label` cited only from inside another note's own body?
    ///
    /// `label` has a definition (someone wrote `[^label]: …` somewhere in this
    /// document) but never earned a number, which — given [`build`](Self::build)
    /// assigns one to every label its main-flow walk sees — can only mean every
    /// citation of it lives inside a definition frame, excluded from that walk
    /// by design. A writer must not link such a reference: nothing will ever
    /// render this label's aside, so an `href`/`[^label]` pointing at it would
    /// dangle. `false` for an ordinary **dangling** reference (no definition at
    /// all) — that one keeps its number and whatever a writer already does with
    /// it; see the module doc for why the two are not the same thing.
    pub fn is_nested_reference(&self, label: &str) -> bool {
        !self.numbers.contains_key(label) && self.definitions.contains_key(label)
    }

    /// What `label`'s reference prints.
    ///
    /// Falls back to the label itself for a reference that somehow never
    /// appeared in the walk — visible and traceable, rather than an empty
    /// marker the reader cannot tell from a rendering fault.
    pub fn marker(&self, label: &str) -> String {
        // The host wins. It knows things this document cannot: which chapter of
        // which book this text came from, and therefore whether note numbering
        // restarted above it. A document holding one exported chapter would
        // otherwise number its notes from one and disagree with the editor.
        if let Some(m) = self.overrides.get(label) {
            return m.clone();
        }
        match self.numbers.get(label) {
            Some(n) => n.to_string(),
            None => label.to_string(),
        }
    }

    /// Whether the document has any footnotes at all — lets a writer skip its
    /// whole notes section rather than emit an empty one.
    pub fn is_empty(&self) -> bool {
        self.numbers.is_empty() && self.definitions.is_empty()
    }

    /// Every note that has a body, in printed order, as `(number, label, frame)`.
    ///
    /// The order an endnote group is written in. A definition whose reference
    /// was deleted has no number and is left out: it is no longer part of the
    /// book, and numbering it would put a note in the list that nothing points
    /// at.
    pub fn in_print_order(&self) -> Vec<(usize, String, EntityId)> {
        let mut out: Vec<(usize, String, EntityId)> = self
            .definitions
            .iter()
            .filter_map(|(label, frame)| {
                self.numbers.get(label).map(|n| (*n, label.clone(), *frame))
            })
            .collect();
        out.sort_unstable();
        out
    }
}

/// A deterministic, format-safe identifier for a footnote `label`, for the two backends whose
/// own repeat-citation syntax needs a bare identifier — LaTeX's `\label{…}`/`\getrefnumber{…}`,
/// Typst's `<…>` — rather than the arbitrary text a Djot `[^label]` may actually carry (any
/// character but `]`, per the importer; no relation to what LaTeX/Typst accept in a label token).
///
/// Keeps the label's own ASCII letters/digits, so the common case (`"n1"`, `"fn3"`) stays
/// legible in the emitted markup, and folds a hash of the **full, original** label on the end —
/// not to disambiguate collisions among the kept characters alone (`"fn.1"` and `"fn-1"` would
/// otherwise both fold to `"fn1"` and collide onto one LaTeX/Typst footnote), but so two labels
/// differing only in punctuation can never be confused for a repeat citation of one note.
/// `DefaultHasher` rather than `HashMap`'s `RandomState`: its seed is fixed, not randomized per
/// process, which is what makes the same label produce the same id on every run — required,
/// since the first and a later citation of one label must resolve to the same id in the same
/// export.
pub fn safe_label_id(label: &str) -> String {
    use std::hash::{Hash, Hasher};

    let kept: String = label
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    label.hash(&mut hasher);
    format!("fn{kept}{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_label_id_is_deterministic() {
        assert_eq!(safe_label_id("n1"), safe_label_id("n1"));
    }

    #[test]
    fn safe_label_id_differs_for_different_labels() {
        assert_ne!(safe_label_id("n1"), safe_label_id("n2"));
    }

    #[test]
    fn safe_label_id_does_not_collide_on_shared_alphanumerics() {
        // "fn.1" and "fn-1" keep the identical alphanumeric characters ("fn1"); only the
        // hash suffix (computed over the whole original label) can tell them apart.
        assert_ne!(safe_label_id("fn.1"), safe_label_id("fn-1"));
    }

    #[test]
    fn safe_label_id_is_ascii_and_starts_with_a_letter() {
        // A LaTeX \label / Typst <label> must not start with a digit, and must contain
        // nothing a label/reference token in either language would choke on.
        let id = safe_label_id("héllo Wörld! 42");
        assert!(id.chars().next().unwrap().is_ascii_alphabetic());
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
