// choices: selection controls with distinct contracts.
//
// - `Select` stores one stable item key (or none). Typing selects by
//   typeahead; the popup's highlight is separate from the committed key;
//   Enter or a click commits, Escape rolls back, focus stays on the select.
// - `Picker` (searchable, single or multiple) keeps a query separate from
//   the selection: filtering never changes what is selected, selected keys
//   missing from the current items are still shown and removable, and
//   multiple selections render as tokens removable by keyboard.
// - `Palette` searches the window's commands and runs the one accepted;
//   disabled commands are listed but cannot run.
//
// Editable combos and autocomplete fields are text inputs with a
// completion source (`TextInput::completion`, `TextInput::dropdown`).
use crate::strext::StrExt;
use std::collections::HashMap;
use windowing::{Key, MouseButton};

use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Rect, Size};
use crate::render::icons::Icon;
use crate::style::{TextRole, Theme};
use crate::text::layout;
use crate::text::layout::LayoutOptions;
use crate::widgets::completion;
use crate::widgets::completion::{CompletionItem, MatchMode, SharedItems, VISIBLE_ROWS};
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx, Placement, PopupRequest, UiMsg};
use crate::widgets::state::{StateStore, WState};

/// A non-editable choice of one item.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Select {
    pub id: String,
    pub items: SharedItems,
    /// The committed item key, or "".
    pub selected: String,
    pub placeholder: String,
    pub label: String,
    pub loading: bool,
    pub error: String,
}

/// A searchable picker of one or several items.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Picker {
    pub id: String,
    pub items: SharedItems,
    pub selected: Vec<String>,
    pub multiple: bool,
    pub placeholder: String,
    pub label: String,
    pub loading: bool,
    pub error: String,
}

/// A command search list shown inline.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Palette {
    pub id: String,
    pub placeholder: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Choice {
    One(Select),
    Pick(Picker),
    Commands(Palette),
}

impl Choice {
    pub fn id(&self) -> String {
        match self {
            Choice::One(s) => s.id.clone(),
            Choice::Pick(p) => p.id.clone(),
            Choice::Commands(p) => p.id.clone(),
        }
    }
}

fn label_of(items: &[CompletionItem], key: &str) -> String {
    for it in items {
        if it.key == key {
            return it.label.clone();
        }
    }
    String::new()
}

fn present(items: &[CompletionItem], key: &str) -> bool {
    items.iter().any(|it| it.key == key)
}

/// Reconciles an open choice with its source while preserving selection and query.
pub fn sync_model(ch: &Choice, st: &mut StateStore) {
    let id = ch.id();
    let mut cs = st.choice(&id);
    if !cs.open {
        return;
    };
    let (items, loading, error) = match ch {
        Choice::One(s) => (s.items.clone(), s.loading, s.error.clone()),
        Choice::Pick(p) => (p.items.clone(), p.loading, p.error.clone()),
        Choice::Commands(_) => return,
    };
    if cs.model_items == items && cs.loading == loading && cs.error == error {
        return;
    };
    cs.model_items = items.clone();
    let ready = if loading || !error.is_empty() {
        SharedItems::default()
    } else {
        match ch {
            Choice::Pick(p) => picker_items(p, &cs.filter).into(),
            _ => items,
        }
    };
    cs.set_shared_items(ready);
    cs.loading = loading;
    cs.error = error;
    let at = cs.highlight_index();
    cs.first = cs.first.min((cs.items.len() as i64 - VISIBLE_ROWS).max(0));
    if at >= 0 {
        if at < cs.first {
            cs.first = at
        };
        if at >= cs.first + VISIBLE_ROWS {
            cs.first = at - VISIBLE_ROWS + 1
        }
    }
    st.set(&id, WState::Choice(cs))
}

// ---- select ----

pub fn layout_choice(ch: &Choice, c: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    let h = theme.control_height();
    match ch {
        Choice::One(s) => {
            let w = natural_width(&s.items, theme);
            let width = if c.min_width == c.max_width && c.min_width > 0 {
                c.max_width
            } else {
                w.min(c.max_width)
            };
            let mut n = Node::leaf(c.constrain(Size::new(width, h)));
            let (a, _, lh) = layout::metrics(&theme.font(TextRole::Body));
            n.baseline = (h - lh as i64) / 2 + a as i64;
            n
        }
        Choice::Pick(p) => {
            let width = if c.has_bounded_width() {
                c.max_width.min(theme.px(420))
            } else {
                theme.px(320)
            };
            let tokens = token_rects(p, theme, width);
            let rows = tokens.iter().map(|r| r.bottom()).fold(0, |a, b| a.max(b));
            let height = (rows + theme.px(6)).max(h);
            let mut n = Node::leaf(c.constrain(Size::new(width, height)));
            n.parts = tokens;
            n
        }
        Choice::Commands(_) => {
            let width = if c.has_bounded_width() {
                c.max_width.min(theme.px(560))
            } else {
                theme.px(420)
            };
            let height = h + theme.row_height() * VISIBLE_ROWS + theme.px(8);
            Node::leaf(c.constrain(Size::new(width, height)))
        }
    }
}

// The width that fits the widest item label, with room for the chevron.
fn natural_width(items: &[CompletionItem], theme: &Theme) -> i64 {
    theme
        .px(180)
        .max((completion::widest(items, theme, false)).ceil() as i64 + theme.px(64))
}

// Token chips of a multiple picker (local rects), each with its remove box.
fn token_rects(p: &Picker, theme: &Theme, width: i64) -> Vec<Rect> {
    let mut out = vec![];
    if !p.multiple {
        return out;
    }
    let small = theme.font(TextRole::Small);
    let pad = theme.px(6);
    let chip_h = theme.control_height() - theme.px(12);
    let mut x = pad;
    let mut y = theme.px(6);
    for key in &p.selected {
        let label = label_of(&p.items, key);
        let shown = if label.is_empty() {
            format!("{} (unavailable)", key)
        } else {
            label
        };
        let (tw, _) = layout::measure(&shown, &small);
        let w = tw.ceil() as i64 + theme.px(theme.shape.close_icon + 20);
        if x + w > width - theme.px(36) && x > pad {
            x = pad;
            y += chip_h + theme.px(4);
        }
        out.push(Rect::new(x, y, w, chip_h));
        x += w + theme.px(4);
    }
    out
}

fn field_frame(cx: &mut PaintCx, id: &str, r: Rect, open: bool) {
    let theme = cx.theme.clone();
    let hover = cx.is_hover(id);
    let bg = if cx.disabled {
        theme.surface_alt
    } else if hover {
        theme.hover.mix(theme.field, 0.5)
    } else {
        theme.field
    };
    cx.fill_round(r, theme.radius(), bg);
    let focused = cx.is_focused(id);
    let edge = if focused || open {
        theme.accent
    } else {
        theme.border_strong.with_alpha(180)
    };
    cx.stroke_round(
        r,
        theme.radius(),
        if focused {
            theme.border_width() * 2
        } else {
            theme.border_width()
        },
        edge,
    );
    let a = cx.abs(r);
    let isz = theme.px(theme.shape.field_icon);
    cx.icon(
        if open {
            &Icon::ChevronUp
        } else {
            &Icon::ChevronDown
        },
        a.right() - isz - theme.px(10),
        a.y + (theme.control_height() - isz) / 2,
        isz,
        if cx.disabled {
            theme.text_disabled
        } else {
            theme.text_muted
        },
    )
}

