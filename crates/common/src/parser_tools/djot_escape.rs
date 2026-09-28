// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Turn arbitrary plain text into Djot that renders it back verbatim.
//!
//! The inverse of [`djot_to_plain_text`](super::djot_to_plain_text), and the counterpart
//! it had been missing. The Djot exporter has always needed this — it cannot write a
//! paragraph containing `*` without the re-parse reading emphasis that the writer never
//! typed — but the two halves lived privately inside `export_djot_uc`, so anything *else*
//! holding plain text destined to become Djot had to reinvent them.
//!
//! That reinvention is the failure this module exists to prevent. A host app promoting a
//! stored plain-text field to a Djot one (a comment body, say) has to escape the values
//! already on disk, and a second, slightly-different escaper would disagree with the
//! exporter about exactly the awkward strings — a paragraph opening `- ` , a title with
//! `[brackets]`, prose about `snake_case` — while agreeing on everything easy enough to
//! notice in review.
//!
//! Two levels, because Djot has two:
//!
//! * [`escape_djot_inline`] neutralises the characters that can start *inline* markup
//!   anywhere in a line.
//! * [`guard_djot_block_start`] neutralises the markers that mean something only at the
//!   **start of a line** — a leading `#` is a heading, a leading `- ` a list item, and no
//!   amount of inline escaping reaches them.
//!
//! [`plain_text_to_djot`] composes both over every line, which is what a caller
//! converting a whole stored string wants.

/// Backslash-escape every character that can trigger Djot *inline* markup, so arbitrary
/// text survives a re-parse verbatim.
///
/// jotdown turns `\x` into an `Escape` event followed by the literal character, so
/// **over-escaping is always round-trip-safe**. That is why a markup character is escaped
/// wherever it occurs rather than only where it would open or close something: deciding
/// that means tracking Djot's inline state machine, and being wrong about it silently
/// rewrites the writer's text. The characters Djot turns into typography or symbols are
/// the exception, and are escaped only where the parser would act on them (below): their
/// rules look at a character's immediate neighbours only, and they are common enough in
/// prose that escaping every one would bury the stored text in backslashes.
///
/// # Markup, and the smart punctuation Djot applies on its own
///
/// Two families of characters are escaped:
///
/// * the ones that open or close markup: `\` `*` `_` `` ` `` `~` `^` `[` `]` `(` `)` `{`
///   `}` `|` `<`, escaped wherever they occur;
/// * the ones Djot's parser **rewrites** even though nobody asked for markup. It curls a
///   straight `'` or `"` into an English typographic quote, reads `--`, `---` and `...` as
///   an en dash, an em dash and an ellipsis, and takes `:name:` (a run of ASCII letters,
///   digits, `_`, `+` or `-` between two colons, possibly empty) as a *symbol*, which a
///   renderer is free to drop. Typed text like `10:30:45`, `std::vector`, `wrote--with`
///   or a French `"guillemet-less"` quote came back altered after one save and reload.
///
/// The second family is escaped only where it would be rewritten, so ordinary prose keeps
/// readable source: every `'` and `"`; every `-` or `.` that has a neighbour of the same
/// character (the whole run, not just enough of it to break it up); and every `:` that
/// could open or close a symbol, that is a colon with only symbol characters between it
/// and another colon. A single hyphen, a sentence's full stop, a time like `10:30` and
/// the colons of a URL (`https://example.com:8080/`) are left alone.
///
/// Block-start markers (`#`, `>`, `-`, …) are *not* covered here — they are only
/// meaningful at the start of a line, and escaping them mid-sentence would litter
/// ordinary prose with backslashes. Use [`guard_djot_block_start`] for those.
///
/// `s` is taken as a whole line. A caller writing one line in several pieces (the
/// exporter, whose formatting runs each become one piece) wants
/// [`escape_djot_inline_in_context`] instead, since `a-` and `-b` are each harmless alone
/// and one `--` side by side.
pub fn escape_djot_inline(s: &str) -> String {
    escape_djot_inline_in_context("", s, "")
}

/// [`escape_djot_inline`] for one piece of a longer line.
///
/// `before` and `after` are the text that will sit on either side of `s` once the line is
/// assembled, as plain text; only `s` is escaped and returned. The context matters for the
/// rules that look at neighbours: a `-` closing one formatting run and a `-` opening the
/// next are read as one `--`, and `10:3` in one run followed by `0:45` in the next is the
/// symbol `:30:` again. Markup the caller places between the pieces may already keep such
/// characters apart; treating them as adjacent regardless only over-escapes, which is safe.
///
/// Passing the whole line as `before` and `after` is cheap: at most the characters next to
/// `s` are read, plus the run of symbol characters next to a colon.
pub fn escape_djot_inline_in_context(before: &str, s: &str, after: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut result = String::with_capacity(s.len() + s.len() / 8);
    for (i, &c) in chars.iter().enumerate() {
        if needs_inline_escape(&chars, i, before, after) {
            result.push('\\');
        }
        result.push(c);
    }
    result
}

/// Whether `chars[i]` must be backslash-escaped, given the text around the piece.
fn needs_inline_escape(chars: &[char], i: usize, before: &str, after: &str) -> bool {
    let c = chars[i];
    match c {
        '\\' | '*' | '_' | '`' | '~' | '^' | '[' | ']' | '(' | ')' | '{' | '}' | '|' | '<'
        | '\'' | '"' => true,
        // jotdown reads any run of two or more of either as smart punctuation.
        '-' | '.' => {
            let prev = match i {
                0 => before.chars().next_back(),
                _ => Some(chars[i - 1]),
            };
            let next = chars.get(i + 1).copied().or_else(|| after.chars().next());
            prev == Some(c) || next == Some(c)
        }
        ':' => {
            let ahead = chars[i + 1..].iter().copied().chain(after.chars());
            let behind = chars[..i].iter().rev().copied().chain(before.chars().rev());
            reaches_colon_through_symbol_chars(ahead) || reaches_colon_through_symbol_chars(behind)
        }
        _ => false,
    }
}

