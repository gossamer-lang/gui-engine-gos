// numeric: spin boxes, sliders, range sliders, and dials. Every pointer
// gesture has a keyboard equivalent (arrows step, Page Up/Down page,
// Home/End jump to the bounds). Spin boxes keep a draft separate from the
// committed value: intermediate text such as "-" or "1." stays editable,
// invalid text is marked and never committed, and the value commits on
// Enter, on a step, or when focus leaves.
use crate::strext::StrExt;
use windowing::{Key, MouseButton};

use crate::accessibility::{AccessNode, Role};
use crate::editing::EditState;
use crate::geometry::{Constraints, Rect, Size};
use crate::render::icons::Icon;
use crate::style::{TextRole, Theme};
use crate::text::layout;
use crate::text::layout::LayoutOptions;
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx, UiMsg};
use crate::widgets::state::{StateStore, WState};

/// A spin box: a number field with step buttons.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpinBox {
    pub id: String,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub page: f64,
    /// Digits after the decimal point (0 for integers).
    pub decimals: i64,
    /// A unit shown after the number ("px", "%").
    pub suffix: String,
    pub label: String,
}

/// A slider along one axis.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Slider {
    pub id: String,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub page: f64,
    pub vertical: bool,
    /// Tick marks drawn under the track (0 for none).
    pub ticks: i64,
    pub label: String,
}

/// A slider with a low and a high thumb.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RangeSlider {
    pub id: String,
    pub low: f64,
    pub high: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub label: String,
}

/// A rotary knob.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Dial {
    pub id: String,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    /// Turning past the maximum wraps to the minimum (an angle).
    pub wraps: bool,
    pub label: String,
}

/// The numeric controls.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Numeric {
    Spin(SpinBox),
    Slide(Slider),
    Range(RangeSlider),
    Knob(Dial),
}

impl Numeric {
    pub fn id(&self) -> String {
        match self {
            Numeric::Spin(s) => s.id.clone(),
            Numeric::Slide(s) => s.id.clone(),
            Numeric::Range(r) => r.id.clone(),
            Numeric::Knob(d) => d.id.clone(),
        }
    }
}

/// `v` clamped to the bounds and snapped to `step` from `min`.
pub fn snap(v: f64, min: f64, max: f64, step: f64) -> f64 {
    let lo = min.min(max);
    let hi = min.max(max);
    let c = v.max(lo).min(hi);
    if step <= 0.0 {
        return c;
    }
    let n = ((c - lo) / step + 0.5).floor();
    (lo + n * step).min(hi)
}

/// Text for a value with `decimals` digits.
pub fn format_number(v: f64, decimals: i64) -> String {
    if decimals <= 0 {
        return format!("{}", (v + if v >= 0.0 { 0.5 } else { -0.5 }) as i64);
    }
    let mut scale = 1.0;
    let mut k = 0;
    while k < decimals {
        scale *= 10.0;
        k += 1;
    }
    let rounded = (v.abs() * scale + 0.5).floor();
    let whole = (rounded / scale) as i64;
    let frac = (rounded - whole as f64 * scale) as i64;
    let mut digits = format!("{}", frac);
    while digits.byte_len() < decimals {
        digits = "0".to_owned() + &digits
    }
    let sign = if v < 0.0 && rounded > 0.0 { "-" } else { "" };
    format!("{}{}.{}", sign, whole, digits)
}

/// How spin-box text parses: a value, intermediate text, or invalid.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Parsed {
    Number(f64),
    Partial,
    Invalid(String),
}

/// Parses spin-box text as the installed locale writes numbers
/// (`localization::parse_number`); a separator still being typed is partial.
pub fn parse_number(text: &str, suffix: &str) -> Parsed {
    let mut t = text.trim();
    if !suffix.is_empty() && t.ends_with(&suffix) {
        t = t.substring(0, t.byte_len() - suffix.byte_len()).trim();
    }
    let decimal = crate::localization::conventions(&crate::localization::locale()).decimal;
    if t.is_empty() || t == "-" || t == "+" || t == "-." {
        return Parsed::Partial;
    }
    if t.ends_with(".") || t.ends_with(",") || t.ends_with(&decimal) {
        return Parsed::Partial;
    }
    match crate::localization::parse_number(t) {
        Some(v) => Parsed::Number(v),
        None => Parsed::Invalid(crate::localization::message(
            "number",
            "\u{201C}{text}\u{201D} is not a number",
            1,
            &crate::hmap! { "text" => text.to_string() },
        )),
    }
}

fn spin_text(s: &SpinBox) -> String {
    let n = crate::localization::format_number(s.value, s.decimals);
    if s.suffix.is_empty() {
        n
    } else {
        format!("{} {}", n, s.suffix)
    }
}

