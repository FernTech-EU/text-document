// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Feature tests for M-T2b — ODT comment-range export (`export_odt_uc.rs`'s comment machinery:
//! `prepare_comments`, `CommentEmitState`, `add_inline_content`'s run splitter, and
//! `annotation_open_xml`/`render_comment_body_odt`).
//!
//! Assertions are on the raw `content.xml` string, the same way `odt_export_tests.rs` asserts on
//! every other M-T2a feature — there is no typed ODF builder to inspect instead (see
//! `crate::odt_render`'s module doc for why). Six things are proven here, matching the
//! milestone's own requirements:
//!
//!  - [`annotation_carries_author_date_resolved_and_uid`] — the fields DOCX needs a raw-XML
//!    patch to reach at all (`skrb:uid`, a resolved flag) are just ordinary attributes here,
//!    written directly by `annotation_open_xml`, plus the standard `dc:creator`/`dc:date` pair.
//!  - [`unresolved_thread_writes_loext_resolved_false_explicitly`] — the false case is spelled
//!    out too, not merely left absent.
//!  - [`a_reply_threads_via_loext_parent_name_naming_the_roots_own_office_name`] — a reply is a
//!    sibling annotation whose `loext:parent-name` names the thread root's own `office:name`.
//!  - [`comment_boundary_splits_a_run_mid_hyperlink`] — a boundary landing inside a hyperlink's
//!    own anchor text splits it correctly, with the annotation pair inside `<text:a>`, not
//!    merely around it.
//!  - [`two_comments_overlap_in_one_block`] — two threads with crossing (non-nested) ranges in
//!    the same paragraph interleave correctly and neither range drops a character.
//!  - [`a_multi_paragraph_body_becomes_real_multiple_text_p_elements`] — the one DOCX constraint
//!    (collapsing every body to one paragraph) that does not carry over to ODF.
//!  - [`a_comment_that_never_intersects_any_block_is_a_loud_error_not_a_silent_drop`] —
//!    `ensure_all_anchored`'s contract.
//!  - [`rich_document_with_comments_packs_to_a_valid_odt_file_soffice_can_convert`] — a real
//!    `.odt`, packed through the exact production path (`export_odt`), opens in LibreOffice and
//!    survives a conversion to PDF with comments present — proof the file is valid to a real
//!    reader, not merely well-formed XML this crate wrote and re-parsed itself.

extern crate text_document_io as document_io;

use common::parser_tools::{CommentReply, DocumentComment, DocumentComments, OdtExportOptions};
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

/// The `[start, end)` addressable-character range of the first occurrence of `needle` — the
/// identical recipe `docx_comment_export_tests.rs::char_range_of` uses, so a test target is
/// exact, never a guess about djot's block layout.
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

fn comment(uid: &str, author: &str, range: (u32, u32), body: &str) -> DocumentComment {
    DocumentComment {
        start: range.0,
        end: range.1,
        uid: uid.to_string(),
        author: author.to_string(),
        author_initials: String::new(),
        date: "2026-01-01T00:00:00Z".to_string(),
        resolved: false,
        body: body.to_string(),
        replies: Vec::new(),
    }
}

fn build_odt(db: &DbContext, comments: DocumentComments) -> Vec<u8> {
    document_io_controller::build_odt_document(
        db,
        &ExportOdtDto {
            output_path: "unused.odt".to_string(),
            options: OdtExportOptions {
                comments,
                ..Default::default()
            },
        },
    )
    .expect("build_odt_document")
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

/// The attribute value of `attr` inside the first XML tag containing `marker` — a small,
/// deliberately independent string scan (mirrors
/// `docx_comment_export_tests.rs::attr_after`), so this is a genuine black-box check of the
/// produced XML, not a restatement of the production code that wrote it.
fn attr_after(xml: &str, marker: &str, attr: &str) -> String {
    let marker_at = xml
        .find(marker)
        .unwrap_or_else(|| panic!("{marker:?} not found in {xml}"));
    let tag_start = xml[..marker_at].rfind('<').expect("marker is inside a tag");
    let tag_end = xml[tag_start..].find('>').expect("tag is closed") + tag_start;
    let tag = &xml[tag_start..tag_end];
    let needle = format!(r#"{attr}=""#);
    let value_start = tag
        .find(&needle)
        .unwrap_or_else(|| panic!("{attr} not found on the tag containing {marker:?}: {tag}"))
        + needle.len();
    let value_end = tag[value_start..]
        .find('"')
        .expect("attribute value is closed")
        + value_start;
    tag[value_start..value_end].to_string()
}

// --- author / date / resolved / uid -----------------------------------------------------

#[test]
fn annotation_carries_author_date_resolved_and_uid() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "The quick brown fox jumps over the lazy dog.");

    let range = char_range_of(&db, "quick brown");
    let mut c = comment(
        "cmt-quick-1",
        "Alice Editor",
        range,
        "Please reconsider this phrase.",
    );
    c.resolved = true;
    c.date = "2026-03-04T09:30:00Z".to_string();

    let mut comments = DocumentComments::new();
    comments.insert(c);

    let content = content_xml(&build_odt(&db, comments));

    assert!(
        content.contains("<office:annotation "),
        "no annotation found: {content}"
    );
    assert_eq!(
        attr_after(&content, "office:annotation ", "loext:resolved"),
        "true"
    );
    assert_eq!(
        attr_after(&content, "office:annotation ", "skrb:uid"),
        "cmt-quick-1"
    );
    assert!(
        content.contains("<dc:creator>Alice Editor</dc:creator>"),
        "author missing: {content}"
    );
    assert!(
        content.contains("<dc:date>2026-03-04T09:30:00Z</dc:date>"),
        "date missing: {content}"
    );
    assert!(
        content.contains("Please reconsider this phrase."),
        "body missing: {content}"
    );

    // The opening tag's `office:name` must be matched by a real `office:annotation-end` of the
    // same name — the pairing key `document_ingest::sources::odt`'s reader keys on.
    let name = attr_after(&content, "office:annotation ", "office:name");
    assert!(
        content.contains(&format!("<office:annotation-end office:name=\"{name}\"/>")),
        "no matching annotation-end for {name:?}: {content}"
    );
}

