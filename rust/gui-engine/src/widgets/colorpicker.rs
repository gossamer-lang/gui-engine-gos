// Shared color draft for a reusable picker and dialog. CSS edits retain invalid
// or incomplete text without changing the last valid color. Values are sRGB.
use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Rect, Size};
use crate::memory::ByteCache;
use crate::strext::StrExt;
use crate::style::color::{Color, CssColor};
use crate::widgets::Widget;
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx, UiMsg};
use crate::widgets::state::{StateStore, WState};
use parking_lot::Mutex;
use std::sync::LazyLock;
use windowing::{Key, MouseButton};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColorPicker {
    pub id: String,
    pub original: Color,
    pub value: Color,
    pub draft: String,
    pub hue: f64,
    pub alpha: bool,
    pub error: String,
}
impl ColorPicker {
    pub fn new(id: &str, value: Color) -> ColorPicker {
        let (h, _, _) = value.hsv_parts();
        ColorPicker {
            id: id.to_string(),
            original: value,
            value,
            draft: value.css(),
            hue: h,
            alpha: true,
            error: "".to_string(),
        }
    }
    /// Consumes child messages. Returns a changed valid color once; previews
    /// remain drafts until the caller explicitly commits or restores original.
    pub fn update(&mut self, msg: &UiMsg) -> Option<Color> {
        let before = self.value;
        let prefix = format!("{}:", self.id);
        let id = crate::app::msg_id(msg);
        if !id.starts_with(&prefix) {
            return None;
        };
        let part = id.substring(prefix.byte_len(), id.byte_len());
        let mut color = self.value;
        match msg {
            UiMsg::Text(_, text) if part == "css" => {
                self.draft = text.to_string();
                match crate::style::color::parse(text) {
                    CssColor::Valid(c) => color = c,
                    CssColor::Intermediate => {
                        self.error = crate::localization::tr("Complete the CSS color");
                        return None;
                    }
                    CssColor::Invalid(why) | CssColor::Unsupported(why) => {
                        self.error = why;
                        return None;
                    }
                }
            }
            UiMsg::Value(_, v) => {
                let (_, s, val) = color.hsv_parts();
                let (_, hs, l) = color.hsl_parts();
                match part {
                    "r" => color.r = v.clamp(0.0, 255.0).round() as u8,
                    "g" => color.g = v.clamp(0.0, 255.0).round() as u8,
                    "b" => color.b = v.clamp(0.0, 255.0).round() as u8,
                    "a" => color.a = v.clamp(0.0, 255.0).round() as u8,
                    "h" => {
                        self.hue = v.clamp(0.0, 360.0) % 360.0;
                        color = Color::hsv(self.hue, s, val).with_alpha(color.a as i64)
                    }
                    "s" => color = Color::hsv(self.hue, v / 100.0, val).with_alpha(color.a as i64),
                    "v" => color = Color::hsv(self.hue, s, v / 100.0).with_alpha(color.a as i64),
                    "hs" => color = Color::hsl(self.hue, v / 100.0, l).with_alpha(color.a as i64),
                    "l" => color = Color::hsl(self.hue, hs, v / 100.0).with_alpha(color.a as i64),
                    _ => return None,
                }
            }
            UiMsg::Custom(_, values) if part == "wheel" => {
                let p = values.split("|").collect::<Vec<_>>();
                if p.len() != 3 {
                    return None;
                };
                let h = p[0].to_f64()?;
                let s = p[1].to_f64()?;
                let v = p[2].to_f64()?;
                self.hue = h;
                color = Color::hsv(h, s, v).with_alpha(color.a as i64);
            }
            UiMsg::Click(_) if part == "original" => color = self.original,
            _ => return None,
        }
        if !self.alpha {
            color.a = 255
        };
        let (h, s, _) = color.hsv_parts();
        if s > 0.00001 {
            self.hue = h
        };
        self.value = color;
        self.draft = color.css();
        self.error = "".to_string();
        if before == color { None } else { Some(color) }
    }
    pub fn view(&self) -> Widget {
        let id = &self.id;
        let (_, s, v) = self.value.hsv_parts();
        let (_, hs, l) = self.value.hsl_parts();
        // Each channel is its label over a spin box; a model's channels wrap
        // to the next line rather than narrow when the picker is narrow or
        // the text large.
        let row = |part: String, title: String, value: f64, max: f64| -> Widget {
            Widget::column(
                &[
                    Widget::paragraph(&title).key(&format!("{}:{}:label", id, part)),
                    Widget::spin_box(&format!("{}:{}", id, part), value, 0.0, max, 1.0, 0)
                        .accessible(&title),
                ],
                4,
            )
        };
        // One row per color model: RGB(A), HSV, then HSL.
        let mut rgb = vec![
            row(
                "r".to_string(),
                crate::localization::tr("Red"),
                self.value.r as f64,
                255.0,
            ),
            row(
                "g".to_string(),
                crate::localization::tr("Green"),
                self.value.g as f64,
                255.0,
            ),
            row(
                "b".to_string(),
                crate::localization::tr("Blue"),
                self.value.b as f64,
                255.0,
            ),
        ];
        if self.alpha {
            rgb.push(row(
                "a".to_string(),
                crate::localization::tr("Alpha"),
                self.value.a as f64,
                255.0,
            ))
        };
        let mut channels = vec![Widget::flow(&rgb, 16)];
        channels.push(Widget::flow(
            &[
                row(
                    "h".to_string(),
                    crate::localization::tr("Hue"),
                    self.hue,
                    360.0,
                ),
                row(
                    "s".to_string(),
                    crate::localization::tr("Saturation"),
                    s * 100.0,
                    100.0,
                ),
                row(
                    "v".to_string(),
                    crate::localization::tr("Value"),
                    v * 100.0,
                    100.0,
                ),
            ],
            16,
        ));
        channels.push(Widget::flow(
            &[
                row(
                    "hs".to_string(),
                    crate::localization::tr("HSL saturation"),
                    hs * 100.0,
                    100.0,
                ),
                row(
                    "l".to_string(),
                    crate::localization::tr("Lightness"),
                    l * 100.0,
                    100.0,
                ),
            ],
            16,
        ));
        let mut css = crate::widgets::textinput::TextInput::new(
            &format!("{}:css", id),
            &self.draft,
            &crate::localization::tr("CSS color"),
            crate::widgets::textinput::InputKind::Line,
        );
        css.label = crate::localization::tr("CSS color");
        css.error = self.error.clone();
        let original = self.original;
        let current = self.value;
        Widget::column(
            &[
                Widget::row(
                    &[
                        Widget::ColorWheel(ColorWheel {
                            id: format!("{}:wheel", id),
                            value: current,
                            hue: self.hue,
                        }),
                        Widget::column(&channels, 12).grow(1),
                    ],
                    16,
                ),
                Widget::flow(
                    &[
                        Widget::column(
                            &[
                                Widget::paragraph(&crate::localization::tr("CSS color"))
                                    .key(&format!("{}:css:label", id)),
                                Widget::input(&css),
                            ],
                            4,
                        )
                        .width(240),
                        Widget::column(
                            &[
                                Widget::paragraph(&crate::localization::tr("Original and new")),
                                Widget::flex(
                                    crate::widgets::Axis::Horizontal,
                                    &[
                                        checkerboard(original).width(64),
                                        checkerboard(current).width(64),
                                        Widget::spacer(8),
                                        Widget::button(
                                            &format!("{}:original", id),
                                            &crate::localization::tr("Revert"),
                                        )
                                        .disabled(current == original),
                                    ],
                                    0,
                                    crate::widgets::MainAlign::Start,
                                    crate::widgets::CrossAlign::Center,
                                ),
                            ],
                            4,
                        ),
                    ],
                    16,
                ),
            ],
            16,
        )
        .key(id)
        .role(Role::ColorWell)
        .accessible(&crate::localization::message(
            "color",
            "Color {color}",
            1,
            &crate::hmap! { "color" => current.css() },
        ))
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColorWheel {
    pub id: String,
    pub value: Color,
    pub hue: f64,
}

// Rendered rings by size and fade color; a handful of sizes are live at once.
static RINGS: LazyLock<Mutex<ByteCache<String, Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(ByteCache::new(4 * 1024 * 1024)));

pub(crate) fn purge_cache() {
    RINGS.lock().clear();
}

// The hue ring as `side` x `side` RGBA pixels: hue follows the angle from
// the positive x axis, clockwise on screen as `wheel_event` reads it, and
// both edges are antialiased by their pixel coverage.
fn hue_ring(side: i64, fade: Option<Color>) -> Vec<u8> {
    let key = match fade {
        Some(c) => format!("{}:{}", side, c.css()),
        None => format!("{}", side),
    };
    if let Some(cached) = RINGS.lock().get(&key) {
        return cached.to_vec();
    }
    let mid = side as f64 / 2.0;
    let outer = side as f64 * 0.47;
    let inner = outer * 0.78;
    let mut px = vec![0u8; (side * side * 4) as usize];
    for y in 0..side {
        for x in 0..side {
            let dx = x as f64 + 0.5 - mid;
            let dy = y as f64 + 0.5 - mid;
            let d = (dx * dx + dy * dy).sqrt();
            let cover = (outer - d + 0.5).clamp(0.0, 1.0) * (d - inner + 0.5).clamp(0.0, 1.0);
            if cover <= 0.0 {
                continue;
            };
            let hue = (dy.atan2(dx).to_degrees() + 360.0) % 360.0;
            let base = Color::hsv(hue, 1.0, 1.0);
            let c = match fade {
                Some(f) => base.mix(f, 0.65),
                None => base,
            };
            let at = (y * side + x) * 4;
            px[at as usize] = c.r;
            px[(at + 1) as usize] = c.g;
            px[(at + 2) as usize] = c.b;
            px[(at + 3) as usize] = (cover * 255.0 + 0.5) as u8
        }
    }
    let bytes = key.capacity() + px.capacity();
    RINGS.lock().insert(key, px.clone(), bytes);
    px
}
pub fn layout_wheel(c: Constraints, cx: &mut LayoutCx) -> Node {
    Node::leaf(c.constrain(Size::new(cx.theme.px(180), cx.theme.px(180))))
}
pub fn paint_wheel(w: ColorWheel, n: &Node, cx: &mut PaintCx) {
    let side = n.size().width.min(n.size().height);
    if side < 12 {
        return;
    };
    let origin = cx.abs(Rect::new(0, 0, 0, 0));
    let fade = if cx.disabled {
        Some(cx.theme.surface)
    } else {
        None
    };
    cx.canvas
        .image(origin.x, origin.y, side, side, &hue_ring(side, fade));
    let start = side / 4;
    let width = side / 2;
    for y in 0..32 {
        for x in 0..32 {
            let color = Color::hsv(w.hue, x as f64 / 31.0, 1.0 - y as f64 / 31.0);
            cx.fill(
                Rect::new(
                    start + x * width / 32,
                    start + y * width / 32,
                    (width + 31) / 32,
                    (width + 31) / 32,
                ),
                color,
            )
        }
    }
    let (_, s, v) = w.value.hsv_parts();
    let at = cx.abs(Rect::new(
        start + (s * width as f64) as i64,
        start + ((1.0 - v) * width as f64) as i64,
        0,
        0,
    ));
    cx.canvas
        .circle_stroke(at.x, at.y, cx.theme.px(4), cx.theme.px(1), Color::white());
    if cx.is_focused(&w.id) {
        cx.focus_ring(Rect::new(0, 0, side, side), side / 2)
    }
}
pub fn wheel_event(w: ColorWheel, n: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    if cx.disabled {
        return;
    };
    let side = n.size().width.min(n.size().height).max(1) as f64;
    let mut h = w.hue;
    let (_, mut s, mut v) = w.value.hsv_parts();
    let mut active = st.drag(&w.id);
    match ev {
        Event::CaptureLost | Event::FocusOut => {
            active.active = false;
            st.set(&w.id, WState::Drag(active));
            return;
        }
        Event::PointerUp { .. } => {
            active.active = false;
            st.set(&w.id, WState::Drag(active));
            cx.release_pointer();
            return;
        }
        Event::PointerDown { x, y, button, .. } => {
            if *button != MouseButton::Left {
                return;
            };
            let dx = (x - cx.origin_x) as f64 - side / 2.0;
            let dy = (y - cx.origin_y) as f64 - side / 2.0;
            let radius = (dx * dx + dy * dy).sqrt();
            active.part = if radius > side * 0.35 && radius < side * 0.5 {
                0
            } else if dx.abs() <= side / 4.0 && dy.abs() <= side / 4.0 {
                1
            } else {
                return;
            };
            active.active = true;
            cx.focus_on(&w.id);
            cx.capture_pointer(&w.id);
            if active.part == 0 {
                h = (dy.atan2(dx).to_degrees() + 360.0) % 360.0
            } else {
                s = (dx / (side / 2.0) + 0.5).clamp(0.0, 1.0);
                v = (0.5 - dy / (side / 2.0)).clamp(0.0, 1.0)
            }
        }
        Event::PointerMove { x, y, .. } => {
            if !active.active {
                return;
            };
            let dx = (x - cx.origin_x) as f64 - side / 2.0;
            let dy = (y - cx.origin_y) as f64 - side / 2.0;
            if active.part == 0 {
                h = (dy.atan2(dx).to_degrees() + 360.0) % 360.0
            } else {
                s = (dx / (side / 2.0) + 0.5).clamp(0.0, 1.0);
                v = (0.5 - dy / (side / 2.0)).clamp(0.0, 1.0)
            }
        }
        Event::KeyDown { key, mods, .. } => {
            let step = if mods.shift { 10.0 } else { 1.0 };
            match key {
                Key::Left => h = (h - step + 360.0) % 360.0,
                Key::Right => h = (h + step) % 360.0,
                Key::Up => v = (v + step / 100.0).min(1.0),
                Key::Down => v = (v - step / 100.0).max(0.0),
                Key::PageUp => s = (s + step / 100.0).min(1.0),
                Key::PageDown => s = (s - step / 100.0).max(0.0),
                Key::Home => h = 0.0,
                Key::End => h = 359.0,
                _ => return,
            }
        }
        _ => return,
    }
    st.set(&w.id, WState::Drag(active));
    cx.emit(&UiMsg::Custom(w.id, format!("{}|{}|{}", h, s, v)));
    cx.redraw = true;
    cx.consume()
}
pub fn wheel_semantics(w: ColorWheel) -> Option<AccessNode> {
    let mut n = AccessNode::control(
        Role::Slider,
        &w.id,
        &crate::localization::tr("Hue wheel and saturation/value"),
        "increment",
    );
    n.actions.push("decrement".to_string());
    n.value = w.value.css();
    n.description =
        crate::localization::tr("Left/right: hue; up/down: value; Page Up/Down: saturation");
    n.range(0.0, 360.0, w.hue);
    Some(n)
}

/// The preview composites sRGB alpha over theme-colored checker squares.
pub fn checkerboard(color: Color) -> Widget {
    Widget::Custom(crate::widgets::Custom {
        id: "".to_string(),
        focusable: false,
        role: Role::Image,
        label: crate::localization::message(
            "color",
            "Transparency preview {color}",
            1,
            &crate::hmap! { "color" => color.css() },
        ),
        measure: crate::Callback::new(std::rc::Rc::new(move |c, t| {
            c.constrain(Size::new(t.px(120), t.px(32)))
        })),
        draw: crate::Callback::new(std::rc::Rc::new(move |size, theme| {
            let cell = theme.px(8).max(1);
            let mut ops = vec![];
            for y in 0..((size.height + cell - 1) / cell) {
                for x in 0..((size.width + cell - 1) / cell) {
                    let background = if (x + y) % 2 == 0 {
                        theme.surface
                    } else {
                        theme.border
                    };
                    ops.push(crate::render::display::DrawOp::FillRect {
                        rect: Rect::new(
                            x * cell,
                            y * cell,
                            cell.min(size.width - x * cell),
                            cell.min(size.height - y * cell),
                        ),
                        color: color.over(background),
                    })
                }
            }
            ops
        })),
        input: crate::Callback::new(std::rc::Rc::new(move |_| vec![])),
        captures_input: false,
    })
}

#[cfg(test)]
mod tests {
    use super::ColorPicker;

