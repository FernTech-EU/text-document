//! TextCursor implementation — Qt-style multi-cursor with automatic position adjustment.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::{DocumentError, Result};

use crate::ListStyle;
use frontend::commands::{
    document_editing_commands, document_formatting_commands, document_inspection_commands,
    undo_redo_commands,
};

use unicode_segmentation::UnicodeSegmentation;

use crate::convert::{to_i64, to_usize};
use crate::events::DocumentEvent;
use crate::flow::{CellRange, FlowElement, FrameRef, SelectionKind, TableCellRef};
use crate::fragment::DocumentFragment;
use crate::inner::{CursorData, QueuedEvents, TextDocumentInner};
use crate::text_table::TextTable;
use crate::{BlockFormat, FrameFormat, MoveMode, MoveOperation, SelectionType, TextFormat};

use crate::document::get_main_frame_id;

/// Compute the maximum valid cursor position from document stats.
///
/// Cursor positions include block separators (one between each pair of adjacent
/// blocks), but `character_count` does not. The max position is therefore
/// `character_count + (block_count - 1)`.
fn max_cursor_position(stats: &frontend::document_inspection::DocumentStatsDto) -> usize {
    let chars = to_usize(stats.character_count);
    let blocks = to_usize(stats.block_count);
    if blocks > 1 {
        chars + blocks - 1
    } else {
        chars
    }
}

/// A cursor into a [`TextDocument`](crate::TextDocument).
///
/// Multiple cursors can coexist on the same document (like Qt's `QTextCursor`).
/// When any cursor edits text, all other cursors' positions are automatically
/// adjusted by the document.
///
/// Cloning a cursor creates an **independent** cursor at the same position.
pub struct TextCursor {
    pub(crate) doc: Arc<Mutex<TextDocumentInner>>,
    pub(crate) data: Arc<Mutex<CursorData>>,
}

impl Clone for TextCursor {
    fn clone(&self) -> Self {
        let (position, anchor) = {
            let d = self.data.lock();
            (d.position, d.anchor)
        };
        let data = {
            let mut inner = self.doc.lock();
            let data = Arc::new(Mutex::new(CursorData {
                position,
                anchor,
                cell_selection_override: None,
            }));
            inner.cursors.push(Arc::downgrade(&data));
            data
        };
        TextCursor {
            doc: self.doc.clone(),
            data,
        }
    }
}

impl TextCursor {
    // ── Helpers (called while doc lock is NOT held) ──────────

    fn read_cursor(&self) -> (usize, usize) {
        let d = self.data.lock();
        (d.position, d.anchor)
    }

    /// Common post-edit bookkeeping: adjust all cursors, set this cursor to
    /// `new_pos`, mark modified, invalidate text cache, queue a
    /// `ContentsChanged` event, and return the queued events for dispatch.
    fn finish_edit(
        &self,
        inner: &mut TextDocumentInner,
        edit_pos: usize,
        removed: usize,
        new_pos: usize,
        blocks_affected: usize,
    ) -> QueuedEvents {
        self.finish_edit_ext(inner, edit_pos, removed, new_pos, blocks_affected, true)
    }

    fn finish_edit_ext(
        &self,
        inner: &mut TextDocumentInner,
        edit_pos: usize,
        removed: usize,
        new_pos: usize,
        blocks_affected: usize,
        flow_may_change: bool,
    ) -> QueuedEvents {
        // Defensive: a use case can return new_position < edit_pos when
        // invoked through a stale or out-of-range cursor (e.g. after an
        // undo restores a state where the previously-saved cursor position
        // is no longer valid — fuzz finds this). Treat the edit as adding
        // 0 chars rather than overflowing; the cursor still moves to
        // `new_pos` below.
        let added = new_pos.saturating_sub(edit_pos);
        inner.adjust_cursors(edit_pos, removed, added);
        {
            let mut d = self.data.lock();
            d.position = new_pos;
            d.anchor = new_pos;
        }
        inner.modified = true;
        inner.invalidate_text_cache();
        inner.rehighlight_affected(edit_pos);
        inner.queue_event(DocumentEvent::ContentsChanged {
            position: edit_pos,
            chars_removed: removed,
            chars_added: added,
            blocks_affected,
        });
        inner.check_block_count_changed();
        if flow_may_change {
            inner.check_flow_changed();
        }
        self.queue_undo_redo_event(inner)
    }

    // ── Position & selection ─────────────────────────────────

    /// Current cursor position (between characters).
    pub fn position(&self) -> usize {
        self.data.lock().position
    }

    /// Anchor position. Equal to `position()` when no selection.
    pub fn anchor(&self) -> usize {
        self.data.lock().anchor
    }

    /// Returns true if there is a selection.
    pub fn has_selection(&self) -> bool {
        let d = self.data.lock();
        d.position != d.anchor
    }

    /// Start of the selection (min of position and anchor).
    pub fn selection_start(&self) -> usize {
        let d = self.data.lock();
        d.position.min(d.anchor)
    }

    /// End of the selection (max of position and anchor).
    pub fn selection_end(&self) -> usize {
        let d = self.data.lock();
        d.position.max(d.anchor)
    }

    /// Get the selected text. Returns empty string if no selection.
    pub fn selected_text(&self) -> Result<String> {
        let (pos, anchor) = self.read_cursor();
        if pos == anchor {
            return Ok(String::new());
        }
        let start = pos.min(anchor);
        let len = pos.max(anchor) - start;
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetTextAtPositionDto {
            position: to_i64(start),
            length: to_i64(len),
        };
        let result = document_inspection_commands::get_text_at_position(&inner.ctx, &dto)?;
        Ok(result.text)
    }

    /// Up to `max_len` characters of plain text immediately preceding the cursor position,
    /// bounded and read directly from the store — the same fast block lookup
    /// [`char_format`](Self::char_format) uses, never materializing the whole document the
    /// way [`selected_text`](Self::selected_text)'s
    /// `document_inspection_commands::get_text_at_position` does internally. Block
    /// boundaries appear as `'\n'`. Returns fewer than `max_len` characters near the start of
    /// the document.
    ///
    /// Falls back to the (correct, but O(document size)) slow path for documents containing
    /// tables or unmirrored sub-frames, where the fast rope-index lookup doesn't apply — see
    /// [`common::database::rope_helpers::find_block_at_char_position`]'s own doc comment for
    /// exactly which documents that is.
    pub fn text_before(&self, max_len: usize) -> Result<String> {
        if max_len == 0 {
            return Ok(String::new());
        }
        let pos = self.position();
        let inner = self.doc.lock();
        let store = inner.ctx.db_context.get_store();

        // One O(log n) probe decides whether the fast path even applies to this document —
        // same gate `find_block_at_char_position` itself uses (tables / unmirrored
        // sub-frames disqualify it). Cheaper to check once up front than to discover it mid
        // walk.
        if pos > 0 && common::database::rope_helpers::find_block_at_char_position(store, 0).is_none()
        {
            let dto = frontend::document_inspection::GetTextAtPositionDto {
                position: 0,
                length: to_i64(pos),
            };
            let result = document_inspection_commands::get_text_at_position(&inner.ctx, &dto)?;
            let full = result.text;
            let total = full.chars().count();
            let skip = total.saturating_sub(max_len);
            return Ok(full.chars().skip(skip).collect());
        }

        let mut pieces: Vec<String> = Vec::new();
        let mut remaining = max_len;
        let mut end_pos = pos;

        while remaining > 0 && end_pos > 0 {
            let query = (end_pos - 1) as i64;
            // `find_block_at_char_position` returns the *previous*-block answer at a
            // boundary (char_in_block == the block's own length), which is exactly the
            // convention this backward walk needs — unlike `get_block_at_position`'s command,
            // which deliberately advances to the *next* block at a boundary for its own
            // (forward/click-mapping) callers.
            let Some((block_id, char_in_block, block_char_start)) =
                common::database::rope_helpers::find_block_at_char_position(store, query)
            else {
                // A table or sub-frame appeared partway through the walk (shouldn't happen
                // given the up-front check above, but the primitive's contract only promises
                // this per-call, not for the whole document) — stop rather than guess.
                break;
            };
            let block_dto = frontend::commands::block_commands::get_block(&inner.ctx, &block_id)?
                .ok_or_else(|| DocumentError::NotFound("block not found".into()))?;
            let entity: common::entities::Block = block_dto.into();
            let block_text = common::database::rope_helpers::block_content_via_store(&entity, store);
            let block_len = block_text.chars().count() as i64;
            let block_char_start = block_char_start as usize;

            if char_in_block == block_len {
                // `query` landed exactly on this (non-empty) block's own trailing separator.
                pieces.push("\n".to_string());
                remaining -= 1;
                if remaining == 0 {
                    break;
                }
                let take = remaining.min(block_len as usize);
                let local_start = block_len as usize - take;
                let slice: String = block_text.chars().skip(local_start).take(take).collect();
                pieces.push(slice);
                remaining -= take;
                end_pos = block_char_start + local_start;
            } else {
                // `query` is a real character at local index `char_in_block`.
                let available = char_in_block as usize + 1;
                let take = remaining.min(available);
                let local_start = available - take;
                let slice: String = block_text.chars().skip(local_start).take(take).collect();
                pieces.push(slice);
                remaining -= take;
                end_pos = block_char_start + local_start;
            }
        }

        pieces.reverse();
        Ok(pieces.concat())
    }

    /// Collapse the selection by moving anchor to position.
    pub fn clear_selection(&self) {
        let mut d = self.data.lock();
        d.anchor = d.position;
    }

    // ── Boundary queries ─────────────────────────────────────

    /// True if the cursor is at the start of a block.
    pub fn at_block_start(&self) -> bool {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        if let Ok(info) = document_inspection_commands::get_block_at_position(&inner.ctx, &dto) {
            pos == to_usize(info.block_start)
        } else {
            false
        }
    }

    /// True if the cursor is at the end of a block.
    pub fn at_block_end(&self) -> bool {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        if let Ok(info) = document_inspection_commands::get_block_at_position(&inner.ctx, &dto) {
            pos == to_usize(info.block_start) + to_usize(info.block_length)
        } else {
            false
        }
    }

    /// True if the cursor is at position 0.
    pub fn at_start(&self) -> bool {
        self.data.lock().position == 0
    }

    /// True if the cursor is at the very end of the document.
    pub fn at_end(&self) -> bool {
        let pos = self.position();
        let inner = self.doc.lock();
        let stats = document_inspection_commands::get_document_stats(&inner.ctx).unwrap_or({
            frontend::document_inspection::DocumentStatsDto {
                character_count: 0,
                word_count: 0,
                block_count: 0,
                frame_count: 0,
                image_count: 0,
                list_count: 0,
                table_count: 0,
            }
        });
        pos >= max_cursor_position(&stats)
    }

    /// The block number (0-indexed) containing the cursor.
    pub fn block_number(&self) -> usize {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        document_inspection_commands::get_block_at_position(&inner.ctx, &dto)
            .map(|info| to_usize(info.block_number))
            .unwrap_or(0)
    }

    /// The cursor's column within the current block (0-indexed).
    pub fn position_in_block(&self) -> usize {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        document_inspection_commands::get_block_at_position(&inner.ctx, &dto)
            .map(|info| pos.saturating_sub(to_usize(info.block_start)))
            .unwrap_or(0)
    }

    // ── Movement ─────────────────────────────────────────────

