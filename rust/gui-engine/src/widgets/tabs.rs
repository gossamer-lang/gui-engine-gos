// tabs: a window's tab strip and the content of its active tab. The
// application owns the tab list and which tab is active; the strip reports
// what the user asked for and the next view shows it. Selecting reports
// `UiMsg::Tab(id, key)`, a drag reorder reports `UiMsg::Reorder(id, key,
// index)`, and every other request (close, close others, close to the
// right, pin, unpin, move left or right, new, reopen, move to a new window)
// reports `UiMsg::TabAction(id, key, action)`. The `gui.tab.*` commands
// from `tab_commands` reach the innermost strip around the focus, so the
// same shortcuts, menus, and palette entries work in every tabbed window.
use crate::strext::StrExt;
use std::collections::HashMap;
use windowing::{Key, MouseButton};

use crate::accessibility::{AccessNode, Role};
use crate::commands::Command;
use crate::geometry::{Constraints, Rect, Size};
use crate::render::icons::Icon;
use crate::style::{TextRole, Theme};
use crate::text::layout;
use crate::text::layout::LayoutOptions;
use crate::widgets::Widget;
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx, Placement, PopupRequest, UiMsg};
use crate::widgets::state::StateStore;

/// One tab: a stable key, what the strip shows for it, and what it allows.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TabItem {
    pub key: String,
    pub title: String,
    /// An icon name from `render::icons`, or "".
    pub icon: Option<crate::render::icons::Icon>,
    pub tooltip: String,
    /// Unsaved changes: the strip marks the tab.
    pub dirty: bool,
    /// Work in progress: the strip shows activity.
    pub busy: bool,
    /// Pinned tabs sit first, show no close button, and refuse close.
    pub pinned: bool,
    pub closable: bool,
}

impl TabItem {
    pub fn new(key: &str, title: &str) -> TabItem {
        TabItem {
            key: key.to_string(),
            title: title.to_string(),
            icon: None,
            tooltip: String::new(),
            dirty: false,
            busy: false,
            pinned: false,
            closable: true,
        }
    }

    pub fn dirty(self, dirty: bool) -> TabItem {
        let mut t = self;
        t.dirty = dirty;
        t
    }

    pub fn busy(self, busy: bool) -> TabItem {
        let mut t = self;
        t.busy = busy;
        t
    }

    pub fn pinned(self, pinned: bool) -> TabItem {
        let mut t = self;
        t.pinned = pinned;
        t
    }

    pub fn closable(self, closable: bool) -> TabItem {
        let mut t = self;
        t.closable = closable;
        t
    }

    pub fn icon(self, icon: &Icon) -> TabItem {
        let mut t = self;
        t.icon = Some(icon.clone());
        t
    }

    pub fn tooltip(self, tooltip: &str) -> TabItem {
        let mut t = self;
        t.tooltip = tooltip.to_string();
        t
    }

    fn shows_close(&self) -> bool {
        self.closable && !self.pinned
    }
}

/// A tab strip over the content of its active tab.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tabs {
    pub id: String,
    pub items: Vec<TabItem>,
    pub active: String,
    pub content: Box<Widget>,
    /// Inactive pages retain interaction state without being laid out or painted.
    pub pages: HashMap<String, Widget>,
    /// Shared identity for pages allowed to migrate between windows.
    pub workspace: String,
    /// Shows a new-tab button after the tabs.
    pub add_button: bool,
    /// Offers "Move to New Window" in the tab menu.
    pub detachable: bool,
    /// Joins the strip to the content below it: the strip takes the field
    /// color a text area or terminal fills with, draws no dividing line, and
    /// labels its tabs at the desktop's menu text size.
    #[serde(default)]
    pub joined: bool,
}

