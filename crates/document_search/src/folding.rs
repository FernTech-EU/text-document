//! The fold: turning a source char into the chars a *match* compares.
//!
//! A writer looking for `Aurelien` expects to find `Aurélien`; one looking for `احمد`
//! expects `أَحْمَد`; one looking for `strasse` expects `Straße`. None of those are string
//! equality, and none of them are `to_lowercase`.
//!
//! ## The pipeline, per source char
//!
//! 1. **Turkic tailoring**, if the text's language is Turkish or Azerbaijani (below).
//! 2. **Canonical caseless matching**, exactly as Unicode 3.13 defines it: `NFD → case-fold
//!    → NFD`. Case folding is not lowercasing — `ß` folds to `ss`, `ﬁ` to `fi`, `ς` and `Σ`
//!    both to `σ`.
//! 3. **Drop nonspacing marks** (`General_Category = Mn`). One rule covers Latin accents,
//!    Arabic harakat *and* Hebrew niqqud. Deliberately not the `Diacritic` property (too
//!    broad) and deliberately not `Mc` — a Devanagari vowel sign is not decoration.
//! 4. **The letter table**, for what no normalization form can reach.
//! 5. **Arabic orthographic normalization**, and the tatweel.
//!
//! Steps 3–5 are gated by `diacritic_sensitive`; step 2's fold by `case_sensitive`.
//!
//! ## Why the marks are dropped rather than the `Diacritic` property tested
//!
//! `ø ł đ ħ ı ŧ æ œ þ ð` have an **empty** canonical decomposition — the stroke is part of
//! the letter, not a combining mark, so *no* normalization form touches them. They need the
//! table in step 4, and the table is keyed on the already-case-folded char so that `Ø` and
//! `ø` reach it alike. Get that wrong and `Ørsted` is not found by `orsted`, which is the
//! kind of miss a writer never reports as a bug — they just conclude the search is broken.
//!
//! `ß` is deliberately **absent** from the table: full case folding already owns it. Having
//! it in both would mean the table always won, and the `Straße`/`strasse` test would prove
//! nothing about the case fold it was written to verify.
//!
//! ## Why Turkic runs first, before NFD
//!
//! `İ` (U+0130) canonically decomposes to `I` + COMBINING DOT ABOVE. Fold *that* under the
//! Turkic rules and the `I` becomes `ı` — the **dotless** letter, which in Turkish is a
//! different letter from `i` and means a different word. Turkish `İstanbul` would fold to
//! `ıstanbul` and never be found. The tailoring is defined on the composed letter, so it
//! must be applied to the composed letter — before anything decomposes it.
//!
//! Locale is the *only* axis case folding has (there is no Greek or Lithuanian tailoring of
//! `Case_Folding`, unlike of lower/upper/title), and `tr`/`az` is the whole of it: two code
//! points, which is the entire `T` status in `CaseFolding.txt`.

use caseless::Caseless;
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

/// COMBINING DOT ABOVE — the mark an already-decomposed `İ` carries.
const COMBINING_DOT_ABOVE: char = '\u{0307}';

/// ARABIC TATWEEL: a pure typographic stretch with no phonetic value. Dropped, so that
/// `كتاب` finds `كــتــاب`.
const TATWEEL: char = '\u{0640}';

/// The locale tailoring that changes how text folds.
///
/// Only Turkish and Azerbaijani change *folding* (the dotted/dotless i), so this is not a
/// full locale — carrying one would invite the reader to expect tailoring that does not
/// exist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FoldLocale {
    /// Untailored — the `und` root locale.
    #[default]
    Root,
    /// Turkish and Azerbaijani: `I` folds to `ı` and `İ` to `i`, and the dotless `ı` is a
    /// letter in its own right rather than an `i` wearing a missing dot.
    Turkic,
}

impl FoldLocale {
    /// Resolve a BCP-47-ish language tag.
    ///
    /// A tag that is empty, unknown or **malformed** falls back to [`FoldLocale::Root`]. It
    /// never fails: the tag comes from a writer's project settings, and a typo there must
    /// degrade to an untailored search, not break searching.
    pub fn from_tag(tag: &str) -> Self {
        let primary = tag.split(['-', '_']).next().unwrap_or("");
        if primary.eq_ignore_ascii_case("tr") || primary.eq_ignore_ascii_case("az") {
            FoldLocale::Turkic
        } else {
            FoldLocale::Root
        }
    }
}

