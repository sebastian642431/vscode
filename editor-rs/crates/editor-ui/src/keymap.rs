//! JSON-configurable keybindings with chords and `when`-clause contexts.
//!
//! The format mirrors VS Code's `keybindings.json` closely enough that users
//! can mostly copy theirs across:
//!
//! ```json
//! [
//!   { "key": "ctrl+shift+p", "command": "workbench.showCommandPalette" },
//!   { "key": "ctrl+k ctrl+s", "command": "workbench.openKeybindings" },
//!   { "key": "ctrl+c", "command": "editor.copy", "when": "editorFocus" }
//! ]
//! ```
//!
//! * `key` is a chord: one or more space-separated strokes, each a `+`-joined
//!   set of modifiers and one key (`ctrl+shift+p`).
//! * `when` is an optional guard over context flags, supporting `!`, `&&`, `||`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// One keybinding as it appears in the JSON file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyBinding {
    pub key: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
}

/// A single physical key press: modifiers plus one main key, normalised to
/// lower case.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct KeyStroke {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// The platform "command"/"super"/"meta" key.
    pub cmd: bool,
    pub key: String,
}

impl KeyStroke {
    /// Parse a single stroke like `"ctrl+shift+p"`. Unknown modifiers are
    /// treated as the main key. Returns `None` if there is no main key.
    pub fn parse(s: &str) -> Option<KeyStroke> {
        let mut stroke = KeyStroke {
            ctrl: false,
            shift: false,
            alt: false,
            cmd: false,
            key: String::new(),
        };
        for part in s.split('+') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => stroke.ctrl = true,
                "shift" => stroke.shift = true,
                "alt" | "option" => stroke.alt = true,
                "cmd" | "super" | "meta" | "win" => stroke.cmd = true,
                other => stroke.key = other.to_string(),
            }
        }
        if stroke.key.is_empty() {
            None
        } else {
            Some(stroke)
        }
    }

    /// Parse a full chord (`"ctrl+k ctrl+s"`) into its strokes.
    pub fn parse_chord(s: &str) -> Option<Vec<KeyStroke>> {
        let strokes: Vec<KeyStroke> = s.split_whitespace().filter_map(KeyStroke::parse).collect();
        if strokes.is_empty() {
            None
        } else {
            Some(strokes)
        }
    }
}

/// The set of context flags that are currently true (e.g. `editorFocus`).
#[derive(Clone, Debug, Default)]
pub struct Context {
    flags: HashSet<String>,
}

impl Context {
    pub fn new() -> Self {
        Context::default()
    }

    pub fn with(mut self, flag: impl Into<String>) -> Self {
        self.flags.insert(flag.into());
        self
    }

    pub fn set(&mut self, flag: impl Into<String>, on: bool) {
        let flag = flag.into();
        if on {
            self.flags.insert(flag);
        } else {
            self.flags.remove(&flag);
        }
    }

    fn is_set(&self, flag: &str) -> bool {
        self.flags.contains(flag)
    }

    /// Evaluate a `when` expression (`!`, `&&`, `||`, identifiers). `||` binds
    /// looser than `&&`. Unparsable expressions evaluate to `false`.
    fn eval(&self, expr: &str) -> bool {
        expr.split("||").any(|or_term| {
            or_term.split("&&").all(|and_term| {
                let t = and_term.trim();
                if let Some(rest) = t.strip_prefix('!') {
                    !self.is_set(rest.trim())
                } else if t.is_empty() {
                    false
                } else {
                    self.is_set(t)
                }
            })
        })
    }
}

/// The outcome of feeding a stroke sequence to the keymap.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Resolution {
    /// No binding matches this sequence; the UI should treat the strokes as
    /// literal input (e.g. typing a character).
    None,
    /// The sequence is a prefix of a longer chord; the UI should wait for more.
    Pending,
    /// A command matched.
    Matched(String),
}

#[derive(Clone, Debug)]
struct ParsedBinding {
    chord: Vec<KeyStroke>,
    command: String,
    when: Option<String>,
}

/// A resolved set of keybindings.
#[derive(Clone, Debug, Default)]
pub struct Keymap {
    bindings: Vec<ParsedBinding>,
}

