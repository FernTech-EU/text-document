//! **Where text came from, as a fact about the channel and nothing more.**
//!
//! [`InsertionOrigin`] says which route characters arrived by. It says nothing
//! about who was at the other end of that route, and no consumer can make it
//! say so: an application knows the method it was called through, and the step
//! from there to an author is not one software takes.
//!
//! ## The two things these tests hold
//!
//! **The default is `Unspecified`, not `Programmatic`.** A caller who does not
//! say has not said "the application did this" — those are different claims, and
//! collapsing them would put a wrong fact where a missing one belongs.
//!
//! **`TextInserted` carries what the insertion introduced**, not
//! `ContentsChanged`'s net delta for the region. Replacing a long selection with
//! a short paste reports a removal and an addition, and neither is "how much
//! this paste brought in" — which is exactly the figure an attribution needs.

use text_document::{DocumentEvent, InsertionOrigin, TextDocument};

fn doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc.poll_events();
    doc
}

/// The one `TextInserted` in a batch, if there is exactly one.
fn one_insertion(events: &[DocumentEvent], what: &str) -> (usize, usize, InsertionOrigin) {
    let found: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            DocumentEvent::TextInserted {
                position,
                chars_inserted,
                origin,
            } => Some((*position, *chars_inserted, *origin)),
            _ => None,
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{what} should report exactly one insertion; events were {events:?}"
    );
    found[0]
}

/// **The default is "nobody said".** Not `Programmatic`, which would be an
/// assertion about the application rather than an absence of information.
#[test]
fn a_plain_insert_reports_that_the_caller_did_not_say() {
    let doc = doc("Hello");
    doc.cursor_at(5).insert_text(" world").unwrap();

    let (position, chars, origin) = one_insertion(&doc.poll_events(), "a plain insert");
    assert_eq!(origin, InsertionOrigin::Unspecified);
    assert_ne!(
        origin,
        InsertionOrigin::Programmatic,
        "an unstated origin is missing information, not a claim about the application"
    );
    assert_eq!(position, 5);
    assert_eq!(chars, 6);
}

/// …and a caller who says is believed, for every route.
#[test]
fn every_route_reaches_the_event_under_its_own_name() {
    for origin in InsertionOrigin::ALL {
        let doc = doc("start");
        doc.cursor_at(5)
            .insert_text_with_origin(" more", origin)
            .unwrap();

        let (_, chars, reported) = one_insertion(&doc.poll_events(), origin.token());
        assert_eq!(
            reported,
            origin,
            "{} was reported as {reported:?}",
            origin.token()
        );
        assert_eq!(chars, 5);
    }
}

/// **`Assistive` is never folded into `Typed`.** Some people write that way, and
/// a report that erased the difference would be describing them as something
/// they are not.
#[test]
fn dictation_is_not_typing() {
    let doc = doc("");
    doc.cursor_at(0)
        .insert_text_with_origin("dictated", InsertionOrigin::Assistive)
        .unwrap();

    let (_, _, origin) = one_insertion(&doc.poll_events(), "a dictated insert");
    assert_eq!(origin, InsertionOrigin::Assistive);
    assert_ne!(origin, InsertionOrigin::Typed);
}

/// **The figure is what arrived, not the net delta.** This is the whole reason
/// the origin is not a field on `ContentsChanged`.
#[test]
fn the_reported_count_is_what_arrived_and_not_the_net_change() {
    let doc = doc("a long stretch of text");

    // Replace twelve characters with four: the region shrinks, and yet four
    // characters genuinely arrived.
    let cursor = doc.cursor_at(2);
    cursor.set_position(14, text_document::MoveMode::KeepAnchor);
    cursor
        .insert_text_with_origin("word", InsertionOrigin::Pasted)
        .unwrap();

    let events = doc.poll_events();
    let (_, chars_inserted, origin) = one_insertion(&events, "a replacing paste");
    assert_eq!(chars_inserted, 4, "four characters arrived");
    assert_eq!(origin, InsertionOrigin::Pasted);

    let net = events.iter().find_map(|e| match e {
        DocumentEvent::ContentsChanged {
            chars_removed,
            chars_added,
            ..
        } => Some((*chars_removed, *chars_added)),
        _ => None,
    });
    let (removed, _) = net.expect("a ContentsChanged too");
    assert!(
        removed > 0,
        "the net delta reports a removal as well, which is why it is the wrong \
         figure to attribute a channel to"
    );
}

