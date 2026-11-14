// color_glyph: paints a `COLR` glyph into straight-alpha RGBA pixels at one
// size. Clips come from glyph outlines and boxes, fills are solid colors or
// linear, radial, and sweep gradients, and layers composite by the modes the
// table names. Colors come from the font's first palette, with the text's
// color standing in for the palette's foreground entry.

use skrifa::GlyphId;
use skrifa::font::Font as Model;
use skrifa::font::glyph::color::{
    Brush, ColorGlyph as ColrGlyph, ColorStop, CompositeMode, Extend, PaintSink,
};
use skrifa::font::glyph::outline::{DrawMemory, DrawOptions};
use skrifa::raw::types::{BoundingBox, Matrix};

use crate::render::vector::{FillRule, Mask};
use crate::text::font::{CLOSE, ColorGlyph, LINE, MOVE, PathCommands};

/// Largest side of a painted color glyph, in pixels.
const MAX_SIDE: i64 = 1024;

/// The palette index that stands for the text's own color.
const FOREGROUND: u16 = 0xffff;

// `(x, y) -> (a x + c y + e, b x + d y + f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Affine {
    const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    fn from_matrix(m: Matrix<f32>) -> Affine {
        Affine {
            a: f64::from(m.xx),
            b: f64::from(m.yx),
            c: f64::from(m.xy),
            d: f64::from(m.yy),
            e: f64::from(m.dx),
            f: f64::from(m.dy),
        }
    }

    // `self` after `inner`: the point goes through `inner` first.
    fn then_inner(self, inner: Affine) -> Affine {
        Affine {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            e: self.a * inner.e + self.c * inner.f + self.e,
            f: self.b * inner.e + self.d * inner.f + self.f,
        }
    }

    fn inverse(self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 {
            return None;
        }
        let a = self.d / det;
        let b = -self.b / det;
        let c = -self.c / det;
        let d = self.a / det;
        Some(Affine {
            a,
            b,
            c,
            d,
            e: -(a * self.e + c * self.f),
            f: -(b * self.e + d * self.f),
        })
    }

    fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
}

/// Paints `glyph` at `ppem` pixels per em; `None` when it paints nothing or
/// its paint graph is malformed.
pub(crate) fn paint(
    model: &Model,
    glyph: &ColrGlyph,
    ppem: f64,
    foreground: [u8; 4],
) -> Option<ColorGlyph> {
    let units = f64::from(model.units_per_em().max(16));
    let s = ppem / units;
    let bounds = glyph.clip_box().or_else(|| {
        glyph.extents().ok().map(|e| BoundingBox {
            x_min: e.x_bearing,
            y_min: e.y_bearing - e.height,
            x_max: e.x_bearing + e.width,
            y_max: e.y_bearing,
        })
    })?;
    let left = (f64::from(bounds.x_min) * s).floor() as i64;
    let right = (f64::from(bounds.x_max) * s).ceil() as i64;
    let top = (-f64::from(bounds.y_max) * s).floor() as i64;
    let bottom = (-f64::from(bounds.y_min) * s).ceil() as i64;
    let (width, height) = (right - left, bottom - top);
    if width <= 0 || height <= 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    let palette = model
        .palettes()
        .get(0)
        .map(|p| {
            p.colors()
                .iter()
                .map(|c| [c.red, c.green, c.blue, c.alpha].map(|v| f32::from(v) / 255.0))
                .collect()
        })
        .unwrap_or_default();
    let pixels = (width * height) as usize;
    let mut painter = Painter {
        model,
        palette,
        foreground: foreground.map(|v| f32::from(v) / 255.0),
        width: width as usize,
        height: height as usize,
        base: Affine {
            a: s,
            b: 0.0,
            c: 0.0,
            d: -s,
            e: -(left as f64),
            f: -(top as f64),
        },
        transforms: vec![Affine::IDENTITY],
        clips: vec![vec![1.0; pixels]],
        layers: vec![vec![[0.0; 4]; pixels]],
        modes: vec![],
    };
    glyph.paint(&mut painter).ok()?;
    let canvas = painter.layers.into_iter().next()?;
    let rgba = canvas
        .iter()
        .flat_map(|&[r, g, b, a]| {
            if a <= 0.0 {
                [0; 4]
            } else {
                let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                [byte(r / a), byte(g / a), byte(b / a), byte(a)]
            }
        })
        .collect();
    Some(ColorGlyph {
        rgba,
        ppem,
        width,
        height,
        bearing_x: left,
        bearing_y: -top,
    })
}

