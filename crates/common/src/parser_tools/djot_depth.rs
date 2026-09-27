//! What a Djot document has to stay within before `jotdown` 0.10 is given it: how deeply
//! it nests, how deep its headings go and how many lines its paragraphs run to. The one
//! door to the parser, [`parsable`], sees to all three.
//!
//! # The failure this prevents
//!
//! `jotdown` 0.10 descends once per nested block container (`parse_block` calls
//! `parse_container`, which calls `parse_block` for the container's content) and has
//! no depth limit of its own. Measured in a debug build on the 2 MiB stack a spawned
//! thread gets, it parses 491 containers nested inside one another and aborts at 492,
//! and every kind of container costs it the same: a blockquote, a list item of any
//! kind, a footnote, a div. A line of `"> ".repeat(492)` is under a kilobyte.
//!
//! That is not a panic. **A stack overflow aborts the process**: it cannot be caught
//! by `catch_unwind`, a panic hook does not run, and every unsaved document in every
//! window of the embedding application dies with it. So it cannot be handled by the
//! caller after the fact; it has to be refused before `jotdown` is handed the text at
//! all.
//!
//! The input is not always the author's own. A project bundle is mailed, shared on a
//! drive and restored from someone else's backup; an imported `.docx` comes from an
//! editor. Any of those can carry prose this crate then parses.
//!
//! # What is counted: the containers `jotdown` opens, exactly
//!
//! [`nesting_depth`] reads the text the way `jotdown`'s block pass does, one line at a
//! time and without recursing, and reports the most containers any line sits inside.
//! It keeps the containers open at the current line as a stack, outermost first, each
//! with the state `jotdown` keeps for it (`Kind` in its `block.rs`), and for each new
//! line:
//!
//! 1. asks each open container, outermost first, whether it continues on this line,
//!    with `jotdown`'s own rule (`Kind::continues`), and hands the next one in the line
//!    as this one strips it (`parse_container`: a blockquote strips its `> `, a list item
//!    or a footnote at most its marker's width of indentation, a div at most its fence's
//!    indentation). The first that does not continue closes, and every container inside
//!    it closes with it;
//! 2. if every container continued, asks the block last opened inside the innermost one
//!    (a paragraph, a heading, a code block, a table) whether the line is more of it;
//! 3. otherwise reads the line as a new block (`IdentifiedBlock::new`), and while that
//!    block is a container, reads what follows its marker on the same line as the first
//!    block inside it, so `- > 1. [^a]: x` opens four containers on one line.
//!
//! So a line that opens nothing counts nothing, however far it is indented: `jotdown`
//! identifies a block after trimming every byte of leading whitespace, and Djot has no
//! indented code block. A list nested one level per line counts one level per item,
//! however many spaces the writer puts in front of each. A line that only continues a
//! container sits exactly as deep as the containers it continues, lazily or not: a
//! paragraph line with no indentation at all still continues every list item and
//! footnote whose previous line was not blank, and a list whose items step in one
//! column a level between such lines nests one level per step.
//!
//! **Whitespace is ASCII whitespace**, the only kind the parser reads as indentation
//! or as the space that ends a marker. A paragraph opening with a hundred no-break
//! spaces is a paragraph whose text starts with them.
//!
//! A table is counted as one container, as `jotdown`'s events nest it, though its
//! cells hold no blocks and cost no recursion.
//!
//! # Why a scan rather than a limit inside the parser
//!
//! A depth limit belongs in the recursive descent itself, and this is not that.
//! `jotdown` is an external crate and its recursion is not reachable from here, so
//! what this module does instead is walk the same block structure without the
//! recursion, and refuse the input before the parser is given it. Each line costs the
//! scan what it costs `jotdown`'s own block pass: one identification per open
//! container. [`is_too_deep`] stops at the first line past the ceiling, and opens no
//! more than one container past it on that line, so a refused document costs at most
//! the ceiling's number of identifications per line.
//!
//! The scan is exact, not an estimate: its count is the nesting `jotdown` builds, which
//! the tests hold it to on generated documents of every shape the block grammar has.
//! An estimate has to err on one side, and both have already gone wrong: counting
//! indentation as nesting showed a writer's own indented paragraph as raw source, and
//! a count that forgot a list item continued by a paragraph line let a document through
//! that ends the process.
//!
//! # What callers do with it
//!
//! [`parsable`] is the one door to `jotdown`:
//! [`parse_djot`](super::content_parser::parse_djot) and the DOCX and ODT writers, which
//! walk a comment's Djot body with `jotdown` themselves, all go through it. It
//! **degrades rather than refusing**, a step at a time, each measured again by the same
//! scan:
//!
//! 1. A heading deeper than [`MAX_HEADING_LEVEL`] gets a backslash before its first `#`,
//!    which makes its line text (see *Headings* below).
//! 2. A document still nested too deeply is given shallower indentation
//!    ([`flatten_deep_indentation`]). When its depth came from indentation, the way a
//!    deeply nested list is written, it then parses, its lists flattened below a fixed
//!    depth with every item and every word. A heading the shallower indentation opens
//!    is escaped as in 1.
//! 3. A block at the top level of the document holding a paragraph, heading or caption
//!    longer than [`MAX_LEAF_LINES`] is set down as its lines (see *Long paragraphs*
//!    below). The rest of the document keeps its structure.
//! 4. [`Parsable::events`] runs the parser on a stack large enough for the document's
//!    longest paragraph.
//!
//! A document still nested too deeply once flattened is not given a structure.
//! `parse_djot` shows it as one plain paragraph for each line of its source that holds
//! text, with that line's text as it is written, and the writers set a comment body down
//! as its text. One paragraph a line is what keeps the text the same through a save: the
//! Djot writer escapes whatever each paragraph opens with, so what it saves reads back as
//! the same paragraphs. Shown as a single paragraph, the source kept its line breaks
//! through a save, read back as the same document the parser could not take, and each
//! save escaped its backslashes once more. A block set down as its lines is shown the
//! same way, one plain paragraph a line, for the same reason.
//!
//! # The limit
//!
//! [`MAX_NESTING_DEPTH`] is 128. For scale, a blockquote inside a list inside a
//! footnote inside a div is 4, and a list the editor nests fifty levels deep is 50.
//!
//! * It sits above the 96 that Skribisto, the application this crate was written for,
//!   refuses a project past when it loads one, and that guard counts at least what this
//!   one does. So an application holding its documents to 96 never has one shown as
//!   raw source here.
//! * It sits near a quarter of the 491 levels that abort a 2 MiB thread in a debug
//!   build. Measured through a whole reload (`set_djot` and a save), a document nested
//!   to the ceiling needs about 600 KiB of that stack. The rest is for the stack the
//!   caller has already used when it reaches the parse (an editor's event handling, an
//!   import's use case), which this module cannot see.
//!
//! # Headings
//!
//! A heading nests nothing, but `jotdown` 0.10 has a limit on it all the same, and one
//! that ends the process too: it keeps a heading's level (its number of `#`) in 16 bits,
//! converted with an `unwrap`, so a line of 65,536 `#` and a space panics inside the
//! parser. A host parsing a project's prose on its interface thread goes down with every
//! window. The scan reads every heading `jotdown` reads, and [`parsable`] puts a
//! backslash before the first `#` of any deeper than [`MAX_HEADING_LEVEL`]. `\#` is a
//! plain `#` in Djot, so the line reads as a line of text written in its place would,
//! its marks shown as they were written. The rest of the document keeps its structure.
//! The Djot writer escapes that `#` again when it saves the paragraph, so it reads back
//! the same.
//!
//! Such a line is a paragraph with the lines after it up to a blank line, unless the
//! block just before it goes on through a line of text. It is then more of that block:
//! more of a heading, or part of a list item, quotation or note whose last line was not
//! blank. In one of those, it goes on with the block that one ends with when that block
//! takes a line of text: a paragraph, a heading, a code block, which shows the backslash
//! too, or a table's caption, which the importer does not keep. After any other block it
//! is a paragraph of its own there.
//!
//! So escaping a heading can make a line that went on with it open a heading of its
//! own, once the escaped line has joined the list item, quotation or note before it,
//! and a line whose indentation flattening cuts can open one where it was more of a
//! list's paragraph. [`parsable`] escapes headings until it finds none.
//!
//! # Long paragraphs
//!
//! `jotdown` 0.10 recurses in a second place, the pass that reads a block's text. While
//! an inline opener waits for its closer (a `_`, a `*`, a `[`, a backtick, a quotation
//! mark, an attribute set), it reads each further line of the block one call deeper. So
//! a paragraph, heading or caption of many lines that opens one of those and does not
//! close it costs the stack a frame a line. Measured on a 2 MiB thread, a debug build
//! aborts at 730 lines (under 4 KB of text) and a release build at 3,960. It is the
//! same abort, and no count of containers sees it.
//!
//! So the scan also counts the lines of every paragraph, heading and caption, as
//! `jotdown` reads them. A code block and a link definition cost it nothing, since it
//! reads them verbatim, and each cell of a table's rows is a block of one line.
//! [`Parsable::events`] runs the parser on the caller's stack when no block is longer
//! than 128 lines, which costs less of it than a document nested to the ceiling. A
//! longer one is parsed on a thread of its own, with a stack sized from its length at
//! 8 KiB a line, nearly three times what a debug build uses.
//!
//! A block longer than [`MAX_LEAF_LINES`] is not parsed: the stack it needs would run to
//! hundreds of megabytes, and only a document made for that has one. The scan cannot
//! tell whether an opener waits in it, which takes the parser itself, so this holds
//! whether one does or not. [`parsable`] sets down as its lines the block at the top
//! level of the document that holds it (a paragraph, a whole quotation, one list item
//! with all it holds, a table with its caption): one plain paragraph for each of its
//! lines, with the line's text as it is written, and a blank line before and after. That
//! block loses its formatting, a link or note defined in it is no longer defined, and a
//! list it was an item of is split in two around it. Every other block reads as it did,
//! since a block at the top level starts on a line that closes every container. The same
//! is done, on the caller's stack, to every block longer than 128 lines when the thread
//! they need cannot be started.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::ops::Range;

