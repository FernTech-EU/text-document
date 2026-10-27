// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! A line break inside a block: the one Shift+Enter makes in a word processor, held in the
//! block's text as one [`LINE_SEPARATOR`].
//!
//! What has to hold, from the cursor to every format:
//!
//! * it is inserted as one character and leaves one block;
//! * it is saved as a Djot hard break and read back as the same character, in every
//!   container a block can be in, with the marks around it, whatever the next line opens
//!   with, and at the block's two ends;
//! * every reader makes one of every hard break it meets, and every writer puts it back
//!   as its own format's line break.

use common::parser_tools::djot_depth::{MAX_LEAF_LINES, longest_leaf_lines};
use text_document::{
    DjotImportOptions, LINE_SEPARATOR, MoveMode, MoveOperation, PlainTextExportOptions,
    TextDocument, djot_to_plain_text,
};

const LS: &str = "\u{2028}";

fn djot(src: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_sync(src).unwrap();
    doc
}

fn texts(doc: &TextDocument) -> Vec<String> {
    doc.blocks().iter().map(|block| block.text()).collect()
}

/// The Djot `doc` saves as, the document that reopens from it, and whether a second save
/// writes the first again.
fn save_reopen(doc: &TextDocument) -> (String, TextDocument, bool) {
    let saved = doc.to_djot().unwrap();
    let reopened = djot(&saved);
    let resaved = reopened.to_djot().unwrap();
    (saved.clone(), reopened, resaved == saved)
}

/// `doc`'s first block, ended by `text` written with its line breaks: each `¶` in it is
/// one, inserted through the cursor.
fn type_at_end_of_first_block(doc: &TextDocument, text: &str) {
    let block = doc.blocks()[0].clone();
    let cursor = doc.cursor_at(block.position() + block.text().chars().count());
    for (i, piece) in text.split('¶').enumerate() {
        if i > 0 {
            cursor.insert_line_break().unwrap();
        }
        if !piece.is_empty() {
            cursor.insert_text(piece).unwrap();
        }
    }
}

#[test]
fn the_constant_is_the_line_separator() {
    assert_eq!(LINE_SEPARATOR.to_string(), LS);
}

// ── The cursor ──────────────────────────────────────────────────

#[test]
fn a_line_break_is_one_character_and_leaves_one_block() {
    let doc = djot("Roses are red,");
    type_at_end_of_first_block(&doc, "¶violets are blue.");
    assert_eq!(
        texts(&doc),
        [format!("Roses are red,{LS}violets are blue.")]
    );
    assert_eq!(doc.character_count(), "Roses are red,".chars().count() + 18);
}

#[test]
fn a_line_break_replaces_the_selection() {
    let doc = djot("one two three");
    let cursor = doc.cursor_at(3);
    cursor.set_position(8, MoveMode::KeepAnchor);
    cursor.insert_line_break().unwrap();
    assert_eq!(texts(&doc), [format!("one{LS}three")]);
}

#[test]
fn undo_takes_a_line_break_back() {
    let doc = djot("one");
    type_at_end_of_first_block(&doc, "¶");
    assert_eq!(texts(&doc), [format!("one{LS}")]);
    doc.undo().unwrap();
    assert_eq!(texts(&doc), ["one"]);
}

// ── Saved and reopened ──────────────────────────────────────────

/// Typed in every container a block can be in, a line break is saved as a hard break and
/// reopens as the same one block, with the same Djot at every later save.
#[test]
fn a_line_break_survives_a_save_in_every_container() {
    for container in [
        "",
        "> ",
        "> > ",
        "- ",
        "3. ",
        "- [ ] ",
        "(iv) ",
        "## ",
        "> - ",
        "- a\n\n  - ",
    ] {
        let doc = djot(&format!("{container}first"));
        let last = doc.blocks().len() - 1;
        let block = doc.blocks()[last].clone();
        let cursor = doc.cursor_at(block.position() + block.text().chars().count());
        cursor.insert_line_break().unwrap();
        cursor.insert_text("second").unwrap();

        let (saved, reopened, stable) = save_reopen(&doc);
        assert_eq!(
            texts(&reopened),
            texts(&doc),
            "{container:?} saved as {saved:?}"
        );
        assert_eq!(
            texts(&reopened).last().unwrap(),
            &format!("first{LS}second"),
            "{container:?} saved as {saved:?}"
        );
        assert!(saved.contains("\\\n"), "{container:?} saved as {saved:?}");
        assert!(stable, "{container:?}: {saved:?} does not save stably");
        // The container is kept: the reopened document saves to the same Djot.
        assert_eq!(
            reopened.to_djot().unwrap(),
            saved,
            "{container:?} lost its container"
        );
    }
}

