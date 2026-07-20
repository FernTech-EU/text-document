//! Conversion helpers between public API types and backend DTOs.
//!
//! The backend uses `i64` for all positions/sizes. The public API uses `usize`.
//! All Option mapping between public format structs and backend DTOs lives here.

use crate::{
    BlockFormat, BlockInfo, DocumentStats, FindMatch, FindOptions, FrameFormat, ListFormat,
    TextFormat,
};

// ── Position conversion ─────────────────────────────────────────

pub fn to_i64(v: usize) -> i64 {
    debug_assert!(v <= i64::MAX as usize, "position overflow: {v}");
    v as i64
}

pub fn to_usize(v: i64) -> usize {
    assert!(v >= 0, "negative position: {v}");
    v as usize
}

// ── DocumentStats ───────────────────────────────────────────────

impl From<&frontend::document_inspection::DocumentStatsDto> for DocumentStats {
    fn from(dto: &frontend::document_inspection::DocumentStatsDto) -> Self {
        Self {
            character_count: to_usize(dto.character_count),
            word_count: to_usize(dto.word_count),
            block_count: to_usize(dto.block_count),
            frame_count: to_usize(dto.frame_count),
            image_count: to_usize(dto.image_count),
            list_count: to_usize(dto.list_count),
            table_count: to_usize(dto.table_count),
        }
    }
}

// ── BlockInfo ───────────────────────────────────────────────────

impl From<&frontend::document_inspection::BlockInfoDto> for BlockInfo {
    fn from(dto: &frontend::document_inspection::BlockInfoDto) -> Self {
        Self {
            block_id: to_usize(dto.block_id),
            block_number: to_usize(dto.block_number),
            start: to_usize(dto.block_start),
            length: to_usize(dto.block_length),
        }
    }
}

// ── FindMatch / FindOptions ─────────────────────────────────────

