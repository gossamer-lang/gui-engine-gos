pub mod buttons;
pub mod choices;
pub mod collections;
pub mod colorpicker;
pub mod completion;
pub mod containers;
pub mod cx;
pub mod display;
pub mod feedback;
pub mod find;
pub mod menus;
pub mod navigation;
pub mod numeric;
pub mod popup;
pub mod state;
pub mod tabs;
pub mod temporal;
pub mod textinput;
pub mod tree;

// widgets: the widget tree. An application's view function builds a
// `Widget` value from its model after every change; the toolkit lays it
// out into a `Node` tree, paints it, routes input through it, and
// mirrors it into the accessibility tree. Widget behavior (keyboard,
// pointer, focus, semantics) lives with each control; containers only
// arrange. Interaction state that must survive a rebuild (caret, scroll
// offset, open popup) lives in the window's `StateStore`, keyed by id.
//
// Contract per widget:
// - `layout(constraints)` answers a size inside the constraints and the
//   children's nodes; it reads state but never writes it.
// - `paint` draws inside the node's rectangle; popups and dialogs draw in
//   their own layers.
// - `event` handles input aimed at this widget or bubbling through it and
//   may only request changes through the `EventCx`.
// - `semantics` describes the widget for assistive technology.

use crate::strext::StrExt;
use std::collections::HashMap;
use windowing;

use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Insets, Rect, Size, UNBOUNDED};
use crate::render::display::DrawOp;
use crate::style::color::Color;
use crate::style::{TextRole, Theme};
use crate::text::layout::TextAlign;
use crate::widgets::completion::CompletionItem;
use crate::widgets::cx::{
    CheckState, Event, EventCx, LayoutCx, Node, PaintCx, PopupRequest, UiMsg,
};
use crate::widgets::state::StateStore;

/// A main-axis direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// Main-axis distribution in a flex container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MainAlign {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
}

/// Cross-axis placement in a flex container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CrossAlign {
    Start,
    Center,
    End,
    Stretch,
    Baseline,
}

/// Whether a widget shows, keeps its space invisibly, or takes no space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Visibility {
    Visible,
    Hidden,
    Collapsed,
}

/// A color role for text and fills, resolved against the theme at paint.
#[derive(Debug, Clone, PartialEq, Copy, serde::Serialize, serde::Deserialize)]
pub enum Tone {
    Normal,
    Muted,
    Disabled,
    Accent,
    OnAccent,
    Danger,
    Success,
    Warning,
    Info,
    Link,
    Surface,
    SurfaceAlt,
    Field,
    Background,
    Selection,
    Custom(Color),
}

impl Tone {
    /// The theme color for this role.
    pub fn resolve(&self, theme: &Theme) -> Color {
        match self {
            Tone::Normal => theme.text,
            Tone::Muted => theme.text_muted,
            Tone::Disabled => theme.text_disabled,
            Tone::Accent => theme.accent,
            Tone::OnAccent => theme.accent_text,
            Tone::Danger => theme.danger,
            Tone::Success => theme.success,
            Tone::Warning => theme.warning,
            Tone::Info => theme.info,
            Tone::Link => theme.link,
            Tone::Surface => theme.surface,
            Tone::SurfaceAlt => theme.surface_alt,
            Tone::Field => theme.field,
            Tone::Background => theme.background,
            Tone::Selection => theme.selection,
            Tone::Custom(c) => *c,
        }
    }
}

/// Layout and presentation modifiers a `Decorated` wrapper applies. Sizes
/// are interface units; -1 means "not set".
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Modifiers {
    pub padding: Insets,
    pub margin: Insets,
    pub width: i64,
    pub height: i64,
    pub min_width: i64,
    pub min_height: i64,
    pub max_width: i64,
    pub max_height: i64,
    /// Share of a flex container's free space (0 = natural size).
    pub grow: i64,
    /// Placement inside the space given (0..1), or -1 to fill it.
    pub align_x: f64,
    pub align_y: f64,
    pub background: Option<Tone>,
    pub border: bool,
    pub radius: i64,
    pub elevation: i64,
    pub tooltip: String,
    pub disabled: bool,
    pub visibility: Visibility,
    /// Accessible name, description, and role overrides.
    pub label: String,
    pub description: String,
    pub role: Option<Role>,
    /// Identity for focus scopes, drop targets, and inspection.
    pub key: String,
    pub drop_target: bool,
    /// What dragging the subtree away offers, when it is a drag source.
    pub drag: Option<containers::DragSource>,
    /// Width / height ratio to keep, or 0.
    pub aspect: f64,
    /// A theme override for the subtree: a palette, density, direction.
    pub theme: Option<ThemeOverride>,
    /// Shown but left out of the accessibility tree (separators, ornaments).
    pub decorative: bool,
    /// Pointer input lands on what is below except where the child is (an
    /// overlay that covers the window but shows only part of it).
    pub pass_through: bool,
}

impl Default for Modifiers {
    fn default() -> Modifiers {
        Modifiers {
            padding: Insets::all(0),
            margin: Insets::all(0),
            width: -1,
            height: -1,
            min_width: -1,
            min_height: -1,
            max_width: -1,
            max_height: -1,
            grow: 0,
            align_x: -1.0,
            align_y: -1.0,
            background: None,
            border: false,
            radius: -1,
            elevation: 0,
            tooltip: String::new(),
            disabled: false,
            visibility: Visibility::Visible,
            label: String::new(),
            description: String::new(),
            role: None,
            key: String::new(),
            drop_target: false,
            drag: None,
            aspect: 0.0,
            theme: None,
            decorative: false,
            pass_through: false,
        }
    }
}

/// A subtree theme change.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThemeOverride {
    pub kind: Option<crate::style::ThemeKind>,
    pub density: Option<crate::style::Density>,
    pub rtl: Option<bool>,
    pub text_scale: f64,
    /// Typography, shape, motion and icon tokens for the subtree.
    pub tokens: Option<crate::style::ThemeTokens>,
    /// Overrides the keyboard focus outline within this subtree.
    #[serde(default)]
    pub focus: Option<Color>,
}

impl ThemeOverride {
    pub fn apply(&self, theme: Theme) -> Theme {
        let mut t = match self.kind {
            Some(k) => theme.with_kind(k),
            None => theme,
        };
        if let Some(d) = self.density {
            t.density = d
        };
        if let Some(r) = self.rtl {
            t.rtl = r
        };
        if self.text_scale > 0.0 {
            t.text_scale = self.text_scale
        };
        if let Some(ref tokens) = self.tokens {
            t = t.with_tokens(tokens)
        };
        if let Some(color) = self.focus {
            t.focus = color;
        }
        t
    }
}

/// A widget the application draws itself: a display list from its size,
/// and messages from its input.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Custom {
    pub id: String,
    pub focusable: bool,
    pub role: Role,
    pub label: String,
    /// Natural size (pixels) for given constraints and theme.
    pub measure: crate::Callback<dyn Fn(Constraints, Theme) -> Size>,
    /// Draw operations in local pixels for a size and theme.
    pub draw: crate::Callback<dyn Fn(Size, Theme) -> Vec<DrawOp>>,
    /// Messages for an event in local pixels.
    pub input: crate::Callback<dyn Fn(CustomInput) -> Vec<UiMsg>>,
    /// Keeps every event it receives, even with no messages to emit (an
    /// embedded document), so keys such as Tab reach it.
    pub captures_input: bool,
}

impl PartialEq for Custom {
    fn eq(&self, other: &Custom) -> bool {
        self.id == other.id
    }
}

/// Input a custom widget receives, in its local pixels.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CustomInput {
    Press {
        x: i64,
        y: i64,
        button: windowing::MouseButton,
        clicks: i64,
        mods: windowing::Mods,
    },
    /// The pointer moved with a button held.
    Drag {
        x: i64,
        y: i64,
    },
    /// The pointer moved with no button held.
    Hover {
        x: i64,
        y: i64,
    },
    Release {
        x: i64,
        y: i64,
        button: windowing::MouseButton,
        mods: windowing::Mods,
    },
    /// The pointer left the widget.
    Leave,
    KeyPress(windowing::Key, windowing::Mods),
    KeyRelease(windowing::Key, windowing::Mods),
    /// Committed text: typed, chosen in an input method, or pasted.
    Text(String),
    /// The wheel turned at (`x`, `y`) by `dx`, `dy` notches.
    Wheel {
        x: i64,
        y: i64,
        dx: f64,
        dy: f64,
        mods: windowing::Mods,
    },
    /// Keyboard focus arrived (`true`) or left.
    Focus(bool),
}

/// One node of a window's view tree.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Widget {
    Empty,
    Text(display::Label),
    Spacer(display::Spacer),
    Separator(display::Separator),
    Image(display::Image),
    Icon(display::Icon),
    Badge(display::Badge),
    Flex(containers::Flex),
    Grid(containers::Grid),
    Stack(containers::Stack),
    Flow(containers::Flow),
    Scroll(containers::Scroll),
    Split(containers::Split),
    Decorated(Box<containers::DecoratedBox>),
    Button(buttons::Button),
    Check(buttons::Check),
    RadioGroup(buttons::RadioGroup),
    Segmented(buttons::Segmented),
    TextInput(textinput::TextInput),
    Feedback(feedback::Feedback),
    Numeric(numeric::Numeric),
    Calendar(temporal::Calendar),
    TemporalEditor(temporal::TemporalEditor),
    ColorWheel(colorpicker::ColorWheel),
    Choice(choices::Choice),
    MenuBar(menus::MenuBar),
    ContextMenu(menus::ContextMenu),
    Collection(collections::Collection),
    Tabs(tabs::Tabs),
    Terminal(crate::terminal::widget::View),
    PagePreview(crate::print::document::Preview),
    Chart(crate::chart::widget::View),
    Scene(crate::scene::widget::View),
    ImageView(crate::imageview::widget::View),
    Media(crate::media::widget::View),
    Custom(Custom),
}

