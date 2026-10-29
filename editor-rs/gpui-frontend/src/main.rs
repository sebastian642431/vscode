//! The GPUI desktop shell for editor-rs.
//!
//! **This crate is intentionally excluded from the core Cargo workspace** and
//! is not built by the headless CI in this repository, because GPUI requires a
//! GPU, a display server and the platform graphics stack. It is the real
//! window: it renders the [`editor_ui::Workspace`] model and routes key events
//! through the keymap into [`editor_ui::Workspace::execute`].
//!
//! The code targets the GPUI revision pinned in `Cargo.toml`. GPUI's API
//! evolves quickly; when bumping the pin you may need small adjustments to the
//! element builder calls below. The *architecture* — a `Workspace` owned by a
//! GPUI view, key events resolved by the keymap, syntax highlighting computed
//! per viewport on the change stream — is stable regardless of those details.
//!
//! Run with `cargo run --release` from this directory, optionally passing a
//! folder or file to open.

mod theme;

use std::path::PathBuf;

use gpui::{
    div, px, rgb, Application, Bounds, Context, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, Styled, Window, WindowBounds, WindowOptions,
};

use editor_core::Movement;
use editor_syntax::{highlight_name, HighlightConfigs, Highlighter, Language};
use editor_ui::keymap::{Context as KeyContext, KeyStroke, Resolution};
use editor_ui::Workspace;

use crate::theme::{token_color, Theme};

/// How many recent strokes to retain while matching a multi-key chord.
const MAX_CHORD_LEN: usize = 4;

/// The root GPUI view: owns the whole editor.
struct EditorApp {
    workspace: Workspace,
    theme: Theme,
    focus: FocusHandle,
    /// Syntax-highlight configurations, built once.
    highlight_configs: HighlightConfigs,
    /// Strokes typed so far for an in-progress chord.
    pending_strokes: Vec<KeyStroke>,
}

impl EditorApp {
    fn new(initial: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut workspace = Workspace::new();
        match initial {
            Some(path) if path.is_dir() => workspace.open_folder(path),
            Some(path) => {
                workspace.open_folder(path.parent().unwrap_or(&path).to_path_buf());
                let _ = workspace.open_file(&path);
            }
            None => {
                workspace.new_untitled();
            }
        }
        EditorApp {
            workspace,
            theme: Theme::default(),
            focus: cx.focus_handle(),
            highlight_configs: HighlightConfigs::new(),
            pending_strokes: Vec::new(),
        }
    }

    /// Translate a GPUI key event into our [`KeyStroke`], if it is a key we
    /// model. GPUI already decodes modifiers and names keys; we normalise.
    fn to_stroke(event: &KeyDownEvent) -> Option<KeyStroke> {
        let k = &event.keystroke;
        let key = k.key.to_ascii_lowercase();
        if key.is_empty() {
            return None;
        }
        Some(KeyStroke {
            ctrl: k.modifiers.control,
            shift: k.modifiers.shift,
            alt: k.modifiers.alt,
            cmd: k.modifiers.platform,
            key,
        })
    }

    /// The editor context flags for `when`-clause evaluation.
    fn key_context(&self) -> KeyContext {
        KeyContext::new().with("editorFocus")
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(stroke) = Self::to_stroke(event) else {
            return;
        };

        self.pending_strokes.push(stroke.clone());
        if self.pending_strokes.len() > MAX_CHORD_LEN {
            self.pending_strokes.remove(0);
        }

        let ctx = self.key_context();
        match self
            .workspace
            .keymap()
            .resolve(&self.pending_strokes, &ctx)
        {
            Resolution::Matched(command) => {
                self.pending_strokes.clear();
                self.workspace.execute(&command);
                self.after_edit();
                cx.notify();
            }
            Resolution::Pending => {
                // Wait for the rest of the chord.
            }
            Resolution::None => {
                self.pending_strokes.clear();
                // Unbound printable key: insert it as text.
                if is_printable(&stroke) {
                    if let Some(doc) = self.workspace.active_document_mut() {
                        if stroke.key == "enter" || stroke.key == "return" {
                            doc.document.insert_newline();
                        } else if stroke.key == "tab" {
                            doc.document.insert("    ");
                        } else if stroke.key == "backspace" {
                            doc.document.backspace();
                        } else if let Some(ch) = printable_char(&stroke) {
                            doc.document.insert(&ch.to_string());
                        } else {
                            self.handle_bare_navigation(&stroke);
                        }
                    }
                } else {
                    self.handle_bare_navigation(&stroke);
                }
                self.after_edit();
                cx.notify();
            }
        }
    }

