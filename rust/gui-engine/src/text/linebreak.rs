// linebreak: line break opportunities (UAX #14) by the pair-table method,
// with the rules the table cannot express (LB8a zero-width joiner, LB21a
// Hebrew hyphens, LB30a regional-indicator pairs) applied on top. Classes
// AI, SG, XX, SA, and the Aksara classes resolve to AL; CJ resolves to
// NS (the strict-line-breaking choice is left to callers).

use crate::strext::StrExt;
use crate::unicode;

use crate::text::ucd::LINE_BREAK;

// Class indices as tools/gen-ucd.gos numbers them.
const LB_XX: i64 = 0;
const LB_BK: i64 = 1;
const LB_CR: i64 = 2;
const LB_LF: i64 = 3;
const LB_CM: i64 = 4;
const LB_NL: i64 = 5;
const LB_SG: i64 = 6;
const LB_WJ: i64 = 7;
const LB_ZW: i64 = 8;
const LB_GL: i64 = 9;
const LB_SP: i64 = 10;
const LB_ZWJ: i64 = 11;
const LB_B2: i64 = 12;
const LB_BA: i64 = 13;
const LB_BB: i64 = 14;
const LB_HY: i64 = 15;
const LB_CB: i64 = 16;
const LB_CL: i64 = 17;
const LB_CP: i64 = 18;
const LB_EX: i64 = 19;
const LB_IN: i64 = 20;
const LB_NS: i64 = 21;
const LB_OP: i64 = 22;
const LB_QU: i64 = 23;
const LB_IS: i64 = 24;
const LB_NU: i64 = 25;
const LB_PO: i64 = 26;
const LB_PR: i64 = 27;
const LB_SY: i64 = 28;
const LB_AI: i64 = 29;
const LB_AL: i64 = 30;
const LB_CJ: i64 = 31;
const LB_EB: i64 = 32;
const LB_EM: i64 = 33;
const LB_H2: i64 = 34;
const LB_H3: i64 = 35;
const LB_HL: i64 = 36;
const LB_ID: i64 = 37;
const LB_JL: i64 = 38;
const LB_JV: i64 = 39;
const LB_JT: i64 = 40;
const LB_RI: i64 = 41;
const LB_SA: i64 = 42;

// Pair-table column/row order.
const ORDER: [i64; 32] = [
    LB_OP, LB_CL, LB_CP, LB_QU, LB_GL, LB_NS, LB_EX, LB_SY, LB_IS, LB_PR, LB_PO, LB_NU, LB_AL,
    LB_HL, LB_ID, LB_IN, LB_HY, LB_BA, LB_BB, LB_B2, LB_ZW, LB_CM, LB_WJ, LB_H2, LB_H3, LB_JL,
    LB_JV, LB_JT, LB_RI, LB_EB, LB_EM, LB_ZWJ,
];

// '_' direct break, '%' indirect (break only after spaces), '#' combining
// indirect, '@' combining prohibited, '^' prohibited.
const PAIRS: [&str; 32] = [
    "^^^^^^^^^^^^^^^^^^^^^@^^^^^^^^^^",
    "_^^%%^^^^%%_____%%__^#^________%",
    "_^^%%^^^^%%%%%__%%__^#^________%",
    "^^^%%%^^^%%%%%%%%%%%^#^%%%%%%%%%",
    "%^^%%%^^^%%%%%%%%%%%^#^%%%%%%%%%",
    "_^^%%%^^^_______%%__^#^________%",
    "_^^%%%^^^______%%%__^#^________%",
    "_^^%%%^^^__%_%__%%__^#^________%",
    "_^^%%%^^^__%%%__%%__^#^________%",
    "%^^%%%^^^__%%%%_%%__^#^%%%%%_%%%",
    "%^^%%%^^^__%%%__%%__^#^________%",
    "%^^%%%^^^%%%%%_%%%__^#^________%",
    "%^^%%%^^^%%%%%_%%%__^#^________%",
    "%^^%%%^^^%%%%%_%%%__^#^________%",
    "_^^%%%^^^_%____%%%__^#^________%",
    "_^^%%%^^^______%%%__^#^________%",
    "_^^%_%^^^__%____%%__^#^________%",
    "_^^%_%^^^_______%%__^#^________%",
    "%^^%%%^^^%%%%%%%%%%%^#^%%%%%%%%%",
    "_^^%%%^^^_______%%_^^#^________%",
    "____________________^___________",
    "%^^%%%^^^%%%%%_%%%__^#^________%",
    "%^^%%%^^^%%%%%%%%%%%^#^%%%%%%%%%",
    "_^^%%%^^^_%____%%%__^#^___%%___%",
    "_^^%%%^^^_%____%%%__^#^____%___%",
    "_^^%%%^^^_%____%%%__^#^%%%%____%",
    "_^^%%%^^^_%____%%%__^#^___%%___%",
    "_^^%%%^^^_%____%%%__^#^____%___%",
    "_^^%%%^^^_______%%__^#^_____%__%",
    "_^^%%%^^^_%____%%%__^#^_______%%",
    "_^^%%%^^^_%____%%%__^#^________%",
    "%^^%%%^^^%%%%%%%%%%%^#^%%%%%%%%%",
];

/// A place a line may (or must) break: before the byte at `offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Break {
    pub offset: i64,
    pub mandatory: bool,
}