pub fn paint_choice(ch: &Choice, node: &Node, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let r = Rect::from_size(node.size());
    let body = theme.font(TextRole::Body);
    match ch {
        Choice::One(s) => {
            let st = cx.states.choice(&s.id);
            field_frame(cx, &s.id, r, st.open);
            let label = label_of(&s.items, &s.selected);
            let shown = if label.is_empty() {
                if s.selected.is_empty() {
                    s.placeholder.clone()
                } else {
                    format!("{} (unavailable)", s.selected)
                }
            } else {
                label.to_string()
            };
            let muted = label.is_empty();
            let l = layout::layout(&shown, &body, LayoutOptions::single_line());
            let a = cx.abs(r);
            let saved = cx.canvas.clip;
            cx.canvas
                .push_clip(Rect::new(a.x, a.y, a.width - theme.px(36), a.height));
            let fg = if cx.disabled {
                theme.text_disabled
            } else if muted {
                theme.text_muted
            } else {
                theme.text
            };
            layout::draw(
                &mut cx.canvas,
                &l,
                (a.x + theme.padding()) as f64,
                (a.y + (a.height - l.height.ceil() as i64) / 2) as f64,
                &body,
                fg,
            );
            cx.canvas.pop_clip(saved);
            if cx.is_focused(&s.id) {
                let rad = theme.radius();
                cx.focus_ring(r, rad)
            }
        }
        Choice::Pick(p) => {
            let st = cx.states.choice(&p.id);
            field_frame(cx, &p.id, r, st.open);
            let small = theme.font(TextRole::Small);
            let focus_token = cx.states.field(&p.id, "token", "-1").to_i64().unwrap_or(-1);
            if p.multiple {
                let mut i = 0;
                while i < p.selected.len() && i < node.parts.len() {
                    let key = &p.selected[i];
                    let tr = node.parts[i];
                    let label = label_of(&p.items, key);
                    let missing = label.is_empty();
                    let shown = if missing {
                        format!("{} (unavailable)", key)
                    } else {
                        label
                    };
                    let lit = focus_token == i as i64 && cx.is_focused(&p.id);
                    cx.fill_round(
                        tr,
                        tr.height / 2,
                        if lit { theme.selection } else { theme.surface },
                    );
                    cx.stroke_round(
                        tr,
                        tr.height / 2,
                        theme.border_width(),
                        if missing { theme.warning } else { theme.border },
                    );
                    let a = cx.abs(tr);
                    let fg = if lit {
                        theme.selection_text
                    } else {
                        theme.text
                    };
                    let l = layout::layout(&shown, &small, LayoutOptions::single_line());
                    layout::draw(
                        &mut cx.canvas,
                        &l,
                        (a.x + theme.px(10)) as f64,
                        (a.y + (a.height - l.height.ceil() as i64) / 2) as f64,
                        &small,
                        fg,
                    );
                    let isz = theme.px(theme.shape.close_icon);
                    cx.icon(
                        &Icon::Close,
                        a.right() - isz - theme.px(8),
                        a.y + (a.height - isz) / 2,
                        isz,
                        fg,
                    );
                    i += 1;
                }
                if p.selected.is_empty() {
                    let a = cx.abs(r);
                    let l = layout::layout(&p.placeholder, &body, LayoutOptions::single_line());
                    layout::draw(
                        &mut cx.canvas,
                        &l,
                        (a.x + theme.padding()) as f64,
                        (a.y + (theme.control_height() - l.height.ceil() as i64) / 2) as f64,
                        &body,
                        theme.text_muted,
                    )
                }
            } else {
                let key = if p.selected.is_empty() {
                    String::new()
                } else {
                    p.selected[0].clone()
                };
                let label = label_of(&p.items, &key);
                let shown = if key.is_empty() {
                    p.placeholder.clone()
                } else if label.is_empty() {
                    format!("{} (unavailable)", key)
                } else {
                    label.to_string()
                };
                let l = layout::layout(&shown, &body, LayoutOptions::single_line());
                let a = cx.abs(r);
                layout::draw(
                    &mut cx.canvas,
                    &l,
                    (a.x + theme.padding()) as f64,
                    (a.y + (a.height - l.height.ceil() as i64) / 2) as f64,
                    &body,
                    if key.is_empty() {
                        theme.text_muted
                    } else {
                        theme.text
                    },
                )
            }
            if cx.is_focused(&p.id) {
                let rad = theme.radius();
                cx.focus_ring(r, rad)
            }
        }
        Choice::Commands(p) => {
            let st = cx.states.choice(&p.id);
            let h = theme.control_height();
            let field = Rect::new(0, 0, r.width, h);
            cx.fill_round(field, theme.radius(), theme.field);
            let focused = cx.is_focused(&p.id);
            cx.stroke_round(
                field,
                theme.radius(),
                if focused {
                    theme.border_width() * 2
                } else {
                    theme.border_width()
                },
                if focused { theme.accent } else { theme.border },
            );
            let a = cx.abs(field);
            let isz = theme.px(theme.shape.field_icon);
            cx.icon(
                &Icon::Search,
                a.x + theme.px(10),
                a.y + (h - isz) / 2,
                isz,
                theme.text_muted,
            );
            let shown = if st.filter.is_empty() {
                &p.placeholder
            } else {
                &st.filter
            };
            let l = layout::layout(shown, &body, LayoutOptions::single_line());
            let tx = a.x + theme.px(36);
            let ty = a.y + (h - l.height.ceil() as i64) / 2;
            layout::draw(
                &mut cx.canvas,
                &l,
                tx as f64,
                ty as f64,
                &body,
                if st.filter.is_empty() {
                    theme.text_muted
                } else {
                    theme.text
                },
            );
            if focused && cx.caret_on {
                let cr = l.caret_rect(if st.filter.is_empty() {
                    0
                } else {
                    st.filter.byte_len()
                });
                cx.canvas
                    .rect(tx + cr.x, ty + cr.y, theme.px(2), cr.height, theme.text)
            }
            let list = cx.abs(Rect::new(
                0,
                h + theme.px(4),
                r.width,
                r.height - h - theme.px(4),
            ));
            let items = palette_items(&cx.registry, &st.filter);
            let mut shown_state = st;
            shown_state.items = items.clone().into();
            if shown_state.highlight_index() < 0 && !items.is_empty() {
                shown_state.highlight = items[0].key.clone()
            };
            completion::paint_list(
                &shown_state,
                list,
                "",
                &crate::localization::tr("No matching commands"),
                cx,
            );
            if focused {
                let rad = theme.radius();
                cx.focus_ring(field, rad)
            }
        }
    }
}

/// The palette's entries for `query`: matching commands, enabled first.
pub fn palette_items(registry: &[crate::commands::Command], query: &str) -> Vec<CompletionItem> {
    let mut items = vec![];
    for c in registry {
        if !c.visible {
            continue;
        };
        let mut it = CompletionItem::new(&c.id, &c.label);
        it.detail = c.shortcut_label();
        it.category = c.category.clone();
        it.disabled = !c.enabled;
        it.score = if c.enabled { 1.0 } else { 0.0 };
        items.push(it)
    }
    completion::rank(&items, query, MatchMode::Fuzzy, 200)
}

