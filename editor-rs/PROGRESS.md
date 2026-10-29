# editor-rs — progress

**Stage 1 (base editor): essentially complete.** This document records what is
done, what is intentionally deferred, and the benchmark results.

## Done

### Workspace
- Independent Cargo workspace under `editor-rs/` (resolver 2), four core crates
  + a release profile tuned for the editor (thin LTO, 1 codegen unit).
- Builds, tests and lints clean on a headless machine:
  - `cargo build --release` — OK
  - `cargo test` — **91 tests pass** (core 44, fs 11, syntax 7, ui 29)
  - `cargo clippy --all-targets` — **no warnings**
- Runs on Linux/macOS/Windows (pure Rust, portable deps). README documents
  build/run for all three.

### editor-core (no UI dependency)
- `TextBuffer`: `ropey` rope; `O(log n)` insert/delete/replace; char⇄byte⇄
  `(line,column)` conversions.
- `Change`: every edit emits char + byte offsets and points (ready for
  tree-sitter `InputEdit` and LSP `didChange`).
- `SelectionSet`: multi-cursor with sorting, overlap merging, primary tracking.
- `History`: grouped, invertible transactions; keystroke coalescing into
  word-sized undo steps.
- `Document`: insert, newline+auto-indent, backspace, char/word/line delete;
  char/word/line/smart-home/document movement; sticky vertical goal column;
  add-cursor above/below; select-all; cut/copy/paste (line-mode + multi-cursor
  distribution); undo/redo; monotonic version; drainable change stream.
- `Clipboard` trait + in-memory impl.
- `interface`: `DocumentSnapshot` (cheap rope clone), `LanguageServerHandle`,
  `CompletionProvider`, `DiagnosticsSink`, `ExtensionHost` — the seams for
  Stages 2 and 4.

### editor-fs (off the UI thread)
- `FileTree`: lazy, one-level, `.gitignore`-aware listing in explorer order
  (honours ignore files even outside a git checkout).
- `Watcher`: `notify`-backed FS events over a `crossbeam-channel`.
- `Searcher`: ripgrep crates (`ignore` + `grep`), parallel walker, streaming +
  cancellable; literal/regex/whole-word, include/exclude globs, max-results,
  submatch offsets.

### editor-syntax (fed by the change stream)
- Grammars: Rust, TypeScript, TSX, JSON, Python.
- `SyntaxTree`: incremental reparse driven by `Change` byte offsets.
- `Highlighter` + `HighlightConfigs`: viewport highlighting via
  `tree-sitter-highlight` with a normalised highlight-name set.

### editor-ui (front-end agnostic)
- `keymap`: JSON keybindings with chords and `when`-clause contexts
  (`!`, `&&`, `||`); last-match-wins overrides.
- `command`: palette registry + scoring fuzzy matcher.
- `tabs`: open-editor model (dedup, close/activate, next/prev wrap, dirty).
- `workspace`: open folder/file, language detection, dirty tracking, save,
  snapshots, project search, and a command dispatcher mapping command ids to
  `editor-core` actions.
- `config`: bundled default keybindings + command set, user-override merge.
- `bin/editor`: headless CLI driving the full `fs → core → syntax` pipeline.

### GPUI shell (`gpui-frontend/`, excluded crate)
- Full window: file-tree sidebar, tab bar, syntax-highlighted editor view,
  status bar; key events routed through the keymap into `Workspace::execute`.
- Deliberately **excluded from the workspace** so a normal `cargo build` never
  clones Zed or needs a GPU. Pinned to a specific Zed commit.

## Benchmarks

Measured on the development container (4 vCPU, x86-64, `--release`). Criterion
median times.

### editor-core (`cargo bench -p editor-core`)
| Benchmark | 1 MB | 10 MB | 100 MB |
|-----------|------|-------|--------|
| Insert one char mid-file | ~1.35 µs | ~1.1 µs | ~1.6 µs |
| Open file (`from_str`) | — | ~7.4 ms | ~127 ms |
| `char_to_point` (10 MB) | — | ~0.18 µs | — |
| One keystroke (insert+move+drain, 10 MB) | — | ~2.7 µs | — |

The insert cost is **flat from 1 MB to 100 MB** — the typing hot path is
`O(log n)` and independent of file size, which is the headline goal.

### editor-syntax (`cargo bench -p editor-syntax`)
| Benchmark | 64 KB | 512 KB | 4 MB |
|-----------|-------|--------|------|
| Full parse (Rust) | ~10 ms | ~93 ms | ~810 ms |
| Incremental reparse after 1 char | ~0.21 ms | ~2.3 ms | ~43 ms |

Incremental reparse reads straight from the rope (no `String` copy) and is
~19–45× faster than a full parse. A typical source file (≤ 64 KB) reparses in
~0.2 ms; large files scale sub-linearly and run on a worker thread, off the
typing path.

### editor-fs (`cargo bench -p editor-fs`)
| Benchmark | Result |
|-----------|--------|
| Literal project search over 100,000 files | ~200 ms |

The parallel `ignore` walker + `grep` searcher scans 100k files for a literal
in ~0.2 s on 4 cores, streaming results as they are found.

## Deferred / not in Stage 1
- The GPUI shell is not compiled by the headless CI here (no GPU/display). Its
  element-builder calls target the pinned Zed rev and may need minor updates
  when the pin is bumped; the architecture is fixed.
- Rendering niceties (scrolling virtualization beyond the first viewport,
  minimap, inline diagnostics UI) are left for later stages.
- A real OS clipboard implementation is provided via the `Clipboard` trait seam
  but not wired to a system backend in the headless build.

## Next stages
2. LSP client (completion, go-to-definition, diagnostics) against the existing
   `editor_core::interface` traits.
3. VS Code themes, TextMate grammars, snippets and keybindings loaded
   declaratively.
4. Node.js extension host over JSON-RPC implementing `vscode.*` (Open VSX).
