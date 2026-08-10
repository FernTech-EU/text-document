// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Feature tests for M-T1 — DOCX comment-range export
//! (`export_docx_uc.rs`'s comment machinery: `PreparedComment`, `CommentEmitState`,
//! `add_inline_content`'s run splitter, and `patch_comment_extras`'s raw-XML patch).
//!
//! Four things are proven here, matching the milestone's own requirements:
//!
//!  - [`patch_writes_uid_initials_and_resolved_flag`] — the three fields unreachable through
//!    `docx-rs`'s public builder API (`w15:done`, `w:initials`, the `skrb:uid` attribute) land
//!    in the packed XML, correctly keyed per comment.
//!  - [`comment_boundary_splits_a_run_mid_hyperlink`] — a boundary landing inside a hyperlink's
//!    own anchor text splits it correctly, with the marker inside the `<w:hyperlink>`, not
//!    merely around it.
//!  - [`two_comments_overlap_in_one_block`] — two threads with crossing (non-nested) ranges in
//!    the same paragraph interleave correctly and neither range drops a character.
//!  - [`resolved_comment_thread_round_trips_through_libreoffice`] — a real `.docx`, packed
//!    through the exact production path (`execute()`'s `build()` → `patch_comment_extras` →
//!    `pack()`), opens in LibreOffice and survives a conversion to ODF with its comment
//!    threads (author, reply, body text) intact — proof the file is valid to a real reader,
//!    not merely well-formed XML this crate wrote and re-parsed itself.

extern crate text_document_io as document_io;

use common::parser_tools::{CommentReply, DocumentComment, DocumentComments, DocxExportOptions};
use document_io::docx_rs::{DocumentChild, Docx, Hyperlink, Paragraph, ParagraphChild, RunChild};
use document_io::{ExportDocxDto, ImportDjotDto, document_io_controller};
use test_harness::{DbContext, EventHub, setup};

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

/// The `[start, end)` addressable-character range of the first occurrence of `needle`,
/// computed the same way `export_docx_uc.rs`'s own `render_block` does — `document_position`
/// (block-relative char offset base) plus the char offset of `needle` inside that block's own
/// text — so a test target is exact, never a guess about djot's block layout.
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

fn comment(
    uid: &str,
    author: &str,
    initials: &str,
    range: (u32, u32),
    body: &str,
) -> DocumentComment {
    DocumentComment {
        start: range.0,
        end: range.1,
        uid: uid.to_string(),
        author: author.to_string(),
        author_initials: initials.to_string(),
        date: "2026-01-01T00:00:00Z".to_string(),
        resolved: false,
        body: body.to_string(),
        replies: Vec::new(),
    }
}

fn build_docx(db: &DbContext, comments: DocumentComments) -> Docx {
    document_io_controller::build_docx_document(
        db,
        &ExportDocxDto {
            output_path: "unused.docx".to_string(),
            options: DocxExportOptions {
                comments,
                ..Default::default()
            },
        },
    )
    .expect("build_docx_document")
}

fn paragraphs(docx: &Docx) -> Vec<&Paragraph> {
    docx.document
        .children
        .iter()
        .filter_map(|c| match c {
            DocumentChild::Paragraph(p) => Some(&**p),
            _ => None,
        })
        .collect()
}

/// One [`ParagraphChild`] flattened to a symbol easy to assert a sequence over: `S<id>`/`E<id>`
/// for a comment range marker, or the run's own text otherwise.
#[derive(Debug, PartialEq, Eq)]
enum Sym {
    Text(String),
    Start(usize),
    End(usize),
}

fn comment_end_id(end: &document_io::docx_rs::CommentRangeEnd) -> usize {
    // `CommentRangeEnd::id` is a private field in docx-rs — it derives `Serialize`, so read it
    // back through JSON the same way `docx_export_tests.rs`'s `space_before`/
    // `page_break_before` helpers already do for other docx-rs internals this crate doesn't
    // expose a getter for.
    serde_json::to_value(end)
        .ok()
        .and_then(|v| v.get("id").and_then(|i| i.as_u64()))
        .expect("CommentRangeEnd serializes its id") as usize
}