/// The raw class of `c` from the pinned data (XX when unlisted).
pub fn raw_class(c: char) -> i64 {
    let v = c as i64;
    let mut lo = 0;
    let mut hi = LINE_BREAK.len() / 3;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if v < LINE_BREAK[mid * 3] {
            hi = mid;
        } else if v > LINE_BREAK[mid * 3 + 1] {
            lo = mid + 1;
        } else {
            return LINE_BREAK[mid * 3 + 2];
        }
    }
    // Unlisted CJK ideograph ranges default to ID per LineBreak.txt.
    if (0x3400..=0x4dbf).contains(&v)
        || (0x4e00..=0x9fff).contains(&v)
        || (0xf900..=0xfaff).contains(&v)
        || (0x20000..=0x3fffd).contains(&v)
    {
        return LB_ID;
    }
    LB_XX
}

fn resolved(c: char) -> i64 {
    let k = raw_class(c);
    if k == LB_AI || k == LB_SG || k == LB_XX || k == LB_CB || k >= 43 {
        return LB_AL;
    };
    if k == LB_SA {
        return if unicode::is_mark(c) { LB_CM } else { LB_AL };
    };
    if k == LB_CJ {
        return LB_NS;
    };
    k
}

fn column(k: i64) -> i64 {
    let mut i = 0;
    while i < 32 {
        if ORDER[i] == k {
            return i as i64;
        };
        i += 1;
    }
    12
}

fn pair(a: i64, b: i64) -> char {
    let row = PAIRS[(column(a)) as usize];
    let idx = column(b);
    for (i, ch) in row.chars().enumerate() {
        if i as i64 == idx {
            return ch;
        };
    }
    '_'
}

fn first_class(k: i64) -> i64 {
    if k == LB_LF || k == LB_NL {
        LB_BK
    } else if k == LB_SP {
        LB_WJ
    } else {
        k
    }
}

/// Every break opportunity in `text`, in order. The end of the text is
/// always a mandatory break.
pub fn breaks(text: &str) -> Vec<Break> {
    let mut out = vec![];
    let mut offsets = vec![];
    let mut classes = vec![];
    let mut at = 0;
    for c in text.chars() {
        offsets.push(at);
        classes.push(resolved(c));
        at += c.len_utf8();
    }
    let n = classes.len();
    if n == 0 {
        return out;
    }
    let mut cur = first_class(classes[0]);
    let mut next = classes[0];
    let mut lb8a = next == LB_ZWJ;
    let mut lb21a = false;
    let mut ri_run = 0;
    let mut i = 1;
    while i < n {
        let last = next;
        next = classes[i];
        if cur == LB_BK || (cur == LB_CR && next != LB_LF) {
            out.push(Break {
                offset: offsets[i] as i64,
                mandatory: true,
            });
            cur = first_class(next);
            lb8a = next == LB_ZWJ;
            i += 1;
            continue;
        }
        let mut simple = true;
        let mut should = false;
        match next {
            LB_SP => should = false,
            LB_BK | LB_LF | LB_NL => cur = LB_BK,
            LB_CR => cur = LB_CR,
            _ => simple = false,
        }
        if !simple {
            let mut keep_cur = false;
            match pair(cur, next) {
                '_' => should = true,
                '%' => should = last == LB_SP,
                '#' => {
                    should = last == LB_SP;
                    if !should {
                        keep_cur = true
                    }
                }
                '@' if last != LB_SP => keep_cur = true,
                _ => (),
            }
            if !keep_cur {
                if lb8a {
                    should = false
                };
                if lb21a && (cur == LB_HY || cur == LB_BA) {
                    should = false;
                    lb21a = false;
                } else {
                    lb21a = cur == LB_HL;
                }
                if cur == LB_RI {
                    ri_run += 1;
                    if ri_run == 2 && next == LB_RI {
                        should = true;
                        ri_run = 0;
                    }
                } else {
                    ri_run = 0;
                }
                cur = next;
            }
        }
        lb8a = next == LB_ZWJ;
        if should {
            out.push(Break {
                offset: offsets[i] as i64,
                mandatory: false,
            })
        }
        i += 1;
    }
    out.push(Break {
        offset: text.byte_len(),
        mandatory: true,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::breaks;

    fn offsets(text: &str) -> Vec<i64> {
        breaks(text).iter().map(|b| b.offset).collect::<Vec<_>>()
    }

    #[test]
    fn spaces_allow_breaks_after_them() {
        assert_eq!(offsets("ab cd ef"), vec![3, 6, 8])
    }

    #[test]
    fn closing_punctuation_never_starts_a_line() {
        assert_eq!(offsets("word, next"), vec![6, 10]);
        assert_eq!(offsets("(a)"), vec![3])
    }

    #[test]
    fn hyphens_break_after() {
        assert_eq!(offsets("well-known"), vec![5, 10])
    }

    #[test]
    fn newlines_are_mandatory() {
        let b = breaks("a\nb");
        assert_eq!(b[0].offset, 2);
        assert!(b[0].mandatory)
    }

    #[test]
    fn ideographs_break_between_each_other() {
        // Three CJK ideographs of three bytes each.
        assert_eq!(offsets("\u{4e00}\u{4e01}\u{4e02}"), vec![3, 6, 9])
    }

    #[test]
    fn numbers_with_separators_stay_together() {
        assert_eq!(offsets("1,000.50 x"), vec![9, 10])
    }
}
