use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::time::Duration;
use text_document::{
    Alignment, BlockFormat, FindOptions, ListStyle, MoveMode, MoveOperation, SelectionType,
    TextDocument, TextFormat,
};

const PARAGRAPH: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.";

const SIZES: &[(usize, &str)] = &[
    (1, "small/1para"),
    (100, "medium/100para"),
    (1000, "large/1000para"),
];

fn make_doc(paragraphs: usize) -> (TextDocument, usize) {
    let text: String = (0..paragraphs)
        .map(|_| PARAGRAPH)
        .collect::<Vec<_>>()
        .join("\n");
    let len = text.len();
    let doc = TextDocument::new();
    doc.set_plain_text(&text).unwrap();
    (doc, len)
}

// ── Creation ────────────────────────────────────────────────────

fn bench_document_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("creation");

    group.bench_function("document_new", |b| {
        b.iter(|| {
            black_box(TextDocument::new());
        });
    });

    group.bench_function("document_new_with_text", |b| {
        b.iter(|| {
            let doc = TextDocument::new();
            doc.set_plain_text(black_box("Hello, world!")).unwrap();
        });
    });

    group.finish();
}

// ── Insertion ───────────────────────────────────────────────────

fn bench_insertion(c: &mut Criterion) {
    let mut group = c.benchmark_group("insertion");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    for &(n, label) in SIZES {
        group.bench_with_input(
            BenchmarkId::new("insert_char_at_start", label),
            &n,
            |b, &n| {
                b.iter_batched(
                    || {
                        let (doc, _) = make_doc(n);
                        let cursor = doc.cursor_at(0);
                        (doc, cursor)
                    },
                    |(_doc, cursor)| cursor.insert_text(black_box("X")).unwrap(),
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("insert_char_at_middle", label),
            &n,
            |b, &n| {
                b.iter_batched(
                    || {
                        let (doc, len) = make_doc(n);
                        let cursor = doc.cursor_at(len / 2);
                        (doc, cursor)
                    },
                    |(_doc, cursor)| cursor.insert_text(black_box("X")).unwrap(),
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("insert_char_at_end", label),
            &n,
            |b, &n| {
                b.iter_batched(
                    || {
                        let (doc, len) = make_doc(n);
                        let cursor = doc.cursor_at(len);
                        (doc, cursor)
                    },
                    |(_doc, cursor)| cursor.insert_text(black_box("X")).unwrap(),
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(BenchmarkId::new("insert_word", label), &n, |b, &n| {
            b.iter_batched(
                || {
                    let (doc, len) = make_doc(n);
                    let cursor = doc.cursor_at(len / 2);
                    (doc, cursor)
                },
                |(_doc, cursor)| cursor.insert_text(black_box("benchmark ")).unwrap(),
                BatchSize::SmallInput,
            );
        });

        group.bench_with_input(BenchmarkId::new("insert_paragraph", label), &n, |b, &n| {
            b.iter_batched(
                || {
                    let (doc, len) = make_doc(n);
                    let cursor = doc.cursor_at(len / 2);
                    (doc, cursor)
                },
                |(_doc, cursor)| cursor.insert_text(black_box(PARAGRAPH)).unwrap(),
                BatchSize::SmallInput,
            );
        });

        group.bench_with_input(BenchmarkId::new("insert_block", label), &n, |b, &n| {
            b.iter_batched(
                || {
                    let (doc, len) = make_doc(n);
                    let cursor = doc.cursor_at(len / 2);
                    (doc, cursor)
                },
                |(_doc, cursor)| cursor.insert_block().unwrap(),
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

// ── Deletion ────────────────────────────────────────────────────

fn bench_deletion(c: &mut Criterion) {
    let mut group = c.benchmark_group("deletion");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    for &(n, label) in SIZES {
        group.bench_with_input(
            BenchmarkId::new("delete_char_forward", label),
            &n,
            |b, &n| {
                b.iter_batched(
                    || {
                        let (doc, len) = make_doc(n);
                        let cursor = doc.cursor_at(len / 2);
                        (doc, cursor)
                    },
                    |(_doc, cursor)| cursor.delete_char().unwrap(),
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("delete_char_backward", label),
            &n,
            |b, &n| {
                b.iter_batched(
                    || {
                        let (doc, len) = make_doc(n);
                        let cursor = doc.cursor_at(len / 2);
                        (doc, cursor)
                    },
                    |(_doc, cursor)| cursor.delete_previous_char().unwrap(),
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(BenchmarkId::new("delete_selection", label), &n, |b, &n| {
            b.iter_batched(
                || {
                    let (doc, len) = make_doc(n);
                    let cursor = doc.cursor_at(len / 2);
                    let select_len = 100.min(len / 2);
                    cursor.set_position(len / 2 + select_len, MoveMode::KeepAnchor);
                    (doc, cursor)
                },
                |(_doc, cursor)| {
                    let _ = cursor.remove_selected_text().unwrap();
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

// ── Cursor Movement ─────────────────────────────────────────────

fn bench_cursor_movement(c: &mut Criterion) {
    let mut group = c.benchmark_group("cursor_movement");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    for &(n, label) in SIZES {
        group.bench_with_input(BenchmarkId::new("move_next_char", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            let cursor = doc.cursor_at(0);
            b.iter(|| {
                cursor.set_position(0, MoveMode::MoveAnchor);
                black_box(cursor.move_position(
                    MoveOperation::NextCharacter,
                    MoveMode::MoveAnchor,
                    1,
                ));
            });
        });

        group.bench_with_input(BenchmarkId::new("move_next_word", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            let cursor = doc.cursor_at(0);
            b.iter(|| {
                cursor.set_position(0, MoveMode::MoveAnchor);
                black_box(cursor.move_position(MoveOperation::NextWord, MoveMode::MoveAnchor, 1));
            });
        });

        group.bench_with_input(BenchmarkId::new("move_next_block", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            let cursor = doc.cursor_at(0);
            b.iter(|| {
                cursor.set_position(0, MoveMode::MoveAnchor);
                black_box(cursor.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 1));
            });
        });

        group.bench_with_input(BenchmarkId::new("move_to_end", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            let cursor = doc.cursor_at(0);
            b.iter(|| {
                cursor.set_position(0, MoveMode::MoveAnchor);
                black_box(cursor.move_position(MoveOperation::End, MoveMode::MoveAnchor, 1));
            });
        });

        group.bench_with_input(BenchmarkId::new("move_to_start", label), &n, |b, &n| {
            let (doc, len) = make_doc(n);
            let cursor = doc.cursor_at(len);
            b.iter(|| {
                cursor.set_position(len, MoveMode::MoveAnchor);
                black_box(cursor.move_position(MoveOperation::Start, MoveMode::MoveAnchor, 1));
            });
        });

        group.bench_with_input(BenchmarkId::new("select_word", label), &n, |b, &n| {
            let (doc, len) = make_doc(n);
            let cursor = doc.cursor_at(len / 2);
            b.iter(|| {
                cursor.select(SelectionType::WordUnderCursor);
                cursor.clear_selection();
            });
        });

        group.bench_with_input(BenchmarkId::new("select_block", label), &n, |b, &n| {
            let (doc, len) = make_doc(n);
            let cursor = doc.cursor_at(len / 2);
            b.iter(|| {
                cursor.select(SelectionType::BlockUnderCursor);
                cursor.clear_selection();
            });
        });
    }

    group.finish();
}

// ── Search ──────────────────────────────────────────────────────

fn bench_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("search");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    for &(n, label) in SIZES {
        group.bench_with_input(BenchmarkId::new("find_first", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            let opts = FindOptions::default();
            b.iter(|| {
                black_box(doc.find(black_box("amet"), 0, &opts).unwrap());
            });
        });

        group.bench_with_input(BenchmarkId::new("find_all", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            let opts = FindOptions::default();
            b.iter(|| {
                black_box(doc.find_all(black_box("amet"), &opts).unwrap());
            });
        });

        group.bench_with_input(BenchmarkId::new("find_regex", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            let opts = FindOptions {
                use_regex: true,
                ..Default::default()
            };
            b.iter(|| {
                black_box(doc.find(black_box("\\b[Ll]orem\\b"), 0, &opts).unwrap());
            });
        });

        group.bench_with_input(
            BenchmarkId::new("find_all_case_insensitive", label),
            &n,
            |b, &n| {
                let (doc, _) = make_doc(n);
                let opts = FindOptions {
                    case_sensitive: false,
                    ..Default::default()
                };
                b.iter(|| {
                    black_box(doc.find_all(black_box("LOREM"), &opts).unwrap());
                });
            },
        );
    }

    group.finish();
}

// ── Undo / Redo ─────────────────────────────────────────────────

fn bench_undo_redo(c: &mut Criterion) {
    let mut group = c.benchmark_group("undo_redo");

    group.bench_function("undo_single", |b| {
        b.iter_batched(
            || {
                let (doc, len) = make_doc(100);
                let cursor = doc.cursor_at(len / 2);
                cursor.insert_text("bench").unwrap();
                doc
            },
            |doc| doc.undo().unwrap(),
            BatchSize::SmallInput,
        );
    });

    group.bench_function("redo_single", |b| {
        b.iter_batched(
            || {
                let (doc, len) = make_doc(100);
                let cursor = doc.cursor_at(len / 2);
                cursor.insert_text("bench").unwrap();
                doc.undo().unwrap();
                doc
            },
            |doc| doc.redo().unwrap(),
            BatchSize::SmallInput,
        );
    });

    group.bench_function("undo_chain_10", |b| {
        b.iter_batched(
            || {
                let (doc, len) = make_doc(100);
                let cursor = doc.cursor_at(len / 2);
                for i in 0..10 {
                    cursor.insert_text(&format!("item{i} ")).unwrap();
                }
                doc
            },
            |doc| {
                for _ in 0..10 {
                    doc.undo().unwrap();
                }
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ── Formatting ──────────────────────────────────────────────────

fn bench_formatting(c: &mut Criterion) {
    let mut group = c.benchmark_group("formatting");

    group.bench_function("insert_formatted_text", |b| {
        b.iter_batched(
            || {
                let (doc, _) = make_doc(100);
                let cursor = doc.cursor_at(10);
                let format = cursor.char_format().unwrap();
                (doc, cursor, format)
            },
            |(_doc, cursor, format)| {
                cursor
                    .insert_formatted_text(black_box("formatted "), &format)
                    .unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("merge_char_format_bold", |b| {
        b.iter_batched(
            || {
                let (doc, len) = make_doc(100);
                let cursor = doc.cursor_at(len / 2);
                cursor.set_position(len / 2 + 100, MoveMode::KeepAnchor);
                let format = TextFormat {
                    font_bold: Some(true),
                    ..Default::default()
                };
                (doc, cursor, format)
            },
            |(_doc, cursor, format)| {
                cursor.merge_char_format(&format).unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("set_block_format_center", |b| {
        b.iter_batched(
            || {
                let (doc, len) = make_doc(100);
                let cursor = doc.cursor_at(len / 2);
                let format = BlockFormat {
                    alignment: Some(Alignment::Center),
                    ..Default::default()
                };
                (doc, cursor, format)
            },
            |(_doc, cursor, format)| {
                cursor.set_block_format(&format).unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ── Tables ──────────────────────────────────────────────────────

fn bench_tables(c: &mut Criterion) {
    let mut group = c.benchmark_group("tables");

    group.bench_function("insert_table_3x3", |b| {
        b.iter_batched(
            || {
                let doc = TextDocument::new();
                doc.set_plain_text("Hello world").unwrap();
                let cursor = doc.cursor_at(5);
                (doc, cursor)
            },
            |(_doc, cursor)| {
                cursor.insert_table(3, 3).unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("insert_table_10x10", |b| {
        b.iter_batched(
            || {
                let doc = TextDocument::new();
                doc.set_plain_text("Hello world").unwrap();
                let cursor = doc.cursor_at(5);
                (doc, cursor)
            },
            |(_doc, cursor)| {
                cursor.insert_table(10, 10).unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("table_cell_access", |b| {
        let doc = TextDocument::new();
        doc.set_plain_text("Hello").unwrap();
        let cursor = doc.cursor_at(0);
        let table = cursor.insert_table(5, 5).unwrap();
        b.iter(|| {
            black_box(table.cell(2, 2));
        });
    });

    group.bench_function("insert_table_row", |b| {
        b.iter_batched(
            || {
                let doc = TextDocument::new();
                doc.set_plain_text("Hello").unwrap();
                let cursor = doc.cursor_at(0);
                let table = cursor.insert_table(5, 5).unwrap();
                let table_id = table.id();
                (doc, cursor, table_id)
            },
            |(_doc, cursor, table_id)| {
                cursor.insert_table_row(table_id, 2).unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("insert_table_column", |b| {
        b.iter_batched(
            || {
                let doc = TextDocument::new();
                doc.set_plain_text("Hello").unwrap();
                let cursor = doc.cursor_at(0);
                let table = cursor.insert_table(5, 5).unwrap();
                let table_id = table.id();
                (doc, cursor, table_id)
            },
            |(_doc, cursor, table_id)| {
                cursor.insert_table_column(table_id, 2).unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ── Lists ───────────────────────────────────────────────────────

fn bench_lists(c: &mut Criterion) {
    let mut group = c.benchmark_group("lists");

    group.bench_function("create_list", |b| {
        b.iter_batched(
            || {
                let (doc, len) = make_doc(100);
                let cursor = doc.cursor_at(len / 2);
                (doc, cursor)
            },
            |(_doc, cursor)| {
                cursor.create_list(ListStyle::Disc).unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("insert_list", |b| {
        b.iter_batched(
            || {
                let (doc, len) = make_doc(100);
                let cursor = doc.cursor_at(len / 2);
                (doc, cursor)
            },
            |(_doc, cursor)| {
                cursor.insert_list(ListStyle::Disc).unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ── Complex Editing Session ──────────────────────────────────────

fn bench_editing_session(c: &mut Criterion) {
    let mut group = c.benchmark_group("editing_session");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(15));

    // Simulates a realistic editing session: navigate, type, format, delete,
    // move around, insert structures, undo — all in sequence on a medium doc.
    group.bench_function("mixed_operations_30", |b| {
        b.iter_batched(
            || make_doc(100),
            |(doc, len)| {
                let c1 = doc.cursor_at(len / 4);

                // 1. Type a sentence
                c1.insert_text("This is a new sentence. ").unwrap();

                // 2. Move to next word, select it, delete
                c1.move_position(MoveOperation::NextWord, MoveMode::MoveAnchor, 1);
                c1.select(SelectionType::WordUnderCursor);
                c1.remove_selected_text().unwrap();

                // 3. Insert a paragraph break
                c1.insert_block().unwrap();
                c1.insert_text("New paragraph after break.").unwrap();

                // 4. Move back, bold a word
                c1.move_position(MoveOperation::PreviousWord, MoveMode::MoveAnchor, 3);
                c1.move_position(MoveOperation::NextWord, MoveMode::KeepAnchor, 1);
                c1.merge_char_format(&TextFormat {
                    font_bold: Some(true),
                    ..Default::default()
                })
                .unwrap();

                // 5. Jump to end of block, type more
                c1.move_position(MoveOperation::EndOfBlock, MoveMode::MoveAnchor, 1);
                c1.insert_text(" Appended at end of block.").unwrap();

                // 6. Navigate forward several blocks
                c1.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 5);

                // 7. Select entire block, replace content
                c1.select(SelectionType::BlockUnderCursor);
                c1.insert_text("Replaced block content entirely.").unwrap();

                // 8. Set block alignment
                c1.set_block_format(&BlockFormat {
                    alignment: Some(Alignment::Center),
                    ..Default::default()
                })
                .unwrap();

                // 9. Move to start of document
                c1.move_position(MoveOperation::Start, MoveMode::MoveAnchor, 1);

                // 10. Insert a list
                c1.insert_list(ListStyle::Decimal).unwrap();
                c1.insert_text("First item").unwrap();
                c1.insert_block().unwrap();
                c1.insert_text("Second item").unwrap();
                c1.insert_block().unwrap();
                c1.insert_text("Third item").unwrap();

                // 11. Move to middle, insert formatted text
                c1.move_position(MoveOperation::End, MoveMode::MoveAnchor, 1);
                c1.move_position(MoveOperation::PreviousBlock, MoveMode::MoveAnchor, 10);
                c1.insert_formatted_text(
                    "italic insertion ",
                    &TextFormat {
                        font_italic: Some(true),
                        ..Default::default()
                    },
                )
                .unwrap();

                // 12. Select a range and delete with backspace
                c1.move_position(MoveOperation::NextCharacter, MoveMode::KeepAnchor, 20);
                c1.delete_previous_char().unwrap();

                // 13. Forward delete a few chars
                for _ in 0..5 {
                    c1.delete_char().unwrap();
                }

                // 14. Use a second cursor concurrently
                let c2 = doc.cursor_at(0);
                c2.insert_text("Prepended by cursor2. ").unwrap();
                c2.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 2);
                c2.insert_block().unwrap();
                c2.insert_text("Cursor2 inserted block.").unwrap();

                // 15. Back to c1: navigate by words
                c1.move_position(MoveOperation::NextWord, MoveMode::MoveAnchor, 10);
                c1.insert_text("mid-word-jump ").unwrap();

                // 16. Undo a few operations
                doc.undo().unwrap();
                doc.undo().unwrap();
                doc.undo().unwrap();

                // 17. Redo one
                doc.redo().unwrap();

                // 18. Final: select line and apply bold+italic
                c1.select(SelectionType::LineUnderCursor);
                c1.merge_char_format(&TextFormat {
                    font_bold: Some(true),
                    font_italic: Some(true),
                    ..Default::default()
                })
                .unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    // Same idea but on a large document to measure scaling
    group.bench_function("mixed_operations_30_large", |b| {
        b.iter_batched(
            || make_doc(1000),
            |(doc, len)| {
                let c1 = doc.cursor_at(len / 4);

                c1.insert_text("This is a new sentence. ").unwrap();
                c1.move_position(MoveOperation::NextWord, MoveMode::MoveAnchor, 1);
                c1.select(SelectionType::WordUnderCursor);
                c1.remove_selected_text().unwrap();

                c1.insert_block().unwrap();
                c1.insert_text("New paragraph after break.").unwrap();

                c1.move_position(MoveOperation::PreviousWord, MoveMode::MoveAnchor, 3);
                c1.move_position(MoveOperation::NextWord, MoveMode::KeepAnchor, 1);
                c1.merge_char_format(&TextFormat {
                    font_bold: Some(true),
                    ..Default::default()
                })
                .unwrap();

                c1.move_position(MoveOperation::EndOfBlock, MoveMode::MoveAnchor, 1);
                c1.insert_text(" Appended at end of block.").unwrap();

                c1.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 5);
                c1.select(SelectionType::BlockUnderCursor);
                c1.insert_text("Replaced block content entirely.").unwrap();

                c1.set_block_format(&BlockFormat {
                    alignment: Some(Alignment::Center),
                    ..Default::default()
                })
                .unwrap();

                c1.move_position(MoveOperation::Start, MoveMode::MoveAnchor, 1);
                c1.insert_list(ListStyle::Decimal).unwrap();
                c1.insert_text("First item").unwrap();
                c1.insert_block().unwrap();
                c1.insert_text("Second item").unwrap();
                c1.insert_block().unwrap();
                c1.insert_text("Third item").unwrap();

                c1.move_position(MoveOperation::End, MoveMode::MoveAnchor, 1);
                c1.move_position(MoveOperation::PreviousBlock, MoveMode::MoveAnchor, 10);
                c1.insert_formatted_text(
                    "italic insertion ",
                    &TextFormat {
                        font_italic: Some(true),
                        ..Default::default()
                    },
                )
                .unwrap();

                c1.move_position(MoveOperation::NextCharacter, MoveMode::KeepAnchor, 20);
                c1.delete_previous_char().unwrap();

                for _ in 0..5 {
                    c1.delete_char().unwrap();
                }

                let c2 = doc.cursor_at(0);
                c2.insert_text("Prepended by cursor2. ").unwrap();
                c2.move_position(MoveOperation::NextBlock, MoveMode::MoveAnchor, 2);
                c2.insert_block().unwrap();
                c2.insert_text("Cursor2 inserted block.").unwrap();

                c1.move_position(MoveOperation::NextWord, MoveMode::MoveAnchor, 10);
                c1.insert_text("mid-word-jump ").unwrap();

                doc.undo().unwrap();
                doc.undo().unwrap();
                doc.undo().unwrap();
                doc.redo().unwrap();

                c1.select(SelectionType::LineUnderCursor);
                c1.merge_char_format(&TextFormat {
                    font_bold: Some(true),
                    font_italic: Some(true),
                    ..Default::default()
                })
                .unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ── Migration-relevant XL workloads ─────────────────────────────
// 1 MB-scale and format-run-heavy benches added for the rope-backend
// migration baseline. The existing SIZES top out at ~130 KB ("large").
// These workloads are where the rope migration is expected to win
// hardest. Sample size is small to keep total runtime reasonable.

fn make_doc_xl(bytes: usize) -> TextDocument {
    let body: String = "a".repeat(bytes);
    let doc = TextDocument::new();
    doc.set_plain_text(&body).unwrap();
    doc
}

fn bench_xl_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("xl");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    // 1 MB plain doc construction (data + format_runs/block_images clone cost).
    group.bench_function("set_plain_text_1mb", |b| {
        b.iter_batched(
            || "a".repeat(1_000_000),
            |text| {
                let doc = TextDocument::new();
                doc.set_plain_text(black_box(&text)).unwrap();
                doc
            },
            BatchSize::LargeInput,
        );
    });

    // Single-char insert at end of 1 MB doc.
    group.bench_function("insert_char_at_end_of_1mb", |b| {
        b.iter_batched(
            || {
                let doc = make_doc_xl(1_000_000);
                let cursor = doc.cursor_at(1_000_000);
                (doc, cursor)
            },
            |(_doc, cursor)| cursor.insert_text(black_box("X")).unwrap(),
            BatchSize::SmallInput,
        );
    });

    // Single-char insert at mid of 1 MB doc.
    group.bench_function("insert_char_at_mid_of_1mb", |b| {
        b.iter_batched(
            || {
                let doc = make_doc_xl(1_000_000);
                let cursor = doc.cursor_at(500_000);
                (doc, cursor)
            },
            |(_doc, cursor)| cursor.insert_text(black_box("X")).unwrap(),
            BatchSize::SmallInput,
        );
    });

    // 1 KB bulk insert into 1 MB doc.
    group.bench_function("insert_1kb_at_mid_of_1mb", |b| {
        b.iter_batched(
            || {
                let doc = make_doc_xl(1_000_000);
                let cursor = doc.cursor_at(500_000);
                let chunk = "x".repeat(1024);
                (doc, cursor, chunk)
            },
            |(_doc, cursor, chunk)| cursor.insert_text(black_box(&chunk)).unwrap(),
            BatchSize::LargeInput,
        );
    });

    // Undo of single-char insert on 1 MB doc.
    group.bench_function("undo_single_char_on_1mb", |b| {
        b.iter_batched(
            || {
                let doc = make_doc_xl(1_000_000);
                let cursor = doc.cursor_at(500_000);
                cursor.insert_text("X").unwrap();
                doc
            },
            |doc| doc.undo().unwrap(),
            BatchSize::SmallInput,
        );
    });

    // Undo-redo cycle of single-char insert on 1 MB doc.
    group.bench_function("undo_redo_cycle_single_char_on_1mb", |b| {
        b.iter_batched(
            || {
                let doc = make_doc_xl(1_000_000);
                let cursor = doc.cursor_at(500_000);
                cursor.insert_text("X").unwrap();
                doc
            },
            |doc| {
                doc.undo().unwrap();
                doc.redo().unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    // find_all across 1 MB doc with ~100 matches.
    group.bench_function("find_all_100_matches_in_1mb", |b| {
        let needle = "needle";
        b.iter_batched(
            || {
                // 100 needles spread across ~1 MB.
                let padding = "a".repeat(10_000 - needle.len());
                let unit = format!("{padding}{needle}");
                let text = unit.repeat(100);
                let doc = TextDocument::new();
                doc.set_plain_text(&text).unwrap();
                doc
            },
            |doc| {
                doc.find_all(black_box(needle), &text_document::FindOptions::default())
                    .unwrap()
            },
            BatchSize::LargeInput,
        );
    });

    // Format toggle on 100 KB selection (format-run-heavy regime).
    group.bench_function("set_bold_on_100kb_selection", |b| {
        b.iter_batched(
            || {
                let doc = make_doc_xl(100_000);
                let cursor = doc.cursor_at(0);
                cursor.move_position(MoveOperation::Right, MoveMode::KeepAnchor, 100_000);
                (doc, cursor)
            },
            |(_doc, cursor)| {
                let bold = TextFormat {
                    font_bold: Some(true),
                    ..Default::default()
                };
                cursor.merge_char_format(black_box(&bold)).unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ── Main ────────────────────────────────────────────────────────

criterion_group!(creation, bench_document_creation);
criterion_group!(editing, bench_insertion, bench_deletion);
criterion_group!(navigation, bench_cursor_movement, bench_search);
criterion_group!(history, bench_undo_redo);
criterion_group!(
    formatting_group,
    bench_formatting,
    bench_tables,
    bench_lists
);
criterion_group!(session, bench_editing_session);
criterion_group!(xl, bench_xl_workloads);
criterion_main!(
    creation,
    editing,
    navigation,
    history,
    formatting_group,
    session,
    xl
);
