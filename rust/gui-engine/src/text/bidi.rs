// bidi: the Unicode Bidirectional Algorithm (UAX #9) over one paragraph.
//
// Implements paragraph level detection (P2-P3), explicit embeddings,
// overrides, and isolates (X1-X10), weak types (W1-W7), neutrals (N1-N2),
// implicit levels (I1-I2), whitespace reset (L1), and visual reordering
// (L2). Bracket pairing (N0) is not implemented: paired brackets resolve
// as ordinary neutrals, which differs from the reference only when a
// bracket pair encloses text of the opposite direction.

use crate::text::ucd::{BIDI, MIRROR};

pub const BIDI_L: i64 = 0;
pub const BIDI_R: i64 = 1;
pub const BIDI_AL: i64 = 2;
pub const BIDI_EN: i64 = 3;
pub const BIDI_ES: i64 = 4;
pub const BIDI_ET: i64 = 5;
pub const BIDI_AN: i64 = 6;
pub const BIDI_CS: i64 = 7;
pub const BIDI_NSM: i64 = 8;
pub const BIDI_BN: i64 = 9;
pub const BIDI_B: i64 = 10;
pub const BIDI_S: i64 = 11;
pub const BIDI_WS: i64 = 12;
pub const BIDI_ON: i64 = 13;
pub const BIDI_LRE: i64 = 14;
pub const BIDI_LRO: i64 = 15;
pub const BIDI_RLE: i64 = 16;
pub const BIDI_RLO: i64 = 17;
pub const BIDI_PDF: i64 = 18;
pub const BIDI_LRI: i64 = 19;
pub const BIDI_RLI: i64 = 20;
pub const BIDI_FSI: i64 = 21;
pub const BIDI_PDI: i64 = 22;

const MAX_DEPTH: i64 = 125;

/// A paragraph's base direction request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Direction {
    /// From the first strong character; left-to-right when there is none.
    Auto,
    Ltr,
    Rtl,
}

/// The bidi class of `c` from the pinned Unicode data.
pub fn class(c: char) -> i64 {
    let v = c as i64;
    let mut lo = 0;
    let mut hi = BIDI.len() / 3;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let start = BIDI[mid * 3];
        let end = BIDI[mid * 3 + 1];
        if v < start {
            hi = mid;
        } else if v > end {
            lo = mid + 1;
        } else {
            return BIDI[mid * 3 + 2];
        }
    }
    // Unlisted code points: right-to-left in the Hebrew and Arabic
    // blocks per DerivedBidiClass defaults, left-to-right elsewhere.
    if (0x0590..=0x05ff).contains(&v)
        || (0x07c0..=0x085f).contains(&v)
        || (0xfb1d..=0xfb4f).contains(&v)
        || (0x10800..=0x10fff).contains(&v)
        || (0x1e800..=0x1edff).contains(&v)
    {
        return BIDI_R;
    }
    if (0x0600..=0x07bf).contains(&v)
        || (0x0860..=0x08ff).contains(&v)
        || (0xfb50..=0xfdcf).contains(&v)
        || (0xfdf0..=0xfdff).contains(&v)
        || (0xfe70..=0xfeff).contains(&v)
        || (0x1ee00..=0x1eeff).contains(&v)
    {
        return BIDI_AL;
    }
    BIDI_L
}

/// The mirrored form of `c` for right-to-left display, or `c`.
pub fn mirror(c: char) -> char {
    let v = c as i64;
    let mut lo = 0;
    let mut hi = MIRROR.len() / 3;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let k = MIRROR[mid * 3];
        if v < k {
            hi = mid;
        } else if v > k {
            lo = mid + 1;
        } else {
            return char::from_u32(MIRROR[mid * 3 + 2] as u32).unwrap_or(c);
        }
    }
    c
}

fn is_isolate_initiator(t: i64) -> bool {
    t == BIDI_LRI || t == BIDI_RLI || t == BIDI_FSI
}

fn is_removed(t: i64) -> bool {
    t == BIDI_LRE
        || t == BIDI_RLE
        || t == BIDI_LRO
        || t == BIDI_RLO
        || t == BIDI_PDF
        || t == BIDI_BN
}

