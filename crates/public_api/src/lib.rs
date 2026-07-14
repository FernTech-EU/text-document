//! # text-document
//!
//! A rich text document model for Rust.
//!
//! Provides a [`TextDocument`] as the main entry point and [`TextCursor`] for
//! cursor-based editing, inspired by Qt's QTextDocument/QTextCursor API.
//!
//! ```rust,no_run
//! use text_document::{TextDocument, MoveMode, MoveOperation};
//!
//! let doc = TextDocument::new();
//! doc.set_plain_text("Hello world").unwrap();
//!
//! let cursor = doc.cursor();
//! cursor.move_position(MoveOperation::EndOfWord, MoveMode::KeepAnchor, 1);
//! cursor.insert_text("Goodbye").unwrap(); // replaces "Hello"
//!
//! // Multiple cursors on the same document
//! let c1 = doc.cursor();
//! let c2 = doc.cursor_at(5);
//! c1.insert_text("A").unwrap();
//! // c2's position is automatically adjusted
//!
//! doc.undo().unwrap();
//! ```

mod batch;
mod convert;
mod cursor;
mod document;
mod error;
mod events;
mod flow;
mod fragment;
mod highlight;
mod inner;
mod operation;
mod text_block;
mod text_frame;
mod text_list;
mod text_table;

// ── Re-exports from entity DTOs (enums that consumers need) ──────
pub use frontend::block::dtos::{Alignment, MarkerType};
pub use frontend::block::dtos::{CharVerticalAlignment, InlineContent, UnderlineStyle};
pub use frontend::common::parser_tools::{DjotExportOptions, DjotImportOptions};
pub use frontend::document::dtos::{TextDirection, WrapMode};
pub use frontend::frame::dtos::FramePosition;
pub use frontend::list::dtos::ListStyle;
pub use frontend::resource::dtos::ResourceType;

// ── Error type ───────────────────────────────────────────────────
pub use batch::BatchDocument;
pub use error::{DocumentError, Result};

// ── Public API types ─────────────────────────────────────────────
pub use cursor::TextCursor;
pub use document::TextDocument;
pub use events::{DocumentEvent, Subscription};
pub use fragment::DocumentFragment;
pub use highlight::{HighlightContext, HighlightFormat, HighlightSpan, SyntaxHighlighter};
pub use operation::{DocxExportResult, HtmlImportResult, MarkdownImportResult, Operation};

// ── Layout engine API types ─────────────────────────────────────
pub use flow::{
    BlockSnapshot, CellFormat, CellRange, CellSnapshot, CellVerticalAlignment, FlowElement,
    FlowElementSnapshot, FlowSnapshot, FormatChangeKind, FragmentContent, FrameRef, FrameSnapshot,
    ListInfo, PaintHighlightSpan, SelectionKind, TableCellContext, TableCellRef, TableFormat,
    TableSnapshot,
};
pub use text_block::TextBlock;
pub use text_frame::TextFrame;
pub use text_list::TextList;
pub use text_table::{TextTable, TextTableCell};

// All public handle types are Send + Sync (all fields are Arc<Mutex<...>> + Copy).
const _: () = {
    #[allow(dead_code)]
    fn assert_send_sync<T: Send + Sync>() {}
    fn _assert_all() {
        assert_send_sync::<TextDocument>();
        assert_send_sync::<TextCursor>();
        assert_send_sync::<TextBlock>();
        assert_send_sync::<TextFrame>();
        assert_send_sync::<TextTable>();
        assert_send_sync::<TextTableCell>();
        assert_send_sync::<TextList>();
    }
};

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Color
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// An RGBA color value. Each component is 0–255.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Color {
    /// Create an opaque color (alpha = 255).
    pub fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: 255,
        }
    }

    /// Create a color with explicit alpha.
    pub fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Public format types
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Character/text formatting. All fields are optional: `None` means
/// "not set — inherit from the block's default or the document's default."
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextFormat {
    pub font_family: Option<String>,
    pub font_point_size: Option<u32>,
    pub font_weight: Option<u32>,
    pub font_bold: Option<bool>,
    pub font_italic: Option<bool>,
    pub font_underline: Option<bool>,
    pub font_overline: Option<bool>,
    pub font_strikeout: Option<bool>,
    pub letter_spacing: Option<i32>,
    pub word_spacing: Option<i32>,
    pub underline_style: Option<UnderlineStyle>,
    pub vertical_alignment: Option<CharVerticalAlignment>,
    pub anchor_href: Option<String>,
    pub anchor_names: Vec<String>,
    pub is_anchor: Option<bool>,
    pub tooltip: Option<String>,
    pub foreground_color: Option<Color>,
    pub background_color: Option<Color>,
    pub underline_color: Option<Color>,
}

