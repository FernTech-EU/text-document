//! Sentence boundaries — the granularity between a word and a block.
//!
//! [`TextDocument::sentence_at`](crate::TextDocument::sentence_at) and
//! [`SelectionType::SentenceUnderCursor`](crate::SelectionType) rest on this module. Like
//! `find_word_boundaries` and `SelectionType::BlockUnderCursor`, a sentence is **block-scoped**:
//! a paragraph break always ends a sentence, so the search never leaves the caret's block.
//!
//! ## Why UAX #29 alone is not enough
//!
//! [`unicode-segmentation`]'s `split_sentence_bound_indices` implements UAX #29, which already
//! gets far more right than it is usually given credit for. Its rule SB8 suppresses a break
//! after a period when a **lower-case** word follows, which covers the bulk of real
//! abbreviations for free — German `z.B. gestern`, Russian `т. д. и т. п.`, Swedish
//! `bl.a. igår` and Finnish `esim. eilen` all stay in one sentence with no help from us. Digits
//! behave the same way, so `Nr. 5`, `ca. 1920` and `S. 42` are fine too.
//!
//! Exactly one failure mode survives: **an abbreviation followed by a capitalised word**. In
//! prose that is nearly always a title before a name — `Mr. Smith`, `M. Dupont`, `Dr. Ayşe`,
//! `Sr. García`, `Sig. Rossi`, `prof. Nowak`, `κ. Παπαδόπουλος`, `د. أحمد` — plus a short tail
//! of reference abbreviations that precede a capitalised noun (`Vgl. Abb.`, `Kap. Zwei`).
//! [`Profile::abbreviations`] is that list and nothing more, which is what keeps it reviewable:
//! a term that can legitimately *end* a sentence must never appear in it. `etc.` is the
//! cautionary example — "…pears, etc. Then he left." is two sentences, so suppressing `etc.`
//! would silently weld them together.
//!
//! Two smaller corrections are punctuation, not vocabulary.
//!
//! *Spaced closing marks.* UAX #29 keeps a closing quotation mark with the sentence it closes
//! (rules SB9/SB10 admit `Close*` after a terminator), so English `?"`, German `?«` and Polish
//! `?”` all need no help. French is the exception, because it writes a space before the closing
//! guillemet: `« Vraiment ? »` strands the `»` at the head of the next sentence.
//! [`Profile::spaced_closers`] is that narrow repair, and it is deliberately per-language — `"`
//! opens as often as it closes, so a general rule here would weld `He left. "Come," she said.`
//! into one sentence.
//!
//! *Extra terminators.* Greek asks questions with `;` — an ordinary ASCII semicolon, which UAX
//! #29 quite correctly does not treat as a sentence ending. [`Profile::extra_terminators`] adds
//! it back for Greek only. (Its `·` is the Greek *semicolon* and rightly keeps not terminating.)
//!
//! A language with no profile falls back to plain UAX #29. That is a real fallback rather than a
//! stub — it mis-splits only at *title + Name*, and every other rule above still applies.
//!
//! Hebrew needs no abbreviation list at all, and its empty profile is deliberate: Hebrew
//! abbreviations end in geresh (`׳`) or gershayim (`״`), not a full stop, so UAX #29 never
//! splits them in the first place.
//!
//! [`unicode-segmentation`]: https://docs.rs/unicode-segmentation

use unicode_segmentation::UnicodeSegmentation;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Locale profiles
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// How one language tailors the UAX #29 defaults.
struct Profile {
    /// Abbreviations that do not end a sentence, **lower-cased and without the trailing
    /// period**. Only terms that essentially never end a sentence belong here — see the module
    /// docs.
    abbreviations: &'static [&'static str],
    /// Closing marks this language writes with a **space** before them, which is the one case
    /// UAX #29's own `Close*` handling cannot reach. Empty for every language but French and
    /// the ones that follow its typography.
    spaced_closers: &'static [char],
    /// Characters this language ends a sentence with that UAX #29 does not — Greek `;`.
    extra_terminators: &'static [char],
    /// Whether this language also gets [`LATIN_SHARED`], the titles most Latin-script European
    /// languages spell the same way.
    ///
    /// An explicit flag rather than something inferred from the profile's own entries. An
    /// earlier version guessed — "a profile whose entries are all ASCII-ish is Latin-script" —
    /// and was wrong three ways: `[].iter().all(..)` is vacuously **true**, so both the
    /// no-locale fallback and Hebrew (deliberately empty) silently inherited the whole list,
    /// and Serbian, which mixes both scripts in one row, was excluded from it.
    latin_titles: bool,
}

