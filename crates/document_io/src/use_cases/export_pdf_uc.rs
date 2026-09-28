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
use crate::typst_markup::{
    hoist_leading_page_break, page_break_in, render_blocks_typst, render_table_typst,
    typst_preamble,
};
use anyhow::{Result, anyhow};
use common::database::QueryUnitOfWork;
use common::entities::{
    Alignment, Block, Document, Frame, List, Root, SemanticRole, Table, TableCell,
};
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

        let (pdf_bytes, page_count) = compile_typst_pdf(
            &markup,
            self.dto.options.font_bytes.clone(),
            &self.dto.options.images,
        )?;

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
        let (pdf_bytes, page_count) = compile_typst_pdf(
            &markup,
            self.dto.options.font_bytes.clone(),
            &self.dto.options.images,
        )?;
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

        let notes = crate::footnotes::Footnotes::build(&uow.store());

        // Each note's body as Typst markup, rendered before the prose that
        // cites it — `#footnote[…]` takes its text at the reference. Built with
        // an empty map so a note citing another note degrades to a marker
        // rather than recursing.
        let mut note_bodies = crate::typst_markup::TypstNotes::new();
        {
            let empty = crate::typst_markup::TypstNotes::new();
            for (_, label, frame_id) in notes.in_print_order() {
                let block_ids = uow.get_frame_relationship(
                    &frame_id,
                    &common::direct_access::frame::FrameRelationshipField::Blocks,
                )?;
                let blocks_opt = uow.get_block_multi(&block_ids)?;
                let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
                // Ordered by where each block starts in the rope: the stored field lags it by
                // whatever was typed since something last wrote it.
                common::database::rope_helpers::refresh_block_positions(&mut blocks, &uow.store());
                blocks.sort_by_key(|b| b.document_position);
                // A note cannot start a page: its text is set at the foot of the page
                // that cites it, inside the footnote, where a page break failed the
                // whole export. A host can set one on a note's paragraph all the same.
                for block in &mut blocks {
                    block.fmt_page_break_before = None;
                }
                let body = crate::typst_markup::render_blocks_typst(
                    &uow.store(),
                    &blocks,
                    &self.dto.options,
                    &empty,
                    false,
                );
                note_bodies.insert(label, body.trim().to_string());
            }
        }

        let total_frames = frame_ids.len().max(1);
        for (frame_idx, frame_id) in frame_ids.iter().enumerate() {
            check_cancelled(cancel_flag)?;

            // Skip cell frames — they're rendered as part of their table.
            if cell_frame_ids.contains(frame_id) {
                continue;
            }
            // Skip note bodies: a definition is a top-level frame, so this
            // walk would otherwise render it as ordinary prose in the middle of
            // the chapter, at the point the definition happened to be typed.
            if notes.is_definition(*frame_id) {
                continue;
            }
            // Skip sub-frames (parent_frame != None) — recursively rendered by their parent's
            // walk; rendering them again at the top level would duplicate their content.
            if let Some(f) = uow.get_frame(frame_id)?
                && f.parent_frame.is_some()
            {
                continue;
            }

            let frame_typst =
                self.render_frame_typst(uow, frame_id, &cell_frame_ids, &note_bodies, 0)?;
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
    ///
    /// `quote_depth` is how many quotations the frame's content sits in: see
    /// [`MAX_TYPST_QUOTE_NESTING`] for what is written past it.
    fn render_frame_typst(
        &self,
        uow: &dyn ExportPdfUnitOfWorkTrait,
        frame_id: &EntityId,
        cell_frame_ids: &HashSet<EntityId>,
        notes: &crate::typst_markup::TypstNotes,
        quote_depth: usize,
    ) -> Result<String> {
        let frame = uow
            .get_frame(frame_id)?
            .ok_or_else(|| anyhow!("Frame not found"))?;

        // Table anchor frame — render the table instead of blocks.
        if let Some(table_id) = frame.table {
            let table = render_table_typst(
                &uow.store(),
                table_id,
                &crate::typst_markup::typst_image_paths(&self.dto.options.images),
                notes,
            )?;
            return Ok(indent_past_quote_nesting(&table, quote_depth));
        }

        // If child_order is populated, use it to interleave blocks and sub-frames
        if !frame.child_order.is_empty() {
            return self.render_frame_typst_by_child_order(
                uow,
                &frame,
                cell_frame_ids,
                notes,
                quote_depth,
            );
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
        // Ordered by where each block starts in the rope: the stored field lags it by
        // whatever was typed since something last wrote it.
        common::database::rope_helpers::refresh_block_positions(&mut blocks, &uow.store());
        blocks.sort_by_key(|b| b.document_position);

        let typst = render_blocks_typst(
            &uow.store(),
            &blocks,
            &self.dto.options,
            notes,
            quote_depth > 0,
        );
        Ok(indent_past_quote_nesting(&typst, quote_depth))
    }

    /// An epigraph's body and its attribution, rendered separately, or `None` when the
    /// quote has no attribution line to lift out.
    ///
    /// The attribution is the trailing right-aligned block — the convention the editor
    /// writes and every other writer already renders on. Nothing extra is recorded to
    /// mark it, because the alignment the author gave the line already says which it is.
    /// `None` when there is no such trailing block (a bare quotation with no source), so
    /// the caller falls back to an ordinary `#quote` rather than inventing an empty
    /// attribution.
    fn split_epigraph_typst(
        &self,
        uow: &dyn ExportPdfUnitOfWorkTrait,
        frame: &Frame,
        _cell_frame_ids: &HashSet<EntityId>,
        notes: &crate::typst_markup::TypstNotes,
    ) -> Result<Option<(String, String)>> {
        let mut blocks: Vec<Block> = Vec::new();
        for &entry in &frame.child_order {
            if entry > 0
                && let Some(block) = uow.get_block(&(entry as u64))?
            {
                blocks.push(block);
            }
        }
        // A single block is the quotation itself; there is nothing to attribute to.
        if blocks.len() < 2 {
            return Ok(None);
        }
        let last_is_attribution = blocks
            .last()
            .is_some_and(|b| b.fmt_alignment == Some(Alignment::Right));
        if !last_is_attribution {
            return Ok(None);
        }
        let attribution_block = blocks.pop().expect("checked non-empty above");
        // Both are written inside the epigraph's quotation.
        let body = render_blocks_typst(&uow.store(), &blocks, &self.dto.options, notes, true);
        let attribution = render_blocks_typst(
            &uow.store(),
            std::slice::from_ref(&attribution_block),
            &self.dto.options,
            notes,
            true,
        );
        Ok(Some((body, attribution)))
    }

    /// Walk `child_order` entries: positive values are block IDs, negative values are negated
    /// sub-frame IDs. Mirrors `export_html_uc::render_frame_by_child_order`. A blockquote
    /// sub-frame is wrapped in `#quote(block: true)[...]`, the Typst analogue of LaTeX's
    /// `\begin{quote}`/HTML's `<blockquote>`, up to [`MAX_TYPST_QUOTE_NESTING`] deep, and
    /// written beside the quotation it sits in past that.
    fn render_frame_typst_by_child_order(
        &self,
        uow: &dyn ExportPdfUnitOfWorkTrait,
        frame: &Frame,
        cell_frame_ids: &HashSet<EntityId>,
        notes: &crate::typst_markup::TypstNotes,
        quote_depth: usize,
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
                    let typst = render_blocks_typst(
                        &uow.store(),
                        &pending_blocks,
                        &self.dto.options,
                        notes,
                        quote_depth > 0,
                    );
                    if !typst.is_empty() {
                        parts.push(indent_past_quote_nesting(&typst, quote_depth));
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
                    if sf.fmt_is_blockquote == Some(true) && quote_depth >= MAX_TYPST_QUOTE_NESTING
                    {
                        // Past the nesting Typst lays out, the quotation is written
                        // beside the one it sits in, each of its pieces indented as far
                        // as the quotations it is in.
                        let inner = self.render_frame_typst(
                            uow,
                            &sub_frame_id,
                            cell_frame_ids,
                            notes,
                            quote_depth + 1,
                        )?;
                        if !inner.is_empty() {
                            parts.push(inner);
                        }
                    } else if sf.fmt_is_blockquote == Some(true) {
                        // An epigraph uses Typst's own attribution slot rather than
                        // letting the source line fall through as one more paragraph:
                        // `quote` then sets it the way a quotation's attribution is set,
                        // and a reader's tooling can tell the two apart.
                        if sf.fmt_semantic_role == Some(SemanticRole::Epigraph)
                            && let Some((body, attribution)) =
                                self.split_epigraph_typst(uow, sf, cell_frame_ids, notes)?
                        {
                            // A page break opening the quotation has to come out of it:
                            // a weak break opening a container does nothing, and it means
                            // "start a page, then quote" in any case.
                            let (brk, body) = hoist_leading_page_break(&body);
                            if brk {
                                parts.push(page_break_in(quote_depth > 0).to_string());
                            }
                            parts.push(format!(
                                "#quote(block: true, attribution: [{attribution}])[{body}]"
                            ));
                            continue;
                        }
                        // Recursively render the blockquote frame content
                        let inner = self.render_frame_typst(
                            uow,
                            &sub_frame_id,
                            cell_frame_ids,
                            notes,
                            quote_depth + 1,
                        )?;
                        if !inner.is_empty() {
                            let (brk, inner) = hoist_leading_page_break(&inner);
                            if brk {
                                parts.push(page_break_in(quote_depth > 0).to_string());
                            }
                            parts.push(format!("#quote(block: true)[{inner}]"));
                        }
                    } else {
                        // Non-blockquote sub-frame: render normally
                        let inner = self.render_frame_typst(
                            uow,
                            &sub_frame_id,
                            cell_frame_ids,
                            notes,
                            quote_depth,
                        )?;
                        if !inner.is_empty() {
                            parts.push(inner);
                        }
                    }
                }
            }
        }

        // Flush remaining blocks
        if !pending_blocks.is_empty() {
            let typst = render_blocks_typst(
                &uow.store(),
                &pending_blocks,
                &self.dto.options,
                notes,
                quote_depth > 0,
            );
            if !typst.is_empty() {
                parts.push(indent_past_quote_nesting(&typst, quote_depth));
            }
        }

        Ok(parts.join("\n\n"))
    }
}

