// menus: menu bars, pull-down and context menus, and submenus over the
// shared popup layer. Items name commands, so labels, shortcuts, and
// enabled/checked state come from the window's command registry and a
// disabled command cannot run from a menu.
//
// Keyboard: F10 or Alt focuses the bar; Left/Right move between menus;
// Down/Enter/Space open; inside a menu Up/Down/Home/End move (skipping
// separators and disabled items), Right opens a submenu, Left closes it,
// Enter activates, Escape closes one level, a letter jumps to the next
// item starting with it, and Alt+mnemonic opens a menu from anywhere.
// Pointer: click opens, hovering another title switches menus, hovering
// a submenu row opens it after a short delay, and an outside click
// closes everything without reaching what is under it.
use crate::strext::StrExt;
use windowing::{Key, MouseButton};

use crate::accessibility::{AccessNode, Role};
use crate::commands::Command;
use crate::geometry::{Constraints, Rect, Size};
use crate::render::icons::Icon;
use crate::style::{TextRole, Theme};
use crate::text::layout;
use crate::text::layout::LayoutOptions;
use crate::widgets::Widget;
use crate::widgets::cx::{
    Event, EventCx, LayoutCx, Node, PaintCx, Placement, PopupRequest, command,
};
use crate::widgets::state::{StateStore, WState};

/// One menu entry.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MenuItem {
    /// Runs `command`; `label` overrides the command's label when set.
    Action {
        command: String,
        label: String,
        icon: Option<Icon>,
    },
    /// A radio entry: checked when its command is checked; one of a group.
    Radio {
        command: String,
        label: String,
    },
    Submenu {
        label: String,
        items: Vec<MenuItem>,
    },
    Separator,
    /// A heading row that cannot be chosen.
    Header(String),
}

/// A titled menu (a menu bar entry or a context menu body).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Menu {
    /// `&` marks the mnemonic letter: `&File`.
    pub label: String,
    pub items: Vec<MenuItem>,
}

impl Menu {
    pub fn new(label: &str, items: &[MenuItem]) -> Menu {
        Menu {
            label: label.to_string(),
            items: items.to_vec(),
        }
    }
}

/// An action entry for `command`.
pub fn item(command: &str) -> MenuItem {
    MenuItem::Action {
        command: command.to_string(),
        label: String::new(),
        icon: None,
    }
}

/// An action entry with its own label.
pub fn labeled(command: &str, label: &str) -> MenuItem {
    MenuItem::Action {
        command: command.to_string(),
        label: label.to_string(),
        icon: None,
    }
}

pub fn radio(command: &str, label: &str) -> MenuItem {
    MenuItem::Radio {
        command: command.to_string(),
        label: label.to_string(),
    }
}

pub fn submenu(label: &str, items: &[MenuItem]) -> MenuItem {
    MenuItem::Submenu {
        label: label.to_string(),
        items: items.to_vec(),
    }
}

pub fn separator() -> MenuItem {
    MenuItem::Separator
}

/// Open-menu state of a menu bar or context menu.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MenuState {
    /// The open bar menu, or -1 (context menus use 0).
    pub open: i64,
    /// The highlighted row per open level.
    pub path: Vec<i64>,
    /// The bar title focused for keyboard navigation (menu bar only).
    pub title: i64,
    pub keyboard: bool,
    /// Where a context menu was requested (window pixels).
    pub at_x: i64,
    pub at_y: i64,
    pub typeahead: String,
    pub typeahead_time: i64,
}

impl Default for MenuState {
    fn default() -> MenuState {
        MenuState {
            open: -1,
            path: vec![],
            title: 0,
            keyboard: false,
            at_x: 0,
            at_y: 0,
            typeahead: String::new(),
            typeahead_time: 0,
        }
    }
}

/// A horizontal bar of menus.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MenuBar {
    pub id: String,
    pub menus: Vec<Menu>,
    /// Joins the bar to the content below it: the bar takes the field
    /// color a text area or terminal fills with and draws no dividing line.
    #[serde(default)]
    pub joined: bool,
}

/// A widget with a context menu on right-click, Shift+F10, or the Menu key.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContextMenu {
    pub id: String,
    pub child: Box<Widget>,
    pub items: Vec<MenuItem>,
}

/// The visible label and mnemonic letter of `label` (`&File` -> "File", 'f').
pub fn mnemonic(label: &str) -> (String, String) {
    let mut out = String::new();
    let mut key = String::new();
    let mut amp = false;
    for c in label.chars() {
        if amp {
            if c != '&' && key.is_empty() {
                key = format!("{}", c).to_lowercase()
            };
            out.push(c);
            amp = false;
        } else if c == '&' {
            amp = true;
        } else {
            out.push(c)
        }
    }
    (out, key)
}

// The byte offset of the mnemonic letter in the visible label, or -1.
fn mnemonic_offset(label: &str) -> i64 {
    let mut at = 0;
    let mut amp = false;
    for c in label.chars() {
        if amp {
            if c != '&' {
                return at;
            };
            at += 1;
            amp = false;
        } else if c == '&' {
            amp = true;
        } else {
            at += c.len_utf8() as i64;
        }
    }
    -1
}

fn item_label(it: &MenuItem, registry: &[Command]) -> String {
    match it {
        MenuItem::Action {
            command: cmd,
            label,
            ..
        }
        | MenuItem::Radio {
            command: cmd,
            label,
        } => {
            if !label.is_empty() {
                return label.to_string();
            };
            match command(registry, cmd) {
                Some(c) => c.label,
                None => cmd.to_string(),
            }
        }
        MenuItem::Submenu { label, .. } => label.to_string(),
        MenuItem::Header(t) => t.to_string(),
        MenuItem::Separator => String::new(),
    }
}

