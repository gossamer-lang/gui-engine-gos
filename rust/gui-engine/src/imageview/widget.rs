//! The image viewer widget. The wheel zooms about the pointer, dragging
//! pans, and double-click switches between fitting and actual size. From the
//! keyboard, Page Up and Page Down zoom, Home fits, 1 shows actual size,
//! arrows pan and R turns the image clockwise.

use serde::{Deserialize, Serialize};
use windowing::{Key, MouseButton};

use super::{Image, State, Zoom};
use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Rect, Size};
use crate::text::layout::{self, LayoutOptions};
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx};
use crate::widgets::state::StateStore;

/// A view of image viewer `viewer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub id: String,
    pub viewer: i64,
    /// The image's alternative text.
    pub label: String,
    pub width: i64,
    pub height: i64,
}

pub fn layout_viewer(v: &View, c: Constraints, cx: &mut LayoutCx) -> Node {
    Node::leaf(c.constrain(Size::new(
        cx.theme.px(v.width.max(48)),
        cx.theme.px(v.height.max(48)),
    )))
}

/// Device pixels per image pixel for a zoom in a view of `size`.
fn scale(zoom: Zoom, image: &Image, size: Size) -> f64 {
    match zoom {
        Zoom::Fit => (size.width as f64 / image.width as f64)
            .min(size.height as f64 / image.height as f64)
            .min(1.0),
        Zoom::Scale(s) => s,
    }
}

/// Where the image's top-left lands and its drawn size.
fn placement(
    zoom: Zoom,
    center: Option<(f64, f64)>,
    image: &Image,
    size: Size,
) -> (f64, f64, f64, f64) {
    let s = scale(zoom, image, size);
    let (w, h) = (image.width as f64 * s, image.height as f64 * s);
    let (cx, cy) = center.unwrap_or((image.width as f64 / 2.0, image.height as f64 / 2.0));
    (
        size.width as f64 / 2.0 - cx * s,
        size.height as f64 / 2.0 - cy * s,
        w,
        h,
    )
}

pub fn paint_viewer(v: &View, node: &Node, cx: &mut PaintCx) {
    let size = node.size();
    let r = Rect::from_size(size);
    cx.fill(r, cx.theme.surface_alt);
    let at = cx.abs(r);
    let drawn = super::view_of(v.viewer, |image, preview, zoom, center, state| {
        let Some(image) = image else {
            return Some(match state {
                State::Loading(_) => "Loading…".to_string(),
                State::Failed(why) => format!("Cannot show the image: {why}"),
                State::Cancelled => "Loading cancelled".to_string(),
                _ => String::new(),
            });
        };
        let (x, y, w, h) = placement(zoom, center, image, size);
        // A large image drawn small is drawn from its reduced copy.
        let source = match preview {
            Some(p) if w <= p.width as f64 && h <= p.height as f64 => p,
            _ => image,
        };
        let saved = cx.canvas.clip;
        cx.canvas.clip = saved.intersect(at);
        cx.canvas.pixels_scaled(
            at.x + x.round() as i64,
            at.y + y.round() as i64,
            w.round().max(1.0) as i64,
            h.round().max(1.0) as i64,
            &source.rgba,
            source.width,
            source.height,
        );
        cx.canvas.clip = saved;
        None
    });
    if let Ok(Some(text)) = drawn
        && !text.is_empty()
    {
        let style = cx.theme.font(crate::style::TextRole::Body);
        let laid = layout::layout(
            &text,
            &style,
            LayoutOptions::wrapped((size.width - cx.theme.px(24)) as f64),
        );
        let color = cx.theme.text_muted;
        layout::draw(
            &mut cx.canvas,
            &laid,
            (at.x + cx.theme.px(12)) as f64,
            (at.y + size.height / 2) as f64 - laid.height / 2.0,
            &style,
            color,
        );
    }
    if cx.focus == v.id && cx.focus_visible {
        cx.focus_ring(r, 0);
    }
}

/// Zooms by `factor` keeping image point under view pixel (`px`, `py`) still.
fn zoom_about(v: &View, size: Size, factor: f64, px: f64, py: f64) -> Option<f64> {
    let next = super::view_of(v.viewer, |image, _, zoom, center, _| {
        let image = image.as_ref()?;
        let s = scale(zoom, image, size);
        let (x, y, _, _) = placement(zoom, center, image, size);
        let (ix, iy) = ((px - x) / s, (py - y) / s);
        let ns = (s * factor).clamp(0.01, 64.0);
        let (cx, cy) = (
            ix + (size.width as f64 / 2.0 - px) / ns,
            iy + (size.height as f64 / 2.0 - py) / ns,
        );
        Some((ns, (cx, cy)))
    })
    .ok()
    .flatten()?;
    let _ = super::set_view(v.viewer, Zoom::Scale(next.0), Some(next.1));
    Some(next.0)
}

fn pan(v: &View, size: Size, dx: f64, dy: f64, from: Option<(f64, f64)>) {
    let next = super::view_of(v.viewer, |image, _, zoom, center, _| {
        let image = image.as_ref()?;
        let s = scale(zoom, image, size);
        let (cx, cy) = from
            .or(center)
            .unwrap_or((image.width as f64 / 2.0, image.height as f64 / 2.0));
        // Panning a fitted image keeps the scale it was fitted at.
        Some((Zoom::Scale(s), (cx - dx / s, cy - dy / s)))
    });
    if let Ok(Some((zoom, center))) = next {
        let _ = super::set_view(v.viewer, zoom, Some(center));
    }
}

