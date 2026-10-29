//! End-to-end editing scenarios driven only through the public API.

use editor_core::{Document, Granularity, MemoryClipboard, Movement, Point, Selection, SelectionSet};

#[test]
fn write_a_small_program_then_edit_it() {
    let mut doc = Document::new();

    // Type a function, letting newlines preserve indentation.
    for ch in "fn main() {".chars() {
        doc.insert(&ch.to_string());
    }
    doc.insert_newline();
    doc.insert("    println!(\"hi\");");
    doc.insert_newline();
    doc.insert("}");

    assert_eq!(doc.text(), "fn main() {\n    println!(\"hi\");\n    }");

    // Fix the stray indentation on the closing brace: put the caret just
    // before `}` and backspace away the four leading spaces.
    doc.set_caret(Point::new(2, 4));
    for _ in 0..4 {
        doc.backspace();
    }
    assert_eq!(doc.text(), "fn main() {\n    println!(\"hi\");\n}");
}

#[test]
fn multi_cursor_rename_with_undo() {
    let mut doc = Document::from_str("x = 1\nx = 2\nx = 3");
    // Put a caret before each `x` (offsets 0, 6, 12).
    let mut set = SelectionSet::single(Selection::caret(0));
    set.add(Selection::caret(6));
    set.add(Selection::caret(12));
    doc.set_selections(set);

    // Select the `x` at each caret and replace with `count`.
    doc.move_cursors(Movement::CharRight, true);
    doc.insert("count");
    assert_eq!(doc.text(), "count = 1\ncount = 2\ncount = 3");

    // A single undo reverts all three edits.
    assert!(doc.undo());
    assert_eq!(doc.text(), "x = 1\nx = 2\nx = 3");
}

#[test]
fn cut_a_line_and_paste_it_elsewhere() {
    let mut doc = Document::from_str("one\ntwo\nthree");
    let mut clip = MemoryClipboard::new();

    // Caret on line "two" with no selection -> line-mode cut.
    doc.set_caret(Point::new(1, 1));
    doc.cut(&mut clip);
    assert_eq!(doc.text(), "one\nthree");

    // Move to the top and paste the whole line back above line 0.
    doc.set_caret(Point::new(0, 0));
    doc.paste(&clip);
    assert_eq!(doc.text(), "two\none\nthree");
}

#[test]
fn word_delete_and_navigation() {
    let mut doc = Document::from_str("alpha beta gamma");
    doc.move_cursors(Movement::BufferEnd, false);
    doc.delete(Granularity::Word, false); // delete "gamma"
    assert_eq!(doc.text(), "alpha beta ");
    doc.move_cursors(Movement::WordLeft, false);
    doc.move_cursors(Movement::WordLeft, false);
    assert_eq!(doc.selections().primary().head, 0);
}

#[test]
fn changes_stream_is_consumable_by_a_worker() {
    let mut doc = Document::from_str("abc");
    doc.set_caret(Point::new(0, 3));
    doc.insert("d");
    doc.insert("e");
    let changes = doc.drain_changes();
    // Every change carries byte + point coordinates for the syntax/LSP layer.
    assert!(changes.iter().all(|c| c.start_byte <= c.new_end_byte));
    assert_eq!(doc.text(), "abcde");
}