impl Widget {
    pub(crate) fn compact_maps(&mut self) {
        match self {
            Self::Flex(container) => {
                for child in &mut container.children {
                    child.compact_maps();
                }
            }
            Self::Grid(container) => {
                for child in &mut container.children {
                    child.compact_maps();
                }
            }
            Self::Stack(container) => {
                for child in &mut container.layers {
                    child.compact_maps();
                }
            }
            Self::Flow(container) => {
                for child in &mut container.children {
                    child.compact_maps();
                }
            }
            Self::Scroll(container) => container.child.compact_maps(),
            Self::Split(container) => {
                container.first.compact_maps();
                container.second.compact_maps();
            }
            Self::Decorated(container) => container.child.compact_maps(),
            Self::ContextMenu(container) => container.child.compact_maps(),
            Self::Tabs(tabs) => {
                tabs.content.compact_maps();
                tabs.pages = tabs
                    .pages
                    .iter()
                    .map(|(key, value)| {
                        let mut value = value.clone();
                        value.compact_maps();
                        (key.clone(), value)
                    })
                    .collect();
            }
            _ => {}
        }
    }

    /// A validated Gregorian date editor; incomplete text remains a draft.
    pub fn date_field(id: &str, value: &str) -> Widget {
        Widget::TemporalEditor(temporal::TemporalEditor::new(
            id,
            value,
            temporal::TemporalKind::DateOnly,
        ))
    }
    pub fn time_field(id: &str, value: &str) -> Widget {
        Widget::TemporalEditor(temporal::TemporalEditor::new(
            id,
            value,
            temporal::TemporalKind::TimeOnly,
        ))
    }
    pub fn datetime_field(id: &str, value: &str) -> Widget {
        Widget::TemporalEditor(temporal::TemporalEditor::new(
            id,
            value,
            temporal::TemporalKind::DateAndTime,
        ))
    }
    pub fn duration_field(id: &str, value: &str) -> Widget {
        Widget::TemporalEditor(temporal::TemporalEditor::new(
            id,
            value,
            temporal::TemporalKind::Elapsed,
        ))
    }
    /// A virtual list, grid, table, or tree with stable row keys.
    pub fn collection(view: &collections::Collection) -> Widget {
        Widget::Collection(view.clone())
    }

    // ---- display ----

    /// Body text on one line.
    pub fn label(text: &str) -> Widget {
        Widget::Text(display::Label::new(text, TextRole::Body))
    }

    /// A section heading.
    pub fn heading(text: &str) -> Widget {
        Widget::Text(display::Label::new(text, TextRole::Heading))
    }

    /// A page or window title.
    pub fn title(text: &str) -> Widget {
        Widget::Text(display::Label::new(text, TextRole::Title))
    }

    /// Secondary text.
    pub fn muted(text: &str) -> Widget {
        let mut l = display::Label::new(text, TextRole::Small);
        l.tone = Tone::Muted;
        Widget::Text(l)
    }

    /// Body text that wraps to the width it is given.
    pub fn paragraph(text: &str) -> Widget {
        let mut l = display::Label::new(text, TextRole::Body);
        l.wrap = true;
        Widget::Text(l)
    }

    /// Text in a role with a tone.
    pub fn text(text: &str, role: TextRole, tone: Tone) -> Widget {
        let mut l = display::Label::new(text, role);
        l.tone = tone;
        Widget::Text(l)
    }

    /// One line of `text` that ends in an ellipsis where it does not fit.
    pub fn text_line(text: &str, role: TextRole, tone: Tone) -> Widget {
        let mut l = display::Label::new(text, role);
        l.tone = tone;
        l.max_lines = 1;
        Widget::Text(l)
    }

    /// Read-only text the user can select and copy.
    pub fn selectable(id: &str, text: &str) -> Widget {
        let mut l = display::Label::new(text, TextRole::Body);
        l.id = id.to_string();
        l.selectable = true;
        l.wrap = true;
        Widget::Text(l)
    }

    /// A focusable hyperlink-styled text; activation emits `Click(id)`.
    pub fn link(id: &str, text: &str) -> Widget {
        let mut l = display::Label::new(text, TextRole::Body);
        l.id = id.to_string();
        l.link = true;
        l.tone = Tone::Link;
        Widget::Text(l)
    }

    /// Fixed empty space of `units` on both axes.
    pub fn spacer(units: i64) -> Widget {
        Widget::Spacer(display::Spacer {
            units,
            flexible: false,
        })
    }

    /// Space that takes the free room in a flex container.
    pub fn stretch() -> Widget {
        Widget::Spacer(display::Spacer {
            units: 0,
            flexible: true,
        })
    }

    /// A horizontal rule.
    pub fn separator() -> Widget {
        Widget::Separator(display::Separator { vertical: false })
    }

    pub fn vseparator() -> Widget {
        Widget::Separator(display::Separator { vertical: true })
    }

    /// An RGBA image with alternative text (empty `alt` marks it decorative).
    pub fn image(rgba: &[u8], width: i64, height: i64, alt: &str) -> Widget {
        Widget::Image(display::Image {
            rgba: rgba.to_vec(),
            width,
            height,
            alt: alt.to_string(),
            fit: display::Fit::Contain,
            error: String::new(),
        })
    }

    /// A named vector icon from the built-in set, `units` square.
    pub fn icon(icon: &crate::render::icons::Icon, units: i64) -> Widget {
        Widget::Icon(display::Icon {
            icon: icon.clone(),
            units,
            tone: Tone::Normal,
            label: String::new(),
        })
    }

    /// A button that opens `items` as a menu below itself (on press, Enter,
    /// Space or Down).
    pub fn menu_button(id: &str, label: &str, items: &[menus::MenuItem]) -> Widget {
        let mut b = buttons::Button::new(id, label, buttons::ButtonKind::Normal);
        b.menu_arrow = true;
        b.menu = items.to_vec();
        Widget::Button(b)
    }

    /// A button whose main part emits `Click(id)` and whose arrow part (or
    /// Down from the keyboard) opens `items` as a menu.
    pub fn split_button(id: &str, label: &str, items: &[menus::MenuItem]) -> Widget {
        let mut b = buttons::Button::new(id, label, buttons::ButtonKind::Normal);
        b.menu = items.to_vec();
        b.split = true;
        Widget::Button(b)
    }

    /// A small pill showing a count or status word. `label` is what
    /// assistive technology reads ("3 unread"); empty reads `text`.
    pub fn badge(text: &str, tone: Tone, label: &str) -> Widget {
        Widget::Badge(display::Badge {
            text: text.to_string(),
            tone,
            label: label.to_string(),
        })
    }

    // ---- containers ----

    /// Children left to right (start to end in right-to-left layouts).
    pub fn row(children: &[Widget], spacing: i64) -> Widget {
        Widget::Flex(containers::Flex::new(Axis::Horizontal, children, spacing))
    }

    /// Children top to bottom.
    pub fn column(children: &[Widget], spacing: i64) -> Widget {
        Widget::Flex(containers::Flex::new(Axis::Vertical, children, spacing))
    }

    /// A flex container with explicit alignment.
    pub fn flex(
        axis: Axis,
        children: &[Widget],
        spacing: i64,
        main: MainAlign,
        cross: CrossAlign,
    ) -> Widget {
        let mut f = containers::Flex::new(axis, children, spacing);
        f.main = main;
        f.cross = cross;
        Widget::Flex(f)
    }

    /// Children in `columns` columns, row-major. Column widths follow the
    /// widest cell; `stretch_last` gives the last column the free width.
    pub fn grid(children: &[Widget], columns: i64, gap: i64) -> Widget {
        Widget::Grid(containers::Grid {
            children: children.to_vec(),
            columns,
            column_gap: gap,
            row_gap: gap,
            stretch_last: false,
        })
    }

    /// Label/field pairs: labels end-aligned in the first column, fields
    /// filling the second.
    pub fn form(rows: &[(String, Widget)]) -> Widget {
        let mut cells = vec![];
        for (label, field) in rows {
            let mut l = display::Label::new(label, TextRole::Body);
            l.align = TextAlign::End;
            cells.push(Widget::Text(l).align(1.0, 0.5));
            cells.push(field.clone())
        }
        Widget::Grid(containers::Grid {
            children: cells,
            columns: 2,
            column_gap: 12,
            row_gap: 10,
            stretch_last: true,
        })
    }

    /// Children painted over each other, first at the bottom.
    pub fn stack(layers: &[Widget]) -> Widget {
        Widget::Stack(containers::Stack {
            layers: layers.to_vec(),
        })
    }

    /// Children that wrap onto new lines when the width runs out.
    pub fn flow(children: &[Widget], spacing: i64) -> Widget {
        Widget::Flow(containers::Flow {
            children: children.to_vec(),
            spacing,
            line_spacing: spacing,
        })
    }

    /// A vertically scrolling viewport over `child`.
    pub fn scroll(id: &str, child: Widget) -> Widget {
        Widget::Scroll(containers::Scroll {
            id: id.to_string(),
            child: Box::new(child),
            horizontal: false,
            vertical: true,
            fit: false,
        })
    }

    /// A vertical scroll as tall as its content, up to the height offered;
    /// inside a flex it takes no more of its share than the content needs.
    pub fn scroll_to_fit(id: &str, child: Widget) -> Widget {
        Widget::Scroll(containers::Scroll {
            id: id.to_string(),
            child: Box::new(child),
            horizontal: false,
            vertical: true,
            fit: true,
        })
    }

    /// A horizontally scrolling strip over `child`, as tall as its content.
    pub fn scroll_horizontal(id: &str, child: Widget) -> Widget {
        Widget::Scroll(containers::Scroll {
            id: id.to_string(),
            child: Box::new(child),
            horizontal: true,
            vertical: false,
            fit: false,
        })
    }

    /// A viewport scrolling on both axes.
    pub fn scroll_both(id: &str, child: Widget) -> Widget {
        Widget::Scroll(containers::Scroll {
            id: id.to_string(),
            child: Box::new(child),
            horizontal: true,
            vertical: true,
            fit: false,
        })
    }

    /// Two panes with a draggable divider; `ratio` (0..1) is the first
    /// pane's share and `Split(id, ratio)` reports drags.
    pub fn split(id: &str, first: Widget, second: Widget, axis: Axis, ratio: f64) -> Widget {
        Widget::Split(containers::Split {
            id: id.to_string(),
            first: Box::new(first),
            second: Box::new(second),
            axis,
            ratio,
            min_first: 80,
            min_second: 80,
        })
    }

