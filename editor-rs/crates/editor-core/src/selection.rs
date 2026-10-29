//! Carets, selections and the multi-cursor [`SelectionSet`].

use std::ops::Range;

/// A single selection, stored as an `anchor` and a `head` (both char offsets).
///
/// The `head` is where the caret is and where it moves from; the `anchor` is
/// the fixed end set when the selection started. `anchor == head` is a plain
/// caret with no selected text. Keeping them separate (rather than a sorted
/// `start..end`) is what lets a selection be extended in either direction and
/// preserves the caret side across edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    /// An empty selection (a caret) at `pos`.
    pub fn caret(pos: usize) -> Self {
        Selection {
            anchor: pos,
            head: pos,
        }
    }

    /// A selection spanning `anchor..head`.
    pub fn new(anchor: usize, head: usize) -> Self {
        Selection { anchor, head }
    }

    /// The lower offset of the two ends.
    pub fn min(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// The higher offset of the two ends.
    pub fn max(&self) -> usize {
        self.anchor.max(self.head)
    }

    /// The selected span as a sorted range.
    pub fn range(&self) -> Range<usize> {
        self.min()..self.max()
    }

    /// `true` if nothing is selected (a bare caret).
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Collapse the selection to a caret at its head.
    pub fn collapse_to_head(&self) -> Selection {
        Selection::caret(self.head)
    }

    /// Whether this selection overlaps (or directly abuts) `other`.
    pub fn overlaps(&self, other: &Selection) -> bool {
        self.min() <= other.max() && other.min() <= self.max()
    }
}

/// The complete set of cursors in a document: one or more [`Selection`]s plus
/// an index naming the *primary* one (the cursor that scrolls into view and
/// that single-caret operations act through).
///
/// Invariants maintained by every mutating method:
/// * there is always at least one selection;
/// * selections are kept sorted by their start offset;
/// * overlapping selections are merged;
/// * `primary` is always a valid index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionSet {
    selections: Vec<Selection>,
    primary: usize,
}

impl Default for SelectionSet {
    fn default() -> Self {
        SelectionSet::single(Selection::caret(0))
    }
}

impl SelectionSet {
    /// A set containing exactly one selection, which becomes primary.
    pub fn single(sel: Selection) -> Self {
        SelectionSet {
            selections: vec![sel],
            primary: 0,
        }
    }

    /// A single caret at offset 0.
    pub fn at_origin() -> Self {
        SelectionSet::single(Selection::caret(0))
    }

    pub fn len(&self) -> usize {
        self.selections.len()
    }

    pub fn is_empty(&self) -> bool {
        // A SelectionSet always holds at least one selection.
        false
    }

    /// All selections, sorted by start offset.
    pub fn all(&self) -> &[Selection] {
        &self.selections
    }

    /// The primary selection.
    pub fn primary(&self) -> Selection {
        self.selections[self.primary]
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// Replace every selection with the result of `f`.
    pub fn map(&mut self, mut f: impl FnMut(Selection) -> Selection) {
        for sel in &mut self.selections {
            *sel = f(*sel);
        }
        self.normalize();
    }

    /// Replace the whole set with a single selection.
    pub fn set_single(&mut self, sel: Selection) {
        self.selections.clear();
        self.selections.push(sel);
        self.primary = 0;
    }

    /// Add another cursor, keeping the existing primary unless the new one
    /// lands on top of it. Returns the index the new selection ended up at.
    pub fn add(&mut self, sel: Selection) {
        // Remember the primary selection's identity by value so we can find it
        // again after sorting/merging.
        let primary_val = self.selections[self.primary];
        self.selections.push(sel);
        self.normalize();
        // Keep the old primary as primary if it survived the merge.
        self.primary = self
            .selections
            .iter()
            .position(|s| s.overlaps(&primary_val))
            .unwrap_or(0);
    }

    /// Make the selection whose head is `head` (or the nearest) primary.
    pub fn set_primary_to(&mut self, index: usize) {
        if index < self.selections.len() {
            self.primary = index;
        }
    }

    /// Sort by start offset and merge overlapping selections. The merged
    /// selection keeps the orientation (anchor/head order) of the one that
    /// extended furthest, so directionality survives a merge.
    fn normalize(&mut self) {
        if self.selections.len() <= 1 {
            self.primary = 0;
            return;
        }
        let primary_val = self.selections[self.primary];

        self.selections.sort_by_key(|s| (s.min(), s.max()));

        let mut merged: Vec<Selection> = Vec::with_capacity(self.selections.len());
        for sel in self.selections.drain(..) {
            match merged.last_mut() {
                Some(last) if last.overlaps(&sel) => {
                    // Union the two, preferring to extend to the wider span.
                    let new_min = last.min().min(sel.min());
                    let new_max = last.max().max(sel.max());
                    // Preserve orientation of whichever had the rightmost head.
                    let forward = last.head >= last.anchor || sel.head >= sel.anchor;
                    *last = if forward {
                        Selection::new(new_min, new_max)
                    } else {
                        Selection::new(new_max, new_min)
                    };
                }
                _ => merged.push(sel),
            }
        }

        self.selections = merged;
        self.primary = self
            .selections
            .iter()
            .position(|s| s.overlaps(&primary_val))
            .unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_is_empty() {
        let s = Selection::caret(5);
        assert!(s.is_empty());
        assert_eq!(s.range(), 5..5);
    }

    #[test]
    fn selection_range_is_sorted() {
        let s = Selection::new(10, 3);
        assert_eq!(s.min(), 3);
        assert_eq!(s.max(), 10);
        assert_eq!(s.range(), 3..10);
    }

    #[test]
    fn default_is_single_caret_at_origin() {
        let set = SelectionSet::default();
        assert_eq!(set.len(), 1);
        assert_eq!(set.primary(), Selection::caret(0));
    }

    #[test]
    fn adding_cursors_keeps_sorted_order() {
        let mut set = SelectionSet::single(Selection::caret(5));
        set.add(Selection::caret(1));
        set.add(Selection::caret(9));
        let starts: Vec<_> = set.all().iter().map(|s| s.head).collect();
        assert_eq!(starts, vec![1, 5, 9]);
    }

    #[test]
    fn overlapping_cursors_merge() {
        let mut set = SelectionSet::single(Selection::new(2, 6));
        set.add(Selection::new(4, 8));
        assert_eq!(set.len(), 1);
        assert_eq!(set.all()[0].range(), 2..8);
    }

    #[test]
    fn primary_survives_merge() {
        let mut set = SelectionSet::single(Selection::new(2, 6));
        set.set_primary_to(0);
        set.add(Selection::caret(20));
        // The caret at 20 was added; primary should remain the 2..6 selection.
        assert_eq!(set.primary().range(), 2..6);
    }
}
