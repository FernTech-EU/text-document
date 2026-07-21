use text_document::{Alignment, DocumentEvent, TextDocument};

fn new_doc_with_text(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    // Drain any events from setup
    doc.poll_events();
    doc
}

#[test]
fn poll_events_after_insert() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::ContentsChanged { .. })),
        "expected ContentsChanged event, got: {:?}",
        events
    );
}

#[test]
fn poll_events_drains() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();

    let events1 = doc.poll_events();
    assert!(!events1.is_empty());

    let events2 = doc.poll_events();
    assert!(events2.is_empty());
}

#[test]
fn poll_events_after_clear() {
    let doc = new_doc_with_text("Hello");
    doc.poll_events(); // drain setup events
    doc.clear().unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::DocumentReset)),
        "expected DocumentReset event, got: {:?}",
        events
    );
}

#[test]
fn modified_flag() {
    let doc = new_doc_with_text("Hello");
    assert!(!doc.is_modified());

    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();
    assert!(doc.is_modified());

    doc.set_modified(false);
    assert!(!doc.is_modified());
}

#[test]
fn content_revision_bumps_once_per_edit_and_ignores_non_content_events() {
    let doc = new_doc_with_text("Hello");
    let start = doc.content_revision();

    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();
    assert_eq!(
        doc.content_revision(),
        start + 1,
        "one content edit must bump the counter by exactly one"
    );

    // A non-content event (ModificationChanged, via set_modified) must not
    // move the counter — it exists specifically to answer "was there a
    // content edit", not "was there any event at all".
    doc.set_modified(false);
    assert_eq!(
        doc.content_revision(),
        start + 1,
        "a modified-flag-only event must not bump content_revision"
    );

    cursor.insert_text("!").unwrap();
    assert_eq!(
        doc.content_revision(),
        start + 2,
        "a second edit must bump the counter again"
    );
}

#[test]
fn on_change_callback_fires() {
    use std::sync::{Arc, Mutex};

    let doc = new_doc_with_text("Hello");
    let received = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();

    let _sub = doc.on_change(move |event| {
        received_clone.lock().unwrap().push(event);
    });

    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();

    let events = received.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::ContentsChanged { .. })),
        "callback should have received ContentsChanged, got: {:?}",
        *events
    );
}

#[test]
fn subscription_drop_stops_events() {
    use std::sync::{Arc, Mutex};

    let doc = new_doc_with_text("Hello");
    let received = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();

    let sub = doc.on_change(move |event| {
        received_clone.lock().unwrap().push(event);
    });

    drop(sub);

    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();

    let events = received.lock().unwrap();
    assert!(
        events.is_empty(),
        "no events should be received after dropping subscription, got: {:?}",
        *events
    );
}

// ── FormatChanged tests ─────────────────────────────────────

#[test]
fn format_changed_on_set_char_format() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor_at(0);
    cursor.move_position(
        text_document::MoveOperation::EndOfWord,
        text_document::MoveMode::KeepAnchor,
        1,
    );

    let format = text_document::TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    cursor.set_char_format(&format).unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::FormatChanged { .. })),
        "expected FormatChanged event, got: {:?}",
        events
    );
}

#[test]
fn format_changed_on_merge_char_format() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor_at(0);
    cursor.move_position(
        text_document::MoveOperation::EndOfWord,
        text_document::MoveMode::KeepAnchor,
        1,
    );

    let format = text_document::TextFormat {
        font_italic: Some(true),
        ..Default::default()
    };
    cursor.merge_char_format(&format).unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::FormatChanged { .. })),
        "expected FormatChanged event, got: {:?}",
        events
    );
}

#[test]
fn format_changed_on_set_block_format() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor_at(0);

    let format = text_document::BlockFormat {
        alignment: Some(Alignment::Center),
        ..Default::default()
    };
    cursor.set_block_format(&format).unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::FormatChanged { .. })),
        "expected FormatChanged event, got: {:?}",
        events
    );
}

// ── BlockCountChanged tests ─────────────────────────────────

#[test]
fn block_count_changed_on_insert_block() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_block().unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::BlockCountChanged(2))),
        "expected BlockCountChanged(2) event, got: {:?}",
        events
    );
}

#[test]
fn block_count_changed_on_set_plain_text_multiline() {
    let doc = TextDocument::new();
    doc.poll_events(); // drain setup
    doc.set_plain_text("line1\nline2\nline3").unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::BlockCountChanged(3))),
        "expected BlockCountChanged(3) event, got: {:?}",
        events
    );
}

#[test]
fn no_block_count_changed_when_count_unchanged() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();

    let events = doc.poll_events();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, DocumentEvent::BlockCountChanged(_))),
        "should not emit BlockCountChanged for same-block insert, got: {:?}",
        events
    );
}

// ── UndoRedoChanged tests ───────────────────────────────────

#[test]
fn undo_redo_changed_after_edit() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::UndoRedoChanged { can_undo: true, .. })),
        "expected UndoRedoChanged with can_undo=true after edit, got: {:?}",
        events
    );
}

#[test]
fn undo_redo_changed_after_undo() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();
    doc.poll_events(); // drain edit events

    doc.undo().unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::UndoRedoChanged { can_redo: true, .. })),
        "expected UndoRedoChanged with can_redo=true after undo, got: {:?}",
        events
    );
}

