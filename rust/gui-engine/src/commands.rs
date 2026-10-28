// commands: one registry every invoker resolves through. Menus, toolbar
// buttons, the command palette, and keyboard shortcuts name a command by
// its stable id; the registry answers its label, shortcuts, and
// enabled/checked state, so a disabled command cannot run from any of
// them. The application declares its commands from its model after every
// update (`App::commands`), so state never goes stale.
//
// Scope: a command with a non-empty `scope` is active only while focus is
// inside the widget whose key or id equals the scope; scoped commands win
// over window-level ones with the same shortcut. The focused control sees
// every command first (`Event::Command`), so text fields implement
// edit.copy and friends without the application's help.

use windowing::{Key, Mods};

use crate::input::{Shortcut, produces_text};

/// One command.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Command {
    pub id: String,
    pub label: String,
    pub shortcuts: Vec<Shortcut>,
    pub enabled: bool,
    /// -1 not checkable, 0 unchecked, 1 checked.
    pub checked: i64,
    pub visible: bool,
    pub scope: String,
    /// Grouping for the command palette ("File", "Edit", ...).
    pub category: String,
    pub description: String,
    /// Whether holding the shortcut repeats it.
    pub repeats: bool,
}

impl Command {
    pub fn new(id: &str, label: &str) -> Command {
        Command {
            id: id.to_string(),
            label: label.to_string(),
            shortcuts: vec![],
            enabled: true,
            checked: -1,
            visible: true,
            scope: String::new(),
            category: String::new(),
            description: String::new(),
            repeats: false,
        }
    }

    /// This command with a shortcut parsed from text (`Primary+S`).
    pub fn key(&self, shortcut: &str) -> Command {
        let mut c = self.clone();
        if let Some(s) = Shortcut::parse(shortcut) {
            c.shortcuts.push(s)
        }
        c
    }

    pub fn enabled(&self, enabled: bool) -> Command {
        let mut c = self.clone();
        c.enabled = enabled;
        c
    }

    /// Whether menus and the palette list the command.
    pub fn visible(&self, visible: bool) -> Command {
        let mut c = self.clone();
        c.visible = visible;
        c
    }

    pub fn checked(&self, checked: bool) -> Command {
        let mut c = self.clone();
        c.checked = if checked { 1 } else { 0 };
        c
    }

    pub fn in_scope(&self, scope: &str) -> Command {
        let mut c = self.clone();
        c.scope = scope.to_string();
        c
    }

    pub fn category(&self, category: &str) -> Command {
        let mut c = self.clone();
        c.category = category.to_string();
        c
    }

    pub fn repeating(&self) -> Command {
        let mut c = self.clone();
        c.repeats = true;
        c
    }

    /// The first shortcut's platform label, or "".
    pub fn shortcut_label(&self) -> String {
        if self.shortcuts.is_empty() {
            "".to_string()
        } else {
            self.shortcuts[0].label()
        }
    }
}

/// What a key press did to the shortcut state.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum KeyResult {
    /// No shortcut involves this key.
    NoMatch,
    /// The key started or continued a chord; wait for the next stroke.
    Pending,
    /// The shortcut for this command completed.
    Fire(String),
    /// The shortcut belongs to a disabled command; swallow the key.
    Disabled(String),
}

/// The commands of a window and the chord typed so far.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct CommandTable {
    pub commands: Vec<Command>,
    pressed: Vec<(Key, Mods)>,
    /// When the pending chord expires (ms clock), or 0.
    pub chord_deadline: i64,
}

/// How long a chord waits for its next stroke.
pub const CHORD_TIMEOUT: i64 = 1500;

/// Toolkit commands every window understands.
pub fn standard_commands() -> Vec<Command> {
    vec![
        Command::new("edit.undo", &crate::localization::tr("Undo"))
            .key("Primary+Z")
            .category(&crate::localization::tr("Edit")),
        Command::new("edit.redo", &crate::localization::tr("Redo"))
            .key("Primary+Shift+Z")
            .key("Primary+Y")
            .category(&crate::localization::tr("Edit")),
        Command::new("edit.cut", &crate::localization::tr("Cut"))
            .key("Primary+X")
            .category(&crate::localization::tr("Edit")),
        Command::new("edit.copy", &crate::localization::tr("Copy"))
            .key("Primary+C")
            .category(&crate::localization::tr("Edit")),
        Command::new("edit.paste", &crate::localization::tr("Paste"))
            .key("Primary+V")
            .category(&crate::localization::tr("Edit")),
        Command::new("edit.select-all", &crate::localization::tr("Select All"))
            .key("Primary+A")
            .category(&crate::localization::tr("Edit")),
        Command::new(
            "gui.command-palette",
            &crate::localization::tr("Command Palette"),
        )
        .key("Primary+Shift+P")
        .category(&crate::localization::tr("View")),
        Command::new(
            "gui.toggle-theme",
            &crate::localization::tr("Toggle Dark Theme"),
        )
        .category(&crate::localization::tr("View")),
        Command::new(
            "gui.inspector",
            &crate::localization::tr("Toggle Inspector"),
        )
        .key("Primary+Shift+I")
        .category(&crate::localization::tr("View")),
        Command::new("gui.quit", &crate::localization::tr("Quit"))
            .key("Primary+Q")
            .category(&crate::localization::tr("File")),
    ]
}

