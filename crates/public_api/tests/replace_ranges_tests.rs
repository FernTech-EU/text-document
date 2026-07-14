//! `replace_ranges` / `find_and_replace` — a reviewed bulk rename, done safely.
//!
//! `replace_text` can only put the *same* string at every match. A reviewed rename needs
//! neither of those things: the writer unticks some occurrences, and the ones that stay must
//! keep the case they were found in. So the caller decides, per occurrence — and the scan and
//! the splice happen together, because doing them in two calls is a race that rewrites the
//! wrong words rather than failing.

use text_document::{
    DjotExportOptions, DjotImportOptions, FindOptions, ReplaceFormatPolicy, ReplaceOptions,
    ReplaceRange, TextDocument,
};

fn doc_with(djot: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_djot_with_options(djot, DjotImportOptions::default())
        .and_then(|op| op.wait())
        .expect("set_djot");
    doc
}

fn djot(doc: &TextDocument) -> String {
    doc.to_djot_with_options(DjotExportOptions::default())
        .expect("to_djot")
        .trim()
        .to_string()
}

fn opts() -> ReplaceOptions {
    ReplaceOptions::new(FindOptions::default())
}

/// **The point of the whole thing:** a rename that skips what the writer unticked and keeps
/// the case of what it replaces. `replace_text` cannot express either.
#[test]
fn a_reviewed_rename_skips_the_unticked_and_keeps_the_case() {
    let doc = doc_with("Aurélien woke. AURÉLIEN shouted. Aurélien slept. aurélien dreamt.");

    // The writer unticked occurrence #2 (the one they want left alone).
    let excluded = [2usize];

    let count = doc
        .find_and_replace("aurélien", &opts(), |matched, i| {
            if excluded.contains(&i) {
                return None;
            }
            // Preserve the case we found.
            Some(
                if matched
                    .chars()
                    .all(|c| c.is_uppercase() || !c.is_alphabetic())
                {
                    "AURÉLIAN".to_string()
                } else if matched.chars().next().is_some_and(char::is_uppercase) {
                    "Aurélian".to_string()
                } else {
                    "aurélian".to_string()
                },
            )
        })
        .expect("find_and_replace");

    assert_eq!(count, 3, "four matches, one unticked");
    assert_eq!(
        djot(&doc),
        "Aurélian woke. AURÉLIAN shouted. Aurélien slept. aurélian dreamt.",
        "each occurrence keeps the case it was found in, and the unticked one is untouched"
    );
}

/// The whole batch is **one** undo. A bulk rewrite of someone's manuscript that could not be
/// taken back in a single action is the most destructive thing this API offers.
#[test]
fn the_whole_batch_is_one_undo() {
    let source = "Elena went. Elena stayed. Elena returned.";
    let doc = doc_with(source);

    let count = doc
        .find_and_replace("Elena", &opts(), |_, _| Some("Marta".to_string()))
        .unwrap();
    assert_eq!(count, 3);
    assert_eq!(djot(&doc), "Marta went. Marta stayed. Marta returned.");

    doc.undo().expect("undo");
    assert_eq!(
        djot(&doc),
        source,
        "ONE undo must restore all three, exactly — not one of them, and not none"
    );

    doc.redo().expect("redo");
    assert_eq!(djot(&doc), "Marta went. Marta stayed. Marta returned.");
}

/// Replacements of **different lengths** must not shift each other. This is what applying the
/// edits descending buys: a splice at the front cannot move a range still waiting behind it.
#[test]
fn edits_of_different_lengths_do_not_shift_one_another() {
    let doc = doc_with("x A x A x A x");

    let count = doc
        .find_and_replace("A", &opts(), |_, i| {
            // Wildly different lengths, in both directions.
            Some(match i {
                0 => "LONGER-REPLACEMENT".to_string(),
                1 => "B".to_string(),
                _ => "MEDIUM".to_string(),
            })
        })
        .unwrap();

    assert_eq!(count, 3);
    assert_eq!(djot(&doc), "x LONGER-REPLACEMENT x B x MEDIUM x");
}