struct Painter<'a> {
    model: &'a Model,
    /// Straight RGBA, 0 to 1.
    palette: Vec<[f32; 4]>,
    foreground: [f32; 4],
    width: usize,
    height: usize,
    /// Font units to canvas pixels, y down.
    base: Affine,
    /// The transform stack in font units; the last is current.
    transforms: Vec<Affine>,
    /// Coverage per pixel, 0 to 1; the last is current.
    clips: Vec<Vec<f32>>,
    /// Premultiplied RGBA per pixel; the last is drawn into.
    layers: Vec<Vec<[f32; 4]>>,
    /// The composite mode of each pushed layer.
    modes: Vec<CompositeMode>,
}

impl Painter<'_> {
    fn current(&self) -> Affine {
        self.transforms.last().copied().unwrap_or(Affine::IDENTITY)
    }

    // Coverage of `path` (font units under the current transform) over the
    // whole canvas.
    fn coverage(&self, path: &[f64]) -> Vec<f32> {
        let t = self.base.then_inner(self.current());
        let mask = Mask::fill(path, t.a, t.b, t.c, t.d, t.e, t.f, FillRule::NonZero);
        let mut out = vec![0.0; self.width * self.height];
        for row in 0..mask.height {
            let y = mask.top + row;
            if y < 0 || y >= self.height as i64 {
                continue;
            }
            for col in 0..mask.width {
                let x = mask.left + col;
                if x < 0 || x >= self.width as i64 {
                    continue;
                }
                out[y as usize * self.width + x as usize] =
                    f32::from(mask.coverage[(row * mask.width + col) as usize]) / 255.0;
            }
        }
        out
    }

    fn push_clip(&mut self, coverage: Vec<f32>) {
        let previous = self.clips.last();
        let clip = match previous {
            Some(p) => p.iter().zip(coverage).map(|(a, b)| a * b).collect(),
            None => coverage,
        };
        self.clips.push(clip);
    }

    // Premultiplied RGBA of a palette entry scaled by `alpha`.
    fn color(&self, index: u16, alpha: f32) -> [f32; 4] {
        let [r, g, b, a] = if index == FOREGROUND {
            self.foreground
        } else {
            self.palette
                .get(usize::from(index))
                .copied()
                .unwrap_or([0.0, 0.0, 0.0, 1.0])
        };
        let a = a * alpha;
        [r * a, g * a, b * a, a]
    }

    // The premultiplied color at `t` along `stops`, which run 0 to 1 in
    // order. Where several stops share an offset, the last of them holds
    // from that offset on.
    fn stop_color(&self, stops: &[ColorStop], t: f32) -> [f32; 4] {
        let after = stops.partition_point(|s| s.offset <= t);
        let (Some(a), Some(b)) = (after.checked_sub(1).map(|i| stops[i]), stops.get(after)) else {
            let edge = if after == 0 {
                stops.first()
            } else {
                stops.last()
            };
            return edge.map_or([0.0; 4], |s| self.color(s.palette_index, s.alpha));
        };
        let k = (t - a.offset) / (b.offset - a.offset);
        let ca = self.color(a.palette_index, a.alpha);
        let cb = self.color(b.palette_index, b.alpha);
        std::array::from_fn(|i| ca[i] + (cb[i] - ca[i]) * k)
    }

    // Each canvas pixel's position in the current transform's font units.
    fn sampler(&self) -> Option<Affine> {
        self.base.then_inner(self.current()).inverse()
    }

    fn fill_with(&mut self, color_at: impl Fn(f64, f64) -> Option<[f32; 4]>) {
        let Some(to_font) = self.sampler() else {
            return;
        };
        let Some(clip) = self.clips.last() else {
            return;
        };
        let Some(layer) = self.layers.last_mut() else {
            return;
        };
        for y in 0..self.height {
            for x in 0..self.width {
                let i = y * self.width + x;
                let cover = clip[i];
                if cover <= 0.0 {
                    continue;
                }
                let (fx, fy) = to_font.apply(x as f64 + 0.5, y as f64 + 0.5);
                let Some(src) = color_at(fx, fy) else {
                    continue;
                };
                let dst = &mut layer[i];
                let a = src[3] * cover;
                for k in 0..4 {
                    dst[k] = src[k] * cover + dst[k] * (1.0 - a);
                }
            }
        }
    }
}

