// variation: OpenType font variations for TrueType outlines. `fvar` names
// the axes and instances; `avar` remaps normalized coordinates; `gvar` moves
// glyph points (and composite offsets) by deltas scaled for the coordinates,
// inferring deltas for points a variation leaves out; `HVAR` (or the phantom
// points when it is absent) adjusts advances.

use crate::text::font::{Face, be_i16, be_u8, be_u16, be_u32, tag};
use std::collections::HashMap;

/// One variation axis, in user units.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Axis {
    pub tag: String,
    pub min: f64,
    pub default: f64,
    pub max: f64,
}

/// A named instance: the `name` table id of its subfamily and its user
/// coordinates, one per axis.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Instance {
    pub name_id: i64,
    pub coords: Vec<f64>,
}

fn fixed(d: &[u8], at: i64) -> f64 {
    be_u32(d, at) as f64 / 65536.0
        - (if be_u16(d, at) >= 0x8000 {
            65536.0
        } else {
            0.0
        })
}

fn f2dot14(d: &[u8], at: i64) -> f64 {
    be_i16(d, at) as f64 / 16384.0
}

/// The face's variation axes, in `fvar` order; empty for a static face.
pub fn axes(face: &Face) -> Vec<Axis> {
    let Some((fvar, _)) = face.table("fvar") else {
        return vec![];
    };
    let d = face.bytes();
    let first = fvar + be_u16(d, fvar + 4);
    let count = be_u16(d, fvar + 8);
    let size = be_u16(d, fvar + 10);
    (0..count)
        .map(|i| {
            let at = first + i * size;
            Axis {
                tag: tag(d, at),
                min: fixed(d, at + 4),
                default: fixed(d, at + 8),
                max: fixed(d, at + 12),
            }
        })
        .collect()
}

/// The face's named instances, in `fvar` order.
pub fn instances(face: &Face) -> Vec<Instance> {
    let Some((fvar, _)) = face.table("fvar") else {
        return vec![];
    };
    let d = face.bytes();
    let axis_count = be_u16(d, fvar + 8);
    let first = fvar + be_u16(d, fvar + 4) + axis_count * be_u16(d, fvar + 10);
    let count = be_u16(d, fvar + 12);
    let size = be_u16(d, fvar + 14);
    (0..count)
        .map(|i| {
            let at = first + i * size;
            Instance {
                name_id: be_u16(d, at),
                coords: (0..axis_count).map(|k| fixed(d, at + 4 + k * 4)).collect(),
            }
        })
        .collect()
}

/// Normalized coordinates (-1..1, through `avar`) for user values by axis
/// tag; axes without a value sit at their default.
pub fn normalize(face: &Face, values: &HashMap<String, f64>) -> Vec<f64> {
    let all = axes(face);
    let mut out = vec![];
    for a in all {
        let v = values
            .get(&a.tag)
            .unwrap_or(&a.default)
            .max(a.min)
            .min(a.max);
        let n = if v < a.default && a.default > a.min {
            (v - a.default) / (a.default - a.min)
        } else if v > a.default && a.max > a.default {
            (v - a.default) / (a.max - a.default)
        } else {
            0.0
        };
        out.push(n)
    }
    remap(face, &out)
}

// Applies the `avar` segment maps, axis by axis.
fn remap(face: &Face, coords: &[f64]) -> Vec<f64> {
    let Some((avar, _)) = face.table("avar") else {
        return coords.to_vec();
    };
    let d = face.bytes();
    let count = be_u16(d, avar + 6);
    let mut out = coords.to_vec();
    let mut at = avar + 8;
    for i in 0..count {
        let pairs = be_u16(d, at);
        let maps = at + 2;
        if i < out.len() as i64 && pairs > 0 {
            let v = out[i as usize];
            let mut k = 0;
            while k + 1 < pairs && f2dot14(d, maps + (k + 1) * 4) < v {
                k += 1
            }
            let from0 = f2dot14(d, maps + k * 4);
            let to0 = f2dot14(d, maps + k * 4 + 2);
            if k + 1 < pairs && v > from0 {
                let from1 = f2dot14(d, maps + (k + 1) * 4);
                let to1 = f2dot14(d, maps + (k + 1) * 4 + 2);
                out[i as usize] = if from1 > from0 {
                    to0 + (v - from0) * (to1 - to0) / (from1 - from0)
                } else {
                    to0
                };
            } else {
                out[i as usize] = to0 + (v - from0);
            }
        }
        at += 2 + pairs * 4;
    }
    out.to_vec()
}

