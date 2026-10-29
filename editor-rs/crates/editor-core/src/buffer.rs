//! The rope-backed text buffer and the [`Change`] events it emits.

use std::fmt;
use std::ops::Range;

use ropey::Rope;

use crate::position::Point;

/// A fully described text change.
///
/// Every mutation of a [`TextBuffer`] produces exactly one `Change`. It is the
/// single source of truth consumed by three very different downstream
/// consumers, which is why it carries redundant coordinate systems:
///
/// * **Undo/redo** needs the removed and inserted text ([`removed`], [`inserted`]).
/// * **tree-sitter** needs byte offsets and `(row, column)` points to build an
///   `InputEdit` for incremental reparsing.
/// * **LSP `didChange`** needs `(line, column)` ranges.
///
/// Computing all of this once, at edit time, means none of the async consumers
/// ever has to walk the (possibly huge) buffer to figure out *where* an edit
/// landed.
///
/// [`removed`]: Change::removed
/// [`inserted`]: Change::inserted
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Char offset where the replaced region begins.
    pub start_char: usize,
    /// Char offset of the end of the region *before* the edit.
    pub old_end_char: usize,
    /// Char offset of the end of the region *after* the edit.
    pub new_end_char: usize,

    /// Byte offset where the replaced region begins.
    pub start_byte: usize,
    /// Byte offset of the end of the region before the edit.
    pub old_end_byte: usize,
    /// Byte offset of the end of the region after the edit.
    pub new_end_byte: usize,

    /// `(line, column)` of [`start_char`](Change::start_char).
    pub start_point: Point,
    /// `(line, column)` of the old end.
    pub old_end_point: Point,
    /// `(line, column)` of the new end.
    pub new_end_point: Point,

    /// Text that was removed (empty for a pure insertion).
    pub removed: String,
    /// Text that was inserted (empty for a pure deletion).
    pub inserted: String,
}

impl Change {
    /// The exact inverse change: applying it undoes `self`. Byte/point fields
    /// are left zeroed because the buffer recomputes them when the inverse is
    /// applied — only the char range and the swapped text matter for replay.
    pub fn inverse(&self) -> InverseChange {
        InverseChange {
            range: self.start_char..self.new_end_char,
            text: self.removed.clone(),
        }
    }
}

/// A minimal description of how to reverse a [`Change`]: replace `range` with
/// `text`. The buffer turns this back into a full [`Change`] on replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InverseChange {
    pub range: Range<usize>,
    pub text: String,
}

/// A rope-backed, UTF-8 text buffer.
///
/// All public offsets are **char** offsets unless a method name says `byte`.
/// Indexing, slicing, insertion and deletion are all `O(log n)`.
#[derive(Clone, Debug)]
pub struct TextBuffer {
    rope: Rope,
}

impl Default for TextBuffer {
    fn default() -> Self {
        TextBuffer::new()
    }
}

/// Renders the whole buffer. This gives `TextBuffer` a free `to_string()` via
/// the blanket [`ToString`] impl; for huge buffers prefer streaming
/// [`rope()`](TextBuffer::rope) directly to disk.
impl fmt::Display for TextBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rope)
    }
}

impl TextBuffer {
    /// An empty buffer.
    pub fn new() -> Self {
        TextBuffer { rope: Rope::new() }
    }

