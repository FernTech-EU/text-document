use crate::ExtractFragmentDto;
use crate::ExtractFragmentResultDto;
use anyhow::{Result, anyhow};
use common::database::QueryUnitOfWork;
use common::database::rope_helpers::{
    block_char_length, block_content_via_store, range_covers_table_anchor,
};
use common::direct_access::document::document_repository::DocumentRelationshipField;
use common::direct_access::frame::frame_repository::FrameRelationshipField;
use common::direct_access::root::root_repository::RootRelationshipField;
use common::direct_access::table::TableRelationshipField;
use common::entities::{Block, Frame, List, Root, SemanticRole, Table, TableCell};
use common::format_runs::{InlineContent, InlineSegment};
use common::format_runs_query::inline_segments_for_block;
use common::parser_tools::fragment_schema::{
    FragmentBlock, FragmentData, FragmentElement, FragmentList, FragmentQuoting, FragmentTable,
    FragmentTableCell, fragment_to_json,
};
use common::types::{EntityId, ROOT_ENTITY_ID};
use std::collections::{HashMap, HashSet};

pub trait ExtractFragmentUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn ExtractFragmentUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "GetRO")]
#[macros::uow_action(entity = "Root", action = "GetRelationshipRO")]
#[macros::uow_action(entity = "Document", action = "GetRelationshipRO")]
#[macros::uow_action(entity = "Frame", action = "GetRO")]
#[macros::uow_action(entity = "Frame", action = "GetRelationshipRO")]
#[macros::uow_action(entity = "Block", action = "GetMultiRO")]
#[macros::uow_action(entity = "Block", action = "GetRelationshipRO")]
#[macros::uow_action(entity = "List", action = "GetRO")]
#[macros::uow_action(entity = "Table", action = "GetRO")]
#[macros::uow_action(entity = "Table", action = "GetRelationshipRO")]
#[macros::uow_action(entity = "TableCell", action = "GetMultiRO")]
pub trait ExtractFragmentUnitOfWorkTrait: QueryUnitOfWork {}

pub struct ExtractFragmentUseCase {
    uow_factory: Box<dyn ExtractFragmentUnitOfWorkFactoryTrait>,
}

impl ExtractFragmentUseCase {
    pub fn new(uow_factory: Box<dyn ExtractFragmentUnitOfWorkFactoryTrait>) -> Self {
        ExtractFragmentUseCase { uow_factory }
    }

    pub fn execute(&mut self, dto: &ExtractFragmentDto) -> Result<ExtractFragmentResultDto> {
        let uow = self.uow_factory.create();
        uow.begin_transaction()?;

        let store = uow.store();

        let start = dto.position.min(dto.anchor);
        let end = dto.position.max(dto.anchor);

        // Empty range
        if start == end {
            uow.end_transaction()?;
            let empty = FragmentData {
                blocks: vec![],
                tables: vec![],
            };
            return Ok(ExtractFragmentResultDto {
                fragment_data: serde_json::to_string(&empty)?,
                plain_text: String::new(),
            });
        }

        // Get Root -> Document
        let root = uow
            .get_root(&ROOT_ENTITY_ID)?
            .ok_or_else(|| anyhow!("Root entity not found"))?;
        let doc_ids = uow.get_root_relationship(&root.id, &RootRelationshipField::Document)?;
        let doc_id = *doc_ids
            .first()
            .ok_or_else(|| anyhow!("Root has no document"))?;

        let frame_ids =
            uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Frames)?;

        // ── Build block→cell mapping from all tables ──────────────
        let table_ids =
            uow.get_document_relationship(&doc_id, &DocumentRelationshipField::Tables)?;

        // block_id → (cell_frame_id, table_id, cell entity)
        let mut block_to_cell: HashMap<EntityId, (EntityId, EntityId, TableCell)> = HashMap::new();

        for &tid in &table_ids {
            let cell_ids = uow.get_table_relationship(&tid, &TableRelationshipField::Cells)?;
            let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
            for cell in cells_opt.into_iter().flatten() {
                if let Some(cf_id) = cell.cell_frame {
                    let blk_ids =
                        uow.get_frame_relationship(&cf_id, &FrameRelationshipField::Blocks)?;
                    for bid in blk_ids {
                        block_to_cell.insert(bid, (cf_id, tid, cell.clone()));
                    }
                }
            }
        }