/// How many quotations are written inside one another as Typst's own `#quote`.
///
/// Typst gives up on content nested past a fixed depth of show rules and layouts: a whole
/// export failed with "maximum show rule depth exceeded" for any quotation 17 deep, and at
/// 15 for one holding a list, a table, a heading, a code block or a footnote, which the
/// editor and the Djot reader both allow many times over. Each `#quote` costs four levels
/// of the 64, so eight leave half of them for what the quotation holds.
///
/// A quotation deeper than this is written beside the one it sits in rather than inside
/// it, each of its pieces in a `#pad` as wide as the quotations it has left out, the 1em a
/// side each `#quote` adds (see [`indent_past_quote_nesting`]). Every word is kept, and
/// the page shows it indented as far as the quotation nests, up to a limit.
const MAX_TYPST_QUOTE_NESTING: usize = 8;

/// The deepest extra indentation, in quotations, [`indent_past_quote_nesting`] gives a
/// piece: 1em a side each, on top of the [`MAX_TYPST_QUOTE_NESTING`] quotations around it.
/// Deeper still, the text keeps this width rather than being squeezed out of the page.
const MAX_TYPST_QUOTE_INDENT: usize = 8;

/// `typst`, one piece of a frame's content, as it is written inside `quote_depth`
/// quotations: unchanged within [`MAX_TYPST_QUOTE_NESTING`], since the `#quote`s around
/// it indent it, and past that in a `#pad` indenting it as far as the quotations left out.
/// A page break opening it is lifted out first, since a weak break opening a container
/// does nothing; it stays inside the quotations around the piece.
fn indent_past_quote_nesting(typst: &str, quote_depth: usize) -> String {
    let Some(levels) = quote_depth.checked_sub(MAX_TYPST_QUOTE_NESTING) else {
        return typst.to_string();
    };
    if levels == 0 || typst.is_empty() {
        return typst.to_string();
    }
    let em = levels.min(MAX_TYPST_QUOTE_INDENT);
    let (brk, body) = hoist_leading_page_break(typst);
    let padded = format!("#pad(x: {em}em)[{body}]");
    if brk {
        format!("{}\n{padded}", page_break_in(true))
    } else {
        padded
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

#[cfg(test)]
mod tests {
    //! Asserts on the raw Typst *source* `build_markup` produces, not the compiled PDF.
    //!
    //! `pdf_export_tests.rs`'s footnote tests ("compiling is the assertion") only prove the
    //! specific `#footnote(<label>)` reference syntax this exporter emits resolves to a real,
    //! existing label — they do NOT prove there is only one `#footnote[…]` definition. Typst
    //! happily compiles two *independent* elements that carry the identical `<label>`, as long
    //! as nothing ever queries it ambiguously (confirmed by hand against `typst_compile`), so a
    //! regression that re-defined the footnote on every citation instead of reusing it via
    //! `mark_emitted` would still produce a valid PDF and keep every existing PDF test green.
    //! Only the source markup can show the actual count of `#footnote[…]` definitions.

    use super::*;
    use crate::ImportDjotDto;
    use crate::document_io_controller;
    use crate::units_of_work::export_pdf_uow::ExportPdfUnitOfWorkFactory;
    use common::parser_tools::PdfExportOptions;

    /// Import `djot` into a fresh store and return the Typst source `build_markup` produces for
    /// it — no font, no compile: `build_markup` never touches either.
    fn markup_from_djot(djot: &str) -> String {
        let (db, ev, _) = test_harness::setup().expect("setup");
        document_io_controller::import_djot_sync(
            &db,
            &ev,
            &ImportDjotDto {
                djot_text: djot.to_string(),
                options: Default::default(),
            },
        )
        .expect("import_djot_sync");

        let dto = ExportPdfDto {
            output_path: String::new(),
            options: PdfExportOptions::default(),
        };
        let uc = ExportPdfUseCase::new(Box::new(ExportPdfUnitOfWorkFactory::new(&db)), &dto);
        let uow = uc.uow_factory.create();
        uow.begin_transaction().expect("begin_transaction");
        let markup = uc
            .build_markup(&*uow, &|_progress| {}, None)
            .expect("build_markup");
        uow.end_transaction().expect("end_transaction");
        markup
    }

    /// Citing the same label twice must define exactly ONE `#footnote[…]`, carrying the note's
    /// body once, and reuse it a second time via Typst's own reference form —
    /// `#footnote(<anchor>)` — pointing at the SAME `<anchor>` the definition labels itself
    /// with. `footnotes.rs`'s own invariant ("a label referenced twice keeps one number — it is
    /// one note"), proven here against the exact markup the PDF is compiled from.
    #[test]
    fn repeat_citation_defines_one_footnote_and_references_it_once() {
        let markup =
            markup_from_djot("First[^n1] and second[^n1] citation.\n\n[^n1]: The note body.\n");
        let anchor = crate::footnotes::safe_label_id("n1");

        assert_eq!(
            markup.matches("The note body.").count(),
            1,
            "the note's body must not be duplicated: {markup}"
        );
        assert_eq!(
            markup.matches("#footnote[").count(),
            1,
            "only the first citation may define a #footnote[...]: {markup}"
        );
        assert_eq!(
            markup.matches("#footnote(<").count(),
            1,
            "the second citation must reuse the note via #footnote(<label>), not redefine it: {markup}"
        );
        assert!(
            markup.contains(&format!("#footnote[The note body.] <{anchor}>")),
            "the definition must label itself with the citation's own anchor: {markup}"
        );
        assert!(
            markup.contains(&format!("#footnote(<{anchor}>)")),
            "the repeat citation must reference that SAME anchor: {markup}"
        );
    }

    /// Past [`MAX_TYPST_QUOTE_NESTING`] a quotation is written beside the one it sits in,
    /// in a `#pad` as wide as the quotations it leaves out, and no `#quote` sits inside
    /// more than that many others: Typst failed the whole export past 16. Every level
    /// keeps its words, in order.
    #[test]
    fn a_quotation_past_the_nesting_typst_lays_out_is_written_beside_it() {
        let depth = 20;
        // Each level in the one before it: the line between two levels stays in the
        // shallower quotation. A blank line there ends every quotation, and the next level
        // opens quotations of its own.
        let djot: String = (1..=depth)
            .map(|d| {
                let markers = "> ".repeat(d);
                format!("{markers}level {d}\n{}\n", markers.trim_end())
            })
            .collect();
        let markup = markup_from_djot(&djot);
        // Each level sits in the one before it, so one `#quote` a level up to the limit
        // is all of them nested.
        assert_eq!(
            markup.matches("#quote(block: true)[").count(),
            MAX_TYPST_QUOTE_NESTING,
            "{markup}"
        );
        let mut from = 0;
        for d in 1..=depth {
            let at = markup[from..]
                .find(&format!("level {d}"))
                .unwrap_or_else(|| panic!("level {d} is missing or out of order: {markup}"));
            from += at;
        }
        for levels in 1..=(depth - MAX_TYPST_QUOTE_NESTING) {
            let em = levels.min(MAX_TYPST_QUOTE_INDENT);
            assert!(
                markup.contains(&format!("#pad(x: {em}em)[level")),
                "{markup}"
            );
        }
        assert!(!markup.contains("#pad(x: 0em)"), "{markup}");
    }

    /// A page break inside a quotation is written as a column break, which Typst lays out
    /// inside a container, and one opening a quotation is lifted out of it, as far out as
    /// it opens quotations: a `#pagebreak` inside one failed the whole export.
    #[test]
    fn a_page_break_inside_a_quotation_is_written_as_a_column_break() {
        let markup = markup_from_djot(
            "Body.\n\n> First.\n>\n> {page_break_before=true}\n> Second.\n>\n> > {page_break_before=true}\n> > Third.\n\nBetween.\n\n> > {page_break_before=true}\n> > Fourth.\n",
        );
        let column_break = "#colbreak(weak: true)";
        let page_break = "#pagebreak(weak: true)";
        assert!(
            markup.contains(&format!(
                "#quote(block: true)[First.\n\n{column_break}\n\nSecond.\n\n{column_break}\n\n#quote(block: true)[Third.]]"
            )),
            "{markup}"
        );
        // Opening a quotation and the one around it, it comes out of both.
        assert!(
            markup.contains(&format!(
                "{page_break}\n\n#quote(block: true)[#quote(block: true)[Fourth.]]"
            )),
            "{markup}"
        );
        assert_eq!(markup.matches(page_break).count(), 1, "{markup}");
    }

    /// Two distinct labels, each cited twice, exercises `TypstNotes::mark_emitted`'s
    /// per-label bookkeeping independently — a shared/mis-scoped flag would either dedupe
    /// across labels (silently dropping a real second note) or never dedupe at all.
    #[test]
    fn independently_repeated_labels_each_get_their_own_one_definition() {
        let markup = markup_from_djot(
            "One[^a] two[^a] three[^b] four[^b].\n\n[^a]: Body A.\n\n[^b]: Body B.\n",
        );
        let anchor_a = crate::footnotes::safe_label_id("a");
        let anchor_b = crate::footnotes::safe_label_id("b");

        assert_eq!(
            markup.matches("Body A.").count(),
            1,
            "note A duplicated: {markup}"
        );
        assert_eq!(
            markup.matches("Body B.").count(),
            1,
            "note B duplicated: {markup}"
        );
        assert_eq!(
            markup.matches("#footnote[").count(),
            2,
            "exactly one definition per label: {markup}"
        );
        assert!(markup.contains(&format!("#footnote[Body A.] <{anchor_a}>")));
        assert!(markup.contains(&format!("#footnote(<{anchor_a}>)")));
        assert!(markup.contains(&format!("#footnote[Body B.] <{anchor_b}>")));
        assert!(markup.contains(&format!("#footnote(<{anchor_b}>)")));
    }
}
