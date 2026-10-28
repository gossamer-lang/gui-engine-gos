// cx: the contexts widgets work in. A frame runs three passes over the
// application's widget tree: layout (constraints down, sizes up, into a
// `Node` tree), paint (into the window canvas), and, per input event,
// dispatch (target first, then each ancestor until one handles it).
// Widgets never touch a native window: they ask the context for focus,
// pointer capture, popups, timers, IME placement, and messages, and the
// session applies those requests after the pass.

use windowing;
use windowing::{Key, Mods, MouseButton};

use crate::commands::Command;
use crate::dialogs::Outcome;
use crate::geometry::{Constraints, Rect, Size};
use crate::render::Canvas;
use crate::style::Theme;
use crate::style::color::Color;
use crate::widgets::state::StateStore;

/// The laid-out form of one widget: its rectangle relative to its
/// parent's origin, its children's nodes in `Widget::children` order, and
/// widget-specific sub-rectangles (`parts`) relative to its own origin.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Node {
    pub rect: Rect,
    pub children: Vec<Node>,
    /// Distance from the top to the first baseline, or -1.
    pub baseline: i64,
    /// Content extent for scrolling widgets.
    pub content: Size,
    pub parts: Vec<Rect>,
}

impl Default for Node {
    fn default() -> Node {
        Node {
            rect: Rect::new(0, 0, 0, 0),
            children: vec![],
            baseline: -1,
            content: Size::new(0, 0),
            parts: vec![],
        }
    }
}

impl Node {
    pub fn leaf(size: Size) -> Node {
        Node {
            rect: Rect::from_size(size),
            children: vec![],
            baseline: -1,
            content: size,
            parts: vec![],
        }
    }

    pub fn size(&self) -> Size {
        self.rect.size()
    }

    /// This node placed at (`x`, `y`) in its parent.
    pub fn at(&self, x: i64, y: i64) -> Node {
        let mut n = self.clone();
        n.rect.x = x;
        n.rect.y = y;
        n
    }
}

/// What layout reads: the theme and widget state (scroll offsets), and
/// which widgets asked for animation frames.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutCx {
    pub theme: Theme,
    pub states: StateStore,
    /// The window's size, for widgets that size to it.
    pub window: Size,
    pub animating: Vec<String>,
}

impl LayoutCx {
    pub fn new(theme: &Theme, states: &StateStore, window: Size) -> LayoutCx {
        LayoutCx {
            theme: theme.clone(),
            states: states.clone(),
            window,
            animating: vec![],
        }
    }

    /// Asks for repaints while this widget stays laid out.
    pub fn animate(&mut self, id: &str) {
        self.animating.push(id.to_string())
    }
}

/// Input as widgets see it. Coordinates are window pixels.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Event {
    PointerDown {
        x: i64,
        y: i64,
        button: MouseButton,
        clicks: i64,
        mods: Mods,
    },
    PointerUp {
        x: i64,
        y: i64,
        button: MouseButton,
        mods: Mods,
    },
    PointerMove {
        x: i64,
        y: i64,
        pressed: bool,
        mods: Mods,
    },
    PointerLeave,
    Scroll {
        x: i64,
        y: i64,
        dx: f64,
        dy: f64,
        mods: Mods,
    },
    KeyDown {
        key: Key,
        mods: Mods,
        repeat: bool,
    },
    KeyUp {
        key: Key,
        mods: Mods,
    },
    /// Committed text (typed, chosen in an input method, or pasted).
    Text(String),
    /// Input-method preedit; `cursor`/`selection` are character offsets.
    Composing {
        text: String,
        cursor: i64,
        selection: i64,
    },
    FocusIn,
    FocusOut,
    /// A timer the widget started fired, with the tag it gave.
    Timer(String),
    /// A command offered to the focused path before its default handler.
    Command(String),
    /// Files or text dropped at (x, y).
    Drop {
        x: i64,
        y: i64,
        paths: Vec<String>,
        text: String,
    },
    /// The pointer capture this widget held was taken away.
    CaptureLost,
    /// A validated native accessibility request, delivered on the UI thread.
    Accessible {
        target: String,
        action: String,
        value: String,
        start: i64,
        end: i64,
    },
    /// The application selected UTF-8 bytes `anchor..caret`.
    SetSelection {
        anchor: i64,
        caret: i64,
    },
    /// The application replaced UTF-8 bytes `start..end` with `text` as one edit.
    ReplaceRange {
        start: i64,
        end: i64,
        text: String,
    },
}

