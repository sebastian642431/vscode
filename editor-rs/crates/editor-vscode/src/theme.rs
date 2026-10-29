//! Loading VS Code color themes.
//!
//! A VS Code theme carries two things editor-rs cares about:
//! * `colors` — workbench UI colours keyed by dotted ids
//!   (`editor.background`, `editorCursor.foreground`, …).
//! * `tokenColors` — TextMate-style rules mapping *scopes*
//!   (`keyword.control`, `string.quoted`) to foreground colours and font
//!   styles.
//!
//! Our syntax layer produces tree-sitter highlight names (`keyword`, `string`,
//! `function`, …). [`Theme::highlight_color`] bridges the two: each highlight
//! name maps to an ordered list of candidate TextMate scopes, and the theme is
//! queried with TextMate's longest-prefix-wins scope matching — exactly how
//! VS Code resolves a token's colour — so real themes light up our tokens.

use std::collections::HashMap;

use serde::Deserialize;

use crate::color::Color;

/// A font style on a token rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FontStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl FontStyle {
    fn parse(s: &str) -> FontStyle {
        FontStyle {
            bold: s.contains("bold"),
            italic: s.contains("italic"),
            underline: s.contains("underline"),
        }
    }
}

/// A resolved token colour + style.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TokenStyle {
    pub color: Option<Color>,
    pub font: FontStyle,
}

#[derive(Clone, Debug)]
struct TokenRule {
    /// A single TextMate scope selector (e.g. `keyword.control`).
    scope: String,
    style: TokenStyle,
}

/// A loaded VS Code color theme.
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub is_dark: bool,
    colors: HashMap<String, Color>,
    rules: Vec<TokenRule>,
}

// ---- raw JSON shapes -------------------------------------------------------

#[derive(Deserialize)]
struct RawTheme {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    colors: HashMap<String, String>,
    #[serde(default, rename = "tokenColors")]
    token_colors: Vec<RawTokenColor>,
}

#[derive(Deserialize)]
struct RawTokenColor {
    #[serde(default)]
    scope: Option<RawScope>,
    #[serde(default)]
    settings: RawSettings,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawScope {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize, Default)]
struct RawSettings {
    #[serde(default)]
    foreground: Option<String>,
    #[serde(default, rename = "fontStyle")]
    font_style: Option<String>,
}

impl Theme {
    /// Parse a theme from its JSON. `include` directives are ignored (the
    /// theme is treated as self-contained).
    pub fn from_json(json: &str) -> serde_json::Result<Theme> {
        let raw: RawTheme = serde_json::from_str(&crate::jsonc::strip_jsonc(json))?;

        let colors = raw
            .colors
            .iter()
            .filter_map(|(k, v)| Color::parse_hex(v).map(|c| (k.clone(), c)))
            .collect();

        let mut rules = Vec::new();
        for tc in &raw.token_colors {
            let style = TokenStyle {
                color: tc.settings.foreground.as_deref().and_then(Color::parse_hex),
                font: tc
                    .settings
                    .font_style
                    .as_deref()
                    .map(FontStyle::parse)
                    .unwrap_or_default(),
            };
            let scopes: Vec<String> = match &tc.scope {
                Some(RawScope::One(s)) => s
                    .split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect(),
                Some(RawScope::Many(v)) => v.iter().map(|s| s.trim().to_string()).collect(),
                None => Vec::new(),
            };
            for scope in scopes {
                rules.push(TokenRule {
                    scope,
                    style,
                });
            }
        }

        Ok(Theme {
            name: raw.name.unwrap_or_else(|| "Untitled".to_string()),
            is_dark: raw.kind.as_deref() != Some("light"),
            colors,
            rules,
        })
    }

    /// Look up a workbench UI colour (e.g. `"editor.background"`).
    pub fn ui_color(&self, key: &str) -> Option<Color> {
        self.colors.get(key).copied()
    }

    /// Resolve a TextMate scope to a token style using longest-prefix matching:
    /// a rule `keyword` matches `keyword.control.rust`, and `keyword.control`
    /// beats it by being more specific.
    pub fn token_style(&self, scope: &str) -> Option<TokenStyle> {
        let mut best: Option<(usize, TokenStyle)> = None;
        for rule in &self.rules {
            if scope_matches(&rule.scope, scope) {
                let specificity = rule.scope.split('.').count();
                if best.map(|(s, _)| specificity >= s).unwrap_or(true) {
                    best = Some((specificity, rule.style));
                }
            }
        }
        best.map(|(_, style)| style)
    }

    /// Resolve the colour for one of our tree-sitter highlight names by trying
    /// its candidate TextMate scopes in order.
    pub fn highlight_style(&self, highlight_name: &str) -> Option<TokenStyle> {
        for scope in candidate_scopes(highlight_name) {
            if let Some(style) = self.token_style(scope) {
                if style.color.is_some() {
                    return Some(style);
                }
            }
        }
        // Fall back to the editor's default foreground.
        self.ui_color("editor.foreground").map(|c| TokenStyle {
            color: Some(c),
            font: FontStyle::default(),
        })
    }

