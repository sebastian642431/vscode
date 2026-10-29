//! The workspace model: open folder, open documents, tabs, keymap, commands,
//! and the command dispatcher that turns a command id into an action.
//!
//! This is the front-end-agnostic brain of the editor. The GPUI shell owns a
//! `Workspace`, renders its state, translates key events into command ids via
//! the [`Keymap`], and calls [`Workspace::execute`]. Because none of that needs
//! a screen, the whole control flow is unit-tested here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use editor_core::interface::{DocumentId, DocumentSnapshot};
use editor_core::{Clipboard, Document, Granularity, MemoryClipboard, Movement};
use editor_fs::{FileTree, SearchOptions, Searcher};
use editor_syntax::Language;

use crate::command::CommandRegistry;
use crate::config::{default_commands, default_keymap};
use crate::keymap::Keymap;
use crate::tabs::{Tab, TabBar};

/// One open document plus its editor state.
pub struct OpenDocument {
    pub id: DocumentId,
    pub path: Option<PathBuf>,
    pub language: Option<Language>,
    pub document: Document,
    /// The document version last persisted to disk; used to compute "dirty".
    saved_version: u64,
}

impl OpenDocument {
    /// Whether the buffer has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.document.version() != self.saved_version
    }
}

/// The editor workspace.
pub struct Workspace {
    root: Option<PathBuf>,
    tree: Option<FileTree>,
    docs: HashMap<DocumentId, OpenDocument>,
    tabs: TabBar,
    next_id: u64,
    keymap: Keymap,
    commands: CommandRegistry,
    clipboard: Box<dyn Clipboard>,
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace::new()
    }
}

impl Workspace {
    /// A workspace with the default keymap, commands and an in-memory
    /// clipboard (the GPUI shell swaps in a system clipboard).
    pub fn new() -> Self {
        Workspace {
            root: None,
            tree: None,
            docs: HashMap::new(),
            tabs: TabBar::new(),
            next_id: 1,
            keymap: default_keymap(),
            commands: default_commands(),
            clipboard: Box::new(MemoryClipboard::new()),
        }
    }

    /// Replace the keymap (e.g. after merging user overrides).
    pub fn set_keymap(&mut self, keymap: Keymap) {
        self.keymap = keymap;
    }

