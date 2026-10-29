//! `editor-workbench` — the integration layer that lets VS Code extensions
//! edit real editor-rs documents.
//!
//! It owns a shared set of open documents (editor-core [`Document`]s keyed by
//! URI) and an [`ExtensionHost`]. It:
//!
//! * mirrors documents into the extension host, so an extension's
//!   `TextDocument` APIs (`getText`, `lineAt`, …) answer synchronously;
//! * implements the host's [`HostHandler`] so `TextEditor.edit` /
//!   `workspace.applyEdit` apply to the **actual** editor-core buffer (with
//!   undo + the change stream), then resyncs the host mirror;
//! * delegates the non-document parts of the API (messages, quick pick,
//!   configuration, …) to a UI handler the embedder supplies.
//!
//! This is the seam the GPUI shell drives: open a document, make it active, let
//! the user type (which calls [`Workbench::apply_user_edit`]), and extensions
//! see and edit the same text.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use editor_core::interface::{Completion, CompletionKind};
use editor_core::Document;
use editor_ext::{EditOutcome, ExtensionHost, HostHandler, MessageLevel, RangeEdit};
use ropey::Rope;

/// One open document in the workbench.
pub struct DocState {
    pub document: Document,
    pub language_id: String,
    /// Editor-managed document version (monotonic), mirrored to the host.
    pub version: i64,
}

/// The shared document set, accessible from both the workbench API and the
/// host handler (which runs on the host's pump thread).
#[derive(Default)]
pub struct Shared {
    pub docs: HashMap<String, DocState>,
    pub active: Option<String>,
}

/// Convert a UTF-16 `(line, character)` position (VS Code's unit) to a char
/// offset in `rope`.
fn utf16_to_char(rope: &Rope, line: u32, character: u32) -> usize {
    let line = (line as usize).min(rope.len_lines().saturating_sub(1));
    let line_start = rope.line_to_char(line);
    let line_start_u16 = rope.char_to_utf16_cu(line_start);
    let target_u16 = (line_start_u16 + character as usize).min(rope.len_utf16_cu());
    rope.utf16_cu_to_char(target_u16)
}

/// Convert a char offset to a UTF-16 `(line, character)` position.
fn char_to_utf16(rope: &Rope, char_off: usize) -> (u32, u32) {
    let char_off = char_off.min(rope.len_chars());
    let line = rope.char_to_line(char_off);
    let line_start = rope.line_to_char(line);
    let col = rope.char_to_utf16_cu(char_off) - rope.char_to_utf16_cu(line_start);
    (line as u32, col as u32)
}

/// Map a VS Code `CompletionItemKind` number to our [`CompletionKind`].
fn map_completion_kind(n: Option<i64>) -> CompletionKind {
    match n {
        Some(1) => CompletionKind::Method,
        Some(2) => CompletionKind::Function,
        Some(3) => CompletionKind::Method, // Constructor
        Some(4) => CompletionKind::Field,
        Some(5) => CompletionKind::Variable,
        Some(6) => CompletionKind::Class,
        Some(8) => CompletionKind::Module,
        Some(13) => CompletionKind::Keyword,
        Some(14) => CompletionKind::Snippet,
        Some(0) => CompletionKind::Text,
        _ => CompletionKind::Other,
    }
}

/// The [`HostHandler`] the workbench installs: document edits hit the real
/// buffers; everything else is delegated to the embedder's UI handler.
struct WorkbenchHandler {
    shared: Arc<Mutex<Shared>>,
    ui: Arc<Mutex<dyn HostHandler>>,
}

impl HostHandler for WorkbenchHandler {
    fn apply_edits(&mut self, uri: &str, edits: &[RangeEdit]) -> Option<EditOutcome> {
        let mut shared = self.shared.lock().unwrap();
        let entry = shared.docs.get_mut(uri)?;

        // Convert every edit to a char range against the current text first,
        // then apply from the rightmost edit leftward so earlier offsets stay
        // valid (VS Code requires the edits in a batch to be non-overlapping).
        let rope = entry.document.buffer().rope().clone();
        let mut char_edits: Vec<(usize, usize, String)> = edits
            .iter()
            .map(|e| {
                let a = utf16_to_char(&rope, e.start_line, e.start_character);
                let b = utf16_to_char(&rope, e.end_line, e.end_character);
                (a.min(b), a.max(b), e.text.clone())
            })
            .collect();
        char_edits.sort_by_key(|e| std::cmp::Reverse(e.0));

        for (start, end, text) in char_edits {
            entry.document.edit(start..end, &text);
        }
        entry.version += 1;
        Some(EditOutcome {
            version: entry.version,
            text: entry.document.text(),
        })
    }