fn current_scale(v: &View, size: Size) -> Zoom {
    super::view_of(v.viewer, |image, _, zoom, _, _| {
        image.as_ref().map(|i| Zoom::Scale(scale(zoom, i, size)))
    })
    .ok()
    .flatten()
    .unwrap_or(Zoom::Fit)
}

fn percent(v: &View, size: Size) -> String {
    match current_scale(v, size) {
        Zoom::Scale(s) => format!("{:.0}%", s * 100.0),
        Zoom::Fit => "fit".into(),
    }
}

pub fn viewer_event(v: &View, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    let ev = &crate::widgets::cx::local_event(ev, cx);
    let size = node.size();
    match ev {
        Event::PointerDown {
            x,
            y,
            button: MouseButton::Left,
            clicks,
            ..
        } => {
            cx.focus_on(&v.id);
            if *clicks >= 2 {
                let fit =
                    super::view_of(v.viewer, |_, _, zoom, _, _| zoom == Zoom::Fit).unwrap_or(true);
                let _ = if fit {
                    super::set_view(v.viewer, Zoom::Scale(1.0), None)
                } else {
                    super::set_zoom(v.viewer, Zoom::Fit)
                };
            } else {
                let center = super::view_of(v.viewer, |image, _, zoom, center, _| {
                    image
                        .as_ref()
                        .map(|i| center.unwrap_or((i.width as f64 / 2.0, i.height as f64 / 2.0)))
                        .map(|c| (zoom, c))
                });
                if let Ok(Some((_, c))) = center {
                    st.set_field(
                        &v.id,
                        "pan",
                        &serde_json::to_string(&(x, y, c)).unwrap_or_default(),
                    );
                    cx.capture_pointer(&v.id);
                }
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
                serde_json::from_str::<(i64, i64, (f64, f64))>(&st.field(&v.id, "pan", ""))
            {
                pan(v, size, (*x - sx) as f64, (*y - sy) as f64, Some(from));
                cx.redraw = true;
                cx.consume();
            }
        }
        Event::PointerUp { .. } | Event::CaptureLost if !st.field(&v.id, "pan", "").is_empty() => {
            st.set_field(&v.id, "pan", "");
            cx.release_pointer();
        }
        Event::Scroll { x, y, dy, .. } if *dy != 0.0 => {
            // Without an image there is nothing to zoom, and the wheel goes on
            // to the views around this one.
            let zoomed = zoom_about(v, size, 1.15f64.powf(*dy), *x as f64, *y as f64);
            if zoomed.is_some() {
                cx.redraw = true;
                cx.consume();
            }
        }
        Event::KeyDown { key, .. } if cx.is_focused(&v.id) => {
            let (w, h) = (size.width as f64, size.height as f64);
            let step = w.min(h) / 8.0;
            match key {
                Key::PageUp => {
                    zoom_about(v, size, 1.25, w / 2.0, h / 2.0);
                }
                Key::PageDown => {
                    zoom_about(v, size, 0.8, w / 2.0, h / 2.0);
                }
                Key::Home => {
                    let _ = super::set_zoom(v.viewer, Zoom::Fit);
                }
                Key::Num1 | Key::Kp1 => {
                    let _ = super::set_view(v.viewer, Zoom::Scale(1.0), None);
                }
                Key::Left => pan(v, size, step, 0.0, None),
                Key::Right => pan(v, size, -step, 0.0, None),
                Key::Up => pan(v, size, 0.0, step, None),
                Key::Down => pan(v, size, 0.0, -step, None),
                Key::R => {
                    let _ = super::turn(v.viewer, 1);
                }
                _ => return,
            }
            cx.announce(&percent(v, size));
            cx.redraw = true;
            cx.consume();
        }
        Event::FocusIn | Event::FocusOut => cx.redraw = true,
        _ => {}
    }
}

pub fn viewer_semantics(v: &View, node: &Node) -> Option<AccessNode> {
    let mut n = AccessNode::control(Role::Image, &v.id, &v.label, "");
    n.bounds = Rect::from_size(node.size());
    n.actions = ["zoom-in", "zoom-out", "fit", "actual-size", "rotate"]
        .map(String::from)
        .to_vec();
    let size = node.size();
    n.description = super::view_of(v.viewer, |image, _, zoom, _, state| match state {
        State::Ready { width, height, .. } => {
            let s = image.as_ref().map_or(1.0, |i| scale(zoom, i, size));
            format!("{width} by {height} pixels, shown at {:.0}%", s * 100.0)
        }
        State::Loading(path) => format!("Loading {path}"),
        State::Failed(why) => format!("Cannot show the image: {why}"),
        State::Cancelled => "Loading cancelled".into(),
        State::Empty => "No image".into(),
    })
    .unwrap_or_default();
    Some(n)
}

/// Runs an accessible action, answering what to announce.
pub fn viewer_action(v: &View, node: &Node, action: &str) -> Option<String> {
    let size = node.size();
    let (w, h) = (size.width as f64 / 2.0, size.height as f64 / 2.0);
    match action {
        "zoom-in" => zoom_about(v, size, 1.25, w, h).map(|_| percent(v, size)),
        "zoom-out" => zoom_about(v, size, 0.8, w, h).map(|_| percent(v, size)),
        "fit" => super::set_zoom(v.viewer, Zoom::Fit)
            .ok()
            .map(|_| "Fit".into()),
        "actual-size" => super::set_view(v.viewer, Zoom::Scale(1.0), None)
            .ok()
            .map(|_| "100%".into()),
        "rotate" => super::turn(v.viewer, 1).ok().map(|_| "Rotated".into()),
        _ => None,
    }
}
