// opentype: GSUB substitution and GPOS positioning over a glyph buffer.
//
// Supported: GSUB single, multiple, ligature, contextual and chained
// contextual (all three formats), and extension lookups; GPOS single,
// pair (kerning), cursive, mark-to-base, mark-to-ligature (as base),
// mark-to-mark, and extension lookups; lookup flags for skipping marks,
// bases, ligatures, and mark attachment classes through GDEF.
// Not supported: alternate and reverse-chaining substitution, contextual
// positioning, device tables, and variable-font deltas. Fonts using
// them render with the supported lookups applied and the rest skipped.

use crate::text::font::{Face, be_i16, be_u16, tag};
use parking_lot::Mutex;
use std::sync::LazyLock;

/// One glyph in a shaping buffer.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GlyphInfo {
    pub gid: i64,
    /// Byte offset of the first source character this glyph renders.
    pub cluster: i64,
    /// The masked features this glyph takes (Arabic positional forms, Indic
    /// reph, half, below- and post-base and initial forms); see `MASKED`.
    pub takes: Vec<String>,
    /// A default-ignorable character the font may use in substitutions but
    /// that never shows.
    pub ignorable: bool,
    /// Shaper classification carried through substitutions (see `indic`).
    pub category: i64,
    pub role: i64,
    /// Positioning in font units, filled by GPOS.
    pub x_advance: i64,
    pub y_advance: i64,
    pub x_offset: i64,
    pub y_offset: i64,
    /// Number of source characters a ligature glyph stands for.
    pub components: i64,
    /// The glyph a right-to-left cursive attachment aligns this one to
    /// vertically, or -1; resolved when its lookup finishes.
    pub cursive_parent: i64,
}

impl Default for GlyphInfo {
    fn default() -> GlyphInfo {
        GlyphInfo {
            gid: 0,
            cluster: 0,
            takes: vec![],
            ignorable: false,
            category: 0,
            role: 0,
            x_advance: 0,
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
            components: 0,
            cursive_parent: -1,
        }
    }
}

impl GlyphInfo {
    /// A glyph for `cluster`; `form` names a masked feature it takes, or "".
    pub fn new(gid: i64, cluster: i64, form: &str) -> GlyphInfo {
        let takes = if form.is_empty() {
            vec![]
        } else {
            vec![form.to_string()]
        };
        GlyphInfo {
            gid,
            cluster,
            takes,
            x_advance: 0,
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
            components: 1,
            cursive_parent: -1,
            ignorable: false,
            category: 0,
            role: 0,
        }
    }
}

// Lookup flags.
const RIGHT_TO_LEFT: i64 = 0x1;
const USE_MARK_FILTERING_SET: i64 = 0x10;

// GDEF glyph classes.
const BASE: i64 = 1;
const LIGATURE: i64 = 2;
const MARK: i64 = 3;

/// The features applied for a script, in application order.
pub fn substitute(face: &Face, script: &str, glyphs: &mut Vec<GlyphInfo>, features: &[&str]) {
    let Some((gsub, _)) = face.table("GSUB") else {
        return;
    };
    let d = face.bytes();
    let gdef = gdef_of(face);
    for (lookup, feature) in lookups_of(face, d, gsub, script, features) {
        apply_gsub_lookup(d, gsub, &gdef, lookup, &feature, glyphs)
    }
}

/// Applies kerning, cursive and mark positioning to glyphs in logical order;
/// advances must be preset. `rtl` is the run's direction.
pub fn position(
    face: &Face,
    script: &str,
    glyphs: &mut Vec<GlyphInfo>,
    features: &[&str],
    rtl: bool,
) -> bool {
    let d = face.bytes();
    let gdef = gdef_of(face);
    match face.table("GPOS") {
        Some((gpos, _)) => {
            let lookups = lookups_of(face, d, gpos, script, features);
            if lookups.is_empty() {
                return false;
            }
            for (lookup, _) in lookups {
                apply_gpos_lookup(d, gpos, &gdef, lookup, glyphs, rtl)
            }
            true
        }
        None => false,
    }
}

/// Old-style `kern` table pairs (format 0), for fonts without GPOS kerning.
pub fn kern_table(face: &Face, glyphs: &mut [GlyphInfo]) {
    let Some((kern, len)) = face.table("kern") else {
        return;
    };
    let d = face.bytes();
    let tables = be_u16(d, kern + 2);
    let mut at = kern + 4;
    let mut t = 0;
    while t < tables && at < kern + len {
        let length = be_u16(d, at + 2);
        let coverage = be_u16(d, at + 4);
        if coverage >> 8 == 0 && coverage & 1 != 0 {
            let pairs = be_u16(d, at + 6);
            let mut i = 0;
            while i + 1 < glyphs.len() {
                let key = glyphs[i].gid * 65_536 + glyphs[i + 1].gid;
                let mut lo = 0;
                let mut hi = pairs - 1;
                while lo <= hi {
                    let mid = (lo + hi) / 2;
                    let p = at + 14 + mid * 6;
                    let k = be_u16(d, p) * 65_536 + be_u16(d, p + 2);
                    if k < key {
                        lo = mid + 1;
                    } else if k > key {
                        hi = mid - 1;
                    } else {
                        glyphs[i].x_advance += be_i16(d, p + 4);
                        break;
                    }
                }
                i += 1;
            }
        }
        at += length;
        t += 1;
    }
}