impl Event {
    /// The event's pointer position, if it has one.
    pub fn position(&self) -> Option<(i64, i64)> {
        match self {
            Event::PointerDown { x, y, .. }
            | Event::PointerUp { x, y, .. }
            | Event::PointerMove { x, y, .. }
            | Event::Scroll { x, y, .. }
            | Event::Drop { x, y, .. } => Some((*x, *y)),
            _ => None,
        }
    }
}

/// A tri-state checkbox value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CheckState {
    #[default]
    Unchecked,
    Checked,
    Mixed,
}

/// The result of background work started with `Effect::Task`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TaskOutcome {
    Done(String),
    Failed(String),
    Cancelled,
}

/// What the toolkit tells the application happened. Ids are the widget
/// ids the view was built with; item keys are the stable keys of model
/// items, never positions.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum UiMsg {
    RichLayoutReady(String),
    RichLink(String, String),
    RichChanged(String, i64, i64),
    /// A tab was dropped on a group of dock `id`, beside an edge or among its tabs.
    Dock(String, crate::dock::Drop),
    /// Media player `id` changed state or moved on while playing.
    Media(i64, crate::media::Status),
    /// Image viewer `id` finished, failed or cancelled a load.
    Image(i64, crate::imageview::Status),
    /// The user edited scene widget `id` or changed its selection.
    Scene(String, crate::scene::Info),
    /// A point of chart widget `id` was clicked.
    ChartPoint(String, crate::chart::Hit),
    /// Print or export job `id` progressed, finished, failed or was cancelled.
    Print(i64, crate::print::job::Event),
    /// Terminal `id`'s program set a title, rang the bell, or ended.
    Terminal(i64, crate::terminal::Event),
    /// A button, link, or menu item was activated.
    Click(String),
    /// A checkbox, switch, or toggle button changed.
    Toggle(String, bool),
    /// A tri-state checkbox changed.
    Check(String, CheckState),
    /// A radio group (or segmented control) chose the item `key`.
    Choose(String, String),
    /// A text input's text changed.
    Text(String, String),
    /// A text input's selection moved: id, anchor and caret as UTF-8 byte
    /// offsets on grapheme boundaries (equal when nothing is selected).
    Caret(String, i64, i64),
    /// Enter was pressed in a single-line text input.
    Submit(String, String),
    /// A single-selection control selected `key` ("" for none).
    Select(String, String),
    /// A multi-selection control's selected keys, in selection order.
    SelectMany(String, Vec<String>),
    /// A numeric control's value.
    Value(String, f64),
    /// A range control's low and high values.
    Range(String, f64, f64),
    /// A color control's color.
    Color(String, Color),
    /// A date control's ISO-8601 date (`YYYY-MM-DD`), time (`HH:MM:SS`),
    /// or duration in seconds as text.
    Date(String, String),
    /// A tab container switched to the tab `key`.
    Tab(String, String),
    /// A tab strip asked for an action on the tab `key` ("" for none):
    /// "new", "reopen", "close", "close-others", "close-right", "pin",
    /// "unpin", "move-left", "move-right", or "detach".
    TabAction(String, String, String),
    /// A captured pointer drop in source-window pixels, resolved by the session.
    TabDrop(String, String, i64, i64),
    /// A page dropped into another compatible strip at an insertion index.
    TabMove(String, String, String, i64),
    /// A detached page and its requested logical desktop placement.
    TabDetach(String, String, i64, i64),
    /// A tree item or expander opened (`true`) or closed.
    Expand(String, String, bool),
    /// A collection item was activated (double-click or Enter).
    Activate(String, String),
    /// A table cell edit was committed: id, row key, column key, text.
    CellEdit(String, String, String, String),
    /// A table header asked to sort by `column`, ascending when true.
    Sort(String, String, bool),
    /// A table's column widths changed: id, column key, new width units.
    ColumnResize(String, String, i64),
    /// Items were reordered by dragging: id, moved key, new index.
    Reorder(String, String, i64),
    /// A split pane's divider moved to `ratio`.
    Split(String, f64),
    /// A command fired from a menu, toolbar, palette, or shortcut.
    Command(String),
    /// A drag started by `Effect::StartDrag` ended: its id and the operation
    /// the drop target performed, or None when it was dropped nowhere or the
    /// platform could not start it.
    DragFinished(String, Option<windowing::DragOperation>),
    /// The desktop asked the application to open `path`: a file opened with
    /// it, or the files of a later launch of a single-instance application.
    Open(String),
    OpenUri(String),
    DocumentChanged(String, i64, i64),
    WindowCommand(String, String),
    QuitRequested,
    /// A dialog answered, exactly once.
    Dialog(String, Outcome),
    /// Reversible live preview; distinct from the terminal dialog outcome.
    DialogPreview(String, Outcome),
    /// An explicit Apply committed a new baseline without closing.
    DialogApplied(String, Outcome),
    /// A checked dialog asks before a step ("apply", "ok", "next" or
    /// "finish") takes effect: dialog id, request, step, and the values;
    /// answer with `Effect::DialogChecked`.
    DialogCheck(String, i64, String, Outcome),
    /// The user stopped waiting for a check: dialog id and request.
    DialogCheckStopped(String, i64),
    /// Background work finished.
    Task(String, TaskOutcome),
    /// A revisioned file, analysis or completion job result.
    Worked(String, i64, i64, crate::services::work::Outcome),
    /// Answers `Effect::ListDirectory`: owner, request, and what was found.
    Listed(String, i64, crate::services::listing::Listing),
    /// Answers `Effect::LoadFonts`: the installed-font catalog, for
    /// `text::fonts::adopt_installed`.
    FontsLoaded(String, Vec<crate::text::catalog::FontRecord>),
    FontReady(String),
    LayoutReady(String),
    /// Answers `Effect::LoadApps`: the installed applications, by name.
    AppsLoaded(String, Vec<crate::services::apps::AppEntry>),
    /// An application timer fired.
    Timer(String),
    /// The user asked to close the window; the application decides.
    CloseRequested(String),
    /// Files were dropped on the widget `id`.
    Dropped(String, Vec<String>),
    /// A completion provider of `id` needs results for `query`
    /// (request id, query, revision); answer with `Effect::Complete`.
    CompletionRequest(String, i64, String, i64),
    /// A custom widget's message.
    Custom(String, String),
}

