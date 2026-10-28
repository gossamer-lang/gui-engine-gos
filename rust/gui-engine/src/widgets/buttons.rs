// buttons: action controls. Buttons activate on pointer release over the
// pressed control (a release elsewhere cancels), on Enter, and on Space
// release; checkboxes, switches, radio groups, and segmented controls
// change on the same gestures and report the new value. None of them
// changes its own value: the application stores the reported value and
// the next view shows it.
use windowing::{Key, MouseButton};

use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Rect, Size};
use crate::localization;
use crate::render::icons::Icon;
use crate::style::color::Color;
use crate::style::{TextRole, Theme};
use crate::text::layout;
use crate::text::layout::LayoutOptions;
use crate::widgets::cx::{CheckState, Event, EventCx, LayoutCx, Node, PaintCx, Placement, UiMsg};
use crate::widgets::menus;
use crate::widgets::menus::MenuItem;
use crate::widgets::state::{StateStore, WState};

/// A button's visual weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ButtonKind {
    Normal,
    Primary,
    Danger,
    Flat,
    Tool,
}

/// A push, toggle, tool, or command button.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Button {
    pub id: String,
    pub label: String,
    /// A built-in icon name shown before the label (or alone for tools).
    pub icon: Option<crate::render::icons::Icon>,
    pub kind: ButtonKind,
    /// A command to run instead of emitting `Click`.
    pub command: String,
    /// -1 a push button; 0/1 a toggle button's state.
    pub toggled: i64,
    /// The dialog default: Enter in the dialog activates it.
    pub is_default: bool,
    /// A small menu arrow after the label (a menu button).
    pub menu_arrow: bool,
    /// The menu it opens: the whole button opens it, or with `split` only
    /// its arrow part does while the rest runs the button.
    pub menu: Vec<MenuItem>,
    pub split: bool,
    /// A disclosure header: `toggled` is whether its section is open, shown
    /// by a chevron and reported as expanded rather than pressed.
    pub expander: bool,
    /// A color a tool button shows in place of an icon (a palette swatch).
    pub swatch: Option<Color>,
}

impl Button {
    pub fn new(id: &str, label: &str, kind: ButtonKind) -> Button {
        Button {
            id: id.to_string(),
            label: label.to_string(),
            icon: None,
            kind,
            command: String::new(),
            toggled: -1,
            is_default: false,
            menu_arrow: false,
            menu: vec![],
            split: false,
            expander: false,
            swatch: None,
        }
    }
}

fn label_style(theme: &Theme, kind: ButtonKind) -> crate::text::layout::TextStyle {
    let mut s = theme.font(TextRole::Body);
    if kind == ButtonKind::Primary {
        s.bold = true
    };
    s
}

pub fn layout_button(b: &Button, c: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    let h = theme.control_height();
    let pad = theme.padding() + theme.px(4);
    let icon = if b.icon.is_none() && !b.expander {
        0
    } else {
        theme.px(theme.shape.icon)
    };
    let mut w;
    if b.kind == ButtonKind::Tool && (b.icon.is_some() || b.swatch.is_some()) {
        w = h;
    } else {
        let (tw, _) = layout::measure(&b.label, &label_style(theme, b.kind));
        w = tw.ceil() as i64 + pad * 2 + if icon > 0 { icon + theme.px(8) } else { 0 };
        if b.menu_arrow {
            w += theme.px(theme.shape.small_icon + 4)
        };
        w = w.max(theme.px(80));
        if b.split {
            w += arrow_width(theme)
        }
    }
    let mut node = Node::leaf(c.constrain(Size::new(w, h)));
    if b.split {
        // The main part, then the arrow part (on the left in right-to-left layouts).
        let size = node.size();
        let a = arrow_width(theme);
        let main = Rect::new(0, 0, size.width - a, size.height);
        let arrow = Rect::new(size.width - a, 0, a, size.height);
        node.parts = if theme.rtl {
            vec![
                main.mirror_in(Rect::from_size(size)),
                arrow.mirror_in(Rect::from_size(size)),
            ]
        } else {
            vec![main, arrow]
        };
    }
    let (a, _, lh) = layout::metrics(&label_style(theme, b.kind));
    node.baseline = (h - lh as i64) / 2 + a as i64;
    node
}

// A pointer press is tracked in the control's "ppress" field: "1" while the
// pointer is over where it went down, "0" once it has left, absent otherwise.
// Only a release while it reads "1" acts.

/// Starts a press gesture on control `id`: focuses it and takes the pointer.
pub fn press_begin(id: &str, st: &mut StateStore, cx: &mut EventCx) {
    cx.focus_on(id);
    cx.capture_pointer(id);
    st.set_field(id, "ppress", "1");
    cx.redraw = true;
    cx.consume()
}

/// Follows a held press as the pointer moves `inside` the control or out of it.
pub fn press_track(id: &str, inside: bool, st: &mut StateStore, cx: &mut EventCx) {
    let was = st.field(id, "ppress", "");
    if was.is_empty() {
        return;
    };
    let now = if inside { "1" } else { "0" };
    if now != was {
        st.set_field(id, "ppress", now);
        cx.redraw = true;
    }
}

/// Ends a press gesture released `inside` the control or not; true when the
/// control should act (it was pressed here and released here).
pub fn press_end(id: &str, inside: bool, st: &mut StateStore, cx: &mut EventCx) -> bool {
    let held = st.field(id, "ppress", "") == "1";
    st.set_field(id, "ppress", "");
    cx.release_pointer();
    cx.redraw = true;
    cx.consume();
    held && inside
}

/// Whether control `id` paints pressed: the pointer holds it down over it,
/// or Space does.
pub fn held(id: &str, cx: &PaintCx) -> bool {
    !cx.disabled
        && (cx.states.field(id, "ppress", "") == "1" || cx.states.field(id, "kpress", "") == "1")
}

/// Drops a press gesture (focus or the pointer went elsewhere).
pub fn press_cancel(id: &str, st: &mut StateStore, cx: &mut EventCx) {
    if !st.field(id, "ppress", "").is_empty() || st.field(id, "kpress", "") == "1" {
        st.set_field(id, "ppress", "");
        st.set_field(id, "kpress", "");
        cx.redraw = true;
    }
}

fn arrow_width(theme: &Theme) -> i64 {
    theme.px(28)
}

