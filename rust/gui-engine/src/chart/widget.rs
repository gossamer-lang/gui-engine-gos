//! The chart widget. The pointer hovers points for their values, clicks a
//! legend entry to show or hide its series, drags to pan and scrolls to zoom
//! (Shift zooms the vertical axis); double-click restores the fitted view.
//! The keyboard moves a cursor point by point (Left and Right) and between
//! series (Up and Down), announcing each value, pans with Ctrl+arrows, zooms
//! with Page Up and Page Down, hides the current series with H and restores
//! the view with Home.

use serde::{Deserialize, Serialize};
use windowing::{Key, MouseButton};

use super::render::{self, Mapping};
use super::{Hit, Model, Scale, Window};
use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Rect, Size};
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx};
use crate::widgets::state::StateStore;

/// A view of chart `chart`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub id: String,
    pub chart: i64,
    /// The accessible name.
    pub label: String,
    /// The size the view asks for, in interface units.
    pub width: i64,
    pub height: i64,
}

pub fn layout_chart(v: &View, c: Constraints, cx: &mut LayoutCx) -> Node {
    Node::leaf(c.constrain(Size::new(
        cx.theme.px(v.width.max(64)),
        cx.theme.px(v.height.max(48)),
    )))
}

pub fn paint_chart(v: &View, node: &Node, cx: &mut PaintCx) {
    let size = node.size();
    let r = Rect::from_size(size);
    cx.fill(r, cx.theme.background);
    let theme = cx.theme.clone();
    if let Ok(ops) = super::read(v.chart, |model| {
        render::render(model, size.width, size.height, &theme)
    }) {
        let at = cx.abs(r);
        crate::render::display::draw(&mut cx.canvas, &ops, at.x, at.y);
    }
    if cx.focus == v.id && cx.focus_visible {
        cx.focus_ring(r, 0);
    }
}

fn mapping_window(model: &Model, size: Size, theme: &crate::style::Theme) -> (Rect, Window) {
    let frame = render::frame(model, size.width, size.height, theme);
    (frame.plot, render::ranges(model))
}

fn log(scale: &Scale) -> bool {
    matches!(scale, Scale::Log)
}

/// Shifts one axis's range by `delta` in its own scale's space.
fn shift(a: f64, b: f64, delta: f64, log_scale: bool) -> (f64, f64) {
    if log_scale {
        let f = 10f64.powf(delta);
        (a * f, b * f)
    } else {
        (a + delta, b + delta)
    }
}

/// Scales one axis's range by `factor` about `at`, in its own scale's space.
fn scale_about(a: f64, b: f64, at: f64, factor: f64, log_scale: bool) -> (f64, f64) {
    if log_scale {
        let (la, lb, lc) = (a.log10(), b.log10(), at.max(f64::MIN_POSITIVE).log10());
        (
            10f64.powf(lc + (la - lc) * factor),
            10f64.powf(lc + (lb - lc) * factor),
        )
    } else {
        (at + (a - at) * factor, at + (b - at) * factor)
    }
}

fn zoom(
    v: &View,
    size: Size,
    theme: &crate::style::Theme,
    factor: f64,
    at: (f64, f64),
    vertical: bool,
) {
    super::change(v.chart, |model| {
        let (plot, w) = mapping_window(model, size, theme);
        let map = Mapping::new(model, plot);
        let mut next = w;
        if vertical {
            (next.y0, next.y1) = scale_about(
                w.y0,
                w.y1,
                map.data_y(at.1),
                factor,
                log(&model.spec.y.scale),
            );
        } else {
            (next.x0, next.x1) = scale_about(
                w.x0,
                w.x1,
                map.data_x(at.0),
                factor,
                log(&model.spec.x.scale),
            );
        }
        if next.x1 > next.x0 && next.y1 > next.y0 && next.x0.is_finite() && next.y1.is_finite() {
            model.window = Some(next);
        }
    });
}

fn pan(v: &View, size: Size, theme: &crate::style::Theme, from: Window, dx: f64, dy: f64) {
    super::change(v.chart, |model| {
        let plot = render::frame(model, size.width, size.height, theme).plot;
        let (lx, ly) = (log(&model.spec.x.scale), log(&model.spec.y.scale));
        let span = |a: f64, b: f64, l: bool| if l { b.log10() - a.log10() } else { b - a };
        let ddx = -dx / plot.width.max(1) as f64 * span(from.x0, from.x1, lx);
        let ddy = dy / plot.height.max(1) as f64 * span(from.y0, from.y1, ly);
        let (x0, x1) = shift(from.x0, from.x1, ddx, lx);
        let (y0, y1) = shift(from.y0, from.y1, ddy, ly);
        model.window = Some(Window { x0, x1, y0, y1 });
    });
}

