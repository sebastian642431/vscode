//! Benchmark: project search across a large synthetic tree.
//!
//! Builds a throwaway directory of ~100,000 files once, then measures a full
//! project search over it. Run with `cargo bench -p editor-fs`.

use std::fs;
use std::path::Path;

use criterion::{criterion_group, criterion_main, Criterion};
use editor_fs::{SearchOptions, Searcher};

/// Create `dirs` directories each holding `files_per_dir` small source files,
/// a handful of which contain the needle.
fn build_tree(root: &Path, dirs: usize, files_per_dir: usize) {
    let body_plain = "fn helper() {\n    let x = 1 + 2;\n    do_work(x);\n}\n";
    let body_needle =
        "fn helper() {\n    let NEEDLE_TOKEN = 1;\n    do_work(NEEDLE_TOKEN);\n}\n";
    for d in 0..dirs {
        let dir = root.join(format!("pkg_{d:04}"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..files_per_dir {
            let body = if d % 50 == 0 && f == 0 {
                body_needle
            } else {
                body_plain
            };
            fs::write(dir.join(format!("mod_{f:04}.rs")), body).unwrap();
        }
    }
}

fn bench_project_search(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    // 1000 dirs * 100 files = 100,000 files.
    build_tree(tmp.path(), 1000, 100);

    let mut group = c.benchmark_group("project_search_100k_files");
    group.sample_size(10);
    group.bench_function("literal_needle", |b| {
        b.iter(|| {
            let s = Searcher::new(tmp.path(), SearchOptions::literal("NEEDLE_TOKEN"));
            let matches = s.collect().unwrap();
            criterion::black_box(matches.len());
        });
    });
    group.finish();
}

criterion_group!(benches, bench_project_search);
criterion_main!(benches);
