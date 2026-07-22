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
mod streaming;
mod text_block;
mod text_frame;
mod text_list;
mod text_table;

// ── Re-exports from entity DTOs (enums that consumers need) ──────
pub use frontend::block::dtos::{Alignment, MarkerType};
pub use frontend::block::dtos::{CharVerticalAlignment, InlineContent, UnderlineStyle};
pub use frontend::common::format_runs::ReplaceFormatPolicy;
pub use frontend::common::parser_tools::{
    CountMethod, DjotExportOptions, DjotImportOptions, DocxExportOptions, EpubExportOptions,
    PdfExportOptions, TABLE_ANCHOR, WordCharCounts, count, count_djot, djot_to_plain_text,
};

/// The matcher, as a pure function over `&str` — no document, no store, no threads.
///
/// A host app searching a whole project cannot afford to build a document per row just
/// to ask "does this contain that": it would parse every scene in the manuscript on
/// every keystroke. It extracts the prose cheaply and matches it here instead.
///
/// Exposing it is what keeps there being **one** definition of a match. An app that
/// rolled its own would disagree with this crate's in-document find about whole-word
/// rules and case folding, and a writer would meet that as "the editor found it but the
/// search panel didn't".
/// The same goes for **folding** and for **case preservation**: an app that lowercased its
/// own corpus would miss `Straße` and half-rename a Turkish manuscript. `FoldLocale` is how
/// a per-scene language reaches the fold.
/// [`FoldedText`](matching::FoldedText) is the *prepared* form: a haystack folded once and
/// searched many times. A search box re-searches the same corpus on every keystroke, and
/// folding it costs several times what scanning it does — so an app that searches a whole
/// project keeps one of these per scene rather than rebuilding the fold per character typed.
pub mod matching {
    pub use frontend::document_search::matching::{
        FoldLocale, FoldSpec, FoldedText, Match, MatchOptions, find_all, preserve_case,
    };
}
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
pub use highlight::{
    HighlightContext, HighlightFormat, HighlightMask, HighlightSpan, RangeHighlight, SessionId,
    SyntaxHighlighter,
};
pub use operation::{
    DocxExportResult, EpubExportResult, HtmlImportResult, MarkdownImportResult, Operation,
    PdfExportResult,
};

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
    /// Unset the block's direction rather than setting one.
    ///
    /// Every other field merges (`None` = "don't change this"), so this
    /// is the only way to take a paragraph back to automatic direction
    /// detection once a direction has been stored. Wins over
    /// `direction` if both are set.
    pub clear_direction: bool,
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
    /// The text that was actually matched, sliced from the document's own search text.
    ///
    /// Carried here so no caller ever slices it themselves — and with folding on, that is no
    /// longer a convenience. A search for `cafe` matches `café`; a search for `strasse`
    /// matches `straße`. The query is **not** the matched text, `length` is not the query's
    /// length, and the only other whole-document string a caller can reach
    /// ([`TextDocument::to_plain_text`]) does not even use the same offset space — it drops
    /// the `U+FFFC` anchor an embedded table occupies.
    pub matched_text: String,
}

/// Options for find / find_all / replace operations.
///
/// Both folding toggles default to **off = folded**, which is what a writer means by
/// "search": `aurelien` finds `Aurélien`, `strasse` finds `Straße`, `احمد` finds `أَحْمَد`.
/// Turn one on to be literal about it.
#[derive(Debug, Clone, Default)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// `false` (the default) folds diacritics, ligatures and Arabic orthography.
    pub diacritic_sensitive: bool,
    /// The BCP-47 tag of the text being searched — **per document**, not per search.
    ///
    /// Only Turkish and Azerbaijani (`tr`, `az`) change how text folds: there the dotted
    /// and dotless `i` are different letters, and merging them turns one word into another.
    /// Every other tag — including an empty or malformed one — folds untailored, so this is
    /// safe to leave alone and safe to feed a user's raw project setting.
    ///
    /// It decides *how* to fold, never *whether* to: the toggles above stay global across a
    /// search, or the same checkbox would mean different things in different chapters.
    pub language: String,
    pub use_regex: bool,
    pub search_backward: bool,
}

/// Options for a replace: how to *find* the text, and what the replacement wears where
/// it overwrites formatted prose.
///
/// The format policy is deliberately not on [`FindOptions`] — it means nothing to a
/// find, and a search option that silently only applies to half the calls that take it
/// is how dead toggles are born.
#[derive(Debug, Clone, Default)]
pub struct ReplaceOptions {
    pub find: FindOptions,
    /// Defaults to [`ReplaceFormatPolicy::InheritPreceding`] — the behaviour that has
    /// always shipped, which drops the formatting under the replaced range. Choose
    /// another policy when the range may be formatted and losing that would be wrong
    /// (a character rename landing on a partly-bold name).
    pub format_policy: ReplaceFormatPolicy,
}

/// One range to replace, with **its own** replacement text.
///
/// `position` and `length` are **char** offsets into the document's text — the same space
/// [`FindMatch`] reports in, so a match can be turned into a range directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceRange {
    pub position: usize,
    pub length: usize,
    pub replacement: String,
}

impl ReplaceOptions {
    /// A replace that finds the text exactly as `find` describes and keeps the default
    /// (historical) format policy.
    pub fn new(find: FindOptions) -> Self {
        Self {
            find,
            format_policy: ReplaceFormatPolicy::default(),
        }
    }

    pub fn with_format_policy(mut self, policy: ReplaceFormatPolicy) -> Self {
        self.format_policy = policy;
        self
    }
}
