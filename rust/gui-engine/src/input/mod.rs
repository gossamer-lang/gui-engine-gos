// input: keyboard shortcuts. A shortcut is one or more key strokes (a
// chord like Ctrl+K Ctrl+C) named by logical keys, so bindings follow the
// user's layout; `Primary` is Command on macOS and Ctrl elsewhere. AltGr
// combinations never match Ctrl+Alt bindings, so third-level characters
// on European layouts stay typeable.

use crate::strext::StrExt;
use windowing::{Key, Mods};

/// Whether this build targets macOS.
#[cfg(target_os = "macos")]
pub fn platform_is_macos() -> bool {
    true
}

/// Whether this build targets macOS.
#[cfg(not(target_os = "macos"))]
pub fn platform_is_macos() -> bool {
    false
}

/// One key with modifiers. `primary` stands for the platform shortcut
/// modifier and is resolved when matching.
#[derive(Debug, Clone, PartialEq, Eq, Copy, serde::Serialize, serde::Deserialize)]
pub struct KeyStroke {
    pub key: Key,
    pub primary: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl KeyStroke {
    pub fn new(key: Key) -> KeyStroke {
        KeyStroke {
            key,
            primary: false,
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
        }
    }

    pub fn primary(key: Key) -> KeyStroke {
        KeyStroke {
            key,
            primary: true,
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
        }
    }

    pub fn with_shift(&self) -> KeyStroke {
        let mut k = *self;
        k.shift = true;
        k
    }

    pub fn with_alt(&self) -> KeyStroke {
        let mut k = *self;
        k.alt = true;
        k
    }

    pub fn with_ctrl(&self) -> KeyStroke {
        let mut k = *self;
        k.ctrl = true;
        k
    }

    /// The effective (ctrl, alt, shift, meta) this stroke requires here.
    fn required(&self) -> (bool, bool, bool, bool) {
        let mac = platform_is_macos();
        let ctrl = self.ctrl || (self.primary && !mac);
        let meta = self.meta || (self.primary && mac);
        (ctrl, self.alt, self.shift, meta)
    }

    /// Whether a key press matches this stroke exactly.
    pub fn matches(&self, key: Key, mods: Mods) -> bool {
        if key != self.key || mods.alt_gr {
            return false;
        }
        let (ctrl, alt, shift, meta) = self.required();
        mods.ctrl == ctrl && mods.alt == alt && mods.shift == shift && mods.gui == meta
    }

    /// The platform's label: `Ctrl+Shift+S`, or `⇧⌘S` on macOS.
    pub fn label(&self) -> String {
        let (ctrl, alt, shift, meta) = self.required();
        let name = key_name(self.key);
        if platform_is_macos() {
            let mut s = String::new();
            if ctrl {
                s += "\u{2303}"
            };
            if alt {
                s += "\u{2325}"
            };
            if shift {
                s += "\u{21E7}"
            };
            if meta {
                s += "\u{2318}"
            };
            return s + name;
        }
        let mut parts = vec![];
        if ctrl {
            parts.push("Ctrl")
        };
        if alt {
            parts.push("Alt")
        };
        if shift {
            parts.push("Shift")
        };
        if meta {
            parts.push("Super")
        };
        parts.push(name);
        parts.join("+")
    }
}

/// A key sequence: one stroke, or a chord of several.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Shortcut {
    pub strokes: Vec<KeyStroke>,
}

impl Shortcut {
    pub fn of(stroke: KeyStroke) -> Shortcut {
        Shortcut {
            strokes: vec![stroke],
        }
    }

    pub fn chord(strokes: &[KeyStroke]) -> Shortcut {
        Shortcut {
            strokes: strokes.to_vec(),
        }
    }

    /// `Primary+key`.
    pub fn primary(key: Key) -> Shortcut {
        Shortcut::of(KeyStroke::primary(key))
    }

    pub fn plain(key: Key) -> Shortcut {
        Shortcut::of(KeyStroke::new(key))
    }

