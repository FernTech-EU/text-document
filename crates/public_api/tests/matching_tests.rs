//! Find & replace must not corrupt prose — end to end, through the real document.
//!
//! The literal search path used to lowercase the haystack, compute char offsets in the
//! *lowercased copy*, and hand them to a caller that applied them to the original. Where
//! folding changes a string's length — `'İ'.to_lowercase()` is two chars — the offsets
//! drifted, and replace overwrote the wrong characters.
//!
//! This is not a highlight being off by one. On `"İİİ ipsum dolor"`, `replace_text` with
//! the DEFAULT (case-insensitive) options produced `"İİİ ipsLOREMlor"` — it destroyed the
//! writer's text. Any manuscript containing a Turkish capital İ was exposed.

use text_document::matching::{MatchOptions, find_all};
use text_document::{
    DjotExportOptions, DjotImportOptions, FindOptions, ReplaceOptions, TextDocument,
};

fn doc_with(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_with_options(text, DjotImportOptions::default())
        .and_then(|op| op.wait())
        .expect("set_djot");
    doc
}

fn export(doc: &TextDocument) -> String {
    doc.to_djot_with_options(DjotExportOptions::default())
        .expect("to_djot")
        .trim()
        .to_string()
}

/// **The corruption, pinned.** If this ever fails, find & replace is destroying prose
/// again.
#[test]
fn a_case_fold_that_changes_length_does_not_corrupt_a_replace() {
    let source = "İİİ ipsum dolor";
    assert!(
        source.to_lowercase().chars().count() > source.chars().count(),
        "the fixture must actually grow when lowercased, or this proves nothing"
    );

    let doc = doc_with(source);

    // Case-insensitive is the DEFAULT — this is the ordinary path, not an exotic one.
    let hits = doc.find_all("ipsum", &FindOptions::default()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].position, 4,
        "the match position must be valid in the SOURCE text (it used to report 7, \
         drifting by one per İ)"
    );

    let count = doc
        .replace_text(
            "ipsum",
            "LOREM",
            true,
            &ReplaceOptions::new(FindOptions::default()),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        export(&doc),
        "İİİ LOREM dolor",
        "the replace must overwrite the word it matched — it used to produce \
         \"İİİ ipsLOREMlor\", shredding the prose"
    );
}

/// Whole-word Replace All must not leave a manuscript half-renamed. UAX#29 glues the
/// apostrophe into the word, so `Elena` used to miss `Elena's` entirely — a *silent*
/// miss, which in a bulk rename is the worst possible failure.
#[test]
fn whole_word_replace_all_does_not_half_rename_a_manuscript() {
    let doc = doc_with("Elena went home. Elena's coat stayed. They called Elena again.");

    let opts = FindOptions {
        whole_word: true,
        ..Default::default()
    };
    let count = doc
        .replace_text("Elena", "Marta", true, &ReplaceOptions::new(opts))
        .unwrap();

    assert_eq!(
        count, 3,
        "all three occurrences must be renamed, including the possessive"
    );
    let out = export(&doc);
    assert!(
        !out.contains("Elena"),
        "no trace of the old name may survive a rename: {out:?}"
    );
    // Djot's exporter renders `'` as the typographic `’`, so accept either — the point
    // is that the possessive was renamed, not which apostrophe the exporter chose.
    assert!(
        out.contains("Marta's coat") || out.contains("Marta\u{2019}s coat"),
        "the possessive must be renamed too: {out:?}"
    );
}

/// The public matcher and the in-document find must agree — that is the entire reason
/// the matcher is exposed rather than reimplemented by the host app.
#[test]
fn the_public_matcher_agrees_with_the_documents_own_find() {
    let text = "Elena's coat. Elena went home. la promesse d'Aurélien.";
    let doc = doc_with(text);

    for (query, whole_word) in [
        ("Elena", false),
        ("Elena", true),
        ("Aurélien", true),
        ("elena", false),
    ] {
        let find_opts = FindOptions {
            whole_word,
            ..Default::default()
        };
        let from_document: Vec<(usize, usize)> = doc
            .find_all(query, &find_opts)
            .unwrap()
            .into_iter()
            .map(|m| (m.position, m.length))
            .collect();

        let match_opts = MatchOptions {
            case_sensitive: false,
            whole_word,
            ..MatchOptions::default()
        };
        let from_matcher: Vec<(usize, usize)> = find_all(text, query, &match_opts)
            .into_iter()
            .map(|m| (m.char_start, m.char_len))
            .collect();

        assert_eq!(
            from_document, from_matcher,
            "the document's find and the public matcher disagreed on {query:?} \
             (whole_word={whole_word}) — that divergence is exactly what a shared \
             matcher exists to prevent"
        );
    }
}
