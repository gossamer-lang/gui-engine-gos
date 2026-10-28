// search: find and replace over committed text. A match is a byte range
// whose ends are grapheme cluster boundaries, so a query never matches part
// of a combined character. Matches are found left to right without overlap.

use crate::strext::StrExt;
use crate::text::folding::folded_chars;
use crate::text::offsets::{ByteOffset, ByteRange};

/// How a query is compared with the text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchOptions {
    /// Compare text exactly instead of case-folded.
    pub match_case: bool,
    /// Accept only matches that start and end on word boundaries.
    pub whole_word: bool,
    /// Read the query as a regular expression (the `regex` crate's syntax).
    #[serde(default)]
    pub regex: bool,
}

/// The non-overlapping matches of `query` in `text`, at most `limit` of
/// them (0 for all). An empty query matches nothing.
pub fn find_all(text: &str, query: &str, options: SearchOptions, limit: i64) -> Vec<ByteRange> {
    find_all_cancellable(text, query, options, limit, || true).unwrap_or_default()
}

/// The matches of regular expression `query`, compiled with `options`.
fn find_pattern(
    text: &str,
    query: &str,
    options: SearchOptions,
    limit: i64,
    active: impl Fn() -> bool,
) -> Result<Vec<ByteRange>, String> {
    let source = if options.whole_word {
        format!(r"\b(?:{query})\b")
    } else {
        query.to_string()
    };
    let pattern = regex::RegexBuilder::new(&source)
        .case_insensitive(!options.match_case)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| format!("invalid regular expression: {e}"))?;
    let mut out = Vec::new();
    for found in pattern.find_iter(text) {
        if !active() {
            return Err("search cancelled or expired".into());
        }
        // An empty match selects nothing.
        if found.is_empty() {
            continue;
        }
        out.push(ByteRange {
            start: ByteOffset(found.start() as i64),
            end: ByteOffset(found.end() as i64),
        });
        if limit > 0 && out.len() as i64 >= limit {
            break;
        }
    }
    Ok(out)
}

/// Streaming KMP search retains only a query-sized origin window, preserving
/// Unicode case folding, grapheme endpoints and the controller's word rules.
pub fn find_all_cancellable(
    text: &str,
    query: &str,
    options: SearchOptions,
    limit: i64,
    active: impl Fn() -> bool,
) -> Result<Vec<ByteRange>, String> {
    use std::collections::VecDeque;
    if query.len() > 4096 {
        return Err("search query exceeds its byte budget".into());
    }
    if options.regex {
        return find_pattern(text, query, options, limit, active);
    }
    let wanted = if options.match_case {
        query.chars().collect::<Vec<_>>()
    } else {
        folded_chars(query)
    };
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let mut prefix = vec![0; wanted.len()];
    for index in 1..wanted.len() {
        let mut matched = prefix[index - 1];
        while matched > 0 && wanted[index] != wanted[matched] {
            matched = prefix[matched - 1];
        }
        if wanted[index] == wanted[matched] {
            matched += 1;
        }
        prefix[index] = matched;
    }
    let mut origins = VecDeque::with_capacity(wanted.len());
    let mut graphemes = crate::unicode::graphemes(text).peekable();
    let mut at = 0usize;
    let mut previous = -1;
    let mut matched = 0usize;
    let mut out = Vec::new();
    while let Some(grapheme) = graphemes.next() {
        if !active() {
            return Err("search cancelled or expired".into());
        }
        let class = super::word_class(grapheme);
        let word_start = at == 0 || class != previous || class == 3;
        let word_end = graphemes
            .peek()
            .is_none_or(|next| super::word_class(next) != class || class == 3);
        let end = at + grapheme.len();
        let folded = if options.match_case {
            grapheme.to_owned()
        } else {
            crate::unicode::fold_case(grapheme)
        };
        let count = folded.chars().count();
        for (index, scalar) in folded.chars().enumerate() {
            origins.push_back((at, index == 0, word_start));
            if origins.len() > wanted.len() {
                origins.pop_front();
            }
            while matched > 0 && scalar != wanted[matched] {
                matched = prefix[matched - 1];
            }
            if scalar == wanted[matched] {
                matched += 1;
            }
            if matched == wanted.len() {
                let (start, boundary, whole) = origins[origins.len() - wanted.len()];
                if boundary && index + 1 == count && (!options.whole_word || whole && word_end) {
                    out.push(ByteRange {
                        start: ByteOffset(start as i64),
                        end: ByteOffset(end as i64),
                    });
                    if out.len()
                        >= if limit > 0 {
                            (limit as usize).min(100_000)
                        } else {
                            100_000
                        }
                    {
                        return Ok(out);
                    }
                    matched = 0;
                } else {
                    matched = prefix[matched - 1];
                }
            }
        }
        at = end;
        previous = class;
    }
    Ok(out)
}