    /// Parses text such as `Primary+Shift+S`, `Ctrl+K Ctrl+C`, `F5`.
    pub fn parse(text: &str) -> Option<Shortcut> {
        let mut strokes = vec![];
        for part in text.split_whitespace() {
            let mut stroke = KeyStroke::new(Key::Unknown);
            let pieces = part.split("+").collect::<Vec<_>>();
            let mut i = 0;
            while i < pieces.len() {
                let p = pieces[i].to_lowercase();
                if i + 1 < pieces.len() {
                    match p.as_str() {
                        "primary" | "mod" | "cmdorctrl" => stroke.primary = true,
                        "ctrl" | "control" => stroke.ctrl = true,
                        "alt" | "option" => stroke.alt = true,
                        "shift" => stroke.shift = true,
                        "super" | "meta" | "cmd" | "command" | "win" => stroke.meta = true,
                        _ => return None,
                    }
                } else {
                    stroke.key = key_from_name(pieces[i])?;
                }
                i += 1;
            }
            if stroke.key == Key::Unknown {
                return None;
            }
            strokes.push(stroke)
        }
        if strokes.is_empty() {
            None
        } else {
            Some(Shortcut { strokes })
        }
    }

    pub fn label(&self) -> String {
        self.strokes
            .iter()
            .map(|s| s.label())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn is_chord(&self) -> bool {
        self.strokes.len() > 1
    }

    /// Whether `pressed` (the strokes so far, as matched keys) is this
    /// shortcut exactly, or a proper prefix of it.
    pub fn progress(&self, pressed: &[(Key, Mods)]) -> (bool, bool) {
        if pressed.len() > self.strokes.len() {
            return (false, false);
        }
        let mut i = 0;
        while i < pressed.len() {
            let (k, m) = pressed[i];
            if !self.strokes[i].matches(k, m) {
                return (false, false);
            }
            i += 1;
        }
        (
            pressed.len() == self.strokes.len(),
            pressed.len() < self.strokes.len(),
        )
    }
}

/// Whether a stroke would produce text in an editable context (no
/// Ctrl/Alt/Meta), so chords never start with it there.
pub fn produces_text(key: Key, mods: Mods) -> bool {
    if mods.ctrl || mods.gui || (mods.alt && !mods.alt_gr) {
        return false;
    }
    let name = key_name(key);
    name.byte_len() == 1 || key == Key::Space
}

/// A readable key name for labels.
pub fn key_name(key: Key) -> &'static str {
    match key {
        Key::Return => "Enter",
        Key::Escape => "Esc",
        Key::Backspace => "Backspace",
        Key::Tab => "Tab",
        Key::Space => "Space",
        Key::Delete => "Delete",
        Key::Insert => "Insert",
        Key::Home => "Home",
        Key::End => "End",
        Key::PageUp => "PageUp",
        Key::PageDown => "PageDown",
        Key::Left => "Left",
        Key::Right => "Right",
        Key::Up => "Up",
        Key::Down => "Down",
        Key::Num0 => "0",
        Key::Num1 => "1",
        Key::Num2 => "2",
        Key::Num3 => "3",
        Key::Num4 => "4",
        Key::Num5 => "5",
        Key::Num6 => "6",
        Key::Num7 => "7",
        Key::Num8 => "8",
        Key::Num9 => "9",
        Key::A => "A",
        Key::B => "B",
        Key::C => "C",
        Key::D => "D",
        Key::E => "E",
        Key::F => "F",
        Key::G => "G",
        Key::H => "H",
        Key::I => "I",
        Key::J => "J",
        Key::K => "K",
        Key::L => "L",
        Key::M => "M",
        Key::N => "N",
        Key::O => "O",
        Key::P => "P",
        Key::Q => "Q",
        Key::R => "R",
        Key::S => "S",
        Key::T => "T",
        Key::U => "U",
        Key::V => "V",
        Key::W => "W",
        Key::X => "X",
        Key::Y => "Y",
        Key::Z => "Z",
        Key::F1 => "F1",
        Key::F2 => "F2",
        Key::F3 => "F3",
        Key::F4 => "F4",
        Key::F5 => "F5",
        Key::F6 => "F6",
        Key::F7 => "F7",
        Key::F8 => "F8",
        Key::F9 => "F9",
        Key::F10 => "F10",
        Key::F11 => "F11",
        Key::F12 => "F12",
        Key::Menu | Key::Application => "Menu",
        Key::Char('=') => "=",
        Key::Char('-') => "-",
        Key::Char(',') => ",",
        Key::Char('.') => ".",
        Key::Char('/') => "/",
        Key::Char(';') => ";",
        Key::Char('\'') => "'",
        Key::Char('[') => "[",
        Key::Char(']') => "]",
        Key::Char('\\') => "\\",
        Key::Char('`') => "`",
        Key::KpPlus => "NumPlus",
        Key::KpMinus => "NumMinus",
        _ => "?",
    }
}

