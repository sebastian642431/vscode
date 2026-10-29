//! `editor-rpc` — the JSON-RPC 2.0 base protocol shared by editor-rs's LSP
//! client and its Node.js extension host.
//!
//! `Content-Length` framing over a byte stream, plus a [`Connection`] that runs
//! a reader thread and matches responses to the requests that produced them.
//!
//! This layer is transport-agnostic: it works over a child process's
//! stdin/stdout or over in-memory pipes (which is how the tests drive a mock
//! peer without spawning anything).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use serde_json::Value;

/// A JSON-RPC error object returned by the peer.
#[derive(Clone, Debug)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// Anything that can go wrong talking to the server.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Rpc(RpcError),
    /// The connection's reader thread has stopped (server exited).
    Disconnected,
    /// A blocking request timed out.
    Timeout,
    Serde(serde_json::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Rpc(e) => write!(f, "{e}"),
            Error::Disconnected => write!(f, "server disconnected"),
            Error::Timeout => write!(f, "request timed out"),
            Error::Serde(e) => write!(f, "serialization error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Serde(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A message pushed by the peer that isn't a response to one of our requests:
/// a notification (e.g. `publishDiagnostics`) or a server→client request.
#[derive(Clone, Debug)]
pub enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

type PendingMap = Arc<Mutex<HashMap<i64, Sender<std::result::Result<Value, RpcError>>>>>;

/// A live JSON-RPC connection over a reader/writer pair.
pub struct Connection {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pending: PendingMap,
    next_id: AtomicI64,
    incoming_rx: Receiver<Incoming>,
    reader_handle: Option<JoinHandle<()>>,
}

impl Connection {
    /// Start a connection, spawning a reader thread over `reader`.
    pub fn new(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> Connection {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (incoming_tx, incoming_rx) = unbounded();
        let pending_for_reader = Arc::clone(&pending);

        let reader_handle = std::thread::spawn(move || {
            read_loop(reader, pending_for_reader, incoming_tx);
        });

        Connection {
            writer: Arc::new(Mutex::new(Box::new(writer))),
            pending,
            next_id: AtomicI64::new(1),
            incoming_rx,
            reader_handle: Some(reader_handle),
        }
    }

    /// The channel of server-initiated messages (notifications + requests).
    pub fn incoming(&self) -> &Receiver<Incoming> {
        &self.incoming_rx
    }

    /// Send a request and block until the response arrives (or `timeout`).
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = bounded(1);
        self.pending.lock().unwrap().insert(id, tx);

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(e) = self.write_message(&msg) {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }

        match rx.recv_timeout(timeout) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(rpc)) => Err(Error::Rpc(rpc)),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                self.pending.lock().unwrap().remove(&id);
                Err(Error::Timeout)
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(Error::Disconnected),
        }
    }

    /// Send a notification (no response expected).
    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&msg)
    }

    /// Reply to a server→client request.
    pub fn respond(&self, id: Value, result: Value) -> Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });
        self.write_message(&msg)
    }

    fn write_message(&self, msg: &Value) -> Result<()> {
        let body = serde_json::to_vec(msg)?;
        let mut writer = self.writer.lock().unwrap();
        write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
        writer.write_all(&body)?;
        writer.flush()?;
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // The reader thread exits on its own when the stream closes. We don't
        // join (the server may keep the pipe open); detach instead.
        if let Some(handle) = self.reader_handle.take() {
            drop(handle);
        }
    }
}