#[test]
fn unresolved_thread_writes_loext_resolved_false_explicitly() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "Nice ending to this sentence.");
    let range = char_range_of(&db, "Nice ending");
    let mut comments = DocumentComments::new();
    comments.insert(comment("cmt-1", "Bob Reviewer", range, "Nice ending."));

    let content = content_xml(&build_odt(&db, comments));
    assert_eq!(
        attr_after(&content, "office:annotation ", "loext:resolved"),
        "false",
        "an unresolved thread must spell out loext:resolved=\"false\", not merely omit it: {content}"
    );
}

// --- threading -----------------------------------------------------------------------------

#[test]
fn a_reply_threads_via_loext_parent_name_naming_the_roots_own_office_name() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(
        &db,
        &ev,
        "This manuscript opens with a sentence that needs review.",
    );

    let range = char_range_of(&db, "needs review");
    let mut root = comment(
        "cmt-root-1",
        "Alice Editor",
        range,
        "Please tighten this phrase.",
    );
    root.replies.push(CommentReply {
        uid: "cmt-reply-1".to_string(),
        author: "Bob Writer".to_string(),
        author_initials: "BW".to_string(),
        date: "2026-01-02T00:00:00Z".to_string(),
        body: "Good catch, will fix in the next pass.".to_string(),
    });
    let mut comments = DocumentComments::new();
    comments.insert(root);

    let content = content_xml(&build_odt(&db, comments));

    // Two distinct annotations: root ("Alice Editor") and reply ("Bob Writer").
    assert!(content.contains("Alice Editor"));
    assert!(content.contains("Bob Writer"));
    assert!(content.contains("Please tighten this phrase."));
    assert!(content.contains("Good catch, will fix in the next pass."));

    // Locate the annotation tag whose body contains "Alice Editor" and read its own
    // `office:name` directly.
    let alice_annotation_start = content.find("Alice Editor").unwrap();
    let alice_tag_start = content[..alice_annotation_start]
        .rfind("<office:annotation ")
        .expect("an <office:annotation> precedes Alice Editor's dc:creator");
    let alice_tag_end = content[alice_tag_start..].find('>').unwrap() + alice_tag_start;
    let alice_tag = &content[alice_tag_start..alice_tag_end];
    let alice_name = {
        let needle = "office:name=\"";
        let start = alice_tag.find(needle).unwrap() + needle.len();
        let end = alice_tag[start..].find('"').unwrap() + start;
        alice_tag[start..end].to_string()
    };

    let bob_annotation_start = content.find("Bob Writer").unwrap();
    let bob_tag_start = content[..bob_annotation_start]
        .rfind("<office:annotation ")
        .expect("an <office:annotation> precedes Bob Writer's dc:creator");
    let bob_tag_end = content[bob_tag_start..].find('>').unwrap() + bob_tag_start;
    let bob_tag = &content[bob_tag_start..bob_tag_end];

    assert!(
        bob_tag.contains(&format!("loext:parent-name=\"{alice_name}\"")),
        "the reply's own annotation tag must carry loext:parent-name naming the root's \
         office:name ({alice_name:?}): {bob_tag}"
    );
    // The root's own tag must NOT carry a parent-name — it IS the thread root.
    assert!(
        !alice_tag.contains("loext:parent-name"),
        "the root comment must not carry loext:parent-name: {alice_tag}"
    );
}