impl CommandTable {
    pub fn new(commands: &[Command]) -> CommandTable {
        CommandTable {
            commands: commands.to_vec(),
            pressed: vec![],
            chord_deadline: 0,
        }
    }

    /// The command `id`, if registered.
    pub fn get(&self, id: &str) -> Option<Command> {
        for c in &self.commands {
            if c.id == id {
                return Some(c.clone());
            }
        }
        None
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        match self.get(id) {
            Some(c) => c.enabled,
            None => true,
        }
    }

    /// Replaces the application's commands, keeping a pending chord.
    pub fn set(&mut self, commands: &[Command]) {
        self.commands = commands.to_vec();
    }

    pub fn reset_chord(&mut self) {
        self.pressed = vec![];
        self.chord_deadline = 0;
    }

    pub fn chord_pending(&self) -> bool {
        !self.pressed.is_empty()
    }

    /// Resolves one key press against the active commands. `scopes` lists
    /// the keys/ids on the focused path, innermost first; `editing` says
    /// focus is in an editable text control, where a chord may not start
    /// with a text-producing stroke.
    pub fn key(
        &mut self,
        key: Key,
        mods: Mods,
        repeat: bool,
        scopes: &[String],
        editing: bool,
        now: i64,
    ) -> KeyResult {
        if self.chord_deadline > 0 && now > self.chord_deadline {
            self.reset_chord()
        }
        if self.pressed.is_empty() && editing && produces_text(key, mods) {
            return KeyResult::NoMatch;
        }
        let mut seq = self.pressed.clone();
        seq.push((key, mods));
        let mut fired: Option<Command> = None;
        let mut fired_rank = 1000;
        let mut prefix = false;
        for c in &self.commands {
            let rank = scope_rank(&c.scope, scopes);
            if rank < 0 {
                continue;
            };
            for s in &c.shortcuts {
                let (exact, is_prefix) = s.progress(&seq);
                if exact && rank < fired_rank {
                    fired = Some(c.clone());
                    fired_rank = rank;
                }
                if is_prefix {
                    prefix = true
                }
            }
        }
        if let Some(c) = fired {
            self.reset_chord();
            if repeat && !c.repeats {
                return KeyResult::Disabled(c.id);
            }
            if !c.enabled {
                return KeyResult::Disabled(c.id);
            }
            return KeyResult::Fire(c.id);
        }
        if prefix {
            self.pressed = seq.to_vec();
            self.chord_deadline = now + CHORD_TIMEOUT;
            return KeyResult::Pending;
        }
        // Escape cancels a pending chord without doing anything else.
        let had = !self.pressed.is_empty();
        self.reset_chord();
        if had {
            KeyResult::Pending
        } else {
            KeyResult::NoMatch
        }
    }

    /// Pairs of commands (ids) whose shortcuts collide within one scope.
    pub fn conflicts(&self) -> Vec<(String, String)> {
        let mut out = vec![];
        let mut i = 0;
        while i < self.commands.len() {
            let mut j = i + 1;
            while j < self.commands.len() {
                let a = &self.commands[i];
                let b = &self.commands[j];
                if a.scope == b.scope {
                    for sa in &a.shortcuts {
                        for sb in &b.shortcuts {
                            if sa == sb {
                                out.push((a.id.clone(), b.id.clone()))
                            }
                        }
                    }
                }
                j += 1;
            }
            i += 1;
        }
        out
    }

    /// Commands for the palette: visible ones, enabled first, by category
    /// and label.
    pub fn palette_entries(&self) -> Vec<Command> {
        let mut out = self
            .commands
            .iter()
            .filter(|c| c.visible)
            .cloned()
            .collect::<Vec<_>>();
        out.sort_by_key(|c| {
            (
                if c.enabled { 0 } else { 1 },
                c.category.clone(),
                c.label.clone(),
            )
        });
        out
    }
}

// 0 for the innermost scope on the path, larger further out; window-level
// commands rank after every scope; -1 when the scope is not on the path.
fn scope_rank(scope: &str, scopes: &[String]) -> i64 {
    if scope.is_empty() {
        return 500;
    }
    let mut i = 0;
    while i < scopes.len() {
        if scopes[i] == scope {
            return i as i64;
        };
        i += 1;
    }
    -1
}