// An expander paints as a flat push button; its chevron shows the state.
fn expander_face(b: Button) -> Button {
    let mut face = b;
    face.toggled = -1;
    face
}

// The chevron of an expander: down when open, else pointing along the text.
fn expander_icon(b: &Button, rtl: bool) -> Icon {
    if b.toggled == 1 {
        Icon::ChevronDown
    } else if rtl {
        Icon::ChevronLeft
    } else {
        Icon::ChevronRight
    }
}

fn colors(b: &Button, cx: &PaintCx) -> (Color, Color, Color) {
    let t = &cx.theme;
    let hover = cx.is_hover(&b.id);
    let pressed = held(&b.id, cx);
    let on = b.toggled == 1;
    if cx.disabled {
        let bg = if b.kind == ButtonKind::Flat || b.kind == ButtonKind::Tool {
            Color::transparent()
        } else {
            t.surface
        };
        return (bg, t.text_disabled, t.border);
    }
    match b.kind {
        ButtonKind::Primary => {
            let bg = if pressed {
                t.accent.mix(Color::black(), 0.25)
            } else if hover {
                t.accent.mix(Color::white(), 0.12)
            } else {
                t.accent
            };
            (bg, t.accent_text, bg)
        }
        ButtonKind::Danger => {
            let bg = if pressed {
                t.danger.mix(Color::black(), 0.25)
            } else if hover {
                t.danger.mix(Color::white(), 0.12)
            } else {
                t.danger
            };
            (bg, t.danger_text, bg)
        }
        ButtonKind::Flat | ButtonKind::Tool => {
            let bg = if pressed || on {
                t.pressed
            } else if hover {
                t.hover
            } else {
                Color::transparent()
            };
            (
                bg,
                t.text,
                if on {
                    t.border_strong
                } else {
                    Color::transparent()
                },
            )
        }
        ButtonKind::Normal => {
            let bg = if pressed || on {
                t.pressed
            } else if hover {
                t.hover
            } else {
                t.surface
            };
            (bg, t.text, if on { t.accent } else { t.border })
        }
    }
}

pub fn paint_button(b: &Button, node: &Node, cx: &mut PaintCx) {
    let r = Rect::from_size(node.size());
    let theme = cx.theme.clone();
    let face = if b.expander {
        expander_face(b.clone())
    } else {
        b.clone()
    };
    let (bg, fg, border) = colors(&face, cx);
    let radius = theme.radius();
    if bg.a > 0 {
        cx.fill_round(r, radius, bg)
    };
    if border.a > 0 {
        cx.stroke_round(r, radius, theme.border_width(), border)
    };
    if b.is_default && !cx.disabled && b.kind == ButtonKind::Normal {
        cx.stroke_round(
            r.inset(theme.px(2)),
            radius - theme.px(2),
            theme.border_width(),
            theme.accent,
        )
    }
    let abs = cx.abs(r);
    let style = label_style(&theme, b.kind);
    let icon_size = theme.px(theme.shape.icon);
    if b.kind == ButtonKind::Tool
        && let Some(color) = b.swatch
    {
        // Two tones behind the color show how transparent it is.
        let side = (abs.height - theme.px(12)).max(icon_size);
        let chip = Rect::new(
            abs.x + (abs.width - side) / 2,
            abs.y + (abs.height - side) / 2,
            side,
            side,
        );
        let half = side / 2;
        cx.canvas.rect(
            chip.x,
            chip.y,
            chip.width,
            chip.height,
            color.over(theme.surface),
        );
        cx.canvas.rect(
            chip.x + half,
            chip.y,
            chip.width - half,
            half,
            color.over(theme.border),
        );
        cx.canvas.rect(
            chip.x,
            chip.y + half,
            half,
            chip.height - half,
            color.over(theme.border),
        );
        cx.canvas.round_rect_stroke(
            chip.x,
            chip.y,
            chip.width,
            chip.height,
            0,
            theme.border_width(),
            theme.border,
        )
    } else if b.kind == ButtonKind::Tool
        && let Some(ref tool) = b.icon
    {
        cx.icon(
            tool,
            abs.x + (abs.width - icon_size) / 2,
            abs.y + (abs.height - icon_size) / 2,
            icon_size,
            fg,
        )
    } else {
        let gap = theme.px(8);
        let arrow = if b.menu_arrow {
            theme.px(theme.shape.small_icon + 4)
        } else {
            0
        };
        let icon = if b.expander {
            Some(expander_icon(b, theme.rtl))
        } else {
            b.icon.clone()
        };
        let main = if b.split && node.parts.len() == 2 {
            cx.abs(node.parts[0])
        } else {
            abs
        };
        let beside = if icon.is_none() { 0 } else { icon_size + gap } + arrow;
        // A label wider than the button ends in an ellipsis.
        let room = (main.width - (theme.padding() + theme.px(4)) * 2 - beside).max(0) as f64;
        let mut l = layout::layout(&b.label, &style, LayoutOptions::single_line());
        if l.width > room {
            l = layout::layout(&b.label, &style, LayoutOptions::elided(room));
        }
        let content = l.width.ceil() as i64 + beside;
        if b.split && node.parts.len() == 2 {
            let part = cx.abs(node.parts[1]);
            let edge = if theme.rtl {
                part.x + part.width - theme.border_width()
            } else {
                part.x
            };
            cx.fill(
                Rect::new(
                    edge - abs.x,
                    part.y - abs.y + theme.px(6),
                    theme.border_width(),
                    part.height - theme.px(12),
                ),
                border,
            );
            let s = theme.px(theme.shape.small_icon);
            cx.icon(
                &Icon::ChevronDown,
                part.x + (part.width - s) / 2,
                part.y + (part.height - s) / 2,
                s,
                fg,
            )
        }
        let mut x = main.x + (main.width - content) / 2;
        if let Some(drawn) = icon {
            cx.icon(
                &drawn,
                x,
                abs.y + (abs.height - icon_size) / 2,
                icon_size,
                fg,
            );
            x += icon_size + gap;
        }
        let ty = abs.y + (abs.height - l.height.ceil() as i64) / 2;
        layout::draw(&mut cx.canvas, &l, x as f64, ty as f64, &style, fg);
        if b.menu_arrow {
            let s = theme.px(theme.shape.small_icon);
            cx.icon(
                &Icon::ChevronDown,
                x + l.width.ceil() as i64 + theme.px(4),
                abs.y + (abs.height - s) / 2,
                s,
                fg,
            )
        }
    }
    if cx.is_focused(&b.id) {
        cx.focus_ring(r, radius)
    }
}

