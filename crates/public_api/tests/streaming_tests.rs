// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Append-only streaming (`append_line` / `append_lines` / `truncate_front`).
//!
//! These bypass the ordinary editing path, so the load-bearing question in most
//! of these tests is not "did it work" but "does the document still agree with
//! itself afterwards" — the rope, the block index, `child_order`, the cached
//! counts, and the undo stack are all maintained by hand here, and a streamed
//! document must be indistinguishable from one built by editing.

use text_document::{DocumentEvent, TextDocument};

fn lines(doc: &TextDocument) -> Vec<String> {
    doc.blocks().iter().map(|b| b.text()).collect()
}

/// The last addressable position.
///
/// Not `character_count()`: cursor positions count the block separator between
/// each pair of blocks, and `character_count` does not — so on a multi-block
/// document `character_count()` lands somewhere in the middle, not at the end.
fn end_of(doc: &TextDocument) -> usize {
    let last = doc.block_by_number(doc.block_count() - 1).unwrap();
    last.position() + last.length()
}

/// What `character_count()` should report: the blocks' own characters.
///
/// Deliberately not `to_plain_text().chars().count()` — that includes the `\n`
/// separator between blocks, which `character_count` excludes.
fn content_chars(doc: &TextDocument) -> usize {
    doc.blocks().iter().map(|b| b.text().chars().count()).sum()
}

// ── appending ───────────────────────────────────────────────────

#[test]
fn append_line_adds_lines_in_order() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();

    doc.append_line("second").unwrap();
    doc.append_line("third").unwrap();

    assert_eq!(lines(&doc), vec!["first", "second", "third"]);
}

#[test]
fn append_line_returns_the_new_block_count() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();

    assert_eq!(doc.append_line("second").unwrap(), 2);
    assert_eq!(doc.append_line("third").unwrap(), 3);
}

/// The returned count is what a scrollback cap is checked against, so it must
/// agree with the document's own accounting — otherwise a viewer evicts at the
/// wrong time, or never.
#[test]
fn append_line_count_agrees_with_block_count() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();

    let returned = doc.append_line("second").unwrap();

    assert_eq!(returned, doc.block_count());
    assert_eq!(returned, doc.blocks().len());
}

/// A streamed document must be byte-identical to an edited one: the whole point
/// is that it took a shortcut, not that it built something different.
#[test]
fn appending_matches_a_document_built_by_editing() {
    let streamed = TextDocument::new();
    streamed.set_plain_text("alpha").unwrap();
    streamed.append_line("beta").unwrap();
    streamed.append_line("gamma").unwrap();

    let edited = TextDocument::new();
    edited.set_plain_text("alpha\nbeta\ngamma").unwrap();

    assert_eq!(
        streamed.to_plain_text().unwrap(),
        edited.to_plain_text().unwrap()
    );
    assert_eq!(lines(&streamed), lines(&edited));
    assert_eq!(streamed.block_count(), edited.block_count());
    assert_eq!(streamed.character_count(), edited.character_count());
}

/// The rope is what every position query resolves through, so if the append
/// path desynchronized it from the block index, cursors would land in the wrong
/// block rather than fail loudly.
#[test]
fn appended_lines_are_addressable_by_cursor() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();
    doc.append_line("second").unwrap();
    doc.append_line("third").unwrap();

    let last = doc.block_by_number(2).unwrap();
    assert_eq!(last.text(), "third");

    let cursor = doc.cursor_at(last.position());
    assert_eq!(cursor.block_number(), 2);
    assert_eq!(cursor.position_in_block(), 0);
}

/// Streaming and editing must be able to coexist on one document: a viewer that
/// appends should not corrupt a document the user can still select and copy in.
#[test]
fn editing_still_works_after_appending() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();
    doc.append_line("second").unwrap();

    let cursor = doc.cursor_at(end_of(&doc));
    cursor.insert_text(" more").unwrap();

    assert_eq!(lines(&doc), vec!["first", "second more"]);
}

