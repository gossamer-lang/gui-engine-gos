//! Hyperlinks on a terminal's screen: the links programs mark with OSC 8,
//! and URLs, email addresses and file paths found in the text, read across
//! rows a long line wraps over.

use std::sync::Arc;

use super::frame::Cell;

/// The columns `start..end` of one row that a link covers. A link that wraps
/// covers one run on each of its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub row: u16,
    pub start: u16,
    pub end: u16,
    /// The URI, or for a path the text as the screen shows it.
    pub uri: Arc<str>,
    /// A file path found in the text, which is a link only where the file
    /// exists.
    pub path: bool,
}

/// A file path as text names it: the path, and the line and column a
/// `:line[:col]` suffix gives (0 when absent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub path: String,
    pub line: i64,
    pub col: i64,
}

/// The place `text` names, splitting off a `:line[:col]` suffix.
pub fn place(text: &str) -> Place {
    let mut path = text;
    let mut numbers = Vec::new();
    while numbers.len() < 2
        && let Some((head, tail)) = path.rsplit_once(':')
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_digit())
    {
        numbers.push(tail.parse::<i64>().unwrap_or(0));
        path = head;
    }
    numbers.reverse();
    Place {
        path: path.to_string(),
        line: numbers.first().copied().unwrap_or(0),
        col: numbers.get(1).copied().unwrap_or(0),
    }
}

// Characters a path found in text is made of.
fn path_char(c: char) -> bool {
    c.is_alphanumeric() || "_-./~+@%:".contains(c)
}

// Whether `token` reads as a file path: a name after a slash, or a name
// with an extension and a line number, and no scheme.
fn path_like(token: &str) -> bool {
    if token.contains("://") || token.starts_with('-') || !token.chars().all(path_char) {
        return false;
    }
    let named = place(token);
    let Some(name) = named.path.rsplit('/').next() else {
        return false;
    };
    if name.is_empty() || name.contains(':') || !name.chars().any(char::is_alphabetic) {
        return false;
    }
    let slashed = named.path.contains('/');
    let extension = name
        .rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty());
    slashed || (extension && named.line > 0)
}

// The byte ranges of the file paths in `text`: runs of path characters,
// without the punctuation that ends a sentence or a compiler's message.
fn paths(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let run = rest.find(|c: char| !path_char(c)).unwrap_or(rest.len());
        if run == 0 {
            at += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let token = rest[..run].trim_end_matches(['.', ',', ':', ';']);
        if path_like(token) {
            out.push((at, at + token.len()));
        }
        at += run;
    }
    out
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
const SCHEMES: [&str; 4] = ["http://", "https://", "file://", "mailto:"];

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// Whether a word starts or ends between `before` and `after`.
fn boundary(before: Option<char>, after: Option<char>) -> bool {
    before.is_some_and(is_word) != after.is_some_and(is_word)
}

// The byte ranges of the links in `text`, leftmost first and not
// overlapping, before their trailing punctuation is trimmed: a scheme and
// the characters up to whitespace, a quote or an angle bracket; or an email
// address, whose domain ends in a top-level domain of two letters or more.
fn spans(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let before = text[..at].chars().next_back();
        let rest = &text[at..];
        if let Some(found) = scheme_link(rest, before).or_else(|| address(rest, before)) {
            out.push((at, at + found));
            at += found;
        } else {
            at += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

// The length of a link with a scheme starting `rest`, if one does.
fn scheme_link(rest: &str, before: Option<char>) -> Option<usize> {
    if !boundary(before, rest.chars().next()) {
        return None;
    }
    let scheme = SCHEMES.iter().find(|s| {
        rest.len() >= s.len() && rest.as_bytes()[..s.len()].eq_ignore_ascii_case(s.as_bytes())
    })?;
    let tail = rest[scheme.len()..]
        .find(|c: char| c.is_whitespace() || "<>\"'`".contains(c))
        .unwrap_or(rest.len() - scheme.len());
    (tail > 0).then_some(scheme.len() + tail)
}

// The length of an email address starting `rest`, if one does.
fn address(rest: &str, before: Option<char>) -> Option<usize> {
    if !boundary(before, rest.chars().next()) {
        return None;
    }
    let local = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || "._%+-".contains(c)))
        .unwrap_or(rest.len());
    if local == 0 || rest.as_bytes().get(local) != Some(&b'@') {
        return None;
    }
    let domain = &rest[local + 1..];
    let run = domain
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        .unwrap_or(domain.len());
    // The longest prefix of the run that is labels ending in a top-level
    // domain, followed by the end of a word.
    (1..=run).rev().find_map(|end| {
        let name = &domain[..end];
        let (labels, top) = name.rsplit_once('.')?;
        let whole = top.len() >= 2
            && top.bytes().all(|b| b.is_ascii_alphabetic())
            && labels.split('.').all(|l| !l.is_empty())
            && boundary(name.chars().next_back(), domain[end..].chars().next());
        whole.then_some(local + 1 + end)
    })
}

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
                path: false,
            }),
        }
    }
    out
}

