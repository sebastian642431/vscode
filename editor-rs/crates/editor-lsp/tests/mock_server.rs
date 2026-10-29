//! Drives the LSP client against an in-process mock server connected by real
//! OS pipes — exercising the full framing + request/response + notification
//! path without spawning anything.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::Duration;

use editor_core::Document;
use editor_lsp::lsp_types::Uri;
use editor_lsp::LspClient;
use serde_json::{json, Value};

// ----- minimal framing (the mock server's own copy) -------------------------

fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let t = line.trim_end();
        if t.is_empty() {
            break;
        }
        if let Some(rest) = t.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = rest.trim().parse::<usize>().ok();
        }
    }
    let len = content_length?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn write_message(writer: &mut impl Write, msg: &Value) {
    let bytes = serde_json::to_vec(msg).unwrap();
    write!(writer, "Content-Length: {}\r\n\r\n", bytes.len()).unwrap();
    writer.write_all(&bytes).unwrap();
    writer.flush().unwrap();
}

/// A mock language server: UTF-8 positions, incremental sync, canned replies.
fn run_mock_server(reader: impl Read, writer: impl Write) {
    let mut reader = BufReader::new(reader);
    let mut writer = writer;
    while let Some(msg) = read_message(&mut reader) {
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned();
        match method {
            "initialize" => write_message(
                &mut writer,
                &json!({
                    "jsonrpc":"2.0","id": id,
                    "result": {
                        "capabilities": {
                            "positionEncoding": "utf-8",
                            "textDocumentSync": 2,
                            "completionProvider": {},
                            "hoverProvider": true,
                            "definitionProvider": true
                        }
                    }
                }),
            ),
            "textDocument/didOpen" => write_message(
                &mut writer,
                &json!({
                    "jsonrpc":"2.0","method":"textDocument/publishDiagnostics",
                    "params": {
                        "uri": msg["params"]["textDocument"]["uri"],
                        "diagnostics": [{
                            "range": {"start":{"line":0,"character":0},"end":{"line":0,"character":3}},
                            "severity": 1,
                            "message": "mock diagnostic"
                        }]
                    }
                }),
            ),
            "textDocument/didChange" => {
                // Echo the first content change's range back as a diagnostic so
                // the test can confirm incremental sync produced a range.
                let first = &msg["params"]["contentChanges"][0];
                let range = &first["range"];
                let message = if range.is_null() {
                    "full-sync".to_string()
                } else {
                    format!(
                        "{}:{}-{}:{} text={}",
                        range["start"]["line"],
                        range["start"]["character"],
                        range["end"]["line"],
                        range["end"]["character"],
                        first["text"].as_str().unwrap_or("")
                    )
                };
                write_message(
                    &mut writer,
                    &json!({
                        "jsonrpc":"2.0","method":"textDocument/publishDiagnostics",
                        "params": {
                            "uri": msg["params"]["textDocument"]["uri"],
                            "diagnostics": [{
                                "range": {"start":{"line":0,"character":0},"end":{"line":0,"character":0}},
                                "severity": 2,
                                "message": message
                            }]
                        }
                    }),
                );
            }
            "textDocument/completion" => write_message(
                &mut writer,
                &json!({
                    "jsonrpc":"2.0","id": id,
                    "result": [
                        {"label":"foo","kind":3},
                        {"label":"bar","kind":6}
                    ]
                }),
            ),
            "textDocument/hover" => write_message(
                &mut writer,
                &json!({
                    "jsonrpc":"2.0","id": id,
                    "result": {"contents": "hover text"}
                }),
            ),
            "textDocument/definition" => write_message(
                &mut writer,
                &json!({
                    "jsonrpc":"2.0","id": id,
                    "result": [{
                        "uri": msg["params"]["textDocument"]["uri"],
                        "range": {"start":{"line":5,"character":0},"end":{"line":5,"character":3}}
                    }]
                }),
            ),
            "shutdown" => write_message(
                &mut writer,
                &json!({"jsonrpc":"2.0","id": id, "result": null}),
            ),
            "exit" => break,
            _ => {
                if let Some(id) = id {
                    write_message(
                        &mut writer,
                        &json!({"jsonrpc":"2.0","id": id, "result": null}),
                    );
                }
            }
        }
    }
}

/// Wire a client to a mock server over two pipes.
fn connect() -> LspClient {
    let (to_server_rx, to_server_tx) = std::io::pipe().unwrap();
    let (from_server_rx, from_server_tx) = std::io::pipe().unwrap();
    std::thread::spawn(move || run_mock_server(to_server_rx, from_server_tx));
    LspClient::with_streams(from_server_rx, to_server_tx)
}

fn uri() -> Uri {
    "file:///project/src/main.rs".parse().unwrap()
}

/// Wait up to a second for the client to pump at least one diagnostics update.
fn wait_for_diagnostics(client: &LspClient) -> Vec<editor_lsp::DiagnosticsUpdate> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let updates = client.pump();
        if !updates.is_empty() {
            return updates;
        }
        if std::time::Instant::now() > deadline {
            return Vec::new();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn initialize_negotiates_utf8() {
    let mut client = connect();
    client.initialize(Some("file:///project".parse().unwrap())).unwrap();
    assert_eq!(
        client.position_encoding(),
        editor_lsp::PositionEncoding::Utf8
    );
}

#[test]
fn did_open_yields_diagnostics() {
    let mut client = connect();
    client.initialize(None).unwrap();
    client.did_open(uri(), "rust", "fn main() {}").unwrap();
    let updates = wait_for_diagnostics(&client);
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].diagnostics.len(), 1);
    assert_eq!(updates[0].diagnostics[0].message, "mock diagnostic");
}

#[test]
fn completion_returns_items() {
    let mut client = connect();
    client.initialize(None).unwrap();
    client.did_open(uri(), "rust", "fn main() {}").unwrap();
    let items = client.completion(&uri(), 3).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["label"], "foo");
}

#[test]
fn definition_returns_location() {
    let mut client = connect();
    client.initialize(None).unwrap();
    client.did_open(uri(), "rust", "fn main() {}").unwrap();
    let locs = client.definition(&uri(), 3).unwrap();
    assert_eq!(locs.len(), 1);
    assert_eq!(locs[0]["range"]["start"]["line"], 5);
}

#[test]
fn hover_returns_contents() {
    let mut client = connect();
    client.initialize(None).unwrap();
    client.did_open(uri(), "rust", "fn main() {}").unwrap();
    let hover = client.hover(&uri(), 3).unwrap().unwrap();
    assert_eq!(hover["contents"], "hover text");
}

#[test]
fn did_change_sends_incremental_range() {
    let mut client = connect();
    client.initialize(None).unwrap();
    client.did_open(uri(), "rust", "abc").unwrap();
    let _ = wait_for_diagnostics(&client); // consume the didOpen diagnostics

    // Produce a real editor-core change: insert "X" at the start.
    let mut doc = Document::from_str("abc");
    doc.insert("X");
    let changes = doc.drain_changes();
    client
        .did_change(&uri(), doc.buffer().rope(), &changes)
        .unwrap();

    let updates = wait_for_diagnostics(&client);
    assert_eq!(updates.len(), 1);
    // The mock echoes the incremental range it received.
    assert_eq!(updates[0].diagnostics[0].message, "0:0-0:0 text=X");
}
