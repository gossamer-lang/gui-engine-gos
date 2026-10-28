//! Classic xterm keyboard encoding. Extended keyboard protocols are not claimed.
#[derive(Clone, Copy, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}
#[derive(Clone, Copy)]
pub enum Key {
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Function(u8),
    Character(char),
}
pub fn encode(key: Key, mods: Mods, application_cursor: bool) -> Vec<u8> {
    let modifier = 1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl);
    let cursor = |last: char| {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{last}")
        } else {
            format!("\x1b{}{last}", if application_cursor { 'O' } else { '[' })
        }
    };
    let tilde = |n: u8| {
        if modifier > 1 {
            format!("\x1b[{n};{modifier}~")
        } else {
            format!("\x1b[{n}~")
        }
    };
    let (text, prefix) = match key {
        Key::Up => (cursor('A'), false),
        Key::Down => (cursor('B'), false),
        Key::Right => (cursor('C'), false),
        Key::Left => (cursor('D'), false),
        Key::Home => (cursor('H'), false),
        Key::End => (cursor('F'), false),
        Key::Insert => (tilde(2), false),
        Key::Delete => (tilde(3), false),
        Key::PageUp => (tilde(5), false),
        Key::PageDown => (tilde(6), false),
        Key::Enter => ("\r".into(), true),
        Key::Escape => ("\x1b".into(), true),
        Key::Backspace => (if mods.ctrl { "\x08" } else { "\x7f" }.into(), true),
        Key::Tab => (if mods.shift { "\x1b[Z" } else { "\t" }.into(), true),
        Key::Function(n @ 1..=4) => {
            let last = (b'P' + n - 1) as char;
            (
                if modifier > 1 {
                    format!("\x1b[1;{modifier}{last}")
                } else {
                    format!("\x1bO{last}")
                },
                false,
            )
        }
        Key::Function(n @ 5..=12) => (
            tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]),
            false,
        ),
        Key::Function(_) => (String::new(), false),
        Key::Character(c) => {
            let upper = c.to_ascii_uppercase();
            let text = if mods.ctrl && ('@'..='_').contains(&upper) {
                ((upper as u8) & 31) as char
            } else if mods.ctrl && c == '?' {
                127 as char
            } else if mods.ctrl && c == ' ' {
                0 as char
            } else {
                c
            };
            (text.to_string(), true)
        }
    };
    if prefix && mods.alt {
        format!("\x1b{text}").into_bytes()
    } else {
        text.into_bytes()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classic_modes_modifiers_and_control_input() {
        assert_eq!(encode(Key::Up, Mods::default(), false), b"\x1b[A");
        assert_eq!(encode(Key::Up, Mods::default(), true), b"\x1bOA");
        assert_eq!(
            encode(
                Key::Right,
                Mods {
                    ctrl: true,
                    ..Mods::default()
                },
                true
            ),
            b"\x1b[1;5C"
        );
        assert_eq!(
            encode(
                Key::Character('c'),
                Mods {
                    ctrl: true,
                    ..Mods::default()
                },
                false
            ),
            b"\x03"
        );
        assert_eq!(
            encode(
                Key::Character('界'),
                Mods {
                    alt: true,
                    ..Mods::default()
                },
                false
            ),
            "\x1b界".as_bytes()
        );
        assert_eq!(
            encode(
                Key::Tab,
                Mods {
                    shift: true,
                    ..Mods::default()
                },
                false
            ),
            b"\x1b[Z"
        );
        assert_eq!(
            encode(Key::Function(12), Mods::default(), false),
            b"\x1b[24~"
        );
    }
}
