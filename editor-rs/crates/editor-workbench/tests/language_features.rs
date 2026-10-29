//! An extension registers completion / hover / definition providers; the
//! editor (via the workbench) calls them and gets results back, mapped to the
//! editor-core model. Requires `node`; skips otherwise.

use std::process::Command;
use std::sync::{Arc, Mutex};

use editor_core::interface::CompletionKind;
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
exports.activate = () => {
    vscode.languages.registerCompletionItemProvider('rust', {
        provideCompletionItems(document, position) {
            const fn1 = new vscode.CompletionItem('println', vscode.CompletionItemKind.Function);
            fn1.insertText = 'println!';
            fn1.detail = 'macro';
            const kw = new vscode.CompletionItem('fn', vscode.CompletionItemKind.Keyword);
            return [fn1, kw];
        }
    });
    vscode.languages.registerHoverProvider('rust', {
        provideHover(document, position) {
            const word = document.getWordRangeAtPosition(position);
            const text = word ? document.getText(word) : '';
            return new vscode.Hover('hovering **' + text + '**');
        }
    });
    vscode.languages.registerDefinitionProvider('rust', {
        provideDefinition(document, position) {
            return new vscode.Location(document.uri, new vscode.Range(new vscode.Position(10, 0), new vscode.Position(10, 4)));
        }
    });
};
"#;

const URI: &str = "file:///project/a.rs";
const TEXT: &str = "fn main() {\n    prin\n}\n";

fn setup() -> Option<Workbench> {
    if !node_available() {
        eprintln!("skipping: `node` not found");
        return None;
    }
    let ui: Arc<Mutex<dyn HostHandler>> = Arc::new(Mutex::new(NullHandler));
    let wb = Workbench::new(ui).expect("spawn");
    wb.open_document(URI, "rust", TEXT).unwrap();
    wb.set_active(URI).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let pkg = r#"{ "name":"lang", "publisher":"editor-rs", "main":"./extension.js" }"#;
    std::fs::write(dir.path().join("package.json"), pkg).unwrap();
    std::fs::write(dir.path().join("extension.js"), EXT).unwrap();
    let manifest = ExtensionManifest::from_json(pkg).unwrap();
    wb.host()
        .activate_parts(dir.path().to_str().unwrap(), &manifest)
        .unwrap();
    // Keep the tempdir alive for the life of the workbench.
    std::mem::forget(dir);
    Some(wb)
}

#[test]
fn completion_from_extension_provider() {
    let Some(wb) = setup() else { return };
    // Offset at the end of "prin" on line 1 (char offset 19).
    let off = TEXT.find("prin").unwrap() + 4;
    let items = wb.completion_at(URI, off).unwrap();
    assert_eq!(items.len(), 2);
    let println = items.iter().find(|c| c.label == "println").unwrap();
    assert_eq!(println.insert_text, "println!");
    assert_eq!(println.detail.as_deref(), Some("macro"));
    assert_eq!(println.kind, CompletionKind::Function);
    assert!(items.iter().any(|c| c.kind == CompletionKind::Keyword));
}

#[test]
fn hover_from_extension_provider() {
    let Some(wb) = setup() else { return };
    // Hover over "main" (offset 3 is inside "main").
    let off = TEXT.find("main").unwrap() + 1;
    let hover = wb.hover_at(URI, off).unwrap();
    assert_eq!(hover.as_deref(), Some("hovering **main**"));
}

#[test]
fn definition_from_extension_provider() {
    let Some(wb) = setup() else { return };
    let off = TEXT.find("main").unwrap() + 1;
    let locs = wb.definition_at(URI, off).unwrap();
    assert_eq!(locs.len(), 1);
    assert_eq!(locs[0]["startLine"], 10);
    assert_eq!(locs[0]["uri"], "file:///project/a.rs");
}
