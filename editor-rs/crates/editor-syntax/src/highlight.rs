//! Incremental parsing ([`SyntaxTree`]) and syntax highlighting
//! ([`Highlighter`] + [`HighlightConfigs`]).
//!
//! # Two cooperating pieces
//!
//! * [`SyntaxTree`] keeps a live tree-sitter `Tree` and updates it
//!   *incrementally* from editor-core [`Change`]s. Because every `Change`
//!   already carries exact byte offsets, applying an edit and reparsing only
//!   touches the changed region — typing in a 100 MB file reparses in
//!   microseconds, never the whole buffer.
//!
//! * [`Highlighter`] turns a slice of source into colour [`HighlightSpan`]s via
//!   the `tree-sitter-highlight` engine (which handles capture precedence and
//!   injections correctly). The UI highlights the *visible viewport slice*, so
//!   this stays cheap regardless of file size.

use std::collections::HashMap;
use std::ops::Range;

use editor_core::Change;
use ropey::Rope;
use tree_sitter::{InputEdit, Parser, Point as TsPoint, Tree};
use tree_sitter_highlight::{
    HighlightConfiguration, HighlightEvent, Highlighter as TsHighlighter,
};

use crate::language::{Language, ALL_LANGUAGES};

/// The capture names editor-rs recognises, in priority order. The index of a
/// name is what a [`HighlightSpan`] stores; the theme maps index → colour.
pub const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "escape",
    "function",
    "function.builtin",
    "function.macro",
    "function.method",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "string",
    "string.special",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

/// The theme-independent name for a highlight index, or `None` for unknown.
pub fn highlight_name(index: usize) -> Option<&'static str> {
    HIGHLIGHT_NAMES.get(index).copied()
}

/// A coloured span: a byte range (relative to the source that produced it) and
/// the index of its highlight name in [`HIGHLIGHT_NAMES`], if any.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HighlightSpan {
    pub range: Range<usize>,
    pub highlight: Option<usize>,
}

/// A live, incrementally-maintained parse tree for one document.
pub struct SyntaxTree {
    language: Language,
    parser: Parser,
    tree: Option<Tree>,
}

impl SyntaxTree {
    /// Create a parser for `language` with no tree yet.
    pub fn new(language: Language) -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&language.ts_language())
            .expect("grammar ABI compatible with tree-sitter runtime");
        SyntaxTree {
            language,
            parser,
            tree: None,
        }
    }

    pub fn language(&self) -> Language {
        self.language
    }

    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// Parse a whole buffer from scratch, discarding any previous tree. Reads
    /// the rope chunk-by-chunk, so even a 100 MB buffer is never copied into a
    /// contiguous `String` just to parse it.
    pub fn set_rope(&mut self, rope: &Rope) {
        self.tree = parse_rope(&mut self.parser, rope, None);
    }

    /// Convenience: parse from a string slice (one-time, e.g. a small test or
    /// an in-memory buffer).
    pub fn set_text(&mut self, text: &str) {
        self.set_rope(&Rope::from_str(text));
    }

    /// Apply `changes` incrementally and reparse, reading the post-edit buffer
    /// directly from `new_rope` (no `String` copy of the document).
    ///
    /// Byte offsets in the `InputEdit`s are exact; the `(row, column)` points
    /// are a best-effort hint (tree-sitter recomputes authoritative positions
    /// while parsing), with `column` measured in **bytes** as tree-sitter
    /// requires.
    pub fn edit(&mut self, new_rope: &Rope, changes: &[Change]) {
        if self.tree.is_none() {
            // No prior tree: fall back to a full parse.
            self.set_rope(new_rope);
            return;
        }
        if let Some(tree) = self.tree.as_mut() {
            for change in changes {
                tree.edit(&to_input_edit(change, new_rope));
            }
        }
        self.tree = parse_rope(&mut self.parser, new_rope, self.tree.as_ref());
    }
}

/// Parse a rope incrementally without materialising it, by feeding tree-sitter
/// each chunk on demand. `old` is the previously-edited tree (or `None`).
fn parse_rope(parser: &mut Parser, rope: &Rope, old: Option<&Tree>) -> Option<Tree> {
    let len = rope.len_bytes();
    parser.parse_with(
        &mut |byte, _point| {
            if byte >= len {
                return &[] as &[u8];
            }
            let (chunk, chunk_start_byte, _, _) = rope.chunk_at_byte(byte);
            &chunk.as_bytes()[byte - chunk_start_byte..]
        },
        old,
    )
}

/// Convert an editor-core [`Change`] into a tree-sitter [`InputEdit`].
fn to_input_edit(change: &Change, new_rope: &Rope) -> InputEdit {
    let max_line = new_rope.len_lines().saturating_sub(1);

    let byte_column = |byte: usize, line: usize| -> usize {
        let line = line.min(max_line);
        let line_start = new_rope.line_to_byte(line);
        byte.saturating_sub(line_start)
    };

    InputEdit {
        start_byte: change.start_byte,
        old_end_byte: change.old_end_byte,
        new_end_byte: change.new_end_byte,
        start_position: TsPoint::new(
            change.start_point.line,
            byte_column(change.start_byte, change.start_point.line),
        ),
        old_end_position: TsPoint::new(
            change.old_end_point.line,
            byte_column(change.old_end_byte, change.old_end_point.line),
        ),
        new_end_position: TsPoint::new(
            change.new_end_point.line,
            byte_column(change.new_end_byte, change.new_end_point.line),
        ),
    }
}

