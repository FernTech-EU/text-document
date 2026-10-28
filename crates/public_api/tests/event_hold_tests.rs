// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Holding the document's events: what a host editing under an input method's lock relies on.
//!
//! What has to hold:
//!
//! * while a hold is alive, no callback runs and `poll_events` returns nothing, whoever
//!   edits; the edits, the cursors they move and every read are current;
//! * holds nest, and the last release delivers every event held, in order, none merged;
//! * an edit a callback makes during that delivery is delivered after what was held;
//! * transient edits, their revert and commit, undo and redo are held like any edit, and a
//!   callback that writes the document cannot end a composition under the hold;
//! * a subscription made during the hold gets only later events, one dropped gets nothing
//!   more, and a panicking callback leaves the document delivering as before.

use std::sync::{Arc, Mutex};

use text_document::{DocumentEvent, EventHold, Subscription, TextDocument};

fn doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc.poll_events();
    doc
}

fn text(doc: &TextDocument) -> String {
    doc.to_plain_text().unwrap()
}

/// Every event the callbacks receive, in order.
fn record(doc: &TextDocument) -> (Arc<Mutex<Vec<DocumentEvent>>>, Subscription) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let subscription = doc.on_change(move |event| sink.lock().unwrap().push(event));
    (seen, subscription)
}

fn taken(seen: &Mutex<Vec<DocumentEvent>>) -> Vec<DocumentEvent> {
    std::mem::take(&mut *seen.lock().unwrap())
}

/// The `chars_added` of each `ContentsChanged`, in order: which edit each one was.
fn added(events: &[DocumentEvent]) -> Vec<usize> {
    events
        .iter()
        .filter_map(|event| match event {
            DocumentEvent::ContentsChanged { chars_added, .. } => Some(*chars_added),
            _ => None,
        })
        .collect()
}

/// Some typing, a deletion, an undo and a redo, the same on every document it is run on.
fn edits(doc: &TextDocument) {
    let cursor = doc.cursor_at(5);
    cursor.insert_text("a").unwrap();
    cursor.insert_text("b").unwrap();
    cursor.delete_previous_char().unwrap();
    doc.undo().unwrap();
    doc.redo().unwrap();
}

#[test]
fn nothing_goes_out_during_a_hold_and_everything_after_it_in_order() {
    let unheld = doc("Hello world");
    let (expected, _sub) = record(&unheld);
    edits(&unheld);
    let expected = taken(&expected);
    let expected_polled = unheld.poll_events();
    assert!(!expected.is_empty());

    let held = doc("Hello world");
    let (seen, _sub) = record(&held);
    let hold = held.hold_events();
    edits(&held);
    assert!(taken(&seen).is_empty(), "a callback ran during the hold");
    assert!(
        held.poll_events().is_empty(),
        "poll_events returned events during the hold"
    );
    drop(hold);
    // Each event as it was queued: two keystrokes are two events, none merged.
    assert_eq!(taken(&seen), expected);
    assert_eq!(held.poll_events(), expected_polled);
    assert_eq!(text(&held), text(&unheld));
}

#[test]
fn reads_and_cursors_are_current_during_a_hold() {
    let doc = doc("Hello world");
    let (seen, _sub) = record(&doc);
    let other = doc.cursor_at(6);
    let revision = doc.content_revision();
    let hold = doc.hold_events();

    doc.cursor_at(0).insert_text(">> ").unwrap();
    assert_eq!(text(&doc), ">> Hello world");
    assert_eq!(
        other.position(),
        9,
        "a cursor after the edit did not move with it"
    );
    assert_eq!(doc.content_revision(), revision + 1);
    assert!(doc.can_undo());

    doc.undo().unwrap();
    assert_eq!(text(&doc), "Hello world");
    assert_eq!(other.position(), 6, "the undo did not move the cursor back");
    assert!(doc.can_redo());

    assert!(taken(&seen).is_empty(), "a callback ran during the hold");
    drop(hold);
    assert!(!taken(&seen).is_empty());
}

#[test]
fn holds_nest_and_only_the_last_release_delivers() {
    let doc = doc("Hello");
    let (seen, _sub) = record(&doc);
    let outer = doc.hold_events();
    let inner = doc.clone().hold_events();
    doc.cursor_at(5).insert_text("!").unwrap();
    drop(inner);
    assert!(
        taken(&seen).is_empty(),
        "an inner release delivered the events"
    );
    assert!(doc.poll_events().is_empty());
    doc.cursor_at(0).insert_text("?").unwrap();
    drop(outer);
    assert_eq!(added(&taken(&seen)), vec![1, 1]);
}