    /// Convenience: the colour for a highlight name, if any.
    pub fn highlight_color(&self, highlight_name: &str) -> Option<Color> {
        self.highlight_style(highlight_name).and_then(|s| s.color)
    }
}

/// Does TextMate scope selector `rule` match `query`? A selector matches the
/// query when it is equal to it or a dotted prefix of it.
fn scope_matches(rule: &str, query: &str) -> bool {
    query == rule || query.starts_with(&format!("{rule}."))
}

/// The ordered TextMate scopes to try for one of our highlight names. Themes
/// vary, so we list the common scopes most specific first.
fn candidate_scopes(highlight_name: &str) -> &'static [&'static str] {
    match highlight_name {
        "attribute" => &["entity.other.attribute-name", "meta.attribute"],
        "comment" => &["comment"],
        "constant" => &["constant", "constant.language"],
        "constant.builtin" => &["constant.language", "constant"],
        "constructor" => &["entity.name.function", "support.class"],
        "escape" => &["constant.character.escape"],
        "function" => &["entity.name.function", "support.function"],
        "function.builtin" => &["support.function", "entity.name.function"],
        "function.macro" => &["entity.name.function.macro", "entity.name.function"],
        "function.method" => &["entity.name.function.member", "entity.name.function"],
        "keyword" => &["keyword", "keyword.control"],
        "label" => &["entity.name.label", "constant.other.label"],
        "number" => &["constant.numeric"],
        "operator" => &["keyword.operator"],
        "property" => &["variable.other.property", "support.type.property-name"],
        "punctuation" => &["punctuation"],
        "punctuation.bracket" => &["punctuation.definition", "punctuation"],
        "punctuation.delimiter" => &["punctuation.separator", "punctuation"],
        "string" => &["string"],
        "string.special" => &["string.regexp", "string"],
        "tag" => &["entity.name.tag"],
        "type" => &["entity.name.type", "support.type", "storage.type"],
        "type.builtin" => &["support.type", "storage.type"],
        "variable" => &["variable"],
        "variable.builtin" => &["variable.language", "variable"],
        "variable.parameter" => &["variable.parameter", "variable"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME: &str = r##"{
        "name": "Test Dark",
        "type": "dark",
        "colors": {
            "editor.background": "#1e1e1e",
            "editor.foreground": "#d4d4d4"
        },
        "tokenColors": [
            { "scope": "comment", "settings": { "foreground": "#6a9955", "fontStyle": "italic" } },
            { "scope": ["keyword", "storage.type"], "settings": { "foreground": "#569cd6" } },
            { "scope": "keyword.control", "settings": { "foreground": "#c586c0" } },
            { "scope": "string", "settings": { "foreground": "#ce9178" } },
            { "scope": "entity.name.function", "settings": { "foreground": "#dcdcaa" } }
        ]
    }"##;

    #[test]
    fn loads_metadata_and_ui_colors() {
        let theme = Theme::from_json(THEME).unwrap();
        assert_eq!(theme.name, "Test Dark");
        assert!(theme.is_dark);
        assert_eq!(theme.ui_color("editor.background"), Color::parse_hex("#1e1e1e"));
    }

    #[test]
    fn scope_longest_prefix_wins() {
        let theme = Theme::from_json(THEME).unwrap();
        // "keyword" rule matches a bare keyword...
        assert_eq!(
            theme.token_style("keyword").unwrap().color,
            Color::parse_hex("#569cd6")
        );
        // ...but "keyword.control.rust" prefers the more specific rule.
        assert_eq!(
            theme.token_style("keyword.control.rust").unwrap().color,
            Color::parse_hex("#c586c0")
        );
    }

    #[test]
    fn highlight_names_map_to_theme_colors() {
        let theme = Theme::from_json(THEME).unwrap();
        assert_eq!(theme.highlight_color("comment"), Color::parse_hex("#6a9955"));
        assert_eq!(theme.highlight_color("keyword"), Color::parse_hex("#569cd6"));
        assert_eq!(theme.highlight_color("string"), Color::parse_hex("#ce9178"));
        assert_eq!(theme.highlight_color("function"), Color::parse_hex("#dcdcaa"));
        assert!(theme.highlight_style("comment").unwrap().font.italic);
    }

    #[test]
    fn unknown_highlight_falls_back_to_foreground() {
        let theme = Theme::from_json(THEME).unwrap();
        // "label" has no matching rule here -> editor.foreground.
        assert_eq!(theme.highlight_color("label"), Color::parse_hex("#d4d4d4"));
    }

    #[test]
    fn every_syntax_highlight_name_resolves() {
        let theme = Theme::from_json(THEME).unwrap();
        for name in editor_syntax::HIGHLIGHT_NAMES {
            assert!(
                theme.highlight_color(name).is_some(),
                "no colour for highlight `{name}`"
            );
        }
    }
}
