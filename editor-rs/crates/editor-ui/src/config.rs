//! Default configuration: the bundled keybindings and command set, plus
//! loading/merging of user overrides.

use crate::command::{Command, CommandRegistry};
use crate::keymap::{KeyBinding, Keymap};

/// The default keybindings, embedded at compile time.
pub const DEFAULT_KEYBINDINGS_JSON: &str = include_str!("../assets/keybindings.default.json");

/// Parse the bundled default keybindings into a [`Keymap`].
pub fn default_keymap() -> Keymap {
    Keymap::from_json(DEFAULT_KEYBINDINGS_JSON)
        .expect("bundled default keybindings are valid JSON")
}

/// Parse the bundled defaults, then append the user's bindings so they take
/// precedence (the keymap resolves "last match wins").
pub fn keymap_with_user_overrides(user_json: &str) -> serde_json::Result<Keymap> {
    let mut bindings: Vec<KeyBinding> = serde_json::from_str(DEFAULT_KEYBINDINGS_JSON)
        .expect("bundled default keybindings are valid JSON");
    let mut user: Vec<KeyBinding> = serde_json::from_str(user_json)?;
    bindings.append(&mut user);
    Ok(Keymap::from_bindings(&bindings))
}

/// The set of built-in commands, matching the ids used in the default keymap.
pub fn default_commands() -> CommandRegistry {
    let mut r = CommandRegistry::new();
    let cmds = [
        ("workbench.action.showCommands", "Show All Commands", None),
        ("workbench.action.quickOpen", "Go to File…", None),
        ("workbench.action.findInFiles", "Find in Files", Some("Search")),
        ("actions.find", "Find", Some("Edit")),
        ("workbench.action.toggleSidebar", "Toggle Sidebar", Some("View")),
        ("workbench.action.files.save", "Save", Some("File")),
        ("workbench.action.closeActiveEditor", "Close Editor", Some("View")),
        ("workbench.action.nextEditor", "Next Editor", Some("View")),
        ("workbench.action.previousEditor", "Previous Editor", Some("View")),
        ("undo", "Undo", Some("Edit")),
        ("redo", "Redo", Some("Edit")),
        ("editor.action.clipboardCutAction", "Cut", Some("Edit")),
        ("editor.action.clipboardCopyAction", "Copy", Some("Edit")),
        ("editor.action.clipboardPasteAction", "Paste", Some("Edit")),
        ("editor.action.selectAll", "Select All", Some("Edit")),
        ("editor.action.insertCursorBelow", "Add Cursor Below", Some("Edit")),
        ("editor.action.insertCursorAbove", "Add Cursor Above", Some("Edit")),
        (
            "workbench.action.openGlobalKeybindings",
            "Open Keyboard Shortcuts",
            Some("Preferences"),
        ),
    ];
    for (id, title, category) in cmds {
        let mut cmd = Command::new(id, title);
        if let Some(cat) = category {
            cmd = cmd.with_category(cat);
        }
        r.register(cmd);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Context, KeyStroke, Resolution};

    #[test]
    fn default_keymap_parses_and_resolves() {
        let km = default_keymap();
        let r = km.resolve(
            &[KeyStroke::parse("ctrl+shift+p").unwrap()],
            &Context::new(),
        );
        assert_eq!(
            r,
            Resolution::Matched("workbench.action.showCommands".into())
        );
    }

    #[test]
    fn every_default_binding_has_a_command() {
        // Keeps the keymap and the command registry in sync.
        let bindings: Vec<KeyBinding> =
            serde_json::from_str(DEFAULT_KEYBINDINGS_JSON).unwrap();
        let registry = default_commands();
        // These command ids are editing/cursor primitives handled directly by
        // the editor view rather than listed in the palette.
        let editor_primitives = [
            "cursorWordLeft",
            "cursorWordRight",
            "cursorWordLeftSelect",
            "cursorWordRightSelect",
            "cursorHome",
            "cursorEnd",
            "cursorTop",
            "cursorBottom",
            "deleteWordLeft",
            "deleteWordRight",
            "workbench.action.closeQuickOpen",
        ];
        for b in &bindings {
            if editor_primitives.contains(&b.command.as_str()) {
                continue;
            }
            assert!(
                registry.find(&b.command).is_some(),
                "command `{}` bound in defaults but not registered",
                b.command
            );
        }
    }

    #[test]
    fn user_override_wins() {
        let user = r#"[{"key":"ctrl+shift+p","command":"my.customPalette"}]"#;
        let km = keymap_with_user_overrides(user).unwrap();
        let r = km.resolve(
            &[KeyStroke::parse("ctrl+shift+p").unwrap()],
            &Context::new(),
        );
        assert_eq!(r, Resolution::Matched("my.customPalette".into()));
    }
}
