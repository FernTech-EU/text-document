// Custom implementation — hand-maintained, do NOT blanket-regenerate. Mirrors
// `export_epub_uc.rs`'s `LongOperation` shape (frozen read transaction via the uow,
// progress/cancel, build-then-write) and `export_html_uc.rs`'s Root→Document→Frame→Block walk
// (`render_frame_html`/`render_frame_by_child_order`), but the walk here builds a Typst markup
// string via `crate::typst_markup` — never HTML — then compiles it to PDF bytes via
// `crate::typst_compile::compile_typst_pdf`. Per the "a use case may not call another use case"
// rule, the markup emission itself lives in `typst_markup` (a plain function library both this
// use case and its own tests call), not duplicated here and not borrowed from `html_render`
// (Typst is a third, distinct output substrate — same reasoning that gives DOCX and LaTeX each
// their own from-scratch walk).

use crate::ExportPdfDto;
use crate::ExportPdfResultDto;
use crate::typst_compile::compile_typst_pdf;
use crate::typst_markup::{render_blocks_typst, render_table_typst, typst_preamble};
use anyhow::{Result, anyhow};
use common::database::QueryUnitOfWork;
use common::entities::{Block, Document, Frame, List, Root, Table, TableCell};
use common::long_operation::{LongOperation, OperationProgress};
use common::types::{EntityId, ROOT_ENTITY_ID};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub trait ExportPdfUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn ExportPdfUnitOfWorkTrait>;
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
pub trait ExportPdfUnitOfWorkTrait: QueryUnitOfWork + Send + Sync {}

pub struct ExportPdfUseCase {
    uow_factory: Box<dyn ExportPdfUnitOfWorkFactoryTrait>,
    dto: ExportPdfDto,
}

impl ExportPdfUseCase {
    pub fn new(uow_factory: Box<dyn ExportPdfUnitOfWorkFactoryTrait>, dto: &ExportPdfDto) -> Self {
        ExportPdfUseCase {
            uow_factory,
            dto: dto.clone(),
        }
    }
}

impl LongOperation for ExportPdfUseCase {
    type Output = ExportPdfResultDto;

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
            Some("Starting PDF export...".to_string()),
        ));

        let uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let build_result = self.build_markup(
            &*uow,
            progress_callback.as_ref(),
            Some(cancel_flag.as_ref()),
        );

        uow.end_transaction()?;

        let markup = build_result?;

        progress_callback(OperationProgress::new(
            85.0,
            Some("Compiling Typst to PDF...".to_string()),
        ));

        let (pdf_bytes, page_count) =
            compile_typst_pdf(&markup, self.dto.options.font_bytes.clone())?;

        progress_callback(OperationProgress::new(
            95.0,
            Some("Writing PDF file...".to_string()),
        ));

        std::fs::write(&self.dto.output_path, &pdf_bytes).map_err(|e| {
            anyhow!(
                "Failed to write output file '{}': {}",
                self.dto.output_path,
                e
            )
        })?;

        progress_callback(OperationProgress::new(100.0, Some("completed".to_string())));

        Ok(ExportPdfResultDto {
            file_path: self.dto.output_path.clone(),
            page_count: page_count as i64,
        })
    }
}

impl ExportPdfUseCase {
    /// Build the PDF bytes without any file I/O, using a no-op progress callback and no
    /// cancellation, together with the page count. Intended for callers (notably tests) that want
    /// to inspect the compiled PDF directly without touching the filesystem.
    ///
    /// `execute` uses [`Self::build_markup`] + [`compile_typst_pdf`] the same way, then writes the
    /// bytes to disk; the controller exposes this file-less variant as
    /// [`crate::document_io_controller::build_pdf_document`].
    pub(crate) fn build_document(&self) -> Result<(Vec<u8>, i64)> {
        let uow = self.uow_factory.create();
        uow.begin_transaction()?;
        let result = self.build_markup(&*uow, &|_progress| {}, None);
        uow.end_transaction()?;
        let markup = result?;
        let (pdf_bytes, page_count) =
            compile_typst_pdf(&markup, self.dto.options.font_bytes.clone())?;
        Ok((pdf_bytes, page_count as i64))
    }

