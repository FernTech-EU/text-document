//! Image survival across import/export round trips.
//!
//! Before this, `content_parser` matched `Image` and discarded it
//! (`E::Start(C::Image(..), _) | E::End(C::Image(..)) => {}`), so every image
//! in a document died the moment that document was re-imported — and the
//! embedding application persists prose *as Djot* and reloads it with
//! `set_djot`, which means an image inserted in the editor was destroyed on
//! the next open. These tests are the gate on that path.
//!
//! Alt text is checked as carefully as the source: it must land in the image's
//! description and **not** in the paragraph's prose, or a photo's caption
//! silently becomes manuscript text — counted in word counts and matched by
//! search.

use text_document::TextDocument;

fn roundtrip_djot(source: &str) -> String {
    let doc = TextDocument::new();
    doc.set_djot_sync(source).expect("import");
    doc.to_djot().expect("export")
}

/// Plain text of a Djot source, used to prove alt text does not become prose.
fn plain_of_djot(source: &str) -> String {
    let doc = TextDocument::new();
    doc.set_djot_sync(source).expect("import");
    doc.to_plain_text().expect("plain text")
}

#[test]
fn a_djot_image_survives_a_round_trip() {
    let out = roundtrip_djot("![a black cat](assets/cat.png)\n");
    assert!(
        out.contains("assets/cat.png"),
        "the image source was dropped: {out:?}"
    );
    assert!(
        out.contains("![a black cat]"),
        "the alt text was dropped: {out:?}"
    );
}

#[test]
fn display_size_survives_a_round_trip() {
    // Djot attributes are how a resize is persisted, so they have to survive.
    let out = roundtrip_djot("![cat](assets/cat.png){width=800 height=600}\n");
    assert!(out.contains("width=800"), "width lost: {out:?}");
    assert!(out.contains("height=600"), "height lost: {out:?}");
}

#[test]
fn a_round_trip_is_a_fixpoint() {
    // Exporting what was imported, then importing that again, must not drift.
    let once = roundtrip_djot("![cat](assets/cat.png){width=800 height=600}\n");
    let twice = roundtrip_djot(&once);
    assert_eq!(once.trim(), twice.trim());
}

#[test]
fn an_image_keeps_its_place_in_a_sentence() {
    let out = roundtrip_djot("Before ![cat](c.png) after.\n");
    let before = out.find("Before").expect("before");
    let img = out.find("c.png").expect("image");
    let after = out.find("after").expect("after");
    assert!(
        before < img && img < after,
        "the image moved out of the sentence: {out:?}"
    );
}

#[test]
fn an_image_inside_emphasis_keeps_its_place() {
    // The runs/images merge has to split the formatted run around the image
    // rather than deferring it to the end of the block.
    let out = roundtrip_djot("_before ![cat](c.png) after_\n");
    let img = out.find("c.png").expect("image");
    let after = out.find("after").expect("after");
    assert!(img < after, "the image jumped past its run: {out:?}");
}

#[test]
fn alt_text_does_not_leak_into_the_prose() {
    let plain = plain_of_djot("Some prose ![a black cat](c.png) more prose.\n");
    assert!(
        !plain.contains("a black cat"),
        "alt text became manuscript prose: {plain:?}"
    );
    assert!(plain.contains("Some prose"), "prose lost: {plain:?}");
    assert!(plain.contains("more prose"), "prose lost: {plain:?}");
}

#[test]
fn an_image_contributes_exactly_one_character_to_the_document() {
    let doc = TextDocument::new();
    doc.set_djot_sync("ab![alt here](c.png)cd\n")
        .expect("import");
    // "ab" + image + "cd" = 5 logical characters, whatever the alt text says.
    // A parsed image must cost exactly what an inserted one costs, or cursor
    // positions drift the moment a document is reloaded.
    assert_eq!(doc.character_count(), 5);
    // `to_plain_text` is the *addressable* view, pinned character-for-character
    // to the document — so the image is still there as its sentinel. Dropping it
    // here would shift every search hit, cursor position and comment anchor
    // after it by one.
    assert_eq!(doc.to_plain_text().unwrap(), "ab\u{FFFC}cd");
    assert_eq!(doc.to_plain_text().unwrap().chars().count(), 5);
    // The `.txt` file is the other view, and there the image is omitted.
    assert_eq!(
        doc.to_plain_text_with(text_document::PlainTextExportOptions::presentation())
            .unwrap()
            .trim_end(),
        "abcd"
    );
}