        // ── Collect the blocks of every tree: the main text (its quotations and table
        // cells included) and each footnote's body ──
        let mut all_block_ids: Vec<EntityId> = Vec::new();
        let mut tree_of_block: HashMap<EntityId, EntityId> = HashMap::new();
        // The quotations each block, and each table, stands in.
        let mut quoted_of_block: HashMap<EntityId, Quoted> = HashMap::new();
        let mut table_quoted: HashMap<EntityId, Quoted> = HashMap::new();
        for (i, frame_id) in frame_ids.iter().enumerate() {
            let is_tree = i == 0
                || uow
                    .get_frame(frame_id)?
                    .is_some_and(|frame| frame.footnote_label.is_some());
            if !is_tree {
                continue;
            }
            let mut tree_blocks: Vec<(EntityId, Quoted)> = Vec::new();
            collect_tree_blocks_ro(
                &*uow,
                frame_id,
                Quoted::default(),
                &mut tree_blocks,
                &mut table_quoted,
            )?;
            for (block_id, quoted) in tree_blocks {
                tree_of_block.insert(block_id, *frame_id);
                quoted_of_block.insert(block_id, quoted);
                all_block_ids.push(block_id);
            }
        }

        let blocks_opt = uow.get_block_multi(&all_block_ids)?;
        let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
        // The stored field lags the rope by whatever was typed since the last deletion, and the
        // caller's positions are rope positions: read the rope's.
        common::database::rope_helpers::refresh_block_positions(&mut blocks, &store);
        blocks.sort_by_key(|b| b.document_position);

        // A selection copies from the tree it starts in: the main text, or the one note's
        // body it starts in. Every document frame used to be read, footnote definitions
        // included, and a selection running across a body (select all, or from a paragraph
        // to the end) copied the note's text as a paragraph: a paste put it into the prose.
        // A clipboard fragment carries prose, not note bodies, as one made from Djot does.
        let starting_tree = blocks
            .iter()
            .find(|block| block.document_position + block_char_length(block, &store) >= start)
            .and_then(|block| tree_of_block.get(&block.id).copied());
        blocks.retain(|block| tree_of_block.get(&block.id).copied() == starting_tree);

        // Whether the range takes anything of `block`. An empty paragraph closing the tree
        // stands where a range reaching the tree's end ends, and is taken with it: left out,
        // a cut of all of a text and its paste dropped the empty code block or heading the
        // text ended with, which a save keeps.
        let last_block_id = blocks.last().map(|block| block.id);
        let takes = |block: &Block| {
            let block_start = block.document_position;
            let length = block_char_length(block, &store);
            if block_start + length < start {
                return false;
            }
            block_start < end
                || (block_start == end
                    && length == 0
                    && start < end
                    && Some(block.id) == last_block_id)
        };

        // ── Detect cross-cell selection ───────────────────────────
        // Check ALL blocks in range (not just endpoints) — an intermediate
        // block could be in a different cell. A range holding a table's anchor holds the
        // table whole, as a deletion of it takes the table: over a table of one cell, from
        // its anchor on, the range only meets that cell, and a select all of a text that is
        // such a table copied its words and not the table, which the cut then removed.
        let is_cross_cell = range_covers_table_anchor(&store, start, end) || {
            let mut first_cell: Option<Option<EntityId>> = None;
            let mut cross = false;
            for block in &blocks {
                if !takes(block) {
                    continue;
                }
                let cell = block_to_cell.get(&block.id).map(|(cf, _, _)| *cf);
                match first_cell {
                    None => first_cell = Some(cell),
                    Some(fc) if fc != cell => {
                        cross = true;
                        break;
                    }
                    _ => {}
                }
            }
            cross
        };

