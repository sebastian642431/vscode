//! The high-level LSP client used by the editor.
//!
//! It owns a [`Connection`], tracks per-document state (so document sync and
//! coordinate conversion are correct), negotiates the position encoding, and
//! exposes the handful of requests the editor needs. Everything is synchronous
//! and intended to run on a worker thread — never the typing path.

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use editor_core::interface::Diagnostic;
use editor_core::Change;
use lsp_types::Uri;
use ropey::Rope;
use serde_json::{json, Value};

use crate::convert::{char_to_lsp, diagnostic_from_lsp, PositionEncoding};
use crate::jsonrpc::{Connection, Incoming, Result};

/// How the server wants document changes delivered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SyncKind {
    Full,
    Incremental,
    None,
}

struct DocState {
    rope: Rope,
    version: i32,
}

/// A diagnostics update for one document.
#[derive(Clone, Debug)]
pub struct DiagnosticsUpdate {
    pub uri: Uri,
    pub version: Option<i32>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A connected language server.
pub struct LspClient {
    conn: Connection,
    child: Option<Child>,
    encoding: PositionEncoding,
    sync_kind: SyncKind,
    docs: Mutex<HashMap<Uri, DocState>>,
    timeout: Duration,
}

impl LspClient {
    /// Build a client over an existing reader/writer pair (used by tests).
    pub fn with_streams(
        reader: impl std::io::Read + Send + 'static,
        writer: impl std::io::Write + Send + 'static,
    ) -> LspClient {
        LspClient {
            conn: Connection::new(reader, writer),
            child: None,
            encoding: PositionEncoding::Utf16,
            sync_kind: SyncKind::Full,
            docs: Mutex::new(HashMap::new()),
            timeout: Duration::from_secs(10),
        }
    }

    /// Spawn a language server process and connect to its stdio.
    pub fn spawn(mut command: Command) -> std::io::Result<LspClient> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut client = LspClient::with_streams(stdout, stdin);
        client.child = Some(child);
        Ok(client)
    }

    pub fn position_encoding(&self) -> PositionEncoding {
        self.encoding
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.conn.request(method, params, self.timeout)
    }

    // ----- lifecycle ---------------------------------------------------------