    /// A raised card: surface background, border, radius, padding.
    pub fn card(child: Widget) -> Widget {
        let m = Modifiers {
            padding: Insets::all(16),
            background: Some(Tone::Surface),
            border: true,
            ..Modifiers::default()
        };
        Widget::Decorated(Box::new(containers::DecoratedBox {
            child: Box::new(child),
            mods: m,
        }))
    }

    /// A titled group box.
    pub fn group(title: &str, child: Widget) -> Widget {
        let body = Widget::column(
            &[Widget::text(title, TextRole::Label, Tone::Normal), child],
            8,
        );
        let m = Modifiers {
            padding: Insets::all(12),
            border: true,
            role: Some(Role::Group),
            label: title.to_string(),
            ..Modifiers::default()
        };
        Widget::Decorated(Box::new(containers::DecoratedBox {
            child: Box::new(body),
            mods: m,
        }))
    }

    // ---- actions ----

    /// A push button; activation emits `Click(id)`.
    pub fn button(id: &str, label: &str) -> Widget {
        Widget::Button(buttons::Button::new(id, label, buttons::ButtonKind::Normal))
    }

    /// The emphasised default action of a view or dialog.
    pub fn primary_button(id: &str, label: &str) -> Widget {
        Widget::Button(buttons::Button::new(
            id,
            label,
            buttons::ButtonKind::Primary,
        ))
    }

    /// A destructive action.
    pub fn danger_button(id: &str, label: &str) -> Widget {
        Widget::Button(buttons::Button::new(id, label, buttons::ButtonKind::Danger))
    }

    /// A borderless button for toolbars and dense rows.
    pub fn flat_button(id: &str, label: &str) -> Widget {
        Widget::Button(buttons::Button::new(id, label, buttons::ButtonKind::Flat))
    }

    /// An icon-only toolbar button; `label` is its accessible name and tooltip.
    pub fn tool_button(id: &str, icon: &crate::render::icons::Icon, label: &str) -> Widget {
        let mut b = buttons::Button::new(id, label, buttons::ButtonKind::Tool);
        b.icon = Some(icon.clone());
        Widget::Button(b).tooltip(label)
    }

    /// A square button showing `color`, named `label` for its tooltip and
    /// for assistive technology (a palette swatch).
    pub fn swatch_button(id: &str, color: crate::style::color::Color, label: &str) -> Widget {
        let mut b = buttons::Button::new(id, label, buttons::ButtonKind::Tool);
        b.swatch = Some(color);
        Widget::Button(b).tooltip(label)
    }

    /// A button bound to a command: label, shortcut, and enabled state
    /// come from the command; activation runs it.
    pub fn command_button(command: &str, label: &str) -> Widget {
        let mut b = buttons::Button::new(
            &format!("cmd:{}", command),
            label,
            buttons::ButtonKind::Normal,
        );
        b.command = command.to_string();
        Widget::Button(b)
    }

    /// A button that stays pressed; emits `Toggle(id, on)`.
    pub fn toggle_button(id: &str, label: &str, on: bool) -> Widget {
        let mut b = buttons::Button::new(id, label, buttons::ButtonKind::Normal);
        b.toggled = if on { 1 } else { 0 };
        Widget::Button(b)
    }

    /// A checkbox; emits `Toggle(id, checked)`.
    pub fn checkbox(id: &str, label: &str, checked: bool) -> Widget {
        Widget::Check(buttons::Check::checkbox(
            id,
            label,
            if checked {
                CheckState::Checked
            } else {
                CheckState::Unchecked
            },
        ))
    }

    /// A tri-state checkbox; emits `Check(id, state)`, cycling through
    /// mixed only when `cycle_mixed`.
    pub fn tristate(id: &str, label: &str, state: CheckState, cycle_mixed: bool) -> Widget {
        let mut c = buttons::Check::checkbox(id, label, state);
        c.tristate = true;
        c.cycle_mixed = cycle_mixed;
        Widget::Check(c)
    }

    /// An on/off switch; emits `Toggle(id, on)`.
    pub fn switch(id: &str, label: &str, on: bool) -> Widget {
        let mut c = buttons::Check::checkbox(
            id,
            label,
            if on {
                CheckState::Checked
            } else {
                CheckState::Unchecked
            },
        );
        c.switch = true;
        Widget::Check(c)
    }

    /// Mutually exclusive options; arrow keys move the choice and
    /// `Choose(id, key)` reports it.
    pub fn radio_group(
        id: &str,
        options: &[(String, String)],
        selected: &str,
        horizontal: bool,
    ) -> Widget {
        Widget::RadioGroup(buttons::RadioGroup {
            id: id.to_string(),
            options: options.to_vec(),
            selected: selected.to_string(),
            horizontal,
            disabled_keys: vec![],
        })
    }

    /// A row of exclusive buttons; emits `Choose(id, key)`.
    pub fn segmented(id: &str, options: &[(String, String)], selected: &str) -> Widget {
        Widget::Segmented(buttons::Segmented {
            id: id.to_string(),
            options: options.to_vec(),
            selected: selected.to_string(),
        })
    }

    // ---- text ----

    /// A single-line text field; edits emit `Text(id, text)` and Enter
    /// emits `Submit(id, text)`.
    pub fn text_field(id: &str, text: &str, placeholder: &str) -> Widget {
        Widget::TextInput(textinput::TextInput::new(
            id,
            text,
            placeholder,
            textinput::InputKind::Line,
        ))
    }

    /// A multi-line text area `rows` lines tall.
    pub fn text_area(id: &str, text: &str, rows: i64) -> Widget {
        let mut t = textinput::TextInput::new(id, text, "", textinput::InputKind::Area);
        t.rows = rows;
        Widget::TextInput(t)
    }

    /// A field that hides its text, with a reveal toggle; copy and cut are
    /// disabled and its value is never exposed to assistive technology.
    pub fn password_field(id: &str, text: &str, placeholder: &str) -> Widget {
        Widget::TextInput(textinput::TextInput::new(
            id,
            text,
            placeholder,
            textinput::InputKind::Password,
        ))
    }

    /// A search field with a clear button; Escape clears it.
    pub fn search_field(id: &str, text: &str, placeholder: &str) -> Widget {
        Widget::TextInput(textinput::TextInput::new(
            id,
            text,
            placeholder,
            textinput::InputKind::Search,
        ))
    }

    /// A text input built field by field (validation, masks, history).
    pub fn input(input: &textinput::TextInput) -> Widget {
        Widget::TextInput(input.clone())
    }

    // ---- numbers ----

    /// A number field with step buttons; reports `Value(id, v)`.
    pub fn spin_box(id: &str, value: f64, min: f64, max: f64, step: f64, decimals: i64) -> Widget {
        Widget::Numeric(numeric::Numeric::Spin(numeric::SpinBox {
            id: id.to_string(),
            value,
            min,
            max,
            step,
            page: step * 10.0,
            decimals,
            suffix: String::new(),
            label: String::new(),
        }))
    }

    /// A horizontal slider; reports `Value(id, v)`.
    pub fn slider(id: &str, value: f64, min: f64, max: f64, step: f64) -> Widget {
        Widget::Numeric(numeric::Numeric::Slide(numeric::Slider {
            id: id.to_string(),
            value,
            min,
            max,
            step,
            page: 0.0,
            vertical: false,
            ticks: 0,
            label: String::new(),
        }))
    }

    /// A two-thumb slider; reports `Range(id, low, high)`.
    pub fn range_slider(id: &str, low: f64, high: f64, min: f64, max: f64, step: f64) -> Widget {
        Widget::Numeric(numeric::Numeric::Range(numeric::RangeSlider {
            id: id.to_string(),
            low,
            high,
            min,
            max,
            step,
            label: String::new(),
        }))
    }

    /// A rotary knob; reports `Value(id, v)`.
    pub fn dial(id: &str, value: f64, min: f64, max: f64, step: f64) -> Widget {
        Widget::Numeric(numeric::Numeric::Knob(numeric::Dial {
            id: id.to_string(),
            value,
            min,
            max,
            step,
            wraps: false,
            label: String::new(),
        }))
    }

    // ---- choices ----

    /// A non-editable choice of one item key; reports `Select(id, key)`.
    pub fn select(id: &str, items: &[CompletionItem], selected: &str, placeholder: &str) -> Widget {
        Widget::Choice(choices::Choice::One(choices::Select {
            id: id.to_string(),
            items: items.to_vec().into(),
            selected: selected.to_string(),
            placeholder: placeholder.to_string(),
            label: String::new(),
            loading: false,
            error: String::new(),
        }))
    }

    /// A searchable picker; reports `Select` (single) or `SelectMany`.
    pub fn picker(
        id: &str,
        items: &[CompletionItem],
        selected: &[String],
        multiple: bool,
        placeholder: &str,
    ) -> Widget {
        Widget::Choice(choices::Choice::Pick(choices::Picker {
            id: id.to_string(),
            items: items.to_vec().into(),
            selected: selected.to_vec(),
            multiple,
            placeholder: placeholder.to_string(),
            label: String::new(),
            loading: false,
            error: String::new(),
        }))
    }

    /// An editable combo: free text (or only listed items when `strict`)
    /// with a dropdown of `items`; reports `Text` and, on choosing, `Select`.
    pub fn combo(
        id: &str,
        text: &str,
        items: &[CompletionItem],
        strict: bool,
        placeholder: &str,
    ) -> Widget {
        let mut t = textinput::TextInput::new(id, text, placeholder, textinput::InputKind::Line);
        let mut c = crate::widgets::completion::Completion::local(
            items,
            crate::widgets::completion::MatchMode::Substring,
        );
        c.min_chars = 1;
        t.completion = Some(c);
        t.dropdown = true;
        t.strict = strict;
        Widget::TextInput(t)
    }

    /// A text field that proposes completions as the user types.
    pub fn autocomplete(
        id: &str,
        text: &str,
        completion: &crate::widgets::completion::Completion,
        placeholder: &str,
    ) -> Widget {
        let mut t = textinput::TextInput::new(id, text, placeholder, textinput::InputKind::Line);
        t.completion = Some(completion.clone());
        Widget::TextInput(t)
    }

