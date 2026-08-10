// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Feature tests for round-trip **marks** in the DOCX writer (`export_docx_uc.rs`'s
//! `prepare_marks` and the `SpanEmit::Mark` half of the shared `PreparedSpan` machinery).
//!
//! The OOXML twin of `odt_mark_export_tests.rs`, proving the same contract through a very
//! different mechanism: ODF spells a bookmark's name on both halves of a pair and offers a
//! self-closing form for a point, while OOXML names only the start, closes by numeric id, and
//! has no self-closing spelling at all — a point mark is a start immediately followed by its
//! end. Those differences are exactly where a second implementation of one idea goes wrong, so
//! each is asserted here rather than assumed to mirror the ODF side.
//!
//! The last test packs a real `.docx` and runs it through LibreOffice, which is the claim that
//! actually matters: not "this crate can re-read what it wrote", but "the identity survives an
//! editor". `document_ingest`'s `roundtrip_marks.rs` pins the same fact from a fixture; this
//! pins it from *this writer's* output.

extern crate text_document_io as document_io;

use common::parser_tools::{
    DocumentComment, DocumentComments, DocumentMark, DocumentMarks, DocxExportOptions,
};
use document_io::{ExportDocxDto, ImportDjotDto, document_io_controller};
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
        author_initials: "E".to_string(),
        date: "2026-01-01T00:00:00Z".to_string(),
        resolved: false,
        body: body.to_string(),
        replies: Vec::new(),
    }
}

fn marks_of(iter: impl IntoIterator<Item = DocumentMark>) -> DocumentMarks {
    let mut out = DocumentMarks::new();
    for m in iter {
        out.insert(m);
    }
    out
}

/// Pack a real `.docx` through the production path and return its bytes.
fn build(
    db: &DbContext,
    comments: DocumentComments,
    marks: DocumentMarks,
) -> anyhow::Result<Vec<u8>> {
    let xml_docx = document_io_controller::build_docx_xml_document(
        db,
        &ExportDocxDto {
            output_path: "unused.docx".to_string(),
            options: DocxExportOptions {
                comments,
                marks,
                ..Default::default()
            },
        },
    )?;
    let mut bytes = Vec::new();
    xml_docx.pack(Cursor::new(&mut bytes))?;
    Ok(bytes)
}

fn part(bytes: &[u8], name: &str) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("packed docx is a valid zip");
    let mut file = archive
        .by_name(name)
        .unwrap_or_else(|e| panic!("{name} missing: {e}"));
    let mut out = String::new();
    file.read_to_string(&mut out).expect("part is valid utf-8");
    out
}