// The weight of one tuple at `coords`: each axis with a non-zero peak scales
// it by how far the coordinate has moved toward the peak.
fn tuple_scalar(coords: &[f64], peak: &[f64], start: &[f64], end: &[f64]) -> f64 {
    let mut scalar = 1.0;
    for i in 0..peak.len() {
        let p = peak[i];
        if p == 0.0 {
            continue;
        };
        let v = if i < coords.len() { coords[i] } else { 0.0 };
        if v == p {
            continue;
        };
        if start.is_empty() {
            if v == 0.0 || v < p.min(0.0) || v > p.max(0.0) {
                return 0.0;
            };
            scalar *= v / p;
        } else {
            if v <= start[i] || v >= end[i] {
                return 0.0;
            };
            scalar *= if v < p {
                (v - start[i]) / (p - start[i])
            } else {
                (end[i] - v) / (end[i] - p)
            };
        }
    }
    scalar
}

// Packed point numbers at `at`: the points (empty for all) and where the
// data after them starts.
fn packed_points(d: &[u8], at: i64) -> (Vec<i64>, i64, bool) {
    let mut p = at;
    let first = be_u8(d, p);
    p += 1;
    if first == 0 {
        return (vec![], p, true);
    };
    let count = if first & 0x80 != 0 {
        let c = ((first & 0x7F) << 8) | be_u8(d, p);
        p += 1;
        c
    } else {
        first
    };
    let mut out = vec![];
    let mut last = 0;
    while out.len() < count as usize {
        let control = be_u8(d, p);
        p += 1;
        let run = (control & 0x7F) + 1;
        for _ in 0..run {
            if out.len() >= count as usize {
                break;
            };
            let step = if control & 0x80 != 0 {
                let v = be_u16(d, p);
                p += 2;
                v
            } else {
                let v = be_u8(d, p);
                p += 1;
                v
            };
            last += step;
            out.push(last)
        }
    }
    (out, p, false)
}

// `count` packed deltas at `at`, and where the data after them starts.
fn packed_deltas(d: &[u8], at: i64, count: i64) -> (Vec<f64>, i64) {
    let mut p = at;
    let mut out = vec![];
    while out.len() < count as usize {
        let control = be_u8(d, p);
        p += 1;
        let run = (control & 0x3F) + 1;
        for _ in 0..run {
            if out.len() >= count as usize {
                break;
            };
            if control & 0x80 != 0 {
                out.push(0.0)
            } else if control & 0x40 != 0 {
                out.push(be_i16(d, p) as f64);
                p += 2;
            } else {
                let v = be_u8(d, p);
                out.push((if v >= 128 { v - 256 } else { v }) as f64);
                p += 1;
            }
        }
    }
    (out, p)
}