/// A mark open over a line break is closed before it and opened again after it, so the
/// parser never carries an opener from one line to the next.
#[test]
fn marks_over_a_line_break_are_closed_and_opened_around_it() {
    let doc = djot("*ab* _cd_");
    doc.cursor_at(1).insert_line_break().unwrap();
    let (saved, reopened, stable) = save_reopen(&doc);
    assert_eq!(saved, "*a*\\\n*b* _cd_");
    assert_eq!(texts(&reopened), [format!("a{LS}b cd")]);
    assert!(stable);
}

/// After a code span's closing fence the parser is still reading verbatim, so the `\` of
/// the break escaped nothing and the break came back as a space.
#[test]
fn a_code_span_ending_a_line_keeps_the_break_after_it() {
    let doc = djot("`code`");
    type_at_end_of_first_block(&doc, "¶next");
    let (saved, reopened, stable) = save_reopen(&doc);
    assert_eq!(
        texts(&reopened),
        [format!("code{LS}next")],
        "saved as {saved:?}"
    );
    assert!(stable, "{saved:?}");
}

/// A line after a break opens no block, whatever it starts with: it is more of the
/// paragraph, and its markup is text.
#[test]
fn a_line_opening_with_markup_stays_text() {
    for line in [
        "- x",
        "# x",
        "> x",
        "1. x",
        "::: x",
        "{.x}",
        "```",
        "| a |",
        "* * *",
        "[^1]: x",
        "    indented",
        " x",
    ] {
        for container in ["", "> ", "- ", "# "] {
            let doc = djot(&format!("{container}first"));
            type_at_end_of_first_block(&doc, &format!("¶{line}"));
            let (saved, reopened, stable) = save_reopen(&doc);
            assert_eq!(
                texts(&reopened),
                [format!("first{LS}{line}")],
                "{container:?} {line:?} saved as {saved:?}"
            );
            assert!(stable, "{container:?} {line:?}: {saved:?}");
        }
    }
}

/// A line break at a block's start, at its end and twice in a row: an empty line. A hard
/// break at the very end of a block reads back as the text `\`, so the last empty line is
/// written as an empty attribute set.
#[test]
fn empty_lines_at_either_end_and_in_between_survive() {
    for text in ["¶a", "a¶", "a¶¶b", "¶", "a¶¶", "¶¶"] {
        for container in ["", "> ", "- ", "## "] {
            let doc = djot(&format!("{container}x"));
            let block = doc.blocks()[0].clone();
            // Replace the `x` with the text.
            let cursor = doc.cursor_at(block.position());
            cursor.set_position(block.position() + 1, MoveMode::KeepAnchor);
            cursor.remove_selected_text().unwrap();
            type_at_end_of_first_block(&doc, text);
            let expected = text.replace('¶', LS);
            assert_eq!(texts(&doc), std::slice::from_ref(&expected));
            let (saved, reopened, stable) = save_reopen(&doc);
            assert_eq!(
                texts(&reopened),
                [expected],
                "{container:?} {text:?} saved as {saved:?}"
            );
            assert!(stable, "{container:?} {text:?}: {saved:?}");
        }
    }
}

/// One paragraph costs the parser a call a line, and one longer than it is given is read
/// back as its source, every `\` showing. Such a block is saved as blocks of at most that
/// many lines, and keeps every line.
#[test]
fn a_block_of_more_lines_than_the_parser_takes_is_saved_in_pieces() {
    let lines: Vec<String> = (0..MAX_LEAF_LINES + 5).map(|i| format!("l{i}")).collect();
    let doc = djot(&lines[0]);
    let rest = format!("{LS}{}", lines[1..].join(LS));
    doc.cursor_at(lines[0].chars().count())
        .insert_text(&rest)
        .unwrap();
    assert_eq!(doc.block_count(), 1);
    let saved = doc.to_djot().unwrap();
    assert!(longest_leaf_lines(&saved) <= MAX_LEAF_LINES);
    let reopened = djot(&saved);
    assert_eq!(
        texts(&reopened),
        [
            lines[..MAX_LEAF_LINES].join(LS),
            lines[MAX_LEAF_LINES..].join(LS)
        ]
    );
}