/// Where a popup sits relative to its anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Placement {
    Below,
    Above,
    /// To the trailing side (submenus).
    Beside,
    /// With its top-left at the anchor's top-left (context menus).
    AtPoint,
    /// Centred in the window.
    Center,
}

/// A popup a widget asks to open. The owner paints it and handles its
/// events (`Widget::popup_paint` / `popup_event`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PopupRequest {
    pub owner: String,
    /// Distinguishes several popups of one owner (submenu depth).
    pub level: i64,
    pub anchor: Rect,
    pub size: Size,
    pub placement: Placement,
    /// Whether keyboard input goes to the popup before the focused widget.
    pub takes_keys: bool,
    /// Whether a click outside dismisses it.
    pub dismiss_outside: bool,
    /// Whether a press on the owner only closes it (a select's list), rather
    /// than also reaching the owner (a text field placing its caret).
    pub closes_on_owner_press: bool,
}

impl PopupRequest {
    pub fn new(
        owner: &str,
        level: i64,
        anchor: Rect,
        size: Size,
        placement: Placement,
        takes_keys: bool,
        dismiss_outside: bool,
    ) -> PopupRequest {
        PopupRequest {
            owner: owner.to_string(),
            level,
            anchor,
            size,
            placement,
            takes_keys,
            dismiss_outside,
            closes_on_owner_press: false,
        }
    }

    /// This request, closed by a press on its owner.
    pub fn closed_by_owner_press(self) -> PopupRequest {
        let mut r = self;
        r.closes_on_owner_press = true;
        r
    }
}

/// A timer request: deliver `Event::Timer(tag)` to `owner` after `delay` ms.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TimerRequest {
    pub owner: String,
    pub tag: String,
    pub delay: i64,
}

/// The requests and messages one dispatch pass collects.
#[derive(Debug, Clone, PartialEq)]
pub struct EventCx {
    pub theme: Theme,
    pub messages: Vec<UiMsg>,
    /// The focused widget id when the pass began.
    pub focus: String,
    pub request_focus: Option<String>,
    pub capture: Option<String>,
    pub release_capture: bool,
    pub handled: bool,
    pub redraw: bool,
    /// The pass moved something layout positions (a scroll offset), so the
    /// window lays out again before it paints and hit-tests.
    pub relayout: bool,
    pub open_popups: Vec<PopupRequest>,
    pub close_popups: Vec<(String, i64)>,
    pub timers: Vec<TimerRequest>,
    pub cancel_timers: Vec<(String, String)>,
    pub cursor: Option<windowing::SystemCursor>,
    /// A drag source asks for a platform drag: its id, items and operation.
    pub start_drag: Option<(String, Vec<windowing::DragItem>, windowing::DragOperation)>,
    /// The caret's line in the focused editor (window pixels) and the caret's
    /// pixel offset from that line's left edge.
    pub ime: Option<(Rect, i64)>,
    pub announcements: Vec<String>,
    /// Commands to run after the pass.
    pub run: Vec<String>,
    pub now: i64,
    pub window: Rect,
    /// Whether an ancestor is disabled.
    pub disabled: bool,
    /// The absolute origin of the node being dispatched to.
    pub origin_x: i64,
    pub origin_y: i64,
    /// The window is active (has keyboard focus).
    pub active: bool,
    /// The popup currently receiving the event, when dispatching to one.
    pub popup_level: i64,
    /// The window's commands, for enabled state and labels.
    pub registry: Vec<Command>,
}