fn symbols(children: &[ParagraphChild]) -> Vec<Sym> {
    children
        .iter()
        .filter_map(|c| match c {
            ParagraphChild::Run(run) => {
                let mut text = String::new();
                for rc in &run.children {
                    if let RunChild::Text(t) = rc {
                        text.push_str(&t.text);
                    }
                }
                (!text.is_empty()).then_some(Sym::Text(text))
            }
            ParagraphChild::CommentStart(start) => Some(Sym::Start(start.id)),
            ParagraphChild::CommentEnd(end) => Some(Sym::End(comment_end_id(end))),
            _ => None,
        })
        .collect()
}

fn first_hyperlink(p: &Paragraph) -> &Hyperlink {
    p.children
        .iter()
        .find_map(|c| match c {
            ParagraphChild::Hyperlink(h) => Some(h),
            _ => None,
        })
        .expect("paragraph has a hyperlink")
}

fn plain_text(symbols: &[Sym]) -> String {
    symbols
        .iter()
        .filter_map(|s| match s {
            Sym::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
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
        "EO",
        range,
        "Is this the right link text?",
    ));

    let docx = build_docx(&db, comments);
    let p = paragraphs(&docx)
        .into_iter()
        .find(|p| {
            p.children
                .iter()
                .any(|c| matches!(c, ParagraphChild::Hyperlink(_)))
        })
        .expect("a paragraph with a hyperlink");
    let link = first_hyperlink(p);
    let syms = symbols(&link.children);

    // The full anchor text survives the split with nothing lost or duplicated.
    assert_eq!(plain_text(&syms), "website link here");

    // The marker pair sits INSIDE the hyperlink's own children (not merely around the whole
    // `<w:hyperlink>` at the paragraph level — `p.children` proves that separately below),
    // wrapping exactly the run(s) reconstructing "link".
    let start_idx = syms
        .iter()
        .position(|s| matches!(s, Sym::Start(_)))
        .expect("a CommentStart inside the hyperlink");
    let end_idx = syms
        .iter()
        .position(|s| matches!(s, Sym::End(_)))
        .expect("a CommentEnd inside the hyperlink");
    assert!(
        start_idx < end_idx,
        "CommentStart must precede CommentEnd: {syms:?}"
    );
    let inner_text: String = syms[start_idx + 1..end_idx]
        .iter()
        .filter_map(|s| match s {
            Sym::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(inner_text, "link");

    // And the paragraph itself carries no top-level CommentStart/End of its own — the whole
    // marker pair is inside the hyperlink, exactly where the boundary actually falls.
    let top_level = symbols(&p.children);
    assert!(
        !top_level
            .iter()
            .any(|s| matches!(s, Sym::Start(_) | Sym::End(_))),
        "the boundary is inside the hyperlink's own text, not at the paragraph level: {top_level:?}"
    );
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
    comments.insert(comment("cmt-a", "Author A", "AA", range1, "First note."));
    comments.insert(comment("cmt-b", "Author B", "BB", range2, "Second note."));

    let docx = build_docx(&db, comments);
    let p = paragraphs(&docx)
        .into_iter()
        .find(|p| {
            p.children
                .iter()
                .any(|c| matches!(c, ParagraphChild::Run(_)))
        })
        .expect("the prose paragraph");
    let syms = symbols(&p.children);

    // No character lost or duplicated by the split.
    assert_eq!(plain_text(&syms), "Alpha beta gamma delta epsilon.");

    // Exactly two Start and two End markers, one pair per thread, and the crossing order
    // holds: Start(1), Start(2), End(1), End(2).
    let starts: Vec<usize> = syms
        .iter()
        .filter_map(|s| match s {
            Sym::Start(id) => Some(*id),
            _ => None,
        })
        .collect();
    let ends: Vec<usize> = syms
        .iter()
        .filter_map(|s| match s {
            Sym::End(id) => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 2, "{syms:?}");
    assert_eq!(ends.len(), 2, "{syms:?}");
    assert_eq!(
        starts[0], ends[0],
        "the thread that opened first must be the one whose End comes first: {syms:?}"
    );
    assert_ne!(starts[0], starts[1], "two distinct threads");

    // The marker order in the flattened symbol stream is exactly Start, Start, End, End —
    // the interleaving a crossing (non-nested) pair of ranges produces.
    let marker_order: Vec<&Sym> = syms
        .iter()
        .filter(|s| matches!(s, Sym::Start(_) | Sym::End(_)))
        .collect();
    assert!(
        matches!(
            marker_order.as_slice(),
            [Sym::Start(_), Sym::Start(_), Sym::End(_), Sym::End(_)]
        ),
        "expected Start, Start, End, End, got {marker_order:?}"
    );
}

// --- raw-XML patch: w15:done / w:initials / uid -----------------------------------------

/// The attribute value of `attr` inside the first XML tag containing `marker` — a small,
/// deliberately independent string scan (not the same regex `patch_comment_extras` itself
/// uses) so this test is a genuine black-box check of the packed bytes, not a restatement of
/// the production code it is testing.
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

#[test]
fn patch_writes_uid_initials_and_resolved_flag() {
    let (db, ev, _) = setup().expect("setup");
    import_djot(&db, &ev, "The quick brown fox jumps over the lazy dog.");

    let quick_range = char_range_of(&db, "quick brown");
    let lazy_range = char_range_of(&db, "lazy dog");

    let mut resolved_comment = comment(
        "cmt-quick-1",
        "Alice Editor",
        "AE",
        quick_range,
        "Please reconsider this phrase.",
    );
    resolved_comment.resolved = true;
    let unresolved_comment = comment(
        "cmt-lazy-2",
        "Bob Reviewer",
        "BR",
        lazy_range,
        "Nice ending.",
    );

    let mut comments = DocumentComments::new();
    comments.insert(resolved_comment);
    comments.insert(unresolved_comment);

    let xml_docx = document_io_controller::build_docx_xml_document(
        &db,
        &ExportDocxDto {
            output_path: "unused.docx".to_string(),
            options: DocxExportOptions {
                comments,
                ..Default::default()
            },
        },
    )
    .expect("build_docx_xml_document");

    let comments_xml = String::from_utf8(xml_docx.comments.clone()).expect("comments.xml is UTF-8");
    let comments_extended_xml = String::from_utf8(xml_docx.comments_extended.clone())
        .expect("commentsExtended.xml is UTF-8");

    // The private namespace is declared on the root, or `skrb:uid` below would be a namespace
    // error rather than valid extension data.
    assert!(
        comments_xml.contains(r#"xmlns:skrb="urn:ferntech:text-document:comment:1""#),
        "comments.xml: {comments_xml}"
    );

    // w:initials + skrb:uid, keyed by author (a stand-in for "the right w:comment tag" — the
    // production patch itself keys by `w:id`, but this test locates the tag independently).
    assert_eq!(
        attr_after(&comments_xml, r#"w:author="Alice Editor""#, "w:initials"),
        "AE"
    );
    assert_eq!(
        attr_after(&comments_xml, r#"w:author="Alice Editor""#, "skrb:uid"),
        "cmt-quick-1"
    );
    assert_eq!(
        attr_after(&comments_xml, r#"w:author="Bob Reviewer""#, "w:initials"),
        "BR"
    );
    assert_eq!(
        attr_after(&comments_xml, r#"w:author="Bob Reviewer""#, "skrb:uid"),
        "cmt-lazy-2"
    );

    // w15:done, keyed by the body paragraph's *actual* w14:paraId — read back the same way
    // `patch_comment_extras` has to (see its doc comment), never assumed.
    let alice_para_id = {
        let alice_at = comments_xml.find(r#"w:author="Alice Editor""#).unwrap();
        let marker = "w14:paraId=\"";
        let at = comments_xml[alice_at..].find(marker).unwrap() + alice_at + marker.len();
        comments_xml[at..at + 8].to_string()
    };
    let bob_para_id = {
        let bob_at = comments_xml.find(r#"w:author="Bob Reviewer""#).unwrap();
        let marker = "w14:paraId=\"";
        let at = comments_xml[bob_at..].find(marker).unwrap() + bob_at + marker.len();
        comments_xml[at..at + 8].to_string()
    };
    assert_ne!(alice_para_id, bob_para_id);

    assert_eq!(
        attr_after(
            &comments_extended_xml,
            &format!(r#"w15:paraId="{alice_para_id}""#),
            "w15:done"
        ),
        "1",
        "Alice's thread is resolved: {comments_extended_xml}"
    );
    assert_eq!(
        attr_after(
            &comments_extended_xml,
            &format!(r#"w15:paraId="{bob_para_id}""#),
            "w15:done"
        ),
        "0",
        "Bob's thread is not resolved: {comments_extended_xml}"
    );
}

// --- golden fixture: a real .docx opens in LibreOffice with its comments intact --------

/// `Some(path)` if `soffice` is on `PATH`, else `None` — this test needs a real LibreOffice
/// binary to prove the packed file is valid to an actual reader, not just well-formed XML this
/// crate wrote and re-parsed with its own eyes. Skips (rather than failing) when unavailable,
/// so `cargo test --workspace` stays green on a machine without LibreOffice installed; this
/// repo's own dev environment has `soffice` (LibreOffice 25.8), so the check runs for real here.
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

#[test]
fn resolved_comment_thread_round_trips_through_libreoffice() {
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

    let range = char_range_of(&db, "needs review");
    let mut root = comment(
        "cmt-root-1",
        "Alice Editor",
        "AE",
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

    // Pack a real .docx through the exact production path this crate ships
    // (`build_docx_xml_document` = `build_docx` -> `Docx::build()` -> `patch_comment_extras` ->
    // the caller packs it, same as `execute()` does before writing to disk).
    let xml_docx = document_io_controller::build_docx_xml_document(
        &db,
        &ExportDocxDto {
            output_path: "unused.docx".to_string(),
            options: DocxExportOptions {
                comments,
                ..Default::default()
            },
        },
    )
    .expect("build_docx_xml_document");

    let dir = std::env::temp_dir().join(format!("docx_comment_export_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let docx_path = dir.join("comment_fixture.docx");
    let file = std::fs::File::create(&docx_path).expect("create docx file");
    xml_docx.pack(file).expect("pack docx");

    // Sandbox LibreOffice's own profile per invocation (`-env:UserInstallation`) so a
    // concurrently-running soffice process (this crate's own test binary runs its tests in
    // parallel by default, and other integration-test binaries could too) never contends over
    // the same profile lock — the standard fix for driving `soffice --headless` from tests.
    let profile_dir = dir.join("lo_profile");
    let output = std::process::Command::new("soffice")
        .args([
            "--headless",
            "--norestore",
            &format!("-env:UserInstallation=file://{}", profile_dir.display()),
            "--convert-to",
            "odt",
            "--outdir",
        ])
        .arg(&dir)
        .arg(&docx_path)
        .output()
        .expect("run soffice");
    assert!(
        output.status.success(),
        "soffice --convert-to odt failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let odt_path = dir.join("comment_fixture.odt");
    assert!(
        odt_path.exists(),
        "soffice reported success but did not write {odt_path:?} — stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    // LibreOffice successfully opening and re-saving the file as ODF is itself the proof the
    // `.docx` is valid to a real reader, not just well-formed XML. Beyond that: its own
    // ODF comment representation (`<office:annotation>`) should carry both authors and both
    // bodies — proof the anchor, the author identity, and the reply thread all survived a
    // real, independent parser, not just this crate's own writer/reader.
    let odt_bytes = std::fs::read(&odt_path).expect("read converted odt");
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(odt_bytes)).expect("odt is a valid zip");
    let mut content_xml = String::new();
    std::io::Read::read_to_string(
        &mut archive.by_name("content.xml").expect("content.xml present"),
        &mut content_xml,
    )
    .expect("content.xml is valid utf-8");

    assert!(
        content_xml.contains("office:annotation"),
        "no annotation found in LibreOffice's own ODF conversion"
    );
    assert!(
        content_xml.contains("Alice Editor"),
        "root comment's author did not survive the round trip"
    );
    assert!(
        content_xml.contains("Please tighten this phrase"),
        "root comment's body did not survive the round trip"
    );
    assert!(
        content_xml.contains("Bob Writer"),
        "reply's author did not survive the round trip"
    );
    assert!(
        content_xml.contains("Good catch, will fix"),
        "reply's body did not survive the round trip"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
