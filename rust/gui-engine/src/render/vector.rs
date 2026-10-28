// vector: vector paths and their exact-area anti-aliased rasterisation.
//
// A path is a flat command stream (the encoding `text::font` emits for
// glyph outlines). `Mask::fill` flattens curves, accumulates each edge's
// signed area per pixel, and integrates the accumulator row by row, so
// coverage is the exact area of each pixel the shape covers.

use crate::text::font::{CLOSE, CUBIC, LINE, MOVE, QUAD};

/// How overlapping sub-paths combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

/// A vector path in pixel coordinates (y down) built with the usual
/// pen commands.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Path {
    pub cmds: Vec<f64>,
}

impl Path {
    pub fn new() -> Path {
        Path { cmds: vec![] }
    }

    pub fn move_to(&mut self, x: f64, y: f64) {
        self.cmds.push(MOVE);
        self.cmds.push(x);
        self.cmds.push(y)
    }

    pub fn line_to(&mut self, x: f64, y: f64) {
        self.cmds.push(LINE);
        self.cmds.push(x);
        self.cmds.push(y)
    }

    pub fn quad_to(&mut self, cx: f64, cy: f64, x: f64, y: f64) {
        self.cmds.push(QUAD);
        self.cmds.push(cx);
        self.cmds.push(cy);
        self.cmds.push(x);
        self.cmds.push(y)
    }

    pub fn cubic_to(&mut self, c1x: f64, c1y: f64, c2x: f64, c2y: f64, x: f64, y: f64) {
        self.cmds.push(CUBIC);
        self.cmds.push(c1x);
        self.cmds.push(c1y);
        self.cmds.push(c2x);
        self.cmds.push(c2y);
        self.cmds.push(x);
        self.cmds.push(y)
    }

    pub fn close(&mut self) {
        self.cmds.push(CLOSE)
    }

    /// This path with every point scaled by `s` and moved by (`ox`, `oy`).
    pub fn scaled(&self, s: f64, ox: f64, oy: f64) -> Path {
        let mut out = Path::new();
        let mut i = 0;
        while i < self.cmds.len() {
            let op = self.cmds[i];
            out.cmds.push(op);
            let points = match op {
                MOVE | LINE => 1,
                QUAD => 2,
                CUBIC => 3,
                _ => 0,
            };
            let mut k = 0;
            while k < points {
                out.cmds.push(ox + self.cmds[i + 1 + k * 2] * s);
                out.cmds.push(oy + self.cmds[i + 2 + k * 2] * s);
                k += 1;
            }
            i += 1 + points * 2;
        }
        out
    }

    /// A rectangle with corner radius `r` (clamped to half the shorter side).
    pub fn rounded_rect(x: f64, y: f64, w: f64, h: f64, r: f64) -> Path {
        let mut p = Path::new();
        let rr = r.min(w / 2.0).min(h / 2.0).max(0.0);
        // Cubic quarter circles: k is the standard 4/3 * (sqrt 2 - 1).
        let k = 0.5522847498 * rr;
        p.move_to(x + rr, y);
        p.line_to(x + w - rr, y);
        p.cubic_to(x + w - rr + k, y, x + w, y + rr - k, x + w, y + rr);
        p.line_to(x + w, y + h - rr);
        p.cubic_to(
            x + w,
            y + h - rr + k,
            x + w - rr + k,
            y + h,
            x + w - rr,
            y + h,
        );
        p.line_to(x + rr, y + h);
        p.cubic_to(x + rr - k, y + h, x, y + h - rr + k, x, y + h - rr);
        p.line_to(x, y + rr);
        p.cubic_to(x, y + rr - k, x + rr - k, y, x + rr, y);
        p.close();
        p
    }