#[test]
fn several_images_in_one_paragraph_keep_their_order() {
    let out = roundtrip_djot("![one](a.png) then ![two](b.png)\n");
    let a = out.find("a.png").expect("a");
    let b = out.find("b.png").expect("b");
    assert!(a < b, "images reordered: {out:?}");
}

#[test]
fn an_image_inside_a_blockquote_stays_quoted() {
    let out = roundtrip_djot("> quoted ![cat](c.png) here\n");
    let line = out
        .lines()
        .find(|l| l.contains("c.png"))
        .unwrap_or_else(|| panic!("image line missing in {out:?}"));
    assert!(line.starts_with('>'), "image left the blockquote: {out:?}");
}

#[test]
fn plain_text_export_omits_images_rather_than_leaking_a_sentinel() {
    // A U+FFFC in a .txt renders as an unrenderable box.
    let doc = TextDocument::new();
    doc.set_djot_sync("before ![cat](c.png) after\n").unwrap();
    let exported = doc
        .to_plain_text_with(text_document::PlainTextExportOptions::presentation())
        .expect("plain text export");
    assert!(
        !exported.contains('\u{FFFC}'),
        "the image sentinel leaked into plain text: {exported:?}"
    );
    assert!(exported.contains("before") && exported.contains("after"));
}

#[test]
fn the_addressable_view_keeps_the_image_so_offsets_do_not_drift() {
    // The one that matters for correctness elsewhere: `to_plain_text` is what
    // search, the cursor and a comment's anchor address positions in. An image
    // removed from it — but still in the document — would put every one of them
    // one character out for each picture above the point they name.
    let doc = TextDocument::new();
    doc.set_djot_sync("before ![cat](c.png) after\n").unwrap();
    let addressable = doc.to_plain_text().expect("addressable text");
    assert!(addressable.contains('\u{FFFC}'), "{addressable:?}");
    assert_eq!(
        addressable.chars().count(),
        doc.character_count(),
        "the addressable view must agree with the document, character for character"
    );
}

#[test]
fn markdown_import_keeps_the_image_instead_of_leaking_its_alt_text() {
    // pulldown-cmark emits an image's alt as an ordinary Text event, which used
    // to fall through into the paragraph while the image itself was dropped.
    let doc = TextDocument::new();
    doc.set_markdown("Some prose ![a black cat](c.png) more.\n")
        .expect("import")
        .wait();
    let plain = doc.to_plain_text().expect("plain");
    assert!(
        !plain.contains("a black cat"),
        "alt text leaked into prose: {plain:?}"
    );
    let md = doc.to_markdown().expect("export");
    assert!(md.contains("c.png"), "image dropped: {md:?}");
    assert!(md.contains("a black cat"), "alt dropped: {md:?}");
}

#[test]
fn html_import_keeps_an_img_tag() {
    // `<img>` matched none of the HTML walker's tag dispatches and was dropped
    // whole — the path a browser or Word paste travels.
    let doc = TextDocument::new();
    doc.set_html(
        "<p>before <img src=\"c.png\" alt=\"a cat\" width=\"320\" height=\"240\"> after</p>",
    )
    .expect("import")
    .wait();
    let html = doc.to_html().expect("export");
    assert!(html.contains("c.png"), "image dropped: {html:?}");
    assert!(html.contains("a cat"), "alt dropped: {html:?}");
    assert!(html.contains("320"), "width dropped: {html:?}");
}

#[test]
fn html_export_always_emits_an_alt_attribute() {
    // An <img> with no alt is inaccessible and EPUB validators reject it. An
    // explicitly empty alt is the correct way to mark a decorative image.
    let doc = TextDocument::new();
    doc.set_djot_sync("![](c.png)\n").expect("import");
    let html = doc.to_html().expect("export");
    assert!(html.contains("<img "), "no image emitted: {html:?}");
    assert!(html.contains("alt="), "no alt attribute: {html:?}");
}

