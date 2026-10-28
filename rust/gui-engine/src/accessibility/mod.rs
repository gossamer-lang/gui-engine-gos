pub mod atspi;
pub mod bridge;
pub mod introspection;
pub mod live;
pub mod native;
pub mod protocol;
pub mod provider;
#[cfg(target_os = "linux")]
pub mod wire;

// accessibility: the semantic tree. Every window mirrors its widget tree
// into `AccessNode`s (role, name, value, states, actions, bounds,
// relationships) independent of how anything is painted. Platform bridges
// (`accessibility::atspi` on Linux) export this tree; tests assert on its
// text dump. Actions arriving from assistive technology are routed back to
// the widget with the same id on the UI thread.

use crate::geometry::Rect;

/// What a node is, in the vocabulary platform accessibility APIs share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    TopLevel,
    Dialog,
    AlertDialog,
    Alert,
    Group,
    Pane,
    Heading,
    Label,
    Paragraph,
    Link,
    Image,
    Separator,
    Button,
    ToggleButton,
    CheckBox,
    Switch,
    RadioButton,
    RadioGroup,
    TextField,
    PasswordField,
    TextArea,
    SearchField,
    ComboBox,
    ListBox,
    ListItem,
    Menu,
    MenuBar,
    MenuItem,
    MenuItemCheck,
    MenuItemRadio,
    Toolbar,
    StatusBar,
    Slider,
    SpinButton,
    ProgressBar,
    ScrollArea,
    ScrollBar,
    Splitter,
    TabList,
    Tab,
    TabPanel,
    Table,
    Row,
    Cell,
    ColumnHeader,
    Tree,
    TreeItem,
    Grid,
    Tooltip,
    Status,
    Calendar,
    ColorWell,
    Document,
    Terminal,
    Canvas,
    Unknown,
}

impl Role {
    /// A stable lowercase name, used in dumps and bridge role mapping.
    pub fn name(&self) -> &'static str {
        match self {
            Role::TopLevel => "window",
            Role::Dialog => "dialog",
            Role::AlertDialog => "alertdialog",
            Role::Alert => "alert",
            Role::Group => "group",
            Role::Pane => "pane",
            Role::Heading => "heading",
            Role::Label => "label",
            Role::Paragraph => "paragraph",
            Role::Link => "link",
            Role::Image => "image",
            Role::Separator => "separator",
            Role::Button => "button",
            Role::ToggleButton => "togglebutton",
            Role::CheckBox => "checkbox",
            Role::Switch => "switch",
            Role::RadioButton => "radio",
            Role::RadioGroup => "radiogroup",
            Role::TextField => "textfield",
            Role::PasswordField => "passwordfield",
            Role::TextArea => "textarea",
            Role::SearchField => "searchfield",
            Role::ComboBox => "combobox",
            Role::ListBox => "listbox",
            Role::ListItem => "listitem",
            Role::Menu => "menu",
            Role::MenuBar => "menubar",
            Role::MenuItem => "menuitem",
            Role::MenuItemCheck => "menuitemcheckbox",
            Role::MenuItemRadio => "menuitemradio",
            Role::Toolbar => "toolbar",
            Role::StatusBar => "statusbar",
            Role::Slider => "slider",
            Role::SpinButton => "spinbutton",
            Role::ProgressBar => "progressbar",
            Role::ScrollArea => "scrollarea",
            Role::ScrollBar => "scrollbar",
            Role::Splitter => "splitter",
            Role::TabList => "tablist",
            Role::Tab => "tab",
            Role::TabPanel => "tabpanel",
            Role::Table => "table",
            Role::Row => "row",
            Role::Cell => "cell",
            Role::ColumnHeader => "columnheader",
            Role::Tree => "tree",
            Role::TreeItem => "treeitem",
            Role::Grid => "grid",
            Role::Tooltip => "tooltip",
            Role::Status => "status",
            Role::Calendar => "calendar",
            Role::ColorWell => "colorwell",
            Role::Document => "document",
            Role::Terminal => "terminal",
            Role::Canvas => "canvas",
            Role::Unknown => "unknown",
        }
    }
}

/// A cheap immutable source for native document accessibility queries.
#[derive(Debug, Clone)]
pub struct TextSnapshot(pub crate::editing::buffer::Buffer);
impl PartialEq for TextSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_revision(&other.0)
    }
}

/// One node of the semantic tree.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AccessNode {
    /// The widget id this node stands for ("" for structural nodes).
    pub id: String,
    pub role: Role,
    pub name: String,
    pub description: String,
    pub value: String,
    #[serde(skip)]
    pub text_source: Option<TextSnapshot>,
    /// Window pixels.
    pub bounds: Rect,
    pub focusable: bool,
    pub focused: bool,
    pub disabled: bool,
    /// -1 not checkable, 0 unchecked, 1 checked, 2 mixed.
    pub checked: i64,
    pub selected: bool,
    /// -1 not expandable, 0 collapsed, 1 expanded.
    pub expanded: i64,
    pub pressed: bool,
    pub read_only: bool,
    pub required: bool,
    pub invalid: bool,
    pub multiline: bool,
    pub modal: bool,
    /// The window or dialog holds the keyboard focus of the desktop.
    pub active: bool,
    pub level: i64,
    pub pos_in_set: i64,
    pub set_size: i64,
    pub range_min: f64,
    pub range_max: f64,
    pub range_now: f64,
    pub has_range: bool,
    /// Editable text: selection start/end and caret, in bytes.
    pub text_start: i64,
    pub text_end: i64,
    pub caret: i64,
    /// The box of each Unicode scalar of `value` in window pixels, for text
    /// the widget laid out; empty when it did not.
    pub text_boxes: Vec<Rect>,
    pub actions: Vec<String>,
    pub labelled_by: String,
    pub described_by: String,
    pub controls: String,
    /// Live region politeness: "", "polite", or "assertive".
    pub live: String,
    pub children: Vec<AccessNode>,
}

