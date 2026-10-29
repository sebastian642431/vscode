//! A small dark theme: workspace chrome colours and a mapping from
//! `editor-syntax` highlight names to token colours.

use gpui::{rgb, Rgba};

/// Workspace chrome palette (VS Code "Dark+"-ish).
pub struct Theme {
    pub background: Rgba,
    pub panel: Rgba,
    pub sidebar: Rgba,
    pub tab_active: Rgba,
    pub tab_inactive: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub text_dim: Rgba,
    pub accent: Rgba,
    pub cursor: Rgba,
    pub selection: Rgba,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            background: rgb(0x1e1e1e),
            panel: rgb(0x252526),
            sidebar: rgb(0x252526),
            tab_active: rgb(0x1e1e1e),
            tab_inactive: rgb(0x2d2d2d),
            border: rgb(0x333333),
            text: rgb(0xd4d4d4),
            text_dim: rgb(0x858585),
            accent: rgb(0x0e639c),
            cursor: rgb(0xaeafad),
            selection: rgb(0x264f78),
        }
    }
}

/// Colour for a syntax highlight name (as produced by
/// [`editor_syntax::highlight_name`]). Falls back to the default text colour.
pub fn token_color(name: &str, theme: &Theme) -> Rgba {
    // Match on the most specific prefix first.
    match name {
        n if n.starts_with("comment") => rgb(0x6a9955),
        n if n.starts_with("keyword") => rgb(0x569cd6),
        n if n.starts_with("string") => rgb(0xce9178),
        n if n.starts_with("number") => rgb(0xb5cea8),
        n if n.starts_with("function") || n.starts_with("constructor") => rgb(0xdcdcaa),
        n if n.starts_with("type") => rgb(0x4ec9b0),
        n if n.starts_with("constant") => rgb(0x4fc1ff),
        n if n.starts_with("variable.builtin") || n.starts_with("variable.parameter") => {
            rgb(0x9cdcfe)
        }
        n if n.starts_with("property") => rgb(0x9cdcfe),
        n if n.starts_with("attribute") || n.starts_with("tag") => rgb(0x569cd6),
        n if n.starts_with("operator") || n.starts_with("punctuation") => rgb(0xd4d4d4),
        n if n.starts_with("label") => rgb(0xc8c8c8),
        n if n.starts_with("escape") => rgb(0xd7ba7d),
        _ => theme.text,
    }
}