/// The commands a tabbed window offers; add them to `App::commands`.
pub fn tab_commands() -> Vec<Command> {
    vec![
        Command::new("gui.tab.new", &crate::localization::tr("New Tab"))
            .key("Primary+T")
            .category(&crate::localization::tr("Tabs")),
        Command::new("gui.tab.close", &crate::localization::tr("Close Tab"))
            .key("Primary+W")
            .key("Primary+F4")
            .category(&crate::localization::tr("Tabs")),
        Command::new(
            "gui.tab.reopen",
            &crate::localization::tr("Reopen Closed Tab"),
        )
        .key("Primary+Shift+T")
        .category(&crate::localization::tr("Tabs")),
        Command::new("gui.tab.next", &crate::localization::tr("Next Tab"))
            .key("Ctrl+Tab")
            .key("Ctrl+PageDown")
            .category(&crate::localization::tr("Tabs")),
        Command::new("gui.tab.previous", &crate::localization::tr("Previous Tab"))
            .key("Ctrl+Shift+Tab")
            .key("Ctrl+PageUp")
            .category(&crate::localization::tr("Tabs")),
        Command::new(
            "gui.tab.move-left",
            &crate::localization::tr("Move Tab Left"),
        )
        .key("Ctrl+Shift+PageUp")
        .category(&crate::localization::tr("Tabs")),
        Command::new(
            "gui.tab.move-right",
            &crate::localization::tr("Move Tab Right"),
        )
        .key("Ctrl+Shift+PageDown")
        .category(&crate::localization::tr("Tabs")),
        Command::new(
            "gui.tab.close-others",
            &crate::localization::tr("Close Other Tabs"),
        )
        .category(&crate::localization::tr("Tabs")),
        Command::new(
            "gui.tab.close-right",
            &crate::localization::tr("Close Tabs to the Right"),
        )
        .category(&crate::localization::tr("Tabs")),
        Command::new(
            "gui.tab.detach",
            &crate::localization::tr("Move Tab to New Window"),
        )
        .category(&crate::localization::tr("Tabs")),
        Command::new("gui.tab.select-1", &crate::localization::tr("Go to Tab 1"))
            .key("Primary+1")
            .category(&crate::localization::tr("Tabs")),
        Command::new("gui.tab.select-2", &crate::localization::tr("Go to Tab 2"))
            .key("Primary+2")
            .category(&crate::localization::tr("Tabs")),
        Command::new("gui.tab.select-3", &crate::localization::tr("Go to Tab 3"))
            .key("Primary+3")
            .category(&crate::localization::tr("Tabs")),
        Command::new("gui.tab.select-4", &crate::localization::tr("Go to Tab 4"))
            .key("Primary+4")
            .category(&crate::localization::tr("Tabs")),
        Command::new("gui.tab.select-5", &crate::localization::tr("Go to Tab 5"))
            .key("Primary+5")
            .category(&crate::localization::tr("Tabs")),
        Command::new("gui.tab.select-6", &crate::localization::tr("Go to Tab 6"))
            .key("Primary+6")
            .category(&crate::localization::tr("Tabs")),
        Command::new("gui.tab.select-7", &crate::localization::tr("Go to Tab 7"))
            .key("Primary+7")
            .category(&crate::localization::tr("Tabs")),
        Command::new("gui.tab.select-8", &crate::localization::tr("Go to Tab 8"))
            .key("Primary+8")
            .category(&crate::localization::tr("Tabs")),
        Command::new(
            "gui.tab.select-last",
            &crate::localization::tr("Go to Last Tab"),
        )
        .key("Primary+9")
        .category(&crate::localization::tr("Tabs")),
    ]
}

// Part layout of a strip node: two rects per tab (the tab, its close
// button), then the new-tab button and the overflow button (empty when
// absent). The content is the node's only child.
fn tab_part(i: i64) -> i64 {
    i * 2
}
fn close_part(i: i64) -> i64 {
    i * 2 + 1
}
fn add_part(n: i64) -> i64 {
    n * 2
}
fn overflow_part(n: i64) -> i64 {
    n * 2 + 1
}

// The text style a strip labels its tabs in.
fn label_font(t: &Tabs, theme: &Theme) -> crate::text::layout::TextStyle {
    theme.font(if t.joined {
        TextRole::Menu
    } else {
        TextRole::Body
    })
}

fn strip_height(t: &Tabs, theme: &Theme) -> i64 {
    if t.joined {
        layout::measure("Mg", &label_font(t, theme)).1.ceil() as i64 + theme.px(14)
    } else {
        theme.control_height() + theme.px(4)
    }
}

fn natural_width(strip: &Tabs, t: &TabItem, theme: &Theme) -> i64 {
    let (tw, _) = layout::measure(&t.title, &label_font(strip, theme));
    let pad = theme.padding();
    let mut w = pad * 2 + tw.ceil() as i64;
    if t.icon.is_some() {
        w += theme.px(theme.shape.small_icon + 6)
    };
    if t.dirty || t.busy {
        w += theme.px(14)
    };
    if t.shows_close() {
        w += theme.px(24)
    };
    w.max(min_width(t, theme)).min(theme.px(240))
}