/// Block (paragraph) formatting. All fields are optional.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockFormat {
    pub alignment: Option<Alignment>,
    pub top_margin: Option<i32>,
    pub bottom_margin: Option<i32>,
    pub left_margin: Option<i32>,
    pub right_margin: Option<i32>,
    pub heading_level: Option<u8>,
    pub indent: Option<u8>,
    pub text_indent: Option<i32>,
    pub marker: Option<MarkerType>,
    pub tab_positions: Vec<i32>,
    pub line_height: Option<f32>,
    pub non_breakable_lines: Option<bool>,
    pub direction: Option<TextDirection>,
    pub background_color: Option<String>,
    pub is_code_block: Option<bool>,
    pub code_language: Option<String>,
    /// Enable automatic + soft-hyphen hyphenation for this block.
    pub hyphenate: Option<bool>,
    /// Block natural language as an ISO 639-1 code (e.g. "en", "fr").
    /// Selects the hyphenation dictionary.
    pub language: Option<String>,
}

/// List formatting. All fields are optional: `None` means
/// "not set — don't change this property."
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListFormat {
    pub style: Option<ListStyle>,
    pub indent: Option<u8>,
    pub prefix: Option<String>,
    pub suffix: Option<String>,
}

/// Frame formatting. All fields are optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameFormat {
    pub height: Option<i32>,
    pub width: Option<i32>,
    pub top_margin: Option<i32>,
    pub bottom_margin: Option<i32>,
    pub left_margin: Option<i32>,
    pub right_margin: Option<i32>,
    pub padding: Option<i32>,
    pub border: Option<i32>,
    pub position: Option<FramePosition>,
    pub is_blockquote: Option<bool>,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Enums for cursor movement
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Controls whether a movement collapses or extends the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveMode {
    /// Move both position and anchor — collapses selection.
    MoveAnchor,
    /// Move only position, keep anchor — creates or extends selection.
    KeepAnchor,
}

/// Semantic cursor movement operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveOperation {
    NoMove,
    Start,
    End,
    StartOfLine,
    EndOfLine,
    StartOfBlock,
    EndOfBlock,
    StartOfWord,
    EndOfWord,
    PreviousBlock,
    NextBlock,
    PreviousCharacter,
    NextCharacter,
    PreviousWord,
    NextWord,
    Up,
    Down,
    Left,
    Right,
    WordLeft,
    WordRight,
}

/// Quick-select a region around the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionType {
    WordUnderCursor,
    LineUnderCursor,
    BlockUnderCursor,
    Document,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Read-only info types
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Document-level statistics. O(1) cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentStats {
    pub character_count: usize,
    pub word_count: usize,
    pub block_count: usize,
    pub frame_count: usize,
    pub image_count: usize,
    pub list_count: usize,
    pub table_count: usize,
}

/// Info about a block at a given position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockInfo {
    pub block_id: usize,
    pub block_number: usize,
    pub start: usize,
    pub length: usize,
}

/// A single search match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindMatch {
    pub position: usize,
    pub length: usize,
}

/// Options for find / find_all / replace operations.
#[derive(Debug, Clone, Default)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
    pub search_backward: bool,
}