impl EventCx {
    pub fn new(theme: &Theme, focus: &str, now: i64, window: Rect) -> EventCx {
        EventCx {
            theme: theme.clone(),
            messages: vec![],
            focus: focus.to_string(),
            request_focus: None,
            capture: None,
            release_capture: false,
            handled: false,
            redraw: false,
            relayout: false,
            open_popups: vec![],
            close_popups: vec![],
            timers: vec![],
            cancel_timers: vec![],
            cursor: None,
            start_drag: None,
            ime: None,
            announcements: vec![],
            run: vec![],
            now,
            window,
            disabled: false,
            origin_x: 0,
            origin_y: 0,
            active: true,
            popup_level: -1,
            registry: vec![],
        }
    }

    pub fn emit(&mut self, msg: &UiMsg) {
        self.messages.push(msg.clone());
        self.redraw = true;
    }

    /// Marks the event consumed so it does not reach ancestors.
    pub fn consume(&mut self) {
        self.handled = true;
    }

    pub fn focus_on(&mut self, id: &str) {
        self.request_focus = Some(id.to_string());
        self.redraw = true;
    }

    pub fn capture_pointer(&mut self, id: &str) {
        self.capture = Some(id.to_string());
    }

    pub fn release_pointer(&mut self) {
        self.release_capture = true;
    }

    pub fn open_popup(&mut self, request: &PopupRequest) {
        self.open_popups.push(request.clone());
        self.redraw = true;
    }

    pub fn close_popup(&mut self, owner: &str, level: i64) {
        self.close_popups.push((owner.to_string(), level));
        self.redraw = true;
    }

    pub fn start_timer(&mut self, owner: &str, tag: &str, delay: i64) {
        self.timers.push(TimerRequest {
            owner: owner.to_string(),
            tag: tag.to_string(),
            delay,
        })
    }

    pub fn cancel_timer(&mut self, owner: &str, tag: &str) {
        self.cancel_timers
            .push((owner.to_string(), tag.to_string()))
    }

    /// Asks assistive technology to speak `text` politely.
    pub fn announce(&mut self, text: &str) {
        self.announcements.push(text.to_string())
    }

    pub fn run_command(&mut self, id: &str) {
        self.run.push(id.to_string())
    }

    pub fn is_focused(&self, id: &str) -> bool {
        !id.is_empty() && self.focus == id
    }

    /// The rectangle `local` (relative to the current node) in window pixels.
    pub fn to_window(&self, local: Rect) -> Rect {
        local.offset(self.origin_x, self.origin_y)
    }
}

/// What painting reads and writes.
#[derive(Debug, Clone, PartialEq)]
pub struct PaintCx {
    pub canvas: Canvas,
    pub theme: Theme,
    pub states: StateStore,
    pub focus: String,
    pub hover: String,
    pub pressed: String,
    /// Show focus rings (keyboard navigation happened since the last click).
    pub focus_visible: bool,
    pub active: bool,
    pub disabled: bool,
    /// The caret blink phase: carets draw only while true.
    pub caret_on: bool,
    pub now: i64,
    pub origin_x: i64,
    pub origin_y: i64,
    /// The window's commands, for labels, shortcuts, and enabled state.
    pub registry: Vec<Command>,
    /// Show mnemonic underlines (Alt is held or menus are keyboard-driven).
    pub mnemonics: bool,
}

impl PaintCx {
    pub fn new(canvas: &Canvas, theme: &Theme, states: &StateStore) -> PaintCx {
        PaintCx {
            canvas: canvas.clone(),
            theme: theme.clone(),
            states: states.clone(),
            focus: String::new(),
            hover: String::new(),
            pressed: String::new(),
            focus_visible: true,
            active: true,
            disabled: false,
            caret_on: true,
            now: 0,
            origin_x: 0,
            origin_y: 0,
            registry: vec![],
            mnemonics: false,
        }
    }