fn item_enabled(it: &MenuItem, registry: &[Command]) -> bool {
    match it {
        MenuItem::Action { command: cmd, .. } | MenuItem::Radio { command: cmd, .. } => {
            match command(registry, cmd) {
                Some(c) => c.enabled && c.visible,
                None => true,
            }
        }
        MenuItem::Submenu { items, .. } => !items.is_empty(),
        _ => false,
    }
}

fn item_checked(it: &MenuItem, registry: &[Command]) -> i64 {
    match it {
        MenuItem::Action { command: cmd, .. } | MenuItem::Radio { command: cmd, .. } => {
            match command(registry, cmd) {
                Some(c) => c.checked,
                None => -1,
            }
        }
        _ => -1,
    }
}

fn item_shortcut(it: &MenuItem, registry: &[Command]) -> String {
    match it {
        MenuItem::Action { command: cmd, .. } | MenuItem::Radio { command: cmd, .. } => {
            match command(registry, cmd) {
                Some(c) => c.shortcut_label(),
                None => String::new(),
            }
        }
        _ => String::new(),
    }
}

// The entries a menu shows: hidden commands are left out, and so is any
// separator that would then open or close the menu or follow another.
fn visible(items: &[MenuItem], registry: &[Command]) -> Vec<MenuItem> {
    let mut out: Vec<MenuItem> = Vec::with_capacity(items.len());
    for it in items {
        let shown = match it {
            MenuItem::Action { command: cmd, .. } | MenuItem::Radio { command: cmd, .. } => {
                command(registry, &(*cmd).to_string()).is_none_or(|c| c.visible)
            }
            MenuItem::Separator => out.last().is_some_and(|last| *last != MenuItem::Separator),
            _ => true,
        };
        if shown {
            out.push(it.clone());
        }
    }
    if out.last() == Some(&MenuItem::Separator) {
        out.pop();
    }
    out
}

// ---- geometry ----

// The height of one line of menu text.
fn menu_line(theme: &Theme) -> i64 {
    layout::measure("Mg", &theme.font(TextRole::Menu)).1.ceil() as i64
}

fn row_h(theme: &Theme) -> i64 {
    menu_line(theme) + theme.px(12)
}

fn sep_h(theme: &Theme) -> i64 {
    theme.px(9)
}

fn pad(theme: &Theme) -> i64 {
    theme.px(5)
}

/// The panel size of a menu's items.
pub fn panel_size(items: &[MenuItem], theme: &Theme, registry: &[Command]) -> Size {
    let body = theme.font(TextRole::Menu);
    let small = body.clone();
    let mut w = theme.px(200);
    let mut h = pad(theme) * 2;
    for it in visible(items, registry) {
        match it {
            MenuItem::Separator => h += sep_h(theme),
            _ => {
                let (label, _) = mnemonic(&item_label(&it, registry));
                let (lw, _) = layout::measure(&label, &body);
                let (sw, _) = layout::measure(&item_shortcut(&it, registry), &small);
                w = w.max(
                    theme.px(36)
                        + lw.ceil() as i64
                        + theme.px(32)
                        + sw.ceil() as i64
                        + theme.px(28),
                );
                h += row_h(theme);
            }
        }
    }
    Size::new(w, h)
}

// The local y of row `index` in a panel, and its height.
fn row_rect(items: &[MenuItem], theme: &Theme, width: i64, index: i64) -> Rect {
    let mut y = pad(theme);
    let mut i = 0;
    while i < items.len() {
        let h = match items[i] {
            MenuItem::Separator => sep_h(theme),
            _ => row_h(theme),
        };
        if i == index as usize {
            return Rect::new(pad(theme), y, width - pad(theme) * 2, h);
        }
        y += h;
        i += 1;
    }
    Rect::new(0, 0, 0, 0)
}

fn row_at(items: &[MenuItem], theme: &Theme, width: i64, ly: i64) -> i64 {
    let mut i = 0;
    while i < items.len() {
        let r = row_rect(items, theme, width, i as i64);
        if ly >= r.y && ly < r.bottom() {
            return i as i64;
        };
        i += 1;
    }
    -1
}

fn selectable(it: &MenuItem, registry: &[Command]) -> bool {
    match it {
        MenuItem::Separator | MenuItem::Header(_) => false,
        _ => item_enabled(it, registry),
    }
}

fn step(items: &[MenuItem], from: i64, dir: i64, registry: &[Command]) -> i64 {
    let n = items.len();
    if n == 0 {
        return -1;
    };
    let mut i = from;
    let mut tries = 0;
    loop {
        i = if i < 0 {
            if dir > 0 { 0 } else { (n - 1) as i64 }
        } else {
            (i + dir + n as i64) % n as i64
        };
        tries += 1;
        if selectable(&items[i as usize], registry) || tries > n {
            break;
        }
    }
    if tries > n { -1 } else { i }
}

// ---- menu bar ----

fn title_rects(b: &MenuBar, theme: &Theme, height: i64) -> Vec<Rect> {
    let body = theme.font(TextRole::Menu);
    let mut x = theme.px(4);
    let mut out = vec![];
    for m in &b.menus {
        let (label, _) = mnemonic(&m.label);
        let (w, _) = layout::measure(&label, &body);
        let tw = w.ceil() as i64 + theme.px(24);
        out.push(Rect::new(x, 0, tw, height));
        x += tw;
    }
    out
}

pub fn layout_menu_bar(b: &MenuBar, c: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    let h = menu_line(theme) + theme.px(10);
    let w = if c.has_bounded_width() {
        c.max_width
    } else {
        theme.px(400)
    };
    let size = c.constrain(Size::new(w, h));
    let mut n = Node::leaf(size);
    n.parts = title_rects(b, theme, h);
    if theme.rtl {
        let mut i = 0;
        while i < n.parts.len() {
            n.parts[i] = n.parts[i].mirror_in(Rect::from_size(size));
            i += 1;
        }
    }
    n
}