/// The paragraph level P2-P3 picks for `types` from `start` to the
/// matching PDI or end.
fn first_strong_level(types: &[i64], start: i64, end: i64) -> i64 {
    let mut depth = 0;
    let mut i = start;
    while i < end {
        let t = types[i as usize];
        if is_isolate_initiator(t) {
            depth += 1;
        } else if t == BIDI_PDI {
            if depth == 0 {
                break;
            }
            depth -= 1;
        } else if depth == 0 {
            if t == BIDI_L {
                return 0;
            };
            if t == BIDI_R || t == BIDI_AL {
                return 1;
            }
        }
        i += 1;
    }
    -1
}

/// The resolved embedding levels of `chars` (one paragraph) and the
/// paragraph's level.
pub fn levels(chars: &[char], direction: Direction) -> (Vec<i64>, i64) {
    let n = chars.len() as i64;
    let original = chars.iter().map(|c| class(*c)).collect::<Vec<_>>();
    let mut types = original.clone();
    let para = match direction {
        Direction::Ltr => 0,
        Direction::Rtl => 1,
        Direction::Auto => first_strong_level(&types, 0, n).max(0),
    };
    if n == 0 {
        return (vec![], para);
    }
    let mut levels = vec![para; n as usize];
    // Explicit levels and directions (X1-X8).
    let mut stack_level = vec![para];
    let mut stack_override = vec![-1];
    let mut stack_isolate = vec![false];
    let mut overflow_isolates = 0;
    let mut overflow_embeddings = 0;
    let mut valid_isolates = 0;
    let mut i = 0;
    while i < n {
        let t = original[i as usize];
        let top = stack_level.len() - 1;
        let current = stack_level[top];
        if t == BIDI_RLE
            || t == BIDI_LRE
            || t == BIDI_RLO
            || t == BIDI_LRO
            || is_isolate_initiator(t)
        {
            let isolate = is_isolate_initiator(t);
            let mut rtl = t == BIDI_RLE || t == BIDI_RLO || t == BIDI_RLI;
            if t == BIDI_FSI {
                rtl = first_strong_level(&original, i + 1, n) == 1;
            }
            if isolate {
                levels[i as usize] = current;
                if stack_override[top] >= 0 {
                    types[i as usize] = stack_override[top];
                }
            }
            let next = if rtl {
                (current + 1) | 1
            } else {
                (current + 2) & !1
            };
            if next <= MAX_DEPTH && overflow_isolates == 0 && overflow_embeddings == 0 {
                if isolate {
                    valid_isolates += 1;
                }
                stack_level.push(next);
                stack_override.push(if t == BIDI_LRO {
                    BIDI_L
                } else if t == BIDI_RLO {
                    BIDI_R
                } else {
                    -1
                });
                stack_isolate.push(isolate)
            } else if isolate {
                overflow_isolates += 1;
            } else if overflow_isolates == 0 {
                overflow_embeddings += 1;
            }
            if !isolate {
                levels[i as usize] = current;
            }
        } else if t == BIDI_PDI {
            if overflow_isolates > 0 {
                overflow_isolates -= 1;
            } else if valid_isolates > 0 {
                overflow_embeddings = 0;
                while !stack_isolate[stack_isolate.len() - 1] {
                    stack_level.pop();
                    stack_override.pop();
                    stack_isolate.pop();
                }
                stack_level.pop();
                stack_override.pop();
                stack_isolate.pop();
                valid_isolates -= 1;
            }
            let t2 = stack_level.len() - 1;
            levels[i as usize] = stack_level[t2];
            if stack_override[t2] >= 0 {
                types[i as usize] = stack_override[t2];
            }
        } else if t == BIDI_PDF {
            if overflow_isolates > 0 {
            } else if overflow_embeddings > 0 {
                overflow_embeddings -= 1;
            } else if !stack_isolate[top] && stack_level.len() >= 2 {
                stack_level.pop();
                stack_override.pop();
                stack_isolate.pop();
            }
            levels[i as usize] = current;
        } else if t == BIDI_B {
            levels[i as usize] = para;
        } else {
            levels[i as usize] = current;
            if stack_override[top] >= 0 && t != BIDI_BN {
                types[i as usize] = stack_override[top];
            }
        }
        i += 1;
    }
    // Level runs over characters that X9 keeps.
    let mut runs: Vec<(i64, i64)> = vec![];
    i = 0;
    while i < n {
        if is_removed(original[i as usize]) {
            i += 1;
            continue;
        }
        let start = i;
        let level = levels[i as usize];
        let mut end = i + 1;
        let mut j = i + 1;
        while j < n {
            if is_removed(original[j as usize]) {
                j += 1;
                continue;
            }
            if levels[j as usize] != level {
                break;
            }
            end = j + 1;
            j += 1;
        }
        runs.push((start, end));
        i = j;
    }
    // Isolating run sequences (X10): chain a run ending in an isolate
    // initiator to the run that starts with its matching PDI.
    let mut used = vec![false; runs.len()];
    let mut r = 0;
    while r < runs.len() {
        if used[r] {
            r += 1;
            continue;
        }
        let mut seq: Vec<i64> = vec![];
        let mut k = r;
        loop {
            used[k] = true;
            let (s, e) = runs[k];
            let mut c = s;
            while c < e {
                if !is_removed(original[c as usize]) {
                    seq.push(c)
                }
                c += 1;
            }
            let last = seq[seq.len() - 1];
            if !is_isolate_initiator(original[last as usize]) {
                break;
            }
            let Some(m) = matching_pdi(&original, last, n) else {
                break;
            };
            let mut found: i64 = -1;
            let mut q = k + 1;
            while q < runs.len() {
                if runs[q].0 == m || (runs[q].0 <= m && m < runs[q].1) {
                    found = q as i64;
                    break;
                }
                q += 1;
            }
            if found < 0 || used[found as usize] {
                break;
            }
            k = found as usize;
        }
        resolve_sequence(&mut types, &mut levels, &original, &seq, para, n);
        r += 1;
    }
    // L1: separators and trailing whitespace return to the paragraph level.
    i = 0;
    while i < n {
        let t = original[i as usize];
        if t == BIDI_S || t == BIDI_B {
            levels[i as usize] = para;
            let mut j = i - 1;
            while j >= 0
                && (original[j as usize] == BIDI_WS
                    || is_isolate_initiator(original[j as usize])
                    || original[j as usize] == BIDI_PDI
                    || is_removed(original[j as usize]))
            {
                levels[j as usize] = para;
                j -= 1;
            }
        }
        i += 1;
    }
    let mut j = n - 1;
    while j >= 0
        && (original[j as usize] == BIDI_WS
            || is_isolate_initiator(original[j as usize])
            || original[j as usize] == BIDI_PDI
            || is_removed(original[j as usize]))
    {
        levels[j as usize] = para;
        j -= 1;
    }
    // Removed characters take the level of the character before them.
    i = 0;
    while i < n {
        if is_removed(original[i as usize]) {
            levels[i as usize] = if i > 0 {
                levels[(i - 1) as usize]
            } else {
                para
            };
        }
        i += 1;
    }
    (levels, para)
}