fn reading(model: &Model, series: usize, index: usize) -> Option<String> {
    let s = model.series.get(series)?;
    let (x, y) = *s.points.get(index)?;
    Some(format!(
        "{}: {}, {} ({} of {})",
        s.style.name,
        render::format_value(&model.spec.x.scale, x),
        render::format_value(&model.spec.y.scale, y),
        index + 1,
        s.points.len()
    ))
}

/// Moves the keyboard cursor and answers what it now reads.
fn step_cursor(v: &View, points: i64, series: i64) -> Option<String> {
    super::change(v.chart, |model| {
        let visible: Vec<usize> = (0..model.series.len())
            .filter(|i| !model.series[*i].hidden && !model.series[*i].points.is_empty())
            .collect();
        if visible.is_empty() {
            model.cursor = None;
            return None;
        }
        let (mut s, mut i) = model.cursor.unwrap_or((visible[0], 0));
        if !visible.contains(&s) {
            s = visible[0];
        }
        if series != 0 {
            let at = visible.iter().position(|x| *x == s).unwrap_or(0) as i64;
            s = visible[(at + series).rem_euclid(visible.len() as i64) as usize];
        }
        let len = model.series[s].points.len() as i64;
        i = (i as i64 + points).clamp(0, len - 1) as usize;
        model.cursor = Some((s, i));
        reading(model, s, i)
    })
    .flatten()
}

pub fn chart_event(v: &View, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    let ev = &crate::widgets::cx::local_event(ev, cx);
    let size = node.size();
    let theme = cx.theme.clone();
    match ev {
        Event::PointerMove {
            x,
            y,
            pressed: false,
            ..
        } => {
            let hit = super::read(v.chart, |m| {
                render::hit(m, size.width, size.height, &theme, *x as f64, *y as f64)
            })
            .ok()
            .flatten();
            let changed = super::change(v.chart, |m| {
                let changed = m.hover != hit;
                m.hover = hit;
                changed
            });
            if changed == Some(true) {
                cx.redraw = true;
            }
        }
        Event::PointerLeave => {
            let cleared = super::change(v.chart, |m| m.hover.take().is_some());
            cx.redraw |= cleared == Some(true);
        }
        Event::PointerDown {
            x,
            y,
            button: MouseButton::Left,
            clicks,
            ..
        } => {
            cx.focus_on(&v.id);
            if *clicks >= 2 {
                let _ = super::set_window(v.chart, None);
            } else if let Ok(Some(series)) = super::read(v.chart, |m| {
                render::legend_hit(m, size.width, size.height, &theme, *x, *y)
            }) {
                let hidden = super::read(v.chart, |m| m.series[series].hidden).unwrap_or(false);
                let _ = super::set_hidden(v.chart, series as i64, !hidden);
            } else if let Some(hit) = point_at(v, size, &theme, *x, *y) {
                cx.emit(&crate::widgets::cx::UiMsg::ChartPoint(v.id.clone(), hit));
            } else if let Ok(window) = super::window(v.chart) {
                st.set_field(
                    &v.id,
                    "pan",
                    &serde_json::to_string(&(x, y, window)).unwrap_or_default(),
                );
                cx.capture_pointer(&v.id);
            }
            cx.redraw = true;
            cx.consume();
        }
        Event::PointerMove {
            x,
            y,
            pressed: true,
            ..
        } => {
            if let Ok((sx, sy, from)) =
                serde_json::from_str::<(i64, i64, Window)>(&st.field(&v.id, "pan", ""))
            {
                pan(v, size, &theme, from, (*x - sx) as f64, (*y - sy) as f64);
                cx.redraw = true;
                cx.consume();
            }
        }
        Event::PointerUp { .. } | Event::CaptureLost if !st.field(&v.id, "pan", "").is_empty() => {
            st.set_field(&v.id, "pan", "");
            cx.release_pointer();
        }
        Event::Scroll { x, y, dy, mods, .. } if *dy != 0.0 => {
            zoom(
                v,
                size,
                &theme,
                0.85f64.powf(*dy),
                (*x as f64, *y as f64),
                mods.shift,
            );
            cx.redraw = true;
            cx.consume();
        }
        Event::KeyDown { key, mods, .. } if cx.is_focused(&v.id) => {
            let center = (size.width as f64 / 2.0, size.height as f64 / 2.0);
            let step = |dx: f64, dy: f64| {
                if let Ok(from) = super::window(v.chart) {
                    pan(v, size, &theme, from, dx, dy);
                }
            };
            let said = match key {
                Key::Left if mods.ctrl => {
                    step(size.width as f64 / 8.0, 0.0);
                    None
                }
                Key::Right if mods.ctrl => {
                    step(-(size.width as f64) / 8.0, 0.0);
                    None
                }
                Key::Up if mods.ctrl => {
                    step(0.0, size.height as f64 / 8.0);
                    None
                }
                Key::Down if mods.ctrl => {
                    step(0.0, -(size.height as f64) / 8.0);
                    None
                }
                Key::Left => step_cursor(v, -1, 0),
                Key::Right => step_cursor(v, 1, 0),
                Key::Up => step_cursor(v, 0, -1),
                Key::Down => step_cursor(v, 0, 1),
                Key::PageUp => {
                    zoom(v, size, &theme, 0.8, center, mods.shift);
                    Some("Zoomed in".to_string())
                }
                Key::PageDown => {
                    zoom(v, size, &theme, 1.25, center, mods.shift);
                    Some("Zoomed out".to_string())
                }
                Key::Home => {
                    let _ = super::set_window(v.chart, None);
                    Some("View reset".to_string())
                }
                Key::H => super::change(v.chart, |m| {
                    let (s, _) = m.cursor?;
                    let series = &mut m.series[s];
                    series.hidden = !series.hidden;
                    Some(format!(
                        "{} {}",
                        series.style.name,
                        if series.hidden { "hidden" } else { "shown" }
                    ))
                })
                .flatten(),
                _ => return,
            };
            if let Some(text) = said {
                cx.announce(&text);
            }
            cx.redraw = true;
            cx.consume();
        }
        Event::FocusIn | Event::FocusOut => cx.redraw = true,
        _ => {}
    }
}

