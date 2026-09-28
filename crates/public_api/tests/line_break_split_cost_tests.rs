// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! Splitting a block at its line breaks must cost memory in proportion to its text.
//!
//! The model keeps no line break inside a block, so three paths split one: a paste of
//! preformatted HTML (`DocumentFragment::from_html`, which an editor calls on its
//! interface thread), `set_html` of a paragraph that keeps its whitespace, and the Djot
//! writer, for a block that holds line breaks all the same (text inserted or dropped with
//! them). Each built every line by cloning the whole block, or the whole run, and then
//! replacing the text: a copy of the whole text per line. A 64,000-line paste took 6.6 s
//! in a release build, and a save of such a block 3.4 s.
//!
//! This counts the bytes each path allocates on the calling thread, against the same
//! text laid out one block a line, which needs no split. A linear split allocates about
//! as much; a copy of the text per line allocates the text again for every line, from
//! 26 to 102 times the reference at the 4,000 lines measured here, and more the longer
//! the text. Counting, not timing, so the guard holds in every build and on any machine.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use common::parser_tools::content_parser::parse_html;
use text_document::{DocumentFragment, TextDocument};

/// Counts the bytes allocated on a thread while that thread has counting switched on.
struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

fn count(bytes: usize) {
    // `try_with`: an allocation made while the thread's locals are being torn down is
    // not counted, rather than panicking inside the allocator.
    let _ = COUNTING.try_with(|on| {
        if on.get() {
            let _ = ALLOCATED.try_with(|total| total.set(total.get() + bytes));
        }
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// What `f` returns, and the bytes it allocated on this thread.
fn allocated_by<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATED.with(|total| total.set(0));
    COUNTING.with(|on| on.set(true));
    let value = f();
    COUNTING.with(|on| on.set(false));
    (value, ALLOCATED.with(Cell::get))
}

/// How many lines each measurement splits: enough that a copy of the text per line
/// allocates hundreds of times what the split itself needs.
const LINES: usize = 4_000;

/// The lines measured, each a little over twenty bytes.
fn lines() -> Vec<String> {
    (0..LINES)
        .map(|i| format!("verse {i} of the poem"))
        .collect()
}

/// The same lines as HTML paragraphs, one each: the reference, which needs no split.
fn paragraphs_html(lines: &[String]) -> String {
    lines.iter().map(|line| format!("<p>{line}</p>")).collect()
}

/// `split` over `reference`, with both counts, for the failure message.
fn ratio(split: usize, reference: usize) -> f64 {
    split as f64 / reference.max(1) as f64
}

/// A split allocates at most this many times what the same text one block a line does.
const MOST_OVER_THE_REFERENCE: f64 = 4.0;

/// A paste of preformatted text: the fragment the editor inserts. Measured on the fix:
/// 0.8 times the reference; with each line built from a copy of the whole block, 26.5.
#[test]
fn pasting_preformatted_text_allocates_in_proportion_to_it() {
    let lines = lines();
    let pre = format!("<pre>{}</pre>", lines.join("\n"));
    let (fragment, pasted) = allocated_by(|| DocumentFragment::from_html(&pre));
    let (_, reference) = allocated_by(|| DocumentFragment::from_html(&paragraphs_html(&lines)));
    println!("paste: {pasted} bytes, against {reference}");
    assert!(
        ratio(pasted, reference) < MOST_OVER_THE_REFERENCE,
        "a paste of {LINES} preformatted lines allocated {pasted} bytes, {:.1} times the \
         {reference} of the same lines as paragraphs: each line is being built from a copy \
         of the whole block again",
        ratio(pasted, reference)
    );
    assert_eq!(fragment.to_plain_text().lines().count(), LINES);
}

/// `set_html` of a paragraph that keeps its whitespace, which the HTML parser splits.
/// Measured on the fix: 0.6 times the reference; with each line built from a copy of
/// the whole block, 75.
#[test]
fn reading_a_paragraph_that_keeps_its_line_breaks_allocates_in_proportion_to_it() {
    let lines = lines();
    let pre_wrap = format!(
        "<p style=\"white-space: pre-wrap\">{}</p>",
        lines.join("\n")
    );
    let (blocks, loaded) = allocated_by(|| parse_html(&pre_wrap));
    let (_, reference) = allocated_by(|| parse_html(&paragraphs_html(&lines)));
    println!("load: {loaded} bytes, against {reference}");
    assert!(
        ratio(loaded, reference) < MOST_OVER_THE_REFERENCE,
        "reading {LINES} lines kept in one paragraph allocated {loaded} bytes, {:.1} times \
         the {reference} of the same lines as paragraphs: each line is being built from a \
         copy of the whole block again",
        ratio(loaded, reference)
    );
    assert_eq!(blocks.len(), LINES);
}

/// A save of a block holding line breaks, which the Djot writer splits. Measured on the
/// fix: 1.6 times the reference; with each line built from a copy of the whole run, 102.
#[test]
fn saving_a_block_holding_line_breaks_allocates_in_proportion_to_it() {
    let lines = lines();
    let held = TextDocument::new();
    held.cursor().insert_text(&lines.join("\n")).unwrap();
    assert_eq!(held.blocks().len(), 1);
    let one_a_line = TextDocument::new();
    one_a_line.set_plain_text(&lines.join("\n")).unwrap();
    assert_eq!(one_a_line.blocks().len(), LINES);
    let (saved, written) = allocated_by(|| held.to_djot().unwrap());
    let (_, reference) = allocated_by(|| one_a_line.to_djot().unwrap());
    println!("save: {written} bytes, against {reference}");
    assert!(
        ratio(written, reference) < MOST_OVER_THE_REFERENCE,
        "saving a block of {LINES} lines allocated {written} bytes, {:.1} times the \
         {reference} of the same lines as blocks: each line is being built from a copy of \
         the whole run again",
        ratio(written, reference)
    );
    assert_eq!(saved.lines().filter(|line| !line.is_empty()).count(), LINES);
}