    /// Arrow keys / home / end without an explicit binding.
    fn handle_bare_navigation(&mut self, stroke: &KeyStroke) {
        let Some(doc) = self.workspace.active_document_mut() else {
            return;
        };
        let movement = match stroke.key.as_str() {
            "left" => Movement::CharLeft,
            "right" => Movement::CharRight,
            "up" => Movement::Up,
            "down" => Movement::Down,
            "home" => Movement::LineFirstNonWhitespace,
            "end" => Movement::LineEnd,
            "delete" => {
                doc.document.delete(editor_core::Granularity::Character, true);
                return;
            }
            _ => return,
        };
        doc.document.move_cursors(movement, stroke.shift);
    }

    /// Drain the active document's change stream to the syntax worker. In this
    /// single-threaded shell we reparse inline; a production build would send
    /// the changes to a background thread.
    fn after_edit(&mut self) {
        if let Some(doc) = self.workspace.active_document_mut() {
            let _changes = doc.document.drain_changes();
            // Incremental syntax/LSP updates would be dispatched here using
            // `_changes` (each carries byte offsets + points). Highlighting of
            // the visible slice is recomputed in `render`.
        }
    }
}

fn is_printable(stroke: &KeyStroke) -> bool {
    !stroke.ctrl
        && !stroke.cmd
        && matches!(
            stroke.key.as_str(),
            "enter" | "return" | "tab" | "backspace" | "space"
        )
        || printable_char(stroke).is_some()
}

fn printable_char(stroke: &KeyStroke) -> Option<char> {
    if stroke.ctrl || stroke.cmd || stroke.alt {
        return None;
    }
    if stroke.key == "space" {
        return Some(' ');
    }
    let mut chars = stroke.key.chars();
    let c = chars.next()?;
    if chars.next().is_none() && !c.is_control() {
        Some(c)
    } else {
        None
    }
}

impl Focusable for EditorApp {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for EditorApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;

        div()
            .track_focus(&self.focus)
            .key_context("Editor")
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.text)
            .font_family("monospace")
            .text_size(px(14.))
            .child(self.render_body())
            .child(self.render_status_bar())
    }
}

