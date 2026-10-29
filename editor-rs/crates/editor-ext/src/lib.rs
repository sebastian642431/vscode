//! `editor-ext` — a VS Code extension host for editor-rs (Stage 4).
//!
//! This is the crate that makes editor-rs *compatible with VS Code extensions*
//! while the editor itself stays native Rust. Extensions are JavaScript, so we
//! run them where they expect to run — in Node.js — and bridge to the editor
//! over JSON-RPC:
//!
//! * [`manifest`] parses an extension's `package.json` and its `contributes`.
//! * [`vsix`] installs a `.vsix` package (and builds Open VSX URLs).
//! * [`host`] spawns Node running [`js/host.js`](../../js/host.js), which
//!   provides a `vscode` module shim and loads/activates the extension; the
//!   editor implements [`HostHandler`] to service the `vscode.*` calls.
//!
//! The architecture mirrors VS Code's own (extensions in a separate Node
//! process, the editor driving them over a message protocol), so the surface
//! can grow toward broad compatibility without reshaping anything. See
//! `host.js` for the implemented API surface and `PROGRESS.md` for coverage.
//!
//! ```no_run
//! use std::sync::{Arc, Mutex};
//! use editor_ext::{ExtensionHost, NullHandler, vsix};
//! use std::path::Path;
//!
//! let handler = Arc::new(Mutex::new(NullHandler));
//! let host = ExtensionHost::spawn(handler)?;
//! let ext = vsix::install_vsix(Path::new("my-ext.vsix"), Path::new("/tmp/my-ext"))?;
//! host.activate(&ext)?;
//! let result = host.execute_command("myExt.doThing", vec![])?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod host;
pub mod manifest;
pub mod vsix;

pub use host::{EditOutcome, ExtensionHost, HostHandler, MessageLevel, NullHandler, RangeEdit};
pub use manifest::{Contributes, ExtensionManifest};
pub use vsix::{install_vsix, open_installed, InstalledExtension};
