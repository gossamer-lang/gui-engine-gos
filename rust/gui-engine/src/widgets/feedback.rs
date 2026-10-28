// feedback: progress bars (determinate and indeterminate), spinners,
// status indicators, inline messages and banners, and toasts. Motion is
// frame-driven only while an animated control is laid out and the theme
// does not ask for reduced motion; otherwise indeterminate states draw a
// still pattern and the session schedules no frames.
use crate::strext::StrExt;
use windowing::{Key, MouseButton};

use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Rect, Size};
use crate::localization::tr;
use crate::render::icons::Icon;
use crate::style::TextRole;
use crate::text::layout;
use crate::text::layout::LayoutOptions;
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx, UiMsg};
use crate::widgets::state::StateStore;
use crate::widgets::{Tone, Widget};

/// Severity of a status, message, or banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

fn level_style(l: Level) -> (Icon, Tone, String) {
    match l {
        Level::Info => (
            Icon::Info,
            Tone::Info,
            crate::localization::tr("Information"),
        ),
        Level::Success => (
            Icon::Check,
            Tone::Success,
            crate::localization::tr("Success"),
        ),
        Level::Warning => (
            Icon::Warning,
            Tone::Warning,
            crate::localization::tr("Warning"),
        ),
        Level::Error => (Icon::Error, Tone::Danger, crate::localization::tr("Error")),
    }
}

/// The feedback controls.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Feedback {
    /// `value` 0..1, or negative for indeterminate.
    Progress {
        id: String,
        value: f64,
        label: String,
    },
    Spinner {
        id: String,
        label: String,
    },
    Status {
        level: Level,
        text: String,
    },
    /// An inline message or a banner (full width with actions).
    Message {
        id: String,
        level: Level,
        title: String,
        text: String,
        actions: Vec<(String, String)>,
        dismissible: bool,
        banner: bool,
    },
}

/// A progress bar; negative `value` is indeterminate.
pub fn progress(id: &str, value: f64) -> Widget {
    Widget::Feedback(Feedback::Progress {
        id: id.to_string(),
        value,
        label: String::new(),
    })
}

pub fn spinner(id: &str, label: &str) -> Widget {
    Widget::Feedback(Feedback::Spinner {
        id: id.to_string(),
        label: label.to_string(),
    })
}

/// A colored dot with text; the level is also spoken in words.
pub fn status(level: Level, text: &str) -> Widget {
    Widget::Feedback(Feedback::Status {
        level,
        text: text.to_string(),
    })
}

/// An inline message, e.g. under a form.
pub fn message(id: &str, level: Level, text: &str) -> Widget {
    Widget::Feedback(Feedback::Message {
        id: id.to_string(),
        level,
        title: String::new(),
        text: text.to_string(),
        actions: vec![],
        dismissible: false,
        banner: false,
    })
}

/// A full-width banner with actions (`Click(id:action)`) and an optional
/// dismiss button (`Click(id:dismiss)`).
pub fn banner(
    id: &str,
    level: Level,
    title: &str,
    text: &str,
    actions: &[(String, String)],
    dismissible: bool,
) -> Widget {
    Widget::Feedback(Feedback::Message {
        id: id.to_string(),
        level,
        title: title.to_string(),
        text: text.to_string(),
        actions: actions.to_vec(),
        dismissible,
        banner: true,
    })
}

/// How one `Toast` looks in `toast_layer`: a small banner with its action
/// and Dismiss. It appears and leaves without motion.
pub fn toast(id: &str, text: &str, action: (String, String)) -> Widget {
    let mut actions = vec![];
    if !action.0.is_empty() {
        actions.push(action)
    };
    banner(id, Level::Info, "", text, &actions, true).padding(4)
}

/// A short message over the window's bottom edge, opened with
/// `Effect::Toast`. It closes after `duration` ms (the time pauses while the
/// pointer or focus is on it), reporting `Click(id:timeout)`, or through its
/// action (`Click(id:action)`) or Dismiss (`Click(id:dismiss)`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Toast {
    pub id: String,
    pub text: String,
    /// (key, label), or empty keys for none.
    pub action: (String, String),
    pub duration: i64,
}