        if is_cross_cell {
            // ── Mixed / cross-cell: extract non-table blocks + full tables ──
            // Single pass in document order so plain_texts stays ordered.
            let mut fragment_blocks: Vec<FragmentBlock> = Vec::new();
            let mut fragment_tables: Vec<FragmentTable> = Vec::new();
            let mut quoting = FragmentQuoting::default();
            let mut plain_texts: Vec<String> = Vec::new();
            let mut processed_tables: HashSet<EntityId> = HashSet::new();

            for block in &blocks {
                let block_start = block.document_position;
                let block_end = block_start + block_char_length(block, &store);

                if !takes(block) {
                    continue;
                }

                if let Some((_, tid, _)) = block_to_cell.get(&block.id) {
                    // Block is inside a table cell — extract the FULL table
                    // on first encounter (all cells, not just touched ones).
                    if !processed_tables.insert(*tid) {
                        continue; // already extracted
                    }

                    let block_insert_index = fragment_blocks.len();
                    let table = uow
                        .get_table(tid)?
                        .ok_or_else(|| anyhow!("Table {} not found", tid))?;

                    let all_cell_ids =
                        uow.get_table_relationship(tid, &TableRelationshipField::Cells)?;
                    let all_cells_opt = uow.get_table_cell_multi(&all_cell_ids)?;
                    let mut all_cells: Vec<TableCell> =
                        all_cells_opt.into_iter().flatten().collect();
                    all_cells.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));

                    let mut frag_cells: Vec<FragmentTableCell> = Vec::new();
                    for cell in &all_cells {
                        let cell_blocks = if let Some(cf_id) = cell.cell_frame {
                            let blk_ids = uow
                                .get_frame_relationship(&cf_id, &FrameRelationshipField::Blocks)?;
                            let blk_opt = uow.get_block_multi(&blk_ids)?;
                            let mut blks: Vec<Block> = blk_opt.into_iter().flatten().collect();
                            // In reading order, read off the rope: the stored field lags it
                            // by whatever was typed since the last deletion, and a cell of
                            // several paragraphs was copied with them out of order.
                            common::database::rope_helpers::refresh_block_positions(
                                &mut blks, &store,
                            );
                            blks.sort_by_key(|b| b.document_position);
                            blks
                        } else {
                            Vec::new()
                        };

                        let mut cell_frag_blocks: Vec<FragmentBlock> = Vec::new();
                        for cb in &cell_blocks {
                            let (extracted_elements, extracted_text) =
                                self.extract_full_block(&*uow, cb)?;
                            plain_texts.push(extracted_text.clone());
                            cell_frag_blocks.push(block_to_fragment_block(
                                cb,
                                extracted_elements,
                                extracted_text,
                                true,
                                None,
                            ));
                        }

                        frag_cells.push(FragmentTableCell {
                            row: cell.row as usize,
                            column: cell.column as usize,
                            row_span: cell.row_span.max(1) as usize,
                            column_span: cell.column_span.max(1) as usize,
                            blocks: cell_frag_blocks,
                            fmt_padding: cell.fmt_padding,
                            fmt_border: cell.fmt_border,
                            fmt_vertical_alignment: cell.fmt_vertical_alignment.clone(),
                            fmt_background_color: cell.fmt_background_color.clone(),
                        });
                    }

                    fragment_tables.push(FragmentTable {
                        rows: table.rows as usize,
                        columns: table.columns as usize,
                        cells: frag_cells,
                        block_insert_index,
                        fmt_border: table.fmt_border,
                        fmt_cell_spacing: table.fmt_cell_spacing,
                        fmt_cell_padding: table.fmt_cell_padding,
                        fmt_width: table.fmt_width,
                        fmt_alignment: table.fmt_alignment.clone(),
                        column_widths: table.column_widths.clone(),
                    });
                    quoting.set_table(
                        fragment_tables.len() - 1,
                        table_quoted.get(tid).map_or(0, |quoted| quoted.depth),
                    );
                } else {
                    // Non-table block — extract with partial-block handling
                    let local_start = if start > block_start {
                        (start - block_start) as usize
                    } else {
                        0
                    };
                    let local_end = if end < block_end {
                        (end - block_start) as usize
                    } else {
                        block_char_length(block, &store) as usize
                    };

                    let block_text = block_content_via_store(block, &uow.store());
                    let elements = inline_segments_for_block(&uow.store(), block.id, &block_text);

                    let list = if let Some(list_id) = block.list {
                        uow.get_list(&list_id)?
                    } else {
                        None
                    };

                    let (extracted_elements, extracted_text) =
                        extract_elements_in_range(&elements, local_start, local_end);

                    // Word paragraph-mark rule: a block is "full" only when the
                    // selection extends past its text into the paragraph break
                    // gap.  Intermediate blocks are always full (the selection
                    // necessarily traverses their gap).
                    // Exception: the last block in the document has no gap after
                    // it, so covering its entire text is sufficient.
                    let is_last_block = block.id == blocks.last().map(|b| b.id).unwrap_or_default();
                    let is_full_block = local_start == 0
                        && local_end == block_char_length(block, &store) as usize
                        && (end > block_start + block_char_length(block, &store) || is_last_block);

                    plain_texts.push(extracted_text.clone());
                    let fragment_block = block_to_fragment_block(
                        block,
                        extracted_elements,
                        extracted_text,
                        is_full_block,
                        if is_full_block {
                            list.as_ref().map(FragmentList::from_entity)
                        } else {
                            None
                        },
                    );
                    push_block(
                        &mut fragment_blocks,
                        &mut quoting,
                        fragment_block,
                        is_full_block,
                        quoted_of_block.get(&block.id),
                    );
                }
            }

            let fragment_data = FragmentData {
                blocks: fragment_blocks,
                tables: fragment_tables,
            };
            let fragment_json = fragment_to_json(&fragment_data, &quoting)?;
            let plain_text = plain_texts.join("\n");

            uow.end_transaction()?;
            return Ok(ExtractFragmentResultDto {
                fragment_data: fragment_json,
                plain_text,
            });
        }

        // ── Normal text extraction (no cross-cell) ────────────────
        let mut fragment_blocks: Vec<FragmentBlock> = Vec::new();
        let mut quoting = FragmentQuoting::default();
        let mut plain_texts: Vec<String> = Vec::new();

        for block in &blocks {
            let block_start = block.document_position;
            let block_end = block_start + block_char_length(block, &store);

            if !takes(block) {
                continue;
            }

            let local_start = if start > block_start {
                (start - block_start) as usize
            } else {
                0
            };
            let local_end = if end < block_end {
                (end - block_start) as usize
            } else {
                block_char_length(block, &store) as usize
            };

            let block_text = block_content_via_store(block, &uow.store());
            let elements = inline_segments_for_block(&uow.store(), block.id, &block_text);

            let list = if let Some(list_id) = block.list {
                uow.get_list(&list_id)?
            } else {
                None
            };

            let (extracted_elements, extracted_text) =
                extract_elements_in_range(&elements, local_start, local_end);

            // Word paragraph-mark rule: a block is "full" only when the
            // selection extends past its text into the paragraph break gap.
            // Exception: the last block in the document has no gap after
            // it, so covering its entire text is sufficient.
            let is_last_block = block.id == blocks.last().map(|b| b.id).unwrap_or_default();
            let is_full_block = local_start == 0
                && local_end == block_char_length(block, &store) as usize
                && (end > block_start + block_char_length(block, &store) || is_last_block);

            plain_texts.push(extracted_text.clone());
            let fragment_block = block_to_fragment_block(
                block,
                extracted_elements,
                extracted_text,
                is_full_block,
                if is_full_block {
                    list.as_ref().map(FragmentList::from_entity)
                } else {
                    None
                },
            );
            push_block(
                &mut fragment_blocks,
                &mut quoting,
                fragment_block,
                is_full_block,
                quoted_of_block.get(&block.id),
            );
        }

        let fragment_data = FragmentData {
            blocks: fragment_blocks,
            tables: vec![],
        };

        let fragment_json = fragment_to_json(&fragment_data, &quoting)?;
        let plain_text = plain_texts.join("\n");

        uow.end_transaction()?;

        Ok(ExtractFragmentResultDto {
            fragment_data: fragment_json,
            plain_text,
        })
    }
}

