//! Grouped, invertible undo/redo history.

use crate::buffer::Change;
use crate::selection::SelectionSet;

/// One atomic unit of undo: the list of [`Change`]s that a single user action
/// produced, plus the selection state before and after.
///
/// A multi-cursor "type a character" action, for instance, becomes *one*
/// transaction holding N changes, so a single undo reverts all N edits and
/// restores all N cursors at once.
#[derive(Clone, Debug)]
pub struct Transaction {
    /// Changes in the order they were applied to the buffer.
    pub changes: Vec<Change>,
    /// Selections before the transaction (restored on undo).
    pub selections_before: SelectionSet,
    /// Selections after the transaction (restored on redo).
    pub selections_after: SelectionSet,
}

/// The undo/redo stacks.
///
/// Undo and redo never touch the disk or allocate beyond the changed text, so
/// they are safe to call from the UI thread. Edits also support *coalescing*:
/// consecutive single-character insertions are merged into the open
/// transaction until a word boundary or an explicit break, so undo works in
/// natural word-sized chunks instead of one keystroke at a time.
#[derive(Clone, Debug, Default)]
pub struct History {
    undo_stack: Vec<Transaction>,
    redo_stack: Vec<Transaction>,
    /// When `true`, the next pushed transaction may merge into the last one.
    coalescing: bool,
}

impl History {
    pub fn new() -> Self {
        History::default()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    /// Record a finished transaction. Pushing a new transaction always clears
    /// the redo stack (the classic linear-history model).
    pub fn push(&mut self, tx: Transaction) {
        self.redo_stack.clear();
        if self.coalescing {
            if let Some(last) = self.undo_stack.last_mut() {
                last.changes.extend(tx.changes);
                last.selections_after = tx.selections_after;
                return;
            }
        }
        self.undo_stack.push(tx);
    }

    /// Record a transaction that *may* merge with the previous one if the next
    /// push also opts into coalescing.
    pub fn push_coalescing(&mut self, tx: Transaction) {
        let was = self.coalescing;
        self.coalescing = true;
        if was {
            self.push(tx);
        } else {
            // First of a potential run: push standalone, then allow merging.
            self.coalescing = false;
            self.push(tx);
            self.coalescing = true;
        }
    }

    /// Break any open coalescing run. Call this on cursor moves, selection
    /// changes, saves, or any non-typing action so the next edit starts a
    /// fresh undo step.
    pub fn break_coalescing(&mut self) {
        self.coalescing = false;
    }

    /// Pop the most recent transaction for undoing. The caller applies the
    /// inverse changes to the buffer and restores `selections_before`.
    pub fn pop_undo(&mut self) -> Option<Transaction> {
        self.coalescing = false;
        let tx = self.undo_stack.pop()?;
        self.redo_stack.push(tx.clone());
        Some(tx)
    }

    /// Pop the most recent undone transaction for redoing.
    pub fn pop_redo(&mut self) -> Option<Transaction> {
        self.coalescing = false;
        let tx = self.redo_stack.pop()?;
        self.undo_stack.push(tx.clone());
        Some(tx)
    }

    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.coalescing = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::TextBuffer;
    use crate::selection::{Selection, SelectionSet};

    fn tx(buf: &mut TextBuffer, range: std::ops::Range<usize>, text: &str) -> Transaction {
        let before = SelectionSet::single(Selection::caret(range.start));
        let change = buf.replace(range, text);
        let after = SelectionSet::single(Selection::caret(change.new_end_char));
        Transaction {
            changes: vec![change],
            selections_before: before,
            selections_after: after,
        }
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut buf = TextBuffer::from_str("hello");
        let mut h = History::new();

        let t = tx(&mut buf, 5..5, " world");
        h.push(t);
        assert_eq!(buf.to_string(), "hello world");
        assert!(h.can_undo());

        let undone = h.pop_undo().unwrap();
        for ch in undone.changes.iter().rev() {
            buf.apply_inverse(&ch.inverse());
        }
        assert_eq!(buf.to_string(), "hello");
        assert!(h.can_redo());

        let redone = h.pop_redo().unwrap();
        for ch in &redone.changes {
            buf.replace(ch.start_char..ch.old_end_char, &ch.inserted);
        }
        assert_eq!(buf.to_string(), "hello world");
    }

    #[test]
    fn push_clears_redo() {
        let mut buf = TextBuffer::from_str("");
        let mut h = History::new();
        h.push(tx(&mut buf, 0..0, "a"));
        h.pop_undo();
        assert!(h.can_redo());
        h.push(tx(&mut buf, 0..0, "b"));
        assert!(!h.can_redo());
    }

    #[test]
    fn coalescing_merges_consecutive_edits() {
        let mut buf = TextBuffer::from_str("");
        let mut h = History::new();
        h.push_coalescing(tx(&mut buf, 0..0, "h"));
        h.push_coalescing(tx(&mut buf, 1..1, "i"));
        // Both merged into a single undo step.
        assert_eq!(h.undo_depth(), 1);
    }

    #[test]
    fn break_coalescing_starts_new_step() {
        let mut buf = TextBuffer::from_str("");
        let mut h = History::new();
        h.push_coalescing(tx(&mut buf, 0..0, "h"));
        h.break_coalescing();
        h.push_coalescing(tx(&mut buf, 1..1, "i"));
        assert_eq!(h.undo_depth(), 2);
    }
}