/// A `\n` inside a block is not a line break: the model marks a new paragraph with a new
/// block, so it is saved as one, as it always was.
#[test]
fn a_newline_inside_a_block_is_still_saved_as_a_new_block() {
    let doc = djot("first");
    doc.cursor_at(5).insert_text("\nsecond").unwrap();
    let (_, reopened, _) = save_reopen(&doc);
    assert_eq!(texts(&reopened), ["first", "second"]);
}

// ── The readers ─────────────────────────────────────────────────

#[test]
fn the_djot_reader_keeps_a_hard_break_in_its_block() {
    assert_eq!(texts(&djot("a\\\nb")), [format!("a{LS}b")]);
    assert_eq!(texts(&djot("> a\\\n> b")), [format!("a{LS}b")]);
    assert_eq!(
        texts(&djot("- a\\\n  b\n- c")),
        [format!("a{LS}b"), "c".into()]
    );
    assert_eq!(texts(&djot("# a\\\n# b")), [format!("a{LS}b")]);
    // A raw line separator is the same character.
    assert_eq!(texts(&djot(&format!("a{LS}b"))), [format!("a{LS}b")]);
    // An image's description is one line.
    let doc = djot("![one\\\ntwo](pic.png)");
    assert!(doc.to_djot().unwrap().contains("![one two]"));
}

#[test]
fn the_markdown_reader_keeps_a_hard_break_in_its_block() {
    let doc = TextDocument::new();
    doc.set_markdown("a  \nb\\\nc\n\nd\n")
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(texts(&doc), [format!("a{LS}b{LS}c"), "d".into()]);
}

/// A `<br>` inside a paragraph used to leave nothing at all: `a<br>b` read as "ab". It is a
/// line break, with the spaces either side of it dropped as a browser lays them out, and
/// the last one of a paragraph shows no line of its own, as in a browser.
#[test]
fn the_html_reader_reads_a_br_as_a_line_break() {
    for (html, expected) in [
        ("<p>a<br>b</p>", format!("a{LS}b")),
        ("<p>a <br> b</p>", format!("a{LS}b")),
        ("<p>a<br></p>", "a".to_string()),
        ("<p>a<br><br></p>", format!("a{LS}")),
        ("<p><br>a</p>", format!("{LS}a")),
        ("<p><b>a<br>b</b></p>", format!("a{LS}b")),
        ("<h2>a<br>b</h2>", format!("a{LS}b")),
    ] {
        let doc = TextDocument::new();
        doc.set_html(html).unwrap().wait().unwrap();
        assert_eq!(texts(&doc), [expected], "{html}");
    }
    let doc = TextDocument::new();
    doc.set_html("<p><b>a<br>b</b></p>")
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(doc.to_djot().unwrap(), "*a*\\\n*b*");
    // A table cell is one line.
    let doc = TextDocument::new();
    doc.set_html("<table><tr><td>a<br>b</td></tr></table>")
        .unwrap()
        .wait()
        .unwrap();
    assert!(
        texts(&doc).contains(&"a b".to_string()),
        "{:?}",
        texts(&doc)
    );
}

/// The cheap extraction a project-wide search runs on is the text the document searches,
/// line breaks included.
#[test]
fn the_extracted_text_holds_the_line_break_the_document_searches() {
    let source = "> a\\\n> b\n\n- c\\\n  d";
    let extracted = djot_to_plain_text(source, &DjotImportOptions::default());
    assert_eq!(extracted, format!("a{LS}b\nc{LS}d"));
    assert_eq!(djot(source).to_addressable_text().unwrap(), extracted);
}

// ── The writers ─────────────────────────────────────────────────