/// The most nested block containers a document may declare before [`is_too_deep`]
/// reports it.
pub const MAX_NESTING_DEPTH: usize = 128;

/// The deepest heading, in `#`, a document may open before [`is_too_deep`] reports it
/// and [`parsable`] escapes it: 65,279.
///
/// `jotdown` 0.10 keeps two counts in 16 bits and converts each with an `unwrap`, so
/// either one past 65,535 panics: a heading's level, and how many blocks are open where
/// a list starts. Each heading at the top level of a document opens a section inside
/// every open section of a lower level, so headings of rising levels add up to as many
/// sections as the deepest level to that second count. The document itself and the
/// containers [`MAX_NESTING_DEPTH`] allows, each list item with the list around it, add
/// at most twice that ceiling. A document whose headings stay within this level keeps
/// both counts within 16 bits.
///
/// No other count the block parser keeps grows with the text: an ordered list's number
/// is at most 19 digits or 13 numerals, and a fence's length is only ever compared.
pub const MAX_HEADING_LEVEL: usize = u16::MAX as usize - 2 * MAX_NESTING_DEPTH;

/// The most lines one paragraph, heading or caption may run to before [`parsable`] sets
/// the block at the top level of the document holding it down as its lines: 16,384.
///
/// The parser reads each further line of such a block one call deeper while an inline
/// opener waits to close (see the module note), and [`Parsable::events`] gives it a stack
/// sized for that. At this length the stack is 130 MiB, reserved rather than used: a debug
/// build writes about 47 MiB of it, a release build about 9 MiB.
pub const MAX_LEAF_LINES: usize = 16_384;

/// The most lines a paragraph, heading or caption may run to for [`Parsable::events`] to
/// read the document on the caller's own stack. Measured through a whole reload in a
/// debug build, a block this long needs about 390 KiB of stack, where a document nested
/// to [`MAX_NESTING_DEPTH`] needs about 600 KiB.
const LEAF_LINES_ON_THE_CALLERS_STACK: usize = 128;

/// The stack [`Parsable::events`] gives the parser for a document with longer blocks,
/// before what their lines add: the stack a spawned thread gets, on which the tests show
/// a document nested to the ceiling parses.
const PARSER_STACK: usize = 2 << 20;