fn activate(b: &Button, cx: &mut EventCx) {
    if !b.command.is_empty() {
        cx.run_command(&b.command)
    } else if b.toggled >= 0 {
        cx.emit(&UiMsg::Toggle(b.id.clone(), b.toggled == 0))
    } else {
        cx.emit(&UiMsg::Click(b.id.clone()))
    }
}

pub fn button_event(b: &Button, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    if cx.disabled {
        return;
    }
    let area = cx.to_window(Rect::from_size(node.size()));
    if !b.menu.is_empty() && menu_event(b, node, ev, st, cx) {
        return;
    }
    let main = if b.split && node.parts.len() == 2 {
        cx.to_window(node.parts[0])
    } else {
        area
    };
    match ev {
        Event::PointerDown { button, .. } if *button == MouseButton::Left => {
            press_begin(&b.id, st, cx)
        }
        Event::PointerMove { x, y, .. } => press_track(&b.id, main.contains(*x, *y), st, cx),
        Event::PointerUp { x, y, button, .. }
            if *button == MouseButton::Left && press_end(&b.id, main.contains(*x, *y), st, cx) =>
        {
            activate(b, cx)
        }
        Event::KeyDown { key, repeat, .. } => {
            if *key == Key::Return || *key == Key::KpEnter {
                if !repeat {
                    activate(b, cx)
                };
                cx.consume()
            } else if *key == Key::Space {
                st.set_field(&b.id, "kpress", "1");
                cx.redraw = true;
                cx.consume()
            }
        }
        Event::KeyUp { key, .. } if *key == Key::Space && st.field(&b.id, "kpress", "") == "1" => {
            st.set_field(&b.id, "kpress", "");
            activate(b, cx);
            cx.consume()
        }
        Event::FocusOut | Event::CaptureLost => press_cancel(&b.id, st, cx),
        _ => (),
    }
}

// The menu part of a menu or split button: a press there, or Down (Alt+Down)
// from the keyboard, opens the menu below the button; a menu button also
// opens it on Enter and Space. True when `ev` was the menu's.
fn menu_event(b: &Button, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) -> bool {
    let anchor = cx.to_window(Rect::from_size(node.size()));
    let open = |keyboard: bool, st: &mut StateStore, cx: &mut EventCx| {
        cx.focus_on(&b.id);
        menus::open_menu(&b.id, &b.menu, anchor, Placement::Below, keyboard, st, cx);
        cx.consume()
    };
    match ev {
        Event::PointerDown { x, y, button, .. } => {
            if *button != MouseButton::Left {
                return false;
            };
            let on_arrow =
                b.split && node.parts.len() == 2 && cx.to_window(node.parts[1]).contains(*x, *y);
            if b.split && !on_arrow {
                return false;
            };
            open(false, st, cx);
            true
        }
        Event::KeyDown { key, repeat, .. } => {
            let opens = *key == Key::Down
                || (!b.split
                    && (*key == Key::Return || *key == Key::KpEnter || *key == Key::Space));
            if !opens {
                return false;
            };
            if !repeat {
                open(true, st, cx)
            };
            cx.consume();
            true
        }
        Event::KeyUp { key, .. } => !b.split && *key == Key::Space,
        Event::FocusOut => {
            // The session closes the panel itself when focus or a click moves away.
            let mut s = st.menu(&b.id);
            if s.open >= 0 {
                s.open = -1;
                s.path = vec![];
                st.set(&b.id, WState::Menu(s))
            }
            false
        }
        _ => false,
    }
}

pub fn button_semantics(b: &Button, st: &StateStore) -> Option<AccessNode> {
    if b.expander {
        let mut n = AccessNode::control(Role::Button, &b.id, &b.label, "press");
        n.expanded = if b.toggled == 1 { 1 } else { 0 };
        return Some(n);
    }
    let role = if b.toggled >= 0 {
        Role::ToggleButton
    } else {
        Role::Button
    };
    let mut n = AccessNode::control(role, &b.id, &b.label, "press");
    if b.toggled >= 0 {
        n.pressed = b.toggled == 1
    };
    if !b.menu.is_empty() {
        let expanded = if st.menu(&b.id).open >= 0 { 1 } else { 0 };
        if b.split {
            let mut more = AccessNode::new(
                Role::Button,
                &localization::message(
                    "button",
                    "More {label} options",
                    1,
                    &crate::hmap! { "label" => b.label.clone() },
                ),
            );
            more.id = format!("{}#menu", b.id);
            more.expanded = expanded;
            more.actions.push("press".to_string());
            n.children.push(more)
        } else {
            n.expanded = expanded;
        }
    }
    Some(n)
}

/// A checkbox or switch.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Check {
    pub id: String,
    pub label: String,
    pub state: CheckState,
    pub tristate: bool,
    /// Clicking a tri-state box cycles through mixed too.
    pub cycle_mixed: bool,
    pub switch: bool,
}

impl Check {
    pub fn checkbox(id: &str, label: &str, state: CheckState) -> Check {
        Check {
            id: id.to_string(),
            label: label.to_string(),
            state,
            tristate: false,
            cycle_mixed: false,
            switch: false,
        }
    }
}

fn indicator_size(theme: &Theme, switch: bool) -> Size {
    let s = theme.indicator();
    if switch {
        Size::new(s * 2, s)
    } else {
        Size::new(s, s)
    }
}

pub fn layout_check(k: &Check, c: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    let ind = indicator_size(theme, k.switch);
    let (tw, th) = layout::measure(&k.label, &theme.font(TextRole::Body));
    let gap = if k.label.is_empty() { 0 } else { theme.px(10) };
    let h = (ind.height.max(th.ceil() as i64)).max(theme.px(32));
    let mut node = Node::leaf(c.constrain(Size::new(ind.width + gap + tw.ceil() as i64, h)));
    let (a, _, lh) = layout::metrics(&theme.font(TextRole::Body));
    node.baseline = (h - lh as i64) / 2 + a as i64;
    node
}

