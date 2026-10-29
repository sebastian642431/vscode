//! Reactive extensions: publishing diagnostics (linters) and reacting to
//! document changes. Requires `node`; skips otherwise.

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use editor_core::interface::DiagnosticSeverity;
use editor_core::Point;
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
exports.activate = (ctx) => {
    const coll = vscode.languages.createDiagnosticCollection('mylint');
    let changeCount = 0;
    ctx.subscriptions.push(vscode.workspace.onDidChangeTextDocument(() => { changeCount++; }));
    vscode.commands.registerCommand('demo.lint', () => {
        const doc = vscode.window.activeTextEditor.document;
        const diag = new vscode.Diagnostic(
            new vscode.Range(new vscode.Position(0, 0), new vscode.Position(0, 3)),
            'bad thing',
            vscode.DiagnosticSeverity.Warning
        );
        diag.source = 'mylint';
        coll.set(doc.uri, [diag]);
        return true;
    });
    vscode.commands.registerCommand('demo.changes', () => changeCount);
};
"#;

const URI: &str = "file:///project/a.rs";

fn setup() -> Option<Workbench> {
    if !node_available() {
        eprintln!("skipping: `node` not found");
        return None;
    }
    let ui: Arc<Mutex<dyn HostHandler>> = Arc::new(Mutex::new(NullHandler));
    let wb = Workbench::new(ui).expect("spawn");
    wb.open_document(URI, "rust", "let x = 1;\n").unwrap();
    wb.set_active(URI).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let pkg = r#"{ "name":"react", "publisher":"editor-rs", "main":"./extension.js" }"#;
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
fn extension_publishes_diagnostics() {
    let Some(wb) = setup() else { return };
    wb.host().execute_command("demo.lint", vec![]).unwrap();

    // The publish is a notification; wait for the pump thread to store it.
    let deadline = Instant::now() + Duration::from_secs(2);
    while wb.diagnostics(URI).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    let diags = wb.diagnostics(URI);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].message, "bad thing");
    assert_eq!(diags[0].severity, DiagnosticSeverity::Warning);
    assert_eq!(diags[0].source.as_deref(), Some("mylint"));
    assert_eq!(diags[0].range.start, Point::new(0, 0));
    assert_eq!(diags[0].range.end, Point::new(0, 3));
}

#[test]
fn extension_reacts_to_document_changes() {
    let Some(wb) = setup() else { return };
    // No changes yet.
    assert_eq!(wb.host().execute_command("demo.changes", vec![]).unwrap(), 0);

    // A user edit pushes document.change to the host; Node processes it before
    // the next request (stdin is ordered), so the count is observable.
    wb.apply_user_edit(URI, 0..0, "// top\n").unwrap();
    assert_eq!(wb.host().execute_command("demo.changes", vec![]).unwrap(), 1);

    wb.apply_user_edit(URI, 0..0, "// more\n").unwrap();
    assert_eq!(wb.host().execute_command("demo.changes", vec![]).unwrap(), 2);
}
