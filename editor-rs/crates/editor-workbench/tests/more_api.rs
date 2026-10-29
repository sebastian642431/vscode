//! Broader vscode API surface: configuration, document formatting, rename
//! (WorkspaceEdit) and code actions — each applied to the real buffer.
//! Requires `node`; skips otherwise.

use std::process::Command;
use std::sync::{Arc, Mutex};

use editor_ext::{ExtensionManifest, HostHandler, NullHandler};
use editor_workbench::Workbench;
use serde_json::json;

fn node_available() -> bool {
    let node = std::env::var("EDITOR_RS_NODE").unwrap_or_else(|_| "node".to_string());
    Command::new(node)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

const EXT: &str = r#"
const vscode = require('vscode');
exports.activate = () => {
    vscode.commands.registerCommand('demo.cfg', () => {
        const c = vscode.workspace.getConfiguration('myext');
        return { tabSize: c.get('tabSize', 2), name: c.get('name', 'none'), has: c.has('tabSize') };
    });
    vscode.languages.registerDocumentFormattingEditProvider('rust', {
        provideDocumentFormattingEdits(document) {
            const last = document.lineCount - 1;
            const end = new vscode.Position(last, document.lineAt(last).text.length);
            const whole = new vscode.Range(new vscode.Position(0, 0), end);
            const trimmed = document.getText().split('\n').map((l) => l.replace(/\s+$/, '')).join('\n');
            return [vscode.TextEdit.replace(whole, trimmed)];
        },
    });
    vscode.languages.registerRenameProvider('rust', {
        provideRenameEdits(document, position, newName) {
            const we = new vscode.WorkspaceEdit();
            const range = document.getWordRangeAtPosition(position);
            we.replace(document.uri, range, newName);
            return we;
        },
    });
    vscode.languages.registerCodeActionsProvider('rust', {
        provideCodeActions(document, range) {
            const a = new vscode.CodeAction('Wrap in parens', vscode.CodeActionKind.RefactorRewrite);
            const we = new vscode.WorkspaceEdit();
            we.replace(document.uri, range, '(' + document.getText(range) + ')');
            a.edit = we;
            return [a];
        },
    });
};
"#;

const URI: &str = "file:///project/a.rs";

/// Spawn a workbench, open `text`, activate the multi-feature extension.
fn setup(text: &str, config: Option<serde_json::Value>) -> Option<Workbench> {
    if !node_available() {
        eprintln!("skipping: `node` not found");
        return None;
    }
    let ui: Arc<Mutex<dyn HostHandler>> = Arc::new(Mutex::new(NullHandler));
    let wb = Workbench::new(ui).expect("spawn");
    if let Some(cfg) = config {
        wb.set_configuration(cfg).unwrap();
    }
    wb.open_document(URI, "rust", text).unwrap();
    wb.set_active(URI).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let pkg = r#"{ "name":"api", "publisher":"editor-rs", "main":"./extension.js" }"#;
    std::fs::write(dir.path().join("package.json"), pkg).unwrap();
    std::fs::write(dir.path().join("extension.js"), EXT).unwrap();
    let manifest = ExtensionManifest::from_json(pkg).unwrap();
    wb.host()
        .activate_parts(dir.path().to_str().unwrap(), &manifest)
        .unwrap();
    std::mem::forget(dir);
    Some(wb)
}

#[test]
fn configuration_is_readable_by_extension() {
    let cfg = json!({ "myext": { "tabSize": 8, "name": "hi" } });
    let Some(wb) = setup("fn main() {}\n", Some(cfg)) else { return };
    let got = wb.host().execute_command("demo.cfg", vec![]).unwrap();
    assert_eq!(got["tabSize"], 8);
    assert_eq!(got["name"], "hi");
    assert_eq!(got["has"], true);
}

#[test]
fn configuration_defaults_when_unset() {
    let Some(wb) = setup("fn main() {}\n", None) else { return };
    let got = wb.host().execute_command("demo.cfg", vec![]).unwrap();
    assert_eq!(got["tabSize"], 2); // the default passed to .get
    assert_eq!(got["name"], "none");
    assert_eq!(got["has"], false);
}

#[test]
fn document_formatting_applies_to_buffer() {
    let Some(wb) = setup("let x = 1;   \nlet y = 2;  \n", None) else { return };
    let changed = wb.format_document(URI).unwrap();
    assert!(changed);
    assert_eq!(wb.document_text(URI).unwrap(), "let x = 1;\nlet y = 2;\n");
}

#[test]
fn rename_applies_workspace_edit() {
    let Some(wb) = setup("foo + bar;\n", None) else { return };
    // Rename the word at offset 0 ("foo") to "renamed".
    let did = wb.rename_at(URI, 0, "renamed").unwrap();
    assert!(did);
    assert_eq!(wb.document_text(URI).unwrap(), "renamed + bar;\n");
}

#[test]
fn code_action_edit_applies() {
    let Some(wb) = setup("x\n", None) else { return };
    // Range covering "x" (offsets 0..1).
    let actions = wb.code_actions_at(URI, 0, 1).unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0]["title"], "Wrap in parens");
    let applied = wb.apply_code_action(&actions[0]).unwrap();
    assert!(applied);
    assert_eq!(wb.document_text(URI).unwrap(), "(x)\n");
}