pub fn paint_check(k: &Check, node: &Node, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let r = Rect::from_size(node.size());
    let ind = indicator_size(&theme, k.switch);
    let iy = (r.height - ind.height) / 2;
    let ix = if theme.rtl { r.width - ind.width } else { 0 };
    let box_r = Rect::new(ix, iy, ind.width, ind.height);
    let on = k.state == CheckState::Checked;
    let mixed = k.state == CheckState::Mixed;
    let hover = cx.is_hover(&k.id);
    let pressed = held(&k.id, cx);
    let disabled = cx.disabled;
    let accent = if disabled {
        theme.text_disabled
    } else if pressed {
        theme.accent.mix(Color::black(), 0.25)
    } else {
        theme.accent
    };
    if k.switch {
        let track = if on {
            accent
        } else if pressed {
            theme.border_strong.mix(Color::black(), 0.25)
        } else if hover {
            theme.border_strong
        } else {
            theme.border
        };
        cx.fill_round(box_r, ind.height / 2, track);
        let knob = ind.height - theme.px(6);
        let kx = if on {
            box_r.x + box_r.width - knob - theme.px(3)
        } else {
            box_r.x + theme.px(3)
        };
        let kr = cx.abs(Rect::new(kx, box_r.y + theme.px(3), knob, knob));
        let knob_color = if on { theme.accent_text } else { theme.field };
        cx.canvas
            .circle(kr.x + knob / 2, kr.y + knob / 2, knob / 2, knob_color)
    } else {
        let fill = if on || mixed {
            accent
        } else if pressed {
            theme.pressed
        } else {
            theme.field
        };
        cx.fill_round(box_r, theme.px(4), fill);
        let edge = if on || mixed {
            accent
        } else if hover {
            theme.border_strong
        } else {
            theme.border_strong.with_alpha(200)
        };
        cx.stroke_round(box_r, theme.px(4), theme.border_width(), edge);
        let a = cx.abs(box_r);
        if on {
            cx.icon(&Icon::Check, a.x, a.y, a.width, theme.accent_text)
        } else if mixed {
            cx.icon(&Icon::Minus, a.x, a.y, a.width, theme.accent_text)
        }
    }
    let style = theme.font(TextRole::Body);
    let l = layout::layout(&k.label, &style, LayoutOptions::single_line());
    let gap = theme.px(10);
    let tx = if theme.rtl {
        r.width - ind.width - gap - l.width.ceil() as i64
    } else {
        ind.width + gap
    };
    let abs = cx.abs(r);
    let color = if disabled {
        theme.text_disabled
    } else {
        theme.text
    };
    layout::draw(
        &mut cx.canvas,
        &l,
        (abs.x + tx) as f64,
        (abs.y + (r.height - l.height.ceil() as i64) / 2) as f64,
        &style,
        color,
    );
    if cx.is_focused(&k.id) {
        let rad = theme.px(4);
        cx.focus_ring(box_r, rad)
    }
}

fn next_state(k: &Check) -> CheckState {
    match k.state {
        CheckState::Unchecked => CheckState::Checked,
        CheckState::Checked => {
            if k.tristate && k.cycle_mixed {
                CheckState::Mixed
            } else {
                CheckState::Unchecked
            }
        }
        CheckState::Mixed => CheckState::Unchecked,
    }
}

fn toggle(k: &Check, cx: &mut EventCx) {
    let next = next_state(k);
    if k.tristate {
        cx.emit(&UiMsg::Check(k.id.clone(), next))
    } else {
        cx.emit(&UiMsg::Toggle(k.id.clone(), next == CheckState::Checked))
    }
}

pub fn check_event(k: &Check, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    if cx.disabled {
        return;
    }
    let area = cx.to_window(Rect::from_size(node.size()));
    match ev {
        Event::PointerDown { button, .. } if *button == MouseButton::Left => {
            press_begin(&k.id, st, cx)
        }
        Event::PointerMove { x, y, .. } => press_track(&k.id, area.contains(*x, *y), st, cx),
        Event::PointerUp { x, y, button, .. }
            if *button == MouseButton::Left && press_end(&k.id, area.contains(*x, *y), st, cx) =>
        {
            toggle(k, cx)
        }
        Event::KeyDown { key, .. } if *key == Key::Space => {
            st.set_field(&k.id, "kpress", "1");
            cx.consume()
        }
        Event::KeyUp { key, .. } if *key == Key::Space && st.field(&k.id, "kpress", "") == "1" => {
            st.set_field(&k.id, "kpress", "");
            toggle(k, cx);
            cx.consume()
        }
        Event::FocusOut | Event::CaptureLost => press_cancel(&k.id, st, cx),
        _ => (),
    }
}

pub fn check_semantics(k: &Check) -> Option<AccessNode> {
    let role = if k.switch {
        Role::Switch
    } else {
        Role::CheckBox
    };
    let mut n = AccessNode::control(role, &k.id, &k.label, "toggle");
    n.checked = match k.state {
        CheckState::Unchecked => 0,
        CheckState::Checked => 1,
        CheckState::Mixed => 2,
    };
    Some(n)
}

/// Exclusive options with one focus stop; arrows move the choice.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RadioGroup {
    pub id: String,
    /// (key, label) pairs.
    pub options: Vec<(String, String)>,
    pub selected: String,
    pub horizontal: bool,
    pub disabled_keys: Vec<String>,
}

pub fn layout_radio_group(r: &RadioGroup, c: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    let ind = theme.indicator();
    let gap = theme.px(10);
    let row_h = (ind + theme.px(8)).max(theme.px(32));
    let mut parts = vec![];
    let mut x = 0;
    let mut y = 0;
    let mut w = 0;
    for (_, label) in &r.options {
        let (tw, _) = layout::measure(label, &theme.font(TextRole::Body));
        let item_w = ind + gap + tw.ceil() as i64;
        parts.push(Rect::new(x, y, item_w, row_h));
        if r.horizontal {
            x += item_w + theme.px(20);
            w = x - theme.px(20);
        } else {
            y += row_h;
            w = w.max(item_w);
        }
    }
    let h = if r.horizontal { row_h } else { y };
    let size = c.constrain(Size::new(w, h));
    if theme.rtl {
        let mut k = 0;
        while k < parts.len() {
            parts[k] = parts[k].mirror_in(Rect::from_size(size));
            k += 1;
        }
    }
    let mut node = Node::leaf(size);
    node.parts = parts;
    node
}

