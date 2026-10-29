//! Lazy, `.gitignore`-aware file-tree model for the side panel.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Whether an [`Entry`] is a file or a directory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    File,
    Directory,
}

/// One item in the file tree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }
}

/// Sort directories before files, then case-insensitively by name — the order
/// VS Code's explorer uses.
fn explorer_order(a: &Entry, b: &Entry) -> Ordering {
    match (a.is_dir(), b.is_dir()) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    }
}

/// A workspace file tree rooted at a folder.
///
/// The tree is populated lazily: [`read_dir`](FileTree::read_dir) lists exactly
/// one directory level, which is what the explorer needs when the user expands
/// a node. Nothing recurses until asked, so attaching a 200k-file repository is
/// instant.
#[derive(Clone, Debug)]
pub struct FileTree {
    root: PathBuf,
    respect_gitignore: bool,
    show_hidden: bool,
}

impl FileTree {
    /// Create a tree rooted at `root`, ignoring `.gitignore`d and hidden files
    /// by default (matching the explorer's defaults).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FileTree {
            root: root.into(),
            respect_gitignore: true,
            show_hidden: false,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn respect_gitignore(mut self, yes: bool) -> Self {
        self.respect_gitignore = yes;
        self
    }

    pub fn show_hidden(mut self, yes: bool) -> Self {
        self.show_hidden = yes;
        self
    }

    /// List the immediate children of `dir` (which must be inside the tree),
    /// sorted in explorer order. Honours the gitignore/hidden settings.
    ///
    /// This performs blocking I/O (one `read_dir` + `stat`s) and should be
    /// called from a worker, not the UI thread.
    pub fn read_dir(&self, dir: impl AsRef<Path>) -> std::io::Result<Vec<Entry>> {
        let dir = dir.as_ref();
        let mut entries = Vec::new();

        // `WalkBuilder` with max_depth(1) gives us gitignore handling for a
        // single level for free, instead of re-implementing ignore matching.
        let walker = WalkBuilder::new(dir)
            .max_depth(Some(1))
            .hidden(!self.show_hidden)
            .git_ignore(self.respect_gitignore)
            .git_global(self.respect_gitignore)
            .git_exclude(self.respect_gitignore)
            // Honour .gitignore even when the folder isn't a git checkout, as
            // VS Code's explorer does.
            .require_git(false)
            .parents(self.respect_gitignore)
            .build();

        for result in walker {
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue,
            };
            // Skip the directory itself (depth 0).
            if entry.depth() == 0 {
                continue;
            }
            let path = entry.path().to_path_buf();
            let name = match path.file_name() {
                Some(n) => n.to_string_lossy().into_owned(),
                None => continue,
            };
            let kind = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                EntryKind::Directory
            } else {
                EntryKind::File
            };
            entries.push(Entry { name, path, kind });
        }

        entries.sort_by(explorer_order);
        Ok(entries)
    }

    /// List the children of the tree root.
    pub fn read_root(&self) -> std::io::Result<Vec<Entry>> {
        let root = self.root.clone();
        self.read_dir(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("README.md"), "# hi").unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(dir.path().join("ignored.txt"), "secret").unwrap();
        dir
    }

    #[test]
    fn lists_root_in_explorer_order() {
        let dir = scratch();
        let tree = FileTree::new(dir.path());
        let entries = tree.read_root().unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        // Directory "src" first, then files alphabetically; ignored.txt hidden.
        assert_eq!(names, vec!["src", "Cargo.toml", "README.md"]);
    }

    #[test]
    fn gitignore_can_be_disabled() {
        let dir = scratch();
        let tree = FileTree::new(dir.path()).respect_gitignore(false);
        let entries = tree.read_root().unwrap();
        assert!(entries.iter().any(|e| e.name == "ignored.txt"));
    }

    #[test]
    fn descends_one_level() {
        let dir = scratch();
        let tree = FileTree::new(dir.path());
        let src = tree.read_dir(dir.path().join("src")).unwrap();
        let names: Vec<_> = src.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["main.rs"]);
    }
}