    /// A buffer initialised from a string slice.
    ///
    /// Named `from_str` for symmetry with [`ropey::Rope::from_str`]; it is
    /// infallible and returns `Self` directly rather than via [`std::str::FromStr`].
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        TextBuffer {
            rope: Rope::from_str(text),
        }
    }

    /// Borrow the underlying rope (read-only), e.g. to stream it to disk.
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    // ----- sizes -------------------------------------------------------------

    /// Number of characters (Unicode scalar values) in the buffer.
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Number of bytes in the buffer's UTF-8 encoding.
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Number of lines. A buffer always has at least one line; a trailing
    /// newline does **not** add an extra empty line to this count (matching
    /// `ropey`'s `len_lines` minus the virtual trailing line).
    pub fn len_lines(&self) -> usize {
        // ropey counts a trailing line break as starting a new (empty) line.
        // Editors almost always want "number of lines with content or the
        // single empty line", so we normalise.
        let raw = self.rope.len_lines();
        if raw == 0 {
            1
        } else if self.rope.len_chars() > 0 && self.ends_with_newline() {
            raw - 1
        } else {
            raw
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rope.len_chars() == 0
    }

    fn ends_with_newline(&self) -> bool {
        self.rope
            .len_chars()
            .checked_sub(1)
            .map(|last| self.rope.char(last) == '\n')
            .unwrap_or(false)
    }

    // ----- conversions -------------------------------------------------------

    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        self.rope.char_to_byte(char_idx)
    }

    pub fn byte_to_char(&self, byte_idx: usize) -> usize {
        self.rope.byte_to_char(byte_idx)
    }

    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx)
    }

    /// Char offset of the first character of `line`.
    pub fn line_to_char(&self, line: usize) -> usize {
        self.rope.line_to_char(line)
    }

    /// Convert a char offset into a `(line, column)` [`Point`].
    pub fn char_to_point(&self, char_idx: usize) -> Point {
        let char_idx = char_idx.min(self.rope.len_chars());
        let line = self.rope.char_to_line(char_idx);
        let line_start = self.rope.line_to_char(line);
        Point::new(line, char_idx - line_start)
    }

    /// Convert a `(line, column)` [`Point`] into a char offset, clamping any
    /// out-of-range line or column to the nearest valid position.
    pub fn point_to_char(&self, point: Point) -> usize {
        let line = point.line.min(self.len_lines().saturating_sub(1));
        let line_start = self.rope.line_to_char(line);
        let line_len = self.line_len_chars(line);
        line_start + point.column.min(line_len)
    }

    /// Length of `line` in characters, **excluding** its trailing line break.
    pub fn line_len_chars(&self, line: usize) -> usize {
        if line >= self.rope.len_lines() {
            return 0;
        }
        let slice = self.rope.line(line);
        let mut len = slice.len_chars();
        // Trim a trailing "\r\n" or "\n".
        if len > 0 && slice.char(len - 1) == '\n' {
            len -= 1;
            if len > 0 && slice.char(len - 1) == '\r' {
                len -= 1;
            }
        }
        len
    }

    // ----- reading -----------------------------------------------------------

    /// The char at `char_idx`. Panics if out of bounds.
    pub fn char(&self, char_idx: usize) -> char {
        self.rope.char(char_idx)
    }

    /// An owned copy of the text in `range` (char offsets).
    pub fn slice(&self, range: Range<usize>) -> String {
        let end = range.end.min(self.rope.len_chars());
        let start = range.start.min(end);
        self.rope.slice(start..end).to_string()
    }

    /// The text of `line`, including any trailing line break.
    pub fn line(&self, line: usize) -> String {
        if line >= self.rope.len_lines() {
            return String::new();
        }
        self.rope.line(line).to_string()
    }

    // ----- mutation ----------------------------------------------------------

    /// Replace the characters in `range` with `text` and return a fully
    /// described [`Change`].
    ///
    /// `range` is clamped to the buffer, so callers never need to bounds-check
    /// first. This is the single mutation primitive; every editing command is
    /// expressed in terms of it.
    pub fn replace(&mut self, range: Range<usize>, text: &str) -> Change {
        let len = self.rope.len_chars();
        let start = range.start.min(len);
        let old_end = range.end.min(len).max(start);

        // Capture the "before" coordinate systems.
        let start_byte = self.rope.char_to_byte(start);
        let old_end_byte = self.rope.char_to_byte(old_end);
        let start_point = self.char_to_point(start);
        let old_end_point = self.char_to_point(old_end);
        let removed = if old_end > start {
            self.rope.slice(start..old_end).to_string()
        } else {
            String::new()
        };

        // Apply: delete then insert. ropey does both in O(log n).
        if old_end > start {
            self.rope.remove(start..old_end);
        }
        if !text.is_empty() {
            self.rope.insert(start, text);
        }

        // Capture the "after" coordinate systems.
        let inserted_chars = text.chars().count();
        let new_end = start + inserted_chars;
        let new_end_byte = self.rope.char_to_byte(new_end);
        let new_end_point = self.char_to_point(new_end);

        Change {
            start_char: start,
            old_end_char: old_end,
            new_end_char: new_end,
            start_byte,
            old_end_byte,
            new_end_byte,
            start_point,
            old_end_point,
            new_end_point,
            removed,
            inserted: text.to_string(),
        }
    }

    /// Apply an [`InverseChange`] produced by [`Change::inverse`], returning the
    /// fresh [`Change`] that describes the replay (so redo can invert it again).
    pub fn apply_inverse(&mut self, inv: &InverseChange) -> Change {
        self.replace(inv.range.clone(), &inv.text)
    }

    /// Insert `text` at `char_idx`. Convenience over [`replace`](Self::replace).
    pub fn insert(&mut self, char_idx: usize, text: &str) -> Change {
        self.replace(char_idx..char_idx, text)
    }

    /// Delete the characters in `range`. Convenience over
    /// [`replace`](Self::replace).
    pub fn delete(&mut self, range: Range<usize>) -> Change {
        self.replace(range, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_basics() {
        let b = TextBuffer::new();
        assert_eq!(b.len_chars(), 0);
        assert_eq!(b.len_lines(), 1);
        assert!(b.is_empty());
    }

    #[test]
    fn line_counting_ignores_virtual_trailing_line() {
        assert_eq!(TextBuffer::from_str("a\nb\nc").len_lines(), 3);
        assert_eq!(TextBuffer::from_str("a\nb\nc\n").len_lines(), 3);
        assert_eq!(TextBuffer::from_str("").len_lines(), 1);
        assert_eq!(TextBuffer::from_str("\n").len_lines(), 1);
    }

    #[test]
    fn point_round_trips() {
        let b = TextBuffer::from_str("hello\nwörld\n123");
        for ch in 0..=b.len_chars() {
            let p = b.char_to_point(ch);
            assert_eq!(b.point_to_char(p), ch, "char {ch} round-trip via {p:?}");
        }
    }

    #[test]
    fn unicode_byte_vs_char_offsets() {
        // "wörld": ö is two bytes.
        let b = TextBuffer::from_str("wörld");
        assert_eq!(b.len_chars(), 5);
        assert_eq!(b.len_bytes(), 6);
        assert_eq!(b.char_to_byte(2), 3); // 'r' starts at byte 3
    }

    #[test]
    fn replace_reports_both_coordinate_systems() {
        let mut b = TextBuffer::from_str("let x = 1;");
        let change = b.replace(8..9, "42");
        assert_eq!(b.to_string(), "let x = 42;");
        assert_eq!(change.removed, "1");
        assert_eq!(change.inserted, "42");
        assert_eq!(change.start_char, 8);
        assert_eq!(change.old_end_char, 9);
        assert_eq!(change.new_end_char, 10);
        assert_eq!(change.start_byte, 8);
        assert_eq!(change.old_end_byte, 9);
        assert_eq!(change.new_end_byte, 10);
    }

    #[test]
    fn change_inverse_restores_text() {
        let mut b = TextBuffer::from_str("abcdef");
        let change = b.replace(1..4, "XY");
        assert_eq!(b.to_string(), "aXYef");
        let inv = change.inverse();
        b.apply_inverse(&inv);
        assert_eq!(b.to_string(), "abcdef");
    }

    #[test]
    fn insert_and_delete_helpers() {
        let mut b = TextBuffer::from_str("ac");
        b.insert(1, "b");
        assert_eq!(b.to_string(), "abc");
        b.delete(0..1);
        assert_eq!(b.to_string(), "bc");
    }

    #[test]
    fn line_len_excludes_newline() {
        let b = TextBuffer::from_str("ab\ncde\n");
        assert_eq!(b.line_len_chars(0), 2);
        assert_eq!(b.line_len_chars(1), 3);
    }

    #[test]
    fn replace_clamps_out_of_range() {
        let mut b = TextBuffer::from_str("abc");
        let change = b.replace(2..999, "Z");
        assert_eq!(b.to_string(), "abZ");
        assert_eq!(change.old_end_char, 3);
    }
}