/// French (and Breton, which follows French typography) put a space — often a narrow no-break
/// one — before a closing guillemet.
const FRENCH_SPACED: &[char] = &['»', '\u{203A}'];

/// Languages that spell their titles in their own script, and so do **not** inherit
/// [`LATIN_SHARED`] — `dr`/`prof`/`st` would only add noise there. Every other language in
/// [`PROFILES`] does inherit it, as does no language at all… except the untailored fallback,
/// which by definition gets nothing.
///
/// Serbian is deliberately absent: it is written in both scripts, and its row carries both
/// spellings, so it wants the Latin titles too.
const NON_LATIN_TITLES: &[&str] = &["ru", "uk", "be", "bg", "mk", "el", "ar", "he"];

/// Languages that end sentences with something UAX #29 does not recognise. Kept apart from
/// [`PROFILES`] because exactly one language needs it, and threading a fourth column through
/// forty rows to say "none" would bury the table.
const EXTRA_TERMINATORS: &[(&str, &[char])] = &[
    // The Greek question mark is written as an ordinary semicolon; U+037E is its rarely-typed
    // canonical twin. `·` (U+0387) is Greek's *semicolon* and must keep not terminating.
    ("el", &[';', '\u{37e}']),
];

/// Titles and reference abbreviations shared by most Latin-script European languages. Merged
/// into each profile below rather than repeated in it.
const LATIN_SHARED: &[&str] = &["dr", "prof", "st", "sr", "jr", "cf", "vs", "ing", "mag"];