fn selected_index(r: &RadioGroup) -> i64 {
    let mut i = 0;
    while i < r.options.len() {
        if r.options[i].0 == r.selected {
            return i as i64;
        };
        i += 1;
    }
    -1
}

pub fn paint_radio_group(r: &RadioGroup, node: &Node, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let ind = theme.indicator();
    let gap = theme.px(10);
    let focused = cx.is_focused(&r.id);
    let mut focus_index = selected_index(r);
    if focus_index < 0 {
        focus_index = 0
    };
    let mut i = 0;
    while i < r.options.len() && i < node.parts.len() {
        let (ref key, ref label) = r.options[i];
        let part = node.parts[i];
        let disabled = cx.disabled || r.disabled_keys.contains(key);
        let on = *key == r.selected;
        let lit = !disabled && option_lit(&r.id, i as i64, cx);
        let pressed = !disabled && cx.states.field(&r.id, "ppress", "") == format!("{}", i);
        let ix = if theme.rtl {
            part.x + part.width - ind
        } else {
            part.x
        };
        let circle = cx.abs(Rect::new(ix, part.y + (part.height - ind) / 2, ind, ind));
        let accent = if disabled {
            theme.text_disabled
        } else if pressed {
            theme.accent.mix(Color::black(), 0.25)
        } else {
            theme.accent
        };
        cx.canvas.circle(
            circle.x + ind / 2,
            circle.y + ind / 2,
            ind / 2,
            if pressed {
                theme.pressed
            } else if lit {
                theme.hover
            } else {
                theme.field
            },
        );
        let edge = if on {
            accent
        } else if lit {
            theme.accent
        } else {
            theme.border_strong
        };
        cx.canvas.circle_stroke(
            circle.x + ind / 2,
            circle.y + ind / 2,
            ind / 2 - 1,
            theme.border_width(),
            edge,
        );
        if on {
            cx.canvas
                .circle(circle.x + ind / 2, circle.y + ind / 2, ind / 4, accent)
        }
        let style = theme.font(TextRole::Body);
        let l = layout::layout(label, &style, LayoutOptions::single_line());
        let tx = if theme.rtl {
            ix - gap - l.width.ceil() as i64
        } else {
            ix + ind + gap
        };
        let color = if disabled {
            theme.text_disabled
        } else {
            theme.text
        };
        let base = cx.abs(Rect::new(tx, part.y, 1, part.height));
        layout::draw(
            &mut cx.canvas,
            &l,
            base.x as f64,
            (base.y + (part.height - l.height.ceil() as i64) / 2) as f64,
            &style,
            color,
        );
        if focused && i == focus_index as usize {
            let local = Rect::new(ix, part.y + (part.height - ind) / 2, ind, ind);
            cx.focus_ring(local, ind / 2)
        }
        i += 1;
    }
}

// The option part at local (`x`, `y`), or -1.
fn part_at(node: &Node, x: i64, y: i64) -> i64 {
    let mut i = 0;
    while i < node.parts.len() {
        if node.parts[i].contains(x, y) {
            return i as i64;
        };
        i += 1;
    }
    -1
}

// Whether option `i` of group `id` is under the pointer.
fn option_lit(id: &str, i: i64, cx: &PaintCx) -> bool {
    cx.is_hover(id) && cx.states.field(id, "hover", "") == format!("{}", i)
}

// Keeps the group's "hover" field on the option under the pointer.
fn option_hover(id: &str, i: i64, st: &mut StateStore, cx: &mut EventCx) {
    let now = if i >= 0 {
        format!("{}", i)
    } else {
        String::new()
    };
    if st.field(id, "hover", "") != now {
        st.set_field(id, "hover", &now);
        cx.redraw = true;
    }
}

// An option is chosen by a release over the enabled option it was pressed on;
// "ppress" holds that option's index while the press lasts.
fn option_press(
    id: &str,
    i: i64,
    disabled: &[String],
    options: &[(String, String)],
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    if i >= 0 && i < options.len() as i64 && !disabled.contains(&options[i as usize].0) {
        cx.capture_pointer(id);
        st.set_field(id, "ppress", &format!("{}", i));
        cx.redraw = true;
    }
    cx.consume()
}

fn option_release(
    id: &str,
    i: i64,
    options: &[(String, String)],
    selected: &str,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    let held = st.field(id, "ppress", "");
    st.set_field(id, "ppress", "");
    cx.redraw = true;
    cx.release_pointer();
    if !held.is_empty()
        && held == format!("{}", i)
        && i < options.len() as i64
        && options[i as usize].0 != selected
    {
        cx.emit(&UiMsg::Choose(
            id.to_string(),
            options[i as usize].0.clone(),
        ))
    }
    cx.consume()
}

fn choose_step(r: &RadioGroup, dir: i64, cx: &mut EventCx) {
    let n = r.options.len();
    if n == 0 {
        return;
    };
    let mut i = selected_index(r);
    let mut tries = 0;
    loop {
        i = if i < 0 {
            if dir > 0 { 0 } else { (n - 1) as i64 }
        } else {
            (i + dir + n as i64) % n as i64
        };
        tries += 1;
        if !r.disabled_keys.contains(&r.options[i as usize].0) || tries > n {
            break;
        }
    }
    if tries <= n && r.options[i as usize].0 != r.selected {
        cx.emit(&UiMsg::Choose(
            r.id.clone(),
            r.options[i as usize].0.clone(),
        ))
    }
}