fn open_list(
    id: &str,
    node: &Node,
    items: &[CompletionItem],
    highlight: &str,
    st: &mut StateStore,
    cx: &mut EventCx,
    filterable: bool,
) {
    let mut s = st.choice(id);
    s.open = true;
    s.rollback = highlight.to_string();
    s.filter = "".to_string();
    s.set_items(items);
    if present(items, highlight) {
        s.highlight = highlight.to_string()
    };
    let idx = s.highlight_index().max(0);
    s.first = (idx - VISIBLE_ROWS / 2)
        .max(0)
        .min((items.len() as i64 - VISIBLE_ROWS).max(0));
    st.set(id, WState::Choice(s.clone()));
    let anchor = cx.to_window(Rect::new(0, 0, node.rect.width, node.rect.height));
    let mut size = completion::list_size(&s, &cx.theme, node.rect.width);
    if filterable {
        size.height += cx.theme.control_height()
    };
    let request = PopupRequest::new(id, 0, anchor, size, Placement::Below, false, true);
    let request = if filterable {
        request
    } else {
        request.closed_by_owner_press()
    };
    cx.open_popup(&request);
    cx.redraw = true;
}

fn close_list(id: &str, st: &mut StateStore, cx: &mut EventCx) {
    let mut s = st.choice(id);
    if !s.open {
        return;
    };
    s.open = false;
    s.filter = "".to_string();
    st.set(id, WState::Choice(s));
    cx.close_popup(id, 0);
    cx.redraw = true;
}

fn select_event(s: &Select, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    let mut cs = st.choice(&s.id);
    match ev {
        Event::PointerDown { button, .. } => {
            if *button != MouseButton::Left {
                return;
            };
            cx.focus_on(&s.id);
            if cs.open {
                close_list(&s.id, st, cx)
            } else {
                open_list(&s.id, node, &s.items, &s.selected, st, cx, false)
            };
            cx.consume()
        }
        Event::KeyDown { key, mods, .. } => {
            if cs.open {
                match key {
                    Key::Return | Key::Space | Key::KpEnter => {
                        if let Some(it) = cs.current() {
                            if it.key != s.selected {
                                cx.emit(&UiMsg::Select(s.id.clone(), it.key))
                            };
                            cx.announce(&crate::localization::message(
                                "choices",
                                "{item} selected",
                                1,
                                &crate::hmap! { "item" => it.label },
                            ))
                        }
                        close_list(&s.id, st, cx);
                        cx.consume()
                    }
                    Key::Escape => {
                        // Rollback: nothing was committed while browsing.
                        close_list(&s.id, st, cx);
                        cx.consume()
                    }
                    Key::Tab => close_list(&s.id, st, cx),
                    Key::Home | Key::End => {
                        cs.highlight_edge(*key == Key::End, VISIBLE_ROWS);
                        st.set(&s.id, WState::Choice(cs));
                        cx.redraw = true;
                        cx.consume()
                    }
                    _ => {
                        if mods.alt && *key == Key::Up {
                            close_list(&s.id, st, cx);
                            cx.consume()
                        } else if completion::list_key(&mut cs, *key, cx) {
                            st.set(&s.id, WState::Choice(cs));
                            cx.consume()
                        }
                    }
                }
                return;
            }
            // Closed: Alt+Down, F4, Space, and Enter open; arrows select directly.
            if (mods.alt && *key == Key::Down)
                || *key == Key::F4
                || *key == Key::Space
                || *key == Key::Return
            {
                open_list(&s.id, node, &s.items, &s.selected, st, cx, false);
                cx.consume();
                return;
            }
            if *key == Key::Up || *key == Key::Down || *key == Key::Home || *key == Key::End {
                let mut temp = cs;
                temp.set_items(&s.items);
                if present(&s.items, &s.selected) {
                    temp.highlight = s.selected.clone();
                    if *key == Key::Home || *key == Key::End {
                        temp.highlight_edge(*key == Key::End, VISIBLE_ROWS)
                    } else {
                        temp.move_highlight(
                            if *key == Key::Down { 1 } else { -1 },
                            VISIBLE_ROWS,
                            true,
                        )
                    }
                } else {
                    // With nothing chosen, Down and Home take the first item,
                    // Up and End the last.
                    temp.highlight_edge(*key == Key::Up || *key == Key::End, VISIBLE_ROWS)
                }
                if let Some(it) = temp.current()
                    && it.key != s.selected
                {
                    cx.emit(&UiMsg::Select(s.id.clone(), it.key))
                }
                cx.consume()
            }
        }
        Event::Text(t) => {
            // Typeahead: when closed it commits, when open it highlights.
            let mut temp = cs.clone();
            if !cs.open {
                temp.set_items(&s.items);
                if present(&s.items, &s.selected) {
                    temp.highlight = s.selected.clone()
                }
            }
            if let Some(key) = temp.typeahead_key(t, cx.now, 900) {
                if cs.open {
                    st.set(&s.id, WState::Choice(temp));
                    cx.redraw = true;
                } else {
                    let mut kept = cs;
                    kept.typeahead = temp.typeahead;
                    kept.typeahead_time = temp.typeahead_time;
                    st.set(&s.id, WState::Choice(kept));
                    if key != s.selected {
                        cx.emit(&UiMsg::Select(s.id.clone(), key))
                    }
                }
            } else {
                let mut kept = if cs.open { temp.clone() } else { cs };
                kept.typeahead = temp.typeahead;
                kept.typeahead_time = temp.typeahead_time;
                st.set(&s.id, WState::Choice(kept))
            }
            cx.consume()
        }
        Event::FocusOut => close_list(&s.id, st, cx),
        _ => (),
    }
}

// Picker popup: a filter line above the list.
fn picker_list_rect(rect: Rect, theme: &Theme) -> Rect {
    let h = theme.control_height();
    Rect::new(rect.x, rect.y + h, rect.width, rect.height - h)
}

fn picker_items(p: &Picker, filter: &str) -> Vec<CompletionItem> {
    completion::rank(&p.items, filter, MatchMode::Substring, 0)
}

fn toggle_key(p: &Picker, key: &str, cx: &mut EventCx) {
    if p.multiple {
        let mut next = p.selected.clone();
        if next.iter().any(|k| k == key) {
            next.retain(|k| k != key);
            cx.announce(&crate::localization::message(
                "choices",
                "{item} removed",
                1,
                &crate::hmap! { "item" => label_of(&p.items, key) },
            ))
        } else {
            next.push(key.to_string());
            cx.announce(&crate::localization::message(
                "choices",
                "{item} added",
                1,
                &crate::hmap! { "item" => label_of(&p.items, key) },
            ))
        }
        cx.emit(&UiMsg::SelectMany(p.id.clone(), next.to_vec()))
    } else if p.selected.is_empty() || p.selected[0] != key {
        cx.emit(&UiMsg::Select(p.id.clone(), key.to_string()))
    }
}

