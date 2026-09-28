//! What the editor writes, the depth guard lets through.
//!
//! `parse_djot` shows a document it judges too deep as its own Djot source, a plain
//! paragraph a line, and a save then keeps that source as prose, markers included, so
//! the document's structure is lost for good. So the guard may refuse nothing the
//! editor itself writes: these tests build documents through the cursor
//! calls an editor's keys and menus make, save them with `to_djot`, and hold the saved
//! Djot to the guard and to a reload.
//!
//! The reloads run on a thread with the 2 MiB stack a spawned thread gets, in the debug
//! build the suite runs in. A stack overflow is not a panic, so a document that got
//! past the guard and was too deep for the parser does not fail a test: it aborts the
//! test binary, which is the signal.

use common::parser_tools::djot_depth::{MAX_NESTING_DEPTH, is_too_deep, nesting_depth};
use proptest::prelude::*;
use text_document::{
    ListFormat, ListStyle, MoveMode, MoveOperation, TextCursor, TextDocument, TextFormat,
};

/// The stack `std::thread::spawn` gives a thread by default.
const SPAWNED_THREAD_STACK: usize = 2 << 20;

/// What a reload shows: each block's text, its list indent if it is a list item, and
/// how many blockquotes the caret sits in at its start.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shown {
    text: String,
    list_indent: Option<u8>,
    quotes: usize,
}

/// Reload `djot` into a fresh document on a spawned thread's stack, and read back what
/// it shows.
fn reload_on_a_spawned_thread(djot: &str) -> Vec<Shown> {
    let djot = djot.to_string();
    std::thread::Builder::new()
        .stack_size(SPAWNED_THREAD_STACK)
        .spawn(move || {
            let doc = TextDocument::new();
            doc.set_djot_sync(&djot).expect("reload the saved Djot");
            // Saving again runs the writer over what the reload built, however deep.
            doc.to_djot().expect("save the reloaded document");
            let cursor = doc.cursor();
            doc.blocks()
                .iter()
                .map(|block| {
                    cursor.set_position(block.position(), MoveMode::MoveAnchor);
                    Shown {
                        text: block.text(),
                        list_indent: block.list().map(|list| list.indent()),
                        quotes: cursor.blockquote_depth_at_cursor(),
                    }
                })
                .collect()
        })
        .expect("spawn the reload thread")
        .join()
        .expect("the reload must not unwind")
}

/// A list the editor Tabbed `levels` deep: one item a level, each made a list item and
/// then Tabbed one level further in than the one before, as `teksilo`'s editor answers
/// Tab in a list item (the item leaves its list for a new one a level further in).
fn tabbed_list(levels: usize) -> TextDocument {
    let doc = TextDocument::new();
    let cursor = doc.cursor();
    for level in 0..levels {
        cursor.move_position(MoveOperation::End, MoveMode::MoveAnchor, 1);
        if level > 0 {
            cursor.insert_block().expect("Enter");
        }
        cursor
            .insert_text(&format!("level {level}"))
            .expect("type the item");
        tab_into_list(&cursor, ListStyle::Disc, level);
    }
    doc
}

/// Make the caret's paragraph a list item of `style`, then Tab it `depth` levels in.
fn tab_into_list(cursor: &TextCursor, style: ListStyle, depth: usize) {
    cursor.create_list(style.clone()).expect("make a list item");
    for level in 1..=depth {
        cursor
            .remove_current_block_from_list()
            .expect("leave the list");
        cursor
            .create_list(style.clone())
            .expect("start a deeper list");
        cursor
            .set_current_list_format(&ListFormat {
                indent: Some(level as u8),
                ..ListFormat::default()
            })
            .expect("Tab one level in");
    }
}

/// A paragraph quoted `depth` levels deep, as the quote command nests it.
fn quoted(depth: usize) -> TextDocument {
    let doc = TextDocument::new();
    let cursor = doc.cursor();
    cursor.insert_text("deep").expect("type the paragraph");
    for _ in 0..depth {
        cursor
            .increase_blockquote_depth()
            .expect("quote it once more");
    }
    doc
}

