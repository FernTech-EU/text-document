// Custom implementation — mirrors `export_docx_uc.rs`'s `LongOperation` shape (frozen read
// transaction via the uow, progress/cancel, build-then-write), but assembles an EPUB 3 package
// via `epub_builder` instead of a `docx_rs::Docx`. Block/inline HTML rendering is NOT
// reimplemented here — it calls the same `crate::html_render` functions `export_html_uc` uses,
// per the "a use case may not call another use case" rule: the shared logic lives in a module
// both use cases call, neither use case calls the other.
use crate::ExportEpubDto;
use crate::ExportEpubResultDto;
use crate::html_render;
use anyhow::{Result, anyhow};
use common::database::QueryUnitOfWork;
use common::database::Store;
use common::entities::{Block, Document, Frame, List, Root, Table, TableCell};
use common::long_operation::{LongOperation, OperationProgress};
use common::parser_tools::EpubExportOptions;
use common::types::{EntityId, ROOT_ENTITY_ID};
use epub_builder::{
    EpubBuilder, EpubContent, EpubVersion, PageDirection, ReferenceType, ZipLibrary,
};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub trait ExportEpubUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn ExportEpubUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "GetRO", thread_safe = true)]
#[macros::uow_action(entity = "Root", action = "GetRelationshipRO", thread_safe = true)]
#[macros::uow_action(entity = "Document", action = "GetRO", thread_safe = true)]
#[macros::uow_action(entity = "Document", action = "GetRelationshipRO", thread_safe = true)]
#[macros::uow_action(entity = "Frame", action = "GetRO", thread_safe = true)]
#[macros::uow_action(entity = "Frame", action = "GetRelationshipRO", thread_safe = true)]
#[macros::uow_action(entity = "Block", action = "GetRO", thread_safe = true)]
#[macros::uow_action(entity = "Block", action = "GetMultiRO", thread_safe = true)]
#[macros::uow_action(entity = "Block", action = "GetRelationshipRO", thread_safe = true)]
#[macros::uow_action(entity = "List", action = "GetRO", thread_safe = true)]
#[macros::uow_action(entity = "Table", action = "GetRO", thread_safe = true)]
#[macros::uow_action(entity = "Table", action = "GetRelationshipRO", thread_safe = true)]
#[macros::uow_action(entity = "TableCell", action = "GetMultiRO", thread_safe = true)]
pub trait ExportEpubUnitOfWorkTrait: QueryUnitOfWork + Send + Sync {}

pub struct ExportEpubUseCase {
    uow_factory: Box<dyn ExportEpubUnitOfWorkFactoryTrait>,
    dto: ExportEpubDto,
}

impl ExportEpubUseCase {
    pub fn new(
        uow_factory: Box<dyn ExportEpubUnitOfWorkFactoryTrait>,
        dto: &ExportEpubDto,
    ) -> Self {
        ExportEpubUseCase {
            uow_factory,
            dto: dto.clone(),
        }
    }
}

impl LongOperation for ExportEpubUseCase {
    type Output = ExportEpubResultDto;

    fn execute(
        &self,
        progress_callback: Box<dyn Fn(OperationProgress) + Send>,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<Self::Output> {
        // Validate output path
        let output_path = std::path::Path::new(&self.dto.output_path);
        if let Some(parent) = output_path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            return Err(anyhow!(
                "Output directory does not exist: '{}'",
                parent.display()
            ));
        }

        progress_callback(OperationProgress::new(
            0.0,
            Some("Starting EPUB export...".to_string()),
        ));

        let uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let build_result = self.build_chapters(
            &*uow,
            progress_callback.as_ref(),
            Some(cancel_flag.as_ref()),
        );

        uow.end_transaction()?;

        let chapters = build_result?;
        let chapter_count = chapters.len() as i64;

        progress_callback(OperationProgress::new(
            85.0,
            Some("Packaging EPUB...".to_string()),
        ));

        let epub_bytes = package_epub(&self.dto.options, &chapters)?;

        progress_callback(OperationProgress::new(
            90.0,
            Some("Writing EPUB file...".to_string()),
        ));

        std::fs::write(&self.dto.output_path, &epub_bytes).map_err(|e| {
            anyhow!(
                "Failed to write output file '{}': {}",
                self.dto.output_path,
                e
            )
        })?;

        progress_callback(OperationProgress::new(100.0, Some("completed".to_string())));

        Ok(ExportEpubResultDto {
            file_path: self.dto.output_path.clone(),
            chapter_count,
        })
    }
}