// ---- common tables ----

/// GDEF offsets of the glyph class definition, the mark attachment class
/// definition and the mark glyph sets, each -1 when absent.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Gdef<'a> {
    classes: i64,
    attach: i64,
    mark_sets: i64,
    /// The font file the offsets are into.
    data: &'a [u8],
}

impl Default for Gdef<'_> {
    fn default() -> Self {
        Gdef {
            classes: -1,
            attach: -1,
            mark_sets: -1,
            data: &[],
        }
    }
}

fn gdef_of(face: &Face) -> Gdef<'_> {
    let d = face.bytes();
    match face.table("GDEF") {
        Some((g, _)) => {
            let c = be_u16(d, g + 4);
            let a = be_u16(d, g + 10);
            // Mark glyph sets arrived with GDEF 1.2.
            let m = if be_u16(d, g) == 1 && be_u16(d, g + 2) >= 2 {
                be_u16(d, g + 12)
            } else {
                0
            };
            Gdef {
                classes: if c == 0 { -1 } else { g + c },
                attach: if a == 0 { -1 } else { g + a },
                mark_sets: if m == 0 { -1 } else { g + m },
                data: d,
            }
        }
        None => Gdef {
            classes: -1,
            attach: -1,
            mark_sets: -1,
            data: d,
        },
    }
}

fn glyph_class(g: &Gdef, gid: i64) -> i64 {
    if g.classes < 0 {
        0
    } else {
        class_of(g.data, g.classes, gid)
    }
}

/// The lookups a feature list applies, in order: (lookup index, feature tag).
type Plan = Vec<(i64, String)>;

/// The (lookup index, feature tag) pairs to apply, in lookup-list order.
// The lookups a registered face applies, by (registry slot, table offset,
// script, features): reading the script and feature lists is costly per run,
// and they never change for a loaded face.
static PLANS: crate::memory::SharedCache<(i64, i64, String, String), Plan> =
    LazyLock::new(|| Mutex::new(crate::memory::ByteCache::new(4 * 1024 * 1024)));

pub(crate) fn purge_cache() {
    PLANS.lock().clear();
}

fn lookups_of(face: &Face, d: &[u8], table: i64, script: &str, features: &[&str]) -> Plan {
    if face.slot < 0 {
        return lookups_for(d, table, script, features);
    }
    let key = (face.slot, table, script.to_string(), features.join(","));
    if let Some(plan) = PLANS.lock().get(&key) {
        return plan.to_vec();
    };
    let plan = lookups_for(d, table, script, features);
    let bytes = key.2.capacity()
        + key.3.capacity()
        + plan.capacity() * size_of::<(i64, String)>()
        + plan.iter().map(|(_, tag)| tag.capacity()).sum::<usize>();
    PLANS.lock().insert(key, plan.clone(), bytes);
    plan
}

fn lookups_for(d: &[u8], table: i64, script: &str, features: &[&str]) -> Plan {
    let scripts = table + be_u16(d, table + 4);
    let feature_list = table + be_u16(d, table + 6);
    let mut lang = -1;
    for wanted in [script, "DFLT", "latn"] {
        let count = be_u16(d, scripts);
        let mut i = 0;
        while i < count {
            let rec = scripts + 2 + i * 6;
            if tag(d, rec) == wanted {
                let s = scripts + be_u16(d, rec + 4);
                let default = be_u16(d, s);
                if default != 0 {
                    lang = s + default;
                }
                break;
            }
            i += 1;
        }
        if lang >= 0 {
            break;
        }
    }
    if lang < 0 {
        return vec![];
    }
    let mut picked: Vec<(i64, String)> = vec![];
    let count = be_u16(d, lang + 4);
    let mut i = 0;
    while i < count {
        let fi = be_u16(d, lang + 6 + i * 2);
        let rec = feature_list + 2 + fi * 6;
        let ftag = tag(d, rec);
        if features.contains(&ftag.as_str()) {
            let f = feature_list + be_u16(d, rec + 4);
            let n = be_u16(d, f + 2);
            let mut k = 0;
            while k < n {
                picked.push((be_u16(d, f + 4 + k * 2), ftag.clone()));
                k += 1;
            }
        }
        i += 1;
    }
    picked.sort_by_key(|p| p.0);
    picked
}

/// The coverage index of `gid`, or -1.
fn coverage(d: &[u8], at: i64, gid: i64) -> i64 {
    let kind = be_u16(d, at);
    if kind == 1 {
        let count = be_u16(d, at + 2);
        let mut lo = 0;
        let mut hi = count - 1;
        while lo <= hi {
            let mid = (lo + hi) / 2;
            let g = be_u16(d, at + 4 + mid * 2);
            if g < gid {
                lo = mid + 1;
            } else if g > gid {
                hi = mid - 1;
            } else {
                return mid;
            }
        }
        return -1;
    }
    if kind == 2 {
        let count = be_u16(d, at + 2);
        let mut lo = 0;
        let mut hi = count - 1;
        while lo <= hi {
            let mid = (lo + hi) / 2;
            let r = at + 4 + mid * 6;
            let start = be_u16(d, r);
            let end = be_u16(d, r + 2);
            if gid < start {
                hi = mid - 1;
            } else if gid > end {
                lo = mid + 1;
            } else {
                return be_u16(d, r + 4) + gid - start;
            }
        }
    }
    -1
}

