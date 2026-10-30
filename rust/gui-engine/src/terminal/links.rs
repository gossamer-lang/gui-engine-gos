//! Hyperlinks on a terminal's screen: the links programs mark with OSC 8,
//! and URLs and email addresses found in the text, read across rows a long
//! line wraps over.

use std::sync::{Arc, LazyLock};

use regex::Regex;

use super::frame::Cell;

/// The columns `start..end` of one row that a link covers. A link that wraps
/// covers one run on each of its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub row: u16,
    pub start: u16,
    pub end: u16,
    pub uri: Arc<str>,
}

impl Link {
    /// Whether the link covers the cell at `row`, `col`.
    pub fn contains(&self, row: u16, col: u16) -> bool {
        self.row == row && (self.start..self.end).contains(&col)
    }
}

// Only the schemes the desktop opener passes on are found, so every link the
// screen shows can be opened. A bare email address is a mailto link; a
// scheme earlier on the line takes the address it names first.
static URL: LazyLock<Regex> = LazyLock::new(|| {
    // SAFETY: the pattern is a constant and always compiles.
    Regex::new(
        r#"(?i)\b(?:https?://|file://|mailto:)[^\s<>"'`]+|\b[a-z0-9._%+-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,}\b"#,
    )
    .unwrap()
});

const SCHEMES: [&str; 4] = ["http://", "https://", "file://", "mailto:"];

/// `url` without the punctuation that ends the sentence around it: trailing
/// stops and quotes, and closing brackets it does not open itself.
fn trim(url: &str) -> &str {
    let mut out = url;
    while let Some(last) = out.chars().last() {
        let unbalanced = |open: char| out.matches(open).count() < out.matches(last).count();
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            ')' => unbalanced('('),
            ']' => unbalanced('['),
            '}' => unbalanced('{'),
            _ => false,
        };
        if !drop {
            break;
        }
        out = &out[..out.len() - last.len_utf8()];
    }
    out
}

/// The links a program marked, as runs of cells sharing one URI.
pub(crate) fn marked(row: u16, uris: &[Option<&str>]) -> Vec<Link> {
    let mut out: Vec<Link> = Vec::new();
    for (col, uri) in uris.iter().enumerate() {
        let Some(uri) = uri else { continue };
        let col = col as u16;
        match out.last_mut() {
            Some(link) if link.end == col && &*link.uri == *uri => link.end += 1,
            _ => out.push(Link {
                row,
                start: col,
                end: col + 1,
                uri: Arc::from(*uri),
            }),
        }
    }
    out
}

/// The URLs in the text of `cells`, where a row whose `wrapped` flag is set
/// runs on into the next. A URL overlapping a marked link is left to it.
pub(crate) fn found(cells: &[Vec<Cell>], wrapped: &[bool], marked: &[Link]) -> Vec<Link> {
    let mut out = Vec::new();
    let mut text = String::new();
    // The cell each byte of `text` came from.
    let mut origin: Vec<(u16, u16)> = Vec::new();
    let mut first = 0;
    while first < cells.len() {
        let mut last = first;
        while last + 1 < cells.len() && wrapped.get(last).copied().unwrap_or(false) {
            last += 1;
        }
        text.clear();
        origin.clear();
        for (row, line) in cells.iter().enumerate().take(last + 1).skip(first) {
            for (col, cell) in line.iter().enumerate() {
                if cell.continuation {
                    continue;
                }
                let shown = if cell.text.is_empty() {
                    " "
                } else {
                    cell.text.as_str()
                };
                text.push_str(shown);
                origin.extend(std::iter::repeat_n((row as u16, col as u16), shown.len()));
            }
        }
        if text.contains(':') || text.contains('@') {
            for m in URL.find_iter(&text) {
                let url = trim(m.as_str());
                let address = !url.contains(':');
                if !address && !has_target(url) {
                    continue;
                }
                let (start_row, start_col) = origin[m.start()];
                let (end_row, end_cell) = origin[m.start() + url.len() - 1];
                let end_col = end_cell
                    + if cells[end_row as usize][end_cell as usize].wide {
                        2
                    } else {
                        1
                    };
                let uri: Arc<str> = if address {
                    Arc::from(format!("mailto:{url}"))
                } else {
                    Arc::from(url)
                };
                let runs: Vec<Link> = (start_row..=end_row)
                    .map(|row| Link {
                        row,
                        start: if row == start_row { start_col } else { 0 },
                        end: if row == end_row {
                            end_col
                        } else {
                            cells[row as usize].len() as u16
                        },
                        uri: uri.clone(),
                    })
                    .collect();
                let overlaps = runs.iter().any(|run| {
                    marked
                        .iter()
                        .any(|l| l.row == run.row && l.start < run.end && run.start < l.end)
                });
                if !overlaps {
                    out.extend(runs);
                }
            }
        }
        first = last + 1;
    }
    out
}

/// Whether `url` names something past its scheme.
fn has_target(url: &str) -> bool {
    SCHEMES
        .iter()
        .find(|s| url.len() >= s.len() && url[..s.len()].eq_ignore_ascii_case(s))
        .is_some_and(|s| url.len() > s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentence_punctuation_and_unopened_brackets_are_left_out() {
        assert_eq!(trim("https://a.example/x."), "https://a.example/x");
        assert_eq!(trim("https://a.example/x),"), "https://a.example/x");
        assert_eq!(
            trim("https://en.example/Rust_(language)"),
            "https://en.example/Rust_(language)"
        );
        assert_eq!(
            trim("https://a.example/?q=1&r=[2]"),
            "https://a.example/?q=1&r=[2]"
        );
    }

    #[test]
    fn a_bare_email_address_is_a_mailto_link_and_a_named_one_keeps_its_scheme() {
        let found: Vec<&str> = URL
            .find_iter("mail me@example.org, mailto:you@example.org or dan@host:~$")
            .map(|m| trim(m.as_str()))
            .collect();
        assert_eq!(found, ["me@example.org", "mailto:you@example.org"]);
    }

    #[test]
    fn a_bare_scheme_is_no_link() {
        assert!(!has_target("https://"));
        assert!(has_target("mailto:me@example.org"));
        assert!(has_target("HTTPS://a"));
    }
}
