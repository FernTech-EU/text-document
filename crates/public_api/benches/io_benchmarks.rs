use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::time::Duration;
use text_document::TextDocument;

const PARAGRAPH: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.";

const SIZES: &[(usize, &str)] = &[
    (1, "small/1para"),
    (100, "medium/100para"),
    (1000, "large/1000para"),
];

fn make_plain_text(paragraphs: usize) -> String {
    (0..paragraphs)
        .map(|_| PARAGRAPH)
        .collect::<Vec<_>>()
        .join("\n")
}

fn make_doc(paragraphs: usize) -> (TextDocument, usize) {
    let text = make_plain_text(paragraphs);
    let len = text.len();
    let doc = TextDocument::new();
    doc.set_plain_text(&text).unwrap();
    (doc, len)
}

fn make_markdown(paragraphs: usize) -> String {
    (0..paragraphs)
        .map(|i| {
            if i % 5 == 0 {
                format!("## Heading {i}\n\n{PARAGRAPH}")
            } else {
                format!("This is **bold** and *italic* text. {PARAGRAPH}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn make_html(paragraphs: usize) -> String {
    let mut html = String::from("<html><body>");
    for i in 0..paragraphs {
        if i % 5 == 0 {
            html.push_str(&format!("<h2>Heading {i}</h2>"));
        } else {
            html.push_str(&format!(
                "<p>This is <b>bold</b> and <i>italic</i> text. {PARAGRAPH}</p>"
            ));
        }
    }
    html.push_str("</body></html>");
    html
}

// ── Plain Text I/O ──────────────────────────────────────────────

fn bench_plain_text_io(c: &mut Criterion) {
    let mut group = c.benchmark_group("plain_text_io");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    for &(n, label) in SIZES {
        group.bench_with_input(BenchmarkId::new("set_plain_text", label), &n, |b, &n| {
            let text = make_plain_text(n);
            b.iter_batched(
                TextDocument::new,
                |doc| doc.set_plain_text(black_box(&text)).unwrap(),
                BatchSize::SmallInput,
            );
        });

        group.bench_with_input(BenchmarkId::new("to_plain_text", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            b.iter(|| {
                black_box(doc.to_plain_text().unwrap());
            });
        });
    }

    group.finish();
}

// ── Markdown I/O ────────────────────────────────────────────────

fn bench_markdown_io(c: &mut Criterion) {
    let mut group = c.benchmark_group("markdown_io");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(15));

    for &(n, label) in SIZES {
        group.bench_with_input(BenchmarkId::new("set_markdown", label), &n, |b, &n| {
            let md = make_markdown(n);
            b.iter_batched(
                TextDocument::new,
                |doc| {
                    let op = doc.set_markdown(black_box(&md)).unwrap();
                    op.wait().unwrap();
                },
                BatchSize::SmallInput,
            );
        });

        group.bench_with_input(BenchmarkId::new("to_markdown", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            b.iter(|| {
                black_box(doc.to_markdown().unwrap());
            });
        });
    }

    group.finish();
}

// ── HTML I/O ────────────────────────────────────────────────────

fn bench_html_io(c: &mut Criterion) {
    let mut group = c.benchmark_group("html_io");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(15));

    for &(n, label) in SIZES {
        group.bench_with_input(BenchmarkId::new("set_html", label), &n, |b, &n| {
            let html = make_html(n);
            b.iter_batched(
                TextDocument::new,
                |doc| {
                    let op = doc.set_html(black_box(&html)).unwrap();
                    op.wait().unwrap();
                },
                BatchSize::SmallInput,
            );
        });

        group.bench_with_input(BenchmarkId::new("to_html", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            b.iter(|| {
                black_box(doc.to_html().unwrap());
            });
        });
    }

    group.finish();
}

// ── Snapshots ───────────────────────────────────────────────────

fn bench_snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    for &(n, label) in SIZES {
        group.bench_with_input(BenchmarkId::new("snapshot_flow", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            b.iter(|| {
                black_box(doc.snapshot_flow());
            });
        });

        group.bench_with_input(
            BenchmarkId::new("snapshot_block_at_position", label),
            &n,
            |b, &n| {
                let (doc, len) = make_doc(n);
                let mid = len / 2;
                b.iter(|| {
                    black_box(doc.snapshot_block_at_position(black_box(mid)));
                });
            },
        );

        group.bench_with_input(BenchmarkId::new("flow_traversal", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            b.iter(|| {
                black_box(doc.flow());
            });
        });
    }

    group.finish();
}

// ── Document Queries ────────────────────────────────────────────

fn bench_document_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("document_queries");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    for &(n, label) in SIZES {
        group.bench_with_input(BenchmarkId::new("stats", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            b.iter(|| {
                black_box(doc.stats());
            });
        });

        group.bench_with_input(BenchmarkId::new("block_at_position", label), &n, |b, &n| {
            let (doc, len) = make_doc(n);
            let mid = len / 2;
            b.iter(|| {
                black_box(doc.block_at(black_box(mid)).unwrap());
            });
        });

        group.bench_with_input(BenchmarkId::new("text_at", label), &n, |b, &n| {
            let (doc, len) = make_doc(n);
            let mid = len / 2;
            let read_len = 100.min(len - mid);
            b.iter(|| {
                black_box(doc.text_at(black_box(mid), read_len).unwrap());
            });
        });

        group.bench_with_input(BenchmarkId::new("blocks", label), &n, |b, &n| {
            let (doc, _) = make_doc(n);
            b.iter(|| {
                black_box(doc.blocks());
            });
        });

        group.bench_with_input(BenchmarkId::new("blocks_in_range", label), &n, |b, &n| {
            let (doc, len) = make_doc(n);
            let mid = len / 2;
            b.iter(|| {
                black_box(doc.blocks_in_range(black_box(mid), 200));
            });
        });
    }

    group.finish();
}

// ── Main ────────────────────────────────────────────────────────

criterion_group!(plain_text, bench_plain_text_io);
criterion_group!(rich_text, bench_markdown_io, bench_html_io);
criterion_group!(snapshots, bench_snapshot, bench_document_queries);
criterion_main!(plain_text, rich_text, snapshots);