pub fn paint_menu_bar(b: &MenuBar, node: &Node, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let r = Rect::from_size(node.size());
    if b.joined {
        cx.fill(r, theme.field);
    } else {
        cx.fill(r, theme.surface_alt);
        cx.fill(
            Rect::new(
                0,
                r.height - theme.border_width(),
                r.width,
                theme.border_width(),
            ),
            theme.border,
        );
    }
    let st = cx.states.menu(&b.id);
    let focused = cx.is_focused(&b.id);
    let body = theme.font(TextRole::Menu);
    let mut i = 0;
    while i < b.menus.len() && i < node.parts.len() {
        let part = node.parts[i];
        let open = st.open == i as i64;
        let lit = open || (focused && st.title == i as i64);
        if lit {
            let bg = if open { theme.selection } else { theme.hover };
            cx.fill_round(part.inset(theme.px(3)), theme.px(5), bg)
        }
        let (label, _) = mnemonic(&b.menus[i].label);
        let l = layout::layout(&label, &body, LayoutOptions::single_line());
        let a = cx.abs(part);
        let fg = if open {
            theme.selection_text
        } else {
            theme.text
        };
        let tx = a.x + (a.width - l.width.ceil() as i64) / 2;
        let ty = a.y + (a.height - l.height.ceil() as i64) / 2;
        layout::draw(&mut cx.canvas, &l, tx as f64, ty as f64, &body, fg);
        if cx.mnemonics || focused {
            let mo = mnemonic_offset(&b.menus[i].label);
            if mo >= 0 {
                let x0 = l.caret_x(mo);
                let x1 = l.caret_x(l.next_boundary(mo));
                cx.canvas.rect(
                    tx + x0 as i64,
                    ty + (l.lines[0].baseline).ceil() as i64 + theme.px(2),
                    (x1 - x0).max(1.0) as i64,
                    theme.px(1).max(1),
                    fg,
                )
            }
        }
        i += 1;
    }
    if focused && st.open < 0 && st.title >= 0 && st.title < node.parts.len() as i64 {
        cx.focus_ring(
            node.parts[st.title as usize].inset(theme.px(3)),
            theme.px(5),
        )
    }
}

// The absolute anchor of title `i`.
fn title_anchor(node: &Node, i: i64, cx: &EventCx) -> Rect {
    if i < 0 || i >= node.parts.len() as i64 {
        return Rect::new(0, 0, 0, 0);
    };
    cx.to_window(node.parts[i as usize])
}

fn open_bar_menu(
    b: &MenuBar,
    node: &Node,
    i: i64,
    keyboard: bool,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    if i < 0 || i >= b.menus.len() as i64 {
        return;
    };
    let mut s = st.menu(&b.id);
    s.open = i;
    s.title = i;
    s.keyboard = keyboard;
    let items = visible(&b.menus[i as usize].items, &cx.registry);
    s.path = vec![if keyboard {
        step(&items, -1, 1, &cx.registry)
    } else {
        -1
    }];
    st.set(&b.id, WState::Menu(s));
    cx.close_popup(&b.id, 0);
    let size = panel_size(&items, &cx.theme, &cx.registry);
    cx.open_popup(&PopupRequest::new(
        &b.id,
        0,
        title_anchor(node, i, cx),
        size,
        Placement::Below,
        true,
        true,
    ));
    cx.announce(&format!("{} menu", mnemonic(&b.menus[i as usize].label).0))
}

pub fn menu_bar_event(b: &MenuBar, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    let mut s = st.menu(&b.id);
    match ev {
        Event::PointerDown { x, y, button, .. } => {
            if *button != MouseButton::Left {
                return;
            };
            let lx = x - cx.origin_x;
            let ly = y - cx.origin_y;
            let mut i = 0;
            while i < node.parts.len() {
                if node.parts[i].contains(lx, ly) {
                    if s.open == i as i64 {
                        close_all(&b.id, st, cx)
                    } else {
                        open_bar_menu(b, node, i as i64, false, st, cx)
                    }
                    cx.consume();
                    return;
                }
                i += 1;
            }
        }
        Event::PointerMove { x, y, .. } if s.open >= 0 => {
            let lx = x - cx.origin_x;
            let ly = y - cx.origin_y;
            let mut i = 0;
            while i < node.parts.len() {
                if node.parts[i].contains(lx, ly) && i != s.open as usize {
                    open_bar_menu(b, node, i as i64, false, st, cx);
                    return;
                }
                i += 1;
            }
        }
        Event::KeyDown { key, mods, .. } => {
            let n = b.menus.len();
            if n == 0 {
                return;
            };
            if mods.alt && !mods.ctrl {
                // Alt+mnemonic opens a menu from anywhere.
                let letter = crate::input::key_name(*key).to_lowercase();
                let mut i = 0;
                while i < n {
                    if mnemonic(&b.menus[i].label).1 == letter {
                        open_bar_menu(b, node, i as i64, true, st, cx);
                        cx.consume();
                        return;
                    }
                    i += 1;
                }
            }
            if !cx.is_focused(&b.id) {
                if *key == Key::F10 && !mods.shift {
                    s.title = 0;
                    st.set(&b.id, WState::Menu(s));
                    cx.focus_on(&b.id);
                    cx.consume()
                }
                return;
            }
            let back = if cx.theme.rtl { Key::Right } else { Key::Left };
            let fwd = if cx.theme.rtl { Key::Left } else { Key::Right };
            if *key == fwd || *key == back {
                s.title = (s.title + if *key == fwd { 1 } else { -1 } + n as i64) % n as i64;
                st.set(&b.id, WState::Menu(s));
                cx.redraw = true;
                cx.consume()
            } else if *key == Key::Down || *key == Key::Return || *key == Key::Space {
                open_bar_menu(b, node, s.title, true, st, cx);
                cx.consume()
            } else if *key == Key::Escape || *key == Key::F10 {
                cx.focus_on("");
                cx.consume()
            }
        }
        _ => (),
    }
}