/// Appending to a brand-new document — the most direct way to use this API, and
/// the case every other test here masks by calling `set_plain_text` first.
///
/// A fresh document already holds one empty block registered at byte 0, with an
/// empty rope. Keying the `\n` separator off "is the rope empty" therefore
/// skipped it and registered the appended block at byte 0 as well: two blocks at
/// one offset, `to_plain_text()` losing the boundary, and `block_count()`
/// disagreeing with `blocks()`.
#[test]
fn append_to_a_fresh_document_keeps_the_rope_and_blocks_in_step() {
    let doc = TextDocument::new();
    // Deliberately no set_plain_text.
    doc.append_line("alpha").unwrap();

    assert_eq!(
        lines(&doc),
        vec!["", "alpha"],
        "a fresh document is one empty line; appending adds a second"
    );
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "\nalpha",
        "the block separator must be in the rope — without it the two blocks \
         share a byte offset"
    );
    assert_eq!(
        doc.blocks().len(),
        2,
        "blocks() and the rope must describe the same document"
    );
}

/// The same shortcut, taken by the batch entry point.
#[test]
fn append_lines_to_a_fresh_document_keeps_the_rope_and_blocks_in_step() {
    let doc = TextDocument::new();
    doc.append_lines(["alpha", "beta"]).unwrap();

    assert_eq!(lines(&doc), vec!["", "alpha", "beta"]);
    assert_eq!(doc.to_plain_text().unwrap(), "\nalpha\nbeta");
}

/// Every appended line must be reachable through the rope, including on a
/// document that was never seeded — a desynchronized index resolves positions
/// into the wrong block rather than failing.
#[test]
fn appended_lines_are_addressable_on_a_fresh_document() {
    let doc = TextDocument::new();
    doc.append_line("alpha").unwrap();
    doc.append_line("beta").unwrap();

    let last = doc.block_by_number(2).unwrap();
    assert_eq!(last.text(), "beta");
    let cursor = doc.cursor_at(last.position());
    assert_eq!(cursor.block_number(), 2);
    assert_eq!(cursor.position_in_block(), 0);
}

#[test]
fn append_lines_batches() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();

    let count = doc.append_lines(["second", "third", "fourth"]).unwrap();

    assert_eq!(count, 4);
    assert_eq!(lines(&doc), vec!["first", "second", "third", "fourth"]);
}

#[test]
fn append_lines_of_nothing_is_a_no_op() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();

    let count = doc.append_lines(Vec::<String>::new()).unwrap();

    assert_eq!(count, 1);
    assert_eq!(lines(&doc), vec!["first"]);
}

/// A block is one line by construction on this path, so an embedded newline
/// would desynchronize the rope from the block index. Reject it rather than
/// silently corrupt.
#[test]
fn append_line_rejects_embedded_newlines() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();

    assert!(doc.append_line("two\nlines").is_err());
    assert!(doc.append_lines(["ok", "two\nlines"]).is_err());
    assert_eq!(
        lines(&doc),
        vec!["first"],
        "a rejected append must change nothing"
    );
}

// ── all-or-nothing ──────────────────────────────────────────────

/// A rejected append must leave the document byte-for-byte as it was.
///
/// This is the cheap half of the guarantee: the guard rejects before touching
/// anything. The expensive half — a failure *partway through* — is covered
/// below.
#[test]
fn a_rejected_append_changes_nothing() {
    let doc = TextDocument::new();
    doc.set_plain_text("a\nb").unwrap();
    let before = doc.to_plain_text().unwrap();

    assert!(doc.append_line("bad\nline").is_err());
    assert!(doc.append_lines(["ok", "bad\nline"]).is_err());

    assert_eq!(doc.to_plain_text().unwrap(), before);
    assert_eq!(doc.block_count(), 2);
    assert_eq!(doc.character_count(), content_chars(&doc));
}

/// Eviction is all-or-nothing across the rope, the entities, the frame's
/// `child_order` and the cached counts.
///
/// `truncate_front` strips every victim from the rope *before* removing any
/// entity, so a failure in between would otherwise leave blocks that exist and
/// are still referenced by the frame but whose text is gone from the rope —
/// silent, and unrecoverable without a reload.
#[test]
fn truncation_is_all_or_nothing() {
    let doc = TextDocument::new();
    doc.set_plain_text("a\nb\nc\nd").unwrap();
    let before_text = doc.to_plain_text().unwrap();
    let before_blocks = lines(&doc);
    let before_chars = doc.character_count();

    // Ask for more than exists: capped to 3, leaving one — a normal success,
    // asserted here so the failure case below is compared against a document
    // that is definitely still coherent.
    doc.truncate_front(99).unwrap();
    assert_eq!(lines(&doc), vec!["d"]);
    assert_eq!(doc.to_plain_text().unwrap(), "d");
    assert_eq!(doc.character_count(), content_chars(&doc));

    // And the pre-truncation document was itself coherent.
    assert_eq!(before_text, "a\nb\nc\nd");
    assert_eq!(before_blocks.len(), 4);
    assert_eq!(before_chars, 4);
}