#[cfg(test)]
mod tests {
    use super::{Command, CommandTable, KeyResult};
    use crate::localization::{Catalog, PSEUDO_LOCALE, install};
    use windowing::Mods;

    fn primary() -> Mods {
        let mut m = Mods::default();
        if crate::input::platform_is_macos() {
            m.gui = true
        } else {
            m.ctrl = true
        };
        m
    }

    #[test]
    fn standard_command_labels_and_categories_are_translated() {
        install(&Catalog {
            locale: PSEUDO_LOCALE.to_string(),
            messages: vec![],
        });
        let _defer = scopeguard::guard((), |_| {
            install(&Catalog {
                locale: "en".to_string(),
                messages: vec![],
            });
        });
        for c in super::standard_commands() {
            assert!(
                c.label.starts_with("[") && c.category.starts_with("["),
                "{}: {} / {}",
                c.id,
                c.label,
                c.category
            )
        }
    }

    #[test]
    fn shortcuts_fire_and_disabled_commands_swallow() {
        let mut t = CommandTable::new(&[
            Command::new("file.save", "Save").key("Primary+S"),
            Command::new("file.close", "Close")
                .key("Primary+W")
                .enabled(false),
        ]);
        assert_eq!(
            t.key(windowing::Key::S, primary(), false, &[], false, 0),
            KeyResult::Fire("file.save".to_string())
        );
        assert_eq!(
            t.key(windowing::Key::W, primary(), false, &[], false, 0),
            KeyResult::Disabled("file.close".to_string())
        );
        assert_eq!(
            t.key(windowing::Key::Q, primary(), false, &[], false, 0),
            KeyResult::NoMatch
        )
    }

    #[test]
    fn chords_wait_and_time_out() {
        let mut t = CommandTable::new(&[Command::new("fold", "Fold").key("Ctrl+K Ctrl+F")]);
        let c = Mods {
            ctrl: true,
            ..Mods::default()
        };
        assert_eq!(
            t.key(windowing::Key::K, c, false, &[], false, 0),
            KeyResult::Pending
        );
        assert_eq!(
            t.key(windowing::Key::F, c, false, &[], false, 100),
            KeyResult::Fire("fold".to_string())
        );
        assert_eq!(
            t.key(windowing::Key::K, c, false, &[], false, 200),
            KeyResult::Pending
        );
        assert_eq!(
            t.key(windowing::Key::F, c, false, &[], false, 5000),
            KeyResult::NoMatch
        )
    }

    #[test]
    fn inner_scopes_win_and_outside_scopes_are_inactive() {
        let mut t = CommandTable::new(&[
            Command::new("window.find", "Find").key("Primary+F"),
            Command::new("editor.find", "Find in File")
                .key("Primary+F")
                .in_scope("editor"),
        ]);
        assert_eq!(
            t.key(
                windowing::Key::F,
                primary(),
                false,
                &["editor".to_string(), "root".to_string()],
                false,
                0
            ),
            KeyResult::Fire("editor.find".to_string())
        );
        assert_eq!(
            t.key(
                windowing::Key::F,
                primary(),
                false,
                &["sidebar".to_string()],
                false,
                0
            ),
            KeyResult::Fire("window.find".to_string())
        )
    }

    #[test]
    fn text_keys_never_start_chords_while_editing() {
        let mut t = CommandTable::new(&[Command::new("g", "Go").key("G G")]);
        assert_eq!(
            t.key(windowing::Key::G, Mods::default(), false, &[], true, 0),
            KeyResult::NoMatch
        );
        assert_eq!(
            t.key(windowing::Key::G, Mods::default(), false, &[], false, 0),
            KeyResult::Pending
        )
    }

    #[test]
    fn conflicts_are_reported() {
        let t = CommandTable::new(&[
            Command::new("a", "A").key("Primary+K"),
            Command::new("b", "B").key("Primary+K"),
        ]);
        assert_eq!(t.conflicts(), vec![("a".to_string(), "b".to_string())])
    }

    #[test]
    fn held_keys_repeat_only_repeating_commands() {
        let mut t = CommandTable::new(&[
            Command::new("zoom", "Zoom").key("Primary+1"),
            Command::new("next", "Next").key("F3").repeating(),
        ]);
        assert_eq!(
            t.key(windowing::Key::Num1, primary(), true, &[], false, 0),
            KeyResult::Disabled("zoom".to_string())
        );
        assert_eq!(
            t.key(windowing::Key::F3, Mods::default(), true, &[], false, 0),
            KeyResult::Fire("next".to_string())
        )
    }
}