/// Whether `run`, read away from a colon, holds only symbol characters (possibly none)
/// before the next colon, which makes the two colons a Djot symbol.
fn reaches_colon_through_symbol_chars(mut run: impl Iterator<Item = char>) -> bool {
    run.find(|c| !is_symbol_char(*c)) == Some(':')
}

/// The characters jotdown accepts in a symbol's name.
fn is_symbol_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')
}

/// Neutralise a line's leading characters so they are not parsed as a block-construct
/// marker.
///
/// Mirrors jotdown's own classification of a line, so the rules are the parser's, not a
/// guess at them:
///
/// * the block-only markers `#`, `>`, `-`, `+` and `:` (which also covers a `:::` fence),
///   escaped whenever they lead the line;
/// * an ordered-list marker followed by a space, a tab or the end of the line: a run of
///   digits, a single ASCII letter or a run of roman numerals, then `.` or `)`, optionally
///   opened by `(` (`1. `, `A. `, `iv.\t`, `B)`, `(c)`). A roman numeral is a run of one
///   case, so an ordinary lower-case word made only of `i v x l c d m` counts too:
///   `mix. ` or `civil. ` would start a list;
/// * shapes whose first character is inline markup the Djot exporter itself writes and so
///   cannot be escaped away: a strong span holding only dashes (`*-*`) is a thematic
///   break, a link or footnote reference followed by a colon (`[^1]: …`) is a definition,
///   and a struck run shaped like `key=value` filling the line (`{-debug=true-}`) is a
///   block attribute that swallows the paragraph. The colon of a definition is escaped,
///   unless the `]` before it sits inside a code span, where a backslash would be text:
///   the line then opens with an empty attribute set, `{}`, which shows nothing and
///   leaves a `{` rather than a `[` for the block parser to read;
/// * the rest of what jotdown recognises (a `*` bullet, a `|` table row, a backtick or
///   tilde fence, a `^ ` caption), for callers that pass text [`escape_djot_inline`] has
///   not seen. After inline escaping their first character is already escaped.
///
/// Leading spaces and tabs do not hide a marker from the parser (`\t- item` is a list
/// item), so they are skipped and the marker behind them is guarded. The whitespace itself
/// is left where it is, and the parser drops it from a paragraph it opens: a caller that
/// has to keep it adds [`keep_djot_edge_whitespace`], as [`plain_text_to_djot`] and the
/// Djot writer do.
///
/// For an ordered list the **delimiter** is escaped rather than the numeral: a backslash
/// before a letter or digit is a literal backslash in Djot, so escaping `1` in `1.` would
/// add a visible `\` and still leave the list marker intact, which is wrong twice over.
pub fn guard_djot_block_start(s: &str) -> String {
    let line = s.trim_start_matches(is_line_indent);
    let indent = s.len() - line.len();
    let (at, insert) = match block_marker_guard(line) {
        Some(BlockGuard::EscapeAt(at)) => (indent + at, "\\"),
        Some(BlockGuard::EmptyAttributesFirst) => (indent, "{}"),
        None => return s.to_string(),
    };
    let mut out = String::with_capacity(s.len() + insert.len());
    out.push_str(&s[..at]);
    out.push_str(insert);
    out.push_str(&s[at..]);
    out
}

/// How [`guard_djot_block_start`] keeps a line from opening a block.
enum BlockGuard {
    /// Backslash-escape the character at this byte offset of the line.
    EscapeAt(usize),
    /// Open the line with an empty attribute set, `{}`.
    EmptyAttributesFirst,
}

/// The whitespace jotdown skips before looking for a block marker.
fn is_line_indent(c: char) -> bool {
    c.is_ascii_whitespace() && c != '\n'
}

/// How to make `line` (which starts at its first non-indent character) read as a
/// paragraph, or `None` when it already does.
fn block_marker_guard(line: &str) -> Option<BlockGuard> {
    if let Some(label) = line.strip_prefix('[') {
        // `[label]:` is a definition, found by the first `]` whatever precedes it.
        let colon = 1 + label.find(']')? + 1;
        if !line[colon..].starts_with(':') {
            return None;
        }
        return Some(if inside_verbatim(line, colon - 1) {
            BlockGuard::EmptyAttributesFirst
        } else {
            BlockGuard::EscapeAt(colon)
        });
    }
    block_marker_escape_offset(line).map(BlockGuard::EscapeAt)
}

/// Whether byte `at` of `line` lies inside a verbatim (code) span, reading the line as
/// jotdown's lexer does: a run of backticks opens one and the next run of the same length
/// closes it, and outside one a backslash before ASCII punctuation or whitespace escapes
/// that character. A span left open runs to the end of the line.
fn inside_verbatim(line: &str, at: usize) -> bool {
    let bytes = line.as_bytes();
    let mut open: Option<usize> = None;
    let mut i = 0;
    while i < at {
        match bytes[i] {
            b'\\' if open.is_none() => {
                let escapes = bytes
                    .get(i + 1)
                    .is_some_and(|c| c.is_ascii_punctuation() || c.is_ascii_whitespace());
                i += if escapes { 2 } else { 1 };
            }
            b'`' => {
                let run = bytes[i..].iter().take_while(|c| **c == b'`').count();
                open = match open {
                    None => Some(run),
                    Some(fence) if fence == run => None,
                    still_open => still_open,
                };
                i += run;
            }
            _ => i += 1,
        }
    }
    open.is_some()
}