/// A paragraph typed after `spaces` spaces, below one that was not.
fn typed_after_spaces(spaces: usize) -> TextDocument {
    let doc = TextDocument::new();
    let cursor = doc.cursor();
    cursor.insert_text("First.").expect("type");
    cursor.insert_block().expect("Enter");
    cursor
        .insert_text(&format!("{}Indented paragraph.", " ".repeat(spaces)))
        .expect("type after the spaces");
    doc
}

/// The shapes the editor writes deep: a paragraph typed after 194 spaces, a list
/// Tabbed 48 deep and a quotation 64 deep. Each is counted at the nesting it has, which
/// is well within the ceiling, and each reloads as its structure: the paragraph as a
/// paragraph with its spaces, every item at its own indent, the quotation 64 deep.
///
/// Before the scan followed the parser, the paragraph counted 97 (one level per two
/// spaces) and reloaded as its own Djot source, spaces and all, and the list counted 47.
#[test]
fn the_deep_shapes_the_editor_writes_reload_as_their_structure() {
    // The paragraph typed after 194 spaces, which the writer keeps with an empty
    // attribute set in front of them: they open no container.
    let indented = format!("{}Indented paragraph.", " ".repeat(194));
    let djot = typed_after_spaces(194).to_djot().expect("save");
    assert_eq!(nesting_depth(&djot), 0, "{djot:?}");
    assert!(!is_too_deep(&djot));
    let shown = reload_on_a_spawned_thread(&djot);
    let texts: Vec<&str> = shown.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts,
        ["First.", indented.as_str()],
        "reloaded as raw source: {djot:.80?}"
    );

    // The list Tabbed 48 deep.
    let djot = tabbed_list(48).to_djot().expect("save");
    assert_eq!(nesting_depth(&djot), 48);
    let shown = reload_on_a_spawned_thread(&djot);
    assert_eq!(shown.len(), 48, "reloaded as raw source: {djot:.80?}");
    for (level, block) in shown.iter().enumerate() {
        assert_eq!(block.text, format!("level {level}"));
        assert_eq!(block.list_indent, Some(level as u8), "item {level}");
    }

    // The quotation 64 deep.
    let djot = quoted(64).to_djot().expect("save");
    assert_eq!(nesting_depth(&djot), 64);
    let shown = reload_on_a_spawned_thread(&djot);
    assert_eq!(
        shown,
        [Shown {
            text: "deep".to_string(),
            list_indent: None,
            quotes: 64,
        }]
    );
}

/// The ceiling is safe through the whole reload, not only through the parser: every
/// kind of container, nested to the ceiling on one line, reloads on a spawned thread's
/// stack with its words, and one level past it is refused.
#[test]
fn every_container_nested_to_the_ceiling_reloads_on_a_spawned_thread() {
    for marker in [
        "- ", "* ", "+ ", "1. ", "1) ", "(1) ", "a. ", "B) ", "(c) ", "iv. ", "XII) ", "(ix) ",
        "- [ ] ", "* [x] ", "[^a]: ", ": ", "> ",
    ] {
        let djot = format!("{}deep\n", marker.repeat(MAX_NESTING_DEPTH));
        assert!(!is_too_deep(&djot), "{marker:?} at the ceiling");
        let shown = reload_on_a_spawned_thread(&djot);
        // A footnote's body is not a block of the document, and a definition's term is
        // empty; the other kinds hold the word.
        if !marker.starts_with(['[', ':']) {
            assert!(
                shown.iter().any(|s| s.text == "deep"),
                "{marker:?}: {shown:?}"
            );
        }
        assert!(is_too_deep(&format!(
            "{}deep\n",
            marker.repeat(MAX_NESTING_DEPTH + 1)
        )));
    }
}

/// What a document shows once `djot` is loaded into it, a block's text at a time, and
/// the Djot it saves as, on a spawned thread's stack.
fn load_and_save(djot: &str) -> (Vec<String>, String) {
    let djot = djot.to_string();
    std::thread::Builder::new()
        .stack_size(SPAWNED_THREAD_STACK)
        .spawn(move || {
            let doc = TextDocument::new();
            doc.set_djot_sync(&djot).expect("load the Djot");
            let shown = doc.blocks().iter().map(|block| block.text()).collect();
            (shown, doc.to_djot().expect("save it"))
        })
        .expect("spawn the load thread")
        .join()
        .expect("the load must not unwind")
}