/// The (x, y) deltas at `face.coords` for each of glyph `g`'s points: its
/// outline points (`xs`, `ys`, with contours ending at `ends`), then the four
/// phantom points. A composite glyph passes one point per component and no
/// contours.
pub fn glyph_deltas(
    face: &Face,
    g: i64,
    xs: &[f64],
    ys: &[f64],
    ends: &[i64],
) -> (Vec<f64>, Vec<f64>) {
    let n = xs.len() + 4;
    let mut dx = vec![0.0; n];
    let mut dy = vec![0.0; n];
    let Some((gvar, _)) = face.table("gvar") else {
        return (dx, dy);
    };
    if face.coords.iter().all(|c| *c == 0.0) {
        return (dx, dy);
    };
    let d = face.bytes();
    let axis_count = be_u16(d, gvar + 4);
    let shared_count = be_u16(d, gvar + 6);
    let shared = gvar + be_u32(d, gvar + 8);
    let glyph_count = be_u16(d, gvar + 12);
    let long = be_u16(d, gvar + 14) & 1 != 0;
    let array = gvar + be_u32(d, gvar + 16);
    if g < 0 || g >= glyph_count {
        return (dx, dy);
    };
    let offset_at = |i: i64| -> i64 {
        if long {
            be_u32(d, gvar + 20 + i * 4)
        } else {
            be_u16(d, gvar + 20 + i * 2) * 2
        }
    };
    let start = offset_at(g);
    if offset_at(g + 1) <= start {
        return (dx, dy);
    };
    let data = array + start;
    let header = be_u16(d, data);
    let tuples = header & 0x0FFF;
    let mut serialized = data + be_u16(d, data + 2);
    let mut shared_points = vec![];
    let mut shared_all = true;
    if header & 0x8000 != 0 {
        let (points, next, all) = packed_points(d, serialized);
        shared_points = points;
        shared_all = all;
        serialized = next;
    }
    let mut at = data + 4;
    for _ in 0..tuples {
        let size = be_u16(d, at);
        let index = be_u16(d, at + 2);
        at += 4;
        let mut peak = vec![];
        if index & 0x8000 != 0 {
            peak = (0..axis_count).map(|k| f2dot14(d, at + k * 2)).collect();
            at += axis_count * 2;
        } else if (index & 0x0FFF) < shared_count {
            let t = shared + (index & 0x0FFF) * axis_count * 2;
            peak = (0..axis_count).map(|k| f2dot14(d, t + k * 2)).collect();
        }
        let mut lo = vec![];
        let mut hi = vec![];
        if index & 0x4000 != 0 {
            lo = (0..axis_count).map(|k| f2dot14(d, at + k * 2)).collect();
            hi = (0..axis_count)
                .map(|k| f2dot14(d, at + axis_count * 2 + k * 2))
                .collect();
            at += axis_count * 4;
        }
        let scalar = tuple_scalar(&face.coords, &peak, &lo, &hi);
        let body = serialized;
        serialized += size;
        if scalar == 0.0 {
            continue;
        };
        let mut points = shared_points.clone();
        let mut all = shared_all;
        let mut p = body;
        if index & 0x2000 != 0 {
            let (own, next, every) = packed_points(d, p);
            points = own;
            all = every;
            p = next;
        }
        let count = if all { n } else { points.len() };
        let (xd, after) = packed_deltas(d, p, count as i64);
        let (yd, _) = packed_deltas(d, after, count as i64);
        if all {
            for k in 0..n {
                dx[k] += xd[k] * scalar;
                dy[k] += yd[k] * scalar;
            }
            continue;
        }
        let (tx, ty) = inferred(xs, ys, ends, &points, &xd, &yd);
        for k in 0..n {
            dx[k] += tx[k] * scalar;
            dy[k] += ty[k] * scalar;
        }
    }
    (dx, dy)
}

// One tuple's deltas for every point, given deltas for `points` only: within
// each contour, an untouched point takes deltas interpolated (per axis) from
// the touched points around it; phantom points left out move by nothing.
fn inferred(
    xs: &[f64],
    ys: &[f64],
    ends: &[i64],
    points: &[i64],
    xd: &[f64],
    yd: &[f64],
) -> (Vec<f64>, Vec<f64>) {
    let n = xs.len() + 4;
    let mut tx = vec![0.0; n];
    let mut ty = vec![0.0; n];
    let mut touched = vec![false; n];
    for (k, &p) in points.iter().enumerate() {
        let p = p as usize;
        if p < n {
            tx[p] = xd[k];
            ty[p] = yd[k];
            touched[p] = true;
        }
    }
    let mut first = 0;
    for last in ends {
        let marked: Vec<i64> = (first..last + 1).filter(|&p| touched[p as usize]).collect();
        if !marked.is_empty() {
            for p in first..last + 1 {
                if touched[p as usize] {
                    continue;
                };
                // The touched points before and after `p` in contour order.
                let mut before = marked[marked.len() - 1];
                let mut after = marked[0];
                for m in &marked {
                    if *m < p {
                        before = *m
                    }
                }
                for m in marked.iter().rev() {
                    if *m > p {
                        after = *m
                    }
                }
                tx[p as usize] = interpolate(
                    xs[p as usize],
                    xs[before as usize],
                    xs[after as usize],
                    tx[before as usize],
                    tx[after as usize],
                );
                ty[p as usize] = interpolate(
                    ys[p as usize],
                    ys[before as usize],
                    ys[after as usize],
                    ty[before as usize],
                    ty[after as usize],
                );
            }
        }
        first = last + 1;
    }
    (tx, ty)
}

