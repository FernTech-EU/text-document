//! Read-only table and table cell handles.

use std::sync::Arc;

use parking_lot::Mutex;

use frontend::commands::{block_commands, frame_commands, table_cell_commands, table_commands};
use frontend::common::types::EntityId;

use crate::convert::to_usize;
use crate::flow::{BlockSnapshot, CellFormat, TableFormat, TableSnapshot};
use crate::inner::TextDocumentInner;
use crate::text_block::TextBlock;
use crate::text_frame::{cell_dto_to_format, table_dto_to_format};

/// A read-only handle to a table in the document.
///
/// Obtained from [`FlowElement::Table`](crate::FlowElement::Table) during flow traversal.
#[derive(Clone)]
pub struct TextTable {
    pub(crate) doc: Arc<Mutex<TextDocumentInner>>,
    pub(crate) table_id: usize,
}

impl TextTable {
    /// Stable entity ID. O(1).
    pub fn id(&self) -> usize {
        self.table_id
    }

    /// Number of rows. O(1).
    pub fn rows(&self) -> usize {
        let inner = self.doc.lock();
        table_commands::get_table(&inner.ctx, &(self.table_id as u64))
            .ok()
            .flatten()
            .map(|t| to_usize(t.rows))
            .unwrap_or(0)
    }

    /// Number of columns. O(1).
    pub fn columns(&self) -> usize {
        let inner = self.doc.lock();
        table_commands::get_table(&inner.ctx, &(self.table_id as u64))
            .ok()
            .flatten()
            .map(|t| to_usize(t.columns))
            .unwrap_or(0)
    }

    /// Cell at grid position. O(c) where c = total cells.
    pub fn cell(&self, row: usize, col: usize) -> Option<TextTableCell> {
        let inner = self.doc.lock();
        let table_dto = table_commands::get_table(&inner.ctx, &(self.table_id as u64))
            .ok()
            .flatten()?;

        for &cell_id in &table_dto.cells {
            if let Some(cell_dto) = table_cell_commands::get_table_cell(&inner.ctx, &{ cell_id })
                .ok()
                .flatten()
                && cell_dto.row as usize == row
                && cell_dto.column as usize == col
            {
                return Some(TextTableCell {
                    doc: Arc::clone(&self.doc),
                    cell_id: cell_dto.id as usize,
                });
            }
        }
        None
    }

    /// Column widths. O(1).
    pub fn column_widths(&self) -> Vec<i32> {
        let inner = self.doc.lock();
        table_commands::get_table(&inner.ctx, &(self.table_id as u64))
            .ok()
            .flatten()
            .map(|t| t.column_widths.iter().map(|&v| v as i32).collect())
            .unwrap_or_default()
    }

    /// Table-level formatting. O(1).
    pub fn format(&self) -> TableFormat {
        let inner = self.doc.lock();
        table_commands::get_table(&inner.ctx, &(self.table_id as u64))
            .ok()
            .flatten()
            .map(|t| table_dto_to_format(&t))
            .unwrap_or_default()
    }

    /// All cells with block snapshots. O(c·k).
    pub fn snapshot(&self) -> TableSnapshot {
        let inner = self.doc.lock();
        crate::text_frame::build_table_snapshot(
            &inner,
            self.table_id as u64,
            crate::highlight::SnapshotHighlights {
                kind: inner.highlight_kind,
                mask: &crate::highlight::HighlightMask::ALL,
            },
        )
        .unwrap_or_else(|| TableSnapshot {
            table_id: self.table_id,
            rows: 0,
            columns: 0,
            column_widths: Vec::new(),
            format: TableFormat::default(),
            cells: Vec::new(),
        })
    }
}

/// A read-only handle to a single cell within a table.
#[derive(Clone)]
pub struct TextTableCell {
    pub(crate) doc: Arc<Mutex<TextDocumentInner>>,
    pub(crate) cell_id: usize,
}

impl TextTableCell {
    /// Stable entity ID. O(1).
    pub fn id(&self) -> usize {
        self.cell_id
    }

    /// Cell row index. O(1).
    pub fn row(&self) -> usize {
        let inner = self.doc.lock();
        table_cell_commands::get_table_cell(&inner.ctx, &(self.cell_id as u64))
            .ok()
            .flatten()
            .map(|c| to_usize(c.row))
            .unwrap_or(0)
    }

    /// Cell column index. O(1).
    pub fn column(&self) -> usize {
        let inner = self.doc.lock();
        table_cell_commands::get_table_cell(&inner.ctx, &(self.cell_id as u64))
            .ok()
            .flatten()
            .map(|c| to_usize(c.column))
            .unwrap_or(0)
    }

    /// Row span. O(1).
    pub fn row_span(&self) -> usize {
        let inner = self.doc.lock();
        table_cell_commands::get_table_cell(&inner.ctx, &(self.cell_id as u64))
            .ok()
            .flatten()
            .map(|c| to_usize(c.row_span))
            .unwrap_or(1)
    }

    /// Column span. O(1).
    pub fn column_span(&self) -> usize {
        let inner = self.doc.lock();
        table_cell_commands::get_table_cell(&inner.ctx, &(self.cell_id as u64))
            .ok()
            .flatten()
            .map(|c| to_usize(c.column_span))
            .unwrap_or(1)
    }

    /// Cell-level formatting. O(1).
    pub fn format(&self) -> CellFormat {
        let inner = self.doc.lock();
        table_cell_commands::get_table_cell(&inner.ctx, &(self.cell_id as u64))
            .ok()
            .flatten()
            .map(|c| cell_dto_to_format(&c))
            .unwrap_or_default()
    }

    /// Blocks within this cell's frame. Returns empty `Vec` if `cell_frame` is `None`.
    pub fn blocks(&self) -> Vec<TextBlock> {
        let inner = self.doc.lock();
        let cell_dto = match table_cell_commands::get_table_cell(&inner.ctx, &(self.cell_id as u64))
            .ok()
            .flatten()
        {
            Some(c) => c,
            None => return Vec::new(),
        };

        let cell_frame_id = match cell_dto.cell_frame {
            Some(id) => id,
            None => return Vec::new(),
        };

        let frame_dto = match frame_commands::get_frame(&inner.ctx, &(cell_frame_id as EntityId))
            .ok()
            .flatten()
        {
            Some(f) => f,
            None => return Vec::new(),
        };

        // Get blocks sorted by document_position
        let mut block_dtos: Vec<_> = frame_dto
            .blocks
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

        block_dtos
            .iter()
            .map(|b| TextBlock {
                doc: Arc::clone(&self.doc),
                block_id: b.id as usize,
            })
            .collect()
    }

    /// Snapshot all cell blocks in one lock. Returns empty `Vec` if `cell_frame` is `None`.
    pub fn snapshot_blocks(&self) -> Vec<BlockSnapshot> {
        let inner = self.doc.lock();
        let cell_dto = match table_cell_commands::get_table_cell(&inner.ctx, &(self.cell_id as u64))
            .ok()
            .flatten()
        {
            Some(c) => c,
            None => return Vec::new(),
        };

        match cell_dto.cell_frame {
            Some(frame_id) => crate::text_block::build_blocks_snapshot_for_frame(
                &inner,
                frame_id,
                crate::highlight::SnapshotHighlights {
                    kind: inner.highlight_kind,
                    mask: &crate::highlight::HighlightMask::ALL,
                },
            ),
            None => Vec::new(),
        }
    }
}
