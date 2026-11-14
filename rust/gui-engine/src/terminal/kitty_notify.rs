//! Kitty's desktop notifications (OSC 99): `ESC ] 99 ; metadata ; payload ST`,
//! where the metadata is `key=value` pairs split by `:`. A notification may
//! arrive in parts that share an id (`i`), each a title or body (`p`), plain
//! or base64 (`e=1`), until one marks it done (`d=1`, the default).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// The OSC number the protocol uses, as it starts the string.
pub const PREFIX: &[u8] = b"\x1b]99;";

/// The most text one notification gathers, title and body together.
const TEXT_LIMIT: usize = 8 * 1024;
/// The most notifications a terminal gathers at once.
const PENDING_LIMIT: usize = 8;
const ID_LIMIT: usize = 64;

/// What one OSC 99 string asks of the terminal.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// Bytes to answer the program with.
    Answer(Vec<u8>),
    /// A notification is complete: its title and body.
    Show(String, String),
    /// A part was taken, or the string asked for nothing the terminal does.
    Nothing,
}

/// Notifications that arrived in parts and wait for their last one.
#[derive(Default)]
pub struct Gathering {
    pending: Vec<(String, String, String)>,
}

impl Gathering {
    /// Reads the body of an OSC 99 string (after `99;`, before its end).
    pub fn read(&mut self, body: &[u8]) -> Request {
        let Ok(text) = std::str::from_utf8(body) else {
            return Request::Nothing;
        };
        let (metadata, payload) = text.split_once(';').unwrap_or((text, ""));
        let mut id = "";
        let mut done = true;
        let mut base64 = false;
        let mut kind = "title";
        for pair in metadata.split(':') {
            match pair.split_once('=') {
                Some(("i", v)) => id = v,
                Some(("d", v)) => done = v != "0",
                Some(("e", v)) => base64 = v == "1",
                Some(("p", v)) => kind = v,
                _ => {}
            }
        }
        if id.len() > ID_LIMIT
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_+.".contains(&b))
        {
            return Request::Nothing;
        }
        if kind == "?" {
            return Request::Answer(
                format!(
                    "\x1b]99;i={id}:p=?;p=title,body:o=always,unfocused,invisible:u=0,1,2\x1b\\"
                )
                .into_bytes(),
            );
        }
        let part = if base64 {
            match STANDARD
                .decode(payload)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
            {
                Some(decoded) => decoded,
                None => return Request::Nothing,
            }
        } else {
            payload.to_owned()
        };
        let at = match self.pending.iter().position(|p| p.0 == id) {
            Some(at) => at,
            None => {
                if self.pending.len() == PENDING_LIMIT {
                    self.pending.remove(0);
                }
                self.pending
                    .push((id.to_owned(), String::new(), String::new()));
                self.pending.len() - 1
            }
        };
        let entry = &mut self.pending[at];
        let room = TEXT_LIMIT.saturating_sub(entry.1.len() + entry.2.len());
        let part: String = part.chars().take(room).collect();
        match kind {
            "title" => entry.1.push_str(&part),
            "body" => entry.2.push_str(&part),
            // Closing, icons, buttons and the rest are not shown.
            _ => {}
        }
        if !done {
            return Request::Nothing;
        }
        let (_, title, body) = self.pending.remove(at);
        if title.is_empty() && body.is_empty() {
            return Request::Nothing;
        }
        Request::Show(title, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_one_part_notification_shows_its_title() {
        let mut g = Gathering::default();
        assert_eq!(
            g.read(b";Build done"),
            Request::Show("Build done".into(), String::new())
        );
    }

    #[test]
    fn parts_with_one_id_gather_until_the_last() {
        let mut g = Gathering::default();
        assert_eq!(g.read(b"i=1:d=0;Pi"), Request::Nothing);
        assert_eq!(
            g.read(b"i=1:p=body;Ready for input"),
            Request::Show("Pi".into(), "Ready for input".into())
        );
        let encoded = STANDARD.encode("Approve?");
        assert_eq!(
            g.read(format!("i=2:e=1:p=body;{encoded}").as_bytes()),
            Request::Show(String::new(), "Approve?".into())
        );
    }

    #[test]
    fn a_query_is_answered_and_bad_parts_are_dropped() {
        let mut g = Gathering::default();
        let Request::Answer(answer) = g.read(b"i=q1:p=?;") else {
            panic!("no answer")
        };
        assert!(answer.starts_with(b"\x1b]99;i=q1:p=?;p=title,body"));
        assert_eq!(g.read(b"i=b/d:p=?;"), Request::Nothing);
        assert_eq!(g.read(b"e=1;%%%"), Request::Nothing);
        assert_eq!(g.read(b"p=icon;xyz"), Request::Nothing);
    }
}
