//! Chart geometry: data ranges, scales, ticks, decimation, drawing
//! operations, legend placement and hit testing.

use std::collections::VecDeque;

use super::{Hit, Kind, Model, Scale, Window};
use crate::geometry::Rect;
use crate::render::display::DrawOp;
use crate::render::vector::Path;
use crate::style::Theme;
use crate::style::color::Color;
use crate::text::layout::{self, TextStyle};

/// A colorblind-safe palette (Okabe and Ito), cycled by series position.
const PALETTE: [(u8, u8, u8); 7] = [
    (0, 114, 178),
    (230, 159, 0),
    (0, 158, 115),
    (204, 121, 167),
    (86, 180, 233),
    (213, 94, 0),
    (120, 120, 120),
];

/// Pixels within which a pointer picks a point.
pub const PICK_RADIUS: f64 = 12.0;

pub fn series_color(model: &Model, i: usize) -> Color {
    model.series[i].style.color.unwrap_or_else(|| {
        let (r, g, b) = PALETTE[i % PALETTE.len()];
        Color::rgb(r, g, b)
    })
}

fn usable(scale: &Scale, v: f64) -> bool {
    !matches!(scale, Scale::Log) || v > 0.0
}

/// The visible ranges: the chart's window, or the visible data padded.
pub fn ranges(model: &Model) -> Window {
    if let Some(window) = model.window {
        return window;
    }
    let (mut x0, mut x1, mut y0, mut y1) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    let mut bars = false;
    for s in model.series.iter().filter(|s| !s.hidden) {
        bars |= s.style.kind == Kind::Bar || s.style.kind == Kind::Area;
        for (x, y) in &s.points {
            if usable(&model.spec.x.scale, *x) {
                x0 = x0.min(*x);
                x1 = x1.max(*x);
            }
            if usable(&model.spec.y.scale, *y) {
                y0 = y0.min(*y);
                y1 = y1.max(*y);
            }
        }
    }
    if !x0.is_finite() {
        (x0, x1) = (0.0, 1.0);
    }
    if !y0.is_finite() {
        (y0, y1) = (0.0, 1.0);
    }
    if bars && !matches!(model.spec.y.scale, Scale::Log) {
        y0 = y0.min(0.0);
        y1 = y1.max(0.0);
    }
    let pad = |a: f64, b: f64, log: bool, categories: bool| {
        if categories {
            return (a - 0.5, b + 0.5);
        }
        if log {
            let (la, lb) = (a.log10(), b.log10());
            let p = ((lb - la) * 0.05).max(0.05);
            return (10f64.powf(la - p), 10f64.powf(lb + p));
        }
        if a == b {
            let d = if a == 0.0 { 1.0 } else { a.abs() * 0.1 };
            return (a - d, b + d);
        }
        let p = (b - a) * 0.05;
        (a - p, b + p)
    };
    let categories = matches!(model.spec.x.scale, Scale::Category(_));
    let (x0, x1) = pad(x0, x1, matches!(model.spec.x.scale, Scale::Log), categories);
    let (y0, y1) = pad(y0, y1, matches!(model.spec.y.scale, Scale::Log), false);
    Window { x0, x1, y0, y1 }
}

fn forward(scale: &Scale, v: f64) -> f64 {
    match scale {
        Scale::Log => v.max(f64::MIN_POSITIVE).log10(),
        _ => v,
    }
}

fn inverse(scale: &Scale, v: f64) -> f64 {
    match scale {
        Scale::Log => 10f64.powf(v),
        _ => v,
    }
}

/// Maps data to pixels inside a plot rectangle and back.
#[derive(Clone, Copy)]
pub struct Mapping<'a> {
    pub plot: Rect,
    pub window: Window,
    x: &'a Scale,
    y: &'a Scale,
}

impl<'a> Mapping<'a> {
    pub fn new(model: &'a Model, plot: Rect) -> Self {
        Mapping {
            plot,
            window: ranges(model),
            x: &model.spec.x.scale,
            y: &model.spec.y.scale,
        }
    }