    /// Install a front-end-provided clipboard (e.g. the OS clipboard).
    pub fn set_clipboard(&mut self, clipboard: Box<dyn Clipboard>) {
        self.clipboard = clipboard;
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub fn commands(&self) -> &CommandRegistry {
        &self.commands
    }

    // ----- folder / tree -----------------------------------------------------

    /// Open a folder as the workspace root.
    pub fn open_folder(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        self.tree = Some(FileTree::new(&path));
        self.root = Some(path);
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn tree(&self) -> Option<&FileTree> {
        self.tree.as_ref()
    }

    // ----- documents / tabs --------------------------------------------------

    pub fn tabs(&self) -> &TabBar {
        &self.tabs
    }

    fn alloc_id(&mut self) -> DocumentId {
        let id = DocumentId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Open a file from disk, or focus it if already open. The file's language
    /// is detected from its extension.
    pub fn open_file(&mut self, path: impl AsRef<Path>) -> std::io::Result<DocumentId> {
        let path = path.as_ref().to_path_buf();
        // Already open? Focus its tab.
        if let Some(existing) = self
            .docs
            .values()
            .find(|d| d.path.as_deref() == Some(path.as_path()))
            .map(|d| d.id)
        {
            self.tabs.activate(existing);
            return Ok(existing);
        }

        let text = std::fs::read_to_string(&path)?;
        let id = self.alloc_id();
        let language = Language::from_path(&path);
        let document = Document::from_str(&text);
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".to_string());

        self.docs.insert(
            id,
            OpenDocument {
                id,
                path: Some(path.clone()),
                language,
                document,
                saved_version: 0,
            },
        );
        self.tabs.open(Tab {
            id,
            title,
            path: Some(path),
            dirty: false,
        });
        Ok(id)
    }

    /// Create a new, empty, untitled document and focus it.
    pub fn new_untitled(&mut self) -> DocumentId {
        let id = self.alloc_id();
        self.docs.insert(
            id,
            OpenDocument {
                id,
                path: None,
                language: None,
                document: Document::new(),
                saved_version: 0,
            },
        );
        self.tabs.open(Tab {
            id,
            title: format!("Untitled-{}", id.0),
            path: None,
            dirty: false,
        });
        id
    }

    pub fn document(&self, id: DocumentId) -> Option<&OpenDocument> {
        self.docs.get(&id)
    }

    pub fn document_mut(&mut self, id: DocumentId) -> Option<&mut OpenDocument> {
        self.docs.get_mut(&id)
    }

    pub fn active_id(&self) -> Option<DocumentId> {
        self.tabs.active().map(|t| t.id)
    }

    pub fn active_document(&self) -> Option<&OpenDocument> {
        self.active_id().and_then(|id| self.docs.get(&id))
    }

    pub fn active_document_mut(&mut self) -> Option<&mut OpenDocument> {
        let id = self.active_id()?;
        self.docs.get_mut(&id)
    }

    /// Close a document and its tab.
    pub fn close(&mut self, id: DocumentId) {
        self.docs.remove(&id);
        self.tabs.close(id);
    }

    /// Persist a document to its path. Untitled documents return an error.
    pub fn save(&mut self, id: DocumentId) -> std::io::Result<()> {
        let doc = self
            .docs
            .get_mut(&id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such document"))?;
        let path = doc.path.clone().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "untitled document")
        })?;
        std::fs::write(&path, doc.document.text())?;
        doc.saved_version = doc.document.version();
        self.tabs.set_dirty(id, false);
        Ok(())
    }

    /// A snapshot of a document for a background worker (syntax/LSP).
    pub fn snapshot(&self, id: DocumentId) -> Option<DocumentSnapshot> {
        let doc = self.docs.get(&id)?;
        Some(
            doc.document
                .snapshot(id, doc.language.map(|l| l.id().to_string())),
        )
    }

    // ----- search ------------------------------------------------------------

    /// Build a project [`Searcher`] rooted at the workspace folder.
    pub fn searcher(&self, options: SearchOptions) -> Option<Searcher> {
        self.root.as_ref().map(|root| Searcher::new(root, options))
    }

    // ----- command dispatch --------------------------------------------------

    /// Execute a command id against the current workspace/active document.
    ///
    /// Returns `true` if the command was recognised (even UI-only commands,
    /// which the model treats as no-ops and the shell handles visually).
    pub fn execute(&mut self, command_id: &str) -> bool {
        // Editing/navigation commands act on the active document.
        let editor_handled = self.execute_editor_command(command_id);
        if editor_handled {
            if let Some(id) = self.active_id() {
                let dirty = self.docs.get(&id).map(|d| d.is_dirty()).unwrap_or(false);
                self.tabs.set_dirty(id, dirty);
            }
            return true;
        }

        match command_id {
            "workbench.action.files.save" => {
                if let Some(id) = self.active_id() {
                    let _ = self.save(id);
                }
                true
            }
            "workbench.action.closeActiveEditor" => {
                if let Some(id) = self.active_id() {
                    self.close(id);
                }
                true
            }
            "workbench.action.nextEditor" => {
                self.tabs.next();
                true
            }
            "workbench.action.previousEditor" => {
                self.tabs.prev();
                true
            }
            // Recognised UI-only commands handled by the shell's views.
            "workbench.action.showCommands"
            | "workbench.action.quickOpen"
            | "workbench.action.findInFiles"
            | "actions.find"
            | "workbench.action.toggleSidebar"
            | "workbench.action.openGlobalKeybindings"
            | "workbench.action.closeQuickOpen" => true,
            _ => false,
        }
    }

    fn execute_editor_command(&mut self, command_id: &str) -> bool {
        // Commands needing the clipboard are split out because they borrow it
        // alongside the document.
        match command_id {
            "editor.action.clipboardCopyAction" => {
                if let Some(doc) = active_doc_mut(&mut self.docs, self.tabs.active().map(|t| t.id)) {
                    doc.document.copy(self.clipboard.as_mut());
                }
                return true;
            }
            "editor.action.clipboardCutAction" => {
                if let Some(doc) = active_doc_mut(&mut self.docs, self.tabs.active().map(|t| t.id)) {
                    doc.document.cut(self.clipboard.as_mut());
                }
                return true;
            }
            "editor.action.clipboardPasteAction" => {
                if let Some(doc) = active_doc_mut(&mut self.docs, self.tabs.active().map(|t| t.id)) {
                    doc.document.paste(self.clipboard.as_ref());
                }
                return true;
            }
            _ => {}
        }

        let Some(doc) = active_doc_mut(&mut self.docs, self.tabs.active().map(|t| t.id)) else {
            return false;
        };
        let d = &mut doc.document;
        match command_id {
            "undo" => {
                d.undo();
            }
            "redo" => {
                d.redo();
            }
            "editor.action.selectAll" => d.select_all(),
            "cursorWordLeft" => d.move_cursors(Movement::WordLeft, false),
            "cursorWordRight" => d.move_cursors(Movement::WordRight, false),
            "cursorWordLeftSelect" => d.move_cursors(Movement::WordLeft, true),
            "cursorWordRightSelect" => d.move_cursors(Movement::WordRight, true),
            "cursorHome" => d.move_cursors(Movement::LineFirstNonWhitespace, false),
            "cursorEnd" => d.move_cursors(Movement::LineEnd, false),
            "cursorTop" => d.move_cursors(Movement::BufferStart, false),
            "cursorBottom" => d.move_cursors(Movement::BufferEnd, false),
            "deleteWordLeft" => d.delete(Granularity::Word, false),
            "deleteWordRight" => d.delete(Granularity::Word, true),
            "editor.action.insertCursorBelow" => d.add_cursor_below(),
            "editor.action.insertCursorAbove" => d.add_cursor_above(),
            _ => return false,
        }
        true
    }
}

/// Helper to borrow the active document mutably without borrowing all of
/// `self` (keeps the clipboard borrow separate).
fn active_doc_mut(
    docs: &mut HashMap<DocumentId, OpenDocument>,
    active: Option<DocumentId>,
) -> Option<&mut OpenDocument> {
    active.and_then(move |id| docs.get_mut(&id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn open_folder_and_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        let mut ws = Workspace::new();
        ws.open_folder(dir.path());
        assert!(ws.root().is_some());

        let id = ws.open_file(dir.path().join("main.rs")).unwrap();
        assert_eq!(ws.active_id(), Some(id));
        assert_eq!(ws.document(id).unwrap().language, Some(Language::Rust));
        assert_eq!(ws.tabs().len(), 1);
    }

    #[test]
    fn opening_same_file_twice_focuses_one_tab() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "x").unwrap();
        let mut ws = Workspace::new();
        let id1 = ws.open_file(dir.path().join("a.rs")).unwrap();
        let id2 = ws.open_file(dir.path().join("a.rs")).unwrap();
        assert_eq!(id1, id2);
        assert_eq!(ws.tabs().len(), 1);
    }

