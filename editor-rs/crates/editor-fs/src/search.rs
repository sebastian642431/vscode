//! Project-wide search built on the ripgrep crates.
//!
//! The [`ignore`] parallel walker fans file traversal across every core while
//! respecting `.gitignore`; [`grep_searcher`] does the fast line scanning and
//! [`grep_regex`] the matching. Results stream through a [`crossbeam_channel`]
//! so the UI can show hits as they arrive and cancel mid-flight simply by
//! dropping the receiver.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher as GrepSearcher;
use ignore::overrides::OverrideBuilder;
use ignore::{WalkBuilder, WalkState};

/// A single matching line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SearchMatch {
    pub path: PathBuf,
    /// 1-based line number, as ripgrep reports.
    pub line_number: u64,
    /// The matching line, with any trailing newline trimmed.
    pub line: String,
    /// Byte ranges of the matches within [`line`](SearchMatch::line).
    pub submatches: Vec<Range<usize>>,
}

/// How a search should behave.
#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub query: String,
    /// Treat `query` as a regular expression (otherwise it's a literal).
    pub is_regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub respect_gitignore: bool,
    /// Only search files matching these globs (empty = all files).
    pub include_globs: Vec<String>,
    /// Skip files matching these globs.
    pub exclude_globs: Vec<String>,
    /// Stop after this many matches (across all files).
    pub max_results: Option<usize>,
}

impl SearchOptions {
    /// A literal, case-insensitive search for `query`.
    pub fn literal(query: impl Into<String>) -> Self {
        SearchOptions {
            query: query.into(),
            is_regex: false,
            case_sensitive: false,
            whole_word: false,
            respect_gitignore: true,
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            max_results: None,
        }
    }

    pub fn regex(mut self, yes: bool) -> Self {
        self.is_regex = yes;
        self
    }

    pub fn case_sensitive(mut self, yes: bool) -> Self {
        self.case_sensitive = yes;
        self
    }

    pub fn whole_word(mut self, yes: bool) -> Self {
        self.whole_word = yes;
        self
    }

    pub fn max_results(mut self, max: usize) -> Self {
        self.max_results = Some(max);
        self
    }
}

/// Errors a search can fail with before it starts streaming.
#[derive(Debug)]
pub enum SearchError {
    /// The query (or a glob) was not a valid pattern.
    Pattern(String),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::Pattern(msg) => write!(f, "invalid search pattern: {msg}"),
        }
    }
}

impl std::error::Error for SearchError {}