impl Toast {
    pub fn new(id: &str, text: &str) -> Toast {
        Toast {
            id: id.to_string(),
            text: text.to_string(),
            action: (String::new(), String::new()),
            duration: 5000,
        }
    }

    /// This toast with an action button.
    pub fn action(self, key: &str, label: &str) -> Toast {
        let mut t = self;
        t.action = (key.to_string(), label.to_string());
        t
    }

    /// This toast, shown for `ms` milliseconds.
    pub fn lasting(self, ms: i64) -> Toast {
        let mut t = self;
        t.duration = ms;
        t
    }
}

/// The overlay the session lays over a window showing `toasts`, newest last.
pub fn toast_layer(toasts: &[Toast]) -> Widget {
    Widget::column(
        &toasts
            .iter()
            .map(|t| toast(&t.id, &t.text, t.action.clone()).max_width(560))
            .collect::<Vec<_>>(),
        8,
    )
    .padding(16)
    .align(0.5, 1.0)
    .pass_through()
}

impl Feedback {
    pub fn id(&self) -> String {
        match self {
            Feedback::Progress { id, .. }
            | Feedback::Spinner { id, .. }
            | Feedback::Message { id, .. } => id.clone(),
            _ => String::new(),
        }
    }

    pub fn focusable(&self) -> bool {
        match self {
            Feedback::Message {
                actions,
                dismissible,
                ..
            } => !actions.is_empty() || *dismissible,
            _ => false,
        }
    }
}

// Message geometry: icon, text block, action buttons (local rects).
fn message_parts(f: &Feedback, width: i64, theme: &crate::style::Theme) -> (Vec<Rect>, i64) {
    let Feedback::Message {
        title,
        text,
        actions,
        dismissible,
        ..
    } = f
    else {
        return (vec![], 0);
    };
    let pad = theme.px(12);
    let icon = theme.px(22);
    let mut btn_w = 0;
    let mut buttons = vec![];
    for (_, label) in actions {
        let (tw, _) = layout::measure(label, &theme.font(TextRole::Body));
        buttons.push(tw as i64 + theme.px(24));
        btn_w += tw as i64 + theme.px(32);
    }
    if *dismissible {
        btn_w += theme.px(40)
    };
    let text_w = (width - pad * 3 - icon - btn_w).max(theme.px(80));
    let body = if title.is_empty() {
        text.to_string()
    } else {
        format!("{}\n{}", title, text)
    };
    let l = layout::layout(
        &body,
        &theme.font(TextRole::Body),
        LayoutOptions::wrapped(text_w as f64),
    );
    let h = (l.height as i64).max(theme.control_height()) + pad * 2;
    let mut parts = vec![
        Rect::new(pad, pad, icon, icon),
        Rect::new(pad * 2 + icon, pad, text_w, l.height as i64),
    ];
    let mut x = width - pad - btn_w;
    let bh = theme.control_height();
    for w in buttons {
        parts.push(Rect::new(x, (h - bh) / 2, w, bh));
        x += w + theme.px(8);
    }
    if *dismissible {
        parts.push(Rect::new(
            width - pad - theme.px(32),
            (h - theme.px(32)) / 2,
            theme.px(32),
            theme.px(32),
        ))
    }
    if theme.rtl {
        let whole = Rect::new(0, 0, width, h);
        parts = parts.iter().map(|r| r.mirror_in(whole)).collect::<Vec<_>>();
    }
    (parts, h)
}