// --- mid-hyperlink boundary -----------------------------------------------------------

#[test]
fn comment_boundary_splits_a_run_mid_hyperlink() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(
        &db,
        &ev,
        "Visit our [website link here](https://example.com) for more.",
    );

    // Anchors to "link" — strictly inside the hyperlink's own anchor text
    // ("website link here"), not at either edge of it.
    let range = char_range_of(&db, "link");
    let mut comments = DocumentComments::new();
    comments.insert(comment(
        "cmt-hyperlink-1",
        "Editor One",
        range,
        "Is this the right link text?",
    ));

    let content = content_xml(&build_odt(&db, comments));

    let link_start = content
        .find("<text:a ")
        .expect("a <text:a> hyperlink must exist");
    let link_end = content[link_start..]
        .find("</text:a>")
        .map(|i| i + link_start)
        .expect("the hyperlink must close");
    let link_inner = &content[link_start..link_end];

    assert!(
        link_inner.contains("<office:annotation "),
        "the comment must open INSIDE the hyperlink's own text, not merely around it: {link_inner}"
    );
    assert!(
        link_inner.contains("<office:annotation-end "),
        "the comment must also close INSIDE the hyperlink: {link_inner}"
    );

    // The full anchor text survives the split with nothing lost or duplicated. Search only the
    // hyperlink's own BODY (after its opening tag), never the tag itself — `<text:a ...
    // xlink:href="...">`'s attributes contain the substring "link" (inside "xlink"), which would
    // otherwise give a false match ahead of the real word.
    let body_start = link_inner
        .find('>')
        .map(|i| i + 1)
        .expect("text:a has a body");
    let body = &link_inner[body_start..];
    let website_at = body.find("website").expect("website");
    let link_at = body
        .find(">link<")
        .map(|i| i + 1)
        .expect("the split-off \"link\" word");
    let here_at = body.rfind("here").expect("here");
    assert!(website_at < link_at, "{body}");
    assert!(link_at < here_at, "{body}");

    // Nothing about the comment's own range should have been anchored OUTSIDE the hyperlink —
    // the whole document only has one paragraph, so if the annotation pair also appeared at the
    // top level (outside `<text:a>`) `ensure_all_anchored` would have complained about a comment
    // anchored twice; since it did not error, and both markers are inside the link, this is the
    // single true placement.
}

// --- overlapping comments ---------------------------------------------------------------

#[test]
fn two_comments_overlap_in_one_block() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "Alpha beta gamma delta epsilon.");

    // "beta gamma" and "gamma delta" cross without nesting: comment 1 opens first and closes
    // first, comment 2 opens second (inside comment 1's still-open range) and closes second
    // (after comment 1 has already closed).
    let range1 = char_range_of(&db, "beta gamma");
    let range2 = char_range_of(&db, "gamma delta");
    assert!(
        range1.0 < range2.0 && range1.1 < range2.1,
        "ranges must cross, not nest"
    );

    let mut comments = DocumentComments::new();
    comments.insert(comment("cmt-a", "Author A", range1, "First note."));
    comments.insert(comment("cmt-b", "Author B", range2, "Second note."));

    let content = content_xml(&build_odt(&db, comments));

    for word in ["Alpha", "beta", "gamma", "delta", "epsilon"] {
        assert!(content.contains(word), "{word} missing: {content}");
    }

    // Exactly two annotation starts and two ends.
    assert_eq!(
        content.matches("<office:annotation ").count(),
        2,
        "expected exactly two annotation starts: {content}"
    );
    assert_eq!(
        content.matches("<office:annotation-end ").count(),
        2,
        "expected exactly two annotation ends: {content}"
    );

    // The marker order in the body is exactly Start, Start, End, End — the interleaving a
    // crossing (non-nested) pair of ranges produces (mirrors
    // `docx_comment_export_tests::two_comments_overlap_in_one_block`).
    let mut positions: Vec<(usize, bool)> = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel) = content[search_from..].find("<office:annotation") {
        let at = search_from + rel;
        let is_end = content[at..].starts_with("<office:annotation-end");
        positions.push((at, is_end));
        search_from = at + 1;
    }
    positions.sort_by_key(|&(at, _)| at);
    let order: Vec<bool> = positions.into_iter().map(|(_, is_end)| is_end).collect();
    assert_eq!(
        order,
        vec![false, false, true, true],
        "expected Start, Start, End, End (false = start, true = end): {content}"
    );
}

// --- rich, multi-paragraph bodies ---------------------------------------------------------