/// The stack [`Parsable::events`] adds for each line of the longest block: nearly three
/// times the 2.9 KiB a debug build spends on one, and fifteen times a release build's.
const STACK_PER_LEAF_LINE: usize = 8 << 10;

/// How many columns of indentation [`flatten_deep_indentation`] leaves in a line's
/// container prefix: four a level (room for markers up to `10.`) for the
/// [`MAX_LIST_INDENT`](super::list_depth::MAX_LIST_INDENT) levels an insertion keeps
/// below the top one.
pub const FLATTENED_INDENT_COLUMNS: usize = 4 * super::list_depth::MAX_LIST_INDENT as usize;

/// The most block containers any line of `text` sits inside, as `jotdown` 0.10 nests
/// them.
///
/// See the module note for what is counted. The scan is a loop over the lines, never a
/// recursion.
pub fn nesting_depth(text: &str) -> usize {
    deepest(text, usize::MAX)
}

/// Whether `text` nests deeper than [`MAX_NESTING_DEPTH`], the most `jotdown` is given,
/// or opens a heading deeper than [`MAX_HEADING_LEVEL`], the deepest it is given: whether
/// the parser can be handed `text` as it is.
///
/// Stops at the first line past either. It does not measure the length of a paragraph,
/// which decides the stack the parser needs rather than whether it can have the text:
/// hand the text to the parser through [`parsable`], which sees to that as well.
pub fn is_too_deep(text: &str) -> bool {
    let reach = reach(text, MAX_NESTING_DEPTH, MAX_HEADING_LEVEL, usize::MAX);
    reach.depth > MAX_NESTING_DEPTH || reach.heading > MAX_HEADING_LEVEL
}

/// The number of lines in the longest paragraph, heading or caption of `text`, as
/// `jotdown` 0.10 reads them: how many calls deep it may read that block's text.
pub fn longest_leaf_lines(text: &str) -> usize {
    reach(text, usize::MAX, usize::MAX, usize::MAX).longest_leaf
}

/// `text` made ready for `jotdown` 0.10, or `None` when its structure cannot be computed
/// without ending the process.
///
/// A heading deeper than [`MAX_HEADING_LEVEL`] gets a backslash before its first `#`, a
/// document nested deeper than [`MAX_NESTING_DEPTH`] is flattened
/// ([`flatten_deep_indentation`]), and a block at the top level of the document holding a
/// paragraph, heading or caption longer than [`MAX_LEAF_LINES`] is set down as its lines,
/// one plain paragraph each, the rest of the document as it was. Each is measured again.
/// What is still nested too deeply once flattened is `None`. Text within every limit is
/// borrowed as it is, measured once.
///
/// Read what it returns through [`Parsable::events`], which gives the parser the stack
/// its longest block needs.
pub fn parsable(text: &str) -> Option<Parsable<'_>> {
    let mut text = Cow::Borrowed(text);
    let mut flattened = false;
    let mut set_down = false;
    // Each round escapes at least one run of more than MAX_HEADING_LEVEL `#` that no
    // round escaped before, or flattens, once, or sets long blocks down as their lines,
    // once. A run once escaped stays text, since flattening only takes out whitespace
    // and setting down escapes more, and each run takes that many bytes of the text. So
    // the rounds are at most three more than the runs the text can hold, and they end.
    // Any round can find headings to escape, not only the first: a line that went on
    // with a heading escaped before it can open one of its own, and so can a line whose
    // indentation flattening cut.
    loop {
        let reach = reach(&text, MAX_NESTING_DEPTH, usize::MAX, MAX_LEAF_LINES);
        if !reach.overlong_headings.is_empty() {
            text = Cow::Owned(escape_at(&text, &reach.overlong_headings)?);
        } else if reach.depth > MAX_NESTING_DEPTH {
            if flattened {
                return None;
            }
            text = Cow::Owned(flatten_deep_indentation(&text));
            flattened = true;
        } else if !reach.long_blocks.is_empty() {
            // A block set down holds paragraphs of one line, and nothing around it
            // changes, so a second round never finds one.
            if set_down {
                return None;
            }
            text = Cow::Owned(set_down_as_lines(&text, &reach.long_blocks)?);
            set_down = true;
        } else {
            return Some(Parsable {
                text,
                longest_leaf: reach.longest_leaf,
                on_the_callers_stack: OnceCell::new(),
            });
        }
    }
}

/// Djot text `jotdown` 0.10 can be given, made by [`parsable`], and the length of its
/// longest paragraph, heading or caption, which decides the stack the parser needs.
#[derive(Debug)]
pub struct Parsable<'a> {
    text: Cow<'a, str>,
    longest_leaf: usize,
    /// `text` with every block too long to read on the caller's stack set down as its
    /// lines: made the first time [`Parsable::events`] cannot start the thread it needs.
    on_the_callers_stack: OnceCell<Option<String>>,
}

/// How [`Parsable::events`] runs the parser over all of a text on a thread with a stack
/// of the given size: the events, `None` if the parser panicked, or why the thread could
/// not be started.
type ReadOnAThread = for<'t> fn(&'t str, usize) -> std::io::Result<Option<Vec<jotdown::Event<'t>>>>;