    /// An inline command palette over the window's commands.
    pub fn command_palette(id: &str) -> Widget {
        Widget::Choice(choices::Choice::Commands(choices::Palette {
            id: id.to_string(),
            placeholder: "Search commands".to_string(),
        }))
    }

    /// Items from (key, label) pairs.
    pub fn items(pairs: &[(String, String)]) -> Vec<CompletionItem> {
        pairs
            .iter()
            .map(|p| CompletionItem::new(&p.0, &p.1))
            .collect::<Vec<_>>()
    }

    // ---- menus ----

    /// A menu bar; items run commands from the window's registry.
    pub fn menu_bar(id: &str, menus: &[menus::Menu]) -> Widget {
        Widget::MenuBar(menus::MenuBar {
            id: id.to_string(),
            menus: menus.to_vec(),
            joined: false,
        })
    }

    /// A menu bar joined to the content below it: it takes the field color
    /// a text area or terminal fills with, and no line divides the two.
    pub fn joined_menu_bar(id: &str, menus: &[menus::Menu]) -> Widget {
        Widget::MenuBar(menus::MenuBar {
            id: id.to_string(),
            menus: menus.to_vec(),
            joined: true,
        })
    }

    /// This widget with a context menu (right-click, Shift+F10, Menu key).
    pub fn context_menu(self, id: &str, items: &[menus::MenuItem]) -> Widget {
        Widget::ContextMenu(menus::ContextMenu {
            id: id.to_string(),
            child: Box::new(self),
            items: items.to_vec(),
        })
    }

    /// A tab strip over `content`, the active tab's content. The
    /// application keeps the tab list and the active key; see `tabs`.
    pub fn tabs(id: &str, items: &[tabs::TabItem], active: &str, content: Widget) -> Widget {
        Widget::Tabs(tabs::Tabs {
            id: id.to_string(),
            items: items.to_vec(),
            active: active.to_string(),
            content: Box::new(content),
            pages: HashMap::new(),
            workspace: "".to_string(),
            add_button: true,
            detachable: false,
            joined: false,
        })
    }

    /// A tab strip joined to the content below it, as a terminal's tabs are:
    /// the field color, the desktop's menu text size, no dividing line, and
    /// no new-tab button.
    pub fn joined_tabs(id: &str, items: &[tabs::TabItem], active: &str, content: Widget) -> Widget {
        Widget::Tabs(tabs::Tabs {
            id: id.to_string(),
            items: items.to_vec(),
            active: active.to_string(),
            content: Box::new(content),
            pages: HashMap::new(),
            workspace: "".to_string(),
            add_button: false,
            detachable: false,
            joined: true,
        })
    }

    /// A tab strip whose tabs can move to a new window (`detach` actions).
    pub fn detachable_tabs(
        id: &str,
        items: &[tabs::TabItem],
        active: &str,
        content: Widget,
    ) -> Widget {
        Widget::Tabs(tabs::Tabs {
            id: id.to_string(),
            items: items.to_vec(),
            active: active.to_string(),
            content: Box::new(content),
            pages: HashMap::new(),
            workspace: "".to_string(),
            add_button: true,
            detachable: true,
            joined: false,
        })
    }

    /// Retains every page's caret, undo, and scroll state, including inactive pages.
    /// A workspace gives each page one owner across the application's windows.
    pub fn tab_pages(
        id: &str,
        workspace: &str,
        items: &[tabs::TabItem],
        active: &str,
        pages: &HashMap<String, Widget>,
    ) -> Widget {
        let content = Box::new(pages.get(active).cloned().unwrap_or(Widget::Empty));
        Widget::Tabs(tabs::Tabs {
            id: id.to_string(),
            items: items.to_vec(),
            active: active.to_string(),
            content,
            pages: pages.clone(),
            workspace: workspace.to_string(),
            add_button: true,
            detachable: true,
            joined: false,
        })
    }

    // ---- custom ----

    pub fn custom(custom: &Custom) -> Widget {
        Widget::Custom(custom.clone())
    }

    // ---- modifiers ----

    fn decorate(self, apply: impl FnOnce(Modifiers) -> Modifiers) -> Widget {
        match self {
            Widget::Decorated(d) => Widget::Decorated(Box::new(containers::DecoratedBox {
                child: d.child,
                mods: apply(d.mods),
            })),
            other => Widget::Decorated(Box::new(containers::DecoratedBox {
                child: Box::new(other),
                mods: apply(Modifiers::default()),
            })),
        }
    }