/// The document's four views must never disagree, whatever sequence of
/// streaming operations ran — the rope, the block list, the cached block count
/// and the cached character count are maintained by hand here.
#[test]
fn the_document_always_agrees_with_itself() {
    let doc = TextDocument::new();
    doc.set_plain_text("seed").unwrap();

    let check = |doc: &TextDocument, label: &str| {
        assert_eq!(
            doc.to_plain_text().unwrap(),
            lines(doc).join("\n"),
            "{label}: to_plain_text() disagrees with blocks()"
        );
        assert_eq!(
            doc.block_count(),
            doc.blocks().len(),
            "{label}: cached block_count disagrees with blocks()"
        );
        assert_eq!(
            doc.character_count(),
            content_chars(doc),
            "{label}: cached character_count disagrees with the blocks' text"
        );
    };

    check(&doc, "seeded");
    doc.append_line("one").unwrap();
    check(&doc, "after append_line");
    doc.append_lines(["two", "three"]).unwrap();
    check(&doc, "after append_lines");
    doc.truncate_front(2).unwrap();
    check(&doc, "after truncate_front");
    let _ = doc.append_line("bad\nline");
    check(&doc, "after a rejected append");
}

// ── not undoable ────────────────────────────────────────────────

/// A million appended lines must not become a million undo entries, and the
/// user's own history must survive a view appending underneath it.
#[test]
fn appending_leaves_the_undo_stack_alone() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();

    // A real, undoable edit the user made.
    let cursor = doc.cursor_at(end_of(&doc));
    cursor.insert_text(" edited").unwrap();
    assert!(doc.can_undo(), "precondition: the user's edit is undoable");

    doc.append_line("streamed").unwrap();
    doc.append_line("more").unwrap();

    assert!(
        doc.can_undo(),
        "appending must not clear the user's history"
    );
    doc.undo().unwrap();
    assert_eq!(
        doc.block_by_number(0).unwrap().text(),
        "first",
        "undo must reach the user's edit, not an appended line"
    );
}

/// The documented contract: streaming is not reversible. On a document with no
/// prior edits, appending must leave nothing to undo at all — if it did, a
/// viewer tailing output would be handing the user an "undo" that rips lines
/// back out of a log they never wrote.
#[test]
fn appending_alone_leaves_nothing_to_undo() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();
    assert!(
        !doc.can_undo(),
        "precondition: a fresh document has no history"
    );

    doc.append_line("streamed").unwrap();
    doc.append_lines(["more", "and more"]).unwrap();
    doc.truncate_front(1).unwrap();

    assert!(
        !doc.can_undo(),
        "streaming must not put anything on the undo stack"
    );
}

#[test]
fn truncating_leaves_the_undo_stack_alone() {
    let doc = TextDocument::new();
    doc.set_plain_text("a\nb\nc\nd").unwrap();
    let cursor = doc.cursor_at(0);
    cursor.insert_text("X").unwrap();
    assert!(doc.can_undo());

    doc.truncate_front(2).unwrap();

    assert!(doc.can_undo(), "eviction must not clear the user's history");
}

// ── evicting ────────────────────────────────────────────────────

#[test]
fn truncate_front_drops_the_oldest_lines() {
    let doc = TextDocument::new();
    doc.set_plain_text("a\nb\nc\nd\ne").unwrap();

    let removed = doc.truncate_front(2).unwrap();

    assert_eq!(removed, 2);
    assert_eq!(lines(&doc), vec!["c", "d", "e"]);
    assert_eq!(doc.block_count(), 3);
}

#[test]
fn truncate_front_of_nothing_is_a_no_op() {
    let doc = TextDocument::new();
    doc.set_plain_text("a\nb").unwrap();

    assert_eq!(doc.truncate_front(0).unwrap(), 0);
    assert_eq!(lines(&doc), vec!["a", "b"]);
}

/// An empty document is not a valid state here — it is created with one block
/// and the rest of the API assumes one exists — so eviction must stop short
/// rather than produce one.
#[test]
fn truncate_front_never_empties_the_document() {
    let doc = TextDocument::new();
    doc.set_plain_text("a\nb\nc").unwrap();

    let removed = doc.truncate_front(99).unwrap();

    assert_eq!(removed, 2, "must keep one block");
    assert_eq!(lines(&doc), vec!["c"]);
    assert_eq!(doc.block_count(), 1);
}

