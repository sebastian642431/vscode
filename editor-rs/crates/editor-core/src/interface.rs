//! Integration seams for later stages.
//!
//! Stage 1 ships no language server and no extension host, but the whole point
//! of keeping `editor-core` UI-free is that those layers can be bolted on
//! without reshaping the editing model. This module defines the *contracts*
//! they will implement, so the data types (snapshots, changes, diagnostics,
//! completions) are fixed now and the rest of the editor can already be written
//! against them.
//!
//! Everything here is transport-agnostic. The real LSP client (Stage 2) and the
//! Node.js extension host (Stage 4) run in separate threads/processes and talk
//! over JSON-RPC; these traits are the in-process boundary the UI sees, so the
//! UI never blocks on a socket.

use crate::buffer::Change;
use crate::document::Document;
use crate::position::Point;

/// A stable identifier for an open document, assigned by the workspace.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DocumentId(pub u64);

/// A half-open range expressed in `(line, column)` space — the coordinate
/// system both the LSP and the UI speak.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextRange {
    pub start: Point,
    pub end: Point,
}

impl TextRange {
    pub fn new(start: Point, end: Point) -> Self {
        TextRange { start, end }
    }
}

/// An immutable, cheaply-cloned view of a document handed to a background
/// worker (parser, language server, formatter).
///
/// The text is a [`ropey::Rope`], whose clone is a copy-on-write `O(log n)`
/// operation sharing structure with the live buffer — so snapshotting a 100 MB
/// file to ship to a worker thread costs almost nothing and never copies the
/// bytes.
#[derive(Clone, Debug)]
pub struct DocumentSnapshot {
    pub id: DocumentId,
    pub version: u64,
    pub language_id: Option<String>,
    pub rope: ropey::Rope,
}

/// Severity of a [`Diagnostic`], mirroring the LSP values.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// Ergonomic alias.
pub type Severity = DiagnosticSeverity;

/// A single problem reported for a document (error, warning, …).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Diagnostic {
    pub range: TextRange,
    pub severity: DiagnosticSeverity,
    pub message: String,
    /// E.g. `"rustc"`, `"eslint"`.
    pub source: Option<String>,
}

/// Where diagnostics are delivered. The LSP client calls this (on the UI
/// thread, after receiving a `publishDiagnostics` notification) to update the
/// gutter/underlines.
pub trait DiagnosticsSink {
    fn publish(&mut self, document: DocumentId, diagnostics: Vec<Diagnostic>);
}

/// The kind of a completion item, a trimmed subset of the LSP enum.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompletionKind {
    Text,
    Function,
    Method,
    Variable,
    Field,
    Class,
    Module,
    Keyword,
    Snippet,
    Other,
}

/// A single completion suggestion.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Completion {
    pub label: String,
    /// Text actually inserted (may differ from `label`).
    pub insert_text: String,
    pub detail: Option<String>,
    pub kind: CompletionKind,
}

/// Something that can produce completions at a position. Implemented by the LSP
/// client in Stage 2 and, later, by extension-provided providers.
///
/// The signature is synchronous for simplicity of the contract, but real
/// implementations will return quickly with cached/pending results and deliver
/// the full list asynchronously through the UI's event loop.
pub trait CompletionProvider {
    fn completions(&self, snapshot: &DocumentSnapshot, at: Point) -> Vec<Completion>;
}

/// The in-process handle the UI uses to drive a language server. Each method
/// corresponds to an LSP message; implementations marshal to JSON-RPC and never
/// block the caller.
pub trait LanguageServerHandle {
    /// `textDocument/didOpen`.
    fn did_open(&mut self, snapshot: &DocumentSnapshot);

    /// `textDocument/didChange`. The [`Change`]s carry byte offsets and points,
    /// so they translate directly to incremental LSP content changes.
    fn did_change(&mut self, document: DocumentId, version: u64, changes: &[Change]);

    /// `textDocument/didClose`.
    fn did_close(&mut self, document: DocumentId);
}

/// The boundary to the Node.js extension host (Stage 4). Kept deliberately tiny
/// for now: it names the shape of the future bridge without committing to a
/// protocol.
pub trait ExtensionHost {
    /// Dispatch a command contributed by an extension (e.g. from the command
    /// palette) by its identifier, with JSON arguments.
    fn execute_command(&mut self, command_id: &str, args: serde_json_value::Value);
}

/// A tiny stand-in for `serde_json::Value` so `editor-core` need not depend on
/// serde. Stage 4 will replace this with the real type at the host boundary.
pub mod serde_json_value {
    /// Opaque JSON payload placeholder.
    #[derive(Clone, Debug, Default, PartialEq)]
    pub enum Value {
        #[default]
        Null,
        /// Raw JSON text, parsed by the extension-host bridge.
        Raw(String),
    }
}

impl Document {
    /// Produce an immutable [`DocumentSnapshot`] to hand to a background worker.
    /// Cloning the rope is `O(log n)` and shares structure with the live
    /// buffer, so this is safe to call on the UI thread after every edit.
    pub fn snapshot(&self, id: DocumentId, language_id: Option<String>) -> DocumentSnapshot {
        DocumentSnapshot {
            id,
            version: self.version(),
            language_id,
            rope: self.buffer().rope().clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_shares_text_cheaply() {
        let doc = Document::from_str("fn main() {}");
        let snap = doc.snapshot(DocumentId(1), Some("rust".into()));
        assert_eq!(snap.version, 0);
        assert_eq!(snap.rope.to_string(), "fn main() {}");
        assert_eq!(snap.language_id.as_deref(), Some("rust"));
    }
}