/// Closes every open level of the menu `owner`.
fn close_all(owner: &str, st: &mut StateStore, cx: &mut EventCx) {
    let mut s = st.menu(owner);
    s.open = -1;
    s.path = vec![];
    st.set(owner, WState::Menu(s));
    cx.close_popup(owner, 0)
}

// The items shown at `level` for the open menu.
fn level_items(
    root: &[MenuItem],
    s: &MenuState,
    level: i64,
    registry: &[Command],
) -> Vec<MenuItem> {
    let mut items = visible(root, registry);
    let mut k = 0;
    while k < level {
        if k >= s.path.len() as i64 {
            return vec![];
        };
        let at = s.path[k as usize];
        if at < 0 || at >= items.len() as i64 {
            return vec![];
        };
        match items[at as usize] {
            MenuItem::Submenu { items: ref sub, .. } => items = visible(sub, registry),
            _ => return vec![],
        }
        k += 1;
    }
    items
}

/// Paints the panel of `level` of a menu whose top items are `root`.
/// The size the open menu `owner` needs at `level` for the entries the
/// registry shows now.
pub fn panel_extent(
    root: &[MenuItem],
    owner: &str,
    level: i64,
    theme: &Theme,
    states: &StateStore,
    registry: &[Command],
) -> Size {
    let s = states.menu(owner);
    panel_size(&level_items(root, &s, level, registry), theme, registry)
}

pub fn paint_panel(root: &[MenuItem], owner: &str, level: i64, rect: Rect, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let s = cx.states.menu(owner);
    let items = level_items(root, &s, level, &cx.registry);
    let highlighted = if level < s.path.len() as i64 {
        s.path[level as usize]
    } else {
        -1
    };
    let radius = theme.radius();
    let (offset, blur) = theme.shadow_of(theme.shape.popup_elevation);
    cx.canvas.shadow(
        rect.x,
        rect.y + offset,
        rect.width,
        rect.height,
        radius,
        blur,
        theme.shadow,
    );
    cx.canvas.round_rect(
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        radius,
        theme.surface,
    );
    cx.canvas.round_rect_stroke(
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        radius,
        theme.border_width(),
        theme.border,
    );
    let body = theme.font(TextRole::Menu);
    let small = body.clone();
    let saved = cx.canvas.clip;
    cx.canvas.push_clip(rect);
    let mut i = 0;
    while i < items.len() {
        let it = &items[i];
        let r = row_rect(&items, &theme, rect.width, i as i64).offset(rect.x, rect.y);
        match it {
            MenuItem::Separator => cx.canvas.rect(
                r.x + theme.px(8),
                r.y + r.height / 2,
                r.width - theme.px(16),
                theme.border_width(),
                theme.border,
            ),
            _ => {
                let enabled = item_enabled(it, &cx.registry);
                let lit = i == highlighted as usize && enabled;
                if lit {
                    cx.canvas
                        .round_rect(r.x, r.y, r.width, r.height, theme.px(5), theme.selection)
                }
                let fg = if !enabled {
                    theme.text_disabled
                } else if lit {
                    theme.selection_text
                } else {
                    theme.text
                };
                let checked = item_checked(it, &cx.registry);
                let is_radio = matches!(it, MenuItem::Radio { .. });
                let isz = theme.px(theme.shape.field_icon);
                if checked == 1 {
                    if is_radio {
                        cx.canvas
                            .circle(r.x + theme.px(16), r.y + r.height / 2, theme.px(4), fg)
                    } else {
                        cx.icon(
                            &Icon::Check,
                            r.x + theme.px(7),
                            r.y + (r.height - isz) / 2,
                            isz,
                            fg,
                        )
                    }
                }
                if let MenuItem::Action { icon, .. } = it
                    && let Some(own) = icon
                    && checked < 0
                {
                    cx.icon(own, r.x + theme.px(7), r.y + (r.height - isz) / 2, isz, fg)
                }
                let raw = item_label(it, &cx.registry);
                let (label, _) = mnemonic(&raw);
                let is_header = matches!(it, MenuItem::Header(_));
                let style = if is_header {
                    small.with_bold(true)
                } else {
                    body.clone()
                };
                let l = layout::layout(&label, &style, LayoutOptions::single_line());
                let tx = r.x + theme.px(32);
                let ty = r.y + (r.height - l.height.ceil() as i64) / 2;
                layout::draw(
                    &mut cx.canvas,
                    &l,
                    tx as f64,
                    ty as f64,
                    &style,
                    if is_header { theme.text_muted } else { fg },
                );
                let mo = mnemonic_offset(&raw);
                if mo >= 0 && (s.keyboard || cx.mnemonics) {
                    let x0 = l.caret_x(mo);
                    let x1 = l.caret_x(l.next_boundary(mo));
                    cx.canvas.rect(
                        tx + x0 as i64,
                        ty + (l.lines[0].baseline).ceil() as i64 + theme.px(2),
                        (x1 - x0).max(1.0) as i64,
                        theme.px(1).max(1),
                        fg,
                    )
                }
                let sc = item_shortcut(it, &cx.registry);
                if !sc.is_empty() {
                    let sl = layout::layout(&sc, &small, LayoutOptions::single_line());
                    let muted = if lit {
                        theme.selection_text
                    } else {
                        theme.text_muted
                    };
                    layout::draw(
                        &mut cx.canvas,
                        &sl,
                        (r.right() - theme.px(12) - sl.width.ceil() as i64) as f64,
                        (r.y + (r.height - sl.height.ceil() as i64) / 2) as f64,
                        &small,
                        muted,
                    )
                }
                if let MenuItem::Submenu { .. } = it {
                    let arrow = if theme.rtl {
                        Icon::ChevronLeft
                    } else {
                        Icon::ChevronRight
                    };
                    cx.icon(
                        &arrow,
                        r.right() - theme.px(22),
                        r.y + (r.height - isz) / 2,
                        isz,
                        fg,
                    )
                }
            }
        }
        i += 1;
    }
    cx.canvas.pop_clip(saved)
}