#[test]
fn an_edit_a_callback_makes_during_the_release_goes_out_after_the_held_events() {
    let doc = doc("Hello");
    let (seen, _sub) = record(&doc);
    let writer = doc.clone();
    let wrote = Arc::new(Mutex::new(false));
    let once = Arc::clone(&wrote);
    let _editing = doc.on_change(move |event| {
        if matches!(event, DocumentEvent::ContentsChanged { .. }) {
            let mut wrote = once.lock().unwrap();
            if !*wrote {
                *wrote = true;
                drop(wrote);
                writer.cursor_at(0).insert_text("XYZ").unwrap();
            }
        }
    });

    let hold = doc.hold_events();
    doc.cursor_at(5).insert_text("a").unwrap();
    doc.cursor_at(6).insert_text("bb").unwrap();
    drop(hold);

    assert_eq!(text(&doc), "XYZHelloabb");
    assert_eq!(
        added(&taken(&seen)),
        vec![1, 2, 3],
        "the callback's edit was delivered in the middle of the held events"
    );
}

#[test]
fn a_callback_that_writes_the_document_cannot_end_a_composition_under_the_hold() {
    let doc = doc("Hello");
    let writer = doc.clone();
    // An application callback that writes the document on every change of its content.
    let _app = doc.on_change(move |event| {
        if let DocumentEvent::ContentsChanged { chars_added, .. } = event
            && chars_added > 0
            && writer.transient_range().is_some()
        {
            writer.cursor_at(0).insert_text("#").unwrap();
        }
    });
    let (seen, _sub) = record(&doc);
    let cursor = doc.cursor_at(5);

    let hold = doc.hold_events();
    cursor.insert_text_transient("ka").unwrap();
    assert_eq!(doc.transient_range(), Some(5..7));
    let range = doc.transient_range().unwrap();
    cursor.set_position(range.start, text_document::MoveMode::MoveAnchor);
    cursor.set_position(range.end, text_document::MoveMode::KeepAnchor);
    cursor.insert_text_transient("か").unwrap();
    assert_eq!(
        doc.transient_range(),
        Some(5..6),
        "the composition was ended under the hold"
    );
    assert_eq!(text(&doc), "Helloか");
    assert!(taken(&seen).is_empty());
    drop(hold);

    // Delivered after the lock, the callback's write ends the composition there.
    assert_eq!(doc.transient_range(), None);
    assert_eq!(text(&doc), "#Hello");
    assert!(!taken(&seen).is_empty());
}

#[test]
fn transient_edits_their_revert_and_their_commit_are_held_in_order() {
    fn compose_then(doc: &TextDocument, commit: bool) {
        let cursor = doc.cursor_at(5);
        cursor.insert_text_transient("ka").unwrap();
        if commit {
            cursor.commit_transient_text("か").unwrap();
        } else {
            assert!(doc.revert_transient_edits());
        }
    }
    for commit in [false, true] {
        let unheld = doc("Hello");
        let (expected, _sub) = record(&unheld);
        compose_then(&unheld, commit);
        let expected = taken(&expected);

        let held = doc("Hello");
        let (seen, _sub) = record(&held);
        let hold = held.hold_events();
        compose_then(&held, commit);
        assert_eq!(text(&held), text(&unheld));
        assert!(taken(&seen).is_empty());
        drop(hold);
        assert_eq!(taken(&seen), expected, "commit: {commit}");
    }
}

#[test]
fn a_subscription_made_during_the_hold_gets_only_what_is_queued_after_it() {
    let doc = doc("Hello");
    let hold = doc.hold_events();
    doc.cursor_at(5).insert_text("a").unwrap();
    let (seen, _sub) = record(&doc);
    doc.cursor_at(6).insert_text("bb").unwrap();
    drop(hold);
    assert_eq!(added(&taken(&seen)), vec![2]);
}