impl ExtractFragmentUseCase {
    /// Extract all elements from a full block.
    fn extract_full_block(
        &self,
        uow: &dyn ExtractFragmentUnitOfWorkTrait,
        block: &Block,
    ) -> Result<(Vec<FragmentElement>, String)> {
        let store = uow.store();
        let block_text = block_content_via_store(block, &store);
        let elements = inline_segments_for_block(&store, block.id, &block_text);
        Ok(extract_elements_in_range(
            &elements,
            0,
            block_char_length(block, &store) as usize,
        ))
    }
}

/// Build a `FragmentBlock` from a block entity and its extracted elements. A whole block
/// carries its block formatting; a part of one carries none, and goes into the paragraph it
/// is pasted into. The quotations a whole block stands in go beside it (see [`push_block`]).
fn block_to_fragment_block(
    block: &Block,
    elements: Vec<FragmentElement>,
    plain_text: String,
    is_full_block: bool,
    list: Option<FragmentList>,
) -> FragmentBlock {
    FragmentBlock {
        plain_text,
        elements,
        heading_level: if is_full_block {
            block.fmt_heading_level
        } else {
            None
        },
        list,
        alignment: if is_full_block {
            block.fmt_alignment.clone()
        } else {
            None
        },
        indent: if is_full_block {
            block.fmt_indent
        } else {
            None
        },
        text_indent: if is_full_block {
            block.fmt_text_indent
        } else {
            None
        },
        marker: if is_full_block {
            block.fmt_marker.clone()
        } else {
            None
        },
        top_margin: if is_full_block {
            block.fmt_top_margin
        } else {
            None
        },
        bottom_margin: if is_full_block {
            block.fmt_bottom_margin
        } else {
            None
        },
        left_margin: if is_full_block {
            block.fmt_left_margin
        } else {
            None
        },
        right_margin: if is_full_block {
            block.fmt_right_margin
        } else {
            None
        },
        tab_positions: if is_full_block {
            block.fmt_tab_positions.clone()
        } else {
            vec![]
        },
        line_height: if is_full_block {
            block.fmt_line_height
        } else {
            None
        },
        non_breakable_lines: if is_full_block {
            block.fmt_non_breakable_lines
        } else {
            None
        },
        page_break_before: if is_full_block {
            block.fmt_page_break_before
        } else {
            None
        },
        direction: if is_full_block {
            block.fmt_direction.clone()
        } else {
            None
        },
        background_color: if is_full_block {
            block.fmt_background_color.clone()
        } else {
            None
        },
        is_code_block: if is_full_block {
            block.fmt_is_code_block
        } else {
            None
        },
        code_language: if is_full_block {
            block.fmt_code_language.clone()
        } else {
            None
        },
        hyphenate: if is_full_block {
            block.fmt_hyphenate
        } else {
            None
        },
        language: if is_full_block {
            block.fmt_language.clone()
        } else {
            None
        },
    }
}