/// A text summary of a chart: its kind, series and their ranges.
pub fn summary(model: &Model) -> String {
    let mut parts = vec![format!("{} series", model.series.len())];
    for s in &model.series {
        let ys = s.points.iter().map(|p| p.1);
        let (lo, hi) = ys.fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), y| {
            (a.min(y), b.max(y))
        });
        let range = if lo.is_finite() {
            format!(
                "{} to {}",
                render::format_value(&model.spec.y.scale, lo),
                render::format_value(&model.spec.y.scale, hi)
            )
        } else {
            "no data".to_string()
        };
        parts.push(format!(
            "{}{}: {} points, {range}",
            s.style.name,
            if s.hidden { " (hidden)" } else { "" },
            s.points.len()
        ));
    }
    parts.join("; ")
}

pub fn chart_semantics(v: &View, node: &Node) -> Option<AccessNode> {
    let mut n = AccessNode::control(Role::Canvas, &v.id, &v.label, "");
    n.actions = [
        "next-point",
        "previous-point",
        "next-series",
        "zoom-in",
        "zoom-out",
        "reset-view",
    ]
    .map(String::from)
    .to_vec();
    n.bounds = Rect::from_size(node.size());
    if let Ok((description, value)) = super::read(v.chart, |m| {
        let value = m
            .cursor
            .and_then(|(s, i)| reading(m, s, i))
            .unwrap_or_default();
        (summary(m), value)
    }) {
        n.description = description;
        n.value = value;
    }
    Some(n)
}

/// Runs an accessible action, answering what to announce.
pub fn chart_action(
    v: &View,
    node: &Node,
    action: &str,
    theme: &crate::style::Theme,
) -> Option<String> {
    let size = node.size();
    let center = (size.width as f64 / 2.0, size.height as f64 / 2.0);
    match action {
        "next-point" => step_cursor(v, 1, 0),
        "previous-point" => step_cursor(v, -1, 0),
        "next-series" => step_cursor(v, 0, 1),
        "zoom-in" => {
            zoom(v, size, theme, 0.8, center, false);
            Some("Zoomed in".into())
        }
        "zoom-out" => {
            zoom(v, size, theme, 1.25, center, false);
            Some("Zoomed out".into())
        }
        "reset-view" => super::set_window(v.chart, None)
            .ok()
            .map(|_| "View reset".into()),
        _ => None,
    }
}

/// The point under a local pixel, for applications reacting to clicks.
pub fn point_at(v: &View, size: Size, theme: &crate::style::Theme, x: i64, y: i64) -> Option<Hit> {
    super::read(v.chart, |m| {
        render::hit(m, size.width, size.height, theme, x as f64, y as f64)
    })
    .ok()
    .flatten()
}
