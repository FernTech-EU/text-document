// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! A tab, two spaces in a row and a space at a line's edge survive HTML out and back in.
//!
//! An HTML reader collapses whitespace: a tab is a space to it, and a run of spaces is
//! one. The writers put such text out bare, so `A\tb` came back `A b`, and the HTML of a
//! text read back was not the HTML it had been read from
//! (`fuzz_robustness_tests::html_roundtrip_stabilises`, on `<oL\t>A\tà`). The writers now
//! keep it in a `<span style="white-space: pre-wrap">`, which browsers, Qt and this crate's
//! reader keep as it is, and put the rest out as before.

use text_document::{MoveMode, MoveOperation, TextDocument};

fn imported(html: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_html(html).unwrap().wait().unwrap();
    doc
}

fn plain(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

/// The whole document's HTML as the clipboard gets it.
fn selection_html(doc: &TextDocument) -> String {
    let cursor = doc.cursor_at(0);
    cursor.move_position(MoveOperation::End, MoveMode::KeepAnchor, 1);
    cursor.selection().to_html()
}

/// The seeds `html_roundtrip_stabilises` failed on: text in a list outside any item,
/// which the reader kept raw, a tab or a newline in it, where it collapses a paragraph's
/// whitespace as a browser does. Read so, it gave the model a newline inside a paragraph.
#[test]
fn the_fuzz_seeds_round_trip() {
    for (seed, text) in [
        ("<oL\t>A\tà", "A à"),
        ("<oL\t>a\t!", "a !"),
        ("<oL\t>,\n!", ", !"),
    ] {
        let doc = imported(seed);
        assert_eq!(doc.to_plain_text().unwrap(), text, "seed {seed:?}");
        let first = selection_html(&doc);
        let second = selection_html(&imported(&first));
        assert_eq!(first, second, "seed {seed:?}");
    }
}

const KEPT: [&str; 6] = [
    "A\tb",
    "two  spaces",
    " leading",
    "trailing ",
    "\tindented\n\tlines",
    "tab\tin the first\nand  in the second",
];

#[test]
fn whitespace_survives_the_documents_html() {
    for text in KEPT {
        let html = plain(text).to_html().unwrap();
        assert_eq!(
            imported(&html).to_plain_text().unwrap(),
            text,
            "through {html}"
        );
    }
}

#[test]
fn whitespace_survives_the_clipboards_html() {
    for text in KEPT {
        let html = selection_html(&plain(text));
        let target = TextDocument::new();
        target.cursor_at(0).insert_html(&html).unwrap();
        assert_eq!(target.to_plain_text().unwrap(), text, "through {html}");
    }
}

#[test]
fn formatted_runs_and_a_line_break_read_back_the_same() {
    let doc = imported("<p><b>bold\t</b><i> after</i></p><p>a <br> b</p>");
    // `<br> b` reads as a line starting with `b`, as a browser shows it.
    let text = doc.to_plain_text().unwrap();
    let html = doc.to_html().unwrap();
    assert_eq!(imported(&html).to_plain_text().unwrap(), text, "{html}");
    let clip = selection_html(&doc);
    assert_eq!(selection_html(&imported(&clip)), clip);
}

#[test]
fn text_a_reader_keeps_as_it_is_goes_out_bare() {
    let doc = plain("Hello world, one space at a time.");
    assert!(!doc.to_html().unwrap().contains("white-space"));
    assert!(!selection_html(&doc).contains("white-space"));
}

#[test]
fn the_reader_keeps_whitespace_an_element_keeps() {
    let kept = imported("<p>a <span style=\"white-space: pre-wrap\">\t two  </span> b</p>");
    assert_eq!(kept.to_plain_text().unwrap(), "a \t two   b");
    let collapsed = imported("<p>a <span>\t two  </span> b</p>");
    assert_eq!(collapsed.to_plain_text().unwrap(), "a two b");
    // The last declaration wins, as in CSS.
    let normal =
        imported("<p><span style=\"white-space: pre; white-space: normal\">a  b</span></p>");
    assert_eq!(normal.to_plain_text().unwrap(), "a b");
}

/// A space after an image or a footnote reference goes out bare, and the reader keeps
/// it: a replaced element is content, and the space after it is not at a line's edge.
#[test]
fn a_space_after_an_image_or_a_footnote_reference_survives() {
    for html in [
        r#"<p>See <img src="a.png" alt="a"> below</p>"#,
        r#"<p>See <img src="a.png" alt="a"><b> below</b></p>"#,
    ] {
        let doc = imported(html);
        let text = doc.to_addressable_text().unwrap();
        assert_eq!(text, "See \u{FFFC} below", "{html}");
        for out in [doc.to_html().unwrap(), selection_html(&doc)] {
            assert_eq!(
                imported(&out).to_addressable_text().unwrap(),
                text,
                "{html} through {out}"
            );
        }
    }
    let doc = TextDocument::new();
    doc.set_djot("See[^n] below\n\n[^n]: A note.\n")
        .unwrap()
        .wait()
        .unwrap();
    // The reader reads a reference back as the number it shows, so only the space is
    // compared: it is still there, after the reference.
    for out in [doc.to_html().unwrap(), selection_html(&doc)] {
        let back = imported(&out).to_plain_text().unwrap();
        let below = back.find("below").expect(&out);
        assert_eq!(&back[below - 1..below], " ", "{out} read back as {back:?}");
        assert!(!back.starts_with("See "), "{back:?}");
    }
}