fn extended(t: f32, extend: Extend) -> f32 {
    match extend {
        Extend::Repeat => t - t.floor(),
        Extend::Reflect => {
            let m = t.rem_euclid(2.0);
            if m > 1.0 { 2.0 - m } else { m }
        }
        _ => t.clamp(0.0, 1.0),
    }
}

impl PaintSink for Painter<'_> {
    fn push_transform(&mut self, transform: Matrix<f32>) {
        let next = self.current().then_inner(Affine::from_matrix(transform));
        self.transforms.push(next);
    }

    fn pop_transform(&mut self) {
        if self.transforms.len() > 1 {
            self.transforms.pop();
        }
    }

    fn push_clip_glyph(&mut self, glyph: GlyphId) {
        let mut path = PathCommands::default();
        if let Some(outline) = self.model.outline_glyphs().get(glyph) {
            let _ = outline.draw(&DrawOptions::new(), DrawMemory::Scratch, &mut path);
        }
        let coverage = self.coverage(&path.0);
        self.push_clip(coverage);
    }

    fn push_clip_box(&mut self, clip_box: BoundingBox<f32>) {
        let (x0, y0) = (f64::from(clip_box.x_min), f64::from(clip_box.y_min));
        let (x1, y1) = (f64::from(clip_box.x_max), f64::from(clip_box.y_max));
        let path = [
            MOVE, x0, y0, LINE, x1, y0, LINE, x1, y1, LINE, x0, y1, CLOSE,
        ];
        let coverage = self.coverage(&path);
        self.push_clip(coverage);
    }

    fn pop_clip(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
        }
    }

    fn fill(&mut self, brush: Brush<'_>) {
        match brush {
            Brush::Solid {
                palette_index,
                alpha,
            } => {
                let color = self.color(palette_index, alpha);
                self.fill_with(|_, _| Some(color));
            }
            Brush::LinearGradient {
                p0,
                p1,
                color_stops,
                extend,
            } => {
                let (dx, dy) = (f64::from(p1.x - p0.x), f64::from(p1.y - p0.y));
                let length = dx * dx + dy * dy;
                if length <= 0.0 {
                    return;
                }
                let colors = |t: f32| self.stop_color(color_stops, extended(t, extend));
                let samples = gradient_table(colors);
                self.fill_with(|x, y| {
                    let t = ((x - f64::from(p0.x)) * dx + (y - f64::from(p0.y)) * dy) / length;
                    Some(lookup(&samples, t as f32, extend))
                });
            }
            Brush::RadialGradient {
                c0,
                r0,
                c1,
                r1,
                color_stops,
                extend,
            } => {
                let colors = |t: f32| self.stop_color(color_stops, extended(t, extend));
                let samples = gradient_table(colors);
                let (cx, cy) = (f64::from(c1.x - c0.x), f64::from(c1.y - c0.y));
                let (r0, dr) = (f64::from(r0), f64::from(r1 - r0));
                let a = cx * cx + cy * cy - dr * dr;
                self.fill_with(|x, y| {
                    let (px, py) = (x - f64::from(c0.x), y - f64::from(c0.y));
                    let b = px * cx + py * cy + r0 * dr;
                    let c = px * px + py * py - r0 * r0;
                    let t = if a.abs() < 1e-9 {
                        if b.abs() < 1e-9 {
                            return None;
                        }
                        c / (2.0 * b)
                    } else {
                        let disc = b * b - a * c;
                        if disc < 0.0 {
                            return None;
                        }
                        let root = disc.sqrt();
                        let (t1, t2) = ((b + root) / a, (b - root) / a);
                        let (hi, lo) = if t1 > t2 { (t1, t2) } else { (t2, t1) };
                        if r0 + hi * dr >= 0.0 {
                            hi
                        } else if r0 + lo * dr >= 0.0 {
                            lo
                        } else {
                            return None;
                        }
                    };
                    if r0 + t * dr < 0.0 {
                        return None;
                    }
                    Some(lookup(&samples, t as f32, extend))
                });
            }
            Brush::SweepGradient {
                c0,
                start_angle,
                end_angle,
                color_stops,
                extend,
            } => {
                let span = end_angle - start_angle;
                if span < 0.0 {
                    return;
                }
                let colors = |t: f32| self.stop_color(color_stops, extended(t, extend));
                let samples = gradient_table(colors);
                self.fill_with(|x, y| {
                    // Angles run clockwise from the positive x axis, y up.
                    let angle = (-(y - f64::from(c0.y)))
                        .atan2(x - f64::from(c0.x))
                        .to_degrees()
                        .rem_euclid(360.0) as f32;
                    // Coincident angles (only padded ones reach here) make a
                    // hard edge: the last color from the angle on, the first
                    // before it.
                    let t = if span == 0.0 {
                        if angle >= start_angle { 1.0 } else { 0.0 }
                    } else {
                        (angle - start_angle) / span
                    };
                    Some(lookup(&samples, t, extend))
                });
            }
        }
    }

    fn push_layer(&mut self, composite_mode: CompositeMode) {
        self.layers.push(vec![[0.0; 4]; self.width * self.height]);
        self.modes.push(composite_mode);
    }

    fn pop_layer_with_mode(&mut self, composite_mode: CompositeMode) {
        if self.layers.len() < 2 {
            return;
        }
        self.modes.pop();
        let Some(source) = self.layers.pop() else {
            return;
        };
        let Some(dest) = self.layers.last_mut() else {
            return;
        };
        for (d, s) in dest.iter_mut().zip(source) {
            *d = composite(s, *d, composite_mode);
        }
    }
}

