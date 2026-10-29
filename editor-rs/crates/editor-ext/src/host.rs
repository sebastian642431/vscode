//! The Rust side of the extension host: spawn Node, speak JSON-RPC, and
//! service the `vscode.*` API calls the extension makes.
//!
//! ```text
//!   editor-rs (Rust)                     Node.js (host.js + vscode shim)
//!   ─────────────────                    ───────────────────────────────
//!   activate(ext)         ── request ─▶  require(ext.main); ext.activate(ctx)
//!                                        ctx.subscriptions, registerCommand…
//!   HostHandler.show_msg  ◀─ request ──  vscode.window.showInformationMessage
//!   executeCommand(id)    ── request ─▶  invoke the registered callback
//! ```
//!
//! The editor implements [`HostHandler`] to back the API; a request from the
//! extension (e.g. showing a message) is dispatched to it on the pump thread
//! and the result is returned to Node, so the extension's `await` resolves.

use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use editor_rpc::{Connection, Incoming};
use serde_json::{json, Value};

use crate::manifest::ExtensionManifest;
use crate::vsix::InstalledExtension;

/// The embedded Node host bootstrap + `vscode` shim.
const HOST_JS: &str = include_str!("../js/host.js");

static HOST_SEQ: AtomicU64 = AtomicU64::new(0);

/// Severity of a `window.show*Message` call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageLevel {
    Info,
    Warning,
    Error,
}

/// One edit from `TextEditor.edit` / `WorkspaceEdit`, in LSP-style
/// `(line, character)` positions. `character` is a UTF-16 code-unit offset (how
/// JS, and thus the Node mirror, measures columns); the editor converts it to a
/// char offset against the document's rope when applying.
#[derive(Clone, Debug)]
pub struct RangeEdit {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
    pub text: String,
}

/// What the editor must provide so the extension's `vscode.*` calls do
/// something. Every method has a no-op default, so a minimal editor only
/// implements what it supports. Called on the host's pump thread.
pub trait HostHandler: Send {
    /// `window.showInformationMessage` / `showWarningMessage` /
    /// `showErrorMessage`. Return the chosen item, or `None`.
    fn show_message(
        &mut self,
        level: MessageLevel,
        message: &str,
        items: &[String],
    ) -> Option<String> {
        let _ = (level, message, items);
        None
    }

    /// `window.showQuickPick`. Return the chosen item, or `None`.
    fn quick_pick(&mut self, items: &[String]) -> Option<String> {
        let _ = items;
        None
    }

    /// `window.showInputBox`. Return the entered text, or `None`.
    fn input_box(&mut self, prompt: Option<&str>, value: Option<&str>) -> Option<String> {
        let _ = (prompt, value);
        None
    }

    /// `workspace.getConfiguration(section)`. Return the settings object.
    fn get_configuration(&mut self, section: &str) -> Value {
        let _ = section;
        json!({})
    }

    /// Notified when the extension calls `commands.registerCommand(id, …)`.
    fn command_registered(&mut self, id: &str) {
        let _ = id;
    }

    /// `commands.executeCommand(id, …args)` for a command the extension host
    /// doesn't own (a built-in). Return its result.
    fn execute_builtin(&mut self, command: &str, args: &[Value]) -> Value {
        let _ = (command, args);
        Value::Null
    }

    /// A `console.log`/`OutputChannel` line from the extension.
    fn log(&mut self, message: &str) {
        let _ = message;
    }

    /// An extension published diagnostics for `uri` under `owner` (its
    /// `DiagnosticCollection` name). An empty slice clears them.
    fn publish_diagnostics(&mut self, owner: &str, uri: &str, diagnostics: &[DiagnosticInfo]) {
        let _ = (owner, uri, diagnostics);
    }

    /// An extension changed a setting via `config.update(key, value)`.
    fn configuration_updated(&mut self, key: &str, value: &serde_json::Value) {
        let _ = (key, value);
    }

    /// `TextEditor.edit` / `workspace.applyEdit`: apply `edits` to the document
    /// identified by `uri` and return the edited document's new full text and
    /// version so the Node mirror can resync. Return `None` if the document is
    /// unknown or the edit could not be applied.
    fn apply_edits(&mut self, uri: &str, edits: &[RangeEdit]) -> Option<EditOutcome> {
        let _ = (uri, edits);
        None
    }

