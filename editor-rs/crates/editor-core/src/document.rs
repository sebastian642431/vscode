//! [`Document`] — the editable unit of editor-rs.
//!
//! A `Document` owns a [`TextBuffer`], a [`SelectionSet`] (multi-cursor) and a
//! [`History`], and exposes the editing commands the UI binds keys to. Every
//! command is synchronous and allocation-light so it is safe on the UI thread;
//! the [`Change`]s each command produces are buffered in [`Document::drain_changes`]
//! for the asynchronous syntax/LSP layers to consume off the hot path.

use std::ops::Range;

use crate::buffer::{Change, TextBuffer};
use crate::clipboard::Clipboard;
use crate::history::{History, Transaction};
use crate::position::Point;
use crate::selection::{Selection, SelectionSet};

/// A cursor movement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Movement {
    CharLeft,
    CharRight,
    Up,
    Down,
    WordLeft,
    WordRight,
    /// Beginning of the line (column 0).
    LineStart,
    /// End of the line (before the line break).
    LineEnd,
    /// First non-whitespace char, or column 0 if already there ("smart home").
    LineFirstNonWhitespace,
    BufferStart,
    BufferEnd,
}

/// The unit a deletion operates on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Granularity {
    Character,
    Word,
    Line,
}

/// Classes used for word-wise movement and deletion.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Whitespace,
    Word,
    Punctuation,
}