pub fn layout_feedback(f: &Feedback, c: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = cx.theme.clone();
    match f {
        Feedback::Progress { id, value, label } => {
            if *value < 0.0 && !theme.reduced_motion {
                cx.animate(id)
            };
            let w = if c.has_bounded_width() {
                c.max_width
            } else {
                theme.px(240)
            };
            let label_h = if label.is_empty() { 0 } else { theme.px(24) };
            Node::leaf(c.constrain(Size::new(w, theme.px(8) + label_h)))
        }
        Feedback::Spinner { id, label } => {
            if !theme.reduced_motion {
                cx.animate(id)
            };
            let s = theme.px(24);
            let (tw, _) = layout::measure(label, &theme.font(TextRole::Body));
            Node::leaf(c.constrain(Size::new(
                s + if label.is_empty() {
                    0
                } else {
                    theme.px(8) + tw as i64
                },
                s.max(theme.px(28)),
            )))
        }
        Feedback::Status { text, .. } => {
            let (tw, th) = layout::measure(text, &theme.font(TextRole::Body));
            Node::leaf(c.constrain(Size::new(
                theme.px(18) + tw as i64,
                (th as i64).max(theme.px(24)),
            )))
        }
        Feedback::Message { .. } => {
            let w = if c.has_bounded_width() {
                c.max_width
            } else {
                theme.px(480)
            };
            let (parts, h) = message_parts(f, w, &theme);
            let mut n = Node::leaf(c.constrain(Size::new(w, h)));
            n.parts = parts;
            n
        }
    }
}

pub fn paint_feedback(f: &Feedback, node: &Node, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let r = Rect::from_size(node.size());
    match f {
        Feedback::Progress { value, label, .. } => {
            let bar_h = theme.px(8);
            let track = Rect::new(0, r.height - bar_h, r.width, bar_h);
            cx.fill_round(track, bar_h / 2, theme.border);
            let accent = if cx.disabled {
                theme.text_disabled
            } else {
                theme.accent
            };
            if *value >= 0.0 {
                let w = (r.width as f64 * value.min(1.0)) as i64;
                if w > 0 {
                    cx.fill_round(Rect::new(0, track.y, w, bar_h), bar_h / 2, accent)
                }
            } else {
                // A moving segment, or a still half-bar under reduced motion.
                let seg = r.width / 3;
                let phase = if theme.reduced_motion {
                    0
                } else {
                    (cx.now / 8) % (r.width + seg).max(1)
                };
                let x0 = (phase - seg).max(0);
                let x1 = phase.min(r.width);
                let (a, b) = if theme.reduced_motion {
                    (0, r.width / 2)
                } else {
                    (x0, x1)
                };
                if b > a {
                    cx.fill_round(Rect::new(a, track.y, b - a, bar_h), bar_h / 2, accent)
                }
            }
            if !label.is_empty() {
                let style = theme.font(TextRole::Small);
                let l = layout::layout(label, &style, LayoutOptions::single_line());
                let a = cx.abs(r);
                layout::draw(
                    &mut cx.canvas,
                    &l,
                    a.x as f64,
                    a.y as f64,
                    &style,
                    theme.text_muted,
                )
            }
        }
        Feedback::Spinner { label, .. } => {
            let s = theme.px(24);
            let a = cx.abs(Rect::new(0, (r.height - s) / 2, s, s));
            let cx0 = a.x + s / 2;
            let cy0 = a.y + s / 2;
            let rad = (s / 2 - theme.px(3)) as f64;
            let spin = if theme.reduced_motion {
                0.0
            } else {
                (cx.now % 1000) as f64 / 1000.0 * std::f64::consts::TAU
            };
            let mut k = 0;
            while k < 8 {
                let ang = spin + k as f64 * std::f64::consts::FRAC_PI_4;
                let alpha = 40 + k * 26;
                let px = cx0 + (ang.cos() * rad) as i64;
                let py = cy0 + (ang.sin() * rad) as i64;
                cx.canvas
                    .circle(px, py, theme.px(2).max(1), theme.accent.with_alpha(alpha));
                k += 1;
            }
            if !label.is_empty() {
                let style = theme.font(TextRole::Body);
                let l = layout::layout(label, &style, LayoutOptions::single_line());
                let base = cx.abs(r);
                layout::draw(
                    &mut cx.canvas,
                    &l,
                    (base.x + s + theme.px(8)) as f64,
                    (base.y + (r.height - l.height as i64) / 2) as f64,
                    &style,
                    theme.text,
                )
            }
        }
        Feedback::Status { level, text } => {
            let (_, tone, _) = level_style(*level);
            let a = cx.abs(r);
            let d = theme.px(10);
            cx.canvas.circle(
                a.x + d / 2 + 1,
                a.y + r.height / 2,
                d / 2,
                tone.resolve(&theme),
            );
            let style = theme.font(TextRole::Body);
            let l = layout::layout(text, &style, LayoutOptions::single_line());
            layout::draw(
                &mut cx.canvas,
                &l,
                (a.x + theme.px(18)) as f64,
                (a.y + (r.height - l.height as i64) / 2) as f64,
                &style,
                theme.text,
            )
        }
        Feedback::Message {
            id,
            level,
            title,
            text,
            actions,
            dismissible,
            banner,
        } => {
            let (icon, tone, _) = level_style(*level);
            let color = tone.resolve(&theme);
            let bg = color
                .with_alpha(if theme.is_high_contrast() { 0 } else { 36 })
                .over(theme.background);
            let radius = if *banner { 0 } else { theme.radius() };
            cx.fill_round(r, radius, bg);
            cx.stroke_round(r, radius, theme.border_width(), color);
            if node.parts.len() >= 2 {
                let ir = cx.abs(node.parts[0]);
                cx.icon(&icon, ir.x, ir.y, ir.width, color);
                let tr = cx.abs(node.parts[1]);
                let style = theme.font(TextRole::Body);
                let body = if title.is_empty() {
                    text.to_string()
                } else {
                    format!("{}\n{}", title, text)
                };
                let l = layout::layout(&body, &style, LayoutOptions::wrapped(tr.width as f64));
                layout::draw(
                    &mut cx.canvas,
                    &l,
                    tr.x as f64,
                    tr.y as f64,
                    &style,
                    theme.text,
                )
            }
            let current = cx.states.field(id, "key-action", "0").to_i64().unwrap_or(0);
            let mut i = 0;
            while i < actions.len() && 2 + i < node.parts.len() {
                let br = node.parts[2 + i];
                let hover =
                    cx.is_hover(id) && cx.states.field(id, "hover-action", "") == format!("{}", i);
                let fill = if hover { theme.hover } else { theme.surface };
                cx.fill_round(br, theme.radius(), fill);
                cx.stroke_round(br, theme.radius(), theme.border_width(), theme.border);
                let style = theme.font(TextRole::Body);
                let l = layout::layout(&actions[i].1, &style, LayoutOptions::single_line());
                let a = cx.abs(br);
                layout::draw(
                    &mut cx.canvas,
                    &l,
                    (a.x + (a.width - l.width as i64) / 2) as f64,
                    (a.y + (a.height - l.height as i64) / 2) as f64,
                    &style,
                    theme.text,
                );
                i += 1;
            }
            if *dismissible && !node.parts.is_empty() {
                let dr = cx.abs(node.parts[node.parts.len() - 1]);
                cx.icon(
                    &Icon::Close,
                    dr.x + theme.px(6),
                    dr.y + theme.px(6),
                    dr.width - theme.px(12),
                    theme.text_muted,
                )
            }
            if cx.is_focused(id) {
                // The ring marks the action Enter would run.
                let at = 2 + current;
                if at < node.parts.len() as i64 {
                    cx.focus_ring(node.parts[at as usize], theme.radius())
                } else {
                    cx.focus_ring(r, theme.radius())
                }
            }
        }
    }
}