    /// Inner space of `units` on every side.
    pub fn padding(self, units: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.padding = Insets::all(units);
            n
        })
    }

    pub fn padding_xy(self, horizontal: i64, vertical: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.padding = Insets::symmetric(horizontal, vertical);
            n
        })
    }

    pub fn margin(self, units: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.margin = Insets::all(units);
            n
        })
    }

    /// Inner space on each side, by logical side: `start` is the left in
    /// left-to-right layouts and the right in right-to-left ones.
    pub fn padding_sides(self, start: i64, top: i64, end: i64, bottom: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.padding = Insets::new(start, top, end, bottom);
            n
        })
    }

    /// Outer space on each side, by logical side (see `padding_sides`).
    pub fn margin_sides(self, start: i64, top: i64, end: i64, bottom: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.margin = Insets::new(start, top, end, bottom);
            n
        })
    }

    /// Takes `weight` shares of a flex container's free space.
    pub fn grow(self, weight: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.grow = weight;
            n
        })
    }

    pub fn width(self, units: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.width = units;
            n
        })
    }

    pub fn height(self, units: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.height = units;
            n
        })
    }

    pub fn min_width(self, units: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.min_width = units;
            n
        })
    }

    pub fn max_width(self, units: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.max_width = units;
            n
        })
    }

    pub fn min_height(self, units: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.min_height = units;
            n
        })
    }

    pub fn max_height(self, units: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.max_height = units;
            n
        })
    }

    /// Placed at fractions of the space given instead of filling it.
    pub fn align(self, x: f64, y: f64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.align_x = x;
            n.align_y = y;
            n
        })
    }

    pub fn center(self) -> Widget {
        self.align(0.5, 0.5)
    }

    pub fn background(self, tone: Tone) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.background = Some(tone);
            n
        })
    }

    pub fn bordered(self) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.border = true;
            n
        })
    }

    pub fn radius(self, units: i64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.radius = units;
            n
        })
    }

    pub fn tooltip(self, text: &str) -> Widget {
        let base = self.id();
        self.decorate(|m| {
            let mut n = m;
            n.tooltip = text.to_string();
            if n.key.is_empty() {
                n.key = format!("tip:{}:{}", base, text)
            };
            n
        })
    }

    /// Disables the subtree: no input, no focus, disabled styling.
    pub fn disabled(self, disabled: bool) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.disabled = disabled;
            n
        })
    }

    pub fn visibility(self, v: Visibility) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.visibility = v;
            n
        })
    }

    /// Collapses the subtree unless `shown`.
    pub fn shown(self, shown: bool) -> Widget {
        self.visibility(if shown {
            Visibility::Visible
        } else {
            Visibility::Collapsed
        })
    }

    /// The accessible name assistive technology announces.
    pub fn accessible(self, label: &str) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.label = label.to_string();
            n
        })
    }

    /// Lets pointer input through to the layers below except over the child.
    pub fn pass_through(self) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.pass_through = true;
            n
        })
    }

    /// Leaves the subtree out of the accessibility tree; it still shows.
    pub fn decorative(self) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.decorative = true;
            n
        })
    }

    pub fn described(self, description: &str) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.description = description.to_string();
            n
        })
    }

    pub fn role(self, role: Role) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.role = Some(role);
            n
        })
    }

    pub fn key(self, key: &str) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.key = key.to_string();
            n
        })
    }

    /// Accepts dropped files, reported as `Dropped(key, paths)`.
    pub fn drop_target(self, key: &str) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.key = key.to_string();
            n.drop_target = true;
            n
        })
    }

    /// Lets the pointer drag `items` out of the subtree, for content that
    /// does not take the press itself (labels, icons, previews): a press and
    /// a move of a few units start a platform drag, answered by
    /// `UiMsg::DragFinished(id, operation)`.
    pub fn drag_source(
        self,
        id: &str,
        items: &[windowing::DragItem],
        operation: windowing::DragOperation,
    ) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.key = id.to_string();
            n.drag = Some(containers::DragSource {
                items: items.to_vec(),
                operation,
            });
            n
        })
    }

    pub fn aspect(self, ratio: f64) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.aspect = ratio;
            n
        })
    }

    /// Paints the subtree with a different theme palette, density, or
    /// direction.
    pub fn themed(self, change: &ThemeOverride) -> Widget {
        self.decorate(|m| {
            let mut n = m;
            n.theme = Some(change.clone());
            n
        })
    }

    /// Lays the subtree out right to left (or left to right).
    pub fn direction_rtl(self, rtl: bool) -> Widget {
        self.themed(&ThemeOverride {
            kind: None,
            density: None,
            rtl: Some(rtl),
            text_scale: 0.0,
            tokens: None,
            focus: None,
        })
    }

    // ---- structure ----

    /// The widget's id, or "" for widgets without identity.
    pub fn id(&self) -> String {
        match self {
            Widget::Text(l) => l.id.clone(),
            Widget::Scroll(s) => s.id.clone(),
            Widget::Split(s) => s.id.clone(),
            Widget::Decorated(d) => d.mods.key.clone(),
            Widget::Button(b) => b.id.clone(),
            Widget::Check(c) => c.id.clone(),
            Widget::RadioGroup(r) => r.id.clone(),
            Widget::Segmented(s) => s.id.clone(),
            Widget::TextInput(t) => t.id.clone(),
            Widget::Feedback(f) => f.id(),
            Widget::ColorWheel(c) => c.id.clone(),
            Widget::TemporalEditor(t) => t.id.clone(),
            Widget::Calendar(c) => c.id.clone(),
            Widget::Numeric(n) => n.id(),
            Widget::Choice(c) => c.id(),
            Widget::MenuBar(b) => b.id.clone(),
            Widget::ContextMenu(m) => m.id.clone(),
            Widget::Collection(c) => c.id.clone(),
            Widget::Tabs(t) => t.id.clone(),
            Widget::Terminal(t) => t.id.clone(),
            Widget::PagePreview(p) => p.id.clone(),
            Widget::Chart(c) => c.id.clone(),
            Widget::Scene(c) => c.id.clone(),
            Widget::ImageView(c) => c.id.clone(),
            Widget::Media(c) => c.id.clone(),
            Widget::Custom(c) => c.id.clone(),
            _ => String::new(),
        }
    }

    /// The children, in the order their nodes appear.
    pub fn children(&self) -> Vec<&Widget> {
        match self {
            Widget::Flex(f) => f.children.iter().collect(),
            Widget::Grid(g) => g.children.iter().collect(),
            Widget::Stack(s) => s.layers.iter().collect(),
            Widget::Flow(f) => f.children.iter().collect(),
            Widget::Scroll(s) => vec![&*s.child],
            Widget::Split(s) => vec![&*s.first, &*s.second],
            Widget::Decorated(d) => vec![&*d.child],
            Widget::ContextMenu(m) => vec![&*m.child],
            Widget::Tabs(t) => vec![&*t.content],
            _ => vec![],
        }
    }

    /// Whether this widget takes typed text while focused, so the window
    /// turns on the platform's text input (keyboard text and input methods).
    pub fn accepts_text(&self) -> bool {
        match self {
            Widget::TextInput(t) => !t.read_only,
            Widget::Custom(k) => k.focusable,
            Widget::Terminal(_)
            | Widget::TemporalEditor(_)
            | Widget::Numeric(_)
            | Widget::Choice(_)
            | Widget::Collection(_) => true,
            _ => false,
        }
    }

    /// Whether keyboard focus can rest on this widget.
    pub fn focusable(&self) -> bool {
        match self {
            Widget::Text(l) => !l.id.is_empty() && (l.selectable || l.link),
            Widget::Button(_)
            | Widget::Check(_)
            | Widget::TextInput(_)
            | Widget::Split(_)
            | Widget::ColorWheel(_)
            | Widget::TemporalEditor(_)
            | Widget::Calendar(_)
            | Widget::Numeric(_)
            | Widget::Choice(_) => true,
            Widget::RadioGroup(r) => !r.options.is_empty(),
            Widget::Segmented(s) => !s.options.is_empty(),
            Widget::Feedback(f) => f.focusable(),
            Widget::MenuBar(b) => !b.menus.is_empty() && !windowing::menu::has_native_menu_bar(),
            Widget::Collection(c) => c.count > 0,
            Widget::Tabs(t) => !t.items.is_empty(),
            Widget::Terminal(_)
            | Widget::Chart(_)
            | Widget::Scene(_)
            | Widget::ImageView(_)
            | Widget::Media(_) => true,
            Widget::Custom(c) => c.focusable,
            _ => false,
        }
    }

    /// Whether the widget clips its children (for hit testing and paint).
    pub fn clips(&self) -> bool {
        matches!(self, Widget::Scroll(_) | Widget::Split(_))
    }

    /// Whether this child takes only as much of a flex share as its content needs.
    pub fn fits_content(&self) -> bool {
        match self {
            Widget::Decorated(d) => d.child.fits_content(),
            Widget::Scroll(s) => s.fit,
            _ => false,
        }
    }

    /// The flex weight a container gives this child.
    pub fn grow_weight(&self) -> i64 {
        match self {
            Widget::Decorated(d) => {
                if d.mods.visibility == Visibility::Collapsed {
                    0
                } else {
                    d.mods.grow
                }
            }
            Widget::Spacer(s) if s.flexible => 1,
            _ => 0,
        }
    }

    /// Whether the widget takes no space (`Visibility::Collapsed`).
    pub fn is_collapsed(&self) -> bool {
        match self {
            Widget::Decorated(d) => d.mods.visibility == Visibility::Collapsed,
            _ => false,
        }
    }

    // ---- passes ----

    /// Lays the widget out within `c`; the node's rect sits at (0, 0).
    pub fn layout(&self, c: Constraints, cx: &mut LayoutCx) -> Node {
        match self {
            Widget::Empty => Node::leaf(c.constrain(Size::zero())),
            Widget::Text(l) => display::layout_label(l, c, cx),
            Widget::Spacer(s) => display::layout_spacer(*s, c, cx),
            Widget::Separator(s) => display::layout_separator(*s, c, cx),
            Widget::Image(i) => display::layout_image(i, c, cx),
            Widget::Icon(i) => display::layout_icon(i, c, cx),
            Widget::Badge(b) => display::layout_badge(b, c, cx),
            Widget::Flex(f) => containers::layout_flex(f, c, cx),
            Widget::Grid(g) => containers::layout_grid(g, c, cx),
            Widget::Stack(s) => containers::layout_stack(s, c, cx),
            Widget::Flow(f) => containers::layout_flow(f, c, cx),
            Widget::Scroll(s) => containers::layout_scroll(s, c, cx),
            Widget::Split(s) => containers::layout_split(s, c, cx),
            Widget::Decorated(d) => containers::layout_decorated(d, c, cx),
            Widget::Button(b) => buttons::layout_button(b, c, cx),
            Widget::Check(k) => buttons::layout_check(k, c, cx),
            Widget::RadioGroup(r) => buttons::layout_radio_group(r, c, cx),
            Widget::Segmented(s) => buttons::layout_segmented(s, c, cx),
            Widget::TextInput(t) => textinput::layout_input(t, c, cx),
            Widget::Feedback(f) => feedback::layout_feedback(f, c, cx),
            Widget::ColorWheel(_) => colorpicker::layout_wheel(c, cx),
            Widget::TemporalEditor(t) => temporal::layout_editor(t, c, cx),
            Widget::Calendar(_) => temporal::layout_calendar(c, cx),
            Widget::Numeric(n) => numeric::layout_numeric(n, c, cx),
            Widget::Choice(ch) => choices::layout_choice(ch, c, cx),
            // Where the platform draws the menu bar (macOS), the session installs it there.
            Widget::MenuBar(b) => {
                if windowing::menu::has_native_menu_bar() {
                    Node::leaf(c.constrain(Size::new(0, 0)))
                } else {
                    menus::layout_menu_bar(b, c, cx)
                }
            }
            Widget::ContextMenu(m) => menus::layout_context(m, c, cx),
            Widget::Collection(v) => collections::layout_collection(v, c, cx),
            Widget::Tabs(t) => tabs::layout_tabs(t, c, cx),
            Widget::Terminal(t) => crate::terminal::widget::layout_terminal(t, c, cx),
            Widget::PagePreview(p) => crate::print::document::layout_preview(p, c, cx),
            Widget::Chart(v) => crate::chart::widget::layout_chart(v, c, cx),
            Widget::Scene(v) => crate::scene::widget::layout_scene(v, c, cx),
            Widget::ImageView(v) => crate::imageview::widget::layout_viewer(v, c, cx),
            Widget::Media(v) => crate::media::widget::layout_media(v, c, cx),
            Widget::Custom(k) => {
                let measure = &k.measure;
                Node::leaf(c.constrain(measure(c, cx.theme.clone())))
            }
        }
    }

    /// Paints the widget; `cx.origin_*` is the node's absolute position.
    pub fn paint(&self, node: &Node, cx: &mut PaintCx) {
        match self {
            Widget::Empty | Widget::Spacer(_) => (),
            Widget::Text(l) => display::paint_label(l, node, cx),
            Widget::Separator(s) => display::paint_separator(*s, node, cx),
            Widget::Image(i) => display::paint_image(i, node, cx),
            Widget::Icon(i) => display::paint_icon(i, node, cx),
            Widget::Badge(b) => display::paint_badge(b, node, cx),
            Widget::Decorated(d) => containers::paint_decorated(d, node, cx),
            Widget::Scroll(s) => containers::paint_scroll(s, node, cx),
            Widget::Split(s) => containers::paint_split(s, node, cx),
            Widget::Button(b) => buttons::paint_button(b, node, cx),
            Widget::Check(k) => buttons::paint_check(k, node, cx),
            Widget::RadioGroup(r) => buttons::paint_radio_group(r, node, cx),
            Widget::Segmented(s) => buttons::paint_segmented(s, node, cx),
            Widget::TextInput(t) => textinput::paint_input(t, node, cx),
            Widget::Feedback(f) => feedback::paint_feedback(f, node, cx),
            Widget::ColorWheel(w) => colorpicker::paint_wheel(w.clone(), node, cx),
            Widget::TemporalEditor(t) => temporal::paint_editor(t, node, cx),
            Widget::Calendar(c) => temporal::paint_calendar(c, node, cx),
            Widget::Numeric(n) => numeric::paint_numeric(n, node, cx),
            Widget::Choice(ch) => choices::paint_choice(ch, node, cx),
            Widget::MenuBar(b) => {
                if !windowing::menu::has_native_menu_bar() {
                    menus::paint_menu_bar(b, node, cx)
                }
            }
            Widget::Collection(v) => collections::paint_collection(v, node, cx),
            Widget::Tabs(t) => tabs::paint_tabs(t, node, cx),
            Widget::Terminal(t) => crate::terminal::widget::paint_terminal(t, node, cx),
            Widget::PagePreview(p) => crate::print::document::paint_preview(p, node, cx),
            Widget::Chart(v) => crate::chart::widget::paint_chart(v, node, cx),
            Widget::Scene(v) => crate::scene::widget::paint_scene(v, node, cx),
            Widget::ImageView(v) => crate::imageview::widget::paint_viewer(v, node, cx),
            Widget::Media(v) => crate::media::widget::paint_media(v, node, cx),
            Widget::Custom(k) => {
                let draw = &k.draw;
                let ops = draw(node.size(), cx.theme.clone());
                let ox = cx.origin_x;
                let oy = cx.origin_y;
                crate::render::display::draw(&mut cx.canvas, &ops, ox, oy);
                if cx.is_focused(&k.id) {
                    let r = Rect::from_size(node.size());
                    let rad = cx.theme.radius();
                    cx.focus_ring(r, rad)
                }
            }
            _ => paint_children(&self.children(), node, cx),
        }
    }

    /// Handles `ev` at this widget (as target or while bubbling).
    pub fn event(&self, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
        if let Event::Accessible {
            target,
            action,
            value,
            ..
        } = ev
        {
            if cx.disabled {
                return;
            };
            if action == "focus" {
                cx.focus_on(&self.id());
                cx.consume();
                return;
            };
            match self {
                Widget::TextInput(t) => {
                    textinput::input_event(t, node, ev, st, cx);
                    return;
                }
                Widget::TemporalEditor(t) => {
                    temporal::editor_event(t, node, ev, st, cx);
                    return;
                }
                _ => (),
            }
            accessibility_event(self, node, target, action, value, st, cx);
            return;
        }
        match self {
            Widget::Text(l) => display::label_event(l, node, ev, st, cx),
            Widget::Scroll(s) => containers::scroll_event(s, node, ev, st, cx),
            Widget::Split(s) => containers::split_event(s, node, ev, st, cx),
            Widget::Decorated(d) => containers::decorated_event(d, node, ev, st, cx),
            Widget::Button(b) => {
                if *ev == Event::Timer("submenu".to_string()) && !b.menu.is_empty() {
                    menus::hover_timer(&b.menu, &b.id, st, cx)
                } else {
                    buttons::button_event(b, node, ev, st, cx)
                }
            }
            Widget::Check(k) => buttons::check_event(k, node, ev, st, cx),
            Widget::RadioGroup(r) => buttons::radio_group_event(r, node, ev, st, cx),
            Widget::Segmented(s) => buttons::segmented_event(s, node, ev, st, cx),
            Widget::TextInput(t) => textinput::input_event(t, node, ev, st, cx),
            Widget::Feedback(f) => feedback::feedback_event(f, node, ev, st, cx),
            Widget::ColorWheel(w) => colorpicker::wheel_event(w.clone(), node, ev, st, cx),
            Widget::TemporalEditor(t) => temporal::editor_event(t, node, ev, st, cx),
            Widget::Calendar(c) => temporal::calendar_event(c, node, ev, st, cx),
            Widget::Numeric(n) => numeric::numeric_event(n, node, ev, st, cx),
            Widget::Choice(ch) => choices::choice_event(ch, node, ev, st, cx),
            Widget::MenuBar(b) => {
                if windowing::menu::has_native_menu_bar() {
                    return;
                }
                if *ev == Event::Timer("submenu".to_string()) {
                    menus::hover_timer(&menus::bar_menu_items(b, st), &b.id, st, cx)
                } else {
                    menus::menu_bar_event(b, node, ev, st, cx)
                }
            }
            Widget::ContextMenu(m) => {
                if *ev == Event::Timer("submenu".to_string()) {
                    menus::hover_timer(&m.items, &m.id, st, cx)
                } else {
                    menus::context_event(m, node, ev, st, cx)
                }
            }
            Widget::Collection(v) => collections::collection_event(v, node, ev, st, cx),
            Widget::Tabs(t) => tabs::tabs_event(t, node, ev, st, cx),
            Widget::Terminal(t) => crate::terminal::widget::terminal_event(t, node, ev, st, cx),
            Widget::Chart(v) => crate::chart::widget::chart_event(v, node, ev, st, cx),
            Widget::Scene(v) => crate::scene::widget::scene_event(v, node, ev, st, cx),
            Widget::ImageView(v) => crate::imageview::widget::viewer_event(v, node, ev, st, cx),
            Widget::Media(v) => crate::media::widget::media_event(v, node, ev, st, cx),
            Widget::Custom(k) => custom_event(k, ev, cx),
            _ => (),
        }
    }

    /// Paints a popup this widget opened.
    pub fn popup_paint(&self, popup: &PopupRequest, rect: Rect, cx: &mut PaintCx) {
        match self {
            Widget::MenuBar(b) => menus::paint_panel(
                &menus::bar_menu_items(b, &cx.states),
                &b.id,
                popup.level,
                rect,
                cx,
            ),
            Widget::Choice(ch) => choices::choice_popup_paint(ch, rect, cx),
            Widget::ContextMenu(m) => menus::paint_panel(&m.items, &m.id, popup.level, rect, cx),
            Widget::Button(b) => menus::paint_panel(&b.menu, &b.id, popup.level, rect, cx),
            Widget::TextInput(t) => textinput::popup_paint(t, rect, cx),
            Widget::Decorated(d) => containers::tooltip_paint(d, rect, cx),
            Widget::Tabs(t) => tabs::tabs_popup_paint(t, rect, cx),
            _ => (),
        }
    }

    /// The accessibility node of a popup this widget opened, for popups whose
    /// content is not already part of the widget's own node.
    pub fn popup_semantics(
        &self,
        popup: &PopupRequest,
        st: &StateStore,
        registry: &[crate::commands::Command],
    ) -> Option<AccessNode> {
        match self {
            Widget::MenuBar(b) => Some(menus::panel_semantics(
                &menus::bar_menu_items(b, st),
                &b.id,
                popup.level,
                st,
                registry,
            )),
            Widget::ContextMenu(m) => Some(menus::panel_semantics(
                &m.items,
                &m.id,
                popup.level,
                st,
                registry,
            )),
            Widget::Button(b) => {
                if b.menu.is_empty() {
                    None
                } else {
                    Some(menus::panel_semantics(
                        &b.menu,
                        &b.id,
                        popup.level,
                        st,
                        registry,
                    ))
                }
            }
            _ => None,
        }
    }

    /// Handles an event aimed at a popup this widget opened.
    pub fn popup_event(
        &self,
        node: &Node,
        popup: &PopupRequest,
        rect: Rect,
        ev: &Event,
        st: &mut StateStore,
        cx: &mut EventCx,
    ) {
        match self {
            Widget::TextInput(t) => textinput::popup_event(t, rect, ev, st, cx),
            Widget::MenuBar(b) => menus::panel_event(
                &menus::bar_menu_items(b, st),
                &b.id,
                popup.level,
                rect,
                ev,
                st,
                cx,
                Some((b.clone(), node.clone())),
            ),
            Widget::ContextMenu(m) => {
                menus::panel_event(&m.items, &m.id, popup.level, rect, ev, st, cx, None)
            }
            Widget::Button(b) => {
                menus::panel_event(&b.menu, &b.id, popup.level, rect, ev, st, cx, None)
            }
            Widget::Choice(ch) => choices::choice_popup_event(ch, rect, ev, st, cx),
            Widget::Tabs(t) => tabs::tabs_popup_event(t, rect, ev, st, cx),
            _ => (),
        }
    }

    /// This widget's own accessibility node (children are added by the
    /// tree builder). `None` for purely structural widgets.
    pub fn semantics(&self, node: &Node, st: &StateStore, theme: &Theme) -> Option<AccessNode> {
        match self {
            Widget::Text(l) => display::rich_label_semantics(l, node, st, theme),
            Widget::Image(i) => display::image_semantics(i),
            Widget::Icon(i) => display::icon_semantics(i),
            Widget::Badge(b) => display::badge_semantics(b),
            Widget::Separator(_) => Some(AccessNode::new(Role::Separator, "")),
            Widget::Scroll(s) => containers::scroll_semantics(s, node, st),
            Widget::Split(s) => containers::split_semantics(s, node),
            Widget::Decorated(d) => containers::decorated_semantics(d),
            Widget::Button(b) => buttons::button_semantics(b, st),
            Widget::Check(k) => buttons::check_semantics(k),
            Widget::RadioGroup(r) => buttons::radio_group_semantics(r, node),
            Widget::Segmented(s) => buttons::segmented_semantics(s),
            Widget::TextInput(t) => textinput::input_semantics(t, st),
            Widget::Feedback(f) => feedback::feedback_semantics(f),
            Widget::ColorWheel(w) => colorpicker::wheel_semantics(w.clone()),
            Widget::TemporalEditor(t) => temporal::editor_semantics(t, st),
            Widget::Calendar(c) => temporal::calendar_semantics(c, node, st),
            Widget::Numeric(n) => numeric::numeric_semantics(n, st),
            Widget::Choice(ch) => choices::choice_semantics(ch, node, st),
            Widget::MenuBar(b) => {
                if windowing::menu::has_native_menu_bar() {
                    None
                } else {
                    menus::menu_bar_semantics(b, node, st)
                }
            }
            Widget::Collection(v) => collections::collection_semantics(v, node, st, theme),
            Widget::Tabs(t) => tabs::tabs_semantics(t),
            Widget::Terminal(t) => crate::terminal::widget::terminal_semantics(t, node),
            Widget::PagePreview(p) => crate::print::document::preview_semantics(p, node),
            Widget::Chart(v) => crate::chart::widget::chart_semantics(v, node),
            Widget::Scene(v) => crate::scene::widget::scene_semantics(v, node, theme.scale),
            Widget::ImageView(v) => crate::imageview::widget::viewer_semantics(v, node),
            Widget::Media(v) => crate::media::widget::media_semantics(v, node),
            Widget::Custom(k) => {
                let mut n = AccessNode::new(k.role, &k.label);
                n.id = k.id.clone();
                n.focusable = k.focusable;
                Some(n)
            }
            _ => None,
        }
    }
}

