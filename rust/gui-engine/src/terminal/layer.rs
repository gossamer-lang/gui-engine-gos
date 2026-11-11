//! A terminal's text as pixels it keeps between paints. Its rows sit in a
//! ring of equal bands, so output that scrolls turns the ring instead of
//! moving pixels, and only the rows that came in are drawn.

use std::collections::HashMap;

use wezterm_term::color::ColorPalette;

use super::frame::{Frame, RowVersion};
use super::widget::{RowKey, RowPaint, draw_row};
use crate::geometry::Rect;
use crate::memory::ByteCache;
use crate::render::retained::Command;
use crate::render::{Canvas, Surface, Turn};
use crate::style::color::Color;
use crate::text::layout::{TextLayout, TextStyle};
use crate::widgets::cx::PaintCx;

/// What a layer's rows draw with besides their own cells: rows drawn with
/// another context would not draw as this paint does.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct LayerContext {
    pub(super) palette: ColorPalette,
    pub(super) style: TextStyle,
    /// Cell width, line height and text rise, by their bits.
    pub(super) metrics: [u64; 3],
    pub(super) inset: (i64, i64),
    pub(super) rule: (i64, i64),
    pub(super) bold_bright: bool,
    pub(super) legible: bool,
    pub(super) fonts: u64,
    pub(super) size: (i64, i64),
    pub(super) rows: usize,
    pub(super) background: Color,
}

/// What one slot of the ring shows.
#[derive(Debug)]
struct Shown {
    key: RowKey,
    runs: Vec<String>,
}

#[derive(Debug, Default)]
pub(super) struct Layer {
    surface: Surface,
    context: Option<LayerContext>,
    /// The slot screen row 0 sits in.
    base: usize,
    slots: Vec<Option<Shown>>,
    /// A context whose rows draw outside their own bands, which paints
    /// without a layer.
    overflowing: Option<LayerContext>,
    /// Whether a paint drew from the layer since the last sweep.
    used: bool,
    /// When the ring last turned or was drawn anew.
    turned: i64,
}

/// How long a layer outlives the last scroll of its output.
const QUIET_MS: i64 = 1000;

impl Layer {
    /// Whether a paint with `context` can draw from the layer.
    pub(super) fn usable(&self, context: &LayerContext) -> bool {
        self.overflowing.as_ref() != Some(context)
    }

    /// Whether the layer holds pixels.
    pub(super) fn holds(&self) -> bool {
        self.surface.holds()
    }

    /// Whether the output has not scrolled for a while, so the layer's pixels
    /// cost more than they save.
    pub(super) fn quiet(&self, now: i64) -> bool {
        self.holds() && now - self.turned > QUIET_MS
    }

    /// When a paint finds the layer quiet, while it holds pixels.
    pub(super) fn release_due(&self) -> Option<i64> {
        self.holds().then_some(self.turned + QUIET_MS + 1)
    }

    /// Lets the pixels go; the next paint that uses the layer draws it anew.
    pub(super) fn release(&mut self) {
        self.surface.release();
        self.slots.clear();
        self.context = None;
    }

    /// Lets the pixels go when no paint drew from them since the last
    /// sweep; answers whether the layer still holds pixels.
    pub(super) fn sweep(&mut self) -> bool {
        if !std::mem::take(&mut self.used) {
            self.release();
        }
        self.surface.holds()
    }

    // The slot screen row 0 takes in this paint: the turn that keeps the
    // most rows in the slots they already fill, which is how far the output
    // scrolled since the last paint.
    fn turn(&self, keys: &[Option<RowKey>]) -> usize {
        let n = self.slots.len();
        let filled: HashMap<RowVersion, usize> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(s, shown)| shown.as_ref().map(|shown| (shown.key.version, s)))
            .collect();
        let mut turns: HashMap<usize, usize> = HashMap::new();
        for (i, key) in keys.iter().enumerate() {
            if let Some(key) = key
                && let Some(&s) = filled.get(&key.version)
                && self.slots[s]
                    .as_ref()
                    .is_some_and(|shown| shown.key == *key)
            {
                *turns.entry((s + n - i % n) % n).or_default() += 1;
            }
        }
        turns
            .into_iter()
            .max_by_key(|&(base, count)| (count, base == self.base))
            .map_or(self.base, |(base, _)| base)
    }
}