/// The match to select when stepping from the selection `from..to`: the
/// first match starting at or after `to` going forward, or the last one
/// starting before `from` going backward, wrapping past either end. -1 when
/// there are none.
pub fn step(ranges: &[ByteRange], from: ByteOffset, to: ByteOffset, forward: bool) -> i64 {
    if ranges.is_empty() {
        return -1;
    }
    if forward {
        match ranges.iter().position(|r| r.start >= to) {
            Some(i) => i as i64,
            None => 0,
        }
    } else {
        let found = ranges
            .iter()
            .rposition(|r| r.start < from)
            .unwrap_or(ranges.len() - 1);
        found as i64
    }
}

/// The index of the match covering exactly `from..to`, or -1.
pub fn selected(ranges: &[ByteRange], from: ByteOffset, to: ByteOffset) -> i64 {
    match ranges.iter().position(|r| r.start == from && r.end == to) {
        Some(i) => i as i64,
        None => -1,
    }
}

/// `text` with every range in `ranges` replaced by `replacement`. The
/// ranges must be sorted, non-overlapping, and found in this `text`.
pub fn replace_all(text: &str, ranges: &[ByteRange], replacement: &str) -> String {
    let mut out = String::with_capacity(text.byte_len() as usize);
    let mut at = 0;
    for r in ranges {
        out += text.substring(at, r.start.0);
        out += replacement;
        at = r.end.0;
    }
    out += text.substring(at, text.byte_len());
    out
}

#[cfg(test)]
mod tests {
    use super::{SearchOptions, find_all, replace_all, selected, step};

    use crate::text::offsets::{ByteOffset, ByteRange};

    fn starts(text: &str, query: &str, options: SearchOptions) -> Vec<i64> {
        find_all(text, query, options, 0)
            .iter()
            .map(|r| r.start.0)
            .collect::<Vec<_>>()
    }

    fn range(start: i64, end: i64) -> ByteRange {
        ByteRange {
            start: ByteOffset(start),
            end: ByteOffset(end),
        }
    }

    #[test]
    fn a_regular_expression_query_follows_case_and_word_options() {
        let re = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        assert_eq!(starts("err 42, Err 7, error", r"err\s*\d+", re), vec![0, 8]);
        let cased = SearchOptions {
            match_case: true,
            ..re
        };
        assert_eq!(starts("err 42, Err 7", r"err\s*\d+", cased), vec![0]);
        let words = SearchOptions {
            whole_word: true,
            ..re
        };
        assert_eq!(starts("err error err", "err", words), vec![0, 10]);
        assert_eq!(starts("abc", "x*", re), Vec::<i64>::new());
        assert!(super::find_all_cancellable("abc", "(", re, 0, || true).is_err());
    }

    #[test]
    fn matches_fold_case_unless_asked_and_never_overlap() {
        let any = SearchOptions::default();
        assert_eq!(starts("Foo foo FOO", "foo", any), vec![0, 4, 8]);
        assert_eq!(
            starts(
                "Foo foo FOO",
                "foo",
                SearchOptions {
                    match_case: true,
                    ..SearchOptions::default()
                }
            ),
            vec![4]
        );
        assert_eq!(starts("aaaa", "aa", any), vec![0, 2]);
        assert_eq!(starts("abc", "", any), Vec::<i64>::new());
        assert_eq!(find_all("Foo foo FOO", "foo", any, 2).len(), 2)
    }

