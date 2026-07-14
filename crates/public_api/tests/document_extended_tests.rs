use text_document::{
    Alignment, BlockFormat, FindOptions, ResourceType, TextDirection, TextDocument, TextFormat,
    WrapMode,
};

fn new_doc(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

// ── Block format at position ────────────────────────────────────

#[test]
fn block_format_at_position() {
    let doc = new_doc("Hello\nWorld");
    let fmt = doc.block_format_at(0).unwrap();
    assert_eq!(fmt, text_document::BlockFormat::default());
}

#[test]
fn block_format_at_second_block() {
    let doc = new_doc("First\nSecond");
    let fmt = doc.block_format_at(6).unwrap();
    assert_eq!(fmt, text_document::BlockFormat::default());
}

// ── Resources ───────────────────────────────────────────────────

#[test]
fn add_and_retrieve_resource() {
    let doc = new_doc("Hello");
    doc.add_resource(
        ResourceType::Image,
        "test.png",
        "image/png",
        b"fake-png-data",
    )
    .unwrap();
    let data = doc.resource("test.png").unwrap();
    assert!(data.is_some());
    assert_eq!(data.unwrap(), b"fake-png-data");
}

#[test]
fn resource_not_found() {
    let doc = new_doc("Hello");
    let data = doc.resource("nonexistent.png").unwrap();
    assert!(data.is_none());
}

#[test]
fn add_multiple_resources() {
    let doc = new_doc("Hello");
    doc.add_resource(ResourceType::Image, "a.png", "image/png", b"aaa")
        .unwrap();
    doc.add_resource(ResourceType::StyleSheet, "style.css", "text/css", b"body{}")
        .unwrap();
    assert!(doc.resource("a.png").unwrap().is_some());
    assert!(doc.resource("style.css").unwrap().is_some());
}

// ── Title ───────────────────────────────────────────────────────

#[test]
fn set_and_get_title() {
    let doc = new_doc("Hello");
    doc.set_title("My Document").unwrap();
    assert_eq!(doc.title(), "My Document");
}

#[test]
fn default_title_is_empty() {
    let doc = TextDocument::new();
    assert!(doc.title().is_empty());
}

// ── Text direction ──────────────────────────────────────────────

#[test]
fn text_direction_default_ltr() {
    let doc = TextDocument::new();
    assert_eq!(doc.text_direction(), TextDirection::LeftToRight);
}

#[test]
fn set_text_direction_rtl() {
    let doc = new_doc("Hello");
    doc.set_text_direction(TextDirection::RightToLeft).unwrap();
    assert_eq!(doc.text_direction(), TextDirection::RightToLeft);
}

// ── Wrap mode ───────────────────────────────────────────────────

#[test]
fn wrap_mode_default() {
    let doc = TextDocument::new();
    assert_eq!(doc.default_wrap_mode(), WrapMode::NoWrap);
}

#[test]
fn set_wrap_mode() {
    let doc = new_doc("Hello");
    doc.set_default_wrap_mode(WrapMode::NoWrap).unwrap();
    assert_eq!(doc.default_wrap_mode(), WrapMode::NoWrap);
}

#[test]
fn set_wrap_mode_all_variants() {
    let doc = new_doc("Hello");
    for mode in [
        WrapMode::NoWrap,
        WrapMode::WordWrap,
        WrapMode::WrapAnywhere,
        WrapMode::WrapAtWordBoundaryOrAnywhere,
    ] {
        doc.set_default_wrap_mode(mode.clone()).unwrap();
        assert_eq!(doc.default_wrap_mode(), mode);
    }
}

// ── Text format with full fields ────────────────────────────────

#[test]
fn set_text_format_full() {
    let doc = new_doc("Hello world");
    let c = doc.cursor();
    c.set_position(5, text_document::MoveMode::KeepAnchor);
    let fmt = TextFormat {
        font_family: Some("Courier".into()),
        font_point_size: Some(12),
        font_weight: Some(700),
        font_bold: Some(true),
        font_italic: Some(true),
        font_underline: Some(true),
        font_overline: Some(true),
        font_strikeout: Some(true),
        letter_spacing: Some(2),
        word_spacing: Some(3),
        underline_style: Some(text_document::UnderlineStyle::WaveUnderline),
        vertical_alignment: Some(text_document::CharVerticalAlignment::SuperScript),
        anchor_href: None,
        anchor_names: vec![],
        is_anchor: None,
        tooltip: None,
        foreground_color: None,
        background_color: None,
        underline_color: None,
    };
    c.set_char_format(&fmt).unwrap();
}

// ── Block format with full fields ───────────────────────────────

#[test]
fn set_block_format_full() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    let fmt = BlockFormat {
        alignment: Some(Alignment::Center),
        heading_level: Some(2),
        indent: Some(1),
        marker: Some(text_document::MarkerType::Checked),
        ..Default::default()
    };
    c.set_block_format(&fmt).unwrap();
}

// ── New BlockFormat fields ──────────────────────────────────────

#[test]
fn set_block_format_line_height() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    let fmt = BlockFormat {
        line_height: Some(1.5),
        ..Default::default()
    };
    c.set_block_format(&fmt).unwrap();
    let read = c.block_format().unwrap();
    assert_eq!(read.line_height, Some(1.5));
}

