//! Read-only frame handle and shared flow traversal logic.

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;

use frontend::commands::{block_commands, frame_commands, table_cell_commands, table_commands};
use frontend::common::types::EntityId;

use crate::FrameFormat;
use crate::convert::to_usize;
use crate::flow::{CellSnapshot, FlowElement, FlowElementSnapshot, FrameSnapshot, TableSnapshot};
use crate::inner::TextDocumentInner;
use crate::text_block::TextBlock;
use crate::text_table::TextTable;

/// A read-only handle to a frame in the document.
///
/// Obtained from [`FlowElement::Frame`] or [`TextBlock::frame()`].
#[derive(Clone)]
pub struct TextFrame {
    pub(crate) doc: Arc<Mutex<TextDocumentInner>>,
    pub(crate) frame_id: usize,
}

impl TextFrame {
    /// Stable entity ID.
    pub fn id(&self) -> usize {
        self.frame_id
    }

    /// Frame formatting (height, width, margins, padding, border, position).
    pub fn format(&self) -> FrameFormat {
        let inner = self.doc.lock();
        let frame_dto = frame_commands::get_frame(&inner.ctx, &(self.frame_id as EntityId))
            .ok()
            .flatten();
        match frame_dto {
            Some(f) => frame_dto_to_format(&f),
            None => FrameFormat::default(),
        }
    }

    /// Nested flow within this frame. Same `child_order` traversal as
    /// [`TextDocument::flow()`](crate::TextDocument::flow).
    pub fn flow(&self) -> Vec<FlowElement> {
        let inner = self.doc.lock();
        build_flow_elements(&inner, &self.doc, self.frame_id as EntityId)
    }

