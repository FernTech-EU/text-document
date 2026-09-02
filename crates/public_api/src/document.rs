//! TextDocument implementation.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::{DocumentError, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::{
    DjotExportOptions, DjotImportOptions, MarkdownExportOptions, PlainTextExportOptions,
    ResourceType, TextDirection, WrapMode,
};
use frontend::commands::{
    block_commands, document_commands, document_inspection_commands, document_io_commands,
    document_search_commands, frame_commands, resource_commands, table_cell_commands,
    table_commands, undo_redo_commands,
};

use crate::HtmlExportOptions;
use crate::convert::{self, to_i64, to_usize};
use crate::cursor::TextCursor;
use crate::events::{self, DocumentEvent, Subscription};
use crate::flow::FormatChangeKind;
use crate::inner::TextDocumentInner;
use crate::operation::{
    DjotImportResult, DocxExportResult, EpubExportResult, HtmlImportResult, MarkdownImportResult,
    OdtExportResult, Operation, PdfExportResult,
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

    /// Create a document inside a shared [`DocumentBackend`].
    ///
    /// The document keeps its own store and its own undo stack, because undo
    /// snapshots and restores a whole store and two documents sharing one would
    /// roll each other back. What it shares is the event hub, the single thread
    /// draining it, and the long-operation manager.
    ///
    /// Use this wherever a host opens many documents at once. Each
    /// [`TextDocument::new`] starts an OS thread of its own, so a manuscript
    /// stream over a book-length project starts one per scene.
    ///
    /// The backend must outlive every document built in it: dropping it stops
    /// the pump. Holding a [`DocumentBackend`] clone beside the documents is
    /// enough, and it is what the documents themselves do.
    pub fn new_in(backend: &crate::DocumentBackend) -> Self {
        Self::try_new_in(backend).expect("failed to initialize document")
    }

    /// [`new_in`](Self::new_in), returning an error instead of panicking.
    pub fn try_new_in(backend: &crate::DocumentBackend) -> Result<Self> {
        let doc_inner = TextDocumentInner::initialize_in(backend)?;
        let inner = Arc::new(Mutex::new(doc_inner));
        Self::subscribe_long_operation_events(&inner);
        Ok(Self { inner })
    }

    /// Subscribe to backend long-operation events and bridge them to DocumentEvent.
    fn subscribe_long_operation_events(inner: &Arc<Mutex<TextDocumentInner>>) {
        use frontend::common::event::{LongOperationEvent as LOE, Origin};

        let weak = Arc::downgrade(inner);
        let mut locked = inner.lock();
        // In a shared backend the subscriptions go on the backend's client,
        // because that is the one with a thread behind it. The document's own
        // client exists but was never started: a second drain on one hub would
        // compete for each event, and flume hands an event to exactly one
        // receiver, so half of them would reach the wrong document.
        let client = match &locked.backend {
            Some(backend) => backend.client().clone(),
            None => locked.event_client.clone(),
        };

        // Progress
        let w = weak.clone();
        let progress_tok = client.subscribe(Origin::LongOperation(LOE::Progress), move |event| {
            if let Some(inner) = w.upgrade() {
                let (op_id, percent, message) = parse_progress_data(&event.data);
                let mut inner = inner.lock();
                if !inner.owns_operation(&op_id) {
                    return;
                }
                inner.queue_event(DocumentEvent::LongOperationProgress {
                    operation_id: op_id,
                    percent,
                    message,
                });
            }
        });

        // Completed
        let w = weak.clone();
        let completed_tok = client.subscribe(Origin::LongOperation(LOE::Completed), move |event| {
            if let Some(inner) = w.upgrade() {
                let op_id = parse_id_data(&event.data);
                let mut inner = inner.lock();
                if !inner.owns_operation(&op_id) {
                    return;
                }
                inner.own_operations.remove(&op_id);
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
        let cancelled_tok = client.subscribe(Origin::LongOperation(LOE::Cancelled), move |event| {
            if let Some(inner) = w.upgrade() {
                let op_id = parse_id_data(&event.data);
                let mut inner = inner.lock();
                if !inner.owns_operation(&op_id) {
                    return;
                }
                inner.own_operations.remove(&op_id);
                inner.queue_event(DocumentEvent::LongOperationFinished {
                    operation_id: op_id,
                    success: false,
                    error: Some("cancelled".into()),
                });
            }
        });

        // Failed
        let failed_tok = client.subscribe(Origin::LongOperation(LOE::Failed), move |event| {
            if let Some(inner) = weak.upgrade() {
                let (op_id, error) = parse_failed_data(&event.data);
                let mut inner = inner.lock();
                if !inner.owns_operation(&op_id) {
                    return;
                }
                inner.own_operations.remove(&op_id);
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
    /// an addressable view — one whose offsets [`find_all`](Self::find_all),
    /// [`replace_text`](Self::replace_text), a block's
    /// [`position()`](crate::TextBlock::position) and a cursor all agree with — use
    /// [`to_addressable_text`](Self::to_addressable_text) on a live document, or
    /// [`djot_to_plain_text`](crate::djot_to_plain_text) when all you hold is Djot source.
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

    /// [`to_plain_text`](Self::to_plain_text) for writing an actual `.txt` file: quoted
    /// blocks are indented four spaces per blockquote level, so an epigraph or a block
    /// quotation still reads as set-off matter in a format with no markup to say so.
    ///
    /// **Not** interchangeable with [`to_plain_text`](Self::to_plain_text), and not cached.
    /// That one is pinned to the document's addressable text — the text
    /// [`find_all`](Self::find_all) and [`replace_text`](Self::replace_text) compute
    /// offsets against — in everything but the object anchors, so indenting it would shift
    /// every offset inside a quote and desynchronise search from the document. Use this
    /// only for output nobody addresses back into the document.
    pub fn to_plain_text_indented(&self) -> Result<String> {
        let inner = self.inner.lock();
        let dto = document_io_commands::export_plain_text_indented(&inner.ctx)?;
        Ok(dto.plain_text)
    }

    /// [`to_plain_text`](Self::to_plain_text) with every presentation option chosen
    /// explicitly — quoted-block indentation, and a `U+000C` form feed before each block
    /// that asks to start a new page.
    ///
    /// Subject to the same warning as [`to_plain_text_indented`](Self::to_plain_text_indented):
    /// anything other than [`PlainTextExportOptions::addressable`] shifts offsets, so this is
    /// for files being written out, never for text anyone addresses back into the document.
    pub fn to_plain_text_with(&self, options: PlainTextExportOptions) -> Result<String> {
        let inner = self.inner.lock();
        let dto = document_io_commands::export_plain_text_with(&inner.ctx, options)?;
        Ok(dto.plain_text)
    }

    /// The document's **addressable text**: the exact string every offset this document
    /// deals out is an index into.
    ///
    /// One char space runs through the whole API — [`find_all`](Self::find_all) match
    /// positions, [`replace_ranges`](Self::replace_ranges) ranges, a block's
    /// [`position()`](crate::TextBlock::position), a cursor, an editor widget's selection.
    /// This is the string that space addresses, character for character: an embedded
    /// table occupies its `U+FFFC` [`TABLE_ANCHOR`](crate::TABLE_ANCHOR) here (plus its
    /// `\n` separator), exactly as the document holds it.
    ///
    /// Use it whenever a document offset and a document string travel together — capturing
    /// the quoted text under a selection, pairing block starts with the text they index,
    /// slicing context around a search hit. Pairing an offset with
    /// [`to_plain_text`](Self::to_plain_text) instead is the classic form of this bug: that
    /// is the human-readable **export**, it omits the anchors, and every offset after a
    /// table lands two characters off in it.
    ///
    /// Built by the same code path [`find_all`](Self::find_all) uses to build the text it
    /// searches, so the two cannot diverge. For the same view of bare Djot source — no live
    /// document at hand — use [`djot_to_plain_text`](crate::djot_to_plain_text), which is
    /// pinned to produce this very string for the same content. Not cached; it is a fresh
    /// read of the document each call.
    pub fn to_addressable_text(&self) -> Result<String> {
        let inner = self.inner.lock();
        let dto = document_search_commands::addressable_text(&inner.ctx)?;
        Ok(dto.text)
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
        inner.own_operations.insert(op_id.clone());
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

    /// [`to_markdown`](Self::to_markdown) with the presentation opt-ins — today, whether a
    /// block that asks to start a new page gets a raw-HTML page break emitted above it.
    /// Off by default, because raw HTML is not Markdown.
    pub fn to_markdown_with(&self, options: MarkdownExportOptions) -> Result<String> {
        let inner = self.inner.lock();
        let dto = document_io_commands::export_markdown_with(&inner.ctx, options)?;
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
        inner.own_operations.insert(op_id.clone());
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

    /// Replace the entire document with djot markup, **synchronously**, on the
    /// calling thread. Clears undo history.
    ///
    /// This is the right call for *loading* a document's initial content — the
    /// case where the caller is going to block for the result anyway.
    /// [`set_djot`](Self::set_djot) starts a long operation: it spawns a thread,
    /// and the caller then blocks in [`Operation::wait`] until that thread
    /// publishes. That round trip is pure overhead when there is no frame loop to
    /// keep responsive, and it does not shrink with the input — an *empty*
    /// document costs the same thread spawn and hand-off as a full one. Loading N
    /// documents in a loop paid it N times.
    ///
    /// Prefer [`set_djot`](Self::set_djot) when the import is genuinely long and
    /// the caller must stay responsive (it reports progress and can be
    /// cancelled); prefer this when the caller just wants the content in.
    ///
    /// Observationally equivalent to `set_djot(..).wait()` — same import, same
    /// `DocumentReset`, same cache/block bookkeeping — except that, having no
    /// operation, it emits no `LongOperation*` events and cannot be cancelled.
    pub fn set_djot_sync(&self, djot: &str) -> Result<DjotImportResult> {
        self.set_djot_sync_with_options(djot, DjotImportOptions::default())
    }

    /// As [`set_djot_sync`](Self::set_djot_sync), selecting which optional block
    /// attributes are applied via `options`.
    pub fn set_djot_sync_with_options(
        &self,
        djot: &str,
        options: DjotImportOptions,
    ) -> Result<DjotImportResult> {
        let (queued, block_count) = {
            let mut inner = self.inner.lock();
            inner.invalidate_text_cache();
            let dto = frontend::document_io::ImportDjotDto {
                djot_text: djot.into(),
                options,
            };
            let result = document_io_commands::import_djot_sync(&inner.ctx, &dto)?;
            // The same settling the async path performs when its operation
            // completes (see `subscribe_long_operation_events`), done inline here
            // because there is no completion event to hang it off.
            inner.queue_event(DocumentEvent::DocumentReset);
            inner.check_block_count_changed();
            inner.reset_cached_child_order();
            (inner.take_queued_events(), result.block_count)
        };
        // Dispatch outside the lock — a subscriber is free to call back in.
        crate::inner::dispatch_queued_events(queued);
        Ok(DjotImportResult {
            block_count: to_usize(block_count),
        })
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
        inner.own_operations.insert(op_id.clone());
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
    ///
    /// Inline images keep whatever `src` the document stores; placing the files
    /// those point at is the caller's business. Use
    /// [`to_html_with_options`](Self::to_html_with_options) to inline them
    /// instead, or to drop them.
    pub fn to_html(&self) -> Result<String> {
        let inner = self.inner.lock();
        let dto = document_io_commands::export_html(&inner.ctx)?;
        Ok(dto.html_text)
    }

    /// Export as HTML, choosing how inline images are represented.
    pub fn to_html_with_options(&self, options: HtmlExportOptions) -> Result<String> {
        let inner = self.inner.lock();
        let dto = document_io_commands::export_html_with_options(&inner.ctx, options)?;
        Ok(dto.html_text)
    }

    /// Export the entire document as LaTeX.
    ///
    /// Images are emitted as `\includegraphics{src}`, which LaTeX resolves
    /// against the filesystem at compile time — so the caller is responsible for
    /// placing those files beside the `.tex`. Use
    /// [`to_latex_with_options`](Self::to_latex_with_options) to drop them
    /// instead.
    pub fn to_latex(&self, document_class: &str, include_preamble: bool) -> Result<String> {
        self.to_latex_with_options(crate::LatexExportOptions {
            document_class: document_class.into(),
            include_preamble,
            omit_images: false,
        })
    }

    /// As [`to_latex`](Self::to_latex), but taking the full
    /// [`LatexExportOptions`](crate::LatexExportOptions) — the same document class and preamble
    /// knobs `to_latex` takes positionally, plus the choice of dropping inline images instead of
    /// emitting `\includegraphics{…}` for them.
    pub fn to_latex_with_options(&self, options: crate::LatexExportOptions) -> Result<String> {
        let inner = self.inner.lock();
        let dto = frontend::document_io::ExportLatexDto { options };
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
        let mut inner = self.inner.lock();
        let dto = frontend::document_io::ExportDocxDto {
            output_path: output_path.into(),
            options,
        };
        let op_id = document_io_commands::export_docx(&inner.ctx, &dto)?;
        inner.own_operations.insert(op_id.clone());
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
        let mut inner = self.inner.lock();
        let dto = frontend::document_io::ExportEpubDto {
            output_path: output_path.into(),
            options,
        };
        let op_id = document_io_commands::export_epub(&inner.ctx, &dto)?;
        inner.own_operations.insert(op_id.clone());
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

    /// Export the entire document as ODT (OpenDocument Text) to a file path.
    ///
    /// This is a **long operation**. Returns a typed [`Operation`] handle.
    pub fn to_odt(&self, output_path: &str) -> Result<Operation<OdtExportResult>> {
        self.to_odt_with_options(output_path, crate::OdtExportOptions::default())
    }

    /// As [`to_odt`](Self::to_odt), but with page geometry + base typography overrides — the ODT
    /// analog of [`to_docx_with_options`](Self::to_docx_with_options), same units and same
    /// per-block-RTL-is-automatic behaviour (see [`OdtExportOptions`](crate::OdtExportOptions)'s
    /// own doc comment).
    pub fn to_odt_with_options(
        &self,
        output_path: &str,
        options: crate::OdtExportOptions,
    ) -> Result<Operation<OdtExportResult>> {
        let mut inner = self.inner.lock();
        let dto = frontend::document_io::ExportOdtDto {
            output_path: output_path.into(),
            options,
        };
        let op_id = document_io_commands::export_odt(&inner.ctx, &dto)?;
        inner.own_operations.insert(op_id.clone());
        Ok(Operation::new(
            op_id,
            &inner.ctx,
            Box::new(|ctx, id| {
                document_io_commands::get_export_odt_result(ctx, id)
                    .ok()
                    .flatten()
                    .map(|r| {
                        Ok(OdtExportResult {
                            file_path: r.file_path,
                            paragraph_count: to_usize(r.paragraph_count),
                        })
                    })
            }),
        ))
    }

    /// Export the entire document as a PDF file, using the given options (page geometry,
    /// typography, embedded font bytes, base language/direction).
    ///
    /// This is a **long operation**. Returns a typed [`Operation`] handle.
    ///
    /// Requires the `pdf` cargo feature on `text-document` (which forwards to `frontend`'s and
    /// `document_io`'s own `pdf` features). If it was not enabled at compile time, this returns
    /// `Err(DocumentError::Unsupported(..))` immediately rather than attempting the export — no
    /// `#[cfg]` is needed at the call site either way.
    pub fn to_pdf(
        &self,
        output_path: &str,
        options: crate::PdfExportOptions,
    ) -> Result<Operation<PdfExportResult>> {
        self.to_pdf_with_options(output_path, options)
    }

    /// As [`to_pdf`](Self::to_pdf) — the two are identical; `to_pdf` is the plain entry point,
    /// `to_pdf_with_options` exists (like [`to_docx_with_options`](Self::to_docx_with_options)
    /// and [`to_epub_with_options`](Self::to_epub_with_options)) so the naming stays consistent
    /// across the three file-based exporters, all of which take a mandatory options struct.
    #[cfg(feature = "pdf")]
    pub fn to_pdf_with_options(
        &self,
        output_path: &str,
        options: crate::PdfExportOptions,
    ) -> Result<Operation<PdfExportResult>> {
        let inner = self.inner.lock();
        let dto = frontend::document_io::ExportPdfDto {
            output_path: output_path.into(),
            options,
        };
        let op_id = document_io_commands::export_pdf(&inner.ctx, &dto)?;
        inner.own_operations.insert(op_id.clone());
        Ok(Operation::new(
            op_id,
            &inner.ctx,
            Box::new(|ctx, id| {
                document_io_commands::get_export_pdf_result(ctx, id)
                    .ok()
                    .flatten()
                    .map(|r| {
                        Ok(PdfExportResult {
                            file_path: r.file_path,
                            page_count: to_usize(r.page_count),
                        })
                    })
            }),
        ))
    }

    /// As [`to_pdf`](Self::to_pdf), when the `pdf` cargo feature was not enabled at compile
    /// time — returns [`DocumentError::Unsupported`] immediately, without starting an operation
    /// or touching the backend at all.
    #[cfg(not(feature = "pdf"))]
    pub fn to_pdf_with_options(
        &self,
        _output_path: &str,
        _options: crate::PdfExportOptions,
    ) -> Result<Operation<PdfExportResult>> {
        Err(DocumentError::Unsupported(
            "PDF export requires the `pdf` cargo feature on the `text-document` crate".into(),
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

    /// Tell the document what each footnote label should print.
    ///
    /// Presentation only: never stored, never exported, never part of the text. A
    /// reference occupies one character whatever its marker says.
    ///
    /// Set it when the numbers are a fact about something larger than this
    /// document — a host compiling one chapter of a book knows the chapter's notes
    /// continue a sequence this document cannot see. Leave it unset and the
    /// document numbers its own references in reading order, which is right when
    /// the document *is* the whole text.
    /// Storing the map is only half of it: a marker is **shaped text**, so a
    /// document already laid out keeps drawing the old one until something tells
    /// it to reshape. Nothing else will — the map is presentation state and
    /// changing it edits no block, so it emits no edit event of its own. Without
    /// the notification below, a host that numbers a note the instant it is
    /// created watches the raw label sit in the writer's prose until an unrelated
    /// keystroke happens to force a relayout.
    ///
    /// `FormatChanged` over the whole document rather than a paint-only event:
    /// the marker's width changes with its text (`9` and `10` are not the same
    /// size), so the line has to be reshaped, not recoloured. Guarded on the map
    /// actually differing, because a host pushes this on every refresh and a
    /// full relayout per keystroke is not a thing to do by accident.
    pub fn set_footnote_markers(&self, markers: std::collections::HashMap<String, String>) {
        let queued = {
            let mut inner = self.inner.lock();
            {
                let store = inner.ctx.db_context.get_store();
                let mut current = store.footnote_markers.write();
                if *current == markers {
                    return;
                }
                *current = markers;
            }
            inner.queue_event(DocumentEvent::FormatChanged {
                position: 0,
                length: 0,
                kind: crate::flow::FormatChangeKind::Character,
            });
            inner.take_queued_events()
        };
        crate::inner::dispatch_queued_events(queued);
    }

    /// Every footnote reference in the document, as `(position, label)`, in
    /// reading order.
    ///
    /// The seam a host uses to tie its own note storage to the prose: Skribisto
    /// keeps note bodies in its store, so what it needs from the document is
    /// only *where* the references are and *which* note each names.
    ///
    /// Positions are document-absolute character offsets — the same space a
    /// cursor and a search hit use — so a caller can go straight from a caret to
    /// the note under it without a second lookup.
    pub fn footnote_references(&self) -> Vec<(usize, String)> {
        let inner = self.inner.lock();
        let store = inner.ctx.db_context.get_store();

        let refs = store.block_footnote_refs.read();
        if refs.is_empty() {
            return Vec::new();
        }

        // Block order, then byte order within a block — the order they are read.
        let mut blocks: Vec<(i64, u64)> = store
            .blocks
            .read()
            .values()
            .map(|b| (b.document_position, b.id))
            .collect();
        blocks.sort_unstable();

        let mut out = Vec::new();
        for (position, block_id) in blocks {
            let Some(anchors) = refs.get(&block_id) else {
                continue;
            };
            let Some(block) = store.blocks.read().get(&block_id).cloned() else {
                continue;
            };
            let text =
                frontend::common::database::rope_helpers::block_content_via_store(&block, store);
            let mut ordered: Vec<_> = anchors.iter().collect();
            ordered.sort_by_key(|a| a.byte_offset);
            for anchor in ordered {
                // Byte offset within the block → character offset within the
                // document. The two differ the moment the block holds anything
                // outside ASCII, which for prose is immediately.
                let chars_before = text
                    .get(..anchor.byte_offset as usize)
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
                out.push((position as usize + chars_before, anchor.label.clone()));
            }
        }
        out
    }

    /// The label of the footnote reference at `position`, if one sits there.
    ///
    /// What "the caret is on a footnote" means, for a host wiring a two-way
    /// selection between its notes list and the prose.
    pub fn footnote_reference_at(&self, position: usize) -> Option<String> {
        self.footnote_references()
            .into_iter()
            .find(|(at, _)| *at == position)
            .map(|(_, label)| label)
    }

    /// Whether this document was built inside a [`DocumentBackend`], rather than
    /// standing alone with an event hub and a drain thread of its own.
    ///
    /// The question a host asks of its own wiring. Documents it means to keep for
    /// the life of a project — a comment body, a footnote body, one per row of a
    /// stream — belong in a shared backend, and one built with
    /// [`TextDocument::new`] instead is indistinguishable in use while costing an
    /// OS thread that will never have anything to deliver. Nothing else reports
    /// that, so nothing else can test for it.
    ///
    /// [`DocumentBackend`]: crate::DocumentBackend
    pub fn shares_a_backend(&self) -> bool {
        self.inner.lock().backend.is_some()
    }

    /// Get the total character count. One entity read, and no document walk.
    ///
    /// It reads the count the `Document` entity carries, through
    /// [`crate::inner::document_counts`]. It used to go through
    /// `get_document_stats`, which returns the same number and then walks every
    /// block materialising its text for the word count in the same DTO — so this
    /// call, which a host may make once per widget per layout pass, cost a
    /// complete scan of the document. `stats()` still pays that walk, and should:
    /// it is the caller that asked for the word count.
    pub fn character_count(&self) -> usize {
        let inner = self.inner.lock();
        crate::inner::document_counts(&inner).map_or(0, |(chars, _)| chars)
    }

    /// Get the number of blocks (paragraphs). One entity read, and no document
    /// walk. See [`character_count`](Self::character_count) for what that
    /// replaced.
    pub fn block_count(&self) -> usize {
        let inner = self.inner.lock();
        crate::inner::document_counts(&inner).map_or(0, |(_, blocks)| blocks)
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
        // Caret semantics, per the boundary convention documented just above: with the
        // character-index `block_at`, a position at the end of a paragraph resolved to the
        // *next* block and the `checked_sub` below then failed, so the last element of every
        // paragraph was unreachable.
        let block_info = self.block_at_caret(position).ok()?;
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
                // Both objects occupy exactly one position and answer for it
                // whole — there is no offset *inside* either to report.
                crate::flow::FragmentContent::Image {
                    offset, element_id, ..
                }
                | crate::flow::FragmentContent::FootnoteReference {
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
    ///
    /// `position` is read as a **character index**, so the inter-block separator belongs to
    /// the block that *follows* it: in `"abc\ndef"`, position 3 is the newline and reports the
    /// second block. For a **caret** offset — where 3 means "after the c", the last place the
    /// caret can sit in the first paragraph — use [`block_at_caret`](Self::block_at_caret).
    pub fn block_at(&self, position: usize) -> Result<BlockInfo> {
        let inner = self.inner.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(position),
        };
        let result = document_inspection_commands::get_block_at_position(&inner.ctx, &dto)?;
        Ok(BlockInfo::from(&result))
    }

    /// The block a **caret** at `position` sits in. O(log n).
    ///
    /// Differs from [`block_at`](Self::block_at) at exactly one place: the end of a paragraph.
    /// A character index and a caret offset disagree there — the character at that index is the
    /// separator, which belongs to the next block, but a caret there is still in the paragraph
    /// it just finished typing. `block_at` answers the first question (and callers that walk
    /// text depend on it — moving the caret across a separator, reading the character under an
    /// offset); this answers the second.
    ///
    /// Ask this one whenever the position came from a cursor. Asking `block_at` instead is why
    /// the caret-band highlight lit the *next* paragraph the moment the caret reached the end of
    /// one.
    pub fn block_at_caret(&self, position: usize) -> Result<BlockInfo> {
        let inner = self.inner.lock();
        let info = crate::inner::block_at_caret_dto(&inner.ctx, position)?;
        Ok(BlockInfo::from(&info))
    }

    /// The sentence containing `position`, as absolute char offsets `(start, end)` — the
    /// granularity between [`word`](TextCursor::select) and [`block_at`](Self::block_at).
    ///
    /// `content_locale` is a BCP-47-ish tag (`"en"`, `"en-US"`, `"pt_BR"`) naming the language
    /// the text is written in. It selects the sentence tailoring for that language —
    /// abbreviations that do not end a sentence, French spaced guillemets, the Greek question
    /// mark. Pass it **fresh on every call**, like [`FindOptions::language`](crate::FindOptions):
    /// only the caller knows what language the text is in, and it is not document state.
    /// `None`, or a language with no tailoring, falls back to plain UAX #29.
    ///
    /// A sentence never crosses a block: a paragraph break always ends one. Returns `None` when
    /// the block holds no sentence to point at (empty, or whitespace only). Trailing whitespace
    /// is trimmed off the end, so the range covers the sentence and not the gap after it.
    ///
    /// The trailing edge is inclusive of the caret: a `position` at the very end of the block
    /// resolves to the last sentence of *that* block rather than to the first sentence of the
    /// next one. `position` is a caret offset, so the block is resolved with
    /// [`block_at_caret`](Self::block_at_caret) and not with the character-index
    /// [`block_at`](Self::block_at).
    pub fn sentence_at(
        &self,
        position: usize,
        content_locale: Option<&str>,
    ) -> Option<(usize, usize)> {
        // Resolved before the lock: `block_at_caret` takes it itself.
        let block = self.block_at_caret(position).ok()?;
        let inner = self.inner.lock();
        let block_start = block.start;
        let block_length = block.length;
        if block_length == 0 {
            return None;
        }
        let text_dto = frontend::document_inspection::GetTextAtPositionDto {
            position: to_i64(block_start),
            length: to_i64(block_length),
        };
        let text = document_inspection_commands::get_text_at_position(&inner.ctx, &text_dto)
            .ok()?
            .text;
        drop(inner);

        let offset = position.saturating_sub(block_start);
        let (start, end) =
            frontend::common::parser_tools::sentence_bounds(&text, offset, content_locale)?;
        Some((block_start + start, block_start + end))
    }

    /// Get the block format at a position.
    ///
    /// `position` is read with **caret** semantics ([`block_at_caret`](Self::block_at_caret)):
    /// at the end of a paragraph this reports that paragraph's format, not the next one's.
    /// Formatting queries are asked about a cursor, never about a character index.
    pub fn block_format_at(&self, position: usize) -> Result<BlockFormat> {
        let inner = self.inner.lock();
        let block_info = crate::inner::block_at_caret_dto(&inner.ctx, position)?;
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
            suppress_paint: false,
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
    /// `HighlighterKind` is resolved **once here**, at the snapshot root,
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
            suppress_paint: false,
        };
        let elements = crate::text_frame::build_flow_snapshot(&inner, main_frame_id, hl);
        crate::flow::FlowSnapshot { elements }
    }

    /// Snapshot the main flow like [`snapshot_flow_masked`](Self::snapshot_flow_masked),
    /// but **without computing the paint-only overlay** (`paint_highlights` is
    /// empty on every block). Fragments are identical — metric sessions still
    /// split them — so a consumer that reads only the fragments and their
    /// geometry gets the exact same tree, minus the `extract_paint_spans` work.
    ///
    /// This is the accessibility path's snapshot: the AT tree reads fragments,
    /// never the paint overlay, so paying to compute a per-block paint span for
    /// each of a spell-checker's tens of thousands of ranges is pure waste (it
    /// dominated the a11y rebuild on a large mis-dictionaried document). Render
    /// and layout keep using [`snapshot_flow_masked`](Self::snapshot_flow_masked),
    /// which they must — they draw the overlay.
    pub fn snapshot_flow_masked_no_paint(
        &self,
        mask: &crate::highlight::HighlightMask,
    ) -> crate::flow::FlowSnapshot {
        let inner = self.inner.lock();
        let main_frame_id = get_main_frame_id(&inner);
        let hl = crate::highlight::SnapshotHighlights {
            kind: inner.highlights.effective_kind(mask),
            mask,
            suppress_paint: true,
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
    /// [`crate::ReplaceOptions::format_policy`] — what the replacement wears where it
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
            let stepped = undo_redo_commands::can_undo(&inner.ctx, Some(inner.stack_id));
            let result = undo_redo_commands::undo(&inner.ctx, Some(inner.stack_id));
            inner.invalidate_text_cache();
            // An undo that actually popped a command changed the buffer, so the
            // document is dirty again. Nothing else sets `modified` here: every
            // other setter is an *edit* in `cursor.rs`/`streaming.rs`. Without
            // this, an embedder that gates its write-back on `is_modified()`
            // silently discards the undo — the text reverts on screen while the
            // persisted copy keeps the pre-undo version, and the next reload
            // brings the stale text back. (Skribisto's `ProseField::flush` did
            // exactly that.)
            //
            // `can_undo` is sampled *before* the call because `undo()` on an
            // empty stack is a successful no-op, and marking a clean document
            // dirty for a keystroke that did nothing would be its own bug.
            //
            // Set *before* `result?`, and deliberately. A composite entry undoes
            // its parts in reverse and gives up on the first failure, so a
            // failed undo can still have reverted some of them — the buffer has
            // moved, which is why `invalidate_text_cache` above is also
            // unconditional. Marking a document dirty that turns out not to need
            // saving costs one redundant write; the other way round loses the
            // writer's text.
            if stepped {
                inner.modified = true;
            }
            result?;
            inner.rehighlight_all();
            emit_content_change_events(&mut inner, &before);
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
            let stepped = undo_redo_commands::can_redo(&inner.ctx, Some(inner.stack_id));
            let result = undo_redo_commands::redo(&inner.ctx, Some(inner.stack_id));
            inner.invalidate_text_cache();
            // A redo re-applies an edit the writer took back, which is a change
            // to the buffer like any other: same reasoning as `undo` above, and
            // the same placement before `result?` for the same reason. Here the
            // predicate is `can_redo`, sampled before the call because `redo()`
            // on an empty redo branch is a successful no-op.
            if stepped {
                inner.modified = true;
            }
            result?;
            inner.rehighlight_all();
            emit_content_change_events(&mut inner, &before);
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

    /// Close the current undo entry, so the next edit starts a new one.
    ///
    /// Typing is coalesced — contiguous inserts within a couple of seconds
    /// become one undo step, which is what makes Ctrl+Z take back a word rather
    /// than a letter. The rule looks only at the *shape* of two edits and cannot
    /// see that something happened between them: an embedder whose user typed,
    /// renamed a chapter somewhere else, then typed again gets one entry
    /// spanning both bursts, and undoing it takes back text entered before an
    /// event the user remembers as a dividing line.
    ///
    /// The embedder is the only one who knows such a line was crossed. This is
    /// how it says so. Idempotent, and harmless on an empty history.
    pub fn break_undo_merge(&self) {
        let inner = self.inner.lock();
        undo_redo_commands::seal_head(&inner.ctx, Some(inner.stack_id));
    }

    /// Bound how many undo entries this document keeps, dropping the oldest
    /// past the limit. `None` — the default — keeps everything.
    ///
    /// Typing history is unbounded by construction: every keystroke that does
    /// not coalesce into the entry below it is another entry, and each holds a
    /// snapshot of what it changed. Over a day-long drafting session on one
    /// document that is a ceiling nobody set. An embedder that cares about the
    /// ceiling needs a way to say so, and this is it — the far end of a long
    /// history is the part nobody reaches for.
    ///
    /// The limit belongs to the document, not to one edit: lowering it trims
    /// on the next push rather than immediately, so an entry the writer can
    /// still see in a menu does not vanish under them.
    pub fn set_undo_limit(&self, limit: Option<usize>) {
        let inner = self.inner.lock();
        undo_redo_commands::set_undo_limit(&inner.ctx, limit);
    }

    /// The current entry limit, if one is set.
    pub fn undo_limit(&self) -> Option<usize> {
        let inner = self.inner.lock();
        undo_redo_commands::undo_limit(&inner.ctx)
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

    /// A monotonic counter, bumped once per [`DocumentEvent::ContentsChanged`]
    /// queued so far. Starts at `0`.
    ///
    /// Lets a caller answer "was this notification caused by exactly the
    /// most recent edit, with nothing else having happened since" precisely
    /// — snapshot the value when acting on a notification, and compare it
    /// against the current value later. This is deliberately *not* the same
    /// as [`is_modified`](Self::is_modified) (a flag, not a count) or the
    /// undo stack's depth (which does not grow when consecutive compatible
    /// edits merge into one entry — e.g. fast consecutive typing).
    pub fn content_revision(&self) -> u64 {
        self.inner.lock().content_revision
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
    /// callback with its own per-block state cascade — and return its [`crate::SessionId`].
    ///
    /// Unlike [`set_syntax_highlighter`](Self::set_syntax_highlighter), this **adds** rather
    /// than replaces: a document can carry a syntax highlighter and a spell-checker at once,
    /// each a session, merged in `(priority, registration)` order (a later session's field
    /// wins). Sessions remain visible only in views whose
    /// [`HighlightMask`](crate::highlight::HighlightMask) admits them.
    pub fn add_syntax_session(
        &self,
        highlighter: Arc<dyn crate::SyntaxHighlighter>,
    ) -> crate::highlight::SessionId {
        self.add_syntax_session_with_priority(highlighter, 0)
    }

    /// [`add_syntax_session`](Self::add_syntax_session) at an explicit merge priority — see
    /// [`add_range_session_with_priority`](Self::add_range_session_with_priority).
    pub fn add_syntax_session_with_priority(
        &self,
        highlighter: Arc<dyn crate::SyntaxHighlighter>,
        priority: i32,
    ) -> crate::highlight::SessionId {
        let (id, queued) = {
            let mut inner = self.inner.lock();
            let prev_kind = inner.highlight_kind;
            let id = inner.highlights.add_syntax(highlighter, priority);
            inner.rehighlight_all();
            Self::queue_highlight_changed(&mut inner, 0, 0, prev_kind);
            (id, inner.take_queued_events())
        };
        crate::inner::dispatch_queued_events(queued);
        id
    }

    /// Register an empty **range session** — absolute-offset ranges set with
    /// [`set_session_ranges`](Self::set_session_ranges), the shape used for search and (later)
    /// an externally-driven spell-checker. Returns its [`crate::SessionId`].
    ///
    /// A view's own find session is a range session it alone admits; that is how two panes
    /// over one document highlight different queries.
    ///
    /// **Shared**: every view renders it unless its mask says otherwise. For a layer that
    /// belongs to one view rather than to the text, see
    /// [`add_opt_in_range_session`](Self::add_opt_in_range_session).
    pub fn add_range_session(&self) -> crate::highlight::SessionId {
        self.add_range_session_with_priority(0)
    }

    /// Register an empty range session that **no view renders until it asks for it** by name
    /// (`HighlightMask::all().with(id)`).
    ///
    /// The session lives on the document like any other (a range session has nowhere else to
    /// live), but it is a fact about one *view*, not about the text, so a second view of the
    /// same document must be left alone. Reach for this whenever the answer to "should the
    /// pane next door draw this too?" is no: a reading that marks every mention of one
    /// character marks it in the reading, not in every editor that happens to hold the same
    /// scene.
    ///
    /// The alternative, [`HighlightMask::only`](crate::highlight::HighlightMask::only) on
    /// every *other* view, cannot be written: a view would have to name every session it
    /// does want, including ones it holds no handle on, and would silently drop the next
    /// layer anyone adds.
    pub fn add_opt_in_range_session(&self) -> crate::highlight::SessionId {
        self.add_opt_in_range_session_with_priority(0)
    }

    /// Every [`OptIn`](crate::highlight::SessionVisibility::OptIn) session on this document,
    /// in merge order.
    ///
    /// Sessions can be added and retired but there was no way to ask what a document carries,
    /// which is the one question worth asking about a private layer: it is invisible to the
    /// plain snapshot by design, so "is it there, and is it marking the right characters" has
    /// no other answer. Shared sessions are deliberately not listed, since every view already
    /// draws those, so enumerating them answers nothing.
    ///
    /// Not a route to a view's mask: a view names the session it *owns*, and one built from
    /// this list would draw whatever the pane next door happens to have registered, which is
    /// exactly what [`add_opt_in_range_session`](Self::add_opt_in_range_session) exists to
    /// prevent.
    pub fn opt_in_session_ids(&self) -> Vec<crate::highlight::SessionId> {
        let inner = self.inner.lock();
        inner
            .highlights
            .sessions
            .iter()
            .filter(|s| s.visibility == crate::highlight::SessionVisibility::OptIn)
            .map(|s| s.id)
            .collect()
    }

    /// [`add_opt_in_range_session`](Self::add_opt_in_range_session) at an explicit **merge
    /// priority**. See
    /// [`add_range_session_with_priority`](Self::add_range_session_with_priority).
    pub fn add_opt_in_range_session_with_priority(
        &self,
        priority: i32,
    ) -> crate::highlight::SessionId {
        let mut inner = self.inner.lock();
        inner
            .highlights
            .add_range(priority, crate::highlight::SessionVisibility::OptIn)
        // No repaint: an empty range session shows nothing until its ranges are set.
    }

    /// [`add_range_session`](Self::add_range_session) at an explicit **merge priority**.
    ///
    /// Where two sessions format the same character, the higher priority wins field by field;
    /// equal priorities fall back to registration order, which is what every session gets by
    /// default (`0`).
    ///
    /// Reach for this when a layer must reliably lose — an ambient background band that every
    /// find match and spell squiggle should paint over. Registration order cannot express that:
    /// a per-view layer is registered when its view appears, so whether it lands before or
    /// after the find session depends on the order the user happened to open things in.
    pub fn add_range_session_with_priority(&self, priority: i32) -> crate::highlight::SessionId {
        let mut inner = self.inner.lock();
        inner
            .highlights
            .add_range(priority, crate::highlight::SessionVisibility::Shared)
        // No repaint: an empty range session shows nothing until its ranges are set.
    }

    /// Replace the ranges of a range session (absolute char offsets, the space
    /// [`FindMatch`] reports in). Returns `false` if `id` is not a range
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
            // The block layout the ranges are bucketed against — cheap (ids + positions, no
            // block text) and computed before the mutable borrow of `highlights`. This is what
            // lets `merged_spans_for_block` look up only a block's own ranges instead of
            // scanning the whole vector per block.
            let block_positions = crate::highlight::ordered_block_positions(&inner);
            let changed = inner.highlights.set_ranges(id, ranges, &block_positions);
            if let Some((position, length)) = changed {
                inner.recompute_highlight_kind();
                // The real extent, not `0, 0`: a view can then recolor just the block it covers
                // rather than re-snapshotting the whole document on every caret move.
                Self::queue_highlight_changed(&mut inner, position, length, prev_kind);
            }
            (changed.is_some(), inner.take_queued_events())
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
    /// `position` / `length` name the extent that changed, so a live view can
    /// recolor just the blocks it covers instead of re-deriving the whole
    /// snapshot. **A `length` of `0` means "unknown — assume the whole
    /// document"**, which is what the genuinely document-wide operations pass
    /// (installing or retiring a highlighter, a full rehighlight). Only
    /// [`set_session_ranges`](Self::set_session_ranges) reports a real extent,
    /// its before/after range sets giving an exact answer.
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

/// Lightweight block state for before/after comparison.
///
/// Named for undo/redo because that is where it started; it is now also how a
/// structural table edit works out what it did to the text. See
/// [`emit_content_change_events`].
pub(crate) struct UndoBlockState {
    id: u64,
    position: i64,
    text_length: i64,
    plain_text: String,
    format: BlockFormat,
}

/// Capture the state of all blocks, sorted by document_position.
///
/// Reads through the store rather than the plain-text cache, so a caller does
/// not have to have invalidated anything first.
pub(crate) fn capture_block_state(inner: &TextDocumentInner) -> Vec<UndoBlockState> {
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

/// Compare block state before and after an edit and emit
/// `ContentsChanged` / `FormatChanged` events for the affected regions.
///
/// ## Why anything else would be a guess
///
/// The delta this computes is a **real text diff**, and consumers rely on that
/// being true rather than approximate: a comment anchor shifts by
/// `(position, chars_removed, chars_added)`, so a figure that is merely
/// plausible moves an anchor to somewhere that was never right — which is
/// harder to notice than not moving it at all.
///
/// That is why the table primitives call this rather than describing their own
/// edit. A row insert knows how many rows it added; it does not know where in
/// the document's text that lands, and working it out by hand would be seven
/// separate opportunities to be subtly wrong.
///
/// Used by undo, redo, and every structural table edit.
pub(crate) fn emit_content_change_events(inner: &mut TextDocumentInner, before: &[UndoBlockState]) {
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
        // **`Replayed`, hard-coded, and it cannot double-count.** Undo and redo
        // never re-enter the insertion API — they snapshot and diff, which is
        // why this function exists — so text restored by them is reported once,
        // under an origin that says it came back rather than arrived.
        //
        // ⚠ Measured as the document's **total** gain, not from `chars_added`.
        // That figure is the size of the restored region, which is non-zero for
        // an undo that removes text: reporting it would put `Replayed` on a
        // count of characters nothing brought back. A test caught exactly that.
        let before_len: i64 = before.iter().map(|b| b.text_length.max(0)).sum();
        let after_len: i64 = after.iter().map(|a| a.text_length.max(0)).sum();
        if after_len > before_len {
            inner.queue_event(DocumentEvent::TextInserted {
                position,
                chars_inserted: (after_len - before_len) as usize,
                origin: crate::InsertionOrigin::Replayed,
            });
        }
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