/// Paints each child at its node position.
pub fn paint_children(kids: &[&Widget], node: &Node, cx: &mut PaintCx) {
    let ox = cx.origin_x;
    let oy = cx.origin_y;
    let mut i = 0;
    while i < kids.len() && i < node.children.len() {
        let child_node = &node.children[i];
        let r = child_node.rect.offset(ox, oy);
        // Skip children wholly outside the clip.
        if r.intersects(cx.canvas.clip) || r.is_empty() {
            cx.origin_x = r.x;
            cx.origin_y = r.y;
            kids[i].paint(child_node, cx)
        }
        i += 1;
    }
    cx.origin_x = ox;
    cx.origin_y = oy;
}

fn custom_event(k: &Custom, ev: &Event, cx: &mut EventCx) {
    let input = &k.input;
    let lx = cx.origin_x;
    let ly = cx.origin_y;
    let translated = match ev {
        Event::PointerDown {
            x,
            y,
            button,
            clicks,
            mods,
        } => {
            if k.focusable {
                cx.focus_on(&k.id)
            };
            cx.capture_pointer(&k.id);
            Some(CustomInput::Press {
                x: x - lx,
                y: y - ly,
                button: *button,
                clicks: *clicks,
                mods: *mods,
            })
        }
        Event::PointerMove { x, y, pressed, .. } => Some(if *pressed {
            CustomInput::Drag {
                x: x - lx,
                y: y - ly,
            }
        } else {
            CustomInput::Hover {
                x: x - lx,
                y: y - ly,
            }
        }),
        Event::PointerUp { x, y, button, mods } => {
            cx.release_pointer();
            Some(CustomInput::Release {
                x: x - lx,
                y: y - ly,
                button: *button,
                mods: *mods,
            })
        }
        Event::PointerLeave => Some(CustomInput::Leave),
        Event::KeyDown { key, mods, .. } => Some(CustomInput::KeyPress(*key, *mods)),
        Event::KeyUp { key, mods } => Some(CustomInput::KeyRelease(*key, *mods)),
        Event::Text(t) => Some(CustomInput::Text(t.to_string())),
        Event::Scroll { x, y, dx, dy, mods } => Some(CustomInput::Wheel {
            x: x - lx,
            y: y - ly,
            dx: *dx,
            dy: *dy,
            mods: *mods,
        }),
        Event::FocusIn => Some(CustomInput::Focus(true)),
        Event::FocusOut => Some(CustomInput::Focus(false)),
        _ => None,
    };
    if let Some(i) = translated {
        let msgs = input(i);
        for m in &msgs {
            cx.emit(m)
        }
        if !msgs.is_empty()
            || (k.captures_input
                && !matches!(ev, Event::PointerLeave)
                && !matches!(ev, Event::FocusIn)
                && !matches!(ev, Event::FocusOut))
        {
            cx.consume()
        }
    }
}