/// The class `gid` belongs to in the class definition at `at`.
fn class_of(d: &[u8], at: i64, gid: i64) -> i64 {
    let kind = be_u16(d, at);
    if kind == 1 {
        let start = be_u16(d, at + 2);
        let count = be_u16(d, at + 4);
        if gid >= start && gid < start + count {
            return be_u16(d, at + 6 + (gid - start) * 2);
        }
        return 0;
    }
    if kind == 2 {
        let count = be_u16(d, at + 2);
        let mut lo = 0;
        let mut hi = count - 1;
        while lo <= hi {
            let mid = (lo + hi) / 2;
            let r = at + 4 + mid * 6;
            let start = be_u16(d, r);
            let end = be_u16(d, r + 2);
            if gid < start {
                hi = mid - 1;
            } else if gid > end {
                lo = mid + 1;
            } else {
                return be_u16(d, r + 4);
            }
        }
    }
    0
}

/// A lookup's type, flag (with any mark filtering set index in bits 16 and
/// up), and subtable offsets with extensions resolved.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Lookup {
    kind: i64,
    flag: i64,
    subtables: Vec<i64>,
}

fn read_lookup(d: &[u8], table: i64, index: i64, extension: i64) -> Lookup {
    let list = table + be_u16(d, table + 8);
    let at = list + be_u16(d, list + 2 + index * 2);
    let declared = be_u16(d, at);
    let mut kind = declared;
    let flag = be_u16(d, at + 2);
    let count = be_u16(d, at + 4);
    let mut subtables = vec![];
    let mut i = 0;
    while i < count {
        let mut st = at + be_u16(d, at + 6 + i * 2);
        // Every subtable of an extension lookup is an extension header
        // naming the real type and pointing at the real subtable.
        if declared == extension {
            kind = be_u16(d, st + 2);
            st = st + be_u16(d, st + 4) * 65_536 + be_u16(d, st + 6);
        }
        subtables.push(st);
        i += 1;
    }
    // A mark filtering set index rides above the 16 flag bits.
    let props = if flag & USE_MARK_FILTERING_SET != 0 {
        flag | (be_u16(d, at + 6 + count * 2) << 16)
    } else {
        flag
    };
    Lookup {
        kind,
        flag: props,
        subtables,
    }
}

// Whether a lookup with `flag` skips glyph `gid`.
fn skipped(g: &Gdef, flag: i64, gid: i64) -> bool {
    if flag & 0xe == 0 && flag & 0xff00 == 0 && flag & USE_MARK_FILTERING_SET == 0 {
        return false;
    }
    let class = glyph_class(g, gid);
    if flag & 2 != 0 && class == BASE {
        return true;
    };
    if flag & 4 != 0 && class == LIGATURE {
        return true;
    };
    if flag & 8 != 0 && class == MARK {
        return true;
    };
    if class != MARK {
        return false;
    };
    if flag & USE_MARK_FILTERING_SET != 0 {
        return !in_mark_set(g, flag >> 16, gid);
    }
    let attach = (flag >> 8) & 0xff;
    if attach != 0 && g.attach >= 0 {
        return class_of(g.data, g.attach, gid) != attach;
    }
    false
}

// Whether mark glyph set `set` of GDEF covers `gid`.
fn in_mark_set(g: &Gdef, set: i64, gid: i64) -> bool {
    if g.mark_sets < 0 || set >= be_u16(g.data, g.mark_sets + 2) {
        return false;
    };
    let at = be_u16(g.data, g.mark_sets + 4 + set * 4) * 65_536
        + be_u16(g.data, g.mark_sets + 6 + set * 4);
    coverage(g.data, g.mark_sets + at, gid) >= 0
}

fn next_unskipped(g: &Gdef, flag: i64, glyphs: &[GlyphInfo], from: i64) -> i64 {
    let mut i = from;
    while i < glyphs.len() as i64 {
        if i % 256 == 0 && !crate::text::layout::work_active() {
            break;
        }
        if !skipped(g, flag, glyphs[i as usize].gid) {
            return i;
        }
        i += 1;
    }
    -1
}

fn prev_unskipped(g: &Gdef, flag: i64, glyphs: &[GlyphInfo], from: i64) -> i64 {
    let mut i = from;
    while i >= 0 {
        if !skipped(g, flag, glyphs[i as usize].gid) {
            return i;
        }
        i -= 1;
    }
    -1
}

// ---- GSUB ----

fn apply_gsub_lookup(
    d: &[u8],
    gsub: i64,
    g: &Gdef,
    index: i64,
    feature: &str,
    glyphs: &mut Vec<GlyphInfo>,
) {
    let lookup = read_lookup(d, gsub, index, 7);
    let mut i = 0;
    while i < glyphs.len() {
        if i % 256 == 0 && !crate::text::layout::work_active() {
            break;
        }
        if skipped(g, lookup.flag, glyphs[i].gid)
            || (MASKED.contains(&feature) && !glyphs[i].takes.iter().any(|t| t == feature))
        {
            i += 1;
            continue;
        }
        let mut advanced = false;
        for st in &lookup.subtables {
            let used = gsub_subtable(
                d,
                gsub,
                g,
                lookup.kind,
                lookup.flag,
                *st,
                glyphs,
                i as i64,
                0,
            );
            if used > 0 {
                i += used as usize;
                advanced = true;
                break;
            }
        }
        if !advanced {
            i += 1;
        }
    }
}