fn min_width(t: &TabItem, theme: &Theme) -> i64 {
    if t.pinned { theme.px(44) } else { theme.px(84) }
}

fn index_of(t: &Tabs, key: &str) -> i64 {
    let mut i = 0;
    while i < t.items.len() {
        if t.items[i].key == key {
            return i as i64;
        };
        i += 1;
    }
    -1
}

pub fn layout_tabs(t: &Tabs, c: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    let h = strip_height(t, theme);
    let width = if c.max_width >= crate::geometry::UNBOUNDED {
        theme.px(640)
    } else {
        c.max_width
    };
    let n = t.items.len();
    let mut widths = vec![];
    let mut total = 0;
    for item in &t.items {
        let w = natural_width(t, item, theme);
        widths.push(w);
        total += w;
    }
    let button = theme.control_height();
    let add_w = if t.add_button { button } else { 0 };
    let mut avail = (width - add_w).max(0);
    // Too wide: shrink unpinned tabs evenly toward their minimum first.
    if total > avail {
        let mut floor_total = 0;
        for item in &t.items {
            floor_total += min_width(item, theme)
        }
        let excess = total - avail;
        let room = total - floor_total;
        let mut k = 0;
        total = 0;
        while k < n {
            let shrink = if room > 0 {
                (widths[k] - min_width(&t.items[k], theme)) * excess.min(room) / room
            } else {
                0
            };
            widths[k] -= shrink;
            total += widths[k];
            k += 1;
        }
    }
    let overflow = total > avail;
    if overflow {
        avail = (avail - button).max(0)
    };
    // When the tabs still do not fit, scroll the strip so the active tab
    // shows; the overflow button lists every tab.
    let active = index_of(t, &t.active);
    let mut offset = 0;
    if overflow && active >= 0 {
        let mut right = 0;
        let mut k = 0;
        while k <= active {
            right += widths[k as usize];
            k += 1;
        }
        if right > avail {
            offset = right - avail
        }
    }
    let mut parts = vec![];
    let mut x = -offset;
    let mut k = 0;
    while k < n {
        let r = Rect::new(x, 0, widths[k], h);
        parts.push(r);
        if t.items[k].shows_close() {
            let s = theme.px(theme.shape.close_icon + 6);
            parts.push(Rect::new(
                x + widths[k] - theme.padding() / 2 - s,
                (h - s) / 2,
                s,
                s,
            ))
        } else {
            parts.push(Rect::new(0, 0, 0, 0))
        }
        x += widths[k];
        k += 1;
    }
    let tabs_end = if overflow { avail } else { x };
    parts.push(if t.add_button {
        Rect::new(tabs_end, (h - button) / 2, button, button)
    } else {
        Rect::new(0, 0, 0, 0)
    });
    parts.push(if overflow {
        Rect::new(width - button, (h - button) / 2, button, button)
    } else {
        Rect::new(0, 0, 0, 0)
    });
    if overflow && t.add_button {
        parts[(add_part(n as i64)) as usize] =
            Rect::new(width - button * 2, (h - button) / 2, button, button);
    }
    let size = c.constrain(Size::new(
        width,
        if c.max_height >= crate::geometry::UNBOUNDED {
            h + theme.px(200)
        } else {
            c.max_height
        },
    ));
    if theme.rtl {
        let frame = Rect::new(0, 0, size.width, h);
        let mut m = 0;
        while m < parts.len() {
            if !parts[m].is_empty() {
                parts[m] = parts[m].mirror_in(frame)
            };
            m += 1;
        }
    }
    let content_h = (size.height - h).max(0);
    let child = t
        .content
        .layout(Constraints::tight(Size::new(size.width, content_h)), cx);
    let mut node = Node::leaf(size);
    node.parts = parts;
    node.children = vec![child.at(0, h)];
    node.content = Size::new(if overflow { avail } else { total }, h);
    node
}

