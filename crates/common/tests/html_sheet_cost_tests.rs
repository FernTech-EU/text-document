// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! The cost of reading a pasted <style> block, counted in bytes allocated and not in time,
//! so that the bound does not depend on the machine.
//!
//! A sheet of 4000 `*` rules and 4000 tag rules (167 KB) used to make 272 MB of strings, and
//! the bytes allocated grew with the square of the sheet. Read in proportion to its size, a
//! sheet costs a few times its own length.

use common::parser_tools::content_parser::parse_html_elements;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATED.try_with(|a| a.set(a.get().saturating_add(layout.size())));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATED.try_with(|a| a.set(a.get().saturating_add(new_size)));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// The bytes the current thread allocates while `parse_html_elements` reads `html`.
fn allocated_by_reading(html: &str) -> usize {
    let before = ALLOCATED.with(Cell::get);
    let read = parse_html_elements(html);
    let after = ALLOCATED.with(Cell::get);
    std::hint::black_box(read);
    after - before
}

#[test]
fn reading_a_sheet_allocates_in_proportion_to_its_size() {
    // Many `*` rules and as many tag rules.
    let mut css = String::new();
    for i in 0..4000 {
        css.push_str("* { white-space: pre }\n");
        css.push_str(&format!("t{i} {{ white-space: pre }}\n"));
    }
    let html = format!("<style>{css}</style><p>a\tb</p>");
    let bytes = allocated_by_reading(&html);
    assert!(
        bytes < html.len() * 100,
        "{} bytes of HTML allocated {bytes} bytes",
        html.len()
    );

    // One rule of 20,000 selectors.
    let selectors: Vec<String> = (0..20_000).map(|i| format!("t{i}")).collect();
    let html = format!(
        "<style>{} {{ white-space: pre }}</style><p>a\tb</p>",
        selectors.join(",")
    );
    let bytes = allocated_by_reading(&html);
    assert!(
        bytes < html.len() * 100,
        "{} bytes of HTML allocated {bytes} bytes",
        html.len()
    );
}

/// Four times the sheet is about four times the bytes, and not sixteen.
#[test]
fn quadrupling_a_sheet_does_not_square_the_bytes_allocated() {
    let sheet = |n: usize| {
        let mut css = String::new();
        for i in 0..n {
            css.push_str("* { white-space: pre }\n");
            css.push_str(&format!("t{i} {{ white-space: pre }}\n"));
        }
        format!("<style>{css}</style><p>a\tb</p>")
    };
    let small = allocated_by_reading(&sheet(1000));
    let large = allocated_by_reading(&sheet(4000));
    assert!(
        large < small * 8,
        "4 times the rules allocated {large} bytes against {small}: {:.1} times",
        large as f64 / small as f64
    );
}