impl FindOptions {
    pub(crate) fn to_find_text_dto(
        &self,
        query: &str,
        start_position: usize,
    ) -> frontend::document_search::FindTextDto {
        frontend::document_search::FindTextDto {
            query: query.into(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            diacritic_sensitive: self.diacritic_sensitive,
            language: self.language.clone(),
            use_regex: self.use_regex,
            search_backward: self.search_backward,
            start_position: to_i64(start_position),
        }
    }

    pub(crate) fn to_find_all_dto(&self, query: &str) -> frontend::document_search::FindAllDto {
        frontend::document_search::FindAllDto {
            query: query.into(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            diacritic_sensitive: self.diacritic_sensitive,
            language: self.language.clone(),
            use_regex: self.use_regex,
        }
    }
}

impl crate::ReplaceOptions {
    /// The typed ranges become the DTO's three PARALLEL lists — a list of structs is not
    /// expressible there (`FindAllResultDto` uses the same idiom). Built in one pass so the
    /// three can never come out of step, which would make range `i` address the wrong text.
    pub(crate) fn to_replace_ranges_dto(
        &self,
        ranges: &[crate::ReplaceRange],
    ) -> frontend::document_search::ReplaceRangesDto {
        let mut positions = Vec::with_capacity(ranges.len());
        let mut lengths = Vec::with_capacity(ranges.len());
        let mut replacements = Vec::with_capacity(ranges.len());
        for range in ranges {
            positions.push(to_i64(range.position));
            lengths.push(to_i64(range.length));
            replacements.push(range.replacement.clone());
        }
        frontend::document_search::ReplaceRangesDto {
            positions,
            lengths,
            replacements,
            format_policy: self.format_policy,
        }
    }

    pub(crate) fn to_replace_dto(
        &self,
        query: &str,
        replacement: &str,
        replace_all: bool,
    ) -> frontend::document_search::ReplaceTextDto {
        frontend::document_search::ReplaceTextDto {
            query: query.into(),
            replacement: replacement.into(),
            case_sensitive: self.find.case_sensitive,
            whole_word: self.find.whole_word,
            diacritic_sensitive: self.find.diacritic_sensitive,
            language: self.find.language.clone(),
            use_regex: self.find.use_regex,
            replace_all,
            format_policy: self.format_policy,
        }
    }
}

pub fn find_result_to_match(dto: &frontend::document_search::FindResultDto) -> Option<FindMatch> {
    if dto.found {
        Some(FindMatch {
            position: to_usize(dto.position),
            length: to_usize(dto.length),
            matched_text: dto.matched_text.clone(),
        })
    } else {
        None
    }
}

/// The three lists are parallel by construction (one `FindAllResultDto` is built in one
/// pass), so `zip` is safe — but zip *truncates*, and a truncated match list is a search that
/// silently loses its last hits. Assert instead.
pub fn find_all_to_matches(dto: &frontend::document_search::FindAllResultDto) -> Vec<FindMatch> {
    debug_assert_eq!(dto.positions.len(), dto.lengths.len());
    debug_assert_eq!(dto.positions.len(), dto.matched_texts.len());
    dto.positions
        .iter()
        .zip(dto.lengths.iter())
        .zip(dto.matched_texts.iter())
        .map(|((&pos, &len), text)| FindMatch {
            position: to_usize(pos),
            length: to_usize(len),
            matched_text: text.clone(),
        })
        .collect()
}

// ── Domain ↔ DTO enum conversions ───────────────────────────────
//
// The DTO layer has its own enum types, separate from domain enums
// in `common::entities`. This keeps the API boundary stable even
// when domain internals change.

// Formatting DTOs have their own enum types (separate from entity DTO enums).
// These conversion functions bridge the two at the public API boundary.
use frontend::document_formatting::dtos as fmt_dto;

fn underline_style_to_dto(v: &crate::UnderlineStyle) -> fmt_dto::UnderlineStyle {
    match v {
        crate::UnderlineStyle::NoUnderline => fmt_dto::UnderlineStyle::NoUnderline,
        crate::UnderlineStyle::SingleUnderline => fmt_dto::UnderlineStyle::SingleUnderline,
        crate::UnderlineStyle::DashUnderline => fmt_dto::UnderlineStyle::DashUnderline,
        crate::UnderlineStyle::DotLine => fmt_dto::UnderlineStyle::DotLine,
        crate::UnderlineStyle::DashDotLine => fmt_dto::UnderlineStyle::DashDotLine,
        crate::UnderlineStyle::DashDotDotLine => fmt_dto::UnderlineStyle::DashDotDotLine,
        crate::UnderlineStyle::WaveUnderline => fmt_dto::UnderlineStyle::WaveUnderline,
        crate::UnderlineStyle::SpellCheckUnderline => fmt_dto::UnderlineStyle::SpellCheckUnderline,
    }
}

fn vertical_alignment_to_dto(v: &crate::CharVerticalAlignment) -> fmt_dto::CharVerticalAlignment {
    match v {
        crate::CharVerticalAlignment::Normal => fmt_dto::CharVerticalAlignment::Normal,
        crate::CharVerticalAlignment::SuperScript => fmt_dto::CharVerticalAlignment::SuperScript,
        crate::CharVerticalAlignment::SubScript => fmt_dto::CharVerticalAlignment::SubScript,
        crate::CharVerticalAlignment::Middle => fmt_dto::CharVerticalAlignment::Middle,
        crate::CharVerticalAlignment::Bottom => fmt_dto::CharVerticalAlignment::Bottom,
        crate::CharVerticalAlignment::Top => fmt_dto::CharVerticalAlignment::Top,
        crate::CharVerticalAlignment::Baseline => fmt_dto::CharVerticalAlignment::Baseline,
    }
}

fn alignment_to_dto(v: &crate::Alignment) -> fmt_dto::Alignment {
    match v {
        crate::Alignment::Left => fmt_dto::Alignment::Left,
        crate::Alignment::Right => fmt_dto::Alignment::Right,
        crate::Alignment::Center => fmt_dto::Alignment::Center,
        crate::Alignment::Justify => fmt_dto::Alignment::Justify,
    }
}

fn marker_to_dto(v: &crate::MarkerType) -> fmt_dto::MarkerType {
    match v {
        crate::MarkerType::NoMarker => fmt_dto::MarkerType::NoMarker,
        crate::MarkerType::Unchecked => fmt_dto::MarkerType::Unchecked,
        crate::MarkerType::Checked => fmt_dto::MarkerType::Checked,
    }
}

// ── TextFormat → SetTextFormatDto ───────────────────────────────
//
// Backend DTOs now use `Option` fields: `None` means "don't change
// this property" and `Some(value)` means "set to value".

impl TextFormat {
    pub(crate) fn to_set_dto(
        &self,
        position: usize,
        anchor: usize,
    ) -> frontend::document_formatting::SetTextFormatDto {
        frontend::document_formatting::SetTextFormatDto {
            position: to_i64(position),
            anchor: to_i64(anchor),
            font_family: self.font_family.clone(),
            font_point_size: self.font_point_size.map(|v| v as i64),
            font_weight: self.font_weight.map(|v| v as i64),
            font_bold: self.font_bold,
            font_italic: self.font_italic,
            font_underline: self.font_underline,
            font_overline: self.font_overline,
            font_strikeout: self.font_strikeout,
            letter_spacing: self.letter_spacing.map(|v| v as i64),
            word_spacing: self.word_spacing.map(|v| v as i64),
            underline_style: self.underline_style.as_ref().map(underline_style_to_dto),
            vertical_alignment: self
                .vertical_alignment
                .as_ref()
                .map(vertical_alignment_to_dto),
        }
    }

    pub(crate) fn to_merge_dto(
        &self,
        position: usize,
        anchor: usize,
    ) -> frontend::document_formatting::MergeTextFormatDto {
        frontend::document_formatting::MergeTextFormatDto {
            position: to_i64(position),
            anchor: to_i64(anchor),
            font_family: self.font_family.clone(),
            font_bold: self.font_bold,
            font_italic: self.font_italic,
            font_underline: self.font_underline,
            font_strikeout: self.font_strikeout,
            vertical_alignment: self
                .vertical_alignment
                .as_ref()
                .map(vertical_alignment_to_dto),
        }
    }
}

// ── CharacterFormat (Phase 1 format_runs) → TextFormat ─────────

impl From<&frontend::common::format_runs::CharacterFormat> for TextFormat {
    fn from(fmt: &frontend::common::format_runs::CharacterFormat) -> Self {
        Self {
            font_family: fmt.font_family.clone(),
            font_point_size: fmt.font_point_size.map(|v| v as u32),
            font_weight: fmt.font_weight.map(|v| v as u32),
            font_bold: fmt.font_bold,
            font_italic: fmt.font_italic,
            font_underline: fmt.font_underline,
            font_overline: fmt.font_overline,
            font_strikeout: fmt.font_strikeout,
            letter_spacing: fmt.letter_spacing.map(|v| v as i32),
            word_spacing: fmt.word_spacing.map(|v| v as i32),
            underline_style: fmt.underline_style.clone(),
            vertical_alignment: fmt.vertical_alignment.clone(),
            anchor_href: fmt.anchor_href.clone(),
            anchor_names: fmt.anchor_names.clone(),
            is_anchor: fmt.is_anchor,
            tooltip: fmt.tooltip.clone(),
            foreground_color: None,
            background_color: None,
            underline_color: None,
        }
    }
}

// ── BlockFormat ─────────────────────────────────────────────────

impl BlockFormat {
    pub(crate) fn to_set_dto(
        &self,
        position: usize,
        anchor: usize,
    ) -> frontend::document_formatting::SetBlockFormatDto {
        frontend::document_formatting::SetBlockFormatDto {
            position: to_i64(position),
            anchor: to_i64(anchor),
            alignment: self.alignment.as_ref().map(alignment_to_dto),
            heading_level: self.heading_level.map(|v| v as i64),
            indent: self.indent.map(|v| v as i64),
            marker: self.marker.as_ref().map(marker_to_dto),
            line_height: self.line_height.map(|v| (v * 1000.0) as i64),
            non_breakable_lines: self.non_breakable_lines,
            direction: self.direction.clone(),
            background_color: self.background_color.clone(),
            is_code_block: self.is_code_block,
            code_language: self.code_language.clone(),
            hyphenate: self.hyphenate,
            language: self.language.clone(),
            top_margin: self.top_margin.map(|v| v as i64),
            bottom_margin: self.bottom_margin.map(|v| v as i64),
            left_margin: self.left_margin.map(|v| v as i64),
            right_margin: self.right_margin.map(|v| v as i64),
            text_indent: self.text_indent.map(|v| v as i64),
        }
    }
}

impl From<&frontend::block::dtos::BlockDto> for BlockFormat {
    fn from(b: &frontend::block::dtos::BlockDto) -> Self {
        Self {
            alignment: b.fmt_alignment.clone(),
            top_margin: b.fmt_top_margin.map(|v| v as i32),
            bottom_margin: b.fmt_bottom_margin.map(|v| v as i32),
            left_margin: b.fmt_left_margin.map(|v| v as i32),
            right_margin: b.fmt_right_margin.map(|v| v as i32),
            heading_level: b.fmt_heading_level.map(|v| v as u8),
            indent: b.fmt_indent.map(|v| v as u8),
            text_indent: b.fmt_text_indent.map(|v| v as i32),
            marker: b.fmt_marker.clone(),
            tab_positions: b.fmt_tab_positions.iter().map(|&v| v as i32).collect(),
            line_height: b.fmt_line_height.map(|v| v as f32 / 1000.0),
            non_breakable_lines: b.fmt_non_breakable_lines,
            direction: b.fmt_direction.clone(),
            background_color: b.fmt_background_color.clone(),
            is_code_block: b.fmt_is_code_block,
            code_language: b.fmt_code_language.clone(),
            hyphenate: b.fmt_hyphenate,
            language: b.fmt_language.clone(),
        }
    }
}

// ── FrameFormat ─────────────────────────────────────────────────

impl FrameFormat {
    pub(crate) fn to_set_dto(
        &self,
        position: usize,
        anchor: usize,
        frame_id: usize,
    ) -> frontend::document_formatting::SetFrameFormatDto {
        frontend::document_formatting::SetFrameFormatDto {
            position: to_i64(position),
            anchor: to_i64(anchor),
            frame_id: to_i64(frame_id),
            height: self.height.map(|v| v as i64),
            width: self.width.map(|v| v as i64),
            top_margin: self.top_margin.map(|v| v as i64),
            bottom_margin: self.bottom_margin.map(|v| v as i64),
            left_margin: self.left_margin.map(|v| v as i64),
            right_margin: self.right_margin.map(|v| v as i64),
            padding: self.padding.map(|v| v as i64),
            border: self.border.map(|v| v as i64),
            is_blockquote: self.is_blockquote,
        }
    }
}

// ── ListFormat ─────────────────────────────────────────────────

impl ListFormat {
    pub(crate) fn to_set_dto(
        &self,
        list_id: usize,
    ) -> frontend::document_formatting::SetListFormatDto {
        frontend::document_formatting::SetListFormatDto {
            list_id: to_i64(list_id),
            style: self.style.clone(),
            indent: self.indent.map(|v| v as i64),
            prefix: self.prefix.clone(),
            suffix: self.suffix.clone(),
        }
    }
}

// ── TableFormat ────────────────────────────────────────────────

impl crate::flow::TableFormat {
    pub(crate) fn to_set_dto(
        &self,
        table_id: usize,
    ) -> frontend::document_formatting::SetTableFormatDto {
        frontend::document_formatting::SetTableFormatDto {
            table_id: to_i64(table_id),
            border: self.border.map(|v| v as i64),
            cell_spacing: self.cell_spacing.map(|v| v as i64),
            cell_padding: self.cell_padding.map(|v| v as i64),
            width: self.width.map(|v| v as i64),
            alignment: self.alignment.as_ref().map(alignment_to_dto),
        }
    }
}

// ── CellFormat ─────────────────────────────────────────────────

fn cell_vertical_alignment_to_dto(
    v: &crate::flow::CellVerticalAlignment,
) -> fmt_dto::CellVerticalAlignment {
    match v {
        crate::flow::CellVerticalAlignment::Top => fmt_dto::CellVerticalAlignment::Top,
        crate::flow::CellVerticalAlignment::Middle => fmt_dto::CellVerticalAlignment::Middle,
        crate::flow::CellVerticalAlignment::Bottom => fmt_dto::CellVerticalAlignment::Bottom,
    }
}

impl crate::flow::CellFormat {
    pub(crate) fn to_set_dto(
        &self,
        cell_id: usize,
    ) -> frontend::document_formatting::SetTableCellFormatDto {
        frontend::document_formatting::SetTableCellFormatDto {
            cell_id: to_i64(cell_id),
            padding: self.padding.map(|v| v as i64),
            border: self.border.map(|v| v as i64),
            vertical_alignment: self
                .vertical_alignment
                .as_ref()
                .map(cell_vertical_alignment_to_dto),
            background_color: self.background_color.clone(),
        }
    }
}