pub fn paint_tabs(t: &Tabs, node: &Node, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let h = strip_height(t, &theme);
    let strip = Rect::new(0, 0, node.size().width, h);
    if t.joined {
        cx.fill(strip, theme.field);
    } else {
        cx.fill(strip, theme.surface_alt);
        cx.fill(
            Rect::new(
                0,
                h - theme.border_width(),
                strip.width,
                theme.border_width(),
            ),
            theme.border,
        );
    }
    let n = t.items.len();
    let overflow = node.parts.len() > overflow_part(n as i64) as usize
        && !node.parts[(overflow_part(n as i64)) as usize].is_empty();
    let mut clip_w = strip.width;
    if overflow {
        clip_w -= theme.control_height()
    };
    if t.add_button && overflow {
        clip_w -= theme.control_height()
    };
    let saved = cx.canvas.clip;
    let clip_rect = cx.abs(Rect::new(
        if theme.rtl { strip.width - clip_w } else { 0 },
        0,
        clip_w,
        h,
    ));
    cx.canvas.push_clip(clip_rect.intersect(saved));
    let body = label_font(t, &theme);
    let drag_from = cx.states.field(&t.id, "drag-key", "");
    let mut i = 0;
    while i < n && close_part(i as i64) < node.parts.len() as i64 {
        let item = &t.items[i];
        let r = node.parts[(tab_part(i as i64)) as usize];
        let on = item.key == t.active;
        let tab_rect = Rect::new(r.x, r.y + theme.px(4), r.width, r.height - theme.px(4));
        if on {
            // A joined strip raises the active tab off the field; a plain one
            // opens it onto the page below.
            let page = if t.joined {
                theme.surface
            } else {
                theme.background
            };
            cx.fill_round(tab_rect, theme.radius(), page);
            if !t.joined {
                cx.fill(
                    Rect::new(
                        r.x,
                        r.y + r.height - theme.border_width(),
                        r.width,
                        theme.border_width(),
                    ),
                    page,
                );
            }
            cx.fill(
                Rect::new(
                    r.x + theme.px(6),
                    r.y + theme.px(4),
                    r.width - theme.px(12),
                    theme.px(2),
                ),
                theme.accent,
            )
        } else if item.key == drag_from {
            cx.fill_round(tab_rect, theme.radius(), theme.pressed)
        } else if i > 0 && t.items[i - 1].key != t.active {
            cx.fill(
                Rect::new(
                    r.x,
                    r.y + theme.px(12),
                    theme.border_width(),
                    r.height - theme.px(20),
                ),
                theme.border,
            )
        }
        let mut x = r.x + theme.padding();
        let a = cx.abs(r);
        if let Some(ref own) = item.icon {
            let isz = theme.px(theme.shape.small_icon);
            cx.icon(
                own,
                a.x + (x - r.x),
                a.y + (r.height - isz) / 2 + theme.px(2),
                isz,
                if on { theme.text } else { theme.text_muted },
            );
            x += theme.px(theme.shape.small_icon + 6);
        }
        let close = node.parts[(close_part(i as i64)) as usize];
        let text_right = if close.is_empty() {
            r.x + r.width - theme.padding()
        } else {
            close.x - theme.px(4)
        };
        let mark_w = if item.dirty || item.busy {
            theme.px(14)
        } else {
            0
        };
        let room = (text_right - x - mark_w).max(0);
        let l = layout::layout(&item.title, &body, LayoutOptions::elided(room as f64));
        let fg = if on { theme.text } else { theme.text_muted };
        layout::draw(
            &mut cx.canvas,
            &l,
            (a.x + (x - r.x)) as f64,
            (a.y + (r.height - l.height.ceil() as i64) / 2 + theme.px(2)) as f64,
            &body,
            fg,
        );
        if mark_w > 0 {
            let mx = a.x + (x - r.x) + l.width.ceil() as i64 + theme.px(8);
            let my = a.y + r.height / 2 + theme.px(2);
            if item.busy {
                cx.canvas
                    .circle_stroke(mx, my, theme.px(4), theme.px(2), theme.accent)
            } else {
                cx.canvas.circle(mx, my, theme.px(4), fg)
            }
        }
        if !close.is_empty() {
            let ca = cx.abs(close);
            let isz = theme.px(theme.shape.close_icon);
            cx.icon(
                &Icon::Close,
                ca.x + (close.width - isz) / 2,
                ca.y + (close.height - isz) / 2,
                isz,
                fg,
            )
        }
        i += 1;
    }
    cx.canvas.pop_clip(saved);
    if node.parts.len() > overflow_part(n as i64) as usize {
        let add = node.parts[(add_part(n as i64)) as usize];
        if !add.is_empty() {
            let aa = cx.abs(add);
            let isz = theme.px(theme.shape.small_icon);
            cx.icon(
                &Icon::Plus,
                aa.x + (add.width - isz) / 2,
                aa.y + (add.height - isz) / 2,
                isz,
                theme.text_muted,
            )
        }
        let more = node.parts[(overflow_part(n as i64)) as usize];
        if !more.is_empty() {
            let ma = cx.abs(more);
            let isz = theme.px(theme.shape.small_icon);
            cx.icon(
                &Icon::ChevronDown,
                ma.x + (more.width - isz) / 2,
                ma.y + (more.height - isz) / 2,
                isz,
                theme.text_muted,
            )
        }
    }
    let active = index_of(t, &t.active);
    if cx.is_focused(&t.id) && active >= 0 {
        let r = node.parts[(tab_part(active)) as usize];
        cx.focus_ring(
            Rect::new(
                r.x + theme.px(2),
                r.y + theme.px(6),
                r.width - theme.px(4),
                r.height - theme.px(8),
            ),
            theme.radius(),
        )
    }
    if !node.children.is_empty() {
        let child = &node.children[0];
        let ox = cx.origin_x;
        let oy = cx.origin_y;
        let cr = child.rect.offset(ox, oy);
        cx.origin_x = cr.x;
        cx.origin_y = cr.y;
        t.content.paint(child, cx);
        cx.origin_x = ox;
        cx.origin_y = oy;
    }
}