#[test]
fn a_subscription_dropped_during_the_release_gets_nothing_more() {
    let doc = doc("Hello");
    let later: Arc<Mutex<Option<Subscription>>> = Arc::new(Mutex::new(None));
    let to_drop = Arc::clone(&later);
    // Called first: drops the other subscription at the first event.
    let _first = doc.on_change(move |_| {
        to_drop.lock().unwrap().take();
    });
    let (seen, subscription) = record(&doc);
    *later.lock().unwrap() = Some(subscription);

    let hold = doc.hold_events();
    doc.cursor_at(5).insert_text("a").unwrap();
    doc.cursor_at(6).insert_text("bb").unwrap();
    drop(hold);
    assert!(
        taken(&seen).is_empty(),
        "a dropped subscription received events"
    );
}

#[test]
fn a_hold_a_callback_keeps_stops_the_release_and_its_own_release_delivers_the_rest() {
    let doc = doc("Hello");
    let kept: Arc<Mutex<Option<EventHold>>> = Arc::new(Mutex::new(None));
    let keeper = Arc::clone(&kept);
    let writer = doc.clone();
    let _holding = doc.on_change(move |_| {
        let mut kept = keeper.lock().unwrap();
        if kept.is_none() {
            // Holds again, then edits under its own hold.
            *kept = Some(writer.hold_events());
            drop(kept);
            writer.cursor_at(0).insert_text("XYZ").unwrap();
        }
    });
    let (seen, _sub) = record(&doc);

    let hold = doc.hold_events();
    doc.cursor_at(5).insert_text("a").unwrap();
    doc.cursor_at(6).insert_text("bb").unwrap();
    drop(hold);
    // The events taken before the callback held again go out; its edit waits.
    assert_eq!(added(&taken(&seen)), vec![1, 2]);
    assert_eq!(text(&doc), "XYZHelloabb");
    let kept = kept.lock().unwrap().take();
    drop(kept);
    assert_eq!(added(&taken(&seen)), vec![3]);
}

#[test]
fn a_panicking_callback_leaves_the_document_delivering() {
    let doc = doc("Hello");
    let panicked = Arc::new(Mutex::new(false));
    let once = Arc::clone(&panicked);
    let _panicky = doc.on_change(move |_| {
        let mut done = once.lock().unwrap();
        if !*done {
            *done = true;
            drop(done);
            panic!("a callback panics");
        }
    });
    let (seen, _sub) = record(&doc);

    let hold = doc.hold_events();
    doc.cursor_at(5).insert_text("a").unwrap();
    let released = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(hold)));
    assert!(released.is_err(), "the callback's panic was swallowed");
    taken(&seen);

    doc.cursor_at(0).insert_text("bb").unwrap();
    assert_eq!(
        added(&taken(&seen)),
        vec![2],
        "the document stopped delivering"
    );
}

#[test]
fn a_hold_dropped_by_a_panic_releases_the_events() {
    let doc = doc("Hello");
    let (seen, _sub) = record(&doc);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _hold = doc.hold_events();
        doc.cursor_at(5).insert_text("a").unwrap();
        panic!("the host panics under its lock");
    }));
    assert!(unwound.is_err());
    assert_eq!(added(&taken(&seen)), vec![1]);
    doc.cursor_at(0).insert_text("bb").unwrap();
    assert_eq!(added(&taken(&seen)), vec![2]);
}

/// A callback that panics while a panic drops the hold is a second panic on an unwinding
/// thread. The release catches it, so it does not abort the process, and the panic that
/// was already going on is the one the host gets back.
#[test]
fn a_callback_panicking_while_a_panic_drops_the_hold_does_not_abort() {
    let doc = doc("Hello");
    let _panicky = doc.on_change(|_| panic!("a callback panics"));
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _hold = doc.hold_events();
        doc.cursor_at(5).insert_text("a").unwrap();
        panic!("the host panics under its lock");
    }));
    let payload = unwound.expect_err("the host's panic");
    assert_eq!(
        payload.downcast_ref::<&str>(),
        Some(&"the host panics under its lock")
    );
}

#[test]
fn a_hold_keeps_back_the_edits_of_another_thread() {
    fn send_and_sync<T: Send + Sync>() {}
    send_and_sync::<EventHold>();

    let doc = doc("Hello");
    let (seen, _sub) = record(&doc);
    let hold = doc.hold_events();
    let other = doc.clone();
    std::thread::spawn(move || other.cursor_at(5).insert_text("a").unwrap())
        .join()
        .unwrap();
    assert!(
        taken(&seen).is_empty(),
        "another thread's edit was delivered"
    );
    // Released on a thread of its own.
    std::thread::spawn(move || drop(hold)).join().unwrap();
    assert_eq!(added(&taken(&seen)), vec![1]);
}