    pub fn px(&self, x: f64) -> f64 {
        let (a, b) = (
            forward(self.x, self.window.x0),
            forward(self.x, self.window.x1),
        );
        self.plot.x as f64 + (forward(self.x, x) - a) / (b - a) * self.plot.width as f64
    }

    pub fn py(&self, y: f64) -> f64 {
        let (a, b) = (
            forward(self.y, self.window.y0),
            forward(self.y, self.window.y1),
        );
        (self.plot.y + self.plot.height) as f64
            - (forward(self.y, y) - a) / (b - a) * self.plot.height as f64
    }

    pub fn data_x(&self, px: f64) -> f64 {
        let (a, b) = (
            forward(self.x, self.window.x0),
            forward(self.x, self.window.x1),
        );
        inverse(
            self.x,
            a + (px - self.plot.x as f64) / self.plot.width.max(1) as f64 * (b - a),
        )
    }

    pub fn data_y(&self, py: f64) -> f64 {
        let (a, b) = (
            forward(self.y, self.window.y0),
            forward(self.y, self.window.y1),
        );
        let bottom = (self.plot.y + self.plot.height) as f64;
        inverse(
            self.y,
            a + (bottom - py) / self.plot.height.max(1) as f64 * (b - a),
        )
    }
}

/// Readable tick positions across `a..=b` for a scale, at most about
/// `count` of them.
pub fn ticks(scale: &Scale, a: f64, b: f64, count: usize) -> Vec<f64> {
    let count = count.max(2);
    match scale {
        Scale::Log => {
            let (la, lb) = (
                a.max(f64::MIN_POSITIVE).log10().floor() as i32,
                b.max(f64::MIN_POSITIVE).log10().ceil() as i32,
            );
            let step = ((lb - la) as usize / count + 1) as i32;
            (la..=lb)
                .step_by(step as usize)
                .map(|e| 10f64.powi(e))
                .filter(|v| *v >= a && *v <= b)
                .collect()
        }
        Scale::Category(names) => {
            let (first, last) = (
                a.ceil().max(0.0) as i64,
                b.floor().min(names.len() as f64 - 1.0) as i64,
            );
            if last < first {
                return Vec::new();
            }
            let step = ((last - first) as usize / count + 1) as i64;
            (first..=last)
                .step_by(step as usize)
                .map(|i| i as f64)
                .collect()
        }
        Scale::Time => {
            const UNITS: [f64; 14] = [
                1.0,
                5.0,
                15.0,
                60.0,
                300.0,
                900.0,
                3600.0,
                3.0 * 3600.0,
                6.0 * 3600.0,
                86_400.0,
                7.0 * 86_400.0,
                30.0 * 86_400.0,
                91.0 * 86_400.0,
                365.25 * 86_400.0,
            ];
            let raw = (b - a) / count as f64;
            let step = UNITS
                .iter()
                .copied()
                .find(|u| *u >= raw)
                .unwrap_or_else(|| nice(raw));
            stepped(a, b, step)
        }
        Scale::Linear => stepped(a, b, nice((b - a) / count as f64)),
    }
}

/// 1, 2 or 5 times a power of ten, at least `raw`.
fn nice(raw: f64) -> f64 {
    if !(raw.is_finite() && raw > 0.0) {
        return 1.0;
    }
    let base = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|m| m * base)
        .find(|v| *v >= raw)
        .unwrap_or(10.0 * base)
}

fn stepped(a: f64, b: f64, step: f64) -> Vec<f64> {
    let first = (a / step).ceil();
    (0..1000)
        .map(|i| (first + i as f64) * step)
        .take_while(|v| *v <= b + step * 1e-9)
        .map(|v| if v.abs() < step * 1e-9 { 0.0 } else { v })
        .collect()
}