/// A heading of 65,536 `#` made the parser panic inside `set_djot_sync`, on the caller's
/// own thread, which for an editor is its interface thread. It now loads as a paragraph
/// showing the heading's line, and the rest of the document keeps its structure. Saved
/// and loaded again, it saves the same.
#[test]
fn a_heading_the_parser_cannot_number_loads_as_its_text_and_saves_the_same() {
    let hashes = "#".repeat(65_536);
    let djot = format!("Before, _emphasis_.\n\n{hashes} Zeus\n\n- an item\n\nAfter.\n");
    let (shown, saved) = load_and_save(&djot);
    assert!(
        shown
            == [
                "Before, emphasis.",
                &format!("{hashes} Zeus"),
                "an item",
                "After."
            ],
        "loaded with its structure"
    );
    assert!(!is_too_deep(&saved), "saved as a paragraph");
    let (shown_again, saved_again) = load_and_save(&saved);
    assert!(shown_again == shown, "loaded again the same");
    assert!(saved_again == saved, "saved again the same");
}

/// A document too deep for the parser loads as its lines, one paragraph each, and saves
/// as those paragraphs, so loading and saving it again gives the same Djot. Loaded as one
/// paragraph of its source, it saved its line breaks as they were, loaded back as the
/// same document the parser could not take, and each save escaped its backslashes again.
#[test]
fn a_document_the_parser_cannot_take_loads_as_its_lines_and_saves_the_same() {
    let deep = format!("{}deep", "- ".repeat(700));
    let djot = format!("Before, _emphasis_.\n\n{deep}\n\nAfter.\n");
    let (shown, saved) = load_and_save(&djot);
    assert_eq!(shown, ["Before, _emphasis_.", deep.as_str(), "After."]);
    assert!(!is_too_deep(&saved), "saved as paragraphs");
    let (shown_again, saved_again) = load_and_save(&saved);
    assert_eq!(shown_again, shown);
    assert_eq!(saved_again, saved);
}

/// A paragraph of thousands of lines that opens an inline opener and never closes it
/// aborted the process inside `set_djot_sync`: the parser reads each further line one
/// call deeper while the opener waits, and a debug build ran out of a spawned thread's
/// stack at 730 lines. It now loads with every line.
///
/// A backtick left open makes the rest of the paragraph one code span, which keeps its
/// line breaks, and a line break is a new block to the model: that paragraph loads as a
/// block a line.
#[test]
fn a_long_paragraph_that_keeps_an_opener_waiting_loads_whole() {
    let lines = 3_000;
    for opener in ["_", "[", "`", "\"", "{_"] {
        let (shown, _) = load_and_save(&format!("{opener}{}", "word\n".repeat(lines)));
        let blocks = if opener == "`" { lines } else { 1 };
        assert_eq!(shown.len(), blocks, "{opener:?}: {blocks} blocks");
        assert_eq!(
            shown
                .iter()
                .map(|s| s.matches("word").count())
                .sum::<usize>(),
            lines,
            "{opener:?}"
        );
    }
}

/// A paragraph longer than the parser is given, 16,385 lines, loads as its lines, one
/// paragraph each, and the blocks around it keep their structure. Saved and loaded again,
/// it saves the same. It opens an emphasis it never closes, which used to abort the
/// process.
#[test]
fn a_block_too_long_for_the_parser_loads_as_its_lines_alone_and_saves_the_same() {
    let lines = 16_385;
    let djot = format!("- an item\n\n_{}\n> quoted\n", "word\n".repeat(lines));
    let shown = reload_on_a_spawned_thread(&djot);
    assert_eq!(shown.len(), lines + 2);
    assert_eq!(shown[0].text, "an item");
    assert!(
        shown[0].list_indent.is_some(),
        "the list item keeps its list"
    );
    assert!(
        shown[1..=lines].iter().enumerate().all(|(line, shown)| {
            shown.list_indent.is_none()
                && shown.quotes == 0
                && shown.text == if line == 0 { "_word" } else { "word" }
        }),
        "the long paragraph loads as its lines"
    );
    assert_eq!(
        shown[lines + 1],
        Shown {
            text: "quoted".to_string(),
            list_indent: None,
            quotes: 1
        },
        "the quotation keeps its quote"
    );
    let (texts, saved) = load_and_save(&djot);
    let (texts_again, saved_again) = load_and_save(&saved);
    assert!(texts_again == texts, "loaded again the same");
    assert!(saved_again == saved, "saved again the same");
}

