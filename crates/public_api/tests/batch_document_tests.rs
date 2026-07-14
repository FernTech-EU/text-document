//! `BatchDocument` — the headless, thread-free document a backend batch uses.
//!
//! The point of this type is that it is *cheap enough to make hundreds of*. So these
//! tests check two things: that it round-trips Djot faithfully (a replace that mangles
//! markup is worse than no replace at all), and that it genuinely costs no thread —
//! because if it did, a project-wide rename would spawn one per touched scene.

use text_document::{BatchDocument, DjotExportOptions, DjotImportOptions, FindOptions};

fn round_trip(djot: &str) -> String {
    let batch = BatchDocument::new().expect("BatchDocument::new");
    batch
        .set_djot(djot, &DjotImportOptions::default())
        .expect("set_djot");
    batch.to_djot(&DjotExportOptions::default()).expect("to_djot")
}

/// Prose survives the trip. This is the load-bearing property: a replace parses the
/// Djot, rewrites text, and serialises it back — so anything the round trip damages,
/// a replace would damage across the whole manuscript.
#[test]
fn a_batch_document_round_trips_djot() {
    for source in [
        "Just a plain paragraph.",
        "A paragraph with *emphasis* and _more_ of it.",
        "# A heading\n\nAnd a paragraph beneath it.",
        "One paragraph.\n\nAnd a second one.",
    ] {
        let out = round_trip(source);
        assert_eq!(
            out.trim(),
            source.trim(),
            "round-tripping this Djot changed it:\n  in:  {source:?}\n  out: {out:?}"
        );
    }
}

/// A **tight** list comes back **loose** — the one place the round trip is not an
/// identity. Pinned rather than wished away, because a `BatchDocument` is what a
/// project-wide replace runs through, and this is the shape of the only reformatting
/// it can cause.
///
/// This is a documented limitation of the model, not a bug in this type: the entity
/// model has no tight/loose distinction, and the exporter's blank line between blocks
/// is what lets an indented sub-list *nest* instead of folding into its parent item's
/// paragraph (see `export_djot_uc`'s own comment). Removing it would trade this for a
/// worse bug.
///
/// It is also **not new to replace**: the editor round-trips a scene's Djot through a
/// document on every flush, so opening and editing a scene already loosens its tight
/// lists. Replace adds no damage that saving does not already do. Fixing it properly
/// means teaching the entity model about tight lists — a separate job.
#[test]
fn a_tight_list_comes_back_loose_and_that_is_known() {
    let out = round_trip("- a list item\n- another one");
    assert_eq!(
        out.trim(),
        "- a list item\n\n- another one",
        "if this now round-trips tightly, the model learned tight/loose — delete this \
         test and fold the case back into `a_batch_document_round_trips_djot`"
    );
}

/// The whole reason this type exists. `TextDocument` starts an `EventHubClient` thread
/// per document; `BatchDocument` must not — otherwise a rename touching 120 scenes
/// spawns 120 threads.
///
/// Counted from the process's own thread count rather than mocked, so it fails if a
/// future refactor quietly reintroduces the spawn.
#[test]
fn a_batch_document_costs_no_thread() {
    fn threads() -> usize {
        // Linux exposes the live thread count of the process directly.
        std::fs::read_to_string("/proc/self/status")
            .expect("/proc/self/status")
            .lines()
            .find_map(|l| l.strip_prefix("Threads:"))
            .and_then(|v| v.trim().parse().ok())
            .expect("Threads: line")
    }

    let before = threads();
    let docs: Vec<BatchDocument> = (0..32)
        .map(|i| {
            let b = BatchDocument::new().expect("BatchDocument::new");
            b.set_djot(
                &format!("Scene {i}: she called his name into the trees."),
                &DjotImportOptions::default(),
            )
            .expect("set_djot");
            b
        })
        .collect();
    let after = threads();

    assert_eq!(docs.len(), 32);
    assert!(
        after <= before + 2,
        "32 BatchDocuments started {} extra thread(s) ({before} -> {after}). \
         BatchDocument exists precisely so a batch does not pay a thread per document; \
         something reintroduced EventHubClient::start.",
        after.saturating_sub(before)
    );
}

/// Search runs against the *parsed text*, not the Djot source — so markup never matches
/// and never shifts an offset. `*Aurélien*` must be found at the position of the name,
/// not the position of the asterisk.
#[test]
fn find_all_searches_the_prose_not_the_markup() {
    let batch = BatchDocument::new().expect("BatchDocument::new");
    batch
        .set_djot(
            "She called *Aurélien* into the trees, and Aurélien did not answer.",
            &DjotImportOptions::default(),
        )
        .expect("set_djot");

    let hits = batch
        .find_all("Aurélien", &FindOptions::default())
        .expect("find_all");
    assert_eq!(hits.len(), 2, "both occurrences, including the emphasised one");

    // The first hit must sit where the NAME is in the prose ("She called " = 11 chars),
    // not where it is in the source (which the `*` would push along by one).
    assert_eq!(
        hits[0].position, 11,
        "the offset must be into the parsed text; a Djot-source offset would be 12 \
         (the emphasis marker) and would corrupt any replace built on it"
    );
    assert_eq!(hits[0].length, "Aurélien".chars().count());
}

/// Reusing one document across several imports must not accumulate the old contents —
/// a batch loop refills the same document for every scene it touches.
#[test]
fn set_djot_replaces_rather_than_appends() {
    let batch = BatchDocument::new().expect("BatchDocument::new");
    batch
        .set_djot("the first scene", &DjotImportOptions::default())
        .expect("first");
    batch
        .set_djot("the second scene", &DjotImportOptions::default())
        .expect("second");

    let out = batch.to_djot(&DjotExportOptions::default()).expect("to_djot");
    assert!(out.contains("second"), "the newest import must be present");
    assert!(
        !out.contains("first"),
        "the previous import must be gone, not appended: {out:?}"
    );
}
