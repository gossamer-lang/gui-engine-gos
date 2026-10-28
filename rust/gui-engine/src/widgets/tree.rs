// tree: walks over a widget tree and its layout. Dispatch, focus order,
// hit testing, disposal marking, and the accessibility mirror all use
// these, so each agrees on what is visible, enabled, and where.

use crate::accessibility::{AccessNode, Role};
use crate::geometry::Rect;
use crate::style::Theme;
use crate::widgets::cx::Node;
use crate::widgets::state::StateStore;
use crate::widgets::{Visibility, Widget};

/// One widget on a path from the root, with its absolute origin, the
/// theme in effect, and whether an ancestor disables it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PathEntry {
    pub widget: Widget,
    pub node: Node,
    pub x: i64,
    pub y: i64,
    pub disabled: bool,
    pub theme: Theme,
}

impl PathEntry {
    /// The node's rectangle in window pixels.
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.node.rect.width, self.node.rect.height)
    }
}

fn hidden(w: &Widget) -> bool {
    match w {
        Widget::Decorated(d) => d.mods.visibility != Visibility::Visible,
        _ => false,
    }
}

fn disables(w: &Widget) -> bool {
    match w {
        Widget::Decorated(d) => d.mods.disabled,
        _ => false,
    }
}

fn theme_after(w: &Widget, theme: Theme) -> Theme {
    match w {
        Widget::Decorated(d) => match d.mods.theme {
            Some(ref change) => change.apply(theme),
            None => theme,
        },
        _ => theme,
    }
}

/// The path to the deepest widget under (`x`, `y`), root first. Later
/// siblings paint on top, so they are tried first; a widget's own
/// rectangle bounds its children.
pub fn hit_path(root: &Widget, node: &Node, theme: &Theme, x: i64, y: i64) -> Vec<PathEntry> {
    let mut out = vec![];
    hit_into(root, node, 0, 0, false, theme, x, y, &mut out);
    out
}

fn hit_into(
    w: &Widget,
    node: &Node,
    px: i64,
    py: i64,
    disabled: bool,
    theme: &Theme,
    x: i64,
    y: i64,
    out: &mut Vec<PathEntry>,
) -> bool {
    let ax = px + node.rect.x;
    let ay = py + node.rect.y;
    let r = Rect::new(ax, ay, node.rect.width, node.rect.height);
    if !r.contains(x, y) || hidden(w) {
        return false;
    }
    let dis = disabled || disables(w);
    let th = theme_after(w, theme.clone());
    out.push(PathEntry {
        widget: w.clone(),
        node: node.clone(),
        x: ax,
        y: ay,
        disabled: dis,
        theme: th.clone(),
    });
    let kids = w.children();
    let mut i = kids.len().min(node.children.len());
    while i > 0 {
        i -= 1;
        if hit_into(kids[i], &node.children[i], ax, ay, dis, &th, x, y, out) {
            return true;
        }
    }
    if let Widget::Decorated(d) = w
        && d.mods.pass_through
    {
        let _ = out.pop();
        return false;
    }
    true
}

/// The path to the widget whose id is `id`, root first (empty if absent).
pub fn path_to(root: &Widget, node: &Node, theme: &Theme, id: &str) -> Vec<PathEntry> {
    let mut out = vec![];
    if id.is_empty() {
        return out;
    }
    let _ = find_into(root, node, 0, 0, false, theme, id, &mut out);
    out
}

fn find_into(
    w: &Widget,
    node: &Node,
    px: i64,
    py: i64,
    disabled: bool,
    theme: &Theme,
    id: &str,
    out: &mut Vec<PathEntry>,
) -> bool {
    let ax = px + node.rect.x;
    let ay = py + node.rect.y;
    let dis = disabled || disables(w);
    let th = theme_after(w, theme.clone());
    out.push(PathEntry {
        widget: w.clone(),
        node: node.clone(),
        x: ax,
        y: ay,
        disabled: dis,
        theme: th.clone(),
    });
    if w.id() == id {
        return true;
    }
    let kids = w.children();
    let mut i = 0;
    while i < kids.len() && i < node.children.len() {
        if find_into(kids[i], &node.children[i], ax, ay, dis, &th, id, out) {
            return true;
        }
        i += 1;
    }
    out.pop();
    false
}