// The message's buttons in order: its actions, then Dismiss when it has one.
// Each reports `Click(id:action)`.
fn targets(id: &str, actions: &[(String, String)], dismissible: bool) -> Vec<String> {
    let mut out = actions
        .iter()
        .map(|a| format!("{}:{}", id, a.0))
        .collect::<Vec<_>>();
    if dismissible {
        out.push(format!("{}:dismiss", id))
    };
    out
}

// The button under local (`x`, `y`), or -1; buttons follow the icon and text parts.
fn button_at(node: &Node, count: i64, x: i64, y: i64) -> i64 {
    let mut i = 0;
    while i < count && 2 + i < node.parts.len() as i64 {
        if node.parts[(2 + i) as usize].contains(x, y) {
            return i;
        };
        i += 1;
    }
    -1
}

pub fn feedback_event(
    f: &Feedback,
    node: &Node,
    ev: &Event,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    let Feedback::Message {
        id,
        actions,
        dismissible,
        ..
    } = f
    else {
        return;
    };
    let buttons = targets(id, actions, *dismissible);
    let n = buttons.len();
    let current = st
        .field(id, "key-action", "0")
        .to_i64()
        .unwrap_or(0)
        .min((n - 1) as i64)
        .max(0);
    match ev {
        // A button runs on a release over the button it was pressed on.
        Event::PointerDown { x, y, button, .. } => {
            let i = button_at(node, n as i64, x - cx.origin_x, y - cx.origin_y);
            if *button == MouseButton::Left && i >= 0 {
                st.set_field(id, "ppress", &format!("{}", i));
                cx.capture_pointer(id);
                cx.consume()
            }
        }
        Event::PointerUp { x, y, button, .. } => {
            let held = st.field(id, "ppress", "");
            if *button == MouseButton::Left && !held.is_empty() {
                st.set_field(id, "ppress", "");
                cx.release_pointer();
                let i = button_at(node, n as i64, x - cx.origin_x, y - cx.origin_y);
                if i >= 0 && held == format!("{}", i) {
                    cx.emit(&UiMsg::Click(buttons[i as usize].clone()))
                };
                cx.consume()
            }
        }
        Event::CaptureLost => st.set_field(id, "ppress", ""),
        Event::FocusIn => st.set_field(id, "key-action", "0"),
        Event::PointerMove { x, y, .. } => {
            let lx = x - cx.origin_x;
            let ly = y - cx.origin_y;
            let mut hovered = String::new();
            let mut i = 0;
            while i < actions.len() && 2 + i < node.parts.len() {
                if node.parts[2 + i].contains(lx, ly) {
                    hovered = format!("{}", i)
                };
                i += 1;
            }
            if st.field(id, "hover-action", "") != hovered {
                st.set_field(id, "hover-action", &hovered);
                cx.redraw = true;
            }
        }
        Event::PointerLeave if !st.field(id, "hover-action", "").is_empty() => {
            st.set_field(id, "hover-action", "");
            cx.redraw = true;
        }
        // The arrows move between the buttons (Left and Right swap in
        // right-to-left layouts); Enter and Space run the current one.
        Event::KeyDown { key, .. } => {
            let next = if cx.theme.rtl { Key::Left } else { Key::Right };
            let back = if cx.theme.rtl { Key::Right } else { Key::Left };
            if n == 0 {
                return;
            }
            if *key == next || *key == back {
                let step = if *key == next { 1 } else { n - 1 };
                st.set_field(
                    id,
                    "key-action",
                    &format!("{}", (current + step as i64) % n as i64),
                );
                cx.redraw = true;
                cx.consume()
            } else if *key == Key::Return || *key == Key::KpEnter || *key == Key::Space {
                cx.emit(&UiMsg::Click(buttons[current as usize].clone()));
                cx.consume()
            } else if (*key == Key::Escape || *key == Key::Delete) && *dismissible {
                cx.emit(&UiMsg::Click(format!("{}:dismiss", id)));
                cx.consume()
            }
        }
        _ => (),
    }
}