// Opens the submenu at row `row` of `level` beside the panel.
fn open_sub(
    root: &[MenuItem],
    owner: &str,
    level: i64,
    row: i64,
    panel: Rect,
    keyboard: bool,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    let mut s = st.menu(owner);
    let items = level_items(root, &s, level, &cx.registry);
    if row < 0 || row >= items.len() as i64 {
        return;
    };
    let MenuItem::Submenu { items: ref sub, .. } = items[row as usize] else {
        return;
    };
    while s.path.len() > (level + 1) as usize {
        let _ = s.path.pop();
    }
    if s.path.len() <= level as usize {
        s.path.push(row)
    } else {
        s.path[level as usize] = row
    };
    let sub_items = visible(sub, &cx.registry);
    s.path.push(if keyboard {
        step(&sub_items, -1, 1, &cx.registry)
    } else {
        -1
    });
    st.set(owner, WState::Menu(s));
    let r = row_rect(&items, &cx.theme, panel.width, row).offset(panel.x, panel.y);
    let size = panel_size(&sub_items, &cx.theme, &cx.registry);
    cx.open_popup(&PopupRequest::new(
        owner,
        level + 1,
        Rect::new(panel.x, r.y - pad(&cx.theme), panel.width, r.height),
        size,
        Placement::Beside,
        true,
        true,
    ))
}

fn activate(
    root: &[MenuItem],
    owner: &str,
    level: i64,
    row: i64,
    panel: Rect,
    keyboard: bool,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    let s = st.menu(owner);
    let items = level_items(root, &s, level, &cx.registry);
    if row < 0 || row >= items.len() as i64 || !item_enabled(&items[row as usize], &cx.registry) {
        return;
    };
    match &items[row as usize] {
        MenuItem::Action { command: cmd, .. } | MenuItem::Radio { command: cmd, .. } => {
            close_all(owner, st, cx);
            if cx.focus == owner {
                cx.focus_on("")
            };
            cx.run_command(cmd)
        }
        MenuItem::Submenu { .. } => open_sub(root, owner, level, row, panel, keyboard, st, cx),
        _ => (),
    }
}

/// Handles events of the panel at `level` of the menu `owner`.
pub fn panel_event(
    root: &[MenuItem],
    owner: &str,
    level: i64,
    rect: Rect,
    ev: &Event,
    st: &mut StateStore,
    cx: &mut EventCx,
    bar: Option<(MenuBar, Node)>,
) {
    let mut s = st.menu(owner);
    let items = level_items(root, &s, level, &cx.registry);
    while s.path.len() <= level as usize {
        s.path.push(-1)
    }
    match ev {
        Event::PointerMove { y, .. } => {
            let row = row_at(&items, &cx.theme, rect.width, y - rect.y);
            if row >= 0
                && row != s.path[level as usize]
                && selectable(&items[row as usize], &cx.registry)
            {
                s.path[level as usize] = row;
                while s.path.len() > (level + 1) as usize {
                    let _ = s.path.pop();
                }
                s.keyboard = false;
                st.set(owner, WState::Menu(s));
                cx.close_popup(owner, level + 1);
                if let MenuItem::Submenu { .. } = items[row as usize] {
                    st.set_field(
                        owner,
                        "hover",
                        &format!("{}:{}:{}:{}:{}", level, row, rect.x, rect.y, rect.width),
                    );
                    cx.start_timer(owner, "submenu", 250)
                }
                cx.redraw = true;
            }
            cx.consume()
        }
        Event::PointerUp { y, button, .. } => {
            if *button == MouseButton::Left || *button == MouseButton::Right {
                let row = row_at(&items, &cx.theme, rect.width, y - rect.y);
                if row >= 0 {
                    activate(root, owner, level, row, rect, false, st, cx)
                }
            }
            cx.consume()
        }
        Event::PointerDown { .. } => cx.consume(),
        Event::KeyDown { key, .. } => {
            let cur = s.path[level as usize];
            let back = if cx.theme.rtl { Key::Right } else { Key::Left };
            let fwd = if cx.theme.rtl { Key::Left } else { Key::Right };
            s.keyboard = true;
            if *key == Key::Down || *key == Key::Up {
                s.path[level as usize] = step(
                    &items,
                    cur,
                    if *key == Key::Down { 1 } else { -1 },
                    &cx.registry,
                );
                while s.path.len() > (level + 1) as usize {
                    let _ = s.path.pop();
                }
                st.set(owner, WState::Menu(s));
                cx.close_popup(owner, level + 1);
                cx.redraw = true;
            } else if *key == Key::Home || *key == Key::End {
                s.path[level as usize] = step(
                    &items,
                    if *key == Key::Home {
                        -1
                    } else {
                        items.len() as i64
                    },
                    if *key == Key::Home { 1 } else { -1 },
                    &cx.registry,
                );
                st.set(owner, WState::Menu(s));
                cx.redraw = true;
            } else if *key == fwd {
                let opens = cur >= 0
                    && cur < items.len() as i64
                    && matches!(items[cur as usize], MenuItem::Submenu { .. });
                st.set(owner, WState::Menu(s));
                if opens {
                    open_sub(root, owner, level, cur, rect, true, st, cx)
                } else if let Some((b, node)) = bar {
                    let n = b.menus.len();
                    open_bar_menu(
                        &b,
                        &node,
                        (st.menu(owner).open + 1) % n.max(1) as i64,
                        true,
                        st,
                        cx,
                    )
                }
            } else if *key == back {
                st.set(owner, WState::Menu(s));
                if level > 0 {
                    let mut s2 = st.menu(owner);
                    while s2.path.len() > level as usize {
                        let _ = s2.path.pop();
                    }
                    st.set(owner, WState::Menu(s2));
                    cx.close_popup(owner, level)
                } else if let Some((b, node)) = bar {
                    let n = b.menus.len();
                    open_bar_menu(
                        &b,
                        &node,
                        (st.menu(owner).open - 1 + n as i64) % n.max(1) as i64,
                        true,
                        st,
                        cx,
                    )
                }
            } else if *key == Key::Return || *key == Key::Space {
                st.set(owner, WState::Menu(s));
                activate(root, owner, level, cur, rect, true, st, cx)
            } else if *key == Key::Escape {
                st.set(owner, WState::Menu(s));
                if level > 0 {
                    let mut s2 = st.menu(owner);
                    while s2.path.len() > level as usize {
                        let _ = s2.path.pop();
                    }
                    st.set(owner, WState::Menu(s2));
                    cx.close_popup(owner, level)
                } else {
                    close_all(owner, st, cx);
                    if bar.is_some() {
                        cx.focus_on(owner)
                    }
                }
            } else {
                // Mnemonics, then typeahead on the first letter.
                let letter = crate::input::key_name(*key).to_lowercase();
                if letter.byte_len() == 1 {
                    let mut hit: i64 = -1;
                    let mut i = 0;
                    while i < items.len() {
                        if selectable(&items[i], &cx.registry)
                            && mnemonic(&item_label(&items[i], &cx.registry)).1 == letter
                        {
                            hit = i as i64;
                            break;
                        };
                        i += 1;
                    }
                    if hit >= 0 {
                        st.set(owner, WState::Menu(s));
                        activate(root, owner, level, hit, rect, true, st, cx);
                        cx.consume();
                        return;
                    }
                    let mut k = 1;
                    while k <= items.len() {
                        let i2 = (cur.max(-1) + k as i64 + items.len() as i64)
                            % (items.len().max(1)) as i64;
                        let label = mnemonic(&item_label(&items[i2 as usize], &cx.registry))
                            .0
                            .to_lowercase();
                        if selectable(&items[i2 as usize], &cx.registry)
                            && label.starts_with(&letter)
                        {
                            s.path[level as usize] = i2;
                            break;
                        }
                        k += 1;
                    }
                    st.set(owner, WState::Menu(s));
                    cx.redraw = true;
                }
            }
            cx.consume()
        }
        _ => (),
    }
}