#[test]
fn plain_text_keeps_the_character_and_a_txt_file_writes_lines() {
    let doc = djot("a\\\nb\n\n> c\\\n> d");
    assert_eq!(doc.to_plain_text().unwrap(), format!("a{LS}b\nc{LS}d"));
    assert_eq!(
        doc.to_plain_text_with(PlainTextExportOptions::presentation())
            .unwrap(),
        "a\nb\n    c\n    d"
    );
    let flat = djot("a\\\nb");
    assert_eq!(
        flat.to_plain_text_with(PlainTextExportOptions {
            line_breaks: true,
            ..PlainTextExportOptions::addressable()
        })
        .unwrap(),
        "a\nb"
    );
}

#[test]
fn html_writes_a_br() {
    let html = djot("a\\\n*b*\\\nc").to_html().unwrap();
    assert!(html.contains("a<br/><strong>"), "{html}");
    assert!(html.contains("</strong><br/>c"), "{html}");
}

#[test]
fn markdown_writes_a_hard_break_and_reads_it_back() {
    for (source, expected) in [
        ("a\\\nb", "a\\\nb"),
        ("> a\\\n> b", "> a\\\n> b"),
        ("- a\\\n  b", "- a\\\n  b"),
        ("12. a\\\n    b", "12. a\\\n    b"),
        // An ATX heading is one line.
        ("# a\\\n# b", "# a b"),
        // Markdown has nothing to write a break at the very end as.
        ("a\\\n{}", "a"),
        ("a\\\n- b", "a\\\n\\- b"),
    ] {
        let markdown = djot(source).to_markdown().unwrap();
        assert_eq!(markdown, expected, "{source:?}");
    }
    let back = TextDocument::new();
    back.set_markdown("> a\\\n> \\- b").unwrap().wait().unwrap();
    assert_eq!(texts(&back), [format!("a{LS}- b")]);
}

#[test]
fn latex_writes_a_break_that_survives_its_neighbours() {
    let latex = djot("\\\n[sic]\\\n*bold*\n\n# a\\\nb")
        .to_latex("article", false)
        .unwrap();
    // At the paragraph's start, and before a `[` or a `*` the break would take as its
    // arguments.
    assert!(
        latex.contains("\\mbox{}\\\\{}[sic]\\mbox{}\\\\{}"),
        "{latex}"
    );
    // A sectioning title is one line.
    assert!(latex.contains("\\section{a b}"), "{latex}");
}

#[cfg(feature = "docx")]
#[test]
fn docx_writes_a_break_inside_the_run() {
    let doc = djot("a\\\nb");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("breaks.docx");
    doc.to_docx(path.to_str().unwrap()).unwrap().wait().unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("word/document.xml").unwrap(), &mut xml)
        .unwrap();
    let a = xml.find(">a</w:t>").expect("the first line");
    let br = xml[a..].find("<w:br").expect("a break after it") + a;
    let b = xml[a..].find(">b</w:t>").expect("the second line") + a;
    assert!(br < b, "{xml}");
    assert!(!xml.contains(LS), "{xml}");
}

#[cfg(feature = "odt")]
#[test]
fn odt_writes_a_line_break() {
    let doc = djot("a\\\nb");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("breaks.odt");
    doc.to_odt(path.to_str().unwrap()).unwrap().wait().unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("content.xml").unwrap(), &mut xml).unwrap();
    assert!(xml.contains("a<text:line-break/>b"), "{xml}");
}

/// The clipboard: a copied fragment keeps the character in its own text, which is the
/// document's, and writes HTML's and Markdown's own line breaks for another program.
#[test]
fn a_copied_fragment_carries_its_line_breaks() {
    let doc = djot("a\\\nb\n\nc");
    let cursor = doc.cursor_at(0);
    cursor.move_position(MoveOperation::End, MoveMode::KeepAnchor, 1);
    let fragment = cursor.selection();
    assert!(fragment.to_plain_text().contains(&format!("a{LS}b")));
    assert!(
        fragment.to_html().contains("a<br>b"),
        "{}",
        fragment.to_html()
    );
    assert!(
        fragment.to_markdown().contains("a\\\nb"),
        "{}",
        fragment.to_markdown()
    );
    // Pasted back, it is the same block.
    let target = djot("");
    target.cursor_at(0).insert_fragment(&fragment).unwrap();
    assert_eq!(texts(&target), [format!("a{LS}b"), "c".into()]);
}