/// The byte offset in `line` (which starts at its first non-indent character) of the
/// character to backslash-escape so the line reads as a paragraph, or `None` when it
/// already does. A line opening with `[` is [`block_marker_guard`]'s.
fn block_marker_escape_offset(line: &str) -> Option<usize> {
    let first = line.chars().next()?;
    let rest = &line[first.len_utf8()..];
    match first {
        '#' | '>' | '-' | '+' | ':' => Some(0),
        '*' => {
            if is_thematic_break(line) {
                // Escape a dash rather than the `*`, which may be the opening of a strong
                // span the exporter wrote around it.
                Some(line.find('-').unwrap_or(0))
            } else if rest.is_empty() || rest.starts_with([' ', '\t']) {
                Some(0)
            } else {
                None
            }
        }
        '{' => attribute_line_escape_offset(line),
        '|' => {
            let t = line.trim_end_matches(|c: char| c.is_ascii_whitespace());
            (t.len() >= 2 && t.ends_with('|') && !t.ends_with("\\|")).then_some(0)
        }
        '`' | '~' => {
            let fence = line.chars().take_while(|c| *c == first).count();
            let spec = line[fence..].trim_matches(|c: char| c.is_ascii_whitespace());
            let valid_spec = !spec.contains(|c: char| c.is_ascii_whitespace() || c == '`');
            (fence >= 3 && valid_spec).then_some(0)
        }
        '^' => rest.starts_with(' ').then_some(0),
        _ => ordered_list_marker_escape_offset(line),
    }
}

/// jotdown's thematic break: only `-`, `*` and whitespace, with at least three of the two.
fn is_thematic_break(line: &str) -> bool {
    let mut marks = 0;
    for c in line.chars() {
        if matches!(c, '-' | '*') {
            marks += 1;
        } else if !c.is_ascii_whitespace() {
            return false;
        }
    }
    marks >= 3
}

/// Where to escape an ordered-list marker opening `line`, if it opens one.
///
/// The numeral's kind is decided by its first character, in jotdown's order: a digit, a
/// lower-case roman numeral, an upper-case one, then any other ASCII letter (which, unlike
/// the others, is exactly one character long). jotdown also caps a decimal run at 19
/// digits and a roman one at 13 characters; the cap is not applied here, which only
/// escapes a delimiter the parser would not have read as one.
fn ordered_list_marker_escape_offset(line: &str) -> Option<usize> {
    fn is_roman_lower(c: u8) -> bool {
        matches!(c, b'i' | b'v' | b'x' | b'l' | b'c' | b'd' | b'm')
    }
    fn is_roman_upper(c: u8) -> bool {
        matches!(c, b'I' | b'V' | b'X' | b'L' | b'C' | b'D' | b'M')
    }

    let bytes = line.as_bytes();
    let paren = bytes.first() == Some(&b'(');
    let start = usize::from(paren);
    let first = *bytes.get(start)?;
    let numeral = &bytes[start..];
    let numeral_len = if first.is_ascii_digit() {
        numeral.iter().take_while(|c| c.is_ascii_digit()).count()
    } else if is_roman_lower(first) {
        numeral.iter().take_while(|c| is_roman_lower(**c)).count()
    } else if is_roman_upper(first) {
        numeral.iter().take_while(|c| is_roman_upper(**c)).count()
    } else if first.is_ascii_alphabetic() {
        1
    } else {
        return None;
    };
    let delimiter_at = start + numeral_len;
    let delimiter = *bytes.get(delimiter_at)?;
    let delimited = if paren {
        delimiter == b')'
    } else {
        matches!(delimiter, b'.' | b')')
    };
    let ends_marker = bytes
        .get(delimiter_at + 1)
        .is_none_or(|c| c.is_ascii_whitespace());
    (delimited && ends_marker).then_some(if paren { 0 } else { delimiter_at })
}

/// Where to escape a line that jotdown would read, whole, as a block attribute `{…}`.
///
/// The exporter reaches this with a struck run filling a paragraph: `{-` is a deletion to
/// the inline parser but, because `-` may start an attribute key, `{-debug=true-}` is also
/// the attribute `-debug="true-"` to the block parser, which wins. Escaping the `=` that
/// ends the key makes the line invalid as an attribute and leaves the deletion intact.
/// Any other attribute line (only reachable from text that was not inline-escaped) has its
/// `{` escaped.
fn attribute_line_escape_offset(line: &str) -> Option<usize> {
    let closed_at = attribute_block_len(line)?;
    if !line[closed_at..].chars().all(|c| c.is_ascii_whitespace()) {
        return None;
    }
    Some(attribute_set_escape_offset(line))
}

/// Keep `s`, written straight after an inline attribute set (the size an image carries,
/// `![alt](src){width=600 height=900}`), from being read as more attributes of it.
///
/// jotdown reads a `{` that directly follows an attribute set as the start of another
/// set for the same element, whatever the lexer would otherwise have made of it. A struck
/// run is written `{-…-}`, and when its text is shaped like `key=value` it is also the
/// attribute `-key="value-"`: typed after an image, `x=5` struck through vanished into the
/// image's attributes at the reload. That is the inline form of the block attribute
/// [`guard_djot_block_start`] guards a whole line against, and the escape is the same one,
/// the `=` that ends the key, which leaves the deletion intact. `s` is returned unchanged
/// when it does not open with a valid attribute set.
pub fn guard_djot_attribute_continuation(s: &str) -> String {
    if attribute_block_len(s).is_none() {
        return s.to_string();
    }
    let at = attribute_set_escape_offset(s);
    let mut out = String::with_capacity(s.len() + 1);
    out.push_str(&s[..at]);
    out.push('\\');
    out.push_str(&s[at..]);
    out
}