/// Paints the rows of `frame` through `layer` and draws its pixels at the
/// view's origin. Answers false, drawing nothing, when a row draws outside
/// its own band; the context then paints without a layer.
pub(super) fn paint(
    layer: &mut Layer,
    context: &LayerContext,
    p: &RowPaint,
    keys: &[Option<RowKey>],
    frame: &Frame,
    layouts: &mut ByteCache<String, TextLayout>,
    cx: &mut PaintCx,
) -> bool {
    let n = keys.len();
    let line = p.lh as i64;
    let valid = layer.context.as_ref() == Some(context) && layer.surface.holds();
    if !valid {
        layer.slots = (0..n).map(|_| None).collect();
        layer.base = 0;
    }
    let base = layer.turn(keys);
    let slot = |i: usize| (i + base) % n.max(1);
    let mut drawn: Vec<Option<(Vec<Command>, Vec<String>)>> = Vec::with_capacity(n);
    for ((i, cells), key) in frame.cells().iter().enumerate().zip(keys) {
        let kept = key.as_ref().is_some_and(|key| {
            layer.slots[slot(i)]
                .as_ref()
                .is_some_and(|shown| shown.key == *key)
        });
        if kept {
            drawn.push(None);
            continue;
        }
        let mark = cx.canvas.recorded_mark();
        let runs = draw_row(p, layouts, cx, i as i64, cells);
        let commands = cx.canvas.take_recorded(mark);
        let (top, height) = p.band(i as i64);
        let band = (p.origin.y + top, p.origin.y + top + height);
        if height != line
            || commands
                .iter()
                .any(|c| c.bounds.y < band.0 || c.bounds.y + c.bounds.height > band.1)
        {
            layer.overflowing = Some(context.clone());
            layer.context = None;
            return false;
        }
        drawn.push(Some((commands, runs)));
    }
    let unchanged = valid && base == layer.base && drawn.iter().all(Option::is_none);
    if !unchanged {
        let (width, height) = context.size;
        let pad = context.inset.1;
        let turn = Turn {
            top: pad,
            len: n as i64 * line,
            shift: base as i64 * line,
        };
        let origin = p.origin;
        let background = context.background;
        // Unturned, only the bands drawn again differ from what was shown.
        let changed = (valid && base == layer.base).then(|| {
            drawn
                .iter()
                .enumerate()
                .filter(|(_, d)| d.is_some())
                .map(|(i, _)| Rect::new(0, pad + i as i64 * line, width, line))
                .collect()
        });
        // Every band starts from the opaque background the context paints.
        // The surface keeps its pixels in the order window surfaces take, so
        // a band drawn in the canvas's order is put in that order at once,
        // while it is still in the cache, and every frame copies it as is.
        let stored_background = Color {
            r: background.b,
            b: background.r,
            ..background
        };
        layer.surface.rewrite(
            width,
            height,
            true,
            turn,
            changed,
            |canvas: &mut Canvas, kept| {
                if !valid || !kept {
                    canvas.fill(stored_background);
                }
                for (i, drawn) in drawn.iter().enumerate() {
                    let Some((commands, _)) = drawn else {
                        continue;
                    };
                    let shown = pad + i as i64 * line;
                    let stored = pad + slot(i) as i64 * line;
                    canvas.clip = Rect::new(0, stored, width, line);
                    canvas.rect(0, stored, width, line, background);
                    for command in commands {
                        command.replay(canvas, -origin.x, -origin.y + stored - shown);
                    }
                    let rows =
                        (stored * width * 4) as usize..((stored + line) * width * 4) as usize;
                    if let Some(band) = canvas.pixels.get_mut(rows) {
                        crate::render::swap_red_blue_in_place(band);
                    }
                }
                canvas.clip = Rect::new(0, 0, width, height);
            },
        );
        // Record which stored rows this paint changed. If the layer was
        // valid, that is just the rows drawn again, wherever they are stored.
        layer.surface.set_written(valid.then(|| {
            drawn
                .iter()
                .enumerate()
                .filter(|(_, d)| d.is_some())
                .map(|(i, _)| Rect::new(0, pad + slot(i) as i64 * line, width, line))
                .collect()
        }));
    }
    for (i, (key, drawn)) in keys.iter().zip(drawn).enumerate() {
        let s = slot(i);
        match drawn {
            Some((_, runs)) => {
                layer.slots[s] = key.clone().map(|key| Shown { key, runs });
            }
            // A row still on the screen keeps its runs cached.
            None => {
                if let Some(shown) = &layer.slots[s] {
                    for run in &shown.runs {
                        layouts.get(run);
                    }
                }
            }
        }
    }
    if !valid || base != layer.base {
        layer.turned = cx.now;
    }
    layer.base = base;
    layer.context = Some(context.clone());
    layer.used = true;
    cx.canvas.surface(p.origin.x, p.origin.y, &layer.surface);
    true
}