/// What a fold is allowed to fold away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FoldSpec {
    /// `false` (the default) folds case.
    pub case_sensitive: bool,
    /// `false` (the default) folds diacritics — steps 3–5 above.
    pub diacritic_sensitive: bool,
    /// How to fold, never *whether* to. The toggles above stay global across a search; the
    /// language only decides what folding *means* in this particular scene — or the same
    /// checkbox would mean different things in different chapters of one book.
    pub locale: FoldLocale,
}

/// Letters whose canonical decomposition is **empty** — the stroke, slash or ligature is
/// part of the letter, so no normalization form will ever separate it. Sorted by code
/// point; binary-searched. Both cases are listed, because in a *case-sensitive* search
/// nothing has folded `Ø` down to `ø` by the time we get here.
///
/// `ß` is not here on purpose (see the module docs), and neither are the Arabic letters
/// that canonical decomposition *does* reach: `أ إ آ` all decompose to `ا` plus a
/// nonspacing mark, which step 3 removes.
const LETTER_FOLD: &[(char, &str)] = &[
    ('\u{00C6}', "AE"),       // Æ
    ('\u{00D0}', "D"),        // Ð
    ('\u{00D8}', "O"),        // Ø
    ('\u{00DE}', "TH"),       // Þ
    ('\u{00E6}', "ae"),       // æ
    ('\u{00F0}', "d"),        // ð
    ('\u{00F8}', "o"),        // ø
    ('\u{00FE}', "th"),       // þ
    ('\u{0110}', "D"),        // Đ
    ('\u{0111}', "d"),        // đ
    ('\u{0126}', "H"),        // Ħ
    ('\u{0127}', "h"),        // ħ
    ('\u{0131}', "i"),        // ı  — suppressed under Turkic
    ('\u{0141}', "L"),        // Ł
    ('\u{0142}', "l"),        // ł
    ('\u{0152}', "OE"),       // Œ
    ('\u{0153}', "oe"),       // œ
    ('\u{0166}', "T"),        // Ŧ
    ('\u{0167}', "t"),        // ŧ
    ('\u{0629}', "\u{0647}"), // ة teh marbuta -> ه heh
    ('\u{0649}', "\u{064A}"), // ى alef maksura -> ي yeh
    ('\u{0671}', "\u{0627}"), // ٱ alef wasla  -> ا alef  (the one alef form NFD cannot reach)
];

fn letter_fold(c: char, locale: FoldLocale) -> Option<&'static str> {
    // In Turkish and Azerbaijani the dotless `ı` is a letter, not an `i` that lost its dot.
    // Folding it to `i` here would reintroduce the very confusion the Turkic case tailoring
    // exists to prevent — `kısa` ("short") would match `kisa`.
    if locale == FoldLocale::Turkic && c == '\u{0131}' {
        return None;
    }
    LETTER_FOLD
        .binary_search_by_key(&c, |&(k, _)| k)
        .ok()
        .map(|i| LETTER_FOLD[i].1)
}

/// The tail of the pipeline: steps 3–5, applied to a char that has already been decomposed
/// and case-folded.
fn emit_folded(c: char, spec: &FoldSpec, emit: &mut impl FnMut(char)) {
    if spec.diacritic_sensitive {
        emit(c);
        return;
    }
    if c == TATWEEL || c.general_category() == GeneralCategory::NonspacingMark {
        return;
    }
    match letter_fold(c, spec.locale) {
        Some(replacement) => replacement.chars().for_each(&mut *emit),
        None => emit(c),
    }
}