    /// `workspace.applyEdit`: apply edits to several documents at once. Return
    /// the new text/version of each document that changed.
    fn apply_workspace_edits(
        &mut self,
        edits: &[(String, Vec<RangeEdit>)],
    ) -> Vec<(String, EditOutcome)> {
        edits
            .iter()
            .filter_map(|(uri, es)| self.apply_edits(uri, es).map(|o| (uri.clone(), o)))
            .collect()
    }
}

/// The result of applying edits to a document.
#[derive(Clone, Debug)]
pub struct EditOutcome {
    pub version: i64,
    pub text: String,
}

/// A diagnostic an extension published via a `DiagnosticCollection`, in
/// LSP-style `(line, character)` (UTF-16) positions.
#[derive(Clone, Debug)]
pub struct DiagnosticInfo {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
    pub message: String,
    /// 0=Error, 1=Warning, 2=Information, 3=Hint (VS Code's numbering).
    pub severity: u8,
    pub source: Option<String>,
}

/// A no-op handler (useful as a base or for headless runs).
#[derive(Default)]
pub struct NullHandler;
impl HostHandler for NullHandler {}

/// A running Node.js extension host.
pub struct ExtensionHost {
    conn: Arc<Connection>,
    child: Option<Child>,
    host_js_path: PathBuf,
    timeout: Duration,
    _pump: JoinHandle<()>,
}

impl ExtensionHost {
    /// Spawn the host, using the `node` binary on `PATH` (override with the
    /// `EDITOR_RS_NODE` environment variable). `handler` backs the `vscode.*`
    /// API.
    pub fn spawn(handler: Arc<Mutex<dyn HostHandler>>) -> io::Result<ExtensionHost> {
        let node = std::env::var("EDITOR_RS_NODE").unwrap_or_else(|_| "node".to_string());
        Self::spawn_with_node(&node, handler)
    }

