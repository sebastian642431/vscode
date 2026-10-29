//! `editor-syntax` — incremental tree-sitter parsing and highlighting.
//!
//! This crate is fed by [`editor_core::Change`]s and never runs on the typing
//! path: the UI drains changes from a [`editor_core::Document`] after each edit
//! and hands them to a [`SyntaxTree`] on a worker, which reparses only the
//! edited region. Highlighting is done per visible slice by a [`Highlighter`].
//!
//! Supported languages (Stage 1): Rust, TypeScript/TSX, JSON and Python.
//!
//! See [`highlight`] for the parsing/highlighting types and [`language`] for
//! the language registry.

pub mod highlight;
pub mod language;

pub use highlight::{
    highlight_name, HighlightConfigs, HighlightSpan, Highlighter, SyntaxTree, HIGHLIGHT_NAMES,
};
pub use language::{Language, ALL_LANGUAGES};
