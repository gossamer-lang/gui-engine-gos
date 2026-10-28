// indic: shaping for the Indic scripts under the OpenType "version 2" model
// (Devanagari, Bengali, Gurmukhi, Gujarati, Oriya, Tamil, Telugu, Kannada and
// Malayalam). Text splits into syllables. In each consonant syllable the base
// consonant is found and the reph, pre-base matras and the consonants before
// and after the base are marked; pre-base matras move to the front. The basic
// shaping features then run one at a time, each only on the glyphs it is meant
// for, and a final pass moves the reph and pre-base matras around the forms
// the font produced. The steps and their order follow Microsoft's script
// development specifications, as HarfBuzz implements them.

use crate::unicode;

use crate::text::font::Face;
use crate::text::opentype;
use crate::text::opentype::GlyphInfo;
use crate::text::ucd::{
    INDIC_POSITIONAL, INDIC_POSITIONAL_NAMES, INDIC_SYLLABIC, INDIC_SYLLABIC_NAMES,
};

// Character categories, carried on glyphs through substitutions.
const OTHER: i64 = 0;
const CONSONANT: i64 = 1;
// The script's Ra, which can become a reph.
const RA: i64 = 2;
// A ready-made reph character (Malayalam dot reph).
const REPHA: i64 = 3;
const VOWEL: i64 = 4;
const MATRA: i64 = 5;
const HALANT: i64 = 6;
const NUKTA: i64 = 7;
const JOINER: i64 = 8;
const NON_JOINER: i64 = 9;
// Bindu, visarga and other marks that end a syllable.
const MODIFIER: i64 = 10;
// A base that stands in for a consonant (NBSP, dotted circle).
const PLACEHOLDER: i64 = 11;

// Where a glyph sits relative to its syllable's base.
const REPH: i64 = 1;
const PRE_MATRA: i64 = 2;
const PRE_BASE: i64 = 3;
const BASE: i64 = 4;
const BELOW: i64 = 5;
const POST: i64 = 6;
const SYLLABLE_END: i64 = 7;

// Where a script's reph ends up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum RephPosition {
    AfterMain,
    AfterSub,
    BeforePost,
    AfterPost,
}

const SCRIPTS: [&str; 9] = [
    "dev2", "bng2", "gur2", "gjr2", "ory2", "tml2", "tel2", "knd2", "mlm2",
];

// The basic shaping features, applied one at a time in this order; `rphf`,
// `pref`, `blwf`, `half` and `pstf` apply only to glyphs that take them.
const BASIC: [&str; 11] = [
    "nukt", "akhn", "rphf", "rkrf", "pref", "blwf", "abvf", "half", "pstf", "vatu", "cjct",
];

// The presentation features, applied together after the final reordering.
const PRESENTATION: [&str; 9] = [
    "init", "pres", "abvs", "blws", "psts", "haln", "calt", "clig", "liga",
];

/// Whether `script` (an OpenType tag) shapes as an Indic script.
pub fn is_indic(script: &str) -> bool {
    SCRIPTS.contains(&script)
}

fn ra_of(script: &str) -> i64 {
    match script {
        "dev2" => 0x930,
        "bng2" => 0x9B0,
        "gur2" => 0xA30,
        "gjr2" => 0xAB0,
        "ory2" => 0xB30,
        "tml2" => 0xBB0,
        "tel2" => 0xC30,
        "knd2" => 0xCB0,
        "mlm2" => 0xD30,
        _ => -1,
    }
}

fn reph_position(script: &str) -> RephPosition {
    match script {
        "bng2" | "gur2" => RephPosition::AfterSub,
        "ory2" | "mlm2" => RephPosition::AfterMain,
        "tml2" | "tel2" | "knd2" => RephPosition::AfterPost,
        _ => RephPosition::BeforePost,
    }
}