// Wide enough for the longest value in range, so no value is cut off.
fn spin_width(s: &SpinBox, theme: &Theme, btn: i64) -> i64 {
    let font = theme.font(TextRole::Body);
    let widest = [s.min, s.max]
        .iter()
        .map(|v| {
            let mut probe = s.clone();
            probe.value = *v;
            let (tw, _) = layout::measure(&spin_text(&probe), &font);
            tw.ceil() as i64
        })
        .max()
        .unwrap_or(0);
    theme.padding() * 2 + widest + btn * 2
}

fn spin_state(s: &SpinBox, st: &StateStore) -> EditState {
    let mut e = st.edit(&s.id);
    e.sync(&spin_text(s));
    e
}

pub fn layout_numeric(n: &Numeric, c: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    match n {
        Numeric::Spin(s) => {
            let h = theme.control_height();
            let btn = h;
            let w = if c.min_width == c.max_width && c.min_width > 0 {
                c.max_width
            } else {
                spin_width(s, theme, btn).min(c.max_width)
            };
            let mut node = Node::leaf(c.constrain(Size::new(w, h)));
            // Text area, decrement, increment.
            node.parts = vec![
                Rect::new(
                    theme.padding(),
                    0,
                    (w - btn * 2 - theme.padding()).max(1),
                    h,
                ),
                Rect::new(w - btn * 2, 0, btn, h),
                Rect::new(w - btn, 0, btn, h),
            ];
            let (a, _, lh) = layout::metrics(&theme.font(TextRole::Body));
            node.baseline = (h - lh as i64) / 2 + a as i64;
            node
        }
        Numeric::Slide(s) => {
            let thick = theme.px(32);
            let long = theme.px(200);
            let size = if s.vertical {
                c.constrain(Size::new(
                    thick,
                    if c.has_bounded_height() {
                        c.max_height.min(theme.px(400))
                    } else {
                        long
                    },
                ))
            } else {
                c.constrain(Size::new(
                    if c.has_bounded_width() {
                        c.max_width.min(theme.px(480))
                    } else {
                        long
                    },
                    thick + if s.ticks > 0 { theme.px(8) } else { 0 },
                ))
            };
            Node::leaf(size)
        }
        Numeric::Range(_) => {
            let w = if c.has_bounded_width() {
                c.max_width.min(theme.px(480))
            } else {
                theme.px(240)
            };
            Node::leaf(c.constrain(Size::new(w, theme.px(32))))
        }
        Numeric::Knob(_) => {
            let s = theme.px(64);
            Node::leaf(c.constrain(Size::new(s, s)))
        }
    }
}

// Track geometry (local): start, length, and cross position.
fn track(vertical: bool, size: Size, theme: &Theme) -> (i64, i64, i64) {
    let inset = theme.px(12);
    if vertical {
        (inset, size.height - inset * 2, size.width / 2)
    } else {
        (inset, size.width - inset * 2, theme.px(16))
    }
}

fn fraction(v: f64, min: f64, max: f64) -> f64 {
    if max == min {
        0.0
    } else {
        ((v - min) / (max - min)).clamp(0.0, 1.0)
    }
}