    use crate::style::color::Color;
    use crate::widgets::cx::UiMsg;
    #[test]
    fn color_labels_are_above_inputs_at_large_text_and_narrow_widths() {
        for width in [320, 480, 760] {
            for scale in [1.0, 1.5] {
                for text_scale in [1.0, 2.0] {
                    let theme = crate::style::Theme {
                        scale,
                        text_scale,
                        rtl: width == 320,
                        ..crate::style::Theme::default()
                    };
                    let picker = ColorPicker::new("c", Color::red());
                    let widget = picker.view();
                    let mut cx = crate::widgets::cx::LayoutCx::new(
                        &theme,
                        &crate::widgets::state::StateStore::new(),
                        crate::geometry::Size::new(width, 4000),
                    );
                    let node = widget.layout(
                        crate::geometry::Constraints::new(width, width, 0, 4000),
                        &mut cx,
                    );
                    for part in vec!["r", "g", "b", "a", "h", "s", "v", "hs", "l", "css"] {
                        let label = crate::widgets::tree::path_to(
                            &widget,
                            &node,
                            &theme,
                            &format!("c:{}:label", part),
                        );
                        let input = crate::widgets::tree::path_to(
                            &widget,
                            &node,
                            &theme,
                            &format!("c:{}", part),
                        );
                        assert!(!label.is_empty() && !input.is_empty());
                        let a = label[label.len() - 1].rect();
                        let b = input[input.len() - 1].rect();
                        assert!(a.bottom() <= b.y)
                    }
                }
            }
        }
    }
    #[test]
    fn css_drafts_alpha_hue_and_no_duplicate_preview() {
        let mut picker = ColorPicker::new("c", Color::red());
        assert_eq!(
            picker.update(&UiMsg::Text("c:css".to_string(), "#12".to_string())),
            None
        );
        assert_eq!(picker.value, Color::red());
        assert!(!picker.error.is_empty());
        assert_eq!(
            picker.update(&UiMsg::Text(
                "c:css".to_string(),
                "rgba(0, 0, 255, 0.5)".to_string()
            )),
            Some(Color::rgba(0, 0, 255, 128))
        );
        assert_eq!(
            picker.update(&UiMsg::Text("c:css".to_string(), "#0000ff80".to_string())),
            None
        );
        picker.update(&UiMsg::Value("c:v".to_string(), 0.0));
        picker.update(&UiMsg::Value("c:h".to_string(), 120.0));
        picker.update(&UiMsg::Value("c:v".to_string(), 100.0));
        assert_eq!(picker.hue, 120.0);
        picker.alpha = false;
        picker.update(&UiMsg::Text("c:css".to_string(), "transparent".to_string()));
        assert_eq!(picker.value.a, 255);
        assert_eq!(picker.original, Color::red())
    }
}