/// The `w:id` of the `w:bookmarkStart` carrying `name`.
fn bookmark_id(document_xml: &str, name: &str) -> String {
    let needle = format!(r#"w:name="{name}""#);
    let at = document_xml
        .find(&needle)
        .unwrap_or_else(|| panic!("no bookmark named {name} in {document_xml}"));
    let tag_start = document_xml[..at].rfind('<').expect("name is inside a tag");
    let tag = &document_xml[tag_start..at];
    let id_at = tag
        .find(r#"w:id=""#)
        .expect("a bookmarkStart carries an id")
        + r#"w:id=""#.len();
    let id_end = tag[id_at..].find('"').expect("id is closed") + id_at;
    tag[id_at..id_end].to_string()
}

// --- shape -------------------------------------------------------------------------------

#[test]
fn a_point_mark_is_a_start_immediately_followed_by_its_end() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "She turned the corner and the street was gone.");
    let (at, _) = char_range_of(&db, "She turned");

    let xml = part(
        &build(
            &db,
            DocumentComments::new(),
            marks_of([DocumentMark::point(
                at,
                "skrb_r0000000000000001_aaaaaaaaaaaa",
            )]),
        )
        .expect("export"),
        "word/document.xml",
    );

    let id = bookmark_id(&xml, "skrb_r0000000000000001_aaaaaaaaaaaa");
    let start =
        format!(r#"<w:bookmarkStart w:id="{id}" w:name="skrb_r0000000000000001_aaaaaaaaaaaa" />"#);
    let end = format!(r#"<w:bookmarkEnd w:id="{id}" />"#);
    let start_at = xml
        .find(&start)
        .unwrap_or_else(|| panic!("bookmarkStart not found in {xml}"));
    let end_at = xml
        .find(&end)
        .unwrap_or_else(|| panic!("bookmarkEnd not found in {xml}"));

    assert!(
        end_at > start_at,
        "a zero-length bookmark closes after it opens"
    );
    // Nothing at all between them: OOXML has no self-closing bookmark, so "point" means the two
    // halves are adjacent. Anything in between would mean the mark had accidentally acquired an
    // extent, and a reader resolving it would report characters the host never marked.
    assert_eq!(
        &xml[start_at + start.len()..end_at],
        "",
        "a point mark must not span any content"
    );
}

#[test]
fn a_range_mark_brackets_exactly_its_characters() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "She turned the corner and the street was gone.");
    let (start, end) = char_range_of(&db, "the street");

    let xml = part(
        &build(
            &db,
            DocumentComments::new(),
            marks_of([DocumentMark::range(start, end, "skrb_c000000000000c001")]),
        )
        .expect("export"),
        "word/document.xml",
    );

    let id = bookmark_id(&xml, "skrb_c000000000000c001");
    let open_at = xml.find(r#"w:name="skrb_c000000000000c001""#).unwrap();
    let close = format!(r#"<w:bookmarkEnd w:id="{id}" />"#);
    let close_at = xml.find(&close).expect("the range closes");
    assert!(close_at > open_at, "the pair is ordered");

    let between = &xml[open_at..close_at];
    assert!(
        between.contains("the street"),
        "the pair does not bracket its own text: {between}"
    );
    assert!(
        !between.contains("She turned"),
        "the pair reaches back over text it never named: {between}"
    );
}

/// OOXML names a bookmark only on its start — this is what forces a reader to keep an id table,
/// and it is the difference from ODF most likely to be papered over by a copy-paste.
#[test]
fn only_the_start_carries_the_name() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "She turned the corner and the street was gone.");
    let (start, end) = char_range_of(&db, "the street");

    let xml = part(
        &build(
            &db,
            DocumentComments::new(),
            marks_of([DocumentMark::range(start, end, "skrb_c000000000000c001")]),
        )
        .expect("export"),
        "word/document.xml",
    );

    assert_eq!(
        xml.matches(r#"w:name="skrb_c000000000000c001""#).count(),
        1,
        "the name appears once, on the start"
    );
}

// --- interaction with comments -----------------------------------------------------------

#[test]
fn a_point_mark_opens_ahead_of_a_comment_starting_on_the_same_character() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "She turned the corner and the street was gone.");
    let range = char_range_of(&db, "She turned");

    let mut comments = DocumentComments::new();
    comments.insert(comment("c-1", range, "A note on the opening."));

    let xml = part(
        &build(
            &db,
            comments,
            marks_of([DocumentMark::point(
                range.0,
                "skrb_r0000000000000001_aaaaaaaaaaaa",
            )]),
        )
        .expect("export"),
        "word/document.xml",
    );

    let mark_at = xml.find("<w:bookmarkStart").expect("the mark is written");
    let comment_at = xml
        .find("<w:commentRangeStart")
        .expect("the comment is written");
    assert!(
        mark_at < comment_at,
        "the row's mark must sit at the front of its paragraph, not inside the comment's range"
    );
}