/// The index of the tab a strip-local point is over, or -1.
fn tab_at(t: &Tabs, node: &Node, lx: i64, ly: i64) -> i64 {
    let mut i = 0;
    while i < t.items.len() && tab_part(i as i64) < node.parts.len() as i64 {
        if node.parts[(tab_part(i as i64)) as usize].contains(lx, ly) {
            return i as i64;
        };
        i += 1;
    }
    -1
}

fn select(t: &Tabs, i: i64, cx: &mut EventCx) {
    if i >= 0 && i < t.items.len() as i64 && t.items[i as usize].key != t.active {
        cx.emit(&UiMsg::Tab(t.id.clone(), t.items[i as usize].key.clone()))
    }
}

fn action(t: &Tabs, key: &str, name: &str, cx: &mut EventCx) {
    cx.emit(&UiMsg::TabAction(
        t.id.clone(),
        key.to_string(),
        name.to_string(),
    ))
}

fn close_tab(t: &Tabs, i: i64, cx: &mut EventCx) {
    if i >= 0 && i < t.items.len() as i64 && t.items[i as usize].shows_close() {
        action(t, &t.items[i as usize].key, "close", cx)
    }
}

// The tab-menu rows: (action, label). Pinning toggles; closing is offered
// only for a closable tab.
fn menu_rows(t: &Tabs, key: &str) -> Vec<(String, String)> {
    let i = index_of(t, key);
    let mut rows = vec![];
    if i < 0 {
        return rows;
    };
    let item = &t.items[i as usize];
    if item.shows_close() {
        rows.push(("close".to_string(), crate::localization::tr("Close Tab")))
    };
    if t.items.len() > 1 {
        rows.push((
            "close-others".to_string(),
            crate::localization::tr("Close Other Tabs"),
        ))
    };
    if i + 1 < t.items.len() as i64 {
        rows.push((
            "close-right".to_string(),
            crate::localization::tr("Close Tabs to the Right"),
        ))
    };
    rows.push(if item.pinned {
        ("unpin".to_string(), crate::localization::tr("Unpin Tab"))
    } else {
        ("pin".to_string(), crate::localization::tr("Pin Tab"))
    });
    if i > 0 {
        rows.push((
            "move-left".to_string(),
            crate::localization::tr("Move Tab Left"),
        ))
    };
    if i + 1 < t.items.len() as i64 {
        rows.push((
            "move-right".to_string(),
            crate::localization::tr("Move Tab Right"),
        ))
    };
    if t.detachable && t.items.len() > 1 {
        rows.push((
            "detach".to_string(),
            crate::localization::tr("Move Tab to New Window"),
        ))
    };
    rows
}

fn popup_rows(t: &Tabs, st: &StateStore) -> Vec<(String, String)> {
    if st.field(&t.id, "popup", "") == "list" {
        let mut rows = vec![];
        for item in &t.items {
            rows.push((
                item.key.clone(),
                if item.dirty {
                    format!("{} •", item.title)
                } else {
                    item.title.clone()
                },
            ))
        }
        rows
    } else {
        menu_rows(t, &st.field(&t.id, "menu-key", ""))
    }
}