impl AccessNode {
    pub fn new(role: Role, name: &str) -> AccessNode {
        AccessNode {
            id: String::new(),
            role,
            name: name.to_string(),
            description: String::new(),
            value: String::new(),
            text_source: None,
            bounds: Rect::new(0, 0, 0, 0),
            focusable: false,
            focused: false,
            disabled: false,
            checked: -1,
            selected: false,
            expanded: -1,
            pressed: false,
            read_only: false,
            required: false,
            invalid: false,
            multiline: false,
            modal: false,
            active: false,
            level: 0,
            pos_in_set: 0,
            set_size: 0,
            range_min: 0.0,
            range_max: 0.0,
            range_now: 0.0,
            has_range: false,
            text_start: -1,
            text_end: -1,
            caret: -1,
            text_boxes: vec![],
            actions: vec![],
            labelled_by: String::new(),
            described_by: String::new(),
            controls: String::new(),
            live: String::new(),
            children: vec![],
        }
    }

    /// A focusable control node with an id and default action.
    pub fn control(role: Role, id: &str, name: &str, action: &str) -> AccessNode {
        let mut n = AccessNode::new(role, name);
        n.id = id.to_string();
        n.focusable = true;
        if !action.is_empty() {
            n.actions.push(action.to_string())
        }
        n
    }

    /// Sets the range of a slider, spin button, or progress bar.
    pub fn range(&mut self, min: f64, max: f64, now: f64) {
        self.has_range = true;
        self.range_min = min;
        self.range_max = max;
        self.range_now = now;
    }

    /// The node whose id is `id`, depth first.
    pub fn find(&self, id: &str) -> Option<AccessNode> {
        if self.id == id {
            return Some(self.clone());
        }
        for c in &self.children {
            if let Some(found) = c.find(id) {
                return Some(found);
            }
        }
        None
    }

    /// Every node whose role is `role`, depth first.
    pub fn find_role(&self, role: Role) -> Vec<AccessNode> {
        let mut out = vec![];
        if self.role == role {
            out.push(self.clone())
        }
        for c in &self.children {
            for f in c.find_role(role) {
                out.push(f)
            }
        }
        out
    }

    /// The focused node, if any.
    pub fn focused_node(&self) -> Option<AccessNode> {
        if self.focused {
            return Some(self.clone());
        }
        for c in &self.children {
            if let Some(f) = c.focused_node() {
                return Some(f);
            }
        }
        None
    }

    /// Indented text: one line per node with role, name, value, states.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        dump_into(self, 0, &mut out);
        out
    }

    /// Number of nodes in this subtree.
    pub fn count(&self) -> i64 {
        let mut n = 1;
        for c in &self.children {
            n += c.count()
        }
        n
    }
}

fn dump_into(n: &AccessNode, depth: i64, out: &mut String) {
    let mut line = String::new();
    let mut i = 0;
    while i < depth {
        line += "  ";
        i += 1;
    }
    line += n.role.name();
    if !n.name.is_empty() {
        line += &format!(" \"{}\"", n.name)
    };
    if !n.value.is_empty() {
        line += &format!(" = \"{}\"", n.value)
    };
    let mut states: Vec<String> = vec![];
    if n.focused {
        states.push("focused".to_string())
    };
    if n.disabled {
        states.push("disabled".to_string())
    };
    if n.checked == 1 {
        states.push("checked".to_string())
    };
    if n.checked == 2 {
        states.push("mixed".to_string())
    };
    if n.selected {
        states.push("selected".to_string())
    };
    if n.expanded == 1 {
        states.push("expanded".to_string())
    };
    if n.expanded == 0 {
        states.push("collapsed".to_string())
    };
    if n.pressed {
        states.push("pressed".to_string())
    };
    if n.read_only {
        states.push("readonly".to_string())
    };
    if n.invalid {
        states.push("invalid".to_string())
    };
    if n.required {
        states.push("required".to_string())
    };
    if n.modal {
        states.push("modal".to_string())
    };
    if n.has_range {
        states.push(format!(
            "range={}/{}..{}",
            n.range_now, n.range_min, n.range_max
        ))
    };
    if n.set_size > 0 {
        states.push(format!("{}/{}", n.pos_in_set, n.set_size))
    };
    if n.level > 0 {
        states.push(format!("level={}", n.level))
    };
    if !states.is_empty() {
        line += &format!(" [{}]", states.join(" "))
    };
    out.push_str(&line);
    out.push('\n');
    for c in &n.children {
        dump_into(c, depth + 1, out)
    }
}

#[cfg(test)]
mod tests {
    use super::AccessNode;

    #[test]
    fn dump_lists_roles_names_and_states() {
        let mut root = AccessNode::new(super::Role::TopLevel, "Demo");
        let mut b = AccessNode::control(super::Role::CheckBox, "c", "Agree", "toggle");
        b.checked = 1;
        b.focused = true;
        root.children.push(b);
        let text = root.dump();
        assert_eq!(
            text,
            "window \"Demo\"\n  checkbox \"Agree\" [focused checked]\n"
        );
        assert_eq!(root.find("c").unwrap().name, "Agree");
        assert_eq!(root.focused_node().unwrap().id, "c");
        assert_eq!(root.count(), 2)
    }
}