/// Fold one source char, emitting the 0..n chars it contributes to the folded text.
///
/// Returns how many **source** chars were consumed — normally 1, but 2 when an
/// already-decomposed Turkish `İ` (`I` + COMBINING DOT ABOVE) was recomposed on the fly.
/// `next` is the following source char, needed only for that case.
///
/// A char can legitimately emit **nothing** (a dropped mark, a tatweel). Callers building
/// an index map must therefore not assume one entry per source char — that asymmetry is the
/// entire reason [`crate::matching::Folded`] exists.
pub(crate) fn fold_char(
    c: char,
    next: Option<char>,
    spec: &FoldSpec,
    emit: &mut impl FnMut(char),
) -> usize {
    // ASCII is the overwhelming majority of any prose — even French, even Turkish — and for
    // ASCII the whole pipeline below collapses to one branch. Every step of it is provably a
    // no-op here:
    //
    //   * no ASCII char has a canonical decomposition, so NFD is the identity;
    //   * `CaseFolding.txt`'s only ASCII entries are `A..Z -> a..z`;
    //   * no ASCII char is a nonspacing mark, none is in the letter table (whose lowest key
    //     is U+00C6), and none is the tatweel.
    //
    // The one exception is Turkish `I`, which folds to the **dotless** `ı` — so it is
    // excluded here and falls through to the tailoring below.
    //
    // This is not a micro-optimisation. Measured over a 300k-word manuscript, the general
    // path costs **172 ms** — an NFD iterator, a binary search over the ~1500-entry
    // case-folding table and a property lookup, per character of the whole novel, on every
    // keystroke. It is 15x the cost of the scan it exists to serve, and 5x the cost of
    // parsing the prose in the first place.
    //
    // It is a *pure* speedup, not an approximation: `the_fast_path_agrees_with_the_general
    // _path_on_every_char` checks the two against each other across the whole of Unicode,
    // under every combination of the toggles.
    if c.is_ascii() && !(spec.locale == FoldLocale::Turkic && c == 'I') {
        emit(if spec.case_sensitive {
            c
        } else {
            c.to_ascii_lowercase()
        });
        return 1;
    }

    fold_char_general(c, next, spec, emit)
}

/// The full pipeline, with no ASCII shortcut. Kept as its own function so the fast path can
/// be checked against it over every char in Unicode rather than trusted.
fn fold_char_general(
    c: char,
    next: Option<char>,
    spec: &FoldSpec,
    emit: &mut impl FnMut(char),
) -> usize {
    if !spec.case_sensitive && spec.locale == FoldLocale::Turkic {
        match c {
            '\u{0130}' => {
                // İ -> i. Composed, so NFD has not had a chance to break it apart.
                emit_folded('i', spec, emit);
                return 1;
            }
            'I' if next == Some(COMBINING_DOT_ABOVE) => {
                // The same İ, stored decomposed (a file that came through a system
                // normalising to NFD). Same letter, so the same answer — and the dot is
                // consumed with it rather than being left to fold on its own.
                emit_folded('i', spec, emit);
                return 2;
            }
            'I' => {
                emit_folded('\u{0131}', spec, emit);
                return 1;
            }
            _ => {}
        }
    }

    if spec.diacritic_sensitive {
        // Literal about marks, so nothing decomposes: `é` stays one char. Decomposing here
        // would make `is_identity()` a lie — a caller that trusted it and skipped the index
        // map back to the source would then read offsets into a string one char longer than
        // the one the writer typed.
        //
        // The consequence, stated: a diacritic-sensitive search is literal about the
        // *encoding* too, so a precomposed `é` and a decomposed `e`+◌́ are different text.
        // Within one document they never are — it all came through one importer — and
        // "sensitive" is precisely a request to stop being clever.
        if spec.case_sensitive {
            emit(c);
        } else {
            for folded in std::iter::once(c).default_case_fold() {
                emit_folded(folded, spec, emit);
            }
        }
        return 1;
    }

    // Canonical caseless matching: NFD -> fold -> NFD. The second NFD is not redundant —
    // folding can *produce* a composed char carrying a mark (`ǰ` -> `j` + ◌̌, `ΐ` -> `ι` +
    // ◌̈ + ◌́), and marks can only be dropped once they are visible as marks.
    for decomposed in c.nfd() {
        if spec.case_sensitive {
            emit_folded(decomposed, spec, emit);
        } else {
            for folded in std::iter::once(decomposed).default_case_fold() {
                for renormalised in folded.nfd() {
                    emit_folded(renormalised, spec, emit);
                }
            }
        }
    }
    1
}