/// Add `block` to `blocks`, and the quotations it stands in to `quoting` when it is a whole
/// block: a part of one goes into the paragraph it is pasted into, quoted or not.
fn push_block(
    blocks: &mut Vec<FragmentBlock>,
    quoting: &mut FragmentQuoting,
    block: FragmentBlock,
    is_full_block: bool,
    quoted: Option<&Quoted>,
) {
    if is_full_block && let Some(quoted) = quoted {
        quoting.set_block(blocks.len(), quoted.depth, quoted.role.clone());
    }
    blocks.push(block);
}

/// Extract elements within a character range [local_start, local_end) of a block.
/// Returns the extracted FragmentElements and the concatenated plain text.
fn extract_elements_in_range(
    elements: &[InlineSegment],
    local_start: usize,
    local_end: usize,
) -> (Vec<FragmentElement>, String) {
    let mut result_elements: Vec<FragmentElement> = Vec::new();
    let mut result_text = String::new();
    let mut char_cursor: usize = 0;

    for elem in elements {
        let elem_char_len = match &elem.content {
            InlineContent::Text(s) => s.chars().count(),
            InlineContent::Image { .. } | InlineContent::FootnoteRef { .. } => 1,
            InlineContent::Empty => 0,
        };

        let elem_start = char_cursor;
        let elem_end = char_cursor + elem_char_len;

        // Skip elements entirely before range
        if elem_end <= local_start {
            char_cursor = elem_end;
            continue;
        }
        // Stop if entirely after range
        if elem_start >= local_end {
            break;
        }

        // This element overlaps with [local_start, local_end)
        let take_start = local_start.saturating_sub(elem_start);
        let take_end = if local_end < elem_end {
            local_end - elem_start
        } else {
            elem_char_len
        };

        match &elem.content {
            InlineContent::Text(s) => {
                let chars: Vec<char> = s.chars().collect();
                let slice: String = chars[take_start..take_end].iter().collect();
                if !slice.is_empty() {
                    let mut fe = FragmentElement::from_segment(elem);
                    fe.content = InlineContent::Text(slice.clone());
                    result_elements.push(fe);
                    result_text.push_str(&slice);
                }
            }
            // Both objects are 1 char, included only whole. The sentinel goes
            // into the fragment's own plain text here, which is the half
            // `insert_fragment_uc` reads back — the two must agree about what
            // the object costs or a paste shifts every position after it.
            InlineContent::Image { .. } | InlineContent::FootnoteRef { .. } => {
                if take_start == 0 && take_end == 1 {
                    result_elements.push(FragmentElement::from_segment(elem));
                    result_text.push('\u{FFFC}');
                }
            }
            InlineContent::Empty => {}
        }

        char_cursor = elem_end;
    }

    (result_elements, result_text)
}