// ── Whatever the editor writes, the guard lets through ─────────────────────────────

/// One piece of what a writer can type: a word, a run of spaces or tabs of any length,
/// any character a Djot marker is made of, alone or as a marker or repeated in a long
/// run, and the break between two paragraphs.
fn typed_piece() -> impl Strategy<Value = String> {
    let blank = prop::sample::select(vec![" ", "\t", " \t", "\t ", "\u{A0}", "\u{3000}"]);
    let marker = prop::sample::select(vec![
        "-", "*", "+", ">", ":", "|", "[", "]", "^", "(", ")", ".", "#", "{", "}", "`", "~", "=",
        "_", "!", "\\", "\"", "'", "<", "&", "$", "%", "1.", "a)", "(iv)", "XII.", "- ", "* ",
        "+ ", "> ", ": ", "1. ", "[^a]: ", "[^a]:", "[l]: ", "- [ ] ", "* [x] ", "::: c", ":::",
        "```", "~~~", "| a |", "* * *", "{.c}", "# ", "10:30:45", "Mr. ",
    ]);
    prop_oneof![
        3 => "[A-Za-z]{1,9}",
        3 => (blank.clone(), 1usize..600).prop_map(|(blank, n)| blank.repeat(n)),
        3 => marker.clone().prop_map(str::to_string),
        1 => (marker, 2usize..300).prop_map(|(marker, n)| marker.repeat(n)),
        1 => (blank, 1usize..300, "[a-z]{1,6}")
            .prop_map(|(blank, n, word)| format!("{}{word}", blank.repeat(n))),
        1 => Just("\n".to_string()),
    ]
}

/// A stretch of typing: paragraphs, and whatever their lines open with.
fn typed_text() -> impl Strategy<Value = String> {
    prop::collection::vec(typed_piece(), 0..16).prop_map(|pieces| pieces.concat())
}

/// One paragraph of a document the writer edited, and how they shaped it.
#[derive(Debug, Clone)]
enum Edit {
    /// Typed as it stands.
    Typed(String),
    /// Typed, then selected and given a character format.
    Formatted(String, u8),
    /// Made a list item, then Tabbed `depth` levels in.
    Listed(String, ListStyle, usize),
    /// Made a quotation, `depth` levels deep.
    Quoted(String, usize),
    /// A footnote's reference at the caret, then the typing after it.
    Noted(String),
}

fn edit() -> impl Strategy<Value = Edit> {
    let style = prop::sample::select(vec![
        ListStyle::Disc,
        ListStyle::Decimal,
        ListStyle::LowerAlpha,
        ListStyle::UpperRoman,
    ]);
    prop_oneof![
        2 => typed_text().prop_map(Edit::Typed),
        1 => (typed_text(), 0u8..4).prop_map(|(text, format)| Edit::Formatted(text, format)),
        1 => (typed_text(), style, 0usize..6)
            .prop_map(|(text, style, depth)| Edit::Listed(text, style, depth)),
        1 => (typed_text(), 1usize..4).prop_map(|(text, depth)| Edit::Quoted(text, depth)),
        1 => typed_text().prop_map(Edit::Noted),
    ]
}

/// Type `text` at the caret as the editor takes it in: every line break a new
/// paragraph, the rest inserted as it stands.
fn type_at(cursor: &TextCursor, text: &str) {
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            let _ = cursor.insert_block();
        }
        if !line.is_empty() {
            let _ = cursor.insert_text(line);
        }
    }
}