    /// Run the `initialize` / `initialized` handshake, negotiating the position
    /// encoding and remembering the server's document-sync preference. Returns
    /// the raw `ServerCapabilities` JSON for the caller to inspect.
    pub fn initialize(&mut self, root_uri: Option<Uri>) -> Result<Value> {
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "editor-rs", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": root_uri,
            "capabilities": {
                "general": {
                    // Prefer UTF-8 so our byte offsets map directly; accept the
                    // others if the server insists.
                    "positionEncodings": ["utf-8", "utf-16", "utf-32"]
                },
                "textDocument": {
                    "synchronization": { "didSave": true },
                    "completion": {
                        "completionItem": { "snippetSupport": true }
                    },
                    "hover": {},
                    "definition": {},
                    "publishDiagnostics": {}
                },
                "workspace": {}
            }
        });

        let result = self.request("initialize", params)?;

        // Negotiated position encoding (capture owned so `result` isn't borrowed).
        let enc_str = result
            .get("capabilities")
            .and_then(|c| c.get("positionEncoding"))
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        if let Some(enc) = enc_str {
            let kind: lsp_types::PositionEncodingKind = enc.into();
            self.encoding = PositionEncoding::from_lsp(&kind);
        }

        // Document sync kind (may be a number or an object with `.change`).
        self.sync_kind = parse_sync_kind(
            result
                .get("capabilities")
                .and_then(|c| c.get("textDocumentSync")),
        );

        self.conn.notify("initialized", json!({}))?;
        Ok(result)
    }

    /// Politely shut the server down (`shutdown` request + `exit` notification).
    pub fn shutdown(&self) -> Result<()> {
        let _ = self.request("shutdown", Value::Null);
        self.conn.notify("exit", Value::Null)
    }

    // ----- document sync -----------------------------------------------------

    pub fn did_open(&self, uri: Uri, language_id: &str, text: &str) -> Result<()> {
        self.docs.lock().unwrap().insert(
            uri.clone(),
            DocState {
                rope: Rope::from_str(text),
                version: 1,
            },
        );
        self.conn.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }),
        )
    }

    /// Send an incremental (or full, per server preference) change built from
    /// editor-core [`Change`]s. `new_rope` is the buffer *after* the changes.
    pub fn did_change(&self, uri: &Uri, new_rope: &Rope, changes: &[Change]) -> Result<()> {
        let mut docs = self.docs.lock().unwrap();
        let Some(state) = docs.get_mut(uri) else {
            return Ok(()); // document not open on the server
        };
        state.version += 1;
        let version = state.version;

        let content_changes = match self.sync_kind {
            SyncKind::None => return Ok(()),
            SyncKind::Full => {
                json!([{ "text": new_rope.to_string() }])
            }
            SyncKind::Incremental => {
                // Replay the changes onto a copy of the last-synced rope,
                // emitting each one's range in the *pre-change* coordinate
                // space the server expects.
                let mut old = state.rope.clone();
                let enc = self.encoding;
                let mut events = Vec::with_capacity(changes.len());
                for ch in changes {
                    let start = char_to_lsp(&old, ch.start_char, enc);
                    let end = char_to_lsp(&old, ch.old_end_char, enc);
                    events.push(json!({
                        "range": { "start": start, "end": end },
                        "text": ch.inserted,
                    }));
                    let s = ch.start_char.min(old.len_chars());
                    let e = ch.old_end_char.min(old.len_chars());
                    if e > s {
                        old.remove(s..e);
                    }
                    if !ch.inserted.is_empty() {
                        old.insert(s, &ch.inserted);
                    }
                }
                Value::Array(events)
            }
        };

        state.rope = new_rope.clone();

        self.conn.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": content_changes,
            }),
        )
    }

    pub fn did_close(&self, uri: &Uri) -> Result<()> {
        self.docs.lock().unwrap().remove(uri);
        self.conn.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        )
    }

    // ----- requests ----------------------------------------------------------

    fn position_params(&self, uri: &Uri, char_offset: usize) -> Option<Value> {
        let docs = self.docs.lock().unwrap();
        let state = docs.get(uri)?;
        let pos = char_to_lsp(&state.rope, char_offset, self.encoding);
        Some(json!({
            "textDocument": { "uri": uri },
            "position": pos,
        }))
    }

    /// Request completions at a char offset. Returns the raw items as JSON
    /// values (the caller maps them to its UI model via `editor-core`).
    pub fn completion(&self, uri: &Uri, char_offset: usize) -> Result<Vec<Value>> {
        let Some(params) = self.position_params(uri, char_offset) else {
            return Ok(Vec::new());
        };
        let result = self.request("textDocument/completion", params)?;
        Ok(match result {
            Value::Array(items) => items,
            Value::Object(mut obj) => match obj.remove("items") {
                Some(Value::Array(items)) => items,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        })
    }

    /// Request the definition location(s) at a char offset.
    pub fn definition(&self, uri: &Uri, char_offset: usize) -> Result<Vec<Value>> {
        let Some(params) = self.position_params(uri, char_offset) else {
            return Ok(Vec::new());
        };
        let result = self.request("textDocument/definition", params)?;
        Ok(match result {
            Value::Array(locs) => locs,
            Value::Null => Vec::new(),
            other => vec![other], // a single Location
        })
    }

    /// Request hover text at a char offset.
    pub fn hover(&self, uri: &Uri, char_offset: usize) -> Result<Option<Value>> {
        let Some(params) = self.position_params(uri, char_offset) else {
            return Ok(None);
        };
        let result = self.request("textDocument/hover", params)?;
        Ok(if result.is_null() { None } else { Some(result) })
    }

    // ----- server-initiated traffic -----------------------------------------

    /// Drain everything the server has pushed: collect `publishDiagnostics`
    /// notifications (converted to editor-core diagnostics) and answer the
    /// common server→client requests so the server doesn't stall. Call this
    /// from the event loop.
    pub fn pump(&self) -> Vec<DiagnosticsUpdate> {
        let mut updates = Vec::new();
        while let Ok(msg) = self.conn.incoming().try_recv() {
            match msg {
                Incoming::Notification { method, params } => {
                    if method == "textDocument/publishDiagnostics" {
                        if let Some(update) = self.parse_diagnostics(&params) {
                            updates.push(update);
                        }
                    }
                }
                Incoming::Request { id, method, .. } => {
                    // Minimal but correct replies to keep servers happy.
                    let result = match method.as_str() {
                        "workspace/configuration" => json!([Value::Null]),
                        "client/registerCapability"
                        | "client/unregisterCapability"
                        | "window/workDoneProgress/create" => Value::Null,
                        _ => Value::Null,
                    };
                    let _ = self.conn.respond(id, result);
                }
            }
        }
        updates
    }

    fn parse_diagnostics(&self, params: &Value) -> Option<DiagnosticsUpdate> {
        let uri: Uri = serde_json::from_value(params.get("uri")?.clone()).ok()?;
        let version = params.get("version").and_then(Value::as_i64).map(|v| v as i32);
        let docs = self.docs.lock().unwrap();
        // Convert against the open rope if we have it; else an empty rope so at
        // least the line numbers survive.
        let rope = docs
            .get(&uri)
            .map(|s| s.rope.clone())
            .unwrap_or_default();
        drop(docs);

        let lsp_diags: Vec<lsp_types::Diagnostic> =
            serde_json::from_value(params.get("diagnostics")?.clone()).ok()?;
        let diagnostics = lsp_diags
            .iter()
            .map(|d| diagnostic_from_lsp(&rope, d, self.encoding))
            .collect();
        Some(DiagnosticsUpdate {
            uri,
            version,
            diagnostics,
        })
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Best-effort: ask it to exit, then make sure it's gone.
            let _ = self.conn.notify("exit", Value::Null);
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Parse the server's `textDocumentSync` capability, which may be a plain kind
/// number or an object carrying `.change`.
fn parse_sync_kind(value: Option<&Value>) -> SyncKind {
    let kind_num = match value {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::Object(obj)) => obj.get("change").and_then(Value::as_i64),
        _ => None,
    };
    match kind_num {
        Some(0) => SyncKind::None,
        Some(1) => SyncKind::Full,
        Some(2) => SyncKind::Incremental,
        // Default to full sync when unspecified: always correct, if chattier.
        _ => SyncKind::Full,
    }
}

// Re-export for convenience at the crate root.
pub use crate::convert::PositionEncoding as Encoding;
