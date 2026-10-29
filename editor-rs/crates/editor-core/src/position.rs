//! Zero-based `(line, column)` positions.

/// A zero-based position in a document.
///
/// `column` counts **characters** (Unicode scalar values) from the start of
/// the line, not bytes and not grapheme clusters. Char columns are what the
/// rope indexes natively and what keeps cursor movement `O(1)` per step; byte
/// offsets (needed by tree-sitter and the LSP) are derived on demand from the
/// buffer, which is the only thing that knows the encoding of each line.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Point {
    pub line: usize,
    pub column: usize,
}

impl Point {
    pub const ZERO: Point = Point { line: 0, column: 0 };

    pub fn new(line: usize, column: usize) -> Self {
        Point { line, column }
    }
}

impl From<(usize, usize)> for Point {
    fn from((line, column): (usize, usize)) -> Self {
        Point { line, column }
    }
}