/// Where to backslash-escape `s`, which opens with a valid attribute set, so that it no
/// longer does.
///
/// A name character right after `{` starts a key, and a key ends only at its `=`: the
/// first `=` of `s` is that one, and a backslash there is not allowed in a key. Any other
/// set has its `{` escaped.
fn attribute_set_escape_offset(s: &str) -> usize {
    let starts_key = s
        .as_bytes()
        .get(1)
        .is_some_and(|c| is_attribute_name_byte(*c));
    match s.find('=') {
        Some(eq) if starts_key => eq,
        _ => 0,
    }
}

/// The length of the attribute block `{…}` that opens `line`, if `line` opens with a
/// valid one on this line. A port of jotdown's attribute validator for a single line.
fn attribute_block_len(line: &str) -> Option<usize> {
    #[derive(Clone, Copy)]
    enum State {
        Start,
        Whitespace,
        Comment,
        ClassFirst,
        IdentifierFirst,
        Name,
        Key,
        ValueFirst,
        ValueQuoted,
        ValueEscape,
    }

    let mut state = State::Start;
    for (i, c) in line.bytes().enumerate() {
        state = match (state, c) {
            (State::Start, b'{') => State::Whitespace,
            (State::Whitespace, b'}') => return Some(i + 1),
            (State::Whitespace, b'.') => State::ClassFirst,
            (State::Whitespace, b'#') => State::IdentifierFirst,
            (State::Whitespace, b'%') => State::Comment,
            (State::Whitespace, c) if is_attribute_name_byte(c) => State::Key,
            (State::Whitespace, c) if c.is_ascii_whitespace() => State::Whitespace,
            (State::Comment, b'%') => State::Whitespace,
            (State::Comment, b'}') => return Some(i + 1),
            (State::Comment, _) => State::Comment,
            (State::ClassFirst | State::IdentifierFirst, c) if is_attribute_name_byte(c) => {
                State::Name
            }
            (State::Name, c) if is_attribute_name_byte(c) => State::Name,
            (State::Name, c) if c.is_ascii_whitespace() => State::Whitespace,
            (State::Name, b'}') => return Some(i + 1),
            (State::Key, c) if is_attribute_name_byte(c) => State::Key,
            (State::Key, b'=') => State::ValueFirst,
            (State::ValueFirst, c) if is_attribute_name_byte(c) => State::Name,
            (State::ValueFirst, b'"') => State::ValueQuoted,
            (State::ValueQuoted, b'"') => State::Whitespace,
            (State::ValueQuoted, b'\\') => State::ValueEscape,
            (State::ValueQuoted | State::ValueEscape, _) => State::ValueQuoted,
            _ => return None,
        };
    }
    None
}

/// jotdown's `attr::is_name`.
fn is_attribute_name_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b':' | b'_' | b'-')
}

/// The characters the Djot writer percent-encodes in an image's source wherever they
/// occur, as the `%XX` it writes for each (upper-case hex), and which the reader decodes
/// wherever it finds them.
///
/// An image's source is written like a link's destination, where a character the
/// inline lexer would act on cannot be written as it is (a `)` ends it, a `|` splits
/// the table cell it sits in) and a backslash escape is kept as part of it. A link
/// keeps the encoded form, which names the same resource. An image's source is the
/// key a host finds the image under, so `photo (1).png` has to come back as itself:
/// the reader decodes these, the two escapes of [`image_source_escape`] where the
/// writer uses them, and nothing else, so a source holding `%20` or `%2F` is read as
/// it always was.
///
/// An address an older version saved as it was, holding one of these escapes, reads
/// back with the character in its place. A URL parser encodes `<`, `>`, `` ` ``, `{` and
/// `}` in a path again (the WHATWG path percent-encode set), `_` and `~` mean the same
/// either way, and servers commonly decode `(`, `)`, `*`, `^` and `|` before they look
/// a file up, so the image found is usually the same one. Only `%0A` and `%0D` change
/// it, and no working address holds them.
const IMAGE_SOURCE_ESCAPES: [(char, &str); 14] = [
    ('\n', "0A"),
    ('\r', "0D"),
    ('(', "28"),
    (')', "29"),
    ('*', "2A"),
    ('<', "3C"),
    ('>', "3E"),
    ('^', "5E"),
    ('_', "5F"),
    ('`', "60"),
    ('{', "7B"),
    ('|', "7C"),
    ('}', "7D"),
    ('~', "7E"),
];

