//! TextDocument implementation.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::{DocumentError, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::{DjotExportOptions, DjotImportOptions, ResourceType, TextDirection, WrapMode};
use frontend::commands::{
    block_commands, document_commands, document_inspection_commands, document_io_commands,
    document_search_commands, frame_commands, resource_commands, table_cell_commands,
    table_commands, undo_redo_commands,
};

use crate::convert::{self, to_i64, to_usize};
use crate::cursor::TextCursor;
use crate::events::{self, DocumentEvent, Subscription};
use crate::flow::FormatChangeKind;
use crate::inner::TextDocumentInner;
use crate::operation::{
    DjotImportResult, DocxExportResult, EpubExportResult, HtmlImportResult, MarkdownImportResult,
    Operation,
};
use crate::{BlockFormat, BlockInfo, DocumentStats, FindMatch, FindOptions, ReplaceRange};

/// A rich text document.
///
/// Owns the backend (database, event hub, undo/redo manager) and provides
/// document-level operations. All cursor-based editing goes through
/// [`TextCursor`], obtained via [`cursor()`](TextDocument::cursor) or
/// [`cursor_at()`](TextDocument::cursor_at).
///
/// Internally uses `Arc<Mutex<...>>` so that multiple [`TextCursor`]s can
/// coexist and edit concurrently. Cloning a `TextDocument` creates a new
/// handle to the **same** underlying document (like Qt's implicit sharing).
#[derive(Clone)]
pub struct TextDocument {
    pub(crate) inner: Arc<Mutex<TextDocumentInner>>,
}

/// Test-only accessor for the underlying rope-backed store. Not part
/// of the stable public API.
impl TextDocument {
    #[doc(hidden)]
    pub fn rope_store_for_test(&self) -> std::sync::Arc<common::database::Store> {
        let inner = self.inner.lock();
        std::sync::Arc::clone(inner.ctx.db_context.get_store())
    }
}

impl TextDocument {
    // ── Construction ──────────────────────────────────────────

    /// Create a new, empty document.
    ///
    /// # Panics
    ///
    /// Panics if the database context cannot be created (e.g. filesystem error).
    /// Use [`TextDocument::try_new`] for a fallible alternative.
    pub fn new() -> Self {
        Self::try_new().expect("failed to initialize document")
    }

    /// Create a new, empty document, returning an error on failure.
    pub fn try_new() -> Result<Self> {
        let ctx = frontend::AppContext::new();
        let doc_inner = TextDocumentInner::initialize(ctx)?;
        let inner = Arc::new(Mutex::new(doc_inner));

        // Bridge backend long-operation events to public DocumentEvent.
        Self::subscribe_long_operation_events(&inner);

        Ok(Self { inner })
    }

    /// Subscribe to backend long-operation events and bridge them to DocumentEvent.
    fn subscribe_long_operation_events(inner: &Arc<Mutex<TextDocumentInner>>) {
        use frontend::common::event::{LongOperationEvent as LOE, Origin};

        let weak = Arc::downgrade(inner);
        let mut locked = inner.lock();

        // Progress
        let w = weak.clone();
        let progress_tok =
            locked
                .event_client
                .subscribe(Origin::LongOperation(LOE::Progress), move |event| {
                    if let Some(inner) = w.upgrade() {
                        let (op_id, percent, message) = parse_progress_data(&event.data);
                        let mut inner = inner.lock();
                        inner.queue_event(DocumentEvent::LongOperationProgress {
                            operation_id: op_id,
                            percent,
                            message,
                        });
                    }
                });

        // Completed
        let w = weak.clone();
        let completed_tok =
            locked
                .event_client
                .subscribe(Origin::LongOperation(LOE::Completed), move |event| {
                    if let Some(inner) = w.upgrade() {
                        let op_id = parse_id_data(&event.data);
                        let mut inner = inner.lock();
                        inner.queue_event(DocumentEvent::DocumentReset);
                        inner.check_block_count_changed();
                        inner.reset_cached_child_order();
                        inner.queue_event(DocumentEvent::LongOperationFinished {
                            operation_id: op_id,
                            success: true,
                            error: None,
                        });
                    }
                });

        // Cancelled
        let w = weak.clone();
        let cancelled_tok =
            locked
                .event_client
                .subscribe(Origin::LongOperation(LOE::Cancelled), move |event| {
                    if let Some(inner) = w.upgrade() {
                        let op_id = parse_id_data(&event.data);
                        let mut inner = inner.lock();
                        inner.queue_event(DocumentEvent::LongOperationFinished {
                            operation_id: op_id,
                            success: false,
                            error: Some("cancelled".into()),
                        });
                    }
                });

        // Failed
        let failed_tok =
            locked
                .event_client
                .subscribe(Origin::LongOperation(LOE::Failed), move |event| {
                    if let Some(inner) = weak.upgrade() {
                        let (op_id, error) = parse_failed_data(&event.data);
                        let mut inner = inner.lock();
                        inner.queue_event(DocumentEvent::LongOperationFinished {
                            operation_id: op_id,
                            success: false,
                            error: Some(error),
                        });
                    }
                });

        locked.long_op_subscriptions.extend([
            progress_tok,
            completed_tok,
            cancelled_tok,
            failed_tok,
        ]);
    }

    // ── Whole-document content ────────────────────────────────

