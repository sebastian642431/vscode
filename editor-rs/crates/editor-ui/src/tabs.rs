//! The open-editor tab bar.

use std::path::PathBuf;

use editor_core::interface::DocumentId;

/// One open editor tab.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Tab {
    pub id: DocumentId,
    pub title: String,
    pub path: Option<PathBuf>,
    pub dirty: bool,
}

/// An ordered set of tabs with one active tab.
///
/// Opening a document that is already open just activates its tab, matching
/// every editor's behaviour.
#[derive(Clone, Debug, Default)]
pub struct TabBar {
    tabs: Vec<Tab>,
    active: Option<usize>,
}

impl TabBar {
    pub fn new() -> Self {
        TabBar::default()
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    pub fn active(&self) -> Option<&Tab> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    /// Open (or re-focus) a tab for `id`. Returns the tab's index.
    pub fn open(&mut self, tab: Tab) -> usize {
        if let Some(i) = self.tabs.iter().position(|t| t.id == tab.id) {
            self.active = Some(i);
            i
        } else {
            self.tabs.push(tab);
            let i = self.tabs.len() - 1;
            self.active = Some(i);
            i
        }
    }

    /// Close the tab for `id`. The active tab shifts to the neighbour. Returns
    /// `true` if a tab was removed.
    pub fn close(&mut self, id: DocumentId) -> bool {
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else {
            return false;
        };
        self.tabs.remove(i);
        self.active = if self.tabs.is_empty() {
            None
        } else {
            // Prefer the tab now at `i` (the one to the right), else the last.
            Some(i.min(self.tabs.len() - 1))
        };
        true
    }

    pub fn activate(&mut self, id: DocumentId) -> bool {
        if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
            self.active = Some(i);
            true
        } else {
            false
        }
    }

    /// Activate the next tab, wrapping around.
    pub fn next(&mut self) {
        if let Some(a) = self.active {
            if !self.tabs.is_empty() {
                self.active = Some((a + 1) % self.tabs.len());
            }
        }
    }

    /// Activate the previous tab, wrapping around.
    pub fn prev(&mut self) {
        if let Some(a) = self.active {
            let n = self.tabs.len();
            if n > 0 {
                self.active = Some((a + n - 1) % n);
            }
        }
    }

    pub fn set_dirty(&mut self, id: DocumentId, dirty: bool) {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            t.dirty = dirty;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: u64, title: &str) -> Tab {
        Tab {
            id: DocumentId(id),
            title: title.to_string(),
            path: None,
            dirty: false,
        }
    }

    #[test]
    fn opening_activates_and_dedups() {
        let mut bar = TabBar::new();
        bar.open(tab(1, "a.rs"));
        bar.open(tab(2, "b.rs"));
        assert_eq!(bar.len(), 2);
        assert_eq!(bar.active().unwrap().id, DocumentId(2));
        // Re-opening a.rs focuses it without adding a tab.
        bar.open(tab(1, "a.rs"));
        assert_eq!(bar.len(), 2);
        assert_eq!(bar.active().unwrap().id, DocumentId(1));
    }

    #[test]
    fn closing_shifts_active() {
        let mut bar = TabBar::new();
        bar.open(tab(1, "a"));
        bar.open(tab(2, "b"));
        bar.open(tab(3, "c"));
        bar.activate(DocumentId(2));
        assert!(bar.close(DocumentId(2)));
        // The tab that took slot 1 (c? no — b removed, c moves to index 1) is active.
        assert_eq!(bar.active().unwrap().id, DocumentId(3));
    }

    #[test]
    fn closing_last_clears_active() {
        let mut bar = TabBar::new();
        bar.open(tab(1, "only"));
        bar.close(DocumentId(1));
        assert!(bar.is_empty());
        assert!(bar.active().is_none());
    }

    #[test]
    fn next_prev_wraps() {
        let mut bar = TabBar::new();
        bar.open(tab(1, "a"));
        bar.open(tab(2, "b"));
        bar.activate(DocumentId(1));
        bar.next();
        assert_eq!(bar.active().unwrap().id, DocumentId(2));
        bar.next();
        assert_eq!(bar.active().unwrap().id, DocumentId(1));
        bar.prev();
        assert_eq!(bar.active().unwrap().id, DocumentId(2));
    }

    #[test]
    fn dirty_flag() {
        let mut bar = TabBar::new();
        bar.open(tab(1, "a"));
        bar.set_dirty(DocumentId(1), true);
        assert!(bar.active().unwrap().dirty);
    }
}