impl ExportEpubUseCase {
    /// Build the in-memory EPUB bytes without any file I/O, using a no-op progress callback and
    /// no cancellation, together with the chapter count. Intended for callers (notably tests)
    /// that want to inspect the packaged EPUB (a zip archive) directly.
    ///
    /// `execute` uses [`Self::build_chapters`] + [`package_epub`] the same way, then writes the
    /// bytes to disk; the controller exposes this file-less variant as
    /// [`crate::document_io_controller::build_epub_document`].
    pub(crate) fn build_document(&self) -> Result<(Vec<u8>, i64)> {
        let uow = self.uow_factory.create();
        uow.begin_transaction()?;
        let result = self.build_chapters(&*uow, &|_progress| {}, None);
        uow.end_transaction()?;
        let chapters = result?;
        let chapter_count = chapters.len() as i64;
        let epub_bytes = package_epub(&self.dto.options, &chapters)?;
        Ok((epub_bytes, chapter_count))
    }

    /// Walk Root→Document→Frame→Block exactly like `export_html_uc`'s traversal (same top-level
    /// frame loop, `child_order` interleaving, cell-frame skip), but instead of joining
    /// everything into one HTML string, collect an ordered stream of [`RenderUnit`]s and then
    /// split that stream into chapters (see [`split_into_chapters`]).
    pub(crate) fn build_chapters(
        &self,
        uow: &dyn ExportEpubUnitOfWorkTrait,
        progress_callback: &dyn Fn(OperationProgress),
        cancel_flag: Option<&AtomicBool>,
    ) -> Result<Vec<Chapter>> {
        // Step 1: Get Root and Document
        let root = uow
            .get_root(&ROOT_ENTITY_ID)?
            .ok_or_else(|| anyhow!("Root entity not found"))?;

        let doc_ids = uow.get_root_relationship(
            &root.id,
            &common::direct_access::root::RootRelationshipField::Document,
        )?;
        let doc_id = *doc_ids
            .first()
            .ok_or_else(|| anyhow!("Root has no associated Document"))?;

        let frame_ids = uow.get_document_relationship(
            &doc_id,
            &common::direct_access::document::DocumentRelationshipField::Frames,
        )?;

        // Collect all cell frame IDs so we can skip them in the main walk; they are rendered as
        // part of their owning table.
        let table_ids = uow.get_document_relationship(
            &doc_id,
            &common::direct_access::document::DocumentRelationshipField::Tables,
        )?;
        let mut cell_frame_ids: HashSet<EntityId> = HashSet::new();
        for tid in &table_ids {
            let cell_ids = uow.get_table_relationship(
                tid,
                &common::direct_access::table::TableRelationshipField::Cells,
            )?;
            let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
            for cell in cells_opt.into_iter().flatten() {
                if let Some(cf_id) = cell.cell_frame {
                    cell_frame_ids.insert(cf_id);
                }
            }
        }

        progress_callback(OperationProgress::new(
            10.0,
            Some("Walking document tree...".to_string()),
        ));

        let mut units: Vec<RenderUnit> = Vec::new();

        let total_frames = frame_ids.len().max(1);
        for (frame_idx, frame_id) in frame_ids.iter().enumerate() {
            check_cancelled(cancel_flag)?;

            // Skip cell frames — they're rendered as part of their table.
            if cell_frame_ids.contains(frame_id) {
                continue;
            }
            // Skip sub-frames (parent_frame != None) — recursively rendered by their parent's
            // walk; rendering them again at the top level would duplicate their content.
            if let Some(f) = uow.get_frame(frame_id)?
                && f.parent_frame.is_some()
            {
                continue;
            }

            self.render_frame_units(uow, frame_id, &cell_frame_ids, &mut units)?;

            let pct = 10.0 + (frame_idx as f32 / total_frames as f32) * 60.0;
            progress_callback(OperationProgress::new(
                pct,
                Some(format!(
                    "Processing frame {}/{}",
                    frame_idx + 1,
                    total_frames
                )),
            ));
        }

        progress_callback(OperationProgress::new(
            75.0,
            Some("Splitting into chapters...".to_string()),
        ));

        Ok(split_into_chapters(units, &self.dto.options))
    }