/// The submenu hover delay elapsed.
pub fn hover_timer(root: &[MenuItem], owner: &str, st: &mut StateStore, cx: &mut EventCx) {
    let spec = st.field(owner, "hover", "");
    let parts = spec.split(":").collect::<Vec<_>>();
    if parts.len() != 5 {
        return;
    };
    let level = parts[0].to_i64().unwrap_or(-1);
    let row = parts[1].to_i64().unwrap_or(-1);
    let rect = Rect::new(
        parts[2].to_i64().unwrap_or(0),
        parts[3].to_i64().unwrap_or(0),
        parts[4].to_i64().unwrap_or(0),
        0,
    );
    let s = st.menu(owner);
    if level >= 0 && level < s.path.len() as i64 && s.path[level as usize] == row {
        open_sub(root, owner, level, row, rect, false, st, cx)
    }
}

pub fn menu_bar_semantics(b: &MenuBar, node: &Node, st: &StateStore) -> Option<AccessNode> {
    let mut bar = AccessNode::control(Role::MenuBar, &b.id, "", "");
    let s = st.menu(&b.id);
    let mut i = 0;
    while i < b.menus.len() {
        let (label, _) = mnemonic(&b.menus[i].label);
        let mut m = AccessNode::new(Role::MenuItem, &label);
        m.id = format!("{}#{}", b.id, i);
        m.expanded = if s.open == i as i64 { 1 } else { 0 };
        m.actions.push("open".to_string());
        if i < node.parts.len() {
            m.bounds = node.parts[i]
        };
        bar.children.push(m);
        i += 1;
    }
    Some(bar)
}

/// The accessibility tree of an open menu level (for the popup layer).
pub fn panel_semantics(
    root: &[MenuItem],
    owner: &str,
    level: i64,
    st: &StateStore,
    registry: &[Command],
) -> AccessNode {
    let s = st.menu(owner);
    let items = level_items(root, &s, level, registry);
    let mut menu = AccessNode::new(Role::Menu, "");
    menu.id = format!("{}#menu{}", owner, level);
    let mut i = 0;
    let n = items.len();
    while i < n {
        let it = &items[i];
        match it {
            MenuItem::Separator => menu.children.push(AccessNode::new(Role::Separator, "")),
            _ => {
                let (label, _) = mnemonic(&item_label(it, registry));
                let checked = item_checked(it, registry);
                let role = match it {
                    MenuItem::Radio { .. } => Role::MenuItemRadio,
                    _ => {
                        if checked >= 0 {
                            Role::MenuItemCheck
                        } else {
                            Role::MenuItem
                        }
                    }
                };
                let mut node = AccessNode::new(role, &label);
                node.disabled = !item_enabled(it, registry);
                node.checked = checked;
                node.selected = level < s.path.len() as i64 && s.path[level as usize] == i as i64;
                node.description = item_shortcut(it, registry);
                node.pos_in_set = (i + 1) as i64;
                node.set_size = n as i64;
                if let MenuItem::Submenu { .. } = it {
                    node.expanded = if s.path.len() > (level + 1) as usize
                        && s.path[level as usize] == i as i64
                    {
                        1
                    } else {
                        0
                    }
                };
                node.actions.push("press".to_string());
                menu.children.push(node)
            }
        }
        i += 1;
    }
    menu
}

// ---- context menus ----