fn picker_event(p: &Picker, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    let mut cs = st.choice(&p.id);
    let token = st.field(&p.id, "token", "-1").to_i64().unwrap_or(-1);
    match ev {
        Event::PointerDown { x, y, button, .. } => {
            if *button != MouseButton::Left {
                return;
            };
            cx.focus_on(&p.id);
            let lx = x - cx.origin_x;
            let ly = y - cx.origin_y;
            // A token's remove box removes it.
            let mut i = 0;
            while i < node.parts.len() && i < p.selected.len() {
                let tr = node.parts[i];
                if tr.contains(lx, ly) {
                    if lx >= tr.right() - cx.theme.px(cx.theme.shape.close_icon + 14) {
                        toggle_key(p, &p.selected[i], cx)
                    };
                    st.set_field(&p.id, "token", &format!("{}", i));
                    cx.consume();
                    return;
                }
                i += 1;
            }
            if cs.open {
                close_list(&p.id, st, cx)
            } else {
                let hl = if p.selected.is_empty() {
                    String::new()
                } else {
                    p.selected[p.selected.len() - 1].clone()
                };
                open_list(&p.id, node, &picker_items(p, ""), &hl, st, cx, true)
            }
            cx.consume()
        }
        Event::Text(t) => {
            if !cs.open {
                let hl = if p.selected.is_empty() {
                    String::new()
                } else {
                    p.selected[0].clone()
                };
                open_list(&p.id, node, &picker_items(p, ""), &hl, st, cx, true);
                cs = st.choice(&p.id);
            }
            cs.filter += t;
            cs.set_items(&picker_items(p, &cs.filter));
            st.set(&p.id, WState::Choice(cs));
            cx.redraw = true;
            cx.consume()
        }
        Event::KeyDown { key, .. } => {
            if cs.open {
                match key {
                    Key::Return | Key::KpEnter | Key::Space => {
                        if *key == Key::Space && !cs.filter.is_empty() {
                            cs.filter += " ";
                            cs.set_items(&picker_items(p, &cs.filter));
                            st.set(&p.id, WState::Choice(cs))
                        } else if let Some(it) = cs.current() {
                            toggle_key(p, &it.key, cx);
                            if !p.multiple {
                                close_list(&p.id, st, cx)
                            }
                        }
                        cx.consume()
                    }
                    Key::Backspace => {
                        if !cs.filter.is_empty() {
                            cs.filter = cs
                                .filter
                                .substring(
                                    0,
                                    crate::editing::prev_boundary(&cs.filter, cs.filter.byte_len()),
                                )
                                .to_string();
                            cs.set_items(&picker_items(p, &cs.filter));
                            st.set(&p.id, WState::Choice(cs));
                            cx.redraw = true;
                        }
                        cx.consume()
                    }
                    Key::Escape => {
                        if cs.filter.is_empty() {
                            close_list(&p.id, st, cx)
                        } else {
                            cs.filter = "".to_string();
                            cs.set_items(&picker_items(p, ""));
                            st.set(&p.id, WState::Choice(cs));
                            cx.redraw = true;
                        }
                        cx.consume()
                    }
                    Key::Tab => close_list(&p.id, st, cx),
                    _ => {
                        if completion::list_key(&mut cs, *key, cx) {
                            st.set(&p.id, WState::Choice(cs));
                            cx.consume()
                        }
                    }
                }
                return;
            }
            if matches!(*key, Key::Down | Key::Return | Key::F4) {
                let hl = if p.selected.is_empty() {
                    String::new()
                } else {
                    p.selected[0].clone()
                };
                open_list(&p.id, node, &picker_items(p, ""), &hl, st, cx, true);
                cx.consume();
                return;
            }
            if p.multiple && !p.selected.is_empty() {
                let n = p.selected.len();
                let back = if cx.theme.rtl { Key::Right } else { Key::Left };
                let fwd = if cx.theme.rtl { Key::Left } else { Key::Right };
                if *key == back {
                    st.set_field(
                        &p.id,
                        "token",
                        &format!(
                            "{}",
                            if token < 0 {
                                n as i64 - 1
                            } else {
                                (token - 1).max(0)
                            }
                        ),
                    );
                    cx.redraw = true;
                    cx.consume()
                } else if *key == fwd && token >= 0 {
                    st.set_field(
                        &p.id,
                        "token",
                        &format!("{}", if token + 1 >= n as i64 { -1 } else { token + 1 }),
                    );
                    cx.redraw = true;
                    cx.consume()
                } else if *key == Key::Backspace || *key == Key::Delete {
                    let at = if token >= 0 {
                        token
                    } else if *key == Key::Backspace {
                        n as i64 - 1
                    } else {
                        -1
                    };
                    if at >= 0 && at < n as i64 {
                        toggle_key(p, &p.selected[at as usize], cx);
                        st.set_field(
                            &p.id,
                            "token",
                            &format!(
                                "{}",
                                if n - 1 == 0 {
                                    -1
                                } else {
                                    at.min((n - 2) as i64)
                                }
                            ),
                        )
                    }
                    cx.consume()
                }
            }
        }
        Event::FocusOut => {
            close_list(&p.id, st, cx);
            st.set_field(&p.id, "token", "-1")
        }
        _ => (),
    }
}

fn palette_event(p: &Palette, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    let mut cs = st.choice(&p.id);
    let items = palette_items(&cx.registry, &cs.filter);
    cs.items = items.clone().into();
    if cs.highlight_index() < 0 && !items.is_empty() {
        cs.highlight = items[0].key.clone()
    };
    let h = cx.theme.control_height();
    let list = cx.to_window(Rect::new(
        0,
        h + cx.theme.px(4),
        node.rect.width,
        node.rect.height - h - cx.theme.px(4),
    ));
    match ev {
        Event::PointerDown { x, y, button, .. } => {
            if *button != MouseButton::Left {
                return;
            };
            cx.focus_on(&p.id);
            let key = completion::list_hit(&cs, list, &cx.theme, *x, *y);
            if !key.is_empty() {
                cs.highlight = key;
                if let Some(it) = cs.current() {
                    cx.run_command(&it.key)
                }
            }
            cx.consume()
        }
        Event::Text(t) => {
            cs.filter += t;
            cs.set_items(&palette_items(&cx.registry, &cs.filter));
            cx.redraw = true;
            cx.consume()
        }
        Event::KeyDown { key, .. } => match key {
            Key::Return | Key::KpEnter => {
                if let Some(it) = cs.current() {
                    cx.run_command(&it.key);
                    cs.filter = "".to_string();
                } else if let Some(it) = cs.items.iter().find(|i| i.key == cs.highlight) {
                    cx.announce(&crate::localization::message(
                        "choices",
                        "{item} is unavailable",
                        1,
                        &crate::hmap! { "item" => it.label.clone() },
                    ))
                }
                cx.consume()
            }
            Key::Backspace => {
                if !cs.filter.is_empty() {
                    cs.filter = cs
                        .filter
                        .substring(
                            0,
                            crate::editing::prev_boundary(&cs.filter, cs.filter.byte_len()),
                        )
                        .to_string();
                    cs.set_items(&palette_items(&cx.registry, &cs.filter));
                    cx.redraw = true;
                }
                cx.consume()
            }
            Key::Escape => {
                if !cs.filter.is_empty() {
                    cs.filter = "".to_string();
                    cs.set_items(&palette_items(&cx.registry, ""));
                    cx.redraw = true;
                    cx.consume()
                }
            }
            _ => {
                if completion::list_key(&mut cs, *key, cx) {
                    cx.consume()
                }
            }
        },
        _ => (),
    }
    st.set(&p.id, WState::Choice(cs))
}