const GRADIENT_STEPS: usize = 256;

// Colors at evenly spaced positions of the color line, 0 to 1.
fn gradient_table(colors: impl Fn(f32) -> [f32; 4]) -> Vec<[f32; 4]> {
    (0..=GRADIENT_STEPS)
        .map(|i| colors(i as f32 / GRADIENT_STEPS as f32))
        .collect()
}

fn lookup(samples: &[[f32; 4]], t: f32, extend: Extend) -> [f32; 4] {
    let t = extended(t, extend);
    let at = (t * GRADIENT_STEPS as f32).round() as usize;
    samples[at.min(samples.len() - 1)]
}

// `source` over `dest` (both premultiplied) by `mode`, following the W3C
// Compositing and Blending formulas the `COLR` specification adopts.
fn composite(s: [f32; 4], d: [f32; 4], mode: CompositeMode) -> [f32; 4] {
    let (sa, da) = (s[3], d[3]);
    let porter_duff =
        |fa: f32, fb: f32| -> [f32; 4] { std::array::from_fn(|i| s[i] * fa + d[i] * fb) };
    match mode {
        CompositeMode::Clear => [0.0; 4],
        CompositeMode::Src => s,
        CompositeMode::Dest => d,
        CompositeMode::SrcOver => porter_duff(1.0, 1.0 - sa),
        CompositeMode::DestOver => porter_duff(1.0 - da, 1.0),
        CompositeMode::SrcIn => porter_duff(da, 0.0),
        CompositeMode::DestIn => porter_duff(0.0, sa),
        CompositeMode::SrcOut => porter_duff(1.0 - da, 0.0),
        CompositeMode::DestOut => porter_duff(0.0, 1.0 - sa),
        CompositeMode::SrcAtop => porter_duff(da, 1.0 - sa),
        CompositeMode::DestAtop => porter_duff(1.0 - da, sa),
        CompositeMode::Xor => porter_duff(1.0 - da, 1.0 - sa),
        CompositeMode::Plus => std::array::from_fn(|i| (s[i] + d[i]).min(1.0)),
        CompositeMode::Screen
        | CompositeMode::Overlay
        | CompositeMode::Darken
        | CompositeMode::Lighten
        | CompositeMode::ColorDodge
        | CompositeMode::ColorBurn
        | CompositeMode::HardLight
        | CompositeMode::SoftLight
        | CompositeMode::Difference
        | CompositeMode::Exclusion
        | CompositeMode::Multiply
        | CompositeMode::HslHue
        | CompositeMode::HslSaturation
        | CompositeMode::HslColor
        | CompositeMode::HslLuminosity => blend(s, d, mode),
        _ => porter_duff(1.0, 1.0 - sa),
    }
}