/// Opens the top-level panel of the menu `owner` with `items`, placed against
/// `anchor` (window coordinates) by `placement`. Opened from the keyboard, the
/// first selectable item is highlighted; from the pointer, none is. The panel
/// is then driven by `paint_panel`, `panel_event`, and `hover_timer`.
pub fn open_menu(
    owner: &str,
    items: &[MenuItem],
    anchor: Rect,
    placement: Placement,
    keyboard: bool,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    let mut s = st.menu(owner);
    s.open = 0;
    s.keyboard = keyboard;
    let shown = visible(items, &cx.registry);
    s.path = vec![if keyboard {
        step(&shown, -1, 1, &cx.registry)
    } else {
        -1
    }];
    st.set(owner, WState::Menu(s));
    let size = panel_size(&shown, &cx.theme, &cx.registry);
    cx.open_popup(&PopupRequest::new(
        owner, 0, anchor, size, placement, true, true,
    ))
}

pub fn context_event(
    m: &ContextMenu,
    node: &Node,
    ev: &Event,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    match ev {
        Event::PointerDown { x, y, button, .. } if *button == MouseButton::Right => {
            open_menu(
                &m.id,
                &m.items,
                Rect::new(*x, *y, 1, 1),
                Placement::AtPoint,
                false,
                st,
                cx,
            );
            cx.consume()
        }
        Event::Command(name) if name == "gui.context-menu" => {
            let r = cx.to_window(Rect::from_size(node.size()));
            open_menu(
                &m.id,
                &m.items,
                Rect::new(r.x + r.width / 4, r.y + r.height / 2, 1, 1),
                Placement::AtPoint,
                true,
                st,
                cx,
            );
            cx.consume()
        }
        _ => (),
    }
}

pub fn layout_context(m: &ContextMenu, c: Constraints, cx: &mut LayoutCx) -> Node {
    let child = m.child.layout(c, cx);
    let mut n = Node::leaf(child.size());
    n.baseline = child.baseline;
    n.children = vec![child.at(0, 0)];
    n
}

/// Opens or reports the menu whose popup level a session event targets.
pub fn bar_menu_items(b: &MenuBar, st: &StateStore) -> Vec<MenuItem> {
    let s = st.menu(&b.id);
    if s.open < 0 || s.open >= b.menus.len() as i64 {
        vec![]
    } else {
        b.menus[s.open as usize].items.clone()
    }
}

/// The native menu bar `menus` describe, for `windowing`'s `App::set_menu_bar`:
/// each command entry gets identity `i + 1`, where entry `i` of the answered
/// list is the command it runs. Hidden commands are left out; enabled and
/// checked state, labels (without mnemonic marks) and single-stroke
/// shortcuts come from the registry. A run of radio entries between
/// separators forms one group.
pub fn native_menu_bar(
    menus: &[Menu],
    registry: &[Command],
) -> (Vec<windowing::Menu>, Vec<String>) {
    let mut commands: Vec<String> = vec![];
    let mut out: Vec<windowing::Menu> = vec![];
    let mut group: u32 = 0;
    for menu in menus {
        let (label, _) = mnemonic(&menu.label);
        let entries = native_entries(&menu.items, registry, &mut commands, &mut group);
        out.push(windowing::Menu {
            title: label,
            entries,
        })
    }
    (out, commands)
}

/// The native entries for one menu body (a tray menu), with the command
/// each identity runs, numbered as in [`native_menu_bar`].
pub fn native_menu_entries(
    items: &[MenuItem],
    registry: &[Command],
) -> (Vec<windowing::MenuEntry>, Vec<String>) {
    let mut commands: Vec<String> = vec![];
    let mut group: u32 = 0;
    let entries = native_entries(items, registry, &mut commands, &mut group);
    (entries, commands)
}

fn native_shortcut(command: &Command) -> Option<windowing::Shortcut> {
    let shortcut = command.shortcuts.first()?;
    if shortcut.strokes.len() != 1 {
        return None;
    }
    let stroke = &shortcut.strokes[0];
    let mods = windowing::Mods {
        shift: stroke.shift,
        ctrl: stroke.ctrl,
        alt: stroke.alt,
        gui: stroke.primary || stroke.meta,
        ..windowing::Mods::default()
    };
    Some(windowing::Shortcut {
        key: stroke.key,
        mods,
    })
}

