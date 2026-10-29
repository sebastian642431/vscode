//! `editor-ui` — the front-end-agnostic UI logic of editor-rs.
//!
//! This crate contains everything the editor's interface *does* without
//! containing any rendering: the [`Workspace`] model, the JSON-configurable
//! [`keymap`], the command-palette [`command`] registry and the [`tabs`] model.
//! The actual GPUI window/views live in the separate `gpui-frontend` crate (see
//! the repository README); keeping them apart is what lets the entire control
//! flow — key event → command id → action — be unit-tested headlessly.
//!
//! # The event loop the shell implements
//!
//! 1. A key event becomes a [`keymap::KeyStroke`].
//! 2. [`keymap::Keymap::resolve`] maps the stroke sequence + [`keymap::Context`]
//!    to a command id (or `Pending` for a half-typed chord).
//! 3. [`Workspace::execute`] runs the command, mutating the active
//!    [`editor_core::Document`].
//! 4. The shell drains [`editor_core::Document::drain_changes`] to the
//!    [`editor_syntax`] worker and repaints.

pub mod command;
pub mod config;
pub mod keymap;
pub mod tabs;
pub mod workspace;

pub use command::{Command, CommandRegistry, Match};
pub use config::{default_commands, default_keymap, keymap_with_user_overrides};
pub use keymap::{Context, KeyBinding, KeyStroke, Keymap, Resolution};
pub use tabs::{Tab, TabBar};
pub use workspace::{OpenDocument, Workspace};