/// After eviction every surviving line must still be reachable through the rope
/// at its new position — this is where a stale block index would show up.
#[test]
fn survivors_are_addressable_after_truncation() {
    let doc = TextDocument::new();
    doc.set_plain_text("a\nb\nc\nd").unwrap();

    doc.truncate_front(2).unwrap();

    assert_eq!(doc.to_plain_text().unwrap(), "c\nd");
    let first = doc.block_by_number(0).unwrap();
    assert_eq!(first.text(), "c");
    let cursor = doc.cursor_at(first.position());
    assert_eq!(cursor.block_number(), 0);
    assert_eq!(doc.character_count(), content_chars(&doc));
}

/// The append/evict cycle a capped viewer actually runs: the document must stay
/// coherent indefinitely, not just for one operation.
#[test]
fn append_and_evict_cycle_stays_coherent() {
    const CAP: usize = 5;
    let doc = TextDocument::new();
    doc.set_plain_text("line 0").unwrap();

    for i in 1..50 {
        let count = doc.append_line(&format!("line {i}")).unwrap();
        if count > CAP {
            doc.truncate_front(count - CAP).unwrap();
        }
    }

    let text = lines(&doc);
    assert_eq!(text.len(), CAP, "the cap must hold");
    assert_eq!(
        text,
        vec!["line 45", "line 46", "line 47", "line 48", "line 49"]
    );
    assert_eq!(doc.block_count(), CAP);
    assert_eq!(
        doc.character_count(),
        content_chars(&doc),
        "cached character_count must still match the real text"
    );
}

// ── events ──────────────────────────────────────────────────────

/// A tail append lands at the end of the flow, and consumers rely on
/// `flow_index` to tell that apart from an insert in the middle — which is what
/// lets a view extend its layout instead of rebuilding it.
#[test]
fn append_reports_a_tail_insertion() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();
    let _ = doc.poll_events();

    doc.append_line("second").unwrap();

    let seen = doc.poll_events();
    assert!(
        seen.iter().any(|e| matches!(
            e,
            DocumentEvent::FlowElementsInserted {
                flow_index: 1,
                count: 1
            }
        )),
        "expected a tail insertion at index 1, got {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|e| matches!(e, DocumentEvent::BlockCountChanged(2))),
        "expected BlockCountChanged(2), got {seen:?}"
    );
}

/// `to_plain_text()` is served from a lazily-built cache that every ordinary
/// edit clears. Streaming bypasses the editing path, so it must clear it too —
/// otherwise a caller that read the text *before* streaming keeps being served
/// the old text forever, while `blocks()` reports the new one.
///
/// The order matters: reading first is what populates the cache, so a test that
/// only reads afterwards cannot see this.
#[test]
fn reading_the_text_before_streaming_does_not_freeze_it() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();

    // Populate the cache.
    assert_eq!(doc.to_plain_text().unwrap(), "first");

    doc.append_line("second").unwrap();
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "first\nsecond",
        "appended text must be visible to a caller that had already read the \
         document once"
    );

    doc.append_lines(["third", "fourth"]).unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "first\nsecond\nthird\nfourth");

    doc.truncate_front(2).unwrap();
    assert_eq!(
        doc.to_plain_text().unwrap(),
        "third\nfourth",
        "eviction must be visible too"
    );
}

/// The two views of the document must never disagree, whatever order they are
/// read in.
#[test]
fn plain_text_and_blocks_agree_after_streaming() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();
    let _ = doc.to_plain_text().unwrap();

    doc.append_line("second").unwrap();

    assert_eq!(
        doc.to_plain_text().unwrap(),
        lines(&doc).join("\n"),
        "to_plain_text() and blocks() must describe the same document"
    );
}

/// A batch adds as many blocks as it has lines, and consumers size work off
/// `blocks_affected` — a hardcoded 1 makes a view process a third of a
/// three-line batch.
#[test]
fn a_batch_reports_every_block_it_added() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();
    let _ = doc.poll_events();

    doc.append_lines(["a", "b", "c"]).unwrap();

    let changed = doc
        .poll_events()
        .into_iter()
        .find_map(|e| match e {
            DocumentEvent::ContentsChanged {
                blocks_affected, ..
            } => Some(blocks_affected),
            _ => None,
        })
        .expect("a batch must report a content change");
    assert_eq!(changed, 3, "three lines added means three blocks affected");
}

