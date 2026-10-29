//! `editor-fs` — everything editor-rs does with the file system, kept off the
//! UI thread.
//!
//! Three concerns live here, and none of them may ever run on the typing path:
//!
//! * [`tree`] — walking a workspace folder into a [`FileTree`], honouring
//!   `.gitignore`. Directory listing is lazy (one level at a time) so opening a
//!   huge monorepo doesn't stat a million files up front.
//! * [`watch`] — a thin wrapper over [`notify`] that turns native file-system
//!   events into a [`crossbeam_channel`] stream, so the workspace model can be
//!   refreshed reactively.
//! * [`search`] — project-wide search built on the ripgrep crates
//!   (`ignore` + `grep`). The `ignore` parallel walker fans work across all
//!   cores; results stream back through a channel and the search is
//!   cancellable.
//!
//! All blocking work (stat, read, walk) is meant to be driven from a worker
//! thread; the functions here are synchronous building blocks the UI schedules.

pub mod search;
pub mod tree;
pub mod watch;

pub use search::{SearchMatch, SearchOptions, Searcher};
pub use tree::{Entry, EntryKind, FileTree};
pub use watch::{FsEvent, FsEventKind, Watcher};