/// Length-changing edits must **rebase** the blocks after them. Skip that and document-wide
/// addressing is silently wrong from the next edit onward — so the *following* rename lands in
/// the wrong place. Two renames in a row is the cheapest way to catch it.
#[test]
fn a_second_rename_after_a_length_changing_one_still_lands_correctly() {
    let doc = doc_with("Elena walked.\n\nThe road was long.\n\nElena rested.");

    doc.find_and_replace("Elena", &opts(), |_, _| {
        Some("Marguerite-Anne".to_string()) // much longer: every later block shifts
    })
    .unwrap();

    // If document_position was not rebased, this second rename addresses stale offsets.
    let count = doc
        .find_and_replace("road", &opts(), |_, _| Some("path".to_string()))
        .unwrap();

    assert_eq!(count, 1);
    assert_eq!(
        djot(&doc),
        "Marguerite-Anne walked.\n\nThe path was long.\n\nMarguerite-Anne rested.",
        "the second rename must land on `road`, not on text shifted by the first"
    );
}

/// Formatting survives, because the edit happens **inside the document** rather than as string
/// surgery on the exported markup.
#[test]
fn the_formatting_under_a_renamed_word_survives() {
    let doc = doc_with("She called *Aurélien* into the trees.");

    doc.find_and_replace(
        "Aurélien",
        &opts().with_format_policy(ReplaceFormatPolicy::PreserveIfFullyCovered),
        |_, _| Some("Aurélian".to_string()),
    )
    .unwrap();

    assert_eq!(
        djot(&doc),
        "She called *Aurélian* into the trees.",
        "the emphasis must survive the rename"
    );
}

/// A query that also occurs inside a **link's URL** must not be rewritten there. The URL is
/// markup, not prose — the writer never typed it into their sentence. This is precisely what
/// string-surgery on exported Djot gets wrong.
#[test]
fn a_match_inside_a_links_url_is_not_rewritten() {
    let doc = doc_with("Read [the note](https://example.test/note) about the note.");

    let count = doc
        .find_and_replace("note", &opts(), |_, _| Some("letter".to_string()))
        .unwrap();

    let out = djot(&doc);
    assert_eq!(
        count, 2,
        "only the two occurrences in the PROSE (the link's label, and the last word)"
    );
    assert!(
        out.contains("https://example.test/note"),
        "the link's destination is markup and must survive untouched: {out:?}"
    );
    assert_eq!(
        out,
        "Read [the letter](https://example.test/note) about the letter."
    );
}

/// Returning `None` for everything changes nothing at all.
#[test]
fn deciding_against_every_occurrence_is_a_no_op() {
    let source = "Elena went. Elena stayed.";
    let doc = doc_with(source);

    let count = doc.find_and_replace("Elena", &opts(), |_, _| None).unwrap();

    assert_eq!(count, 0);
    assert_eq!(djot(&doc), source);
}

/// Overlapping ranges cannot both be honoured. The earlier one wins and the later is **skipped**
/// — not silently applied over the top of it, which would rewrite text the caller never asked
/// about.
#[test]
fn overlapping_ranges_are_refused_not_silently_merged() {
    let doc = doc_with("abcdef");

    let count = doc
        .replace_ranges(
            &[
                ReplaceRange {
                    position: 0,
                    length: 3,
                    replacement: "X".to_string(),
                },
                // Overlaps the first: starts inside it.
                ReplaceRange {
                    position: 2,
                    length: 3,
                    replacement: "Y".to_string(),
                },
            ],
            &opts(),
        )
        .unwrap();

    assert_eq!(count, 1, "only the earlier range is applied");
    assert_eq!(djot(&doc), "Xdef");
}

/// `replace_ranges` addresses the same char space `find_all` reports in — so a match can be
/// turned into a range directly, with no translation.
#[test]
fn a_range_addresses_the_same_offsets_find_all_reports() {
    let doc = doc_with("one two three");
    let hits = doc.find_all("two", &FindOptions::default()).unwrap();
    assert_eq!(hits.len(), 1);

    doc.replace_ranges(
        &[ReplaceRange {
            position: hits[0].position,
            length: hits[0].length,
            replacement: "TWO".to_string(),
        }],
        &opts(),
    )
    .unwrap();

    assert_eq!(djot(&doc), "one TWO three");
}
