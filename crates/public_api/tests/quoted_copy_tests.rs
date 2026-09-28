//! Copying, cutting and dragging text that stands in quotations.
//!
//! A copy is the fragment [`TextCursor::selection`] makes, and a host's copy, cut, drag and
//! paste all go through it. It read every frame of the document and every frame nested in
//! each, so a paragraph standing in `d` quotations was copied `d + 1` times: a select all
//! and copy of `> q1` gave `q1` twice, and a cut then a paste of a nested quotation wrote
//! its paragraphs back two and three times over. A selection of all of a text opening or
//! ending with a table in a quotation copied nothing at all, and cutting it put nothing on
//! the clipboard. A copy holds each paragraph once, in reading order, with the quotations
//! it stands in, and a paste of it puts the same text back.

use text_document::{DocumentFragment, MoveMode, SelectionType, TextCursor, TextDocument};

fn load(djot: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_sync(djot).unwrap();
    doc
}

/// Where `needle` starts in `doc`'s addressable text, as a character position.
fn position_of(doc: &TextDocument, needle: &str) -> usize {
    let text = doc.to_addressable_text().unwrap();
    let byte = text.find(needle).expect("the text is in the document");
    text[..byte].chars().count()
}

fn select(doc: &TextDocument, from: usize, to: usize) -> TextCursor {
    let cursor = doc.cursor();
    cursor.set_position(from, MoveMode::MoveAnchor);
    cursor.set_position(to, MoveMode::KeepAnchor);
    cursor
}

fn select_all(doc: &TextDocument) -> TextCursor {
    let cursor = doc.cursor();
    cursor.select(SelectionType::Document);
    cursor
}

/// A text, the plain text a copy of all of it holds, and the text a paste of that copy
/// into an empty document saves as: the text itself but for the bodies of its notes, which
/// stay where they are defined.
struct Case {
    text: &'static str,
    copied: &'static str,
    pasted: &'static str,
}

const CASES: [Case; 9] = [
    // The two shapes the teksilo review found.
    Case {
        text: "> q1\n>\n> > q2\n",
        copied: "q1\nq2",
        pasted: "> q1\n>\n> > q2",
    },
    Case {
        text: "> q1\n",
        copied: "q1",
        pasted: "> q1",
    },
    // Lists in quotations, one of them in a nested quotation.
    Case {
        text: "Para.\n\n> - a\n> - b\n>\n> > - c\n\nEnd.\n",
        copied: "Para.\na\nb\nc\nEnd.",
        pasted: "Para.\n\n> - a\n>\n> - b\n>\n> > - c\n\nEnd.",
    },
    Case {
        text: "> Letter.\n>\n> > 1. one\n> > 2. two\n>\n> Signed.\n",
        copied: "Letter.\none\ntwo\nSigned.",
        pasted: "> Letter.\n>\n> > 1. one\n> >\n> > 2. two\n>\n> Signed.",
    },
    // A table in a quotation, opening the text, inside it, and closing it.
    Case {
        text: "> | x | y |\n\nOut.\n",
        copied: "x\ny\nOut.",
        pasted: "> | x | y |\n> |---|---|\n\nOut.",
    },
    Case {
        text: "In.\n\n> | x | y |\n> | z | w |\n\nOut.\n",
        copied: "In.\nx\ny\nz\nw\nOut.",
        pasted: "In.\n\n> | x | y |\n> |---|---|\n> | z | w |\n\nOut.",
    },
    Case {
        text: "In.\n\n> | x | y |\n",
        copied: "In.\nx\ny",
        pasted: "In.\n\n> | x | y |\n> |---|---|",
    },
    // Notes referenced from quotations: the references travel, the bodies stay.
    Case {
        text: "> Quoted[^n] text.\n>\n> > Deeper[^m].\n\nAfter.\n\n[^n]: Note body.\n\n\
               [^m]: Other body.\n",
        copied: "Quoted\u{FFFC} text.\nDeeper\u{FFFC}.\nAfter.",
        pasted: "> Quoted[^n] text.\n>\n> > Deeper[^m].\n\nAfter.",
    },
    // An epigraph, its attribution set right.
    Case {
        text: "> {semantic_role=epigraph}\n> The sea.\n>\n> {alignment=right}\n> Anon.\n\n\
               Chapter.\n",
        copied: "The sea.\nAnon.\nChapter.",
        pasted: "> {semantic_role=epigraph}\n> The sea.\n>\n> {alignment=right}\n> Anon.\n\n\
                 Chapter.",
    },
];

/// A copy of all of a text holds each of its paragraphs once, in reading order.
#[test]
fn a_copy_holds_each_quoted_paragraph_once() {
    for case in &CASES {
        let doc = load(case.text);
        let copied = select_all(&doc).selection();
        assert_eq!(
            copied.to_plain_text(),
            case.copied,
            "copying {:?}",
            case.text
        );
    }
}

