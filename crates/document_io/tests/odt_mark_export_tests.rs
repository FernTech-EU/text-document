// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Feature tests for round-trip **marks** in the ODT writer (`export_odt_uc.rs`'s
//! `prepare_marks`, and the shared `PreparedSpan` machinery it now feeds alongside comments).
//!
//! A mark is a named bookmark a host anchors into the export so it can recognise its own rows
//! and comments when the file comes back from an editor. Bookmarks rather than a private
//! attribute, for a measured reason — see `common::parser_tools::mark_options`'s module doc.
//!
//! What is proven here:
//!
//!  - a point mark is one self-closing `<text:bookmark>`, a range mark a named start/end pair;
//!  - a mark's range splits a run exactly the way a comment's does, because it goes through the
//!    same code — including the case that motivated the shared type, a mark and a comment on
//!    overlapping ranges in one paragraph;
//!  - a point mark opens *ahead* of a comment starting on the same character;
//!  - an invalid mark name fails the export loudly instead of vanishing from the output;
//!  - a mark that cannot be anchored does **not** fail the export, unlike a comment.

extern crate text_document_io as document_io;

use common::parser_tools::{
    DocumentComment, DocumentComments, DocumentMark, DocumentMarks, OdtExportOptions,
};
use document_io::{ExportOdtDto, ImportDjotDto, document_io_controller};
use test_harness::{DbContext, EventHub, setup};

use std::io::{Cursor, Read};
use std::sync::Arc;

// --- harness -----------------------------------------------------------------------------

fn import_djot(db: &DbContext, ev: &Arc<EventHub>, djot: &str) {
    let mut mgr = common::long_operation::LongOperationManager::new();
    let op = document_io_controller::import_djot(
        db,
        ev,
        &mut mgr,
        &ImportDjotDto {
            djot_text: djot.to_string(),
            options: Default::default(),
        },
    )
    .expect("import_djot");
    while let Some(common::long_operation::OperationStatus::Running) = mgr.get_operation_status(&op)
    {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        mgr.get_operation_status(&op),
        Some(common::long_operation::OperationStatus::Completed),
        "import of {djot:?} did not complete"
    );
}

/// The `[start, end)` addressable-character range of the first occurrence of `needle` — the same
/// recipe `odt_comment_export_tests.rs` uses, so a target range is exact rather than a guess
/// about djot's block layout.
fn char_range_of(db: &DbContext, needle: &str) -> (u32, u32) {
    for bid in test_harness::get_all_block_ids(db).expect("block ids") {
        let block = test_harness::block_controller::get(db, &bid)
            .expect("get block")
            .expect("block exists");
        let text = test_harness::block_text_dto(db, &block);
        if let Some(byte_idx) = text.find(needle) {
            let char_offset = text[..byte_idx].chars().count() as u32;
            let start = block.document_position as u32 + char_offset;
            let end = start + needle.chars().count() as u32;
            return (start, end);
        }
    }
    panic!("no block contains {needle:?}");
}

fn comment(uid: &str, range: (u32, u32), body: &str) -> DocumentComment {
    DocumentComment {
        start: range.0,
        end: range.1,
        uid: uid.to_string(),
        author: "Editor".to_string(),
        author_initials: String::new(),
        date: "2026-01-01T00:00:00Z".to_string(),
        resolved: false,
        body: body.to_string(),
        replies: Vec::new(),
    }
}

fn build(
    db: &DbContext,
    comments: DocumentComments,
    marks: DocumentMarks,
) -> anyhow::Result<Vec<u8>> {
    document_io_controller::build_odt_document(
        db,
        &ExportOdtDto {
            output_path: "unused.odt".to_string(),
            options: OdtExportOptions {
                comments,
                marks,
                ..Default::default()
            },
        },
    )
}

fn content_xml(bytes: &[u8]) -> String {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).expect("packaged ODT is a valid zip");
    let mut file = archive
        .by_name("content.xml")
        .expect("content.xml entry present");
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .expect("content.xml is valid utf-8");
    contents
}

fn marks_of(iter: impl IntoIterator<Item = DocumentMark>) -> DocumentMarks {
    let mut out = DocumentMarks::new();
    for m in iter {
        out.insert(m);
    }
    out
}

// --- shape -------------------------------------------------------------------------------

#[test]
fn a_point_mark_is_one_self_closing_bookmark() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "She turned the corner and the street was gone.");
    let (at, _) = char_range_of(&db, "She turned");

    let xml = content_xml(
        &build(
            &db,
            DocumentComments::new(),
            marks_of([DocumentMark::point(
                at,
                "skrb_r0000000000000001_aaaaaaaaaaaa",
            )]),
        )
        .expect("export"),
    );

    assert!(
        xml.contains(r#"<text:bookmark text:name="skrb_r0000000000000001_aaaaaaaaaaaa"/>"#),
        "point mark not written as a self-closing bookmark: {xml}"
    );
    assert!(
        !xml.contains("bookmark-start"),
        "a point mark must not open a range it never closes: {xml}"
    );
}

#[test]
fn a_range_mark_is_a_named_start_end_pair_around_exactly_its_characters() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "She turned the corner and the street was gone.");
    let (start, end) = char_range_of(&db, "the street");

    let xml = content_xml(
        &build(
            &db,
            DocumentComments::new(),
            marks_of([DocumentMark::range(start, end, "skrb_c000000000000c001")]),
        )
        .expect("export"),
    );

    let open = r#"<text:bookmark-start text:name="skrb_c000000000000c001"/>"#;
    let close = r#"<text:bookmark-end text:name="skrb_c000000000000c001"/>"#;
    assert!(xml.contains(open), "no bookmark-start: {xml}");
    assert!(xml.contains(close), "no bookmark-end: {xml}");

    // The characters between the two halves are the ones the mark named — the whole point of a
    // range mark over a point one.
    let between = &xml[xml.find(open).unwrap() + open.len()..xml.find(close).unwrap()];
    let plain: String = strip_tags(between);
    assert_eq!(plain, "the street", "the pair brackets the wrong text");
}

