//! A terminal view's keyboard modes. In copy mode a cursor of its own moves
//! over the screen and the history to select and copy text; with link hints,
//! each link on the screen wears a short label that a key or two opens or
//! copies. While a mode is on, keys go to it rather than the program.

use super::links::Link;

/// A position in the buffer: a stable row (see `Engine::stable_top`) and a
/// column.
pub type Place = (isize, u16);

pub enum Mode {
    Copy(Copy),
    Hints(Hints),
}

pub struct Copy {
    pub cursor: Place,
    /// Where the selection started, while one is being made.
    pub anchor: Option<Place>,
}

pub struct Hints {
    /// Copy the chosen link rather than open it.
    pub copy: bool,
    pub labels: Vec<(String, Link)>,
    /// The keys typed so far, the start of a label.
    pub typed: String,
}

// Home row first, so most links take keys under the fingers.
const HINT_KEYS: &[u8] = b"asdfghjklqwertyuiopzxcvbnm";

/// Labels for `n` links: single keys while they suffice, then pairs, so no
/// label begins another.
pub fn hint_labels(n: usize) -> Vec<String> {
    let keys = HINT_KEYS.len();
    if n <= keys {
        return HINT_KEYS[..n]
            .iter()
            .map(|&k| char::from(k).to_string())
            .collect();
    }
    let mut out = Vec::with_capacity(n);
    'pairs: for &a in HINT_KEYS {
        for &b in HINT_KEYS {
            if out.len() == n {
                break 'pairs;
            }
            out.push(format!("{}{}", char::from(a), char::from(b)));
        }
    }
    out
}

/// What a typed key does to the hints: still a label's start, a label, or
/// none.
pub enum Pick<'a> {
    Partial,
    Chosen(&'a Link),
    Unknown,
}

pub fn pick<'a>(hints: &'a Hints, typed: &str) -> Pick<'a> {
    let mut started = false;
    for (label, link) in &hints.labels {
        if label == typed {
            return Pick::Chosen(link);
        }
        started |= label.starts_with(typed);
    }
    if started {
        Pick::Partial
    } else {
        Pick::Unknown
    }
}

fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// The column of the next word's start after `col` in `text`, a row's cells
/// as text; `None` when the row has no later word.
pub fn next_word(text: &str, col: u16) -> Option<u16> {
    let cells: Vec<char> = text.chars().collect();
    let mut i = usize::from(col);
    let start = class(*cells.get(i)?);
    while i < cells.len() && class(cells[i]) == start {
        i += 1;
    }
    while i < cells.len() && class(cells[i]) == 0 {
        i += 1;
    }
    (i < cells.len()).then_some(i as u16)
}

/// The column of the word's start before `col` in `text`; `None` when the
/// row has none before it.
pub fn previous_word(text: &str, col: u16) -> Option<u16> {
    let cells: Vec<char> = text.chars().collect();
    let mut i = usize::from(col).min(cells.len());
    while i > 0 && class(cells[i - 1]) == 0 {
        i -= 1;
    }
    if i == 0 {
        return None;
    }
    let word = class(cells[i - 1]);
    while i > 0 && class(cells[i - 1]) == word {
        i -= 1;
    }
    Some(i as u16)
}

/// The last column of `text` that holds more than a blank.
pub fn line_end(text: &str) -> u16 {
    text.trim_end().chars().count().saturating_sub(1) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_single_keys_until_they_run_out_then_pairs_none_a_prefix_of_another() {
        assert_eq!(hint_labels(3), ["a", "s", "d"]);
        let many = hint_labels(30);
        assert_eq!(many.len(), 30);
        assert!(many.iter().all(|l| l.len() == 2));
        for a in &many {
            assert_eq!(many.iter().filter(|b| b.starts_with(a.as_str())).count(), 1);
        }
    }

    #[test]
    fn word_motions_stop_at_word_starts_and_line_ends() {
        let row = "cargo build --release  ok";
        assert_eq!(next_word(row, 0), Some(6));
        assert_eq!(next_word(row, 6), Some(12));
        assert_eq!(next_word(row, 12), Some(14));
        assert_eq!(next_word(row, 23), None);
        assert_eq!(previous_word(row, 12), Some(6));
        assert_eq!(previous_word(row, 6), Some(0));
        assert_eq!(previous_word(row, 0), None);
        assert_eq!(line_end(row), 24);
    }
}