    /// An ellipse inscribed in the given box.
    pub fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64) -> Path {
        let mut p = Path::new();
        let kx = 0.5522847498 * rx;
        let ky = 0.5522847498 * ry;
        p.move_to(cx + rx, cy);
        p.cubic_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
        p.cubic_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
        p.cubic_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
        p.cubic_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
        p.close();
        p
    }

    /// The path's outline widened into a fillable band `width` pixels
    /// across, with butt caps and mitred joins kept simple: each
    /// flattened segment becomes its own quad, which the non-zero rule
    /// unions.
    pub fn stroke(&self, width: f64) -> Path {
        let lines = flatten_with(&self.cmds, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, false);
        let mut out = Path::new();
        let hw = width / 2.0;
        let mut i = 0;
        while i + 3 < lines.len() {
            let x0 = lines[i];
            let y0 = lines[i + 1];
            let x1 = lines[i + 2];
            let y1 = lines[i + 3];
            i += 4;
            let dx = x1 - x0;
            let dy = y1 - y0;
            let len = (dx * dx + dy * dy).sqrt();
            if len <= 0.0 {
                continue;
            }
            // Extend each segment by half the width so joins overlap.
            let ux = dx / len;
            let uy = dy / len;
            let nx = -uy * hw;
            let ny = ux * hw;
            let ax = x0 - ux * hw * 0.5;
            let ay = y0 - uy * hw * 0.5;
            let bx = x1 + ux * hw * 0.5;
            let by = y1 + uy * hw * 0.5;
            out.move_to(ax + nx, ay + ny);
            out.line_to(bx + nx, by + ny);
            out.line_to(bx - nx, by - ny);
            out.line_to(ax - nx, ay - ny);
            out.close()
        }
        out
    }
}

/// A coverage mask: one 0..=255 alpha per pixel, `width` wide, placed
/// with its top-left at (`left`, `top`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mask {
    pub left: i64,
    pub top: i64,
    pub width: i64,
    pub height: i64,
    pub coverage: Vec<u8>,
}

impl Mask {
    /// Rasterises `cmds` mapped through the affine transform
    /// `(x, y) -> (a x + c y + e, b x + d y + f)`.
    pub fn fill(
        cmds: &[f64],
        a: f64,
        b: f64,
        c: f64,
        d: f64,
        e: f64,
        f: f64,
        rule: FillRule,
    ) -> Mask {
        Self::fill_within(cmds, (a, b, c, d, e, f), rule, None)
    }

    /// Rasterises `cmds` as [`Mask::fill`] does, keeping only the part
    /// inside `within` (a destination's pixels): a shape larger than its
    /// destination costs only the destination's area.
    pub fn fill_within(
        cmds: &[f64],
        (a, b, c, d, e, f): (f64, f64, f64, f64, f64, f64),
        rule: FillRule,
        within: Option<crate::geometry::Rect>,
    ) -> Mask {
        let lines = flatten(cmds, a, b, c, d, e, f);
        if lines.is_empty() {
            return Mask::default();
        }
        let mut minx = 1e18;
        let mut miny = 1e18;
        let mut maxx = -1e18;
        let mut maxy = -1e18;
        let mut i = 0;
        while i < lines.len() {
            let x = lines[i];
            let y = lines[i + 1];
            if x < minx {
                minx = x
            };
            if x > maxx {
                maxx = x
            };
            if y < miny {
                miny = y
            };
            if y > maxy {
                maxy = y
            };
            i += 2;
        }
        if let Some(r) = within {
            minx = minx.max(r.x as f64);
            miny = miny.max(r.y as f64);
            maxx = maxx.min((r.x + r.width) as f64);
            maxy = maxy.min((r.y + r.height) as f64);
            if minx >= maxx || miny >= maxy {
                return Mask::default();
            }
        }
        let left = minx.floor() as i64;
        let top = miny.floor() as i64;
        let Some(width) = (maxx.ceil() as i64)
            .checked_sub(left)
            .and_then(|width| width.checked_add(2))
        else {
            return Mask::default();
        };
        let Some(height) = (maxy.ceil() as i64)
            .checked_sub(top)
            .and_then(|height| height.checked_add(1))
        else {
            return Mask::default();
        };
        if width <= 0
            || height <= 0
            || width
                .checked_mul(height)
                .is_none_or(|pixels| pixels > MASK_PIXELS)
        {
            return Mask::default();
        }
        let mut acc = vec![0.0; (width * height + 1) as usize];
        let ox = left as f64;
        let oy = top as f64;
        // The region's right edge; pixels past it are never read.
        let right = (width - 1) as f64;
        i = 0;
        while i + 3 < lines.len() {
            let (ax, ay, bx, by) = (
                lines[i] - ox,
                lines[i + 1] - oy,
                lines[i + 2] - ox,
                lines[i + 3] - oy,
            );
            for (ax, ay, bx, by) in split_at_columns(ax, ay, bx, by, right) {
                accumulate(&mut acc, width, height, ax, ay, bx, by);
            }
            i += 4;
        }
        let mut coverage = vec![0u8; (width * height) as usize];
        let mut sum = 0.0;
        let n = width * height;
        let mut k = 0;
        while k < n {
            sum += acc[k as usize];
            let v = match rule {
                FillRule::NonZero => sum.abs().min(1.0),
                FillRule::EvenOdd => {
                    let m = sum.abs() % 2.0;
                    if m > 1.0 { 2.0 - m } else { m }
                }
            };
            coverage[k as usize] = (v * 255.0 + 0.5) as u8;
            k += 1;
        }
        Mask {
            left,
            top,
            width,
            height,
            coverage,
        }
    }
}