/// Read framed messages until EOF, routing each to a pending request or the
/// incoming channel.
fn read_loop(reader: impl Read, pending: PendingMap, incoming_tx: Sender<Incoming>) {
    let mut reader = BufReader::new(reader);
    loop {
        let body = match read_message(&mut reader) {
            Ok(Some(body)) => body,
            Ok(None) => break, // clean EOF
            Err(_) => break,   // broken stream
        };
        let Ok(value) = serde_json::from_slice::<Value>(&body) else {
            continue; // skip a malformed frame
        };

        let has_id = value.get("id").map(|v| !v.is_null()).unwrap_or(false);
        let is_response = has_id
            && (value.get("result").is_some() || value.get("error").is_some());

        if is_response {
            let id = value.get("id").and_then(Value::as_i64);
            if let Some(id) = id {
                if let Some(tx) = pending.lock().unwrap().remove(&id) {
                    let result = if let Some(err) = value.get("error") {
                        Err(RpcError {
                            code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                            message: err
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            data: err.get("data").cloned(),
                        })
                    } else {
                        Ok(value.get("result").cloned().unwrap_or(Value::Null))
                    };
                    let _ = tx.send(result);
                }
            }
        } else if let Some(method) = value.get("method").and_then(Value::as_str) {
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            let msg = if has_id {
                Incoming::Request {
                    id: value.get("id").cloned().unwrap_or(Value::Null),
                    method: method.to_string(),
                    params,
                }
            } else {
                Incoming::Notification {
                    method: method.to_string(),
                    params,
                }
            };
            if incoming_tx.send(msg).is_err() {
                break; // client dropped
            }
        }
    }
    // Draining `pending` here would let blocked callers see Disconnected via the
    // channel closing when the senders drop.
}

/// Read one `Content-Length`-framed message body. `Ok(None)` means clean EOF.
fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(rest) = trimmed
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
        {
            content_length = rest.trim().parse().ok();
        }
    }
    let len = content_length.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "missing Content-Length")
    })?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_through_a_pipe() {
        // Write a framed message into one pipe and read it back.
        let (reader, mut writer) = std::io::pipe().unwrap();
        let body = br#"{"jsonrpc":"2.0","method":"hi","params":{}}"#;
        write!(writer, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        writer.write_all(body).unwrap();
        drop(writer);

        let mut buf = BufReader::new(reader);
        let got = read_message(&mut buf).unwrap().unwrap();
        assert_eq!(got, body);
        assert!(read_message(&mut buf).unwrap().is_none()); // EOF
    }

    #[test]
    fn request_response_matching() {
        // A mock peer that echoes every request id back as a result.
        let (to_server_rx, to_server_tx) = std::io::pipe().unwrap();
        let (from_server_rx, from_server_tx) = std::io::pipe().unwrap();

        // Mock server thread.
        std::thread::spawn(move || {
            let mut reader = BufReader::new(to_server_rx);
            let mut writer = from_server_tx;
            while let Ok(Some(body)) = read_message(&mut reader) {
                let v: Value = serde_json::from_slice(&body).unwrap();
                if let Some(id) = v.get("id") {
                    let resp = serde_json::json!({
                        "jsonrpc":"2.0","id": id,
                        "result": {"echoed": v.get("method")}
                    });
                    let bytes = serde_json::to_vec(&resp).unwrap();
                    write!(writer, "Content-Length: {}\r\n\r\n", bytes.len()).unwrap();
                    writer.write_all(&bytes).unwrap();
                    writer.flush().unwrap();
                }
            }
        });

        let conn = Connection::new(from_server_rx, to_server_tx);
        let result = conn
            .request("ping", Value::Null, Duration::from_secs(5))
            .unwrap();
        assert_eq!(result["echoed"], "ping");
    }

    #[test]
    fn notifications_reach_the_incoming_channel() {
        let (to_server_rx, to_server_tx) = std::io::pipe().unwrap();
        let (from_server_rx, from_server_tx) = std::io::pipe().unwrap();

        std::thread::spawn(move || {
            let mut reader = BufReader::new(to_server_rx);
            let mut writer = from_server_tx;
            // Wait for one message, then push a notification back.
            if read_message(&mut reader).unwrap().is_some() {
                let note = serde_json::json!({
                    "jsonrpc":"2.0","method":"window/logMessage",
                    "params":{"message":"hello"}
                });
                let bytes = serde_json::to_vec(&note).unwrap();
                write!(writer, "Content-Length: {}\r\n\r\n", bytes.len()).unwrap();
                writer.write_all(&bytes).unwrap();
                writer.flush().unwrap();
            }
            // Keep the pipe open briefly.
            std::thread::sleep(Duration::from_millis(50));
        });

        let conn = Connection::new(from_server_rx, to_server_tx);
        conn.notify("kick", Value::Null).unwrap();
        let msg = conn
            .incoming()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        match msg {
            Incoming::Notification { method, params } => {
                assert_eq!(method, "window/logMessage");
                assert_eq!(params["message"], "hello");
            }
            other => panic!("expected notification, got {other:?}"),
        }
    }
}