// ── Inserting at the caret ──────────────────────────────────────────────
//
// A different code path from `set_djot` above, and it was completely broken:
// `insert_djot` built a `DocumentFragment`, and the fragment builder mapped
// every parsed span to `InlineContent::Text(span.text)` — which for an image
// span is the empty string. So inserting an image returned `Ok` and put
// nothing in the document at all. This is the path the editor's own
// "Insert image…" uses, and the path a paste travels.

/// The document text with an image at `offset`, inserted through the cursor.
fn insert_at(doc: &TextDocument, offset: usize, djot: &str) {
    let cursor = doc.cursor();
    cursor.set_position(offset, text_document::MoveMode::MoveAnchor);
    cursor.insert_djot(djot).expect("insert");
}

#[test]
fn inserting_an_image_at_the_caret_actually_inserts_it() {
    let doc = TextDocument::new();
    doc.set_djot_sync("ab\n").expect("import");
    insert_at(&doc, 2, "![](assets/x.png){width=10 height=10}");

    let out = doc.to_djot().expect("export");
    assert!(
        out.contains("assets/x.png"),
        "the image never arrived: {out:?}"
    );
    assert!(out.contains("width=10"), "size lost: {out:?}");
}

#[test]
fn an_inserted_image_lands_where_the_caret_is() {
    let doc = TextDocument::new();
    doc.set_djot_sync("ab\n").expect("import");
    insert_at(&doc, 1, "![a gull](assets/x.png)");

    let out = doc.to_djot().expect("export");
    let a = out.find('a').expect("a");
    let img = out.find("assets/x.png").expect("image");
    let b = out.rfind('b').expect("b");
    assert!(
        a < img && img < b,
        "the image did not land mid-word: {out:?}"
    );
}

#[test]
fn an_inserted_image_costs_the_same_as_a_parsed_one() {
    // Both paths must agree, or the caret drifts by one per image the moment a
    // document is reloaded — and every comment anchored below it drifts too.
    let inserted = TextDocument::new();
    inserted.set_djot_sync("ab\n").expect("import");
    insert_at(&inserted, 2, "![](assets/x.png){width=10 height=10}");

    let parsed = TextDocument::new();
    parsed
        .set_djot_sync("ab![](assets/x.png){width=10 height=10}\n")
        .expect("import");

    assert_eq!(
        inserted.character_count(),
        parsed.character_count(),
        "inserting an image and parsing one disagree about what it costs"
    );
    assert_eq!(
        inserted.to_plain_text().unwrap(),
        parsed.to_plain_text().unwrap()
    );
}

#[test]
fn text_after_an_inserted_image_keeps_its_own_formatting() {
    // The image occupies three bytes of the block's text. A run-builder that
    // does not step over them leaves every run after the picture three bytes
    // early: the character following it inherits the image's formatting, and
    // the tail of the block falls outside every run.
    let doc = TextDocument::new();
    doc.set_djot_sync("start\n").expect("import");
    insert_at(&doc, 5, "![](assets/x.png) _slanted_");

    let out = doc.to_djot().expect("export");
    assert!(out.contains("assets/x.png"), "{out:?}");
    assert!(
        out.contains("_slanted_"),
        "the emphasis after the image was lost or displaced: {out:?}"
    );
}

#[test]
fn inserting_an_image_twice_leaves_two_of_them() {
    let doc = TextDocument::new();
    doc.set_djot_sync("ab\n").expect("import");
    insert_at(&doc, 2, "![one](assets/a.png)");
    insert_at(&doc, doc.character_count(), "![two](assets/b.png)");

    let out = doc.to_djot().expect("export");
    let a = out.find("a.png").expect("first");
    let b = out.find("b.png").expect("second");
    assert!(a < b, "images out of order: {out:?}");
    assert_eq!(doc.character_count(), 4, "two images, two characters");
}