/// The most pixels one mask covers: a full canvas.
const MASK_PIXELS: i64 = 16 * 1024 * 1024;

/// An edge cut to the columns `0..=right`. The part left of the region
/// still winds every pixel to its right, so it runs down column 0; the part
/// right of the region affects no pixel inside it and is dropped.
fn split_at_columns(ax: f64, ay: f64, bx: f64, by: f64, right: f64) -> Vec<(f64, f64, f64, f64)> {
    if ax >= 0.0 && bx >= 0.0 && ax <= right && bx <= right {
        return vec![(ax, ay, bx, by)];
    }
    let mut cuts = vec![0.0, 1.0];
    for edge in [0.0, right] {
        if (ax - edge) * (bx - edge) < 0.0 {
            cuts.push((edge - ax) / (bx - ax));
        }
    }
    cuts.sort_by(f64::total_cmp);
    let at = |t: f64| (ax + (bx - ax) * t, ay + (by - ay) * t);
    cuts.windows(2)
        .filter_map(|w| {
            let ((x0, y0), (x1, y1)) = (at(w[0]), at(w[1]));
            let mid = 0.5 * (x0 + x1);
            if mid > right {
                None
            } else if mid < 0.0 {
                Some((0.0, y0, 0.0, y1))
            } else {
                Some((x0.clamp(0.0, right), y0, x1.clamp(0.0, right), y1))
            }
        })
        .collect()
}

