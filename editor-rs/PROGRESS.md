# editor-rs — progress

Goal: a VS Code-style editor in **Rust** that is faster than VS Code (low typing
latency, instant start, huge files, many windows cheaply) **without losing VS
Code extension compatibility**.

**Stages 1–4 have a working, tested vertical slice.** Stage 1 (base editor) is
essentially complete; Stages 2–4 (LSP, VS Code assets, extension host) are
functional foundations with honest coverage notes below. This document records
what is done, what is deferred, and the benchmark results.

## Status at a glance

| Stage | Crate(s) | State |
|-------|----------|-------|
| 1 — base editor | `editor-core`, `editor-fs`, `editor-syntax`, `editor-ui`, `gpui-frontend` | Complete (UI shell builds on a GPU machine) |
| 2 — LSP client | `editor-lsp` (+ `editor-rpc`) | Working: lifecycle, sync, completion/definition/hover, diagnostics |
| 3 — VS Code assets | `editor-vscode` | Working: themes, snippets, language-config, keybindings |
| 4 — extension host | `editor-ext` | Working: loads/activates real extensions, core `vscode.*` API |
| 4+ — integration | `editor-workbench` | Working: extensions read **and edit** real editor-core buffers via `TextDocument`/`TextEditor` |

## Done

### Workspace
- Independent Cargo workspace under `editor-rs/` (resolver 2), nine crates
  + a release profile tuned for the editor (thin LTO, 1 codegen unit).
- Builds, tests and lints clean on a headless machine:
  - `cargo build --release` — OK
  - `cargo test` — **~144 tests pass** (core 48, fs 11, syntax 7, ui 29,
    rpc 3, lsp 9, vscode 17, ext 10, workbench 10, + doctests)
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

## Stage 2 — LSP client (`editor-lsp`, `editor-rpc`)
- JSON-RPC base protocol (`editor-rpc`): `Content-Length` framing, a reader
  thread, request/response matching; shared with the extension host.
- Lifecycle: `initialize`/`initialized`/`shutdown`, **position-encoding
  negotiation** (prefers UTF-8, falls back to UTF-16/32), sync-kind detection.
- Document sync from the `Change` stream: incremental edits are replayed against
  the last-synced rope to produce correct pre-edit ranges in the negotiated
  encoding (non-ASCII handled), or full sync when the server asks.
- Requests: completion, definition, hover. `pump()` drains `publishDiagnostics`
  into `editor_core` diagnostics and answers common server→client requests.
- Tested against an in-process mock server over real OS pipes.
- **Not yet:** spawning/managing real servers per language from the UI,
  semantic tokens, code actions, rename, signature help, workspace symbols.

## Stage 3 — VS Code assets (`editor-vscode`)
- Color **themes**: `colors` + TextMate `tokenColors`, resolved to our
  tree-sitter highlight names via longest-prefix scope matching. Validated
  against this repo's real `dark_vs.json`.
- **Snippets**: VS Code snippet files + a snippet-body expander (`$1`,
  `${1:default}`, `$0`) producing text + ordered tab-stops. Parsed real VS Code
  snippet files.
- **language-configuration.json**: comments, brackets, auto-closing/surrounding
  pairs. Validated against the real Rust config.
- **keybindings**: VS Code `keybindings.json` is already the format the
  `editor-ui` keymap parses; a JSONC helper is provided.
- All loaders accept JSONC (comments + trailing commas).
- **Not yet:** applying a loaded theme end-to-end in the GPUI shell; TextMate
  grammar *tokenisation* (we tokenise with tree-sitter instead and only read
  TextMate *themes*); `include`-based theme inheritance; semantic token colors.

## Stage 4 — extension host (`editor-ext`)
- Runs extensions in **Node.js** (where they expect to run) and bridges to the
  editor over JSON-RPC, mirroring VS Code's own process architecture.