    /// Render a frame's content into [`RenderUnit`]s, walking its `child_order` to interleave
    /// blocks and sub-frames (blockquotes/tables). Falls back to sorted blocks when
    /// `child_order` is empty. Mirrors `export_html_uc::render_frame_html`.
    fn render_frame_units(
        &self,
        uow: &dyn ExportEpubUnitOfWorkTrait,
        frame_id: &EntityId,
        cell_frame_ids: &HashSet<EntityId>,
        out: &mut Vec<RenderUnit>,
    ) -> Result<()> {
        let frame = uow
            .get_frame(frame_id)?
            .ok_or_else(|| anyhow!("Frame not found"))?;

        // Table anchor frame — render the table instead of blocks. A table is always one
        // opaque unit: it never opens a new chapter.
        if let Some(table_id) = frame.table {
            let html = html_render::render_table_html(&uow.store(), table_id)?;
            if !html.is_empty() {
                out.push(RenderUnit::content(html));
            }
            return Ok(());
        }

        // If child_order is populated, use it to interleave blocks and sub-frames
        if !frame.child_order.is_empty() {
            return self.render_frame_units_by_child_order(uow, &frame, cell_frame_ids, out);
        }

        // Fallback: render all blocks in document_position order (original behaviour)
        let block_ids = uow.get_frame_relationship(
            frame_id,
            &common::direct_access::frame::FrameRelationshipField::Blocks,
        )?;

        if block_ids.is_empty() {
            return Ok(());
        }

        let blocks_opt = uow.get_block_multi(&block_ids)?;
        let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
        blocks.sort_by_key(|b| b.document_position);

        push_block_run_units(&uow.store(), &blocks, out);
        Ok(())
    }

    /// Walk `child_order` entries: positive values are block IDs, negative values are negated
    /// sub-frame IDs. Mirrors `export_html_uc::render_frame_by_child_order`.
    fn render_frame_units_by_child_order(
        &self,
        uow: &dyn ExportEpubUnitOfWorkTrait,
        frame: &Frame,
        cell_frame_ids: &HashSet<EntityId>,
        out: &mut Vec<RenderUnit>,
    ) -> Result<()> {
        // Accumulate consecutive blocks so we can group list items (and split at headings).
        let mut pending_blocks: Vec<Block> = Vec::new();

        for &entry in &frame.child_order {
            if entry > 0 {
                // Positive: block ID
                let block_id = entry as u64;
                if let Some(block) = uow.get_block(&block_id)? {
                    pending_blocks.push(block);
                }
            } else {
                // Negative: negated sub-frame ID
                // First, flush any accumulated blocks
                if !pending_blocks.is_empty() {
                    push_block_run_units(&uow.store(), &pending_blocks, out);
                    pending_blocks.clear();
                }

                let sub_frame_id = (-entry) as u64;

                // Skip cell frames
                if cell_frame_ids.contains(&sub_frame_id) {
                    continue;
                }

                let sub_frame = uow.get_frame(&sub_frame_id)?;
                if let Some(ref sf) = sub_frame {
                    if sf.fmt_is_blockquote == Some(true) {
                        // A blockquote is rendered as one opaque unit — a heading quoted inside
                        // it isn't a real chapter boundary, so it doesn't participate in
                        // chapter splitting (unlike a plain non-blockquote sub-frame, below).
                        let mut inner: Vec<RenderUnit> = Vec::new();
                        self.render_frame_units(uow, &sub_frame_id, cell_frame_ids, &mut inner)?;
                        let inner_html: String = inner.into_iter().map(|u| u.html).collect();
                        if !inner_html.is_empty() {
                            out.push(RenderUnit::content(format!(
                                "<blockquote>{}</blockquote>",
                                inner_html
                            )));
                        }
                    } else {
                        // Non-blockquote sub-frame: render normally, into the same stream —
                        // its headings (if any) still participate in chapter splitting.
                        self.render_frame_units(uow, &sub_frame_id, cell_frame_ids, out)?;
                    }
                }
            }
        }

        // Flush remaining blocks
        if !pending_blocks.is_empty() {
            push_block_run_units(&uow.store(), &pending_blocks, out);
        }

        Ok(())
    }
}