/// Features that apply only to the glyphs whose `takes` names them.
pub const MASKED: [&str; 9] = [
    "isol", "init", "medi", "fina", "rphf", "pref", "blwf", "half", "pstf",
];

// Applies one subtable at `i`; answers how many glyphs to move past (0 if
// it did not apply).
fn gsub_subtable(
    d: &[u8],
    gsub: i64,
    g: &Gdef,
    kind: i64,
    flag: i64,
    st: i64,
    glyphs: &mut Vec<GlyphInfo>,
    i: i64,
    depth: i64,
) -> i64 {
    let gid = glyphs[i as usize].gid;
    match kind {
        1 => {
            let cov = coverage(d, st + be_u16(d, st + 2), gid);
            if cov < 0 {
                return 0;
            };
            let fmt = be_u16(d, st);
            glyphs[i as usize].gid = if fmt == 1 {
                (gid + be_i16(d, st + 4) + 65_536) % 65_536
            } else {
                be_u16(d, st + 6 + cov * 2)
            };
            1
        }
        2 => {
            let cov = coverage(d, st + be_u16(d, st + 2), gid);
            if cov < 0 {
                return 0;
            };
            let seq = st + be_u16(d, st + 6 + cov * 2);
            let n = be_u16(d, seq);
            if n == 0 {
                let _ = glyphs.remove(i as usize);
                return 0;
            }
            glyphs[i as usize].gid = be_u16(d, seq + 2);
            let base = glyphs[i as usize].clone();
            let mut k = 1;
            while k < n {
                let mut extra = base.clone();
                extra.gid = be_u16(d, seq + 2 + k * 2);
                glyphs.insert((i + k) as usize, extra.clone());
                k += 1;
            }
            n
        }
        4 => {
            let cov = coverage(d, st + be_u16(d, st + 2), gid);
            if cov < 0 {
                return 0;
            };
            let set = st + be_u16(d, st + 6 + cov * 2);
            let count = be_u16(d, set);
            let mut k = 0;
            while k < count {
                let lig = set + be_u16(d, set + 2 + k * 2);
                let comps = be_u16(d, lig + 2);
                let mut positions = vec![i];
                let mut j = i;
                let mut ok = true;
                let mut c = 1;
                while c < comps {
                    j = next_unskipped(g, flag, &*glyphs, j + 1);
                    if j < 0 || glyphs[j as usize].gid != be_u16(d, lig + 4 + (c - 1) * 2) {
                        ok = false;
                        break;
                    }
                    positions.push(j);
                    c += 1;
                }
                if ok {
                    glyphs[i as usize].gid = be_u16(d, lig);
                    glyphs[i as usize].components = comps;
                    // Remove the absorbed components, last first.
                    let mut p = positions.len() - 1;
                    while p >= 1 {
                        let _ = glyphs.remove(positions[p] as usize);
                        p -= 1;
                    }
                    return 1;
                }
                k += 1;
            }
            0
        }
        5 | 6 => context_subtable(
            d,
            gsub,
            g,
            kind,
            flag,
            st,
            glyphs,
            i,
            depth,
            &Pass::Substitute,
        ),
        _ => 0,
    }
}

// Which table a contextual lookup's nested lookups come from. Positioning
// carries the run direction cursive attachment needs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Pass {
    Substitute,
    Position { rtl: bool },
}