/// The per-language tailoring table, keyed by primary language subtag.
///
/// Adding a language is adding a row. Entries are lower-cased with the trailing period
/// stripped; a multi-word abbreviation such as `p. ex` keeps its internal spacing.
const PROFILES: &[(&str, &[&str], &[char])] = &[
    // ── Germanic ──
    (
        "en",
        &[
            "mr", "mrs", "ms", "mx", "messrs", "rev", "hon", "gen", "col", "capt", "lt", "sgt",
            "maj", "adm", "gov", "pres", "supt", "msgr", "fr", "br", "sen", "rep", "mt", "ft",
            "esq", "viz", "al",
        ],
        &[],
    ),
    (
        "de",
        &[
            "hr", "fr", "frl", "hl", "vgl", "abb", "kap", "nr", "bd", "jg", "ggf", "bspw", "sen",
            "dipl", "verw", "geb", "gest", "ehem",
        ],
        &[],
    ),
    (
        "nl",
        &[
            "dhr", "mevr", "mw", "mej", "ir", "drs", "mr", "jhr", "sint", "afb", "blz", "zgn",
        ],
        &[],
    ),
    ("da", &["hr", "fru", "frk", "skt", "jf", "afd"], &[]),
    ("sv", &["hr", "fru", "frk", "jfr", "avd", "kap"], &[]),
    ("nb", &["hr", "fru", "frk", "jf", "avd", "kap"], &[]),
    ("nn", &["hr", "fru", "frk", "jf", "avd", "kap"], &[]),
    ("no", &["hr", "fru", "frk", "jf", "avd", "kap"], &[]),
    ("is", &["hr", "frú", "sr", "bls", "sbr"], &[]),
    ("af", &["mnr", "mev", "mej", "ds", "prof"], &[]),
    // ── Romance ──
    (
        "fr",
        &[
            "m", "mm", "mme", "mmes", "mlle", "mlles", "me", "mgr", "pr", "ste", "sts", "stes",
            "vve", "p. ex", "av. j.-c", "ap. j.-c", "chap", "réf", "fig",
        ],
        FRENCH_SPACED,
    ),
    (
        "es",
        &[
            "sra", "srta", "dra", "profa", "d", "dª", "dña", "sto", "sta", "san", "lic", "arq",
            "ud", "uds", "vid", "núm", "pág",
        ],
        &[],
    ),
    (
        "pt",
        &[
            "sra", "srta", "dra", "profa", "eng", "arq", "d", "dom", "sto", "sta", "exmo", "exma",
            "pág",
        ],
        &[],
    ),
    (
        "it",
        &[
            "sig", "sigg", "sig.ra", "dott", "dott.ssa", "arch", "avv", "on", "mons", "egr",
            "spett", "geom", "rag", "pag",
        ],
        &[],
    ),
    (
        "ca",
        &["sra", "dra", "sta", "mn", "núm", "pàg", "il·lm"],
        &[],
    ),
    ("gl", &["sra", "srta", "dra", "sta", "sto", "páx"], &[]),
    ("ro", &["dl", "dna", "dra", "sf", "nr", "pag"], &[]),
    // ── Slavic (Latin script) ──
    (
        "pl",
        &["p", "pan", "pani", "inż", "mgr", "ks", "św", "hab", "red", "płk", "gen", "por", "rys"],
        &[],
    ),
    (
        "cs",
        &["p", "pí", "bc", "judr", "mudr", "phdr", "sv", "plk", "gen", "obr", "kap"],
        &[],
    ),
    ("sk", &["p", "pí", "bc", "judr", "mudr", "sv", "plk", "obr"], &[]),
    ("sl", &["g", "ga", "gdč", "sv", "št"], &[]),
    ("hr", &["g", "gđa", "gđica", "sv", "br", "sl"], &[]),
    ("bs", &["g", "gđa", "gđica", "sv", "br"], &[]),
    // Serbian is written in both scripts, so both spellings live in one profile — the match is
    // on the text, not on the tag's script subtag.
    (
        "sr",
        &["g", "gđa", "sv", "br", "г", "гђа", "др", "проф", "св", "инж"],
        &[],
    ),
    // ── Slavic (Cyrillic script) ──
    (
        "ru",
        &[
            "г", "гн", "г-н", "г-жа", "д-р", "проф", "акад", "тов", "св", "им", "ул", "пл", "обл",
            "стр", "рис", "табл", "гл", "изд",
        ],
        &[],
    ),
    (
        "uk",
        &["п", "пан", "пані", "д-р", "проф", "св", "вул", "пл", "обл", "стор", "мал", "гл"],
        &[],
    ),
    ("be", &["сп", "спн", "д-р", "праф", "св", "вул", "стар"], &[]),
    (
        "bg",
        &["г", "г-н", "г-жа", "г-ца", "д-р", "проф", "инж", "св", "ул", "пл", "стр", "фиг"],
        &[],
    ),
    (
        "mk",
        &["г", "г-дин", "г-ѓа", "д-р", "проф", "св", "ул", "стр"],
        &[],
    ),
    // ── Baltic / Finno-Ugric ──
    ("lt", &["p", "ponas", "ponia", "doc", "inž", "šv", "psl"], &[]),
    ("lv", &["k-gs", "k-dze", "doc", "inž", "sv", "lpp"], &[]),
    ("et", &["hr", "pr", "prl", "dots", "lk"], &[]),
    ("fi", &["hra", "rva", "nti", "tri", "ks", "vrt", "kuva"], &[]),
    ("hu", &["id", "ifj", "özv", "szt", "vö", "kb", "ún"], &[]),
    // ── Hellenic ──
    (
        "el",
        &["κ", "κα", "κος", "κύρ", "δρ", "καθ", "αγ", "σελ", "βλ", "εικ", "κεφ"],
        &[],
    ),
    // ── Celtic / other European ──
    ("ga", &["an t-uas", "uas", "naomh", "lch"], &[]),
    ("cy", &["athro", "sant", "santes", "tud"], &[]),
    ("gd", &["mgr", "an t-oll", "naomh"], &[]),
    ("br", &["ao", "itron", "sant", "santez"], FRENCH_SPACED),
    ("sq", &["z", "znj", "zonj", "sh", "shën", "faq"], &[]),
    ("eu", &["jn", "and", "dk", "or"], &[]),
    ("mt", &["sur", "sinjura", "san", "santa", "paġ"], &[]),
    // ── Turkic ──
    // Turkish lower-cases `İ` (U+0130) to `i` + U+0307 in Rust's locale-independent
    // `to_lowercase`, so entries that begin with it are written in that decomposed form.
    (
        "tr",
        &[
            "sn", "doç", "av", "öğr", "yrd", "alb", "gen", "hz", "bkz", "sf", "şek", "i\u{307}st",
        ],
        &[],
    ),
    // ── Semitic ──
    // Arabic honorifics and reference marks; Arabic writes its own question mark (`؟`), which
    // UAX #29 already treats as a terminator.
    ("ar", &["د", "أ", "م", "ص", "ج", "ط", "هـ"], &[]),
    // Hebrew deliberately carries no abbreviations — see the module docs.
    ("he", &[], &[]),
];