/// The URLs and file paths in the text of `cells`, where a row whose
/// `wrapped` flag is set runs on into the next. One overlapping a marked
/// link is left to it.
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
        let mut taken: Vec<(usize, usize)> = Vec::new();
        let urls = if text.contains(':') || text.contains('@') {
            spans(&text)
        } else {
            Vec::new()
        };
        let places = if text.contains('/') || text.contains(':') {
            paths(&text)
        } else {
            Vec::new()
        };
        let found_urls = urls.into_iter().filter_map(|(start, stop)| {
            let url = trim(&text[start..stop]);
            let address = !url.contains(':');
            (address || has_target(url)).then(|| {
                let uri = if address {
                    format!("mailto:{url}")
                } else {
                    url.to_string()
                };
                (start, start + url.len(), uri, false)
            })
        });
        let found_paths = places
            .into_iter()
            .map(|(start, stop)| (start, stop, text[start..stop].to_string(), true));
        for (start, stop, uri, path) in found_urls.chain(found_paths) {
            // A URL takes its text before a path inside it.
            if taken.iter().any(|&(a, b)| a < stop && start < b) {
                continue;
            }
            taken.push((start, stop));
            {
                let (start_row, start_col) = origin[start];
                let (end_row, end_cell) = origin[stop - 1];
                let end_col = end_cell
                    + if cells[end_row as usize][end_cell as usize].wide {
                        2
                    } else {
                        1
                    };
                let uri: Arc<str> = Arc::from(uri);
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
                        path,
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
        let text = "mail me@example.org, mailto:you@example.org or dan@host:~$ x@y.z1 a@b.co.uk.";
        let found: Vec<&str> = spans(text)
            .iter()
            .map(|&(a, b)| trim(&text[a..b]))
            .collect();
        assert_eq!(
            found,
            ["me@example.org", "mailto:you@example.org", "a@b.co.uk"]
        );
    }

    #[test]
    fn a_scheme_starts_a_link_only_at_a_word_and_ends_at_space_or_quotes() {
        let text = "see HTTPS://Example.org/a?b=1 \"https://q.example/\" xhttps://no https:// done";
        let found: Vec<&str> = spans(text)
            .iter()
            .map(|&(a, b)| trim(&text[a..b]))
            .collect();
        assert_eq!(found, ["HTTPS://Example.org/a?b=1", "https://q.example/"]);
    }

    #[test]
    fn paths_with_a_slash_or_a_line_number_are_found_and_words_are_not() {
        let text = "error at src/model.gos:42:7: bad (see ./README.md, ~/x/y.rs:3) main.rs:10 v1.2 a/b/ 10/20 -x/y";
        let found: Vec<&str> = paths(text).iter().map(|&(a, b)| &text[a..b]).collect();
        assert_eq!(
            found,
            [
                "src/model.gos:42:7",
                "./README.md",
                "~/x/y.rs:3",
                "main.rs:10"
            ]
        );
    }

    #[test]
    fn a_place_splits_off_its_line_and_column() {
        assert_eq!(
            place("src/a.rs:12:5"),
            Place {
                path: "src/a.rs".into(),
                line: 12,
                col: 5
            }
        );
        assert_eq!(
            place("src/a.rs:12"),
            Place {
                path: "src/a.rs".into(),
                line: 12,
                col: 0
            }
        );
        assert_eq!(
            place("/etc/hosts"),
            Place {
                path: "/etc/hosts".into(),
                line: 0,
                col: 0
            }
        );
    }

    #[test]
    fn a_bare_scheme_is_no_link() {
        assert!(!has_target("https://"));
        assert!(has_target("mailto:me@example.org"));
        assert!(has_target("HTTPS://a"));
    }
}
