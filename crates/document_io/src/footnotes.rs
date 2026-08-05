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

    /// The frame holding `label`'s body, if this document has one.
    ///
    /// `None` is ordinary, not an error: a host that keeps note bodies in its
    /// own store puts references in the prose and no definitions at all.
    pub fn definition(&self, label: &str) -> Option<EntityId> {
        self.definitions.get(label).copied()
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