/// The quotations a block or a table stands in: how many, and the role of the innermost.
#[derive(Debug, Clone, Default)]
struct Quoted {
    depth: u32,
    role: Option<SemanticRole>,
}

/// Collect the blocks of the tree under `frame_id` in reading order, each with the
/// quotations it stands in, traversing sub-frames (quotations) and table cell frames, and
/// record the quotations each table stands in.
fn collect_tree_blocks_ro(
    uow: &dyn ExtractFragmentUnitOfWorkTrait,
    frame_id: &EntityId,
    quoted: Quoted,
    out: &mut Vec<(EntityId, Quoted)>,
    table_quoted: &mut HashMap<EntityId, Quoted>,
) -> Result<()> {
    let frame = match uow.get_frame(frame_id)? {
        Some(f) => f,
        None => return Ok(()),
    };

    if frame.child_order.is_empty() {
        let block_ids = uow.get_frame_relationship(frame_id, &FrameRelationshipField::Blocks)?;
        out.extend(block_ids.into_iter().map(|id| (id, quoted.clone())));
        return Ok(());
    }
    for &entry in &frame.child_order {
        if entry > 0 {
            out.push((entry as EntityId, quoted.clone()));
            continue;
        }
        if entry == 0 {
            continue;
        }
        let sub_frame_id = (-entry) as EntityId;
        let Some(sub_frame) = uow.get_frame(&sub_frame_id)? else {
            continue;
        };
        if let Some(table_entity_id) = sub_frame.table {
            // Table anchor frame: expand cell frames
            table_quoted.insert(table_entity_id, quoted.clone());
            let cell_ids =
                uow.get_table_relationship(&table_entity_id, &TableRelationshipField::Cells)?;
            let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
            let mut cells: Vec<_> = cells_opt.into_iter().flatten().collect();
            cells.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));
            for c in cells {
                if let Some(cf_id) = c.cell_frame {
                    collect_tree_blocks_ro(uow, &cf_id, Quoted::default(), out, table_quoted)?;
                }
            }
        } else {
            // A quotation one deeper; any other sub-frame at the same depth.
            let inner = if sub_frame.fmt_is_blockquote == Some(true) {
                Quoted {
                    depth: quoted.depth + 1,
                    role: sub_frame.fmt_semantic_role.clone(),
                }
            } else {
                quoted.clone()
            };
            collect_tree_blocks_ro(uow, &sub_frame_id, inner, out, table_quoted)?;
        }
    }
    Ok(())
}
