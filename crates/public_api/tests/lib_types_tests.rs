use text_document::{
    Alignment, BlockFormat, BlockInfo, CharVerticalAlignment, DocumentFragment, DocumentStats,
    FindMatch, FindOptions, FrameFormat, FramePosition, ListStyle, MarkerType, MoveMode,
    MoveOperation, ResourceType, SelectionType, TextDirection, TextDocument, TextFormat,
    UnderlineStyle, WrapMode,
};

// ── Re-exported enums ────────────────────────────────────────────

#[test]
fn alignment_variants() {
    let _ = Alignment::Left;
    let _ = Alignment::Right;
    let _ = Alignment::Center;
    let _ = Alignment::Justify;
}

#[test]
fn text_direction_variants() {
    let _ = TextDirection::LeftToRight;
    let _ = TextDirection::RightToLeft;
}

#[test]
fn wrap_mode_variants() {
    let _ = WrapMode::NoWrap;
    let _ = WrapMode::WordWrap;
    let _ = WrapMode::WrapAnywhere;
    let _ = WrapMode::WrapAtWordBoundaryOrAnywhere;
}

#[test]
fn underline_style_variants() {
    let _ = UnderlineStyle::NoUnderline;
    let _ = UnderlineStyle::SingleUnderline;
    let _ = UnderlineStyle::DashUnderline;
    let _ = UnderlineStyle::DotLine;
    let _ = UnderlineStyle::DashDotLine;
    let _ = UnderlineStyle::DashDotDotLine;
    let _ = UnderlineStyle::WaveUnderline;
    let _ = UnderlineStyle::SpellCheckUnderline;
}

#[test]
fn char_vertical_alignment_variants() {
    let _ = CharVerticalAlignment::Normal;
    let _ = CharVerticalAlignment::SuperScript;
    let _ = CharVerticalAlignment::SubScript;
    let _ = CharVerticalAlignment::Middle;
    let _ = CharVerticalAlignment::Bottom;
    let _ = CharVerticalAlignment::Top;
    let _ = CharVerticalAlignment::Baseline;
}

#[test]
fn frame_position_variants() {
    let _ = FramePosition::InFlow;
    let _ = FramePosition::FloatLeft;
    let _ = FramePosition::FloatRight;
}

#[test]
fn list_style_variants() {
    let _ = ListStyle::Disc;
    let _ = ListStyle::Circle;
    let _ = ListStyle::Square;
    let _ = ListStyle::Decimal;
    let _ = ListStyle::LowerAlpha;
    let _ = ListStyle::UpperAlpha;
    let _ = ListStyle::LowerRoman;
    let _ = ListStyle::UpperRoman;
}

#[test]
fn marker_type_variants() {
    let _ = MarkerType::NoMarker;
    let _ = MarkerType::Unchecked;
    let _ = MarkerType::Checked;
}

#[test]
fn resource_type_variants() {
    let _ = ResourceType::Image;
    let _ = ResourceType::StyleSheet;
    let _ = ResourceType::Other;
}

// ── MoveMode ─────────────────────────────────────────────────────

#[test]
fn move_mode_debug_clone_eq() {
    let m1 = MoveMode::MoveAnchor;
    let m2 = MoveMode::KeepAnchor;
    assert_ne!(m1, m2);
    assert_eq!(m1, m1.clone());
    let _ = format!("{:?}", m1);
}

// ── MoveOperation ────────────────────────────────────────────────

#[test]
fn move_operation_debug_clone_eq() {
    let ops = [
        MoveOperation::NoMove,
        MoveOperation::Start,
        MoveOperation::End,
        MoveOperation::StartOfLine,
        MoveOperation::EndOfLine,
        MoveOperation::StartOfBlock,
        MoveOperation::EndOfBlock,
        MoveOperation::StartOfWord,
        MoveOperation::EndOfWord,
        MoveOperation::PreviousBlock,
        MoveOperation::NextBlock,
        MoveOperation::PreviousCharacter,
        MoveOperation::NextCharacter,
        MoveOperation::PreviousWord,
        MoveOperation::NextWord,
        MoveOperation::Up,
        MoveOperation::Down,
        MoveOperation::Left,
        MoveOperation::Right,
        MoveOperation::WordLeft,
        MoveOperation::WordRight,
    ];
    for op in &ops {
        let _ = format!("{:?}", op);
        assert_eq!(*op, op.clone());
    }
}

// ── SelectionType ────────────────────────────────────────────────

#[test]
fn selection_type_debug_clone_eq() {
    let types = [
        SelectionType::WordUnderCursor,
        SelectionType::LineUnderCursor,
        SelectionType::BlockUnderCursor,
        SelectionType::Document,
    ];
    for t in &types {
        let _ = format!("{:?}", t);
        assert_eq!(*t, t.clone());
    }
}

// ── TextFormat ───────────────────────────────────────────────────

