// inspector: a live panel beside the application showing the window's
// state (focus, hover, capture, popups, dialogs, why it was last laid out),
// its theme and tokens, the widget hierarchy with geometry, the focus chain,
// the commands and their scopes, the accessibility tree, cache counters and
// the recent input and message log. Toggled with the `gui.inspector` command
// (Primary+Shift+I); it uses the same widgets and runtime contracts as any
// application.

use crate::accessibility::AccessNode;
use crate::commands::Command;
use crate::style::{TextRole, Theme};
use crate::text::layout;
use crate::widgets::cx::Node;
use crate::widgets::state::StateStore;
use crate::widgets::{Axis, Tone, Visibility, Widget};

/// What the inspector shows about one window.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub states: StateStore,
    pub log: Vec<String>,
    pub focus: String,
    pub hover: Vec<String>,
    pub capture: String,
    pub pressed: String,
    pub popups: i64,
    pub dialogs: Vec<String>,
    pub frames: i64,
    /// Why the window was last laid out again, newest last.
    pub causes: Vec<String>,
    pub theme: Theme,
    pub commands: Vec<Command>,
    /// The scopes around the focus, innermost first.
    pub scopes: Vec<String>,
    pub semantics: AccessNode,
    /// The last layout of the window's own content.
    pub node: Node,
}

/// A short name for a widget's kind.
pub fn kind(w: &Widget) -> &'static str {
    match w {
        Widget::Empty => "Empty",
        Widget::Text(_) => "Text",
        Widget::Spacer(_) => "Spacer",
        Widget::Separator(_) => "Separator",
        Widget::Image(_) => "Image",
        Widget::Icon(_) => "Icon",
        Widget::Badge(_) => "Badge",
        Widget::Flex(f) => {
            if f.axis == Axis::Horizontal {
                "Row"
            } else {
                "Column"
            }
        }
        Widget::Grid(_) => "Grid",
        Widget::Stack(_) => "Stack",
        Widget::Flow(_) => "Flow",
        Widget::Scroll(_) => "Scroll",
        Widget::Split(_) => "Split",
        Widget::Decorated(_) => "Box",
        Widget::Button(_) => "Button",
        Widget::Check(_) => "Check",
        Widget::RadioGroup(_) => "RadioGroup",
        Widget::Segmented(_) => "Segmented",
        Widget::TextInput(_) => "TextInput",
        Widget::Feedback(_) => "Feedback",
        Widget::ColorWheel(_) => "ColorWheel",
        Widget::TemporalEditor(_) => "DateTimeEditor",
        Widget::Calendar(_) => "Calendar",
        Widget::Numeric(_) => "Numeric",
        Widget::Choice(_) => "Choice",
        Widget::MenuBar(_) => "MenuBar",
        Widget::ContextMenu(_) => "ContextMenu",
        Widget::Collection(_) => "Collection",
        Widget::Tabs(_) => "Tabs",
        Widget::Terminal(_) => "Terminal",
        Widget::PagePreview(_) => "PagePreview",
        Widget::Chart(_) => "Chart",
        Widget::Scene(_) => "Scene",
        Widget::ImageView(_) => "ImageView",
        Widget::Media(_) => "Media",
        Widget::Custom(_) => "Custom",
    }
}

/// The hierarchy as indented lines.
/// The hierarchy as indented lines: kind, id, state, and where each widget
/// was last laid out (`x,y wxh`, window pixels) when `node` is its layout.
pub fn hierarchy(w: &Widget, node: &Node) -> Vec<String> {
    let mut out = vec![];
    walk(w, node, 0, 0, 0, &mut out);
    out
}

fn walk(w: &Widget, node: &Node, x: i64, y: i64, depth: i64, out: &mut Vec<String>) {
    if out.len() > 400 {
        return;
    }
    let mut line = String::new();
    let mut i = 0;
    while i < depth {
        line += "  ";
        i += 1;
    }
    line += kind(w);
    let id = w.id();
    if !id.is_empty() {
        line += &format!(" #{}", id)
    };
    if let Widget::Decorated(d) = w {
        if d.mods.disabled {
            line += " [disabled]"
        };
        if d.mods.visibility != Visibility::Visible {
            line += " [hidden]"
        };
        if d.mods.grow > 0 {
            line += &format!(" grow={}", d.mods.grow)
        }
    }
    let ax = x + node.rect.x;
    let ay = y + node.rect.y;
    line += &format!("  {},{} {}x{}", ax, ay, node.rect.width, node.rect.height);
    out.push(line);
    let kids = w.children();
    let mut k = 0;
    while k < kids.len() {
        let child = if k < node.children.len() {
            &node.children[k]
        } else {
            &Node::default()
        };
        walk(kids[k], child, ax, ay, depth + 1, out);
        k += 1;
    }
}

fn section(lines: &mut Vec<Widget>, title: &str) {
    lines.push(Widget::text(title, TextRole::Label, Tone::Normal))
}

fn mono(lines: &mut Vec<Widget>, text: &str) {
    lines.push(Widget::text(text, TextRole::Mono, Tone::Muted))
}