/// A value as its axis labels it.
pub fn format_value(scale: &Scale, v: f64) -> String {
    match scale {
        Scale::Category(names) => names.get(v.round() as usize).cloned().unwrap_or_default(),
        Scale::Time => jiff::Timestamp::from_second(v.round() as i64)
            .map(|t| t.strftime("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default(),
        _ => {
            if v != 0.0 && (v.abs() >= 1e7 || v.abs() < 1e-4) {
                format!("{v:.3e}")
            } else {
                let text = format!("{v:.4}");
                let text = text.trim_end_matches('0').trim_end_matches('.');
                if text == "-0" {
                    "0".into()
                } else {
                    text.to_string()
                }
            }
        }
    }
}

/// Where the parts of a chart of `width` x `height` go.
pub struct Frame {
    pub plot: Rect,
    /// Each visible legend entry's rectangle, by series.
    pub legend: Vec<(usize, Rect)>,
}

fn small(theme: &Theme) -> TextStyle {
    theme.font(crate::style::TextRole::Small)
}

pub fn frame(model: &Model, width: i64, height: i64, theme: &Theme) -> Frame {
    let style = small(theme);
    let (_, _, line) = layout::metrics(&style);
    let line = line.ceil() as i64;
    let pad = theme.px(8);
    let top = pad
        + if model.spec.title.is_empty() {
            0
        } else {
            line + pad
        };
    let left_labels = [ranges(model).y0, ranges(model).y1]
        .iter()
        .map(|v| {
            layout::measure(&format_value(&model.spec.y.scale, *v), &style)
                .0
                .ceil() as i64
        })
        .max()
        .unwrap_or(0);
    let left = pad
        + left_labels
        + pad
        + if model.spec.y.label.is_empty() {
            0
        } else {
            line + pad
        };
    let bottom = pad
        + line
        + pad
        + if model.spec.x.label.is_empty() {
            0
        } else {
            line + pad
        };
    let mut legend = Vec::new();
    let mut right = pad;
    if model.spec.legend && !model.series.is_empty() {
        let swatch = line;
        let widest = model
            .series
            .iter()
            .map(|s| layout::measure(&s.style.name, &style).0.ceil() as i64)
            .max()
            .unwrap_or(0);
        let entry_w = swatch + pad / 2 + widest;
        let x = width - pad - entry_w;
        for (i, _) in model.series.iter().enumerate() {
            legend.push((
                i,
                Rect::new(x, top + i as i64 * (line + pad / 2), entry_w, line),
            ));
        }
        right += entry_w + pad;
    }
    let plot = Rect::new(
        left,
        top,
        (width - left - right).max(1),
        (height - top - bottom).max(1),
    );
    Frame { plot, legend }
}

/// The visible points of a series, thinned to at most four per pixel column
/// (first, lowest, highest, last), which keeps the drawn shape exact.
pub fn decimate(points: &VecDeque<(f64, f64)>, map: &Mapping, sorted: bool) -> Vec<(f64, f64)> {
    let columns = map.plot.width.max(1) as usize;
    if !sorted || points.len() <= columns * 4 {
        return points.iter().copied().collect();
    }
    let (from, to) = (map.window.x0, map.window.x1);
    let start = partition(points, |p| p.0 < from).saturating_sub(1);
    let end = (partition(points, |p| p.0 <= to) + 1).min(points.len());
    if end - start <= columns * 4 {
        return points.range(start..end).copied().collect();
    }
    let mut out = Vec::with_capacity(columns * 4);
    let flush = |b: [(f64, f64); 4], out: &mut Vec<(f64, f64)>| {
        let mut kept = b.to_vec();
        kept.sort_by(|a, b| a.0.total_cmp(&b.0));
        kept.dedup();
        out.extend(kept);
    };
    let mut bucket: Option<(i64, [(f64, f64); 4])> = None;
    for p in points.range(start..end) {
        let column = map.px(p.0).floor() as i64;
        match &mut bucket {
            Some((c, b)) if *c == column => {
                if p.1 < b[1].1 {
                    b[1] = *p;
                }
                if p.1 > b[2].1 {
                    b[2] = *p;
                }
                b[3] = *p;
            }
            _ => {
                if let Some((_, b)) = bucket.take() {
                    flush(b, &mut out);
                }
                bucket = Some((column, [*p; 4]));
            }
        }
    }
    if let Some((_, b)) = bucket {
        flush(b, &mut out);
    }
    out
}

fn label(
    ops: &mut Vec<DrawOp>,
    text: &str,
    x: f64,
    y: f64,
    align: f64,
    style: &TextStyle,
    color: Color,
) {
    let (w, _) = layout::measure(text, style);
    ops.push(DrawOp::Text {
        x: (x - w * align).round() as i64,
        y: y.round() as i64,
        text: text.to_string(),
        style: style.clone(),
        color,
    });
}

/// The chart drawn at `width` x `height` in local pixels.
pub fn render(model: &Model, width: i64, height: i64, theme: &Theme) -> Vec<DrawOp> {
    let mut ops = Vec::new();
    let style = small(theme);
    let (_, _, line) = layout::metrics(&style);
    let frame = frame(model, width, height, theme);
    let plot = frame.plot;
    let map = Mapping::new(model, plot);
    let pad = theme.px(8) as f64;
    let ink = theme.text;
    let grid = theme.border;
    if !model.spec.title.is_empty() {
        let mut title = style.clone();
        title.bold = true;
        label(
            &mut ops,
            &model.spec.title,
            width as f64 / 2.0,
            pad,
            0.5,
            &title,
            ink,
        );
    }
    let w = map.window;
    for t in ticks(
        &model.spec.x.scale,
        w.x0,
        w.x1,
        (plot.width / theme.px(90)).max(2) as usize,
    ) {
        let x = map.px(t);
        ops.push(DrawOp::Line {
            x0: x.round() as i64,
            y0: plot.y,
            x1: x.round() as i64,
            y1: plot.y + plot.height,
            width: 1.0,
            color: grid,
        });
        label(
            &mut ops,
            &format_value(&model.spec.x.scale, t),
            x,
            (plot.y + plot.height) as f64 + pad / 2.0,
            0.5,
            &style,
            ink,
        );
    }
    for t in ticks(
        &model.spec.y.scale,
        w.y0,
        w.y1,
        (plot.height / theme.px(40)).max(2) as usize,
    ) {
        let y = map.py(t);
        ops.push(DrawOp::Line {
            x0: plot.x,
            y0: y.round() as i64,
            x1: plot.x + plot.width,
            y1: y.round() as i64,
            width: 1.0,
            color: grid,
        });
        label(
            &mut ops,
            &format_value(&model.spec.y.scale, t),
            plot.x as f64 - pad / 2.0,
            y - line / 2.0,
            1.0,
            &style,
            ink,
        );
    }
    if !model.spec.x.label.is_empty() {
        label(
            &mut ops,
            &model.spec.x.label,
            (plot.x + plot.width / 2) as f64,
            (plot.y + plot.height) as f64 + line + pad,
            0.5,
            &style,
            ink,
        );
    }
    if !model.spec.y.label.is_empty() {
        label(
            &mut ops,
            &model.spec.y.label,
            pad,
            plot.y as f64 - line - pad / 2.0,
            0.0,
            &style,
            ink,
        );
    }
    ops.push(DrawOp::Stroke {
        rect: plot,
        radius: 0,
        width: 1,
        color: theme.border_strong,
    });
    ops.push(DrawOp::Clip(plot));
    let bars: Vec<usize> = (0..model.series.len())
        .filter(|i| model.series[*i].style.kind == Kind::Bar && !model.series[*i].hidden)
        .collect();
    for (i, s) in model.series.iter().enumerate() {
        if s.hidden {
            continue;
        }
        let color = series_color(model, i);
        let points = &s.points;
        let usable_points =
            |p: &(f64, f64)| usable(&model.spec.x.scale, p.0) && usable(&model.spec.y.scale, p.1);
        match s.style.kind {
            Kind::Line | Kind::Area => {
                let thinned: Vec<(f64, f64)> = decimate(points, &map, s.sorted)
                    .into_iter()
                    .filter(usable_points)
                    .collect();
                if thinned.is_empty() {
                    continue;
                }
                let mut path = Path::new();
                for (k, p) in thinned.iter().enumerate() {
                    if k == 0 {
                        path.move_to(map.px(p.0), map.py(p.1));
                    } else {
                        path.line_to(map.px(p.0), map.py(p.1));
                    }
                }
                if s.style.kind == Kind::Area {
                    let base = map.py(if matches!(model.spec.y.scale, Scale::Log) {
                        w.y0
                    } else {
                        0.0f64.clamp(w.y0, w.y1)
                    });
                    let mut fill = path.clone();
                    fill.line_to(map.px(thinned[thinned.len() - 1].0), base);
                    fill.line_to(map.px(thinned[0].0), base);
                    fill.close();
                    ops.push(DrawOp::Fill {
                        path: fill,
                        color: color.with_alpha(70),
                    });
                }
                ops.push(DrawOp::StrokePath {
                    path,
                    width: theme.px(2) as f64,
                    color,
                });
            }
            Kind::Bar => {
                let slot = bars.iter().position(|b| *b == i).unwrap_or(0) as f64;
                let spacing = points
                    .iter()
                    .zip(points.iter().skip(1))
                    .map(|(a, b)| (b.0 - a.0).abs())
                    .filter(|d| *d > 0.0)
                    .fold(f64::INFINITY, f64::min);
                let spacing = if spacing.is_finite() { spacing } else { 1.0 };
                let band = (map.px(w.x0 + spacing) - map.px(w.x0)).abs() * 0.8;
                let each = band / bars.len().max(1) as f64;
                let base = map.py(0.0f64.clamp(w.y0, w.y1));
                for p in points
                    .iter()
                    .filter(|p| p.0 >= w.x0 - spacing && p.0 <= w.x1 + spacing)
                {
                    let x = map.px(p.0) - band / 2.0 + slot * each;
                    let y = map.py(p.1);
                    let (top, h) = if y < base {
                        (y, base - y)
                    } else {
                        (base, y - base)
                    };
                    ops.push(DrawOp::FillRect {
                        rect: Rect::new(
                            x.round() as i64,
                            top.round() as i64,
                            (each - 1.0).max(1.0).round() as i64,
                            h.round().max(1.0) as i64,
                        ),
                        color,
                    });
                }
            }
            Kind::Scatter => {
                let radius = theme.px(3);
                let mut taken = std::collections::HashSet::new();
                for p in points.iter().filter(|p| {
                    usable_points(p) && p.0 >= w.x0 && p.0 <= w.x1 && p.1 >= w.y0 && p.1 <= w.y1
                }) {
                    let (x, y) = (map.px(p.0).round() as i64, map.py(p.1).round() as i64);
                    // One mark per pixel: overplotted points draw once.
                    if taken.insert((x, y)) {
                        ops.push(DrawOp::Circle {
                            cx: x,
                            cy: y,
                            r: radius,
                            color,
                        });
                    }
                }
            }
        }
    }
    if let Some((series, index)) = model.cursor
        && let Some(p) = model.series.get(series).and_then(|s| s.points.get(index))
    {
        let (x, y) = (map.px(p.0).round() as i64, map.py(p.1).round() as i64);
        let r = theme.px(6);
        ops.push(DrawOp::Stroke {
            rect: Rect::new(x - r, y - r, 2 * r, 2 * r),
            radius: r,
            width: theme.px(2),
            color: theme.focus,
        });
    }
    ops.push(DrawOp::Unclip);
    if let Some(hit) = &model.hover {
        let text = format!(
            "{}: {}, {}",
            hit.name,
            format_value(&model.spec.x.scale, hit.x),
            format_value(&model.spec.y.scale, hit.y)
        );
        let (tw, _) = layout::measure(&text, &style);
        let (x, y) = (map.px(hit.x), map.py(hit.y));
        let bx = (x + pad).min((plot.x + plot.width) as f64 - tw - pad);
        let by = (y - line - pad * 1.5).max(plot.y as f64);
        ops.push(DrawOp::Circle {
            cx: x.round() as i64,
            cy: y.round() as i64,
            r: theme.px(4),
            color: theme.text,
        });
        ops.push(DrawOp::RoundRect {
            rect: Rect::new(
                bx.round() as i64,
                by.round() as i64,
                (tw + pad).ceil() as i64,
                (line + pad / 2.0).ceil() as i64,
            ),
            radius: theme.px(4),
            color: theme.surface,
        });
        ops.push(DrawOp::Stroke {
            rect: Rect::new(
                bx.round() as i64,
                by.round() as i64,
                (tw + pad).ceil() as i64,
                (line + pad / 2.0).ceil() as i64,
            ),
            radius: theme.px(4),
            width: 1,
            color: theme.border_strong,
        });
        label(
            &mut ops,
            &text,
            bx + pad / 2.0,
            by + pad / 4.0,
            0.0,
            &style,
            theme.text,
        );
    }
    for (i, rect) in &frame.legend {
        let s = &model.series[*i];
        let color = if s.hidden {
            theme.text_disabled
        } else {
            series_color(model, *i)
        };
        ops.push(DrawOp::FillRect {
            rect: Rect::new(
                rect.x,
                rect.y + rect.height / 4,
                rect.height,
                rect.height / 2,
            ),
            color,
        });
        label(
            &mut ops,
            &s.style.name,
            (rect.x + rect.height) as f64 + pad / 2.0,
            rect.y as f64,
            0.0,
            &style,
            if s.hidden { theme.text_disabled } else { ink },
        );
    }
    ops
}

/// The visible point nearest a local pixel, within [`PICK_RADIUS`].
pub fn hit(model: &Model, width: i64, height: i64, theme: &Theme, x: f64, y: f64) -> Option<Hit> {
    let frame = frame(model, width, height, theme);
    let map = Mapping::new(model, frame.plot);
    let radius = theme.px(PICK_RADIUS as i64) as f64;
    let mut best: Option<(f64, Hit)> = None;
    for (i, s) in model.series.iter().enumerate().filter(|(_, s)| !s.hidden) {
        let all = s.points.iter();
        let near: Box<dyn Iterator<Item = (usize, &(f64, f64))>> = if s.sorted {
            let (lo, hi) = (
                map.data_x(x - radius).min(map.data_x(x + radius)),
                map.data_x(x - radius).max(map.data_x(x + radius)),
            );
            let start = partition(&s.points, |p| p.0 < lo);
            let end = partition(&s.points, |p| p.0 <= hi);
            Box::new((start..end).map(move |k| (k, &s.points[k])))
        } else {
            Box::new(all.enumerate())
        };
        for (index, p) in near {
            let d = (map.px(p.0) - x).hypot(map.py(p.1) - y);
            if d <= radius && best.as_ref().is_none_or(|(b, _)| d < *b) {
                best = Some((
                    d,
                    Hit {
                        series: i as i64,
                        name: s.style.name.clone(),
                        index: index as i64,
                        x: p.0,
                        y: p.1,
                    },
                ));
            }
        }
    }
    best.map(|(_, hit)| hit)
}

fn partition(points: &VecDeque<(f64, f64)>, pred: impl Fn(&(f64, f64)) -> bool) -> usize {
    let (mut lo, mut hi) = (0, points.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if pred(&points[mid]) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The series whose legend entry is under a local pixel.
pub fn legend_hit(
    model: &Model,
    width: i64,
    height: i64,
    theme: &Theme,
    x: i64,
    y: i64,
) -> Option<usize> {
    frame(model, width, height, theme)
        .legend
        .into_iter()
        .find(|(_, r)| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
        .map(|(i, _)| i)
}