impl Profile {
    /// The profile for a BCP-47-ish tag (`en`, `en-US`, `pt_BR`, `sr-Latn`), matched on the
    /// primary language subtag. An unknown or absent tag yields the bare UAX #29 profile.
    fn for_locale(locale: Option<&str>) -> Profile {
        // No locale means no tailoring at all — plain UAX #29, including no shared titles.
        const BARE: Profile = Profile {
            abbreviations: &[],
            spaced_closers: &[],
            extra_terminators: &[],
            latin_titles: false,
        };
        let Some(tag) = locale else {
            return BARE;
        };
        let primary = tag
            .split(['-', '_'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let extra_terminators = EXTRA_TERMINATORS
            .iter()
            .find(|(lang, _)| *lang == primary)
            .map_or(&[][..], |(_, marks)| marks);
        match PROFILES.iter().find(|(lang, ..)| *lang == primary) {
            Some((_, abbreviations, spaced_closers)) => Profile {
                abbreviations,
                spaced_closers,
                extra_terminators,
                latin_titles: !NON_LATIN_TITLES.contains(&primary.as_str()),
            },
            // A language the table does not name gets plain UAX #29 — not the Latin titles,
            // which would be a guess about a script we know nothing about.
            None => Profile {
                extra_terminators,
                ..BARE
            },
        }
    }

}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// The two tailoring rules
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Whether the text ending at a candidate break finishes with a known abbreviation, so the
/// break is spurious.
fn ends_with_abbreviation(before: &str, profile: &Profile) -> bool {
    let trimmed = before.trim_end();
    let Some(stem) = trimmed.strip_suffix('.') else {
        // Only a period is ever suppressed. `?` and `!` genuinely end sentences, and no
        // abbreviation ends in one.
        return false;
    };
    let lower = stem.to_lowercase();
    let matches = |abbr: &&str| {
        if lower == *abbr {
            return true;
        }
        // Anchor on a boundary so `st` cannot fire inside `august`. A preceding space covers
        // running prose; the opening bracket and quotation marks cover `(cf` and `“Dr`.
        lower.strip_suffix(*abbr).is_some_and(|head| {
            head.ends_with([' ', '\u{a0}', '\u{202f}', '(', '[', '"', '\'', '«', '»', '“', '„'])
        })
    };
    profile.abbreviations.iter().any(matches)
        || (profile.latin_titles && LATIN_SHARED.iter().any(matches))
}

/// Whether a candidate break would strand this language's spaced closing mark at the head of
/// the next sentence, where it belongs to the one just ended.
///
/// Only the spaced case is ours to fix: UAX #29 already keeps a closer that abuts its
/// terminator, so `?"` / `?«` / `?”` never reach here.
fn strands_closing_mark(after: &str, profile: &Profile) -> bool {
    !profile.spaced_closers.is_empty()
        && after
            .chars()
            .next()
            .is_some_and(|c| profile.spaced_closers.contains(&c))
}

/// Marks that can close a quotation, for the attribution rule below. Unlike the spaced-closer
/// list this one may be general, because the rule it feeds also requires a lower-case word to
/// follow — an opening quote is never followed by one in the same breath.
const QUOTE_CLOSERS: &[char] = &['"', '\'', '»', '«', '”', '“', '’', '›', '‹', ')'];

/// Whether a candidate break would split a line of dialogue from the speech tag that follows
/// it: `"Are you sure?" he asked.`
///
/// UAX #29 breaks unconditionally after `STerm Close* Sp*` (rule SB11), unlike the period case
/// (SB8), which suppresses the break when a lower-case word follows. So `"Go home." She left.`
/// and `"Are you sure?" he asked.` are treated alike, and in fiction — where dialogue plus its
/// attribution is the commonest sentence there is — that splits nearly every quoted line in
/// two. This extends SB8's own signal, a following lower-case word, to the `?`/`!` case.
///
/// Case is the signal, so this cannot help Arabic or Hebrew, which have none. Their dialogue
/// keeps UAX #29's split; there is nothing in the text to distinguish the two readings.
fn splits_dialogue_from_attribution(before: &str, after: &str) -> bool {
    if !before.trim_end().ends_with(QUOTE_CLOSERS) {
        return false;
    }
    after
        .chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(char::is_lowercase)
}

/// Byte offsets just past this language's own terminators, each carrying any whitespace that
/// follows so the segment shape matches UAX #29's (which includes the trailing space).
fn extra_terminator_breaks(text: &str, profile: &Profile) -> Vec<usize> {
    if profile.extra_terminators.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (byte, ch) in text.char_indices() {
        if !profile.extra_terminators.contains(&ch) {
            continue;
        }
        let after = byte + ch.len_utf8();
        let run = text[after..]
            .char_indices()
            .find(|(_, c)| !c.is_whitespace())
            .map_or(text.len() - after, |(i, _)| i);
        let brk = after + run;
        if brk > 0 && brk < text.len() {
            out.push(brk);
        }
    }
    out
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// The query
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// The sentence of `text` containing `char_offset`, as block-relative **char** offsets, with
/// trailing whitespace trimmed off the end so a highlight stops at the terminator rather than
/// trailing into the gap before the next sentence.
///
/// `None` when `text` holds no sentence to point at (empty, or whitespace only).
///
/// `char_offset` may sit at the very end of `text`, which resolves to the last sentence — the
/// caret is *after* the final character, exactly as `find_word_boundaries` treats the end of
/// the last word.
pub(crate) fn sentence_bounds(
    text: &str,
    char_offset: usize,
    locale: Option<&str>,
) -> Option<(usize, usize)> {
    if text.is_empty() {
        return None;
    }
    let profile = Profile::for_locale(locale);

    // UAX #29 byte breakpoints plus this language's own terminators, minus the ones the
    // tailoring rules reject. `0` and `text.len()` bracket the list so every sentence is a
    // `[i, i+1)` window over it.
    let mut byte_breaks: Vec<usize> = vec![0];
    let extra = extra_terminator_breaks(text, &profile);
    let candidates = text
        .split_sentence_bound_indices()
        .map(|(i, _)| i)
        .chain(extra.iter().copied());
    for i in candidates {
        if i == 0 {
            continue;
        }
        if ends_with_abbreviation(&text[..i], &profile)
            || strands_closing_mark(&text[i..], &profile)
            || splits_dialogue_from_attribution(&text[..i], &text[i..])
        {
            continue;
        }
        byte_breaks.push(i);
    }
    // The extra terminators are appended out of order and may duplicate a UAX #29 break.
    byte_breaks.sort_unstable();
    byte_breaks.dedup();
    if byte_breaks.last() != Some(&text.len()) {
        byte_breaks.push(text.len());
    }

    // Byte → char offsets in ONE pass over the text. Converting each breakpoint with
    // `text[..b].chars().count()` would be quadratic, and a scene's paragraph is long enough
    // for that to matter on every keystroke.
    let mut char_breaks: Vec<usize> = Vec::with_capacity(byte_breaks.len());
    let mut next = 0usize;
    let mut chars = 0usize;
    for (byte, _) in text.char_indices() {
        while next < byte_breaks.len() && byte_breaks[next] == byte {
            char_breaks.push(chars);
            next += 1;
        }
        chars += 1;
    }
    // Whatever remains lands at the end of the text (always at least `text.len()` itself).
    while next < byte_breaks.len() {
        char_breaks.push(chars);
        next += 1;
    }

    let total = chars;
    let offset = char_offset.min(total);
    let idx = char_breaks
        .windows(2)
        .position(|w| offset >= w[0] && offset < w[1])
        // A caret at the very end belongs to the final sentence.
        .unwrap_or(char_breaks.len().saturating_sub(2));
    let (start, end) = (*char_breaks.get(idx)?, *char_breaks.get(idx + 1)?);

    trim_trailing_whitespace(text, start, end)
}

/// Pull `end` back over any trailing whitespace, so the returned range covers the sentence and
/// not the gap after it. `None` when nothing but whitespace is left.
fn trim_trailing_whitespace(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let mut trimmed = end;
    let mut it = text.chars().skip(start).take(end - start).collect::<Vec<_>>();
    while trimmed > start {
        match it.pop() {
            Some(c) if c.is_whitespace() => trimmed -= 1,
            _ => break,
        }
    }
    (trimmed > start).then_some((start, trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentences of `text`, as the caret would see them stepping through it.
    fn split(text: &str, locale: Option<&str>) -> Vec<String> {
        let total = text.chars().count();
        let mut out: Vec<String> = Vec::new();
        let mut at = 0usize;
        while at <= total {
            match sentence_bounds(text, at, locale) {
                Some((s, e)) => {
                    let piece: String = text.chars().skip(s).take(e - s).collect();
                    if out.last() != Some(&piece) {
                        out.push(piece);
                    }
                    at = e.max(at + 1);
                }
                None => at += 1,
            }
        }
        out
    }

    // ── the abbreviation rule ──

    #[test]
    fn a_title_does_not_end_a_sentence_even_before_a_capital() {
        assert_eq!(
            split("Mr. Smith went home. He was tired.", Some("en-US")),
            ["Mr. Smith went home.", "He was tired."]
        );
        assert_eq!(
            split("M. Dupont est parti. Il était tard.", Some("fr-FR")),
            ["M. Dupont est parti.", "Il était tard."]
        );
        assert_eq!(
            split("Dr. Ayşe geldi. Sonra gitti.", Some("tr")),
            ["Dr. Ayşe geldi.", "Sonra gitti."]
        );
        assert_eq!(
            split("Vino el Sr. García. Se sentó.", Some("es")),
            ["Vino el Sr. García.", "Se sentó."]
        );
        assert_eq!(
            split("Ήρθε ο κ. Παπαδόπουλος. Κάθισε.", Some("el")),
            ["Ήρθε ο κ. Παπαδόπουλος.", "Κάθισε."]
        );
        assert_eq!(
            split("قال د. أحمد شيئا. ثم صمت.", Some("ar")),
            ["قال د. أحمد شيئا.", "ثم صمت."]
        );
    }

    /// The shared Latin title list reaches a language whose own row does not repeat it.
    #[test]
    fn the_shared_latin_titles_reach_every_latin_language() {
        for (tag, text, want) in [
            ("nl", "Dhr. Jansen kwam. Hij zweeg.", "Dhr. Jansen kwam."),
            ("pl", "Przyszedł prof. Nowak. Potem wyszedł.", "Przyszedł prof. Nowak."),
            ("cs", "Přišel pan Dr. Novák. Pak odešel.", "Přišel pan Dr. Novák."),
            ("hu", "Megjött Dr. Nagy. Aztán elment.", "Megjött Dr. Nagy."),
            ("it", "È arrivato il Sig. Rossi. Poi tacque.", "È arrivato il Sig. Rossi."),
        ] {
            assert_eq!(split(text, Some(tag))[0], want, "{tag}");
        }
    }

    /// The rule that keeps the list safe: a term that can end a sentence must not be in it.
    #[test]
    fn a_terminating_abbreviation_still_ends_its_sentence() {
        assert_eq!(
            split("He bought apples, pears, etc. Then he left.", Some("en")),
            ["He bought apples, pears, etc.", "Then he left."]
        );
    }

    /// `st` must not fire inside `august`, which is what the boundary anchor is for.
    #[test]
    fn an_abbreviation_only_matches_as_a_whole_word() {
        assert_eq!(
            split("They met in august. Rain fell.", Some("en")),
            ["They met in august.", "Rain fell."]
        );
    }

    /// UAX #29 already suppresses an abbreviation before a lower-case word or a digit; these pin
    /// that we have not broken it while adding the capital-letter case.
    #[test]
    fn uax29_already_handles_lowercase_and_numeric_followers() {
        assert_eq!(
            split("Er kam um 5 Uhr, z.B. gestern. Dann ging er.", Some("de")),
            ["Er kam um 5 Uhr, z.B. gestern.", "Dann ging er."]
        );
        assert_eq!(
            split("Он пришёл в 5 ч. утра. Потом ушёл.", Some("ru")),
            ["Он пришёл в 5 ч. утра.", "Потом ушёл."]
        );
        assert_eq!(
            split("It cost 3.50 euros. Not bad.", Some("en")),
            ["It cost 3.50 euros.", "Not bad."]
        );
    }

    // ── the closing-mark rule ──

    /// UAX #29 keeps a closer that abuts its terminator, in every script. These pin that we
    /// rely on it rather than re-implementing it — an earlier draft added a general
    /// closing-mark rule here and welded `He left. "Come,"` into one sentence.
    #[test]
    fn an_abutting_closing_quote_needs_no_tailoring() {
        assert_eq!(
            split("She paused. \"Are you sure?\" he asked. Then silence.", Some("en")),
            ["She paused.", "\"Are you sure?\" he asked.", "Then silence."]
        );
        assert_eq!(
            split("Powiedział: „Naprawdę?” Potem wyszedł.", Some("pl")),
            ["Powiedział: „Naprawdę?”", "Potem wyszedł."]
        );
    }

    /// French writes a space before the closing guillemet; without the spaced rule the `»`
    /// lands at the head of the next sentence.
    #[test]
    fn french_guillemets_close_across_their_space() {
        assert_eq!(
            split("Mme Aubry hésita. « Vraiment ? » demanda-t-il. Puis le silence.", Some("fr")),
            [
                "Mme Aubry hésita.",
                "« Vraiment ? » demanda-t-il.",
                "Puis le silence."
            ]
        );
    }

    /// The commonest sentence in fiction: a quoted line plus its speech tag. UAX #29 rule SB11
    /// splits every one of them, so this is the tailoring that matters most in a novel.
    #[test]
    fn dialogue_keeps_its_speech_tag() {
        for (tag, text) in [
            ("en", "\"Are you sure?\" he asked."),
            ("en", "\"Go away!\" she shouted."),
            ("de", "»Wirklich?« fragte sie."),
            ("fr", "« Vraiment ? » demanda-t-il."),
            ("pl", "„Naprawdę?” zapytał."),
        ] {
            assert_eq!(split(text, Some(tag)), [text], "{tag}: {text}");
        }
    }

    /// …but a capitalised word after the quote really is a new sentence, and must stay one.
    /// This is the same signal UAX #29's own SB8 uses for the period case.
    #[test]
    fn a_new_sentence_after_dialogue_still_splits() {
        assert_eq!(
            split("\"Are you sure?\" He turned away.", Some("en")),
            ["\"Are you sure?\"", "He turned away."]
        );
        assert_eq!(
            split("»Wirklich?« Sie ging fort.", Some("de")),
            ["»Wirklich?«", "Sie ging fort."]
        );
    }

    /// German reverses the guillemets, so its closer abuts the terminator and needs no spaced
    /// rule — and must not gain one, or an opening quote would be swallowed.
    #[test]
    fn an_opening_quote_after_a_terminator_starts_a_new_sentence() {
        assert_eq!(
            split("Er schwieg. »Wirklich?« fragte sie.", Some("de")),
            ["Er schwieg.", "»Wirklich?« fragte sie."]
        );
        assert_eq!(
            split("He left. \"Come,\" she said.", Some("en")),
            ["He left.", "\"Come,\" she said."]
        );
    }

    // ── scripts that need no tailoring ──

    #[test]
    fn scripts_with_their_own_terminators_work_untailored() {
        assert_eq!(
            split("أين تذهب؟ لا أعرف.", Some("ar")),
            ["أين تذهب؟", "لا أعرف."]
        );
        assert_eq!(
            split("¿Adónde vas? No sé.", Some("es")),
            ["¿Adónde vas?", "No sé."]
        );
    }

    /// Greek asks questions with an ASCII semicolon, which UAX #29 rightly leaves alone — so
    /// this is the one place a terminator has to be *added* rather than suppressed.
    #[test]
    fn the_greek_question_mark_ends_a_sentence() {
        assert_eq!(
            split("Πού πηγαίνεις; Δεν ξέρω. Ίσως αύριο.", Some("el")),
            ["Πού πηγαίνεις;", "Δεν ξέρω.", "Ίσως αύριο."]
        );
        // U+037E, the canonical twin nobody types.
        assert_eq!(
            split("Πού πηγαίνεις\u{37e} Δεν ξέρω.", Some("el")),
            ["Πού πηγαίνεις\u{37e}", "Δεν ξέρω."]
        );
        // The ano teleia is Greek's *semicolon* and keeps not terminating.
        assert_eq!(
            split("Ήρθε· κάθισε.", Some("el")),
            ["Ήρθε· κάθισε."]
        );
        // …and a semicolon in any other language stays a semicolon.
        assert_eq!(
            split("He came; she left.", Some("en")),
            ["He came; she left."]
        );
    }

    /// Hebrew abbreviations end in geresh, not a period, so UAX #29 never splits them — which
    /// is why the Hebrew profile is deliberately empty.
    #[test]
    fn hebrew_abbreviations_need_no_suppression() {
        assert_eq!(
            split("הוא קרא ספרים וכו׳ ואז עצר.", Some("he")),
            ["הוא קרא ספרים וכו׳ ואז עצר."]
        );
    }

    /// Turkish `İ` lower-cases to `i` + U+0307 under Rust's locale-independent mapping, so the
    /// table stores that form. This pins the pairing.
    #[test]
    fn turkish_dotted_capital_i_matches_its_table_entry() {
        assert_eq!("İst.".to_lowercase(), "i\u{307}st.");
        assert_eq!(
            split("İst. Üniversitesi açıldı. Sonra kapandı.", Some("tr")),
            ["İst. Üniversitesi açıldı.", "Sonra kapandı."]
        );
    }

    // ── the fallback ──

    #[test]
    fn an_unknown_locale_falls_back_to_plain_uax29() {
        // Everything UAX #29 gets right on its own still works; only the tailoring is absent.
        assert_eq!(
            split("She paused. \"Are you sure?\" he asked.", Some("zz-ZZ")),
            ["She paused.", "\"Are you sure?\" he asked."]
        );
        assert_eq!(
            split("Mr. Smith went home.", Some("zz")),
            ["Mr.", "Smith went home."]
        );
        assert_eq!(split("Mr. Smith went home.", None), ["Mr.", "Smith went home."]);
    }

    /// **The shared-title list must not leak into languages that never asked for it.**
    ///
    /// An earlier version inferred "is this Latin-script?" from the profile's own entries, and
    /// `[].iter().all(..)` is vacuously true — so the untailored fallback and Hebrew (whose row
    /// is deliberately empty) both silently inherited `dr`/`prof`/`st`/…. Only `mr` happening
    /// not to be in that list kept the test above from catching it.
    #[test]
    fn the_shared_latin_titles_reach_only_the_languages_that_want_them() {
        // The untailored fallback tailors nothing at all.
        for locale in [None, Some("zz")] {
            assert_eq!(
                split("Dr. Smith went home. He left.", locale),
                ["Dr.", "Smith went home.", "He left."],
                "locale {locale:?} must get plain UAX #29"
            );
        }
        // Hebrew spells its abbreviations with geresh, so a Latin honorific is not one of its
        // titles and must not suppress a break.
        assert_eq!(
            split("Prof. Cohen arrived. He sat.", Some("he")),
            ["Prof.", "Cohen arrived.", "He sat."]
        );
        // Nor do the languages that spell their own titles in their own script.
        for tag in ["ru", "el", "ar", "bg", "uk", "mk", "be"] {
            assert_eq!(
                split("Dr. Smith went home.", Some(tag))[0],
                "Dr.",
                "{tag} spells its own titles and must not inherit the Latin ones"
            );
        }
    }

    /// Serbian is written in both scripts and its row carries both spellings, so it wants the
    /// Latin titles as well — the case the old entry-sniffing heuristic excluded.
    #[test]
    fn a_dual_script_language_still_gets_the_latin_titles() {
        assert_eq!(
            split("Dr. Novak je stigao. Onda je otišao.", Some("sr")),
            ["Dr. Novak je stigao.", "Onda je otišao."]
        );
        // …and its own Cyrillic titles keep working.
        assert_eq!(
            split("Др. Новак је стигао. Онда је отишао.", Some("sr"))[0],
            "Др. Новак је стигао."
        );
    }

    // ── shape of the returned range ──

    #[test]
    fn the_range_stops_at_the_terminator_not_the_gap_after_it() {
        let text = "One. Two.";
        // The caret inside "One." resolves to exactly "One." — the following space is not part
        // of the band.
        assert_eq!(sentence_bounds(text, 1, Some("en")), Some((0, 4)));
        // The caret in the gap still belongs to the sentence it follows.
        assert_eq!(sentence_bounds(text, 4, Some("en")), Some((0, 4)));
        assert_eq!(sentence_bounds(text, 5, Some("en")), Some((5, 9)));
    }

    #[test]
    fn a_caret_at_the_very_end_belongs_to_the_last_sentence() {
        let text = "One. Two.";
        assert_eq!(sentence_bounds(text, text.chars().count(), Some("en")), Some((5, 9)));
        // Past the end is clamped rather than panicking.
        assert_eq!(sentence_bounds(text, 9_999, Some("en")), Some((5, 9)));
    }

    #[test]
    fn empty_and_blank_text_have_no_sentence() {
        assert_eq!(sentence_bounds("", 0, Some("en")), None);
        assert_eq!(sentence_bounds("   ", 1, Some("en")), None);
    }

    #[test]
    fn a_single_sentence_block_is_one_range() {
        assert_eq!(sentence_bounds("Just the one", 5, Some("en")), Some((0, 12)));
    }

    /// Offsets are **char** offsets, so a block of multi-byte text must not report byte
    /// positions — the caret would land mid-character.
    #[test]
    fn offsets_are_char_based_not_byte_based() {
        let text = "Ééé. Ààà.";
        assert_eq!(text.len(), 15, "multi-byte on purpose");
        assert_eq!(sentence_bounds(text, 0, Some("fr")), Some((0, 4)));
        assert_eq!(sentence_bounds(text, 5, Some("fr")), Some((5, 9)));
    }
}
