//! `editor-vscode` — load VS Code assets directly (Stage 3).
//!
//! editor-rs stays fast by being native Rust, but it should feel like VS Code,
//! so it reads VS Code's own declarative assets rather than inventing new
//! formats:
//!
//! * [`Theme`] — VS Code color themes (`colors` + TextMate `tokenColors`),
//!   resolved against our tree-sitter highlight names with TextMate
//!   longest-prefix scope matching.
//! * [`Snippet`] — VS Code snippet files, with a TextMate snippet-body
//!   expander ([`Snippet::expand`]) producing text + ordered tab-stops.
//! * [`LanguageConfig`] — `language-configuration.json` (comments, brackets,
//!   auto-closing/surrounding pairs).
//! * Keybindings — VS Code's `keybindings.json` is already the format the
//!   [`editor_ui`](../editor_ui/index.html) keymap parses, so no separate
//!   loader is needed here; see [`load_keybindings`] for a thin re-parse helper.
//!
//! All loaders accept **JSONC** (comments + trailing commas), which is how
//! VS Code ships these files. See [`jsonc`] for the stripper.

pub mod color;
pub mod jsonc;
pub mod language_config;
pub mod snippets;
pub mod theme;

pub use color::Color;
pub use language_config::{LanguageConfig, Pair};
pub use snippets::{Expansion, Snippet, TabStop};
pub use theme::{FontStyle, Theme, TokenStyle};

/// Convenience: strip JSONC from a VS Code `keybindings.json` and return plain
/// JSON an `editor_ui::Keymap` can parse. Kept here so the whole VS Code asset
/// surface loads through one crate.
pub fn load_keybindings(jsonc_text: &str) -> String {
    jsonc::strip_jsonc(jsonc_text)
}