pub fn paint_numeric(n: &Numeric, node: &Node, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let accent = if cx.disabled {
        theme.text_disabled
    } else {
        theme.accent
    };
    match n {
        Numeric::Spin(s) => {
            let r = Rect::from_size(node.size());
            let e = spin_state(s, &cx.states);
            let invalid = !cx.states.field(&s.id, "invalid", "").is_empty();
            let focused = cx.is_focused(&s.id);
            cx.fill_round(
                r,
                theme.radius(),
                if cx.disabled {
                    theme.surface_alt
                } else {
                    theme.field
                },
            );
            let edge = if invalid {
                theme.danger
            } else if focused {
                theme.accent
            } else {
                theme.border_strong.with_alpha(180)
            };
            cx.stroke_round(
                r,
                theme.radius(),
                if focused || invalid {
                    theme.border_width() * 2
                } else {
                    theme.border_width()
                },
                edge,
            );
            if node.parts.len() >= 3 {
                let tr = cx.abs(node.parts[0]);
                let style = theme.font(TextRole::Body);
                let l = layout::layout(&e.text, &style, LayoutOptions::single_line());
                let ty = tr.y + (tr.height - l.height.ceil() as i64) / 2;
                // The number stays in its text area, clear of the buttons.
                let saved = cx.canvas.clip;
                cx.canvas.push_clip(tr);
                if e.has_selection() && focused {
                    let (a, b) = e.selection();
                    let sel = if cx.active {
                        theme.selection
                    } else {
                        theme.selection_inactive
                    };
                    for sr in l.selection_rects(a, b) {
                        cx.canvas
                            .rect(tr.x + sr.x, ty + sr.y, sr.width, sr.height, sel)
                    }
                }
                let fg = if cx.disabled {
                    theme.text_disabled
                } else {
                    theme.text
                };
                layout::draw(&mut cx.canvas, &l, tr.x as f64, ty as f64, &style, fg);
                if focused && cx.caret_on && !cx.disabled {
                    let cr = l.caret_rect(e.caret);
                    cx.canvas
                        .rect(tr.x + cr.x, ty + cr.y, theme.px(2), cr.height, theme.text)
                }
                cx.canvas.pop_clip(saved);
                let isz = theme.px(theme.shape.field_icon);
                let minus = cx.abs(node.parts[1]);
                let plus = cx.abs(node.parts[2]);
                cx.canvas.rect(
                    minus.x,
                    minus.y + theme.px(8),
                    theme.border_width(),
                    minus.height - theme.px(16),
                    theme.border,
                );
                let at_min = s.value <= s.min;
                let at_max = s.value >= s.max;
                cx.icon(
                    &Icon::Minus,
                    minus.x + (minus.width - isz) / 2,
                    minus.y + (minus.height - isz) / 2,
                    isz,
                    if at_min || cx.disabled {
                        theme.text_disabled
                    } else {
                        theme.text
                    },
                );
                cx.icon(
                    &Icon::Plus,
                    plus.x + (plus.width - isz) / 2,
                    plus.y + (plus.height - isz) / 2,
                    isz,
                    if at_max || cx.disabled {
                        theme.text_disabled
                    } else {
                        theme.text
                    },
                )
            }
            if focused {
                let rad = theme.radius();
                cx.focus_ring(r, rad)
            }
        }
        Numeric::Slide(s) => {
            let size = node.size();
            let (start, len, cross) = track(s.vertical, size, &theme);
            let f = fraction(s.value, s.min, s.max);
            let t = theme.px(4);
            let knob = theme.px(10);
            if s.vertical {
                let pos = start + len - (f * len as f64) as i64;
                cx.fill_round(Rect::new(cross - t / 2, start, t, len), t / 2, theme.border);
                cx.fill_round(
                    Rect::new(cross - t / 2, pos, t, start + len - pos),
                    t / 2,
                    accent,
                );
                let a = cx.abs(Rect::new(cross, pos, 0, 0));
                cx.canvas.circle(a.x, a.y, knob, accent);
                cx.canvas
                    .circle(a.x, a.y, knob - theme.px(3), thumb_fill(&s.id, cx));
                if cx.is_focused(&s.id) {
                    cx.focus_ring(
                        Rect::new(cross - knob, pos - knob, knob * 2, knob * 2),
                        knob,
                    )
                }
            } else {
                let pos = start + (f * len as f64) as i64;
                cx.fill_round(Rect::new(start, cross - t / 2, len, t), t / 2, theme.border);
                cx.fill_round(
                    Rect::new(start, cross - t / 2, pos - start, t),
                    t / 2,
                    accent,
                );
                if s.ticks > 1 {
                    let mut k = 0;
                    while k < s.ticks {
                        let x = start + len * k / (s.ticks - 1);
                        cx.fill(
                            Rect::new(
                                x,
                                cross + knob + theme.px(2),
                                theme.border_width(),
                                theme.px(6),
                            ),
                            theme.border_strong,
                        );
                        k += 1;
                    }
                }
                let a = cx.abs(Rect::new(pos, cross, 0, 0));
                cx.canvas.circle(a.x, a.y, knob, accent);
                cx.canvas
                    .circle(a.x, a.y, knob - theme.px(3), thumb_fill(&s.id, cx));
                if cx.is_focused(&s.id) {
                    cx.focus_ring(
                        Rect::new(pos - knob, cross - knob, knob * 2, knob * 2),
                        knob,
                    )
                }
            }
        }
        Numeric::Range(r) => {
            let size = node.size();
            let (start, len, cross) = track(false, size, &theme);
            let t = theme.px(4);
            let knob = theme.px(10);
            let lo = start + (fraction(r.low, r.min, r.max) * len as f64) as i64;
            let hi = start + (fraction(r.high, r.min, r.max) * len as f64) as i64;
            cx.fill_round(Rect::new(start, cross - t / 2, len, t), t / 2, theme.border);
            cx.fill(Rect::new(lo, cross - t / 2, hi - lo, t), accent);
            let active = cx.states.drag(&r.id).part;
            for (i, x) in [(0, lo), (1, hi)] {
                let a = cx.abs(Rect::new(x, cross, 0, 0));
                cx.canvas.circle(a.x, a.y, knob, accent);
                let fill = if cx.states.drag(&r.id).active && active == i {
                    theme.pressed
                } else if cx.is_hover(&r.id) {
                    theme.hover
                } else {
                    theme.field
                };
                cx.canvas.circle(a.x, a.y, knob - theme.px(3), fill);
                if cx.is_focused(&r.id) && active == i {
                    cx.focus_ring(Rect::new(x - knob, cross - knob, knob * 2, knob * 2), knob)
                }
            }
        }
        Numeric::Knob(d) => {
            let size = node.size();
            let rad = size.width.min(size.height) / 2 - theme.px(4);
            let c = cx.abs(Rect::new(size.width / 2, size.height / 2, 0, 0));
            let face = if cx.states.drag(&d.id).active && !cx.disabled {
                theme.pressed
            } else if cx.is_hover(&d.id) {
                theme.hover
            } else {
                theme.surface
            };
            cx.canvas.circle(c.x, c.y, rad, face);
            cx.canvas
                .circle_stroke(c.x, c.y, rad, theme.border_width(), theme.border_strong);
            let f = fraction(d.value, d.min, d.max);
            // 270 degrees of travel from bottom-left, clockwise.
            let ang = (135.0 + f * 270.0_f64).to_radians();
            let px = c.x + (ang.cos() * (rad - theme.px(8)) as f64) as i64;
            let py = c.y + (ang.sin() * (rad - theme.px(8)) as f64) as i64;
            cx.canvas
                .line_aa_w(c.x, c.y, px, py, theme.px(3) as f64, accent);
            cx.canvas.circle(px, py, theme.px(4), accent);
            if cx.is_focused(&d.id) {
                cx.focus_ring(Rect::from_size(size), size.width / 2)
            }
        }
    }
}