/// One EPUB chapter: a title (used for the chapter's own `<title>` and its table-of-contents
/// entry) and its content as an HTML fragment — not yet wrapped in a complete XHTML document;
/// [`wrap_xhtml`] does that once per chapter at packaging time.
pub(crate) struct Chapter {
    title: String,
    body_html: String,
}

/// One renderable, chapter-splittable unit of document content in flow order.
///
/// A `Some` `heading_level` marks a unit that is exactly one heading block, rendered on its own
/// (never batched with neighbours) precisely so [`split_into_chapters`] can find the boundary
/// between it and whatever came before — `heading_text` is that heading's plain visible text,
/// used as the chapter's title. Everything else (a run of body/list blocks, a rendered
/// blockquote, a table) is `None` and just contributes HTML to whichever chapter it falls into.
struct RenderUnit {
    heading_level: Option<i64>,
    heading_text: Option<String>,
    html: String,
}

impl RenderUnit {
    fn content(html: String) -> Self {
        RenderUnit {
            heading_level: None,
            heading_text: None,
            html,
        }
    }
}

/// A block counts as a chapter-splittable heading only when it actually renders as `<hN>` —
/// i.e. it is not a code block and not part of a list. List membership takes priority in
/// [`html_render::render_blocks_html`]'s own dispatch (a heading-tagged list item still renders
/// as `<li>`, never `<hN>`), so it must take the same priority here, or a chapter split could
/// land on a block that never actually produced a heading.
fn heading_level_for_split(store: &Store, block: &Block) -> Option<i64> {
    if block.fmt_is_code_block == Some(true) {
        return None;
    }
    let is_listed = block
        .list
        .is_some_and(|list_id| store.lists.read().contains_key(&list_id));
    if is_listed {
        return None;
    }
    block.fmt_heading_level
}

/// Split `blocks` into [`RenderUnit`]s: each heading block (per [`heading_level_for_split`])
/// becomes its own unit (rendered alone, so its HTML is exactly its `<hN>...</hN>`); runs of
/// non-heading blocks in between — which may themselves group into one `<ul>`/`<ol>`, a code
/// block, or plain paragraphs — are rendered together via [`html_render::render_blocks_html`]
/// as one unit.
fn push_block_run_units(store: &Store, blocks: &[Block], out: &mut Vec<RenderUnit>) {
    let mut i = 0;
    while i < blocks.len() {
        if let Some(level) = heading_level_for_split(store, &blocks[i]) {
            let html = html_render::render_blocks_html(store, std::slice::from_ref(&blocks[i]));
            let text = html_render::block_plain_text(store, &blocks[i]);
            out.push(RenderUnit {
                heading_level: Some(level),
                heading_text: Some(text),
                html,
            });
            i += 1;
            continue;
        }

        let start = i;
        while i < blocks.len() && heading_level_for_split(store, &blocks[i]).is_none() {
            i += 1;
        }
        let html = html_render::render_blocks_html(store, &blocks[start..i]);
        if !html.is_empty() {
            out.push(RenderUnit::content(html));
        }
    }
}

/// Group render units into chapters, splitting at the SHALLOWEST heading level actually present
/// in the document (the smallest `fmt_heading_level` among all heading units) — that is the
/// level "Chapter"/"Part" headings use in a typical manuscript; any deeper level (e.g. `##`
/// scene breaks under `#` chapters) stays inline as ordinary heading markup within whichever
/// chapter it falls in, rather than starting a new one.
///
/// Content before the first split-level heading becomes a front-matter chapter — only emitted
/// when non-empty, so a document that opens with its first chapter heading gets no empty
/// leading chapter. A document with no headings at all is one chapter. Every chapter's title is
/// the text of the heading that opened it, except the front-matter chapter (and the single
/// chapter of a headingless document), which takes the book title from `options` (or
/// "Untitled" when that's blank too).
fn split_into_chapters(units: Vec<RenderUnit>, options: &EpubExportOptions) -> Vec<Chapter> {
    let front_title = if options.title.trim().is_empty() {
        "Untitled".to_string()
    } else {
        options.title.clone()
    };

    let Some(target_level) = units.iter().filter_map(|u| u.heading_level).min() else {
        // No headings anywhere: the whole document is one chapter.
        let body_html: String = units.into_iter().map(|u| u.html).collect();
        return vec![Chapter {
            title: front_title,
            body_html,
        }];
    };

    let mut chapters: Vec<Chapter> = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_html = String::new();

    for unit in units {
        if unit.heading_level == Some(target_level) {
            if !current_html.is_empty() || current_title.is_some() {
                chapters.push(Chapter {
                    title: current_title.take().unwrap_or_else(|| front_title.clone()),
                    body_html: std::mem::take(&mut current_html),
                });
            }
            current_title = unit.heading_text.clone();
        }
        current_html.push_str(&unit.html);
    }

    if !current_html.is_empty() || current_title.is_some() {
        chapters.push(Chapter {
            title: current_title.unwrap_or(front_title),
            body_html: current_html,
        });
    }

    chapters
}