/// Constraints for a child of natural size inside `c`, minimums dropped.
pub fn loose(c: Constraints) -> Constraints {
    c.loosen()
}

/// The unbounded size marker, re-exported for containers.
pub const NO_LIMIT: i64 = UNBOUNDED;

impl std::fmt::Display for Custom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Custom({})", self.id)
    }
}

impl std::fmt::Debug for Custom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Custom {{ id: {:?} }}", self.id)
    }
}

// Native actions use the same validation and message contracts as local input.
fn accessibility_event(
    w: &Widget,
    node: &Node,
    target: &str,
    action: &str,
    value: &str,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    let id = w.id();
    let prefix = id.clone() + "#";
    let key = if target.starts_with(&prefix) {
        target.substring(prefix.byte_len(), target.byte_len())
    } else {
        ""
    };
    match w {
        Widget::Terminal(t) => {
            if crate::terminal::widget::terminal_action(t, action) {
                cx.redraw = true;
            }
        }
        Widget::Scene(v) => crate::scene::widget::scene_action(v, key, action, cx),
        Widget::Media(v) => {
            if let Some(text) = crate::media::widget::media_action(v, action) {
                cx.announce(&text);
                cx.redraw = true;
            }
        }
        Widget::ImageView(v) => {
            if let Some(text) = crate::imageview::widget::viewer_action(v, node, action) {
                cx.announce(&text);
                cx.redraw = true;
            }
        }
        Widget::Chart(v) => {
            let theme = cx.theme.clone();
            if let Some(text) = crate::chart::widget::chart_action(v, node, action, &theme) {
                cx.announce(&text);
                cx.redraw = true;
            }
        }
        Widget::ColorWheel(wheel) => {
            let (_, s, v) = wheel.value.hsv_parts();
            let h = match action {
                "set-value" => value.to_f64().unwrap_or(wheel.hue),
                "increment" => (wheel.hue + 1.0) % 360.0,
                "decrement" => (wheel.hue + 359.0) % 360.0,
                _ => wheel.hue,
            };
            if (0.0..=360.0).contains(&h) && h != wheel.hue {
                cx.emit(&UiMsg::Custom(id, format!("{}|{}|{}", h, s, v)))
            }
        }
        Widget::Calendar(c) => {
            if action == "press"
                && let Some(date) = temporal::Date::parse(key)
                && date.ordinal() >= c.min.ordinal()
                && date.ordinal() <= c.max.ordinal()
                && date != c.value
            {
                cx.emit(&UiMsg::Date(id, date.iso()))
            }
        }
        Widget::Tabs(t) => {
            for item in &t.items {
                if item.key == key && key != t.active {
                    cx.emit(&UiMsg::Tab(id.clone(), key.to_string()))
                }
            }
        }
        Widget::RadioGroup(r) => {
            for (k, _) in &r.options {
                if k == key && !r.disabled_keys.iter().any(|d| d == key) && key != r.selected {
                    cx.emit(&UiMsg::Choose(id.clone(), key.to_string()))
                }
            }
        }
        Widget::Segmented(s) => {
            for (k, _) in &s.options {
                if k == key && key != s.selected {
                    cx.emit(&UiMsg::Choose(id.clone(), key.to_string()))
                }
            }
        }
        Widget::Collection(c) => collections::accessible(c, node, key, action, st, cx),
        Widget::Numeric(n) => numeric::accessible(n, action, value, cx),
        Widget::Choice(ch) => choices::accessible(ch, key, action, cx),
        Widget::Feedback(f) => feedback::accessible(f, target, action, cx),
        Widget::Button(b) if key == "menu" && action == "press" && !b.menu.is_empty() => w.event(
            node,
            &Event::KeyDown {
                key: windowing::Key::Down,
                mods: windowing::Mods::default(),
                repeat: false,
            },
            st,
            cx,
        ),
        _ => {
            let key = if action == "increment" {
                windowing::Key::Up
            } else if action == "decrement" {
                windowing::Key::Down
            } else {
                windowing::Key::Space
            };
            if action == "press"
                || action == "toggle"
                || action == "activate"
                || action == "increment"
                || action == "decrement"
            {
                w.event(
                    node,
                    &Event::KeyDown {
                        key,
                        mods: windowing::Mods::default(),
                        repeat: false,
                    },
                    st,
                    cx,
                );
                w.event(
                    node,
                    &Event::KeyUp {
                        key,
                        mods: windowing::Mods::default(),
                    },
                    st,
                    cx,
                )
            }
        }
    }
    cx.consume()
}

#[cfg(test)]
mod tests {
    use super::Widget;
    use crate::accessibility::Role;
    use crate::geometry::Rect;
    use crate::geometry::Size;
    use crate::render::Canvas;
    use crate::strext::StrExt;
    use crate::widgets::Custom;
    use crate::widgets::Visibility;
    use crate::widgets::completion::CompletionItem;
    use crate::widgets::cx::UiMsg;
    use crate::widgets::textinput::{InputKind, TextInput};
    use std::collections::HashMap;
    use windowing::{Key, Mods};

    // Ids of the controls the state matrix covers, in view order.
    const CONTROLS: [&str; 12] = [
        "button", "toggle", "check", "switch", "radio", "segment", "link", "field", "spin",
        "slider", "select", "bad",
    ];

    // Mode 0 shows every control enabled, 1 disabled; mode 2 clears the
    // error of the `bad` field.
    fn matrix_view(mode: &i64) -> Widget {
        let items = vec![
            CompletionItem::new("s", "Small"),
            CompletionItem::new("m", "Medium"),
        ];
        let mut bad = TextInput::new("bad", "4x", "Number", InputKind::Line);
        if *mode != 2 {
            bad.error = "Not a number".to_string()
        };
        let controls = vec![
            Widget::button("button", "Button"),
            Widget::toggle_button("toggle", "Toggle", false),
            Widget::checkbox("check", "Check", false),
            Widget::switch("switch", "Switch", false),
            Widget::radio_group(
                "radio",
                &[
                    ("a".to_string(), "Apple".to_string()),
                    ("b".to_string(), "Banana".to_string()),
                ],
                "a",
                true,
            ),
            Widget::segmented(
                "segment",
                &[
                    ("a".to_string(), "Apple".to_string()),
                    ("b".to_string(), "Banana".to_string()),
                ],
                "a",
            ),
            Widget::link("link", "Link"),
            Widget::text_field("field", "Text", ""),
            Widget::spin_box("spin", 3.0, 0.0, 10.0, 1.0, 0),
            Widget::slider("slider", 0.5, 0.0, 1.0, 0.1),
            Widget::select("select", &items, "m", "Size"),
            Widget::input(&bad),
        ];
        Widget::column(
            &controls
                .iter()
                .map(|w| w.clone().disabled(*mode == 1))
                .collect::<Vec<_>>(),
            6,
        )
        .padding(12)
    }

    fn matrix_update(m: i64, msg: UiMsg) -> i64 {
        match msg {
            UiMsg::Click(id) => {
                if id.starts_with("mode:") {
                    id.substring(5, id.byte_len()).to_i64().unwrap_or(m)
                } else {
                    m
                }
            }
            _ => m,
        }
    }

    // The pixels of `r` and a margin for focus rings, as RGB rows.
    fn crop(c: &Canvas, r: Rect) -> Vec<u8> {
        let mut out = vec![];
        let x0 = (r.x - 4).max(0);
        let x1 = (r.x + r.width + 4).min(c.width);
        let mut y = (r.y - 4).max(0);
        while y < (r.y + r.height + 4).min(c.height) {
            let mut x = x0;
            while x < x1 {
                let i = (y * c.width + x) * 4;
                out.push(c.pixels[i as usize]);
                out.push(c.pixels[(i + 1) as usize]);
                out.push(c.pixels[(i + 2) as usize]);
                x += 1;
            }
            y += 1;
        }
        out
    }

    // Where the pointer points at `id`: its first option for option groups.
    fn target(s: &crate::Session, id: &str) -> (i64, i64) {
        let r = s.rect_of(id).unwrap();
        if id == "radio" || id == "segment" {
            (r.x + r.width / 8, r.center_y())
        } else {
            (r.center_x(), r.center_y())
        }
    }

    fn look(s: &mut crate::Session, id: &str) -> Vec<u8> {
        let c = s.capture().unwrap();
        crop(&c, s.rect_of(id).unwrap())
    }

    fn width_with(w: &Widget, theme: &crate::style::Theme) -> i64 {
        let mut lcx = crate::widgets::cx::LayoutCx::new(
            theme,
            &crate::widgets::state::StateStore::new(),
            crate::geometry::Size::new(800, 600),
        );
        w.layout(
            crate::geometry::Constraints::loose(crate::geometry::Size::new(800, 600)),
            &mut lcx,
        )
        .size()
        .width
    }