/// The character that a `%` followed by `rest` decodes to in an image's source, if the
/// `%` opens one of the escapes the writer uses there.
///
/// Those are the escapes of [`IMAGE_SOURCE_ESCAPES`], anywhere, and two more that the
/// writer uses only where it has to, which the reader decodes only there:
///
/// * `%5C` for a `\` ending the source, which would escape the closing `)`: decoded at
///   the end only.
/// * `%25` for a `%` that would otherwise read as an escape: decoded only before
///   another escape, `%2528` being a `%` written before `28`.
///
/// Older versions wrote a source as it was, and a web address for a file named
/// `100%.png` holds `%25`: decoded everywhere, it read as `100%.png`, an address no
/// server answers to, and a `%5C` inside one read as a backslash, which a browser takes
/// for a `/`.
fn image_source_escape(rest: &str) -> Option<char> {
    // Past a run of `25`, the `%` is one the writer encoded exactly when an escape
    // follows the run. The run holds no `%`, so every byte is looked at once, however
    // many escapes the source holds.
    let mut after = rest;
    let mut percent = false;
    loop {
        let hex = after.get(..2)?;
        let decoded = match IMAGE_SOURCE_ESCAPES.iter().find(|(_, code)| *code == hex) {
            Some((c, _)) => *c,
            None if hex == "5C" && after.len() == 2 => '\\',
            None if hex == "25" => {
                percent = true;
                after = &after[2..];
                continue;
            }
            None => return None,
        };
        return Some(if percent { '%' } else { decoded });
    }
}

/// Whether a `%` followed by `rest`, in an image's source, would be read back as one
/// of the escapes the writer uses there, so that the writer has to encode the `%`
/// itself for the source to come back as it is.
pub fn reads_as_image_source_escape(rest: &str) -> bool {
    image_source_escape(rest).is_some()
}

/// An image's source as Djot writes it, read back: every escape the writer uses there
/// decoded, and everything else as it is.
///
/// The writer percent-encodes, in upper-case hex, the characters the inline lexer would
/// act on in a destination (`(`, `)`, `` ` ``, `{`, `}`, `<`, `>`, a line break, a `\`
/// ending it, a `|` in a table cell, and `*`, `_`, `^` or `~` inside those marks), and a
/// `%` that would read as one of those escapes. Those are the escapes decoded here, where
/// the writer puts them (`%25` before another escape, `%5C` at the end), and no others:
/// a source holding `%20`, `%2F`, a `%25` before plain text or a `%5C` before more of the
/// source reads as it always did.
pub fn decode_djot_image_source(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        match image_source_escape(rest) {
            Some(c) => {
                out.push(c);
                rest = &rest[2..];
            }
            None => out.push('%'),
        }
    }
    out.push_str(rest);
    out
}

/// Keep the whitespace `line` opens and ends with, which the parser drops from a
/// paragraph, a heading, a list item's text and a table cell: an empty attribute set `{}`
/// before and after it shows nothing and makes the whitespace text.
///
/// `line` is the whole text of one such block, on one line of Djot, already escaped and
/// guarded. A line of whitespace alone gets both: with only the `{}` after it, the line
/// would be read as a block attribute.
pub fn keep_djot_edge_whitespace(line: &str) -> String {
    let opens = line.starts_with(|c: char| c.is_ascii_whitespace());
    let ends = line.ends_with(|c: char| c.is_ascii_whitespace());
    if !opens && !ends {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + 4);
    if opens {
        out.push_str("{}");
    }
    out.push_str(line);
    if ends {
        out.push_str("{}");
    }
    out
}

/// Convert a whole plain-text string into Djot that parses back to exactly that text.
///
/// Escapes inline markup everywhere and guards each line's own start, since a line
/// beginning `- ` is a list item wherever it sits in the string, not only in the first.
/// The whitespace a line opens or ends with is kept (see [`keep_djot_edge_whitespace`]).
///
/// # Each line becomes its own paragraph, and that is forced, not chosen
///
/// A single newline *inside* a Djot paragraph is a soft break, and
/// [`djot_to_plain_text`](super::djot_to_plain_text) collapses it to a space — so
/// emitting the lines as one paragraph loses every line ending. Blocks, meanwhile, are
/// joined by exactly one `\n` when read back. One paragraph per line is therefore the
/// only shape whose round trip is the identity, and it is also what the text it will
/// meet already means: a `.docx`/`.odt` comment body is assembled by joining its
/// paragraphs with `\n`, so each newline in such a string *is* a paragraph boundary.
///
/// # The contract, stated exactly
///
/// `djot_to_plain_text(plain_text_to_djot(s)) == s` for every `s` that contains **no
/// empty line**: no two consecutive newlines, and no leading or trailing one. A line of
/// spaces or tabs is not empty, and whitespace at either end of a line is kept.
///
/// That restriction is not a gap left open; it is how the reader treats an empty
/// paragraph. `djot_to_plain_text` joins blocks with exactly one newline and reads a
/// paragraph with nothing in it as no block at all, so it never emits two consecutive
/// newlines and no string containing an empty line can come back from it. Empty lines in
/// the input collapse, which for the paragraph-joined text this serves is a no-op. Use
/// [`needs_djot_escaping`] to find values a conversion would alter.
pub fn plain_text_to_djot(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for line in s.split('\n') {
        if line.is_empty() {
            continue;
        }
        if !first {
            out.push_str("\n\n");
        }
        first = false;
        let guarded = guard_djot_block_start(&escape_djot_inline(line));
        out.push_str(&keep_djot_edge_whitespace(&guarded));
    }
    out
}

/// Whether [`plain_text_to_djot`] would rewrite `s` at all.
///
/// For a caller migrating a stored field from plain text to Djot: a value this returns
/// `false` for is *already* legal Djot meaning exactly itself, so it can be left
/// byte-identical on disk and stays readable by an older build. Only the values this
/// returns `true` for force a rewrite — which is the distinction a format-version floor
/// should be gated on, rather than stamping every project that merely *has* comments.
///
/// Note this asks whether the **stored bytes** change, not whether meaning survives. For
/// that, see [`djot_round_trip_is_lossy`] — the two are independent, and a migration
/// generally wants both.
pub fn needs_djot_escaping(s: &str) -> bool {
    plain_text_to_djot(s) != s
}