#[test]
fn undo_redo_changed_after_redo() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();
    doc.undo().unwrap();
    doc.poll_events(); // drain

    doc.redo().unwrap();

    let events = doc.poll_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            DocumentEvent::UndoRedoChanged {
                can_undo: true,
                can_redo: false
            }
        )),
        "expected UndoRedoChanged(can_undo=true, can_redo=false) after redo, got: {:?}",
        events
    );
}

#[test]
fn undo_redo_changed_after_set_plain_text() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();
    doc.poll_events(); // drain

    doc.set_plain_text("Reset").unwrap();

    let events = doc.poll_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            DocumentEvent::UndoRedoChanged {
                can_undo: false,
                can_redo: false
            }
        )),
        "expected UndoRedoChanged(false, false) after set_plain_text, got: {:?}",
        events
    );
}

#[test]
fn undo_redo_changed_after_formatting() {
    let doc = new_doc_with_text("Hello world");
    let cursor = doc.cursor_at(0);
    cursor.move_position(
        text_document::MoveOperation::EndOfWord,
        text_document::MoveMode::KeepAnchor,
        1,
    );

    let format = text_document::TextFormat {
        font_bold: Some(true),
        ..Default::default()
    };
    cursor.set_char_format(&format).unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::UndoRedoChanged { can_undo: true, .. })),
        "expected UndoRedoChanged with can_undo=true after formatting, got: {:?}",
        events
    );
}

// ── Undo/redo ContentsChanged / FormatChanged events ────────

#[test]
fn undo_emits_contents_changed() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();
    doc.poll_events(); // drain edit events

    doc.undo().unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::ContentsChanged { .. })),
        "undo should emit ContentsChanged, got: {:?}",
        events
    );
}

#[test]
fn redo_emits_contents_changed() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();
    doc.undo().unwrap();
    doc.poll_events(); // drain

    doc.redo().unwrap();

    let events = doc.poll_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DocumentEvent::ContentsChanged { .. })),
        "redo should emit ContentsChanged, got: {:?}",
        events
    );
}

#[test]
fn undo_contents_changed_reports_affected_region() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(5);
    cursor.insert_text(" world").unwrap();
    doc.poll_events(); // drain

    doc.undo().unwrap();

    let events = doc.poll_events();
    let content_event = events.iter().find_map(|e| match e {
        DocumentEvent::ContentsChanged {
            position,
            chars_removed,
            chars_added,
            blocks_affected,
        } => Some((*position, *chars_removed, *chars_added, *blocks_affected)),
        _ => None,
    });
    assert!(content_event.is_some(), "should have ContentsChanged event");
    let (pos, removed, added, affected) = content_event.unwrap();
    // The undo restores from a full snapshot, so position may be 0 (entire
    // document) and removed/added report the full old/new char counts rather
    // than the fine-grained delta.
    assert!(pos <= 5, "position should be at or before the edit point");
    assert!(affected >= 1, "at least one block should be affected");
    assert!(
        removed > 0 || added > 0,
        "should report non-zero chars_removed or chars_added"
    );
}

#[test]
fn undo_format_change_emits_format_changed() {
    let doc = new_doc_with_text("Hello");
    let cursor = doc.cursor_at(0);
    let fmt = text_document::BlockFormat {
        heading_level: Some(2),
        ..Default::default()
    };
    cursor.set_block_format(&fmt).unwrap();
    doc.poll_events(); // drain

    doc.undo().unwrap();

    let events = doc.poll_events();
    // Should have either FormatChanged or ContentsChanged
    // (the undo restores the block format, which may also change text layout)
    let has_change_event = events.iter().any(|e| {
        matches!(
            e,
            DocumentEvent::FormatChanged { .. } | DocumentEvent::ContentsChanged { .. }
        )
    });
    assert!(
        has_change_event,
        "undo of format change should emit FormatChanged or ContentsChanged, got: {:?}",
        events
    );
}

// ── LongOperationProgress / LongOperationFinished tests ─────

#[test]
fn long_operation_events_on_markdown_import() {
    let doc = TextDocument::new();
    doc.poll_events(); // drain setup

    let op = doc
        .set_markdown("# Title\n\nParagraph one.\n\nParagraph two.")
        .unwrap();
    let _result = op.wait().unwrap();

    // Give the background event hub thread a moment to deliver events
    std::thread::sleep(std::time::Duration::from_millis(300));

    let events = doc.poll_events();

    // Should have at least a LongOperationFinished with success=true
    assert!(
        events.iter().any(|e| matches!(
            e,
            DocumentEvent::LongOperationFinished { success: true, .. }
        )),
        "expected LongOperationFinished(success=true), got: {:?}",
        events
    );
}

#[test]
fn long_operation_events_on_html_import() {
    let doc = TextDocument::new();
    doc.poll_events(); // drain setup

    let op = doc.set_html("<p>Hello</p><p>World</p>").unwrap();
    let _result = op.wait().unwrap();

    // Give the background event hub thread a moment to deliver events
    std::thread::sleep(std::time::Duration::from_millis(300));

    let events = doc.poll_events();

    assert!(
        events.iter().any(|e| matches!(
            e,
            DocumentEvent::LongOperationFinished { success: true, .. }
        )),
        "expected LongOperationFinished(success=true), got: {:?}",
        events
    );
}