/// An edit that only deletes has no origin to report, and must not invent one.
#[test]
fn a_deletion_reports_no_insertion() {
    let doc = doc("Hello world");
    doc.cursor_at(5).delete_char().unwrap();

    let events = doc.poll_events();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, DocumentEvent::TextInserted { .. })),
        "a deletion put a channel's name on text that never arrived: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::ContentsChanged { .. })),
        "…but it is still a content change"
    );
}

/// **Undo and redo report `Replayed`, and cannot double-count.** They never
/// re-enter the insertion API — they snapshot and diff — so text coming back is
/// reported once, as having come back rather than arrived.
#[test]
fn undo_and_redo_report_replayed() {
    let doc = doc("Hello");
    doc.cursor_at(5)
        .insert_text_with_origin(" world", InsertionOrigin::Typed)
        .unwrap();
    assert_eq!(
        one_insertion(&doc.poll_events(), "the original insert").2,
        InsertionOrigin::Typed
    );

    doc.undo().unwrap();
    assert!(
        !doc.poll_events()
            .iter()
            .any(|e| matches!(e, DocumentEvent::TextInserted { .. })),
        "an undo that only removes text reports no insertion"
    );

    doc.redo().unwrap();
    let (_, _, origin) = one_insertion(&doc.poll_events(), "the redo");
    assert_eq!(
        origin,
        InsertionOrigin::Replayed,
        "text coming back is not text arriving, and must not be counted as Typed twice"
    );
}

/// The paste-shaped routes all pass the origin on rather than losing it: they
/// delegate to `insert_fragment`, one call deep and with no ambient state.
#[test]
fn the_format_wrappers_pass_the_origin_through() {
    for (what, insert) in [
        (
            "markdown",
            Box::new(|d: &TextDocument| {
                d.cursor_at(d.character_count())
                    .insert_markdown_with_origin("some **text**", InsertionOrigin::Pasted)
            }) as Box<dyn Fn(&TextDocument) -> text_document::Result<()>>,
        ),
        (
            "html",
            Box::new(|d: &TextDocument| {
                d.cursor_at(d.character_count())
                    .insert_html_with_origin("<p>some text</p>", InsertionOrigin::Dropped)
            }),
        ),
    ] {
        let doc = doc("start");
        insert(&doc).unwrap();

        let origins: Vec<_> = doc
            .poll_events()
            .iter()
            .filter_map(|e| match e {
                DocumentEvent::TextInserted { origin, .. } => Some(*origin),
                _ => None,
            })
            .collect();
        assert!(!origins.is_empty(), "{what} reported no insertion at all");
        assert!(
            origins.iter().all(|o| *o != InsertionOrigin::Unspecified),
            "{what} lost the origin on the way through: {origins:?}"
        );
    }
}

/// The tokens are what anything persisting an origin writes down, so they are
/// spelled out rather than derived from a variant name that could be renamed.
#[test]
fn every_variant_has_a_distinct_stable_token() {
    let tokens: Vec<&str> = InsertionOrigin::ALL.iter().map(|o| o.token()).collect();
    let unique: std::collections::HashSet<&&str> = tokens.iter().collect();
    assert_eq!(
        unique.len(),
        tokens.len(),
        "two variants share a token: {tokens:?}"
    );
    assert_eq!(InsertionOrigin::default(), InsertionOrigin::Unspecified);
    assert_eq!(InsertionOrigin::Unspecified.token(), "unspecified");
    assert_eq!(InsertionOrigin::Assistive.token(), "assistive");
}