// Signed-area accumulation of one edge into `acc` (row-major, `w` wide).
fn accumulate(acc: &mut [f64], w: i64, h: i64, ax: f64, ay: f64, bx: f64, by: f64) {
    if ay == by {
        return;
    }
    let mut dir = 1.0;
    let mut x0 = ax;
    let mut y0 = ay;
    let mut x1 = bx;
    let mut y1 = by;
    if ay > by {
        dir = -1.0;
        x0 = bx;
        y0 = by;
        x1 = ax;
        y1 = ay;
    }
    let dxdy = (x1 - x0) / (y1 - y0);
    let mut x = x0;
    if y0 < 0.0 {
        x -= y0 * dxdy;
    }
    let ystart = if y0 < 0.0 { 0 } else { y0 as i64 };
    let yend = (y1.ceil() as i64).min(h);
    let limit = acc.len() as i64;
    let mut y = ystart;
    while y < yend {
        let line = y * w;
        let dy = (y as f64 + 1.0).min(y1) - (y as f64).max(y0);
        let xnext = x + dxdy * dy;
        let d = dy * dir;
        let lo = if x < xnext { x } else { xnext };
        let hi = if x < xnext { xnext } else { x };
        let lo_floor = lo.floor();
        let lo_i = (lo_floor as i64).max(0);
        let hi_ceil = hi.ceil();
        let hi_i = (hi_ceil as i64).max(0);
        if hi_i <= lo_i + 1 {
            let xmf = 0.5 * (x + xnext) - lo_floor;
            add(acc, limit, line + lo_i, d - d * xmf);
            add(acc, limit, line + lo_i + 1, d * xmf)
        } else {
            let s = 1.0 / (hi - lo);
            let lo_frac = lo - lo_floor;
            let a0 = 0.5 * s * (1.0 - lo_frac) * (1.0 - lo_frac);
            let hi_frac = hi - hi_ceil + 1.0;
            let am = 0.5 * s * hi_frac * hi_frac;
            add(acc, limit, line + lo_i, d * a0);
            if hi_i == lo_i + 2 {
                add(acc, limit, line + lo_i + 1, d * (1.0 - a0 - am))
            } else {
                let a1 = s * (1.5 - lo_frac);
                add(acc, limit, line + lo_i + 1, d * (a1 - a0));
                let mut xi = lo_i + 2;
                while xi < hi_i - 1 {
                    add(acc, limit, line + xi, d * s);
                    xi += 1;
                }
                let a2 = a1 + (hi_i - lo_i - 3) as f64 * s;
                add(acc, limit, line + hi_i - 1, d * (1.0 - a2 - am))
            }
            add(acc, limit, line + hi_i, d * am)
        }
        x = xnext;
        y += 1;
    }
}

fn add(acc: &mut [f64], limit: i64, at: i64, v: f64) {
    if at >= 0 && at < limit {
        acc[at as usize] += v;
    }
}