fn open_popup_at(
    t: &Tabs,
    kind: &str,
    key: &str,
    anchor: Rect,
    place: Placement,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    st.set_field(&t.id, "popup", kind);
    st.set_field(&t.id, "menu-key", key);
    let rows = popup_rows(t, st);
    let mut hl = 0;
    if kind == "list" {
        let a = index_of(t, &t.active);
        if a >= 0 {
            hl = a
        }
    }
    st.set_field(&t.id, "highlight", &format!("{}", hl));
    let theme = &cx.theme;
    let mut w = theme.px(200);
    for (_, label) in &rows {
        let (tw, _) = layout::measure(label, &theme.font(TextRole::Body));
        w = w.max(tw.ceil() as i64 + theme.padding() * 4);
    }
    let size = Size::new(
        w.min(theme.px(420)),
        (rows.len() * theme.row_height() as usize + theme.px(8) as usize) as i64,
    );
    cx.open_popup(&PopupRequest::new(
        &t.id, 0, anchor, size, place, true, true,
    ))
}

pub fn tabs_event(t: &Tabs, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    let theme = cx.theme.clone();
    let h = strip_height(t, &theme);
    let n = t.items.len();
    let active = index_of(t, &t.active);
    match ev {
        Event::PointerDown {
            x,
            y,
            button,
            clicks,
            ..
        } => {
            let lx = x - cx.origin_x;
            let ly = y - cx.origin_y;
            if ly < 0 || ly >= h {
                return;
            };
            let i = tab_at(t, node, lx, ly);
            if *button == MouseButton::Middle {
                close_tab(t, i, cx);
                cx.consume();
                return;
            }
            if *button == MouseButton::Right {
                if i >= 0 {
                    let anchor = cx.to_window(Rect::new(lx, ly, 1, 1));
                    open_popup_at(
                        t,
                        "menu",
                        &t.items[i as usize].key,
                        anchor,
                        Placement::AtPoint,
                        st,
                        cx,
                    )
                }
                cx.consume();
                return;
            }
            if *button != MouseButton::Left {
                return;
            };
            if node.parts.len() > overflow_part(n as i64) as usize {
                if node.parts[(add_part(n as i64)) as usize].contains(lx, ly) {
                    action(t, "", "new", cx);
                    cx.consume();
                    return;
                }
                if node.parts[(overflow_part(n as i64)) as usize].contains(lx, ly) {
                    let anchor = cx.to_window(node.parts[(overflow_part(n as i64)) as usize]);
                    open_popup_at(t, "list", "", anchor, Placement::Below, st, cx);
                    cx.consume();
                    return;
                }
            }
            if i < 0 {
                if *clicks == 2 {
                    action(t, "", "new", cx)
                };
                cx.consume();
                return;
            }
            if node.parts[(close_part(i)) as usize].contains(lx, ly) {
                close_tab(t, i, cx);
                cx.consume();
                return;
            }
            select(t, i, cx);
            st.set_field(&t.id, "drag-key", &t.items[i as usize].key);
            st.set_field(&t.id, "drag-x", &format!("{}", lx));
            st.set_field(&t.id, "drag-y", &format!("{}", ly));
            st.set_field(&t.id, "dragging", "");
            cx.capture_pointer(&t.id);
            cx.consume()
        }
        Event::PointerMove { x, y, pressed, .. } => {
            let key = st.field(&t.id, "drag-key", "");
            if !pressed || key.is_empty() {
                return;
            };
            let lx = x - cx.origin_x;
            let start = st.field(&t.id, "drag-x", "0").to_i64().unwrap_or(0);
            let sy = st.field(&t.id, "drag-y", "0").to_i64().unwrap_or(0);
            if (lx - start).abs() > theme.px(6) || (y - cx.origin_y - sy).abs() > theme.px(6) {
                st.set_field(&t.id, "dragging", "1")
            };
            cx.redraw = true;
            cx.consume()
        }
        Event::PointerUp { x, y, .. } => {
            let key = st.field(&t.id, "drag-key", "");
            if key.is_empty() {
                return;
            };
            let dragging = st.field(&t.id, "dragging", "") == "1";
            st.set_field(&t.id, "drag-key", "");
            st.set_field(&t.id, "dragging", "");
            cx.release_pointer();
            if dragging {
                let lx = x - cx.origin_x;
                let ly = y - cx.origin_y;
                if ly < -theme.px(40) || ly > h + theme.px(40) {
                    if t.detachable {
                        cx.emit(&UiMsg::TabDrop(t.id.clone(), key, *x, *y))
                    }
                } else {
                    let to = drop_index(t, node, lx);
                    let from = index_of(t, &key);
                    if to >= 0 && to != from {
                        cx.emit(&UiMsg::Reorder(t.id.clone(), key, to))
                    }
                }
            }
            cx.consume()
        }
        Event::CaptureLost => {
            st.set_field(&t.id, "drag-key", "");
            st.set_field(&t.id, "dragging", "")
        }
        Event::Scroll { y, dx, dy, .. } => {
            let ly = y - cx.origin_y;
            if ly < 0 || ly >= h || n == 0 {
                return;
            };
            let d = if dy.abs() >= dx.abs() { dy } else { dx };
            if *d < 0.0 {
                select(t, (active + 1).min((n - 1) as i64), cx)
            } else if *d > 0.0 {
                select(t, (active - 1).max(0), cx)
            };
            cx.consume()
        }
        Event::KeyDown { key, mods, .. } => {
            if !cx.is_focused(&t.id) || n == 0 {
                return;
            };
            let forward = if theme.rtl { Key::Left } else { Key::Right };
            let back = if theme.rtl { Key::Right } else { Key::Left };
            if mods.ctrl && mods.shift && (*key == forward || *key == back) {
                if active >= 0 {
                    action(
                        t,
                        &t.active,
                        if *key == forward {
                            "move-right"
                        } else {
                            "move-left"
                        },
                        cx,
                    )
                };
                cx.consume()
            } else if *key == forward {
                select(t, (active + 1).min((n - 1) as i64), cx);
                cx.consume()
            } else if *key == back {
                select(t, (active - 1).max(0), cx);
                cx.consume()
            } else if *key == Key::Home {
                select(t, 0, cx);
                cx.consume()
            } else if *key == Key::End {
                select(t, (n - 1) as i64, cx);
                cx.consume()
            } else if *key == Key::Delete {
                close_tab(t, active, cx);
                cx.consume()
            } else if *key == Key::Menu || (*key == Key::F10 && mods.shift) {
                if active >= 0 {
                    let anchor = cx.to_window(node.parts[(tab_part(active)) as usize]);
                    open_popup_at(t, "menu", &t.active, anchor, Placement::Below, st, cx)
                }
                cx.consume()
            }
        }
        Event::Command(name) => tab_command(t, name, cx),
        _ => (),
    }
}