pub fn choice_event(ch: &Choice, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    if cx.disabled {
        return;
    }
    sync_model(ch, st);
    match ch {
        Choice::One(s) => select_event(s, node, ev, st, cx),
        Choice::Pick(p) => picker_event(p, node, ev, st, cx),
        Choice::Commands(p) => palette_event(p, node, ev, st, cx),
    }
    sync_model(ch, st)
}

pub fn choice_popup_paint(ch: &Choice, rect: Rect, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    match ch {
        Choice::One(s) => {
            let mut cs = cx.states.choice(&s.id);
            cs.loading = s.loading;
            cs.error = s.error.clone();
            completion::paint_list(
                &cs,
                rect,
                &s.selected,
                &crate::localization::tr("No items"),
                cx,
            )
        }
        Choice::Pick(p) => {
            let cs = cx.states.choice(&p.id);
            let h = theme.control_height();
            cx.canvas
                .round_rect(rect.x, rect.y, rect.width, h, theme.radius(), theme.field);
            cx.canvas.round_rect_stroke(
                rect.x,
                rect.y,
                rect.width,
                h,
                theme.radius(),
                theme.border_width(),
                theme.accent,
            );
            let isz = theme.px(theme.shape.field_icon);
            cx.icon(
                &Icon::Search,
                rect.x + theme.px(10),
                rect.y + (h - isz) / 2,
                isz,
                theme.text_muted,
            );
            let body = theme.font(TextRole::Body);
            let shown = if cs.filter.is_empty() {
                crate::localization::tr("Type to filter")
            } else {
                cs.filter.clone()
            };
            let l = layout::layout(&shown, &body, LayoutOptions::single_line());
            layout::draw(
                &mut cx.canvas,
                &l,
                (rect.x + theme.px(36)) as f64,
                (rect.y + (h - l.height.ceil() as i64) / 2) as f64,
                &body,
                if cs.filter.is_empty() {
                    theme.text_muted
                } else {
                    theme.text
                },
            );
            let mut marked = cs;
            marked.loading = p.loading;
            marked.error = p.error.clone();
            // Checkmarks show every selected key, not just one.
            let list = picker_list_rect(rect, &theme);
            completion::paint_list(
                &marked,
                list,
                if p.multiple || p.selected.is_empty() {
                    ""
                } else {
                    &p.selected[0]
                },
                &crate::localization::tr("No matches"),
                cx,
            );
            if p.multiple {
                let row = theme.row_height();
                let mut i = marked.first;
                while i < marked.items.len() as i64 && i < marked.first + VISIBLE_ROWS {
                    if p.selected.contains(&marked.items[i as usize].key) {
                        let y = list.y + theme.px(4) + (i - marked.first) * row;
                        cx.icon(
                            &Icon::Check,
                            list.x + theme.px(12),
                            y + (row - theme.px(theme.shape.field_icon)) / 2,
                            theme.px(theme.shape.field_icon),
                            if marked.items[i as usize].key == marked.highlight {
                                theme.selection_text
                            } else {
                                theme.text
                            },
                        )
                    }
                    i += 1;
                }
            }
        }
        Choice::Commands(_) => (),
    }
}

pub fn choice_popup_event(
    ch: &Choice,
    rect: Rect,
    ev: &Event,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    sync_model(ch, st);
    let id = ch.id();
    let mut cs = st.choice(&id);
    let list = match ch {
        Choice::Pick(_) => picker_list_rect(rect, &cx.theme),
        _ => rect,
    };
    match ev {
        Event::PointerMove { x, y, .. } => {
            let key = completion::list_hit(&cs, list, &cx.theme, *x, *y);
            if !key.is_empty() && key != cs.highlight {
                cs.highlight = key;
                st.set(&id, WState::Choice(cs));
                cx.redraw = true;
            }
        }
        Event::PointerUp { x, y, .. } => {
            let key = completion::list_hit(&cs, list, &cx.theme, *x, *y);
            if !key.is_empty() {
                cs.highlight = key;
                st.set(&id, WState::Choice(cs.clone()));
                if let Some(it) = cs.current() {
                    match ch {
                        Choice::One(s) => {
                            if it.key != s.selected {
                                cx.emit(&UiMsg::Select(s.id.clone(), it.key))
                            };
                            close_list(&s.id, st, cx)
                        }
                        Choice::Pick(p) => {
                            toggle_key(p, &it.key, cx);
                            if !p.multiple {
                                close_list(&p.id, st, cx)
                            }
                        }
                        _ => (),
                    }
                }
            }
        }
        Event::Scroll { dy, .. } => {
            let max_first = cs.items.len() - VISIBLE_ROWS as usize;
            cs.first = (cs.first - dy.round() as i64 * 3)
                .max(0)
                .min(max_first as i64);
            st.set(&id, WState::Choice(cs));
            cx.redraw = true;
        }
        _ => (),
    }
    cx.consume()
}

/// An assistive-technology action on a choice's node `key` (the part of
/// its semantic id after `#`): pressing a list item chooses it, pressing a
/// token removes it.
pub fn accessible(ch: &Choice, key: &str, action: &str, cx: &mut EventCx) {
    if action != "press" {
        return;
    };
    match ch {
        Choice::One(s) => {
            if !s.loading
                && s.error.is_empty()
                && s.items.iter().any(|it| it.key == key && !it.disabled)
                && key != s.selected
            {
                cx.emit(&UiMsg::Select(s.id.clone(), key.to_string()))
            }
        }
        Choice::Pick(p) => {
            if let Some(token) = key
                .strip_prefix("token:")
                .filter(|t| p.selected.iter().any(|s| s == t))
            {
                toggle_key(p, token, cx)
            }
        }
        Choice::Commands(_) => (),
    }
}