    #[test]
    fn whole_words_reject_matches_inside_identifiers() {
        let words = SearchOptions {
            whole_word: true,
            ..SearchOptions::default()
        };
        assert_eq!(
            starts("let x = xs.len() + x_1 + x", "x", words),
            vec![4, 25]
        );
        assert_eq!(starts("foo.bar foobar", "foo", words), vec![0])
    }

    #[test]
    fn a_query_never_matches_part_of_a_grapheme() {
        // "e" followed by a combining acute is one grapheme.
        let text = "e\u{301}e caf\u{e9}";
        assert_eq!(starts(text, "e", SearchOptions::default()), vec![3]);
        assert_eq!(
            find_all(text, "e\u{301}", SearchOptions::default(), 0),
            vec![range(0, 3)]
        );
        assert_eq!(
            starts(
                "\u{1F469}\u{200D}\u{1F4BB} \u{1F469}",
                "\u{1F469}",
                SearchOptions::default()
            ),
            vec![12]
        )
    }

    #[test]
    fn folding_that_expands_still_matches_whole_graphemes() {
        // `İ` folds to `i` plus a combining dot, two characters for one grapheme.
        let text = "\u{130}STANBUL istanbul \u{130}x i";
        assert_eq!(
            starts(text, "\u{130}stanbul", SearchOptions::default()),
            vec![0]
        );
        assert_eq!(starts(text, "istanbul", SearchOptions::default()), vec![10]);
        assert_eq!(starts(text, "i", SearchOptions::default()), vec![10, 23]);
        assert_eq!(
            find_all(text, "\u{130}x", SearchOptions::default(), 0),
            vec![range(19, 22)]
        );
        // `ß` folds to `ss`, so either spelling finds the other.
        let street = "Stra\u{DF}e STRASSE";
        assert_eq!(
            starts(street, "strasse", SearchOptions::default()),
            vec![0, 8]
        );
        assert_eq!(
            starts(street, "Stra\u{DF}e", SearchOptions::default()),
            vec![0, 8]
        );
        assert_eq!(
            find_all(street, "ss", SearchOptions::default(), 0),
            vec![range(4, 6), range(12, 14)]
        );
        assert_eq!(
            starts(street, "s", SearchOptions::default()),
            vec![0, 8, 12, 13]
        )
    }

    #[test]
    fn stepping_wraps_and_starts_from_the_selection() {
        let m = vec![range(0, 3), range(4, 7), range(8, 11)];
        assert_eq!(step(&m, ByteOffset(0), ByteOffset(0), true), 0);
        assert_eq!(step(&m, ByteOffset(0), ByteOffset(3), true), 1);
        assert_eq!(step(&m, ByteOffset(8), ByteOffset(11), true), 0);
        assert_eq!(step(&m, ByteOffset(4), ByteOffset(7), false), 0);
        assert_eq!(step(&m, ByteOffset(0), ByteOffset(3), false), 2);
        assert_eq!(step(&[], ByteOffset(0), ByteOffset(0), true), -1);
        assert_eq!(selected(&m, ByteOffset(4), ByteOffset(7)), 1);
        assert_eq!(selected(&m, ByteOffset(4), ByteOffset(6)), -1)
    }

    #[test]
    fn replace_all_rewrites_each_match_once() {
        let text = "a cat, a Cat";
        let m = find_all(text, "cat", SearchOptions::default(), 0);
        assert_eq!(replace_all(text, &m, "dog"), "a dog, a dog");
        assert_eq!(replace_all(text, &[], "dog"), text);
        assert_eq!(
            replace_all(
                "xx",
                &find_all("xx", "x", SearchOptions::default(), 0),
                "xx"
            ),
            "xxxx"
        )
    }
}