/// Escape a literal string so it can be fed to the regex engine verbatim.
fn escape_literal(query: &str) -> String {
    let mut out = String::with_capacity(query.len() * 2);
    for c in query.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A configured project searcher.
pub struct Searcher {
    root: PathBuf,
    opts: SearchOptions,
}

impl Searcher {
    pub fn new(root: impl Into<PathBuf>, opts: SearchOptions) -> Self {
        Searcher {
            root: root.into(),
            opts,
        }
    }

    fn build_matcher(&self) -> Result<RegexMatcher, SearchError> {
        let pattern = if self.opts.is_regex {
            self.opts.query.clone()
        } else {
            escape_literal(&self.opts.query)
        };
        RegexMatcherBuilder::new()
            .case_insensitive(!self.opts.case_sensitive)
            .word(self.opts.whole_word)
            .build(&pattern)
            .map_err(|e| SearchError::Pattern(e.to_string()))
    }

    fn build_walker(&self) -> Result<WalkBuilder, SearchError> {
        let mut overrides = OverrideBuilder::new(&self.root);
        for glob in &self.opts.include_globs {
            overrides
                .add(glob)
                .map_err(|e| SearchError::Pattern(e.to_string()))?;
        }
        for glob in &self.opts.exclude_globs {
            overrides
                .add(&format!("!{glob}"))
                .map_err(|e| SearchError::Pattern(e.to_string()))?;
        }
        let overrides = overrides
            .build()
            .map_err(|e| SearchError::Pattern(e.to_string()))?;

        let mut builder = WalkBuilder::new(&self.root);
        builder
            .overrides(overrides)
            .git_ignore(self.opts.respect_gitignore)
            .git_global(self.opts.respect_gitignore)
            .git_exclude(self.opts.respect_gitignore)
            .require_git(false)
            .hidden(true);
        Ok(builder)
    }

    /// Run the search, streaming each [`SearchMatch`] into `tx`.
    ///
    /// Returns the total number of matches sent. Dropping the corresponding
    /// receiver cancels the search: sends start failing and every worker quits.
    /// This is a blocking call meant for a worker thread.
    pub fn stream(&self, tx: Sender<SearchMatch>) -> Result<usize, SearchError> {
        let matcher = self.build_matcher()?;
        let walker = self.build_walker()?.build_parallel();
        let count = Arc::new(AtomicUsize::new(0));
        let max = self.opts.max_results;

        walker.run(|| {
            let matcher = matcher.clone();
            let tx = tx.clone();
            let count = Arc::clone(&count);
            Box::new(move |result| {
                let entry = match result {
                    Ok(e) => e,
                    Err(_) => return WalkState::Continue,
                };
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    return WalkState::Continue;
                }
                if let Some(max) = max {
                    if count.load(Ordering::Relaxed) >= max {
                        return WalkState::Quit;
                    }
                }

                let path = entry.path().to_path_buf();
                let mut quit = false;
                let mut searcher = GrepSearcher::new();
                let search_result = searcher.search_path(
                    &matcher,
                    entry.path(),
                    UTF8(|line_number, line| {
                        if let Some(max) = max {
                            if count.load(Ordering::Relaxed) >= max {
                                quit = true;
                                return Ok(false);
                            }
                        }
                        let trimmed = line.strip_suffix('\n').unwrap_or(line);
                        let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);

                        let mut submatches = Vec::new();
                        let _ = matcher.find_iter(trimmed.as_bytes(), |m| {
                            submatches.push(m.start()..m.end());
                            true
                        });

                        let m = SearchMatch {
                            path: path.clone(),
                            line_number,
                            line: trimmed.to_string(),
                            submatches,
                        };
                        if tx.send(m).is_err() {
                            quit = true;
                            return Ok(false);
                        }
                        count.fetch_add(1, Ordering::Relaxed);
                        Ok(true)
                    }),
                );
                // A binary file or read error just means "no matches here".
                let _ = search_result;
                if quit {
                    WalkState::Quit
                } else {
                    WalkState::Continue
                }
            })
        });

        Ok(count.load(Ordering::Relaxed))
    }

    /// Convenience wrapper that collects every match into a `Vec` (sorted by
    /// path then line). Primarily for tests and small searches.
    pub fn collect(&self) -> Result<Vec<SearchMatch>, SearchError> {
        let (tx, rx): (Sender<SearchMatch>, Receiver<SearchMatch>) = unbounded();
        // The walker holds clones of `tx`; drop our handle so `rx` closes once
        // the search finishes.
        let count = {
            let tx = tx;
            self.stream(tx)?
        };
        let mut all: Vec<SearchMatch> = rx.try_iter().collect();
        debug_assert_eq!(all.len(), count);
        all.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.line_number.cmp(&b.line_number))
        });
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    let needle = 1;\n    println!(\"needle\");\n}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn helper() {}\n// no match here\n",
        )
        .unwrap();
        fs::write(dir.path().join("notes.md"), "a needle in markdown\n").unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored/\n").unwrap();
        fs::create_dir(dir.path().join("ignored")).unwrap();
        fs::write(dir.path().join("ignored/secret.rs"), "needle\n").unwrap();
        dir
    }

    #[test]
    fn literal_search_finds_all_hits_but_respects_gitignore() {
        let dir = project();
        let s = Searcher::new(dir.path(), SearchOptions::literal("needle"));
        let matches = s.collect().unwrap();
        // main.rs (2 lines), notes.md (1). ignored/secret.rs excluded.
        assert_eq!(matches.len(), 3);
        assert!(matches.iter().all(|m| !m.path.to_string_lossy().contains("ignored")));
    }

    #[test]
    fn submatch_offsets_are_reported() {
        let dir = project();
        let s = Searcher::new(dir.path(), SearchOptions::literal("needle"));
        let matches = s.collect().unwrap();
        let first = matches
            .iter()
            .find(|m| m.line.contains("let needle"))
            .unwrap();
        let (start, end) = (first.submatches[0].start, first.submatches[0].end);
        assert_eq!(&first.line[start..end], "needle");
    }

    #[test]
    fn whole_word_filters_substrings() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "cat category cats\n").unwrap();
        let opts = SearchOptions::literal("cat").whole_word(true);
        let matches = Searcher::new(dir.path(), opts).collect().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].submatches.len(), 1); // only the standalone "cat"
    }

    #[test]
    fn regex_search() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "foo123 bar baz456\n").unwrap();
        let opts = SearchOptions::literal(r"\w+\d+").regex(true);
        let matches = Searcher::new(dir.path(), opts).collect().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].submatches.len(), 2); // foo123 and baz456
    }

    #[test]
    fn include_glob_limits_file_types() {
        let dir = project();
        let opts = SearchOptions {
            include_globs: vec!["*.md".to_string()],
            ..SearchOptions::literal("needle")
        };
        let matches = Searcher::new(dir.path(), opts).collect().unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].path.extension().unwrap() == "md");
    }

    #[test]
    fn max_results_caps_output() {
        let dir = project();
        let opts = SearchOptions::literal("needle").max_results(1);
        let matches = Searcher::new(dir.path(), opts).collect().unwrap();
        assert_eq!(matches.len(), 1);
    }
}