// The inside of a slider's thumb: pressed while dragged, lit under the pointer.
fn thumb_fill(id: &str, cx: &PaintCx) -> crate::style::color::Color {
    if cx.states.drag(id).active && !cx.disabled {
        cx.theme.pressed
    } else if cx.is_hover(id) {
        cx.theme.hover
    } else {
        cx.theme.field
    }
}

fn step_value(
    v: f64,
    min: f64,
    max: f64,
    step: f64,
    page: f64,
    key: Key,
    rtl: bool,
    vertical: bool,
) -> Option<f64> {
    let s = if step > 0.0 {
        step
    } else {
        (max - min) / 100.0
    };
    let p = if page > 0.0 { page } else { s * 10.0 };
    let inc = if vertical {
        Key::Up
    } else if rtl {
        Key::Left
    } else {
        Key::Right
    };
    let dec = if vertical {
        Key::Down
    } else if rtl {
        Key::Right
    } else {
        Key::Left
    };
    if key == inc || key == Key::Up {
        return Some(snap(v + s, min, max, step));
    };
    if key == dec || key == Key::Down {
        return Some(snap(v - s, min, max, step));
    };
    if key == Key::PageUp {
        return Some(snap(v + p, min, max, step));
    };
    if key == Key::PageDown {
        return Some(snap(v - p, min, max, step));
    };
    if key == Key::Home {
        return Some(min);
    };
    if key == Key::End {
        return Some(max);
    };
    None
}

fn commit_spin(s: &SpinBox, e: &mut EditState, st: &mut StateStore, cx: &mut EventCx) {
    match parse_number(&e.text, &s.suffix) {
        Parsed::Number(v) => {
            let snapped = snap(v, s.min, s.max, 0.0);
            st.set_field(&s.id, "invalid", "");
            if snapped != s.value {
                cx.emit(&UiMsg::Value(s.id.clone(), snapped))
            };
            let text = if s.suffix.is_empty() {
                format_number(snapped, s.decimals)
            } else {
                format!("{} {}", format_number(snapped, s.decimals), s.suffix)
            };
            e.text = text.clone().into();
            e.external = text.into();
            e.move_to(e.text.byte_len(), false)
        }
        Parsed::Partial => {
            // An abandoned partial entry returns to the committed value.
            let text = spin_text(s);
            e.text = text.clone().into();
            e.external = text.into();
            e.move_to(e.text.byte_len(), false);
            st.set_field(&s.id, "invalid", "")
        }
        Parsed::Invalid(why) => {
            st.set_field(&s.id, "invalid", &why);
            cx.announce(&why)
        }
    }
    cx.redraw = true;
}

// Reports `next` unless it is the value the control already shows.
fn report(id: &str, current: f64, next: f64, cx: &mut EventCx) {
    if next != current {
        cx.emit(&UiMsg::Value(id.to_string(), next))
    }
}

