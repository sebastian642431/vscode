//! Benchmarks for tree-sitter parsing: a full parse of a large file vs. an
//! incremental reparse after a one-character edit.
//!
//! Run with `cargo bench -p editor-syntax`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use editor_core::{Document, Point};
use editor_syntax::{Language, SyntaxTree};

fn rust_source(target_bytes: usize) -> String {
    let unit = "fn compute_value(index: usize, offset: usize) -> usize {\n    let total = index * 2 + offset; // work\n    total.wrapping_add(7)\n}\n\n";
    let mut s = String::with_capacity(target_bytes + unit.len());
    while s.len() < target_bytes {
        s.push_str(unit);
    }
    s
}

fn bench_full_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_sitter_full_parse");
    for kb in [64usize, 512, 4096] {
        let src = rust_source(kb * 1024);
        group.throughput(Throughput::Bytes(src.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(format!("{kb}KB")), &src, |b, src| {
            b.iter(|| {
                let mut st = SyntaxTree::new(Language::Rust);
                st.set_text(src);
                criterion::black_box(st.tree().unwrap().root_node().end_byte());
            });
        });
    }
    group.finish();
}

fn bench_incremental_reparse(c: &mut Criterion) {
    // The hot path: a parsed large file, then one character typed in the
    // middle. This should be orders of magnitude faster than a full parse.
    let mut group = c.benchmark_group("tree_sitter_incremental_reparse");
    for kb in [64usize, 512, 4096] {
        let src = rust_source(kb * 1024);
        group.throughput(Throughput::Bytes(1)); // one char changed
        group.bench_with_input(BenchmarkId::from_parameter(format!("{kb}KB")), &src, |b, src| {
            b.iter_batched(
                || {
                    let mut doc = Document::from_str(src);
                    let mut st = SyntaxTree::new(Language::Rust);
                    st.set_text(src);
                    // Position a caret inside a function body near the middle.
                    let mid_line = doc.buffer().len_lines() / 2;
                    doc.set_caret(Point::new(mid_line, 0));
                    (doc, st)
                },
                |(mut doc, mut st)| {
                    doc.insert("x");
                    let changes = doc.drain_changes();
                    let rope = doc.buffer().rope().clone();
                    st.edit(&doc.text(), &rope, &changes);
                    criterion::black_box(st.tree().unwrap().root_node().end_byte());
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_full_parse, bench_incremental_reparse);
criterion_main!(benches);
