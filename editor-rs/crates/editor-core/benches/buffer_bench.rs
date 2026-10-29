//! Benchmarks proving the typing hot path stays `O(log n)` even on huge files.
//!
//! The buffers are built **once** per size; each timed iteration performs a
//! single edit, so the measurement reflects the operation and not the cost of
//! constructing a 100 MB rope. Run with `cargo bench -p editor-core`.

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
    let mut group = c.benchmark_group("insert_char_into_large_file");
    for mb in [1usize, 10, 100] {
        let text = build_text(mb * 1024 * 1024);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(format!("{mb}MB")), &text, |b, text| {
            // Build the buffer once; each iteration inserts one char near the
            // middle. The buffer grows by ~1 char/iter, negligible vs its size.
            let mut buf = TextBuffer::from_str(text);
            let base = buf.len_chars() / 2;
            let mut i = 0usize;
            b.iter(|| {
                let change = buf.insert(base + (i % 64), "x");
                i += 1;
                criterion::black_box(change.new_end_byte);
            });
        });
    }
    group.finish();
}

fn bench_open_large_file(c: &mut Criterion) {
    // This one legitimately measures construction: "opening" a file means
    // building the rope from its text.
    let mut group = c.benchmark_group("open_large_file");
    for mb in [10usize, 100] {
        let text = build_text(mb * 1024 * 1024);
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.sample_size(20);
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

fn bench_typing_keystroke(c: &mut Criterion) {
    // The realistic hot path: one keystroke = insert + caret move + drain the
    // change stream, on a pre-built 10 MB document.
    let text = build_text(10 * 1024 * 1024);
    let mut doc = Document::from_str(&text);
    doc.set_caret(Point::new(doc.buffer().len_lines() / 2, 0));
    c.bench_function("typing_keystroke_10MB", |b| {
        b.iter(|| {
            doc.insert("a");
            doc.move_cursors(Movement::CharRight, false);
            criterion::black_box(doc.drain_changes().len());
        });
    });
}

criterion_group!(
    benches,
    bench_insert_into_large_file,
    bench_open_large_file,
    bench_char_to_point,
    bench_typing_keystroke
);
criterion_main!(benches);
