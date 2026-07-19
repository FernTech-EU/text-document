//! Per-feature selection for djot import/export.
//!
//! The djot importer and exporter round-trip a set of *optional* block-level
//! attributes — paragraph alignment, line height, text direction, non-breakable
//! lines and background color — through djot's native `{key=value}` block
//! attribute syntax (e.g. `{alignment=center}` on the line before a paragraph).
//! These two option structs let a caller choose which of those attributes are
//! carried. Everything else (headings, lists, tables, blockquotes, code blocks
//! and all inline formatting) is always imported/exported and is not gated.
//!
//! Both structs default to **all enabled** — the fully lossless round-trip. Use
//! [`DjotImportOptions::none`] / [`DjotExportOptions::none`] to restrict the
//! round-trip to the core structural and inline feature set only.
//!
//! The attribute keys used on the wire are the model field names:
//! `alignment`, `line_height`, `direction`, `non_breakable_lines`,
//! `background_color`. Block attributes are only emitted/read for standalone
//! paragraphs and headings; list items, code blocks and table cells normalise
//! their block styling away (the same boundary the other targets observe).

use serde::{Deserialize, Serialize};

/// Selects which optional block attributes the djot **importer** applies to the
/// document model. An attribute present in the source but disabled here is
/// parsed and discarded, exactly like an unrepresentable construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DjotImportOptions {
    /// Apply paragraph alignment from `{alignment=left|right|center|justify}`.
    pub alignment: bool,
    /// Apply line height from `{line_height=<int>}`.
    pub line_height: bool,
    /// Apply text direction from `{direction=ltr|rtl}`.
    pub direction: bool,
    /// Apply non-breakable lines from `{non_breakable_lines=true|false}`.
    pub non_breakable_lines: bool,
    /// Apply block background color from `{background_color="<value>"}`.
    pub background_color: bool,
    /// Apply the block's own space-above from `{top_margin=<int>}`, overriding
    /// the document-wide paragraph spacing for that one block.
    pub top_margin: bool,
    /// Apply the block's own first-line indent from `{text_indent=<int>}`,
    /// overriding the document-wide first-line indent for that one block.
    pub text_indent: bool,
}

impl DjotImportOptions {
    /// Every optional block attribute enabled — the lossless default.
    pub const fn all() -> Self {
        Self {
            alignment: true,
            line_height: true,
            direction: true,
            non_breakable_lines: true,
            background_color: true,
            top_margin: true,
            text_indent: true,
        }
    }

    /// No optional block attributes — import only the core structural and
    /// inline feature set, discarding any block-attribute styling.
    pub const fn none() -> Self {
        Self {
            alignment: false,
            line_height: false,
            direction: false,
            non_breakable_lines: false,
            background_color: false,
            top_margin: false,
            text_indent: false,
        }
    }
}

impl Default for DjotImportOptions {
    fn default() -> Self {
        Self::all()
    }
}

/// Selects which optional block attributes the djot **exporter** emits as
/// `{key=value}` block attributes. A disabled attribute is omitted from the
/// output even when the model carries a value for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DjotExportOptions {
    /// Emit paragraph alignment as `{alignment=…}`.
    pub alignment: bool,
    /// Emit line height as `{line_height=…}`.
    pub line_height: bool,
    /// Emit text direction as `{direction=…}`.
    pub direction: bool,
    /// Emit non-breakable lines as `{non_breakable_lines=…}`.
    pub non_breakable_lines: bool,
    /// Emit block background color as `{background_color=…}`.
    pub background_color: bool,
    /// Emit the block's own space-above as `{top_margin=…}`.
    pub top_margin: bool,
    /// Emit the block's own first-line indent as `{text_indent=…}`.
    pub text_indent: bool,
}

impl DjotExportOptions {
    /// Every optional block attribute emitted — the lossless default.
    pub const fn all() -> Self {
        Self {
            alignment: true,
            line_height: true,
            direction: true,
            non_breakable_lines: true,
            background_color: true,
            top_margin: true,
            text_indent: true,
        }
    }

    /// No optional block attributes — emit only the core structural and inline
    /// feature set, dropping any block-attribute styling.
    pub const fn none() -> Self {
        Self {
            alignment: false,
            line_height: false,
            direction: false,
            non_breakable_lines: false,
            background_color: false,
            top_margin: false,
            text_indent: false,
        }
    }
}

impl Default for DjotExportOptions {
    fn default() -> Self {
        Self::all()
    }
}