/// The path as straight segments `[x0, y0, x1, y1, ..]` after the
/// affine transform, curves subdivided to within a twentieth of a pixel.
pub fn flatten(cmds: &[f64], a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Vec<f64> {
    flatten_with(cmds, a, b, c, d, e, f, true)
}

/// Like `flatten`, but sub-paths without a `CLOSE` stay open when
/// `close_open` is false (strokes of polylines).
pub fn flatten_with(
    cmds: &[f64],
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
    close_open: bool,
) -> Vec<f64> {
    let mut out = vec![];
    let mut i = 0;
    let n = cmds.len();
    if n > 1_000_000 || [a, b, c, d, e, f].iter().any(|value| !value.is_finite()) {
        return Vec::new();
    }
    let mut sx = 0.0;
    let mut sy = 0.0;
    let mut px = 0.0;
    let mut py = 0.0;
    let mut open = false;
    while i < n {
        if out.len() >= 2_000_000 {
            return Vec::new();
        }
        let op = cmds[i];
        let needed = match op {
            MOVE | LINE => 3,
            QUAD => 5,
            CUBIC => 7,
            _ => 1,
        };
        if i + needed > n {
            return Vec::new();
        }
        match op {
            MOVE => {
                if close_open && open && (px != sx || py != sy) {
                    push_line(&mut out, px, py, sx, sy)
                }
                let x = cmds[i + 1];
                let y = cmds[i + 2];
                sx = a * x + c * y + e;
                sy = b * x + d * y + f;
                px = sx;
                py = sy;
                open = true;
                i += 3;
            }
            LINE => {
                let x = cmds[i + 1];
                let y = cmds[i + 2];
                let nx = a * x + c * y + e;
                let ny = b * x + d * y + f;
                push_line(&mut out, px, py, nx, ny);
                px = nx;
                py = ny;
                i += 3;
            }
            QUAD => {
                let cx0 = a * cmds[i + 1] + c * cmds[i + 2] + e;
                let cy0 = b * cmds[i + 1] + d * cmds[i + 2] + f;
                let x = a * cmds[i + 3] + c * cmds[i + 4] + e;
                let y = b * cmds[i + 3] + d * cmds[i + 4] + f;
                let ddx = px - 2.0 * cx0 + x;
                let ddy = py - 2.0 * cy0 + y;
                let dev = (ddx * ddx + ddy * ddy).sqrt();
                let steps = (1.0 + ((dev * 5.0).sqrt()).floor()) as i64;
                let mut k = 1;
                let mut lx = px;
                let mut ly = py;
                if !(0..=65_536).contains(&steps)
                    || out.len().saturating_add(steps as usize * 4) > 2_000_000
                {
                    return Vec::new();
                }
                while k <= steps {
                    let t = k as f64 / steps as f64;
                    let mt = 1.0 - t;
                    let qx = mt * mt * px + 2.0 * mt * t * cx0 + t * t * x;
                    let qy = mt * mt * py + 2.0 * mt * t * cy0 + t * t * y;
                    push_line(&mut out, lx, ly, qx, qy);
                    lx = qx;
                    ly = qy;
                    k += 1;
                }
                px = x;
                py = y;
                i += 5;
            }
            CUBIC => {
                let c1x = a * cmds[i + 1] + c * cmds[i + 2] + e;
                let c1y = b * cmds[i + 1] + d * cmds[i + 2] + f;
                let c2x = a * cmds[i + 3] + c * cmds[i + 4] + e;
                let c2y = b * cmds[i + 3] + d * cmds[i + 4] + f;
                let x = a * cmds[i + 5] + c * cmds[i + 6] + e;
                let y = b * cmds[i + 5] + d * cmds[i + 6] + f;
                let d1x = px - 2.0 * c1x + c2x;
                let d1y = py - 2.0 * c1y + c2y;
                let d2x = c1x - 2.0 * c2x + x;
                let d2y = c1y - 2.0 * c2y + y;
                let dev = (d1x * d1x + d1y * d1y)
                    .sqrt()
                    .max((d2x * d2x + d2y * d2y).sqrt());
                let steps = (1.0 + ((dev * 15.0).sqrt()).floor()) as i64;
                let mut k = 1;
                let mut lx = px;
                let mut ly = py;
                if !(0..=65_536).contains(&steps)
                    || out.len().saturating_add(steps as usize * 4) > 2_000_000
                {
                    return Vec::new();
                }
                while k <= steps {
                    let t = k as f64 / steps as f64;
                    let mt = 1.0 - t;
                    let qx = mt * mt * mt * px
                        + 3.0 * mt * mt * t * c1x
                        + 3.0 * mt * t * t * c2x
                        + t * t * t * x;
                    let qy = mt * mt * mt * py
                        + 3.0 * mt * mt * t * c1y
                        + 3.0 * mt * t * t * c2y
                        + t * t * t * y;
                    push_line(&mut out, lx, ly, qx, qy);
                    lx = qx;
                    ly = qy;
                    k += 1;
                }
                px = x;
                py = y;
                i += 7;
            }
            _ => {
                if open && (px != sx || py != sy) {
                    push_line(&mut out, px, py, sx, sy)
                }
                px = sx;
                py = sy;
                open = false;
                i += 1;
            }
        }
    }
    if close_open && open && (px != sx || py != sy) {
        push_line(&mut out, px, py, sx, sy)
    }
    out
}

fn push_line(out: &mut Vec<f64>, x0: f64, y0: f64, x1: f64, y1: f64) {
    out.push(x0);
    out.push(y0);
    out.push(x1);
    out.push(y1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_larger_than_their_destination_fill_the_part_inside_it() {
        // A square far larger than the destination, partly to its left.
        let mut path = Path::new();
        path.move_to(-5000.0, -100.0);
        path.line_to(3000.0, -100.0);
        path.line_to(3000.0, 4000.0);
        path.line_to(-5000.0, 4000.0);
        path.close();
        let unclipped = Mask::fill(&path.cmds, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, FillRule::NonZero);
        assert_eq!(
            unclipped.width, 0,
            "an unbounded fill beyond the mask limit draws nothing"
        );
        let within = crate::geometry::Rect::new(0, 0, 2048, 2048);
        let m = Mask::fill_within(
            &path.cmds,
            (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            FillRule::NonZero,
            Some(within),
        );
        assert!(m.width >= 2048 && m.height >= 2048);
        let at = |x: i64, y: i64| m.coverage[((y - m.top) * m.width + (x - m.left)) as usize];
        assert_eq!((at(0, 0), at(1024, 1024), at(2047, 2047)), (255, 255, 255));
        // A triangle crossing the left edge keeps its slanted side inside.
        let mut tri = Path::new();
        tri.move_to(-100.0, 0.0);
        tri.line_to(100.0, 0.0);
        tri.line_to(-100.0, 200.0);
        tri.close();
        let m = Mask::fill_within(
            &tri.cmds,
            (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            FillRule::NonZero,
            Some(crate::geometry::Rect::new(0, 0, 300, 300)),
        );
        let at = |x: i64, y: i64| {
            let (cx, cy) = (x - m.left, y - m.top);
            if cx < 0 || cy < 0 || cx >= m.width || cy >= m.height {
                return 0;
            }
            m.coverage[(cy * m.width + cx) as usize]
        };
        assert_eq!(at(10, 10), 255);
        assert_eq!(at(150, 10), 0);
        assert_eq!(at(10, 150), 0, "beyond the slanted side");
    }

    use super::{Mask, Path};

    #[test]
    fn a_pixel_aligned_square_covers_exactly_its_pixels() {
        let mut p = Path::new();
        p.move_to(1.0, 1.0);
        p.line_to(3.0, 1.0);
        p.line_to(3.0, 3.0);
        p.line_to(1.0, 3.0);
        p.close();
        let m = Mask::fill(
            &p.cmds,
            1.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            super::FillRule::NonZero,
        );
        assert_eq!(m.left, 1);
        assert_eq!(m.top, 1);
        let full = m.coverage.iter().filter(|v| **v == 255).count();
        assert_eq!(full, 4);
        let partial = m.coverage.iter().filter(|&&v| v > 0 && v < 255).count();
        assert_eq!(partial, 0)
    }

    #[test]
    fn a_half_pixel_edge_covers_half() {
        let mut p = Path::new();
        p.move_to(0.0, 0.0);
        p.line_to(1.5, 0.0);
        p.line_to(1.5, 1.0);
        p.line_to(0.0, 1.0);
        p.close();
        let m = Mask::fill(
            &p.cmds,
            1.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            super::FillRule::NonZero,
        );
        assert_eq!(m.coverage[0], 255);
        assert!(m.coverage[1] >= 126 && m.coverage[1] <= 129)
    }

    #[test]
    fn even_odd_punches_holes_non_zero_does_not() {
        let mut p = Path::rounded_rect(0.0, 0.0, 10.0, 10.0, 0.0);
        let inner = Path::rounded_rect(3.0, 3.0, 4.0, 4.0, 0.0);
        for v in inner.cmds {
            p.cmds.push(v)
        }
        let nz = Mask::fill(
            &p.cmds,
            1.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            super::FillRule::NonZero,
        );
        let eo = Mask::fill(
            &p.cmds,
            1.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            super::FillRule::EvenOdd,
        );
        let centre = 5 * nz.width + 5;
        assert_eq!(nz.coverage[centre as usize], 255);
        assert_eq!(eo.coverage[centre as usize], 0)
    }

    #[test]
    fn circle_area_matches_pi_r_squared() {
        let p = Path::ellipse(20.0, 20.0, 10.0, 10.0);
        let m = Mask::fill(
            &p.cmds,
            1.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            super::FillRule::NonZero,
        );
        let mut total = 0.0;
        for v in m.coverage {
            total += v as f64 / 255.0
        }
        assert!((total - 314.159).abs() < 3.0)
    }
}
