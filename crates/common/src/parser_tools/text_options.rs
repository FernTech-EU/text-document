//! Opt-ins for the two *flowing* export formats.
//!
//! Plain text and Markdown have no page geometry and no typography, so unlike
//! [`DocxExportOptions`](super::DocxExportOptions) and its siblings these carry nothing but a
//! handful of presentation choices — the things a **file being written out** wants and a
//! string being computed against does not.
//!
//! Both default to *off*, and that default is load-bearing rather than merely conservative:
//! `to_plain_text()` is pinned character-for-character to the document's addressable text, the
//! text `find_all`/`replace_text` compute offsets against. Anything that inserts or indents
//! shifts every offset after it and silently desynchronises search from the document. So the
//! plain view stays plain, and only a caller writing a file asks for the rest.

use serde::{Deserialize, Serialize};

/// The CSS3 property and its CSS2 predecessor, both, because reading systems and print
/// engines are still split between them. Shared with the HTML writer so the two formats
/// cannot drift apart on what a page break looks like.
pub const HTML_PAGE_BREAK_STYLE: &str = "break-before: page; page-break-before: always;";

/// A raw HTML block standing in for a page break in Markdown.
///
/// Markdown has no page-break construct — not in CommonMark, not in any widely-implemented
/// extension. Of the three conventions in the wild (a bare `U+000C`, an HTML-comment sentinel
/// a toolchain greps for, and this) only this one *does* anything without a bespoke
/// processor: it survives Pandoc's reader as raw HTML, reaches HTML output intact, and is
/// honoured by browser and EPUB print engines. It renders as nothing on screen.
pub fn markdown_page_break() -> String {
    format!("<div style=\"{HTML_PAGE_BREAK_STYLE}\"></div>")
}

/// U+000C FORM FEED — what a page break has meant in a text file since line printers, and
/// still what `pr`, `less` and most terminal pagers act on.
pub const FORM_FEED: char = '\u{000C}';

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PlainTextExportOptions {
    /// Indent blockquoted blocks four spaces per level, so quoted matter still reads as
    /// set-off in a format with no markup to say so.
    pub quote_indent: bool,
    /// Emit [`FORM_FEED`] before a block that asks to start a new page.
    pub page_breaks: bool,
}

impl PlainTextExportOptions {
    /// Nothing added — the addressable view, pinned to the document's own search text.
    pub const fn addressable() -> Self {
        Self {
            quote_indent: false,
            page_breaks: false,
        }
    }

    /// Everything a written-out `.txt` wants.
    pub const fn presentation() -> Self {
        Self {
            quote_indent: true,
            page_breaks: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MarkdownExportOptions {
    /// Emit [`markdown_page_break`] before a block that asks to start a new page. Off by
    /// default: raw HTML is not Markdown, and an export nobody is going to paginate is
    /// better off clean.
    pub page_breaks: bool,
}