pub fn choice_semantics(ch: &Choice, node: &Node, st: &StateStore) -> Option<AccessNode> {
    match ch {
        Choice::One(s) => {
            let mut n = AccessNode::control(Role::ComboBox, &s.id, &s.label, "open");
            n.value = label_of(&s.items, &s.selected);
            let cs = st.choice(&s.id);
            n.expanded = if cs.open { 1 } else { 0 };
            if cs.open {
                n.children.push(completion::list_semantics(
                    &cs,
                    &format!("{}#list", s.id),
                    &s.label,
                    &s.id,
                    &s.selected,
                ))
            };
            if s.loading {
                n.description = crate::localization::tr("Loading")
            };
            if !s.error.is_empty() {
                n.description = s.error.clone()
            };
            Some(n)
        }
        Choice::Pick(p) => {
            let mut n = AccessNode::control(Role::ComboBox, &p.id, &p.label, "open");
            n.value = p
                .selected
                .iter()
                .map(|k| label_of(&p.items, k))
                .collect::<Vec<_>>()
                .join(", ");
            let cs = st.choice(&p.id);
            n.expanded = if cs.open { 1 } else { 0 };
            for (i, key) in p.selected.iter().enumerate() {
                let label = label_of(&p.items, key);
                let mut tok = AccessNode::new(
                    Role::Button,
                    &crate::localization::message(
                        "choices",
                        "Remove {item}",
                        1,
                        &crate::hmap! { "item" => if label.is_empty() { key.clone() } else { label } },
                    ),
                );
                tok.id = format!("{}#token:{}", p.id, key);
                tok.actions.push("press".to_string());
                if i < node.parts.len() {
                    tok.bounds = node.parts[i]
                };
                n.children.push(tok);
            }
            Some(n)
        }
        Choice::Commands(p) => {
            let mut n = AccessNode::control(
                Role::SearchField,
                &p.id,
                &crate::localization::tr("Command search"),
                "focus",
            );
            let cs = st.choice(&p.id);
            n.value = cs.filter;
            n.description = crate::localization::message(
                "choices",
                "{count} commands",
                cs.items.len() as i64,
                &HashMap::new(),
            );
            Some(n)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Picker, palette_items, token_rects};
    use crate::commands::Command;

    use crate::style::Theme;
    use crate::widgets::completion::{ChoiceState, CompletionItem};
    use crate::widgets::state::{StateStore, WState};

    #[test]
    fn palette_lists_enabled_matches_first() {
        let reg = vec![
            Command::new("a.open", "Open File"),
            Command::new("a.close", "Close File").enabled(false),
            Command::new("b", "Other"),
        ];
        let items = palette_items(&reg, "file");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].key, "a.open");
        assert!(items[1].disabled)
    }

    #[test]
    fn open_models_preserve_keys_and_filters_but_drop_deleted_and_disabled_matches() {
        let state = ChoiceState {
            open: true,
            filter: "dup".to_string(),
            ..ChoiceState::default()
        };
        let mut store = StateStore::new();
        store.set("p", WState::Choice(state));
        let mut p = Picker {
            id: "p".to_string(),
            items: vec![
                CompletionItem::new("a", "Duplicate"),
                CompletionItem::new("b", "Duplicate"),
            ]
            .into(),
            selected: vec!["missing".to_string()],
            multiple: true,
            placeholder: "".to_string(),
            label: "".to_string(),
            loading: false,
            error: "".to_string(),
        };
        super::sync_model(&super::Choice::Pick(p.clone()), &mut store);
        assert_eq!(store.choice("p").highlight, "a");
        p.items = vec![p.items[1].clone()].into();
        super::sync_model(&super::Choice::Pick(p.clone()), &mut store);
        assert_eq!(store.choice("p").highlight, "b");
        assert_eq!(store.choice("p").filter, "dup");
        let mut changed = p.items.to_vec();
        changed[0].disabled = true;
        p.items = changed.into();
        super::sync_model(&super::Choice::Pick(p.clone()), &mut store);
        assert!(store.choice("p").current().is_none());
        let mut changed = p.items.to_vec();
        changed[0].disabled = false;
        p.items = changed.into();
        p.loading = true;
        super::sync_model(&super::Choice::Pick(p.clone()), &mut store);
        assert!(store.choice("p").items.is_empty());
        assert!(store.choice("p").loading);
        p.loading = false;
        p.error = "Unavailable".to_string();
        super::sync_model(&super::Choice::Pick(p.clone()), &mut store);
        assert!(store.choice("p").current().is_none());
        p.error = "".to_string();
        super::sync_model(&super::Choice::Pick(p.clone()), &mut store);
        assert_eq!(store.choice("p").highlight, "b");
        assert_eq!(p.selected, vec!["missing"])
    }

    #[test]
    fn tokens_wrap_onto_new_rows() {
        let mut items = vec![];
        let mut keys = vec![];
        let mut i = 0;
        while i < 12 {
            items.push(CompletionItem::new(
                &format!("k{}", i),
                &format!("Item number {}", i),
            ));
            keys.push(format!("k{}", i));
            i += 1;
        }
        let p = Picker {
            id: "p".to_string(),
            items: items.into(),
            selected: keys,
            multiple: true,
            placeholder: "".to_string(),
            label: "".to_string(),
            loading: false,
            error: "".to_string(),
        };
        let rects = token_rects(&p, &Theme::light(), 300);
        assert_eq!(rects.len(), 12);
        assert!(rects[11].y > rects[0].y)
    }
}

#[cfg(test)]
mod session_tests {
    use crate::strext::StrExt;
    use crate::widgets::Widget;
    use crate::widgets::cx::UiMsg;
    use windowing::{Key, Mods};

    fn revised_items(revision: i64) -> Vec<crate::widgets::completion::CompletionItem> {
        let mut items = Widget::items(&[
            ("a".to_string(), "Duplicate".to_string()),
            ("b".to_string(), "Duplicate".to_string()),
            ("c".to_string(), "Other".to_string()),
        ]);
        if revision >= 1 {
            items = vec![items[2].clone(), items[1].clone()];
        }
        if revision == 2 {
            items[0].disabled = true;
            items[1].disabled = true;
        }
        if revision >= 3 {
            items.clear();
        }
        items
    }