/// Marks share the prepared list with comments, and `word/comments.xml` is patched by counting
/// what is in that list. Miscount it and the patch step refuses the whole export — which is how
/// this was found the first time.
#[test]
fn marks_do_not_disturb_the_comments_part() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "She turned the corner and the street was gone.");
    let comment_range = char_range_of(&db, "the street");
    let (mark_at, _) = char_range_of(&db, "She turned");

    let mut comments = DocumentComments::new();
    comments.insert(comment("c-1", comment_range, "Is this the right word?"));

    let bytes = build(
        &db,
        comments,
        marks_of([
            DocumentMark::point(mark_at, "skrb_r0000000000000001_aaaaaaaaaaaa"),
            DocumentMark::range(comment_range.0, comment_range.1, "skrb_c000000000000c001"),
        ]),
    )
    .expect("marks alongside comments must not upset the comments patch");

    let comments_xml = part(&bytes, "word/comments.xml");
    assert_eq!(
        comments_xml.matches("<w:comment ").count(),
        1,
        "exactly the one comment, no marks leaking in: {comments_xml}"
    );
    assert!(
        comments_xml.contains(r#"skrb:uid="c-1""#),
        "the comment still carries its uid: {comments_xml}"
    );
    assert!(
        !comments_xml.contains("skrb_r"),
        "a mark must not appear in the comments part: {comments_xml}"
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
        marks_of([DocumentMark::point(at, "skrb-row-1")]),
    )
    .expect_err("an unusable mark name must not be written silently");

    let msg = format!("{err:#}");
    assert!(msg.contains("skrb-row-1"), "{msg}");
    assert!(msg.contains("round-trip mark"), "{msg}");
}

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

    let xml = part(&bytes, "word/document.xml");
    assert!(
        xml.contains("She turned the corner."),
        "the manuscript is still fully written"
    );
    assert!(
        !xml.contains("skrb_r0000000000000009_ffffffffffff"),
        "an unplaceable mark is simply absent, not written at a guessed position"
    );
}

// --- the claim that matters: survival ----------------------------------------------------

fn soffice_path() -> Option<std::path::PathBuf> {
    std::process::Command::new("which")
        .arg("soffice")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
}

/// Both mark kinds, written by *this* writer, survive being opened and saved by a real editor.
///
/// This is the whole reason marks are bookmarks. The same file's `skrb:uid` on `<w:comment>` —
/// the carrier this writer used before — does not survive, and that asymmetry is asserted here
/// too, because a future change that "simplifies" identity back onto the attribute would
/// otherwise pass every other test in this crate.
#[test]
fn both_mark_kinds_survive_a_real_editor_saving_the_file() {
    let Some(_soffice) = soffice_path() else {
        eprintln!("skipping: soffice not found on PATH");
        return;
    };

    let (db, ev, _) = setup().expect("setup");
    import_djot(
        &db,
        &ev,
        "This manuscript opens with a sentence that needs review.\n\n\
         A second, unrelated paragraph follows.",
    );
    let comment_range = char_range_of(&db, "needs review");
    let (row_at, _) = char_range_of(&db, "This manuscript");

    let mut comments = DocumentComments::new();
    comments.insert(comment("c-1", comment_range, "Please tighten this phrase."));

    let bytes = build(
        &db,
        comments,
        marks_of([
            DocumentMark::point(row_at, "skrb_r0000000000000001_aaaaaaaaaaaa"),
            DocumentMark::range(comment_range.0, comment_range.1, "skrb_c000000000000c001"),
        ]),
    )
    .expect("export");

    let dir = std::env::temp_dir().join(format!("docx_mark_export_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let docx_path = dir.join("marks.docx");
    std::fs::write(&docx_path, &bytes).expect("write docx");

    // Sandboxed profile per invocation, so a concurrently-running soffice never contends over
    // the same profile lock — the same precaution `docx_comment_export_tests.rs` takes.
    let profile_dir = dir.join("lo_profile");
    let output = std::process::Command::new("soffice")
        .args([
            "--headless",
            "--norestore",
            &format!("-env:UserInstallation=file://{}", profile_dir.display()),
            "--convert-to",
            "docx:MS Word 2007 XML",
            "--outdir",
        ])
        .arg(dir.join("out"))
        .arg(&docx_path)
        .output()
        .expect("run soffice");
    assert!(
        output.status.success(),
        "soffice round trip failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let returned = std::fs::read(dir.join("out").join("marks.docx")).expect("read returned docx");
    let document_xml = part(&returned, "word/document.xml");

    for name in [
        "skrb_r0000000000000001_aaaaaaaaaaaa",
        "skrb_c000000000000c001",
    ] {
        assert!(
            document_xml.contains(name),
            "{name} did not survive a real editor's save"
        );
    }

    let comments_xml = part(&returned, "word/comments.xml");
    assert!(
        !comments_xml.contains("skrb:uid"),
        "the private attribute survived — if this ever becomes true, revisit whether marks are \
         still the only usable identity carrier: {comments_xml}"
    );
}