impl Parsable<'_> {
    /// The text the parser is given: the source, with whatever [`parsable`] changed in it.
    ///
    /// When [`events`](Self::events) cannot start the thread a long block needs, the
    /// parser reads this text with those blocks set down as their lines.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The events `jotdown` reads from [`text`](Self::text), in order.
    ///
    /// When no paragraph, heading or caption is longer than 128 lines, the parser reads
    /// them as they are asked for, on the caller's stack. Otherwise it reads them all at
    /// once, on a thread with a stack sized for the longest (see the module note).
    ///
    /// If that thread cannot be started (the system refuses it a thread or the memory for
    /// its stack), every block at the top level holding one of those longer paragraphs,
    /// headings or captions is set down as its lines, one plain paragraph each, and the
    /// parser reads the rest as it is written, on the caller's stack. Only those blocks
    /// lose their formatting, and a host that saves what it shows keeps that loss.
    ///
    /// `None` only if the parser panicked on that thread. A caller has nothing left to
    /// read the text's structure with then, and shows it as its lines.
    pub fn events(&self) -> Option<DjotEvents<'_>> {
        self.events_read_by(read_on_a_thread)
    }

    /// [`events`](Self::events), with `read` running the parser on a thread of its own
    /// when a block is too long for the caller's stack.
    fn events_read_by(&self, read: ReadOnAThread) -> Option<DjotEvents<'_>> {
        let text: &str = &self.text;
        if self.longest_leaf <= LEAF_LINES_ON_THE_CALLERS_STACK {
            return Some(DjotEvents::streamed(text));
        }
        let stack = PARSER_STACK + self.longest_leaf * STACK_PER_LEAF_LINE;
        match read(text, stack) {
            Ok(events) => events.map(|events| DjotEvents(EventSource::Read(events.into_iter()))),
            Err(error) => {
                log::warn!(
                    "no thread with a {stack}-byte stack for a Djot block of {} lines, so every \
                     block longer than {LEAF_LINES_ON_THE_CALLERS_STACK} lines is read as its \
                     lines: {error}",
                    self.longest_leaf
                );
                self.on_the_callers_stack
                    .get_or_init(|| set_down_long_blocks(text, LEAF_LINES_ON_THE_CALLERS_STACK))
                    .as_deref()
                    .map(DjotEvents::streamed)
            }
        }
    }
}

/// Run the parser over all of `text` on a thread of its own with a `stack`-byte stack: its
/// events, `None` if it panicked, or why the thread could not be started.
fn read_on_a_thread(text: &str, stack: usize) -> std::io::Result<Option<Vec<jotdown::Event<'_>>>> {
    std::thread::scope(|scope| {
        let parser = std::thread::Builder::new()
            .name("djot parser".to_owned())
            .stack_size(stack)
            .spawn_scoped(scope, || jotdown::Parser::new(text).collect::<Vec<_>>())?;
        Ok(parser.join().ok())
    })
}

/// The events of a [`Parsable`] text, from [`Parsable::events`].
pub struct DjotEvents<'s>(EventSource<'s>);

/// Where [`DjotEvents`] takes its events from.
enum EventSource<'s> {
    /// The parser, reading them as they are asked for.
    Streamed(Box<jotdown::Parser<'s>>),
    /// The events the parser read in advance, on a stack large enough for them.
    Read(std::vec::IntoIter<jotdown::Event<'s>>),
}

impl<'s> DjotEvents<'s> {
    /// The events of `text`, read by the parser as they are asked for.
    fn streamed(text: &'s str) -> Self {
        DjotEvents(EventSource::Streamed(Box::new(jotdown::Parser::new(text))))
    }
}

impl<'s> Iterator for DjotEvents<'s> {
    type Item = jotdown::Event<'s>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            EventSource::Streamed(parser) => parser.next(),
            EventSource::Read(events) => events.next(),
        }
    }
}

/// `text` with a backslash put before the byte at each of `offsets`, which ascend. `None`
/// if one of them is not at a character's start.
fn escape_at(text: &str, offsets: &[usize]) -> Option<String> {
    let mut out = String::with_capacity(text.len() + offsets.len());
    let mut from = 0;
    for &offset in offsets {
        out.push_str(text.get(from..offset)?);
        out.push('\\');
        from = offset;
    }
    out.push_str(text.get(from..)?);
    Some(out)
}

/// The deepest line of `text`, read no further than the first line deeper than `limit`.
fn deepest(text: &str, limit: usize) -> usize {
    reach(text, limit, usize::MAX, usize::MAX).depth
}

/// How far a document reaches: the most containers any line sits in, the deepest
/// heading any line opens, where each heading deeper than [`MAX_HEADING_LEVEL`] starts,
/// the most lines any paragraph, heading or caption runs to, and the blocks at the top
/// level of the document holding one longer than the length [`reach`] was given.
struct Reach {
    depth: usize,
    heading: usize,
    overlong_headings: Vec<usize>,
    longest_leaf: usize,
    /// Each as the bytes it takes. Complete only when the scan read the whole text.
    long_blocks: Vec<Range<usize>>,
}

/// How far `text` reaches, read no further than the first line deeper than `depth`
/// containers or opening a heading deeper than `heading`, and which of its blocks at the
/// top level hold a paragraph, heading or caption longer than `long_block` lines.
fn reach(text: &str, depth: usize, heading: usize, long_block: usize) -> Reach {
    let mut scan = Scan {
        long_block,
        ..Scan::default()
    };
    let mut deepest = 0usize;
    let mut whole = true;
    // `jotdown` reads lines with their line break, which decides a few of its
    // markers: a lone `-` ending a line opens nothing, a lone `-` ending the text
    // opens a list item.
    for line in text.split_inclusive('\n') {
        scan.line_end += line.len();
        deepest = deepest.max(scan.read(line.as_bytes(), depth));
        if deepest > depth || scan.heading > heading {
            whole = false;
            break;
        }
    }
    if whole {
        // The last block ends with the text.
        scan.begin_block(text.len());
    }
    Reach {
        depth: deepest,
        heading: scan.heading,
        overlong_headings: scan.overlong_headings,
        longest_leaf: scan.longest_leaf,
        long_blocks: scan.long_blocks,
    }
}

/// `text` with each block at the top level of the document that holds a paragraph,
/// heading or caption longer than `lines` lines set down as its lines
/// ([`set_down_as_lines`]). `None` only if a block's bounds are not at a character's
/// start, which a line's never is.
fn set_down_long_blocks(text: &str, lines: usize) -> Option<String> {
    set_down_as_lines(
        text,
        &reach(text, usize::MAX, usize::MAX, lines).long_blocks,
    )
}