// The value of the `start, end, value` triple covering `code`, or 0.
fn range_value(table: &[i64], code: i64) -> i64 {
    let mut lo = 0;
    let mut hi = table.len() / 3;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if code < table[mid * 3] {
            hi = mid;
        } else if code > table[mid * 3 + 1] {
            lo = mid + 1;
        } else {
            return table[mid * 3 + 2];
        }
    }
    0
}

fn positional(c: char) -> String {
    INDIC_POSITIONAL_NAMES[(range_value(&INDIC_POSITIONAL, c as i64)) as usize].to_string()
}

// The syllable category of `c` in `script`.
fn category(c: char, script: &str) -> i64 {
    let v = c as i64;
    if v == 0x200D {
        return JOINER;
    };
    if v == 0x200C {
        return NON_JOINER;
    };
    if v == ra_of(script) {
        return RA;
    };
    match INDIC_SYLLABIC_NAMES[(range_value(&INDIC_SYLLABIC, v)) as usize] {
        "Consonant" | "Consonant_Dead" | "Consonant_Head_Letter" | "Consonant_With_Stacker" => {
            CONSONANT
        }
        "Consonant_Placeholder" => PLACEHOLDER,
        "Consonant_Preceding_Repha" => REPHA,
        "Vowel_Independent" | "Vowel" => VOWEL,
        "Vowel_Dependent" => MATRA,
        "Virama" | "Invisible_Stacker" | "Pure_Killer" => HALANT,
        "Nukta" => NUKTA,
        "Bindu" | "Visarga" | "Syllable_Modifier" | "Cantillation_Mark" | "Gemination_Mark"
        | "Tone_Mark" | "Register_Shifter" => MODIFIER,
        _ => {
            if v == 0x25CC {
                PLACEHOLDER
            } else {
                OTHER
            }
        }
    }
}

fn is_consonant(cat: i64) -> bool {
    cat == CONSONANT || cat == RA || cat == PLACEHOLDER
}

fn is_joiner(cat: i64) -> bool {
    cat == JOINER || cat == NON_JOINER
}

// Split matras (drawn on both sides of the consonant) become their canonical
// parts, so the left part reorders on its own. Answers the characters and the
// source index of each.
fn decompose(chars: &[char]) -> (Vec<char>, Vec<i64>) {
    let mut out = vec![];
    let mut source = vec![];
    for (i, c) in chars.iter().enumerate() {
        let parts = if positional(*c).contains("_And_") {
            unicode::nfd(&c.to_string()).chars().collect::<Vec<_>>()
        } else {
            vec![*c]
        };
        for part in parts {
            out.push(part);
            source.push(i as i64)
        }
    }
    (out, source)
}

// The end (exclusive) of the syllable starting at `start`: a consonant
// syllable (an optional reph, consonants joined by halants, then matras and
// modifiers), a vowel syllable, or a lone character.
fn syllable_end(cats: &[i64], start: i64) -> i64 {
    let n = cats.len();
    let at = |k: i64, cat: i64| -> bool { k < n as i64 && cats[k as usize] == cat };
    let consonant_at = |k: i64| -> bool { k < n as i64 && is_consonant(cats[k as usize]) };
    let joiner_at = |k: i64| -> bool { k < n as i64 && is_joiner(cats[k as usize]) };
    let mut k = if at(start, REPHA) && consonant_at(start + 1) {
        start + 1
    } else {
        start
    };
    if consonant_at(k) {
        loop {
            k += 1;
            if at(k, NUKTA) {
                k += 1
            };
            if at(k, HALANT) {
                let next = if joiner_at(k + 1) { k + 2 } else { k + 1 };
                if consonant_at(next) {
                    k = next;
                    continue;
                }
                k += 1;
                if joiner_at(k) {
                    k += 1
                };
                break;
            }
            if joiner_at(k) && at(k + 1, HALANT) && consonant_at(k + 2) {
                k += 2;
                continue;
            }
            break;
        }
    } else if at(k, VOWEL) {
        k += 1;
        if at(k, NUKTA) {
            k += 1
        }
    } else {
        return start + 1;
    }
    if at(k, JOINER) && at(k + 1, MATRA) {
        k += 1
    };
    while at(k, MATRA) || at(k, NUKTA) || (at(k, HALANT) && at(k - 1, MATRA)) {
        k += 1
    }
    while at(k, MODIFIER) {
        k += 1
    }
    k
}