fn classify(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Whitespace
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// The editable document model.
#[derive(Clone, Debug)]
pub struct Document {
    buffer: TextBuffer,
    selections: SelectionSet,
    history: History,
    /// Monotonic version, bumped on every content change. Mirrors the LSP
    /// `textDocument/version` so the language-server layer can sync safely.
    version: u64,
    /// Sticky target column for consecutive vertical moves with a single
    /// cursor, so moving down through a short line and back keeps the column.
    vertical_goal: Option<usize>,
    /// Changes produced since the last [`drain_changes`](Self::drain_changes).
    recent_changes: Vec<Change>,
}

impl Default for Document {
    fn default() -> Self {
        Document::new()
    }
}

impl Document {
    pub fn new() -> Self {
        Document::from_str("")
    }

    /// Create a document from initial text. Infallible; returns `Self` directly
    /// rather than via [`std::str::FromStr`].
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        Document {
            buffer: TextBuffer::from_str(text),
            selections: SelectionSet::at_origin(),
            history: History::new(),
            version: 0,
            vertical_goal: None,
            recent_changes: Vec::new(),
        }
    }

    // ----- accessors ---------------------------------------------------------

    pub fn buffer(&self) -> &TextBuffer {
        &self.buffer
    }

    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn text(&self) -> String {
        self.buffer.to_string()
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Take the [`Change`]s accumulated since the previous call. The UI calls
    /// this after each edit and forwards the result to the syntax/LSP workers.
    pub fn drain_changes(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.recent_changes)
    }

    /// Explicitly break undo coalescing (e.g. on save or focus change). Public
    /// so the UI can start a fresh undo step without a cursor move.
    pub fn break_undo_group(&mut self) {
        self.history.break_coalescing();
    }

    /// Replace the entire set of selections (e.g. in response to a mouse click
    /// or a "go to line"). Breaks undo coalescing.
    pub fn set_selections(&mut self, selections: SelectionSet) {
        self.history.break_coalescing();
        self.selections = selections;
        self.vertical_goal = None;
    }

    /// Place a single caret at `point`.
    pub fn set_caret(&mut self, point: Point) {
        let offset = self.buffer.point_to_char(point);
        self.set_selections(SelectionSet::single(Selection::caret(offset)));
    }

    // ----- editing -----------------------------------------------------------

    /// Insert `text` at every selection, replacing any selected text, then
    /// collapse each selection to a caret after the inserted text. Single
    /// characters coalesce in the undo history into word-sized steps.
    pub fn insert(&mut self, text: &str) {
        let coalesce = text.chars().count() == 1 && !text.contains('\n');
        self.edit_selections(coalesce, |_buf, sel| (sel.range(), text.to_string()));
    }

    /// Insert a newline at each caret, preserving the current line's leading
    /// indentation (spaces/tabs) on the new line.
    pub fn insert_newline(&mut self) {
        self.edit_selections(false, |buf, sel| {
            let line = buf.char_to_line(sel.min());
            let line_start = buf.line_to_char(line);
            let indent: String = buf
                .line(line)
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            // Don't carry indentation past the caret if the caret is inside it.
            let indent_len = indent.chars().count();
            let caret_col = sel.min() - line_start;
            let keep = indent_len.min(caret_col);
            let new_text = format!("\n{}", &indent[..indent.char_indices().nth(keep).map(|(i, _)| i).unwrap_or(indent.len())]);
            (sel.range(), new_text)
        });
    }

    /// Delete text relative to each selection. With a non-empty selection the
    /// selection itself is deleted and `granularity`/`forward` are ignored.
    pub fn delete(&mut self, granularity: Granularity, forward: bool) {
        self.edit_selections(false, |buf, sel| {
            if !sel.is_empty() {
                return (sel.range(), String::new());
            }
            let pos = sel.head;
            let range = match (granularity, forward) {
                (Granularity::Character, false) => pos.saturating_sub(1)..pos,
                (Granularity::Character, true) => pos..(pos + 1).min(buf.len_chars()),
                (Granularity::Word, false) => word_boundary_left(buf, pos)..pos,
                (Granularity::Word, true) => pos..word_boundary_right(buf, pos),
                (Granularity::Line, _) => {
                    let line = buf.char_to_line(pos);
                    let start = buf.line_to_char(line);
                    let end = if line + 1 < buf.len_lines() {
                        buf.line_to_char(line + 1)
                    } else {
                        buf.len_chars()
                    };
                    start..end
                }
            };
            (range, String::new())
        });
    }

    /// Backspace: delete the selection, or the character before each caret.
    pub fn backspace(&mut self) {
        self.delete(Granularity::Character, false);
    }

    // ----- movement ----------------------------------------------------------

    /// Move every cursor by `movement`. When `extend` is true the anchor is
    /// kept (growing the selection); otherwise each selection collapses to a
    /// caret at the new head.
    pub fn move_cursors(&mut self, movement: Movement, extend: bool) {
        self.history.break_coalescing();
        let is_vertical = matches!(movement, Movement::Up | Movement::Down);
        let single = self.selections.len() == 1;

        let sels = self.selections.all().to_vec();
        let primary = self.selections.primary_index();
        let mut goal = self.vertical_goal;

        let new: Vec<Selection> = sels
            .iter()
            .map(|sel| {
                // When collapsing a non-empty selection with a horizontal move,
                // VS Code jumps to the selection edge rather than moving a char.
                let head = if !extend
                    && !sel.is_empty()
                    && matches!(movement, Movement::CharLeft | Movement::CharRight)
                {
                    match movement {
                        Movement::CharLeft => sel.min(),
                        Movement::CharRight => sel.max(),
                        _ => unreachable!(),
                    }
                } else {
                    self.target_head(sel.head, movement, &mut goal, single)
                };
                if extend {
                    Selection::new(sel.anchor, head)
                } else {
                    Selection::caret(head)
                }
            })
            .collect();

        self.rebuild_selections(new, primary);
        self.vertical_goal = if is_vertical { goal } else { None };
    }

    fn target_head(
        &self,
        head: usize,
        movement: Movement,
        goal: &mut Option<usize>,
        single: bool,
    ) -> usize {
        let buf = &self.buffer;
        match movement {
            Movement::CharLeft => head.saturating_sub(1),
            Movement::CharRight => (head + 1).min(buf.len_chars()),
            Movement::WordLeft => word_boundary_left(buf, head),
            Movement::WordRight => word_boundary_right(buf, head),
            Movement::LineStart => {
                let line = buf.char_to_line(head);
                buf.line_to_char(line)
            }
            Movement::LineEnd => {
                let line = buf.char_to_line(head);
                buf.line_to_char(line) + buf.line_len_chars(line)
            }
            Movement::LineFirstNonWhitespace => {
                let line = buf.char_to_line(head);
                let line_start = buf.line_to_char(line);
                let first_non_ws = buf
                    .line(line)
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .count();
                let smart = line_start + first_non_ws;
                if head == smart {
                    line_start
                } else {
                    smart
                }
            }
            Movement::BufferStart => 0,
            Movement::BufferEnd => buf.len_chars(),
            Movement::Up | Movement::Down => {
                let point = buf.char_to_point(head);
                // With a single cursor the goal column is sticky, so moving
                // through a short line and back preserves the column.
                let target_col = if single {
                    *goal.get_or_insert(point.column)
                } else {
                    point.column
                };
                let target_line = match movement {
                    Movement::Up => {
                        if point.line == 0 {
                            return buf.line_to_char(0); // top: go to start
                        }
                        point.line - 1
                    }
                    Movement::Down => {
                        if point.line + 1 >= buf.len_lines() {
                            return buf.len_chars(); // bottom: go to end
                        }
                        point.line + 1
                    }
                    _ => unreachable!(),
                };
                if single {
                    *goal = Some(target_col);
                }
                buf.point_to_char(Point::new(target_line, target_col))
            }
        }
    }

    /// Add a caret one line below each existing selection head, at the same
    /// column (VS Code's "Add Cursor Below").
    pub fn add_cursor_below(&mut self) {
        self.add_cursor_vertical(1);
    }

    /// Add a caret one line above each existing selection head.
    pub fn add_cursor_above(&mut self) {
        self.add_cursor_vertical(-1);
    }

    fn add_cursor_vertical(&mut self, dir: isize) {
        self.history.break_coalescing();
        let existing = self.selections.all().to_vec();
        for sel in existing {
            let point = self.buffer.char_to_point(sel.head);
            let target_line = point.line as isize + dir;
            if target_line < 0 || target_line as usize >= self.buffer.len_lines() {
                continue;
            }
            let offset = self
                .buffer
                .point_to_char(Point::new(target_line as usize, point.column));
            self.selections.add(Selection::caret(offset));
        }
        self.vertical_goal = None;
    }

    /// Select the whole document with a single selection.
    pub fn select_all(&mut self) {
        self.history.break_coalescing();
        self.selections = SelectionSet::single(Selection::new(0, self.buffer.len_chars()));
        self.vertical_goal = None;
    }

    // ----- undo / redo -------------------------------------------------------

    pub fn undo(&mut self) -> bool {
        let Some(tx) = self.history.pop_undo() else {
            return false;
        };
        for change in tx.changes.iter().rev() {
            let inv = change.inverse();
            let replay = self.buffer.apply_inverse(&inv);
            self.recent_changes.push(replay);
        }
        self.selections = tx.selections_before;
        self.version += 1;
        self.vertical_goal = None;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(tx) = self.history.pop_redo() else {
            return false;
        };
        for change in &tx.changes {
            let replay = self
                .buffer
                .replace(change.start_char..change.old_end_char, &change.inserted);
            self.recent_changes.push(replay);
        }
        self.selections = tx.selections_after;
        self.version += 1;
        self.vertical_goal = None;
        true
    }

    // ----- clipboard ---------------------------------------------------------

    /// Copy the selected text to `clipboard`. With only empty carets, copies
    /// the whole line of each caret in "line mode" (as VS Code does).
    pub fn copy(&self, clipboard: &mut dyn Clipboard) {
        let any_selection = self.selections.all().iter().any(|s| !s.is_empty());
        if any_selection {
            let text = self
                .selections
                .all()
                .iter()
                .filter(|s| !s.is_empty())
                .map(|s| self.buffer.slice(s.range()))
                .collect::<Vec<_>>()
                .join("\n");
            clipboard.set_text(text);
        } else {
            // Line mode: copy each caret's line (deduplicated by line).
            let mut lines: Vec<usize> = self
                .selections
                .all()
                .iter()
                .map(|s| self.buffer.char_to_line(s.head))
                .collect();
            lines.dedup();
            let text = lines
                .iter()
                .map(|&l| {
                    let s = self.buffer.line(l);
                    if s.ends_with('\n') {
                        s
                    } else {
                        format!("{s}\n")
                    }
                })
                .collect::<String>();
            clipboard.set_line_text(text);
        }
    }

    /// Cut: copy, then delete the selection (or the whole line in line mode).
    pub fn cut(&mut self, clipboard: &mut dyn Clipboard) {
        self.copy(clipboard);
        let any_selection = self.selections.all().iter().any(|s| !s.is_empty());
        if any_selection {
            self.edit_selections(false, |_buf, sel| (sel.range(), String::new()));
        } else {
            self.delete(Granularity::Line, true);
        }
    }

    /// Paste the clipboard at each selection. If the clipboard holds exactly as
    /// many lines as there are cursors, one line is distributed per cursor
    /// (VS Code behaviour); line-mode content pasted at a single caret inserts
    /// a whole line above the caret.
    pub fn paste(&mut self, clipboard: &dyn Clipboard) {
        let Some(text) = clipboard.text() else {
            return;
        };
        let cursor_count = self.selections.len();

        if clipboard.is_line_mode() && cursor_count == 1 {
            let head = self.selections.primary().head;
            let line = self.buffer.char_to_line(head);
            let line_start = self.buffer.line_to_char(line);
            let insert = if text.ends_with('\n') {
                text.clone()
            } else {
                format!("{text}\n")
            };
            let change = self.buffer.insert(line_start, &insert);
            self.record_single_change(change, Selection::caret(head + insert.chars().count()));
            return;
        }

        let lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
        let distribute = cursor_count > 1 && lines.len() == cursor_count;
        let mut idx = 0usize;
        self.edit_selections(false, move |_buf, sel| {
            let chunk = if distribute {
                let c = lines[idx].clone();
                idx += 1;
                c
            } else {
                text.clone()
            };
            (sel.range(), chunk)
        });
    }

    // ----- internal edit machinery ------------------------------------------

    /// Apply one edit per selection. `compute` is given the *pre-edit* buffer
    /// and a selection, and returns `(char_range, replacement)`. Ranges are
    /// computed against the original buffer and then shifted by the running
    /// delta as edits are applied left-to-right, so multi-cursor edits stay
    /// consistent in a single pass.
    fn edit_selections<F>(&mut self, coalesce: bool, mut compute: F)
    where
        F: FnMut(&TextBuffer, Selection) -> (Range<usize>, String),
    {
        let before = self.selections.clone();
        let primary = before.primary_index();
        let sels = before.all().to_vec();

        // Compute all edits against the original buffer first.
        let specs: Vec<(Range<usize>, String)> =
            sels.iter().map(|s| compute(&self.buffer, *s)).collect();

        if specs
            .iter()
            .all(|(r, t)| r.start == r.end && t.is_empty())
        {
            return; // nothing to do (e.g. backspace at start of document)
        }

        let mut changes = Vec::with_capacity(specs.len());
        let mut new_heads = Vec::with_capacity(specs.len());
        let mut delta: isize = 0;

        for (range, text) in specs {
            let start = (range.start as isize + delta).max(0) as usize;
            let end = (range.end as isize + delta).max(start as isize) as usize;
            let change = self.buffer.replace(start..end, &text);
            let inserted = text.chars().count();
            new_heads.push(Selection::caret(start + inserted));
            delta += inserted as isize - (end as isize - start as isize);
            changes.push(change);
        }

        self.recent_changes.extend(changes.iter().cloned());
        self.rebuild_selections(new_heads, primary);
        self.version += 1;

        let tx = Transaction {
            changes,
            selections_before: before,
            selections_after: self.selections.clone(),
        };
        if coalesce {
            self.history.push_coalescing(tx);
        } else {
            self.history.break_coalescing();
            self.history.push(tx);
        }
        self.vertical_goal = None;
    }

    fn record_single_change(&mut self, change: Change, new_sel: Selection) {
        let before = self.selections.clone();
        self.recent_changes.push(change.clone());
        self.selections = SelectionSet::single(new_sel);
        self.version += 1;
        self.history.break_coalescing();
        self.history.push(Transaction {
            changes: vec![change],
            selections_before: before,
            selections_after: self.selections.clone(),
        });
        self.vertical_goal = None;
    }

    fn rebuild_selections(&mut self, new: Vec<Selection>, old_primary: usize) {
        debug_assert!(!new.is_empty());
        let mut set = SelectionSet::single(new[0]);
        for s in &new[1..] {
            set.add(*s);
        }
        set.set_primary_to(old_primary.min(set.len().saturating_sub(1)));
        self.selections = set;
    }
}

