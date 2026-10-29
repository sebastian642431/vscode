# gpui-frontend

The GPUI desktop shell for editor-rs — the actual window, file-tree sidebar,
tabs, syntax-highlighted editor view and status bar.

## Why it's a separate crate

GPUI (from [Zed](https://github.com/zed-industries/zed)) is pulled in as a git
dependency and requires a GPU, a display server and the platform graphics stack
(Vulkan on Linux, Metal on macOS, DirectX on Windows). Building it also clones
the large Zed repository.

To keep the **core workspace** (`../`) fast to build, testable on headless CI,
and free of that heavy dependency, this crate is **excluded** from the core
Cargo workspace (see `exclude` in `../Cargo.toml`). It has its own `Cargo.lock`
and is built on its own.

## Building

You need a machine with a working display/GPU and the platform graphics
development packages installed. On Linux that typically means Vulkan loader +
drivers and the usual X11/Wayland dev libraries.

```bash
cd editor-rs/gpui-frontend
cargo run --release
# or open something:
cargo run --release -- /path/to/folder
cargo run --release -- /path/to/file.rs
```

The GPUI dependency is pinned to a specific Zed commit in `Cargo.toml`. GPUI's
API changes frequently; if you bump the pin you may need small adjustments to
the element-builder calls in `src/main.rs`. The architecture does not change:

1. A key event is turned into an `editor_ui::keymap::KeyStroke`.
2. `Keymap::resolve` maps the stroke(s) + context to a command id (or reports a
   pending chord).
3. `Workspace::execute` runs the command against the active
   `editor_core::Document`.
4. The change stream is drained to the syntax layer and the view repaints.

All of that logic lives in the core crates and is unit-tested headlessly; this
crate only renders it and feeds it input.