impl EditorApp {
    fn render_body(&self) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .flex_grow()
            .min_h(px(0.))
            .child(self.render_sidebar())
            .child(self.render_editor_pane())
    }

    fn render_sidebar(&self) -> impl IntoElement {
        let theme = &self.theme;
        let mut col = div()
            .flex()
            .flex_col()
            .w(px(240.))
            .h_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_color(theme.text_dim)
                    .child("EXPLORER"),
            );

        if let Some(tree) = self.workspace.tree() {
            if let Ok(entries) = tree.read_root() {
                for entry in entries {
                    let label = if entry.is_dir() {
                        format!("▸ {}", entry.name)
                    } else {
                        format!("   {}", entry.name)
                    };
                    col = col.child(div().px_3().py_0p5().child(label));
                }
            }
        }
        col
    }

    fn render_editor_pane(&self) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_grow()
            .h_full()
            .child(self.render_tab_bar())
            .child(self.render_editor())
    }

    fn render_tab_bar(&self) -> impl IntoElement {
        let theme = &self.theme;
        let active = self.workspace.tabs().active_index();
        let mut bar = div()
            .flex()
            .flex_row()
            .h(px(35.))
            .bg(theme.panel)
            .border_b_1()
            .border_color(theme.border);

        for (i, tab) in self.workspace.tabs().tabs().iter().enumerate() {
            let is_active = Some(i) == active;
            let title = if tab.dirty {
                format!("● {}", tab.title)
            } else {
                tab.title.clone()
            };
            bar = bar.child(
                div()
                    .px_3()
                    .flex()
                    .items_center()
                    .bg(if is_active {
                        theme.tab_active
                    } else {
                        theme.tab_inactive
                    })
                    .border_r_1()
                    .border_color(theme.border)
                    .text_color(if is_active { theme.text } else { theme.text_dim })
                    .child(title),
            );
        }
        bar
    }

    fn render_editor(&self) -> impl IntoElement {
        let theme = &self.theme;
        let container = div()
            .flex()
            .flex_col()
            .flex_grow()
            .min_h(px(0.))
            .bg(theme.background)
            .px_3()
            .py_2()
            .overflow_hidden();

        let Some(doc) = self.workspace.active_document() else {
            return container.child(div().text_color(theme.text_dim).child("No file open"));
        };

        let text = doc.document.text();
        let language = doc.language;

        // Render up to a viewport's worth of lines, each syntax-highlighted.
        let mut body = container;
        for (line_idx, line) in text.lines().take(200).enumerate() {
            body = body.child(self.render_line(line_idx, line, language));
        }
        body
    }

    fn render_line(&self, line_idx: usize, line: &str, language: Option<Language>) -> impl IntoElement {
        let theme = &self.theme;
        let row = div()
            .flex()
            .flex_row()
            .child(
                div()
                    .w(px(48.))
                    .flex_shrink_0()
                    .text_color(theme.text_dim)
                    .child(format!("{:>4}", line_idx + 1)),
            );

        // Highlight just this line (a stand-in for true viewport slicing).
        let spans = match language {
            Some(lang) => {
                let mut hl = Highlighter::new();
                hl.highlight(self.highlight_configs.get(lang), line)
            }
            None => Vec::new(),
        };

        if spans.is_empty() {
            return row.child(div().child(line.to_string()));
        }

        let mut text_row = div().flex().flex_row();
        let mut cursor = 0usize;
        for span in spans {
            if span.range.start > cursor {
                let gap = line[cursor..span.range.start].to_string();
                text_row = text_row.child(div().child(gap));
            }
            let token = line[span.range.clone()].to_string();
            let color = span
                .highlight
                .and_then(highlight_name)
                .map(|name| token_color(name, theme))
                .unwrap_or(theme.text);
            text_row = text_row.child(div().text_color(color).child(token));
            cursor = span.range.end;
        }
        if cursor < line.len() {
            text_row = text_row.child(div().child(line[cursor..].to_string()));
        }
        row.child(text_row)
    }

    fn render_status_bar(&self) -> impl IntoElement {
        let theme = &self.theme;
        let status = match self.workspace.active_document() {
            Some(doc) => {
                let sel = doc.document.selections().primary();
                let point = doc.document.buffer().char_to_point(sel.head);
                let lang = doc.language.map(|l| l.id()).unwrap_or("plain text");
                format!(
                    "Ln {}, Col {}   {}   {} cursors",
                    point.line + 1,
                    point.column + 1,
                    lang,
                    doc.document.selections().len()
                )
            }
            None => "ready".to_string(),
        };
        div()
            .flex()
            .items_center()
            .h(px(22.))
            .px_3()
            .bg(theme.accent)
            .text_color(rgb(0xffffff))
            .child(status)
    }
}

fn main() {
    let initial = std::env::args().nth(1).map(PathBuf::from);

    Application::new().run(move |cx| {
        let bounds = Bounds::centered(None, gpui::size(px(1100.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| EditorApp::new(initial.clone(), cx));
                window.focus(&view.focus_handle(cx));
                view
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