/// `root` with the inspector panel beside it.
pub fn wrap(root: Widget, snap: &Snapshot) -> Widget {
    let (entries, hits, misses, evictions) = layout::cache_stats();
    let (glyphs, glyph_bytes) = layout::glyph_cache_stats();
    let t = &snap.theme;
    let mut lines = vec![Widget::heading("Inspector")];
    section(&mut lines, "Window");
    mono(
        &mut lines,
        &format!(
            "focus: {}",
            if snap.focus.is_empty() {
                "(none)"
            } else {
                &snap.focus
            }
        ),
    );
    mono(&mut lines, &format!("hover: {}", snap.hover.join(" > ")));
    mono(
        &mut lines,
        &format!("capture: {}  pressed: {}", snap.capture, snap.pressed),
    );
    mono(
        &mut lines,
        &format!(
            "popups: {}  dialogs: {}  frames: {}",
            snap.popups,
            snap.dialogs.join(", "),
            snap.frames
        ),
    );
    mono(&mut lines, &format!("widget states: {}", snap.states.len()));
    section(&mut lines, "Layout invalidation");
    for c in &snap.causes {
        mono(&mut lines, c)
    }
    section(&mut lines, "Theme");
    mono(
        &mut lines,
        &format!(
            "{:?}  scale {}  text {}  {:?}  rtl {}  reduced motion {}",
            t.kind, t.scale, t.text_scale, t.density, t.rtl, t.reduced_motion
        ),
    );
    mono(
        &mut lines,
        &format!(
            "radius {}  control {}  row {}  body {}  icons {}",
            t.shape.radius,
            t.shape.control_height,
            t.shape.row_height,
            t.typography.body.size,
            if t.icons.is_empty() {
                "built-in"
            } else {
                &t.icons
            }
        ),
    );
    mono(
        &mut lines,
        &format!(
            "accent {}  text {}  background {}",
            t.accent.css(),
            t.text.css(),
            t.background.css()
        ),
    );
    section(&mut lines, "Caches");
    mono(
        &mut lines,
        &format!(
            "layout: {} entries, {} hits, {} misses, {} evictions",
            entries, hits, misses, evictions
        ),
    );
    mono(
        &mut lines,
        &format!("glyphs: {}, {} KiB", glyphs, glyph_bytes / 1024),
    );
    section(&mut lines, "Hierarchy");
    for l in hierarchy(&root, &snap.node) {
        mono(&mut lines, &l)
    }
    section(&mut lines, "Focus chain");
    mono(
        &mut lines,
        &crate::widgets::tree::focus_chain(&root, &snap.node, "").join(" > "),
    );
    section(&mut lines, "Commands");
    mono(&mut lines, &format!("scopes: {}", snap.scopes.join(" < ")));
    for c in &snap.commands {
        let state = if c.enabled { "" } else { " [disabled]" };
        let scope = if c.scope.is_empty() {
            ""
        } else {
            &format!(" in {}", c.scope)
        };
        mono(
            &mut lines,
            &format!(
                "{}  {}  {}{}{}",
                c.id,
                c.label,
                c.shortcut_label(),
                scope,
                state
            ),
        )
    }
    section(&mut lines, "Accessibility");
    let dump = snap.semantics.dump();
    let tree = dump.lines();
    for (shown, l) in tree.enumerate() {
        if shown >= 300 {
            break;
        };
        mono(&mut lines, l);
    }
    section(&mut lines, "Recent events");
    for entry in &snap.log[snap.log.len().saturating_sub(40)..] {
        mono(&mut lines, entry);
    }
    let panel = Widget::scroll("gui:inspector", Widget::column(&lines, 4).padding(12))
        .background(Tone::SurfaceAlt);
    Widget::split("gui:inspector-split", root, panel, Axis::Horizontal, 0.62)
}

#[cfg(test)]
mod tests {
    use crate::widgets::Widget;
    use crate::widgets::cx::UiMsg;
    use windowing::{Key, Mods};

    fn view(m: &i64) -> Widget {
        Widget::column(
            &[
                Widget::button("go", "Go"),
                Widget::text_field("name", "", "Name"),
                Widget::label(&format!("clicks {}", m)),
            ],
            8,
        )
        .padding(8)
    }

    #[test]
    fn the_inspector_shows_geometry_focus_theme_commands_and_semantics() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Inspected",
            "dev.gossamer.gui-inspector-test",
            || 0,
            |m: i64, msg: UiMsg| {
                if msg == UiMsg::Click("go".to_string()) {
                    m + 1
                } else {
                    m
                }
            },
            view,
        )
        .size(900, 600)
        .start()
        .unwrap();
        assert!(s.focus("name").unwrap());
        let keys = Mods {
            ctrl: true,
            shift: true,
            ..Mods::default()
        };
        s.press(Key::I, keys).unwrap();
        let dump = s.semantics(0).dump();
        for heading in [
            "Window",
            "Layout invalidation",
            "Theme",
            "Hierarchy",
            "Focus chain",
            "Commands",
            "Accessibility",
            "Recent events",
        ] {
            assert!(
                dump.contains(&format!("\"{}\"", heading)),
                "missing {}: {}",
                heading,
                dump
            )
        }
        assert!(dump.contains("focus: name"), "{}", dump);
        assert!(dump.contains("inspector toggled"), "{}", dump);
        assert!(dump.contains("Button #go  "), "{}", dump);
        assert!(dump.contains("go > name"), "{}", dump);
        assert!(dump.contains("gui.inspector  Toggle Inspector"), "{}", dump);
        assert!(dump.contains("textfield"), "{}", dump);
        // Input reaches the application's widgets beside the panel.
        assert!(s.click("go").unwrap());
        assert!(
            s.semantics(0).dump().contains("clicks 1"),
            "{}",
            s.semantics(0).dump()
        );
        s.shutdown()
    }
}
