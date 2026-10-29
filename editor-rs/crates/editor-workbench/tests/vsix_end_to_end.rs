//! Capstone: take a marketplace-format `.vsix`, install it, activate it through
//! the workbench, and run a command it contributes that edits a document — the
//! whole real-world path in one test. Requires `node`; skips otherwise.

use std::io::Write;
use std::process::Command;
use std::sync::{Arc, Mutex};

use editor_ext::{vsix, HostHandler, NullHandler};
use editor_workbench::Workbench;
use zip::write::SimpleFileOptions;

fn node_available() -> bool {
    let node = std::env::var("EDITOR_RS_NODE").unwrap_or_else(|_| "node".to_string());
    Command::new(node)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

const PKG: &str = r#"{
    "name": "upcase",
    "publisher": "editor-rs",
    "version": "1.0.0",
    "main": "./out/extension.js",
    "engines": { "vscode": "^1.75.0" },
    "activationEvents": ["onCommand:upcase.run"],
    "contributes": {
        "commands": [{ "command": "upcase.run", "title": "Upper-case the document" }]
    }
}"#;

const EXT_JS: &str = r#"
const vscode = require('vscode');
exports.activate = () => {
    vscode.commands.registerCommand('upcase.run', async () => {
        const ed = vscode.window.activeTextEditor;
        const doc = ed.document;
        const last = doc.lineCount - 1;
        const end = new vscode.Position(last, doc.lineAt(last).text.length);
        const full = doc.getText();
        await ed.edit((b) => b.replace(new vscode.Range(new vscode.Position(0, 0), end), full.toUpperCase()));
        return doc.getText();
    });
};
"#;

/// Build a `.vsix` (zip with an `extension/` prefix) at `path`.
fn build_vsix(path: &std::path::Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default();
    zip.start_file("extension/package.json", opts).unwrap();
    zip.write_all(PKG.as_bytes()).unwrap();
    zip.start_file("extension/out/extension.js", opts).unwrap();
    zip.write_all(EXT_JS.as_bytes()).unwrap();
    zip.start_file("extension.vsixmanifest", opts).unwrap();
    zip.write_all(b"<PackageManifest/>").unwrap();
    zip.finish().unwrap();
}

#[test]
fn install_vsix_then_activate_and_run() {
    if !node_available() {
        eprintln!("skipping: `node` not found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let vsix_path = dir.path().join("upcase-1.0.0.vsix");
    build_vsix(&vsix_path);

    // Install it the way the marketplace path would.
    let installed = vsix::install_vsix(&vsix_path, &dir.path().join("installed")).unwrap();
    assert_eq!(installed.manifest.id(), "editor-rs.upcase");
    assert_eq!(installed.manifest.contributes.commands[0].command, "upcase.run");

    // Run it in a workbench with an open document.
    let ui: Arc<Mutex<dyn HostHandler>> = Arc::new(Mutex::new(NullHandler));
    let wb = Workbench::new(ui).expect("spawn");
    let uri = "file:///project/greeting.txt";
    wb.open_document(uri, "plaintext", "hello world\nsecond line\n").unwrap();
    wb.set_active(uri).unwrap();

    wb.host().activate(&installed).expect("activate");
    let returned = wb.host().execute_command("upcase.run", vec![]).unwrap();

    let expected = "HELLO WORLD\nSECOND LINE\n";
    assert_eq!(returned, expected);
    assert_eq!(wb.document_text(uri).unwrap(), expected);
}