    /// Snapshot of this frame and all its contents, captured in a single
    /// lock acquisition. Thread-safe — the returned [`FrameSnapshot`]
    /// contains only plain data.
    pub fn snapshot(&self) -> FrameSnapshot {
        let inner = self.doc.lock();
        let format = frame_commands::get_frame(&inner.ctx, &(self.frame_id as EntityId))
            .ok()
            .flatten()
            .map(|f| frame_dto_to_format(&f))
            .unwrap_or_default();
        let elements = build_flow_snapshot(&inner, self.frame_id as EntityId, inner.highlight_kind);
        FrameSnapshot {
            frame_id: self.frame_id,
            format,
            elements,
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Shared flow traversal (used by TextDocument and TextFrame)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Build flow elements for a frame, returning FlowElement variants.
///
/// This is the main entry point. `doc_arc` is the shared document handle
/// that will be cloned into each returned handle.
pub(crate) fn build_flow_elements(
    inner: &TextDocumentInner,
    doc_arc: &Arc<Mutex<TextDocumentInner>>,
    frame_id: EntityId,
) -> Vec<FlowElement> {
    let frame_dto = match frame_commands::get_frame(&inner.ctx, &frame_id)
        .ok()
        .flatten()
    {
        Some(f) => f,
        None => return Vec::new(),
    };

    if !frame_dto.child_order.is_empty() {
        flow_from_child_order(inner, doc_arc, &frame_dto.child_order)
    } else {
        flow_fallback(inner, doc_arc, &frame_dto)
    }
}

/// Build flow from populated `child_order`.
fn flow_from_child_order(
    inner: &TextDocumentInner,
    doc_arc: &Arc<Mutex<TextDocumentInner>>,
    child_order: &[i64],
) -> Vec<FlowElement> {
    let mut elements = Vec::with_capacity(child_order.len());

    for &entry in child_order {
        if entry > 0 {
            // Positive: block ID
            elements.push(FlowElement::Block(TextBlock {
                doc: Arc::clone(doc_arc),
                block_id: entry as usize,
            }));
        } else if entry < 0 {
            // Negative: frame ID (negated)
            let sub_frame_id = (-entry) as EntityId;
            if let Some(sub_frame) = frame_commands::get_frame(&inner.ctx, &sub_frame_id)
                .ok()
                .flatten()
            {
                if let Some(table_id) = sub_frame.table {
                    // Anchor frame for a table
                    elements.push(FlowElement::Table(TextTable {
                        doc: Arc::clone(doc_arc),
                        table_id: table_id as usize,
                    }));
                } else {
                    // Non-table sub-frame
                    elements.push(FlowElement::Frame(TextFrame {
                        doc: Arc::clone(doc_arc),
                        frame_id: sub_frame_id as usize,
                    }));
                }
            }
        }
        // entry == 0 is ignored (shouldn't happen)
    }

    elements
}

/// Fallback flow: iterate blocks sorted by document_position, skip cell frames.
fn flow_fallback(
    inner: &TextDocumentInner,
    doc_arc: &Arc<Mutex<TextDocumentInner>>,
    frame_dto: &frontend::frame::dtos::FrameDto,
) -> Vec<FlowElement> {
    // Build set of cell frame IDs to skip
    let cell_frame_ids = build_cell_frame_ids(inner);

    // Get blocks in this frame, sorted by document_position
    let block_ids = &frame_dto.blocks;
    let mut block_dtos: Vec<_> = block_ids
        .iter()
        .filter_map(|&id| {
            block_commands::get_block(&inner.ctx, &{ id })
                .ok()
                .flatten()
        })
        .collect();
    let store = inner.ctx.db_context.get_store();
    crate::inner::refresh_block_positions(&mut block_dtos, store);
    block_dtos.sort_by_key(|b| b.document_position);

    let mut elements: Vec<FlowElement> = block_dtos
        .iter()
        .map(|b| {
            FlowElement::Block(TextBlock {
                doc: Arc::clone(doc_arc),
                block_id: b.id as usize,
            })
        })
        .collect();

    // Also check for sub-frames that are children of this frame's document
    // but not cell frames. In fallback mode, we can't interleave perfectly,
    // so we append sub-frames after blocks.
    // For the main frame, get all document frames and check parentage.
    let all_frames = frame_commands::get_all_frame(&inner.ctx).unwrap_or_default();
    for f in &all_frames {
        if f.id == frame_dto.id {
            continue; // skip self
        }
        if cell_frame_ids.contains(&(f.id as EntityId)) {
            continue; // skip cell frames
        }
        // Check if this frame's parent is the current frame
        if f.parent_frame == Some(frame_dto.id) {
            if let Some(table_id) = f.table {
                elements.push(FlowElement::Table(TextTable {
                    doc: Arc::clone(doc_arc),
                    table_id: table_id as usize,
                }));
            } else {
                elements.push(FlowElement::Frame(TextFrame {
                    doc: Arc::clone(doc_arc),
                    frame_id: f.id as usize,
                }));
            }
        }
    }

    elements
}

/// Build a set of all frame IDs that are table cell frames.
fn build_cell_frame_ids(inner: &TextDocumentInner) -> HashSet<EntityId> {
    let mut ids = HashSet::new();
    let all_cells = table_cell_commands::get_all_table_cell(&inner.ctx).unwrap_or_default();
    for cell in &all_cells {
        if let Some(frame_id) = cell.cell_frame {
            ids.insert(frame_id);
        }
    }
    ids
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Snapshot helpers (called while lock is held)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Build a FlowSnapshot for the given frame. Called while lock is held.
///
/// Block positions are computed on-the-fly from child_order + text_length
/// rather than using stored `document_position` values (which may be stale
/// after insert_text defers position updates for performance).
pub(crate) fn build_flow_snapshot(
    inner: &TextDocumentInner,
    frame_id: EntityId,
    effective_kind: crate::highlight::HighlighterKind,
) -> Vec<FlowElementSnapshot> {
    let frame_dto = match frame_commands::get_frame(&inner.ctx, &frame_id)
        .ok()
        .flatten()
    {
        Some(f) => f,
        None => return Vec::new(),
    };

    if !frame_dto.child_order.is_empty() {
        let (elements, _) =
            snapshot_from_child_order(inner, &frame_dto.child_order, 0, frame_id, effective_kind);
        elements
    } else {
        snapshot_fallback(inner, &frame_dto, effective_kind)
    }
}

/// Walk child_order, building snapshots with on-the-fly position computation.
/// `parent_frame_id` is passed down so per-block snapshots skip the
/// expensive `find_parent_frame` walk over every frame in the store —
/// a major contributor to per-keystroke editor lag.
/// Returns (elements, running_position_after_last_block).
fn snapshot_from_child_order(
    inner: &TextDocumentInner,
    child_order: &[i64],
    start_pos: usize,
    parent_frame_id: EntityId,
    effective_kind: crate::highlight::HighlighterKind,
) -> (Vec<FlowElementSnapshot>, usize) {
    let mut elements = Vec::with_capacity(child_order.len());
    let mut running_pos = start_pos;

    for &entry in child_order {
        if entry > 0 {
            let block_id = entry as u64;
            if let Some(snap) = crate::text_block::build_block_snapshot_with_position_and_parent(
                inner,
                block_id,
                Some(running_pos),
                Some(parent_frame_id),
                effective_kind,
            ) {
                running_pos += snap.length + 1; // +1 for block separator
                elements.push(FlowElementSnapshot::Block(snap));
            }
        } else if entry < 0 {
            let sub_frame_id = (-entry) as EntityId;
            if let Some(sub_frame) = frame_commands::get_frame(&inner.ctx, &sub_frame_id)
                .ok()
                .flatten()
            {
                if let Some(table_id) = sub_frame.table {
                    if let Some((snap, new_pos)) = build_table_snapshot_with_positions(
                        inner,
                        table_id,
                        running_pos,
                        effective_kind,
                    ) {
                        running_pos = new_pos;
                        elements.push(FlowElementSnapshot::Table(snap));
                    }
                } else {
                    let (nested, new_pos) = snapshot_from_child_order(
                        inner,
                        &sub_frame.child_order,
                        running_pos,
                        sub_frame_id,
                        effective_kind,
                    );
                    running_pos = new_pos;
                    elements.push(FlowElementSnapshot::Frame(FrameSnapshot {
                        frame_id: sub_frame_id as usize,
                        format: frame_dto_to_format(&sub_frame),
                        elements: nested,
                    }));
                }
            }
        }
    }

    (elements, running_pos)
}

fn snapshot_fallback(
    inner: &TextDocumentInner,
    frame_dto: &frontend::frame::dtos::FrameDto,
    effective_kind: crate::highlight::HighlighterKind,
) -> Vec<FlowElementSnapshot> {
    let cell_frame_ids = build_cell_frame_ids(inner);

    let block_ids = &frame_dto.blocks;
    let mut block_dtos: Vec<_> = block_ids
        .iter()
        .filter_map(|&id| {
            block_commands::get_block(&inner.ctx, &{ id })
                .ok()
                .flatten()
        })
        .collect();
    let store = inner.ctx.db_context.get_store();
    crate::inner::refresh_block_positions(&mut block_dtos, store);
    block_dtos.sort_by_key(|b| b.document_position);

    let mut elements: Vec<FlowElementSnapshot> = block_dtos
        .iter()
        .filter_map(|b| crate::text_block::build_block_snapshot(inner, b.id, effective_kind))
        .map(FlowElementSnapshot::Block)
        .collect();

    let all_frames = frame_commands::get_all_frame(&inner.ctx).unwrap_or_default();
    for f in &all_frames {
        if f.id == frame_dto.id {
            continue;
        }
        if cell_frame_ids.contains(&(f.id as EntityId)) {
            continue;
        }
        if f.parent_frame == Some(frame_dto.id) {
            if let Some(table_id) = f.table {
                if let Some(snap) = build_table_snapshot(inner, table_id, effective_kind) {
                    elements.push(FlowElementSnapshot::Table(snap));
                }
            } else {
                let nested = build_flow_snapshot(inner, f.id as EntityId, effective_kind);
                elements.push(FlowElementSnapshot::Frame(FrameSnapshot {
                    frame_id: f.id as usize,
                    format: frame_dto_to_format(f),
                    elements: nested,
                }));
            }
        }
    }

    elements
}

/// Build a TableSnapshot for the given table ID. Called while lock is held.
pub(crate) fn build_table_snapshot(
    inner: &TextDocumentInner,
    table_id: u64,
    effective_kind: crate::highlight::HighlighterKind,
) -> Option<TableSnapshot> {
    let table_dto = table_commands::get_table(&inner.ctx, &table_id)
        .ok()
        .flatten()?;

    let mut cells = Vec::new();
    for &cell_id in &table_dto.cells {
        if let Some(cell_dto) = table_cell_commands::get_table_cell(&inner.ctx, &{ cell_id })
            .ok()
            .flatten()
        {
            let blocks = if let Some(cell_frame_id) = cell_dto.cell_frame {
                crate::text_block::build_blocks_snapshot_for_frame(
                    inner,
                    cell_frame_id,
                    effective_kind,
                )
            } else {
                Vec::new()
            };
            cells.push(CellSnapshot {
                row: to_usize(cell_dto.row),
                column: to_usize(cell_dto.column),
                row_span: to_usize(cell_dto.row_span),
                column_span: to_usize(cell_dto.column_span),
                format: cell_dto_to_format(&cell_dto),
                blocks,
            });
        }
    }

    Some(TableSnapshot {
        table_id: table_id as usize,
        rows: to_usize(table_dto.rows),
        columns: to_usize(table_dto.columns),
        column_widths: table_dto.column_widths.iter().map(|&v| v as i32).collect(),
        format: table_dto_to_format(&table_dto),
        cells,
    })
}

/// Build a TableSnapshot with computed positions for cell blocks, starting from
/// `start_pos`. Returns `(snapshot, running_pos_after_last_cell_block)`.
///
/// Cells are processed in row-major order, and block positions within each cell
/// are computed sequentially — matching `find_block_at_position_sequential` in
/// the editing use cases.
fn build_table_snapshot_with_positions(
    inner: &TextDocumentInner,
    table_id: u64,
    start_pos: usize,
    effective_kind: crate::highlight::HighlighterKind,
) -> Option<(TableSnapshot, usize)> {
    let table_dto = table_commands::get_table(&inner.ctx, &table_id)
        .ok()
        .flatten()?;

    // Collect and sort cell DTOs in row-major order
    let mut cell_dtos: Vec<_> = table_dto
        .cells
        .iter()
        .filter_map(|&cell_id| {
            table_cell_commands::get_table_cell(&inner.ctx, &{ cell_id })
                .ok()
                .flatten()
        })
        .collect();
    cell_dtos.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));

    let mut running_pos = start_pos;
    let mut cells = Vec::with_capacity(cell_dtos.len());
    for cell_dto in &cell_dtos {
        let blocks = if let Some(cell_frame_id) = cell_dto.cell_frame {
            let (snaps, new_pos) =
                crate::text_block::build_blocks_snapshot_for_frame_with_positions(
                    inner,
                    cell_frame_id,
                    running_pos,
                    effective_kind,
                );
            running_pos = new_pos;
            snaps
        } else {
            Vec::new()
        };
        cells.push(CellSnapshot {
            row: to_usize(cell_dto.row),
            column: to_usize(cell_dto.column),
            row_span: to_usize(cell_dto.row_span),
            column_span: to_usize(cell_dto.column_span),
            format: cell_dto_to_format(cell_dto),
            blocks,
        });
    }

    Some((
        TableSnapshot {
            table_id: table_id as usize,
            rows: to_usize(table_dto.rows),
            columns: to_usize(table_dto.columns),
            column_widths: table_dto.column_widths.iter().map(|&v| v as i32).collect(),
            format: table_dto_to_format(&table_dto),
            cells,
        },
        running_pos,
    ))
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// DTO → public format conversions
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

pub(crate) fn frame_dto_to_format(f: &frontend::frame::dtos::FrameDto) -> FrameFormat {
    FrameFormat {
        height: f.fmt_height.map(|v| v as i32),
        width: f.fmt_width.map(|v| v as i32),
        top_margin: f.fmt_top_margin.map(|v| v as i32),
        bottom_margin: f.fmt_bottom_margin.map(|v| v as i32),
        left_margin: f.fmt_left_margin.map(|v| v as i32),
        right_margin: f.fmt_right_margin.map(|v| v as i32),
        padding: f.fmt_padding.map(|v| v as i32),
        border: f.fmt_border.map(|v| v as i32),
        position: f.fmt_position.clone(),
        is_blockquote: f.fmt_is_blockquote,
    }
}

pub(crate) fn table_dto_to_format(t: &frontend::table::dtos::TableDto) -> crate::flow::TableFormat {
    crate::flow::TableFormat {
        border: t.fmt_border.map(|v| v as i32),
        cell_spacing: t.fmt_cell_spacing.map(|v| v as i32),
        cell_padding: t.fmt_cell_padding.map(|v| v as i32),
        width: t.fmt_width.map(|v| v as i32),
        alignment: t.fmt_alignment.clone(),
    }
}

pub(crate) fn cell_dto_to_format(
    c: &frontend::table_cell::dtos::TableCellDto,
) -> crate::flow::CellFormat {
    use frontend::common::entities::CellVerticalAlignment as BackendCVA;
    crate::flow::CellFormat {
        padding: c.fmt_padding.map(|v| v as i32),
        border: c.fmt_border.map(|v| v as i32),
        vertical_alignment: c.fmt_vertical_alignment.as_ref().map(|v| match v {
            BackendCVA::Top => crate::flow::CellVerticalAlignment::Top,
            BackendCVA::Middle => crate::flow::CellVerticalAlignment::Middle,
            BackendCVA::Bottom => crate::flow::CellVerticalAlignment::Bottom,
        }),
        background_color: c.fmt_background_color.clone(),
    }
}
