use text_document::{FindOptions, FlowElementSnapshot, TextDocument};

fn new_doc_with_text(text: &str) -> TextDocument {
    let doc = TextDocument::new();
    doc.set_plain_text(text).unwrap();
    doc
}

/// Snapshot position of the first block in cell (row, col), if any.
fn cell_block_position(doc: &TextDocument, row: usize, col: usize) -> Option<(usize, usize)> {
    let snap = doc.snapshot_flow();
    for el in &snap.elements {
        if let FlowElementSnapshot::Table(ts) = el {
            for cell in &ts.cells {
                if cell.row == row
                    && cell.column == col
                    && let Some(b) = cell.blocks.first()
                {
                    return Some((b.position, b.length));
                }
            }
        }
    }
    None
}

#[test]
fn find_text_basic() {
    let doc = new_doc_with_text("Hello world Hello");
    let opts = FindOptions::default();
    let result = doc.find("Hello", 0, &opts).unwrap();
    assert!(result.is_some());
    let m = result.unwrap();
    assert_eq!(m.position, 0);
    assert_eq!(m.length, 5);
}

#[test]
fn find_text_from_offset() {
    let doc = new_doc_with_text("Hello world Hello");
    let opts = FindOptions::default();
    let result = doc.find("Hello", 1, &opts).unwrap();
    assert!(result.is_some());
    let m = result.unwrap();
    assert_eq!(m.position, 12);
}

#[test]
fn find_text_not_found() {
    let doc = new_doc_with_text("Hello world");
    let opts = FindOptions::default();
    let result = doc.find("xyz", 0, &opts).unwrap();
    assert!(result.is_none());
}

#[test]
fn find_all() {
    let doc = new_doc_with_text("abcabcabc");
    let opts = FindOptions::default();
    let matches = doc.find_all("abc", &opts).unwrap();
    assert_eq!(matches.len(), 3);
    assert_eq!(matches[0].position, 0);
    assert_eq!(matches[1].position, 3);
    assert_eq!(matches[2].position, 6);
}

#[test]
fn find_case_sensitive() {
    let doc = new_doc_with_text("Hello hello");
    let opts = FindOptions {
        case_sensitive: true,
        ..Default::default()
    };
    let matches = doc.find_all("Hello", &opts).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].position, 0);
}

#[test]
fn replace_text_all() {
    let doc = new_doc_with_text("foo bar foo");
    let opts = FindOptions::default();
    let count = doc.replace_text("foo", "baz", true, &opts).unwrap();
    assert_eq!(count, 2);
    assert_eq!(doc.to_plain_text().unwrap(), "baz bar baz");
}

#[test]
fn replace_text_is_undoable() {
    let doc = new_doc_with_text("foo bar foo");
    let opts = FindOptions::default();
    doc.replace_text("foo", "baz", true, &opts).unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "baz bar baz");

    doc.undo().unwrap();
    assert_eq!(doc.to_plain_text().unwrap(), "foo bar foo");
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Full-text search reaches table-cell content
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[test]
fn find_text_typed_into_table_cell() {
    // "Before" + 2x2 table; type a distinctive word into cell (0,0).
    let doc = TextDocument::new();
    doc.set_plain_text("Before").unwrap();
    doc.cursor_at(6).insert_table(2, 2).unwrap();
    let (cell_pos, _len) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    doc.cursor_at(cell_pos).insert_text("ZEBRA").unwrap();

    let opts = FindOptions::default();
    let m = doc.find("ZEBRA", 0, &opts).unwrap();
    assert!(
        m.is_some(),
        "text typed into a table cell must be reachable by full-text search"
    );

    // The reported position must round-trip back into the document.
    let pos = m.unwrap().position;
    assert_eq!(doc.cursor_at(pos).position(), pos);
}

#[test]
fn find_all_includes_table_cell_matches() {
    // A word that appears once outside the table and once inside a cell.
    let doc = TextDocument::new();
    doc.set_plain_text("needle before").unwrap();
    let end = doc.character_count();
    doc.cursor_at(end).insert_table(1, 1).unwrap();
    let (cell_pos, _len) = cell_block_position(&doc, 0, 0).expect("cell (0,0)");
    doc.cursor_at(cell_pos).insert_text("needle").unwrap();

    let opts = FindOptions::default();
    let matches = doc.find_all("needle", &opts).unwrap();
    assert_eq!(
        matches.len(),
        2,
        "find_all must match both the main-flow and the in-cell occurrence"
    );
}

#[test]
fn find_text_in_markdown_imported_table_cell() {
    let doc = TextDocument::new();
    doc.set_markdown("Before\n\n| ZEBRA | gamma |\n|---|---|\n| delta | omega |\n\nAfter")
        .unwrap()
        .wait()
        .unwrap();

    let opts = FindOptions::default();
    assert!(
        doc.find("ZEBRA", 0, &opts).unwrap().is_some(),
        "header-cell text should be searchable"
    );
    assert!(
        doc.find("omega", 0, &opts).unwrap().is_some(),
        "body-cell text should be searchable"
    );
}