/// A copy of all of a text, pasted into an empty document, is the same text, its
/// quotations, lists, tables and notes' references where they stood.
#[test]
fn a_copy_pasted_into_an_empty_document_is_the_same_text() {
    for case in &CASES {
        let copied = select_all(&load(case.text)).selection();
        let pasted = TextDocument::new();
        pasted.cursor().insert_fragment(&copied).unwrap();
        assert_eq!(
            pasted.to_djot().unwrap(),
            case.pasted,
            "pasting {:?}",
            case.text
        );
    }
}

/// Cutting all of a text and pasting it back gives the text as it was, the bodies of its
/// notes included, which the cut left where they are.
#[test]
fn cutting_all_of_a_text_and_pasting_it_back_changes_nothing() {
    for case in &CASES {
        let doc = load(case.text);
        let original = doc.to_djot().unwrap();
        let cursor = select_all(&doc);
        let cut = cursor.selection();
        cursor.remove_selected_text().unwrap();
        assert!(
            !cut.is_empty(),
            "the cut of {:?} put nothing aside",
            case.text
        );
        cursor.insert_fragment(&cut).unwrap();
        assert_eq!(doc.to_djot().unwrap(), original, "cutting {:?}", case.text);
    }
}

/// How many times `word` stands in `doc`'s addressable text.
fn occurrences(doc: &TextDocument, word: &str) -> usize {
    doc.to_addressable_text().unwrap().matches(word).count()
}

/// A copy pasted at the end of the text it was taken from doubles each of its words: no
/// more. A paragraph in a quotation nested in another went in three times.
#[test]
fn a_copy_pasted_after_its_text_holds_each_word_twice() {
    let text = "Start.\n\n> quoted one\n>\n> > nested two\n> >\n> > - item three\n\nEnd.\n";
    let doc = load(text);
    let copied = select_all(&doc).selection();
    let end = doc.to_addressable_text().unwrap().chars().count();
    doc.cursor_at(end).insert_fragment(&copied).unwrap();
    for word in ["Start", "quoted one", "nested two", "item three", "End"] {
        assert_eq!(
            occurrences(&doc, word),
            2,
            "{word:?} in {:?}",
            doc.to_djot()
        );
    }
}

/// A selection from inside one quotation to inside another nested in it copies the part
/// of each paragraph it covers, once.
#[test]
fn a_selection_across_nested_quotations_copies_what_it_covers() {
    let doc = load("Before.\n\n> quoted one\n>\n> > nested two\n\nAfter.\n");
    let from = position_of(&doc, "one");
    let to = position_of(&doc, " two");
    let copied = select(&doc, from, to).selection();
    assert_eq!(copied.to_plain_text(), "one\nnested");
    let pasted = TextDocument::new();
    pasted.cursor().insert_fragment(&copied).unwrap();
    assert_eq!(pasted.to_addressable_text().unwrap(), "one\nnested");
}

/// Dragging a nested quotation to the end of the text, a cut and a paste, moves it: each
/// of its paragraphs stands once in the text, after the paragraph it was dropped behind.
#[test]
fn dragging_a_nested_quotation_moves_each_paragraph_once() {
    let doc = load("Start.\n\n> q1\n>\n> > q2\n\nMiddle.\n\nEnd.\n");
    let from = position_of(&doc, "q1");
    let to = position_of(&doc, "Middle.");
    let cursor = select(&doc, from, to);
    let dragged = cursor.selection();
    assert_eq!(dragged.to_plain_text(), "q1\nq2");
    cursor.remove_selected_text().unwrap();
    let end = doc.to_addressable_text().unwrap().chars().count();
    doc.cursor_at(end).insert_fragment(&dragged).unwrap();
    for word in ["Start", "q1", "q2", "Middle", "End"] {
        assert_eq!(
            occurrences(&doc, word),
            1,
            "{word:?} in {:?}",
            doc.to_djot()
        );
    }
    let text = doc.to_addressable_text().unwrap();
    assert!(
        text.find("End").unwrap() < text.find("q1").unwrap()
            && text.find("q1").unwrap() < text.find("q2").unwrap(),
        "{text:?}"
    );
}

/// The copy of a quoted table is the whole table, and a fragment made from the whole
/// document is the same text as the copy of all of it.
#[test]
fn a_copy_of_a_text_opening_with_a_quoted_table_holds_the_table() {
    let doc = load("> | x | y |\n> | z | w |\n\nOut.\n");
    let copied = select_all(&doc).selection();
    let whole = DocumentFragment::from_document(&doc).unwrap();
    assert_eq!(copied.to_plain_text(), "x\ny\nz\nw\nOut.");
    assert_eq!(whole.to_plain_text(), copied.to_plain_text());

    // From the text after the table back into its cells: the whole table is copied.
    let from = position_of(&doc, "Out.") + 2;
    let to = position_of(&doc, "z");
    let copied = select(&doc, from, to).selection();
    assert_eq!(copied.to_plain_text(), "x\ny\nz\nw\nOu");
}