pub fn feedback_semantics(f: &Feedback) -> Option<AccessNode> {
    match f {
        Feedback::Progress { id, value, label } => {
            let mut n = AccessNode::new(Role::ProgressBar, label);
            n.id = id.to_string();
            if *value >= 0.0 {
                n.range(0.0, 100.0, (value * 100.0).round())
            } else {
                n.value = "busy".to_string()
            };
            Some(n)
        }
        Feedback::Spinner { id, label } => {
            let mut n = AccessNode::new(
                Role::ProgressBar,
                &if label.is_empty() {
                    crate::localization::tr("Working")
                } else {
                    label.to_string()
                },
            );
            n.id = id.to_string();
            n.value = "busy".to_string();
            Some(n)
        }
        Feedback::Status { level, text } => {
            let (_, _, word) = level_style(*level);
            let mut n = AccessNode::new(Role::Status, &format!("{}: {}", word, text));
            n.live = "polite".to_string();
            Some(n)
        }
        Feedback::Message {
            id,
            level,
            title,
            text,
            actions,
            dismissible,
            ..
        } => {
            let (_, _, word) = level_style(*level);
            let role = if *level == Level::Error || *level == Level::Warning {
                Role::Alert
            } else {
                Role::Status
            };
            let mut n = AccessNode::new(
                role,
                &if title.is_empty() {
                    format!("{}: {}", word, text)
                } else {
                    format!("{}: {}. {}", word, title, text)
                },
            );
            n.id = id.to_string();
            n.live = if role == Role::Alert {
                "assertive".to_string()
            } else {
                "polite".to_string()
            };
            n.focusable = !actions.is_empty() || *dismissible;
            for (aid, label) in actions {
                let mut b =
                    AccessNode::control(Role::Button, &format!("{}:{}", id, aid), label, "press");
                b.focusable = false;
                n.children.push(b)
            }
            if *dismissible {
                let mut b = AccessNode::control(
                    Role::Button,
                    &format!("{}:dismiss", id),
                    &tr("Dismiss"),
                    "press",
                );
                b.focusable = false;
                n.children.push(b)
            }
            Some(n)
        }
    }
}