/// Whether converting `s` to Djot and reading it back would **lose text**.
///
/// Distinct from [`needs_djot_escaping`], and not derivable from it: the escape is a pure
/// string transform, while this runs the real parse. One shape never comes back from the
/// reader, so this reports it: an **empty line**. Blocks are joined by exactly one `\n` on
/// the way back and an empty paragraph is no block, so two consecutive newlines never come
/// out, nor does one at either end. Whitespace at either end of a line is kept (see
/// [`plain_text_to_djot`]).
///
/// A migration should report the values this flags rather than rewrite them silently: the
/// text is the writer's, and quietly dropping a blank line out of someone's remark is the
/// same class of failure as quietly moving their comment.
pub fn djot_round_trip_is_lossy(s: &str) -> bool {
    use crate::parser_tools::djot_options::DjotImportOptions;
    crate::parser_tools::content_parser::djot_to_plain_text(
        &plain_text_to_djot(s),
        &DjotImportOptions::default(),
    ) != s
}

#[cfg(test)]
mod tests {
    use super::super::content_parser::djot_to_plain_text;
    use super::*;
    use crate::parser_tools::djot_options::DjotImportOptions;

    /// The contract, over the strings that actually break naive escaping.
    #[test]
    fn escaped_plain_text_parses_back_to_itself() {
        for original in [
            "plain prose, nothing special",
            "a *starred* word",
            "snake_case and more_snake_case",
            "code `backticks` here",
            "brackets [like this] and (parens)",
            "a title: The Lighthouse [Revised]",
            "# not a heading",
            "- not a list item",
            "1. not an ordered list",
            "12) also not an ordered list",
            "> not a quote",
            "+ not a list",
            ": not a definition",
            "a backslash \\ alone",
            "tilde ~sub~ and caret ^sup^",
            "braces {attr} and a pipe | here",
            "an angle <bracket>",
            "line one\nline two",
            "- leading marker\nand a second line",
            "1. first\n2. second\n3. third",
            "unicode — em dash, ellipsis …, quotes “ ”",
            "We met at 10:30:45 sharp.",
            "Use std::vector and a::b.",
            "a :smile: b",
            "::: warning",
            "He said \"hi\" and 'bye'.",
            "Pages 10--20, then---nothing... or ----",
            "I. The beginning",
            "mix. then stir",
            "A.\tIntroduction",
            "A.",
            "(c) third",
            "B) plan",
        ] {
            let djot = plain_text_to_djot(original);
            let round_tripped = djot_to_plain_text(&djot, &DjotImportOptions::default());
            assert_eq!(
                round_tripped, *original,
                "escaping {original:?} produced {djot:?}, which parsed back as \
                 {round_tripped:?} — the escape is not round-trip safe"
            );
        }
    }

    /// Text with nothing syntactic must be left byte-identical, or migrating a stored
    /// field would rewrite every ordinary value for no reason.
    #[test]
    fn ordinary_prose_is_left_untouched() {
        for plain in [
            "Just an ordinary remark.",
            "Two sentences. Both ordinary!",
            "A question? Yes.",
            "",
        ] {
            assert_eq!(plain_text_to_djot(plain), plain);
            assert!(!needs_djot_escaping(plain), "{plain:?} needs no escaping");
        }
    }

    #[test]
    fn text_with_markup_characters_is_reported_as_needing_escaping() {
        for plain in ["a *star*", "# heading-ish", "1. listish", "under_score"] {
            assert!(needs_djot_escaping(plain), "{plain:?} must need escaping");
        }
    }

    /// The documented restriction, asserted rather than left implicit: a blank line
    /// cannot survive, because `djot_to_plain_text` joins blocks with exactly one `\n`
    /// and so never emits two in a row. A caller that needs to know beforehand has
    /// [`needs_djot_escaping`].
    #[test]
    fn a_blank_line_collapses_because_no_djot_can_produce_one() {
        let round_tripped =
            djot_to_plain_text(&plain_text_to_djot("a\n\nb"), &DjotImportOptions::default());
        assert_eq!(round_tripped, "a\nb");
    }

    /// The two predicates answer different questions and neither implies the other —
    /// which is exactly why both exist. `"a\n\nb"` escapes to itself byte-for-byte (the
    /// blank line is dropped and the paragraph join puts it back), so a pure string
    /// comparison sees no change while the round trip genuinely loses a line.
    #[test]
    fn lossiness_is_not_detectable_by_string_comparison_alone() {
        assert!(
            !needs_djot_escaping("a\n\nb"),
            "the escape happens to reproduce the input byte-for-byte here"
        );
        assert!(
            djot_round_trip_is_lossy("a\n\nb"),
            "…but the round trip still loses the blank line, and a migration must be able \
             to see that"
        );
    }

    /// The parser drops a paragraph's edge whitespace, and an empty attribute set keeps
    /// it: a tab-indented line, trailing spaces, a line of whitespace alone and the other
    /// ASCII whitespace all come back as they were.
    #[test]
    fn edge_whitespace_survives_the_round_trip() {
        for original in [
            "trailing spaces are content   ",
            "\tShe opened the door.",
            "  two leading spaces",
            " \t mixed \t ",
            "   ",
            "\t",
            "\u{c}form feed\u{c}",
            "\rcarriage return\r",
            "\t- a marker behind a tab",
            "first\n\tsecond, indented\nthird  ",
            "no edge whitespace",
        ] {
            let djot = plain_text_to_djot(original);
            let round_tripped = djot_to_plain_text(&djot, &DjotImportOptions::default());
            assert_eq!(
                round_tripped, original,
                "{original:?} was written as {djot:?}"
            );
            assert!(!djot_round_trip_is_lossy(original), "{original:?}");
        }
        assert_eq!(plain_text_to_djot("\tx"), "{}\tx");
        assert_eq!(plain_text_to_djot("x  "), "x  {}");
        assert_eq!(plain_text_to_djot("  "), "{}  {}");
        assert_eq!(plain_text_to_djot("no edges"), "no edges");
    }