// A blend mode: the blended color where both are present, each color alone
// where only it is, all source-over.
fn blend(s: [f32; 4], d: [f32; 4], mode: CompositeMode) -> [f32; 4] {
    let (sa, da) = (s[3], d[3]);
    let straight = |c: [f32; 4]| -> [f32; 3] {
        if c[3] <= 0.0 {
            [0.0; 3]
        } else {
            [c[0] / c[3], c[1] / c[3], c[2] / c[3]]
        }
    };
    let (cs, cb) = (straight(s), straight(d));
    let mixed = blended(cs, cb, mode);
    let alpha = sa + da - sa * da;
    let mut out = [0.0; 4];
    for i in 0..3 {
        out[i] = (1.0 - da) * s[i] + (1.0 - sa) * d[i] + sa * da * mixed[i];
    }
    out[3] = alpha;
    out
}

fn blended(cs: [f32; 3], cb: [f32; 3], mode: CompositeMode) -> [f32; 3] {
    let separable =
        |f: fn(f32, f32) -> f32| -> [f32; 3] { std::array::from_fn(|i| f(cs[i], cb[i])) };
    match mode {
        CompositeMode::Multiply => separable(|s, b| s * b),
        CompositeMode::Screen => separable(|s, b| s + b - s * b),
        CompositeMode::Overlay => separable(|s, b| hard_light(b, s)),
        CompositeMode::Darken => separable(f32::min),
        CompositeMode::Lighten => separable(f32::max),
        CompositeMode::ColorDodge => separable(|s, b| {
            if b <= 0.0 {
                0.0
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        }),
        CompositeMode::ColorBurn => separable(|s, b| {
            if b >= 1.0 {
                1.0
            } else if s <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s).min(1.0)
            }
        }),
        CompositeMode::HardLight => separable(hard_light),
        CompositeMode::SoftLight => separable(|s, b| {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 {
                    ((16.0 * b - 12.0) * b + 4.0) * b
                } else {
                    b.sqrt()
                };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }),
        CompositeMode::Difference => separable(|s, b| (s - b).abs()),
        CompositeMode::Exclusion => separable(|s, b| s + b - 2.0 * s * b),
        CompositeMode::HslHue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        CompositeMode::HslSaturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        CompositeMode::HslColor => set_lum(cs, lum(cb)),
        CompositeMode::HslLuminosity => set_lum(cb, lum(cs)),
        _ => cs,
    }
}

