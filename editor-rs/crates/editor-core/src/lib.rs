//! `editor-core` — the UI-independent heart of editor-rs.
//!
//! This crate owns everything that defines *what the text is and how it
//! changes*, with **no dependency on any UI toolkit**. That separation is
//! deliberate: every piece of editing behaviour can be unit-tested and
//! benchmarked on a headless machine, and the same model can later be driven
//! by the GPUI front-end, an LSP client, or the extension host.
//!
//! # Performance contract
//!
//! The golden rule of editor-rs is **nothing on the typing path may block**.
//! This crate upholds its half of that contract:
//!
//! * Text is stored in a [`ropey`] rope, so inserting or deleting at any
//!   offset — even in a 100 MB file — is `O(log n)`, never `O(n)`.
//! * Every edit returns a [`Change`] carrying both *char* and *byte* offsets
//!   plus `(line, column)` [`Point`]s. That is exactly the shape tree-sitter's
//!   `InputEdit` and the LSP `textDocument/didChange` protocol need, so the
//!   async syntax and language-server layers can be fed incrementally without
//!   re-reading the whole document.
//! * No method here spawns threads, touches the disk, or waits on a lock.
//!   Expensive, cancellable work (parsing, search, I/O) lives in the sibling
//!   crates and consumes the [`Change`] stream off the hot path.
//!
//! # Module map
//!
//! * [`position`] — `(line, column)` [`Point`]s and offset conversions.
//! * [`buffer`] — the rope-backed [`TextBuffer`] and the [`Change`] type.
//! * [`selection`] — carets, [`Selection`]s and the multi-cursor
//!   [`SelectionSet`].
//! * [`history`] — grouped, invertible [`Transaction`]s for undo/redo.
//! * [`clipboard`] — a [`Clipboard`] trait plus an in-memory implementation.
//! * [`document`] — [`Document`], the editable unit that ties the buffer,
//!   selections and history together and exposes the editing commands.
//! * [`interface`] — the traits the LSP client and extension host will plug
//!   into in later stages.

pub mod buffer;
pub mod clipboard;
pub mod document;
pub mod history;
pub mod interface;
pub mod position;
pub mod selection;

pub use buffer::{Change, TextBuffer};
pub use clipboard::{Clipboard, MemoryClipboard};
pub use document::{Document, Granularity, Movement};
pub use history::{History, Transaction};
pub use interface::{
    Completion, CompletionProvider, Diagnostic, DiagnosticSeverity, DiagnosticsSink, DocumentId,
    DocumentSnapshot, LanguageServerHandle, Severity, TextRange,
};
pub use position::Point;
pub use selection::{Selection, SelectionSet};
