//! The `editor` binary.
//!
//! The real graphical editor is the GPUI shell in the `gpui-frontend` crate,
//! which needs a GPU/display. This binary is the **headless driver**: it
//! exercises the same [`editor_ui::Workspace`] model from the terminal, which
//! is useful for scripting, CI smoke-tests and running on machines without a
//! display. It never opens a window.
//!
//! Usage:
//! ```text
//! editor                 # show status and usage
//! editor <folder>        # open a folder and list its top level
//! editor <file>          # open a file: show language, lines, first tokens
//! editor --version
//! ```

use std::path::Path;
use std::process::ExitCode;

use editor_fs::EntryKind;
use editor_syntax::{HighlightConfigs, Highlighter, Language};
use editor_ui::Workspace;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None => {
            print_status();
            ExitCode::SUCCESS
        }
        Some("--version") | Some("-V") => {
            println!("editor-rs {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") => {
            print_usage();
            ExitCode::SUCCESS
        }
        Some(path) => {
            let path = Path::new(path);
            let result = if path.is_dir() {
                open_folder(path)
            } else {
                open_file(path)
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn print_status() {
    let ws = Workspace::new();
    println!("editor-rs {} — headless driver", env!("CARGO_PKG_VERSION"));
    println!(
        "  {} commands, {} default key bindings loaded",
        ws.commands().all().len(),
        // Count by reparsing the embedded defaults for display only.
        editor_ui::config::DEFAULT_KEYBINDINGS_JSON
            .matches("\"key\"")
            .count(),
    );
    println!("  supported languages: Rust, TypeScript/TSX, JSON, Python");
    println!();
    print_usage();
}

fn print_usage() {
    println!("usage:");
    println!("  editor <folder>     open a folder and list its top level");
    println!("  editor <file>       open a file: language, line count, first tokens");
    println!("  editor --version");
    println!();
    println!("the graphical GPUI editor lives in the gpui-frontend crate");
}

fn open_folder(path: &Path) -> std::io::Result<()> {
    let mut ws = Workspace::new();
    ws.open_folder(path);
    let tree = ws.tree().expect("folder just opened");
    println!("{}", path.display());
    for entry in tree.read_root()? {
        let marker = match entry.kind {
            EntryKind::Directory => "/",
            EntryKind::File => "",
        };
        println!("  {}{}", entry.name, marker);
    }
    Ok(())
}

fn open_file(path: &Path) -> std::io::Result<()> {
    let mut ws = Workspace::new();
    let id = ws.open_file(path)?;
    let doc = ws.document(id).expect("just opened");
    let language = doc.language;
    let lines = doc.document.buffer().len_lines();
    let bytes = doc.document.buffer().len_bytes();

    println!("{}", path.display());
    println!(
        "  language: {}",
        language.map(|l| l.id()).unwrap_or("plain text")
    );
    println!("  {lines} lines, {bytes} bytes");

    if let Some(language) = language {
        print_first_tokens(&doc.document.text(), language);
    }
    Ok(())
}

/// Highlight the first chunk of a file and print the first few coloured tokens,
/// demonstrating the syntax pipeline end to end.
fn print_first_tokens(text: &str, language: Language) {
    let configs = HighlightConfigs::new();
    let mut hl = Highlighter::new();
    // Only the first ~2 KB, as the UI would highlight a viewport slice.
    let slice_end = text
        .char_indices()
        .take_while(|(i, _)| *i < 2048)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let slice = &text[..slice_end];
    let spans = hl.highlight(configs.get(language), slice);

    println!("  first highlighted tokens:");
    let mut shown = 0;
    for span in spans {
        let Some(idx) = span.highlight else { continue };
        let token = slice[span.range].trim();
        if token.is_empty() {
            continue;
        }
        if let Some(name) = editor_syntax::highlight_name(idx) {
            println!("    {name:<20} {token}");
            shown += 1;
            if shown >= 12 {
                break;
            }
        }
    }
}