/// The key named `name` (case-insensitive), as `key_name` writes it.
pub fn key_from_name(name: &str) -> Option<Key> {
    let n = name.to_lowercase();
    let named = match n.as_str() {
        "enter" | "return" => Some(Key::Return),
        "esc" | "escape" => Some(Key::Escape),
        "backspace" => Some(Key::Backspace),
        "tab" => Some(Key::Tab),
        "space" => Some(Key::Space),
        "delete" | "del" => Some(Key::Delete),
        "insert" | "ins" => Some(Key::Insert),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" => Some(Key::PageUp),
        "pagedown" => Some(Key::PageDown),
        "left" => Some(Key::Left),
        "right" => Some(Key::Right),
        "up" => Some(Key::Up),
        "down" => Some(Key::Down),
        "menu" => Some(Key::Menu),
        "equals" => Some(Key::Char('=')),
        "minus" => Some(Key::Char('-')),
        "numplus" | "plus" | "+" => Some(Key::KpPlus),
        "numminus" => Some(Key::KpMinus),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    // The punctuation keys `key_name` spells, by their character.
    let mut chars = n.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && "=-,./;'[]\\`".contains(c)
    {
        return Some(Key::Char(c));
    }
    // Letters, digits, and function keys by their label.
    let all = vec![
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
        Key::Num0,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
    ];
    all.into_iter().find(|&k| key_name(k).to_lowercase() == n)
}

#[cfg(test)]
mod tests {
    use super::{KeyStroke, Shortcut, produces_text};

    use windowing::Mods;

    fn ctrl() -> Mods {
        let mut m = Mods::default();
        if super::platform_is_macos() {
            m.gui = true
        } else {
            m.ctrl = true
        };
        m
    }

    #[test]
    fn primary_matches_the_platform_modifier_only() {
        let s = KeyStroke::primary(windowing::Key::S);
        assert!(s.matches(windowing::Key::S, ctrl()));
        assert!(!s.matches(windowing::Key::S, Mods::default()));
        let mut both = ctrl();
        both.shift = true;
        assert!(!s.matches(windowing::Key::S, both));
        let mut altgr = ctrl();
        altgr.alt_gr = true;
        assert!(!s.matches(windowing::Key::S, altgr))
    }

    #[test]
    fn the_equals_and_minus_keys_and_their_keypad_twins_name_themselves_apart() {
        use windowing::Key;
        for key in [Key::Char('='), Key::Char('-'), Key::KpPlus, Key::KpMinus] {
            assert_eq!(super::key_from_name(super::key_name(key)), Some(key));
        }
        let zoom = Shortcut::parse("Ctrl+Shift+=").unwrap();
        let mods = Mods {
            ctrl: true,
            shift: true,
            ..Mods::default()
        };
        assert!(zoom.strokes[0].matches(Key::Char('='), mods));
        assert!(!zoom.strokes[0].matches(Key::KpPlus, mods));
    }

    #[test]
    fn parse_and_label_round_trip() {
        let s = Shortcut::parse("Primary+Shift+S").unwrap();
        // macOS labels draw the modifiers as symbols, which are not spellings.
        if super::platform_is_macos() {
            assert_eq!(s.label(), "\u{21E7}\u{2318}S");
        } else {
            assert_eq!(
                Shortcut::parse(&s.label().replace("Ctrl", "Primary")),
                Some(s)
            );
        }
        let chord = Shortcut::parse("Ctrl+K Ctrl+C").unwrap();
        assert!(chord.is_chord());
        assert_eq!(chord.strokes.len(), 2);
        assert!(Shortcut::parse("Hyper+Q").is_none());
        assert!(Shortcut::parse("").is_none())
    }

    #[test]
    fn chord_progress_reports_prefixes() {
        let chord = Shortcut::parse("Ctrl+K Ctrl+C").unwrap();
        let m = Mods {
            ctrl: true,
            ..Mods::default()
        };
        let (exact, prefix) = chord.progress(&[(windowing::Key::K, m)]);
        assert!(!exact && prefix);
        let (exact2, prefix2) = chord.progress(&[(windowing::Key::K, m), (windowing::Key::C, m)]);
        assert!(exact2 && !prefix2);
        let (e3, p3) = chord.progress(&[(windowing::Key::X, m)]);
        assert!(!e3 && !p3)
    }

    #[test]
    fn plain_letters_produce_text() {
        assert!(produces_text(windowing::Key::A, Mods::default()));
        assert!(!produces_text(windowing::Key::A, ctrl()));
        assert!(!produces_text(windowing::Key::F5, Mods::default()))
    }
}