/// `text` with each of `blocks` set down as its lines: `blocks` are the bytes of whole
/// blocks at the top level of the document, in the order they come, and each becomes one
/// paragraph for each of its lines that holds text. The paragraph holds that line's text
/// as it is written, its outer whitespace aside, with a backslash in front of every ASCII
/// punctuation character so that none of it opens anything, and a blank line comes before
/// and after each, so that none runs into another block. `None` if a bound is not at a
/// character's start.
///
/// The parser then reads each line as a paragraph of one line, whatever it opened before,
/// and shows exactly the text `parse_djot` shows for each line of a document it cannot
/// take at all. Every other block reads as it did: a block at the top level starts on a
/// line that closes every container, and the blank line put in front of it closes nothing
/// that line did not.
fn set_down_as_lines(text: &str, blocks: &[Range<usize>]) -> Option<String> {
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let mut from = 0;
    for block in blocks {
        out.push_str(text.get(from..block.start)?);
        out.push('\n');
        for line in text.get(block.clone())?.lines() {
            let line = line.trim_matches(|c: char| c.is_ascii_whitespace());
            if line.is_empty() {
                continue;
            }
            for c in line.chars() {
                if c.is_ascii_punctuation() {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push_str("\n\n");
        }
        from = block.end;
    }
    out.push_str(text.get(from..)?);
    Some(out)
}

/// `text` with the indentation of each line's container prefix cut to at most
/// [`FLATTENED_INDENT_COLUMNS`] columns in all, and nothing else changed.
///
/// A line's container prefix is the run of blockquote, list item and footnote markers
/// it opens with, each identified as `jotdown` identifies it, and the whitespace before
/// and between them. Indentation is how Djot nests a list: an item indented past the
/// item above it goes inside it. Cut at a fixed column, a list nested deeper than that
/// comes out with its deeper items side by side at the deepest level left, every item
/// kept, in its order, with its text. Each marker keeps the whitespace byte that ends
/// it, so a quotation stays a quotation and an item stays an item, and every line keeps
/// its line break.
///
/// Meant for text nested deeper than [`MAX_NESTING_DEPTH`], before giving up on its
/// structure: [`parsable`] calls it once the headings it found too deep are escaped. It
/// reads each line on its own, so indentation inside a code block is its content and
/// this cuts it too, and what it returns has to be measured again: the markers it keeps
/// can still nest past the ceiling, and a line it cuts out of a list's paragraph can
/// open a heading, which [`parsable`] then escapes. It reads no more than one marker past the ceiling into
/// a line, the most the scan opens on one, and leaves the rest of that line as it is.
pub fn flatten_deep_indentation(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let mut columns_left = FLATTENED_INDENT_COLUMNS;
        let mut markers = 0usize;
        let mut rest = line;
        loop {
            let whitespace = rest.bytes().take_while(|&byte| is_space(byte)).count();
            // A line break is whitespace too, and has to stay.
            let indent = rest[..whitespace].find(['\r', '\n']).unwrap_or(whitespace);
            let keep = indent.min(columns_left);
            columns_left -= keep;
            out.push_str(&rest[..keep]);
            out.push_str(&rest[indent..whitespace]);
            let content = &rest[whitespace..];
            let marker = match identify(content.as_bytes()) {
                (Block::Blockquote | Block::Item { .. }, end) if markers <= MAX_NESTING_DEPTH => {
                    // The marker, and the whitespace byte that ends it.
                    end + usize::from(content.as_bytes().get(end).is_some_and(|&b| is_space(b)))
                }
                _ => {
                    out.push_str(content);
                    break;
                }
            };
            markers += 1;
            out.push_str(&content[..marker]);
            rest = &content[marker..];
        }
    }
    out
}

/// Whitespace as the block parser reads it.
fn is_space(byte: u8) -> bool {
    byte.is_ascii_whitespace()
}

/// Whether `view` holds nothing but whitespace.
fn is_blank(view: &[u8]) -> bool {
    view.iter().all(|&byte| is_space(byte))
}

/// `view` without its whitespace at either end, and how much whitespace led it:
/// `str::trim_matches`, as `jotdown` measures a container's prefix with it. A line of
/// nothing but whitespace trims to nothing, led by none.
fn trimmed(view: &[u8]) -> (usize, &[u8]) {
    let Some(start) = view.iter().position(|&byte| !is_space(byte)) else {
        return (0, &[]);
    };
    let end = view.len()
        - view
            .iter()
            .rev()
            .take_while(|&&byte| is_space(byte))
            .count();
    (start, &view[start..end])
}

/// What a line starts, as `jotdown` 0.10's `IdentifiedBlock::new` identifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    /// Nothing but whitespace.
    Blank,
    /// A paragraph: anything no other kind claims.
    Paragraph,
    /// A heading, at its level.
    Heading(usize),
    /// A block attribute line or a thematic break, complete on its one line.
    Atom,
    /// A blockquote: `>`, then whitespace or the end of the line.
    Blockquote,
    /// A list item of any kind, a definition's `:` included, or a footnote
    /// definition: the containers continued by indentation, which `jotdown`
    /// continues by the same rule. `indent` is the whitespace in front of the marker.
    Item { indent: usize },
    /// A link definition, `[label]: destination`, which holds no blocks.
    LinkDefinition,
    /// A table row, `| … |`.
    TableRow,
    /// A fence of three or more of `mark`: a div's colons, or a code block's
    /// backticks or tildes. `bare` when nothing follows the marks, which is the only
    /// fence that closes a block. `indent` is the whitespace in front of it.
    Fence {
        mark: u8,
        len: usize,
        bare: bool,
        indent: usize,
    },
}