/// Uppercase one char under the locale's rules.
///
/// Rust's `to_uppercase` is the untailored mapping, so `i` becomes `I`. In Turkish that is
/// the *wrong letter*: the uppercase of `i` is `İ`, and `I` is the uppercase of the dotless
/// `ı`. A case-preserving rename that used the untailored mapping would silently rewrite
/// Turkish prose into a different word.
pub(crate) fn to_upper(c: char, locale: FoldLocale, out: &mut String) {
    if locale == FoldLocale::Turkic {
        match c {
            'i' => {
                out.push('\u{0130}');
                return;
            }
            '\u{0131}' => {
                out.push('I');
                return;
            }
            _ => {}
        }
    }
    out.extend(c.to_uppercase());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(text: &str, spec: &FoldSpec) -> String {
        let mut out = String::new();
        let mut it = text.chars().peekable();
        while let Some(c) = it.next() {
            let consumed = fold_char(c, it.peek().copied(), spec, &mut |g| out.push(g));
            if consumed == 2 {
                it.next();
            }
        }
        out
    }

    /// The default: fold case and diacritics both.
    fn loose() -> FoldSpec {
        FoldSpec::default()
    }

    fn turkic() -> FoldSpec {
        FoldSpec {
            locale: FoldLocale::Turkic,
            ..FoldSpec::default()
        }
    }

    /// `binary_search` on an unsorted table silently returns `Err` — every letter in it
    /// would stop folding, and nothing would fail loudly.
    #[test]
    fn the_letter_table_is_sorted_and_has_no_duplicates() {
        let keys: Vec<char> = LETTER_FOLD.iter().map(|&(k, _)| k).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(keys, sorted, "LETTER_FOLD must be sorted and unique");
    }

    /// The headline: a writer types plain ASCII and finds the accented word.
    #[test]
    fn a_plain_query_folds_onto_accented_prose() {
        for (accented, plain) in [
            ("Aurélien", "aurelien"),
            ("café", "cafe"),
            ("Brønnøysund", "bronnoysund"),
            ("œuvre", "oeuvre"),
            ("Ægir", "aegir"),
            ("Þingvellir", "thingvellir"),
            ("Łódź", "lodz"),
            ("ệ", "e"),
        ] {
            assert_eq!(
                fold(accented, &loose()),
                fold(plain, &loose()),
                "{accented:?} must be found by {plain:?}"
            );
        }
    }

    /// The capital-letter case that would otherwise have shipped broken. A table keyed on
    /// the *raw* char and holding only lowercase entries lets `Ø` fall through to case
    /// folding, become `ø`, and never reach the table — so `orsted` would not find
    /// `Ørsted`, with both toggles at their defaults.
    #[test]
    fn the_letter_table_is_reached_by_the_uppercase_form_too() {
        assert_eq!(fold("Ørsted", &loose()), fold("orsted", &loose()));
        assert_eq!(fold("Ørsted", &loose()), "orsted");
    }

    /// …and in a *case-sensitive*, diacritic-folding search the case must survive the
    /// table: `Ø` becomes `O`, not `o`.
    #[test]
    fn the_letter_table_keeps_case_when_case_matters() {
        let spec = FoldSpec {
            case_sensitive: true,
            ..FoldSpec::default()
        };
        assert_eq!(fold("Ørsted", &spec), "Orsted");
        assert_eq!(fold("ÆØÅ", &spec), "AEOA");
        assert_ne!(fold("Ørsted", &spec), fold("ørsted", &spec));
    }

    /// Full case folding, which `to_lowercase` is not. This is the only configuration that
    /// actually exercises it — with diacritics folded too, several other rules could carry
    /// the same fixture and it would prove nothing.
    #[test]
    fn full_case_folding_expands_the_sharp_s() {
        let spec = FoldSpec {
            diacritic_sensitive: true,
            ..FoldSpec::default()
        };
        assert_eq!(fold("Straße", &spec), "strasse");
        assert_eq!(fold("Straße", &spec), fold("STRASSE", &spec));
        assert_eq!(fold("ﬁnal", &spec), "final");
        // Both sigmas fold together — a real bug for Greek prose under `to_lowercase`,
        // which leaves final sigma alone in some positions.
        assert_eq!(fold("ΟΔΟΣ", &spec), fold("οδος", &spec));
    }

    /// Arabic: the harakat are `Mn` and vanish with the accents; the letters no
    /// decomposition reaches are in the table; the tatweel is decoration.
    #[test]
    fn arabic_orthographic_variants_fold_together() {
        // Fully vocalised, with hamza — the marks go, and `أ` decomposes to `ا` + a mark.
        assert_eq!(fold("أَحْمَد", &loose()), fold("احمد", &loose()));
        // Alef wasla, which has NO canonical decomposition — this is the table's job.
        assert_eq!(fold("ٱلكتاب", &loose()), fold("الكتاب", &loose()));
        // Alef maksura / yeh, and teh marbuta / heh.
        assert_eq!(fold("مصطفى", &loose()), fold("مصطفي", &loose()));
        assert_eq!(fold("مدينة", &loose()), fold("مدينه", &loose()));
        // Tatweel is pure typographic stretch.
        assert_eq!(fold("كــتــاب", &loose()), fold("كتاب", &loose()));
    }

    /// Hebrew niqqud are `Mn` and fold away — but the geresh and gershayim are `Po` and
    /// carry meaning, so they must **not**.
    #[test]
    fn hebrew_niqqud_fold_but_the_geresh_survives() {
        assert_eq!(fold("שָׁלוֹם", &loose()), fold("שלום", &loose()));
        assert!(
            fold("צ\u{05F3}", &loose()).contains('\u{05F3}'),
            "the geresh is punctuation, not a diacritic"
        );
    }

    /// Turkish. `İstanbul` must be found by `istanbul`, and `kısa` must **not** be found by
    /// `kisa` — they are different words.
    #[test]
    fn turkish_keeps_the_dotted_and_dotless_i_apart() {
        assert_eq!(fold("İstanbul", &turkic()), fold("istanbul", &turkic()));
        assert_eq!(fold("KISA", &turkic()), fold("kısa", &turkic()));
        assert_ne!(
            fold("kısa", &turkic()),
            fold("kisa", &turkic()),
            "in Turkish the dotless i is a different letter"
        );
        // The untailored fold deliberately merges them: a French reader searching a
        // Turkish name should still find it.
        assert_eq!(fold("kısa", &loose()), fold("kisa", &loose()));
    }

    /// The ordering trap. `İ` decomposes to `I` + COMBINING DOT ABOVE, so folding *after*
    /// NFD would turn Turkish `İstanbul` into `ıstanbul` — the dotless letter, a different
    /// word, never found. The tailoring must run on the composed char.
    #[test]
    fn a_decomposed_turkish_capital_i_folds_the_same_as_the_composed_one() {
        let decomposed = "I\u{0307}stanbul";
        assert_eq!(
            decomposed.chars().count(),
            9,
            "the fixture must really be NFD"
        );
        assert_eq!(fold(decomposed, &turkic()), fold("İstanbul", &turkic()));
        assert_eq!(fold(decomposed, &turkic()), "istanbul");
    }

    /// Prove the order over the **whole** `F`-status set (the 104 chars whose case fold is
    /// more than one char), not the two anyone checks by hand: dropping marks after folding
    /// must give the same answer as folding an already-decomposed char.
    #[test]
    fn folding_and_mark_stripping_commute_across_the_whole_f_status_set() {
        let mut checked = 0;
        for cp in 0u32..=0x10FFFF {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let expanded: String = std::iter::once(c).default_case_fold().collect();
            if expanded.chars().count() < 2 {
                continue;
            }
            checked += 1;

            // Through the pipeline as one char...
            let direct = fold(&c.to_string(), &loose());
            // ...versus decomposing first and folding each piece.
            let piecewise: String = c
                .nfd()
                .map(|d| fold(&d.to_string(), &loose()))
                .collect::<Vec<_>>()
                .concat();
            assert_eq!(
                direct, piecewise,
                "U+{cp:04X} {c:?} folds differently depending on when it is decomposed"
            );
        }
        assert_eq!(
            checked, 104,
            "the F-status set changed size — re-check the fold against the new Unicode data"
        );
    }

    /// Diacritic-sensitive still folds *case*: the two toggles are independent.
    #[test]
    fn the_two_toggles_are_independent() {
        let dia = FoldSpec {
            diacritic_sensitive: true,
            ..FoldSpec::default()
        };
        assert_eq!(fold("CAFÉ", &dia), fold("café", &dia));
        assert_ne!(fold("cafe", &dia), fold("café", &dia));

        let case = FoldSpec {
            case_sensitive: true,
            ..FoldSpec::default()
        };
        assert_eq!(fold("cafe", &case), fold("café", &case));
        assert_ne!(fold("CAFE", &case), fold("cafe", &case));

        let strict = FoldSpec {
            case_sensitive: true,
            diacritic_sensitive: true,
            ..FoldSpec::default()
        };
        assert_eq!(fold("Café ΟΔΟΣ İ ß", &strict), "Café ΟΔΟΣ İ ß");
    }

    /// With both toggles on, the fold must be the **identity** — not merely equivalent.
    ///
    /// It is tempting to decompose anyway (canonical equivalence is free that way), and the
    /// first cut of this did. But then a source `é` becomes two chars, every offset after it
    /// shifts, and a caller that reasonably assumed "sensitive to everything" meant "the
    /// text as I typed it" reads the wrong characters back. Checked over every char that
    /// decomposes at all, not the handful anyone would think to try.
    #[test]
    fn the_strictest_fold_changes_nothing_at_all() {
        let strict = FoldSpec {
            case_sensitive: true,
            diacritic_sensitive: true,
            ..FoldSpec::default()
        };
        let mut checked = 0;
        for cp in 0u32..=0x10FFFF {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            if c.nfd().count() == 1 && std::iter::once(c).default_case_fold().count() == 1 {
                continue; // nothing here to get wrong
            }
            checked += 1;
            let s = c.to_string();
            assert_eq!(
                fold(&s, &strict),
                s,
                "U+{cp:04X} {c:?} was altered by a fold that must be the identity"
            );
        }
        assert_eq!(
            checked, 12253,
            "the count of chars that decompose or multi-fold changed — the Unicode data \
             moved under us, so re-check the fold against it"
        );
    }

    /// Turkish uppercase is a different letter, and a case-preserving rename that ignored
    /// that would silently rewrite Turkish prose into a different word.
    #[test]
    fn turkish_uppercase_keeps_the_dot() {
        let mut out = String::new();
        to_upper('i', FoldLocale::Turkic, &mut out);
        assert_eq!(out, "İ");

        let mut out = String::new();
        to_upper('\u{0131}', FoldLocale::Turkic, &mut out);
        assert_eq!(out, "I");

        let mut out = String::new();
        to_upper('i', FoldLocale::Root, &mut out);
        assert_eq!(out, "I");
    }

    /// The ASCII fast path must be a **pure speedup**, not an approximation.
    ///
    /// It skips the NFD, the case-folding table, the general-category lookup and the letter
    /// table on the grounds that all four are no-ops for ASCII. That reasoning is exactly the
    /// kind that is right until it isn't — Turkish `I` is already one exception to it — so it
    /// is checked rather than believed: every char in Unicode, under every combination of the
    /// toggles, must fold identically with and without the shortcut.
    #[test]
    fn the_fast_path_agrees_with_the_general_path_on_every_char() {
        let specs = [
            FoldSpec::default(),
            FoldSpec {
                case_sensitive: true,
                ..FoldSpec::default()
            },
            FoldSpec {
                diacritic_sensitive: true,
                ..FoldSpec::default()
            },
            FoldSpec {
                case_sensitive: true,
                diacritic_sensitive: true,
                ..FoldSpec::default()
            },
            FoldSpec {
                locale: FoldLocale::Turkic,
                ..FoldSpec::default()
            },
            FoldSpec {
                locale: FoldLocale::Turkic,
                case_sensitive: true,
                ..FoldSpec::default()
            },
        ];

        // …and with the COMBINING DOT ABOVE as the lookahead too, since that is the one case
        // where a fold consumes two source chars.
        for next in [None, Some(COMBINING_DOT_ABOVE), Some('x')] {
            for spec in &specs {
                for cp in 0u32..=0x10FFFF {
                    let Some(c) = char::from_u32(cp) else {
                        continue;
                    };

                    let (mut fast, mut general) = (String::new(), String::new());
                    let fast_consumed = fold_char(c, next, spec, &mut |g| fast.push(g));
                    let general_consumed =
                        fold_char_general(c, next, spec, &mut |g| general.push(g));

                    assert_eq!(
                        (fast, fast_consumed),
                        (general, general_consumed),
                        "U+{cp:04X} {c:?} folds differently on the fast path \
                         (spec={spec:?}, next={next:?})"
                    );
                }
            }
        }
    }

    #[test]
    fn a_malformed_language_tag_falls_back_to_the_root_locale() {
        assert_eq!(FoldLocale::from_tag("tr"), FoldLocale::Turkic);
        assert_eq!(FoldLocale::from_tag("tr-TR"), FoldLocale::Turkic);
        assert_eq!(FoldLocale::from_tag("TR_tr"), FoldLocale::Turkic);
        assert_eq!(FoldLocale::from_tag("az-Latn-AZ"), FoldLocale::Turkic);
        assert_eq!(FoldLocale::from_tag("fr-FR"), FoldLocale::Root);
        assert_eq!(FoldLocale::from_tag(""), FoldLocale::Root);
        assert_eq!(FoldLocale::from_tag("-----"), FoldLocale::Root);
        assert_eq!(FoldLocale::from_tag("nonsense!!"), FoldLocale::Root);
    }
}