    /// Replace the entire document with plain text. Clears undo history.
    pub fn set_plain_text(&self, text: &str) -> Result<()> {
        let queued = {
            let mut inner = self.inner.lock();
            let dto = frontend::document_io::ImportPlainTextDto {
                plain_text: text.into(),
            };
            document_io_commands::import_plain_text(&inner.ctx, &dto)?;
            undo_redo_commands::clear_stack(&inner.ctx, inner.stack_id);
            inner.invalidate_text_cache();
            inner.rehighlight_all();
            inner.queue_event(DocumentEvent::DocumentReset);
            inner.check_block_count_changed();
            inner.reset_cached_child_order();
            inner.queue_event(DocumentEvent::UndoRedoChanged {
                can_undo: false,
                can_redo: false,
            });
            inner.take_queued_events()
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Export the entire document as plain text, in reading order.
    ///
    /// This is the **human-readable** view: prose only. Embedded objects (a table) contribute
    /// their content but not the `U+FFFC` anchor the document holds where they sit — which is
    /// what you want for a `cat`-style export, and is why the crate's fast path bails the
    /// moment a table exists.
    ///
    /// **Do not compute offsets from this string.** It is deliberately not
    /// character-for-character the text a search runs against: that text carries the object
    /// anchors, so a position taken here is short by two characters per preceding table. For
    /// an addressable view — one whose offsets [`find_all`](Self::find_all) and
    /// [`replace_text`](Self::replace_text) agree with — use
    /// [`djot_to_plain_text`](crate::djot_to_plain_text), which is pinned to match the
    /// document's own search text exactly.
    ///
    /// The two are allowed to differ in that one respect and no other; in particular they
    /// agree on **order**. They did not always: this export used to hoist every blockquote's
    /// prose to the end of the document (`"> a0\n\na"` came back as `"a\na0"`), because it
    /// concatenated frames in creation order instead of sorting all blocks by
    /// `document_position`. See `plain_text_order_tests`.
    pub fn to_plain_text(&self) -> Result<String> {
        let mut inner = self.inner.lock();
        Ok(inner.plain_text()?.to_string())
    }

    /// Replace the entire document with Markdown. Clears undo history.
    ///
    /// This is a **long operation**. Returns a typed [`Operation`] handle.
    pub fn set_markdown(&self, markdown: &str) -> Result<Operation<MarkdownImportResult>> {
        let mut inner = self.inner.lock();
        inner.invalidate_text_cache();
        let dto = frontend::document_io::ImportMarkdownDto {
            markdown_text: markdown.into(),
        };
        let op_id = document_io_commands::import_markdown(&inner.ctx, &dto)?;
        Ok(Operation::new(
            op_id,
            &inner.ctx,
            Box::new(|ctx, id| {
                document_io_commands::get_import_markdown_result(ctx, id)
                    .ok()
                    .flatten()
                    .map(|r| {
                        Ok(MarkdownImportResult {
                            block_count: to_usize(r.block_count),
                        })
                    })
            }),
        ))
    }

    /// Export the entire document as Markdown.
    pub fn to_markdown(&self) -> Result<String> {
        let inner = self.inner.lock();
        let dto = document_io_commands::export_markdown(&inner.ctx)?;
        Ok(dto.markdown_text)
    }

    /// Replace the entire document with djot markup. Clears undo history.
    ///
    /// This is a **long operation**. Returns a typed [`Operation`] handle.
    pub fn set_djot(&self, djot: &str) -> Result<Operation<DjotImportResult>> {
        self.set_djot_with_options(djot, DjotImportOptions::default())
    }

    /// Replace the entire document with djot markup, selecting which optional
    /// block attributes (alignment, line height, direction, non-breakable
    /// lines, background color) are applied via `options`. Clears undo history.
    ///
    /// This is a **long operation**. Returns a typed [`Operation`] handle.
    pub fn set_djot_with_options(
        &self,
        djot: &str,
        options: DjotImportOptions,
    ) -> Result<Operation<DjotImportResult>> {
        let mut inner = self.inner.lock();
        inner.invalidate_text_cache();
        let dto = frontend::document_io::ImportDjotDto {
            djot_text: djot.into(),
            options,
        };
        let op_id = document_io_commands::import_djot(&inner.ctx, &dto)?;
        Ok(Operation::new(
            op_id,
            &inner.ctx,
            Box::new(|ctx, id| {
                document_io_commands::get_import_djot_result(ctx, id)
                    .ok()
                    .flatten()
                    .map(|r| {
                        Ok(DjotImportResult {
                            block_count: to_usize(r.block_count),
                        })
                    })
            }),
        ))
    }

    /// Export the entire document as djot markup.
    pub fn to_djot(&self) -> Result<String> {
        self.to_djot_with_options(DjotExportOptions::default())
    }

    /// Export the entire document as djot markup, selecting which optional block
    /// attributes (alignment, line height, direction, non-breakable lines,
    /// background color) are emitted via `options`.
    pub fn to_djot_with_options(&self, options: DjotExportOptions) -> Result<String> {
        let inner = self.inner.lock();
        let dto = document_io_commands::export_djot(&inner.ctx, &options)?;
        Ok(dto.djot_text)
    }

    /// Replace the entire document with HTML. Clears undo history.
    ///
    /// This is a **long operation**. Returns a typed [`Operation`] handle.
    pub fn set_html(&self, html: &str) -> Result<Operation<HtmlImportResult>> {
        let mut inner = self.inner.lock();
        inner.invalidate_text_cache();
        let dto = frontend::document_io::ImportHtmlDto {
            html_text: html.into(),
        };
        let op_id = document_io_commands::import_html(&inner.ctx, &dto)?;
        Ok(Operation::new(
            op_id,
            &inner.ctx,
            Box::new(|ctx, id| {
                document_io_commands::get_import_html_result(ctx, id)
                    .ok()
                    .flatten()
                    .map(|r| {
                        Ok(HtmlImportResult {
                            block_count: to_usize(r.block_count),
                        })
                    })
            }),
        ))
    }

    /// Export the entire document as HTML.
    pub fn to_html(&self) -> Result<String> {
        let inner = self.inner.lock();
        let dto = document_io_commands::export_html(&inner.ctx)?;
        Ok(dto.html_text)
    }

    /// Export the entire document as LaTeX.
    pub fn to_latex(&self, document_class: &str, include_preamble: bool) -> Result<String> {
        let inner = self.inner.lock();
        let dto = frontend::document_io::ExportLatexDto {
            document_class: document_class.into(),
            include_preamble,
        };
        let result = document_io_commands::export_latex(&inner.ctx, &dto)?;
        Ok(result.latex_text)
    }

    /// Export the entire document as DOCX to a file path.
    ///
    /// This is a **long operation**. Returns a typed [`Operation`] handle.
    pub fn to_docx(&self, output_path: &str) -> Result<Operation<DocxExportResult>> {
        self.to_docx_with_options(output_path, crate::DocxExportOptions::default())
    }

    /// As [`to_docx`](Self::to_docx), but with page geometry + base typography overrides — a
    /// *manuscript* style (page size, margins, body font, line spacing, first-line indent,
    /// alignment, and an optional page-number header). Per-block RTL is emitted automatically
    /// from each block's own direction, independent of these options.
    pub fn to_docx_with_options(
        &self,
        output_path: &str,
        options: crate::DocxExportOptions,
    ) -> Result<Operation<DocxExportResult>> {
        let inner = self.inner.lock();
        let dto = frontend::document_io::ExportDocxDto {
            output_path: output_path.into(),
            options,
        };
        let op_id = document_io_commands::export_docx(&inner.ctx, &dto)?;
        Ok(Operation::new(
            op_id,
            &inner.ctx,
            Box::new(|ctx, id| {
                document_io_commands::get_export_docx_result(ctx, id)
                    .ok()
                    .flatten()
                    .map(|r| {
                        Ok(DocxExportResult {
                            file_path: r.file_path,
                            paragraph_count: to_usize(r.paragraph_count),
                        })
                    })
            }),
        ))
    }

    /// Export the entire document as an EPUB 3 file to a file path.
    ///
    /// This is a **long operation**. Returns a typed [`Operation`] handle.
    pub fn to_epub(&self, output_path: &str) -> Result<Operation<EpubExportResult>> {
        self.to_epub_with_options(output_path, crate::EpubExportOptions::default())
    }

    /// As [`to_epub`](Self::to_epub), but with book-level metadata (title, author, language,
    /// reading direction). The document is split into chapters at the shallowest heading level
    /// present (e.g. every top-level `# Chapter` heading) — see
    /// [`EpubExportOptions`](crate::EpubExportOptions) for details.
    pub fn to_epub_with_options(
        &self,
        output_path: &str,
        options: crate::EpubExportOptions,
    ) -> Result<Operation<EpubExportResult>> {
        let inner = self.inner.lock();
        let dto = frontend::document_io::ExportEpubDto {
            output_path: output_path.into(),
            options,
        };
        let op_id = document_io_commands::export_epub(&inner.ctx, &dto)?;
        Ok(Operation::new(
            op_id,
            &inner.ctx,
            Box::new(|ctx, id| {
                document_io_commands::get_export_epub_result(ctx, id)
                    .ok()
                    .flatten()
                    .map(|r| {
                        Ok(EpubExportResult {
                            file_path: r.file_path,
                            chapter_count: to_usize(r.chapter_count),
                        })
                    })
            }),
        ))
    }

    /// Clear all document content and reset to an empty state.
    pub fn clear(&self) -> Result<()> {
        let queued = {
            let mut inner = self.inner.lock();
            let dto = frontend::document_io::ImportPlainTextDto {
                plain_text: String::new(),
            };
            document_io_commands::import_plain_text(&inner.ctx, &dto)?;
            undo_redo_commands::clear_stack(&inner.ctx, inner.stack_id);
            inner.invalidate_text_cache();
            inner.rehighlight_all();
            inner.queue_event(DocumentEvent::DocumentReset);
            inner.check_block_count_changed();
            inner.reset_cached_child_order();
            inner.queue_event(DocumentEvent::UndoRedoChanged {
                can_undo: false,
                can_redo: false,
            });
            inner.take_queued_events()
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    // ── Cursor factory ───────────────────────────────────────

    /// Create a cursor at position 0.
    pub fn cursor(&self) -> TextCursor {
        self.cursor_at(0)
    }

    /// Create a cursor at the given position. If `position` falls
    /// inside an extended grapheme cluster (decomposed accents, ZWJ
    /// emoji, skin-tone sequences, flag pairs), the cursor snaps
    /// forward to the end of the containing cluster so subsequent
    /// `NextCharacter`/`PreviousCharacter` round-trips remain identity.
    pub fn cursor_at(&self, position: usize) -> TextCursor {
        let data = {
            let mut inner = self.inner.lock();
            inner.register_cursor(position)
        };
        let cursor = TextCursor {
            doc: self.inner.clone(),
            data,
        };
        cursor.snap_position_to_grapheme_boundary();
        cursor
    }

    // ── Document queries ─────────────────────────────────────

    /// Get document statistics. O(1) — reads cached values.
    pub fn stats(&self) -> DocumentStats {
        let inner = self.inner.lock();
        let dto = document_inspection_commands::get_document_stats(&inner.ctx)
            .expect("get_document_stats should not fail");
        DocumentStats::from(&dto)
    }

    /// Get the total character count. O(1) — reads cached value.
    pub fn character_count(&self) -> usize {
        let inner = self.inner.lock();
        let dto = document_inspection_commands::get_document_stats(&inner.ctx)
            .expect("get_document_stats should not fail");
        to_usize(dto.character_count)
    }

    /// Get the number of blocks (paragraphs). O(1) — reads cached value.
    pub fn block_count(&self) -> usize {
        let inner = self.inner.lock();
        let dto = document_inspection_commands::get_document_stats(&inner.ctx)
            .expect("get_document_stats should not fail");
        to_usize(dto.block_count)
    }

    /// Returns true if the document has no text content.
    pub fn is_empty(&self) -> bool {
        self.character_count() == 0
    }

    /// Get text at a position for a given length.
    pub fn text_at(&self, position: usize, length: usize) -> Result<String> {
        let inner = self.inner.lock();
        let dto = frontend::document_inspection::GetTextAtPositionDto {
            position: to_i64(position),
            length: to_i64(length),
        };
        let result = document_inspection_commands::get_text_at_position(&inner.ctx, &dto)?;
        Ok(result.text)
    }

    /// Find the inline segment containing `position` and return its
    /// stable element id (synthesized from `(block_id, byte_start)`
    /// via [`common::format_runs::synth_element_id`]) together with the
    /// segment's absolute start position and the character offset of
    /// `position` within the segment. Used by accessibility layers to
    /// convert a document-absolute character position into the
    /// `(element_id, character_index_in_run)` coordinate space
    /// AccessKit's `TextPosition` expects.
    ///
    /// Returns `None` when the position is outside the document.
    /// Returns the element at position `position - 1` when `position`
    /// falls exactly on an element boundary, matching the "cursor
    /// belongs to the preceding element at a boundary" convention
    /// used throughout text-document.
    pub fn find_element_at_position(&self, position: usize) -> Option<(u64, usize, usize)> {
        let block_info = self.block_at(position).ok()?;
        let block_start = block_info.start;
        let offset_in_block = position.checked_sub(block_start)?;
        let block = crate::text_block::TextBlock {
            doc: std::sync::Arc::clone(&self.inner),
            block_id: block_info.block_id,
        };
        let frags = block.fragments();
        // Walk fragments; match the fragment that contains
        // `offset_in_block`. For a boundary position shared with the
        // next fragment, prefer the preceding fragment (boundary
        // belongs to the end of the previous element).
        let mut last_text: Option<(u64, usize, usize, usize)> = None; // (id, abs_start, frag_offset, frag_length)
        for frag in &frags {
            match frag {
                crate::flow::FragmentContent::Text {
                    offset,
                    length,
                    element_id,
                    ..
                } => {
                    let frag_start = *offset;
                    let frag_end = frag_start + *length;
                    if offset_in_block >= frag_start && offset_in_block < frag_end {
                        let abs_start = block_start + frag_start;
                        let offset_within = offset_in_block - frag_start;
                        return Some((*element_id, abs_start, offset_within));
                    }
                    // Record as a candidate for the "end-of-element"
                    // boundary fallback (offset_in_block == frag_end).
                    if offset_in_block == frag_end {
                        last_text =
                            Some((*element_id, block_start + frag_start, frag_start, *length));
                    }
                }
                crate::flow::FragmentContent::Image {
                    offset, element_id, ..
                } => {
                    if offset_in_block == *offset {
                        return Some((*element_id, block_start + offset, 0));
                    }
                }
            }
        }
        // Boundary fallback: position was at the end of the last text
        // fragment we saw.
        last_text.map(|(id, abs_start, _, length)| (id, abs_start, length))
    }

    /// Get info about the block at a position. O(log n).
    pub fn block_at(&self, position: usize) -> Result<BlockInfo> {
        let inner = self.inner.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(position),
        };
        let result = document_inspection_commands::get_block_at_position(&inner.ctx, &dto)?;
        Ok(BlockInfo::from(&result))
    }

    /// Get the block format at a position.
    pub fn block_format_at(&self, position: usize) -> Result<BlockFormat> {
        let inner = self.inner.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(position),
        };
        let block_info = document_inspection_commands::get_block_at_position(&inner.ctx, &dto)?;
        let block_id = block_info.block_id;
        let block_id = block_id as u64;
        let block_dto = frontend::commands::block_commands::get_block(&inner.ctx, &block_id)?
            .ok_or_else(|| DocumentError::NotFound("block not found".into()))?;
        Ok(BlockFormat::from(&block_dto))
    }

    // ── Flow traversal (layout engine API) ─────────────────

    /// Walk the main frame's visual flow in document order.
    ///
    /// Returns the top-level flow elements — blocks, tables, and
    /// sub-frames — in the order defined by the main frame's
    /// `child_order`. Table cell contents are NOT included here;
    /// access them through [`TextTableCell::blocks()`](crate::TextTableCell::blocks).
    ///
    /// This is the primary entry point for layout initialization.
    pub fn flow(&self) -> Vec<crate::flow::FlowElement> {
        let inner = self.inner.lock();
        let main_frame_id = get_main_frame_id(&inner);
        crate::text_frame::build_flow_elements(&inner, &self.inner, main_frame_id)
    }

    /// Get a read-only handle to a block by its entity ID.
    ///
    /// Entity IDs are stable across insertions and deletions.
    /// Returns `None` if no block with this ID exists.
    pub fn block_by_id(&self, block_id: usize) -> Option<crate::text_block::TextBlock> {
        let inner = self.inner.lock();
        let exists = frontend::commands::block_commands::get_block(&inner.ctx, &(block_id as u64))
            .ok()
            .flatten()
            .is_some();

        if exists {
            Some(crate::text_block::TextBlock {
                doc: self.inner.clone(),
                block_id,
            })
        } else {
            None
        }
    }

    /// Build a single `BlockSnapshot` for the block at the given position.
    ///
    /// This is O(k) where k = format runs + image anchors in that block,
    /// compared to `snapshot_flow()` which is O(n) over the entire document.
    /// Use for incremental layout updates after single-block edits.
    pub fn snapshot_block_at_position(
        &self,
        position: usize,
    ) -> Option<crate::flow::BlockSnapshot> {
        self.snapshot_block_at_position_masked(position, &crate::highlight::HighlightMask::all())
    }

    /// Like [`snapshot_block_at_position`](Self::snapshot_block_at_position)
    /// but with **no highlights applied** — base fragments and empty
    /// `paint_highlights`, regardless of the active sessions. Used by the
    /// incremental relayout path of a view that has opted out of highlights.
    pub fn snapshot_block_at_position_without_highlights(
        &self,
        position: usize,
    ) -> Option<crate::flow::BlockSnapshot> {
        self.snapshot_block_at_position_masked(position, &crate::highlight::HighlightMask::none())
    }

    /// Like [`snapshot_block_at_position`](Self::snapshot_block_at_position) but rendering
    /// only the sessions `mask` admits — the per-view incremental path (two panes over one
    /// document can carry different find sessions). `all()` = the plain method; `none()` = the
    /// without-highlights method.
    pub fn snapshot_block_at_position_masked(
        &self,
        position: usize,
        mask: &crate::highlight::HighlightMask,
    ) -> Option<crate::flow::BlockSnapshot> {
        let inner = self.inner.lock();
        // Effective kind resolved once here (the join over the mask's sessions), then threaded
        // down with the mask itself.
        let hl = crate::highlight::SnapshotHighlights {
            kind: inner.highlights.effective_kind(mask),
            mask,
        };
        let main_frame_id = get_main_frame_id(&inner);
        let store = inner.ctx.db_context.get_store();

        // Rope-authoritative fast path. When every block is mirrored to the
        // rope (now true with tables — see `rope_positions_match_flow`), the
        // rope IS the position space the snapshot reports in, so we must also
        // *locate* the block via the rope. Walking a hand-rolled `running_pos`
        // here instead would search in the old cells-inline-no-sentinel space
        // and then report the rope position — an off-by-the-sentinel mismatch
        // for any block after a table.
        if common::database::rope_helpers::rope_positions_match_flow(store)
            && let Some((block_id, _, _)) =
                common::database::rope_helpers::find_block_at_char_position(store, position as i64)
        {
            return crate::text_block::build_block_snapshot(&inner, block_id, hl);
        }

        // Collect all block IDs in document order, traversing into nested frames
        let ordered_block_ids = collect_frame_block_ids(&inner, main_frame_id)?;

        // Walk blocks computing positions on the fly
        let pos = position as i64;
        let mut running_pos: i64 = 0;
        for &block_id in &ordered_block_ids {
            let block_dto = block_commands::get_block(&inner.ctx, &block_id)
                .ok()
                .flatten()?;
            let entity: common::entities::Block = block_dto.clone().into();
            let block_end =
                running_pos + common::database::rope_helpers::block_char_length(&entity, store);
            if pos >= running_pos && pos <= block_end {
                return crate::text_block::build_block_snapshot_with_position(
                    &inner,
                    block_id,
                    Some(running_pos as usize),
                    hl,
                );
            }
            running_pos = block_end + 1;
        }

        // Fallback to last block
        if let Some(&last_id) = ordered_block_ids.last() {
            return crate::text_block::build_block_snapshot(&inner, last_id, hl);
        }
        None
    }

    /// Get a read-only handle to the block containing the given
    /// character position. Returns `None` if position is out of range.
    pub fn block_at_position(&self, position: usize) -> Option<crate::text_block::TextBlock> {
        let inner = self.inner.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(position),
        };
        let result = document_inspection_commands::get_block_at_position(&inner.ctx, &dto).ok()?;
        Some(crate::text_block::TextBlock {
            doc: self.inner.clone(),
            block_id: result.block_id as usize,
        })
    }

    /// Get a read-only handle to a block by its 0-indexed global
    /// block number.
    ///
    /// **O(n)**: requires scanning all blocks sorted by
    /// `document_position` to find the nth one. Prefer
    /// [`block_at_position()`](TextDocument::block_at_position) or
    /// [`block_by_id()`](TextDocument::block_by_id) in
    /// performance-sensitive paths.
    pub fn block_by_number(&self, block_number: usize) -> Option<crate::text_block::TextBlock> {
        let inner = self.inner.lock();
        let all_blocks = frontend::commands::block_commands::get_all_block(&inner.ctx).ok()?;
        let mut sorted: Vec<_> = all_blocks.into_iter().collect();
        let store = inner.ctx.db_context.get_store();
        crate::inner::refresh_block_positions(&mut sorted, store);
        sorted.sort_by_key(|b| b.document_position);

        sorted
            .get(block_number)
            .map(|b| crate::text_block::TextBlock {
                doc: self.inner.clone(),
                block_id: b.id as usize,
            })
    }

    /// All blocks in the document, sorted by `document_position`. **O(n)**.
    ///
    /// Returns blocks from all frames, including those inside table cells.
    /// This is the efficient way to iterate all blocks — avoids the O(n^2)
    /// cost of calling `block_by_number(i)` in a loop.
    pub fn blocks(&self) -> Vec<crate::text_block::TextBlock> {
        let inner = self.inner.lock();
        let all_blocks =
            frontend::commands::block_commands::get_all_block(&inner.ctx).unwrap_or_default();
        let mut sorted: Vec<_> = all_blocks.into_iter().collect();
        let store = inner.ctx.db_context.get_store();
        crate::inner::refresh_block_positions(&mut sorted, store);
        sorted.sort_by_key(|b| b.document_position);
        sorted
            .iter()
            .map(|b| crate::text_block::TextBlock {
                doc: self.inner.clone(),
                block_id: b.id as usize,
            })
            .collect()
    }

    /// All blocks whose character range intersects `[position, position + length)`.
    ///
    /// **O(n)**: scans all blocks once. Returns them sorted by `document_position`.
    /// A block intersects if its range `[block.position, block.position + block.length)`
    /// overlaps the query range. An empty query range (`length == 0`) returns the
    /// block containing that position, if any.
    pub fn blocks_in_range(
        &self,
        position: usize,
        length: usize,
    ) -> Vec<crate::text_block::TextBlock> {
        let inner = self.inner.lock();
        let all_blocks =
            frontend::commands::block_commands::get_all_block(&inner.ctx).unwrap_or_default();
        let mut sorted: Vec<_> = all_blocks.into_iter().collect();
        let store = inner.ctx.db_context.get_store();
        crate::inner::refresh_block_positions(&mut sorted, store);
        sorted.sort_by_key(|b| b.document_position);

        let range_start = position;
        let range_end = position + length;
        sorted
            .iter()
            .filter(|b| {
                let block_start = b.document_position.max(0) as usize;
                let entity: common::entities::Block = (*b).clone().into();
                let block_end = block_start
                    + common::database::rope_helpers::block_char_length(&entity, store).max(0)
                        as usize;
                // Overlap check: block intersects [range_start, range_end)
                if length == 0 {
                    // Point query: block contains the position
                    range_start >= block_start && range_start < block_end
                } else {
                    block_start < range_end && block_end > range_start
                }
            })
            .map(|b| crate::text_block::TextBlock {
                doc: self.inner.clone(),
                block_id: b.id as usize,
            })
            .collect()
    }

    /// Snapshot the entire main flow in a single lock acquisition.
    ///
    /// Returns a [`FlowSnapshot`](crate::FlowSnapshot) containing snapshots
    /// for every element in the flow.
    pub fn snapshot_flow(&self) -> crate::flow::FlowSnapshot {
        self.snapshot_flow_masked(&crate::highlight::HighlightMask::all())
    }

    /// Snapshot the entire main flow with **no highlights applied** — base
    /// fragments and empty `paint_highlights` on every block, regardless of
    /// the active sessions.
    ///
    /// This is the per-view opt-out: a read-only viewer that should stay
    /// free of search / spell / syntax highlighting pulls *this* snapshot
    /// instead of [`snapshot_flow`](Self::snapshot_flow). Because suppression
    /// happens at build time, it works for metric-affecting sessions too
    /// (whose highlights are otherwise merged into `fragments` irreversibly).
    pub fn snapshot_flow_without_highlights(&self) -> crate::flow::FlowSnapshot {
        self.snapshot_flow_masked(&crate::highlight::HighlightMask::none())
    }

    /// Snapshot the entire main flow rendering only the sessions `mask` admits.
    ///
    /// The generalization of the plain / without-highlights pair: `all()` shows every session,
    /// `none()` shows none, and `only([...])` shows a chosen set — which is how two panes over
    /// one shared document carry different find sessions. The effective
    /// [`HighlighterKind`](crate::highlight) is resolved **once here**, at the snapshot root,
    /// and threaded down, so a view showing only paint-only sessions never pays the reshape
    /// path for a metric session it does not show.
    pub fn snapshot_flow_masked(
        &self,
        mask: &crate::highlight::HighlightMask,
    ) -> crate::flow::FlowSnapshot {
        let inner = self.inner.lock();
        let main_frame_id = get_main_frame_id(&inner);
        let hl = crate::highlight::SnapshotHighlights {
            kind: inner.highlights.effective_kind(mask),
            mask,
        };
        let elements = crate::text_frame::build_flow_snapshot(&inner, main_frame_id, hl);
        crate::flow::FlowSnapshot { elements }
    }

    // ── Search ───────────────────────────────────────────────

    /// Find the next (or previous) occurrence. Returns `None` if not found.
    pub fn find(
        &self,
        query: &str,
        from: usize,
        options: &FindOptions,
    ) -> Result<Option<FindMatch>> {
        let inner = self.inner.lock();
        let dto = options.to_find_text_dto(query, from);
        let result = document_search_commands::find_text(&inner.ctx, &dto)?;
        Ok(convert::find_result_to_match(&result))
    }

    /// Find all occurrences.
    pub fn find_all(&self, query: &str, options: &FindOptions) -> Result<Vec<FindMatch>> {
        let inner = self.inner.lock();
        let dto = options.to_find_all_dto(query);
        let result = document_search_commands::find_all(&inner.ctx, &dto)?;
        Ok(convert::find_all_to_matches(&result))
    }

    /// Replace occurrences. Returns the number of replacements. Undoable.
    ///
    /// `options` carries both how to find the text and — via
    /// [`ReplaceOptions::format_policy`] — what the replacement wears where it
    /// overwrites formatted prose. The default drops the formatting under the replaced
    /// range, which is fine for plain text and destructive for a rename that lands on a
    /// partly-bold name; pass a different policy when that matters.
    pub fn replace_text(
        &self,
        query: &str,
        replacement: &str,
        replace_all: bool,
        options: &crate::ReplaceOptions,
    ) -> Result<usize> {
        let (count, queued) = {
            let mut inner = self.inner.lock();
            let dto = options.to_replace_dto(query, replacement, replace_all);
            let result =
                document_search_commands::replace_text(&inner.ctx, Some(inner.stack_id), &dto)?;
            let count = to_usize(result.replacements_count);
            inner.invalidate_text_cache();
            if count > 0 {
                inner.modified = true;
                inner.rehighlight_all();
                // Replacements are scattered across the document — we can't
                // provide a single position/chars delta. Signal "content changed
                // from position 0, affecting `count` sites" so the consumer
                // knows to re-read.
                inner.queue_event(DocumentEvent::ContentsChanged {
                    position: 0,
                    chars_removed: 0,
                    chars_added: 0,
                    blocks_affected: count,
                });
                inner.check_block_count_changed();
                inner.check_flow_changed();
                let can_undo = undo_redo_commands::can_undo(&inner.ctx, Some(inner.stack_id));
                let can_redo = undo_redo_commands::can_redo(&inner.ctx, Some(inner.stack_id));
                inner.queue_event(DocumentEvent::UndoRedoChanged { can_undo, can_redo });
            }
            (count, inner.take_queued_events())
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(count)
    }

    /// Replace an explicit set of ranges, each with **its own** replacement text. Undoable
    /// as one action, however many ranges it touches.
    ///
    /// [`replace_text`](Self::replace_text) can only put the same string at every match.
    /// This is for the case where the caller decides *per occurrence* — a reviewed bulk
    /// rename where some occurrences are unticked, or one that preserves the case it found
    /// (`AURÉLIEN` → `AURÉLIAN`, not `aurélian`).
    ///
    /// ⚠ **Do not build the ranges with a separate `find_all` call.** The document can move
    /// between the two, and the ranges then address text that is no longer there — which
    /// does not fail, it rewrites *the wrong words*. Use
    /// [`find_and_replace`](Self::find_and_replace), which does both under one lock.
    ///
    /// Ranges that straddle a block boundary, or that overlap one another, are **skipped**;
    /// the returned count reflects only what was actually applied.
    pub fn replace_ranges(
        &self,
        ranges: &[ReplaceRange],
        options: &crate::ReplaceOptions,
    ) -> Result<usize> {
        let (count, queued) = {
            let mut inner = self.inner.lock();
            let count = Self::replace_ranges_locked(&mut inner, ranges, options)?;
            (count, inner.take_queued_events())
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(count)
    }

    /// Find every match of `query` and let `decide` choose what each becomes — **atomically**.
    ///
    /// `decide` is handed the matched text and the index of the match, and returns the
    /// replacement, or `None` to leave that occurrence alone. So a rename that preserves case
    /// and skips the occurrences a writer unticked is one call:
    ///
    /// ```no_run
    /// # use text_document::{TextDocument, FindOptions, ReplaceOptions};
    /// # let doc = TextDocument::new();
    /// # let excluded: Vec<usize> = vec![];
    /// doc.find_and_replace("Aurélien", &ReplaceOptions::new(FindOptions::default()), |matched, i| {
    ///     if excluded.contains(&i) {
    ///         return None; // the writer unticked this one
    ///     }
    ///     Some(if matched.chars().all(char::is_uppercase) { "AURÉLIAN".into() } else { "Aurélian".into() })
    /// })?;
    /// # Ok::<(), text_document::DocumentError>(())
    /// ```
    ///
    /// **The scan and the splice happen under one lock**, which is the whole point. Calling
    /// `find_all` and then `replace_ranges` would drop the lock in between, and the document
    /// can be edited there — after which every range addresses text that has moved. That does
    /// not raise an error; it silently rewrites the wrong words. The document mutex is not
    /// reentrant, so composing the two public methods cannot close the gap; only doing both
    /// inside one can.
    pub fn find_and_replace(
        &self,
        query: &str,
        options: &crate::ReplaceOptions,
        mut decide: impl FnMut(&str, usize) -> Option<String>,
    ) -> Result<usize> {
        let (count, queued) = {
            let mut inner = self.inner.lock();

            // Scan. The matched TEXT comes back with the offsets, sliced by the use case from
            // the very text it searched — deliberately, so this never has to slice a
            // whole-document string of its own. The only one reachable here is
            // `to_plain_text`, which is the human-readable view and carries no `U+FFFC` anchor
            // for an embedded table; slicing it with these offsets would be wrong by two
            // characters per preceding table, and the rename would rewrite the wrong words.
            let found = {
                let dto = options.find.to_find_all_dto(query);
                document_search_commands::find_all(&inner.ctx, &dto)?
            };

            // …decide, against the document as it is RIGHT NOW…
            let mut ranges: Vec<ReplaceRange> = Vec::new();
            for (i, ((&position, &length), matched)) in found
                .positions
                .iter()
                .zip(found.lengths.iter())
                .zip(found.matched_texts.iter())
                .enumerate()
            {
                if let Some(replacement) = decide(matched, i) {
                    ranges.push(ReplaceRange {
                        position: to_usize(position),
                        length: to_usize(length),
                        replacement,
                    });
                }
            }

            // …and splice — all without ever letting go of the lock.
            let count = if ranges.is_empty() {
                0
            } else {
                Self::replace_ranges_locked(&mut inner, &ranges, options)?
            };
            (count, inner.take_queued_events())
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(count)
    }

    /// The splice, with the lock already held. Shared by [`Self::replace_ranges`] and
    /// [`Self::find_and_replace`] so the second cannot drift from the first.
    fn replace_ranges_locked(
        inner: &mut crate::inner::TextDocumentInner,
        ranges: &[ReplaceRange],
        options: &crate::ReplaceOptions,
    ) -> Result<usize> {
        let dto = options.to_replace_ranges_dto(ranges);
        let result =
            document_search_commands::replace_ranges(&inner.ctx, Some(inner.stack_id), &dto)?;
        let count = to_usize(result.replacements_count);

        inner.invalidate_text_cache();
        if count > 0 {
            inner.modified = true;
            inner.rehighlight_all();
            inner.queue_event(DocumentEvent::ContentsChanged {
                position: 0,
                chars_removed: 0,
                chars_added: 0,
                blocks_affected: count,
            });
            inner.check_block_count_changed();
            inner.check_flow_changed();
            let can_undo = undo_redo_commands::can_undo(&inner.ctx, Some(inner.stack_id));
            let can_redo = undo_redo_commands::can_redo(&inner.ctx, Some(inner.stack_id));
            inner.queue_event(DocumentEvent::UndoRedoChanged { can_undo, can_redo });
        }
        Ok(count)
    }

    // ── Resources ────────────────────────────────────────────

    /// Add a resource (image, stylesheet) to the document.
    pub fn add_resource(
        &self,
        resource_type: ResourceType,
        name: &str,
        mime_type: &str,
        data: &[u8],
    ) -> Result<()> {
        let mut inner = self.inner.lock();
        let dto = frontend::resource::dtos::CreateResourceDto {
            created_at: Default::default(),
            updated_at: Default::default(),
            resource_type,
            name: name.into(),
            url: String::new(),
            mime_type: mime_type.into(),
            data_base64: BASE64.encode(data),
        };
        let created = resource_commands::create_resource(
            &inner.ctx,
            Some(inner.stack_id),
            &dto,
            inner.document_id,
            -1,
        )?;
        inner.resource_cache.insert(name.to_string(), created.id);
        Ok(())
    }

    /// Get a resource by name. Returns `None` if not found.
    ///
    /// Uses an internal cache to avoid scanning all resources on repeated lookups.
    pub fn resource(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let mut inner = self.inner.lock();

        // Fast path: check the name → ID cache.
        if let Some(&id) = inner.resource_cache.get(name) {
            if let Some(r) = resource_commands::get_resource(&inner.ctx, &id)? {
                let bytes = BASE64
                    .decode(&r.data_base64)
                    .map_err(|e| DocumentError::Internal(e.into()))?;
                return Ok(Some(bytes));
            }
            // ID was stale — fall through to full scan.
            inner.resource_cache.remove(name);
        }

        // Slow path: linear scan, then populate cache for the match.
        let all = resource_commands::get_all_resource(&inner.ctx)?;
        for r in &all {
            if r.name == name {
                inner.resource_cache.insert(name.to_string(), r.id);
                let bytes = BASE64
                    .decode(&r.data_base64)
                    .map_err(|e| DocumentError::Internal(e.into()))?;
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }

    // ── Undo / Redo ──────────────────────────────────────────

    /// Undo the last operation.
    pub fn undo(&self) -> Result<()> {
        let queued = {
            let mut inner = self.inner.lock();
            let before = capture_block_state(&inner);
            let result = undo_redo_commands::undo(&inner.ctx, Some(inner.stack_id));
            inner.invalidate_text_cache();
            result?;
            inner.rehighlight_all();
            emit_undo_redo_change_events(&mut inner, &before);
            inner.check_block_count_changed();
            inner.check_flow_changed();
            let can_undo = undo_redo_commands::can_undo(&inner.ctx, Some(inner.stack_id));
            let can_redo = undo_redo_commands::can_redo(&inner.ctx, Some(inner.stack_id));
            inner.queue_event(DocumentEvent::UndoRedoChanged { can_undo, can_redo });
            inner.take_queued_events()
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Redo the last undone operation.
    pub fn redo(&self) -> Result<()> {
        let queued = {
            let mut inner = self.inner.lock();
            let before = capture_block_state(&inner);
            let result = undo_redo_commands::redo(&inner.ctx, Some(inner.stack_id));
            inner.invalidate_text_cache();
            result?;
            inner.rehighlight_all();
            emit_undo_redo_change_events(&mut inner, &before);
            inner.check_block_count_changed();
            inner.check_flow_changed();
            let can_undo = undo_redo_commands::can_undo(&inner.ctx, Some(inner.stack_id));
            let can_redo = undo_redo_commands::can_redo(&inner.ctx, Some(inner.stack_id));
            inner.queue_event(DocumentEvent::UndoRedoChanged { can_undo, can_redo });
            inner.take_queued_events()
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Returns true if there are operations that can be undone.
    pub fn can_undo(&self) -> bool {
        let inner = self.inner.lock();
        undo_redo_commands::can_undo(&inner.ctx, Some(inner.stack_id))
    }

    /// Returns true if there are operations that can be redone.
    pub fn can_redo(&self) -> bool {
        let inner = self.inner.lock();
        undo_redo_commands::can_redo(&inner.ctx, Some(inner.stack_id))
    }

    /// Clear all undo/redo history.
    pub fn clear_undo_redo(&self) {
        let inner = self.inner.lock();
        undo_redo_commands::clear_stack(&inner.ctx, inner.stack_id);
    }

    // ── Modified state ───────────────────────────────────────

    /// Returns true if the document has been modified since creation or last reset.
    pub fn is_modified(&self) -> bool {
        self.inner.lock().modified
    }

    /// Set or clear the modified flag.
    pub fn set_modified(&self, modified: bool) {
        let queued = {
            let mut inner = self.inner.lock();
            if inner.modified != modified {
                inner.modified = modified;
                inner.queue_event(DocumentEvent::ModificationChanged(modified));
            }
            inner.take_queued_events()
        };
        crate::inner::dispatch_queued_events(queued);
    }

    // ── Document properties ──────────────────────────────────

    /// Get the document title.
    pub fn title(&self) -> String {
        let inner = self.inner.lock();
        document_commands::get_document(&inner.ctx, &inner.document_id)
            .ok()
            .flatten()
            .map(|d| d.title)
            .unwrap_or_default()
    }

    /// Set the document title.
    pub fn set_title(&self, title: &str) -> Result<()> {
        let inner = self.inner.lock();
        let doc = document_commands::get_document(&inner.ctx, &inner.document_id)?
            .ok_or_else(|| DocumentError::NotFound("document not found".into()))?;
        let mut update: frontend::document::dtos::UpdateDocumentDto = doc.into();
        update.title = title.into();
        document_commands::update_document(&inner.ctx, Some(inner.stack_id), &update)?;
        Ok(())
    }

    /// Get the text direction.
    pub fn text_direction(&self) -> TextDirection {
        let inner = self.inner.lock();
        document_commands::get_document(&inner.ctx, &inner.document_id)
            .ok()
            .flatten()
            .map(|d| d.text_direction)
            .unwrap_or(TextDirection::LeftToRight)
    }

    /// Set the text direction.
    pub fn set_text_direction(&self, direction: TextDirection) -> Result<()> {
        let inner = self.inner.lock();
        let doc = document_commands::get_document(&inner.ctx, &inner.document_id)?
            .ok_or_else(|| DocumentError::NotFound("document not found".into()))?;
        let mut update: frontend::document::dtos::UpdateDocumentDto = doc.into();
        update.text_direction = direction;
        document_commands::update_document(&inner.ctx, Some(inner.stack_id), &update)?;
        Ok(())
    }

    /// Get the default wrap mode.
    pub fn default_wrap_mode(&self) -> WrapMode {
        let inner = self.inner.lock();
        document_commands::get_document(&inner.ctx, &inner.document_id)
            .ok()
            .flatten()
            .map(|d| d.default_wrap_mode)
            .unwrap_or(WrapMode::WordWrap)
    }

    /// Set the default wrap mode.
    pub fn set_default_wrap_mode(&self, mode: WrapMode) -> Result<()> {
        let inner = self.inner.lock();
        let doc = document_commands::get_document(&inner.ctx, &inner.document_id)?
            .ok_or_else(|| DocumentError::NotFound("document not found".into()))?;
        let mut update: frontend::document::dtos::UpdateDocumentDto = doc.into();
        update.default_wrap_mode = mode;
        document_commands::update_document(&inner.ctx, Some(inner.stack_id), &update)?;
        Ok(())
    }

    /// Get the document-wide default language (ISO 639-1 code, e.g. "en").
    /// This is the fallback hyphenation language for blocks that don't set
    /// their own `language`. Defaults to `"en"` when never set.
    pub fn default_language(&self) -> String {
        let inner = self.inner.lock();
        document_commands::get_document(&inner.ctx, &inner.document_id)
            .ok()
            .flatten()
            .and_then(|d| d.default_language)
            .unwrap_or_else(|| "en".to_string())
    }

    /// Set the document-wide default language (ISO 639-1 code). Blocks
    /// without an explicit `language` inherit this for hyphenation.
    pub fn set_default_language(&self, language: &str) -> Result<()> {
        let inner = self.inner.lock();
        let doc = document_commands::get_document(&inner.ctx, &inner.document_id)?
            .ok_or_else(|| DocumentError::NotFound("document not found".into()))?;
        let mut update: frontend::document::dtos::UpdateDocumentDto = doc.into();
        update.default_language = Some(language.to_string());
        document_commands::update_document(&inner.ctx, Some(inner.stack_id), &update)?;
        Ok(())
    }

    // ── Event subscription ───────────────────────────────────

    /// Subscribe to document events via callback.
    ///
    /// Callbacks are invoked **outside** the document lock (after the editing
    /// operation completes and the lock is released). It is safe to call
    /// `TextDocument` or `TextCursor` methods from within the callback without
    /// risk of deadlock. However, keep callbacks lightweight — they run
    /// synchronously on the calling thread and block the caller until they
    /// return.
    ///
    /// Drop the returned [`Subscription`] to unsubscribe.
    ///
    /// # Breaking change (v0.0.6)
    ///
    /// The callback bound changed from `Send` to `Send + Sync` in v0.0.6
    /// to support `Arc`-based dispatch. Callbacks that capture non-`Sync`
    /// types (e.g., `Rc<T>`, `Cell<T>`) must be wrapped in a `Mutex`.
    pub fn on_change<F>(&self, callback: F) -> Subscription
    where
        F: Fn(DocumentEvent) + Send + Sync + 'static,
    {
        let mut inner = self.inner.lock();
        events::subscribe_inner(&mut inner, callback)
    }

    /// Return events accumulated since the last `poll_events()` call.
    ///
    /// This delivery path is independent of callback dispatch via
    /// [`on_change`](Self::on_change) — using both simultaneously is safe
    /// and each path sees every event exactly once.
    pub fn poll_events(&self) -> Vec<DocumentEvent> {
        let mut inner = self.inner.lock();
        inner.drain_poll_events()
    }

    // ── Syntax highlighting ──────────────────────────────────

    /// Attach a single syntax highlighter to this document — the classic, one-highlighter
    /// entry point.
    ///
    /// Immediately re-highlights the entire document. **Replaces** the one highlighter this
    /// method manages, and *only* that one: a spell-checker or find layer registered
    /// independently via [`add_syntax_session`](Self::add_syntax_session) /
    /// [`add_range_session`](Self::add_range_session) is left untouched. Pass `None` to remove
    /// it.
    ///
    /// This is a convenience over the session registry — it owns exactly one "shim" session. A
    /// host that wants to manage several layers uses the session methods directly.
    pub fn set_syntax_highlighter(&self, highlighter: Option<Arc<dyn crate::SyntaxHighlighter>>) {
        let queued = {
            let mut inner = self.inner.lock();
            let prev_kind = inner.highlight_kind;
            let installed = highlighter.is_some();
            inner.highlights.set_shim(highlighter);
            if installed {
                inner.rehighlight_all(); // recomputes highlight_kind
            } else {
                inner.recompute_highlight_kind();
            }
            Self::queue_highlight_changed(&mut inner, 0, 0, prev_kind);
            inner.take_queued_events()
        };
        crate::inner::dispatch_queued_events(queued);
    }

    /// Register a **syntax session** — a [`SyntaxHighlighter`](crate::SyntaxHighlighter)
    /// callback with its own per-block state cascade — and return its [`SessionId`].
    ///
    /// Unlike [`set_syntax_highlighter`](Self::set_syntax_highlighter), this **adds** rather
    /// than replaces: a document can carry a syntax highlighter and a spell-checker at once,
    /// each a session, merged in registration order (a later session's field wins). Sessions
    /// remain visible only in views whose [`HighlightMask`](crate::highlight::HighlightMask)
    /// admits them.
    pub fn add_syntax_session(
        &self,
        highlighter: Arc<dyn crate::SyntaxHighlighter>,
    ) -> crate::highlight::SessionId {
        let (id, queued) = {
            let mut inner = self.inner.lock();
            let prev_kind = inner.highlight_kind;
            let id = inner.highlights.add_syntax(highlighter);
            inner.rehighlight_all();
            Self::queue_highlight_changed(&mut inner, 0, 0, prev_kind);
            (id, inner.take_queued_events())
        };
        crate::inner::dispatch_queued_events(queued);
        id
    }

    /// Register an empty **range session** — absolute-offset ranges set with
    /// [`set_session_ranges`](Self::set_session_ranges), the shape used for search and (later)
    /// an externally-driven spell-checker. Returns its [`SessionId`].
    ///
    /// A view's own find session is a range session it alone admits; that is how two panes
    /// over one document highlight different queries.
    pub fn add_range_session(&self) -> crate::highlight::SessionId {
        let mut inner = self.inner.lock();
        inner.highlights.add_range()
        // No repaint: an empty range session shows nothing until its ranges are set.
    }

    /// Replace the ranges of a range session (absolute char offsets, the space
    /// [`FindMatch`](crate::FindMatch) reports in). Returns `false` if `id` is not a range
    /// session.
    ///
    /// Fires a highlight-changed event so live views showing this session re-snapshot — the
    /// only signal there is, since the ranges do not mutate the document.
    pub fn set_session_ranges(
        &self,
        id: crate::highlight::SessionId,
        ranges: Vec<crate::highlight::RangeHighlight>,
    ) -> bool {
        let (ok, queued) = {
            let mut inner = self.inner.lock();
            let prev_kind = inner.highlight_kind;
            let ok = inner.highlights.set_ranges(id, ranges);
            if ok {
                inner.recompute_highlight_kind();
                Self::queue_highlight_changed(&mut inner, 0, 0, prev_kind);
            }
            (ok, inner.take_queued_events())
        };
        crate::inner::dispatch_queued_events(queued);
        ok
    }

    /// Retire a session (of either kind). Returns whether it existed.
    pub fn remove_session(&self, id: crate::highlight::SessionId) -> bool {
        let (existed, queued) = {
            let mut inner = self.inner.lock();
            let prev_kind = inner.highlight_kind;
            let existed = inner.highlights.remove(id);
            if existed {
                inner.recompute_highlight_kind();
                Self::queue_highlight_changed(&mut inner, 0, 0, prev_kind);
            }
            (existed, inner.take_queued_events())
        };
        crate::inner::dispatch_queued_events(queued);
        existed
    }

    /// Re-highlight the entire document.
    ///
    /// Call this when the highlighter's rules change (e.g., new keywords
    /// were added, spellcheck dictionary updated).
    pub fn rehighlight(&self) {
        let queued = {
            let mut inner = self.inner.lock();
            let prev_kind = inner.highlight_kind;
            inner.rehighlight_all();
            Self::queue_highlight_changed(&mut inner, 0, 0, prev_kind);
            inner.take_queued_events()
        };
        crate::inner::dispatch_queued_events(queued);
    }

    /// Re-highlight a single block and cascade to subsequent blocks if
    /// the block state changes.
    pub fn rehighlight_block(&self, block_id: usize) {
        let queued = {
            let mut inner = self.inner.lock();
            let prev_kind = inner.highlight_kind;
            inner.rehighlight_from_block(block_id);
            Self::queue_highlight_changed(&mut inner, 0, 0, prev_kind);
            inner.take_queued_events()
        };
        crate::inner::dispatch_queued_events(queued);
    }

    /// Queue the relayout/repaint notification for a highlight-only change.
    ///
    /// Highlighting overlays the layout without touching stored formatting,
    /// so it emits no edit event on its own — subscribers (live editors)
    /// must be told to re-snapshot. The event kind depends on whether the
    /// shaping input (`fragments`) changed:
    ///
    /// - A change that leaves `fragments` BASE on both sides (paint-only ↔
    ///   paint-only / none) emits [`DocumentEvent::HighlightPaintChanged`],
    ///   which the editor handles by recoloring the cached layout without
    ///   reshaping.
    /// - Any transition involving a metric-affecting highlighter changes
    ///   `fragments` (highlights are merged in / removed), so it emits
    ///   [`DocumentEvent::FormatChanged`] (full relayout, caret/scroll
    ///   preserved).
    ///
    /// `position` / `length` are advisory: the editor's recolor path
    /// re-derives the whole snapshot, so callers pass `0, 0` (whole-document)
    /// today.
    fn queue_highlight_changed(
        inner: &mut TextDocumentInner,
        position: usize,
        length: usize,
        prev_kind: crate::highlight::HighlighterKind,
    ) {
        use crate::highlight::HighlighterKind::{Metric, None as KNone, PaintOnly};
        let new_kind = inner.highlight_kind;
        let event = match (prev_kind, new_kind) {
            // No highlighter before or after — nothing changed.
            (KNone, KNone) => return,
            // Fragments are BASE on both sides: recolor-only.
            (PaintOnly, PaintOnly) | (KNone, PaintOnly) | (PaintOnly, KNone) => {
                DocumentEvent::HighlightPaintChanged { position, length }
            }
            // A metric highlighter is involved on one side: fragments change.
            (KNone, Metric)
            | (Metric, Metric)
            | (Metric, PaintOnly)
            | (Metric, KNone)
            | (PaintOnly, Metric) => DocumentEvent::FormatChanged {
                position,
                length,
                kind: crate::flow::FormatChangeKind::Character,
            },
        };
        inner.queue_event(event);
    }
}

impl Default for TextDocument {
    fn default() -> Self {
        Self::new()
    }
}

// ── Undo/redo change detection helpers ─────────────────────────

/// Lightweight block state for before/after comparison during undo/redo.
struct UndoBlockState {
    id: u64,
    position: i64,
    text_length: i64,
    plain_text: String,
    format: BlockFormat,
}

/// Capture the state of all blocks, sorted by document_position.
fn capture_block_state(inner: &TextDocumentInner) -> Vec<UndoBlockState> {
    let mut all_blocks =
        frontend::commands::block_commands::get_all_block(&inner.ctx).unwrap_or_default();
    let store = inner.ctx.db_context.get_store();
    crate::inner::refresh_block_positions(&mut all_blocks, store);
    let mut states: Vec<UndoBlockState> = all_blocks
        .into_iter()
        .map(|b| {
            let format = BlockFormat::from(&b);
            let entity: common::entities::Block = b.clone().into();
            let plain_text =
                common::database::rope_helpers::block_content_via_store(&entity, store);
            let text_length = common::database::rope_helpers::block_char_length(&entity, store);
            UndoBlockState {
                id: b.id,
                position: b.document_position,
                text_length,
                plain_text,
                format,
            }
        })
        .collect();
    states.sort_by_key(|s| s.position);
    states
}

/// Build the full document text from sorted block states (joined with newlines).
fn build_doc_text(states: &[UndoBlockState]) -> String {
    states
        .iter()
        .map(|s| s.plain_text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compute the precise edit between two strings by comparing common prefix and suffix.
/// Returns `(edit_offset, chars_removed, chars_added)`.
fn compute_text_edit(before: &str, after: &str) -> (usize, usize, usize) {
    let before_chars: Vec<char> = before.chars().collect();
    let after_chars: Vec<char> = after.chars().collect();

    // Common prefix
    let prefix_len = before_chars
        .iter()
        .zip(after_chars.iter())
        .take_while(|(a, b)| a == b)
        .count();

    // Common suffix (not overlapping with prefix)
    let before_remaining = before_chars.len() - prefix_len;
    let after_remaining = after_chars.len() - prefix_len;
    let suffix_len = before_chars
        .iter()
        .rev()
        .zip(after_chars.iter().rev())
        .take(before_remaining.min(after_remaining))
        .take_while(|(a, b)| a == b)
        .count();

    let removed = before_remaining - suffix_len;
    let added = after_remaining - suffix_len;

    (prefix_len, removed, added)
}

/// Compare block state before and after undo/redo and emit
/// ContentsChanged / FormatChanged events for affected regions.
fn emit_undo_redo_change_events(inner: &mut TextDocumentInner, before: &[UndoBlockState]) {
    let after = capture_block_state(inner);

    // Build a map of block id → state for the "before" set.
    let before_map: std::collections::HashMap<u64, &UndoBlockState> =
        before.iter().map(|s| (s.id, s)).collect();
    let after_map: std::collections::HashMap<u64, &UndoBlockState> =
        after.iter().map(|s| (s.id, s)).collect();

    // Track the affected content region (earliest position, total old/new length).
    let mut content_changed = false;
    let mut earliest_pos: Option<usize> = None;
    let mut old_end: usize = 0;
    let mut new_end: usize = 0;
    let mut blocks_affected: usize = 0;

    let mut format_only_changes: Vec<(usize, usize)> = Vec::new(); // (position, length)

    // Check blocks present in both before and after.
    for after_state in &after {
        if let Some(before_state) = before_map.get(&after_state.id) {
            let text_changed = before_state.plain_text != after_state.plain_text
                || before_state.text_length != after_state.text_length;
            let format_changed = before_state.format != after_state.format;

            if text_changed {
                content_changed = true;
                blocks_affected += 1;
                let pos = after_state.position.max(0) as usize;
                earliest_pos = Some(earliest_pos.map_or(pos, |p: usize| p.min(pos)));
                old_end = old_end.max(
                    before_state.position.max(0) as usize
                        + before_state.text_length.max(0) as usize,
                );
                new_end = new_end.max(pos + after_state.text_length.max(0) as usize);
            } else if format_changed {
                let pos = after_state.position.max(0) as usize;
                let len = after_state.text_length.max(0) as usize;
                format_only_changes.push((pos, len));
            }
        } else {
            // Block exists in after but not in before — new block from undo/redo.
            content_changed = true;
            blocks_affected += 1;
            let pos = after_state.position.max(0) as usize;
            earliest_pos = Some(earliest_pos.map_or(pos, |p: usize| p.min(pos)));
            new_end = new_end.max(pos + after_state.text_length.max(0) as usize);
        }
    }

    // Check blocks that were removed (present in before but not after).
    for before_state in before {
        if !after_map.contains_key(&before_state.id) {
            content_changed = true;
            blocks_affected += 1;
            let pos = before_state.position.max(0) as usize;
            earliest_pos = Some(earliest_pos.map_or(pos, |p: usize| p.min(pos)));
            old_end = old_end.max(pos + before_state.text_length.max(0) as usize);
        }
    }

    if content_changed {
        let position = earliest_pos.unwrap_or(0);
        let chars_removed = old_end.saturating_sub(position);
        let chars_added = new_end.saturating_sub(position);

        // Use a precise text-level diff for cursor adjustment so cursors land
        // at the actual edit point rather than the end of the affected block.
        let before_text = build_doc_text(before);
        let after_text = build_doc_text(&after);
        let (edit_offset, precise_removed, precise_added) =
            compute_text_edit(&before_text, &after_text);
        if precise_removed > 0 || precise_added > 0 {
            inner.adjust_cursors(edit_offset, precise_removed, precise_added);
        }

        inner.queue_event(DocumentEvent::ContentsChanged {
            position,
            chars_removed,
            chars_added,
            blocks_affected,
        });
    }

    // Emit FormatChanged for blocks where only formatting changed (not content).
    for (position, length) in format_only_changes {
        inner.queue_event(DocumentEvent::FormatChanged {
            position,
            length,
            kind: FormatChangeKind::Block,
        });
    }
}

// ── Flow helpers ──────────────────────────────────────────────

/// Get the main frame ID for the document.
/// Collect all block IDs in document order from a frame, recursing into nested
/// sub-frames (negative entries in child_order).
fn collect_frame_block_ids(
    inner: &TextDocumentInner,
    frame_id: frontend::common::types::EntityId,
) -> Option<Vec<u64>> {
    let frame_dto = frame_commands::get_frame(&inner.ctx, &frame_id)
        .ok()
        .flatten()?;

    if !frame_dto.child_order.is_empty() {
        let mut block_ids = Vec::new();
        for &entry in &frame_dto.child_order {
            if entry > 0 {
                block_ids.push(entry as u64);
            } else if entry < 0 {
                let sub_frame_id = (-entry) as u64;
                let sub_frame = frame_commands::get_frame(&inner.ctx, &sub_frame_id)
                    .ok()
                    .flatten();
                if let Some(ref sf) = sub_frame {
                    if let Some(table_id) = sf.table {
                        // Table anchor frame: collect blocks from cell frames
                        // in row-major order, matching collect_block_ids_recursive.
                        if let Some(table_dto) = table_commands::get_table(&inner.ctx, &table_id)
                            .ok()
                            .flatten()
                        {
                            let mut cell_dtos: Vec<_> = table_dto
                                .cells
                                .iter()
                                .filter_map(|&cid| {
                                    table_cell_commands::get_table_cell(&inner.ctx, &cid)
                                        .ok()
                                        .flatten()
                                })
                                .collect();
                            cell_dtos
                                .sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));
                            for cell_dto in &cell_dtos {
                                if let Some(cf_id) = cell_dto.cell_frame
                                    && let Some(cf_ids) = collect_frame_block_ids(inner, cf_id)
                                {
                                    block_ids.extend(cf_ids);
                                }
                            }
                        }
                    } else if let Some(sub_ids) = collect_frame_block_ids(inner, sub_frame_id) {
                        block_ids.extend(sub_ids);
                    }
                }
            }
        }
        Some(block_ids)
    } else {
        Some(frame_dto.blocks.to_vec())
    }
}

pub(crate) fn get_main_frame_id(inner: &TextDocumentInner) -> frontend::common::types::EntityId {
    // The document's first frame is the main frame.
    let frames = frontend::commands::document_commands::get_document_relationship(
        &inner.ctx,
        &inner.document_id,
        &frontend::document::dtos::DocumentRelationshipField::Frames,
    )
    .unwrap_or_default();

    frames.first().copied().unwrap_or(0)
}

// ── Long-operation event data helpers ─────────────────────────

/// Parse progress JSON: `{"id":"...", "percentage": 50.0, "message": "..."}`
fn parse_progress_data(data: &Option<String>) -> (String, f64, String) {
    let Some(json) = data else {
        return (String::new(), 0.0, String::new());
    };
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let id = v["id"].as_str().unwrap_or_default().to_string();
    let pct = v["percentage"].as_f64().unwrap_or(0.0);
    let msg = v["message"].as_str().unwrap_or_default().to_string();
    (id, pct, msg)
}

/// Parse completed/cancelled JSON: `{"id":"..."}`
fn parse_id_data(data: &Option<String>) -> String {
    let Some(json) = data else {
        return String::new();
    };
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    v["id"].as_str().unwrap_or_default().to_string()
}

/// Parse failed JSON: `{"id":"...", "error":"..."}`
fn parse_failed_data(data: &Option<String>) -> (String, String) {
    let Some(json) = data else {
        return (String::new(), "unknown error".into());
    };
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let id = v["id"].as_str().unwrap_or_default().to_string();
    let error = v["error"].as_str().unwrap_or("unknown error").to_string();
    (id, error)
}