    /// Build the complete Typst source (preamble + body) for `db_context`'s document, walking
    /// Root→Document→Frame→Block exactly like `export_html_uc`'s traversal (same top-level frame
    /// loop, `child_order` interleaving, cell-frame skip) but emitting Typst markup via
    /// `crate::typst_markup` instead of HTML.
    fn build_markup(
        &self,
        uow: &dyn ExportPdfUnitOfWorkTrait,
        progress_callback: &dyn Fn(OperationProgress),
        cancel_flag: Option<&AtomicBool>,
    ) -> Result<String> {
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

        let mut body_parts: Vec<String> = Vec::new();

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

            let frame_typst = self.render_frame_typst(uow, frame_id, &cell_frame_ids)?;
            if !frame_typst.is_empty() {
                body_parts.push(frame_typst);
            }

            let pct = 10.0 + (frame_idx as f32 / total_frames as f32) * 70.0;
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
            80.0,
            Some("Assembling document...".to_string()),
        ));

        let body = body_parts.join("\n\n");
        let preamble = typst_preamble(&self.dto.options);

        Ok(if preamble.is_empty() {
            body
        } else {
            format!("{preamble}\n{body}")
        })
    }

    /// Render a frame's content as Typst markup, walking its `child_order` to interleave blocks
    /// and sub-frames (blockquotes/tables). Falls back to sorted blocks when `child_order` is
    /// empty. Mirrors `export_html_uc::render_frame_html`.
    fn render_frame_typst(
        &self,
        uow: &dyn ExportPdfUnitOfWorkTrait,
        frame_id: &EntityId,
        cell_frame_ids: &HashSet<EntityId>,
    ) -> Result<String> {
        let frame = uow
            .get_frame(frame_id)?
            .ok_or_else(|| anyhow!("Frame not found"))?;

        // Table anchor frame — render the table instead of blocks.
        if let Some(table_id) = frame.table {
            return render_table_typst(&uow.store(), table_id);
        }

        // If child_order is populated, use it to interleave blocks and sub-frames
        if !frame.child_order.is_empty() {
            return self.render_frame_typst_by_child_order(uow, &frame, cell_frame_ids);
        }

        // Fallback: render all blocks in document_position order (original behaviour)
        let block_ids = uow.get_frame_relationship(
            frame_id,
            &common::direct_access::frame::FrameRelationshipField::Blocks,
        )?;

        if block_ids.is_empty() {
            return Ok(String::new());
        }

        let blocks_opt = uow.get_block_multi(&block_ids)?;
        let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
        blocks.sort_by_key(|b| b.document_position);

        Ok(render_blocks_typst(
            &uow.store(),
            &blocks,
            &self.dto.options,
        ))
    }

    /// Walk `child_order` entries: positive values are block IDs, negative values are negated
    /// sub-frame IDs. Mirrors `export_html_uc::render_frame_by_child_order`. A blockquote
    /// sub-frame is wrapped in `#quote(block: true)[...]` — the Typst analogue of LaTeX's
    /// `\begin{quote}`/HTML's `<blockquote>`.
    fn render_frame_typst_by_child_order(
        &self,
        uow: &dyn ExportPdfUnitOfWorkTrait,
        frame: &Frame,
        cell_frame_ids: &HashSet<EntityId>,
    ) -> Result<String> {
        let mut parts: Vec<String> = Vec::new();
        // Accumulate consecutive blocks so we can group list items.
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
                    let typst =
                        render_blocks_typst(&uow.store(), &pending_blocks, &self.dto.options);
                    if !typst.is_empty() {
                        parts.push(typst);
                    }
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
                        // Recursively render the blockquote frame content
                        let inner = self.render_frame_typst(uow, &sub_frame_id, cell_frame_ids)?;
                        if !inner.is_empty() {
                            parts.push(format!("#quote(block: true)[{inner}]"));
                        }
                    } else {
                        // Non-blockquote sub-frame: render normally
                        let inner = self.render_frame_typst(uow, &sub_frame_id, cell_frame_ids)?;
                        if !inner.is_empty() {
                            parts.push(inner);
                        }
                    }
                }
            }
        }

        // Flush remaining blocks
        if !pending_blocks.is_empty() {
            let typst = render_blocks_typst(&uow.store(), &pending_blocks, &self.dto.options);
            if !typst.is_empty() {
                parts.push(typst);
            }
        }

        Ok(parts.join("\n\n"))
    }
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
