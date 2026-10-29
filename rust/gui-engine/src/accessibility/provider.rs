// Platform-neutral published objects. Stable widget identities keep their native
// object paths across updates; removed identities are released and never reused.
use crate::accessibility::{AccessNode, Role};
use crate::geometry::Rect;
use crate::strext::StrExt;
use std::collections::{HashMap, HashSet};

pub const ROOT: &str = "/org/a11y/atspi/accessible/root";
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NativeAction {
    pub window: String,
    pub owner: String,
    pub target: String,
    pub kind: String,
    pub value: String,
    pub start: i64,
    pub end: i64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Published {
    pub object: String,
    pub identity: String,
    pub window: String,
    pub owner: String,
    pub node: AccessNode,
    pub parent: String,
    pub children: Vec<String>,
    pub index: i64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub app_id: String,
    pub objects: HashMap<String, Published>,
    pub identities: HashMap<String, String>,
    pub windows: Vec<String>,
    pub roots: HashMap<String, String>,
    pub origins: HashMap<String, (i64, i64)>,
    pub next: i64,
}
impl Snapshot {
    pub fn new(app_id: &str) -> Snapshot {
        let mut app = AccessNode::new(Role::TopLevel, app_id);
        app.description = crate::localization::tr("Gossamer desktop application");
        let root = Published {
            object: ROOT.to_string(),
            identity: "application".to_string(),
            window: "".to_string(),
            owner: "".to_string(),
            node: app,
            parent: "".to_string(),
            children: vec![],
            index: -1,
        };
        Snapshot {
            app_id: app_id.to_string(),
            objects: crate::hmap! { ROOT => root },
            identities: HashMap::new(),
            windows: vec![],
            roots: HashMap::new(),
            origins: HashMap::new(),
            next: 1,
        }
    }
    pub fn publish(&mut self, window: &str, tree: &AccessNode) -> Vec<String> {
        if !self.windows.iter().any(|w| w == window) {
            self.windows.push(window.to_string())
        };
        let mut seen = vec![];
        let mut claimed = HashSet::new();
        let object = self.walk(
            window,
            tree,
            ROOT,
            "0",
            "",
            (self
                .windows
                .iter()
                .position(|key| key == window)
                .unwrap_or(0)) as i64,
            &mut seen,
            &mut claimed,
        );
        self.roots.insert(window.to_string(), object);
        let mut removed = vec![];
        for (path, old) in &self.objects {
            if old.window == window && !seen.contains(path) {
                removed.push(path.clone())
            }
        }
        for path in removed {
            if let Some(old) = self.objects.remove(&path) {
                let _ = self.identities.remove(&old.identity);
            }
        }
        self.refresh_root();
        seen
    }
    fn walk(
        &mut self,
        window: &str,
        input: &AccessNode,
        parent: &str,
        position: &str,
        owner: &str,
        index: i64,
        seen: &mut Vec<String>,
        claimed: &mut HashSet<String>,
    ) -> String {
        // A widget id names one object per window. A node repeating an id
        // already claimed in this tree is identified by its position, so
        // every node is its own object and the published graph stays a tree.
        let by_id = format!("{}:{}:id:{}", window.byte_len(), window, input.id);
        let identity = if !input.id.is_empty() && claimed.insert(by_id.clone()) {
            by_id
        } else {
            format!("{}:{}:path:{}", window.byte_len(), window, position)
        };
        let path = match self.identities.get(&identity) {
            Some(path) => path.clone(),
            None => {
                let path = format!("/org/a11y/atspi/accessible/n{}", self.next);
                self.next += 1;
                self.identities.insert(identity.to_string(), path.clone());
                path
            }
        };
        let target = if input.focusable && !input.id.is_empty() {
            input.id.clone()
        } else {
            owner.to_string()
        };
        let mut child_paths = vec![];
        for (i, child) in input.children.iter().enumerate() {
            child_paths.push(self.walk(
                window,
                child,
                &path,
                &format!("{}/{}", position, i),
                &target,
                i as i64,
                seen,
                claimed,
            ));
        }
        let mut node = input.clone();
        node.children = vec![];
        self.objects.insert(
            path.to_string(),
            Published {
                object: path.to_string(),
                identity,
                window: window.to_string(),
                owner: target.to_string(),
                node: node.clone(),
                parent: parent.to_string(),
                children: child_paths,
                index,
            },
        );
        seen.push(path.to_string());
        path.to_string()
    }
    fn refresh_root(&mut self) {
        let children = self
            .windows
            .iter()
            .filter_map(|key| self.roots.get(key).cloned())
            .collect();
        // The root object is published when the snapshot is made.
        if let Some(app) = self.objects.get_mut(ROOT) {
            app.children = children;
        }
    }
    pub fn close(&mut self, window: &str) {
        let paths = self
            .objects
            .iter()
            .filter(|entry| entry.1.window == window)
            .map(|entry| entry.0.clone())
            .collect::<Vec<String>>();
        for path in paths {
            if let Some(old) = self.objects.remove(&path) {
                let _ = self.identities.remove(&old.identity);
            }
        }
        self.windows = self
            .windows
            .iter()
            .filter(|key| *key != window)
            .cloned()
            .collect::<Vec<_>>();
        self.roots.remove(window);
        self.origins.remove(window);
        self.refresh_root()
    }
    pub fn related(&self, window: &str, id: &str) -> Option<String> {
        self.identities
            .get(&format!("{}:{}:id:{}", window.byte_len(), window, id))
            .cloned()
    }
    pub fn extents(&self, item: &Published, coordinate: i64) -> Rect {
        self.place(item, item.node.bounds, coordinate)
    }
    /// `r`, a rectangle in `item`'s window, in AT-SPI coordinate space
    /// `coordinate`: 0 the screen, 1 the window, 2 the parent's bounds.
    pub fn place(&self, item: &Published, r: Rect, coordinate: i64) -> Rect {
        if coordinate == 0 {
            let (x, y) = self.origins.get(&item.window).unwrap_or(&(0, 0));
            r.offset(*x, *y)
        } else if coordinate == 2 {
            if let Some(parent) = self.objects.get(&item.parent) {
                r.offset(-parent.node.bounds.x, -parent.node.bounds.y)
            } else {
                r
            }
        } else {
            r
        }
    }
    pub fn request(
        &self,
        object: &str,
        kind: &str,
        value: &str,
        start: i64,
        end: i64,
    ) -> Option<NativeAction> {
        let item = self.objects.get(object)?;
        if item.node.disabled || item.owner.is_empty() {
            return None;
        };
        if (kind == "set-text" || kind == "replace-text")
            && (item.node.read_only
                || (item.node.role != Role::TextField
                    && item.node.role != Role::TextArea
                    && item.node.role != Role::SearchField
                    && item.node.role != Role::PasswordField))
        {
            return None;
        };
        if (kind == "select-text" || kind == "replace-text")
            && (item.node.text_start < 0
                || start < 0
                || end < start
                || end > text_length(&item.node))
        {
            return None;
        };
        if kind == "set-value" && (!item.node.has_range || item.node.read_only) {
            return None;
        };
        Some(NativeAction {
            window: item.window.clone(),
            owner: item.owner.clone(),
            target: item.node.id.clone(),
            kind: kind.to_string(),
            value: value.to_string(),
            start,
            end,
        })
    }
}
/// The box of scalars `start` up to `end` of laid-out text: the union of
/// their boxes, or `None` when the range holds none of them.
pub fn range_box(boxes: &[Rect], start: i64, end: i64) -> Option<Rect> {
    let lo = start.max(0);
    let hi = end.min(boxes.len() as i64);
    if lo >= hi {
        return None;
    };
    let mut r = boxes[lo as usize];
    for i in (lo + 1)..hi {
        r = r.union(boxes[i as usize])
    }
    Some(r)
}
/// The scalar whose box holds (`x`, `y`); else the one nearest it on the
/// row through `y`; else -1.
pub fn offset_at(boxes: &[Rect], x: i64, y: i64) -> i64 {
    let mut best: i64 = -1;
    let mut distance = i64::MAX;
    for (i, &b) in boxes.iter().enumerate() {
        if b.contains(x, y) {
            return i as i64;
        };
        if y >= b.y && y < b.bottom() {
            let d = if x < b.x { b.x - x } else { x - b.right() + 1 };
            if d < distance {
                distance = d;
                best = i as i64
            }
        }
    }
    best
}
/// AT-SPI text offsets count Unicode scalar values; edit state uses UTF-8 bytes.
pub fn byte_offset(text: &str, offset: i64) -> i64 {
    if offset < 0 {
        return text.byte_len();
    };
    let mut byte = 0;
    for (count, ch) in text.chars().enumerate() {
        if count as i64 >= offset {
            break;
        };
        byte += ch.len_utf8()
    }
    byte as i64
}
pub fn character_offset(text: &str, byte: i64) -> i64 {
    let mut count = 0;
    let mut at = 0;
    for ch in text.chars() {
        if at >= byte {
            break;
        };
        at += ch.len_utf8() as i64;
        count += 1
    }
    count
}
pub fn text_range(text: &str, start: i64, end: i64) -> String {
    let a = byte_offset(text, start.max(0));
    let b = byte_offset(text, end);
    if b < a {
        "".to_string()
    } else {
        text.to_string().substring(a, b).to_string()
    }
}
pub fn node_text(node: &AccessNode) -> String {
    node.text_source
        .as_ref()
        .map_or_else(|| node.value.clone(), |source| source.0.to_string())
}
pub fn node_character_offset(node: &AccessNode, byte: i64) -> i64 {
    node.text_source.as_ref().map_or_else(
        || character_offset(&node.value, byte),
        |source| source.0.scalar_of(byte.max(0) as usize) as i64,
    )
}
pub fn node_text_range(node: &AccessNode, start: i64, end: i64) -> String {
    if let Some(source) = &node.text_source {
        let start = source.0.byte_of_scalar(start.max(0) as usize);
        let end = if end < 0 {
            source.0.len()
        } else {
            source.0.byte_of_scalar(end as usize)
        };
        source.0.slice(start, end.max(start))
    } else {
        text_range(&node.value, start, end)
    }
}
/// Baseline glyph extent for platform text queries. Actual laid-out text bounds
/// are supplied through AccessNode; no hidden text is exported for passwords.
pub fn text_length(node: &AccessNode) -> i64 {
    if node.role == Role::PasswordField {
        0
    } else {
        node.text_source.as_ref().map_or_else(
            || character_offset(&node.value, node.value.byte_len()),
            |source| source.0.scalar_of(source.0.len()) as i64,
        )
    }
}
pub fn state_words(n: &AccessNode) -> (i64, i64) {
    let mut bits = (1i64 << 25) | (1i64 << 30);
    if !n.disabled {
        bits |= (1i64 << 8) | (1i64 << 24)
    };
    if n.active {
        bits |= 1i64 << 1
    };
    if n.focusable {
        bits |= 1i64 << 11
    };
    if n.focused {
        bits |= 1i64 << 12
    };
    if n.checked == 1 {
        bits |= 1i64 << 4
    };
    if n.checked == 2 {
        bits |= 1i64 << 32
    };
    if n.expanded >= 0 {
        bits |= 1i64 << 9;
        bits |= 1i64 << if n.expanded == 1 { 10 } else { 5 }
    };
    if n.selected {
        bits |= 1i64 << 23
    };
    if n.modal {
        bits |= 1i64 << 16
    };
    if n.multiline {
        bits |= 1i64 << 17
    } else if n.text_start >= 0 {
        bits |= 1i64 << 26
    };
    if n.text_start >= 0 && !n.read_only {
        bits |= 1i64 << 7
    };
    if n.pressed {
        bits |= 1i64 << 20
    };
    if n.required {
        bits |= 1i64 << 33
    };
    if n.invalid {
        bits |= 1i64 << 36
    };
    (bits & 0xffffffff, (bits >> 32) & 0xffffffff)
}
pub fn role(n: Role) -> i64 {
    match n {
        Role::TopLevel => 23,
        Role::Dialog => 16,
        Role::AlertDialog | Role::Alert => 2,
        Role::Group | Role::RadioGroup => 99,
        Role::Pane | Role::TabPanel => 39,
        Role::Heading => 83,
        Role::Label => 29,
        Role::Paragraph => 73,
        Role::Link => 88,
        Role::Image => 27,
        Role::Separator => 50,
        Role::Button => 43,
        Role::ToggleButton => 62,
        Role::CheckBox | Role::Switch => 7,
        Role::RadioButton => 44,
        Role::TextField | Role::SearchField => 79,
        Role::PasswordField => 40,
        Role::TextArea => 61,
        Role::ComboBox => 11,
        Role::ListBox => 98,
        Role::ListItem => 32,
        Role::Menu => 33,
        Role::MenuBar => 34,
        Role::MenuItem => 35,
        Role::MenuItemCheck => 8,
        Role::MenuItemRadio => 45,
        Role::Toolbar => 63,
        Role::StatusBar | Role::Status => 54,
        Role::Slider => 51,
        Role::SpinButton => 52,
        Role::ProgressBar => 42,
        Role::ScrollArea => 49,
        Role::ScrollBar => 48,
        Role::Splitter => 53,
        Role::TabList => 38,
        Role::Tab => 37,
        Role::Table | Role::Grid => 55,
        Role::Row => 90,
        Role::Cell => 56,
        Role::ColumnHeader => 57,
        Role::Tree => 65,
        Role::TreeItem => 91,
        Role::Tooltip => 64,
        Role::Calendar => 5,
        Role::ColorWell => 9,
        Role::Document => 94,
        Role::Terminal => 60,
        Role::Canvas => 6,
        Role::Unknown => 67,
    }
}

/// Character, word and paragraph ranges use the editor's UTF-8 boundaries.
/// Visual line/sentence boundaries require layout/locale data and are not guessed.
/// The end of the text answers an empty character and the last word or
/// paragraph, where a caret after the last character reads from.
pub fn text_segment(text: &str, offset: i64, granularity: i64) -> Option<(String, i64, i64)> {
    let length = character_offset(text, text.byte_len());
    if offset < 0 || offset > length {
        return None;
    };
    if granularity == 0 {
        if offset == length {
            return Some(("".to_string(), length, length));
        };
        return Some((text_range(text, offset, offset + 1), offset, offset + 1));
    }
    if length == 0 {
        return if granularity == 1 || granularity == 4 {
            Some(("".to_string(), 0, 0))
        } else {
            None
        };
    };
    let offset = if offset == length { length - 1 } else { offset };
    let mut boundaries = vec![0];
    if granularity == 1 {
        boundaries = crate::editing::word_boundaries(text)
    } else if granularity == 4 {
        let mut at = 0;
        for ch in text.chars() {
            at += ch.len_utf8();
            if ch == '\n' {
                boundaries.push(at as i64)
            }
        }
        if boundaries[boundaries.len() - 1] != text.byte_len() {
            boundaries.push(text.byte_len())
        }
    } else {
        return None;
    }
    let byte = byte_offset(text, offset);
    for i in 0..(boundaries.len() - 1) {
        if byte >= boundaries[i] && byte < boundaries[i + 1] {
            return Some((
                text.substring(boundaries[i], boundaries[i + 1]).to_string(),
                character_offset(text, boundaries[i]),
                character_offset(text, boundaries[i + 1]),
            ));
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::{ROOT, Snapshot};
    use crate::accessibility::{AccessNode, Role};
    use crate::geometry::Rect;

    use crate::style::Theme;
    use crate::widgets::Widget;
    use crate::widgets::cx::{Event, EventCx, Node, UiMsg};
    use crate::widgets::state::StateStore;
    #[test]
    fn native_actions_share_widget_selection_text_and_range_contracts() {
        let mut st = StateStore::new();
        let mut cx = EventCx::new(&Theme::default(), "", 0, Rect::new(0, 0, 800, 600));
        let radio = Widget::radio_group(
            "r",
            &[
                ("a".to_string(), "A".to_string()),
                ("b".to_string(), "B".to_string()),
            ],
            "a",
            true,
        );
        radio.event(
            &Node::default(),
            &Event::Accessible {
                target: "r#b".to_string(),
                action: "select".to_string(),
                value: "".to_string(),
                start: 0,
                end: 0,
            },
            &mut st,
            &mut cx,
        );
        assert_eq!(
            cx.messages,
            vec![UiMsg::Choose("r".to_string(), "b".to_string())]
        );
        cx.messages = vec![];
        let slider = Widget::slider("s", 5.0, 0.0, 10.0, 1.0);
        slider.event(
            &Node::default(),
            &Event::Accessible {
                target: "s".to_string(),
                action: "set-value".to_string(),
                value: "99".to_string(),
                start: 0,
                end: 0,
            },
            &mut st,
            &mut cx,
        );
        assert!(cx.messages.is_empty());
        slider.event(
            &Node::default(),
            &Event::Accessible {
                target: "s".to_string(),
                action: "set-value".to_string(),
                value: "7".to_string(),
                start: 0,
                end: 0,
            },
            &mut st,
            &mut cx,
        );
        assert_eq!(cx.messages, vec![UiMsg::Value("s".to_string(), 7.0)]);
        assert_eq!(
            super::text_segment("aé🙂 word", 5, 1),
            Some(("word".to_string(), 4, 8))
        );
        assert_eq!(
            super::text_segment("aé🙂", 2, 0),
            Some(("🙂".to_string(), 2, 3))
        );
        assert_eq!(
            super::text_segment("aé🙂", 3, 0),
            Some(("".to_string(), 3, 3))
        );
        assert_eq!(
            super::text_segment("one two", 7, 1),
            Some(("two".to_string(), 4, 7))
        );
        assert_eq!(
            super::text_segment("one\ntwo", 7, 4),
            Some(("two".to_string(), 4, 7))
        );
        assert_eq!(super::text_segment("", 0, 4), Some(("".to_string(), 0, 0)));
        assert_eq!(super::text_segment("abc", 4, 0), None)
    }
    #[test]
    fn a_repeated_widget_id_publishes_distinct_objects_in_a_tree() {
        let mut p = Snapshot::new("app");
        let mut inner = AccessNode::control(Role::Button, "x", "Inner", "press");
        inner.children = vec![AccessNode::new(Role::Label, "Caption")];
        let mut outer = AccessNode::control(Role::Group, "x", "Outer", "");
        outer.children = vec![inner];
        let mut window = AccessNode::new(Role::TopLevel, "Main");
        window.children = vec![outer];
        p.publish("main", &window);
        let mut visited = std::collections::HashSet::new();
        let mut pending = vec![p.roots["main"].clone()];
        while let Some(path) = pending.pop() {
            assert!(visited.insert(path.clone()), "{path} is reachable twice");
            pending.extend(p.objects[&path].children.iter().cloned());
        }
        assert_eq!(visited.len(), 4);
        assert_eq!(
            p.objects[&p.related("main", "x").unwrap()].node.name,
            "Outer"
        );
    }
    #[test]
    fn provider_identity_disposal_virtual_actions_and_unicode_offsets() {
        let mut p = Snapshot::new("app");
        let mut list = AccessNode::control(Role::ListBox, "list", "Items", "select");
        let mut row = AccessNode::new(Role::ListItem, "One");
        row.id = "list#one".to_string();
        row.actions = vec!["press".to_string()];
        list.children = vec![row];
        p.publish("main", &list);
        let a = p.related("main", "list#one").unwrap();
        assert_eq!(p.request(&a, "press", "", 0, 0).unwrap().owner, "list");
        p.publish("main", &list);
        assert_eq!(p.related("main", "list#one"), Some(a));
        p.close("main");
        assert_eq!(p.objects.len(), 1);
        assert_eq!(p.identities.len(), 0);
        assert!(p.objects.contains_key(ROOT));
        assert_eq!(super::text_range("aé🙂z", 1, 3), "é🙂");
        assert_eq!(super::byte_offset("aé🙂z", 3), 7);
        assert_eq!(super::character_offset("aé🙂z", 7), 3);
        let mut frame = AccessNode::new(Role::TopLevel, "Main");
        assert_eq!(super::state_words(&frame).0 & 2, 0);
        frame.active = true;
        assert_eq!(super::state_words(&frame).0 & 2, 2)
    }
}