pub fn numeric_event(n: &Numeric, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    if cx.disabled {
        return;
    }
    let theme = cx.theme.clone();
    match n {
        Numeric::Spin(s) => {
            let mut e = spin_state(s, st);
            let now = cx.now;
            match ev {
                Event::PointerDown { x, y, button, .. } => {
                    if *button != MouseButton::Left {
                        return;
                    };
                    cx.focus_on(&s.id);
                    let lx = x - cx.origin_x;
                    let ly = y - cx.origin_y;
                    if node.parts.len() >= 3 && node.parts[1].contains(lx, ly) {
                        report(
                            &s.id,
                            s.value,
                            snap(s.value - s.step.max(0.0), s.min, s.max, s.step),
                            cx,
                        )
                    } else if node.parts.len() >= 3 && node.parts[2].contains(lx, ly) {
                        report(
                            &s.id,
                            s.value,
                            snap(s.value + s.step.max(0.0), s.min, s.max, s.step),
                            cx,
                        )
                    } else if !node.parts.is_empty() {
                        let tl = layout::layout(
                            &e.text,
                            &theme.font(TextRole::Body),
                            LayoutOptions::single_line(),
                        );
                        e.move_to(tl.hit((lx - node.parts[0].x) as f64, 1.0), false)
                    }
                    cx.consume()
                }
                Event::Text(t) => {
                    e.insert(t, now, 0);
                    match parse_number(&e.text, &s.suffix) {
                        Parsed::Invalid(why) => st.set_field(&s.id, "invalid", &why),
                        _ => st.set_field(&s.id, "invalid", ""),
                    }
                    cx.redraw = true;
                    cx.consume()
                }
                Event::KeyDown { key, mods, .. } => {
                    if let Some(v) =
                        step_value(s.value, s.min, s.max, s.step, s.page, *key, false, true)
                        && (*key == Key::Up
                            || *key == Key::Down
                            || *key == Key::PageUp
                            || *key == Key::PageDown)
                    {
                        report(&s.id, s.value, v, cx);
                        cx.consume()
                    }
                    match key {
                        Key::Left => {
                            e.step(-1, false, mods.shift);
                            cx.consume()
                        }
                        Key::Right => {
                            e.step(1, false, mods.shift);
                            cx.consume()
                        }
                        Key::Home => {
                            e.move_to(0, mods.shift);
                            cx.consume()
                        }
                        Key::End => {
                            e.move_to(e.text.byte_len(), mods.shift);
                            cx.consume()
                        }
                        Key::Backspace => {
                            e.backspace(false, now);
                            cx.consume()
                        }
                        Key::Delete => {
                            e.delete_forward(false, now);
                            cx.consume()
                        }
                        Key::Return | Key::KpEnter => {
                            commit_spin(s, &mut e, st, cx);
                            cx.consume()
                        }
                        Key::Escape => {
                            let text = spin_text(s);
                            e.text = text.clone().into();
                            e.external = text.into();
                            st.set_field(&s.id, "invalid", "")
                        }
                        _ => {
                            if mods.shortcut() && *key == Key::A {
                                e.select_all();
                                cx.consume()
                            }
                        }
                    }
                    cx.redraw = true;
                }
                Event::FocusIn => {
                    e.select_all();
                    cx.redraw = true;
                }
                Event::FocusOut => commit_spin(s, &mut e, st, cx),
                Event::Scroll { dy, .. } if cx.is_focused(&s.id) && *dy != 0.0 => {
                    report(
                        &s.id,
                        s.value,
                        snap(
                            s.value + if *dy > 0.0 { s.step } else { -s.step },
                            s.min,
                            s.max,
                            s.step,
                        ),
                        cx,
                    );
                    cx.consume()
                }
                _ => (),
            }
            st.set(&s.id, WState::Edit(e))
        }
        Numeric::Slide(s) => {
            let size = node.size();
            let (start, len, _) = track(s.vertical, size, &theme);
            let ox = cx.origin_x;
            let oy = cx.origin_y;
            let value_at = |x: i64, y: i64| -> f64 {
                let p = if s.vertical {
                    start + len - (y - oy)
                } else {
                    x - ox - start
                };
                let f = (p as f64 / len.max(1) as f64).clamp(0.0, 1.0);
                let f2 = if theme.rtl && !s.vertical { 1.0 - f } else { f };
                snap(s.min + f2 * (s.max - s.min), s.min, s.max, s.step)
            };
            let mut d = st.drag(&s.id);
            match ev {
                Event::PointerDown { x, y, button, .. } => {
                    if *button != MouseButton::Left {
                        return;
                    };
                    cx.focus_on(&s.id);
                    d.active = true;
                    cx.capture_pointer(&s.id);
                    let v = value_at(*x, *y);
                    if v != s.value {
                        cx.emit(&UiMsg::Value(s.id.clone(), v))
                    };
                    cx.consume()
                }
                Event::PointerMove { x, y, pressed, .. } if *pressed && d.active => {
                    let v = value_at(*x, *y);
                    if v != s.value {
                        cx.emit(&UiMsg::Value(s.id.clone(), v))
                    };
                    cx.consume()
                }
                Event::PointerUp { .. } | Event::CaptureLost if d.active => {
                    d.active = false;
                    cx.release_pointer()
                }
                Event::KeyDown { key, .. } => {
                    if let Some(v) = step_value(
                        s.value, s.min, s.max, s.step, s.page, *key, theme.rtl, s.vertical,
                    ) {
                        if v != s.value {
                            cx.emit(&UiMsg::Value(s.id.clone(), v))
                        };
                        cx.consume()
                    }
                }
                _ => (),
            }
            st.set(&s.id, WState::Drag(d))
        }
        Numeric::Range(r) => {
            let size = node.size();
            let (start, len, _) = track(false, size, &theme);
            let mut d = st.drag(&r.id);
            let ox = cx.origin_x;
            let to_value = |x: i64| -> f64 {
                let f = ((x - ox - start) as f64 / len.max(1) as f64).clamp(0.0, 1.0);
                snap(r.min + f * (r.max - r.min), r.min, r.max, r.step)
            };
            let emit_pair = |lo: f64, hi: f64, cx: &mut EventCx| {
                let a = lo.min(hi);
                let b = lo.max(hi);
                if a != r.low || b != r.high {
                    cx.emit(&UiMsg::Range(r.id.clone(), a, b))
                }
            };
            match ev {
                Event::PointerDown { x, button, .. } => {
                    if *button != MouseButton::Left {
                        return;
                    };
                    cx.focus_on(&r.id);
                    let v = to_value(*x);
                    // The nearer thumb follows the pointer.
                    d.part = if (v - r.low).abs() <= (v - r.high).abs() {
                        0
                    } else {
                        1
                    };
                    d.active = true;
                    cx.capture_pointer(&r.id);
                    if d.part == 0 {
                        emit_pair(v.min(r.high), r.high, cx)
                    } else {
                        emit_pair(r.low, v.max(r.low), cx)
                    };
                    cx.consume()
                }
                Event::PointerMove { x, pressed, .. } if *pressed && d.active => {
                    let v = to_value(*x);
                    if d.part == 0 {
                        emit_pair(v.min(r.high), r.high, cx)
                    } else {
                        emit_pair(r.low, v.max(r.low), cx)
                    };
                    cx.consume()
                }
                Event::PointerUp { .. } | Event::CaptureLost => {
                    d.active = false;
                    cx.release_pointer()
                }
                Event::KeyDown { key, mods, .. } => {
                    // Tab moves from the low thumb to the high one before leaving,
                    // Shift+Tab back from the high thumb to the low one.
                    let along = *key == Key::Tab && !d.active && !cx.handled;
                    if along && !mods.shift && d.part == 0 {
                        d.part = 1;
                        cx.redraw = true;
                        cx.consume()
                    } else if along && mods.shift && d.part == 1 {
                        d.part = 0;
                        cx.redraw = true;
                        cx.consume()
                    } else {
                        let cur = if d.part == 0 { r.low } else { r.high };
                        if let Some(v) =
                            step_value(cur, r.min, r.max, r.step, 0.0, *key, theme.rtl, false)
                        {
                            if d.part == 0 {
                                emit_pair(v.min(r.high), r.high, cx)
                            } else {
                                emit_pair(r.low, v.max(r.low), cx)
                            };
                            cx.consume()
                        }
                    }
                }
                // Focus from the keyboard starts at the low thumb; a press keeps
                // the thumb it grabbed.
                Event::FocusIn if !d.active => d.part = 0,
                _ => (),
            }
            st.set(&r.id, WState::Drag(d))
        }
        Numeric::Knob(k) => {
            let size = node.size();
            let mut d = st.drag(&k.id);
            let ox = cx.origin_x;
            let oy = cx.origin_y;
            let value_at = |x: i64, y: i64| -> f64 {
                let dx = (x - ox - size.width / 2) as f64;
                let dy = (y - oy - size.height / 2) as f64;
                let mut deg = dy.atan2(dx).to_degrees() - 135.0;
                while deg < 0.0 {
                    deg += 360.0
                }
                let f = (deg / 270.0).min(1.0);
                snap(k.min + f * (k.max - k.min), k.min, k.max, k.step)
            };
            match ev {
                Event::PointerDown { x, y, button, .. } => {
                    if *button != MouseButton::Left {
                        return;
                    };
                    cx.focus_on(&k.id);
                    d.active = true;
                    cx.capture_pointer(&k.id);
                    report(&k.id, k.value, value_at(*x, *y), cx);
                    cx.consume()
                }
                Event::PointerMove { x, y, pressed, .. } if *pressed && d.active => {
                    report(&k.id, k.value, value_at(*x, *y), cx);
                    cx.consume()
                }
                Event::PointerUp { .. } | Event::CaptureLost => {
                    d.active = false;
                    cx.release_pointer()
                }
                Event::KeyDown { key, .. } => {
                    if let Some(v) =
                        step_value(k.value, k.min, k.max, k.step, 0.0, *key, false, false)
                    {
                        let mut out = v;
                        if k.wraps && *key == Key::Right && k.value >= k.max {
                            out = k.min
                        };
                        if k.wraps && *key == Key::Left && k.value <= k.min {
                            out = k.max
                        };
                        report(&k.id, k.value, out, cx);
                        cx.consume()
                    }
                }
                _ => (),
            }
            st.set(&k.id, WState::Drag(d))
        }
    }
}