/// Identify the block `view` starts, and where its marker ends: the bytes a
/// container's marker takes from the line, after which its content begins on the
/// same line.
fn identify(view: &[u8]) -> (Block, usize) {
    let indent = view
        .iter()
        .take_while(|&&byte| is_space(byte) && byte != b'\n')
        .count();
    let line = &view[indent..];
    let content = line.len()
        - line
            .iter()
            .rev()
            .take_while(|&&byte| is_space(byte))
            .count();
    let line_t = &line[..content];
    let Some(&first) = line.first() else {
        return (Block::Blank, indent);
    };
    let ends_marker = |at: usize| line.get(at).is_none_or(|&byte| is_space(byte));

    let found = match first {
        b'\n' => Some((Block::Blank, indent + 1)),
        b'#' => {
            let level = line.iter().take_while(|&&byte| byte == b'#').count();
            ends_marker(level).then_some((Block::Heading(level), indent + level))
        }
        b'>' => ends_marker(1).then_some((Block::Blockquote, indent + 1)),
        b'{' => (attributes_len(line) == content).then_some((Block::Atom, indent + line.len())),
        b'|' => (content >= 2 && line_t.ends_with(b"|") && !line_t.ends_with(b"\\|"))
            .then_some((Block::TableRow, indent)),
        b'[' => definition(line).map(|(label, footnote)| {
            let end = indent + 3 + label;
            if footnote {
                (Block::Item { indent }, end)
            } else {
                (Block::LinkDefinition, end)
            }
        }),
        b'-' | b'*' if is_thematic_break(&line[1..]) => Some((Block::Atom, indent + content)),
        b'-' | b'*' | b'+' => line.get(1).is_none_or(|&byte| byte == b' ').then(|| {
            // A task item's box belongs to its marker: `- [ ]` is five bytes wide.
            let task = line.get(2) == Some(&b'[')
                && matches!(line.get(3), Some(b'x' | b'X' | b' '))
                && line.get(4) == Some(&b']')
                && ends_marker(5);
            (Block::Item { indent }, indent + if task { 5 } else { 1 })
        }),
        b':' if ends_marker(1) => Some((Block::Item { indent }, indent + 1)),
        b'`' | b':' | b'~' => fence(line_t, first).map(|(len, bare)| {
            (
                Block::Fence {
                    mark: first,
                    len,
                    bare,
                    indent,
                },
                indent + line.len(),
            )
        }),
        _ => ordered_marker(line).map(|len| (Block::Item { indent }, indent + len)),
    };
    found.unwrap_or((Block::Paragraph, indent))
}

/// The length of the attribute block `line` opens with (the line from its `{` to its
/// end, line break included), or 0 if it does not open with a complete one: `jotdown`
/// 0.10's `attr::valid`. A line is an attribute block only when this reaches exactly
/// the end of its content.
fn attributes_len(line: &[u8]) -> usize {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Start,
        Whitespace,
        CommentFirst,
        Comment,
        CommentNewline,
        ClassFirst,
        Class,
        IdentifierFirst,
        Identifier,
        Key,
        ValueFirst,
        Value,
        ValueQuoted,
        ValueEscape,
        ValueNewline,
        ValueContinued,
        Done,
        Invalid,
    }
    fn is_name(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-')
    }
    fn step(state: State, c: u8) -> State {
        use State::*;
        match state {
            Start if c == b'{' => Whitespace,
            Start => Invalid,
            Whitespace => match c {
                b'}' => Done,
                b'.' => ClassFirst,
                b'#' => IdentifierFirst,
                b'%' => CommentFirst,
                c if is_name(c) => Key,
                c if c.is_ascii_whitespace() => Whitespace,
                _ => Invalid,
            },
            CommentFirst | Comment | CommentNewline if c == b'%' => Whitespace,
            CommentFirst | Comment | CommentNewline if c == b'}' => Done,
            CommentFirst | Comment | CommentNewline if c == b'\n' => CommentNewline,
            CommentFirst | Comment | CommentNewline => Comment,
            ClassFirst if is_name(c) => Class,
            ClassFirst => Invalid,
            IdentifierFirst if is_name(c) => Identifier,
            IdentifierFirst => Invalid,
            s @ (Class | Identifier | Value) if is_name(c) => s,
            Class | Identifier | Value if c.is_ascii_whitespace() => Whitespace,
            Class | Identifier | Value if c == b'}' => Done,
            Class | Identifier | Value => Invalid,
            Key if is_name(c) => Key,
            Key if c == b'=' => ValueFirst,
            Key => Invalid,
            ValueFirst if is_name(c) => Value,
            ValueFirst if c == b'"' => ValueQuoted,
            ValueFirst => Invalid,
            ValueQuoted | ValueNewline | ValueContinued if c == b'"' => Whitespace,
            ValueQuoted | ValueNewline | ValueContinued | ValueEscape if c == b'\n' => ValueNewline,
            ValueQuoted if c == b'\\' => ValueEscape,
            ValueQuoted | ValueEscape => ValueQuoted,
            ValueNewline | ValueContinued => ValueContinued,
            // Never stepped from: the loop below stops on either.
            Done | Invalid => state,
        }
    }

    let mut state = State::Start;
    for (at, &byte) in line.iter().enumerate() {
        state = step(state, byte);
        match state {
            State::Done => return at + 1,
            State::Invalid => return 0,
            _ => {}
        }
    }
    0
}

/// A footnote or link definition opening `line` at its `[`: the byte length of its
/// label (a footnote's `^` included), and whether it is a footnote. The label runs to
/// the first `]`, which a `:` must follow.
fn definition(line: &[u8]) -> Option<(usize, bool)> {
    let rest = line.get(1..)?;
    let label = rest.iter().position(|&byte| byte == b']')?;
    (rest.get(label + 1) == Some(&b':')).then_some((label, rest.first() == Some(&b'^')))
}

/// Whether what follows a `-` or `*` makes a thematic break: at least two more of
/// either mark, and nothing but whitespace besides.
fn is_thematic_break(after: &[u8]) -> bool {
    let mut marks = 1usize;
    for &byte in after {
        if matches!(byte, b'-' | b'*') {
            marks += 1;
        } else if !is_space(byte) {
            return false;
        }
    }
    marks >= 3
}

