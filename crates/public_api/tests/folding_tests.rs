//! Folding, through the API a host app actually calls.
//!
//! The unit tests in `document_search::folding` pin the fold itself. These pin that it is
//! **reachable**: every option has to land in `FindOptions`, in the DTO, *and* in the three
//! `convert.rs` methods, or it is a toggle the user can flip that changes nothing. The two
//! flags added here (`diacritic_sensitive`, `language`) join `whole_word`, which spent its
//! first months in this codebase as exactly that — a dead flag.

use text_document::{
    DjotExportOptions, DjotImportOptions, FindOptions, ReplaceFormatPolicy, ReplaceOptions,
    TextDocument,
    matching::{FoldLocale, preserve_case},
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

/// The default: fold case *and* diacritics, which is what a writer means by "search".
fn loose() -> FindOptions {
    FindOptions::default()
}

/// A writer types what is on their keyboard, not what is on the page. This is the whole
/// point of the feature.
#[test]
fn a_plain_ascii_query_finds_accented_prose() {
    let doc = doc_with("Aurélien traversa la forêt. Le café était froid.");

    for (query, expected) in [
        ("aurelien", "Aurélien"),
        ("foret", "forêt"),
        ("cafe", "café"),
        ("AURELIEN", "Aurélien"),
    ] {
        let hits = doc.find_all(query, &loose()).expect("find_all");
        assert_eq!(hits.len(), 1, "{query:?} must find something");
        assert_eq!(
            hits[0].matched_text, expected,
            "{query:?} must report the ACCENTED text it actually matched"
        );
    }
}

/// …and the toggle turns it off, all the way through the DTO and back. If the flag did not
/// reach the matcher, this test would find the word anyway and pass a lie.
#[test]
fn the_diacritic_toggle_reaches_the_matcher() {
    let doc = doc_with("Le café était froid.");

    let strict = FindOptions {
        diacritic_sensitive: true,
        ..FindOptions::default()
    };
    assert!(
        doc.find_all("cafe", &strict).unwrap().is_empty(),
        "diacritic_sensitive must REFUSE the unaccented query — if this finds `café`, the \
         flag never left the DTO"
    );
    assert_eq!(doc.find_all("café", &strict).unwrap().len(), 1);
    assert_eq!(doc.find_all("cafe", &loose()).unwrap().len(), 1);
}

/// The German fixture is the only configuration that genuinely exercises *full* case folding
/// — with diacritics folded too, other rules could carry it and it would prove nothing.
#[test]
fn full_case_folding_finds_the_sharp_s() {
    let doc = doc_with("Sie wohnte in der Bahnhofstraße.");
    let strict_diacritics = FindOptions {
        diacritic_sensitive: true,
        ..FindOptions::default()
    };
    let hits = doc.find_all("strasse", &strict_diacritics).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].matched_text, "straße",
        "`ss` matched the `ß` itself"
    );
}

/// A folded letter is not divisible. `ß` folds to two chars, so a scan finds an `s` in the
/// middle of it — a position that names half a letter. Replacing there would splice into the
/// middle of a character and produce mojibake in the writer's book.
#[test]
fn a_query_cannot_replace_half_of_a_folded_letter() {
    let doc = doc_with("Bahnhofstraße");

    let hits = doc.find_all("s", &loose()).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "only the real `s` of `Bahnhofs`, never half the ß"
    );
    assert_eq!(hits[0].position, 7);

    let count = doc
        .replace_text("s", "S", true, &ReplaceOptions::new(loose()))
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        djot(&doc),
        "BahnhofStraße",
        "the ß must come through the replace intact"
    );
}

/// Arabic fuses its definite article to the noun with no separator, so no boundary rule in
/// any spec can see the seam. Whole-word `كتاب` must still find `الكتاب`.
#[test]
fn whole_word_sees_through_the_arabic_article() {
    let doc = doc_with("قرأت الكتاب ثم كتبت.");
    let whole = FindOptions {
        whole_word: true,
        ..FindOptions::default()
    };
    let hits = doc.find_all("كتاب", &whole).unwrap();
    assert_eq!(hits.len(), 1, "the article must not hide the noun");
    assert_eq!(hits[0].matched_text, "كتاب");
}

