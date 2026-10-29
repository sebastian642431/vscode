//! The command registry and the fuzzy matcher behind the command palette.

/// A command the user can run from the palette or bind a key to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Command {
    /// Stable identifier used by keybindings (e.g. `"editor.action.copy"`).
    pub id: String,
    /// Human-readable title shown in the palette.
    pub title: String,
    /// Optional grouping prefix shown as `Category: Title`.
    pub category: Option<String>,
}

impl Command {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Command {
            id: id.into(),
            title: title.into(),
            category: None,
        }
    }

    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    /// The text shown and searched in the palette (`Category: Title`).
    pub fn display(&self) -> String {
        match &self.category {
            Some(cat) => format!("{cat}: {}", self.title),
            None => self.title.clone(),
        }
    }
}

/// A palette search hit: the matched command plus its score and the byte
/// positions in [`Command::display`] that matched (for highlighting).
#[derive(Clone, Debug)]
pub struct Match<'a> {
    pub command: &'a Command,
    pub score: i32,
    pub positions: Vec<usize>,
}

/// A registry of available commands.
#[derive(Clone, Debug, Default)]
pub struct CommandRegistry {
    commands: Vec<Command>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        CommandRegistry::default()
    }

    pub fn register(&mut self, command: Command) {
        self.commands.push(command);
    }

    pub fn all(&self) -> &[Command] {
        &self.commands
    }

    pub fn find(&self, id: &str) -> Option<&Command> {
        self.commands.iter().find(|c| c.id == id)
    }

    /// Fuzzy-search the palette. An empty query returns every command in
    /// registration order; otherwise results are sorted best-first.
    pub fn search(&self, query: &str) -> Vec<Match<'_>> {
        if query.trim().is_empty() {
            return self
                .commands
                .iter()
                .map(|c| Match {
                    command: c,
                    score: 0,
                    positions: Vec::new(),
                })
                .collect();
        }
        let mut hits: Vec<Match<'_>> = self
            .commands
            .iter()
            .filter_map(|c| {
                fuzzy_match(&c.display(), query).map(|(score, positions)| Match {
                    command: c,
                    score,
                    positions,
                })
            })
            .collect();
        // Highest score first; ties broken by shorter display (more precise).
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.command.display().len().cmp(&b.command.display().len()))
        });
        hits
    }
}

/// A small case-insensitive subsequence fuzzy matcher. Returns `None` if
/// `query` is not a subsequence of `text`; otherwise a score (higher is
/// better, rewarding contiguous runs and word-start matches) and the matched
/// byte positions in `text`.
pub fn fuzzy_match(text: &str, query: &str) -> Option<(i32, Vec<usize>)> {
    let text_lower = text.to_lowercase();
    let query_lower = query.to_lowercase();
    let query_chars: Vec<char> = query_lower.chars().filter(|c| !c.is_whitespace()).collect();
    if query_chars.is_empty() {
        return Some((0, Vec::new()));
    }

    let mut positions = Vec::with_capacity(query_chars.len());
    let mut score = 0i32;
    let mut qi = 0usize;
    let mut last_match: Option<usize> = None;
    let mut prev_char: Option<char> = None;

    for (byte_idx, ch) in text_lower.char_indices() {
        if qi >= query_chars.len() {
            break;
        }
        if ch == query_chars[qi] {
            score += 1;
            // Bonus for a contiguous match.
            if last_match == Some(byte_idx.saturating_sub(ch.len_utf8())) {
                score += 3;
            }
            // Bonus for matching at a word boundary.
            let at_boundary = prev_char
                .map(|p| p == ' ' || p == ':' || p == '.' || p == '_' || p == '-')
                .unwrap_or(true);
            if at_boundary {
                score += 2;
            }
            positions.push(byte_idx);
            last_match = Some(byte_idx);
            qi += 1;
        }
        prev_char = Some(ch);
    }

    if qi == query_chars.len() {
        // Prefer matches that start earlier.
        if let Some(first) = positions.first() {
            score -= (*first as i32) / 8;
        }
        Some((score, positions))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        r.register(Command::new("file.save", "Save").with_category("File"));
        r.register(Command::new("file.saveAll", "Save All").with_category("File"));
        r.register(Command::new("editor.format", "Format Document").with_category("Editor"));
        r.register(Command::new("workbench.togglePanel", "Toggle Panel"));
        r
    }

    #[test]
    fn empty_query_returns_all() {
        let r = registry();
        assert_eq!(r.search("").len(), 4);
    }

    #[test]
    fn finds_by_subsequence() {
        let r = registry();
        let hits = r.search("save");
        assert!(hits
            .iter()
            .any(|m| m.command.id == "file.save"));
        assert!(hits.iter().all(|m| m.command.display().to_lowercase().contains('s')));
    }

    #[test]
    fn ranks_better_match_first() {
        let r = registry();
        let hits = r.search("fsave");
        // "File: Save" should outrank "File: Save All" for "fsave".
        assert_eq!(hits[0].command.id, "file.save");
    }

    #[test]
    fn no_match_returns_empty() {
        let r = registry();
        assert!(r.search("zzzz").is_empty());
    }

    #[test]
    fn find_by_id() {
        let r = registry();
        assert_eq!(r.find("editor.format").unwrap().title, "Format Document");
        assert!(r.find("nope").is_none());
    }

    #[test]
    fn positions_point_at_matched_chars() {
        let (_, positions) = fuzzy_match("Format Document", "fd").unwrap();
        assert_eq!(positions.len(), 2);
        // 'F' at 0, 'D' at 7.
        assert_eq!(positions[0], 0);
        assert_eq!(&"Format Document"[positions[1]..positions[1] + 1], "D");
    }
}