#[test]
fn text_format_default() {
    let fmt = TextFormat::default();
    assert_eq!(fmt.font_family, None);
    assert_eq!(fmt.font_bold, None);
    assert_eq!(fmt.font_italic, None);
    assert_eq!(fmt.font_underline, None);
    assert_eq!(fmt.font_overline, None);
    assert_eq!(fmt.font_strikeout, None);
    assert_eq!(fmt.font_point_size, None);
    assert_eq!(fmt.font_weight, None);
    assert_eq!(fmt.letter_spacing, None);
    assert_eq!(fmt.word_spacing, None);
    assert_eq!(fmt.underline_style, None);
    assert_eq!(fmt.vertical_alignment, None);
    assert_eq!(fmt.anchor_href, None);
    assert!(fmt.anchor_names.is_empty());
    assert_eq!(fmt.is_anchor, None);
    assert_eq!(fmt.tooltip, None);
}

#[test]
fn text_format_clone_eq() {
    let fmt = TextFormat {
        font_bold: Some(true),
        font_family: Some("Arial".into()),
        ..Default::default()
    };
    let cloned = fmt.clone();
    assert_eq!(fmt, cloned);
    let _ = format!("{:?}", fmt);
}

// ── BlockFormat ──────────────────────────────────────────────────

#[test]
fn block_format_default() {
    let fmt = BlockFormat::default();
    assert_eq!(fmt.alignment, None);
    assert_eq!(fmt.heading_level, None);
    assert_eq!(fmt.indent, None);
    assert_eq!(fmt.marker, None);
    assert_eq!(fmt.top_margin, None);
    assert_eq!(fmt.bottom_margin, None);
    assert_eq!(fmt.left_margin, None);
    assert_eq!(fmt.right_margin, None);
    assert_eq!(fmt.text_indent, None);
    assert!(fmt.tab_positions.is_empty());
}

#[test]
fn block_format_clone_eq() {
    let fmt = BlockFormat {
        alignment: Some(Alignment::Center),
        heading_level: Some(2),
        ..Default::default()
    };
    assert_eq!(fmt, fmt.clone());
    let _ = format!("{:?}", fmt);
}

// ── FrameFormat ──────────────────────────────────────────────────

#[test]
fn frame_format_default() {
    let fmt = FrameFormat::default();
    assert_eq!(fmt.height, None);
    assert_eq!(fmt.width, None);
    assert_eq!(fmt.top_margin, None);
    assert_eq!(fmt.bottom_margin, None);
    assert_eq!(fmt.left_margin, None);
    assert_eq!(fmt.right_margin, None);
    assert_eq!(fmt.padding, None);
    assert_eq!(fmt.border, None);
    assert_eq!(fmt.position, None);
}

#[test]
fn frame_format_clone_eq() {
    let fmt = FrameFormat {
        width: Some(100),
        height: Some(200),
        position: Some(FramePosition::FloatLeft),
        ..Default::default()
    };
    assert_eq!(fmt, fmt.clone());
    let _ = format!("{:?}", fmt);
}

// ── DocumentStats ────────────────────────────────────────────────

#[test]
fn document_stats_debug_clone_eq() {
    let stats = DocumentStats {
        character_count: 10,
        word_count: 2,
        block_count: 1,
        frame_count: 1,
        image_count: 0,
        list_count: 0,
        table_count: 0,
    };
    assert_eq!(stats, stats.clone());
    let _ = format!("{:?}", stats);
}

// ── BlockInfo ────────────────────────────────────────────────────

#[test]
fn block_info_debug_clone_eq() {
    let info = BlockInfo {
        block_id: 1,
        block_number: 0,
        start: 0,
        length: 5,
    };
    assert_eq!(info, info.clone());
    let _ = format!("{:?}", info);
}

// ── FindMatch ────────────────────────────────────────────────────

#[test]
fn find_match_debug_clone_eq() {
    let m = FindMatch {
        position: 5,
        length: 3,
        matched_text: "café".to_string(),
    };
    assert_eq!(m, m.clone());
    let _ = format!("{:?}", m);
}

// ── FindOptions ──────────────────────────────────────────────────

#[test]
fn find_options_default() {
    let opts = FindOptions::default();
    assert!(!opts.case_sensitive);
    assert!(!opts.whole_word);
    assert!(!opts.use_regex);
    assert!(!opts.search_backward);
}

#[test]
fn find_options_debug_clone() {
    let opts = FindOptions {
        case_sensitive: true,
        whole_word: true,
        diacritic_sensitive: true,
        language: "tr".to_string(),
        use_regex: false,
        search_backward: true,
    };
    let cloned = opts.clone();
    assert_eq!(opts.case_sensitive, cloned.case_sensitive);
    let _ = format!("{:?}", opts);
}

// ── TextDocument: Default impl ───────────────────────────────────

#[test]
fn text_document_default() {
    let doc = TextDocument::default();
    assert!(doc.is_empty());
}

// ── Send + Sync assertions compile ──────────────────────────────

#[test]
fn text_document_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TextDocument>();
    assert_send_sync::<text_document::TextCursor>();
}

// ── Document properties ─────────────────────────────────────────

#[test]
fn document_block_count() {
    let doc = TextDocument::new();
    doc.set_plain_text("Line 1\nLine 2\nLine 3").unwrap();
    assert_eq!(doc.block_count(), 3);
}

#[test]
fn document_modified_no_double_emit() {
    let doc = TextDocument::new();
    doc.set_modified(true);
    doc.set_modified(true);
    assert!(doc.is_modified());
}

// ── DocumentFragment ────────────────────────────────────────────

#[test]
fn document_fragment_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<DocumentFragment>();
}