    /// Set the cursor to an absolute position.
    ///
    /// When extending a selection (`KeepAnchor`) across a table boundary,
    /// the position is snapped to the adjacent block outside the table so
    /// the entire table is "trapped" inside the selection range. This
    /// mirrors LibreOffice's behaviour: partial table selections from
    /// outside are not allowed; the table is always fully enclosed.
    ///
    /// The snap is skipped when:
    /// - `mode` is `MoveAnchor` (plain click / move without selection)
    /// - No adjacent block exists (table is first or last in the document)
    pub fn set_position(&self, position: usize, mode: MoveMode) {
        // Clamp to max document position (includes block separators)
        let end = {
            let inner = self.doc.lock();
            document_inspection_commands::get_document_stats(&inner.ctx)
                .map(|s| max_cursor_position(&s))
                .unwrap_or(0)
        };
        let mut pos = position.min(end);

        // Table-trap snap: when extending a selection, if one endpoint is
        // inside a table and the other is outside, relocate the inside
        // endpoint to the boundary of the adjacent block.
        if mode == MoveMode::KeepAnchor {
            let anchor = self.data.lock().anchor;
            let pos_cell = self.table_cell_at(pos);
            let anchor_cell = self.table_cell_at(anchor);
            match (&pos_cell, &anchor_cell) {
                (Some(tc), None) => {
                    // Position is inside a table, anchor is outside.
                    let before = anchor < pos;
                    if let Some(boundary) = self.table_boundary_position(tc.table.id(), !before) {
                        pos = boundary;
                    }
                }
                (None, Some(tc)) => {
                    // Anchor is inside a table, position is outside.
                    // Snap the position so the table is enclosed.
                    let before = pos < anchor;
                    if let Some(boundary) = self.table_boundary_position(tc.table.id(), !before) {
                        pos = boundary;
                    }
                }
                _ => {}
            }
        }

        {
            let mut d = self.data.lock();
            d.position = pos;
            if mode == MoveMode::MoveAnchor {
                d.anchor = pos;
            }
            d.cell_selection_override = None;
        }
        // Snap forward to the nearest grapheme cluster boundary so
        // a caller passing an arbitrary scalar index (e.g. computed
        // from a hit-test or a plain-text search) never leaves the
        // cursor inside a multi-scalar grapheme cluster.
        self.snap_position_to_grapheme_boundary();
    }

    /// Move the cursor by a semantic operation.
    ///
    /// `n` is used as a repeat count for character-level movements
    /// (`NextCharacter`, `PreviousCharacter`, `Left`, `Right`).
    /// For all other operations it is ignored. Returns `true` if the cursor moved.
    pub fn move_position(&self, operation: MoveOperation, mode: MoveMode, n: usize) -> bool {
        let old_pos = self.position();
        let target = self.resolve_move(operation, n);
        self.set_position(target, mode);
        self.position() != old_pos
    }

    /// Select a region relative to the cursor position.
    pub fn select(&self, selection: SelectionType) {
        match selection {
            SelectionType::Document => {
                let end = {
                    let inner = self.doc.lock();
                    document_inspection_commands::get_document_stats(&inner.ctx)
                        .map(|s| max_cursor_position(&s))
                        .unwrap_or(0)
                };
                let mut d = self.data.lock();
                d.anchor = 0;
                d.position = end;
                d.cell_selection_override = None;
            }
            SelectionType::BlockUnderCursor | SelectionType::LineUnderCursor => {
                let pos = self.position();
                let inner = self.doc.lock();
                let dto = frontend::document_inspection::GetBlockAtPositionDto {
                    position: to_i64(pos),
                };
                if let Ok(info) =
                    document_inspection_commands::get_block_at_position(&inner.ctx, &dto)
                {
                    let start = to_usize(info.block_start);
                    let end = start + to_usize(info.block_length);
                    drop(inner);
                    let mut d = self.data.lock();
                    d.anchor = start;
                    d.position = end;
                    d.cell_selection_override = None;
                }
            }
            SelectionType::WordUnderCursor => {
                let pos = self.position();
                let (word_start, word_end) = self.find_word_boundaries(pos);
                let mut d = self.data.lock();
                d.anchor = word_start;
                d.position = word_end;
                d.cell_selection_override = None;
            }
        }
    }

    // ── Text editing ─────────────────────────────────────────