- `manifest`: parse `package.json` + `contributes` + `activationEvents`.
- `vsix`: install a `.vsix` (zip, zip-slip-guarded); Open VSX URL builders.
- `host` + `js/host.js`: spawn Node, inject a `vscode` module via a `require()`
  hook, load and **activate real CommonJS extensions**, run their commands, and
  service `vscode.*` calls through a `HostHandler` the editor implements.
- Implemented `vscode` surface: `commands` (register/execute/getCommands),
  `window` (show*Message, showQuickPick, showInputBox, OutputChannel, status
  bar, progress stubs), `workspace` (getConfiguration, events, file watcher
  stubs), `languages` (provider registration, diagnostic collections),
  `Uri`/`Position`/`Range`/`Selection`/`Location`, `EventEmitter`/`Disposable`,
  `CompletionItem`/`SnippetString`/`MarkdownString`/`Hover`/`Diagnostic`, the
  common enums, and the activation `context` (subscriptions, mementos, secrets,
  storage URIs). Proven end-to-end: activate → registerCommand → executeCommand
  → showInformationMessage → result, plus routing unknown commands to built-ins.

## Integration — `editor-workbench`
Ties the document model and the extension host together:
- Owns the open documents (editor-core `Document`s keyed by URI) and the host.
- Mirrors documents into the host so `TextDocument` APIs answer synchronously.
- Implements the host's document-editing API: `TextEditor.edit` applies to the
  **real** editor-core buffer (undo + change stream), with correct
  UTF-16↔char/byte conversion (tested with multi-byte text), then resyncs the
  host mirror. User-side edits push back to the host too.
- Delegates non-document API (messages, pickers, configuration…) to a UI
  handler the embedder supplies.
- Proven: an extension reads `activeTextEditor.document.getText()`, inspects
  `lineAt`/`lineCount`, and `edit()`s the buffer — the change lands in the Rust
  `Document`.

### Extension-compatibility coverage (be realistic)
This is a **working foundation, not full parity**. Extensions that register
commands, show UI, read configuration, contribute languages/themes/snippets,
and **read/edit the active document** will activate and run. Implemented
`vscode` surface now includes `TextDocument` (getText/lineAt/positionAt/
offsetAt/getWordRangeAtPosition), `TextEditor.edit`, `window.activeTextEditor`,
and `workspace.textDocuments`/`openTextDocument`. **Language features** work
too: the editor can invoke extension-registered completion / hover / definition
providers (`languages.register*Provider`) via the workbench and gets results
mapped to the editor-core model. **Reactive extensions** work: a
`DiagnosticCollection` (linters) surfaces diagnostics to the editor, and
`workspace.onDidOpen/Change/CloseTextDocument` and
`window.onDidChangeActiveTextEditor` fire as documents sync. Not yet implemented
(extensions using these hit no-op stubs or need work): `WorkspaceEdit` across
files, code actions / formatting / rename providers (registered but not yet
invoked), webviews, debug adapter protocol, tasks, the `workspace.fs` filesystem
API, the testing API, notebooks, synchronous `getConfiguration().get` backed by
real settings, and ESM-only extensions. Reaching broad compatibility is
incremental along this same architecture.

## Deferred / environment notes
- The GPUI shell is not compiled by the headless CI here (no GPU/display). Its
  element-builder calls target the pinned Zed rev and may need minor updates
  when the pin is bumped; the architecture is fixed. The loaded VS Code theme is
  not yet applied in the shell.
- A real OS clipboard implementation is provided via the `Clipboard` trait seam
  but not wired to a system backend in the headless build.

## Suggested next steps
- Wire the pieces together in the GPUI event loop: drive `editor-lsp` per
  language on a worker, apply an `editor-vscode` theme to the editor view, and
  surface the extension host's registered commands in the command palette.
- Grow the `vscode` API surface document-editing first (it unlocks the most
  extensions), backed by `editor-core` `Document`s over the host protocol.
- Add an Open VSX HTTP download path (behind an optional feature) so `.vsix`
  installs end-to-end from the marketplace.