impl Keymap {
    /// Build from parsed [`KeyBinding`]s, skipping any with an unparsable key.
    pub fn from_bindings(bindings: &[KeyBinding]) -> Self {
        let bindings = bindings
            .iter()
            .filter_map(|b| {
                KeyStroke::parse_chord(&b.key).map(|chord| ParsedBinding {
                    chord,
                    command: b.command.clone(),
                    when: b.when.clone(),
                })
            })
            .collect();
        Keymap { bindings }
    }

    /// Parse bindings from a JSON array.
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        let bindings: Vec<KeyBinding> = serde_json::from_str(json)?;
        Ok(Keymap::from_bindings(&bindings))
    }

    /// Resolve a sequence of strokes typed so far against `context`.
    ///
    /// Later bindings win over earlier ones for the same chord (so a user
    /// keymap appended after the defaults overrides them), matching VS Code.
    pub fn resolve(&self, strokes: &[KeyStroke], context: &Context) -> Resolution {
        if strokes.is_empty() {
            return Resolution::None;
        }
        let mut matched: Option<&str> = None;
        let mut pending = false;

        for binding in &self.bindings {
            if let Some(when) = &binding.when {
                if !context.eval(when) {
                    continue;
                }
            }
            if binding.chord == strokes {
                matched = Some(&binding.command); // last match wins
            } else if binding.chord.len() > strokes.len()
                && binding.chord.starts_with(strokes)
            {
                pending = true;
            }
        }

        match matched {
            Some(cmd) => Resolution::Matched(cmd.to_string()),
            None if pending => Resolution::Pending,
            None => Resolution::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(s: &str) -> KeyStroke {
        KeyStroke::parse(s).unwrap()
    }

    #[test]
    fn parses_modifiers_and_key() {
        let s = stroke("ctrl+shift+p");
        assert!(s.ctrl && s.shift && !s.alt && !s.cmd);
        assert_eq!(s.key, "p");
    }

    #[test]
    fn parses_chord() {
        let chord = KeyStroke::parse_chord("ctrl+k ctrl+s").unwrap();
        assert_eq!(chord.len(), 2);
        assert_eq!(chord[1].key, "s");
    }

    #[test]
    fn resolves_simple_binding() {
        let km = Keymap::from_json(
            r#"[{"key":"ctrl+shift+p","command":"showPalette"}]"#,
        )
        .unwrap();
        let r = km.resolve(&[stroke("ctrl+shift+p")], &Context::new());
        assert_eq!(r, Resolution::Matched("showPalette".into()));
    }

    #[test]
    fn chord_reports_pending_then_matched() {
        let km = Keymap::from_json(r#"[{"key":"ctrl+k ctrl+s","command":"openKeys"}]"#).unwrap();
        let ctx = Context::new();
        assert_eq!(km.resolve(&[stroke("ctrl+k")], &ctx), Resolution::Pending);
        assert_eq!(
            km.resolve(&[stroke("ctrl+k"), stroke("ctrl+s")], &ctx),
            Resolution::Matched("openKeys".into())
        );
    }

    #[test]
    fn when_clause_gates_binding() {
        let km = Keymap::from_json(
            r#"[{"key":"ctrl+c","command":"copy","when":"editorFocus && !readonly"}]"#,
        )
        .unwrap();
        let focused = Context::new().with("editorFocus");
        assert_eq!(
            km.resolve(&[stroke("ctrl+c")], &focused),
            Resolution::Matched("copy".into())
        );
        let readonly = Context::new().with("editorFocus").with("readonly");
        assert_eq!(km.resolve(&[stroke("ctrl+c")], &readonly), Resolution::None);
        assert_eq!(km.resolve(&[stroke("ctrl+c")], &Context::new()), Resolution::None);
    }

    #[test]
    fn later_binding_overrides_earlier() {
        let km = Keymap::from_json(
            r#"[{"key":"ctrl+b","command":"old"},{"key":"ctrl+b","command":"new"}]"#,
        )
        .unwrap();
        assert_eq!(
            km.resolve(&[stroke("ctrl+b")], &Context::new()),
            Resolution::Matched("new".into())
        );
    }

    #[test]
    fn or_clause() {
        let km =
            Keymap::from_json(r#"[{"key":"escape","command":"close","when":"a || b"}]"#).unwrap();
        assert_eq!(
            km.resolve(&[stroke("escape")], &Context::new().with("b")),
            Resolution::Matched("close".into())
        );
    }
}