pub fn numeric_semantics(n: &Numeric, st: &StateStore) -> Option<AccessNode> {
    match n {
        Numeric::Spin(s) => {
            let mut node = AccessNode::control(Role::SpinButton, &s.id, &s.label, "increment");
            node.actions.push("decrement".to_string());
            node.range(s.min, s.max, s.value);
            node.value = spin_text(s);
            let why = st.field(&s.id, "invalid", "");
            node.invalid = !why.is_empty();
            node.description = why;
            Some(node)
        }
        Numeric::Slide(s) => {
            let mut node = AccessNode::control(Role::Slider, &s.id, &s.label, "increment");
            node.actions.push("decrement".to_string());
            node.range(s.min, s.max, s.value);
            Some(node)
        }
        Numeric::Range(r) => {
            let mut g = AccessNode::new(Role::Group, &r.label);
            g.id = r.id.clone();
            let mut lo = AccessNode::control(
                Role::Slider,
                &format!("{}#low", r.id),
                &crate::localization::message(
                    "range",
                    "{control} minimum",
                    1,
                    &crate::hmap! { "control" => r.label.clone() },
                ),
                "increment",
            );
            lo.range(r.min, r.high, r.low);
            lo.focusable = false;
            let mut hi = AccessNode::control(
                Role::Slider,
                &format!("{}#high", r.id),
                &crate::localization::message(
                    "range",
                    "{control} maximum",
                    1,
                    &crate::hmap! { "control" => r.label.clone() },
                ),
                "increment",
            );
            hi.range(r.low, r.max, r.high);
            hi.focusable = false;
            g.children = vec![lo, hi];
            g.focusable = true;
            Some(g)
        }
        Numeric::Knob(d) => {
            let mut node = AccessNode::control(Role::Slider, &d.id, &d.label, "increment");
            node.actions.push("decrement".to_string());
            node.range(d.min, d.max, d.value);
            Some(node)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{format_number, parse_number, snap};
    use crate::geometry::{Constraints, Size};

    use crate::style::{TextRole, Theme};
    use crate::widgets::Widget;
    use crate::widgets::cx::LayoutCx;
    use crate::widgets::state::StateStore;

    #[test]
    fn a_spin_box_has_room_for_its_longest_value_at_every_text_size() {
        for text_scale in [1.0, 1.5, 2.0] {
            let theme = Theme {
                text_scale,
                ..Theme::default()
            };
            let mut cx = LayoutCx::new(&theme, &StateStore::new(), Size::new(800, 600));
            let node = Widget::spin_box("hue", 5.0, 0.0, 360.0, 1.0, 0)
                .layout(Constraints::loose(Size::new(800, 600)), &mut cx);
            let (text, _) = crate::text::layout::measure("360", &theme.font(TextRole::Body));
            assert!(
                node.parts[0].width as f64 >= text,
                "{} < {} at text size {}",
                node.parts[0].width,
                text,
                text_scale
            )
        }
    }

    #[test]
    fn snapping_clamps_and_steps() {
        assert_eq!(snap(12.4, 0.0, 10.0, 1.0), 10.0);
        assert_eq!(snap(3.26, 0.0, 10.0, 0.5), 3.5);
        assert_eq!(snap(-4.0, 0.0, 10.0, 0.0), 0.0)
    }

    #[test]
    fn numbers_format_with_fixed_decimals() {
        assert_eq!(format_number(1.23456, 2), "1.23");
        assert_eq!(format_number(-0.5, 1), "-0.5");
        assert_eq!(format_number(2.0, 0), "2");
        assert_eq!(format_number(1.005, 1), "1.0")
    }

    #[test]
    fn partial_and_invalid_text_are_told_apart() {
        assert_eq!(parse_number("-", ""), super::Parsed::Partial);
        assert_eq!(parse_number("1.", ""), super::Parsed::Partial);
        assert_eq!(parse_number("2,5", ""), super::Parsed::Number(2.5));
        assert_eq!(parse_number("40 px", "px"), super::Parsed::Number(40.0));
        assert!(matches!(parse_number("4x", ""), super::Parsed::Invalid(_)))
    }
}

pub fn accessible(n: &Numeric, action: &str, text: &str, cx: &mut EventCx) {
    let (value, min, max, step) = match n {
        Numeric::Spin(s) => (s.value, s.min, s.max, s.step),
        Numeric::Slide(s) => (s.value, s.min, s.max, s.step),
        Numeric::Knob(s) => (s.value, s.min, s.max, s.step),
        Numeric::Range(s) => (s.low, s.min, s.high, s.step),
    };
    let proposed = match action {
        "set-value" => text.to_f64().unwrap_or(value),
        "increment" => value + step,
        "decrement" => value - step,
        _ => value,
    };
    if !(proposed >= min && proposed <= max) {
        return;
    };
    let next = snap(proposed, min, max, step);
    if next == value {
        return;
    };
    if let Numeric::Range(s) = n {
        cx.emit(&UiMsg::Range(s.id.clone(), next, s.high))
    } else {
        cx.emit(&UiMsg::Value(n.id(), next))
    }
}

#[cfg(test)]
mod session_tests {
    use crate::strext::StrExt;
    use crate::widgets::Widget;
    use crate::widgets::cx::UiMsg;
    use windowing::{Key, Mods};

    #[derive(Debug, Clone, Default, PartialEq)]
    struct Model {
        count: f64,
        low: f64,
        high: f64,
        angle: f64,
        log: Vec<String>,
    }

    fn update(m: Model, msg: UiMsg) -> Model {
        let mut n = m;
        match msg {
            UiMsg::Value(id, v) => {
                n.log.push(format!("{}={}", id, v));
                if id == "count" {
                    n.count = v
                } else {
                    n.angle = v
                }
            }
            UiMsg::Range(id, lo, hi) => {
                n.log.push(format!("{}={}..{}", id, lo, hi));
                n.low = lo;
                n.high = hi;
            }
            _ => (),
        }
        n
    }

    fn view(m: &Model) -> Widget {
        Widget::column(
            &[
                Widget::spin_box("count", m.count, 0.0, 10.0, 1.0, 0),
                Widget::range_slider("span", m.low, m.high, 0.0, 100.0, 10.0).width(300),
                Widget::dial("angle", m.angle, 0.0, 90.0, 15.0),
                Widget::label(&format!("log {}", m.log.join("; "))),
            ],
            8,
        )
        .padding(8)
    }

    fn log(s: &crate::Session) -> String {
        let dump = s.semantics(0).dump();
        let at = dump.find("log ").unwrap_or(0);
        let rest = dump.substring((at + 4) as i64, dump.byte_len());
        rest.substring(
            0,
            (rest.find("\"").unwrap_or(rest.byte_len() as usize)) as i64,
        )
        .to_string()
    }

    fn press(s: &mut crate::Session, key: Key) {
        s.press(key, Mods::default()).unwrap()
    }

    #[test]
    fn values_at_a_bound_or_unchanged_report_nothing() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Numbers",
            "dev.gossamer.gui-numeric-test",
            || Model {
                count: 9.0,
                low: 20.0,
                high: 80.0,
                angle: 75.0,
                ..Model::default()
            },
            update,
            view,
        )
        .size(420, 420)
        .start()
        .unwrap();
        assert!(s.focus("count").unwrap());
        press(&mut s, Key::Up);
        press(&mut s, Key::Up);
        press(&mut s, Key::PageUp);
        assert!(s.focus("span").unwrap());
        press(&mut s, Key::Left);
        press(&mut s, Key::Left);
        press(&mut s, Key::Left);
        assert!(s.focus("angle").unwrap());
        press(&mut s, Key::Right);
        press(&mut s, Key::Right);
        assert_eq!(log(&s), "count=10; span=10..80; span=0..80; angle=90");
        s.shutdown()
    }

    #[test]
    fn dragging_a_range_thumb_reports_each_new_value_once() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Numbers",
            "dev.gossamer.gui-numeric-test",
            || Model {
                low: 20.0,
                high: 80.0,
                ..Model::default()
            },
            update,
            view,
        )
        .size(420, 420)
        .start()
        .unwrap();
        let r = s.rect_of("span").unwrap();
        let y = r.center_y();
        // Grab the high thumb and move a pixel at a time: values change in
        // steps of 10, each reported once.
        let start = r.x + r.width * 8 / 10;
        s.press_pointer(start, y).unwrap();
        let mut x = start;
        while x < r.x + r.width {
            x += 1;
            s.move_pointer(x, y, true).unwrap()
        }
        s.release_pointer(x, y).unwrap();
        let logged = log(&s);
        let events = logged.split("; ").filter(|e| !e.is_empty());
        let mut seen: Vec<String> = vec![];
        for e in events {
            assert!(
                !seen.iter().any(|x| x == e),
                "{} reported twice in {}",
                e,
                log(&s)
            );
            seen.push(e.to_string())
        }
        assert!(log(&s).ends_with("span=20..100"), "{}", log(&s));
        s.shutdown()
    }

    #[test]
    fn shift_tab_moves_a_range_back_to_its_low_thumb() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Numbers",
            "dev.gossamer.gui-numeric-test",
            || Model {
                low: 20.0,
                high: 80.0,
                ..Model::default()
            },
            update,
            view,
        )
        .size(420, 420)
        .start()
        .unwrap();
        assert!(s.focus("span").unwrap());
        press(&mut s, Key::Tab);
        press(&mut s, Key::Right);
        s.press(
            Key::Tab,
            Mods {
                shift: true,
                ..Mods::default()
            },
        )
        .unwrap();
        assert_eq!(s.focused(), "span");
        press(&mut s, Key::Right);
        assert_eq!(log(&s), "span=20..90; span=30..90");
        s.shutdown()
    }
}
