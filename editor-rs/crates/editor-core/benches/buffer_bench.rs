//! Benchmarks proving the typing hot path stays `O(log n)` even on huge files.
//!
//! Run with `cargo bench -p editor-core`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use editor_core::{Document, Movement, Point, TextBuffer};

/// Build a buffer of roughly `target_bytes` bytes of realistic source-like text.
fn build_text(target_bytes: usize) -> String {
    let line = "    let value = compute(index, offset) + delta * 2; // a line of code\n";
    let mut s = String::with_capacity(target_bytes + line.len());
    while s.len() < target_bytes {
        s.push_str(line);
    }
    s
}

fn bench_insert_into_large_file(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_into_large_file");
    for mb in [1usize, 10, 100] {
        let text = build_text(mb * 1024 * 1024);
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(format!("{mb}MB")), &text, |b, text| {
            // Fresh buffer per iteration batch; measure a single mid-file insert.
            b.iter_batched(
                || TextBuffer::from_str(text),
                |mut buf| {
                    let mid = buf.len_chars() / 2;
                    buf.insert(mid, "x");
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

fn bench_open_large_file(c: &mut Criterion) {
    let mut group = c.benchmark_group("open_large_file");
    for mb in [10usize, 100] {
        let text = build_text(mb * 1024 * 1024);
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(format!("{mb}MB")), &text, |b, text| {
            b.iter(|| {
                let buf = TextBuffer::from_str(text);
                criterion::black_box(buf.len_lines());
            });
        });
    }
    group.finish();
}

fn bench_char_to_point(c: &mut Criterion) {
    let text = build_text(10 * 1024 * 1024);
    let buf = TextBuffer::from_str(&text);
    let mid = buf.len_chars() / 2;
    c.bench_function("char_to_point_10MB", |b| {
        b.iter(|| criterion::black_box(buf.char_to_point(criterion::black_box(mid))));
    });
}

fn bench_typing_session(c: &mut Criterion) {
    // Simulate typing 200 characters into a 10 MB document, the realistic
    // hot path: each keystroke is an insert + caret move + change drain.
    let text = build_text(10 * 1024 * 1024);
    c.bench_function("typing_200_chars_10MB", |b| {
        b.iter_batched(
            || {
                let mut doc = Document::from_str(&text);
                doc.set_caret(Point::new(1000, 0));
                doc
            },
            |mut doc| {
                for _ in 0..200 {
                    doc.insert("a");
                    doc.move_cursors(Movement::CharRight, false);
                    criterion::black_box(doc.drain_changes());
                }
            },
            criterion::BatchSize::LargeInput,
        );
    });
}

criterion_group!(
    benches,
    bench_insert_into_large_file,
    bench_open_large_file,
    bench_char_to_point,
    bench_typing_session
);
criterion_main!(benches);