    /// The reader decodes `%25` only before another escape and `%5C` only at the end,
    /// where the writer puts them, and the other escapes anywhere. An address an older
    /// version saved as it was, holding `%25` or `%5C` elsewhere, reads as written.
    #[test]
    fn an_image_source_is_decoded_only_where_the_writer_encodes() {
        for (source, name) in [
            // Where the writer puts them.
            ("a%28b%29.png", "a(b).png"),
            ("a%5C", "a\\"),
            ("a\\%5C", "a\\\\"),
            ("%2528.png", "%28.png"),
            ("%255C", "%5C"),
            ("%252528", "%2528"),
            ("%%2529", "%%29"),
            // Anywhere else, as written.
            (
                "https://example.com/100%25.png",
                "https://example.com/100%25.png",
            ),
            ("a%5Cb.png", "a%5Cb.png"),
            ("%5C%5C.png", "%5C%5C.png"),
            ("%25", "%25"),
            ("%2525", "%2525"),
            ("a%20b%2F", "a%20b%2F"),
            ("a%28b", "a(b"),
            ("%2", "%2"),
            ("%", "%"),
            ("é%28é", "é(é"),
            ("%é", "%é"),
        ] {
            assert_eq!(decode_djot_image_source(source), name, "{source:?}");
        }
        // The writer encodes a `%` exactly where the reader would decode it.
        assert!(reads_as_image_source_escape("28"));
        assert!(reads_as_image_source_escape("5C"));
        assert!(reads_as_image_source_escape("2528"));
        assert!(reads_as_image_source_escape("25255C"));
        assert!(!reads_as_image_source_escape("5Cb"));
        assert!(!reads_as_image_source_escape("25"));
        assert!(!reads_as_image_source_escape("25.png"));
        assert!(!reads_as_image_source_escape("2F"));
    }

    /// Multi-line text is the shape a `.docx`/`.odt` comment body actually arrives in —
    /// its paragraphs joined with `\n` by the scanners. It must survive exactly.
    #[test]
    fn a_multi_paragraph_comment_body_round_trips() {
        let body = "First paragraph of the note.\nA second one, with *emphasis* typed literally.";
        let djot = plain_text_to_djot(body);
        assert_eq!(
            djot_to_plain_text(&djot, &DjotImportOptions::default()),
            body
        );
    }

    /// The ordered-list guard must escape the delimiter, never the digit — a backslash
    /// before a digit is a literal backslash in Djot.
    #[test]
    fn an_ordered_list_guard_escapes_the_delimiter_not_the_digit() {
        assert_eq!(guard_djot_block_start("1. text"), "1\\. text");
        assert_eq!(guard_djot_block_start("42) text"), "42\\) text");
    }

    /// The characters Djot rewrites on its own are escaped where it would rewrite them,
    /// and only there: stored text stays readable.
    #[test]
    fn smart_punctuation_and_symbols_are_escaped_only_where_the_parser_would_act() {
        for (plain, escaped) in [
            ("10:30:45", "10\\:30\\:45"),
            ("std::vector", "std\\:\\:vector"),
            (":+1:", "\\:+1\\:"),
            ("It's \"so\"", "It\\'s \\\"so\\\""),
            ("a--b---c", "a\\-\\-b\\-\\-\\-c"),
            ("Wait...", "Wait\\.\\.\\."),
            ("a..b", "a\\.\\.b"),
            // Left alone.
            ("At 10:30.", "At 10:30."),
            (
                "https://example.com:8080/path",
                "https://example.com:8080/path",
            ),
            ("Il dit : oui", "Il dit : oui"),
            ("well-known", "well-known"),
            ("End. Start.", "End. Start."),
            (":-) and :)", ":-\\) and :\\)"),
        ] {
            assert_eq!(escape_djot_inline(plain), escaped, "escaping {plain:?}");
        }
    }

    /// A piece of a line is escaped against its neighbours, since the parser sees the
    /// assembled line and not the pieces.
    #[test]
    fn a_piece_is_escaped_against_the_text_around_it() {
        assert_eq!(escape_djot_inline_in_context("a-", "-b", ""), "\\-b");
        assert_eq!(escape_djot_inline_in_context("", "a-", "-b"), "a\\-");
        assert_eq!(escape_djot_inline_in_context("Wait.", ".", ". what"), "\\.");
        assert_eq!(escape_djot_inline_in_context("", "10:3", "0:45"), "10\\:3");
        assert_eq!(escape_djot_inline_in_context("10:3", "0:45", ""), "0\\:45");
        assert_eq!(
            escape_djot_inline_in_context("std:", ":vector", ""),
            "\\:vector"
        );
        // A neighbour that breaks the run keeps the piece as it is.
        assert_eq!(escape_djot_inline_in_context("a ", "-b", ""), "-b");
        assert_eq!(escape_djot_inline_in_context("", "10:3", " 45"), "10:3");
    }

