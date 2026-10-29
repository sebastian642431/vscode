//! The set of languages editor-rs can parse, and how to reach their grammars.

use std::path::Path;

use tree_sitter::Language as TsLanguage;

/// A language editor-rs has a tree-sitter grammar for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Language {
    Rust,
    TypeScript,
    /// TypeScript with JSX.
    Tsx,
    Json,
    Python,
}

/// Every language, for building the highlight-config registry.
pub const ALL_LANGUAGES: &[Language] = &[
    Language::Rust,
    Language::TypeScript,
    Language::Tsx,
    Language::Json,
    Language::Python,
];

impl Language {
    /// Detect the language of a file from its extension.
    pub fn from_path(path: impl AsRef<Path>) -> Option<Language> {
        let ext = path.as_ref().extension()?.to_str()?;
        Language::from_extension(ext)
    }

    /// Detect a language from a bare file extension (without the dot).
    pub fn from_extension(ext: &str) -> Option<Language> {
        match ext {
            "rs" => Some(Language::Rust),
            "ts" | "mts" | "cts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            "json" | "jsonc" => Some(Language::Json),
            "py" | "pyi" => Some(Language::Python),
            _ => None,
        }
    }

    /// The LSP-style identifier for this language.
    pub fn id(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::TypeScript => "typescript",
            Language::Tsx => "typescriptreact",
            Language::Json => "json",
            Language::Python => "python",
        }
    }

    /// The tree-sitter grammar.
    pub fn ts_language(self) -> TsLanguage {
        match self {
            Language::Rust => TsLanguage::new(tree_sitter_rust::LANGUAGE),
            Language::TypeScript => TsLanguage::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
            Language::Tsx => TsLanguage::new(tree_sitter_typescript::LANGUAGE_TSX),
            Language::Json => TsLanguage::new(tree_sitter_json::LANGUAGE),
            Language::Python => TsLanguage::new(tree_sitter_python::LANGUAGE),
        }
    }

    /// The grammar's highlight query.
    pub fn highlights_query(self) -> &'static str {
        match self {
            Language::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
            Language::TypeScript | Language::Tsx => tree_sitter_typescript::HIGHLIGHTS_QUERY,
            Language::Json => tree_sitter_json::HIGHLIGHTS_QUERY,
            Language::Python => tree_sitter_python::HIGHLIGHTS_QUERY,
        }
    }

    /// The grammar's injection query (empty if none).
    pub fn injections_query(self) -> &'static str {
        match self {
            Language::Rust => tree_sitter_rust::INJECTIONS_QUERY,
            _ => "",
        }
    }

    /// The grammar's locals query (empty if none).
    pub fn locals_query(self) -> &'static str {
        match self {
            Language::TypeScript | Language::Tsx => tree_sitter_typescript::LOCALS_QUERY,
            _ => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_from_extension() {
        assert_eq!(Language::from_path("src/main.rs"), Some(Language::Rust));
        assert_eq!(Language::from_path("app.ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_path("ui.tsx"), Some(Language::Tsx));
        assert_eq!(Language::from_path("pkg.json"), Some(Language::Json));
        assert_eq!(Language::from_path("run.py"), Some(Language::Python));
        assert_eq!(Language::from_path("notes.txt"), None);
        assert_eq!(Language::from_path("README"), None);
    }

    #[test]
    fn every_grammar_loads() {
        // Constructing the tree-sitter Language validates the grammar's ABI.
        for lang in ALL_LANGUAGES {
            let ts = lang.ts_language();
            assert!(ts.version() >= 13, "{lang:?} grammar ABI too old");
        }
    }
}
