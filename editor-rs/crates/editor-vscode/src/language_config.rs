//! Loading VS Code `language-configuration.json`.
//!
//! This is what a language extension contributes to describe comment syntax,
//! bracket pairs, auto-closing and surrounding pairs — the data the editor
//! needs for comment toggling, bracket matching and auto-insertion. The files
//! are JSONC.

use serde::Deserialize;

use crate::jsonc::strip_jsonc;

/// A bracket / auto-closing pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    pub open: String,
    pub close: String,
    /// Scopes the pair should NOT auto-close in (e.g. `"string"`, `"comment"`).
    pub not_in: Vec<String>,
}

/// A loaded language configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LanguageConfig {
    pub line_comment: Option<String>,
    pub block_comment: Option<(String, String)>,
    pub brackets: Vec<(String, String)>,
    pub auto_closing_pairs: Vec<Pair>,
    pub surrounding_pairs: Vec<(String, String)>,
}

// ---- raw JSON shapes -------------------------------------------------------

#[derive(Deserialize, Default)]
struct RawComments {
    #[serde(default, rename = "lineComment")]
    line_comment: Option<String>,
    #[serde(default, rename = "blockComment")]
    block_comment: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawPair {
    Tuple([String; 2]),
    Object {
        open: String,
        close: String,
        #[serde(default, rename = "notIn")]
        not_in: Vec<String>,
    },
}

impl RawPair {
    fn into_pair(self) -> Pair {
        match self {
            RawPair::Tuple([open, close]) => Pair {
                open,
                close,
                not_in: Vec::new(),
            },
            RawPair::Object {
                open,
                close,
                not_in,
            } => Pair {
                open,
                close,
                not_in,
            },
        }
    }
}

#[derive(Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    comments: RawComments,
    #[serde(default)]
    brackets: Vec<[String; 2]>,
    #[serde(default, rename = "autoClosingPairs")]
    auto_closing_pairs: Vec<RawPair>,
    #[serde(default, rename = "surroundingPairs")]
    surrounding_pairs: Vec<RawPair>,
}

impl LanguageConfig {
    /// Parse a `language-configuration.json` (JSONC).
    pub fn from_json(json: &str) -> serde_json::Result<LanguageConfig> {
        let raw: RawConfig = serde_json::from_str(&strip_jsonc(json))?;
        let block_comment = raw.comments.block_comment.and_then(|v| {
            if v.len() == 2 {
                Some((v[0].clone(), v[1].clone()))
            } else {
                None
            }
        });
        Ok(LanguageConfig {
            line_comment: raw.comments.line_comment,
            block_comment,
            brackets: raw
                .brackets
                .into_iter()
                .map(|[a, b]| (a, b))
                .collect(),
            auto_closing_pairs: raw
                .auto_closing_pairs
                .into_iter()
                .map(RawPair::into_pair)
                .collect(),
            surrounding_pairs: raw
                .surrounding_pairs
                .into_iter()
                .map(|p| {
                    let p = p.into_pair();
                    (p.open, p.close)
                })
                .collect(),
        })
    }

    /// The closing bracket the editor should auto-insert after `open`, if any.
    pub fn closing_for(&self, open: &str) -> Option<&str> {
        self.auto_closing_pairs
            .iter()
            .find(|p| p.open == open)
            .map(|p| p.close.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A trimmed version of a real rust language-configuration.json (with
    // comments and a trailing comma, as VS Code ships them).
    const CONFIG: &str = r#"{
        // Rust language configuration
        "comments": {
            "lineComment": "//",
            "blockComment": ["/*", "*/"],
        },
        "brackets": [["{", "}"], ["[", "]"], ["(", ")"]],
        "autoClosingPairs": [
            { "open": "{", "close": "}" },
            { "open": "\"", "close": "\"", "notIn": ["string"] },
            ["(", ")"]
        ],
        "surroundingPairs": [["{", "}"], ["(", ")"]]
    }"#;

    #[test]
    fn parses_comments() {
        let cfg = LanguageConfig::from_json(CONFIG).unwrap();
        assert_eq!(cfg.line_comment.as_deref(), Some("//"));
        assert_eq!(
            cfg.block_comment,
            Some(("/*".to_string(), "*/".to_string()))
        );
    }

    #[test]
    fn parses_brackets_and_pairs() {
        let cfg = LanguageConfig::from_json(CONFIG).unwrap();
        assert_eq!(cfg.brackets.len(), 3);
        assert_eq!(cfg.closing_for("{"), Some("}"));
        assert_eq!(cfg.closing_for("("), Some(")")); // from the tuple form
        let quote = cfg
            .auto_closing_pairs
            .iter()
            .find(|p| p.open == "\"")
            .unwrap();
        assert_eq!(quote.not_in, vec!["string"]);
    }

    #[test]
    fn empty_config_is_default() {
        let cfg = LanguageConfig::from_json("{}").unwrap();
        assert_eq!(cfg, LanguageConfig::default());
    }
}