// Contextual and chained contextual lookups (GSUB 5 and 6, GPOS 7 and 8),
// formats 1-3; nested lookups run through the subtable function of `pass`.
fn context_subtable(
    d: &[u8],
    table: i64,
    g: &Gdef,
    kind: i64,
    flag: i64,
    st: i64,
    glyphs: &mut Vec<GlyphInfo>,
    i: i64,
    depth: i64,
    pass: &Pass,
) -> i64 {
    if depth > 6 {
        return 0;
    }
    let fmt = be_u16(d, st);
    let chained = kind == 6 || kind == 8;
    if fmt == 3 {
        let mut at = st + 2;
        let mut backtrack = vec![];
        let mut lookahead = vec![];
        if chained {
            let n = be_u16(d, at);
            let mut k = 0;
            while k < n {
                backtrack.push(st + be_u16(d, at + 2 + k * 2));
                k += 1
            }
            at += 2 + n * 2;
        }
        let input_count = be_u16(d, at);
        let mut input = vec![];
        let subst_count;
        if chained {
            let mut k = 0;
            while k < input_count {
                input.push(st + be_u16(d, at + 2 + k * 2));
                k += 1
            }
            at += 2 + input_count * 2;
            let n = be_u16(d, at);
            k = 0;
            while k < n {
                lookahead.push(st + be_u16(d, at + 2 + k * 2));
                k += 1
            }
            at += 2 + n * 2;
            subst_count = be_u16(d, at);
            at += 2;
        } else {
            subst_count = be_u16(d, at + 2);
            let mut k = 0;
            while k < input_count {
                input.push(st + be_u16(d, at + 4 + k * 2));
                k += 1
            }
            at += 4 + input_count * 2;
        }
        let Some(positions) =
            match_coverages(d, g, flag, &*glyphs, i, &backtrack, &input, &lookahead)
        else {
            return 0;
        };
        apply_nested(
            d,
            table,
            g,
            glyphs,
            &positions,
            at,
            subst_count,
            depth,
            pass,
        );
        return 1;
    }
    let cov = coverage(d, st + be_u16(d, st + 2), glyphs[i as usize].gid);
    if cov < 0 {
        return 0;
    }
    // Formats 1 (glyph sequences) and 2 (class sequences).
    let mut backtrack_classes = -1;
    let mut input_classes = -1;
    let mut lookahead_classes = -1;
    let sets;
    if fmt == 2 {
        if chained {
            backtrack_classes = st + be_u16(d, st + 4);
            input_classes = st + be_u16(d, st + 6);
            lookahead_classes = st + be_u16(d, st + 8);
            sets = st + 10;
        } else {
            input_classes = st + be_u16(d, st + 4);
            sets = st + 6;
        }
    } else {
        sets = st + 4;
    }
    let key = if fmt == 2 {
        class_of(d, input_classes, glyphs[i as usize].gid)
    } else {
        cov
    };
    let set_count = be_u16(d, sets);
    if key >= set_count {
        return 0;
    }
    let set_off = be_u16(d, sets + 2 + key * 2);
    if set_off == 0 {
        return 0;
    }
    let set = st + set_off;
    let rules = be_u16(d, set);
    let mut r = 0;
    while r < rules {
        let rule = set + be_u16(d, set + 2 + r * 2);
        let mut at = rule;
        let mut backtrack = vec![];
        let mut lookahead = vec![];
        if chained {
            let n = be_u16(d, at);
            let mut k = 0;
            while k < n {
                backtrack.push(be_u16(d, at + 2 + k * 2));
                k += 1
            }
            at += 2 + n * 2;
        }
        let input_count = be_u16(d, at);
        let mut input = vec![];
        let subst_count;
        if chained {
            let mut k = 1;
            while k < input_count {
                input.push(be_u16(d, at + 2 + (k - 1) * 2));
                k += 1
            }
            at += 2 + (input_count - 1).max(0) * 2;
            let n = be_u16(d, at);
            k = 0;
            while k < n {
                lookahead.push(be_u16(d, at + 2 + k * 2));
                k += 1
            }
            at += 2 + n * 2;
            subst_count = be_u16(d, at);
            at += 2;
        } else {
            subst_count = be_u16(d, at + 2);
            let mut k = 1;
            while k < input_count {
                input.push(be_u16(d, at + 4 + (k - 1) * 2));
                k += 1
            }
            at += 4 + (input_count - 1).max(0) * 2;
        }
        let seq_match = |gid: i64, value: i64, classes: i64| -> bool {
            if fmt == 2 {
                class_of(d, classes, gid) == value
            } else {
                gid == value
            }
        };
        let mut positions = vec![i];
        let mut ok = true;
        let mut j = i;
        for value in input {
            j = next_unskipped(g, flag, &*glyphs, j + 1);
            if j < 0 || !seq_match(glyphs[j as usize].gid, value, input_classes) {
                ok = false;
                break;
            };
            positions.push(j)
        }
        if ok {
            let mut b = i;
            for value in backtrack {
                b = prev_unskipped(g, flag, &*glyphs, b - 1);
                if b < 0 || !seq_match(glyphs[b as usize].gid, value, backtrack_classes) {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            let mut l = j;
            for value in lookahead {
                l = next_unskipped(g, flag, &*glyphs, l + 1);
                if l < 0 || !seq_match(glyphs[l as usize].gid, value, lookahead_classes) {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            apply_nested(
                d,
                table,
                g,
                glyphs,
                &positions,
                at,
                subst_count,
                depth,
                pass,
            );
            return 1;
        }
        r += 1;
    }
    0
}

fn match_coverages(
    d: &[u8],
    g: &Gdef,
    flag: i64,
    glyphs: &[GlyphInfo],
    i: i64,
    backtrack: &[i64],
    input: &[i64],
    lookahead: &[i64],
) -> Option<Vec<i64>> {
    if input.is_empty() {
        return None;
    }
    let mut positions = vec![];
    let mut j = i;
    let mut first = true;
    for cov in input {
        if !first {
            j = next_unskipped(g, flag, glyphs, j + 1);
        }
        first = false;
        if j < 0 || coverage(d, *cov, glyphs[j as usize].gid) < 0 {
            return None;
        }
        positions.push(j)
    }
    let mut b = i;
    for cov in backtrack {
        b = prev_unskipped(g, flag, glyphs, b - 1);
        if b < 0 || coverage(d, *cov, glyphs[b as usize].gid) < 0 {
            return None;
        }
    }
    let mut l = j;
    for cov in lookahead {
        l = next_unskipped(g, flag, glyphs, l + 1);
        if l < 0 || coverage(d, *cov, glyphs[l as usize].gid) < 0 {
            return None;
        }
    }
    Some(positions)
}

fn apply_nested(
    d: &[u8],
    table: i64,
    g: &Gdef,
    glyphs: &mut Vec<GlyphInfo>,
    positions: &[i64],
    records: i64,
    count: i64,
    depth: i64,
    pass: &Pass,
) {
    let mut k = 0;
    while k < count {
        let seq = be_u16(d, records + k * 4);
        let index = be_u16(d, records + k * 4 + 2);
        k += 1;
        if seq >= positions.len() as i64 {
            continue;
        }
        let at = positions[seq as usize];
        if at >= glyphs.len() as i64 {
            continue;
        }
        let nested = read_lookup(
            d,
            table,
            index,
            if *pass == Pass::Substitute { 7 } else { 9 },
        );
        for st in nested.subtables {
            let used = match pass {
                Pass::Substitute => gsub_subtable(
                    d,
                    table,
                    g,
                    nested.kind,
                    nested.flag,
                    st,
                    glyphs,
                    at,
                    depth + 1,
                ),
                Pass::Position { rtl } => gpos_subtable(
                    d,
                    table,
                    g,
                    nested.kind,
                    nested.flag,
                    st,
                    glyphs,
                    at,
                    depth + 1,
                    *rtl,
                ),
            };
            if used > 0 {
                break;
            }
        }
    }
}

// ---- GPOS ----

fn apply_gpos_lookup(
    d: &[u8],
    gpos: i64,
    g: &Gdef,
    index: i64,
    glyphs: &mut Vec<GlyphInfo>,
    rtl: bool,
) {
    let lookup = read_lookup(d, gpos, index, 9);
    let mut i = 0;
    while i < glyphs.len() {
        if i % 256 == 0 && !crate::text::layout::work_active() {
            break;
        }
        if skipped(g, lookup.flag, glyphs[i].gid) {
            i += 1;
            continue;
        }
        for st in &lookup.subtables {
            if gpos_subtable(
                d,
                gpos,
                g,
                lookup.kind,
                lookup.flag,
                *st,
                glyphs,
                i as i64,
                0,
                rtl,
            ) > 0
            {
                break;
            }
        }
        i += 1;
    }
    // Right-to-left cursive chains hang from their last glyph: resolve each
    // child after the parent that follows it.
    for k in (0..glyphs.len()).rev() {
        let parent = glyphs[k].cursive_parent;
        if parent >= 0 {
            glyphs[k].y_offset += glyphs[parent as usize].y_offset;
            glyphs[k].cursive_parent = -1;
        }
    }
}

/// A value record's (x placement, y placement, x advance, y advance).
fn value_record(d: &[u8], at: i64, value_format: i64) -> (i64, i64, i64, i64) {
    let mut p = at;
    let mut out = [0, 0, 0, 0];
    let mut bit = 0;
    while bit < 4 {
        if value_format & (1 << bit) != 0 {
            out[bit] = be_i16(d, p);
            p += 2;
        }
        bit += 1;
    }
    (out[0], out[1], out[2], out[3])
}

fn value_size(value_format: i64) -> i64 {
    let mut n = 0;
    let mut bit = 0;
    while bit < 8 {
        if value_format & (1 << bit) != 0 {
            n += 2
        };
        bit += 1;
    }
    n
}

fn add_value(glyph: &mut GlyphInfo, v: (i64, i64, i64, i64)) {
    glyph.x_offset += v.0;
    glyph.y_offset += v.1;
    glyph.x_advance += v.2;
    glyph.y_advance += v.3;
}

fn anchor(d: &[u8], at: i64) -> (i64, i64) {
    (be_i16(d, at + 2), be_i16(d, at + 4))
}

fn gpos_subtable(
    d: &[u8],
    gpos: i64,
    g: &Gdef,
    kind: i64,
    flag: i64,
    st: i64,
    glyphs: &mut Vec<GlyphInfo>,
    i: i64,
    depth: i64,
    rtl: bool,
) -> i64 {
    let gid = glyphs[i as usize].gid;
    match kind {
        1 => {
            let cov = coverage(d, st + be_u16(d, st + 2), gid);
            if cov < 0 {
                return 0;
            };
            let fmt = be_u16(d, st);
            let vf = be_u16(d, st + 4);
            let at = if fmt == 1 {
                st + 6
            } else {
                st + 8 + cov * value_size(vf)
            };
            add_value(&mut glyphs[i as usize], value_record(d, at, vf));
            1
        }
        2 => {
            let cov = coverage(d, st + be_u16(d, st + 2), gid);
            if cov < 0 {
                return 0;
            };
            let j = next_unskipped(g, flag, &*glyphs, i + 1);
            if j < 0 {
                return 0;
            };
            let second = glyphs[j as usize].gid;
            let fmt = be_u16(d, st);
            let vf1 = be_u16(d, st + 4);
            let vf2 = be_u16(d, st + 6);
            let s1 = value_size(vf1);
            let s2 = value_size(vf2);
            if fmt == 1 {
                let set = st + be_u16(d, st + 10 + cov * 2);
                let count = be_u16(d, set);
                let rec = 2 + s1 + s2;
                let mut lo = 0;
                let mut hi = count - 1;
                while lo <= hi {
                    let mid = (lo + hi) / 2;
                    let r = set + 2 + mid * rec;
                    let gl = be_u16(d, r);
                    if gl < second {
                        lo = mid + 1;
                    } else if gl > second {
                        hi = mid - 1;
                    } else {
                        add_value(&mut glyphs[i as usize], value_record(d, r + 2, vf1));
                        add_value(&mut glyphs[j as usize], value_record(d, r + 2 + s1, vf2));
                        return 1;
                    }
                }
                return 0;
            }
            let c1 = class_of(d, st + be_u16(d, st + 8), gid);
            let c2 = class_of(d, st + be_u16(d, st + 10), second);
            let n1 = be_u16(d, st + 12);
            let n2 = be_u16(d, st + 14);
            if c1 >= n1 || c2 >= n2 {
                return 0;
            };
            let r = st + 16 + (c1 * n2 + c2) * (s1 + s2);
            add_value(&mut glyphs[i as usize], value_record(d, r, vf1));
            add_value(&mut glyphs[j as usize], value_record(d, r + s1, vf2));
            1
        }
        3 => {
            let cov = coverage(d, st + be_u16(d, st + 2), gid);
            if cov < 0 {
                return 0;
            };
            let j = next_unskipped(g, flag, &*glyphs, i + 1);
            if j < 0 {
                return 0;
            };
            let cov2 = coverage(d, st + be_u16(d, st + 2), glyphs[j as usize].gid);
            if cov2 < 0 {
                return 0;
            };
            let exit_off = be_u16(d, st + 6 + cov * 4 + 2);
            let entry_off = be_u16(d, st + 6 + cov2 * 4);
            if exit_off == 0 || entry_off == 0 {
                return 0;
            };
            let (ex, ey) = anchor(d, st + exit_off);
            let (nx, ny) = anchor(d, st + entry_off);
            // Glyph i's exit meets glyph j's entry: along the run, the pen
            // moves from one anchor to the other; across it, the child glyph
            // aligns to its parent, which stays put.
            if rtl {
                let shift = ex + glyphs[i as usize].x_offset;
                glyphs[i as usize].x_advance -= shift;
                glyphs[i as usize].x_offset -= shift;
                glyphs[j as usize].x_advance = nx + glyphs[j as usize].x_offset;
            } else {
                glyphs[i as usize].x_advance = ex + glyphs[i as usize].x_offset;
                let shift = nx + glyphs[j as usize].x_offset;
                glyphs[j as usize].x_advance -= shift;
                glyphs[j as usize].x_offset -= shift;
            }
            if flag & RIGHT_TO_LEFT != 0 {
                glyphs[i as usize].y_offset = ny - ey;
                glyphs[i as usize].cursive_parent = j;
            } else {
                glyphs[j as usize].y_offset = ey - ny + glyphs[i as usize].y_offset;
            }
            1
        }
        4..=6 => {
            let mark_cov = coverage(d, st + be_u16(d, st + 2), gid);
            if mark_cov < 0 {
                return 0;
            };
            // The base: the previous glyph that is not a mark (for mark-to-
            // mark, the previous mark).
            let mut b = i - 1;
            while b >= 0 {
                let class = glyph_class(g, glyphs[b as usize].gid);
                if kind == 6 {
                    if class == MARK {
                        break;
                    };
                    return 0;
                }
                if class != MARK {
                    break;
                };
                b -= 1;
            }
            if b < 0 {
                return 0;
            };
            let base_cov = coverage(d, st + be_u16(d, st + 4), glyphs[b as usize].gid);
            if base_cov < 0 {
                return 0;
            };
            let classes = be_u16(d, st + 6);
            let marks = st + be_u16(d, st + 8);
            let bases = st + be_u16(d, st + 10);
            let mark_class = be_u16(d, marks + 2 + mark_cov * 4);
            let mark_anchor = marks + be_u16(d, marks + 2 + mark_cov * 4 + 2);
            let base_anchor_off = if kind == 5 {
                // Ligature attach: use the last component's anchor.
                let lig = bases + be_u16(d, bases + 2 + base_cov * 2);
                let comps = be_u16(d, lig);
                if comps == 0 {
                    return 0;
                };
                let rec = lig + 2 + (comps - 1) * classes * 2 + mark_class * 2;
                let off = be_u16(d, rec);
                if off == 0 { 0 } else { lig + off }
            } else {
                let rec = bases + 2 + base_cov * classes * 2 + mark_class * 2;
                let off = be_u16(d, rec);
                if off == 0 { 0 } else { bases + off }
            };
            if base_anchor_off == 0 {
                return 0;
            };
            let (mx, my) = anchor(d, mark_anchor);
            let (bx, by) = anchor(d, base_anchor_off);
            // The pen distance from the base's origin to the mark's. Glyphs
            // are in logical order: left to right the base's own advance lies
            // between them, right to left only what follows the base does,
            // since the mark's advance is zeroed below.
            let mut between = 0;
            let mut k = if rtl { b + 1 } else { b };
            while k < i {
                between += glyphs[k as usize].x_advance;
                k += 1;
            }
            glyphs[i as usize].x_offset =
                bx - mx + glyphs[b as usize].x_offset + if rtl { between } else { -between };
            glyphs[i as usize].y_offset = by - my + glyphs[b as usize].y_offset;
            glyphs[i as usize].x_advance = 0;
            1
        }
        7 | 8 => context_subtable(
            d,
            gpos,
            g,
            kind,
            flag,
            st,
            glyphs,
            i,
            depth,
            &Pass::Position { rtl },
        ),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{Gdef, GlyphInfo, USE_MARK_FILTERING_SET, apply_gpos_lookup, read_lookup, skipped};

    // Big-endian 16-bit words.
    fn words(values: &[i64]) -> Vec<u8> {
        let mut out = vec![];
        for v in values {
            let w = v & 0xffff;
            out.push((w >> 8) as u8);
            out.push((w & 0xff) as u8)
        }
        out
    }

    fn glyph(gid: i64, advance: i64) -> GlyphInfo {
        let mut g = GlyphInfo::new(gid, 0, "");
        g.x_advance = advance;
        g
    }

    // A GPOS table whose lookup list starts at byte 10: one cursive lookup
    // with `flag`. Glyph 1 exits at (500, 100); glyph 2 enters at (50, 20).
    fn cursive(flag: i64) -> Vec<u8> {
        words(&vec![
            1, 0, 0, 0, 10, 1, 4, 3, flag, 1, 8, 1, 14, 2, 22, 28, 34, 40, 1, 2, 1, 2, 1, 0, 0, 1,
            500, 100, 1, 50, 20, 1, 600, 0,
        ])
    }

    #[test]
    fn cursive_attachment_joins_exit_to_entry_in_either_direction() {
        let mut ltr = vec![glyph(1, 700), glyph(2, 800)];
        apply_gpos_lookup(&cursive(0), 0, &Gdef::default(), 0, &mut ltr, false);
        assert_eq!(
            (
                ltr[0].x_advance,
                ltr[1].x_advance,
                ltr[1].x_offset,
                ltr[1].y_offset
            ),
            (500, 750, -50, 80)
        );
        let mut rtl = vec![glyph(1, 700), glyph(2, 800)];
        apply_gpos_lookup(&cursive(0), 0, &Gdef::default(), 0, &mut rtl, true);
        assert_eq!(
            (rtl[0].x_advance, rtl[0].x_offset, rtl[1].x_advance),
            (200, -500, 50)
        );
        // With the RightToLeft flag the later glyph stays on the baseline.
        let mut flagged = vec![glyph(1, 700), glyph(2, 800)];
        apply_gpos_lookup(&cursive(1), 0, &Gdef::default(), 0, &mut flagged, true);
        assert_eq!(
            (
                flagged[0].y_offset,
                flagged[1].y_offset,
                flagged[0].cursive_parent
            ),
            (-80, 0, -1)
        )
    }

    #[test]
    fn chained_context_positioning_runs_its_nested_lookup() {
        // Lookup 0 (chained context, format 3): glyph 5 followed by glyph 6
        // runs lookup 1 (single adjustment) on the 5, shortening it by 100.
        let d = words(&vec![
            1, 0, 0, 0, 10, 2, 6, 44, 8, 0, 1, 8, 3, 0, 1, 18, 1, 24, 1, 0, 1, 1, 1, 5, 1, 1, 6, 1,
            0, 1, 8, 1, 8, 4, -100, 1, 1, 5,
        ]);
        let mut before_six = vec![glyph(5, 600), glyph(6, 600)];
        apply_gpos_lookup(&d, 0, &Gdef::default(), 0, &mut before_six, false);
        assert_eq!(before_six[0].x_advance, 500);
        let mut before_seven = vec![glyph(5, 600), glyph(7, 600)];
        apply_gpos_lookup(&d, 0, &Gdef::default(), 0, &mut before_seven, false);
        assert_eq!(before_seven[0].x_advance, 600)
    }

    #[test]
    fn every_subtable_of_an_extension_lookup_is_resolved() {
        // One extension lookup (type 9) holding two single adjustments: glyph
        // 5 by -100 through the first subtable, glyph 6 by -50 through the
        // second.
        let d = words(&vec![
            1, 0, 0, 0, 10, 1, 4, 9, 0, 2, 10, 18, 1, 1, 0, 16, 1, 1, 0, 24, 1, 8, 4, -100, 1, 1,
            5, 0, 1, 8, 4, -50, 1, 1, 6,
        ]);
        let mut glyphs = vec![glyph(5, 600), glyph(6, 600)];
        apply_gpos_lookup(&d, 0, &Gdef::default(), 0, &mut glyphs, false);
        assert_eq!((glyphs[0].x_advance, glyphs[1].x_advance), (500, 550))
    }

    #[test]
    fn a_mark_filtering_set_skips_marks_outside_it() {
        // Class definition at 0 (glyphs 10-12 are marks); mark glyph sets at
        // 10 with one set covering glyph 11.
        let data = words(&[2, 1, 10, 12, 3, 1, 1, 0, 8, 1, 1, 11]);
        let gdef = Gdef {
            classes: 0,
            attach: -1,
            mark_sets: 10,
            data: &data,
        };
        let flag = USE_MARK_FILTERING_SET;
        assert!(skipped(&gdef, flag, 10));
        assert!(!skipped(&gdef, flag, 11));
        assert!(!skipped(&gdef, flag, 5));
        assert!(!skipped(&gdef, 0, 10));
        // A lookup with the flag carries its set index above the flag bits.
        let table = words(&[1, 0, 0, 0, 10, 1, 4, 6, USE_MARK_FILTERING_SET, 1, 10, 2]);
        assert_eq!(
            read_lookup(&table, 0, 0, 9).flag,
            USE_MARK_FILTERING_SET | (2 << 16)
        )
    }
}
