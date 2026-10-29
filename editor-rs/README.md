# editor-rs

A VS Code-style code editor written in Rust, built for **speed**: minimal
typing latency, instant startup, huge files without stalls, low memory use.

> Status: **Stage 1 — base editor.** The editing engine, file system layer,
> incremental syntax highlighting and the front-end-agnostic UI logic are
> complete, tested and benchmarked. The GPUI window is provided as a separate
> `gpui-frontend` crate (see below). See [`PROGRESS.md`](PROGRESS.md) for what's
> done, what's next, and benchmark results.

## Architecture

editor-rs is a Cargo workspace of four core crates plus a separate GPUI shell.
The guiding rule is that **nothing on the typing path may block** — parsing,
search and disk I/O all happen off the keystroke path — and that **all logic is
testable without a GPU or display**.

```
editor-rs/
├── crates/
│   ├── editor-core      text buffer, editing, selections, undo/redo  (no UI deps)
│   ├── editor-fs        file tree, file watching, project search
│   ├── editor-syntax    incremental tree-sitter parsing + highlighting
│   └── editor-ui        keymap, command palette, tabs, workspace model + CLI
└── gpui-frontend/       the GPUI desktop window (excluded from the workspace)
```

| Crate | Responsibility | Key dependencies |
|-------|----------------|------------------|
| `editor-core` | Rope-backed buffer, multi-cursor selections, grouped undo/redo, editing & movement commands, LSP/extension interfaces | [`ropey`] |
| `editor-fs` | Lazy `.gitignore`-aware file tree, `notify` watcher, parallel project search | [`ignore`], [`grep`], [`notify`] |
| `editor-syntax` | Incremental reparse from the change stream, viewport highlighting | [`tree-sitter`] (+ Rust/TS/JSON/Python grammars) |
| `editor-ui` | JSON keybindings (with chords + `when`), fuzzy command palette, tabs, the `Workspace` brain, and a headless CLI | the three crates above, `serde` |

Data flows one way on each keystroke:

```
key event ─▶ KeyStroke ─▶ Keymap.resolve ─▶ command id ─▶ Workspace.execute
                                                              │
                                                              ▼
                                        editor-core Document mutates (sync, O(log n))
                                                              │
                                      Document.drain_changes() ─▶ editor-syntax / LSP (async, off-thread)
```

Every edit emits a `Change` carrying char **and** byte offsets plus
`(line, column)` points — exactly what tree-sitter's `InputEdit` and the LSP
`didChange` protocol need — so the async layers update incrementally without
ever rescanning the document.

## Build & run

Requires a recent stable Rust toolchain (1.80+). The same commands work on
**Linux, macOS and Windows**.

```bash
cd editor-rs

# build the whole core workspace (release)
cargo build --release

# run the test suite
cargo test

# lint (no warnings)
cargo clippy --all-targets

# run the benchmarks
cargo bench
```

### The headless CLI

The core workspace builds everywhere (no GPU/display needed) and ships a
headless driver binary that exercises the full pipeline:

```bash
# status + usage
cargo run --release -p editor-ui --bin editor

# open a folder and list it (honours .gitignore)
cargo run --release -p editor-ui --bin editor -- path/to/folder

# open a file: detected language, line count, tree-sitter token highlights
cargo run --release -p editor-ui --bin editor -- src/main.rs
```

### The graphical editor (GPUI)

The real window lives in [`gpui-frontend/`](gpui-frontend/README.md). It uses
GPUI from Zed, which needs a GPU, a display server and the platform graphics
stack, and it clones the (large) Zed repository on first build. To keep the
core workspace fast and headless-CI-friendly, that crate is **excluded** from
the workspace and built on its own:

```bash
cd editor-rs/gpui-frontend
cargo run --release                    # empty editor
cargo run --release -- path/to/folder  # open a folder
cargo run --release -- src/main.rs     # open a file
```

On Linux you'll need the Vulkan loader/drivers and the usual X11/Wayland
development libraries installed.

## Features in Stage 1

- **Editing**: insert, newline with indentation, backspace, forward/word/line
  delete; cursor movement by char, word, line (incl. smart-home) and document;
  vertical movement with a sticky goal column.
- **Multi-cursor**: add cursors above/below, column-consistent multi-cursor
  edits applied in a single pass, atomic multi-cursor undo.
- **Undo/redo**: grouped, invertible transactions with keystroke coalescing.
- **Clipboard**: cut/copy/paste with line-mode and multi-cursor distribution.
- **Files**: open folder with a lazy, `.gitignore`-aware tree; file watching;
  open/save documents with dirty tracking.
- **Search**: parallel, cancellable project search (literal/regex/whole-word,
  globs, result caps) over the ripgrep crates.
- **Syntax**: incremental tree-sitter parsing for Rust, TypeScript/TSX, JSON
  and Python; viewport syntax highlighting.
- **Command palette & keybindings**: fuzzy command palette; JSON keybindings
  with chords and `when`-clause contexts, user overrides.

## Later stages (planned)

2. LSP client: completion, go-to-definition, diagnostics (interfaces already
   defined in `editor_core::interface`).
3. VS Code themes, TextMate grammars, snippets and keybindings read
   declaratively from JSON/TextMate.
4. A Node.js extension host over JSON-RPC implementing the `vscode.*` API
   (Open VSX marketplace).

[`ropey`]: https://crates.io/crates/ropey
[`ignore`]: https://crates.io/crates/ignore
[`grep`]: https://crates.io/crates/grep
[`notify`]: https://crates.io/crates/notify
[`tree-sitter`]: https://crates.io/crates/tree-sitter
