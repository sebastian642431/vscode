# editor-rs

A VS Code-style code editor written in Rust, built for **speed**: minimal
typing latency, instant startup, huge files without stalls, low memory use.

> Status: **Stages 1–4 have a working, tested vertical slice** — base editor,
> LSP client, VS Code asset loading, and a Node.js extension host that loads and
> activates real VS Code extensions. Goal: be faster than VS Code **without
> losing extension compatibility**. The GPUI window is a separate
> `gpui-frontend` crate (see below). See [`PROGRESS.md`](PROGRESS.md) for exact
> coverage (including honest extension-compat limits) and benchmark results.

## Architecture

editor-rs is a Cargo workspace of native-Rust crates plus a separate GPUI
shell. The guiding rules: **nothing on the typing path may block** (parsing,
search, I/O, LSP and extensions all run off the keystroke path), **all logic is
testable without a GPU or display**, and the editor **reuses VS Code's own
formats and runs VS Code's own extensions** rather than inventing new ones.

```
editor-rs/
├── crates/
│   ├── editor-core      text buffer, editing, selections, undo/redo  (no UI deps)
│   ├── editor-fs        file tree, file watching, project search
│   ├── editor-syntax    incremental tree-sitter parsing + highlighting
│   ├── editor-rpc       shared JSON-RPC base protocol
│   ├── editor-lsp       Language Server Protocol client
│   ├── editor-vscode    load VS Code themes / snippets / lang-config / keybindings
│   ├── editor-ext       VS Code extension host (Node.js + vscode.* shim)
│   ├── editor-workbench integration: extensions read/edit real documents
│   └── editor-ui        keymap, command palette, tabs, workspace model + CLI
└── gpui-frontend/       the GPUI desktop window (excluded from the workspace)
```

| Crate | Responsibility | Key dependencies |
|-------|----------------|------------------|
| `editor-core` | Rope-backed buffer, multi-cursor selections, grouped undo/redo, editing & movement commands, LSP/extension interfaces | [`ropey`] |
| `editor-fs` | Lazy `.gitignore`-aware file tree, `notify` watcher, parallel project search | [`ignore`], [`grep`], [`notify`] |
| `editor-syntax` | Incremental reparse from the change stream, viewport highlighting | [`tree-sitter`] (+ Rust/TS/JSON/Python grammars) |
| `editor-rpc` | `Content-Length` JSON-RPC framing + request/response matching | `serde_json`, `crossbeam-channel` |
| `editor-lsp` | LSP client: lifecycle, encoding negotiation, incremental sync, completion/definition/hover, diagnostics | [`lsp-types`], `editor-rpc` |
| `editor-vscode` | Load VS Code color themes, snippets, `language-configuration.json`, keybindings (JSONC) | `serde_json` |
| `editor-ext` | VS Code extension host: spawn Node, `vscode.*` shim, load/activate extensions, VSIX/Open VSX | `editor-rpc`, [`zip`] |
| `editor-workbench` | Integration: documents + host wired so extensions read/edit real `editor-core` buffers via the `vscode` API | `editor-core`, `editor-ext` |
| `editor-ui` | JSON keybindings (with chords + `when`), fuzzy command palette, tabs, the `Workspace` brain, and a headless CLI | the editor crates above, `serde` |

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

Requires a recent stable Rust toolchain (1.87+). The same commands work on
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

## What works today

**Stage 1 — editing**
- Insert, newline with indentation, backspace, forward/word/line delete; cursor
  movement by char, word, line (incl. smart-home) and document; sticky vertical
  goal column.
- Multi-cursor: add cursors above/below, column-consistent single-pass edits,
  atomic multi-cursor undo. Grouped undo/redo with keystroke coalescing.
- Cut/copy/paste with line-mode and multi-cursor distribution.
- Files: lazy `.gitignore`-aware tree, file watching, open/save + dirty tracking.
- Search: parallel, cancellable project search (literal/regex/whole-word, globs,
  caps) over the ripgrep crates.
- Syntax: incremental tree-sitter parsing for Rust, TypeScript/TSX, JSON, Python.
- Fuzzy command palette; JSON keybindings with chords + `when` contexts.

**Stage 2 — LSP client** — lifecycle + position-encoding negotiation,
incremental document sync from the change stream, completion/definition/hover,
and diagnostics.

**Stage 3 — VS Code assets** — load VS Code color themes (mapped to our
highlight names), snippet files (with a tab-stop expander),
`language-configuration.json`, and `keybindings.json`. Validated against this
repo's real VS Code assets.

**Stage 4 — extension host** — spawn Node.js, inject a `vscode` module shim,
and **load/activate real VS Code extensions**, run the commands they register,
and service their `vscode.*` calls back in the editor. Install from `.vsix`;
Open VSX URL builders included. The `editor-workbench` crate wires documents to
the host so an extension can **read and edit the real editor-core buffer**
through `TextDocument`/`TextEditor` (with correct UTF-16↔char conversion).

> Extension compatibility is a **working foundation, not full parity yet** —
> simple-to-medium extensions (commands, UI, configuration, static
> contributions) activate and run; heavier APIs (live document editing,
> webviews, debug, tasks, FS) are stubbed. [`PROGRESS.md`](PROGRESS.md) lists
> exactly what is and isn't covered, and the next steps.

[`ropey`]: https://crates.io/crates/ropey
[`ignore`]: https://crates.io/crates/ignore
[`grep`]: https://crates.io/crates/grep
[`notify`]: https://crates.io/crates/notify
[`tree-sitter`]: https://crates.io/crates/tree-sitter
[`lsp-types`]: https://crates.io/crates/lsp-types
[`zip`]: https://crates.io/crates/zip