/// After a cut of all of a text, typed text keeps the formatting of the paragraph the cut
/// left, the first removed paragraph's, as typing over the selection does: only a paste
/// replaces the text.
#[test]
fn typing_after_a_cut_of_everything_keeps_the_first_paragraph_format() {
    let doc = load("> Letter.\n\nAfter.\n");
    let cursor = select_all(&doc);
    cursor.remove_selected_text().unwrap();
    cursor.insert_text("typed").unwrap();
    assert_eq!(doc.to_djot().unwrap(), "> typed");
}

/// A copy of all of a text that is one table of one cell holds the table, as the removal of
/// the same selection takes it: it held the cell's words only, and a cut and a paste turned
/// the table into paragraphs. The same with a note's body after the table, where the
/// selection ends outside the table and was copied from the first cell on, past the anchor.
#[test]
fn a_copy_of_all_of_a_text_that_is_a_table_of_one_cell_holds_the_table() {
    for text in ["| solo |\n", "| solo |\n\n[^d1]: A body.\n"] {
        let doc = load(text);
        let in_cell = position_of(&doc, "solo") + 2;
        doc.cursor_at(in_cell).insert_djot("one\n\ntwo\n").unwrap();
        let before = doc.to_djot().unwrap();
        let cursor = select_all(&doc);
        let cut = cursor.selection();
        cursor.remove_selected_text().unwrap();
        cursor.insert_fragment(&cut).unwrap();
        assert_eq!(doc.to_djot().unwrap(), before, "cutting {text:?}");
        assert_eq!(doc.to_djot().unwrap().matches("|---|").count(), 1);
        let pasted = TextDocument::new();
        pasted.cursor().insert_fragment(&cut).unwrap();
        assert_eq!(
            pasted.to_djot().unwrap(),
            "| soone twolo |\n|---|",
            "copying {text:?}"
        );
    }
}

/// The paragraphs of a table cell are copied in the order they are read in. They were
/// ordered by the position each block last stored, which the rope has moved past since: a
/// cell holding a heading and a passage pasted after it came back with the heading last.
/// (The edits of seed 2500 of the structural differential.)
#[test]
fn a_copied_cell_keeps_its_paragraphs_in_the_order_they_are_read() {
    let doc = load("Now.\n");
    select_all(&doc)
        .insert_html(
            "<table><tr><th colspan=\"900\">Title</th></tr><tr><td>one</td><td>two</td></tr>\
             </table>",
        )
        .unwrap();
    assert_eq!(
        doc.to_addressable_text().unwrap(),
        "\n\u{FFFC}\nTitle\n\none\ntwo"
    );
    select(&doc, 9, 17).insert_text("over").unwrap();
    doc.cursor_at(7)
        .insert_html("<pre>verse one\n  verse two\n\nverse three</pre>")
        .unwrap();
    doc.cursor_at(14).insert_table(2, 2).unwrap();
    // A copy of part of the text pasted over another part of it, which stores new
    // positions for some of the blocks and leaves the others' behind.
    let copied = select(&doc, 7, 49).selection();
    let _ = select(&doc, 18, 42).insert_fragment(&copied);
    let before = doc.to_addressable_text().unwrap();
    assert_eq!(
        before,
        "\n\u{FFFC}\nTitlverse one\n  verse two\n\nverse threee\nover\n\n\n\u{FFFC}\n\n\n\n"
    );

    let cursor = select_all(&doc);
    let cut = cursor.selection();
    cursor.remove_selected_text().unwrap();
    cursor.insert_fragment(&cut).unwrap();
    assert_eq!(doc.to_addressable_text().unwrap(), before);
}

/// The empty paragraph a text ends with is taken with a selection reaching the text's end:
/// a cut and a paste of all of a text ending with an empty code block dropped it, and the
/// save, which keeps an empty code block, lost it.
#[test]
fn a_copy_reaching_the_end_takes_the_empty_block_the_text_ends_with() {
    let doc = load("Prose.\n\n```\n\n```\n");
    let before = doc.to_djot().unwrap();
    assert_eq!(before, "Prose.\n\n```\n\n```");
    let cursor = select_all(&doc);
    let cut = cursor.selection();
    cursor.remove_selected_text().unwrap();
    cursor.insert_fragment(&cut).unwrap();
    assert_eq!(doc.to_djot().unwrap(), before);
}