/// The fence `line` (trimmed of its trailing whitespace) opens with `mark`: its length,
/// and whether nothing follows it. A div's class holds only name characters; a code
/// block's language no whitespace and no backtick.
fn fence(line: &[u8], mark: u8) -> Option<(usize, bool)> {
    let len = line.iter().take_while(|&&byte| byte == mark).count();
    let spec = &line[len..];
    let spec = &spec[spec.iter().take_while(|&&byte| is_space(byte)).count()..];
    let valid = if mark == b':' {
        spec.iter()
            .all(|&byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-'))
    } else {
        !spec.iter().any(|&byte| is_space(byte) || byte == b'`')
    };
    (valid && len >= 3).then_some((len, spec.is_empty()))
}

/// The length of the ordered list marker opening `line`, or `None`: `jotdown` 0.10's
/// `maybe_ordered_list_item`. An optional `(`, then up to 19 digits, up to 13 roman
/// numerals all of one case, or one letter, then `)` (always, after a `(`) or `.`, then
/// whitespace or the end of the line.
fn ordered_marker(line: &[u8]) -> Option<usize> {
    fn roman_lower(byte: u8) -> bool {
        matches!(byte, b'i' | b'v' | b'x' | b'l' | b'c' | b'd' | b'm')
    }
    fn roman_upper(byte: u8) -> bool {
        matches!(byte, b'I' | b'V' | b'X' | b'L' | b'C' | b'D' | b'M')
    }
    let paren = line.first() == Some(&b'(');
    let start = usize::from(paren);
    let first = *line.get(start)?;
    let (numeral, most): (fn(u8) -> bool, usize) = if first.is_ascii_digit() {
        (|byte| byte.is_ascii_digit(), 19)
    } else if roman_lower(first) {
        (roman_lower, 13)
    } else if roman_upper(first) {
        (roman_upper, 13)
    } else if first.is_ascii_lowercase() {
        (|byte| byte.is_ascii_lowercase(), 1)
    } else if first.is_ascii_uppercase() {
        (|byte| byte.is_ascii_uppercase(), 1)
    } else {
        return None;
    };
    let number = 1 + line
        .get(start + 1..)
        .unwrap_or_default()
        .iter()
        .take(most - 1)
        .take_while(|&&byte| numeral(byte))
        .count();
    let closes = match line.get(start + number) {
        Some(b')') => true,
        Some(b'.') => !paren,
        _ => false,
    };
    let len = start + number + 1;
    (closes && line.get(len).is_none_or(|&byte| is_space(byte))).then_some(len)
}

/// A container open at the line being read, with the state `jotdown` keeps for it.
#[derive(Debug)]
enum Frame {
    /// A blockquote.
    Quote,
    /// A list item or a footnote definition. `marker_end` is how many bytes its marker
    /// took from its first line, the most indentation it strips from each later one.
    Item {
        indent: usize,
        marker_end: usize,
        last_blank: bool,
    },
    /// A div. `raw` is a code fence it saw open and not yet closed (`jotdown`'s
    /// `nested_raw`): while one is open, no fence closes the div. `first` until its
    /// first line of content, which `jotdown` does not strip.
    Div {
        indent: usize,
        len: usize,
        raw: Option<(u8, usize)>,
        closed: bool,
        first: bool,
    },
}

impl Frame {
    /// Whether this container goes on to `view`, the line as the containers around it
    /// leave it: `jotdown`'s `Kind::continues`.
    fn continues(&mut self, view: &[u8]) -> bool {
        match self {
            // A blockquote goes on through a line that states it again, and lazily
            // through a paragraph's line.
            Frame::Quote => matches!(identify(view).0, Block::Blockquote | Block::Paragraph),
            // An item goes on through a blank line, a line indented past its marker,
            // and lazily through a paragraph's line after a line that was not blank.
            Frame::Item {
                indent, last_blank, ..
            } => {
                let whitespace = view.iter().take_while(|&&byte| is_space(byte)).count();
                let next = identify(view).0;
                let lazy = !*last_blank && next == Block::Paragraph;
                *last_blank = next == Block::Blank;
                *last_blank || whitespace > *indent || lazy
            }
            // A div goes on until a bare fence of its own kind at least as long as
            // its own, which is the last line it holds.
            Frame::Div {
                len, raw, closed, ..
            } => {
                if *closed {
                    return false;
                }
                if let Block::Fence {
                    mark,
                    len: fence_len,
                    bare,
                    ..
                } = identify(view).0
                {
                    match *raw {
                        Some((open, open_len)) => {
                            if mark == open && fence_len >= open_len && bare {
                                *raw = None;
                            }
                        }
                        None if mark == b':' => *closed = fence_len >= *len && bare,
                        None => *raw = Some((mark, fence_len)),
                    }
                }
                true
            }
        }
    }

    /// `view` as this container hands it to what it holds, on any line after its
    /// first: `jotdown`'s `parse_container`, which never strips the line break.
    fn strip<'a>(&mut self, view: &'a [u8]) -> &'a [u8] {
        let body = view.iter().take_while(|&&byte| byte != b'\n').count();
        let (whitespace, content) = trimmed(view);
        let skip = match self {
            Frame::Quote => {
                if content == b">" {
                    whitespace + 1
                } else if content.first() == Some(&b'>')
                    && content.get(1).is_some_and(|&byte| is_space(byte))
                {
                    whitespace + 2
                } else {
                    0
                }
            }
            Frame::Item { marker_end, .. } => whitespace.min(*marker_end),
            Frame::Div { indent, first, .. } => {
                if std::mem::take(first) {
                    0
                } else {
                    whitespace.min(*indent)
                }
            }
        };
        &view[skip.min(body)..]
    }
}

/// The block last opened inside the innermost open container, when it is not itself a
/// container: what a line that every container goes on to may simply be more of.
#[derive(Debug, Default)]
enum Leaf {
    /// None, or one that ends on its own line (a blank line, an attribute line, a
    /// thematic break): the next line starts a block.
    #[default]
    None,
    Paragraph,
    Heading(usize),
    LinkDefinition,
    Code {
        mark: u8,
        len: usize,
        closed: bool,
    },
    /// A table, which counts as a container.
    Table {
        caption: bool,
        blank: bool,
    },
}

impl Leaf {
    /// Whether `view` is more of this block: `jotdown`'s `Kind::continues`.
    fn continues(&mut self, view: &[u8]) -> bool {
        match self {
            Leaf::None => false,
            Leaf::Paragraph | Leaf::Table { caption: true, .. } => !is_blank(view),
            Leaf::Heading(level) => match identify(view).0 {
                Block::Paragraph => true,
                Block::Heading(next) => next == *level,
                _ => false,
            },
            Leaf::LinkDefinition => view.first() == Some(&b' ') && !is_blank(view),
            Leaf::Code { mark, len, closed } => {
                if *closed {
                    return false;
                }
                if let Block::Fence {
                    mark: fence_mark,
                    len: fence_len,
                    bare,
                    ..
                } = identify(view).0
                    && fence_mark == *mark
                {
                    *closed = fence_len >= *len && bare;
                }
                true
            }
            Leaf::Table { caption, blank } => {
                let (_, row) = trimmed(view);
                if row.is_empty() {
                    *blank = true;
                    true
                } else if row.starts_with(b"^ ") {
                    *caption = true;
                    true
                } else {
                    !*blank
                        && row.starts_with(b"|")
                        && row.ends_with(b"|")
                        && !row.ends_with(b"\\|")
                }
            }
        }
    }
}

