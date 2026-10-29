//! End-to-end: spawn the Node host, load a real (tiny) VS Code extension,
//! activate it, and drive its commands through the `vscode` API shim.
//!
//! Requires `node` on PATH (override with `EDITOR_RS_NODE`). If Node isn't
//! available the tests skip rather than fail.

use std::process::Command;
use std::sync::{Arc, Mutex};

use editor_ext::{ExtensionHost, ExtensionManifest, HostHandler, MessageLevel};
use serde_json::{json, Value};

/// Poll `rec` for up to ~2s until `pred` holds; returns whether it did.
fn wait_until(rec: &Arc<Mutex<Records>>, pred: impl Fn(&Records) -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if pred(&rec.lock().unwrap()) {
            return true;
        }
        if std::time::Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn node_available() -> bool {
    let node = std::env::var("EDITOR_RS_NODE").unwrap_or_else(|_| "node".to_string());
    Command::new(node)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[derive(Default)]
struct Records {
    messages: Vec<(MessageLevel, String)>,
    commands: Vec<String>,
    logs: Vec<String>,
    builtins: Vec<String>,
}

struct Recorder {
    rec: Arc<Mutex<Records>>,
    /// What show_message returns (simulating the user's click).
    message_reply: Option<String>,
}

impl HostHandler for Recorder {
    fn show_message(&mut self, level: MessageLevel, message: &str, _items: &[String]) -> Option<String> {
        self.rec.lock().unwrap().messages.push((level, message.to_string()));
        self.message_reply.clone()
    }
    fn command_registered(&mut self, id: &str) {
        self.rec.lock().unwrap().commands.push(id.to_string());
    }
    fn execute_builtin(&mut self, command: &str, _args: &[Value]) -> Value {
        self.rec.lock().unwrap().builtins.push(command.to_string());
        json!({ "ran": command })
    }
    fn log(&mut self, message: &str) {
        self.rec.lock().unwrap().logs.push(message.to_string());
    }
}

/// Write a minimal CommonJS extension into `dir` and return its manifest.
fn write_extension(dir: &std::path::Path, extension_js: &str) -> ExtensionManifest {
    let pkg = r#"{
        "name": "demo",
        "publisher": "editor-rs",
        "version": "0.0.1",
        "main": "./extension.js",
        "engines": { "vscode": "^1.75.0" },
        "activationEvents": ["onCommand:demo.hello"],
        "contributes": { "commands": [{ "command": "demo.hello", "title": "Hello" }] }
    }"#;
    std::fs::write(dir.join("package.json"), pkg).unwrap();
    std::fs::write(dir.join("extension.js"), extension_js).unwrap();
    ExtensionManifest::from_json(pkg).unwrap()
}

fn setup(ext_js: &str, reply: Option<&str>) -> Option<(ExtensionHost, Arc<Mutex<Records>>, tempfile::TempDir, ExtensionManifest)> {
    if !node_available() {
        eprintln!("skipping: `node` not found on PATH");
        return None;
    }
    let rec = Arc::new(Mutex::new(Records::default()));
    let handler: Arc<Mutex<dyn HostHandler>> = Arc::new(Mutex::new(Recorder {
        rec: Arc::clone(&rec),
        message_reply: reply.map(str::to_string),
    }));
    let host = ExtensionHost::spawn(handler).expect("spawn node host");
    let dir = tempfile::tempdir().unwrap();
    let manifest = write_extension(dir.path(), ext_js);
    Some((host, rec, dir, manifest))
}

const EXT: &str = r#"
const vscode = require('vscode');
exports.activate = (context) => {
    const d = vscode.commands.registerCommand('demo.hello', async (name) => {
        const picked = await vscode.window.showInformationMessage('Hello ' + (name || 'World'), 'OK');
        return { answer: 42, picked: picked };
    });
    context.subscriptions.push(d);
    vscode.commands.registerCommand('demo.callBuiltin', async () => {
        return await vscode.commands.executeCommand('editor.action.formatDocument');
    });
};
exports.deactivate = () => {};
"#;

#[test]
fn activates_and_registers_commands() {
    let Some((host, rec, dir, manifest)) = setup(EXT, None) else { return };
    let report = host
        .activate_parts(dir.path().to_str().unwrap(), &manifest)
        .expect("activate");
    assert_eq!(report["activated"], true);
    let cmds = report["commands"].as_array().unwrap();
    let names: Vec<&str> = cmds.iter().filter_map(|v| v.as_str()).collect();
    assert!(names.contains(&"demo.hello"));
    assert!(names.contains(&"demo.callBuiltin"));
    // The host also notified Rust of each registration (delivered async).
    assert!(
        wait_until(&rec, |r| r.commands.contains(&"demo.hello".to_string())),
        "demo.hello registration should reach the handler"
    );
}

#[test]
fn executes_command_and_rounds_trips_vscode_api() {
    let Some((host, rec, dir, manifest)) = setup(EXT, Some("OK")) else { return };
    host.activate_parts(dir.path().to_str().unwrap(), &manifest).unwrap();

    // Run the command the extension registered, passing an argument.
    let result = host
        .execute_command("demo.hello", vec![json!("Rust")])
        .expect("execute");
    assert_eq!(result["answer"], 42);
    // The user's "OK" click flowed back into the extension.
    assert_eq!(result["picked"], "OK");

    // The extension's showInformationMessage reached our handler. Copy out of
    // the lock before asserting so a failure can't poison the pump thread.
    let messages = rec.lock().unwrap().messages.clone();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].0, MessageLevel::Info);
    assert_eq!(messages[0].1, "Hello Rust");
}

#[test]
fn extension_can_call_a_builtin_command() {
    let Some((host, rec, dir, manifest)) = setup(EXT, None) else { return };
    host.activate_parts(dir.path().to_str().unwrap(), &manifest).unwrap();

    let result = host.execute_command("demo.callBuiltin", vec![]).unwrap();
    // The unknown command was routed to Rust's execute_builtin.
    assert_eq!(result["ran"], "editor.action.formatDocument");
    let builtins = rec.lock().unwrap().builtins.clone();
    assert_eq!(builtins, vec!["editor.action.formatDocument".to_string()]);
}

#[test]
fn unknown_command_errors() {
    let Some((host, _rec, dir, manifest)) = setup(EXT, None) else { return };
    host.activate_parts(dir.path().to_str().unwrap(), &manifest).unwrap();
    let err = host.execute_command("demo.doesNotExist", vec![]);
    assert!(err.is_err(), "executing an unregistered command should error");
}