fn matching_pdi(types: &[i64], from: i64, n: i64) -> Option<i64> {
    let mut depth = 0;
    let mut i = from + 1;
    while i < n {
        let t = types[i as usize];
        if is_isolate_initiator(t) {
            depth += 1;
        } else if t == BIDI_PDI {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
        i += 1;
    }
    None
}

fn resolve_sequence(
    types: &mut [i64],
    levels: &mut [i64],
    original: &[i64],
    seq: &[i64],
    para: i64,
    n: i64,
) {
    if seq.is_empty() {
        return;
    }
    let level = levels[(seq[0]) as usize];
    let first = seq[0];
    let last = seq[seq.len() - 1];
    // sos/eos from the higher of the adjacent levels.
    let mut before = para;
    let mut p = first - 1;
    while p >= 0 && is_removed(original[p as usize]) {
        p -= 1
    }
    if p >= 0 && !is_isolate_initiator(original[first as usize]) {
        before = levels[p as usize];
    }
    let mut after = para;
    if !is_isolate_initiator(original[last as usize]) {
        let mut q = last + 1;
        while q < n && is_removed(original[q as usize]) {
            q += 1
        }
        if q < n {
            after = levels[q as usize]
        }
    }
    let sos = if level.max(before) % 2 == 1 {
        BIDI_R
    } else {
        BIDI_L
    };
    let eos = if level.max(after) % 2 == 1 {
        BIDI_R
    } else {
        BIDI_L
    };
    let m = seq.len();
    let mut t = vec![];
    for i in seq {
        t.push(types[*i as usize])
    }
    // W1: NSM takes the type of the previous character (ON after isolates).
    let mut k = 0;
    while k < m {
        if t[k] == BIDI_NSM {
            t[k] = if k == 0 {
                sos
            } else if is_isolate_initiator(t[k - 1]) || t[k - 1] == BIDI_PDI {
                BIDI_ON
            } else {
                t[k - 1]
            };
        }
        k += 1;
    }
    // W2: EN after AL becomes AN. W3: AL becomes R.
    let mut last_strong = sos;
    k = 0;
    while k < m {
        let c = t[k];
        if c == BIDI_L || c == BIDI_R || c == BIDI_AL {
            last_strong = c;
        } else if c == BIDI_EN && last_strong == BIDI_AL {
            t[k] = BIDI_AN;
        }
        k += 1;
    }
    k = 0;
    while k < m {
        if t[k] == BIDI_AL {
            t[k] = BIDI_R
        };
        k += 1;
    }
    // W4: a single separator between numbers of one kind joins them.
    k = 1;
    while k + 1 < m {
        if (t[k] == BIDI_ES || t[k] == BIDI_CS) && t[k - 1] == BIDI_EN && t[k + 1] == BIDI_EN {
            t[k] = BIDI_EN;
        } else if t[k] == BIDI_CS && t[k - 1] == BIDI_AN && t[k + 1] == BIDI_AN {
            t[k] = BIDI_AN;
        }
        k += 1;
    }
    // W5: terminators adjacent to European numbers become EN.
    k = 0;
    while k < m {
        if t[k] == BIDI_ET {
            let start = k;
            let mut end = k;
            while end < m && t[end] == BIDI_ET {
                end += 1
            }
            let touches = (start > 0 && t[start - 1] == BIDI_EN) || (end < m && t[end] == BIDI_EN);
            if touches {
                let mut z = start;
                while z < end {
                    t[z] = BIDI_EN;
                    z += 1
                }
            }
            k = end;
        } else {
            k += 1;
        }
    }
    // W6: remaining separators and terminators become ON.
    k = 0;
    while k < m {
        if t[k] == BIDI_ET || t[k] == BIDI_ES || t[k] == BIDI_CS {
            t[k] = BIDI_ON
        };
        k += 1;
    }
    // W7: EN after L (or sos L) becomes L.
    last_strong = sos;
    k = 0;
    while k < m {
        if t[k] == BIDI_L || t[k] == BIDI_R {
            last_strong = t[k];
        } else if t[k] == BIDI_EN && last_strong == BIDI_L {
            t[k] = BIDI_L;
        }
        k += 1;
    }
    // N1/N2: neutrals between strong types of one direction take it,
    // others take the embedding direction. EN and AN count as R.
    let embedding = if level % 2 == 1 { BIDI_R } else { BIDI_L };
    k = 0;
    while k < m {
        if is_neutral(t[k]) {
            let start = k;
            let mut end = k;
            while end < m && is_neutral(t[end]) {
                end += 1
            }
            let lead = if start == 0 {
                sos
            } else {
                strong_of(t[start - 1])
            };
            let trail = if end >= m { eos } else { strong_of(t[end]) };
            let resolved = if lead == trail { lead } else { embedding };
            let mut z = start;
            while z < end {
                t[z] = resolved;
                z += 1
            }
            k = end;
        } else {
            k += 1;
        }
    }
    // I1/I2: implicit levels.
    k = 0;
    while k < m {
        let i = seq[k];
        let c = t[k];
        if level % 2 == 0 {
            if c == BIDI_R {
                levels[i as usize] = level + 1
            } else if c == BIDI_AN || c == BIDI_EN {
                levels[i as usize] = level + 2
            }
        } else if c == BIDI_L || c == BIDI_EN || c == BIDI_AN {
            levels[i as usize] = level + 1;
        }
        types[i as usize] = c;
        k += 1;
    }
}

fn is_neutral(t: i64) -> bool {
    t == BIDI_B
        || t == BIDI_S
        || t == BIDI_WS
        || t == BIDI_ON
        || t == BIDI_LRI
        || t == BIDI_RLI
        || t == BIDI_FSI
        || t == BIDI_PDI
}

fn strong_of(t: i64) -> i64 {
    if t == BIDI_EN || t == BIDI_AN {
        BIDI_R
    } else {
        t
    }
}

/// L2: the visual order of positions `0..levels.len()` for one line.
pub fn visual_order(levels: &[i64]) -> Vec<i64> {
    let n = levels.len();
    let mut order: Vec<i64> = vec![];
    let mut i = 0;
    while i < n {
        order.push(i as i64);
        i += 1;
    }
    if n == 0 {
        return order;
    }
    let mut highest = 0;
    let mut lowest_odd = 1000;
    for l in levels {
        if *l > highest {
            highest = *l
        };
        if l % 2 == 1 && *l < lowest_odd {
            lowest_odd = *l
        }
    }
    let mut level = highest;
    while level >= lowest_odd && level > 0 {
        let mut k = 0;
        while k < n {
            if levels[(order[k]) as usize] >= level {
                let start = k;
                while k < n && levels[(order[k]) as usize] >= level {
                    k += 1
                }
                let mut a = start;
                let mut b = k - 1;
                while a < b {
                    order.swap(a, b);
                    a += 1;
                    b -= 1;
                }
            } else {
                k += 1;
            }
        }
        level -= 1;
    }
    order
}

#[cfg(test)]
mod tests {
    use super::{BIDI_AL, BIDI_EN, BIDI_L, BIDI_R, class, levels, mirror, visual_order};

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn classes_come_from_the_pinned_data() {
        assert_eq!(class('a'), BIDI_L);
        assert_eq!(class('\u{05d0}'), BIDI_R);
        assert_eq!(class('\u{0628}'), BIDI_AL);
        assert_eq!(class('7'), BIDI_EN)
    }

    #[test]
    fn hebrew_in_english_reverses_only_the_hebrew() {
        let text = chars("ab \u{05d0}\u{05d1} cd");
        let (lv, para) = levels(&text, super::Direction::Auto);
        assert_eq!(para, 0);
        assert_eq!(lv, vec![0, 0, 0, 1, 1, 0, 0, 0]);
        assert_eq!(visual_order(&lv), vec![0, 1, 2, 4, 3, 5, 6, 7])
    }

    #[test]
    fn numbers_inside_rtl_stay_left_to_right() {
        let text = chars("\u{05d0} 12");
        let (lv, para) = levels(&text, super::Direction::Auto);
        assert_eq!(para, 1);
        assert_eq!(lv, vec![1, 1, 2, 2]);
        assert_eq!(visual_order(&lv), vec![2, 3, 1, 0])
    }

    #[test]
    fn explicit_rtl_paragraph_without_strong_text() {
        let (lv, para) = levels(&chars("!?"), super::Direction::Rtl);
        assert_eq!(para, 1);
        assert_eq!(lv, vec![1, 1])
    }

    #[test]
    fn isolates_keep_their_content_separate() {
        // RLI hebrew PDI inside English: the isolate does not affect the
        // paragraph level and its content is right-to-left.
        let text = chars("a \u{2067}\u{05d0}\u{05d1}\u{2069} b");
        let (lv, para) = levels(&text, super::Direction::Auto);
        assert_eq!(para, 0);
        assert_eq!(lv[3], 1);
        assert_eq!(lv[4], 1);
        assert_eq!(lv[0], 0)
    }

    #[test]
    fn brackets_mirror() {
        assert_eq!(mirror('('), ')');
        assert_eq!(mirror('a'), 'a')
    }
}