fn native_entries(
    items: &[MenuItem],
    registry: &[Command],
    commands: &mut Vec<String>,
    group: &mut u32,
) -> Vec<windowing::MenuEntry> {
    let mut out: Vec<windowing::MenuEntry> = vec![];
    let mut in_radio_run = false;
    for entry in items {
        let found = |id: String| registry.iter().find(|c| c.id == id);
        match entry {
            MenuItem::Action { command, label, .. } => {
                in_radio_run = false;
                let Some(c) = found(command.to_string()) else {
                    continue;
                };
                if !c.visible {
                    continue;
                };
                commands.push(c.id.clone());
                let id = commands.len() as u32;
                let (title, _) = mnemonic(if label.is_empty() { &c.label } else { label });
                let shortcut = native_shortcut(c);
                if c.checked >= 0 {
                    out.push(windowing::MenuEntry::Check {
                        id,
                        title,
                        enabled: c.enabled,
                        checked: c.checked == 1,
                        shortcut,
                    })
                } else {
                    out.push(windowing::MenuEntry::Action {
                        id,
                        title,
                        enabled: c.enabled,
                        shortcut,
                    })
                }
            }
            MenuItem::Radio { command, label } => {
                if !in_radio_run {
                    *group += 1;
                    in_radio_run = true;
                }
                let Some(c) = found(command.to_string()) else {
                    continue;
                };
                if !c.visible {
                    continue;
                };
                commands.push(c.id.clone());
                let id = commands.len() as u32;
                let (title, _) = mnemonic(if label.is_empty() { &c.label } else { label });
                out.push(windowing::MenuEntry::Radio {
                    id,
                    title,
                    enabled: c.enabled,
                    checked: c.checked == 1,
                    group: *group,
                    shortcut: native_shortcut(c),
                })
            }
            MenuItem::Submenu { label, items } => {
                in_radio_run = false;
                let (title, _) = mnemonic(label);
                let entries = native_entries(items, registry, commands, group);
                out.push(windowing::MenuEntry::Submenu {
                    title,
                    enabled: true,
                    entries,
                })
            }
            MenuItem::Separator | MenuItem::Header(_) => {
                in_radio_run = false;
                // Entries left out may leave a separator at an end or after
                // another, which the menu does not show.
                if out
                    .last()
                    .is_some_and(|last| !matches!(last, windowing::MenuEntry::Separator))
                {
                    out.push(windowing::MenuEntry::Separator)
                }
            }
        }
    }
    if matches!(out.last(), Some(windowing::MenuEntry::Separator)) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        Menu, MenuBar, item, layout_menu_bar, menu_line, mnemonic, native_menu_bar, panel_size,
        radio, separator, step, submenu,
    };
    use crate::commands::Command;
    use crate::geometry::{Constraints, Size};
    use crate::style::Theme;
    use crate::widgets::cx::LayoutCx;
    use crate::widgets::state::StateStore;

    #[test]
    fn hidden_entries_take_their_separators_with_them() {
        let reg = vec![
            Command::new("a", "A").visible(false),
            Command::new("b", "B"),
            Command::new("c", "C").visible(false),
        ];
        let items = [
            item("a"),
            separator(),
            item("b"),
            separator(),
            item("c"),
            separator(),
        ];
        assert_eq!(super::visible(&items, &reg), vec![item("b")]);
        let mut commands = vec![];
        let native = super::native_entries(&items, &reg, &mut commands, &mut 0);
        assert_eq!(native.len(), 1, "{native:?}");
        let shown = vec![
            Command::new("a", "A"),
            Command::new("b", "B"),
            Command::new("c", "C"),
        ];
        assert_eq!(super::visible(&items, &shown).len(), 5);
    }

    #[test]
    fn a_menu_bar_maps_onto_native_menus_with_command_state() {
        let reg = vec![
            Command::new("file.open", "&Open").key("Primary+O"),
            Command::new("file.save", "Save").enabled(false),
            Command::new("view.dark", "Dark").checked(true),
            Command::new("view.light", "Light").checked(false),
            Command::new("view.hidden", "Hidden").visible(false),
        ];
        let menus = vec![
            Menu::new(
                "&File",
                &[item("file.open"), separator(), item("file.save")],
            ),
            Menu::new(
                "&View",
                &[
                    radio("view.dark", ""),
                    radio("view.light", ""),
                    item("view.hidden"),
                    submenu("More", &[item("file.open")]),
                ],
            ),
        ];
        let (native, commands) = native_menu_bar(&menus, &reg);
        assert_eq!(
            commands,
            vec![
                "file.open",
                "file.save",
                "view.dark",
                "view.light",
                "file.open"
            ]
        );
        assert_eq!(native.len(), 2);
        assert_eq!(native[0].title, "File");
        assert_eq!(
            format!("{:?}", native[0].entries[0]),
            "Action { id: 1, title: \"Open\", enabled: true, shortcut: Some(Shortcut { key: O, mods: Mods { shift: false, ctrl: false, alt: false, gui: true, alt_gr: false, caps_lock: false, num_lock: false, scroll_lock: false } }) }"
        );
        assert_eq!(
            format!("{:?}", native[0].entries[2]),
            "Action { id: 2, title: \"Save\", enabled: false, shortcut: None }"
        );
        assert_eq!(
            format!("{:?}", native[1].entries[0]),
            "Radio { id: 3, title: \"Dark\", enabled: true, checked: true, group: 1, shortcut: None }"
        );
        assert_eq!(
            format!("{:?}", native[1].entries[1]),
            "Radio { id: 4, title: \"Light\", enabled: true, checked: false, group: 1, shortcut: None }"
        );
        assert_eq!(native[1].entries.len(), 3)
    }

    #[test]
    fn mnemonics_are_parsed_and_doubled_ampersands_kept() {
        assert_eq!(mnemonic("&File"), ("File".to_string(), "f".to_string()));
        assert_eq!(
            mnemonic("Save &As"),
            ("Save As".to_string(), "a".to_string())
        );
        assert_eq!(mnemonic("A && B"), ("A & B".to_string(), "".to_string()))
    }

    #[test]
    fn stepping_skips_separators_and_disabled_items() {
        let reg = vec![
            Command::new("a", "A"),
            Command::new("b", "B").enabled(false),
            Command::new("c", "C"),
        ];
        let items = vec![item("a"), separator(), item("b"), item("c")];
        assert_eq!(step(&items, 0, 1, &reg), 3);
        assert_eq!(step(&items, 3, 1, &reg), 0);
        assert_eq!(step(&items, -1, -1, &reg), 3)
    }

    #[test]
    fn the_bar_and_its_rows_follow_the_menu_font() {
        let reg = vec![Command::new("a", "A")];
        let standard = Theme::light();
        let mut desktop = Theme::light();
        desktop.typography = desktop.typography.with_menu_points(9.0);
        let bar = MenuBar {
            id: "menu".into(),
            menus: vec![Menu::new("&File", &[item("a")])],
            joined: true,
        };
        let height = |theme: &Theme| {
            let mut cx = LayoutCx::new(theme, &StateStore::default(), Size::new(800, 600));
            layout_menu_bar(&bar, Constraints::loose(Size::new(800, 600)), &mut cx)
                .size()
                .height
        };
        assert!(height(&desktop) < height(&standard));
        assert!(height(&desktop) > menu_line(&desktop));
        let rows = |theme: &Theme| panel_size(&[item("a")], theme, &reg).height;
        assert!(rows(&desktop) < rows(&standard));
    }

    #[test]
    fn panels_grow_with_labels_and_rows() {
        let reg = vec![Command::new("a", "A")];
        let small = panel_size(&[item("a")], &Theme::light(), &reg);
        let big = panel_size(
            &[item("a"), separator(), submenu("More", &[item("a")])],
            &Theme::light(),
            &reg,
        );
        assert!(big.height > small.height)
    }
}