/// Focusable widget ids in tree order, skipping hidden, collapsed, and
/// disabled subtrees. With `within` non-empty only that subtree counts.
pub fn focus_chain(root: &Widget, node: &Node, within: &str) -> Vec<String> {
    let mut out = vec![];
    chain_into(root, node, within.is_empty(), within, false, &mut out);
    out
}

fn chain_into(
    w: &Widget,
    node: &Node,
    inside: bool,
    within: &str,
    disabled: bool,
    out: &mut Vec<String>,
) {
    if hidden(w) {
        return;
    }
    let dis = disabled || disables(w);
    let now_inside = inside || (!within.is_empty() && w.id() == within);
    if now_inside
        && !dis
        && w.focusable()
        && !w.id().is_empty()
        && node.rect.width > 0
        && node.rect.height > 0
    {
        out.push(w.id())
    }
    let kids = w.children();
    let mut i = 0;
    while i < kids.len() && i < node.children.len() {
        chain_into(kids[i], &node.children[i], now_inside, within, dis, out);
        i += 1;
    }
}

/// The key of `w` when it is a keyed toolbar.
pub fn toolbar_key(w: &Widget) -> Option<String> {
    match w {
        Widget::Decorated(d) => {
            if d.mods.role == Some(Role::Toolbar) && !d.mods.key.is_empty() {
                Some(d.mods.key.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The focus chain with each entry's toolbar: the key of the nearest
/// enclosing toolbar, or "" outside any.
pub fn focus_groups(root: &Widget, node: &Node) -> Vec<(String, String)> {
    let mut out = vec![];
    groups_into(root, node, "", false, &mut out);
    out
}

fn groups_into(
    w: &Widget,
    node: &Node,
    group: &str,
    disabled: bool,
    out: &mut Vec<(String, String)>,
) {
    if hidden(w) {
        return;
    }
    let dis = disabled || disables(w);
    let inner = toolbar_key(w).unwrap_or(group.to_string());
    if !dis && w.focusable() && !w.id().is_empty() && node.rect.width > 0 && node.rect.height > 0 {
        out.push((w.id(), inner.clone()))
    }
    let kids = w.children();
    let mut i = 0;
    while i < kids.len() && i < node.children.len() {
        groups_into(kids[i], &node.children[i], &inner, dis, out);
        i += 1;
    }
}

/// Marks every id in the tree as alive this frame.
pub fn mark_ids(root: &Widget, st: &mut StateStore) {
    let id = root.id();
    if !id.is_empty() {
        st.mark(&id);
        // Satellite state (completion lists) lives and dies with its owner.
        st.mark(&format!("{}#completion", id));
        st.mark(&format!("{}#edit", id))
    }
    if let Widget::TextInput(input) = root {
        let mut edit = st.edit(&input.id);
        if input.document == 0 {
            edit.sync(&input.text);
        } else if let Ok(current) = crate::editing::documents::state(input.document) {
            edit = current;
        }
        let old_scroll = (edit.scroll_x, edit.scroll_y);
        crate::text::document_layout::adjust_state(&mut edit);
        if input.document > 0 && old_scroll != (edit.scroll_x, edit.scroll_y) {
            let _ = crate::editing::documents::commit(input.document, edit.revision, edit.clone());
        }
        st.set(&input.id, crate::widgets::state::WState::Edit(edit));
    }
    if let Widget::Tabs(t) = root {
        for page in t.pages.values() {
            mark_ids(page, st)
        }
    }
    for c in root.children() {
        mark_ids(c, st)
    }
}

/// Reconciles collection revisions and open choice models before layout.
pub fn sync_models(root: &Widget, st: &mut StateStore, theme: &Theme) {
    if let Widget::Collection(c) = root {
        let before = st.collection(&c.id);
        if before.revision != c.revision
            || (c.height_of.is_some() && before.extents_scale != theme.scale)
        {
            st.set(
                &c.id,
                crate::widgets::state::WState::Collection(crate::widgets::collections::sync(
                    c, before, theme,
                )),
            )
        }
    }
    if let Widget::Choice(ch) = root {
        crate::widgets::choices::sync_model(ch, st)
    };
    for c in root.children() {
        sync_models(c, st, theme)
    }
}

/// Every widget id in the tree, in order (for duplicate detection).
pub fn all_ids(root: &Widget) -> Vec<String> {
    let mut out = vec![];
    ids_into(root, &mut out);
    out
}

fn ids_into(w: &Widget, out: &mut Vec<String>) {
    let id = w.id();
    if !id.is_empty() {
        out.push(id)
    };
    for c in w.children() {
        ids_into(c, out)
    }
}

/// The accessibility tree of a window: the widget semantics, positioned
/// and nested, with focus and disabled states applied.
pub fn semantics(
    root: &Widget,
    node: &Node,
    st: &StateStore,
    theme: &Theme,
    focus: &str,
    title: &str,
) -> AccessNode {
    let mut win = AccessNode::new(Role::TopLevel, title);
    win.bounds = Rect::new(0, 0, node.rect.width, node.rect.height);
    let kids = sem_into(root, node, 0, 0, false, theme, st, focus);
    for k in kids {
        win.children.push(k)
    }
    win
}

fn sem_into(
    w: &Widget,
    node: &Node,
    px: i64,
    py: i64,
    disabled: bool,
    theme: &Theme,
    st: &StateStore,
    focus: &str,
) -> Vec<AccessNode> {
    if hidden(w) {
        return vec![];
    }
    if let Widget::Decorated(d) = w
        && d.mods.decorative
    {
        return vec![];
    }
    let ax = px + node.rect.x;
    let ay = py + node.rect.y;
    let dis = disabled || disables(w);
    let th = theme_after(w, theme.clone());
    let mut children = vec![];
    let kids = w.children();
    let mut i = 0;
    while i < kids.len() && i < node.children.len() {
        for c in sem_into(kids[i], &node.children[i], ax, ay, dis, &th, st, focus) {
            children.push(c)
        }
        i += 1;
    }
    // Modifiers without a role describe the child they wrap.
    if let Widget::Decorated(d) = w
        && d.mods.role.is_none()
    {
        if children.len() == 1 {
            if !d.mods.label.is_empty() {
                children[0].name = d.mods.label.clone()
            };
            if !d.mods.description.is_empty() {
                children[0].description = d.mods.description.clone()
            } else if !d.mods.tooltip.is_empty() && children[0].description.is_empty() {
                children[0].description = d.mods.tooltip.clone()
            }
        }
        return children;
    }
    match w.semantics(node, st, &th) {
        Some(mut n) => {
            if n.bounds.width == 0 && n.bounds.height == 0 {
                n.bounds = Rect::new(ax, ay, node.rect.width, node.rect.height);
            } else {
                n.bounds = n.bounds.offset(ax, ay);
            }
            let mut k = 0;
            while k < n.children.len() {
                if n.children[k].bounds.width > 0 || n.children[k].bounds.height > 0 {
                    n.children[k].bounds = n.children[k].bounds.offset(ax, ay);
                }
                k += 1;
            }
            if dis {
                n.disabled = true;
                n.focusable = false
            };
            if !n.id.is_empty() && n.id == focus {
                n.focused = true
            };
            for c in children {
                n.children.push(c)
            }
            vec![n]
        }
        None => children,
    }
}

/// Every visible widget of `kind` ("menubar") in tree order, with paths
/// reduced to the widget itself.
pub fn find_kind(root: &Widget, node: &Node, theme: &Theme, kind: &str) -> Vec<PathEntry> {
    let mut out = vec![];
    kind_into(root, node, 0, 0, false, theme, kind, &mut out);
    out
}

fn kind_into(
    w: &Widget,
    node: &Node,
    px: i64,
    py: i64,
    disabled: bool,
    theme: &Theme,
    kind: &str,
    out: &mut Vec<PathEntry>,
) {
    if hidden(w) {
        return;
    }
    let ax = px + node.rect.x;
    let ay = py + node.rect.y;
    let dis = disabled || disables(w);
    let th = theme_after(w, theme.clone());
    let matches_kind = match w {
        Widget::MenuBar(_) => kind == "menubar",
        Widget::Tabs(_) => kind == "tabs",
        _ => false,
    };
    if matches_kind && !dis {
        out.push(PathEntry {
            widget: w.clone(),
            node: node.clone(),
            x: ax,
            y: ay,
            disabled: dis,
            theme: th.clone(),
        })
    }
    let kids = w.children();
    let mut i = 0;
    while i < kids.len() && i < node.children.len() {
        kind_into(kids[i], &node.children[i], ax, ay, dis, &th, kind, out);
        i += 1;
    }
}

/// Stateful pages participating in cross-window migration, including inactive pages.
pub fn workspace_pages(root: &Widget) -> Vec<((String, String), Widget)> {
    let mut out = vec![];
    if let Widget::Tabs(t) = root {
        for (key, page) in &t.pages {
            if !t.workspace.is_empty() {
                out.push(((t.workspace.clone(), key.clone()), page.clone()))
            };
            for entry in workspace_pages(page) {
                out.push(entry)
            }
        }
        if !t.pages.is_empty() {
            return out;
        }
    }
    for child in root.children() {
        for entry in workspace_pages(child) {
            out.push(entry)
        }
    }
    out
}

/// Ids whose state belongs to this subtree, including satellites and inactive pages.
pub fn state_ids(root: &Widget) -> Vec<String> {
    let mut out = vec![];
    let id = root.id();
    if !id.is_empty() {
        out.push(id.clone());
        out.push(format!("{}#completion", id));
        out.push(format!("{}#edit", id))
    };
    if let Widget::Tabs(t) = root {
        for page in t.pages.values() {
            for key in state_ids(page) {
                if !out.contains(&key) {
                    out.push(key)
                }
            }
        }
    }
    for child in root.children() {
        for key in state_ids(child) {
            if !out.contains(&key) {
                out.push(key)
            }
        }
    }
    out
}

#[cfg(test)]
mod lifetime_tests {
    use std::collections::HashMap;

    use super::mark_ids;
    use crate::widgets::Widget;
    use crate::widgets::state::StateStore;
    use crate::widgets::tabs::TabItem;

    #[test]
    fn an_inactive_tab_keeps_its_state_and_a_closed_one_disposes_it() {
        let mut pages: HashMap<String, Widget> = HashMap::new();
        pages.insert(
            "first".to_string(),
            Widget::text_area("editor-first", "one", 4),
        );
        pages.insert(
            "second".to_string(),
            Widget::text_area("editor-second", "two", 4),
        );
        let items = [
            TabItem::new("first", "First"),
            TabItem::new("second", "Second"),
        ];
        let mut states = StateStore::new();
        states.begin_frame();
        mark_ids(
            &Widget::tab_pages("tabs", "workspace", &items, "first", &pages),
            &mut states,
        );
        states.end_frame();
        let inactive = states.widget_id("editor-second");
        states.set_field("editor-second", "history", "retained");
        states.begin_frame();
        mark_ids(
            &Widget::tab_pages("tabs", "workspace", &items, "second", &pages),
            &mut states,
        );
        assert!(states.end_frame().is_empty());
        assert!(states.is_live(&inactive));
        assert_eq!(states.field("editor-second", "history", ""), "retained");
        pages.remove("second");
        states.begin_frame();
        mark_ids(
            &Widget::tab_pages("tabs", "workspace", &items[..1], "first", &pages),
            &mut states,
        );
        let disposed = states.end_frame();
        assert!(disposed.contains(&"editor-second".to_string()));
        assert!(!states.is_live(&inactive));
        assert_eq!(states.field("editor-second", "history", ""), "");
    }
}