/// The char offset of the start of the word to the left of `pos`.
fn word_boundary_left(buf: &TextBuffer, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut i = pos;
    // Skip any run of whitespace immediately to the left.
    while i > 0 && classify(buf.char(i - 1)) == CharClass::Whitespace {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    let class = classify(buf.char(i - 1));
    while i > 0 && classify(buf.char(i - 1)) == class {
        i -= 1;
    }
    i
}

/// The char offset of the end of the word to the right of `pos`.
fn word_boundary_right(buf: &TextBuffer, pos: usize) -> usize {
    let len = buf.len_chars();
    if pos >= len {
        return len;
    }
    let mut i = pos;
    // Skip any run of whitespace immediately to the right.
    while i < len && classify(buf.char(i)) == CharClass::Whitespace {
        i += 1;
    }
    if i >= len {
        return len;
    }
    let class = classify(buf.char(i));
    while i < len && classify(buf.char(i)) == class {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::MemoryClipboard;

    #[test]
    fn typing_inserts_and_moves_caret() {
        let mut doc = Document::new();
        doc.insert("hello");
        assert_eq!(doc.text(), "hello");
        assert_eq!(doc.selections().primary(), Selection::caret(5));
    }

    #[test]
    fn backspace_deletes_char_before_caret() {
        let mut doc = Document::from_str("abc");
        doc.set_caret(Point::new(0, 3));
        doc.backspace();
        assert_eq!(doc.text(), "ab");
        assert_eq!(doc.selections().primary(), Selection::caret(2));
    }

    #[test]
    fn backspace_deletes_selection() {
        let mut doc = Document::from_str("abcdef");
        doc.set_selections(SelectionSet::single(Selection::new(1, 4)));
        doc.backspace();
        assert_eq!(doc.text(), "aef");
    }

    #[test]
    fn multicursor_insert_is_single_pass_and_consistent() {
        let mut doc = Document::from_str("a\nb\nc");
        // Carets at start of each line.
        let mut set = SelectionSet::single(Selection::caret(0));
        set.add(Selection::caret(2));
        set.add(Selection::caret(4));
        doc.set_selections(set);
        doc.insert(">");
        assert_eq!(doc.text(), ">a\n>b\n>c");
        assert_eq!(doc.selections().len(), 3);
    }

    #[test]
    fn multicursor_insert_undo_is_atomic() {
        let mut doc = Document::from_str("a\nb");
        let mut set = SelectionSet::single(Selection::caret(0));
        set.add(Selection::caret(2));
        doc.set_selections(set);
        doc.insert("X");
        assert_eq!(doc.text(), "Xa\nXb");
        assert!(doc.undo());
        assert_eq!(doc.text(), "a\nb");
        assert_eq!(doc.selections().len(), 2);
    }

    #[test]
    fn newline_preserves_indent() {
        let mut doc = Document::from_str("    foo");
        doc.set_caret(Point::new(0, 7));
        doc.insert_newline();
        assert_eq!(doc.text(), "    foo\n    ");
    }

    #[test]
    fn word_movement() {
        let mut doc = Document::from_str("hello world_42 foo");
        doc.set_caret(Point::ZERO);
        doc.move_cursors(Movement::WordRight, false);
        assert_eq!(doc.selections().primary().head, 5); // after "hello"
        doc.move_cursors(Movement::WordRight, false);
        // Skips the space, then stops after the whole "world_42" token.
        assert_eq!(doc.selections().primary().head, 14);
    }

    #[test]
    fn word_delete_left() {
        let mut doc = Document::from_str("foo bar");
        doc.set_caret(Point::new(0, 7));
        doc.delete(Granularity::Word, false);
        assert_eq!(doc.text(), "foo ");
    }

    #[test]
    fn vertical_movement_keeps_goal_column() {
        let mut doc = Document::from_str("abcdef\nxy\nghijkl");
        doc.set_caret(Point::new(0, 5)); // column 5 on line 0
        doc.move_cursors(Movement::Down, false); // line 1 only has 2 chars
        assert_eq!(doc.buffer().char_to_point(doc.selections().primary().head), Point::new(1, 2));
        doc.move_cursors(Movement::Down, false); // back to a long line
        assert_eq!(doc.buffer().char_to_point(doc.selections().primary().head), Point::new(2, 5));
    }

    #[test]
    fn smart_home_toggles() {
        let mut doc = Document::from_str("    code");
        doc.set_caret(Point::new(0, 8));
        doc.move_cursors(Movement::LineFirstNonWhitespace, false);
        assert_eq!(doc.selections().primary().head, 4);
        doc.move_cursors(Movement::LineFirstNonWhitespace, false);
        assert_eq!(doc.selections().primary().head, 0);
    }

    #[test]
    fn select_all_then_type_replaces() {
        let mut doc = Document::from_str("old content");
        doc.select_all();
        doc.insert("new");
        assert_eq!(doc.text(), "new");
    }

    #[test]
    fn undo_redo_sequence() {
        let mut doc = Document::new();
        doc.insert("a");
        doc.break_undo_group();
        doc.insert("b");
        doc.break_undo_group();
        doc.insert("c");
        assert_eq!(doc.text(), "abc");
        doc.undo();
        assert_eq!(doc.text(), "ab");
        doc.undo();
        assert_eq!(doc.text(), "a");
        doc.redo();
        assert_eq!(doc.text(), "ab");
    }

    #[test]
    fn typing_coalesces_into_one_undo_step() {
        let mut doc = Document::new();
        doc.insert("h");
        doc.insert("i");
        // Two single-char inserts coalesce: one undo clears both.
        doc.undo();
        assert_eq!(doc.text(), "");
    }

    #[test]
    fn add_cursor_below_then_type() {
        let mut doc = Document::from_str("aa\nbb\ncc");
        doc.set_caret(Point::new(0, 1));
        doc.add_cursor_below();
        doc.add_cursor_below();
        doc.insert("|");
        assert_eq!(doc.text(), "a|a\nb|b\nc|c");
    }

    #[test]
    fn copy_paste_selection() {
        let mut doc = Document::from_str("hello");
        let mut clip = MemoryClipboard::new();
        doc.set_selections(SelectionSet::single(Selection::new(0, 5)));
        doc.copy(&mut clip);
        doc.set_caret(Point::new(0, 5));
        doc.paste(&clip);
        assert_eq!(doc.text(), "hellohello");
    }

    #[test]
    fn drain_changes_feeds_consumers() {
        let mut doc = Document::new();
        doc.insert("x");
        let changes = doc.drain_changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].inserted, "x");
        // Draining is one-shot.
        assert!(doc.drain_changes().is_empty());
    }

    #[test]
    fn version_increments_on_change_only() {
        let mut doc = Document::new();
        let v0 = doc.version();
        doc.move_cursors(Movement::CharRight, false);
        assert_eq!(doc.version(), v0, "movement must not bump version");
        doc.insert("x");
        assert_eq!(doc.version(), v0 + 1);
    }
}
