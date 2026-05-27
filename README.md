[![crates.io](https://img.shields.io/crates/v/text-document?style=flat-square&logo=rust)](https://crates.io/crates/text-document)
[![API](https://docs.rs/text-document/badge.svg)](https://docs.rs/text-document)
![quality](https://img.shields.io/github/actions/workflow/status/ferntech-eu/text-document/ci.yml)
[![codecov](https://codecov.io/gh/FernTech-EU/text-document/graph/badge.svg?token=S4M513A2XR)](https://codecov.io/gh/FernTech-EU/text-document)
[![license](https://img.shields.io/badge/license-MPL--2.0-blue?style=flat-square)](#license)


# text-document

A rich text document model for Rust, inspired by Qt's QTextDocument/QTextCursor API. Companion crate is text-typeset, which render a document to GPU quads.

Built on a [ropey](https://github.com/cessen/ropey)-backed text store with a [Qleany](https://github.com/ferntech-eu/qleany)-generated Clean Architecture skeleton, full undo/redo, and multi-cursor support.

## Features

- **Rich text model**: Frames, Blocks with character data in a shared `ropey::Rope`, per-block byte-ranged `FormatRun`s, and `ImageAnchor`s (`InlineContent::Text | Image`)
- **Multi-cursor editing**: Qt-style cursors with automatic position adjustment
- **Full undo/redo**: Snapshot-based, with composite grouping (`begin_edit_block` / `end_edit_block`)
- **Import/Export**: Plain text, Markdown, HTML, LaTeX, DOCX
- **Search**: Find, find all, regex, replace (undoable)
- **Formatting**: Character format (`bold`, `italic`, `underline`, ...), block format (`alignment`, `heading_level`, ...), frame format
- **Syntax highlighting**: Generic `SyntaxHighlighter` trait (Qt's QSyntaxHighlighter-style) — shadow formatting layer visible to layout but invisible to export/cursor/undo. Multi-block state, per-block user data, full format control (colors, bold, italic, underline styles, ...). Auto re-highlights on edits with cascade.
- **Tables**: Insert, remove, row/column operations, cell merge/split, table/cell formatting, cursor-position-based convenience methods
- **Layout engine API**: Read-only handles (`TextBlock`, `TextFrame`, `TextTable`, `TextTableCell`, `TextList`), flow traversal, fragment-based text shaping, atomic snapshots (`TextFrame::snapshot()`, `FlowElement::snapshot()`), block parent context (`parent_frame_id`, `TableCellContext`), efficient block queries (`blocks()`, `blocks_in_range()`), incremental change events
- **Event system**: Callback-based (`on_change`) and polling-based (`poll_events`), with `FormatChangeKind` (Block vs Character), flow-level insert/remove events, and granular `ContentsChanged`/`FormatChanged` on undo/redo
- **Thread-safe**: `Send + Sync` throughout, `Arc<Mutex<...>>` interior mutability
- **Resources**: Image and stylesheet storage with base64 encoding

## Quick start

```rust
use text_document::{TextDocument, MoveMode, MoveOperation};

let doc = TextDocument::new();
doc.set_plain_text("Hello world").unwrap();

// Cursor-based editing
let cursor = doc.cursor();
cursor.move_position(MoveOperation::EndOfWord, MoveMode::KeepAnchor, 1);
cursor.insert_text("Goodbye").unwrap(); // replaces "Hello"

// Multiple cursors
let c1 = doc.cursor();
let c2 = doc.cursor_at(5);
c1.insert_text("A").unwrap();
// c2's position is automatically adjusted

// Undo
doc.undo().unwrap();

// Search
use text_document::FindOptions;
let matches = doc.find_all("world", &FindOptions::default()).unwrap();

// Export
let html = doc.to_html().unwrap();
let markdown = doc.to_markdown().unwrap();
```

## Layout engine API

Read-only handles for building a layout/rendering engine on top of the document model:

```rust
use text_document::{TextDocument, FlowElement, FragmentContent};

let doc = TextDocument::new();
doc.set_plain_text("Hello\nWorld").unwrap();

// Walk the document's visual flow
for element in doc.flow() {
    match element {
        FlowElement::Block(block) => {
            println!("Block {}: {:?}", block.id(), block.text());
            // Get formatting runs for glyph shaping
            for frag in block.fragments() {
                match frag {
                    FragmentContent::Text { text, format, offset, length } => {
                        println!("  text at {offset}: {text} (len={length})");
                    }
                    FragmentContent::Image { name, width, height, offset, .. } => {
                        println!("  image at {offset}: {name} ({width}x{height})");
                    }
                }
            }
        }
        FlowElement::Table(table) => {
            println!("Table {}x{}", table.rows(), table.columns());
        }
        FlowElement::Frame(frame) => {
            // Nested frames have their own flow
            let nested = frame.flow();
        }
    }
}

// Atomic snapshot for full layout
let snap = doc.snapshot_flow();

// Direct block access
let block = doc.block_at_position(0).unwrap();
let next = block.next(); // O(n) traversal
let snap = block.snapshot(); // all data in one lock
// snap.parent_frame_id  — which frame owns this block
// snap.table_cell        — Some(TableCellContext) if inside a table cell

// Efficient block iteration (O(n) instead of O(n²))
let all_blocks = doc.blocks();
let affected = doc.blocks_in_range(10, 50); // blocks in char range [10..60)

// Frame and flow element snapshots
let frame = block.frame();
let frame_snap = frame.snapshot(); // FrameSnapshot with nested content
let elem_snap = doc.flow()[0].snapshot(); // FlowElementSnapshot
```

## Table operations

```rust
use text_document::TextDocument;

let doc = TextDocument::new();
doc.set_plain_text("Before table").unwrap();

let cursor = doc.cursor_at(12);

// Insert a 3x2 table, get a handle back
let table = cursor.insert_table(3, 2).unwrap();
assert_eq!(table.rows(), 3);

// Explicit-ID mutations
cursor.insert_table_row(table.id(), 1).unwrap();   // insert row at index 1
cursor.remove_table_column(table.id(), 0).unwrap(); // remove first column

// Position-based convenience (cursor must be inside a table cell)
// cursor.insert_row_above().unwrap();
// cursor.remove_current_column().unwrap();
// cursor.set_current_table_format(&format).unwrap();
```

## Syntax highlighting

Attach a `SyntaxHighlighter` to apply visual-only formatting (spellcheck underlines, code coloring, search highlights, ...) that the layout engine sees but export/cursor/undo ignore:

```rust
use std::sync::Arc;
use text_document::{
    TextDocument, SyntaxHighlighter, HighlightContext, HighlightFormat,
    Color, UnderlineStyle,
};

// Spellcheck highlighter example
struct SpellChecker;

impl SyntaxHighlighter for SpellChecker {
    fn highlight_block(&self, text: &str, ctx: &mut HighlightContext) {
        // Mark "wrold" as misspelled wherever it appears
        for (i, _) in text.match_indices("wrold") {
            let char_offset = text[..i].chars().count();
            ctx.set_format(char_offset, 5, HighlightFormat {
                underline_style: Some(UnderlineStyle::SpellCheckUnderline),
                underline_color: Some(Color::rgb(255, 0, 0)),
                ..Default::default()
            });
        }
    }
}

let doc = TextDocument::new();
doc.set_plain_text("Hello wrold").unwrap();
doc.set_syntax_highlighter(Some(Arc::new(SpellChecker)));

// Layout sees the wavy underline; export/undo don't
assert_eq!(doc.to_plain_text().unwrap(), "Hello wrold");

// Multi-block state for constructs like /* ... */ comments:
// use ctx.previous_block_state() and ctx.set_current_block_state(n)

// Force re-highlight after rule changes (e.g. dictionary update):
doc.rehighlight();
```

## CLI

A command-line tool for format conversion and text processing:

```bash
# Convert between formats (detected by file extension)
text-document convert README.md output.html
text-document convert article.html article.tex

# Show document statistics
text-document stats manuscript.md

# Find text (grep-like output)
text-document find paper.md "TODO" --case-sensitive

# Find and replace
text-document replace draft.md "colour" "color" --output fixed.md

# Print to stdout in a different format
text-document cat notes.html --format plain
```

Supported formats:

| Extension | Import | Export |
|-----------|--------|--------|
| `.txt` | yes | yes |
| `.md` | yes | yes |
| `.html`/`.htm` | yes | yes |
| `.tex`/`.latex` | - | yes |
| `.docx` | - | yes |

## Document structure

```
Root
 +-- Document
     +-- Frame (root frame)
     |   +-- Block          // text comes from rope[byte_range]: "Hello world"
     |   |     format_runs:  [(6..11, bold)]            // "world" is bold
     |   |     block_images: [(byte_offset: 11, image)] // image @ U+FFFC sentinel
     |   +-- Block          // text comes from rope[byte_range]: "Second paragraph"
     +-- Table (rows: 2, columns: 3)
     |   +-- TableCell (row: 0, col: 0)
     |   |   +-- Frame (cell frame)
     |   |       +-- Block  // text comes from rope[byte_range]: "Cell content"
     |   +-- TableCell (row: 0, col: 1) ...
     +-- List (style: Decimal, indent: 1)
     +-- Resource (image data, stylesheets)
```

- **Rope**: a single `ropey::Rope` per document holds every block's character data. Block boundaries are encoded as `\n`; table positions and images are marked by the Unicode object-replacement sentinel `U+FFFC`. `BlockOffsetIndex` maps each block (and table marker) to its `(byte_start, byte_end)` range in the rope.
- **Frame**: contains Blocks and child Frames. `child_order` interleaves them (positive = block ID, negative = sub-frame ID).
- **Block**: a paragraph. Its text is a slice of the document rope; per-character formatting lives in a sorted, non-overlapping `Vec<FormatRun>` keyed by block id in `RopeStore.format_runs`. Images are anchored at byte offsets in `RopeStore.block_images`. Has `document_position` for O(log n) lookup.
- **FormatRun**: `{ byte_start, byte_end, format: CharacterFormat }` — one entry per contiguous span of identical formatting. Adjacent equal-format runs are coalesced.
- **ImageAnchor**: `{ byte_offset, name, width, height, quality, format }` — image attached to a specific byte position inside a block (where the rope holds a `U+FFFC` sentinel).
- **InlineSegment** (`common::format_runs::InlineSegment`): transient view type synthesized on demand from the rope slice + `format_runs` + `block_images` for readers (export, fragments, cursor); never stored.
- **List**: styling for list items (Disc, Decimal, LowerAlpha, ...). Blocks reference lists via weak relationship.
- **Table**: grid of TableCells, each with an optional cell frame containing Blocks.
- **Resource**: binary data (images, stylesheets) stored as base64.

All format fields are `Option<T>` — `None` means "inherit from parent/default", `Some(value)` means "explicitly set".

### Public API handles

| Handle | Obtained from | Purpose |
|--------|---------------|---------|
| `TextDocument` | `TextDocument::new()` | Document-level operations, flow traversal, `blocks()`, `blocks_in_range()` |
| `TextCursor` | `doc.cursor()` / `doc.cursor_at(pos)` | All mutations (text, formatting, tables, lists) |
| `TextBlock` | `doc.flow()`, `doc.blocks()`, etc. | Read-only block data, fragments, list membership, `snapshot()` with parent context |
| `TextFrame` | `block.frame()`, `FlowElement::Frame` | Read-only frame data, nested flow, `snapshot()` |
| `TextTable` | `cursor.insert_table()`, `FlowElement::Table` | Read-only table structure, cell access, snapshot |
| `TextTableCell` | `table.cell(row, col)` | Read-only cell data, blocks within cell |
| `TextList` | `block.list()` | Read-only list properties, item markers |

All handles are `Clone + Send + Sync` (backed by `Arc<Mutex<...>>` + entity ID).

## Storage backend

Character data is held in a single `ropey::Rope` per document — an Arc-shared
B+ tree that makes cloning O(1) and undo snapshots near-free regardless of
document size. Block boundaries are encoded as `\n` in the rope; images and
table positions use the Unicode object-replacement sentinel (U+FFFC).
Per-character formatting is stored as sorted, non-overlapping byte-ranged
`FormatRun`s on each block. The structural tree (Frames, Tables, Lists,
Resources) is held in `im::HashMap` tables, also O(1) to clone for snapshots.

The full undo / redo stack continues to use snapshot-based commands for
structural operations and hand-rolled inverse commands for high-frequency
edits (single-character insert, character format toggle). Both rely on the
backend's O(1) snapshot guarantee.

## Architecture

Generated by [Qleany](https://github.com/ferntech-eu/qleany), following Clean Architecture with Package by Feature (Vertical Slice). Character data lives in a `ropey::Rope`; structural entities (frames, blocks, tables, lists, resources) form a slim tree on top.

```
crates/
+-- public_api/       # TextDocument, TextCursor, DocumentEvent (the public crate)
+-- cli/              # Command-line tool
+-- frontend/         # AppContext, commands, event hub client
+-- common/           # Entities, RopeStore (rope + structural entities), events, undo/redo, repositories
+-- macros/           # #[uow_action] proc macro
+-- direct_access/    # Entity CRUD controllers + DTOs
+-- document_editing/ # 19 use cases (insert, delete, block, image, frame, list, fragment, table CRUD, merge/split cells, ...)
+-- document_formatting/ # 6 use cases (set/merge text format, block format, frame format, table format, cell format)
+-- document_io/      # 8 use cases (import/export plain text, markdown, HTML, LaTeX, DOCX)
+-- document_search/  # 3 use cases (find, find_all, replace)
+-- document_inspection/ # 4 use cases (stats, text at position, block at position, extract fragment)
+-- test_harness/       # Shared test setup utilities
```

Data flow: `TextDocument / TextCursor -> frontend::commands -> controllers -> use cases -> UoW -> RopeStore (rope + structural entities)`

## License

Licensed under [Mozilla Public License 2.0](https://www.mozilla.org/en-US/MPL/2.0/).

## Contribution

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

By contributing, you agree that your contributions will be licensed under the [Mozilla Public License 2.0](LICENSE). All commits must be signed off under the [Developer Certificate of Origin](DCO.md) (`git commit -s`).