/// The insertion index a drop at strip-local `lx` lands on.
fn drop_index(t: &Tabs, node: &Node, lx: i64) -> i64 {
    let mut i = 0;
    while i < t.items.len() && tab_part(i as i64) < node.parts.len() as i64 {
        let r = node.parts[(tab_part(i as i64)) as usize];
        if lx < r.x + r.width / 2 {
            return i as i64;
        };
        i += 1;
    }
    t.items.len() as i64 - 1
}

fn tab_command(t: &Tabs, name: &str, cx: &mut EventCx) {
    if !name.starts_with("gui.tab.") {
        return;
    };
    let n = t.items.len();
    let active = index_of(t, &t.active);
    let rest = name.substring(8, name.byte_len());
    match rest {
        "next" => {
            if n > 0 {
                select(t, (active + 1) % n as i64, cx)
            }
        }
        "previous" => {
            if n > 0 {
                select(t, (active - 1 + n as i64) % n as i64, cx)
            }
        }
        "select-last" => select(t, (n - 1) as i64, cx),
        "new" => action(t, "", "new", cx),
        "reopen" => action(t, "", "reopen", cx),
        "close" => close_tab(t, active, cx),
        "close-others" | "close-right" | "move-left" | "move-right" | "detach" => {
            if active >= 0 {
                action(t, &t.active, rest, cx)
            }
        }
        _ => {
            if rest.starts_with("select-") {
                match rest.substring(7, rest.byte_len()).to_i64() {
                    Some(k) => select(t, k - 1, cx),
                    None => return,
                }
            } else {
                return;
            }
        }
    }
    cx.consume()
}