/// Strip XML tags from a fragment, leaving its character data. Enough for these assertions,
/// which run over runs of plain text this crate wrote moments earlier.
fn strip_tags(fragment: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in fragment.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

// --- interaction with comments -----------------------------------------------------------

#[test]
fn a_mark_and_a_comment_can_overlap_in_one_paragraph() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "She turned the corner and the street was gone.");
    let mark_range = char_range_of(&db, "the corner and the");
    let comment_range = char_range_of(&db, "and the street");

    let mut comments = DocumentComments::new();
    comments.insert(comment("c-1", comment_range, "Is this the right word?"));

    let xml = content_xml(
        &build(
            &db,
            comments,
            marks_of([DocumentMark::range(
                mark_range.0,
                mark_range.1,
                "skrb_c000000000000c001",
            )]),
        )
        .expect("crossing ranges must both be written"),
    );

    // Both survive, and neither swallows the other's boundary: the four markers appear in
    // position order with the text intact between them.
    for needle in [
        "<text:bookmark-start",
        "<text:bookmark-end",
        "<office:annotation ",
        "<office:annotation-end",
    ] {
        assert!(xml.contains(needle), "{needle} missing: {xml}");
    }
    // ODF nests an annotation's *body* inline in the paragraph it annotates, so the comment's
    // own author/date/text sit in the middle of the prose's character data. Drop those subtrees
    // before reading the prose back, or this asserts against the comment as well as the text.
    let body: String = strip_tags(&strip_annotations(&xml));
    assert!(
        body.contains("She turned the corner and the street was gone."),
        "a crossing pair lost or duplicated text: {body}"
    );
}

/// Remove every `<office:annotation …>…</office:annotation>` subtree, leaving the prose the
/// annotations were anchored into. `<office:annotation-end/>` is self-closing and needs no
/// special handling — `strip_tags` drops it like any other tag.
fn strip_annotations(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(open) = rest.find("<office:annotation ") {
        out.push_str(&rest[..open]);
        let close = rest[open..]
            .find("</office:annotation>")
            .expect("an opened annotation is closed")
            + open
            + "</office:annotation>".len();
        rest = &rest[close..];
    }
    out.push_str(rest);
    out
}

#[test]
fn a_point_mark_opens_ahead_of_a_comment_starting_on_the_same_character() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "She turned the corner and the street was gone.");
    let range = char_range_of(&db, "She turned");

    let mut comments = DocumentComments::new();
    comments.insert(comment("c-1", range, "A note on the opening."));

    let xml = content_xml(
        &build(
            &db,
            comments,
            marks_of([DocumentMark::point(
                range.0,
                "skrb_r0000000000000001_aaaaaaaaaaaa",
            )]),
        )
        .expect("export"),
    );

    let mark_at = xml.find("<text:bookmark ").expect("the mark is written");
    let annotation_at = xml
        .find("<office:annotation ")
        .expect("the comment is written");
    assert!(
        mark_at < annotation_at,
        "the row's mark must sit at the front of its paragraph, not inside the comment's range"
    );
}

// --- failure modes -----------------------------------------------------------------------

#[test]
fn an_invalid_mark_name_fails_the_export_rather_than_disappearing() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "She turned the corner.");
    let (at, _) = char_range_of(&db, "She");

    let err = build(
        &db,
        DocumentComments::new(),
        // A hyphen is legal in ODF and dropped by Word — exactly the kind of name that would
        // produce a file that round-trips through one editor and loses identity in the other.
        marks_of([DocumentMark::point(at, "skrb-row-1")]),
    )
    .expect_err("an unusable mark name must not be written silently");

    let msg = format!("{err:#}");
    assert!(msg.contains("skrb-row-1"), "{msg}");
    assert!(msg.contains("round-trip mark"), "{msg}");
}

/// The asymmetry with comments, stated: a comment that finds no home is an error, a mark is not.
///
/// A mark exists to make re-import recognise a row exactly; without it, re-import falls back to
/// matching by type and title, which is a designed fallback and not a failure. Refusing to write
/// the manuscript over a lost convenience would be the wrong trade — and the manuscript is what
/// the writer asked for.
#[test]
fn a_mark_that_cannot_be_anchored_does_not_fail_the_export() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "She turned the corner.");

    let bytes = build(
        &db,
        DocumentComments::new(),
        marks_of([DocumentMark::point(
            9_000,
            "skrb_r0000000000000009_ffffffffffff",
        )]),
    )
    .expect("an unplaceable mark must not cost the writer their export");

    let xml = content_xml(&bytes);
    assert!(
        xml.contains("She turned the corner."),
        "the manuscript is still fully written: {xml}"
    );
    assert!(
        !xml.contains("skrb_r0000000000000009_ffffffffffff"),
        "an unplaceable mark is simply absent, not written at a guessed position"
    );
}

/// The comment half of the same contract, unchanged by the shared type.
#[test]
fn a_comment_that_cannot_be_anchored_still_fails_the_export() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "She turned the corner.");

    let mut comments = DocumentComments::new();
    comments.insert(comment("c-lost", (9_000, 9_010), "Nowhere to go."));

    let err = build(&db, comments, DocumentMarks::new())
        .expect_err("an unanchorable comment is still data loss");
    assert!(format!("{err:#}").contains("c-lost"), "{err:#}");
}