    /// Insert plain text at the cursor. Replaces selection if any.
    pub fn insert_text(&self, text: &str) -> Result<()> {
        let (pos, anchor) = self.read_cursor();

        // Try direct insert first (handles same-block selection and no-selection cases)
        let dto = frontend::document_editing::InsertTextDto {
            format_policy: Default::default(),
            position: to_i64(pos),
            anchor: to_i64(anchor),
            text: text.into(),
        };

        let queued = {
            let mut inner = self.doc.lock();
            let result = match document_editing_commands::insert_text(
                &inner.ctx,
                Some(inner.stack_id),
                &dto,
            ) {
                Ok(r) => r,
                Err(_) if pos != anchor => {
                    // Cross-block selection: compose delete + insert as a single undo unit
                    undo_redo_commands::begin_composite(&inner.ctx, Some(inner.stack_id));

                    let del_dto = frontend::document_editing::DeleteTextDto {
                        position: to_i64(pos),
                        anchor: to_i64(anchor),
                    };
                    let del_result = document_editing_commands::delete_text(
                        &inner.ctx,
                        Some(inner.stack_id),
                        &del_dto,
                    )?;
                    let del_pos = to_usize(del_result.new_position);

                    let ins_dto = frontend::document_editing::InsertTextDto {
                        format_policy: Default::default(),
                        position: to_i64(del_pos),
                        anchor: to_i64(del_pos),
                        text: text.into(),
                    };
                    let ins_result = document_editing_commands::insert_text(
                        &inner.ctx,
                        Some(inner.stack_id),
                        &ins_dto,
                    )?;

                    undo_redo_commands::end_composite(&inner.ctx);
                    ins_result
                }
                Err(e) => return Err(e.into()),
            };

            let edit_pos = pos.min(anchor);
            let removed = pos.max(anchor) - edit_pos;
            self.finish_edit_ext(
                &mut inner,
                edit_pos,
                removed,
                to_usize(result.new_position),
                to_usize(result.blocks_affected),
                false,
            )
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Replace `[start, end)` with `text`, choosing what the replacement wears where it
    /// overwrites formatted text — see [`crate::ReplaceFormatPolicy`]. Lands as one atomic
    /// edit (one undo entry), with the cursor left at the end of the inserted text.
    ///
    /// The counterpart to [`insert_text`](Self::insert_text) for callers that must *choose*
    /// the replacement's formatting rather than inherit whatever precedes the range — a
    /// spell-check correction picked from a context menu, an autocorrect, a
    /// replace-this-occurrence action. `start`/`end` may be given in either order.
    ///
    /// A range crossing a block boundary falls back to composing delete + insert as a single
    /// undo unit, exactly like [`insert_text`](Self::insert_text)'s existing cross-block
    /// fallback — `format_policy` only has meaning within one block, since
    /// [`crate::ReplaceFormatPolicy`] operates on a single block's format runs.
    pub fn replace(
        &self,
        start: usize,
        end: usize,
        text: &str,
        policy: crate::ReplaceFormatPolicy,
    ) -> Result<()> {
        let (pos, anchor) = (start, end);

        let dto = frontend::document_editing::InsertTextDto {
            format_policy: policy,
            position: to_i64(pos),
            anchor: to_i64(anchor),
            text: text.into(),
        };

        let queued = {
            let mut inner = self.doc.lock();
            let result = match document_editing_commands::insert_text(
                &inner.ctx,
                Some(inner.stack_id),
                &dto,
            ) {
                Ok(r) => r,
                Err(_) if pos != anchor => {
                    // Cross-block selection: compose delete + insert as a single undo unit,
                    // same as insert_text. format_policy is dropped here — it has no
                    // single-block meaning across a boundary.
                    undo_redo_commands::begin_composite(&inner.ctx, Some(inner.stack_id));

                    let del_dto = frontend::document_editing::DeleteTextDto {
                        position: to_i64(pos),
                        anchor: to_i64(anchor),
                    };
                    let del_result = document_editing_commands::delete_text(
                        &inner.ctx,
                        Some(inner.stack_id),
                        &del_dto,
                    )?;
                    let del_pos = to_usize(del_result.new_position);

                    let ins_dto = frontend::document_editing::InsertTextDto {
                        format_policy: Default::default(),
                        position: to_i64(del_pos),
                        anchor: to_i64(del_pos),
                        text: text.into(),
                    };
                    let ins_result = document_editing_commands::insert_text(
                        &inner.ctx,
                        Some(inner.stack_id),
                        &ins_dto,
                    )?;

                    undo_redo_commands::end_composite(&inner.ctx);
                    ins_result
                }
                Err(e) => return Err(e.into()),
            };

            let edit_pos = pos.min(anchor);
            let removed = pos.max(anchor) - edit_pos;
            self.finish_edit_ext(
                &mut inner,
                edit_pos,
                removed,
                to_usize(result.new_position),
                to_usize(result.blocks_affected),
                false,
            )
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Insert text with a specific character format. Replaces selection if any.
    pub fn insert_formatted_text(&self, text: &str, format: &TextFormat) -> Result<()> {
        let (pos, anchor) = self.read_cursor();

        let make_dto = |p: usize, a: usize| frontend::document_editing::InsertFormattedTextDto {
            position: to_i64(p),
            anchor: to_i64(a),
            text: text.into(),
            font_family: format.font_family.clone().unwrap_or_default(),
            font_point_size: format.font_point_size.map(|v| v as i64).unwrap_or(0),
            font_bold: format.font_bold.unwrap_or(false),
            font_italic: format.font_italic.unwrap_or(false),
            font_underline: format.font_underline.unwrap_or(false),
            font_strikeout: format.font_strikeout.unwrap_or(false),
        };

        let queued = {
            let mut inner = self.doc.lock();
            let result = match document_editing_commands::insert_formatted_text(
                &inner.ctx,
                Some(inner.stack_id),
                &make_dto(pos, anchor),
            ) {
                Ok(r) => r,
                Err(_) if pos != anchor => {
                    // Cross-block selection: compose delete + insert as a single undo unit
                    undo_redo_commands::begin_composite(&inner.ctx, Some(inner.stack_id));

                    let del_dto = frontend::document_editing::DeleteTextDto {
                        position: to_i64(pos),
                        anchor: to_i64(anchor),
                    };
                    let del_result = document_editing_commands::delete_text(
                        &inner.ctx,
                        Some(inner.stack_id),
                        &del_dto,
                    )?;
                    let del_pos = to_usize(del_result.new_position);

                    let ins_result = document_editing_commands::insert_formatted_text(
                        &inner.ctx,
                        Some(inner.stack_id),
                        &make_dto(del_pos, del_pos),
                    )?;

                    undo_redo_commands::end_composite(&inner.ctx);
                    ins_result
                }
                Err(e) => return Err(e.into()),
            };

            let edit_pos = pos.min(anchor);
            let removed = pos.max(anchor) - edit_pos;
            self.finish_edit_ext(
                &mut inner,
                edit_pos,
                removed,
                to_usize(result.new_position),
                1,
                false,
            )
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Insert a block break (new paragraph). Replaces selection if any.
    pub fn insert_block(&self) -> Result<()> {
        let (pos, anchor) = self.read_cursor();
        let queued = {
            let mut inner = self.doc.lock();

            let (insert_pos, removed) = if pos != anchor {
                // Selection active: delete first, then split (Word convention)
                undo_redo_commands::begin_composite(&inner.ctx, Some(inner.stack_id));
                let del_dto = frontend::document_editing::DeleteTextDto {
                    position: to_i64(pos),
                    anchor: to_i64(anchor),
                };
                let del_result = document_editing_commands::delete_text(
                    &inner.ctx,
                    Some(inner.stack_id),
                    &del_dto,
                )?;
                (
                    to_usize(del_result.new_position),
                    pos.max(anchor) - pos.min(anchor),
                )
            } else {
                (pos, 0)
            };

            let dto = frontend::document_editing::InsertBlockDto {
                position: to_i64(insert_pos),
                anchor: to_i64(insert_pos),
            };
            let result =
                document_editing_commands::insert_block(&inner.ctx, Some(inner.stack_id), &dto)?;

            if pos != anchor {
                undo_redo_commands::end_composite(&inner.ctx);
            }

            let edit_pos = pos.min(anchor);
            self.finish_edit(
                &mut inner,
                edit_pos,
                removed,
                to_usize(result.new_position),
                2,
            )
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Insert an HTML fragment at the cursor position. Replaces selection if any.
    pub fn insert_html(&self, html: &str) -> Result<()> {
        // Delegate to insert_fragment so table structure is preserved.
        let frag = DocumentFragment::from_html(html);
        self.insert_fragment(&frag)
    }

    /// Insert a Markdown fragment at the cursor position. Replaces selection if any.
    pub fn insert_markdown(&self, markdown: &str) -> Result<()> {
        let frag = DocumentFragment::from_markdown(markdown);
        self.insert_fragment(&frag)
    }

    /// Insert a djot fragment at the cursor position. Replaces selection if any.
    pub fn insert_djot(&self, djot: &str) -> Result<()> {
        let frag = DocumentFragment::from_djot(djot);
        self.insert_fragment(&frag)
    }

    /// Insert a document fragment at the cursor. Replaces selection if any.
    pub fn insert_fragment(&self, fragment: &DocumentFragment) -> Result<()> {
        let (pos, anchor) = self.read_cursor();
        let queued = {
            let mut inner = self.doc.lock();

            let (insert_pos, removed) = if pos != anchor {
                undo_redo_commands::begin_composite(&inner.ctx, Some(inner.stack_id));
                let del_dto = frontend::document_editing::DeleteTextDto {
                    position: to_i64(pos),
                    anchor: to_i64(anchor),
                };
                let del_result = document_editing_commands::delete_text(
                    &inner.ctx,
                    Some(inner.stack_id),
                    &del_dto,
                )?;
                (
                    to_usize(del_result.new_position),
                    pos.max(anchor) - pos.min(anchor),
                )
            } else {
                (pos, 0)
            };

            let dto = frontend::document_editing::InsertFragmentDto {
                position: to_i64(insert_pos),
                anchor: to_i64(insert_pos),
                fragment_data: fragment.raw_data().into(),
            };
            let result =
                document_editing_commands::insert_fragment(&inner.ctx, Some(inner.stack_id), &dto)?;

            if pos != anchor {
                undo_redo_commands::end_composite(&inner.ctx);
            }

            let edit_pos = pos.min(anchor);
            self.finish_edit(
                &mut inner,
                edit_pos,
                removed,
                to_usize(result.new_position),
                to_usize(result.blocks_added),
            )
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Extract the current selection as a [`DocumentFragment`].
    pub fn selection(&self) -> DocumentFragment {
        let (pos, anchor) = self.read_cursor();

        // For cell/mixed selections, compute position/anchor that span the
        // full cell range so ExtractFragment detects cross-cell correctly.
        let (extract_pos, extract_anchor) = match self.selection_kind() {
            SelectionKind::Cells(ref range) => match self.cell_range_positions(range) {
                Some((start, end)) => (start, end),
                None => return DocumentFragment::new(),
            },
            SelectionKind::Mixed {
                ref cell_range,
                text_before,
                text_after,
            } => {
                let (cell_start, cell_end) = match self.cell_range_positions(cell_range) {
                    Some(p) => p,
                    None => return DocumentFragment::new(),
                };
                let start = if text_before {
                    pos.min(anchor)
                } else {
                    cell_start
                };
                let end = if text_after {
                    pos.max(anchor)
                } else {
                    cell_end
                };
                (start.min(cell_start), end.max(cell_end))
            }
            SelectionKind::None => return DocumentFragment::new(),
            SelectionKind::Text => (pos, anchor),
        };

        if extract_pos == extract_anchor {
            return DocumentFragment::new();
        }

        let inner = self.doc.lock();
        let dto = frontend::document_inspection::ExtractFragmentDto {
            position: to_i64(extract_pos),
            anchor: to_i64(extract_anchor),
        };
        match document_inspection_commands::extract_fragment(&inner.ctx, &dto) {
            Ok(result) => DocumentFragment::from_raw(result.fragment_data, result.plain_text),
            Err(_) => DocumentFragment::new(),
        }
    }

    /// Insert an image at the cursor. Replaces selection if any.
    pub fn insert_image(&self, name: &str, width: u32, height: u32) -> Result<()> {
        let (pos, anchor) = self.read_cursor();
        let queued = {
            let mut inner = self.doc.lock();

            let (insert_pos, removed) = if pos != anchor {
                undo_redo_commands::begin_composite(&inner.ctx, Some(inner.stack_id));
                let del_dto = frontend::document_editing::DeleteTextDto {
                    position: to_i64(pos),
                    anchor: to_i64(anchor),
                };
                let del_result = document_editing_commands::delete_text(
                    &inner.ctx,
                    Some(inner.stack_id),
                    &del_dto,
                )?;
                (
                    to_usize(del_result.new_position),
                    pos.max(anchor) - pos.min(anchor),
                )
            } else {
                (pos, 0)
            };

            let dto = frontend::document_editing::InsertImageDto {
                position: to_i64(insert_pos),
                anchor: to_i64(insert_pos),
                image_name: name.into(),
                width: width as i64,
                height: height as i64,
            };
            let result =
                document_editing_commands::insert_image(&inner.ctx, Some(inner.stack_id), &dto)?;

            if pos != anchor {
                undo_redo_commands::end_composite(&inner.ctx);
            }

            let edit_pos = pos.min(anchor);
            self.finish_edit_ext(
                &mut inner,
                edit_pos,
                removed,
                to_usize(result.new_position),
                1,
                false,
            )
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Insert a new frame at the cursor.
    pub fn insert_frame(&self) -> Result<()> {
        let (pos, anchor) = self.read_cursor();
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::InsertFrameDto {
                position: to_i64(pos),
                anchor: to_i64(anchor),
            };
            document_editing_commands::insert_frame(&inner.ctx, Some(inner.stack_id), &dto)?;
            // Frame insertion adds structural content; adjust cursors and emit event.
            // The backend doesn't return a new_position, so the cursor stays put.
            inner.modified = true;
            inner.invalidate_text_cache();
            inner.rehighlight_affected(pos.min(anchor));
            inner.queue_event(DocumentEvent::ContentsChanged {
                position: pos.min(anchor),
                chars_removed: 0,
                chars_added: 0,
                blocks_affected: 1,
            });
            inner.check_block_count_changed();
            inner.check_flow_changed();
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Insert a table at the cursor position.
    ///
    /// Creates a `rows × columns` table with empty cells.
    /// The cursor moves into the first cell of the table.
    /// Returns a handle to the created table.
    pub fn insert_table(&self, rows: usize, columns: usize) -> Result<TextTable> {
        let (pos, anchor) = self.read_cursor();
        let (table_id, queued) = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::InsertTableDto {
                position: to_i64(pos),
                anchor: to_i64(anchor),
                rows: to_i64(rows),
                columns: to_i64(columns),
            };
            let result =
                document_editing_commands::insert_table(&inner.ctx, Some(inner.stack_id), &dto)?;
            let new_pos = to_usize(result.new_position);
            let table_id = to_usize(result.table_id);
            inner.adjust_cursors(pos.min(anchor), 0, new_pos - pos.min(anchor));
            {
                let mut d = self.data.lock();
                d.position = new_pos;
                d.anchor = new_pos;
            }
            inner.modified = true;
            inner.invalidate_text_cache();
            inner.rehighlight_affected(pos.min(anchor));
            inner.queue_event(DocumentEvent::ContentsChanged {
                position: pos.min(anchor),
                chars_removed: 0,
                chars_added: new_pos - pos.min(anchor),
                blocks_affected: 1,
            });
            inner.check_block_count_changed();
            inner.check_flow_changed();
            (table_id, self.queue_undo_redo_event(&mut inner))
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(TextTable {
            doc: self.doc.clone(),
            table_id,
        })
    }

    /// Returns the table the cursor is currently inside, if any.
    ///
    /// Returns `None` if the cursor is in the main document flow
    /// (not inside a table cell).
    pub fn current_table(&self) -> Option<TextTable> {
        self.current_table_cell().map(|c| c.table)
    }

    /// Returns the table cell the cursor is currently inside, if any.
    ///
    /// Returns `None` if the cursor is not inside a table cell.
    /// When `Some`, provides the table, row, and column.
    pub fn current_table_cell(&self) -> Option<TableCellRef> {
        let pos = self.position();
        let inner = self.doc.lock();
        // Find the block at cursor position
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let block_info =
            document_inspection_commands::get_block_at_position(&inner.ctx, &dto).ok()?;

        // When position < block_start, the cursor sits on the separator between
        // the previous block and this one. Visually the cursor belongs to the
        // end of the previous block, so look up that block instead.
        let block_id = if to_i64(pos) < block_info.block_start && pos > 0 {
            let prev_dto = frontend::document_inspection::GetBlockAtPositionDto {
                position: to_i64(pos - 1),
            };
            let prev_info =
                document_inspection_commands::get_block_at_position(&inner.ctx, &prev_dto).ok()?;
            prev_info.block_id as usize
        } else {
            block_info.block_id as usize
        };

        let block = crate::text_block::TextBlock {
            doc: self.doc.clone(),
            block_id,
        };
        // Release inner lock before calling table_cell() which also locks
        drop(inner);
        block.table_cell()
    }

    // ── Frame / blockquote queries ──────────

    /// The innermost frame enclosing the cursor's current block, or `None`
    /// if the cursor sits directly in the root frame (no enclosing
    /// sub-frame). The returned `depth` is the nesting level from the root
    /// (1 for a direct child of root, 2 for a grandchild, etc.).
    pub fn current_frame(&self) -> Option<FrameRef> {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let block_info =
            document_inspection_commands::get_block_at_position(&inner.ctx, &dto).ok()?;
        let block_id = block_info.block_id as u64;
        cursor_frame_ref(&inner, block_id)
    }

    /// True if the cursor's block lives inside any blockquote frame
    /// (at any nesting level).
    pub fn is_in_blockquote(&self) -> bool {
        self.current_blockquote_frame_id().is_some()
    }

    /// Id of the innermost blockquote frame enclosing the cursor's block,
    /// or `None` if not in a blockquote.
    pub fn current_blockquote_frame_id(&self) -> Option<usize> {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let block_info =
            document_inspection_commands::get_block_at_position(&inner.ctx, &dto).ok()?;
        innermost_blockquote_frame_id(&inner, block_info.block_id as u64)
    }

    /// Nesting depth of the cursor inside blockquote frames: 0 = not in
    /// any quote, 1 = top-level quote, 2 = quote inside a quote, …
    pub fn blockquote_depth_at_cursor(&self) -> usize {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let Some(block_info) =
            document_inspection_commands::get_block_at_position(&inner.ctx, &dto).ok()
        else {
            return 0;
        };
        blockquote_depth_for_block(&inner, block_info.block_id as u64)
    }

    /// True iff the cursor's block is the first positive entry in its
    /// owning frame's `child_order`. Used by the keyboard handler to
    /// decide whether Backspace should unwrap the enclosing frame.
    /// A single-block frame returns true for both `is_first_*` and
    /// `is_last_*`.
    pub fn is_first_block_in_current_frame(&self) -> bool {
        matches!(
            block_position_in_current_frame(self),
            Some(BlockEdge::First) | Some(BlockEdge::OnlyOne)
        )
    }

    /// True iff the cursor's block is the last positive entry in its
    /// owning frame's `child_order`. Used by the keyboard handler to
    /// decide whether forward Delete should unwrap the enclosing frame.
    pub fn is_last_block_in_current_frame(&self) -> bool {
        matches!(
            block_position_in_current_frame(self),
            Some(BlockEdge::Last) | Some(BlockEdge::OnlyOne)
        )
    }

    /// True iff the block at the cursor has no characters of content.
    /// Used by the Enter handler to decide whether to exit a blockquote.
    pub fn current_block_is_empty(&self) -> bool {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let Some(block_info) =
            document_inspection_commands::get_block_at_position(&inner.ctx, &dto).ok()
        else {
            return false;
        };
        let store = inner.ctx.db_context.get_store();
        let block_entity = store
            .blocks
            .read()
            .get(&(block_info.block_id as common::types::EntityId))
            .cloned();
        match block_entity {
            Some(b) => {
                let len = common::database::rope_helpers::block_char_length(&b, store);
                len == 0
            }
            None => false,
        }
    }

    /// True iff the cursor's anchor and head sit in different frames.
    /// Used by the toolbar to disable the "toggle blockquote" button on
    /// selections that cross frame boundaries.
    pub fn selection_spans_multiple_frames(&self) -> bool {
        let (pos, anchor) = self.read_cursor();
        if pos == anchor {
            return false;
        }
        let inner = self.doc.lock();
        let pos_dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let anchor_dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(anchor),
        };
        let Some(pos_block) =
            document_inspection_commands::get_block_at_position(&inner.ctx, &pos_dto).ok()
        else {
            return false;
        };
        let Some(anchor_block) =
            document_inspection_commands::get_block_at_position(&inner.ctx, &anchor_dto).ok()
        else {
            return false;
        };
        let pos_owner = crate::text_block::find_parent_frame(&inner, pos_block.block_id as u64);
        let anchor_owner =
            crate::text_block::find_parent_frame(&inner, anchor_block.block_id as u64);
        pos_owner != anchor_owner
    }

    // ── Blockquote mutations ──────────

    /// Wrap the current block (or the blocks in the current selection)
    /// in a new blockquote frame nested inside the cursor's current
    /// parent frame. Returns an error if the selection spans multiple
    /// frames.
    pub fn wrap_selection_in_blockquote(&self) -> Result<()> {
        if self.selection_spans_multiple_frames() {
            return Err(DocumentError::InvalidArgument(
                "Cannot wrap selection in blockquote: selection spans multiple frames".into(),
            ));
        }
        let (start_block_id, end_block_id) = self.resolve_selection_block_range()?;
        let dto = frontend::document_editing::WrapBlocksInFrameDto {
            start_block_id: start_block_id as i64,
            end_block_id: end_block_id as i64,
            position: Some(frontend::document_editing::FramePosition::InFlow),
            top_margin: None,
            bottom_margin: None,
            left_margin: None,
            right_margin: None,
            padding: None,
            border: None,
            is_blockquote: Some(true),
        };
        let queued = {
            let mut inner = self.doc.lock();
            let _result = document_editing_commands::wrap_blocks_in_frame(
                &inner.ctx,
                Some(inner.stack_id),
                &dto,
            )?;
            inner.modified = true;
            // Frame-structure change: blocks didn't move and no text
            // changed, but block left-margins shift visually. Fire
            // FormatChanged (kind = Block) so the widget triggers a
            // paragraph relayout — same pattern as list operations
            // ([`add_block_to_list`] et al.). `ContentsChanged` with
            // chars_added/removed = 0 was misleading and caused the
            // incremental relayout to no-op until the next full repaint.
            inner.queue_event(DocumentEvent::FormatChanged {
                position: 0,
                length: 0,
                kind: crate::flow::FormatChangeKind::Block,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Wrap the current block in a new blockquote frame at the current
    /// nesting level. Equivalent to `wrap_selection_in_blockquote()`
    /// when there is no selection.
    pub fn insert_blockquote(&self) -> Result<()> {
        self.wrap_selection_in_blockquote()
    }

    /// If the cursor is inside any blockquote, unwrap the innermost one
    /// (lift its blocks into the parent frame and delete the frame).
    /// Otherwise, wrap the current block / selection in a new
    /// blockquote. Mirrors the toggle behaviour of a toolbar button.
    pub fn toggle_blockquote(&self) -> Result<()> {
        if let Some(frame_id) = self.current_blockquote_frame_id() {
            self.unwrap_frame_by_id(frame_id)
        } else {
            self.wrap_selection_in_blockquote()
        }
    }

    /// Unwrap the innermost frame enclosing the cursor (any frame, not
    /// just blockquotes). Errors if the cursor's block sits in the root
    /// frame.
    pub fn unwrap_current_frame(&self) -> Result<()> {
        let frame_ref = self.current_frame().ok_or_else(|| {
            DocumentError::InvalidCursorContext("Cursor is not inside any sub-frame".into())
        })?;
        self.unwrap_frame_by_id(frame_ref.frame_id)
    }

    /// Extract the cursor's current block from its innermost enclosing
    /// blockquote frame, lifting it one nesting level. If the cursor is
    /// not in a blockquote, errors.
    pub fn unwrap_current_block_from_blockquote(&self) -> Result<()> {
        if self.current_blockquote_frame_id().is_none() {
            return Err(DocumentError::InvalidCursorContext(
                "Cursor is not inside a blockquote".into(),
            ));
        }
        let block_id = self.current_block_id_for_mutation()?;
        let dto = frontend::document_editing::UnwrapBlockFromFrameDto {
            block_id: block_id as i64,
        };
        let queued = {
            let mut inner = self.doc.lock();
            let _result = document_editing_commands::unwrap_block_from_frame(
                &inner.ctx,
                Some(inner.stack_id),
                &dto,
            )?;
            inner.modified = true;
            // See note in `wrap_selection_in_blockquote` — frame-structure
            // change without text mutation; fire FormatChanged so the
            // widget relayouts paragraph margins.
            inner.queue_event(DocumentEvent::FormatChanged {
                position: 0,
                length: 0,
                kind: crate::flow::FormatChangeKind::Block,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Wrap the current block in a new blockquote frame. If the cursor
    /// is already inside a blockquote, this creates a deeper nested
    /// quote (depth + 1). If outside, this creates a top-level quote.
    pub fn increase_blockquote_depth(&self) -> Result<()> {
        self.wrap_selection_in_blockquote()
    }

    /// Pop the cursor out of one nesting level of blockquotes. If the
    /// cursor is in a depth-N quote with multiple blocks, the current
    /// block is extracted (splitting the quote if needed). If the
    /// current block is the only one in the quote, the whole quote is
    /// unwrapped.
    pub fn decrease_blockquote_depth(&self) -> Result<()> {
        if self.current_blockquote_frame_id().is_none() {
            return Err(DocumentError::InvalidCursorContext(
                "Cursor is not inside a blockquote to decrease depth".into(),
            ));
        }
        self.unwrap_current_block_from_blockquote()
    }

    fn unwrap_frame_by_id(&self, frame_id: usize) -> Result<()> {
        let dto = frontend::document_editing::UnwrapFrameDto {
            frame_id: frame_id as i64,
        };
        let queued = {
            let mut inner = self.doc.lock();
            let _result =
                document_editing_commands::unwrap_frame(&inner.ctx, Some(inner.stack_id), &dto)?;
            inner.modified = true;
            // See note in `wrap_selection_in_blockquote` — frame-structure
            // change without text mutation; fire FormatChanged so the
            // widget relayouts paragraph margins.
            inner.queue_event(DocumentEvent::FormatChanged {
                position: 0,
                length: 0,
                kind: crate::flow::FormatChangeKind::Block,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    fn current_block_id_for_mutation(&self) -> Result<usize> {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let block_info = document_inspection_commands::get_block_at_position(&inner.ctx, &dto)
            .map_err(|e| anyhow::anyhow!("get_block_at_position: {}", e))?;
        Ok(block_info.block_id as usize)
    }

    fn resolve_selection_block_range(&self) -> Result<(usize, usize)> {
        let (pos, anchor) = self.read_cursor();
        let lo = pos.min(anchor);
        let hi = pos.max(anchor);
        let inner = self.doc.lock();
        let lo_dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(lo),
        };
        let hi_dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(hi),
        };
        let lo_block = document_inspection_commands::get_block_at_position(&inner.ctx, &lo_dto)
            .map_err(|e| anyhow::anyhow!("get_block_at_position(start): {}", e))?;
        let hi_block = document_inspection_commands::get_block_at_position(&inner.ctx, &hi_dto)
            .map_err(|e| anyhow::anyhow!("get_block_at_position(end): {}", e))?;
        Ok((lo_block.block_id as usize, hi_block.block_id as usize))
    }

    // ── Table structure mutations (explicit-ID) ──────────

    /// Remove a table from the document by its ID.
    pub fn remove_table(&self, table_id: usize) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::RemoveTableDto {
                table_id: to_i64(table_id),
            };
            document_editing_commands::remove_table(&inner.ctx, Some(inner.stack_id), &dto)?;
            inner.modified = true;
            inner.invalidate_text_cache();
            inner.rehighlight_all();
            inner.check_block_count_changed();
            inner.check_flow_changed();
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Insert a row into a table at the given index.
    pub fn insert_table_row(&self, table_id: usize, row_index: usize) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::InsertTableRowDto {
                table_id: to_i64(table_id),
                row_index: to_i64(row_index),
            };
            document_editing_commands::insert_table_row(&inner.ctx, Some(inner.stack_id), &dto)?;
            inner.modified = true;
            inner.invalidate_text_cache();
            inner.rehighlight_all();
            inner.check_block_count_changed();
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Insert a column into a table at the given index.
    pub fn insert_table_column(&self, table_id: usize, column_index: usize) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::InsertTableColumnDto {
                table_id: to_i64(table_id),
                column_index: to_i64(column_index),
            };
            document_editing_commands::insert_table_column(&inner.ctx, Some(inner.stack_id), &dto)?;
            inner.modified = true;
            inner.invalidate_text_cache();
            inner.rehighlight_all();
            inner.check_block_count_changed();
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Remove a row from a table. Fails if only one row remains.
    pub fn remove_table_row(&self, table_id: usize, row_index: usize) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::RemoveTableRowDto {
                table_id: to_i64(table_id),
                row_index: to_i64(row_index),
            };
            document_editing_commands::remove_table_row(&inner.ctx, Some(inner.stack_id), &dto)?;
            inner.modified = true;
            inner.invalidate_text_cache();
            inner.rehighlight_all();
            inner.check_block_count_changed();
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Remove a column from a table. Fails if only one column remains.
    pub fn remove_table_column(&self, table_id: usize, column_index: usize) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::RemoveTableColumnDto {
                table_id: to_i64(table_id),
                column_index: to_i64(column_index),
            };
            document_editing_commands::remove_table_column(&inner.ctx, Some(inner.stack_id), &dto)?;
            inner.modified = true;
            inner.invalidate_text_cache();
            inner.rehighlight_all();
            inner.check_block_count_changed();
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Merge a rectangular range of cells within a table.
    pub fn merge_table_cells(
        &self,
        table_id: usize,
        start_row: usize,
        start_column: usize,
        end_row: usize,
        end_column: usize,
    ) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::MergeTableCellsDto {
                table_id: to_i64(table_id),
                start_row: to_i64(start_row),
                start_column: to_i64(start_column),
                end_row: to_i64(end_row),
                end_column: to_i64(end_column),
            };
            document_editing_commands::merge_table_cells(&inner.ctx, Some(inner.stack_id), &dto)?;
            inner.modified = true;
            inner.invalidate_text_cache();
            inner.rehighlight_all();
            inner.check_block_count_changed();
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Split a previously merged cell.
    pub fn split_table_cell(
        &self,
        cell_id: usize,
        split_rows: usize,
        split_columns: usize,
    ) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::SplitTableCellDto {
                cell_id: to_i64(cell_id),
                split_rows: to_i64(split_rows),
                split_columns: to_i64(split_columns),
            };
            document_editing_commands::split_table_cell(&inner.ctx, Some(inner.stack_id), &dto)?;
            inner.modified = true;
            inner.invalidate_text_cache();
            inner.rehighlight_all();
            inner.check_block_count_changed();
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    // ── Table formatting (explicit-ID) ───────────────────

    /// Set formatting on a table.
    pub fn set_table_format(
        &self,
        table_id: usize,
        format: &crate::flow::TableFormat,
    ) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = format.to_set_dto(table_id);
            document_formatting_commands::set_table_format(&inner.ctx, Some(inner.stack_id), &dto)?;
            inner.modified = true;
            inner.queue_event(DocumentEvent::FormatChanged {
                position: 0,
                length: 0,
                kind: crate::flow::FormatChangeKind::Block,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Set formatting on a table cell.
    pub fn set_table_cell_format(
        &self,
        cell_id: usize,
        format: &crate::flow::CellFormat,
    ) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = format.to_set_dto(cell_id);
            document_formatting_commands::set_table_cell_format(
                &inner.ctx,
                Some(inner.stack_id),
                &dto,
            )?;
            inner.modified = true;
            inner.queue_event(DocumentEvent::FormatChanged {
                position: 0,
                length: 0,
                kind: crate::flow::FormatChangeKind::Block,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    // ── Table convenience (position-based) ───────────────

    /// Remove the table the cursor is currently inside.
    /// Returns an error if the cursor is not inside a table.
    pub fn remove_current_table(&self) -> Result<()> {
        let table = self.current_table().ok_or_else(|| {
            DocumentError::InvalidCursorContext("cursor is not inside a table".into())
        })?;
        self.remove_table(table.id())
    }

    /// Insert a row above the cursor's current row.
    /// Returns an error if the cursor is not inside a table.
    pub fn insert_row_above(&self) -> Result<()> {
        let cell_ref = self.current_table_cell().ok_or_else(|| {
            DocumentError::InvalidCursorContext("cursor is not inside a table".into())
        })?;
        self.insert_table_row(cell_ref.table.id(), cell_ref.row)
    }

    /// Insert a row below the cursor's current row.
    /// Returns an error if the cursor is not inside a table.
    pub fn insert_row_below(&self) -> Result<()> {
        let cell_ref = self.current_table_cell().ok_or_else(|| {
            DocumentError::InvalidCursorContext("cursor is not inside a table".into())
        })?;
        self.insert_table_row(cell_ref.table.id(), cell_ref.row + 1)
    }

    /// Insert a column before the cursor's current column.
    /// Returns an error if the cursor is not inside a table.
    pub fn insert_column_before(&self) -> Result<()> {
        let cell_ref = self.current_table_cell().ok_or_else(|| {
            DocumentError::InvalidCursorContext("cursor is not inside a table".into())
        })?;
        self.insert_table_column(cell_ref.table.id(), cell_ref.column)
    }

    /// Insert a column after the cursor's current column.
    /// Returns an error if the cursor is not inside a table.
    pub fn insert_column_after(&self) -> Result<()> {
        let cell_ref = self.current_table_cell().ok_or_else(|| {
            DocumentError::InvalidCursorContext("cursor is not inside a table".into())
        })?;
        self.insert_table_column(cell_ref.table.id(), cell_ref.column + 1)
    }

    /// Remove the row at the cursor's current position.
    /// Returns an error if the cursor is not inside a table.
    pub fn remove_current_row(&self) -> Result<()> {
        let cell_ref = self.current_table_cell().ok_or_else(|| {
            DocumentError::InvalidCursorContext("cursor is not inside a table".into())
        })?;
        self.remove_table_row(cell_ref.table.id(), cell_ref.row)
    }

    /// Remove the column at the cursor's current position.
    /// Returns an error if the cursor is not inside a table.
    pub fn remove_current_column(&self) -> Result<()> {
        let cell_ref = self.current_table_cell().ok_or_else(|| {
            DocumentError::InvalidCursorContext("cursor is not inside a table".into())
        })?;
        self.remove_table_column(cell_ref.table.id(), cell_ref.column)
    }

    /// Merge cells spanned by the current selection.
    ///
    /// Both cursor position and anchor must be inside the same table.
    /// The cell range is derived from the cells at position and anchor.
    /// Returns an error if the cursor is not inside a table or position
    /// and anchor are in different tables.
    pub fn merge_selected_cells(&self) -> Result<()> {
        let pos_cell = self.current_table_cell().ok_or_else(|| {
            DocumentError::InvalidCursorContext("cursor position is not inside a table".into())
        })?;

        // Get anchor cell
        let (_pos, anchor) = self.read_cursor();
        let anchor_cell = {
            // Create a temporary block handle at the anchor position
            let inner = self.doc.lock();
            let dto = frontend::document_inspection::GetBlockAtPositionDto {
                position: to_i64(anchor),
            };
            let block_info = document_inspection_commands::get_block_at_position(&inner.ctx, &dto)
                .map_err(|_| {
                    DocumentError::InvalidCursorContext(
                        "cursor anchor is not inside a table".into(),
                    )
                })?;
            let block = crate::text_block::TextBlock {
                doc: self.doc.clone(),
                block_id: block_info.block_id as usize,
            };
            drop(inner);
            block.table_cell().ok_or_else(|| {
                DocumentError::InvalidCursorContext("cursor anchor is not inside a table".into())
            })?
        };

        if pos_cell.table.id() != anchor_cell.table.id() {
            return Err(DocumentError::InvalidArgument(
                "position and anchor are in different tables".into(),
            ));
        }

        let start_row = pos_cell.row.min(anchor_cell.row);
        let start_col = pos_cell.column.min(anchor_cell.column);
        let end_row = pos_cell.row.max(anchor_cell.row);
        let end_col = pos_cell.column.max(anchor_cell.column);

        self.merge_table_cells(pos_cell.table.id(), start_row, start_col, end_row, end_col)
    }

    /// Split the cell at the cursor's current position.
    /// Returns an error if the cursor is not inside a table.
    pub fn split_current_cell(&self, split_rows: usize, split_columns: usize) -> Result<()> {
        let cell_ref = self.current_table_cell().ok_or_else(|| {
            DocumentError::InvalidCursorContext("cursor is not inside a table".into())
        })?;
        // Get the cell entity ID from the table handle
        let cell = cell_ref
            .table
            .cell(cell_ref.row, cell_ref.column)
            .ok_or_else(|| DocumentError::NotFound("cell not found".into()))?;
        // TextTableCell stores cell_id
        self.split_table_cell(cell.id(), split_rows, split_columns)
    }

    /// Set formatting on the table the cursor is currently inside.
    /// Returns an error if the cursor is not inside a table.
    pub fn set_current_table_format(&self, format: &crate::flow::TableFormat) -> Result<()> {
        let table = self.current_table().ok_or_else(|| {
            DocumentError::InvalidCursorContext("cursor is not inside a table".into())
        })?;
        self.set_table_format(table.id(), format)
    }

    /// Set formatting on the cell the cursor is currently inside.
    /// Returns an error if the cursor is not inside a table.
    pub fn set_current_cell_format(&self, format: &crate::flow::CellFormat) -> Result<()> {
        let cell_ref = self.current_table_cell().ok_or_else(|| {
            DocumentError::InvalidCursorContext("cursor is not inside a table".into())
        })?;
        let cell = cell_ref
            .table
            .cell(cell_ref.row, cell_ref.column)
            .ok_or_else(|| DocumentError::NotFound("cell not found".into()))?;
        self.set_table_cell_format(cell.id(), format)
    }

    // ── Cell selection queries ────────────────────────────────

    /// Determine the kind of selection the cursor currently has.
    ///
    /// Returns [`Cells`](crate::SelectionKind::Cells) when position and anchor are in
    /// different cells of the same table (rectangular cell selection), or
    /// when an explicit cell-selection override is active.
    pub fn selection_kind(&self) -> crate::flow::SelectionKind {
        use crate::flow::{CellRange, SelectionKind};

        // Check override first
        {
            let d = self.data.lock();
            if let Some(ref range) = d.cell_selection_override {
                return SelectionKind::Cells(range.clone());
            }
            if d.position == d.anchor {
                return SelectionKind::None;
            }
        }

        let (pos, anchor) = self.read_cursor();

        // Look up table cell for position and anchor
        let pos_cell = self.table_cell_at(pos);
        let anchor_cell = self.table_cell_at(anchor);

        match (&pos_cell, &anchor_cell) {
            (None, None) => {
                // Both endpoints are outside tables. Check whether a table
                // sits between them — if so, all its cells must be selected
                // (Word behaviour).
                let (start, end) = (pos.min(anchor), pos.max(anchor));
                if let Some(t) = self.find_table_between(start, end) {
                    let table_id = t.id();
                    let rows = t.rows();
                    let cols = t.columns();
                    let range = CellRange {
                        table_id,
                        start_row: 0,
                        start_col: 0,
                        end_row: if rows > 0 { rows - 1 } else { 0 },
                        end_col: if cols > 0 { cols - 1 } else { 0 },
                    };
                    let spans = self.collect_cell_spans(table_id);
                    SelectionKind::Mixed {
                        cell_range: range.expand_for_spans(&spans),
                        text_before: true,
                        text_after: true,
                    }
                } else {
                    SelectionKind::Text
                }
            }
            (Some(pc), Some(ac)) => {
                if pc.table.id() != ac.table.id() {
                    // Different tables — treat as text (whole tables selected between them)
                    return SelectionKind::Text;
                }
                if pc.row == ac.row && pc.column == ac.column {
                    // Same cell — text selection within one cell
                    return SelectionKind::Text;
                }
                // Different cells, same table — rectangular cell selection
                let range = CellRange {
                    table_id: pc.table.id(),
                    start_row: pc.row.min(ac.row),
                    start_col: pc.column.min(ac.column),
                    end_row: pc.row.max(ac.row),
                    end_col: pc.column.max(ac.column),
                };
                let spans = self.collect_cell_spans(pc.table.id());
                SelectionKind::Cells(range.expand_for_spans(&spans))
            }
            (Some(tc), None) | (None, Some(tc)) => {
                // One endpoint inside a table, the other outside — mixed
                // selection.  Following Word behaviour, select ALL cells in
                // the table (not just from the entry edge to the cursor row).
                let table_id = tc.table.id();
                let rows = tc.table.rows();
                let cols = tc.table.columns();

                let inside_pos = if pos_cell.is_some() { pos } else { anchor };
                let outside_pos = if pos_cell.is_some() { anchor } else { pos };

                let text_before = outside_pos < inside_pos;
                let text_after = !text_before;

                let range = CellRange {
                    table_id,
                    start_row: 0,
                    start_col: 0,
                    end_row: if rows > 0 { rows - 1 } else { 0 },
                    end_col: if cols > 0 { cols - 1 } else { 0 },
                };
                let spans = self.collect_cell_spans(table_id);
                SelectionKind::Mixed {
                    cell_range: range.expand_for_spans(&spans),
                    text_before,
                    text_after,
                }
            }
        }
    }

    /// Returns `true` when the current selection involves whole-cell selection.
    pub fn is_cell_selection(&self) -> bool {
        matches!(
            self.selection_kind(),
            crate::flow::SelectionKind::Cells(_) | crate::flow::SelectionKind::Mixed { .. }
        )
    }

    /// Returns the rectangular cell range if the cursor has a cell selection.
    pub fn selected_cell_range(&self) -> Option<crate::flow::CellRange> {
        match self.selection_kind() {
            crate::flow::SelectionKind::Cells(r) => Some(r),
            crate::flow::SelectionKind::Mixed { cell_range, .. } => Some(cell_range),
            _ => None,
        }
    }

    /// Returns all cells in the selected rectangular range.
    pub fn selected_cells(&self) -> Vec<TableCellRef> {
        let range = match self.selected_cell_range() {
            Some(r) => r,
            None => return Vec::new(),
        };
        let table = TextTable {
            doc: self.doc.clone(),
            table_id: range.table_id,
        };
        let mut cells = Vec::new();
        for row in range.start_row..=range.end_row {
            for col in range.start_col..=range.end_col {
                if table.cell(row, col).is_some() {
                    cells.push(TableCellRef {
                        table: table.clone(),
                        row,
                        column: col,
                    });
                }
            }
        }
        cells
    }

    // ── Explicit cell selection ─────────────────────────────

    /// Set an explicit single-cell selection override.
    pub fn select_table_cell(&self, table_id: usize, row: usize, col: usize) {
        let mut d = self.data.lock();
        d.cell_selection_override = Some(crate::flow::CellRange {
            table_id,
            start_row: row,
            start_col: col,
            end_row: row,
            end_col: col,
        });
    }

    /// Set an explicit rectangular cell-range selection override.
    pub fn select_cell_range(
        &self,
        table_id: usize,
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    ) {
        let range = crate::flow::CellRange {
            table_id,
            start_row,
            start_col,
            end_row,
            end_col,
        };
        let spans = self.collect_cell_spans(table_id);
        let mut d = self.data.lock();
        d.cell_selection_override = Some(range.expand_for_spans(&spans));
    }

    /// Clear any cell-selection override without changing position/anchor.
    pub fn clear_cell_selection(&self) {
        let mut d = self.data.lock();
        d.cell_selection_override = None;
    }

    /// Compute (min_position, max_position) spanning all blocks in a cell range.
    /// Returns `None` if the table or cells cannot be found.
    fn cell_range_positions(&self, range: &CellRange) -> Option<(usize, usize)> {
        let inner = self.doc.lock();
        let main_frame_id = get_main_frame_id(&inner);
        let flow = crate::text_frame::build_flow_elements(&inner, &self.doc, main_frame_id);
        drop(inner);

        // Find the table matching the range's table_id
        let table = flow.into_iter().find_map(|e| match e {
            FlowElement::Table(t) if t.id() == range.table_id => Some(t),
            _ => None,
        })?;

        let mut min_pos = usize::MAX;
        let mut max_pos = 0usize;

        for row in range.start_row..=range.end_row {
            for col in range.start_col..=range.end_col {
                if let Some(cell) = table.cell(row, col) {
                    for block in cell.blocks() {
                        let bp = block.position();
                        let bl = block.length();
                        min_pos = min_pos.min(bp);
                        max_pos = max_pos.max(bp + bl);
                    }
                }
            }
        }

        if min_pos == usize::MAX {
            return None;
        }

        // Extend max_pos past the last block to ensure cross-cell detection
        Some((min_pos, max_pos + 1))
    }

    // ── Cell selection helpers (private) ─────────────────────

    /// Look up which table cell contains the given document position, if any.
    fn table_cell_at(&self, position: usize) -> Option<TableCellRef> {
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(position),
        };
        let block_info =
            document_inspection_commands::get_block_at_position(&inner.ctx, &dto).ok()?;

        let block_id = if to_i64(position) < block_info.block_start && position > 0 {
            let prev_dto = frontend::document_inspection::GetBlockAtPositionDto {
                position: to_i64(position - 1),
            };
            let prev_info =
                document_inspection_commands::get_block_at_position(&inner.ctx, &prev_dto).ok()?;
            prev_info.block_id as usize
        } else {
            block_info.block_id as usize
        };

        let block = crate::text_block::TextBlock {
            doc: self.doc.clone(),
            block_id,
        };
        drop(inner);
        block.table_cell()
    }

    /// Find the document position at the boundary of the block adjacent to a
    /// table. Used by the table-trap logic in [`set_position`](Self::set_position).
    ///
    /// - `before == true`: returns the last position of the block immediately
    ///   before the table (i.e. `block.position() + block.length()`).
    /// - `before == false`: returns the first position of the block immediately
    ///   after the table.
    ///
    /// Returns `None` when no adjacent block exists (table is first or last
    /// element in the flow).
    fn table_boundary_position(&self, table_id: usize, before: bool) -> Option<usize> {
        let inner = self.doc.lock();
        let main_frame_id = get_main_frame_id(&inner);
        let flow = crate::text_frame::build_flow_elements(&inner, &self.doc, main_frame_id);
        drop(inner);

        // Find the table in the flow and peek at the adjacent element.
        let idx = flow
            .iter()
            .position(|e| matches!(e, FlowElement::Table(t) if t.id() == table_id))?;

        if before {
            // Walk backwards to find the nearest Block.
            for i in (0..idx).rev() {
                if let FlowElement::Block(b) = &flow[i] {
                    return Some(b.position() + b.length());
                }
            }
        } else {
            // Walk forwards to find the nearest Block.
            for item in flow.iter().skip(idx + 1) {
                if let FlowElement::Block(b) = item {
                    return Some(b.position());
                }
            }
        }
        None
    }

    /// Find the first table whose cell blocks fall within the range `(start, end)`.
    fn find_table_between(&self, start: usize, end: usize) -> Option<TextTable> {
        let inner = self.doc.lock();
        let main_frame_id = get_main_frame_id(&inner);
        let flow = crate::text_frame::build_flow_elements(&inner, &self.doc, main_frame_id);
        drop(inner);

        for elem in flow {
            if let FlowElement::Table(t) = elem {
                // Check whether the first cell's block position is between
                // the two endpoints (i.e. the table is inside the range).
                if let Some(first_cell) = t.cell(0, 0) {
                    let blocks = first_cell.blocks();
                    if let Some(fb) = blocks.first() {
                        let p = fb.position();
                        if p > start && p < end {
                            return Some(t);
                        }
                    }
                }
            }
        }
        None
    }

    /// Collect `(row, col, row_span, col_span)` tuples for all cells in a table.
    fn collect_cell_spans(&self, table_id: usize) -> Vec<(usize, usize, usize, usize)> {
        let inner = self.doc.lock();
        let table_dto =
            match frontend::commands::table_commands::get_table(&inner.ctx, &(table_id as u64))
                .ok()
                .flatten()
            {
                Some(t) => t,
                None => return Vec::new(),
            };

        let mut spans = Vec::with_capacity(table_dto.cells.len());
        for &cell_id in &table_dto.cells {
            if let Some(cell) =
                frontend::commands::table_cell_commands::get_table_cell(&inner.ctx, &cell_id)
                    .ok()
                    .flatten()
            {
                spans.push((
                    cell.row as usize,
                    cell.column as usize,
                    cell.row_span.max(1) as usize,
                    cell.column_span.max(1) as usize,
                ));
            }
        }
        spans
    }

    /// Delete the character after the cursor (Delete key).
    pub fn delete_char(&self) -> Result<()> {
        let (pos, anchor) = self.read_cursor();
        let (del_pos, del_anchor) = if pos != anchor {
            (pos, anchor)
        } else {
            // No-op at end of document (symmetric with delete_previous_char at start)
            let end = {
                let inner = self.doc.lock();
                document_inspection_commands::get_document_stats(&inner.ctx)
                    .map(|s| max_cursor_position(&s))
                    .unwrap_or(0)
            };
            if pos >= end {
                return Ok(());
            }
            // Delete the whole grapheme cluster after the cursor so a
            // single Delete on `👋🏻` or `e\u{0301}` removes the
            // user-perceived character, not just its first scalar.
            let to = self.next_grapheme_boundary(pos);
            if to == pos {
                return Ok(());
            }
            (pos, to)
        };
        self.do_delete(del_pos, del_anchor)
    }

    /// Delete the character before the cursor (Backspace key).
    pub fn delete_previous_char(&self) -> Result<()> {
        let (pos, anchor) = self.read_cursor();
        let (del_pos, del_anchor) = if pos != anchor {
            (pos, anchor)
        } else if pos > 0 {
            let from = self.prev_grapheme_boundary(pos);
            if from == pos {
                return Ok(());
            }
            (from, pos)
        } else {
            return Ok(());
        };
        self.do_delete(del_pos, del_anchor)
    }

    /// Delete the selected text. Returns the deleted text. No-op if no selection.
    pub fn remove_selected_text(&self) -> Result<String> {
        let (pos, anchor) = self.read_cursor();
        if pos == anchor {
            return Ok(String::new());
        }
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::DeleteTextDto {
                position: to_i64(pos),
                anchor: to_i64(anchor),
            };
            let result =
                document_editing_commands::delete_text(&inner.ctx, Some(inner.stack_id), &dto)?;
            let edit_pos = pos.min(anchor);
            let removed = pos.max(anchor) - edit_pos;
            let new_pos = to_usize(result.new_position);
            inner.adjust_cursors(edit_pos, removed, 0);
            {
                let mut d = self.data.lock();
                d.position = new_pos;
                d.anchor = new_pos;
            }
            inner.modified = true;
            inner.invalidate_text_cache();
            inner.rehighlight_affected(edit_pos);
            inner.queue_event(DocumentEvent::ContentsChanged {
                position: edit_pos,
                chars_removed: removed,
                chars_added: 0,
                blocks_affected: 1,
            });
            inner.check_block_count_changed();
            inner.check_flow_changed();
            // Return the deleted text alongside the queued events
            (result.deleted_text, self.queue_undo_redo_event(&mut inner))
        };
        crate::inner::dispatch_queued_events(queued.1);
        Ok(queued.0)
    }

    // ── List operations ──────────────────────────────────────

    /// Returns the list that the block at the cursor position belongs to,
    /// or `None` if the current block is not a list item.
    pub fn current_list(&self) -> Option<crate::TextList> {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let block_info =
            document_inspection_commands::get_block_at_position(&inner.ctx, &dto).ok()?;
        let block = crate::text_block::TextBlock {
            doc: self.doc.clone(),
            block_id: block_info.block_id as usize,
        };
        drop(inner);
        block.list()
    }

    /// Turn the block(s) in the selection into a list.
    pub fn create_list(&self, style: ListStyle) -> Result<()> {
        let (pos, anchor) = self.read_cursor();
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::CreateListDto {
                position: to_i64(pos),
                anchor: to_i64(anchor),
                style: style.clone(),
            };
            document_editing_commands::create_list(&inner.ctx, Some(inner.stack_id), &dto)?;
            inner.modified = true;
            inner.rehighlight_affected(pos.min(anchor));
            inner.queue_event(DocumentEvent::ContentsChanged {
                position: pos.min(anchor),
                chars_removed: 0,
                chars_added: 0,
                blocks_affected: 1,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Insert a new list item at the cursor position.
    pub fn insert_list(&self, style: ListStyle) -> Result<()> {
        let (pos, anchor) = self.read_cursor();
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::InsertListDto {
                position: to_i64(pos),
                anchor: to_i64(anchor),
                style: style.clone(),
            };
            let result =
                document_editing_commands::insert_list(&inner.ctx, Some(inner.stack_id), &dto)?;
            let edit_pos = pos.min(anchor);
            let removed = pos.max(anchor) - edit_pos;
            self.finish_edit_ext(
                &mut inner,
                edit_pos,
                removed,
                to_usize(result.new_position),
                1,
                false,
            )
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Set formatting on a list by its ID.
    pub fn set_list_format(&self, list_id: usize, format: &crate::ListFormat) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = format.to_set_dto(list_id);
            document_formatting_commands::set_list_format(&inner.ctx, Some(inner.stack_id), &dto)?;
            inner.modified = true;
            inner.queue_event(DocumentEvent::FormatChanged {
                position: 0,
                length: 0,
                kind: crate::flow::FormatChangeKind::List,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Set formatting on the list that the current block belongs to.
    /// Returns an error if the cursor is not inside a list item.
    pub fn set_current_list_format(&self, format: &crate::ListFormat) -> Result<()> {
        let list = self.current_list().ok_or_else(|| {
            DocumentError::InvalidCursorContext("cursor is not inside a list".into())
        })?;
        self.set_list_format(list.id(), format)
    }

    /// Add a block to a list by their IDs.
    pub fn add_block_to_list(&self, block_id: usize, list_id: usize) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::AddBlockToListDto {
                block_id: to_i64(block_id),
                list_id: to_i64(list_id),
            };
            document_editing_commands::add_block_to_list(&inner.ctx, Some(inner.stack_id), &dto)?;
            inner.modified = true;
            // List membership is a formatting/layout concern, not a text
            // change — fire FormatChanged so consumers re-layout (the
            // block's horizontal position and list marker depend on
            // its list assignment). ContentsChanged with position=0
            // was misleading and caused incremental relayouts to
            // re-shape the wrong block.
            inner.queue_event(DocumentEvent::FormatChanged {
                position: 0,
                length: 0,
                kind: crate::flow::FormatChangeKind::List,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Add the block at the cursor position to a list.
    pub fn add_current_block_to_list(&self, list_id: usize) -> Result<()> {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let block_info = document_inspection_commands::get_block_at_position(&inner.ctx, &dto)?;
        drop(inner);
        self.add_block_to_list(block_info.block_id as usize, list_id)
    }

    /// Remove a block from its list by block ID.
    pub fn remove_block_from_list(&self, block_id: usize) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::RemoveBlockFromListDto {
                block_id: to_i64(block_id),
            };
            document_editing_commands::remove_block_from_list(
                &inner.ctx,
                Some(inner.stack_id),
                &dto,
            )?;
            inner.modified = true;
            // See `add_block_to_list` — list-membership is a
            // formatting/layout change, not a text content change.
            inner.queue_event(DocumentEvent::FormatChanged {
                position: 0,
                length: 0,
                kind: crate::flow::FormatChangeKind::List,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Remove the block at the cursor position from its list.
    /// Returns an error if the current block is not a list item.
    pub fn remove_current_block_from_list(&self) -> Result<()> {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let block_info = document_inspection_commands::get_block_at_position(&inner.ctx, &dto)?;
        drop(inner);
        self.remove_block_from_list(block_info.block_id as usize)
    }

    /// Remove a list item by index within the list.
    /// Resolves the index to a block, then removes it from the list.
    pub fn remove_list_item(&self, list_id: usize, index: usize) -> Result<()> {
        let list = crate::text_list::TextList {
            doc: self.doc.clone(),
            list_id,
        };
        let block = list.item(index).ok_or_else(|| {
            DocumentError::OutOfRange(format!("list item index {index} out of range"))
        })?;
        self.remove_block_from_list(block.id())
    }

    // ── Format queries ───────────────────────────────────────

    /// Get the character format at the cursor position. Reads the
    /// covering `FormatRun` (or image anchor) directly from the store.
    pub fn char_format(&self) -> Result<TextFormat> {
        let pos = self.position();
        let inner = self.doc.lock();

        // Locate the block containing the cursor.
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let block_info = document_inspection_commands::get_block_at_position(&inner.ctx, &dto)?;
        let block_id = block_info.block_id as u64;
        let mut block_dto =
            frontend::commands::block_commands::get_block(&inner.ctx, &block_id)?
                .ok_or_else(|| DocumentError::NotFound("block not found at position".into()))?;
        let store = inner.ctx.db_context.get_store();
        crate::inner::refresh_block_position(&mut block_dto, store);

        // Convert document-wide char position to a byte offset within
        // the block's content (read from the rope).
        let local_char = pos.saturating_sub(block_dto.document_position as usize);
        let entity: common::entities::Block = block_dto.clone().into();
        let plain_owned = common::database::rope_helpers::block_content_via_store(&entity, store);
        let plain: &str = &plain_owned;
        let byte_offset: u32 = plain
            .char_indices()
            .nth(local_char)
            .map(|(b, _)| b as u32)
            .unwrap_or(plain.len() as u32);

        // If there's an image anchor at this exact byte position, use
        // its format.
        let images = store
            .block_images
            .read()
            .get(&block_id)
            .cloned()
            .unwrap_or_default();
        if let Some(img) = images.iter().find(|i| i.byte_offset == byte_offset) {
            return Ok(TextFormat::from(&img.format));
        }

        // Otherwise find the FormatRun covering the byte position.
        let runs = store
            .format_runs
            .read()
            .get(&block_id)
            .cloned()
            .unwrap_or_default();
        let fmt = runs
            .iter()
            .find(|r| r.byte_start <= byte_offset && byte_offset < r.byte_end)
            .map(|r| TextFormat::from(&r.format))
            .unwrap_or_default();
        Ok(fmt)
    }

    /// Get the block format of the block containing the cursor.
    pub fn block_format(&self) -> Result<BlockFormat> {
        let pos = self.position();
        let inner = self.doc.lock();
        let dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let block_info = document_inspection_commands::get_block_at_position(&inner.ctx, &dto)?;
        let block_id = block_info.block_id as u64;
        let block = frontend::commands::block_commands::get_block(&inner.ctx, &block_id)?
            .ok_or_else(|| DocumentError::NotFound("block not found".into()))?;
        Ok(BlockFormat::from(&block))
    }

    // ── Format application ───────────────────────────────────

    /// Set the character format for the selection.
    pub fn set_char_format(&self, format: &TextFormat) -> Result<()> {
        let (pos, anchor) = self.read_cursor();
        let queued = {
            let mut inner = self.doc.lock();
            let dto = format.to_set_dto(pos, anchor);
            document_formatting_commands::set_text_format(&inner.ctx, Some(inner.stack_id), &dto)?;
            let start = pos.min(anchor);
            let length = pos.max(anchor) - start;
            inner.modified = true;
            inner.queue_event(DocumentEvent::FormatChanged {
                position: start,
                length,
                kind: crate::flow::FormatChangeKind::Character,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Merge a character format into the selection.
    pub fn merge_char_format(&self, format: &TextFormat) -> Result<()> {
        let (pos, anchor) = self.read_cursor();
        let queued = {
            let mut inner = self.doc.lock();
            let dto = format.to_merge_dto(pos, anchor);
            document_formatting_commands::merge_text_format(
                &inner.ctx,
                Some(inner.stack_id),
                &dto,
            )?;
            let start = pos.min(anchor);
            let length = pos.max(anchor) - start;
            inner.modified = true;
            inner.queue_event(DocumentEvent::FormatChanged {
                position: start,
                length,
                kind: crate::flow::FormatChangeKind::Character,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Set the block format for the current block (or all blocks in selection).
    pub fn set_block_format(&self, format: &BlockFormat) -> Result<()> {
        let (pos, anchor) = self.read_cursor();
        let queued = {
            let mut inner = self.doc.lock();
            let dto = format.to_set_dto(pos, anchor);
            document_formatting_commands::set_block_format(&inner.ctx, Some(inner.stack_id), &dto)?;
            let start = pos.min(anchor);
            let length = pos.max(anchor) - start;
            inner.modified = true;
            inner.queue_event(DocumentEvent::FormatChanged {
                position: start,
                length,
                kind: crate::flow::FormatChangeKind::Block,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Set the frame format.
    pub fn set_frame_format(&self, frame_id: usize, format: &FrameFormat) -> Result<()> {
        let (pos, anchor) = self.read_cursor();
        let queued = {
            let mut inner = self.doc.lock();
            let dto = format.to_set_dto(pos, anchor, frame_id);
            document_formatting_commands::set_frame_format(&inner.ctx, Some(inner.stack_id), &dto)?;
            let start = pos.min(anchor);
            let length = pos.max(anchor) - start;
            inner.modified = true;
            inner.queue_event(DocumentEvent::FormatChanged {
                position: start,
                length,
                kind: crate::flow::FormatChangeKind::Block,
            });
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    // ── Edit blocks (composite undo) ─────────────────────────

    /// Begin a group of operations that will be undone as a single unit.
    pub fn begin_edit_block(&self) {
        let inner = self.doc.lock();
        undo_redo_commands::begin_composite(&inner.ctx, Some(inner.stack_id));
    }

    /// End the current edit block.
    pub fn end_edit_block(&self) {
        let inner = self.doc.lock();
        undo_redo_commands::end_composite(&inner.ctx);
    }

    /// Alias for [`begin_edit_block`](Self::begin_edit_block).
    ///
    /// Semantically indicates that the new composite should be merged with
    /// the previous one (e.g., consecutive keystrokes grouped into a single
    /// undo unit). The current backend treats this identically to
    /// `begin_edit_block`; future versions may implement automatic merging.
    pub fn join_previous_edit_block(&self) {
        self.begin_edit_block();
    }

    // ── Private helpers ─────────────────────────────────────

    /// Queue an `UndoRedoChanged` event and return all queued events for dispatch.
    fn queue_undo_redo_event(&self, inner: &mut TextDocumentInner) -> QueuedEvents {
        let can_undo = undo_redo_commands::can_undo(&inner.ctx, Some(inner.stack_id));
        let can_redo = undo_redo_commands::can_redo(&inner.ctx, Some(inner.stack_id));
        inner.queue_event(DocumentEvent::UndoRedoChanged { can_undo, can_redo });
        inner.take_queued_events()
    }

    fn do_delete(&self, pos: usize, anchor: usize) -> Result<()> {
        let queued = {
            let mut inner = self.doc.lock();
            let dto = frontend::document_editing::DeleteTextDto {
                position: to_i64(pos),
                anchor: to_i64(anchor),
            };
            let result =
                document_editing_commands::delete_text(&inner.ctx, Some(inner.stack_id), &dto)?;
            let edit_pos = pos.min(anchor);
            let removed = pos.max(anchor) - edit_pos;
            let new_pos = to_usize(result.new_position);
            inner.adjust_cursors(edit_pos, removed, 0);
            {
                let mut d = self.data.lock();
                d.position = new_pos;
                d.anchor = new_pos;
            }
            inner.modified = true;
            inner.invalidate_text_cache();
            inner.rehighlight_affected(edit_pos);
            inner.queue_event(DocumentEvent::ContentsChanged {
                position: edit_pos,
                chars_removed: removed,
                chars_added: 0,
                blocks_affected: 1,
            });
            inner.check_block_count_changed();
            inner.check_flow_changed();
            self.queue_undo_redo_event(&mut inner)
        };
        crate::inner::dispatch_queued_events(queued);
        Ok(())
    }

    /// Resolve a MoveOperation to a concrete position.
    fn resolve_move(&self, op: MoveOperation, n: usize) -> usize {
        let pos = self.position();
        match op {
            MoveOperation::NoMove => pos,
            MoveOperation::Start => 0,
            MoveOperation::End => {
                let inner = self.doc.lock();
                document_inspection_commands::get_document_stats(&inner.ctx)
                    .map(|s| max_cursor_position(&s))
                    .unwrap_or(pos)
            }
            MoveOperation::NextCharacter | MoveOperation::Right => {
                let mut cur = pos;
                for _ in 0..n {
                    let next = self.next_grapheme_boundary(cur);
                    if next == cur {
                        break;
                    }
                    cur = next;
                }
                cur
            }
            MoveOperation::PreviousCharacter | MoveOperation::Left => {
                let mut cur = pos;
                for _ in 0..n {
                    let prev = self.prev_grapheme_boundary(cur);
                    if prev == cur {
                        break;
                    }
                    cur = prev;
                }
                cur
            }
            MoveOperation::StartOfBlock | MoveOperation::StartOfLine => {
                let inner = self.doc.lock();
                let dto = frontend::document_inspection::GetBlockAtPositionDto {
                    position: to_i64(pos),
                };
                document_inspection_commands::get_block_at_position(&inner.ctx, &dto)
                    .map(|info| to_usize(info.block_start))
                    .unwrap_or(pos)
            }
            MoveOperation::EndOfBlock | MoveOperation::EndOfLine => {
                let inner = self.doc.lock();
                let dto = frontend::document_inspection::GetBlockAtPositionDto {
                    position: to_i64(pos),
                };
                document_inspection_commands::get_block_at_position(&inner.ctx, &dto)
                    .map(|info| to_usize(info.block_start) + to_usize(info.block_length))
                    .unwrap_or(pos)
            }
            MoveOperation::NextBlock => {
                let inner = self.doc.lock();
                let dto = frontend::document_inspection::GetBlockAtPositionDto {
                    position: to_i64(pos),
                };
                document_inspection_commands::get_block_at_position(&inner.ctx, &dto)
                    .map(|info| {
                        // Move past current block + 1 (block separator)
                        to_usize(info.block_start) + to_usize(info.block_length) + 1
                    })
                    .unwrap_or(pos)
            }
            MoveOperation::PreviousBlock => {
                let inner = self.doc.lock();
                let dto = frontend::document_inspection::GetBlockAtPositionDto {
                    position: to_i64(pos),
                };
                let block_start =
                    document_inspection_commands::get_block_at_position(&inner.ctx, &dto)
                        .map(|info| to_usize(info.block_start))
                        .unwrap_or(pos);
                if block_start >= 2 {
                    // Skip past the block separator (which maps to the current block)
                    let prev_dto = frontend::document_inspection::GetBlockAtPositionDto {
                        position: to_i64(block_start - 2),
                    };
                    document_inspection_commands::get_block_at_position(&inner.ctx, &prev_dto)
                        .map(|info| to_usize(info.block_start))
                        .unwrap_or(0)
                } else {
                    0
                }
            }
            MoveOperation::NextWord | MoveOperation::EndOfWord | MoveOperation::WordRight => {
                let (_, end) = self.find_word_boundaries(pos);
                // Move past the word end to the next word
                if end == pos {
                    // Already at a boundary, skip whitespace
                    let inner = self.doc.lock();
                    let max_pos = document_inspection_commands::get_document_stats(&inner.ctx)
                        .map(|s| max_cursor_position(&s))
                        .unwrap_or(0);
                    let scan_len = max_pos.saturating_sub(pos).min(64);
                    if scan_len == 0 {
                        return pos;
                    }
                    let dto = frontend::document_inspection::GetTextAtPositionDto {
                        position: to_i64(pos),
                        length: to_i64(scan_len),
                    };
                    if let Ok(r) =
                        document_inspection_commands::get_text_at_position(&inner.ctx, &dto)
                    {
                        for (i, ch) in r.text.chars().enumerate() {
                            if ch.is_alphanumeric() || ch == '_' {
                                // Found start of next word, find its end
                                let word_pos = pos + i;
                                drop(inner);
                                let (_, word_end) = self.find_word_boundaries(word_pos);
                                return word_end;
                            }
                        }
                    }
                    pos + scan_len
                } else {
                    end
                }
            }
            MoveOperation::PreviousWord | MoveOperation::StartOfWord | MoveOperation::WordLeft => {
                let (start, _) = self.find_word_boundaries(pos);
                if start < pos {
                    start
                } else if pos > 0 {
                    // Cursor is at a word start or on whitespace — scan backwards
                    // to find the start of the previous word.
                    let mut search = pos - 1;
                    loop {
                        let (ws, we) = self.find_word_boundaries(search);
                        if ws < we {
                            // Found a word; return its start
                            break ws;
                        }
                        // Still on whitespace/non-word; keep scanning
                        if search == 0 {
                            break 0;
                        }
                        search -= 1;
                    }
                } else {
                    0
                }
            }
            MoveOperation::Up | MoveOperation::Down => {
                // Up/Down are visual operations that depend on line wrapping.
                // Without layout info, treat as PreviousBlock/NextBlock.
                if matches!(op, MoveOperation::Up) {
                    self.resolve_move(MoveOperation::PreviousBlock, 1)
                } else {
                    self.resolve_move(MoveOperation::NextBlock, 1)
                }
            }
        }
    }

    /// Snap the cursor's current position to the nearest grapheme
    /// cluster boundary, moving forward if currently mid-cluster.
    /// No-op when already at a boundary.
    ///
    /// Applied automatically by `cursor_at` and `set_position` so a
    /// caller passing an arbitrary scalar index never lands inside a
    /// cluster — without this, a round-trip such as
    /// `NextCharacter → PreviousCharacter` would stop at the cluster
    /// start rather than the start position, because the pre-advance
    /// state wasn't a boundary to begin with.
    pub(crate) fn snap_position_to_grapheme_boundary(&self) {
        let pos = {
            let data = self.data.lock();
            data.position
        };
        let snapped = self.forward_grapheme_boundary_at_or_after(pos);
        if snapped != pos {
            let mut data = self.data.lock();
            data.position = snapped;
            if data.anchor == pos {
                data.anchor = snapped;
            }
        }
    }

    /// Return `pos` if it sits at a grapheme cluster boundary within
    /// its block; otherwise return the end position of the containing
    /// cluster (snap forward). Block separators are always treated as
    /// boundaries.
    ///
    /// Leaves out-of-range positions (`pos > max_cursor_position`)
    /// unchanged — the snap must never silently upgrade an out-of-
    /// range cursor to a valid one, because edit ops rely on the
    /// out-of-range check to stay no-ops.
    fn forward_grapheme_boundary_at_or_after(&self, pos: usize) -> usize {
        let inner = self.doc.lock();
        let end = document_inspection_commands::get_document_stats(&inner.ctx)
            .map(|s| max_cursor_position(&s))
            .unwrap_or(pos);
        if pos >= end {
            return pos;
        }
        let block_dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let Ok(block_info) =
            document_inspection_commands::get_block_at_position(&inner.ctx, &block_dto)
        else {
            return pos;
        };
        let block_start = to_usize(block_info.block_start);
        let block_length = to_usize(block_info.block_length);
        let offset_in_block = pos.saturating_sub(block_start);
        // Block boundaries (start / end) are always cluster boundaries.
        if offset_in_block == 0 || offset_in_block >= block_length {
            return pos;
        }
        let text_dto = frontend::document_inspection::GetTextAtPositionDto {
            position: to_i64(block_start),
            length: to_i64(block_length),
        };
        let Ok(r) = document_inspection_commands::get_text_at_position(&inner.ctx, &text_dto)
        else {
            return pos;
        };
        let text = r.text;
        drop(inner);
        // Walk grapheme clusters, accumulating char counts. The first
        // boundary >= offset_in_block is the snap target.
        let mut acc = 0usize;
        for g in text.graphemes(true) {
            if acc >= offset_in_block {
                return block_start + acc;
            }
            acc += g.chars().count();
        }
        block_start + acc
    }

    /// Return the cursor position after advancing one extended grapheme
    /// cluster from `pos`. A grapheme cluster is what a user perceives
    /// as a single character — decomposed accents (`e` + `U+0301`),
    /// skin-tone emoji, ZWJ sequences, and regional-indicator flags
    /// are all single clusters even though they contain multiple
    /// Unicode scalars.
    ///
    /// Block separators (the single scalar between blocks in the
    /// cursor-position space) are treated as their own unit: advancing
    /// from the end of a block goes to the start of the next block
    /// (one scalar forward) without touching the grapheme path.
    /// Returns `pos` unchanged when already at the document end.
    fn next_grapheme_boundary(&self, pos: usize) -> usize {
        let inner = self.doc.lock();
        let end = document_inspection_commands::get_document_stats(&inner.ctx)
            .map(|s| max_cursor_position(&s))
            .unwrap_or(pos);
        if pos >= end {
            return pos;
        }
        let block_dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let block_info =
            match document_inspection_commands::get_block_at_position(&inner.ctx, &block_dto) {
                Ok(info) => info,
                Err(_) => return pos + 1,
            };
        let block_start = to_usize(block_info.block_start);
        let block_length = to_usize(block_info.block_length);
        let offset_in_block = pos.saturating_sub(block_start);
        if offset_in_block >= block_length {
            // At block end — advance across the separator into the
            // next block.
            return (pos + 1).min(end);
        }
        let text_dto = frontend::document_inspection::GetTextAtPositionDto {
            position: to_i64(pos),
            length: to_i64(block_length - offset_in_block),
        };
        let text = match document_inspection_commands::get_text_at_position(&inner.ctx, &text_dto) {
            Ok(r) => r.text,
            Err(_) => return pos + 1,
        };
        drop(inner);
        match text.graphemes(true).next() {
            Some(g) if !g.is_empty() => (pos + g.chars().count()).min(end),
            _ => (pos + 1).min(end),
        }
    }

    /// Return the cursor position before the extended grapheme cluster
    /// that ends at `pos`. Counterpart to [`Self::next_grapheme_boundary`].
    /// Crosses block separators one scalar at a time.
    fn prev_grapheme_boundary(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        let inner = self.doc.lock();
        let block_dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos.saturating_sub(1)),
        };
        let block_info =
            match document_inspection_commands::get_block_at_position(&inner.ctx, &block_dto) {
                Ok(info) => info,
                Err(_) => return pos - 1,
            };
        let block_start = to_usize(block_info.block_start);
        let block_length = to_usize(block_info.block_length);
        let block_end = block_start + block_length;
        // If `pos` sits past the block text (on a separator), step back
        // one scalar rather than running grapheme analysis across a
        // boundary.
        if pos > block_end {
            return pos - 1;
        }
        if block_length == 0 || pos <= block_start {
            return pos.saturating_sub(1);
        }
        let scan_len = pos - block_start;
        let text_dto = frontend::document_inspection::GetTextAtPositionDto {
            position: to_i64(block_start),
            length: to_i64(scan_len),
        };
        let text = match document_inspection_commands::get_text_at_position(&inner.ctx, &text_dto) {
            Ok(r) => r.text,
            Err(_) => return pos - 1,
        };
        drop(inner);
        match text.graphemes(true).next_back() {
            Some(g) if !g.is_empty() => pos - g.chars().count(),
            _ => pos - 1,
        }
    }

    /// Find the word boundaries around `pos`. Returns (start, end).
    /// Uses Unicode word segmentation for correct handling of non-ASCII text.
    ///
    /// Single-pass: tracks the last word seen to avoid a second iteration
    /// when the cursor is at the end of the last word (ISSUE-18).
    fn find_word_boundaries(&self, pos: usize) -> (usize, usize) {
        let inner = self.doc.lock();
        // Get block info so we can fetch the full block text
        let block_dto = frontend::document_inspection::GetBlockAtPositionDto {
            position: to_i64(pos),
        };
        let block_info =
            match document_inspection_commands::get_block_at_position(&inner.ctx, &block_dto) {
                Ok(info) => info,
                Err(_) => return (pos, pos),
            };

        let block_start = to_usize(block_info.block_start);
        let block_length = to_usize(block_info.block_length);
        if block_length == 0 {
            return (pos, pos);
        }

        let dto = frontend::document_inspection::GetTextAtPositionDto {
            position: to_i64(block_start),
            length: to_i64(block_length),
        };
        let text = match document_inspection_commands::get_text_at_position(&inner.ctx, &dto) {
            Ok(r) => r.text,
            Err(_) => return (pos, pos),
        };

        // cursor_offset is the char offset within the block text
        let cursor_offset = pos.saturating_sub(block_start);

        // Single pass: track the last word seen for end-of-last-word check
        let mut last_char_start = 0;
        let mut last_char_end = 0;

        for (word_byte_start, word) in text.unicode_word_indices() {
            // Convert byte offset to char offset
            let word_char_start = text[..word_byte_start].chars().count();
            let word_char_len = word.chars().count();
            let word_char_end = word_char_start + word_char_len;

            last_char_start = word_char_start;
            last_char_end = word_char_end;

            if cursor_offset >= word_char_start && cursor_offset < word_char_end {
                return (block_start + word_char_start, block_start + word_char_end);
            }
        }

        // Check if cursor is exactly at the end of the last word
        if cursor_offset == last_char_end && last_char_start < last_char_end {
            return (block_start + last_char_start, block_start + last_char_end);
        }

        (pos, pos)
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Frame-awareness helpers used by the public Cursor methods above.
// Each takes the locked TextDocumentInner so callers can reuse one
// store snapshot.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockEdge {
    First,
    Middle,
    Last,
    OnlyOne,
}

/// Build a `FrameRef` for the innermost non-root frame containing
/// `block_id`. Returns `None` if the block sits directly in the root
/// frame (i.e. the only enclosing frame is the root).
fn cursor_frame_ref(inner: &TextDocumentInner, block_id: u64) -> Option<FrameRef> {
    let parent = crate::text_block::find_parent_frame(inner, block_id)?;
    let store = inner.ctx.db_context.get_store();
    let frames = store.frames.read();
    let frame = frames.get(&parent)?.clone();
    frame.parent_frame?;
    let is_blockquote = frame.fmt_is_blockquote.unwrap_or(false);

    let mut depth = 0;
    let mut current = Some(parent);
    while let Some(id) = current {
        let Some(f) = frames.get(&id) else {
            break;
        };
        if f.parent_frame.is_none() {
            break;
        }
        depth += 1;
        current = f.parent_frame;
    }

    Some(FrameRef {
        frame_id: frame.id as usize,
        parent_frame_id: frame.parent_frame.map(|id| id as usize),
        is_blockquote,
        depth,
    })
}

/// Walk up the parent_frame chain from the block's immediate parent and
/// return the first blockquote frame found (innermost). `None` if no
/// enclosing frame is a blockquote.
fn innermost_blockquote_frame_id(inner: &TextDocumentInner, block_id: u64) -> Option<usize> {
    let mut current = crate::text_block::find_parent_frame(inner, block_id);
    let store = inner.ctx.db_context.get_store();
    let frames = store.frames.read();
    while let Some(id) = current {
        let f = frames.get(&id)?;
        if f.fmt_is_blockquote == Some(true) {
            return Some(f.id as usize);
        }
        current = f.parent_frame;
    }
    None
}

/// Count how many blockquote frames sit on the parent_frame chain above
/// `block_id`. 0 if no enclosing frame is a blockquote.
fn blockquote_depth_for_block(inner: &TextDocumentInner, block_id: u64) -> usize {
    let mut current = crate::text_block::find_parent_frame(inner, block_id);
    let store = inner.ctx.db_context.get_store();
    let frames = store.frames.read();
    let mut count = 0;
    while let Some(id) = current {
        let Some(f) = frames.get(&id) else {
            break;
        };
        if f.fmt_is_blockquote == Some(true) {
            count += 1;
        }
        current = f.parent_frame;
    }
    count
}

/// Resolve the cursor's block, find its immediate parent frame, and
/// determine the block's edge position within that frame's `child_order`
/// (counting only positive entries — sub-frames are skipped because they
/// are structurally different elements). Returns `None` if the cursor's
/// block has no entry in any frame's `child_order`.
fn block_position_in_current_frame(cursor: &TextCursor) -> Option<BlockEdge> {
    let pos = cursor.position();
    let inner = cursor.doc.lock();
    let dto = frontend::document_inspection::GetBlockAtPositionDto {
        position: to_i64(pos),
    };
    let block_info = document_inspection_commands::get_block_at_position(&inner.ctx, &dto).ok()?;
    let block_id = block_info.block_id as common::types::EntityId;
    let parent_id = crate::text_block::find_parent_frame(&inner, block_info.block_id as u64)?;
    let store = inner.ctx.db_context.get_store();
    let frames = store.frames.read();
    let frame = frames.get(&parent_id)?;
    let block_positions: Vec<usize> = frame
        .child_order
        .iter()
        .enumerate()
        .filter_map(|(i, &e)| {
            if e > 0 {
                Some((i, e as common::types::EntityId))
            } else {
                None
            }
        })
        .filter(|(_, id)| *id == block_id)
        .map(|(i, _)| i)
        .collect();
    let block_idx = *block_positions.first()?;
    let positive_entries: Vec<usize> = frame
        .child_order
        .iter()
        .enumerate()
        .filter_map(|(i, &e)| if e > 0 { Some(i) } else { None })
        .collect();
    let first_pos = *positive_entries.first()?;
    let last_pos = *positive_entries.last()?;
    let is_first = block_idx == first_pos;
    let is_last = block_idx == last_pos;
    let edge = match (is_first, is_last, positive_entries.len()) {
        (_, _, 1) => BlockEdge::OnlyOne,
        (true, _, _) => BlockEdge::First,
        (_, true, _) => BlockEdge::Last,
        _ => BlockEdge::Middle,
    };
    Some(edge)
}
