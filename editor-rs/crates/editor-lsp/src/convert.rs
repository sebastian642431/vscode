//! Conversions between editor-rs coordinates and LSP coordinates.
//!
//! The subtle part is **position encoding**. LSP historically measures a
//! position's `character` in UTF-16 code units; LSP 3.17 lets the client and
//! server negotiate UTF-8 or UTF-32 instead. editor-core works in Unicode
//! scalar values (chars) and bytes. All three conversions are computed from the
//! rope, which is the only thing that knows each line's encoding, so positions
//! are correct for non-ASCII text — not just ASCII.

use editor_core::interface::{Diagnostic, DiagnosticSeverity, TextRange};
use editor_core::Point;
use ropey::Rope;

/// The position encoding in force for a connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PositionEncoding {
    /// `character` counts UTF-8 bytes.
    Utf8,
    /// `character` counts UTF-16 code units (the LSP default).
    Utf16,
    /// `character` counts Unicode scalar values.
    Utf32,
}

impl PositionEncoding {
    pub fn as_lsp(self) -> lsp_types::PositionEncodingKind {
        match self {
            PositionEncoding::Utf8 => lsp_types::PositionEncodingKind::UTF8,
            PositionEncoding::Utf16 => lsp_types::PositionEncodingKind::UTF16,
            PositionEncoding::Utf32 => lsp_types::PositionEncodingKind::UTF32,
        }
    }

    pub fn from_lsp(kind: &lsp_types::PositionEncodingKind) -> PositionEncoding {
        if *kind == lsp_types::PositionEncodingKind::UTF8 {
            PositionEncoding::Utf8
        } else if *kind == lsp_types::PositionEncodingKind::UTF32 {
            PositionEncoding::Utf32
        } else {
            PositionEncoding::Utf16
        }
    }
}

/// Convert a char offset in `rope` to an LSP [`lsp_types::Position`].
pub fn char_to_lsp(rope: &Rope, char_idx: usize, enc: PositionEncoding) -> lsp_types::Position {
    let char_idx = char_idx.min(rope.len_chars());
    let line = rope.char_to_line(char_idx);
    let line_start = rope.line_to_char(line);
    let character = match enc {
        PositionEncoding::Utf8 => rope.char_to_byte(char_idx) - rope.char_to_byte(line_start),
        PositionEncoding::Utf16 => {
            rope.char_to_utf16_cu(char_idx) - rope.char_to_utf16_cu(line_start)
        }
        PositionEncoding::Utf32 => char_idx - line_start,
    };
    lsp_types::Position {
        line: line as u32,
        character: character as u32,
    }
}

/// Convert an LSP [`lsp_types::Position`] to a char offset in `rope`, clamping
/// out-of-range positions.
pub fn lsp_to_char(rope: &Rope, pos: lsp_types::Position, enc: PositionEncoding) -> usize {
    let line = (pos.line as usize).min(rope.len_lines().saturating_sub(1));
    let line_start = rope.line_to_char(line);
    let ch = pos.character as usize;
    match enc {
        PositionEncoding::Utf8 => {
            let line_start_byte = rope.char_to_byte(line_start);
            let target_byte = (line_start_byte + ch).min(rope.len_bytes());
            rope.byte_to_char(target_byte)
        }
        PositionEncoding::Utf16 => {
            let line_start_u16 = rope.char_to_utf16_cu(line_start);
            let total_u16 = rope.len_utf16_cu();
            let target = (line_start_u16 + ch).min(total_u16);
            rope.utf16_cu_to_char(target)
        }
        PositionEncoding::Utf32 => (line_start + ch).min(rope.len_chars()),
    }
}

/// Convert an LSP position to an editor-core [`Point`] (char column).
pub fn lsp_to_point(rope: &Rope, pos: lsp_types::Position, enc: PositionEncoding) -> Point {
    let offset = lsp_to_char(rope, pos, enc);
    rope.char_to_line(offset);
    let line = rope.char_to_line(offset);
    Point::new(line, offset - rope.line_to_char(line))
}

/// Convert an LSP diagnostic into editor-core's representation.
pub fn diagnostic_from_lsp(
    rope: &Rope,
    diag: &lsp_types::Diagnostic,
    enc: PositionEncoding,
) -> Diagnostic {
    let severity = match diag.severity {
        Some(lsp_types::DiagnosticSeverity::ERROR) => DiagnosticSeverity::Error,
        Some(lsp_types::DiagnosticSeverity::WARNING) => DiagnosticSeverity::Warning,
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => DiagnosticSeverity::Information,
        Some(lsp_types::DiagnosticSeverity::HINT) => DiagnosticSeverity::Hint,
        _ => DiagnosticSeverity::Error,
    };
    Diagnostic {
        range: TextRange::new(
            lsp_to_point(rope, diag.range.start, enc),
            lsp_to_point(rope, diag.range.end, enc),
        ),
        severity,
        message: diag.message.clone(),
        source: diag.source.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_positions_match_in_all_encodings() {
        let rope = Rope::from_str("fn main() {}\nlet x = 1;\n");
        // Offset of 'x' on line 1 (char 4 of line 1).
        let off = rope.line_to_char(1) + 4;
        for enc in [
            PositionEncoding::Utf8,
            PositionEncoding::Utf16,
            PositionEncoding::Utf32,
        ] {
            let pos = char_to_lsp(&rope, off, enc);
            assert_eq!(pos.line, 1);
            assert_eq!(pos.character, 4, "enc {enc:?}");
            assert_eq!(lsp_to_char(&rope, pos, enc), off, "roundtrip {enc:?}");
        }
    }

    #[test]
    fn non_ascii_encodings_differ_correctly() {
        // "é" is 2 bytes (UTF-8) and 1 UTF-16 unit; "𝄞" is 4 bytes, 2 UTF-16 units.
        let rope = Rope::from_str("é𝄞x");
        let off_x = 2; // char index of 'x'
        assert_eq!(char_to_lsp(&rope, off_x, PositionEncoding::Utf32).character, 2);
        assert_eq!(char_to_lsp(&rope, off_x, PositionEncoding::Utf16).character, 3); // 1 + 2
        assert_eq!(char_to_lsp(&rope, off_x, PositionEncoding::Utf8).character, 6); // 2 + 4
        // All must map back to the same char offset.
        for enc in [
            PositionEncoding::Utf8,
            PositionEncoding::Utf16,
            PositionEncoding::Utf32,
        ] {
            let pos = char_to_lsp(&rope, off_x, enc);
            assert_eq!(lsp_to_char(&rope, pos, enc), off_x, "{enc:?}");
        }
    }

    #[test]
    fn diagnostic_conversion() {
        let rope = Rope::from_str("let x = ;\n");
        let lsp_diag = lsp_types::Diagnostic {
            range: lsp_types::Range {
                start: lsp_types::Position { line: 0, character: 8 },
                end: lsp_types::Position { line: 0, character: 9 },
            },
            severity: Some(lsp_types::DiagnosticSeverity::ERROR),
            message: "expected expression".into(),
            source: Some("rustc".into()),
            ..Default::default()
        };
        let d = diagnostic_from_lsp(&rope, &lsp_diag, PositionEncoding::Utf16);
        assert_eq!(d.severity, DiagnosticSeverity::Error);
        assert_eq!(d.range.start, Point::new(0, 8));
        assert_eq!(d.source.as_deref(), Some("rustc"));
    }
}