fn hard_light(s: f32, b: f32) -> f32 {
    if s <= 0.5 {
        b * 2.0 * s
    } else {
        let s2 = 2.0 * s - 1.0;
        b + s2 - b * s2
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 {
        out = out.map(|v| l + (v - l) * l / (l - n));
    }
    if x > 1.0 {
        out = out.map(|v| l + (v - l) * (1.0 - l) / (x - l));
    }
    out
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color(c.map(|v| v + d))
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    if max <= min {
        return [0.0; 3];
    }
    c.map(|v| (v - min) * s / (max - min))
}

#[cfg(test)]
mod tests {
    use super::{Affine, CompositeMode, Painter, composite};
    use skrifa::font::Font as Model;
    use skrifa::font::glyph::color::{Brush, ColorStop, Extend, PaintSink};
    use skrifa::raw::types::{BoundingBox, Point};

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

    // A 20 by 20 canvas over font units -10..10 on each axis, y up.
    fn painter(model: &Model) -> Painter<'_> {
        Painter {
            model,
            palette: vec![RED, BLUE],
            foreground: [0.0, 0.0, 0.0, 1.0],
            width: 20,
            height: 20,
            base: Affine {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: -1.0,
                e: 10.0,
                f: 10.0,
            },
            transforms: vec![Affine::IDENTITY],
            clips: vec![vec![1.0; 400]],
            layers: vec![vec![[0.0; 4]; 400]],
            modes: vec![],
        }
    }

    fn model() -> Model {
        Model::new(
            include_bytes!("../../assets/fonts/DejaVuSans.ttf").to_vec(),
            0,
        )
        .unwrap()
    }

    fn stop(offset: f32, palette_index: u16) -> ColorStop {
        ColorStop {
            offset,
            palette_index,
            alpha: 1.0,
        }
    }

    // The pixel at font units (x, y).
    fn at(p: &Painter, x: i64, y: i64) -> [f32; 4] {
        p.layers[0][((10 - y) * 20 + (x + 10)) as usize]
    }

    #[test]
    fn a_padded_sweep_with_coincident_angles_is_a_hard_edge() {
        let model = model();
        let mut p = painter(&model);
        // 270 degrees clockwise from +x, y up, points up: from there round to
        // +x is the last color, the rest the first.
        let stops = [stop(0.0, 0), stop(1.0, 1)];
        p.fill(Brush::SweepGradient {
            c0: Point::new(0.0, 0.0),
            start_angle: 270.0,
            end_angle: 270.0,
            color_stops: &stops,
            extend: Extend::Pad,
        });
        assert_eq!(at(&p, 5, 5), BLUE);
        assert_eq!(at(&p, -5, 5), RED);
        assert_eq!(at(&p, -5, -5), RED);
    }

    #[test]
    fn stops_sharing_an_offset_hand_over_to_the_last_of_them() {
        let model = model();
        let p = painter(&model);
        let stops = [stop(0.0, 0), stop(0.5, 0), stop(0.5, 1), stop(1.0, 1)];
        assert_eq!(p.stop_color(&stops, 0.25), RED);
        assert_eq!(p.stop_color(&stops, 0.5), BLUE);
        assert_eq!(p.stop_color(&stops, 1.0), BLUE);
        assert_eq!(p.stop_color(&[stop(0.0, 1), stop(1.0, 1)], 2.0), BLUE);
    }

    #[test]
    fn a_clip_box_bounds_a_fill_and_layers_composite_by_their_mode() {
        let model = model();
        let mut p = painter(&model);
        p.push_clip_box(BoundingBox {
            x_min: 0.0,
            y_min: 0.0,
            x_max: 10.0,
            y_max: 10.0,
        });
        p.fill(Brush::Solid {
            palette_index: 0,
            alpha: 1.0,
        });
        p.pop_clip();
        assert_eq!(at(&p, 5, 5), RED);
        assert_eq!(at(&p, -5, 5), [0.0; 4]);
        p.push_layer(CompositeMode::DestOut);
        p.fill(Brush::Solid {
            palette_index: 1,
            alpha: 1.0,
        });
        p.pop_layer_with_mode(CompositeMode::DestOut);
        assert_eq!(at(&p, 5, 5), [0.0; 4]);
        assert_eq!(composite(RED, BLUE, CompositeMode::SrcOver), RED);
        assert_eq!(
            composite(RED, BLUE, CompositeMode::Multiply),
            [0.0, 0.0, 0.0, 1.0]
        );
    }
}