/// What `edits` save as, each one a paragraph after the last, made through the cursor
/// calls the editor's keys and menus make. A call the document refuses in that place is
/// ignored, as the editor ignores it.
fn edited_djot(edits: &[Edit]) -> String {
    let doc = TextDocument::new();
    let cursor = doc.cursor();
    for (index, edit) in edits.iter().enumerate() {
        cursor.move_position(MoveOperation::End, MoveMode::MoveAnchor, 1);
        if index > 0 {
            let _ = cursor.insert_block();
        }
        match edit {
            Edit::Typed(text) => type_at(&cursor, text),
            Edit::Formatted(text, format) => {
                let start = cursor.position();
                type_at(&cursor, text);
                cursor.set_position(start, MoveMode::KeepAnchor);
                let _ = cursor.merge_char_format(&TextFormat {
                    font_bold: Some(*format == 0),
                    font_italic: Some(*format == 1),
                    font_underline: Some(*format == 2),
                    font_strikeout: Some(*format == 3),
                    ..TextFormat::default()
                });
                cursor.move_position(MoveOperation::End, MoveMode::MoveAnchor, 1);
            }
            Edit::Listed(text, style, depth) => {
                type_at(&cursor, text);
                let _ = cursor.create_list(style.clone());
                for level in 1..=*depth {
                    let _ = cursor.remove_current_block_from_list();
                    let _ = cursor.create_list(style.clone());
                    let _ = cursor.set_current_list_format(&ListFormat {
                        indent: Some(level as u8),
                        ..ListFormat::default()
                    });
                }
            }
            Edit::Quoted(text, depth) => {
                type_at(&cursor, text);
                for _ in 0..*depth {
                    let _ = cursor.increase_blockquote_depth();
                }
            }
            Edit::Noted(text) => {
                let _ = cursor.insert_djot("[^fn1]");
                cursor.move_position(MoveOperation::End, MoveMode::MoveAnchor, 1);
                type_at(&cursor, text);
            }
        }
    }
    doc.to_djot().expect("the document saves as Djot")
}

/// What the editor saves for `text` set as a document's whole text.
fn typed_djot(text: &str) -> String {
    let doc = TextDocument::new();
    doc.set_plain_text(text).expect("set the typed text");
    doc.to_djot().expect("the document saves as Djot")
}

/// The guard lets `djot` through, and it reloads on a spawned thread's stack.
fn lets_through(djot: &str) -> Result<(), TestCaseError> {
    prop_assert!(
        !is_too_deep(djot),
        "saved {:.120?}, which a reload shows as raw source (counted {})",
        djot,
        nesting_depth(djot)
    );
    reload_on_a_spawned_thread(djot);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// Whatever a writer types, the Djot it saves as reloads as its structure. A
    /// paragraph typed after two hundred spaces or tabs used to be saved as typed and
    /// reloaded as its own source.
    #[test]
    fn whatever_is_typed_reloads_as_its_structure(text in typed_text()) {
        lets_through(&typed_djot(&text))?;
    }

    /// The same of a document shaped as well as typed: formatting, a Tabbed list, a
    /// nested quotation and a footnote's reference, each with typing of any shape in it.
    #[test]
    fn whatever_is_shaped_reloads_as_its_structure(
        edits in prop::collection::vec(edit(), 1..7),
    ) {
        lets_through(&edited_djot(&edits))?;
    }
}

/// The DOCX writer walked a quotation by calling itself once a level, about 31 KiB of
/// stack a call in a debug build, on the 2 MiB thread an export runs on: a quotation 55
/// deep, fewer than the editor nests with its quote command (64) and the reader allows
/// (128), aborted the whole process. It walks them in a loop now, so the depth a
/// document can hold is no limit: each export here runs on the library's own export
/// thread, and every one holds the quoted words.
#[test]
fn a_deep_quotation_exports_to_docx() {
    let dir = tempfile::tempdir().expect("a directory for the exports");
    for depth in [64, MAX_NESTING_DEPTH, 300] {
        let doc = quoted(depth);
        let path = dir.path().join(format!("quoted-{depth}.docx"));
        let path = path.to_str().expect("a UTF-8 path");
        doc.to_docx(path)
            .expect("start the export")
            .wait()
            .expect("the export");
        let bytes = std::fs::read(path).expect("the exported file");
        let mut zip =
            zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a DOCX is a zip archive");
        let mut body = String::new();
        std::io::Read::read_to_string(
            &mut zip.by_name("word/document.xml").expect("the body part"),
            &mut body,
        )
        .expect("the body is text");
        assert!(
            body.contains(">deep<"),
            "{depth} deep: the quoted words are gone"
        );
    }
}
