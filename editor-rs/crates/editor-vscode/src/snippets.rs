//! Loading VS Code snippet files and expanding snippet bodies.
//!
//! A VS Code snippet file maps a name to `{ prefix, body, description }`, where
//! `prefix` and `body` may each be a string or an array of strings. The body
//! uses TextMate snippet syntax: `$1`, `${1:placeholder}`, `$0` (final caret).
//! [`Snippet::expand`] turns that into the literal text to insert plus the
//! ordered tab-stops, which is what the editor needs to drive tab navigation.

use std::collections::HashMap;
use std::ops::Range;

use serde::Deserialize;

/// A single snippet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub name: String,
    pub prefixes: Vec<String>,
    /// Raw body with TextMate placeholders, lines joined by `\n`.
    pub body: String,
    pub description: Option<String>,
}

/// A tab-stop in an expanded snippet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabStop {
    /// The stop number. `0` is the final caret position.
    pub index: u32,
    /// The char range of the placeholder text within the expanded string
    /// (empty range = bare caret).
    pub range: Range<usize>,
}

/// The result of expanding a snippet body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expansion {
    pub text: String,
    /// Tab-stops ordered for navigation: `1, 2, …` then `0` last.
    pub tab_stops: Vec<TabStop>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrVec {
    One(String),
    Many(Vec<String>),
}

impl StringOrVec {
    fn into_lines_joined(self) -> String {
        match self {
            StringOrVec::One(s) => s,
            StringOrVec::Many(v) => v.join("\n"),
        }
    }
    fn into_vec(self) -> Vec<String> {
        match self {
            StringOrVec::One(s) => vec![s],
            StringOrVec::Many(v) => v,
        }
    }
}

#[derive(Deserialize)]
struct RawSnippet {
    prefix: StringOrVec,
    body: StringOrVec,
    #[serde(default)]
    description: Option<String>,
}

impl Snippet {
    /// Parse a VS Code snippet JSON file into snippets (order not guaranteed).
    pub fn from_json(json: &str) -> serde_json::Result<Vec<Snippet>> {
        let raw: HashMap<String, RawSnippet> =
            serde_json::from_str(&crate::jsonc::strip_jsonc(json))?;
        Ok(raw
            .into_iter()
            .map(|(name, r)| Snippet {
                name,
                prefixes: r.prefix.into_vec(),
                body: r.body.into_lines_joined(),
                description: r.description,
            })
            .collect())
    }

    /// Expand the body into literal text + ordered tab-stops.
    pub fn expand(&self) -> Expansion {
        let mut text = String::new();
        let mut char_len = 0usize; // chars pushed so far
        let mut stops: Vec<TabStop> = Vec::new();

        let mut chars = self.body.char_indices().peekable();

        while let Some((_, c)) = chars.next() {
            if c == '\\' {
                // Escaped char: keep the next char literally.
                if let Some((_, next)) = chars.next() {
                    text.push(next);
                    char_len += 1;
                }
                continue;
            }
            if c != '$' {
                text.push(c);
                char_len += 1;
                continue;
            }
            // c == '$'
            match chars.peek().copied() {
                Some((_, '{')) => {
                    chars.next(); // consume '{'
                    // Read until matching '}'.
                    let mut inner = String::new();
                    let mut depth = 1;
                    for (_, ch) in chars.by_ref() {
                        if ch == '{' {
                            depth += 1;
                        } else if ch == '}' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        inner.push(ch);
                    }
                    apply_placeholder(&inner, &mut text, &mut char_len, &mut stops);
                }
                Some((_, d)) if d.is_ascii_digit() => {
                    // $N
                    let mut num = String::new();
                    while let Some((_, d)) = chars.peek().copied() {
                        if d.is_ascii_digit() {
                            num.push(d);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if let Ok(index) = num.parse::<u32>() {
                        stops.push(TabStop {
                            index,
                            range: char_len..char_len,
                        });
                    }
                }
                _ => {
                    // Lone '$' or $VARIABLE (unsupported variable -> empty).
                    text.push('$');
                    char_len += 1;
                }
            }
        }
        // Navigation order: positive stops ascending, then 0 (final) last.
        stops.sort_by_key(|t| if t.index == 0 { u32::MAX } else { t.index });

        Expansion {
            text,
            tab_stops: stops,
        }
    }
}

/// Handle the inside of `${...}`.
fn apply_placeholder(inner: &str, text: &mut String, char_len: &mut usize, stops: &mut Vec<TabStop>) {
    // `${N}` or `${N:default}` or `${VAR}` / `${VAR:default}`.
    let (head, default) = match inner.split_once(':') {
        Some((h, d)) => (h, Some(d)),
        None => (inner, None),
    };
    if let Ok(index) = head.trim().parse::<u32>() {
        let start = *char_len;
        if let Some(def) = default {
            text.push_str(def);
            *char_len += def.chars().count();
        }
        stops.push(TabStop {
            index,
            range: start..*char_len,
        });
    } else if let Some(def) = default {
        // A variable with a default: insert the default text.
        text.push_str(def);
        *char_len += def.chars().count();
    }
    // A bare `${VARIABLE}` we don't know expands to nothing.
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNIPPETS: &str = r#"{
        "For Loop": {
            "prefix": "for",
            "body": ["for (let ${1:i} = 0; ${1:i} < ${2:n}; ${1:i}++) {", "\t$0", "}"],
            "description": "A for loop"
        },
        "Log": {
            "prefix": ["log", "cl"],
            "body": "console.log($1)"
        }
    }"#;

    #[test]
    fn parses_prefixes_and_body() {
        let mut snips = Snippet::from_json(SNIPPETS).unwrap();
        snips.sort_by(|a, b| a.name.cmp(&b.name));
        let for_loop = snips.iter().find(|s| s.name == "For Loop").unwrap();
        assert_eq!(for_loop.prefixes, vec!["for"]);
        assert!(for_loop.body.contains("for (let"));
        let log = snips.iter().find(|s| s.name == "Log").unwrap();
        assert_eq!(log.prefixes, vec!["log", "cl"]);
    }

    #[test]
    fn expands_placeholders_and_tabstops() {
        let snips = Snippet::from_json(r#"{"s":{"prefix":"p","body":"f(${1:x}, $2)$0"}}"#).unwrap();
        let exp = snips[0].expand();
        assert_eq!(exp.text, "f(x, )");
        // Stops: 1 (covers "x"), 2 (empty), 0 (final).
        assert_eq!(exp.tab_stops.len(), 3);
        assert_eq!(exp.tab_stops[0].index, 1);
        assert_eq!(&exp.text[exp.tab_stops[0].range.clone()], "x");
        assert_eq!(exp.tab_stops[1].index, 2);
        assert_eq!(exp.tab_stops[2].index, 0); // final caret last
    }

    #[test]
    fn escaped_dollar_is_literal() {
        let snips = Snippet::from_json(r#"{"s":{"prefix":"p","body":"cost = \\$5"}}"#).unwrap();
        let exp = snips[0].expand();
        assert_eq!(exp.text, "cost = $5");
        assert!(exp.tab_stops.is_empty());
    }

    #[test]
    fn multiline_body_joined_with_newlines() {
        let snips = Snippet::from_json(SNIPPETS).unwrap();
        let for_loop = snips.iter().find(|s| s.name == "For Loop").unwrap();
        let exp = for_loop.expand();
        assert!(exp.text.contains('\n'));
        assert!(exp.text.contains("\t")); // the "\t$0" line
    }
}