pub fn radio_group_event(
    r: &RadioGroup,
    node: &Node,
    ev: &Event,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    if cx.disabled {
        return;
    }
    match ev {
        Event::PointerDown { x, y, button, .. } if *button == MouseButton::Left => {
            cx.focus_on(&r.id);
            option_press(
                &r.id,
                part_at(node, x - cx.origin_x, y - cx.origin_y),
                &r.disabled_keys,
                &r.options,
                st,
                cx,
            )
        }
        Event::PointerUp { x, y, button, .. } if *button == MouseButton::Left => option_release(
            &r.id,
            part_at(node, x - cx.origin_x, y - cx.origin_y),
            &r.options,
            &r.selected,
            st,
            cx,
        ),
        Event::PointerMove { x, y, .. } => option_hover(
            &r.id,
            part_at(node, x - cx.origin_x, y - cx.origin_y),
            st,
            cx,
        ),
        Event::PointerLeave => option_hover(&r.id, -1, st, cx),
        Event::CaptureLost => st.set_field(&r.id, "ppress", ""),
        Event::KeyDown { key, .. } => {
            let back = if cx.theme.rtl && r.horizontal {
                Key::Right
            } else {
                Key::Left
            };
            let fwd = if cx.theme.rtl && r.horizontal {
                Key::Left
            } else {
                Key::Right
            };
            if *key == Key::Down || *key == fwd {
                choose_step(r, 1, cx);
                cx.consume()
            } else if *key == Key::Up || *key == back {
                choose_step(r, -1, cx);
                cx.consume()
            } else if *key == Key::Space && selected_index(r) < 0 && !r.options.is_empty() {
                cx.emit(&UiMsg::Choose(r.id.clone(), r.options[0].0.clone()));
                cx.consume()
            }
        }
        _ => (),
    }
}

pub fn radio_group_semantics(r: &RadioGroup, node: &Node) -> Option<AccessNode> {
    let mut g = AccessNode::control(Role::RadioGroup, &r.id, "", "");
    let mut i = 0;
    while i < r.options.len() {
        let (ref key, ref label) = r.options[i];
        let mut item = AccessNode::new(Role::RadioButton, label);
        item.id = format!("{}#{}", r.id, key);
        item.checked = if *key == r.selected { 1 } else { 0 };
        item.disabled = r.disabled_keys.contains(key);
        item.pos_in_set = (i + 1) as i64;
        item.set_size = r.options.len() as i64;
        item.actions.push("select".to_string());
        if i < node.parts.len() {
            item.bounds = node.parts[i]
        };
        g.children.push(item);
        i += 1;
    }
    Some(g)
}

/// A joined row of exclusive buttons.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Segmented {
    pub id: String,
    pub options: Vec<(String, String)>,
    pub selected: String,
}

pub fn layout_segmented(s: &Segmented, c: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    let h = theme.control_height();
    let pad = theme.padding() + theme.px(4);
    let mut parts = vec![];
    let mut x = 0;
    for (_, label) in &s.options {
        let (tw, _) = layout::measure(label, &theme.font(TextRole::Body));
        let w = tw.ceil() as i64 + pad * 2;
        parts.push(Rect::new(x, 0, w, h));
        x += w;
    }
    let size = c.constrain(Size::new(x, h));
    // A width other than the natural one is shared out: extra room in equal
    // parts, a shortfall in proportion to each option's width.
    let n = parts.len();
    if n > 0 && x > 0 && size.width != x {
        let extra = size.width - x;
        let mut left = 0;
        let mut k = 0;
        while k < n {
            let w = if extra > 0 {
                parts[k].width + extra / n as i64 + if k == n - 1 { extra % n as i64 } else { 0 }
            } else {
                parts[k].width * size.width / x
            };
            let w = if k == n - 1 { size.width - left } else { w };
            parts[k] = Rect::new(left, 0, w, h);
            left += w;
            k += 1;
        }
    }
    if theme.rtl {
        let mut k = 0;
        while k < parts.len() {
            parts[k] = parts[k].mirror_in(Rect::from_size(size));
            k += 1;
        }
    }
    let mut node = Node::leaf(size);
    node.parts = parts;
    let (a, _, lh) = layout::metrics(&theme.font(TextRole::Body));
    node.baseline = (h - lh.ceil() as i64) / 2 + a as i64;
    node
}

pub fn paint_segmented(s: &Segmented, node: &Node, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let r = Rect::from_size(node.size());
    let radius = theme.radius();
    cx.fill_round(r, radius, theme.surface);
    let mut i = 0;
    while i < s.options.len() && i < node.parts.len() {
        let (ref key, ref label) = s.options[i];
        let part = node.parts[i];
        let on = *key == s.selected;
        let pressed = !cx.disabled && cx.states.field(&s.id, "ppress", "") == format!("{}", i);
        if on {
            let color = if cx.disabled {
                theme.text_disabled
            } else if pressed {
                theme.accent.mix(Color::black(), 0.25)
            } else if option_lit(&s.id, i as i64, cx) {
                theme.accent.mix(Color::white(), 0.12)
            } else {
                theme.accent
            };
            cx.fill_round(part.inset(theme.px(2)), radius - theme.px(2), color)
        } else if pressed || (!cx.disabled && option_lit(&s.id, i as i64, cx)) {
            cx.fill_round(
                part.inset(theme.px(2)),
                radius - theme.px(2),
                if pressed { theme.pressed } else { theme.hover },
            )
        }
        if i > 0 && !on && s.options[i - 1].0 != s.selected {
            let line = theme.border;
            cx.fill(
                Rect::new(
                    part.x,
                    part.y + theme.px(8),
                    theme.border_width(),
                    part.height - theme.px(16),
                ),
                line,
            )
        }
        let style = theme.font(TextRole::Body);
        let l = layout::layout(
            label,
            &style,
            LayoutOptions::elided((part.width - theme.px(8)).max(1) as f64),
        );
        let a = cx.abs(part);
        let fg = if cx.disabled {
            theme.text_disabled
        } else if on {
            theme.accent_text
        } else {
            theme.text
        };
        layout::draw(
            &mut cx.canvas,
            &l,
            (a.x + (a.width - l.width.ceil() as i64) / 2) as f64,
            (a.y + (a.height - l.height.ceil() as i64) / 2) as f64,
            &style,
            fg,
        );
        i += 1;
    }
    cx.stroke_round(r, radius, theme.border_width(), theme.border);
    if cx.is_focused(&s.id) {
        cx.focus_ring(r, radius)
    }
}