#[test]
fn set_block_format_non_breakable_lines() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    let fmt = BlockFormat {
        non_breakable_lines: Some(true),
        ..Default::default()
    };
    c.set_block_format(&fmt).unwrap();
    let read = c.block_format().unwrap();
    assert_eq!(read.non_breakable_lines, Some(true));
}

#[test]
fn set_block_format_direction_rtl() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    let fmt = BlockFormat {
        direction: Some(TextDirection::RightToLeft),
        ..Default::default()
    };
    c.set_block_format(&fmt).unwrap();
    let read = c.block_format().unwrap();
    assert_eq!(read.direction, Some(TextDirection::RightToLeft));
}

#[test]
fn set_block_format_background_color() {
    let doc = new_doc("Hello");
    let c = doc.cursor();
    let fmt = BlockFormat {
        background_color: Some("#ff0000".to_string()),
        ..Default::default()
    };
    c.set_block_format(&fmt).unwrap();
    let read = c.block_format().unwrap();
    assert_eq!(read.background_color, Some("#ff0000".to_string()));
}

#[test]
fn block_format_new_fields_default_none() {
    let doc = new_doc("Hello");
    let fmt = doc.block_format_at(0).unwrap();
    assert_eq!(fmt.line_height, None);
    assert_eq!(fmt.non_breakable_lines, None);
    assert_eq!(fmt.direction, None);
    assert_eq!(fmt.background_color, None);
}

// ── Import/export roundtrips ────────────────────────────────────

#[test]
fn plain_text_roundtrip() {
    let doc = new_doc("Hello\nWorld");
    let text = doc.to_plain_text().unwrap();
    assert_eq!(text, "Hello\nWorld");
}

// ── try_new ─────────────────────────────────────────────────────

#[test]
fn try_new_succeeds() {
    let doc = TextDocument::try_new().unwrap();
    assert!(doc.is_empty());
}

// ── Out-of-bounds queries ───────────────────────────────────────

#[test]
fn text_at_beyond_document_length() {
    let doc = new_doc("Hello");
    // Requesting more text than available should truncate to what's available
    let result = doc.text_at(0, 100).unwrap();
    assert_eq!(result, "Hello");
}

#[test]
fn text_at_position_beyond_end() {
    let doc = new_doc("Hello");
    let result = doc.text_at(100, 5);
    assert!(result.is_err());
}

#[test]
fn block_at_beyond_end() {
    let doc = new_doc("Hello");
    // Position beyond end should return the last block
    let result = doc.block_at(100).unwrap();
    assert_eq!(result.block_number, 0);
}

// ── ContentsChanged event payload ───────────────────────────────

#[test]
fn contents_changed_event_has_correct_payload() {
    use text_document::DocumentEvent;

    let doc = new_doc("Hello");
    doc.poll_events(); // drain setup events

    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();

    let events = doc.poll_events();
    let contents_event = events
        .iter()
        .find(|e| matches!(e, DocumentEvent::ContentsChanged { .. }));
    assert!(contents_event.is_some(), "expected ContentsChanged event");

    if let Some(DocumentEvent::ContentsChanged {
        position,
        chars_removed,
        chars_added,
        ..
    }) = contents_event
    {
        assert_eq!(*position, 5, "edit position should be 5");
        assert_eq!(*chars_removed, 0, "no chars removed on insert");
        assert_eq!(*chars_added, 6, "6 chars added (' world')");
    }
}

// ── ModificationChanged event payload ───────────────────────────

#[test]
fn modification_changed_event_payload() {
    use text_document::DocumentEvent;

    let doc = TextDocument::new();
    doc.poll_events(); // drain

    doc.set_modified(true);
    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::ModificationChanged(true))),
        "expected ModificationChanged(true), got: {:?}",
        events
    );

    doc.set_modified(false);
    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::ModificationChanged(false))),
        "expected ModificationChanged(false), got: {:?}",
        events
    );
}

// ── InlineContent enum ──────────────────────────────────────────

#[test]
fn inline_content_variants_are_accessible() {
    use text_document::InlineContent;

    let empty = InlineContent::Empty;
    let text = InlineContent::Text("hello".into());
    let image = InlineContent::Image {
        name: "img.png".into(),
        width: 100,
        height: 50,
        quality: 90,
    };

    // Verify pattern matching works
    assert!(matches!(empty, InlineContent::Empty));
    assert!(matches!(text, InlineContent::Text(_)));
    assert!(matches!(image, InlineContent::Image { .. }));
}

// ── replace_text single (not replace_all) ───────────────────────

#[test]
fn replace_text_single_occurrence() {
    let doc = new_doc("foo bar foo baz foo");
    let opts = FindOptions::default();
    let count = doc
        .replace_text(
            "foo",
            "X",
            false,
            &text_document::ReplaceOptions::new(opts.clone()),
        )
        .unwrap();
    assert_eq!(count, 1);
    let text = doc.to_plain_text().unwrap();
    assert!(text.starts_with("X bar"));
    // Only one replacement
    assert_eq!(text.matches("foo").count(), 2);
}