    #[test]
    fn dirty_flag_tracks_edits_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "hello").unwrap();
        let mut ws = Workspace::new();
        let id = ws.open_file(&path).unwrap();
        assert!(!ws.document(id).unwrap().is_dirty());

        let d = &mut ws.document_mut(id).unwrap().document;
        d.move_cursors(Movement::BufferEnd, false);
        d.insert("!");
        assert!(ws.document(id).unwrap().is_dirty());

        ws.save(id).unwrap();
        assert!(!ws.document(id).unwrap().is_dirty());
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello!");
    }

    #[test]
    fn command_dispatch_edits_active_document() {
        let mut ws = Workspace::new();
        let id = ws.new_untitled();
        ws.document_mut(id).unwrap().document.insert("abc");

        assert!(ws.execute("editor.action.selectAll"));
        assert!(ws.execute("editor.action.clipboardCopyAction"));
        ws.document_mut(id).unwrap().document.move_cursors(Movement::BufferEnd, false);
        assert!(ws.execute("editor.action.clipboardPasteAction"));
        assert_eq!(ws.document(id).unwrap().document.text(), "abcabc");

        assert!(ws.execute("undo"));
        assert_eq!(ws.document(id).unwrap().document.text(), "abc");
    }

    #[test]
    fn unknown_command_is_not_handled() {
        let mut ws = Workspace::new();
        ws.new_untitled();
        assert!(!ws.execute("totally.unknown.command"));
    }

    #[test]
    fn ui_only_commands_are_recognised() {
        let mut ws = Workspace::new();
        assert!(ws.execute("workbench.action.showCommands"));
        assert!(ws.execute("workbench.action.toggleSidebar"));
    }

    #[test]
    fn close_editor_command() {
        let mut ws = Workspace::new();
        let id = ws.new_untitled();
        assert!(ws.execute("workbench.action.closeActiveEditor"));
        assert!(ws.document(id).is_none());
        assert!(ws.tabs().is_empty());
    }

    #[test]
    fn searcher_requires_a_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f.txt"), "find me\n").unwrap();
        let mut ws = Workspace::new();
        assert!(ws.searcher(SearchOptions::literal("find")).is_none());
        ws.open_folder(dir.path());
        let matches = ws
            .searcher(SearchOptions::literal("find"))
            .unwrap()
            .collect()
            .unwrap();
        assert_eq!(matches.len(), 1);
    }
}
