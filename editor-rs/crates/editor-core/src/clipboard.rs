//! Clipboard abstraction.
//!
//! `editor-core` must stay UI- and OS-independent, so it does not talk to the
//! system clipboard directly (that needs a display connection on Linux and
//! would pull a GUI dependency into the core). Instead it defines a trait; the
//! UI layer provides a real system-clipboard implementation, while tests and
//! headless tools use [`MemoryClipboard`].

/// Something that can hold cut/copied text.
///
/// Implementations may talk to the OS clipboard. Because that *can* block on
/// some platforms, the UI layer is expected to call these off the typing hot
/// path (e.g. copy/paste are discrete commands, never per-keystroke).
pub trait Clipboard {
    /// Store `text`, replacing any previous contents. Clears line mode.
    fn set_text(&mut self, text: String);

    /// Store `text` as a whole-line cut/copy (see [`is_line_mode`]). The
    /// default forwards to [`set_text`], losing the line-mode flag; real
    /// clipboards override it to remember it.
    ///
    /// [`is_line_mode`]: Clipboard::is_line_mode
    /// [`set_text`]: Clipboard::set_text
    fn set_line_text(&mut self, text: String) {
        self.set_text(text);
    }

    /// Retrieve the current contents, or `None` if empty/unavailable.
    fn text(&self) -> Option<String>;

    /// Whether the last write was a full-line cut/copy. VS Code uses this so
    /// that pasting a line copied with no selection inserts it as a whole line
    /// above the caret rather than mid-line.
    fn is_line_mode(&self) -> bool {
        false
    }
}

/// A simple in-process clipboard used by tests and headless tools.
#[derive(Clone, Debug, Default)]
pub struct MemoryClipboard {
    text: Option<String>,
    line_mode: bool,
}

impl MemoryClipboard {
    pub fn new() -> Self {
        MemoryClipboard::default()
    }

    pub fn set_line(&mut self, text: String) {
        self.text = Some(text);
        self.line_mode = true;
    }
}

impl Clipboard for MemoryClipboard {
    fn set_text(&mut self, text: String) {
        self.text = Some(text);
        self.line_mode = false;
    }

    fn set_line_text(&mut self, text: String) {
        self.set_line(text);
    }

    fn text(&self) -> Option<String> {
        self.text.clone()
    }

    fn is_line_mode(&self) -> bool {
        self.line_mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get() {
        let mut c = MemoryClipboard::new();
        assert_eq!(c.text(), None);
        c.set_text("hi".into());
        assert_eq!(c.text().as_deref(), Some("hi"));
        assert!(!c.is_line_mode());
    }

    #[test]
    fn line_mode_flag() {
        let mut c = MemoryClipboard::new();
        c.set_line("whole line\n".into());
        assert!(c.is_line_mode());
        c.set_text("x".into());
        assert!(!c.is_line_mode());
    }
}