/// Shapes a run of Indic text in logical order; `clusters` holds each
/// character's byte offset. Every glyph of a syllable carries the syllable's
/// first offset, since reordering mixes its characters.
pub fn shape(face: &Face, script: &str, chars: &[char], clusters: &[i64]) -> Vec<GlyphInfo> {
    let (split, source) = decompose(chars);
    let cats = split
        .iter()
        .map(|c| category(*c, script))
        .collect::<Vec<_>>();
    let mut out = vec![];
    let mut start = 0;
    while start < split.len() {
        let end = syllable_end(&cats, start as i64);
        let word_start = start == 0
            || !(unicode::is_letter(split[start - 1]) || unicode::is_mark(split[start - 1]));
        let mut glyphs = vec![];
        for k in start..end as usize {
            let mut g = GlyphInfo::new(
                face.glyph_for(split[k]),
                clusters[(source[start]) as usize],
                "",
            );
            g.ignorable = is_joiner(cats[k]);
            g.category = cats[k];
            g.role = if cats[k] == MATRA && positional(split[k]) == "Left" {
                PRE_MATRA
            } else {
                0
            };
            glyphs.push(g)
        }
        shape_syllable(face, script, &mut glyphs, word_start);
        for g in glyphs {
            out.push(g)
        }
        start = end as usize;
    }
    out
}

fn shape_syllable(face: &Face, script: &str, glyphs: &mut Vec<GlyphInfo>, word_start: bool) {
    opentype::substitute(face, script, glyphs, &["locl", "ccmp"]);
    if !glyphs.is_empty() && (is_consonant(glyphs[0].category) || glyphs[0].category == REPHA) {
        initial_reorder(face, script, glyphs)
    }
    if word_start && !glyphs.is_empty() {
        glyphs[0].takes.push("init".to_string())
    }
    for feature in BASIC {
        opentype::substitute(face, script, glyphs, &[feature])
    }
    final_reorder(script, glyphs);
    opentype::substitute(face, script, glyphs, &PRESENTATION)
}

// Finds the base consonant, gives every glyph its position and the masked
// features it takes, and moves pre-base matras to the front, after a reph.
fn initial_reorder(face: &Face, script: &str, glyphs: &mut Vec<GlyphInfo>) {
    let n = glyphs.len();
    let cats = glyphs.iter().map(|g| g.category).collect::<Vec<_>>();
    let cat = |k: i64| -> i64 { cats[k as usize] };
    // A reph: a reph character, or Ra and halant opening the syllable with a
    // consonant after them (Ra, halant, ZWJ asks for the half form instead).
    let reph = if cat(0) == REPHA {
        1
    } else if n > 2
        && cat(0) == RA
        && cat(1) == HALANT
        && cat(2) != JOINER
        && (2..n).any(|k| is_consonant(cat(k as i64)))
    {
        2
    } else {
        0
    };
    // The base: scanning back, the first consonant that does not take a
    // below- or post-base form from the halant before it.
    let mut base: i64 = -1;
    for k in (reph..n).rev() {
        if is_consonant(cat(k as i64)) {
            let earlier = (reph..k).any(|j| is_consonant(cat(j as i64)));
            let formed = k > reph
                && cat((k - 1) as i64) == HALANT
                && earlier
                && forms_after_base(face, script, glyphs[k - 1].gid, glyphs[k].gid);
            if !formed {
                base = k as i64;
                break;
            }
        }
    }
    if base < 0 {
        base = (reph..n)
            .find(|&j| is_consonant(cat(j as i64)))
            .unwrap_or(reph) as i64;
    }
    for (i, g) in glyphs.iter_mut().enumerate().take(n) {
        let c = cat(i as i64);
        if g.role == PRE_MATRA {
            continue;
        };
        g.role = if i < reph {
            REPH
        } else if c == MODIFIER {
            SYLLABLE_END
        } else if i < base as usize {
            PRE_BASE
        } else if i == base as usize {
            BASE
        } else if c == MATRA {
            POST
        } else {
            BELOW
        };
        match g.role {
            REPH => g.takes.push("rphf".to_string()),
            PRE_BASE => g.takes.push("half".to_string()),
            BELOW => {
                g.takes.push("blwf".to_string());
                g.takes.push("pstf".to_string())
            }
            _ => (),
        }
    }
    let mut ordered = glyphs[0..reph].to_vec();
    for g in &glyphs[reph..n] {
        if g.role == PRE_MATRA {
            ordered.push(g.clone())
        }
    }
    for g in &glyphs[reph..n] {
        if g.role != PRE_MATRA {
            ordered.push(g.clone())
        }
    }
    *glyphs = ordered.to_vec()
}