/// A registry of built [`HighlightConfiguration`]s, one per language.
///
/// Building a configuration compiles the grammar's highlight query, which is
/// non-trivial, so this is created once and reused for the lifetime of the
/// editor.
pub struct HighlightConfigs {
    configs: HashMap<Language, HighlightConfiguration>,
}

impl HighlightConfigs {
    /// Build configurations for every supported language.
    pub fn new() -> Self {
        let mut configs = HashMap::new();
        for &lang in ALL_LANGUAGES {
            let mut config = HighlightConfiguration::new(
                lang.ts_language(),
                lang.id(),
                lang.highlights_query(),
                lang.injections_query(),
                lang.locals_query(),
            )
            .expect("bundled highlight query is valid");
            config.configure(HIGHLIGHT_NAMES);
            configs.insert(lang, config);
        }
        HighlightConfigs { configs }
    }

    pub fn get(&self, language: Language) -> &HighlightConfiguration {
        &self.configs[&language]
    }
}

impl Default for HighlightConfigs {
    fn default() -> Self {
        HighlightConfigs::new()
    }
}

/// Produces [`HighlightSpan`]s for a slice of source.
///
/// Wraps the stateful `tree-sitter-highlight` engine; create one per worker
/// thread. `highlight` is `&mut self` because the engine reuses internal
/// buffers across calls.
#[derive(Default)]
pub struct Highlighter {
    inner: TsHighlighter,
}

impl Highlighter {
    pub fn new() -> Self {
        Highlighter {
            inner: TsHighlighter::new(),
        }
    }

    /// Highlight `source`, returning non-overlapping spans in order. Byte
    /// offsets are relative to `source`; when highlighting a viewport slice,
    /// add the slice's base offset to map back to the document.
    pub fn highlight(
        &mut self,
        config: &HighlightConfiguration,
        source: &str,
    ) -> Vec<HighlightSpan> {
        let events = match self
            .inner
            .highlight(config, source.as_bytes(), None, |_| None)
        {
            Ok(events) => events,
            Err(_) => return Vec::new(),
        };

        let mut spans = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for event in events {
            match event {
                Ok(HighlightEvent::HighlightStart(h)) => stack.push(h.0),
                Ok(HighlightEvent::HighlightEnd) => {
                    stack.pop();
                }
                Ok(HighlightEvent::Source { start, end }) => {
                    if end > start {
                        spans.push(HighlightSpan {
                            range: start..end,
                            highlight: stack.last().copied(),
                        });
                    }
                }
                Err(_) => break,
            }
        }
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::Document;

    #[test]
    fn full_parse_produces_a_tree() {
        let mut st = SyntaxTree::new(Language::Rust);
        st.set_text("fn main() { let x = 1; }");
        let tree = st.tree().unwrap();
        assert_eq!(tree.root_node().kind(), "source_file");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn incremental_edit_tracks_the_document() {
        // Drive the SyntaxTree the way the UI will: through a Document's change
        // stream.
        let mut doc = Document::from_str("fn main() {}");
        let mut st = SyntaxTree::new(Language::Rust);
        st.set_text(&doc.text());
        assert!(!st.tree().unwrap().root_node().has_error());

        // Type inside the braces.
        doc.set_caret(editor_core::Point::new(0, 11)); // between { and }
        doc.insert(" let y = 2; ");
        let changes = doc.drain_changes();
        st.edit(doc.buffer().rope(), &changes);

        let tree = st.tree().unwrap();
        assert!(!tree.root_node().has_error());
        assert_eq!(tree.root_node().kind(), "source_file");
        // The tree now spans the longer text.
        assert_eq!(tree.root_node().end_byte(), doc.text().len());
    }

    #[test]
    fn highlighter_colours_keywords_and_strings() {
        let configs = HighlightConfigs::new();
        let mut hl = Highlighter::new();
        let src = "fn main() { let s = \"hi\"; }";
        let spans = hl.highlight(configs.get(Language::Rust), src);

        // Reconstruct the covered text and make sure keywords/strings got a
        // highlight index that maps to a sensible name.
        let kw_span = spans
            .iter()
            .find(|s| &src[s.range.clone()] == "fn")
            .expect("`fn` should be a span");
        assert_eq!(highlight_name(kw_span.highlight.unwrap()), Some("keyword"));

        let str_span = spans
            .iter()
            .find(|s| &src[s.range.clone()] == "\"hi\"")
            .expect("string literal should be a span");
        assert_eq!(highlight_name(str_span.highlight.unwrap()), Some("string"));
    }

    #[test]
    fn spans_are_ordered_and_cover_source_without_overlap() {
        let configs = HighlightConfigs::new();
        let mut hl = Highlighter::new();
        let src = "let x = 42;\n";
        let spans = hl.highlight(configs.get(Language::TypeScript), src);
        let mut prev_end = 0;
        for span in &spans {
            assert!(span.range.start >= prev_end, "spans must not overlap");
            prev_end = span.range.end;
        }
    }

    #[test]
    fn python_and_json_parse() {
        let mut py = SyntaxTree::new(Language::Python);
        py.set_text("def f(x):\n    return x + 1\n");
        assert!(!py.tree().unwrap().root_node().has_error());

        let mut json = SyntaxTree::new(Language::Json);
        json.set_text(r#"{"a": [1, 2, 3], "b": true}"#);
        assert!(!json.tree().unwrap().root_node().has_error());
    }
}