pub fn segmented_event(
    s: &Segmented,
    node: &Node,
    ev: &Event,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    if cx.disabled {
        return;
    }
    let as_group = RadioGroup {
        id: s.id.clone(),
        options: s.options.clone(),
        selected: s.selected.clone(),
        horizontal: true,
        disabled_keys: vec![],
    };
    match ev {
        Event::PointerDown { x, y, button, .. } if *button == MouseButton::Left => {
            cx.focus_on(&s.id);
            option_press(
                &s.id,
                part_at(node, x - cx.origin_x, y - cx.origin_y),
                &[],
                &s.options,
                st,
                cx,
            )
        }
        Event::PointerUp { x, y, button, .. } if *button == MouseButton::Left => option_release(
            &s.id,
            part_at(node, x - cx.origin_x, y - cx.origin_y),
            &s.options,
            &s.selected,
            st,
            cx,
        ),
        Event::PointerMove { x, y, .. } => option_hover(
            &s.id,
            part_at(node, x - cx.origin_x, y - cx.origin_y),
            st,
            cx,
        ),
        Event::PointerLeave => option_hover(&s.id, -1, st, cx),
        Event::CaptureLost => st.set_field(&s.id, "ppress", ""),
        Event::KeyDown { .. } => radio_group_event(&as_group, node, ev, st, cx),
        _ => (),
    }
}

pub fn segmented_semantics(s: &Segmented) -> Option<AccessNode> {
    let mut g = AccessNode::control(Role::RadioGroup, &s.id, "", "");
    let mut i = 0;
    while i < s.options.len() {
        let (ref key, ref label) = s.options[i];
        let mut item = AccessNode::new(Role::RadioButton, label);
        item.id = format!("{}#{}", s.id, key);
        item.checked = if *key == s.selected { 1 } else { 0 };
        item.actions.push("select".to_string());
        item.pos_in_set = (i + 1) as i64;
        item.set_size = s.options.len() as i64;
        g.children.push(item);
        i += 1;
    }
    Some(g)
}
#[cfg(test)]
mod session_tests {
    use crate::strext::StrExt;
    use crate::widgets::Widget;
    use crate::widgets::cx::{CheckState, UiMsg};
    use windowing::{Key, Mods};

    #[derive(Debug, Clone, Default, PartialEq)]
    struct Model {
        agree: bool,
        all: CheckState,
        size: String,
        view: String,
        log: Vec<String>,
    }

    fn update(m: Model, msg: UiMsg) -> Model {
        let mut n = m;
        match msg {
            UiMsg::Click(id) => n.log.push(format!("click {}", id)),
            UiMsg::Toggle(id, on) => {
                n.log.push(format!("toggle {} {}", id, on));
                if id == "agree" {
                    n.agree = on
                }
            }
            UiMsg::Check(id, state) => {
                n.log.push(format!("check {} {:?}", id, state));
                n.all = state;
            }
            UiMsg::Choose(id, key) => {
                n.log.push(format!("choose {} {}", id, key));
                if id == "size" {
                    n.size = key.to_string()
                } else {
                    n.view = key.to_string()
                }
            }
            _ => (),
        }
        n
    }

    fn view(m: &Model) -> Widget {
        let mut sizes = Widget::radio_group(
            "size",
            &[
                ("s".to_string(), "Small".to_string()),
                ("m".to_string(), "Medium".to_string()),
                ("l".to_string(), "Large".to_string()),
                ("xl".to_string(), "Huge".to_string()),
            ],
            &m.size,
            false,
        );
        if let Widget::RadioGroup(r) = sizes {
            let mut g = r;
            g.disabled_keys = vec!["l".to_string()];
            sizes = Widget::RadioGroup(g);
        }
        Widget::column(
            &[
                Widget::button("go", "Go"),
                Widget::label("plain text"),
                Widget::checkbox("agree", "Agree", m.agree),
                Widget::tristate("all", "Select all", m.all, true),
                sizes,
                Widget::segmented(
                    "view",
                    &[
                        ("list".to_string(), "List".to_string()),
                        ("grid".to_string(), "Grid".to_string()),
                    ],
                    &m.view,
                ),
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

    fn start(initial: Model) -> crate::Session {
        crate::application(
            "Actions",
            "dev.gossamer.gui-actions-test",
            move || initial.clone(),
            update,
            view,
        )
        .size(420, 520)
        .start()
        .unwrap()
    }

    fn centre(s: &crate::Session, id: &str) -> (i64, i64) {
        let r = s.rect_of(id).unwrap();
        (r.center_x(), r.center_y())
    }

    #[test]
    fn a_button_activates_only_on_a_release_over_where_it_was_pressed() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = start(Model::default());
        let (bx, by) = centre(&s, "go");
        let (lx, ly) = centre(&s, "agree");
        // Pressed and released over the button: one click.
        assert!(s.click("go").unwrap());
        // Pressed, dragged off and released elsewhere: cancelled.
        s.press_pointer(bx, by).unwrap();
        s.move_pointer(bx, by + 200, true).unwrap();
        s.release_pointer(bx, by + 200).unwrap();
        // Pressed elsewhere and released over the button: nothing.
        let (px, py) = centre(&s, "size");
        s.press_pointer(px, py + 200).unwrap();
        s.release_pointer(bx, by).unwrap();
        s.press_pointer(lx, ly + 300).unwrap();
        s.release_pointer(bx, by).unwrap();
        assert_eq!(log(&s), "click go");
        s.shutdown()
    }

    #[test]
    fn keys_activate_a_button_once_and_focus_loss_cancels_space() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = start(Model::default());
        assert!(s.focus("go").unwrap());
        // Enter activates on the press and ignores auto-repeat.
        s.hold_key(Key::Return, Mods::default(), false).unwrap();
        s.hold_key(Key::Return, Mods::default(), true).unwrap();
        s.release_key(Key::Return, Mods::default()).unwrap();
        // Space activates on release.
        s.hold_key(Key::Space, Mods::default(), false).unwrap();
        assert_eq!(log(&s), "click go");
        s.release_key(Key::Space, Mods::default()).unwrap();
        // Space held while focus moves away is cancelled.
        s.hold_key(Key::Space, Mods::default(), false).unwrap();
        assert!(s.focus("agree").unwrap());
        s.release_key(Key::Space, Mods::default()).unwrap();
        assert_eq!(log(&s), "click go; click go");
        s.shutdown()
    }

    #[test]
    fn checkboxes_change_on_release_and_tri_state_cycles_through_mixed() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = start(Model::default());
        assert!(s.focus("agree").unwrap());
        s.hold_key(Key::Space, Mods::default(), false).unwrap();
        assert_eq!(log(&s), "");
        s.release_key(Key::Space, Mods::default()).unwrap();
        let (ax, ay) = centre(&s, "agree");
        s.press_pointer(ax, ay).unwrap();
        s.release_pointer(ax, ay + 300).unwrap();
        assert!(s.click("all").unwrap());
        assert!(s.click("all").unwrap());
        assert!(s.click("all").unwrap());
        assert_eq!(
            log(&s),
            "toggle agree true; check all Checked; check all Mixed; check all Unchecked"
        );
        let dump = s.semantics(0).dump();
        assert!(
            dump.contains("checkbox \"Agree\" [checked]")
                && dump.contains("checkbox \"Select all\" [focused]"),
            "{}",
            dump
        );
        // The model's Mixed shows as the mixed state.
        assert!(s.click("all").unwrap());
        assert!(s.click("all").unwrap());
        assert!(
            s.semantics(0)
                .dump()
                .contains("checkbox \"Select all\" [focused mixed]"),
            "{}",
            s.semantics(0).dump()
        );
        s.shutdown()
    }

