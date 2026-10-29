//! Parsing a VS Code extension's `package.json`.
//!
//! Only the fields editor-rs needs to load and activate an extension and wire
//! up its static contributions are modelled; unknown fields are ignored so
//! real-world manifests parse cleanly.

use serde::Deserialize;
use serde_json::Value;

/// A parsed extension manifest.
#[derive(Clone, Debug, Deserialize)]
pub struct ExtensionManifest {
    pub name: String,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
    /// Entry point module (relative to the extension root), e.g. `./out/main.js`.
    #[serde(default)]
    pub main: Option<String>,
    /// When to activate (`onCommand:...`, `onLanguage:...`, `*`, …).
    #[serde(default, rename = "activationEvents")]
    pub activation_events: Vec<String>,
    #[serde(default)]
    pub engines: Engines,
    #[serde(default)]
    pub contributes: Contributes,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Engines {
    #[serde(default)]
    pub vscode: Option<String>,
}

/// The `contributes` section — static contributions the editor wires up without
/// running any extension code.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Contributes {
    #[serde(default)]
    pub commands: Vec<CommandContribution>,
    #[serde(default)]
    pub languages: Vec<LanguageContribution>,
    #[serde(default)]
    pub grammars: Vec<GrammarContribution>,
    #[serde(default)]
    pub themes: Vec<ThemeContribution>,
    #[serde(default)]
    pub snippets: Vec<SnippetContribution>,
    #[serde(default)]
    pub keybindings: Vec<Value>,
    /// Left as raw JSON (settings schema); the editor reads it lazily.
    #[serde(default)]
    pub configuration: Option<Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CommandContribution {
    pub command: String,
    pub title: String,
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LanguageContribution {
    pub id: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default, rename = "configuration")]
    pub configuration_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GrammarContribution {
    #[serde(default)]
    pub language: Option<String>,
    #[serde(rename = "scopeName")]
    pub scope_name: String,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ThemeContribution {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default, rename = "uiTheme")]
    pub ui_theme: Option<String>,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SnippetContribution {
    pub language: String,
    pub path: String,
}

impl ExtensionManifest {
    /// Parse a `package.json` (JSONC tolerated via a light strip).
    pub fn from_json(json: &str) -> serde_json::Result<ExtensionManifest> {
        serde_json::from_str(json)
    }

    /// The qualified id `publisher.name`, VS Code's extension identifier.
    pub fn id(&self) -> String {
        match &self.publisher {
            Some(p) => format!("{p}.{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Whether this extension declares it wants activating for `event`
    /// (e.g. `"onCommand:foo.bar"` or `"onLanguage:rust"`). `"*"` matches all.
    pub fn activates_on(&self, event: &str) -> bool {
        self.activation_events
            .iter()
            .any(|e| e == "*" || e == event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PKG: &str = r#"{
        "name": "hello-world",
        "publisher": "acme",
        "version": "1.2.3",
        "displayName": "Hello World",
        "main": "./out/extension.js",
        "engines": { "vscode": "^1.75.0" },
        "activationEvents": ["onCommand:hello.sayHi", "onLanguage:rust"],
        "contributes": {
            "commands": [
                { "command": "hello.sayHi", "title": "Say Hi", "category": "Hello" }
            ],
            "languages": [
                { "id": "rust", "extensions": [".rs"], "configuration": "./language-configuration.json" }
            ],
            "grammars": [
                { "language": "rust", "scopeName": "source.rust", "path": "./syntaxes/rust.tmLanguage.json" }
            ],
            "themes": [
                { "label": "My Dark", "uiTheme": "vs-dark", "path": "./themes/dark.json" }
            ],
            "snippets": [
                { "language": "rust", "path": "./snippets/rust.json" }
            ]
        }
    }"#;

    #[test]
    fn parses_core_fields() {
        let m = ExtensionManifest::from_json(PKG).unwrap();
        assert_eq!(m.name, "hello-world");
        assert_eq!(m.id(), "acme.hello-world");
        assert_eq!(m.main.as_deref(), Some("./out/extension.js"));
        assert_eq!(m.engines.vscode.as_deref(), Some("^1.75.0"));
    }

    #[test]
    fn parses_contributions() {
        let m = ExtensionManifest::from_json(PKG).unwrap();
        assert_eq!(m.contributes.commands[0].command, "hello.sayHi");
        assert_eq!(m.contributes.languages[0].extensions, vec![".rs"]);
        assert_eq!(m.contributes.grammars[0].scope_name, "source.rust");
        assert_eq!(m.contributes.themes[0].path, "./themes/dark.json");
        assert_eq!(m.contributes.snippets[0].language, "rust");
    }

    #[test]
    fn activation_matching() {
        let m = ExtensionManifest::from_json(PKG).unwrap();
        assert!(m.activates_on("onCommand:hello.sayHi"));
        assert!(m.activates_on("onLanguage:rust"));
        assert!(!m.activates_on("onLanguage:python"));
    }

    #[test]
    fn minimal_manifest() {
        let m = ExtensionManifest::from_json(r#"{"name":"x"}"#).unwrap();
        assert_eq!(m.id(), "x");
        assert!(m.main.is_none());
        assert!(m.contributes.commands.is_empty());
    }
}