#[test]
fn a_multi_paragraph_body_becomes_real_multiple_text_p_elements() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "One sentence to comment on.");
    let range = char_range_of(&db, "One sentence");

    let body = "First *paragraph* of the remark.\n\nSecond paragraph, **bold** this time.";
    let mut comments = DocumentComments::new();
    comments.insert(comment("cmt-rich-1", "Editor", range, body));

    let content = content_xml(&build_odt(&db, comments));

    let ann_start = content
        .find("<office:annotation ")
        .expect("annotation exists");
    let ann_end = content[ann_start..]
        .find("</office:annotation>")
        .map(|i| i + ann_start)
        .expect("annotation closes");
    let ann_body = &content[ann_start..ann_end];

    assert_eq!(
        ann_body.matches("<text:p>").count(),
        2,
        "a two-Djot-paragraph body must become two real <text:p> elements, not one paragraph \
         full of <text:line-break/>: {ann_body}"
    );
    assert!(ann_body.contains("First "));
    assert!(ann_body.contains("paragraph"));
    assert!(ann_body.contains("of the remark."));
    assert!(ann_body.contains("Second paragraph"));
    assert!(
        ann_body.contains("<text:span text:style-name=") && ann_body.contains("bold"),
        "bold formatting inside the comment body must survive as a styled span: {ann_body}"
    );
}

// --- ensure_all_anchored --------------------------------------------------------------------

#[test]
fn a_comment_that_never_intersects_any_block_is_a_loud_error_not_a_silent_drop() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "Short document.");

    // Far past the end of the document's addressable text — no block can ever claim it.
    let mut comments = DocumentComments::new();
    comments.insert(comment(
        "cmt-orphan-1",
        "Nobody",
        (9_000, 9_010),
        "This can never be anchored.",
    ));

    let err = document_io_controller::build_odt_document(
        &db,
        &ExportOdtDto {
            output_path: "unused.odt".to_string(),
            options: OdtExportOptions {
                comments,
                ..Default::default()
            },
        },
    )
    .expect_err("an unanchored comment must fail the export, not silently vanish");

    let message = err.to_string();
    assert!(
        message.contains("cmt-orphan-1"),
        "the error must name the orphaned comment's uid: {message}"
    );
}

// --- golden fixture: a real .odt opens in LibreOffice with its comments intact --------

fn soffice_path() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("soffice");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[test]
fn rich_document_with_comments_packs_to_a_valid_odt_file_soffice_can_convert() {
    let Some(soffice) = soffice_path() else {
        eprintln!("soffice not found on PATH; skipping LibreOffice validation");
        return;
    };

    let (db, ev, _) = setup().expect("setup");
    import_djot(
        &db,
        &ev,
        "This manuscript opens with a sentence that needs review.\n\n\
         A second, unrelated paragraph follows.",
    );

    let range = char_range_of(&db, "needs review");
    let mut root = comment(
        "cmt-root-1",
        "Alice Editor",
        range,
        "Please tighten this phrase.",
    );
    root.resolved = true;
    root.replies.push(CommentReply {
        uid: "cmt-reply-1".to_string(),
        author: "Bob Writer".to_string(),
        author_initials: "BW".to_string(),
        date: "2026-01-02T00:00:00Z".to_string(),
        body: "Good catch, will fix in the next pass.".to_string(),
    });
    let mut comments = DocumentComments::new();
    comments.insert(root);

    let dir = std::env::temp_dir().join(format!("odt_comment_export_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let odt_path = dir.join("comments.odt");
    let profile_dir = dir.join("lo_profile");

    let mut mgr = common::long_operation::LongOperationManager::new();
    let op = document_io_controller::export_odt(
        &db,
        &ev,
        &mut mgr,
        &ExportOdtDto {
            output_path: odt_path.to_string_lossy().to_string(),
            options: OdtExportOptions {
                comments,
                ..Default::default()
            },
        },
    )
    .expect("export_odt");
    while let Some(common::long_operation::OperationStatus::Running) = mgr.get_operation_status(&op)
    {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        mgr.get_operation_status(&op),
        Some(common::long_operation::OperationStatus::Completed),
        "export should complete"
    );
    assert!(odt_path.exists(), "the .odt file must exist on disk");

    let output = std::process::Command::new(&soffice)
        .arg("--headless")
        .arg("--norestore")
        .arg(format!(
            "-env:UserInstallation=file://{}",
            profile_dir.display()
        ))
        .arg("--convert-to")
        .arg("pdf")
        .arg("--outdir")
        .arg(&dir)
        .arg(&odt_path)
        .output()
        .expect("failed to run soffice");

    assert!(
        output.status.success(),
        "soffice --convert-to pdf failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let pdf_path = dir.join("comments.pdf");
    let pdf_bytes = std::fs::read(&pdf_path).unwrap_or_else(|e| {
        panic!(
            "soffice reported success but produced no PDF at {pdf_path:?}: {e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert!(
        pdf_bytes.starts_with(b"%PDF-"),
        "LibreOffice's own output must be a real PDF"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