/// Runs an assistive-technology `action` on `target`, one of the message's
/// buttons.
pub fn accessible(f: &Feedback, target: &str, action: &str, cx: &mut EventCx) {
    let Feedback::Message {
        id,
        actions,
        dismissible,
        ..
    } = f
    else {
        return;
    };
    if action == "press"
        && targets(id, actions, *dismissible)
            .iter()
            .any(|t| t == target)
    {
        cx.emit(&UiMsg::Click(target.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{Level, banner, message_parts};
    use crate::render::Canvas;
    use crate::strext::StrExt;
    use crate::style::Theme;
    use crate::widgets::Widget;
    use crate::widgets::cx::UiMsg;

    fn view(_m: &i64) -> Widget {
        Widget::column(
            &[banner(
                "note",
                Level::Info,
                "Update",
                "A new version is ready.",
                &[
                    ("later".to_string(), "Later".to_string()),
                    ("now".to_string(), "Install".to_string()),
                ],
                false,
            )],
            0,
        )
        .padding(8)
    }

    // The colour of pixel (`x`, `y`).
    fn centre(c: &Canvas, x: i64, y: i64) -> (u8, u8, u8) {
        let at = (y * c.width + x) * 4;
        (
            c.pixels[at as usize],
            c.pixels[(at + 1) as usize],
            c.pixels[(at + 2) as usize],
        )
    }

    #[test]
    fn only_the_action_under_the_pointer_shows_hover() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Banner test",
            "dev.gossamer.gui-banner-test",
            || 0,
            |m: i64, _msg: UiMsg| m,
            view,
        )
        .size(480, 200)
        .start()
        .unwrap();
        let origin = s.rect_of("note").unwrap();
        let Widget::Feedback(f) = banner(
            "note",
            Level::Info,
            "Update",
            "A new version is ready.",
            &[
                ("later".to_string(), "Later".to_string()),
                ("now".to_string(), "Install".to_string()),
            ],
            false,
        ) else {
            return;
        };
        let (parts, _) = message_parts(&f, origin.width, &Theme::light());
        let later = parts[2].offset(origin.x, origin.y);
        let now = parts[3].offset(origin.x, origin.y);
        let idle = s.capture().unwrap();
        s.hover(now.center_x(), now.center_y()).unwrap();
        let hovered = s.capture().unwrap();
        assert_eq!(
            centre(&hovered, later.x + 2, later.center_y()),
            centre(&idle, later.x + 2, later.center_y())
        );
        assert!(
            centre(&hovered, now.x + 2, now.center_y()) != centre(&idle, now.x + 2, now.center_y())
        );
        s.shutdown()
    }

    #[derive(Debug, Clone, Default, PartialEq)]
    struct Log {
        clicks: Vec<String>,
    }

    fn log_view(m: &Log) -> Widget {
        Widget::column(
            &[
                banner(
                    "b",
                    Level::Warning,
                    "Offline",
                    "Changes are saved locally.",
                    &[
                        ("retry".to_string(), "Retry".to_string()),
                        ("details".to_string(), "Details".to_string()),
                    ],
                    true,
                ),
                Widget::button("other", "Other"),
                Widget::label(&format!("log {}", m.clicks.join("; "))),
            ],
            8,
        )
        .padding(8)
    }

    fn log_update(m: Log, msg: UiMsg) -> Log {
        let mut n = m;
        if let UiMsg::Click(id) = msg {
            n.clicks.push(id.to_string())
        };
        n
    }

    fn clicks(s: &crate::Session) -> String {
        let dump = s.semantics(0).dump();
        let at = dump.find("log ").unwrap_or(0);
        let rest = dump.substring((at + 4) as i64, dump.byte_len());
        rest.substring(
            0,
            (rest.find("\"").unwrap_or(rest.byte_len() as usize)) as i64,
        )
        .to_string()
    }

    fn part(s: &crate::Session, i: i64) -> crate::geometry::Rect {
        let origin = s.rect_of("b").unwrap();
        let Widget::Feedback(f) = banner(
            "b",
            Level::Warning,
            "Offline",
            "Changes are saved locally.",
            &[
                ("retry".to_string(), "Retry".to_string()),
                ("details".to_string(), "Details".to_string()),
            ],
            true,
        ) else {
            return origin;
        };
        let (parts, _) = message_parts(&f, origin.width, &Theme::light());
        parts[i as usize].offset(origin.x, origin.y)
    }

    #[test]
    fn banner_actions_run_on_release_and_every_one_is_reachable_by_keyboard() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Banner test",
            "dev.gossamer.gui-banner-test",
            Log::default,
            log_update,
            log_view,
        )
        .size(560, 260)
        .start()
        .unwrap();
        let details = part(&s, 3);
        // Pressed and dragged off: nothing. Pressed and released over it: one click.
        s.press_pointer(details.center_x(), details.center_y())
            .unwrap();
        assert_eq!(clicks(&s), "");
        s.release_pointer(details.center_x(), details.center_y() + 100)
            .unwrap();
        s.click_at(details.center_x(), details.center_y()).unwrap();
        // From the keyboard the arrows move between Retry, Details and Dismiss.
        assert!(s.focus("b").unwrap());
        s.press(windowing::Key::Return, windowing::Mods::default())
            .unwrap();
        s.press(windowing::Key::Right, windowing::Mods::default())
            .unwrap();
        s.press(windowing::Key::Space, windowing::Mods::default())
            .unwrap();
        s.press(windowing::Key::Right, windowing::Mods::default())
            .unwrap();
        s.press(windowing::Key::Return, windowing::Mods::default())
            .unwrap();
        assert_eq!(clicks(&s), "b:details; b:retry; b:details; b:dismiss");
        s.shutdown()
    }

    #[test]
    fn assistive_technology_presses_banner_actions_and_dismiss() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Banner test",
            "dev.gossamer.gui-banner-test",
            Log::default,
            log_update,
            log_view,
        )
        .size(560, 260)
        .start()
        .unwrap();
        let dump = s.semantics(0).dump();
        assert!(
            dump.contains("button \"Retry\"") && dump.contains("button \"Dismiss\""),
            "{}",
            dump
        );
        s.accessible_action("b", "b:details", "press", "").unwrap();
        s.accessible_action("b", "b:dismiss", "press", "").unwrap();
        assert_eq!(clicks(&s), "b:details; b:dismiss");
        s.shutdown()
    }

    #[test]
    fn banner_parts_mirror_in_right_to_left_layouts() {
        let Widget::Feedback(f) = banner(
            "b",
            Level::Info,
            "",
            "Text",
            &[("ok".to_string(), "OK".to_string())],
            true,
        ) else {
            return;
        };
        let mut rtl = Theme::light();
        rtl.rtl = true;
        let (ltr_parts, _) = message_parts(&f, 500, &Theme::light());
        let (rtl_parts, _) = message_parts(&f, 500, &rtl);
        let mut i = 0;
        while i < ltr_parts.len() {
            let a = ltr_parts[i];
            let b = rtl_parts[i];
            assert_eq!(b.x, 500 - a.x - a.width, "part {}", i);
            assert_eq!(b.y, a.y);
            i += 1;
        }
    }

    fn toast_view(m: &Log) -> Widget {
        Widget::column(
            &[
                Widget::button("work", "Work"),
                Widget::label(&format!("log {}", m.clicks.join("; "))),
            ],
            8,
        )
        .padding(8)
    }

    fn show(t: &super::Toast) {
        crate::perform(crate::app::effects::Effect::Toast(t.clone()))
    }

    #[test]
    fn a_toast_times_out_waits_while_hovered_and_lets_clicks_through() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Toasts",
            "dev.gossamer.gui-toast-test",
            Log::default,
            log_update,
            toast_view,
        )
        .size(560, 400)
        .start()
        .unwrap();
        show(&super::Toast::new("saved", "Document saved").lasting(3000));
        s.flush().unwrap();
        assert!(
            s.semantics(0).dump().contains("Document saved"),
            "{}",
            s.semantics(0).dump()
        );
        assert!(
            s.announcements.contains(&"Document saved".to_string()),
            "{:?}",
            s.announcements
        );
        // The rest of the window still takes the pointer.
        assert!(s.click("work").unwrap());
        // Hovered, it stays past its time; left, it goes and says so.
        let r = s.rect_of("saved").unwrap();
        s.hover(r.center_x(), r.center_y()).unwrap();
        s.advance(4000).unwrap();
        assert!(
            s.semantics(0).dump().contains("Document saved"),
            "{}",
            s.semantics(0).dump()
        );
        s.hover(5, 5).unwrap();
        s.advance(1100).unwrap();
        assert!(
            !s.semantics(0).dump().contains("Document saved"),
            "{}",
            s.semantics(0).dump()
        );
        assert_eq!(clicks(&s), "work; saved:timeout");
        s.shutdown()
    }

    #[test]
    fn a_toast_action_closes_it_and_reaches_the_application() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Toasts",
            "dev.gossamer.gui-toast-test",
            Log::default,
            log_update,
            toast_view,
        )
        .size(560, 400)
        .start()
        .unwrap();
        show(&super::Toast::new("gone", "Message deleted").action("undo", "Undo"));
        s.flush().unwrap();
        assert!(s.focus("gone").unwrap());
        s.press(windowing::Key::Return, windowing::Mods::default())
            .unwrap();
        assert!(
            !s.semantics(0).dump().contains("Message deleted"),
            "{}",
            s.semantics(0).dump()
        );
        // Replacing and dismissing from the application report nothing.
        show(&super::Toast::new("note", "First"));
        show(&super::Toast::new("note", "Second"));
        s.flush().unwrap();
        let dump = s.semantics(0).dump();
        assert!(
            dump.contains("Second") && !dump.contains("First"),
            "{}",
            dump
        );
        crate::perform(crate::app::effects::Effect::DismissToast(
            "note".to_string(),
        ));
        s.flush().unwrap();
        s.advance(6000).unwrap();
        assert!(
            !s.semantics(0).dump().contains("Second"),
            "{}",
            s.semantics(0).dump()
        );
        assert_eq!(clicks(&s), "gone:undo");
        s.shutdown()
    }

    #[test]
    fn a_burst_of_announcements_is_spoken_one_per_interval_newest_kept() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Toasts",
            "dev.gossamer.gui-toast-test",
            Log::default,
            log_update,
            toast_view,
        )
        .size(560, 400)
        .start()
        .unwrap();
        let say = |text: String| crate::perform(crate::app::effects::Effect::Announce(text));
        show(&super::Toast::new("a", "Copied"));
        say("Copied".to_string());
        say("1 of 4".to_string());
        say("2 of 4".to_string());
        say("3 of 4".to_string());
        say("4 of 4".to_string());
        say("4 of 4".to_string());
        s.flush().unwrap();
        // The first is spoken at once; its repeat and the oldest waiting are dropped.
        assert_eq!(s.spoken(), vec!["Copied"]);
        s.advance(750).unwrap();
        assert_eq!(s.spoken(), vec!["Copied", "2 of 4"]);
        s.advance(750).unwrap();
        s.advance(750).unwrap();
        s.advance(750).unwrap();
        assert_eq!(s.spoken(), vec!["Copied", "2 of 4", "3 of 4", "4 of 4"]);
        // Every request is still recorded, and a quiet interval speaks at once.
        assert_eq!(s.announcements.len(), 7);
        say("Copied".to_string());
        s.flush().unwrap();
        assert_eq!(s.spoken().len(), 5);
        s.shutdown()
    }
}