    #[test]
    fn radios_choose_once_on_release_and_skip_disabled_options() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = start(Model {
            size: "m".to_string(),
            ..Model::default()
        });
        let r = s.rect_of("size").unwrap();
        // Rows are stacked; the first is Small.
        let row = r.height / 4;
        let sx = r.x + 20;
        let sy = r.y + row / 2;
        // Released off the option: cancelled. Released on it: chosen.
        s.press_pointer(sx, sy).unwrap();
        s.release_pointer(sx, sy + 300).unwrap();
        s.press_pointer(sx, sy).unwrap();
        assert_eq!(log(&s), "");
        s.release_pointer(sx, sy).unwrap();
        // Choosing the selected option again reports nothing.
        s.click_at(sx, sy).unwrap();
        // Arrows skip the disabled Large and wrap.
        assert!(s.focus("size").unwrap());
        s.press(Key::Down, Mods::default()).unwrap();
        s.press(Key::Down, Mods::default()).unwrap();
        s.press(Key::Down, Mods::default()).unwrap();
        s.press(Key::Up, Mods::default()).unwrap();
        assert_eq!(
            log(&s),
            "choose size s; choose size m; choose size xl; choose size s; choose size xl"
        );
        s.shutdown()
    }

    fn menu_view(m: &Model) -> Widget {
        let items = vec![
            crate::widgets::menus::labeled("doc.export", "Export"),
            crate::widgets::menus::labeled("doc.print", "Print"),
        ];
        Widget::column(
            &[
                Widget::menu_button("file", "File", &items),
                Widget::split_button("save", "Save", &items),
                Widget::label(&format!("log {}", m.log.join("; "))),
            ],
            8,
        )
        .padding(8)
    }

    fn menu_update(m: Model, msg: UiMsg) -> Model {
        let mut n = m;
        match msg {
            UiMsg::Click(id) => n.log.push(format!("click {}", id)),
            UiMsg::Command(id) => n.log.push(format!("command {}", id)),
            _ => (),
        }
        n
    }

    #[test]
    fn menu_and_split_buttons_open_their_menus_and_run_items() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Menus",
            "dev.gossamer.gui-menu-button-test",
            Model::default,
            menu_update,
            menu_view,
        )
        .size(420, 360)
        .start()
        .unwrap();
        // A menu button opens on press; a second press on it closes the menu.
        assert!(s.click("file").unwrap());
        assert_eq!(
            s.popup_owners()
                .iter()
                .map(|p| p.0.clone())
                .collect::<Vec<_>>(),
            vec!["file"]
        );
        assert!(
            s.semantics(0)
                .dump()
                .contains("button \"File\" [focused expanded]"),
            "{}",
            s.semantics(0).dump()
        );
        assert!(s.click("file").unwrap());
        assert!(s.popup_owners().is_empty(), "{:?}", s.popup_owners());
        // From the keyboard it opens on Enter with the first item highlighted.
        s.press(Key::Return, Mods::default()).unwrap();
        s.press(Key::Return, Mods::default()).unwrap();
        assert!(s.popup_owners().is_empty(), "{:?}", s.popup_owners());
        // A split button's main part clicks; Down opens its menu.
        let r = s.rect_of("save").unwrap();
        s.click_at(r.x + r.width / 3, r.center_y()).unwrap();
        assert!(s.popup_owners().is_empty(), "{:?}", s.popup_owners());
        s.press(Key::Down, Mods::default()).unwrap();
        s.press(Key::Down, Mods::default()).unwrap();
        s.press(Key::Return, Mods::default()).unwrap();
        // Its arrow part opens the menu without clicking.
        s.click_at(r.x + r.width - 6, r.center_y()).unwrap();
        assert_eq!(
            s.popup_owners()
                .iter()
                .map(|p| p.0.clone())
                .collect::<Vec<_>>(),
            vec!["save"]
        );
        s.press(Key::Escape, Mods::default()).unwrap();
        // Assistive technology opens it through the "More" button.
        assert!(
            s.semantics(0)
                .dump()
                .contains("button \"More Save options\" [collapsed]"),
            "{}",
            s.semantics(0).dump()
        );
        s.accessible_action("save", "save#menu", "press", "")
            .unwrap();
        assert_eq!(
            s.popup_owners()
                .iter()
                .map(|p| p.0.clone())
                .collect::<Vec<_>>(),
            vec!["save"]
        );
        assert!(
            s.semantics(0).dump().contains("menuitem \"Export\""),
            "{}",
            s.semantics(0).dump()
        );
        assert_eq!(log(&s), "command doc.export; click save; command doc.print");
        s.shutdown()
    }

    #[test]
    fn a_segment_already_chosen_reports_nothing() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = start(Model {
            view: "list".to_string(),
            ..Model::default()
        });
        let r = s.rect_of("view").unwrap();
        let ly = r.center_y();
        s.click_at(r.x + r.width / 4, ly).unwrap();
        s.click_at(r.x + r.width * 3 / 4, ly).unwrap();
        s.press_pointer(r.x + r.width / 4, ly).unwrap();
        s.release_pointer(r.x + r.width / 4, ly + 300).unwrap();
        assert!(s.focus("view").unwrap());
        s.press(Key::Right, Mods::default()).unwrap();
        assert_eq!(log(&s), "choose view grid; choose view list");
        s.shutdown()
    }
}