fn interpolate(c: f64, c1: f64, c2: f64, d1: f64, d2: f64) -> f64 {
    if c1 == c2 {
        return if d1 == d2 { d1 } else { 0.0 };
    }
    let (lo, hi, dlo, dhi) = if c1 < c2 {
        (c1, c2, d1, d2)
    } else {
        (c2, c1, d2, d1)
    };
    if c <= lo {
        return dlo;
    };
    if c >= hi {
        return dhi;
    };
    dlo + (c - lo) * (dhi - dlo) / (hi - lo)
}

/// The change in glyph `g`'s advance at `face.coords`, from `HVAR`.
pub fn advance_delta(face: &Face, g: i64) -> f64 {
    let Some((hvar, _)) = face.table("HVAR") else {
        return 0.0;
    };
    if face.coords.iter().all(|c| *c == 0.0) {
        return 0.0;
    };
    let d = face.bytes();
    let store = hvar + be_u32(d, hvar + 4);
    let map = be_u32(d, hvar + 8);
    let mut outer = 0;
    let mut inner = g;
    if map != 0 {
        let m = hvar + map;
        let map_format = be_u8(d, m);
        let entry_format = be_u8(d, m + 1);
        let count = if map_format == 0 {
            be_u16(d, m + 2)
        } else {
            be_u32(d, m + 2)
        };
        let entries = m + if map_format == 0 { 4 } else { 6 };
        let size = ((entry_format & 0x30) >> 4) + 1;
        let inner_bits = (entry_format & 0x0F) + 1;
        let i = g.min(count - 1);
        let mut value = 0;
        for b in 0..size {
            value = value * 256 + be_u8(d, entries + i * size + b)
        }
        outer = value >> inner_bits;
        inner = value & ((1 << inner_bits) - 1);
    }
    store_delta(d, store, outer, inner, &face.coords)
}

// One item's delta from an item variation store at `coords`.
fn store_delta(d: &[u8], store: i64, outer: i64, inner: i64, coords: &[f64]) -> f64 {
    let regions = store + be_u32(d, store + 2);
    let data_count = be_u16(d, store + 6);
    if outer >= data_count {
        return 0.0;
    };
    let data = store + be_u32(d, store + 8 + outer * 4);
    let items = be_u16(d, data);
    if inner >= items {
        return 0.0;
    };
    let words = be_u16(d, data + 2);
    let long = words & 0x8000 != 0;
    let word_count = words & 0x7FFF;
    let region_count = be_u16(d, data + 4);
    let indexes = data + 6;
    let word = if long { 4 } else { 2 };
    let small = if long { 2 } else { 1 };
    let row_size = word_count * word + (region_count - word_count) * small;
    let row = indexes + region_count * 2 + inner * row_size;
    let axis_count = be_u16(d, regions);
    let mut total = 0.0;
    let mut at = row;
    for r in 0..region_count {
        let wide = r < word_count;
        let delta = if wide {
            if long {
                be_u32(d, at)
                    - (if be_u16(d, at) >= 0x8000 {
                        4_294_967_296
                    } else {
                        0
                    })
            } else {
                be_i16(d, at)
            }
        } else if long {
            be_i16(d, at)
        } else {
            let v = be_u8(d, at);
            if v >= 128 { v - 256 } else { v }
        };
        at += if wide { word } else { small };
        let region = regions + 4 + be_u16(d, indexes + r * 2) * axis_count * 6;
        let mut scalar = 1.0;
        for k in 0..axis_count {
            let s = f2dot14(d, region + k * 6);
            let p = f2dot14(d, region + k * 6 + 2);
            let e = f2dot14(d, region + k * 6 + 4);
            let v = if k < coords.len() as i64 {
                coords[k as usize]
            } else {
                0.0
            };
            if s > p || p > e || (s < 0.0 && e > 0.0 && p != 0.0) || p == 0.0 {
                continue;
            };
            if v < s || v > e {
                scalar = 0.0;
                break;
            }
            if v != p {
                scalar *= if v < p {
                    (v - s) / (p - s)
                } else {
                    (e - v) / (e - p)
                };
            }
        }
        total += delta as f64 * scalar;
    }
    total
}