// Whether the font forms a below- or post-base glyph from `halant` then `c`.
fn forms_after_base(face: &Face, script: &str, halant: i64, c: i64) -> bool {
    for feature in ["blwf", "pstf"] {
        let mut pair = vec![
            GlyphInfo::new(halant, 0, feature),
            GlyphInfo::new(c, 0, feature),
        ];
        opentype::substitute(face, script, &mut pair, &[feature]);
        if pair.len() != 2 || pair[0].gid != halant || pair[1].gid != c {
            return true;
        }
    }
    false
}

// Moves a pre-base matra past the last halant still standing on its own before
// the base, and a formed reph to its script's place.
fn final_reorder(script: &str, glyphs: &mut Vec<GlyphInfo>) {
    let Some(base) = glyphs.iter().position(|g| g.role == BASE) else {
        return;
    };
    if let Some(m) = glyphs.iter().position(|g| g.role == PRE_MATRA)
        && m < base
    {
        let target = glyphs[m + 1..base]
            .iter()
            .rposition(|g| g.category == HALANT)
            .map_or(m, |j| m + 1 + j);
        if target > m && m < glyphs.len() {
            let matra = glyphs.remove(m);
            glyphs.insert(target.min(glyphs.len()), matra);
        }
    }
    // Ra and halant that the font did not join into a reph stay in place.
    if glyphs.iter().filter(|g| g.role == REPH).count() != 1 {
        return;
    };
    let Some(r) = glyphs.iter().position(|g| g.role == REPH) else {
        return;
    };
    let Some(main) = glyphs.iter().position(|g| g.role == BASE) else {
        return;
    };
    let position = reph_position(script);
    let target = glyphs
        .iter()
        .enumerate()
        .skip(main + 1)
        .find(|(_, g)| match position {
            RephPosition::AfterMain => true,
            RephPosition::AfterSub | RephPosition::BeforePost => {
                g.role == POST || g.role == SYLLABLE_END
            }
            RephPosition::AfterPost => g.role == SYLLABLE_END,
        })
        .map_or(glyphs.len(), |(j, _)| j);
    if r < glyphs.len() {
        let reph = glyphs.remove(r);
        glyphs.insert((target - 1).min(glyphs.len()), reph);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BASE, BELOW, CONSONANT, HALANT, MATRA, MODIFIER, PRE_BASE, PRE_MATRA, RA, REPH, category,
        decompose, shape, syllable_end,
    };
    use crate::text::font::Face;

    // The (category, role) of each glyph from shaping `text` with a face that
    // has no layout tables, so only the reordering acts.
    fn reordered(text: &str) -> Vec<(i64, i64)> {
        let chars: Vec<char> = text.chars().collect();
        let clusters: Vec<i64> = (0..chars.len() as i64).collect();
        shape(&Face::default(), "dev2", &chars, &clusters)
            .iter()
            .map(|g| (g.category, g.role))
            .collect::<Vec<_>>()
    }

    fn ends(text: &str, script: &str) -> Vec<i64> {
        let (chars, _) = decompose(&text.chars().collect::<Vec<_>>());
        let cats = chars
            .iter()
            .map(|c| category(*c, script))
            .collect::<Vec<_>>();
        let mut out = vec![];
        let mut start = 0;
        while start < cats.len() {
            start = syllable_end(&cats, start as i64) as usize;
            out.push(start as i64)
        }
        out
    }

    #[test]
    fn devanagari_characters_take_their_syllable_categories() {
        assert_eq!(category('\u{915}', "dev2"), CONSONANT);
        assert_eq!(category('\u{930}', "dev2"), RA);
        assert_eq!(category('\u{94D}', "dev2"), HALANT);
        assert_eq!(category('\u{93F}', "dev2"), MATRA);
        assert_eq!(category('\u{902}', "dev2"), MODIFIER)
    }

    #[test]
    fn syllables_join_consonants_through_halants() {
        // ki | kssa | rka | na ma sa-ta-e | ha-i na-da-ii
        assert_eq!(ends("\u{915}\u{93F}", "dev2"), vec![2]);
        assert_eq!(ends("\u{915}\u{94D}\u{937}", "dev2"), vec![3]);
        assert_eq!(ends("\u{930}\u{94D}\u{915}", "dev2"), vec![3]);
        assert_eq!(
            ends("\u{928}\u{92E}\u{938}\u{94D}\u{924}\u{947}", "dev2"),
            vec![1, 2, 6]
        );
        assert_eq!(
            ends("\u{939}\u{93F}\u{928}\u{94D}\u{926}\u{940}", "dev2"),
            vec![2, 6]
        );
        // A trailing halant and a modifier stay in their syllable.
        assert_eq!(ends("\u{915}\u{94D} \u{915}\u{902}", "dev2"), vec![2, 3, 5])
    }

    #[test]
    fn split_matras_decompose_into_their_parts() {
        // Bengali o: e-matra (left) then aa-matra (right).
        let (chars, source) = decompose(&['\u{995}', '\u{9CB}']);
        assert_eq!(chars, vec!['\u{995}', '\u{9C7}', '\u{9BE}']);
        assert_eq!(source, vec![0, 1, 1])
    }

    #[test]
    fn a_pre_base_matra_moves_before_its_consonants() {
        // ki: the i-matra is drawn first.
        assert_eq!(
            reordered("\u{915}\u{93F}"),
            vec![(MATRA, PRE_MATRA), (CONSONANT, BASE)]
        );
        // sti: with no font to form a half s, its halant stands alone and the
        // matra follows it, next to the t.
        assert_eq!(
            reordered("\u{938}\u{94D}\u{924}\u{93F}"),
            vec![
                (CONSONANT, PRE_BASE),
                (HALANT, PRE_BASE),
                (MATRA, PRE_MATRA),
                (CONSONANT, BASE)
            ]
        )
    }

    #[test]
    fn ra_and_halant_before_a_consonant_take_the_reph_form() {
        // rki: the reph stays first for `rphf`; with no font to join it, Ra and
        // halant remain apart and in place.
        assert_eq!(
            reordered("\u{930}\u{94D}\u{915}\u{93F}"),
            vec![
                (RA, REPH),
                (HALANT, REPH),
                (MATRA, PRE_MATRA),
                (CONSONANT, BASE)
            ]
        );
        // Ra, halant, ZWJ asks for the half form instead.
        assert_eq!(
            reordered("\u{930}\u{94D}\u{200D}\u{915}")[0],
            (RA, PRE_BASE)
        )
    }

    #[test]
    fn a_final_halant_follows_its_base() {
        assert_eq!(
            reordered("\u{915}\u{94D}"),
            vec![(CONSONANT, BASE), (HALANT, BELOW)]
        )
    }
}