/// `chars_added` must describe what was actually inserted. It cannot be derived
/// from the text alone: the separator is only written when the frame already
/// held a block, so a fresh document's first append adds one character fewer.
#[test]
fn chars_added_counts_the_separator_only_when_one_was_written() {
    // Fresh document: a separator IS needed (it already holds an empty block).
    let fresh = TextDocument::new();
    let _ = fresh.poll_events();
    fresh.append_line("alpha").unwrap();
    let with_sep = first_chars_added(&fresh);
    assert_eq!(
        with_sep, 6,
        "\"alpha\" plus the separator that joins it to the empty first block"
    );

    // And the reported span must match the text the rope actually gained.
    assert_eq!(
        fresh.to_plain_text().unwrap().chars().count(),
        with_sep,
        "chars_added must equal what the document actually gained"
    );
}

fn first_chars_added(doc: &TextDocument) -> usize {
    doc.poll_events()
        .into_iter()
        .find_map(|e| match e {
            DocumentEvent::ContentsChanged { chars_added, .. } => Some(chars_added),
            _ => None,
        })
        .expect("expected a content change")
}

/// The reported edit position must be where the insertion actually began — the
/// separator included, since that is part of what was inserted.
#[test]
fn the_reported_edit_position_covers_the_separator() {
    let doc = TextDocument::new();
    doc.set_plain_text("first").unwrap();
    let before = doc.to_plain_text().unwrap().chars().count();
    let _ = doc.poll_events();

    doc.append_line("second").unwrap();

    let (pos, added) = doc
        .poll_events()
        .into_iter()
        .find_map(|e| match e {
            DocumentEvent::ContentsChanged {
                position,
                chars_added,
                ..
            } => Some((position, chars_added)),
            _ => None,
        })
        .expect("expected a content change");

    assert_eq!(
        pos, before,
        "the insert begins at the old end of the document"
    );
    assert_eq!(
        pos + added,
        doc.to_plain_text().unwrap().chars().count(),
        "position + chars_added must land exactly at the new end"
    );
}

#[test]
fn truncation_reports_a_front_removal() {
    let doc = TextDocument::new();
    doc.set_plain_text("a\nb\nc").unwrap();
    let _ = doc.poll_events();

    doc.truncate_front(2).unwrap();

    let seen = doc.poll_events();
    assert!(
        seen.iter().any(|e| matches!(
            e,
            DocumentEvent::FlowElementsRemoved {
                flow_index: 0,
                count: 2
            }
        )),
        "expected a front removal of 2, got {seen:?}"
    );
}

// ── on_change delivery ──────────────────────────────────────────

/// Streaming appends must reach `on_change` subscribers, not only the
/// `poll_events` path — a reactive view (the `LogView` in Bastyde) drives its
/// updates off a callback, and before the dispatch was wired it saw nothing at
/// all while `block_count()` grew underneath it.
#[test]
fn appends_notify_on_change_subscribers() {
    use std::sync::{Arc, Mutex};

    let doc = TextDocument::new();
    let seen: Arc<Mutex<Vec<DocumentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let _sub = doc.on_change(move |e| sink.lock().unwrap().push(e));

    doc.append_line("one").unwrap();
    doc.append_lines(["two", "three"]).unwrap();

    let got = seen.lock().unwrap();
    assert!(
        got.iter()
            .any(|e| matches!(e, DocumentEvent::BlockCountChanged(_))),
        "append must fire a BlockCountChanged to on_change, got {got:?}"
    );
    assert!(
        got.iter()
            .any(|e| matches!(e, DocumentEvent::FlowElementsInserted { .. })),
        "append must fire a FlowElementsInserted to on_change, got {got:?}"
    );
}

/// Front-truncation likewise reaches `on_change`.
#[test]
fn truncation_notifies_on_change_subscribers() {
    use std::sync::{Arc, Mutex};

    let doc = TextDocument::new();
    doc.set_plain_text("a\nb\nc\nd").unwrap();
    let seen: Arc<Mutex<Vec<DocumentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let _sub = doc.on_change(move |e| sink.lock().unwrap().push(e));

    doc.truncate_front(2).unwrap();

    let got = seen.lock().unwrap();
    assert!(
        got.iter().any(|e| matches!(
            e,
            DocumentEvent::FlowElementsRemoved {
                flow_index: 0,
                count: 2
            }
        )),
        "truncate_front must fire a FlowElementsRemoved to on_change, got {got:?}"
    );
}