/// Wrap one chapter's rendered body HTML in a complete, valid XHTML document — required by the
/// EPUB container format (each content document must be well-formed XML, unlike the loose HTML
/// the `html_render` fragments were designed to sit inside as part of a larger `<body>`).
fn wrap_xhtml(title: &str, lang: &str, rtl: bool, body_html: &str) -> String {
    let dir_attr = if rtl { " dir=\"rtl\"" } else { "" };
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <!DOCTYPE html>\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"{lang}\" lang=\"{lang}\"{dir_attr}>\n\
         <head><meta charset=\"utf-8\"/><title>{title}</title></head><body>{body}</body></html>",
        lang = lang,
        dir_attr = dir_attr,
        title = html_render::escape_html(title),
        body = body_html,
    )
}

/// Package `chapters` into a complete EPUB 3 file's bytes via `epub_builder`.
///
/// Note: [`EpubBuilder::epub_direction`] only sets a builder-level field that (in
/// `epub-builder` 0.8.3) is never read back out when rendering `content.opf` — the OPF's
/// `page-progression-direction` actually comes from the generic [`EpubBuilder::metadata`]
/// setter's `"direction"` key. Both are set here (the dedicated setter in case a future
/// `epub-builder` version wires it up; `metadata` because it's what actually reaches the
/// package today), so the RTL option keeps working across an `epub-builder` upgrade either way.
fn package_epub(options: &EpubExportOptions, chapters: &[Chapter]) -> Result<Vec<u8>> {
    let lang = if options.language.trim().is_empty() {
        "en"
    } else {
        options.language.trim()
    };

    let zip = ZipLibrary::new().map_err(|e| anyhow!("EPUB: {e}"))?;
    let mut builder = EpubBuilder::new(zip).map_err(|e| anyhow!("EPUB: {e}"))?;
    builder.epub_version(EpubVersion::V30);
    builder.add_language(lang);
    if !options.title.trim().is_empty() {
        builder.set_title(options.title.trim());
    }
    if !options.author.trim().is_empty() {
        builder.add_author(options.author.trim());
    }
    builder.set_generator("Skribisto");
    if options.rtl {
        builder.epub_direction(PageDirection::Rtl);
        builder
            .metadata("direction", "rtl")
            .map_err(|e| anyhow!("EPUB: {e}"))?;
    }

    for (i, chapter) in chapters.iter().enumerate() {
        let xhtml = wrap_xhtml(&chapter.title, lang, options.rtl, &chapter.body_html);
        let href = format!("chapter_{:03}.xhtml", i + 1);
        builder
            .add_content(
                EpubContent::new(href, xhtml.as_bytes())
                    .title(chapter.title.clone())
                    .reftype(ReferenceType::Text),
            )
            .map_err(|e| anyhow!("EPUB: {e}"))?;
    }

    builder.inline_toc();

    let mut bytes: Vec<u8> = Vec::new();
    builder
        .generate(&mut bytes)
        .map_err(|e| anyhow!("EPUB: {e}"))?;
    Ok(bytes)
}

/// Return `Err` if a cancellation flag is present and set.
fn check_cancelled(cancel_flag: Option<&AtomicBool>) -> Result<()> {
    if let Some(flag) = cancel_flag
        && flag.load(Ordering::Relaxed)
    {
        return Err(anyhow!("Operation was cancelled"));
    }
    Ok(())
}
