// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! A Djot source with CRLF line endings (a Windows checkout with `autocrlf`, an
//! `include_str!`ed help page) imports as the same source with LF endings: no `\r` in the
//! text, and a paragraph that wraps over several source lines is one paragraph.

use text_document::{DocumentFragment, TextDocument};

const LF: &str = "First line of a paragraph\nthat goes on here.\n\nSecond paragraph.\n\n```\nlet a = 1;\nlet b = 2;\n```\n\n- one\n- two\n";

fn crlf() -> String {
    LF.replace('\n', "\r\n")
}

#[test]
fn an_import_reads_a_crlf_source_as_its_lf_twin() {
    let from_lf = TextDocument::new();
    from_lf.set_djot_sync(LF).unwrap();
    let from_crlf = TextDocument::new();
    from_crlf.set_djot_sync(&crlf()).unwrap();

    let text = from_crlf.to_plain_text().unwrap();
    assert!(!text.contains('\r'), "{text:?}");
    assert_eq!(text, from_lf.to_plain_text().unwrap());
    assert_eq!(from_crlf.block_count(), from_lf.block_count());
    // The wrapped paragraph is one block, its soft break a space.
    assert_eq!(
        from_crlf.block_by_number(0).unwrap().text(),
        "First line of a paragraph that goes on here."
    );
    assert_eq!(from_crlf.to_djot().unwrap(), from_lf.to_djot().unwrap());
}

#[test]
fn a_paste_and_a_fragment_read_a_crlf_source_as_its_lf_twin() {
    let doc = TextDocument::new();
    doc.set_plain_text("").unwrap();
    doc.cursor().insert_djot(&crlf()).unwrap();
    assert!(!doc.to_plain_text().unwrap().contains('\r'));

    let fragment = DocumentFragment::from_djot(&crlf());
    assert!(!fragment.to_plain_text().contains('\r'));
    assert_eq!(
        fragment.to_plain_text(),
        DocumentFragment::from_djot(LF).to_plain_text()
    );
}

/// A code block holding a CRLF (what a CRLF import used to leave) is saved with it and
/// reads back with an LF: the model, saved and loaded, is no longer the identity for that
/// one block, and it heals on its first save.
#[test]
fn a_code_block_holding_a_crlf_reads_back_with_an_lf() {
    let doc = TextDocument::new();
    doc.set_djot_sync("```\nx\ny\n```\n").unwrap();
    let cursor = doc.cursor_at(1);
    cursor.insert_text("\r").unwrap();
    assert!(doc.to_plain_text().unwrap().contains("x\r\ny"));
    let djot = doc.to_djot().unwrap();
    let reloaded = TextDocument::new();
    reloaded.set_djot_sync(&djot).unwrap();
    let text = reloaded.to_plain_text().unwrap();
    assert!(!text.contains('\r'), "{djot:?} reloaded as {text:?}");
    assert!(text.contains("x\ny"));
}

/// A lone carriage return in text is kept through a save and a load, as the writer
/// guarantees.
#[test]
fn a_lone_carriage_return_survives_a_save_and_a_load() {
    let doc = TextDocument::new();
    doc.set_plain_text("a\rb").unwrap();
    let djot = doc.to_djot().unwrap();
    let again = TextDocument::new();
    again.set_djot_sync(&djot).unwrap();
    assert_eq!(again.to_plain_text().unwrap(), doc.to_plain_text().unwrap());
}