    #[test]
    fn icon_size_tokens_resize_what_controls_reserve_for_them() {
        let menu = Widget::menu_button("m", "Export", &[]);
        let base = width_with(&menu, &crate::style::Theme::light());
        let mut tokens = crate::style::ThemeTokens::standard();
        tokens.shape.small_icon += 8;
        assert_eq!(
            width_with(&menu, &crate::style::Theme::light().with_tokens(&tokens)),
            base + 8
        )
    }

    // The window-pixel rectangle of `id` in `root` laid out in `width` x `height`.
    fn placed(
        root: &Widget,
        width: i64,
        height: i64,
        rtl: bool,
        id: &str,
    ) -> crate::geometry::Rect {
        let mut theme = crate::style::Theme::light();
        theme.rtl = rtl;
        let mut lcx = crate::widgets::cx::LayoutCx::new(
            &theme,
            &crate::widgets::state::StateStore::new(),
            crate::geometry::Size::new(width, height),
        );
        let node = root.layout(
            crate::geometry::Constraints::tight(crate::geometry::Size::new(width, height)),
            &mut lcx,
        );
        let path = crate::widgets::tree::path_to(root, &node, &theme, id);
        if path.is_empty() {
            crate::geometry::Rect::new(-1, -1, 0, 0)
        } else {
            path[path.len() - 1].rect()
        }
    }

    // A `w` x `h` leaf whose own rectangle the tests read.
    fn block(id: &str, w: i64, h: i64) -> Widget {
        Widget::Custom(Custom {
            id: id.to_string(),
            focusable: false,
            role: Role::Image,
            label: id.to_string(),
            measure: crate::Callback::new(std::rc::Rc::new(move |c, _t| {
                c.constrain(Size::new(w, h))
            })),
            draw: crate::Callback::new(std::rc::Rc::new(move |_s, _t| vec![])),
            input: crate::Callback::new(std::rc::Rc::new(move |_i| vec![])),
            captures_input: false,
        })
    }

    #[test]
    fn a_stack_anchors_layers_by_logical_corner_and_mirrors_in_rtl() {
        let root = Widget::stack(&[
            block("base", 300, 200),
            block("badge", 40, 20).align(1.0, 1.0),
            block("note", 60, 20).align(0.0, 0.0),
        ]);
        let ltr = placed(&root, 300, 200, false, "badge");
        assert_eq!((ltr.x, ltr.y), (260, 180));
        let rtl = placed(&root, 300, 200, true, "badge");
        assert_eq!((rtl.x, rtl.y), (0, 180));
        assert_eq!(placed(&root, 300, 200, true, "note").x, 240)
    }

    #[test]
    fn logical_sides_and_rows_follow_the_direction() {
        let root = Widget::row(&[block("first", 50, 20), block("second", 50, 20)], 10)
            .padding_sides(30, 0, 5, 0);
        assert_eq!(placed(&root, 400, 40, false, "first").x, 30);
        assert_eq!(placed(&root, 400, 40, false, "second").x, 90);
        assert_eq!(placed(&root, 400, 40, true, "first").x, 400 - 30 - 50);
        assert_eq!(
            placed(&root, 400, 40, true, "second").x,
            400 - 30 - 50 - 10 - 50
        );
        // A column stretches its children across; the margin stays at the start side.
        let spaced = Widget::column(&[block("inner", 50, 20).margin_sides(12, 0, 0, 0)], 0);
        assert_eq!(placed(&spaced, 400, 40, false, "inner").x, 12);
        let mirrored = placed(&spaced, 400, 40, true, "inner");
        assert_eq!(mirrored.x + mirrored.width, 400 - 12)
    }

    #[test]
    fn aspect_ratio_and_visibility_shape_the_layout() {
        let wide = Widget::column(
            &[Widget::Empty.aspect(2.0).key("pic"), block("after", 40, 20)],
            0,
        );
        let pic = placed(&wide, 300, 400, false, "pic");
        assert_eq!(pic.height * 2, pic.width);
        let collapsed = Widget::column(
            &[
                block("gone", 40, 30).visibility(Visibility::Collapsed),
                block("next", 40, 20),
            ],
            0,
        );
        assert_eq!(placed(&collapsed, 100, 100, false, "next").y, 0);
        let hidden = Widget::column(
            &[
                block("kept", 40, 30).visibility(Visibility::Hidden),
                block("below", 40, 20),
            ],
            0,
        );
        assert_eq!(placed(&hidden, 100, 100, false, "below").y, 30)
    }

    #[test]
    fn a_flow_wraps_onto_new_lines_from_the_start_edge() {
        let root = Widget::flow(
            &[
                block("a", 100, 20),
                block("b", 100, 20),
                block("c", 100, 20),
            ],
            10,
        );
        assert_eq!(placed(&root, 230, 100, false, "c").y, 30);
        assert_eq!(placed(&root, 230, 100, false, "b").x, 110);
        assert_eq!(placed(&root, 230, 100, true, "a").x, 130);
        assert_eq!(placed(&root, 230, 100, true, "c").x, 130)
    }

    #[test]
    fn a_horizontal_strip_is_as_tall_as_its_content_while_a_list_fills_the_column() {
        // An overflowing strip keeps the scrollbar's track below its content.
        let root = Widget::column(
            &[
                Widget::scroll_horizontal("strip", block("wide", 900, 30)),
                block("below", 40, 20),
            ],
            10,
        );
        assert_eq!(placed(&root, 300, 400, false, "strip").height, 40);
        assert_eq!(placed(&root, 300, 400, false, "strip").width, 300);
        assert_eq!(placed(&root, 300, 400, false, "below").y, 50);
        let fits = Widget::column(
            &[Widget::scroll_horizontal("strip", block("narrow", 200, 30))],
            0,
        );
        assert_eq!(placed(&fits, 300, 400, false, "strip").height, 30);
        let list = Widget::column(
            &[
                Widget::scroll("list", block("tall", 40, 900)).grow(1),
                block("after", 40, 20),
            ],
            0,
        );
        assert_eq!(placed(&list, 300, 400, false, "list").height, 380);
        assert_eq!(placed(&list, 300, 400, false, "list").width, 300)
    }

    #[test]
    fn a_subtree_override_carries_tokens_and_keeps_them_across_its_palette() {
        let mut tokens = crate::style::ThemeTokens::standard();
        tokens.shape.radius = 3;
        tokens.icons = "brand".to_string();
        let change = super::ThemeOverride {
            kind: Some(crate::style::ThemeKind::Dark),
            density: None,
            rtl: None,
            text_scale: 0.0,
            tokens: Some(tokens),
            focus: None,
        };
        let t = change.apply(crate::style::Theme::light());
        assert!(t.is_dark());
        assert_eq!(t.radius(), 3);
        assert_eq!(t.icons, "brand")
    }

    #[test]
    fn every_control_shows_hover_press_focus_disabled_invalid_and_inactive_states() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "States",
                "dev.gossamer.gui-state-matrix",
                || 0,
                matrix_update,
                matrix_view,
            )
            .size(480, 720)
            .start()
            .unwrap();
            s.flush().unwrap();
            let mut rest = HashMap::new();
            for id in CONTROLS {
                rest.insert(id, look(&mut s, id));
            }
            // Keyboard focus shows on every control.
            s.press(Key::Tab, Mods::default()).unwrap();
            for id in CONTROLS {
                assert!(s.focus(id).unwrap(), "{} takes focus", id);
                assert!(look(&mut s, id) != rest[id], "{}: focus does not show", id)
            }
            // Hover and press differ from rest and from each other; a release
            // away from the control cancels the press.
            for id in CONTROLS {
                if id == "field" || id == "bad" || id == "spin" {
                    continue;
                };
                s.hover(1, 1).unwrap();
                let (x, y) = target(&s, id);
                s.hover(x, y).unwrap();
                let hovered = look(&mut s, id);
                s.press_pointer(x, y).unwrap();
                let pressed = look(&mut s, id);
                s.move_pointer(1, 1, true).unwrap();
                s.release_pointer(1, 1).unwrap();
                assert!(hovered != rest[id], "{}: hover does not show", id);
                assert!(pressed != hovered, "{}: press does not show", id)
            }
            s.press(Key::Escape, Mods::default()).unwrap();
            // A field marks its error beyond color: the text says what is wrong.
            let bad = s.semantics(0).find("bad").unwrap();
            assert!(
                bad.invalid && bad.description == "Not a number",
                "{} {}",
                bad.invalid,
                bad.description
            );
            let invalid = look(&mut s, "bad");
            s.emit(&UiMsg::Click("mode:2".to_string()));
            s.flush().unwrap();
            assert!(
                look(&mut s, "bad") != invalid,
                "an invalid field looks like a valid one"
            );
            // An inactive window dims the selection and hides the caret.
            assert!(s.focus("field").unwrap());
            let primary = Mods {
                ctrl: true,
                ..Mods::default()
            };
            s.press(Key::A, primary).unwrap();
            let active = look(&mut s, "field");
            assert!(
                s.semantics(0).active,
                "the active window's node lacks the active state"
            );
            s.set_active(false).unwrap();
            assert!(
                look(&mut s, "field") != active,
                "an inactive window paints the selection as active"
            );
            assert!(
                !s.semantics(0).active,
                "an inactive window's node keeps the active state"
            );
            s.set_active(true).unwrap();
            assert!(
                s.semantics(0).active,
                "activation does not restore the active state"
            );
            // Disabled controls look different, ignore the pointer, and skip focus.
            s.emit(&UiMsg::Click("mode:1".to_string()));
            s.flush().unwrap();
            for id in CONTROLS {
                let off = look(&mut s, id);
                assert!(off != rest[id], "{}: disabled looks enabled", id);
                let (x, y) = target(&s, id);
                s.hover(x, y).unwrap();
                assert!(
                    look(&mut s, id) == off,
                    "{}: a disabled control shows hover",
                    id
                );
                assert!(
                    !s.focus(id).unwrap(),
                    "{}: a disabled control takes focus",
                    id
                )
            }
            s.shutdown()
        })
    }
}