    #[test]
    fn a_filtered_picker_whose_items_all_go_keeps_its_filter() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let view = |revision: &i64| {
                Widget::column(
                    &[
                        Widget::select("choice", &revised_items(*revision), "", "Choose"),
                        Widget::picker(
                            "many",
                            &revised_items(*revision),
                            &["a".to_string()],
                            true,
                            "Many",
                        ),
                    ],
                    4,
                )
            };
            let update = |revision: i64, msg: UiMsg| match msg {
                UiMsg::Click(id) if id == "change" => revision + 1,
                _ => revision,
            };
            let mut s = crate::application(
                "Revised",
                "dev.gossamer.gui-revised-test",
                || 0,
                update,
                view,
            )
            .size(420, 280)
            .start()
            .unwrap();
            s.emit(&UiMsg::Click("change".to_string()));
            s.emit(&UiMsg::Click("change".to_string()));
            s.flush().unwrap();
            assert!(s.focus("many").unwrap());
            s.type_text("Dup").unwrap();
            s.emit(&UiMsg::Click("change".to_string()));
            s.flush().unwrap();
            let empty = s.windows[0].states.choice("many");
            assert_eq!(empty.filter, "Dup");
            assert!(empty.items.is_empty());
            s.press(Key::Down, Mods::default()).unwrap();
            s.press(Key::Return, Mods::default()).unwrap();
            s.shutdown()
        })
    }

    #[derive(Debug, Clone, Default, PartialEq)]
    struct Picks {
        fruit: String,
        tags: Vec<String>,
        city: String,
        log: Vec<String>,
    }

    fn update(m: Picks, msg: UiMsg) -> Picks {
        let mut n = m;
        match msg {
            UiMsg::Select(id, key) => {
                n.log.push(format!("{}={}", id, key));
                if id == "fruit" {
                    n.fruit = key.to_string()
                }
            }
            UiMsg::SelectMany(id, keys) => {
                n.log.push(format!("{}={}", id, keys.join(",")));
                n.tags = keys.to_vec();
            }
            UiMsg::Text(id, text) if id == "city" => n.city = text.to_string(),
            _ => (),
        }
        n
    }

    fn view(m: &Picks) -> Widget {
        let fruit = Widget::items(&[
            ("a".to_string(), "Apple".to_string()),
            ("b".to_string(), "Banana".to_string()),
            ("c".to_string(), "Cherry".to_string()),
        ]);
        let tags = Widget::items(&[
            ("apple".to_string(), "Apple".to_string()),
            ("banana".to_string(), "Banana".to_string()),
            ("mango".to_string(), "Mango".to_string()),
            ("orange".to_string(), "Orange".to_string()),
            ("kiwi".to_string(), "Kiwi".to_string()),
        ]);
        let cities = Widget::items(&[
            ("paris".to_string(), "Paris".to_string()),
            ("rome".to_string(), "Rome".to_string()),
        ]);
        Widget::column(
            &[
                Widget::select("fruit", &fruit, &m.fruit, "Fruit"),
                Widget::picker("tags", &tags, &m.tags, true, "Tags"),
                Widget::combo("city", &m.city, &cities, true, "City"),
                Widget::button("after", "After"),
                Widget::label(&format!("city is {}", m.city)),
                Widget::label(&format!("log {}", m.log.join(" "))),
            ],
            8,
        )
        .padding(8)
    }

    fn press(s: &mut crate::Session, key: Key) {
        s.press(key, Mods::default()).unwrap()
    }

    // The messages the choices reported, from the view's log line.
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

    fn start(initial: Picks) -> crate::Session {
        crate::application(
            "Choices",
            "dev.gossamer.gui-choice-test",
            move || initial.clone(),
            update,
            view,
        )
        .size(420, 360)
        .start()
        .unwrap()
    }

    #[test]
    fn a_select_browses_by_keyboard_and_rolls_back_on_escape() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = start(Picks::default());
            assert!(s.focus("fruit").unwrap());
            // Closed, the arrows and typeahead commit directly.
            press(&mut s, Key::Down);
            press(&mut s, Key::Down);
            s.type_text("c").unwrap();
            // Open, browsing commits nothing until Enter; Escape keeps Cherry.
            press(&mut s, Key::Return);
            press(&mut s, Key::Up);
            press(&mut s, Key::Escape);
            // Alt+Down opens; Home then Enter commits Apple and keeps focus.
            s.press(
                Key::Down,
                Mods {
                    alt: true,
                    ..Mods::default()
                },
            )
            .unwrap();
            press(&mut s, Key::Home);
            press(&mut s, Key::Return);
            assert_eq!(s.focused(), "fruit");
            assert_eq!(log(&s), "fruit=a fruit=b fruit=c fruit=a");
            // Assistive technology chooses an item by pressing it.
            s.accessible_action("fruit", "fruit#b", "press", "")
                .unwrap();
            assert_eq!(log(&s), "fruit=a fruit=b fruit=c fruit=a fruit=b");
            s.shutdown()
        })
    }

    #[test]
    fn clicking_an_open_select_closes_it() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = start(Picks::default());
            assert!(s.click("fruit").unwrap());
            assert_eq!(
                s.popup_owners()
                    .iter()
                    .map(|p| p.0.clone())
                    .collect::<Vec<_>>(),
                vec!["fruit"]
            );
            assert!(s.click("fruit").unwrap());
            assert!(s.popup_owners().is_empty(), "{:?}", s.popup_owners());
            assert!(s.click("fruit").unwrap());
            assert_eq!(
                s.popup_owners()
                    .iter()
                    .map(|p| p.0.clone())
                    .collect::<Vec<_>>(),
                vec!["fruit"]
            );
            s.shutdown()
        })
    }

    #[test]
    fn a_multi_picker_keeps_its_selection_across_filters_and_removes_tokens() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = start(Picks::default());
            assert!(s.focus("tags").unwrap());
            s.type_text("ban").unwrap();
            press(&mut s, Key::Return);
            // Escape clears the query first; a new query keeps Banana selected.
            press(&mut s, Key::Escape);
            s.type_text("kiwi").unwrap();
            press(&mut s, Key::Return);
            press(&mut s, Key::Escape);
            press(&mut s, Key::Escape);
            // Closed, Backspace removes the last token; Left then Delete the first.
            press(&mut s, Key::Backspace);
            press(&mut s, Key::Left);
            press(&mut s, Key::Delete);
            assert_eq!(log(&s), "tags=banana tags=banana,kiwi tags=banana tags=");
            // Each token is a "Remove" button assistive technology can press.
            s.type_text("apple").unwrap();
            press(&mut s, Key::Return);
            press(&mut s, Key::Escape);
            press(&mut s, Key::Escape);
            assert!(
                s.semantics(0).dump().contains("button \"Remove Apple\""),
                "{}",
                s.semantics(0).dump()
            );
            s.accessible_action("tags", "tags#token:apple", "press", "")
                .unwrap();
            assert!(log(&s).ends_with("tags=apple tags="), "{}", log(&s));
            s.shutdown()
        })
    }

    #[test]
    fn a_strict_combo_restores_its_item_when_left_with_other_text() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = start(Picks {
                city: "Rome".to_string(),
                ..Picks::default()
            });
            assert!(s.focus("city").unwrap());
            s.type_text("x").unwrap();
            press(&mut s, Key::Tab);
            assert!(
                s.semantics(0).dump().contains("city is Rome"),
                "{}",
                s.semantics(0).dump()
            );
            // Accepting a proposal is what a later blur restores.
            assert!(s.focus("city").unwrap());
            primary_select_all(&mut s);
            s.type_text("Par").unwrap();
            press(&mut s, Key::Down);
            press(&mut s, Key::Return);
            s.type_text("zz").unwrap();
            press(&mut s, Key::Tab);
            assert!(
                s.semantics(0).dump().contains("city is Paris"),
                "{}",
                s.semantics(0).dump()
            );
            s.shutdown()
        })
    }

    #[derive(Debug, Clone, Default, PartialEq)]
    struct Lookup {
        text: String,
        requests: Vec<(i64, String)>,
    }

    fn lookup_update(m: Lookup, msg: UiMsg) -> Lookup {
        let mut n = m;
        match msg {
            UiMsg::CompletionRequest(_, request, query, _) => {
                n.requests.push((request, query.to_string()))
            }
            UiMsg::Text(id, text) if id == "q" => n.text = text.to_string(),
            _ => (),
        }
        n
    }

    fn lookup_view(m: &Lookup) -> Widget {
        let completion = crate::widgets::completion::Completion::asynchronous(0);
        Widget::column(
            &[
                Widget::autocomplete("q", &m.text, &completion, "City"),
                Widget::label(&format!("text {} asked {:?}", m.text, m.requests)),
            ],
            8,
        )
        .padding(8)
    }

    #[test]
    fn late_answers_are_dropped_and_a_failure_retries_its_query() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Lookup",
                "dev.gossamer.gui-lookup-test",
                Lookup::default,
                lookup_update,
                lookup_view,
            )
            .size(420, 320)
            .start()
            .unwrap();
            assert!(s.focus("q").unwrap());
            s.type_text("p").unwrap();
            s.advance(10).unwrap();
            s.type_text("a").unwrap();
            s.advance(10).unwrap();
            assert!(
                s.semantics(0)
                    .dump()
                    .contains("asked [(1, \"p\"), (2, \"pa\")]"),
                "{}",
                s.semantics(0).dump()
            );
            // The answer to the newer query arrives first; the older one is late.
            crate::perform(crate::app::effects::Effect::Complete(
                "q".to_string(),
                2,
                Widget::items(&[("paris".to_string(), "Paris".to_string())]),
                "".to_string(),
            ));
            crate::perform(crate::app::effects::Effect::Complete(
                "q".to_string(),
                1,
                Widget::items(&[("prague".to_string(), "Prague".to_string())]),
                "".to_string(),
            ));
            s.flush().unwrap();
            press(&mut s, Key::Return);
            assert!(
                s.semantics(0).dump().contains("text Paris"),
                "{}",
                s.semantics(0).dump()
            );
            // Accepting cancels whatever was outstanding, so the next query is
            // request 4. A failed answer shows its error; Enter asks again.
            primary_select_all(&mut s);
            s.type_text("r").unwrap();
            s.advance(10).unwrap();
            crate::perform(crate::app::effects::Effect::Complete(
                "q".to_string(),
                4,
                vec![],
                "offline".to_string(),
            ));
            s.flush().unwrap();
            press(&mut s, Key::Return);
            s.advance(10).unwrap();
            assert!(
                s.semantics(0).dump().contains("(4, \"r\"), (5, \"r\")]"),
                "{}",
                s.semantics(0).dump()
            );
            assert!(
                s.semantics(0).dump().contains("text r"),
                "{}",
                s.semantics(0).dump()
            );
            s.shutdown()
        })
    }

    fn numbered(n: i64) -> Vec<crate::widgets::completion::CompletionItem> {
        (0..n)
            .map(|i| {
                crate::widgets::completion::CompletionItem::new(
                    &format!("k{}", i),
                    &format!("Item {}", i),
                )
            })
            .collect()
    }

    #[test]
    fn an_open_select_exposes_the_rows_it_shows() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let twenty = numbered(20);
            let view = move |m: &Picks| {
                Widget::column(&[Widget::select("fruit", &twenty, &m.fruit, "Item")], 8).padding(8)
            };
            let mut s = crate::application(
                "Rows",
                "dev.gossamer.gui-rows-test",
                Picks::default,
                update,
                view,
            )
            .size(420, 360)
            .start()
            .unwrap();
            assert!(s.focus("fruit").unwrap());
            press(&mut s, Key::Return);
            let top = s.semantics(0).dump();
            assert!(
                top.contains("\"Item 0\"") && top.contains("1/20") && top.contains("8/20"),
                "{}",
                top
            );
            assert!(
                !top.contains("9/20") && !top.contains("\"Item 8\""),
                "{}",
                top
            );
            press(&mut s, Key::End);
            let bottom = s.semantics(0).dump();
            assert!(
                bottom.contains("\"Item 19\"")
                    && bottom.contains("13/20")
                    && bottom.contains("20/20"),
                "{}",
                bottom
            );
            assert!(
                !bottom.contains("12/20") && !bottom.contains("\"Item 0\""),
                "{}",
                bottom
            );
            s.shutdown()
        })
    }

    #[derive(Debug, Clone, Default, PartialEq)]
    struct Owner {
        hidden: bool,
        fruit: String,
        text: String,
        log: Vec<String>,
    }

    fn owner_update(m: Owner, msg: UiMsg) -> Owner {
        let mut n = m;
        match msg {
            UiMsg::Click(id) if id == "toggle" => n.hidden = !n.hidden,
            UiMsg::Select(id, key) => n.log.push(format!("{}={}", id, key)),
            UiMsg::Text(id, text) if id == "q" => n.text = text.to_string(),
            UiMsg::CompletionRequest(_, request, query, _) => {
                n.log.push(format!("asked {} {}", request, query))
            }
            _ => (),
        }
        n
    }

    fn owner_view(m: &Owner) -> Widget {
        let fruit = Widget::items(&[
            ("a".to_string(), "Apple".to_string()),
            ("b".to_string(), "Banana".to_string()),
            ("c".to_string(), "Cherry".to_string()),
        ]);
        let completion = crate::widgets::completion::Completion::asynchronous(0);
        let mut rows = vec![Widget::button("toggle", "Toggle")];
        if !m.hidden {
            rows.push(Widget::select("fruit", &fruit, &m.fruit, "Fruit"));
            rows.push(Widget::autocomplete("q", &m.text, &completion, "City"))
        }
        rows.push(Widget::label(&format!("log {}", m.log.join(" "))));
        Widget::column(&rows, 8).padding(8)
    }

    #[test]
    fn removing_an_owner_closes_its_popup_and_drops_its_answers() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Owner",
                "dev.gossamer.gui-owner-test",
                Owner::default,
                owner_update,
                owner_view,
            )
            .size(420, 360)
            .start()
            .unwrap();
            assert!(s.focus("fruit").unwrap());
            press(&mut s, Key::Return);
            press(&mut s, Key::Down);
            assert_eq!(
                s.popup_owners()
                    .iter()
                    .map(|p| p.0.clone())
                    .collect::<Vec<_>>(),
                vec!["fruit"]
            );
            // The select goes away while its list is open: the popup closes with
            // it, focus leaves it, and keys reach nothing that was there.
            s.emit(&UiMsg::Click("toggle".to_string()));
            s.flush().unwrap();
            assert!(s.popup_owners().is_empty(), "{:?}", s.popup_owners());
            assert!(s.focused() != "fruit", "{}", s.focused());
            press(&mut s, Key::Return);
            press(&mut s, Key::Down);
            assert_eq!(log(&s), "");
            // Shown again, it starts closed with nothing browsed.
            s.emit(&UiMsg::Click("toggle".to_string()));
            s.flush().unwrap();
            assert!(s.focus("fruit").unwrap());
            press(&mut s, Key::Return);
            press(&mut s, Key::Return);
            assert_eq!(log(&s), "fruit=a");
            // An autocomplete removed while its request is outstanding drops the
            // late answer, and a new one asks again from nothing.
            assert!(s.focus("q").unwrap());
            s.type_text("p").unwrap();
            s.advance(10).unwrap();
            assert!(log(&s).ends_with("asked 1 p"), "{}", log(&s));
            s.emit(&UiMsg::Click("toggle".to_string()));
            s.flush().unwrap();
            crate::perform(crate::app::effects::Effect::Complete(
                "q".to_string(),
                1,
                Widget::items(&[("paris".to_string(), "Paris".to_string())]),
                "".to_string(),
            ));
            s.flush().unwrap();
            assert!(s.popup_owners().is_empty(), "{:?}", s.popup_owners());
            s.emit(&UiMsg::Click("toggle".to_string()));
            s.flush().unwrap();
            assert!(s.focus("q").unwrap());
            press(&mut s, Key::Down);
            press(&mut s, Key::Return);
            assert!(
                !s.semantics(0).dump().contains("Paris"),
                "{}",
                s.semantics(0).dump()
            );
            s.shutdown()
        })
    }

    fn primary_select_all(s: &mut crate::Session) {
        let mut mods = Mods::default();
        if crate::input::platform_is_macos() {
            mods.gui = true
        } else {
            mods.ctrl = true
        };
        s.press(Key::A, mods).unwrap()
    }
}