/// A bilingual manuscript: the same rename, in two scenes, under two languages — and the
/// language decides both what *matches* and what the preserved case *is*.
///
/// The Turkish scene contains `İlse` **and** `Ilse`, which in Turkish are two different
/// words (`i` and `ı` are different letters, and `I` is the capital of the dotless one). The
/// rename must take the first and leave the second — and must write `İrene`, not `Irene`,
/// because the untailored uppercase of `i` is the capital of the *other* letter. Get either
/// half wrong and the rename silently turns Turkish prose into different words.
#[test]
fn one_rename_across_a_french_scene_and_a_turkish_scene() {
    let french = doc_with("Ilse était là. ILSE cria.");
    let turkish = doc_with("İlse oradaydı. İLSE bağırdı. Ilse başka bir kelime.");

    let fr_opts = FindOptions {
        language: "fr-FR".to_string(),
        ..FindOptions::default()
    };
    let tr_opts = FindOptions {
        language: "tr-TR".to_string(),
        ..FindOptions::default()
    };

    let rename = |doc: &TextDocument, opts: &FindOptions, locale: FoldLocale| {
        doc.find_and_replace(
            "ilse",
            &ReplaceOptions::new(opts.clone())
                .with_format_policy(ReplaceFormatPolicy::PreserveIfFullyCovered),
            |matched, _| Some(preserve_case(matched, "irene", locale)),
        )
        .expect("find_and_replace")
    };

    // French: `I` is just a capital `i`, so both occurrences are the same word.
    assert_eq!(rename(&french, &fr_opts, FoldLocale::Root), 2);
    assert_eq!(djot(&french), "Irene était là. IRENE cria.");

    // Turkish: the dotted forms are the word; the dotless `Ilse` is not, and survives.
    assert_eq!(
        rename(&turkish, &tr_opts, FoldLocale::Turkic),
        2,
        "`İlse` and `İLSE` are the word — `Ilse` is a different one"
    );
    assert_eq!(
        djot(&turkish),
        "İrene oradaydı. İRENE bağırdı. Ilse başka bir kelime.",
        "Turkish uppercases `i` to `İ`, and the dotless `Ilse` must be left alone"
    );

    // The proof that the locale is what did it: untailored, the SAME Turkish sentence
    // renames all three, because `I` and `İ` both fold onto `i`.
    let untailored = doc_with("İlse oradaydı. İLSE bağırdı. Ilse başka bir kelime.");
    assert_eq!(rename(&untailored, &loose(), FoldLocale::Root), 3);
}

/// In a Turkish scene the dotted and dotless `i` are different letters, and merging them
/// turns one word into another. In every other language a reader searching a Turkish name
/// should still find it.
#[test]
fn the_language_decides_whether_two_letters_are_the_same_letter() {
    let doc = doc_with("KISA bir yol.");

    let turkish = FindOptions {
        language: "tr".to_string(),
        ..FindOptions::default()
    };
    assert_eq!(doc.find_all("kısa", &turkish).unwrap().len(), 1);
    assert!(
        doc.find_all("kisa", &turkish).unwrap().is_empty(),
        "in a Turkish scene, `kisa` is not `kısa`"
    );

    // Same prose, untailored: the two fold together.
    assert_eq!(doc.find_all("kisa", &loose()).unwrap().len(), 1);
}

/// A malformed tag must degrade to an untailored search, never fail. It comes from a
/// writer's project settings, and a typo there must not break searching.
#[test]
fn a_malformed_language_tag_still_searches() {
    let doc = doc_with("Le café était froid.");
    for tag in ["", "---", "klingon", "tr-", "!!!"] {
        let opts = FindOptions {
            language: tag.to_string(),
            ..FindOptions::default()
        };
        assert_eq!(
            doc.find_all("cafe", &opts).unwrap().len(),
            1,
            "tag {tag:?} must fall back to an untailored fold, not break the search"
        );
    }
}

/// The regex path folds diacritics too — with the same index map back to the source. An
/// option that silently meant nothing on half the calls that take it is exactly the dead
/// flag this work exists to kill.
///
/// *Case* deliberately stays with the regex engine: fold it away and a pattern like `[A-Z]`
/// would match nothing, because there would be no uppercase left for it to match.
#[test]
fn the_regex_path_honours_the_diacritic_fold() {
    let doc = doc_with("Aurélien et Aurelie.");

    let folded = FindOptions {
        use_regex: true,
        ..FindOptions::default()
    };
    let hits = doc.find_all(r"aurel\w+", &folded).unwrap();
    assert_eq!(hits.len(), 2, "the pattern runs against the FOLDED text");
    assert_eq!(
        hits[0].matched_text, "Aurélien",
        "and the offsets still address the SOURCE, accents and all"
    );
    assert_eq!(hits[1].matched_text, "Aurelie");

    let strict = FindOptions {
        use_regex: true,
        diacritic_sensitive: true,
        ..FindOptions::default()
    };
    let hits = doc.find_all(r"aurel\w+", &strict).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "diacritic-sensitive: only the unaccented one"
    );
    assert_eq!(hits[0].matched_text, "Aurelie");

    // …and the regex engine still owns case, so an uppercase class still works.
    let cased = FindOptions {
        use_regex: true,
        case_sensitive: true,
        ..FindOptions::default()
    };
    assert_eq!(
        doc.find_all(r"[A-Z]\w+", &cased).unwrap().len(),
        2,
        "`[A-Z]` must still find the capitals — folding case away would leave none"
    );
}

/// Folding runs per char over the whole document on every keystroke of a search box. It has
/// to be allocation-free per char; the first draft of this plan called `fold_string(&c
/// .to_string())`, which heap-allocates for every character in the manuscript.
#[test]
fn folding_a_large_document_stays_cheap() {
    let paragraph = "Aurélien traversa la forêt où l'ombre s'étirait, et le café refroidissait \
                     doucement sur la table de chêne.\n\n";
    let doc = doc_with(&paragraph.repeat(400));

    let started = std::time::Instant::now();
    for _ in 0..10 {
        let hits = doc.find_all("cafe", &loose()).unwrap();
        assert_eq!(hits.len(), 400);
    }
    let per_search = started.elapsed() / 10;

    assert!(
        per_search < std::time::Duration::from_millis(120),
        "a folded search over ~40k words took {per_search:?} — a per-char allocation would \
         look exactly like this, and it runs on every keystroke"
    );
}
