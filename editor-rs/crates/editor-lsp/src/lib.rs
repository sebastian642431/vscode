//! `editor-lsp` — a Language Server Protocol client for editor-rs (Stage 2).
//!
//! The client speaks JSON-RPC over a byte stream (a spawned server's stdio, or
//! in-memory pipes in tests), runs the `initialize`/`initialized` handshake,
//! negotiates the position encoding, keeps documents in sync from editor-core's
//! [`editor_core::Change`] stream, and exposes completion / definition / hover
//! plus a diagnostics pump.
//!
//! It is built to satisfy the [`editor_core::interface`] seams defined in
//! Stage 1, and to run entirely on a worker thread so the typing path is never
//! blocked on a socket.
//!
//! ```no_run
//! use std::process::Command;
//! use editor_lsp::LspClient;
//! use editor_lsp::lsp_types::Uri;
//!
//! let mut client = LspClient::spawn(Command::new("rust-analyzer"))?;
//! client.initialize(Some("file:///my/project".parse::<Uri>().unwrap()))?;
//! client.did_open(
//!     "file:///my/project/src/main.rs".parse::<Uri>().unwrap(),
//!     "rust",
//!     "fn main() {}",
//! )?;
//! // ... later, on the event loop:
//! for update in client.pump() {
//!     // push `update.diagnostics` into the gutter
//! }
//! # Ok::<(), editor_lsp::Error>(())
//! ```

pub mod client;
pub mod convert;
pub mod jsonrpc;

pub use client::{DiagnosticsUpdate, LspClient};
pub use convert::PositionEncoding;
pub use jsonrpc::{Connection, Error, Incoming, Result, RpcError};

// Re-export the LSP types crate so downstream code shares one version.
pub use lsp_types;