    /// Spawn the host using a specific `node` binary.
    pub fn spawn_with_node(
        node: &str,
        handler: Arc<Mutex<dyn HostHandler>>,
    ) -> io::Result<ExtensionHost> {
        // Materialise the bootstrap script next to the system temp dir.
        let seq = HOST_SEQ.fetch_add(1, Ordering::Relaxed);
        let host_js_path = std::env::temp_dir()
            .join(format!("editor-rs-exthost-{}-{}.js", std::process::id(), seq));
        std::fs::write(&host_js_path, HOST_JS)?;

        let mut child = Command::new(node)
            .arg(&host_js_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        let conn = Arc::new(Connection::new(stdout, stdin));
        let pump = spawn_pump(Arc::clone(&conn), handler);

        Ok(ExtensionHost {
            conn,
            child: Some(child),
            host_js_path,
            timeout: Duration::from_secs(20),
            _pump: pump,
        })
    }

    /// Load and activate an installed extension. Returns the host's activation
    /// report (e.g. the list of commands the extension registered).
    pub fn activate(&self, ext: &InstalledExtension) -> editor_rpc::Result<Value> {
        self.activate_parts(&ext.root.to_string_lossy(), &ext.manifest)
    }

    /// Activate from an explicit root path + manifest (used by tests).
    pub fn activate_parts(
        &self,
        extension_path: &str,
        manifest: &ExtensionManifest,
    ) -> editor_rpc::Result<Value> {
        self.conn.request(
            "activate",
            json!({
                "extensionPath": extension_path,
                "main": manifest.main,
                "id": manifest.id(),
            }),
            self.timeout,
        )
    }

    /// Run a command the extension registered, returning its result.
    pub fn execute_command(&self, command: &str, args: Vec<Value>) -> editor_rpc::Result<Value> {
        self.conn.request(
            "executeCommand",
            json!({ "command": command, "args": args }),
            self.timeout,
        )
    }

    /// Ask the extension to deactivate (calls its `deactivate` export).
    pub fn deactivate(&self) -> editor_rpc::Result<()> {
        self.conn.notify("deactivate", Value::Null)
    }

    // ----- document sync (editor -> extension host) --------------------------

    /// Tell the host a document opened; it mirrors the text so the extension's
    /// `TextDocument` APIs (`getText`, `lineAt`, …) answer synchronously.
    pub fn open_document(
        &self,
        uri: &str,
        language_id: &str,
        version: i64,
        text: &str,
    ) -> editor_rpc::Result<()> {
        self.conn.notify(
            "document.open",
            json!({ "uri": uri, "languageId": language_id, "version": version, "text": text }),
        )
    }

    /// Tell the host a document's full text changed (keeps the mirror in sync
    /// with editor-side edits such as the user typing).
    pub fn change_document(&self, uri: &str, version: i64, text: &str) -> editor_rpc::Result<()> {
        self.conn.notify(
            "document.change",
            json!({ "uri": uri, "version": version, "text": text }),
        )
    }

    pub fn close_document(&self, uri: &str) -> editor_rpc::Result<()> {
        self.conn
            .notify("document.close", json!({ "uri": uri }))
    }

    /// Set (or clear, with `None`) the active editor the extension sees via
    /// `window.activeTextEditor`.
    pub fn set_active_editor(&self, uri: Option<&str>) -> editor_rpc::Result<()> {
        self.conn
            .notify("editor.setActive", json!({ "uri": uri }))
    }

    /// Push the merged workspace settings to the host so extensions'
    /// `workspace.getConfiguration(...).get(key)` answers synchronously.
    pub fn update_configuration(&self, settings: Value) -> editor_rpc::Result<()> {
        self.conn
            .notify("configuration.update", json!({ "settings": settings }))
    }

    // ----- language features provided by extensions --------------------------

    /// Ask the extension's registered completion providers for items at a
    /// `(line, character)` position (UTF-16 column). Returns normalised items.
    pub fn provide_completion(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> editor_rpc::Result<Vec<Value>> {
        let result = self.conn.request(
            "provider.completion",
            json!({ "uri": uri, "line": line, "character": character }),
            self.timeout,
        )?;
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Ask the extension's hover providers for hover info at a position.
    pub fn provide_hover(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> editor_rpc::Result<Option<Value>> {
        let result = self.conn.request(
            "provider.hover",
            json!({ "uri": uri, "line": line, "character": character }),
            self.timeout,
        )?;
        Ok(if result.is_null() { None } else { Some(result) })
    }

    /// Ask the extension's definition providers for definition location(s).
    pub fn provide_definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> editor_rpc::Result<Vec<Value>> {
        let result = self.conn.request(
            "provider.definition",
            json!({ "uri": uri, "line": line, "character": character }),
            self.timeout,
        )?;
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Ask the extension's document-formatting providers for edits (as
    /// `{startLine, startCharacter, endLine, endCharacter, text}` objects).
    pub fn provide_formatting(&self, uri: &str) -> editor_rpc::Result<Vec<Value>> {
        let result = self.conn.request(
            "provider.formatting",
            json!({ "uri": uri, "options": { "tabSize": 4, "insertSpaces": true } }),
            self.timeout,
        )?;
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Ask the extension's rename providers for the WorkspaceEdit to apply.
    /// Returns the `{ edits: { uri: [rangeEdit] } }` object, or `None`.
    pub fn provide_rename(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> editor_rpc::Result<Option<Value>> {
        let result = self.conn.request(
            "provider.rename",
            json!({ "uri": uri, "line": line, "character": character, "newName": new_name }),
            self.timeout,
        )?;
        Ok(if result.is_null() { None } else { Some(result) })
    }

    /// Ask the extension's code-action providers for actions over a range.
    pub fn provide_code_actions(
        &self,
        uri: &str,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) -> editor_rpc::Result<Vec<Value>> {
        let result = self.conn.request(
            "provider.codeAction",
            json!({
                "uri": uri,
                "startLine": start_line, "startCharacter": start_character,
                "endLine": end_line, "endCharacter": end_character,
            }),
            self.timeout,
        )?;
        Ok(result.as_array().cloned().unwrap_or_default())
    }
}

impl Drop for ExtensionHost {
    fn drop(&mut self) {
        let _ = self.conn.notify("shutdown", Value::Null);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.host_js_path);
    }
}

/// Spawn the thread that services `vscode.*` calls coming from Node.
fn spawn_pump(conn: Arc<Connection>, handler: Arc<Mutex<dyn HostHandler>>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        while let Ok(msg) = conn.incoming().recv() {
            match msg {
                Incoming::Request { id, method, params } => {
                    let result = dispatch_request(&handler, &method, &params);
                    let _ = conn.respond(id, result);
                }
                Incoming::Notification { method, params } => {
                    dispatch_notification(&handler, &method, &params);
                }
            }
        }
    })
}

fn dispatch_request(
    handler: &Arc<Mutex<dyn HostHandler>>,
    method: &str,
    params: &Value,
) -> Value {
    let mut h = handler.lock().unwrap();
    match method {
        "window.showMessage" => {
            let level = match params.get("level").and_then(Value::as_str) {
                Some("warning") => MessageLevel::Warning,
                Some("error") => MessageLevel::Error,
                _ => MessageLevel::Info,
            };
            let message = params.get("message").and_then(Value::as_str).unwrap_or("");
            let items = string_array(params.get("items"));
            match h.show_message(level, message, &items) {
                Some(choice) => Value::String(choice),
                None => Value::Null,
            }
        }
        "window.showQuickPick" => {
            let items = string_array(params.get("items"));
            match h.quick_pick(&items) {
                Some(choice) => Value::String(choice),
                None => Value::Null,
            }
        }
        "window.showInputBox" => {
            let prompt = params.get("prompt").and_then(Value::as_str);
            let value = params.get("value").and_then(Value::as_str);
            match h.input_box(prompt, value) {
                Some(text) => Value::String(text),
                None => Value::Null,
            }
        }
        "workspace.getConfiguration" => {
            let section = params.get("section").and_then(Value::as_str).unwrap_or("");
            h.get_configuration(section)
        }
        "commands.executeCommand" => {
            let command = params.get("command").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("args")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            h.execute_builtin(command, &args)
        }
        "editor.applyEdits" => {
            let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
            let edits = parse_edits(params.get("edits"));
            match h.apply_edits(uri, &edits) {
                Some(outcome) => json!({
                    "applied": true,
                    "version": outcome.version,
                    "text": outcome.text,
                }),
                None => json!({ "applied": false }),
            }
        }
        "workspace.applyEdit" => {
            // params.edits is { uri: [rangeEdit, ...], ... }
            let mut per_doc: Vec<(String, Vec<RangeEdit>)> = Vec::new();
            if let Some(obj) = params.get("edits").and_then(Value::as_object) {
                for (uri, list) in obj {
                    per_doc.push((uri.clone(), parse_edits(Some(list))));
                }
            }
            let outcomes = h.apply_workspace_edits(&per_doc);
            let mut documents = serde_json::Map::new();
            for (uri, outcome) in &outcomes {
                documents.insert(
                    uri.clone(),
                    json!({ "version": outcome.version, "text": outcome.text }),
                );
            }
            json!({ "applied": !outcomes.is_empty(), "documents": documents })
        }
        _ => Value::Null,
    }
}

fn parse_edits(value: Option<&Value>) -> Vec<RangeEdit> {
    value
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|e| RangeEdit {
                    start_line: u32_at(e, "startLine"),
                    start_character: u32_at(e, "startCharacter"),
                    end_line: u32_at(e, "endLine"),
                    end_character: u32_at(e, "endCharacter"),
                    text: e.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn u32_at(v: &Value, key: &str) -> u32 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0) as u32
}

fn dispatch_notification(handler: &Arc<Mutex<dyn HostHandler>>, method: &str, params: &Value) {
    let mut h = handler.lock().unwrap();
    match method {
        "command.registered" => {
            if let Some(id) = params.get("id").and_then(Value::as_str) {
                h.command_registered(id);
            }
        }
        "log" => {
            if let Some(msg) = params.get("message").and_then(Value::as_str) {
                h.log(msg);
            }
        }
        "diagnostics.publish" => {
            let owner = params.get("owner").and_then(Value::as_str).unwrap_or("");
            let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
            let diags = parse_diagnostics(params.get("diagnostics"));
            h.publish_diagnostics(owner, uri, &diags);
        }
        "configuration.didUpdate" => {
            let key = params.get("key").and_then(Value::as_str).unwrap_or("");
            let value = params.get("value").cloned().unwrap_or(Value::Null);
            h.configuration_updated(key, &value);
        }
        _ => {}
    }
}

fn parse_diagnostics(value: Option<&Value>) -> Vec<DiagnosticInfo> {
    value
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|d| DiagnosticInfo {
                    start_line: u32_at(d, "startLine"),
                    start_character: u32_at(d, "startCharacter"),
                    end_line: u32_at(d, "endLine"),
                    end_character: u32_at(d, "endCharacter"),
                    message: d.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
                    severity: d.get("severity").and_then(Value::as_u64).unwrap_or(0) as u8,
                    source: d.get("source").and_then(Value::as_str).map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
