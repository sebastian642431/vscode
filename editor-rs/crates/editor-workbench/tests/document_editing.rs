//! Proves the headline capability: a real VS Code extension reads and edits an
//! editor-core document through the `vscode` API, and the edits land in the
//! actual Rust buffer.
//!
//! Requires `node` on PATH (override with `EDITOR_RS_NODE`); skips otherwise.

use std::process::Command;
use std::sync::{Arc, Mutex};

use editor_ext::{ExtensionManifest, HostHandler, NullHandler};
use editor_workbench::Workbench;

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
exports.activate = (context) => {
    vscode.commands.registerCommand('demo.readText', () => {
        const ed = vscode.window.activeTextEditor;
        return ed ? ed.document.getText() : null;
    });
    vscode.commands.registerCommand('demo.lineInfo', () => {
        const doc = vscode.window.activeTextEditor.document;
        return { lineCount: doc.lineCount, line0: doc.lineAt(0).text, lang: doc.languageId };
    });
    vscode.commands.registerCommand('demo.rename', async () => {
        const ed = vscode.window.activeTextEditor;
        const ok = await ed.edit((b) => {
            b.replace(new vscode.Range(new vscode.Position(0, 4), new vscode.Position(0, 5)), 'count');
        });
        return { ok: ok, text: ed.document.getText() };
    });
};
"#;

fn write_ext(dir: &std::path::Path) -> ExtensionManifest {
    let pkg = r#"{ "name":"demo", "publisher":"editor-rs", "main":"./extension.js" }"#;
    std::fs::write(dir.join("package.json"), pkg).unwrap();
    std::fs::write(dir.join("extension.js"), EXT).unwrap();
    ExtensionManifest::from_json(pkg).unwrap()
}

const URI: &str = "file:///project/a.rs";
const TEXT: &str = "let x = 1;\nlet y = 2;\n";

fn setup() -> Option<(Workbench, tempfile::TempDir, ExtensionManifest)> {
    if !node_available() {
        eprintln!("skipping: `node` not found");
        return None;
    }
    let ui: Arc<Mutex<dyn HostHandler>> = Arc::new(Mutex::new(NullHandler));
    let wb = Workbench::new(ui).expect("spawn workbench");
    wb.open_document(URI, "rust", TEXT).unwrap();
    wb.set_active(URI).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let manifest = write_ext(dir.path());
    wb.host()
        .activate_parts(dir.path().to_str().unwrap(), &manifest)
        .expect("activate");
    Some((wb, dir, manifest))
}

#[test]
fn extension_reads_active_document_text() {
    let Some((wb, _dir, _m)) = setup() else { return };
    let text = wb.host().execute_command("demo.readText", vec![]).unwrap();
    assert_eq!(text, TEXT);
}

#[test]
fn extension_sees_document_structure() {
    let Some((wb, _dir, _m)) = setup() else { return };
    let info = wb.host().execute_command("demo.lineInfo", vec![]).unwrap();
    assert_eq!(info["lineCount"], 3); // two lines + trailing empty
    assert_eq!(info["line0"], "let x = 1;");
    assert_eq!(info["lang"], "rust");
}

#[test]
fn extension_edit_lands_in_the_real_buffer() {
    let Some((wb, _dir, _m)) = setup() else { return };
    let result = wb.host().execute_command("demo.rename", vec![]).unwrap();
    assert_eq!(result["ok"], true);
    // The extension's own view is updated...
    assert_eq!(result["text"], "let count = 1;\nlet y = 2;\n");
    // ...and crucially, so is the actual editor-core buffer on the Rust side.
    assert_eq!(
        wb.document_text(URI).unwrap(),
        "let count = 1;\nlet y = 2;\n"
    );
}

#[test]
fn user_edits_resync_to_the_extension() {
    let Some((wb, _dir, _m)) = setup() else { return };
    // An editor-side edit: prepend a comment line (char range 0..0).
    wb.apply_user_edit(URI, 0..0, "// header\n").unwrap();
    assert!(wb.document_text(URI).unwrap().starts_with("// header\n"));
    // The extension now observes the updated text through its mirror.
    let seen = wb.host().execute_command("demo.readText", vec![]).unwrap();
    assert_eq!(seen.as_str().unwrap(), wb.document_text(URI).unwrap());
    assert!(seen.as_str().unwrap().starts_with("// header\n"));
}

/// A non-ASCII edit to make sure UTF-16 (extension) ↔ char (buffer) conversion
/// is correct, not just ASCII-lucky. Uses its own workbench so `require` loads
/// this extension cleanly.
#[test]
fn unicode_positions_convert_correctly() {
    if !node_available() {
        eprintln!("skipping: `node` not found");
        return;
    }
    let ui: Arc<Mutex<dyn HostHandler>> = Arc::new(Mutex::new(NullHandler));
    let wb = Workbench::new(ui).expect("spawn workbench");
    let uri = "file:///project/u.txt";
    // 'a' scalar 0, 'é' scalar 1, '𝄞' scalar 2 (2 UTF-16 units: cols 2..4), 'b' scalar 3.
    wb.open_document(uri, "plaintext", "aé𝄞b\n").unwrap();
    wb.set_active(uri).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let ext = r#"
const vscode = require('vscode');
exports.activate = () => {
    vscode.commands.registerCommand('demo.u', async () => {
        const ed = vscode.window.activeTextEditor;
        // Replace '𝄞' (UTF-16 columns 2..4) with "X".
        await ed.edit((b) => b.replace(new vscode.Range(new vscode.Position(0,2), new vscode.Position(0,4)), 'X'));
        return ed.document.getText();
    });
};
"#;
    let pkg = r#"{ "name":"demou", "publisher":"editor-rs", "main":"./extension.js" }"#;
    std::fs::write(dir.path().join("package.json"), pkg).unwrap();
    std::fs::write(dir.path().join("extension.js"), ext).unwrap();
    let manifest = ExtensionManifest::from_json(pkg).unwrap();
    wb.host()
        .activate_parts(dir.path().to_str().unwrap(), &manifest)
        .unwrap();

    let text = wb.host().execute_command("demo.u", vec![]).unwrap();
    // The multi-byte scalar was replaced correctly on both sides.
    assert_eq!(text, "aéXb\n");
    assert_eq!(wb.document_text(uri).unwrap(), "aéXb\n");
}