/// The containers open at the line being read, outermost first, and the block last
/// opened inside the innermost of them; and what the scan has seen so far of the
/// headings and of the lines of the blocks `jotdown` reads as text.
#[derive(Debug, Default)]
struct Scan {
    frames: Vec<Frame>,
    leaf: Leaf,
    /// The deepest heading opened so far.
    heading: usize,
    /// Where each heading deeper than [`MAX_HEADING_LEVEL`] starts, as a byte offset in
    /// the text: its first `#`.
    overlong_headings: Vec<usize>,
    /// The byte offset in the text just past the line being read.
    line_end: usize,
    /// How many lines `leaf` has run to, when `jotdown` reads it as text: a paragraph, a
    /// heading, a table's caption, or one of its rows, whose cells are a line each.
    leaf_lines: usize,
    /// The most lines any block `jotdown` reads as text has run to so far.
    longest_leaf: usize,
    /// Where the block open at the top level of the document starts, as a byte offset in
    /// the text: the start of the line that opened it, where every container had closed.
    block_start: usize,
    /// The most lines a block `jotdown` reads as text has run to in that block.
    block_longest: usize,
    /// How many lines a block `jotdown` reads as text may run to before the block at the
    /// top level holding it goes into `long_blocks`.
    long_block: usize,
    /// Each block at the top level holding a block `jotdown` reads as text longer than
    /// `long_block` lines, as the bytes it takes: from the start of its first line to the
    /// start of the line that opens the next block at the top level.
    long_blocks: Vec<Range<usize>>,
}

impl Scan {
    /// Read one line, line break included, and return how many containers it sits in.
    /// Opens no more than `limit + 1` containers on it.
    fn read(&mut self, line: &[u8], limit: usize) -> usize {
        let mut view = line;
        let mut level = 0;
        while let Some(frame) = self.frames.get_mut(level) {
            if !frame.continues(view) {
                // It ends before this line, and all it held with it.
                self.frames.truncate(level);
                return self.open(view, limit);
            }
            if matches!(frame, Frame::Div { closed: true, .. }) {
                // A div's closing fence belongs to it and to nothing inside it.
                self.frames.truncate(level + 1);
                self.leaf = Leaf::None;
                self.leaf_lines = 0;
                return self.depth();
            }
            view = frame.strip(view);
            level += 1;
        }
        let in_caption = matches!(self.leaf, Leaf::Table { caption: true, .. });
        if self.leaf.continues(view) {
            match self.leaf {
                Leaf::Paragraph | Leaf::Heading(_) => self.leaf_line(),
                Leaf::Table { caption: true, .. } if in_caption => self.leaf_line(),
                // Each cell of a row is a block of one line, and so is a caption's first.
                Leaf::Table { .. } => {
                    self.leaf_lines = 0;
                    self.leaf_line();
                }
                // Read verbatim, or no block at all.
                Leaf::None | Leaf::LinkDefinition | Leaf::Code { .. } => {}
            }
            return self.depth();
        }
        self.open(view, limit)
    }

    /// Count one more line of the block `jotdown` reads as text.
    fn leaf_line(&mut self) {
        self.leaf_lines += 1;
        self.longest_leaf = self.longest_leaf.max(self.leaf_lines);
        self.block_longest = self.block_longest.max(self.leaf_lines);
    }

    /// Start a new block at the top level of the document at byte `at`, which ends the
    /// one before it.
    fn begin_block(&mut self, at: usize) {
        if self.block_longest > self.long_block {
            self.long_blocks.push(self.block_start..at);
        }
        self.block_start = at;
        self.block_longest = 0;
    }

    /// Read `view` as the first line of a new block inside the innermost open
    /// container, opening every container its markers open.
    fn open(&mut self, mut view: &[u8], limit: usize) -> usize {
        if self.frames.is_empty() {
            // Every container has closed, so nothing stripped `view`: it is the line.
            self.begin_block(self.line_end - view.len());
        }
        self.leaf = Leaf::None;
        self.leaf_lines = 0;
        while self.frames.len() <= limit {
            let (block, marker_end) = identify(view);
            match block {
                Block::Blank | Block::Atom => break,
                Block::Paragraph => {
                    self.leaf = Leaf::Paragraph;
                    self.leaf_line();
                    break;
                }
                Block::Heading(level) => {
                    self.heading = self.heading.max(level);
                    if level > MAX_HEADING_LEVEL {
                        // `view` is the end of the line, and its marks end the marker.
                        self.overlong_headings
                            .push(self.line_end - view.len() + marker_end - level);
                    }
                    self.leaf = Leaf::Heading(level);
                    self.leaf_line();
                    break;
                }
                Block::LinkDefinition => {
                    self.leaf = Leaf::LinkDefinition;
                    break;
                }
                Block::TableRow => {
                    self.leaf = Leaf::Table {
                        caption: false,
                        blank: false,
                    };
                    self.leaf_line();
                    break;
                }
                Block::Fence {
                    mark: b':',
                    len,
                    indent,
                    ..
                } => {
                    // A div's content starts on the line after its fence.
                    self.frames.push(Frame::Div {
                        indent,
                        len,
                        raw: None,
                        closed: false,
                        first: true,
                    });
                    break;
                }
                Block::Fence { mark, len, .. } => {
                    self.leaf = Leaf::Code {
                        mark,
                        len,
                        closed: false,
                    };
                    break;
                }
                Block::Blockquote => {
                    self.frames.push(Frame::Quote);
                    view = &view[marker_end..];
                    // The one space or tab after a `>` is the quote's own.
                    if matches!(view.first(), Some(b' ' | b'\t')) {
                        view = &view[1..];
                    }
                }
                Block::Item { indent } => {
                    self.frames.push(Frame::Item {
                        indent,
                        marker_end,
                        last_blank: false,
                    });
                    view = &view[marker_end..];
                }
            }
        }
        self.depth()
    }

    /// How many containers the line just read sits in.
    fn depth(&self) -> usize {
        self.frames.len() + usize::from(matches!(self.leaf, Leaf::Table { .. }))
    }
}

#[cfg(test)]
#[path = "djot_depth_tests.rs"]
mod tests;
