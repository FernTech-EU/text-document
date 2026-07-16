//! Book-level metadata for EPUB export.
//!
//! Unlike [`super::docx_options::DocxExportOptions`] (page geometry + typography — DOCX has no
//! notion of "book metadata"), an EPUB package is built around exactly this: title, author,
//! language, and reading direction are OPF/package-level metadata fields, not per-block
//! formatting. There is no "plain" EPUB the way `to_docx` has docx-rs's built-in defaults — every
//! field here lands somewhere in the generated `.epub`, so [`Default`] picks the most harmless
//! values ("Untitled" title, empty author, `en` language, LTR) rather than leaving them unset.

use serde::{Deserialize, Serialize};

/// Book-level metadata for an EPUB export. Every field is required by the container format in
/// some form, so unlike [`super::docx_options::DocxExportOptions`] there is no "no overrides"
/// default that matches an unconfigured EPUB — [`Default`] instead picks the most reasonable
/// placeholders.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EpubExportOptions {
    /// Book title, used for the package's `dc:title` and (when a chapter has no heading of its
    /// own — the front-matter chapter, or the whole document when it has no headings at all) as
    /// that chapter's title too.
    pub title: String,
    /// Author name, used for the package's `dc:creator`. Empty ⇒ no author is emitted.
    pub author: String,
    /// BCP-47/ISO 639-1 language code (e.g. `"en"`, `"fr"`), used for the package's `dc:language`
    /// and every chapter XHTML document's `xml:lang`/`lang`. Empty ⇒ `"en"`.
    pub language: String,
    /// Right-to-left reading direction: sets the package's `page-progression-direction` to `rtl`
    /// and adds `dir="rtl"` to every chapter's `<html>` element.
    pub rtl: bool,
}