    // --- everything else is the embedder's concern ---
    fn show_message(&mut self, level: MessageLevel, message: &str, items: &[String]) -> Option<String> {
        self.ui.lock().unwrap().show_message(level, message, items)
    }
    fn quick_pick(&mut self, items: &[String]) -> Option<String> {
        self.ui.lock().unwrap().quick_pick(items)
    }
    fn input_box(&mut self, prompt: Option<&str>, value: Option<&str>) -> Option<String> {
        self.ui.lock().unwrap().input_box(prompt, value)
    }
    fn get_configuration(&mut self, section: &str) -> serde_json::Value {
        self.ui.lock().unwrap().get_configuration(section)
    }
    fn command_registered(&mut self, id: &str) {
        self.ui.lock().unwrap().command_registered(id)
    }
    fn execute_builtin(&mut self, command: &str, args: &[serde_json::Value]) -> serde_json::Value {
        self.ui.lock().unwrap().execute_builtin(command, args)
    }
    fn log(&mut self, message: &str) {
        self.ui.lock().unwrap().log(message)
    }
}

/// The editor session: documents + the extension host, wired together.
pub struct Workbench {
    shared: Arc<Mutex<Shared>>,
    host: ExtensionHost,
}

impl Workbench {
    /// Create a workbench, spawning the extension host. `ui` backs the
    /// non-document `vscode.*` API (messages, pickers, configuration…).
    pub fn new(ui: Arc<Mutex<dyn HostHandler>>) -> std::io::Result<Workbench> {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let handler: Arc<Mutex<dyn HostHandler>> = Arc::new(Mutex::new(WorkbenchHandler {
            shared: Arc::clone(&shared),
            ui,
        }));
        let host = ExtensionHost::spawn(handler)?;
        Ok(Workbench { shared, host })
    }

    /// The underlying extension host (for `activate`, `execute_command`, …).
    pub fn host(&self) -> &ExtensionHost {
        &self.host
    }

    /// Open a document and mirror it into the extension host.
    pub fn open_document(&self, uri: &str, language_id: &str, text: &str) -> editor_rpc::Result<()> {
        {
            let mut shared = self.shared.lock().unwrap();
            shared.docs.insert(
                uri.to_string(),
                DocState {
                    document: Document::from_str(text),
                    language_id: language_id.to_string(),
                    version: 1,
                },
            );
        }
        self.host.open_document(uri, language_id, 1, text)
    }

    /// Make `uri` the active editor the extension sees.
    pub fn set_active(&self, uri: &str) -> editor_rpc::Result<()> {
        self.shared.lock().unwrap().active = Some(uri.to_string());
        self.host.set_active_editor(Some(uri))
    }

    /// Apply an editor-side (user) edit to a document and resync the host.
    pub fn apply_user_edit(
        &self,
        uri: &str,
        range: std::ops::Range<usize>,
        text: &str,
    ) -> editor_rpc::Result<()> {
        let (version, full) = {
            let mut shared = self.shared.lock().unwrap();
            let Some(entry) = shared.docs.get_mut(uri) else {
                return Ok(());
            };
            entry.document.edit(range, text);
            entry.version += 1;
            (entry.version, entry.document.text())
        };
        self.host.change_document(uri, version, &full)
    }

    /// The current text of a document, if open.
    pub fn document_text(&self, uri: &str) -> Option<String> {
        self.shared
            .lock()
            .unwrap()
            .docs
            .get(uri)
            .map(|d| d.document.text())
    }

    /// Run a closure with shared read access (e.g. to read selections).
    pub fn with_shared<R>(&self, f: impl FnOnce(&Shared) -> R) -> R {
        f(&self.shared.lock().unwrap())
    }

    // ----- language features from extensions ---------------------------------

    /// UTF-16 `(line, character)` of a char offset in document `uri`.
    fn position_of(&self, uri: &str, char_offset: usize) -> Option<(u32, u32)> {
        let shared = self.shared.lock().unwrap();
        let entry = shared.docs.get(uri)?;
        Some(char_to_utf16(entry.document.buffer().rope(), char_offset))
    }

    /// Completions from extension providers at a char offset, mapped to the
    /// editor-core [`Completion`] model.
    pub fn completion_at(
        &self,
        uri: &str,
        char_offset: usize,
    ) -> editor_rpc::Result<Vec<Completion>> {
        let Some((line, character)) = self.position_of(uri, char_offset) else {
            return Ok(Vec::new());
        };
        let items = self.host.provide_completion(uri, line, character)?;
        Ok(items
            .iter()
            .map(|it| {
                let label = it.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let insert_text = it
                    .get("insertText")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&label)
                    .to_string();
                Completion {
                    label,
                    insert_text,
                    detail: it.get("detail").and_then(|v| v.as_str()).map(str::to_string),
                    kind: map_completion_kind(it.get("kind").and_then(|v| v.as_i64())),
                }
            })
            .collect())
    }

    /// Hover text from extension providers at a char offset (contents joined).
    pub fn hover_at(&self, uri: &str, char_offset: usize) -> editor_rpc::Result<Option<String>> {
        let Some((line, character)) = self.position_of(uri, char_offset) else {
            return Ok(None);
        };
        let hover = self.host.provide_hover(uri, line, character)?;
        Ok(hover.and_then(|h| {
            h.get("contents")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
        }))
    }

    /// Definition location(s) from extension providers at a char offset.
    pub fn definition_at(
        &self,
        uri: &str,
        char_offset: usize,
    ) -> editor_rpc::Result<Vec<serde_json::Value>> {
        let Some((line, character)) = self.position_of(uri, char_offset) else {
            return Ok(Vec::new());
        };
        self.host.provide_definition(uri, line, character)
    }
}
