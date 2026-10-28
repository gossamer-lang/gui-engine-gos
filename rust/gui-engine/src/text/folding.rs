// folding: case-folded text that remembers which source character or
// grapheme each folded character came from. Folding can turn one source unit
// into several characters (`İ` folds to `i` plus a combining dot), so
// matching runs over the folded characters and the result maps back to source
// byte ranges through the units.

use crate::strext::StrExt;
use crate::unicode;

/// Folded characters with the source unit each one came from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FoldedText {
    /// The folded characters, in order.
    pub chars: Vec<char>,
    /// For each folded character, the index of its source unit.
    pub unit: Vec<i64>,
    /// The byte offset where each source unit starts, then the text's length.
    pub offsets: Vec<i64>,
}

impl FoldedText {
    /// `text` folded one Unicode scalar at a time.
    pub fn of_chars(text: &str) -> FoldedText {
        FoldedText::of_units(&text.chars().map(String::from).collect::<Vec<_>>(), true)
    }

    /// `text` folded one grapheme cluster at a time, or kept exact when
    /// `fold` is false.
    pub fn of_graphemes(text: &str, fold: bool) -> FoldedText {
        FoldedText::of_units(
            &unicode::graphemes(text)
                .map(String::from)
                .collect::<Vec<_>>(),
            fold,
        )
    }

    fn of_units(units: &[String], fold: bool) -> FoldedText {
        let mut out = FoldedText::default();
        let mut at = 0;
        for (u, s) in units.iter().enumerate() {
            out.offsets.push(at);
            at += s.byte_len();
            for c in if fold {
                unicode::fold_case(s)
            } else {
                s.to_string()
            }
            .chars()
            {
                out.chars.push(c);
                out.unit.push(u as i64)
            }
        }
        out.offsets.push(at);
        out
    }

    /// The number of folded characters.
    pub fn len(&self) -> i64 {
        self.chars.len() as i64
    }

    /// Whether there are no folded characters.
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// The number of source units.
    pub fn units(&self) -> i64 {
        self.offsets.len() as i64 - 1
    }

    /// Whether folded position `k` (0..=len) falls between two source units.
    pub fn at_boundary(&self, k: i64) -> bool {
        k == 0
            || k == self.chars.len() as i64
            || self.unit[k as usize] != self.unit[(k - 1) as usize]
    }

    /// Whether `query` occurs at folded position `at`.
    pub fn matches_at(&self, query: &[char], at: i64) -> bool {
        if at < 0 || at + query.len() as i64 > self.chars.len() as i64 {
            return false;
        };
        let mut k = 0;
        while k < query.len() && self.chars[(at + k as i64) as usize] == query[k] {
            k += 1
        }
        k == query.len()
    }

    /// The source byte range covering folded characters `a..b` (a < b),
    /// widened to whole source units.
    pub fn span(&self, a: i64, b: i64) -> (i64, i64) {
        (
            self.offsets[(self.unit[a as usize]) as usize],
            self.offsets[(self.unit[(b - 1) as usize] + 1) as usize],
        )
    }
}

/// `text` case-folded into characters.
pub fn folded_chars(text: &str) -> Vec<char> {
    unicode::fold_case(text).chars().collect()
}

#[cfg(test)]
mod tests {
    use super::{FoldedText, folded_chars};

    #[test]
    fn an_expanding_fold_keeps_its_source_unit() {
        let f = FoldedText::of_chars("\u{130}x");
        assert_eq!(f.chars, vec!['i', '\u{307}', 'x']);
        assert_eq!(f.unit, vec![0, 0, 1]);
        assert_eq!(f.offsets, vec![0, 2, 3]);
        assert!(f.at_boundary(0));
        assert!(!f.at_boundary(1));
        assert!(f.at_boundary(2));
        assert!(f.at_boundary(3));
        assert_eq!(f.span(0, 1), (0, 2));
        assert_eq!(f.span(2, 3), (2, 3));
        assert_eq!(f.units(), 2)
    }

    #[test]
    fn graphemes_are_units_and_exact_text_is_kept() {
        let f = FoldedText::of_graphemes("Ae\u{301}b", false);
        assert_eq!(f.offsets, vec![0, 1, 4, 5]);
        assert_eq!(f.unit, vec![0, 1, 1, 2]);
        assert!(f.matches_at(&['e', '\u{301}'], 1));
        assert!(!f.matches_at(&['a'], 0));
        let g = FoldedText::of_graphemes("Ae\u{301}b", true);
        assert!(g.matches_at(&folded_chars("A"), 0));
        assert!(!g.matches_at(&['b', 'c'], 3))
    }
}