pub fn tabs_popup_paint(t: &Tabs, rect: Rect, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let rows = popup_rows(t, &cx.states);
    let hl = cx
        .states
        .field(&t.id, "highlight", "0")
        .to_i64()
        .unwrap_or(0);
    let (drop, blur) = theme.shadow_of(theme.shape.popup_elevation);
    cx.canvas.shadow(
        rect.x,
        rect.y + drop,
        rect.width,
        rect.height,
        theme.radius(),
        blur,
        theme.shadow,
    );
    cx.canvas.round_rect(
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        theme.radius(),
        theme.surface,
    );
    cx.canvas.round_rect_stroke(
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        theme.radius(),
        theme.border_width(),
        theme.border,
    );
    let body = theme.font(TextRole::Body);
    let row = theme.row_height();
    let list = cx.states.field(&t.id, "popup", "") == "list";
    let mut i = 0;
    while i < rows.len() {
        let (ref key, ref label) = rows[i];
        let y = rect.y + theme.px(4) + (i * row as usize) as i64;
        let on = i == hl as usize;
        if on {
            cx.canvas.rect(
                rect.x + theme.px(4),
                y,
                rect.width - theme.px(8),
                row,
                theme.selection,
            )
        };
        let fg = if on { theme.selection_text } else { theme.text };
        if list && *key == t.active {
            cx.icon(
                &Icon::Check,
                rect.x + theme.px(10),
                y + (row - theme.px(theme.shape.small_icon)) / 2,
                theme.px(theme.shape.small_icon),
                fg,
            )
        }
        let l = layout::layout(
            label,
            &body,
            LayoutOptions::elided((rect.width - theme.px(48)) as f64),
        );
        layout::draw(
            &mut cx.canvas,
            &l,
            (rect.x + theme.px(34)) as f64,
            (y + (row - l.height.ceil() as i64) / 2) as f64,
            &body,
            fg,
        );
        i += 1;
    }
}

pub fn tabs_popup_event(t: &Tabs, rect: Rect, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    let theme = &cx.theme;
    let rows = popup_rows(t, st);
    if rows.is_empty() {
        return;
    };
    let row = theme.row_height();
    let hl = st.field(&t.id, "highlight", "0").to_i64().unwrap_or(0);
    let choose = |i: i64, st: &mut StateStore, cx: &mut EventCx| {
        if i < 0 || i >= rows.len() as i64 {
            return;
        };
        let (ref key, _) = rows[i as usize];
        if st.field(&t.id, "popup", "") == "list" {
            select(t, index_of(t, key), cx)
        } else {
            action(t, &st.field(&t.id, "menu-key", ""), key, cx)
        }
        st.set_field(&t.id, "popup", "");
        cx.close_popup(&t.id, 0)
    };
    match ev {
        Event::PointerMove { y, .. } => {
            let i = (y - rect.y - theme.px(4)) / row;
            if i >= 0 && i < rows.len() as i64 {
                st.set_field(&t.id, "highlight", &format!("{}", i))
            };
            cx.redraw = true;
            cx.consume()
        }
        Event::PointerUp { y, .. } => {
            choose((y - rect.y - theme.px(4)) / row, st, cx);
            cx.consume()
        }
        Event::PointerDown { .. } => cx.consume(),
        Event::KeyDown { key, .. } => {
            match key {
                Key::Down => {
                    st.set_field(
                        &t.id,
                        "highlight",
                        &format!("{}", (hl + 1).min(rows.len() as i64 - 1)),
                    );
                    cx.redraw = true;
                }
                Key::Up => {
                    st.set_field(&t.id, "highlight", &format!("{}", (hl - 1).max(0)));
                    cx.redraw = true;
                }
                Key::Return | Key::KpEnter | Key::Space => choose(hl, st, cx),
                Key::Escape => {
                    st.set_field(&t.id, "popup", "");
                    cx.close_popup(&t.id, 0)
                }
                _ => return,
            }
            cx.consume()
        }
        _ => (),
    }
}

pub fn tabs_semantics(t: &Tabs) -> Option<AccessNode> {
    let mut g = AccessNode::new(Role::Group, "");
    let mut list = AccessNode::control(Role::TabList, &t.id, "", "");
    let mut i = 0;
    while i < t.items.len() {
        let item = &t.items[i];
        let label = if item.dirty {
            crate::localization::message(
                "tabs",
                "{title}, modified",
                1,
                &crate::hmap! { "title" => item.title.clone() },
            )
        } else {
            item.title.clone()
        };
        let mut tab = AccessNode::new(Role::Tab, &label);
        tab.id = format!("{}#{}", t.id, item.key);
        tab.selected = item.key == t.active;
        tab.actions.push("select".to_string());
        tab.pos_in_set = (i + 1) as i64;
        tab.set_size = t.items.len() as i64;
        tab.description = item.tooltip.clone();
        list.children.push(tab);
        i += 1;
    }
    g.children.push(list);
    Some(g)
}