    #[test]
    fn a_letter_or_roman_numeral_list_marker_is_guarded() {
        for (line, guarded) in [
            ("A. Capital", "A\\. Capital"),
            ("I. The beginning", "I\\. The beginning"),
            ("iv.\tFourth", "iv\\.\tFourth"),
            ("mix. then stir", "mix\\. then stir"),
            ("A.", "A\\."),
            ("B) plan", "B\\) plan"),
            ("(c) third", "\\(c) third"),
            ("(12) twelve", "\\(12) twelve"),
            // Not markers: no space after the delimiter, more than one ordinary letter,
            // or mixed case in a roman numeral.
            ("Mr. Smith", "Mr. Smith"),
            ("3.14 is pi", "3.14 is pi"),
            ("Did. Done.", "Did. Done."),
            ("Mild. Very mild.", "Mild. Very mild."),
            ("ab. c", "ab. c"),
            ("(c)opyright", "(c)opyright"),
        ] {
            assert_eq!(guard_djot_block_start(line), guarded, "guarding {line:?}");
        }
    }

    /// Leading spaces and tabs do not hide a marker from the parser.
    #[test]
    fn a_marker_behind_leading_whitespace_is_guarded() {
        assert_eq!(guard_djot_block_start("\t- item"), "\t\\- item");
        assert_eq!(guard_djot_block_start("  # title"), "  \\# title");
        assert_eq!(guard_djot_block_start(" \t1.\tstep"), " \t1\\.\tstep");
        assert_eq!(guard_djot_block_start("   I. roman"), "   I\\. roman");
        assert_eq!(guard_djot_block_start("  plain"), "  plain");
    }

    /// Shapes whose first character is inline markup the exporter writes, so escaping
    /// that character would destroy the markup: the guard escapes a later one instead.
    #[test]
    fn markup_that_opens_a_block_is_guarded_without_breaking_the_markup() {
        // A strong span of dashes is a thematic break.
        assert_eq!(guard_djot_block_start("*-*"), "*\\-*");
        assert_eq!(guard_djot_block_start("*- - -*"), "*\\- - -*");
        assert_eq!(guard_djot_block_start("*bold* text"), "*bold* text");
        // A link or footnote reference followed by a colon is a definition.
        assert_eq!(guard_djot_block_start("[^1]: text"), "[^1]\\: text");
        assert_eq!(guard_djot_block_start("[a\\]: b](u)"), "[a\\]\\: b](u)");
        assert_eq!(guard_djot_block_start("[text](u): more"), "[text](u): more");
        // The `]` inside a code span: a backslash there would be text, so the line opens
        // with `{}` instead. A backtick that is escaped, or a span already closed, leaves
        // the `]` outside.
        assert_eq!(guard_djot_block_start("[`]: x`](u)"), "{}[`]: x`](u)");
        assert_eq!(
            guard_djot_block_start("  [``a`]: x``](u)"),
            "  {}[``a`]: x``](u)"
        );
        assert_eq!(guard_djot_block_start("[\\`]: x"), "[\\`]\\: x");
        assert_eq!(guard_djot_block_start("[`a`]: x"), "[`a`]\\: x");
        // A deletion filling the line and shaped like `key=value` is a block attribute.
        assert_eq!(guard_djot_block_start("{-debug=true-}"), "{-debug\\=true-}");
        assert_eq!(guard_djot_block_start("{-a=b c=d-}  "), "{-a\\=b c=d-}  ");
        assert_eq!(guard_djot_block_start("{-gone-}"), "{-gone-}");
        assert_eq!(
            guard_djot_block_start("{-a=b-} and more"),
            "{-a=b-} and more"
        );
        assert_eq!(guard_djot_block_start("{+a=b+}"), "{+a=b+}");
    }

    /// Straight after an attribute set, a `{…}` that is a valid set is read as more of it,
    /// so a struck run shaped like `key=value` is guarded at its key's `=`, and anything
    /// else is left as written.
    #[test]
    fn a_run_that_would_continue_an_attribute_set_is_guarded() {
        for (piece, guarded) in [
            ("{-x=5-}", "{-x\\=5-}"),
            ("{-=C-}", "{-\\=C-}"),
            ("{-a=b c=d-} then", "{-a\\=b c=d-} then"),
            ("{-x:y=z-}", "{-x:y\\=z-}"),
            ("{-gone-}", "{-gone-}"),
            ("{-a b-}", "{-a b-}"),
            ("{+a=b+}", "{+a=b+}"),
            ("*{-a=b-}*", "*{-a=b-}*"),
            ("plain", "plain"),
            ("", ""),
        ] {
            assert_eq!(
                guard_djot_attribute_continuation(piece),
                guarded,
                "guarding {piece:?}"
            );
        }
        for struck in ["x=5", "=C", "a=b c=d", "debug=true", "gone"] {
            let piece = guard_djot_attribute_continuation(&format!("{{-{struck}-}}"));
            let line = format!("See![plate](a.png){{width=6 height=9}}{piece}");
            let read = djot_to_plain_text(&line, &DjotImportOptions::default());
            assert!(read.ends_with(struck), "{line:?} read back as {read:?}");
        }
    }

    /// The rest of what the parser recognises, for text that was not inline-escaped.
    #[test]
    fn the_other_block_markers_are_guarded() {
        for (line, guarded) in [
            ("* bullet", "\\* bullet"),
            ("*", "\\*"),
            ("* * *", "\\* * *"),
            ("| a | b |", "\\| a | b |"),
            ("```rust", "\\```rust"),
            ("~~~", "\\~~~"),
            ("^ caption", "\\^ caption"),
            ("{.class}", "\\{.class}"),
            (":::", "\\:::"),
            ("| a", "| a"),
            ("``a`` b", "``a`` b"),
        ] {
            assert_eq!(guard_djot_block_start(line), guarded, "guarding {line:?}");
        }
    }
}