    pub fn is_focused(&self, id: &str) -> bool {
        !id.is_empty() && self.focus == id
    }

    pub fn is_hover(&self, id: &str) -> bool {
        !id.is_empty() && self.hover == id && !self.disabled
    }

    pub fn is_pressed(&self, id: &str) -> bool {
        !id.is_empty() && self.pressed == id && !self.disabled
    }

    /// A local rectangle in canvas pixels.
    pub fn abs(&self, local: Rect) -> Rect {
        local.offset(self.origin_x, self.origin_y)
    }

    /// Draws `icon` in the `size`-pixel square at canvas pixels (`x`, `y`),
    /// from the theme's icon set when it has the icon.
    pub fn icon(
        &mut self,
        icon: &crate::render::icons::Icon,
        x: i64,
        y: i64,
        size: i64,
        color: Color,
    ) {
        crate::render::icons::draw_from(
            &mut self.canvas,
            &self.theme.icons,
            icon,
            x,
            y,
            size,
            color,
        )
    }

    /// Fits decoded application pixels into an absolute rectangle, preserving aspect ratio.
    pub fn app_icon(&mut self, image: &crate::services::apps::AppIcon, rect: Rect) {
        if image.width <= 0 || image.height <= 0 {
            return;
        }
        let scale =
            (rect.width as f64 / image.width as f64).min(rect.height as f64 / image.height as f64);
        let width = (image.width as f64 * scale).round() as i64;
        let height = (image.height as f64 * scale).round() as i64;
        self.canvas.image_scaled(
            rect.x + (rect.width - width) / 2,
            rect.y + (rect.height - height) / 2,
            width,
            height,
            &image.rgba,
            image.width,
            image.height,
        );
    }

    /// Draws the focus ring around `local` when focus should show.
    pub fn focus_ring(&mut self, local: Rect, radius: i64) {
        if !self.focus_visible {
            return;
        }
        let r = self.abs(local);
        let w = self.theme.focus_width();
        let color = self.theme.focus;
        self.canvas.round_rect_stroke(
            r.x - w,
            r.y - w,
            r.width + w * 2,
            r.height + w * 2,
            radius + w,
            w,
            color,
        )
    }

    /// Fills `local` (relative to the current origin).
    pub fn fill(&mut self, local: Rect, color: Color) {
        let r = self.abs(local);
        self.canvas.rect(r.x, r.y, r.width, r.height, color)
    }

    pub fn fill_round(&mut self, local: Rect, radius: i64, color: Color) {
        let r = self.abs(local);
        self.canvas
            .round_rect(r.x, r.y, r.width, r.height, radius, color)
    }

    pub fn stroke_round(&mut self, local: Rect, radius: i64, width: i64, color: Color) {
        let r = self.abs(local);
        self.canvas
            .round_rect_stroke(r.x, r.y, r.width, r.height, radius, width, color)
    }
}

/// Layout helper: constraints for a child that may take up to `size`.
pub fn up_to(size: Size) -> Constraints {
    Constraints::loose(size)
}

/// The command `id` among `commands`, if registered.
pub fn command(commands: &[Command], id: &str) -> Option<Command> {
    for c in commands {
        if c.id == id {
            return Some(c.clone());
        }
    }
    None
}

/// `ev` with its pointer position made local to the widget at `cx`'s origin.
pub(crate) fn local_event(ev: &Event, cx: &EventCx) -> Event {
    let (ox, oy) = (cx.origin_x, cx.origin_y);
    match ev {
        Event::PointerDown {
            x,
            y,
            button,
            clicks,
            mods,
        } => Event::PointerDown {
            x: x - ox,
            y: y - oy,
            button: *button,
            clicks: *clicks,
            mods: *mods,
        },
        Event::PointerUp { x, y, button, mods } => Event::PointerUp {
            x: x - ox,
            y: y - oy,
            button: *button,
            mods: *mods,
        },
        Event::PointerMove {
            x,
            y,
            pressed,
            mods,
        } => Event::PointerMove {
            x: x - ox,
            y: y - oy,
            pressed: *pressed,
            mods: *mods,
        },
        Event::Scroll { x, y, dx, dy, mods } => Event::Scroll {
            x: x - ox,
            y: y - oy,
            dx: *dx,
            dy: *dy,
            mods: *mods,
        },
        other => other.clone(),
    }
}
