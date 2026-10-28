pub mod effects;
pub mod mailbox;

// app: the toolkit session. One windowing App, the windows on it, and the
// event loop:
//
// 1. Wait for an event (`wait`, or `wait_timeout` until the next timer;
//    an idle session with no timers sleeps without polling).
// 2. Convert it, then dispatch: popups first, then the top modal dialog,
//    then the window tree; keys go to the focused widget and bubble, then
//    to commands, then to focus traversal.
// 3. Apply what the dispatch asked for (focus, capture, popups, timers,
//    IME placement), fold the messages into the application's model,
//    apply its effects, rebuild changed windows, and repaint dirty ones.
//
// Work on other goroutines posts messages through a `Mailbox`; posts
// wake the loop and arrive like widget messages. Native window calls stay
// on this thread.

use crate::strext::StrExt;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::time::Instant;

use crate::errors;

use windowing;
use windowing::{Key, Mods, MouseButton};

use crate::accessibility::AccessNode;
use crate::app::effects::Effect;
use crate::app::mailbox::{Inbox, Mailbox};
use crate::commands;
use crate::commands::{Command, CommandTable, KeyResult};
use crate::dialogs::Outcome;
use crate::dialogs::host;
use crate::dialogs::request::{DialogRequest, Modality};
use crate::geometry::{Constraints, Rect, Size, UNBOUNDED};
use crate::render::Canvas;
use crate::render::presenter::{Painted, WindowRenderer};
use crate::services;
use crate::style::Theme;
use crate::style::color::Color;
use crate::widgets::Widget;
use crate::widgets::containers;
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx, Placement, PopupRequest, UiMsg};
use crate::widgets::menus;
use crate::widgets::popup;
use crate::widgets::state::StateStore;
use crate::widgets::textinput;
use crate::widgets::tree;
use crate::widgets::tree::PathEntry;

/// A popup open in a window, with its placed rectangle.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OpenPopup {
    pub request: PopupRequest,
    pub rect: Rect,
}

/// What closing a window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ClosePolicy {
    /// Close it (the last window ends the session).
    Close,
    /// Report `UiMsg::CloseRequested(key)` and let the application decide.
    Ask,
}

/// How a window looks and where it opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WindowStyle {
    /// No title bar or border, as a run box or a splash screen has.
    pub frameless: bool,
    /// Opens centered along the top of the primary display's usable area
    /// rather than where it last was. Wayland places every window itself.
    pub top_center: bool,
    /// Closes on Escape, ahead of any popup, and when it stops being the
    /// active window.
    pub dismissible: bool,
}

/// One open window.
#[derive(Clone, PartialEq)]
pub struct WindowState {
    pub key: String,
    pub native: windowing::Window,
    pub title: String,
    /// What the window shows: `view`, with the inspector beside it while
    /// that is open. Layout, painting, input and semantics all use this.
    pub root: Widget,
    /// The application's view, with the toasts over it.
    pub view: Widget,
    /// Whether `node` is the layout of the view with the inspector beside it.
    pub node_inspected: bool,
    pub node: Node,
    pub states: StateStore,
    pub theme: Theme,
    pub focus: String,
    pub focus_visible: bool,
    /// The latest input was a key rather than the pointer. Focus the
    /// toolkit moves on its own (into an opening dialog) shows its ring
    /// only then.
    pub keyboard_input: bool,
    /// Focus to restore as popups and dialogs close.
    pub popup_focus: Option<String>,
    pub focus_stack: Vec<String>,
    pub hover: Vec<String>,
    pub pressed: String,
    pub capture: String,
    pub popups: Vec<OpenPopup>,
    /// The control each toolbar last had focused, by toolbar key.
    pub roving: HashMap<String, String>,
    /// Toasts shown over the window, oldest first.
    pub toasts: Vec<crate::widgets::feedback::Toast>,
    pub dialogs: Vec<host::Instance>,
    /// The focus each layer last had when F6 left it: "" keys the window's
    /// own content, a dialog id its dialog.
    pub layer_focus: HashMap<String, String>,
    pub commands: CommandTable,
    pub dirty: bool,
    pub layout_dirty: bool,
    pub caret_on: bool,
    pub active: bool,
    pub close_policy: ClosePolicy,
    /// The key of the window a dialog window belongs to; "" for the
    /// application's own windows.
    pub owner: String,
    pub semantics: AccessNode,
    pub frames: i64,
    /// Mnemonic underlines show (Alt held, or a menu opened from the keyboard).
    pub mnemonics: bool,
    /// Why the window was last laid out again, newest last, for the inspector.
    pub layout_causes: Vec<String>,
    /// The height, in the platform's logical units, of a window that grows
    /// for its popups (`Session::grow_for_popups`) while none shows; 0 for a
    /// window of fixed height.
    pub grow_base: i64,
    /// The pixel height a growing window last asked for.
    pub grow_target: i64,
    /// A growing window's view height in pixels when not held to the window.
    pub content_height: i64,
    /// Escape and losing activation close it (`WindowStyle::dismissible`).
    pub dismissible: bool,
    /// Whether the system has given it keyboard focus since it opened.
    pub focused_once: bool,
    pub occluded: bool,
}

/// A pending timer.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Timer {
    deadline: i64,
    window: String,
    owner: String,
    tag: String,
    repeat: i64,
}

/// The application's commands for the window with the key it is given.
pub type WindowCommands = Box<dyn Fn(&str) -> Vec<Command>>;

/// The application half of the Elm shape.
pub struct Shell {
    pub update: Box<dyn FnMut(UiMsg)>,
    pub view: Box<dyn Fn(&str) -> Widget>,
    pub commands: WindowCommands,
}

// The key prefix of the window a file dialog opens in, followed by the key
// of the window it belongs to.
const FILE_DIALOG_WINDOW: &str = "gui:file-dialog:";
const SHORTCUT_DIALOG_WINDOW: &str = "gui:shortcut-dialog:";

// The size a file dialog's window opens at, in interface units, until the
// user resizes one, and the smallest it can be.
const FILE_DIALOG_SIZE: (i64, i64) = (900, 620);
const FILE_DIALOG_MIN: (i64, i64) = (560, 420);

/// A running toolkit session.
type DisposedCallback = Box<dyn FnMut(&str)>;

pub struct Session {
    app: windowing::App,
    memory_limit: usize,
    budget: crate::memory::Budget,
    auxiliary: Option<crate::memory::Lease>,
    descriptions: HashMap<String, crate::memory::Lease>,
    frame_leases: HashMap<String, crate::memory::Lease>,
    pending_leases: Vec<crate::memory::Lease>,
    resource_error: String,
    renderers: HashMap<String, WindowRenderer>,
    pub windows: Vec<WindowState>,
    pub app_id: String,
    pub name: String,
    pub base_theme: Theme,
    pub follow_system: bool,
    /// The application set the text size or the motion preference itself,
    /// so the desktop's no longer applies.
    explicit_text_scale: bool,
    explicit_motion: bool,
    /// Where background jobs (`Effect::ListDirectory`, `LoadFonts`) go while
    /// `run` keeps a worker; without one they are answered at the next settle.
    jobs: Option<crate::services::jobs::Worker>,
    /// Whether jobs asked for before `run` starts its worker wait for it
    /// (`held_jobs`) instead of being answered on the event loop's thread.
    holds_jobs: bool,
    inline_work: bool,
    held_jobs: Vec<(crate::services::jobs::Job, crate::memory::Lease)>,
    /// The display scale `Effect::SetDisplayScale` draws every window at,
    /// in place of each display's own.
    scale_override: Option<f64>,
    shell: Option<Shell>,
    disposed: Option<DisposedCallback>,
    timers: Vec<Timer>,
    inbox: Inbox,
    pending: Vec<UiMsg>,
    clock_start: Instant,
    // Milliseconds `advance` moved the session clock ahead of the real one,
    // and the clock's value while an advance holds it still.
    clock_skew: i64,
    clock_held: Option<i64>,
    quit: bool,
    held: Option<windowing::Event>,
    tab_owners: HashMap<(String, String), (String, Vec<String>)>,
    native_dialogs: HashMap<i64, (String, String)>,
    extra_commands: Vec<Command>,
    /// Recent input and messages, newest last, for the inspector.
    pub log: Vec<String>,
    /// Mistakes in the application's views already reported, so each is
    /// reported once.
    warned: HashSet<String>,
    /// Announcements made for assistive technology, newest last.
    pub announcements: Vec<String>,
    pub inspector: bool,
    in_run: bool,
    last_click: (i64, i64, i64, i64),
    a11y: crate::accessibility::bridge::Bridge,
    /// Announcements waiting for the current announcement interval to end,
    /// oldest first, and the last one spoken.
    unspoken: Vec<String>,
    last_spoken: String,
    /// The system cursor last asked for, and its platform cursor (kept
    /// alive while it shows) when the platform could make one.
    cursor_kind: Option<windowing::SystemCursor>,
    cursor: Option<windowing::cursor::Cursor>,
    /// The tray icon `Effect::SetTray` showed.
    tray: Option<services::desktop::TrayHost>,
    /// The menus installed in the platform's menu bar and the command each
    /// item identity (index + 1) runs.
    native_menus: Option<(Vec<windowing::Menu>, Vec<String>)>,
    /// Application ids of the outbound drags in flight, by drag id.
    drags: HashMap<u64, String>,
    /// Whether `shutdown` has run, which it does once.
    shut_down: bool,
    /// The single-instance claim this session serves.
    instance: Option<windowing::instance::Listener>,
}

// A session released without `shutdown` (an early return, a panic) still
// saves its windows and gives the platform session back.
impl Drop for Session {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.shell = None;
            self.disposed = None;
        }
        self.shutdown()
    }
}

pub const DEFAULT_SESSION_BYTE_LIMIT: usize = 1024 * 1024 * 1024;
pub const WINDOW_DESCRIPTION_BYTE_LIMIT: usize = 256 * 1024 * 1024;
pub const FRAME_BYTE_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ResourceUsage {
    pub descriptions: usize,
    pub states: usize,
    pub rendering: usize,
    pub jobs: usize,
    pub messages: usize,
    pub other: usize,
    pub total: usize,
    pub limit: usize,
}

const LOG_LIMIT: i64 = 400;

const FRAME_MS: i64 = 33;
// How long a held toast waits before its timeout is looked at again.
const TOAST_RECHECK_MS: i64 = 1000;
// Assistive technology hears at most one announcement per interval, so a
// burst of status changes cannot drown out speech for focus and caret moves.
const ANNOUNCE_INTERVAL_MS: i64 = 750;
// Announcements a burst may leave waiting; older ones are stale by then.
const ANNOUNCE_BACKLOG: i64 = 3;
const ANNOUNCE_TAG: &str = "gui:announce";
const MEDIA_TAG: &str = "gui:media-tick";
// Owner of the toolkit's own timers that belong to no window, kept apart
// from the application's (no window, no owner) so its tags cannot collide.
const TOOLKIT_OWNER: &str = "gui";

// Tests drive sessions without a display: a window on a desktop would take
// that desktop's pointer and focus changes as input. `SDL_VIDEO_DRIVER` still
// overrides it.
#[cfg(test)]
const VIDEO_DRIVER: &str = "offscreen";
#[cfg(not(test))]
const VIDEO_DRIVER: &str = "";
// Tests see the same theme on every desktop.
#[cfg(test)]
const FOLLOW_DESKTOP_PREFERENCES: bool = false;
#[cfg(not(test))]
const FOLLOW_DESKTOP_PREFERENCES: bool = true;

/// Starts the toolkit: one windowing session for the process.
pub fn start(name: &str, app_id: &str) -> Result<Session, errors::Error> {
    #[cfg(test)]
    assert!(
        crate::testing::is_ui_thread(),
        "a session under test runs inside crate::testing::on_ui_thread"
    );
    let options = windowing::InitOptions {
        app_id: app_id.to_string(),
        driver: VIDEO_DRIVER.to_string(),
        ..Default::default()
    };
    let app = windowing::init(name, &options)?;
    // A session follows the desktop's appearance until the application
    // picks a theme; a desktop that states no light/dark preference is light.
    let theme = Theme::light();
    let follow = true;
    let budget = crate::memory::Budget::new(DEFAULT_SESSION_BYTE_LIMIT);
    let auxiliary = budget.reserve(64 * 1024 * 1024).map_err(errors::new)?;
    let inbox = Inbox::with_budget(budget.clone());
    effects::set_budget(budget.clone());
    let mut session = Session {
        app,
        memory_limit: DEFAULT_SESSION_BYTE_LIMIT,
        budget,
        auxiliary: Some(auxiliary),
        descriptions: HashMap::new(),
        frame_leases: HashMap::new(),
        pending_leases: Vec::new(),
        resource_error: String::new(),
        renderers: HashMap::new(),
        windows: vec![],
        app_id: app_id.to_string(),
        name: name.to_string(),
        base_theme: theme,
        follow_system: follow,
        explicit_text_scale: false,
        explicit_motion: false,
        jobs: None,
        holds_jobs: false,
        inline_work: cfg!(test)
            || std::env::var("GUI_TEST_INLINE_WORK").is_ok_and(|value| value == "1"),
        held_jobs: vec![],
        scale_override: None,
        shell: None,
        disposed: None,
        timers: vec![],
        inbox,
        pending: vec![],
        clock_start: Instant::now(),
        clock_skew: 0,
        clock_held: None,
        quit: false,
        held: None,
        tab_owners: HashMap::new(),
        native_dialogs: HashMap::new(),
        extra_commands: vec![],
        log: vec![],
        warned: HashSet::new(),
        announcements: vec![],
        inspector: false,
        in_run: false,
        last_click: (0, 0, 0, 0),
        a11y: crate::accessibility::bridge::Bridge::new(app_id),
        unspoken: vec![],
        last_spoken: String::new(),
        cursor_kind: None,
        cursor: None,
        tray: None,
        native_menus: None,
        drags: HashMap::new(),
        instance: None,
        shut_down: false,
    };
    effects::set_waker(Some(session.app.waker()));
    effects::set_mailbox(Some(session.mailbox()));
    session.apply_system_preferences();
    Ok(session)
}

// Pixel coordinates from the window's logical event coordinates.
fn pixel_coordinate(value: f64, logical: i64, pixels: i64) -> i64 {
    (value * pixels as f64 / logical.max(1) as f64).floor() as i64
}

// `theme` with the desktop's preferences: while the session `follow`s the
// system, its light/dark (`dark`) and contrast settings pick the palette and
// its accent colors it; its motion and text size apply unless the
// application chose them.
fn with_preferences(
    theme: Theme,
    prefs: windowing::system::Preferences,
    dark: Option<bool>,
    follow: bool,
    explicit_text_scale: bool,
    explicit_motion: bool,
) -> Theme {
    let mut t = theme;
    if follow {
        let dark = dark.unwrap_or(t.is_dark());
        let high = prefs.high_contrast.unwrap_or(false);
        let kind = match (dark, high) {
            (true, true) => crate::style::ThemeKind::HighContrastDark,
            (false, true) => crate::style::ThemeKind::HighContrastLight,
            (true, false) => crate::style::ThemeKind::Dark,
            (false, false) => crate::style::ThemeKind::Light,
        };
        t = t.with_kind(kind);
        // High-contrast palettes keep their own accents.
        if let Some((r, g, b)) = prefs.accent
            && !high
        {
            t = t.with_accent(Color::rgb(r, g, b))
        }
    }
    if !explicit_motion && let Some(reduced) = prefs.reduced_motion {
        t.reduced_motion = reduced
    };
    if !explicit_text_scale && let Some(f) = prefs.text_scale {
        t.text_scale = f.clamp(0.5, 3.0)
    };
    if let Some(points) = prefs.menu_font_size.filter(|p| (4.0..=72.0).contains(p)) {
        t.typography = t.typography.with_menu_points(points)
    }
    t
}

// Logical units from pixels, for native calls.
fn logical_coordinate(value: i64, logical: i64, pixels: i64) -> i64 {
    (value as f64 * logical as f64 / pixels.max(1) as f64).round() as i64
}

impl Session {
    // ---- setup ----

    /// Registers the application's update, view, and command functions.
    pub fn set_shell(
        &mut self,
        update: impl FnMut(UiMsg) + 'static,
        view: impl Fn(&str) -> Widget + 'static,
        commands: impl Fn(&str) -> Vec<Command> + 'static,
    ) {
        self.shell = Some(Shell {
            update: Box::new(update),
            view: Box::new(view),
            commands: Box::new(commands),
        });
    }

    /// Milliseconds since the session started.
    pub fn now(&self) -> i64 {
        match self.clock_held {
            Some(held) => held,
            None => self.clock_start.elapsed().as_millis() as i64 + self.clock_skew,
        }
    }

    /// Opens a window named `key` showing what the view builds for it.
    /// Sizes are interface units; the window scales with its display.
    pub fn open_window(
        &mut self,
        key: &str,
        title: &str,
        width: i64,
        height: i64,
    ) -> Result<(), errors::Error> {
        let existed = self.find(key) >= 0;
        self.prepare_window(key, title, width, height)?;
        if let Err(error) = self.show_window(key) {
            if !existed {
                let window = self.find(key);
                if window >= 0 {
                    self.close_window(window);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    /// Opens window `key` hidden, at its scaled size and saved placement,
    /// so nothing set before `show_window` (a minimum size, growth for
    /// popups) shows as a jump.
    pub fn prepare_window(
        &mut self,
        key: &str,
        title: &str,
        width: i64,
        height: i64,
    ) -> Result<(), errors::Error> {
        self.prepare_window_with(key, title, width, height, WindowStyle::default())
    }

    /// `prepare_window` for a window of `style`.
    pub fn prepare_window_with(
        &mut self,
        key: &str,
        title: &str,
        width: i64,
        height: i64,
        style: WindowStyle,
    ) -> Result<(), errors::Error> {
        self.prepare_window_as(key, title, width, height, style, None)
    }

    // Creates window `key` hidden, modal to `parent` when there is one.
    fn prepare_window_as(
        &mut self,
        key: &str,
        title: &str,
        width: i64,
        height: i64,
        style: WindowStyle,
        parent: Option<windowing::Window>,
    ) -> Result<(), errors::Error> {
        if self.find(key) >= 0 {
            return Ok(());
        }
        if self.windows.len() >= 32 || key.len() > 4096 || title.len() > 64 * 1024 {
            return Err(errors::new(
                "window description exceeds its resource budget",
            ));
        }
        self.check_memory()?;
        // The system places a window when it first shows it, so it opens
        // hidden at its final size and position and that is its only
        // placement. Interface units are the platform's logical units at the
        // display's content scale.
        let placement = if style.top_center {
            None
        } else {
            services::load_window_placement(&self.app_id, key)
        };
        let (units_w, units_h) = match placement {
            Some(p) => (p.width, p.height),
            None => (width, height),
        };
        let display = windowing::display::primary().ok();
        let content = display
            .as_ref()
            .map(|d| d.scale)
            .unwrap_or(1.0)
            .clamp(0.5, 4.0);
        let mut options = windowing::WindowOptions {
            title: title.to_string(),
            width: (units_w as f64 * content).round() as i64,
            height: (units_h as f64 * content).round() as i64,
            resizable: true,
            borderless: style.frameless,
            hidden: true,
            kind: if parent.is_some() {
                windowing::WindowKind::Modal
            } else {
                windowing::WindowKind::Normal
            },
            parent,
            ..windowing::WindowOptions::default()
        };
        if let Some(d) = display.filter(|_| style.top_center) {
            options.x = Some(d.usable.x + (d.usable.width - options.width) / 2);
            options.y = Some(d.usable.y);
        }
        if let Some(p) = placement {
            options.x = Some(p.x);
            options.y = Some(p.y);
            options.maximized = p.maximized;
        }
        let native = self.app.window_with(&options)?;
        let scale = self
            .scale_override
            .unwrap_or(native.scale())
            .clamp(0.5, 4.0);
        let mut theme = self.base_theme.clone();
        theme.scale = scale;
        // A window that opened on a display of another scale, or drawn at an
        // overridden scale, takes its size there before it shows.
        let (lw, lh) = native.size();
        let (pw, ph) = native.pixel_size();
        let want_w = window_extent(theme.px(units_w), lw, pw);
        let want_h = window_extent(theme.px(units_h), lh, ph);
        if want_w != lw || want_h != lh {
            native.set_size(want_w, want_h)?;
            native.sync()?
        }
        let root = self.build_view(key);
        let charge = crate::memory::measure(&root).saturating_mul(3);
        if charge > WINDOW_DESCRIPTION_BYTE_LIMIT.saturating_mul(3) {
            native.close();
            if let Some(callback) = &mut self.disposed {
                callback(key);
            }
            return Err(errors::new("window view exceeds its byte budget"));
        }
        let lease = match self.budget.reserve(charge) {
            Ok(lease) => lease,
            Err(error) => {
                native.close();
                if let Some(callback) = &mut self.disposed {
                    callback(key);
                }
                return Err(errors::new(error));
            }
        };
        self.descriptions.insert(key.to_owned(), lease);
        let mut root = root.clone();
        root.compact_maps();
        self.windows.push(WindowState {
            key: key.to_string(),
            native,
            title: title.to_string(),
            root: root.clone(),
            view: root,
            node_inspected: false,
            node: Node::default(),
            states: StateStore::with_budget(self.budget.clone()),
            theme: theme.clone(),
            focus: String::new(),
            focus_visible: false,
            keyboard_input: false,
            popup_focus: None,
            focus_stack: vec![],
            hover: vec![],
            pressed: String::new(),
            capture: String::new(),
            popups: vec![],
            roving: HashMap::new(),
            toasts: vec![],
            dialogs: vec![],
            layer_focus: HashMap::new(),
            commands: CommandTable::new(&[]),
            dirty: true,
            layout_dirty: true,
            caret_on: true,
            active: true,
            close_policy: ClosePolicy::Close,
            owner: String::new(),
            semantics: AccessNode::new(crate::accessibility::Role::TopLevel, title),
            frames: 0,
            mnemonics: false,
            layout_causes: vec![],
            grow_base: 0,
            grow_target: 0,
            content_height: 0,
            dismissible: style.dismissible,
            focused_once: false,
            occluded: false,
        });
        let w = self.windows.len() - 1;
        self.refresh_commands(w as i64);
        self.mark_tree(w as i64);
        self.frame_theme(w as i64);
        self.render(w as i64)?;
        self.a11y.window_opened(key, title, self.windows[w].native);
        Ok(())
    }

    /// Shows window `key` with its current frame drawn, so it appears whole.
    pub fn show_window(&mut self, key: &str) -> Result<(), errors::Error> {
        let w = self.find(key);
        if w < 0 {
            return Ok(());
        };
        if self.windows[w as usize].dirty || self.windows[w as usize].layout_dirty {
            self.render(w)?
        };
        Ok(self.windows[w as usize].native.show()?)
    }

    /// The application's icon, for every window open now and opened later.
    /// Where the window system shows no icon per window (a Wayland shell
    /// without the toplevel icon protocol), the shell shows the icon of the
    /// desktop entry the application id names.
    pub fn set_icon(&mut self, icon: &services::apps::AppIcon) -> Result<(), errors::Error> {
        Ok(self.app.set_icon(&icon.rgba, icon.width, icon.height)?)
    }

    /// Sets a window's minimum content size in interface units.
    pub fn set_min_size(
        &mut self,
        key: &str,
        width: i64,
        height: i64,
    ) -> Result<(), errors::Error> {
        let w = self.find(key);
        if w < 0 {
            return Ok(());
        };
        let win = self.windows[w as usize].native;
        let (lw, lh) = win.size();
        let (pw, ph) = win.pixel_size();
        let t = &self.windows[w as usize].theme;
        Ok(win.set_min_size(
            window_extent(t.px(width), lw, pw),
            window_extent(t.px(height), lh, ph),
        )?)
    }

    /// Makes window `key` `height` interface units tall and lets it grow
    /// downward while a popup below its anchor needs the room (a run box's
    /// suggestions), back to `height` once none does. The user no longer
    /// resizes it.
    pub fn grow_for_popups(&mut self, key: &str, height: i64) -> Result<(), errors::Error> {
        let w = self.find(key);
        if w < 0 {
            return Ok(());
        };
        let win = self.windows[w as usize].native;
        win.set_resizable(false)?;
        let (lw, lh) = win.size();
        let (_, ph) = win.pixel_size();
        let base = window_extent(self.windows[w as usize].theme.px(height), lh, ph);
        win.set_size(lw, base)?;
        self.windows[w as usize].grow_base = base;
        self.windows[w as usize].grow_target = 0;
        self.invalidate(w, "window grows for popups");
        Ok(())
    }

    pub fn set_close_policy(&mut self, key: &str, policy: ClosePolicy) {
        let w = self.find(key);
        if w >= 0 {
            self.windows[w as usize].close_policy = policy
        }
    }

    fn find(&self, key: &str) -> i64 {
        let mut i = 0;
        while i < self.windows.len() {
            if self.windows[i].key == key {
                return i as i64;
            };
            i += 1;
        }
        -1
    }

    fn find_native(&self, id: windowing::WindowId) -> i64 {
        let mut i = 0;
        while i < self.windows.len() {
            if self.windows[i].native.id() == id {
                return i as i64;
            };
            i += 1;
        }
        -1
    }

    fn build_view(&self, key: &str) -> Widget {
        if key.starts_with(FILE_DIALOG_WINDOW) || key.starts_with(SHORTCUT_DIALOG_WINDOW) {
            return Widget::Empty;
        }
        match self.shell {
            Some(ref s) => {
                let view = &s.view;
                view(key)
            }
            None => Widget::Empty,
        }
    }

    /// Conservative owned-storage charges, including all open windows. Process
    /// text/font caches have their own independent hard aggregate caps.
    pub fn resource_usage(&self) -> ResourceUsage {
        let mut usage = ResourceUsage {
            limit: self.memory_limit,
            ..Default::default()
        };
        for window in &self.windows {
            usage.descriptions = usage.descriptions.saturating_add(crate::memory::measure(&(
                &window.root,
                &window.view,
                &window.node,
                &window.dialogs,
                &window.toasts,
                &window.semantics,
                &window.commands,
            )));
            usage.states = usage.states.saturating_add(window.states.owned_bytes());
            if let Some(renderer) = self.renderers.get(&window.key) {
                let retained = &renderer.retained;
                usage.rendering = usage
                    .rendering
                    .saturating_add(retained.stats.scene_bytes)
                    .saturating_add(retained.stats.layer_bytes)
                    .saturating_add(retained.stats.gpu_layer_bytes);
            }
        }
        usage.jobs = self
            .jobs
            .as_ref()
            .map_or(0, |jobs| jobs.usage().1)
            .saturating_add(
                self.held_jobs
                    .iter()
                    .map(|(job, _)| job.bytes().unwrap_or(usize::MAX))
                    .fold(0, usize::saturating_add),
            );
        usage.messages = self
            .inbox
            .usage()
            .1
            .saturating_add(crate::memory::measure(&self.pending));
        usage.other = crate::memory::measure(&(
            &self.timers,
            &self.tab_owners,
            &self.native_dialogs,
            &self.extra_commands,
            &self.log,
            &self.warned,
            &self.announcements,
            &self.unspoken,
            &self.last_spoken,
            &self.drags,
        ));
        usage.total = self.budget.usage().0;
        usage
    }

    pub fn set_memory_limit(&mut self, bytes: usize) -> Result<(), errors::Error> {
        self.sync_auxiliary()?;
        if self.resource_usage().total > bytes {
            return Err(errors::new(
                "session already exceeds the requested byte budget",
            ));
        }
        self.budget.set_limit(bytes).map_err(errors::new)?;
        self.memory_limit = bytes;
        Ok(())
    }

    fn sync_auxiliary(&mut self) -> Result<(), errors::Error> {
        let mut bytes = crate::memory::measure(&(
            &self.timers,
            &self.tab_owners,
            &self.native_dialogs,
            &self.extra_commands,
            &self.log,
            &self.warned,
            &self.announcements,
            &self.unspoken,
            &self.last_spoken,
            &self.drags,
        ));
        for window in &self.windows {
            bytes = bytes.saturating_add(crate::memory::measure(&(
                &window.dialogs,
                &window.toasts,
                &window.commands,
                &window.roving,
                &window.layer_focus,
                &window.focus_stack,
                &window.hover,
            )));
        }
        if bytes > 128 * 1024 * 1024 {
            return Err(errors::new("session metadata exceeds its byte budget"));
        }
        if let Some(lease) = &mut self.auxiliary {
            lease.resize(bytes).map_err(errors::new)?;
        }
        Ok(())
    }

    fn check_memory(&mut self) -> Result<(), errors::Error> {
        self.sync_auxiliary()?;
        if !self.resource_error.is_empty() {
            return Err(errors::new(std::mem::take(&mut self.resource_error)));
        }
        for window in &mut self.windows {
            if let Some(error) = window.states.take_error() {
                return Err(errors::new(error));
            }
        }
        let usage = self.resource_usage();
        if usage.total > usage.limit {
            return Err(errors::new(format!(
                "session byte budget exceeded: {} > {}",
                usage.total, usage.limit
            )));
        }
        Ok(())
    }

    pub fn on_window_disposed(&mut self, callback: impl FnMut(&str) + 'static) {
        self.disposed = Some(Box::new(callback));
    }

    // ---- loop ----

    /// Runs until the session ends, then saves window placements and shuts
    /// the platform down. Workers posting to a `Mailbox` must end before
    /// this returns (run the session inside their cohort).
    pub fn run(&mut self) -> Result<(), errors::Error> {
        if self.jobs.is_none() {
            let worker = crate::services::jobs::Worker::new(self.mailbox())
                .map_err(|e| errors::wrap(e, "starting the job worker"))?;
            self.attach_worker(worker);
        }
        self.in_run = true;
        self.a11y.watch(self.app.waker());
        let result = loop {
            match self.step() {
                Ok(true) => {}
                Ok(false) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        // Cancel queued and in-flight work before joining the worker.
        self.jobs = None;
        self.in_run = false;
        self.shutdown();
        result
    }

    /// A mailbox other goroutines post messages through.
    pub fn mailbox(&self) -> Mailbox {
        self.inbox.mailbox(self.app.waker())
    }

    /// Handles everything that is ready, then waits for and handles one
    /// event. Answers false when the session should end.
    pub fn step(&mut self) -> Result<bool, errors::Error> {
        self.settle()?;
        if self.quit || self.windows.is_empty() {
            return Ok(false);
        }
        let ev = self.next_event()?;
        if let Some(e) = ev {
            self.handle(&e)?;
            // A bounded batch of already-queued events before repainting.
            let mut n = 0;
            while n < 64 && self.held.is_none() {
                match self.app.poll() {
                    Some(e2) => {
                        if is_motion(&e2) {
                            self.held = Some(e2);
                            break;
                        }
                        self.handle(&e2)?
                    }
                    None => break,
                }
                n += 1;
            }
        }
        self.fire_timers()?;
        self.settle()?;
        Ok(!self.quit && !self.windows.is_empty())
    }

    // Applies messages, effects, task outcomes, rebuilds, and repaints
    // until nothing more is pending.
    fn settle(&mut self) -> Result<(), errors::Error> {
        self.check_memory()?;
        let mut rounds = 0;
        loop {
            if crate::text::fonts::take_catalog_request() {
                self.run_job(&services::jobs::Job::Fonts("gui:font-catalog".into()));
            }
            for (record, bold) in crate::text::fonts::take_face_requests() {
                use std::hash::{Hash, Hasher};
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                (&record.path, record.index, bold).hash(&mut hash);
                self.run_job(&services::jobs::Job::FontFace(
                    format!("gui:font-face:{:x}", hash.finish()),
                    record,
                    bold,
                ));
            }
            for request in crate::rich::layout_cache::take_requests(&self.budget) {
                self.run_job(&services::jobs::Job::RichLayout(request));
            }
            for request in crate::text::document_layout::take_requests() {
                self.run_job(&services::jobs::Job::DocumentLayout(request));
            }
            for request in crate::text::document_syntax::take_requests() {
                self.run_job(&services::jobs::Job::SyntaxLines(request));
            }
            self.drain_inbox();
            let (terminals_changed, terminal_events) = crate::terminal::pump(&self.budget);
            let (images_changed, image_events) = crate::imageview::pump(&self.budget);
            let (media_changed, media_events) = crate::media::pump(&self.budget);
            self.pending.extend(media_events);
            self.schedule_media_tick();
            if terminals_changed | images_changed | media_changed | crate::chart::pump(&self.budget)
            {
                for window in &mut self.windows {
                    window.dirty = true;
                }
            }
            self.pending.extend(terminal_events);
            self.pending.extend(image_events);
            self.poll_accessibility();
            let had = !self.pending.is_empty() || effects::pending() > 0;
            self.deliver_messages();
            self.apply_effects()?;
            self.check_memory()?;
            rounds += 1;
            if (!had && self.pending.is_empty() && effects::pending() == 0) || rounds > 16 {
                break;
            }
        }
        self.reconcile_tabs()?;
        self.check_memory()?;
        let mut w = 0;
        while w < self.windows.len() {
            self.mark_tree(w as i64);
            self.fit_growing_window(w as i64);
            if self.windows[w].dirty || self.windows[w].layout_dirty {
                self.render(w as i64)?;
                // Rendering measures the view; a growing window fits it now.
                self.fit_growing_window(w as i64);
                if self.windows[w].dirty {
                    self.render(w as i64)?
                }
            }
            w += 1;
        }
        for request in crate::rich::layout_cache::take_requests(&self.budget) {
            self.run_job(&services::jobs::Job::RichLayout(request));
        }
        for request in crate::text::document_layout::take_requests() {
            self.run_job(&services::jobs::Job::DocumentLayout(request));
        }
        for request in crate::text::document_syntax::take_requests() {
            self.run_job(&services::jobs::Job::SyntaxLines(request));
        }
        Ok(())
    }

    // The next event, waiting no longer than the next timer. Runs of
    // pointer motion collapse into their last position.
    fn next_event(&mut self) -> Result<Option<windowing::Event>, errors::Error> {
        let held = self.held.take();
        let first = match held {
            Some(e) => e,
            None => {
                let deadline = self.next_deadline();
                // A session stepped from outside `run` has no bus watch, so it
                // looks at the accessibility bus each frame.
                let polled = self.a11y.active() && !self.a11y.watched();
                if deadline < 0 && !polled {
                    self.app.wait()?
                } else {
                    let wait = if polled {
                        if deadline < 0 {
                            16
                        } else {
                            (deadline - self.now()).clamp(0, 16)
                        }
                    } else {
                        (deadline - self.now()).max(0)
                    };
                    match self.app.wait_timeout(wait)? {
                        Some(e) => e,
                        None => return Ok(None),
                    }
                }
            }
        };
        let windowing::Event::PointerMoved {
            window,
            x,
            y,
            dx,
            dy,
            buttons,
            source,
        } = first
        else {
            return Ok(Some(first));
        };
        let mut lx = x;
        let mut ly = y;
        let mut tdx = dx;
        let mut tdy = dy;
        while let Some(next) = self.app.poll() {
            match next {
                windowing::Event::PointerMoved {
                    window: w2,
                    x: nx,
                    y: ny,
                    dx: ndx,
                    dy: ndy,
                    buttons: b2,
                    source: s2,
                } if w2 == window && b2 == buttons && s2 == source => {
                    lx = nx;
                    ly = ny;
                    tdx += ndx;
                    tdy += ndy;
                }
                other => {
                    self.held = Some(other);
                    break;
                }
            }
        }
        Ok(Some(windowing::Event::PointerMoved {
            window,
            x: lx,
            y: ly,
            dx: tdx,
            dy: tdy,
            buttons,
            source,
        }))
    }

    fn visual_timer_paused(&self, timer: &Timer) -> bool {
        matches!(timer.tag.as_str(), "gui:frame" | "gui:caret")
            && self
                .windows
                .iter()
                .any(|w| w.key == timer.window && w.occluded)
    }

    fn next_deadline(&self) -> i64 {
        let mut best = -1;
        for t in &self.timers {
            if self.visual_timer_paused(t) {
                continue;
            }
            if best < 0 || t.deadline < best {
                best = t.deadline
            }
        }
        best
    }

    /// Saves placements, closes windows, and ends the platform session.
    pub fn shutdown(&mut self) {
        if std::mem::replace(&mut self.shut_down, true) {
            return;
        }
        self.jobs = None;
        self.held_jobs = vec![];
        self.inbox.close();
        if let Some(ref t) = self.tray {
            t.close()
        };
        self.tray = None;
        if let Some(listener) = self.instance.take() {
            listener.close()
        }
        self.renderers.clear();
        let mut warnings = vec![];
        for win in &self.windows {
            for d in &win.dialogs {
                if let Some(value) = crate::dialogs::pickers::rollback(d, Outcome::Disposed) {
                    self.pending.push(UiMsg::DialogPreview(d.id.clone(), value))
                };
                self.pending
                    .push(UiMsg::Dialog(d.id.clone(), Outcome::Disposed))
            }
            if let Err(e) =
                services::save_window_placement(&self.app_id, &win.key, placement_of(win))
            {
                warnings.push(format!(
                    "could not save where window \"{}\" was: {}",
                    win.key, e
                ))
            };
            win.native.close()
        }
        for w in warnings {
            self.warn(&w);
        }
        for (_, id) in self.native_dialogs.values() {
            self.pending
                .push(UiMsg::Dialog(id.to_string(), Outcome::Disposed))
        }
        self.native_dialogs = HashMap::new();
        self.deliver_messages();
        let _ = effects::take();
        self.quit = true;
        self.timers = vec![];
        self.tab_owners = HashMap::new();
        self.shell = None;
        let keys: Vec<_> = self
            .windows
            .iter()
            .map(|window| window.key.clone())
            .collect();
        self.windows = vec![];
        if let Some(callback) = &mut self.disposed {
            for key in keys {
                callback(&key);
            }
        }
        self.disposed = None;
        self.descriptions.clear();
        self.frame_leases.clear();
        crate::editing::documents::close_budget(&self.budget);
        crate::rich::registry::close_budget(&self.budget);
        crate::rich::layout_cache::close_budget(&self.budget);
        crate::terminal::close_budget(&self.budget);
        crate::print::job::close_budget(&self.budget);
        crate::print::document::close_budget(&self.budget);
        crate::chart::close_budget(&self.budget);
        crate::scene::close_budget(&self.budget);
        crate::imageview::close_budget(&self.budget);
        crate::media::close_budget(&self.budget);
        effects::set_waker(None);
        effects::set_mailbox(None);
        crate::text::document_layout::purge();
        crate::text::document_syntax::purge();
        self.pending_leases.clear();
        self.auxiliary = None;
        self.cursor = None;
        self.cursor_kind = None;
        self.native_menus = None;
        self.drags = HashMap::new();
        self.extra_commands = Vec::new();
        self.log = Vec::new();
        self.warned = HashSet::new();
        self.announcements = Vec::new();
        self.unspoken = Vec::new();
        self.last_spoken = String::new();
        self.a11y.shutdown();
        crate::text::fonts::end_session();
        crate::text::layout::purge_caches();
        self.renderers.clear();
        effects::set_budget(crate::memory::Budget::new(effects::EFFECT_BYTE_LIMIT));
        self.app.finalize()
    }

    /// Wakes the event loop from a worker goroutine.
    pub fn waker(&self) -> windowing::Waker {
        self.app.waker()
    }

    // ---- events ----

    // Reports a mistake in the application (a duplicate id, an unknown
    // command) or a failure it cannot act on (saving a window's placement)
    // on stderr and in the inspector's log, once.
    fn warn(&mut self, text: &str) {
        if self.warned.contains(text) {
            return;
        };
        if crate::memory::measure(&self.warned).saturating_add(text.len()) > 1024 * 1024 {
            self.warned = HashSet::new();
        }
        self.warned.insert(text.to_string());
        self.log_line(&format!("warning: {}", text));
        eprintln!("gui: {}", text)
    }

    // Each id names one widget: two widgets sharing one share their state,
    // their messages, and their place in the focus order, so the second can
    // never be focused.
    fn check_unique_ids(&mut self, w: i64) {
        let mut seen = HashSet::new();
        let key = self.windows[w as usize].key.clone();
        for id in tree::all_ids(&self.windows[w as usize].root) {
            if seen.contains(&id) {
                self.warn(&format!("window \"{}\" has two widgets with the id \"{}\"; give each its own id or `.key(..)`", key, id))
            } else {
                seen.insert(id);
            }
        }
    }

    // A menu entry or command button names a command the window declares;
    // one that names none could never run, and menus show it by its id.
    fn check_named_commands(&mut self, w: i64) {
        let mut named = vec![];
        named_commands(&self.windows[w as usize].root, &mut named);
        if named.is_empty() {
            return;
        };
        let declared = self.windows[w as usize]
            .commands
            .commands
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>();
        let key = self.windows[w as usize].key.clone();
        for id in named {
            if !declared.contains(&id) {
                self.warn(&format!(
                    "window \"{}\" names the command \"{}\", which no command declares",
                    key, id
                ))
            }
        }
    }

    fn log_line(&mut self, line: &str) {
        let mut end = line.len().min(4096);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        self.log.push(line[..end].to_owned());
        if self.log.len() > LOG_LIMIT as usize {
            self.log = self.log[self.log.len() - LOG_LIMIT as usize..self.log.len()].to_vec();
        }
    }

    fn pointer(&self, w: i64, x: f64, y: f64) -> (i64, i64) {
        let win = self.windows[w as usize].native;
        let (lw, lh) = win.size();
        let (pw, ph) = win.pixel_size();
        (pixel_coordinate(x, lw, pw), pixel_coordinate(y, lh, ph))
    }

    /// Handles one platform event.
    pub fn handle(&mut self, e: &windowing::Event) -> Result<(), errors::Error> {
        match e {
            windowing::Event::QuitRequested => self.request_quit(),
            windowing::Event::Wake => {
                self.drain_inbox();
                self.serve_instance_requests()
            }
            windowing::Event::MenuSelected { source, item } => self.menu_selected(*source, *item),
            windowing::Event::DragFinished { drag, operation } => {
                let raw: u64 = (*drag).into();
                if let Some(id) = self.drags.remove(&raw) {
                    self.pending.push(UiMsg::DragFinished(id, *operation))
                }
            }
            windowing::Event::OpenRequested { path } => {
                self.pending.push(UiMsg::Open(path.to_string()))
            }
            windowing::Event::LowMemory => {
                crate::text::layout::purge_caches();
                for win in &mut self.windows {
                    win.states.purge_caches();
                }
                for win in &mut self.windows {
                    if let Some(renderer) = self.renderers.get_mut(&win.key) {
                        // A software surface keeps its pixels; a dropped GPU
                        // target leaves the window blank until it repaints.
                        let blank = renderer.gpu_name().is_some();
                        renderer.purge();
                        win.dirty |= blank;
                    }
                }
            }
            windowing::Event::CloseRequested { window } => {
                let w = self.find_native(*window);
                if w >= 0 && self.blocked(w) {
                    let _ = self.windows[self.blocker(w) as usize].native.focus();
                } else if w >= 0 {
                    self.close_requested(w)
                }
            }
            windowing::Event::KeyDown {
                window,
                key,
                mods,
                repeat,
                ..
            } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    self.key_down(w, *key, *mods, *repeat)?
                }
            }
            windowing::Event::KeyUp {
                window, key, mods, ..
            } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    self.key_up(w, *key, *mods)
                }
            }
            windowing::Event::Text { window, text } => {
                let w = self.find_native(*window);
                if w >= 0 && !self.blocked(w) {
                    self.send_to_focus(w, &Event::Text(text.to_string()))
                }
            }
            windowing::Event::Composing {
                window,
                text,
                cursor,
                selection,
            } => {
                let w = self.find_native(*window);
                if w >= 0 && !self.blocked(w) {
                    self.send_to_focus(
                        w,
                        &Event::Composing {
                            text: text.to_string(),
                            cursor: *cursor,
                            selection: *selection,
                        },
                    )
                }
            }
            windowing::Event::ButtonDown {
                window,
                button,
                x,
                y,
                clicks,
                ..
            } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    let (px, py) = self.pointer(w, *x, *y);
                    self.pointer_down(w, px, py, *button, *clicks, self.app.mods())
                }
            }
            windowing::Event::ButtonUp {
                window,
                button,
                x,
                y,
                ..
            } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    let (px, py) = self.pointer(w, *x, *y);
                    self.pointer_up(w, px, py, *button, self.app.mods())
                }
            }
            windowing::Event::PointerMoved {
                window,
                x,
                y,
                buttons,
                ..
            } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    let (px, py) = self.pointer(w, *x, *y);
                    self.pointer_move(w, px, py, buttons.left, self.app.mods())
                }
            }
            windowing::Event::PointerLeft { window } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    self.pointer_left(w)
                }
            }
            windowing::Event::Wheel {
                window,
                dx,
                dy,
                x,
                y,
            } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    let (px, py) = self.pointer(w, *x, *y);
                    self.wheel(w, px, py, *dx, *dy, self.app.mods())
                }
            }
            windowing::Event::DroppedFile { window, path, x, y } => {
                let w = self.find_native(*window);
                if w >= 0 && !self.blocked(w) {
                    let (px, py) = self.pointer(w, *x, *y);
                    self.dispatch_at(
                        w,
                        px,
                        py,
                        &Event::Drop {
                            x: px,
                            y: py,
                            paths: vec![path.to_string()],
                            text: String::new(),
                        },
                    )
                }
            }
            windowing::Event::DroppedText { window, text, x, y } => {
                let w = self.find_native(*window);
                if w >= 0 && !self.blocked(w) {
                    let (px, py) = self.pointer(w, *x, *y);
                    self.dispatch_at(
                        w,
                        px,
                        py,
                        &Event::Drop {
                            x: px,
                            y: py,
                            paths: vec![],
                            text: text.to_string(),
                        },
                    )
                }
            }
            windowing::Event::FocusGained { window } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    self.windows[w as usize].active = true;
                    self.windows[w as usize].focused_once = true;
                    self.windows[w as usize].caret_on = true;
                    self.windows[w as usize].dirty = true;
                    self.update_caret_timer(w)
                }
            }
            windowing::Event::FocusLost { window } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    // Interrupted input clears press, capture, and pending chords.
                    self.windows[w as usize].active = false;
                    self.windows[w as usize].pressed = String::new();
                    if !self.windows[w as usize].capture.is_empty() {
                        let cap = self.windows[w as usize].capture.clone();
                        self.windows[w as usize].capture = String::new();
                        self.send_to(w, &cap, &Event::CaptureLost)
                    }
                    self.windows[w as usize].commands.reset_chord();
                    self.windows[w as usize].dirty = true;
                    self.update_caret_timer(w);
                    // A run box goes away once the user turns to something else.
                    if self.windows[w as usize].dismissible && self.windows[w as usize].focused_once
                    {
                        self.close_requested(w)
                    }
                }
            }
            windowing::Event::ThemeChanged { .. } | windowing::Event::PreferencesChanged => {
                self.apply_system_preferences()
            }
            windowing::Event::RenderReset { window } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    let key = &self.windows[w as usize].key;
                    if let Some(renderer) = self.renderers.get_mut(key) {
                        renderer.device_lost();
                    }
                    self.windows[w as usize].dirty = true;
                }
            }
            windowing::Event::Resumed => {
                for renderer in self.renderers.values_mut() {
                    renderer.invalidate();
                }
                for win in &mut self.windows {
                    win.occluded = false;
                    win.dirty = true;
                }
            }
            windowing::Event::RedrawRequested { window } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    self.windows[w as usize].occluded = false;
                    if let Some(renderer) = self.renderers.get_mut(&self.windows[w as usize].key) {
                        renderer.retained.invalidate();
                    }
                }
                let w = self.find_native(*window);
                if w >= 0 {
                    self.windows[w as usize].dirty = true
                }
            }
            windowing::Event::Resized { window, .. }
            | windowing::Event::PixelSizeChanged { window, .. } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    self.invalidate(w, "window resized");
                    // A window that grows for its popups is resized only by
                    // the session, to show them; it places them again instead.
                    if self.windows[w as usize].grow_base <= 0 {
                        self.close_all_popups(w)
                    }
                }
            }
            windowing::Event::ScaleChanged { window, scale } => {
                let w = self.find_native(*window);
                if w >= 0 && *scale > 0.0 && self.scale_override.is_none() {
                    self.windows[w as usize].theme.scale = scale.clamp(0.5, 4.0);
                    self.invalidate(w, "scale changed");
                    self.close_all_popups(w)
                }
            }
            windowing::Event::DisplayScaleChanged { .. } => {
                let mut w = 0;
                while w < self.windows.len() {
                    let s = self
                        .scale_override
                        .unwrap_or(self.windows[w].native.scale());
                    if s > 0.0 {
                        self.windows[w].theme.scale = s.clamp(0.5, 4.0);
                        self.invalidate(w as i64, "display scale changed")
                    }
                    w += 1;
                }
            }
            windowing::Event::Restored { window } | windowing::Event::WindowShown { window } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    self.windows[w as usize].occluded = false;
                    self.windows[w as usize].dirty = true;
                }
            }
            windowing::Event::Minimized { window } | windowing::Event::Occluded { window } => {
                let w = self.find_native(*window);
                if w >= 0 {
                    self.windows[w as usize].active = false;
                    self.windows[w as usize].occluded = true
                }
            }
            windowing::Event::DialogClosed { dialog, choice } => {
                self.native_dialog_closed(*dialog, choice.clone())
            }
            _ => (),
        }
        Ok(())
    }

    // ---- dispatch ----

    // The topmost layer of window `w`: -1 the root, k a dialog.
    fn top_layer(&self, w: i64) -> i64 {
        self.windows[w as usize].dialogs.len() as i64 - 1
    }

    // The lowest layer input reaches in window `w`: its topmost modal
    // dialog, or -1 (the root) when only modeless dialogs are open.
    fn floor_layer(&self, w: i64) -> i64 {
        self.windows[w as usize]
            .dialogs
            .iter()
            .rposition(|d| d.modality != Modality::Modeless)
            .map_or(-1, |k| k as i64)
    }

    // The window holding an application-modal dialog, or -1.
    fn modal_window(&self) -> i64 {
        let mut w = 0;
        while w < self.windows.len() {
            if self.windows[w]
                .dialogs
                .iter()
                .any(|d| d.modality == Modality::Application)
            {
                return w as i64;
            };
            w += 1;
        }
        -1
    }

    // The window that blocks input to `w`: the dialog window `w` opened, or
    // the window of an application-modal dialog elsewhere; -1 when none.
    fn blocker(&self, w: i64) -> i64 {
        let key = &self.windows[w as usize].key;
        if let Some(d) = self
            .windows
            .iter()
            .rposition(|win| win.owner == *key && !win.dialogs.is_empty())
        {
            return d as i64;
        }
        let m = self.modal_window();
        if m >= 0 && m != w { m } else { -1 }
    }

    fn blocked(&self, w: i64) -> bool {
        self.blocker(w) >= 0
    }

    // The key `layer_focus` files a layer under.
    fn layer_key(&self, w: i64, layer: i64) -> String {
        if layer < 0 {
            String::new()
        } else {
            self.windows[w as usize].dialogs[layer as usize].id.clone()
        }
    }

    // The layer holding widget `id` in window `w`, or -2.
    fn layer_of(&self, w: i64, id: &str) -> i64 {
        if id.is_empty() {
            return -2;
        };
        let theme = &self.windows[w as usize].theme;
        let mut layer = self.top_layer(w);
        while layer >= -1 {
            let (root, node) = self.layer_tree(w, layer);
            if !tree::path_to(&root, &node, theme, id).is_empty() {
                return layer;
            };
            layer -= 1;
        }
        -2
    }

    // Whether input may reach `layer` of window `w`.
    fn reachable(&self, w: i64, layer: i64) -> bool {
        layer >= -1 && layer >= self.floor_layer(w)
    }

    // The reachable layer holding focus, else the topmost.
    fn focus_layer(&self, w: i64) -> i64 {
        let layer = self.layer_of(w, &self.windows[w as usize].focus);
        if self.reachable(w, layer) {
            layer
        } else {
            self.top_layer(w)
        }
    }

    // The dialog Escape and Enter act on: the reachable one holding focus,
    // else the topmost modal one; -1 when neither.
    fn key_dialog(&self, w: i64) -> i64 {
        let layer = self.layer_of(w, &self.windows[w as usize].focus);
        if layer >= 0 && self.reachable(w, layer) {
            layer
        } else {
            self.floor_layer(w)
        }
    }

    // The layer and hit path at (x, y): a modeless dialog answers only
    // inside itself; the floor layer answers everywhere.
    fn layer_at(&self, w: i64, x: i64, y: i64) -> (i64, Vec<PathEntry>) {
        let theme = &self.windows[w as usize].theme;
        let floor = self.floor_layer(w);
        let mut layer = self.top_layer(w);
        while layer > floor {
            let (root, node) = self.layer_tree(w, layer);
            let path = tree::hit_path(&root, &node, theme, x, y);
            if !path.is_empty() {
                return (layer, path);
            };
            layer -= 1;
        }
        let (root, node) = self.layer_tree(w, floor);
        (floor, tree::hit_path(&root, &node, theme, x, y))
    }

    fn layer_tree(&self, w: i64, layer: i64) -> (Widget, Node) {
        if layer < 0 {
            (
                self.windows[w as usize].root.clone(),
                self.windows[w as usize].node.clone(),
            )
        } else {
            let d = &self.windows[w as usize].dialogs[layer as usize];
            (d.root.clone(), d.node.clone())
        }
    }

    fn path_in_layers(&self, w: i64, id: &str) -> Vec<PathEntry> {
        let theme = &self.windows[w as usize].theme;
        let mut layer = self.top_layer(w);
        while layer >= -1 {
            let (root, node) = self.layer_tree(w, layer);
            let p = tree::path_to(&root, &node, theme, id);
            if !p.is_empty() {
                return p;
            };
            layer -= 1;
        }
        vec![]
    }

    fn new_cx(&self, w: i64) -> EventCx {
        let win = &self.windows[w as usize];
        let mut cx = EventCx::new(
            &win.theme,
            &win.focus,
            self.now(),
            Rect::new(0, 0, win.node.rect.width, win.node.rect.height),
        );
        cx.active = win.active;
        cx.registry = win.commands.commands.clone();
        cx
    }

    // Delivers `ev` along `path`, deepest first, until a widget consumes it.
    fn bubble(&mut self, w: i64, path: &[PathEntry], ev: &Event, cx: &mut EventCx) {
        let mut st = self.take_states(w);
        let mut i = path.len();
        while i > 0 && !cx.handled {
            i -= 1;
            let entry = &path[i];
            if entry.disabled && !matches!(ev, Event::FocusOut) {
                continue;
            }
            cx.origin_x = entry.x;
            cx.origin_y = entry.y;
            cx.disabled = entry.disabled;
            cx.theme = entry.theme.clone();
            entry.widget.event(&entry.node, ev, &mut st, cx)
        }
        self.windows[w as usize].states = st;
    }

    fn poll_accessibility(&mut self) {
        if self.a11y.activated() {
            for w in 0..self.windows.len() {
                self.windows[w].dirty = true
            }
        }
        for action in self.a11y.poll() {
            let w = self.find(&action.window);
            if w < 0 || self.blocked(w) {
                continue;
            };
            let layer = self.layer_of(w, &action.owner);
            if !self.reachable(w, layer) {
                continue;
            };
            let (root, node) = self.layer_tree(w, layer);
            let path = tree::path_to(&root, &node, &self.windows[w as usize].theme, &action.owner);
            if path.is_empty() || path[path.len() - 1].disabled {
                continue;
            };
            self.send_to(
                w,
                &action.owner,
                &Event::Accessible {
                    target: action.target,
                    action: action.kind,
                    value: action.value,
                    start: action.start,
                    end: action.end,
                },
            )
        }
    }

    fn take_states(&mut self, w: i64) -> StateStore {
        std::mem::take(&mut self.windows[w as usize].states)
    }

    // Sends `ev` to the widget `id` alone (not bubbling).
    fn send_to(&mut self, w: i64, id: &str, ev: &Event) {
        let path = self.path_in_layers(w, id);
        if path.is_empty() {
            return;
        };
        let mut cx = self.new_cx(w);
        let entry = &path[path.len() - 1];
        let mut st = self.take_states(w);
        cx.origin_x = entry.x;
        cx.origin_y = entry.y;
        cx.disabled = entry.disabled;
        cx.theme = entry.theme.clone();
        entry.widget.event(&entry.node, ev, &mut st, &mut cx);
        self.windows[w as usize].states = st;
        self.apply_cx(w, &cx)
    }

    fn send_to_focus(&mut self, w: i64, ev: &Event) {
        let focus = &self.windows[w as usize].focus;
        if focus.is_empty() {
            return;
        };
        let path = self.path_in_layers(w, focus);
        if path.is_empty() {
            return;
        };
        let mut cx = self.new_cx(w);
        self.bubble(w, &path, ev, &mut cx);
        self.apply_cx(w, &cx)
    }

    fn dispatch_at(&mut self, w: i64, x: i64, y: i64, ev: &Event) {
        let (_, path) = self.layer_at(w, x, y);
        let mut cx = self.new_cx(w);
        self.bubble(w, &path, ev, &mut cx);
        self.apply_cx(w, &cx)
    }

    // The open popup under (x, y), topmost first.
    fn popup_at(&self, w: i64, x: i64, y: i64) -> i64 {
        let ps = &self.windows[w as usize].popups;
        let mut i = ps.len();
        while i > 0 {
            i -= 1;
            if ps[i].rect.contains(x, y) {
                return i as i64;
            }
        }
        -1
    }

    fn popup_dispatch(&mut self, w: i64, index: i64, ev: &Event) -> bool {
        let p = self.windows[w as usize].popups[index as usize].clone();
        let path = self.path_in_layers(w, &p.request.owner);
        if path.is_empty() {
            return false;
        }
        let owner = &path[path.len() - 1];
        let mut cx = self.new_cx(w);
        cx.origin_x = owner.x;
        cx.origin_y = owner.y;
        cx.theme = owner.theme.clone();
        cx.popup_level = p.request.level;
        let mut st = self.take_states(w);
        owner
            .widget
            .popup_event(&owner.node, &p.request, p.rect, ev, &mut st, &mut cx);
        self.windows[w as usize].states = st;
        if let Event::PointerMove { .. } = ev
            && cx.cursor.is_none()
        {
            cx.cursor = Some(windowing::SystemCursor::Default)
        };
        let handled = cx.handled;
        self.apply_cx(w, &cx);
        handled
    }

    fn pointer_down(
        &mut self,
        w: i64,
        x: i64,
        y: i64,
        button: MouseButton,
        clicks: i64,
        mods: Mods,
    ) {
        if self.blocked(w) {
            let _ = self.windows[self.blocker(w) as usize].native.focus();
            return;
        }
        self.windows[w as usize].focus_visible = false;
        self.windows[w as usize].keyboard_input = false;
        self.log_line(&format!("down {},{}", x, y));
        // Clicks count on our clock so simulated input double-clicks too.
        let now = self.now();
        let (lt, lx, ly, lc) = self.last_click;
        let count = if clicks > 1 {
            clicks
        } else if now - lt < self.windows[w as usize].theme.motion.double_click
            && (x - lx).abs() < 5
            && (y - ly).abs() < 5
        {
            lc + 1
        } else {
            1
        };
        self.last_click = (now, x, y, count);
        let ev = Event::PointerDown {
            x,
            y,
            button,
            clicks: count,
            mods,
        };
        let pi = self.popup_at(w, x, y);
        if pi >= 0 {
            self.popup_dispatch(w, pi, &ev);
            return;
        }
        let (layer, path) = self.layer_at(w, x, y);
        // An outside click dismisses popups; a menu also swallows the click,
        // and so does a press on the owner of a popup that press closes.
        if !self.windows[w as usize].popups.is_empty() {
            let mut swallow = false;
            for p in &self.windows[w as usize].popups {
                if p.request.dismiss_outside && p.request.takes_keys {
                    swallow = true
                };
                if p.request.closes_on_owner_press
                    && path.iter().any(|e| e.widget.id() == p.request.owner)
                {
                    swallow = true
                }
            }
            self.dismiss_popups(w);
            if swallow {
                return;
            }
        }
        if layer >= 0 && path.is_empty() {
            return;
        }
        let mut pressed = String::new();
        for e in &path {
            let id = e.widget.id();
            if !id.is_empty() && !e.disabled {
                pressed = id
            }
        }
        self.windows[w as usize].pressed = pressed;
        let mut cx = self.new_cx(w);
        self.bubble(w, &path, &ev, &mut cx);
        self.windows[w as usize].dirty = true;
        self.apply_cx(w, &cx)
    }

    fn pointer_up(&mut self, w: i64, x: i64, y: i64, button: MouseButton, mods: Mods) {
        if self.blocked(w) {
            return;
        };
        let ev = Event::PointerUp { x, y, button, mods };
        self.windows[w as usize].pressed = String::new();
        self.windows[w as usize].dirty = true;
        let cap = &self.windows[w as usize].capture;
        if !cap.is_empty() {
            let path = self.path_in_layers(w, cap);
            self.windows[w as usize].capture = String::new();
            if !path.is_empty() {
                let mut cx = self.new_cx(w);
                self.bubble(w, &path, &ev, &mut cx);
                self.apply_cx(w, &cx)
            }
            return;
        }
        let pi = self.popup_at(w, x, y);
        if pi >= 0 {
            self.popup_dispatch(w, pi, &ev);
            return;
        }
        self.dispatch_at(w, x, y, &ev)
    }

    fn pointer_move(&mut self, w: i64, x: i64, y: i64, pressed: bool, mods: Mods) {
        if self.blocked(w) {
            return;
        };
        let ev = Event::PointerMove {
            x,
            y,
            pressed,
            mods,
        };
        let cap = &self.windows[w as usize].capture;
        if !cap.is_empty() {
            let path = self.path_in_layers(w, cap);
            let mut cx = self.new_cx(w);
            self.bubble(w, &path, &ev, &mut cx);
            // Widgets ask for the cursor they need on each move; anywhere else shows the arrow.
            if cx.cursor.is_none() {
                cx.cursor = Some(windowing::SystemCursor::Default)
            };
            self.apply_cx(w, &cx);
            return;
        }
        let pi = self.popup_at(w, x, y);
        if pi >= 0 {
            self.popup_dispatch(w, pi, &ev);
            return;
        }
        let (_, path) = self.layer_at(w, x, y);
        let mut ids = vec![];
        for e in &path {
            let id = e.widget.id();
            if !id.is_empty() {
                ids.push(id)
            }
        }
        let old = self.windows[w as usize].hover.clone();
        if ids != *old {
            for id in old {
                if !ids.contains(&id) {
                    self.send_to(w, &id, &Event::PointerLeave)
                }
            }
            self.windows[w as usize].hover = ids;
            self.windows[w as usize].dirty = true;
        }
        let mut cx = self.new_cx(w);
        self.bubble(w, &path, &ev, &mut cx);
        if cx.cursor.is_none() {
            cx.cursor = Some(windowing::SystemCursor::Default)
        };
        self.apply_cx(w, &cx)
    }

    // Shows the system cursor `kind`, replacing the one showing. A cursor
    // is destroyed only once another has replaced it.
    fn show_cursor(&mut self, kind: windowing::SystemCursor) {
        if self.cursor_kind == Some(kind) {
            return;
        }
        self.cursor_kind = Some(kind);
        let Ok(next) = windowing::cursor::system(kind) else {
            return;
        };
        if next.show().is_err() {
            next.destroy();
            return;
        }
        if let Some(old) = self.cursor {
            old.destroy()
        }
        self.cursor = Some(next);
    }

    fn pointer_left(&mut self, w: i64) {
        let old = self.windows[w as usize].hover.clone();
        for id in old {
            self.send_to(w, &id, &Event::PointerLeave)
        }
        self.windows[w as usize].hover = vec![];
        self.windows[w as usize].dirty = true;
    }

    fn wheel(&mut self, w: i64, x: i64, y: i64, dx: f64, dy: f64, mods: Mods) {
        if self.blocked(w) {
            return;
        };
        let ev = Event::Scroll { x, y, dx, dy, mods };
        let pi = self.popup_at(w, x, y);
        if pi >= 0 {
            self.popup_dispatch(w, pi, &ev);
            return;
        }
        if !self.windows[w as usize].popups.is_empty() {
            self.dismiss_popups(w)
        }
        self.dispatch_at(w, x, y, &ev)
    }

    fn focused_scopes(&self, w: i64) -> Vec<String> {
        let path = self.path_in_layers(w, &self.windows[w as usize].focus);
        let mut out = vec![];
        let mut i = path.len();
        while i > 0 {
            i -= 1;
            let id = path[i].widget.id();
            if !id.is_empty() {
                out.push(id)
            }
        }
        out
    }

    /// Whether the focused widget draws a blinking text caret.
    fn focus_has_caret(&self, w: i64) -> bool {
        let path = self.path_in_layers(w, &self.windows[w as usize].focus);
        path.last().is_some_and(|step| match step.widget {
            Widget::TextInput(ref t) => !t.read_only,
            Widget::TemporalEditor(_) => true,
            _ => false,
        })
    }

    /// Whether the focused widget takes typed text, which turns the window's
    /// text input on and keeps text keys from starting shortcuts.
    fn focus_takes_text(&self, w: i64) -> bool {
        let path = self.path_in_layers(w, &self.windows[w as usize].focus);
        path.last().is_some_and(|step| step.widget.accepts_text())
    }

    fn key_down(
        &mut self,
        w: i64,
        key: Key,
        mods: Mods,
        repeat: bool,
    ) -> Result<(), errors::Error> {
        if self.blocked(w) {
            return Ok(());
        };
        self.log_line(&format!("key {}", crate::input::key_name(key)));
        self.windows[w as usize].keyboard_input = true;
        if key == Key::Escape && !self.windows[w as usize].capture.is_empty() {
            let capture = self.windows[w as usize].capture.clone();
            self.windows[w as usize].capture = "".to_string();
            self.send_to(w, &capture, &Event::CaptureLost);
            let _ = self.app.capture_pointer(false);
            return Ok(());
        }
        if key == Key::Escape && self.windows[w as usize].dismissible {
            self.close_requested(w);
            return Ok(());
        }
        if (key == Key::LeftAlt || key == Key::RightAlt) && !self.windows[w as usize].mnemonics {
            self.windows[w as usize].mnemonics = true;
            self.windows[w as usize].dirty = true;
        }
        let ev = Event::KeyDown { key, mods, repeat };
        let keyboard_nav = key == Key::Tab
            || key == Key::Up
            || key == Key::Down
            || key == Key::Left
            || key == Key::Right;
        // 1. A popup that takes keys (menus, pickers) sees them first.
        let np = self.windows[w as usize].popups.len();
        if np > 0 && self.windows[w as usize].popups[np - 1].request.takes_keys {
            if self.popup_dispatch(w, (np - 1) as i64, &ev) {
                return Ok(());
            }
            if key == Key::Escape {
                self.close_top_popup(w);
                return Ok(());
            }
        }
        // 2. The focused widget and its ancestors.
        let focus = &self.windows[w as usize].focus;
        let mut handled = false;
        if !focus.is_empty() {
            let path = self.path_in_layers(w, focus);
            let mut cx = self.new_cx(w);
            self.bubble(w, &path, &ev, &mut cx);
            handled = cx.handled;
            self.apply_cx(w, &cx)
        }
        if handled {
            return Ok(());
        }
        // A key the focused widget took was input to it (a caret move, a
        // shell's history), not a move of focus, so only one left to the
        // window shows where focus is.
        if keyboard_nav && !self.windows[w as usize].focus_visible {
            self.windows[w as usize].focus_visible = true;
            self.windows[w as usize].dirty = true;
        }
        if self.floor_layer(w) < 0 {
            // 3. Commands.
            let scopes = self.focused_scopes(w);
            let editing = self.focus_takes_text(w);
            let now = self.now();
            let mut table = self.windows[w as usize].commands.clone();
            let result = table.key(key, mods, repeat, &scopes, editing, now);
            self.windows[w as usize].commands = table.clone();
            match result {
                KeyResult::Fire(id) => {
                    self.run_command(w, &id);
                    return Ok(());
                }
                KeyResult::Pending => {
                    self.log_line("chord pending");
                    return Ok(());
                }
                KeyResult::Disabled(_) => return Ok(()),
                KeyResult::NoMatch => (),
            }
        }
        // 4. Focus traversal and dismissal.
        if key == Key::Tab && !mods.ctrl && !mods.alt {
            self.move_focus(w, if mods.shift { -1 } else { 1 });
            return Ok(());
        }
        if key == Key::F6
            && !mods.ctrl
            && !mods.alt
            && self.cycle_layer(w, if mods.shift { -1 } else { 1 })
        {
            return Ok(());
        }
        // Inside a toolbar the arrows move between its controls (Left and
        // Right swap in right-to-left layouts).
        if (key == Key::Left || key == Key::Right) && !mods.ctrl && !mods.alt {
            let forward = (key == Key::Right) != self.windows[w as usize].theme.rtl;
            if self.rove(w, if forward { 1 } else { -1 }) {
                return Ok(());
            }
        }
        if key == Key::Escape {
            if self.windows[w as usize].popups.is_empty() {
                let k = self.key_dialog(w);
                self.dialog_cancel(w, k)
            } else {
                self.close_top_popup(w)
            }
            return Ok(());
        }
        let dialog = self.key_dialog(w);
        if key == Key::Return && dialog >= 0 {
            self.dialog_default(w, dialog);
            return Ok(());
        }
        if key == Key::Menu || (key == Key::F10 && mods.shift) {
            self.send_to_focus(w, &Event::Command("gui.context-menu".to_string()));
            return Ok(());
        }
        // Menu bars hear F10 and Alt+mnemonics even without focus.
        if key == Key::F10 || (mods.alt && !mods.ctrl) {
            let theme = &self.windows[w as usize].theme;
            let bars = tree::find_kind(
                &self.windows[w as usize].root,
                &self.windows[w as usize].node,
                theme,
                "menubar",
            );
            if !bars.is_empty() {
                let mut cx = self.new_cx(w);
                self.bubble(w, &[bars[0].clone()], &ev, &mut cx);
                self.apply_cx(w, &cx)
            }
        }
        Ok(())
    }

    fn key_up(&mut self, w: i64, key: Key, mods: Mods) {
        if self.blocked(w) {
            return;
        };
        if (key == Key::LeftAlt || key == Key::RightAlt) && self.windows[w as usize].mnemonics {
            self.windows[w as usize].mnemonics = false;
            self.windows[w as usize].dirty = true;
        }
        self.send_to_focus(w, &Event::KeyUp { key, mods })
    }

    /// Runs command `id` in window `w`: the focused path may implement it
    /// first; disabled commands never run.
    pub fn run_command(&mut self, w: i64, id: &str) {
        if w < 0 || w >= self.windows.len() as i64 {
            return;
        };
        if !self.windows[w as usize].commands.is_enabled(id) {
            return;
        }
        self.log_line(&format!("command {}", id));
        let focus = &self.windows[w as usize].focus;
        if !focus.is_empty() {
            let path = self.path_in_layers(w, focus);
            let mut cx = self.new_cx(w);
            self.bubble(w, &path, &Event::Command(id.to_string()), &mut cx);
            let handled = cx.handled;
            self.apply_cx(w, &cx);
            if handled {
                return;
            }
        }
        if self.floor_layer(w) >= 0 {
            return;
        };
        match id {
            "gui.quit" => self.request_quit(),
            "gui.toggle-theme" => {
                self.follow_system = false;
                let kind = if self.base_theme.is_dark() {
                    crate::style::ThemeKind::Light
                } else {
                    crate::style::ThemeKind::Dark
                };
                self.set_theme_kind(kind)
            }
            "gui.inspector" => {
                self.inspector = !self.inspector;
                self.invalidate(w, "inspector toggled")
            }
            "gui.command-palette" => {
                let cmds = self.windows[w as usize].commands.palette_entries();
                self.open_dialog_in(w, &host::palette_request(&cmds))
            }
            _ => {
                if self.windows[w as usize].key == "main" {
                    self.pending.push(UiMsg::Command(id.to_string()))
                } else {
                    self.pending.push(UiMsg::WindowCommand(
                        self.windows[w as usize].key.clone(),
                        id.to_string(),
                    ))
                }
            }
        }
    }

    fn move_focus(&mut self, w: i64, dir: i64) {
        let layer = self.focus_layer(w);
        let (root, node) = self.layer_tree(w, layer);
        let chain = self.tab_stops(w, &tree::focus_groups(&root, &node));
        if chain.is_empty() {
            return;
        };
        let focus = &self.windows[w as usize].focus;
        let mut at: i64 = -1;
        let mut i = 0;
        while i < chain.len() {
            if chain[i] == *focus {
                at = i as i64
            };
            i += 1;
        }
        let n = chain.len();
        let n = n as i64;
        let next = if at < 0 {
            if dir < 0 { n - 1 } else { 0 }
        } else {
            (at + dir + n) % n
        };
        self.set_focus(w, &chain[next as usize])
    }

    // The focus chain with each toolbar reduced to one stop: the control it
    // last had focused (the focused one when focus is inside it), else its first.
    fn tab_stops(&self, w: i64, groups: &[(String, String)]) -> Vec<String> {
        let focus = &self.windows[w as usize].focus;
        let mut entry: HashMap<String, String> = HashMap::new();
        for (id, group) in groups {
            if group.is_empty() {
                continue;
            };
            let remembered = self.windows[w as usize]
                .roving
                .get(group)
                .map_or("", String::as_str);
            let current = entry.get(group).map_or("", String::as_str);
            if *id == *focus || (id == remembered && *current != *focus) || current.is_empty() {
                entry.insert(group.to_string(), id.to_string());
            }
        }
        let mut out = vec![];
        for (id, group) in groups {
            if group.is_empty() || entry.get(group).map_or("", String::as_str) == id {
                out.push(id.to_string())
            }
        }
        out
    }

    // Moves focus to the next reachable layer (the window's content and its
    // modeless dialogs): where it was when F6 last left that layer, else
    // where the layer starts. False when there is only one layer.
    fn cycle_layer(&mut self, w: i64, dir: i64) -> bool {
        let floor = self.floor_layer(w);
        let n = self.top_layer(w) - floor + 1;
        if n < 2 {
            return false;
        };
        let current = self.focus_layer(w);
        let next = floor + ((current - floor + dir) % n + n) % n;
        let here = self.layer_key(w, current);
        let focus = self.windows[w as usize].focus.clone();
        self.windows[w as usize]
            .layer_focus
            .insert(here.clone(), focus);
        let (root, node) = self.layer_tree(w, next);
        let chain = self.tab_stops(w, &tree::focus_groups(&root, &node));
        let remembered = self.windows[w as usize]
            .layer_focus
            .get(&self.layer_key(w, next))
            .map_or("", String::as_str);
        let first = if next >= 0 {
            host::initial_focus(&self.windows[w as usize].dialogs[next as usize])
        } else {
            String::new()
        };
        let target = if chain.iter().any(|c| c == remembered) {
            remembered.to_string()
        } else if chain.contains(&first) {
            first
        } else {
            chain.first().cloned().unwrap_or_default()
        };
        if target.is_empty() {
            return false;
        };
        self.set_focus(w, &target);
        self.windows[w as usize].focus_visible = true;
        self.windows[w as usize].dirty = true;
        true
    }

    // Moves focus within the focused control's toolbar; false outside one.
    fn rove(&mut self, w: i64, dir: i64) -> bool {
        let layer = self.focus_layer(w);
        let (root, node) = self.layer_tree(w, layer);
        let groups = tree::focus_groups(&root, &node);
        let focus = &self.windows[w as usize].focus;
        let mut group = String::new();
        for (id, g) in &groups {
            if id == focus {
                group = g.to_string()
            }
        }
        if group.is_empty() {
            return false;
        };
        let members = groups
            .iter()
            .filter(|e| e.1 == group)
            .cloned()
            .collect::<Vec<_>>()
            .iter()
            .map(|e| e.0.clone())
            .collect::<Vec<_>>();
        let n = members.len();
        let at = members.iter().position(|id| *id == *focus).unwrap_or(0);
        self.set_focus(w, &members[(at as i64 + dir).rem_euclid(n as i64) as usize]);
        true
    }

    /// Moves focus to `id` in window `w`, sending focus-out and focus-in,
    /// revealing it in scroll areas, and starting or stopping text input.
    pub fn set_focus(&mut self, w: i64, id: &str) {
        // A modal owns focus even when background work requests another field.
        if !id.is_empty()
            && !self.windows[w as usize].dialogs.is_empty()
            && !self.reachable(w, self.layer_of(w, id))
        {
            return;
        };
        let old = self.windows[w as usize].focus.clone();
        if old == id {
            return;
        };
        self.windows[w as usize].focus = id.to_string();
        self.windows[w as usize].dirty = true;
        self.windows[w as usize].caret_on = true;
        self.remember_roving(w, id);
        if !old.is_empty() {
            self.send_to(w, &old, &Event::FocusOut)
        };
        if !id.is_empty() {
            self.send_to(w, id, &Event::FocusIn);
            self.reveal(w, id)
        }
        self.update_text_input(w);
        self.update_caret_timer(w);
        self.a11y.focus_changed(&self.windows[w as usize].key, id)
    }

    // Records `id` as its toolbar's stop, when it is in one.
    fn remember_roving(&mut self, w: i64, id: &str) {
        if id.is_empty() {
            return;
        };
        let path = self.path_in_layers(w, id);
        for e in path {
            if let Some(key) = tree::toolbar_key(&e.widget) {
                self.windows[w as usize]
                    .roving
                    .insert(key.clone(), id.to_string());
            }
        }
    }

    // Scrolls every scroll area on the path to `id` so it shows.
    fn reveal(&mut self, w: i64, id: &str) {
        let path = self.path_in_layers(w, id);
        if path.is_empty() {
            return;
        };
        let target = path[path.len() - 1].rect();
        let mut st = self.take_states(w);
        for e in path {
            if let Widget::Scroll(ref s) = e.widget {
                containers::reveal_in(&mut st, &s.id, e.rect(), e.node.content, target)
            }
        }
        self.windows[w as usize].states = st;
        self.invalidate(w, "focus revealed")
    }

    fn update_text_input(&mut self, w: i64) {
        let editing = self.focus_takes_text(w);
        let native = self.windows[w as usize].native;
        if editing && !native.is_text_input_active() {
            let _ = native.start_text_input();
        } else if !editing && native.is_text_input_active() {
            let _ = native.stop_text_input();
        }
    }

    fn update_caret_timer(&mut self, w: i64) {
        let key = self.windows[w as usize].key.clone();
        self.timers = self
            .timers
            .iter()
            .filter(|t| !(t.window == *key && t.owner.is_empty() && t.tag == "gui:caret"))
            .cloned()
            .collect::<Vec<_>>();
        if self.focus_has_caret(w) && self.windows[w as usize].active {
            let blink = self.windows[w as usize].theme.motion.caret_blink;
            self.timers.push(Timer {
                deadline: self.now() + blink,
                window: key.to_string(),
                owner: "".to_string(),
                tag: "gui:caret".to_string(),
                repeat: blink,
            })
        }
    }

    // Applies what one dispatch asked for.
    fn apply_cx(&mut self, w: i64, cx: &EventCx) {
        if w >= self.windows.len() as i64 {
            return;
        };
        for m in &cx.messages {
            if let UiMsg::TabDrop(strip, key, x, y) = m {
                self.tab_drop(w, strip, key, *x, *y);
                continue;
            };
            self.log_line(&format!("msg {}", msg_id(m)));
            self.pending.push(m.clone())
        }
        if cx.redraw {
            self.windows[w as usize].dirty = true
        };
        if cx.relayout {
            self.invalidate(w, "a widget asked");
            self.windows[w as usize].dirty = true;
        }
        if let Some(ref id) = cx.capture {
            self.windows[w as usize].capture = id.clone();
            let _ = self.app.capture_pointer(true);
        }
        if cx.release_capture {
            self.windows[w as usize].capture = String::new();
            let _ = self.app.capture_pointer(false);
        }
        for (owner, level) in &cx.close_popups {
            self.close_popup(w, owner, *level)
        }
        for req in &cx.open_popups {
            self.open_popup(w, req)
        }
        let key = self.windows[w as usize].key.clone();
        for (owner, tag) in &cx.cancel_timers {
            self.timers = self
                .timers
                .iter()
                .filter(|t| !(t.window == key && t.owner == *owner && t.tag == *tag))
                .cloned()
                .collect::<Vec<_>>();
        }
        for t in &cx.timers {
            self.timers = self
                .timers
                .iter()
                .filter(|x| !(x.window == key && x.owner == t.owner && x.tag == t.tag))
                .cloned()
                .collect::<Vec<_>>();
            self.timers.push(Timer {
                deadline: self.now() + t.delay.max(0),
                window: key.clone(),
                owner: t.owner.clone(),
                tag: t.tag.clone(),
                repeat: 0,
            })
        }
        if let Some(ref id) = cx.request_focus {
            self.set_focus(w, id)
        }
        if let Some(c) = cx.cursor {
            self.show_cursor(c)
        }
        if let Some((ref id, ref items, operation)) = cx.start_drag {
            self.start_drag(w, id, items, operation)
        }
        if let Some((rect, caret)) = cx.ime {
            let native = self.windows[w as usize].native;
            let (lw, lh) = native.size();
            let (pw, ph) = native.pixel_size();
            let area = windowing::Rect {
                x: logical_coordinate(rect.x, lw, pw),
                y: logical_coordinate(rect.y, lh, ph),
                width: logical_coordinate(rect.width, lw, pw).max(1),
                height: logical_coordinate(rect.height, lh, ph).max(1),
            };
            let _ = native.set_text_input_area(area, logical_coordinate(caret, lw, pw));
        }
        for a in &cx.announcements {
            self.speak(a)
        }
        for id in &cx.run {
            self.run_command(w, id)
        }
        // Typing restarts the caret blink in the visible phase.
        if cx.redraw && !self.windows[w as usize].caret_on {
            self.windows[w as usize].caret_on = true;
        }
    }

    // ---- popups ----

    fn open_popup(&mut self, w: i64, req: &PopupRequest) {
        let mut req = req.clone();
        let win = &self.windows[w as usize];
        // Async completion clears stale choices while loading. Keep the
        // previous popup's height until the replacement results arrive,
        // rather than briefly shrinking the native window to a loading row.
        if win.grow_base > 0
            && win
                .states
                .choice(&format!("{}#completion", req.owner))
                .loading
            && let Some(previous) = win
                .popups
                .iter()
                .find(|p| p.request.owner == req.owner && p.request.level == req.level)
        {
            req.size.height = req.size.height.max(previous.request.size.height);
        }
        let win_rect = Rect::new(
            0,
            0,
            self.windows[w as usize].node.rect.width,
            self.windows[w as usize].node.rect.height,
        );
        let rect = popup::place(
            req.anchor,
            req.size,
            win_rect,
            req.placement,
            self.windows[w as usize].theme.rtl,
        );
        let had_keys = self.windows[w as usize]
            .popups
            .iter()
            .any(|p| p.request.takes_keys);
        // Replace this owner's popup at this level and every deeper one.
        self.windows[w as usize].popups = self.windows[w as usize]
            .popups
            .iter()
            .filter(|p| !(p.request.owner == req.owner && p.request.level >= req.level))
            .cloned()
            .collect::<Vec<_>>();
        if req.takes_keys && !had_keys {
            self.windows[w as usize].popup_focus = Some(self.windows[w as usize].focus.clone())
        };
        self.windows[w as usize].popups.push(OpenPopup {
            request: req.clone(),
            rect,
        });
        self.windows[w as usize].dirty = true;
    }

    fn close_popup(&mut self, w: i64, owner: &str, level: i64) {
        let had_keys = self.windows[w as usize]
            .popups
            .iter()
            .any(|p| p.request.takes_keys);
        self.windows[w as usize].popups = self.windows[w as usize]
            .popups
            .iter()
            .filter(|p| !(p.request.owner == owner && p.request.level >= level))
            .cloned()
            .collect::<Vec<_>>();
        self.windows[w as usize].dirty = true;
        let has_keys = self.windows[w as usize]
            .popups
            .iter()
            .any(|p| p.request.takes_keys);
        if had_keys && !has_keys {
            let restore = self.windows[w as usize].popup_focus.clone();
            self.windows[w as usize].popup_focus = None;
            if let Some(f) = restore
                && !f.is_empty()
                && self.windows[w as usize].focus.is_empty()
            {
                self.set_focus(w, &f)
            }
        }
    }

    fn close_top_popup(&mut self, w: i64) {
        let n = self.windows[w as usize].popups.len();
        if n == 0 {
            return;
        };
        let p = self.windows[w as usize].popups[n - 1].clone();
        self.popup_dispatch(
            w,
            (n - 1) as i64,
            &Event::KeyDown {
                key: Key::Escape,
                mods: Mods::default(),
                repeat: false,
            },
        );
        self.close_popup(w, &p.request.owner, p.request.level)
    }

    // Closes every popup (outside click), telling owners.
    fn dismiss_popups(&mut self, w: i64) {
        let ps = &self.windows[w as usize].popups;
        let mut owners = vec![];
        for p in ps {
            if !owners.contains(&p.request.owner) {
                owners.push(p.request.owner.clone())
            }
        }
        for o in owners {
            self.send_to(w, &o, &Event::FocusOut);
            self.close_popup(w, &o, 0)
        }
    }

    // A growing window asks for the height its view and the popups below
    // their anchors need, at least its base height, and places each popup
    // in the height it has.
    fn fit_growing_window(&mut self, w: i64) {
        let base = self.windows[w as usize].grow_base;
        if base <= 0 {
            return;
        };
        let win = self.windows[w as usize].native;
        let (lw, lh) = win.size();
        let (_, ph) = win.pixel_size();
        let mut room = ((base as f64 * ph as f64 / lh.max(1) as f64).round() as i64)
            .max(self.windows[w as usize].content_height);
        for p in &self.windows[w as usize].popups {
            if p.request.placement == Placement::Below {
                room = room.max(p.request.anchor.bottom() + p.request.size.height)
            }
        }
        if room != self.windows[w as usize].grow_target {
            self.windows[w as usize].grow_target = room;
            if let Err(e) = win.set_size(lw, window_extent(room, lh, ph)) {
                self.log_line(&format!("window resize unavailable: {}", e))
            } else {
                // Layout must reflect the new native extent immediately;
                // waiting for a resize event leaves a clipped first frame.
                self.invalidate(w, "popup extent changed");
            }
        }
        let (area_width, area_height) = win.pixel_size();
        let area = Rect::new(0, 0, area_width, area_height);
        let rtl = self.windows[w as usize].theme.rtl;
        let mut i = 0;
        while i < self.windows[w as usize].popups.len() {
            let r = &self.windows[w as usize].popups[i].request;
            let rect = popup::place(r.anchor, r.size, area, r.placement, rtl);
            if rect != self.windows[w as usize].popups[i].rect {
                self.windows[w as usize].popups[i].rect = rect;
                self.windows[w as usize].dirty = true;
            }
            i += 1;
        }
    }

    fn close_all_popups(&mut self, w: i64) {
        self.windows[w as usize].popup_focus = None;
        self.windows[w as usize].popups = vec![];
        self.windows[w as usize].dirty = true;
    }

    // ---- timers and mail ----

    /// Keeps a repeating position tick while media plays, and none after.
    fn schedule_media_tick(&mut self) {
        let ticking = self
            .timers
            .iter()
            .any(|t| t.owner == TOOLKIT_OWNER && t.tag == MEDIA_TAG);
        let playing = crate::media::playing(&self.budget);
        if playing && !ticking {
            let now = self.now();
            self.timers.push(Timer {
                deadline: now + crate::media::TICK_MS,
                window: String::new(),
                owner: TOOLKIT_OWNER.to_string(),
                tag: MEDIA_TAG.to_string(),
                repeat: crate::media::TICK_MS,
            });
        } else if !playing && ticking {
            self.timers
                .retain(|t| !(t.owner == TOOLKIT_OWNER && t.tag == MEDIA_TAG));
        }
    }

    fn fire_timers(&mut self) -> Result<(), errors::Error> {
        let now = self.now();
        let mut due = vec![];
        let mut keep = vec![];
        for t in &self.timers {
            if t.deadline <= now && !self.visual_timer_paused(t) {
                due.push(t.clone());
                if t.repeat > 0 {
                    let mut again = t.clone();
                    again.deadline = now + t.repeat;
                    keep.push(again)
                }
            } else {
                keep.push(t.clone())
            }
        }
        self.timers = keep;
        for t in due {
            if t.window.is_empty() && t.owner == TOOLKIT_OWNER && t.tag == ANNOUNCE_TAG {
                self.speak_next();
                continue;
            }
            if t.window.is_empty() && t.owner == TOOLKIT_OWNER && t.tag == MEDIA_TAG {
                crate::media::tick(&self.budget);
                continue;
            }
            if t.window.is_empty() && t.owner.is_empty() {
                self.pending.push(UiMsg::Timer(t.tag));
                continue;
            }
            let w = self.find(&t.window);
            if w < 0 {
                continue;
            };
            if t.owner.is_empty() && t.tag == "gui:frame" {
                self.invalidate(w, "animation frame");
                continue;
            }
            if t.owner.is_empty() && t.tag == "gui:caret" {
                self.windows[w as usize].caret_on = !self.windows[w as usize].caret_on;
                self.windows[w as usize].dirty = true;
                continue;
            }
            if t.owner.is_empty() && t.tag.starts_with("gui:toast:") {
                let id = t.tag.substring(10, t.tag.byte_len());
                // The pointer or focus on a toast holds it; look again later.
                let held = self.windows[w as usize].hover.iter().any(|h| h == id)
                    || self.windows[w as usize].focus == id;
                if held {
                    let mut again = t;
                    again.deadline = now + TOAST_RECHECK_MS;
                    self.timers.push(again)
                } else if self.close_toast(w, id) {
                    self.pending.push(UiMsg::Click(format!("{}:timeout", id)));
                    self.rebuild(w)
                }
                continue;
            }
            self.send_to(w, &t.owner, &Event::Timer(t.tag))
        }
        Ok(())
    }

    // Records `text` and speaks it now, or when the announcement interval
    // running ends; a repeat of what is spoken or waiting adds nothing.
    fn speak(&mut self, text: &str) {
        self.announcements.push(text.to_string());
        if self.announcements.len() > 50 {
            let _ = self.announcements.remove(0);
        };
        let speaking = self
            .timers
            .iter()
            .any(|t| t.window.is_empty() && t.owner == TOOLKIT_OWNER && t.tag == ANNOUNCE_TAG);
        if !speaking {
            self.say(text)
        } else if text != self.last_spoken && !self.unspoken.iter().any(|u| u == text) {
            self.unspoken.push(text.to_string());
            if self.unspoken.len() > ANNOUNCE_BACKLOG as usize {
                let _ = self.unspoken.remove(0);
            }
        }
    }

    // Speaks the oldest waiting announcement when the interval ends.
    fn speak_next(&mut self) {
        if self.unspoken.is_empty() {
            self.last_spoken = String::new();
            return;
        }
        let text = self.unspoken[0].clone();
        let _ = self.unspoken.remove(0);
        self.say(&text)
    }

    // Hands `text` to assistive technology and starts an interval.
    fn say(&mut self, text: &str) {
        self.a11y.announce(text);
        self.last_spoken = text.to_string();
        self.timers.push(Timer {
            deadline: self.now() + ANNOUNCE_INTERVAL_MS,
            window: "".to_string(),
            owner: TOOLKIT_OWNER.to_string(),
            tag: ANNOUNCE_TAG.to_string(),
            repeat: 0,
        })
    }

    /// What assistive technology was told, newest last: announcements after
    /// the one-per-interval throttle.
    pub fn spoken(&self) -> Vec<String> {
        self.a11y.spoken.clone()
    }

    // Removes the toast `id` from window `w` and its timer; false when absent.
    fn close_toast(&mut self, w: i64, id: &str) -> bool {
        let before = self.windows[w as usize].toasts.len();
        self.windows[w as usize].toasts = self.windows[w as usize]
            .toasts
            .iter()
            .filter(|t| t.id != id)
            .cloned()
            .collect::<Vec<_>>();
        let tag = format!("gui:toast:{}", id);
        self.timers = self
            .timers
            .iter()
            .filter(|t| t.tag != tag)
            .cloned()
            .collect::<Vec<_>>();
        self.windows[w as usize].toasts.len() != before
    }

    fn drain_inbox(&mut self) {
        for (message, lease) in self.inbox.drain_leased() {
            self.pending.push(message);
            self.pending_leases.push(lease);
        }
    }

    // ---- messages, effects, rebuilds ----

    fn deliver_messages(&mut self) {
        let msgs = std::mem::take(&mut self.pending);
        let _leases = std::mem::take(&mut self.pending_leases);
        if msgs.is_empty() {
            return;
        }
        let mut app_changed = false;
        for m in msgs {
            if let UiMsg::RichLayoutReady(_) = &m {
                for window in &mut self.windows {
                    window.dirty = true;
                    window.layout_dirty = true;
                }
                continue;
            }
            if matches!(
                &m,
                UiMsg::Worked(_, _, _, crate::services::work::Outcome::RichImageLoaded(..))
            ) {
                for window in &mut self.windows {
                    window.dirty = true;
                }
            }
            if let UiMsg::LayoutReady(_) = &m {
                for window in &mut self.windows {
                    window.dirty = true;
                }
                continue;
            }
            if let UiMsg::FontReady(_) = &m {
                crate::text::layout::purge_caches();
                for window in &mut self.windows {
                    window.layout_dirty = true;
                    window.dirty = true;
                }
            }
            if let UiMsg::Task(owner, _) = &m
                && owner == "gui:font-catalog"
            {
                crate::text::fonts::catalog_failed();
            }
            if let UiMsg::FontsLoaded(_, records) = &m {
                crate::text::fonts::adopt_installed(records);
            }
            if let UiMsg::Worked(owner, request, revision, outcome) = &m {
                if let crate::services::work::Outcome::RichImported(record) = outcome {
                    record.adopt();
                }
                if let crate::services::work::Outcome::DocumentLoaded(record, _) = outcome {
                    crate::editing::documents::persistence::adopt(std::slice::from_ref(record));
                }
                if let crate::services::work::Outcome::SessionRestored(records, _) = outcome {
                    crate::editing::documents::persistence::adopt(records);
                }

                let items = match outcome {
                    services::work::Outcome::Completions(items) => Some((items.as_slice(), "")),
                    services::work::Outcome::Failed(error) => Some((&[][..], error.as_str())),
                    _ => None,
                };
                if let Some((items, error)) = items {
                    for w in 0..self.windows.len() {
                        let mut state = self.take_states(w as i64);
                        if state.edit(owner).revision == *revision
                            && textinput::deliver(&mut state, owner, *request, items, error)
                        {
                            self.windows[w].dirty = true;
                        }
                        self.windows[w].states = state;
                    }
                }
            }
            if self.dialog_message(&m) {
                continue;
            }
            // A toast's action or Dismiss closes it before the application hears it.
            if let UiMsg::Click(ref id) = m {
                let mut w = 0;
                while w < self.windows.len() {
                    let toasts: Vec<String> = self.windows[w]
                        .toasts
                        .iter()
                        .map(|t| t.id.clone())
                        .collect();
                    for t in toasts {
                        if id.starts_with(&format!("{}:", t)) && self.close_toast(w as i64, &t) {
                            app_changed = true
                        }
                    }
                    w += 1;
                }
            }
            if let Some(s) = self.shell.as_mut() {
                (s.update)(m.clone());
                app_changed = true;
            }
        }
        if app_changed {
            let mut w = 0;
            while w < self.windows.len() {
                self.rebuild(w as i64);
                w += 1;
            }
        }
    }

    // Ownership changes are reconciled before disposal so a moved page keeps its shares.
    fn reconcile_tabs(&mut self) -> Result<(), errors::Error> {
        let mut owners: HashMap<(String, String), (String, Vec<String>)> = HashMap::new();
        for win in &self.windows {
            for (key, page) in tree::workspace_pages(&win.root) {
                if owners.contains_key(&key) {
                    return Err(errors::new(format!(
                        "tab {:?} has more than one window owner",
                        key
                    )));
                };
                owners.insert(key.clone(), (win.key.clone(), tree::state_ids(&page)));
            }
        }
        for (key, (window, ids)) in &owners {
            if let Some((old_window, old_ids)) = self.tab_owners.get(key).cloned()
                && *old_window != *window
            {
                let source = self.find(&old_window);
                let target = self.find(window);
                if source >= 0 && target >= 0 {
                    let focused = self.windows[source as usize].focus.clone();
                    let mut from = self.take_states(source);
                    let mut to = self.take_states(target);
                    from.transfer_to(&mut to, &old_ids);
                    self.windows[source as usize].states = from;
                    self.windows[target as usize].states = to;
                    self.windows[target as usize].dirty = true;
                    self.invalidate(target, "tab moved in");
                    self.close_all_popups(source);
                    self.close_all_popups(target);
                    if old_ids.contains(&self.windows[source as usize].capture) {
                        self.windows[source as usize].capture = "".to_string();
                        let _ = self.app.capture_pointer(false);
                    }
                    let mut i = 0;
                    while i < self.timers.len() {
                        if self.timers[i].window == *old_window
                            && old_ids.contains(&self.timers[i].owner)
                        {
                            self.timers[i].window = window.clone()
                        };
                        i += 1;
                    }
                    if old_ids.contains(&focused) && ids.contains(&focused) {
                        self.windows[source as usize].focus = "".to_string();
                        self.update_text_input(source);
                        self.update_caret_timer(source);
                        self.layout(target);
                        self.set_focus(target, &focused)
                    }
                }
            }
        }
        self.tab_owners = owners;
        Ok(())
    }

    /// Rebuilds window `w` from the application's view.
    pub fn rebuild(&mut self, w: i64) {
        let key = self.windows[w as usize].key.clone();
        let view = self.build_view(&key);
        if crate::memory::measure(&view) > WINDOW_DESCRIPTION_BYTE_LIMIT {
            self.resource_error = format!("view for {key} exceeds its byte budget");
            return;
        }
        let charge = crate::memory::measure(&view).saturating_mul(3);
        let reservation = match self.descriptions.get_mut(&key) {
            Some(lease) => lease.resize(charge),
            None => self.budget.reserve(charge).map(|lease| {
                self.descriptions.insert(key.clone(), lease);
            }),
        };
        if let Err(error) = reservation {
            self.resource_error = error;
            return;
        }
        let mut view = view.clone();
        view.compact_maps();
        let toasts = &self.windows[w as usize].toasts;
        self.windows[w as usize].view = if toasts.is_empty() {
            view
        } else {
            Widget::stack(&[view, crate::widgets::feedback::toast_layer(toasts)])
        };
        self.windows[w as usize].root = self.windows[w as usize].view.clone();
        self.refresh_commands(w);
        self.invalidate(w, "view rebuilt")
    }

    fn refresh_commands(&mut self, w: i64) {
        let mut cmds = commands::standard_commands();
        if let Some(ref s) = self.shell {
            let f = &s.commands;
            for c in f(&self.windows[w as usize].key) {
                // Application commands replace standard ones with the same id.
                cmds = cmds
                    .iter()
                    .filter(|x| x.id != c.id)
                    .cloned()
                    .collect::<Vec<_>>();
                cmds.push(c)
            }
        }
        for c in &self.extra_commands {
            cmds = cmds
                .iter()
                .filter(|x| x.id != c.id)
                .cloned()
                .collect::<Vec<_>>();
            cmds.push(c.clone())
        }
        let mut table = self.windows[w as usize].commands.clone();
        table.set(&cmds);
        self.windows[w as usize].commands = table.clone();
        if w == 0 && windowing::menu::has_native_menu_bar() {
            self.sync_native_menus()
        }
    }

    // Starts a platform drag out of window `w`; its end, or a failure to
    // start, arrives as `UiMsg::DragFinished(id, ..)`.
    fn start_drag(
        &mut self,
        w: i64,
        id: &str,
        items: &[windowing::DragItem],
        operation: windowing::DragOperation,
    ) {
        let started = if w < 0 {
            None
        } else {
            match self
                .app
                .start_drag(self.windows[w as usize].native, items.to_vec(), operation)
            {
                Ok(drag) => Some(drag),
                Err(e) => {
                    self.log_line(&format!("drag {}: {}", id, e));
                    None
                }
            }
        };
        match started {
            Some(drag) => {
                let raw: u64 = drag.into();
                self.drags.insert(raw, id.to_string());
            }
            None => self.pending.push(UiMsg::DragFinished(id.to_string(), None)),
        }
    }

    // The main window's commands, or the standard ones before it opens.
    fn registry(&self) -> Vec<Command> {
        if self.windows.is_empty() {
            commands::standard_commands()
        } else {
            self.windows[0].commands.commands.clone()
        }
    }

    // Installs the main window's menu bar in the platform's, when its menus
    // or their command state changed since the last install.
    fn sync_native_menus(&mut self) {
        let Some(bar) = find_menu_bar(&self.windows[0].root) else {
            return;
        };
        let (installed, commands) =
            menus::native_menu_bar(&bar.menus, &self.windows[0].commands.commands);
        if let Some((ref current, _)) = self.native_menus
            && *current == installed
        {
            self.native_menus = Some((installed, commands));
            return;
        }
        match self.app.set_menu_bar(&installed) {
            Ok(()) => self.native_menus = Some((installed, commands)),
            Err(e) => self.log_line(&format!("menu bar: {}", e)),
        }
    }

    // Runs the command a native menu item stands for, through the same
    // enabled check a drawn menu click takes.
    fn menu_selected(&mut self, source: windowing::MenuSource, item: u32) {
        match source {
            windowing::MenuSource::MenuBar { window } => {
                let Some((_, commands)) = self.native_menus.clone() else {
                    return;
                };
                if item == 0 || item as i64 > commands.len() as i64 {
                    return;
                };
                let w = match window {
                    Some(id) => self.find_native(id),
                    None => -1,
                };
                self.run_command(
                    if w >= 0 { w } else { 0 },
                    &commands[(item as i64 - 1) as usize],
                )
            }
            windowing::MenuSource::Tray { .. } => {
                let Some(ref host) = self.tray else { return };
                if let Some(id) = host.command_for(source, item) {
                    self.run_command(0, &id)
                }
            }
        }
    }

    /// Serves `listener`'s single-instance requests: opened files arrive as
    /// `UiMsg::Open`, activation raises the main window. A request wakes the
    /// session's loop, and `shutdown` gives up the claim.
    pub fn serve_instance(&mut self, listener: windowing::instance::Listener) {
        listener.wake_with(self.waker());
        self.instance = Some(listener);
    }

    fn serve_instance_requests(&mut self) {
        let Some(ref listener) = self.instance else {
            return;
        };
        for forwarded in listener.take_forwarded() {
            match forwarded.request {
                windowing::instance::Request::Activate => {
                    if !self.windows.is_empty() {
                        let _ = self.windows[0]
                            .native
                            .focus_with_context(&forwarded.activation);
                    }
                }
                windowing::instance::Request::Open { uris } => {
                    for uri in uris {
                        if let Some(path) = windowing::instance::uri_path(&uri) {
                            self.pending.push(UiMsg::Open(path))
                        } else {
                            self.pending.push(UiMsg::OpenUri(uri))
                        }
                    }
                    if !self.windows.is_empty() {
                        let _ = self.windows[0]
                            .native
                            .focus_with_context(&forwarded.activation);
                    }
                }
            }
        }
    }

    // Marks live ids and disposes the state of widgets that left the tree.
    fn mark_tree(&mut self, w: i64) {
        let mut st = self.take_states(w);
        st.begin_frame();
        tree::mark_ids(&self.windows[w as usize].root, &mut st);
        for d in &self.windows[w as usize].dialogs {
            tree::mark_ids(&d.root, &mut st)
        }
        let gone = st.end_frame();
        self.windows[w as usize].states = st;
        if gone.is_empty() {
            return;
        };
        let key = self.windows[w as usize].key.clone();
        for id in gone {
            if let Some(worker) = &self.jobs {
                worker.cancel_owner(&id);
            }
            self.held_jobs.retain(|(job, _)| job.owner() != id);
            self.timers = self
                .timers
                .iter()
                .filter(|t| !(t.window == key && t.owner == *id))
                .cloned()
                .collect::<Vec<_>>();
            self.windows[w as usize].popups = self.windows[w as usize]
                .popups
                .iter()
                .filter(|p| p.request.owner != id)
                .cloned()
                .collect::<Vec<_>>();
            if self.windows[w as usize].capture == id {
                self.windows[w as usize].capture = String::new()
            };
            if self.windows[w as usize].focus == id {
                // Focus on a removed widget moves to nothing; text input stops.
                self.windows[w as usize].focus = String::new();
                self.update_text_input(w);
                self.update_caret_timer(w)
            }
        }
    }

    fn apply_effects(&mut self) -> Result<(), errors::Error> {
        for (e, _lease) in effects::take_leased() {
            match e {
                Effect::OpenDialog(req) => {
                    if !self.windows.is_empty() {
                        self.open_dialog_in(0, &req)
                    }
                }
                Effect::OpenDialogIn(key, req) => {
                    let w = self.find(&key);
                    if w >= 0 {
                        self.open_dialog_in(w, &req)
                    } else {
                        self.pending
                            .push(UiMsg::Dialog(req.id(), Outcome::Disposed))
                    }
                }
                Effect::OpenDialogWith(key, req, modality) => {
                    let w = if key.is_empty() {
                        if self.windows.is_empty() { -1 } else { 0 }
                    } else {
                        self.find(&key)
                    };
                    if w >= 0 {
                        self.open_dialog_as(w, &req, modality)
                    } else {
                        self.pending
                            .push(UiMsg::Dialog(req.id(), Outcome::Disposed))
                    }
                }
                Effect::OpenWindow {
                    key,
                    title,
                    width,
                    height,
                } => {
                    let policy = if self.windows.is_empty() {
                        ClosePolicy::Close
                    } else {
                        self.windows[0].close_policy
                    };
                    self.open_window(&key, &title, width, height)?;
                    self.set_close_policy(&key, policy)
                }
                Effect::CloseWindow(key) => {
                    let w = self.find(&key);
                    if w >= 0 {
                        self.close_window(w)
                    }
                }
                Effect::Quit => self.quit = true,
                Effect::SetWindowPosition(window, x, y) => {
                    let w = self.find(&window);
                    if w >= 0
                        && windowing::video_driver() != "wayland"
                        && let Err(e) = self.windows[w as usize].native.set_position(x, y)
                    {
                        self.log_line(&format!("window placement unavailable: {}", e))
                    }
                }
                Effect::SetMinSize(window, width, height) => {
                    self.set_min_size(&window, width, height)?
                }
                Effect::FocusIn(window, id) => {
                    let w = self.find(&window);
                    if w >= 0 {
                        self.layout(w);
                        self.set_focus(w, &id)
                    }
                }
                Effect::Focus(id) => {
                    if !self.windows.is_empty() {
                        self.settle_layout(0)?;
                        self.set_focus(0, &id)
                    }
                }
                Effect::SetTitle(key, title) => {
                    let w = self.find(&key);
                    if w >= 0 {
                        let _ = self.windows[w as usize].native.set_title(&title);
                        self.windows[w as usize].title = title;
                    }
                }
                Effect::Timer(tag, delay, repeat) => {
                    self.timers = self
                        .timers
                        .iter()
                        .filter(|t| !(t.window.is_empty() && t.owner.is_empty() && t.tag == tag))
                        .cloned()
                        .collect::<Vec<_>>();
                    self.timers.push(Timer {
                        deadline: self.now() + delay.max(0),
                        window: "".to_string(),
                        owner: "".to_string(),
                        tag,
                        repeat: if repeat { delay.max(1) } else { 0 },
                    })
                }
                Effect::CancelTimer(tag) => {
                    self.timers = self
                        .timers
                        .iter()
                        .filter(|t| !(t.window.is_empty() && t.owner.is_empty() && t.tag == tag))
                        .cloned()
                        .collect::<Vec<_>>()
                }
                Effect::Complete(owner, request, items, error) => {
                    let mut w = 0;
                    while w < self.windows.len() {
                        let mut st = self.take_states(w as i64);
                        let ok = textinput::deliver(&mut st, &owner, request, &items, &error);
                        self.windows[w].states = st;
                        if ok {
                            self.windows[w].dirty = true;
                            self.resize_completion(w as i64, &owner)
                        }
                        w += 1;
                    }
                }
                Effect::Announce(text) => self.speak(&text),
                Effect::Toast(toast) => {
                    if !self.windows.is_empty() {
                        self.close_toast(0, &toast.id);
                        self.windows[0].toasts.push(toast.clone());
                        let key = &self.windows[0].key;
                        self.timers.push(Timer {
                            deadline: self.now() + toast.duration.max(0),
                            window: key.to_string(),
                            owner: "".to_string(),
                            tag: format!("gui:toast:{}", toast.id),
                            repeat: 0,
                        });
                        self.speak(&toast.text);
                        self.rebuild(0)
                    }
                }
                Effect::DismissToast(id) => {
                    let mut w = 0;
                    while w < self.windows.len() {
                        if self.close_toast(w as i64, &id) {
                            self.rebuild(w as i64)
                        };
                        w += 1;
                    }
                }
                Effect::DialogChecked(id, request, error) => {
                    self.dialog_checked(&id, request, &error)
                }
                Effect::Work(work) => self.run_job(&crate::services::jobs::Job::Work(work)),
                Effect::CancelWork(owner) => {
                    if let Some(worker) = &self.jobs {
                        worker.cancel_owner(&owner);
                    }
                    self.held_jobs.retain(|(job, _)| job.owner() != owner);
                }
                Effect::ListDirectory(req) => self.run_job(&crate::services::jobs::Job::List(req)),
                Effect::LoadFonts(owner) => self.run_job(&crate::services::jobs::Job::Fonts(owner)),
                Effect::LoadApps(owner) => self.run_job(&crate::services::jobs::Job::Apps(owner)),
                Effect::LaunchApp(owner, entry, files) => {
                    self.run_job(&crate::services::jobs::Job::Launch(owner, entry, files))
                }
                Effect::Emit(message) => self.pending.push(message),
                Effect::SetClipboard(text) => crate::services::clipboard::set_text(&text),
                Effect::OpenUrl(url) => {
                    if let Err(err) = services::open_url(&url) {
                        self.log_line(&format!("open url failed: {}", err))
                    }
                }
                Effect::RevealFile(path) => {
                    if let Err(err) = services::reveal(&path) {
                        self.log_line(&format!("reveal failed: {}", err))
                    }
                }
                Effect::SetTheme(kind) => {
                    self.follow_system = false;
                    self.set_theme_kind(kind)
                }
                Effect::FollowSystemTheme => {
                    self.follow_system = true;
                    self.apply_system_preferences()
                }
                Effect::SetTextScale(s) => {
                    self.explicit_text_scale = true;
                    self.base_theme.text_scale = s.clamp(0.5, 3.0);
                    self.retheme()
                }
                Effect::SetDisplayScale(scale) => {
                    self.scale_override = scale;
                    let mut w = 0;
                    while w < self.windows.len() {
                        self.windows[w].theme.scale = scale
                            .unwrap_or(self.windows[w].native.scale())
                            .clamp(0.5, 4.0);
                        self.invalidate(w as i64, "display scale set");
                        self.close_all_popups(w as i64);
                        w += 1;
                    }
                }
                Effect::SetReducedMotion(reduced) => {
                    self.explicit_motion = true;
                    self.base_theme.reduced_motion = reduced;
                    self.retheme()
                }
                Effect::SetDensity(d) => {
                    self.base_theme.density = d;
                    self.retheme()
                }
                Effect::SetThemeTokens(tokens) => {
                    self.base_theme = self.base_theme.with_tokens(&tokens);
                    self.retheme();
                    // The tokens carry a whole type ramp; the desktop's menu
                    // size goes back over it.
                    self.apply_system_preferences()
                }
                Effect::SetLocale(catalog) => {
                    self.base_theme.rtl = catalog.rtl();
                    crate::localization::install(&catalog);
                    self.retheme();
                    // Views translate as they are built, so every window is built again.
                    let mut w = 0;
                    while w < self.windows.len() {
                        self.rebuild(w as i64);
                        w += 1;
                    }
                }
                Effect::SetRtl(r) => {
                    self.base_theme.rtl = r;
                    self.retheme()
                }
                Effect::Commands(cmds) => {
                    self.extra_commands = cmds;
                    let mut w = 0;
                    while w < self.windows.len() {
                        self.refresh_commands(w as i64);
                        w += 1;
                    }
                }
                Effect::ScrollTo(id) => {
                    if !self.windows.is_empty() {
                        self.settle_layout(0)?;
                        self.reveal(0, &id)
                    }
                }
                Effect::Notify {
                    id,
                    title,
                    body,
                    urgency,
                } => {
                    let mailbox = self.mailbox();
                    services::desktop::notify(&self.app, &title, &body, urgency, move |sent| {
                        let outcome = match sent {
                            Ok(()) => crate::widgets::cx::TaskOutcome::Done(String::new()),
                            Err(e) => crate::widgets::cx::TaskOutcome::Failed(e.to_string()),
                        };
                        mailbox.post(&UiMsg::Task(id, outcome))
                    })
                }
                Effect::SetTray {
                    id,
                    icon,
                    tooltip,
                    items,
                } => {
                    if let Some(ref old) = self.tray {
                        old.close()
                    };
                    self.tray = None;
                    match services::desktop::TrayHost::open(
                        self.app.clone(),
                        icon,
                        &tooltip,
                        &items,
                        &self.registry(),
                    ) {
                        Ok(host) => self.tray = Some(host),
                        Err(e) => self.pending.push(UiMsg::Task(
                            id,
                            crate::widgets::cx::TaskOutcome::Failed(format!("{}", e)),
                        )),
                    }
                }
                Effect::CloseTray => {
                    if let Some(ref old) = self.tray {
                        old.close()
                    };
                    self.tray = None;
                }
                Effect::StartDrag {
                    window,
                    id,
                    items,
                    operation,
                } => self.start_drag(self.find(&window), &id, &items, operation),
                Effect::SelectText {
                    window,
                    id,
                    anchor,
                    caret,
                } => {
                    let w = self.find(&window);
                    if w >= 0 {
                        self.settle_layout(w)?;
                        self.send_to(w, &id, &Event::SetSelection { anchor, caret });
                        self.reveal(w, &id)
                    }
                }
                Effect::ReplaceText {
                    window,
                    id,
                    start,
                    end,
                    text,
                } => {
                    let w = self.find(&window);
                    if w >= 0 {
                        self.settle_layout(w)?;
                        self.send_to(w, &id, &Event::ReplaceRange { start, end, text });
                        self.reveal(w, &id)
                    }
                }
            }
            self.sync_auxiliary()?;
        }
        Ok(())
    }

    // Convert each window's pixels through its own logical extent before comparing displays.
    fn tab_drop(&mut self, source: i64, strip: &str, key: &str, x: i64, y: i64) {
        let path = self.path_in_layers(source, strip);
        if path.is_empty() {
            return;
        };
        let Widget::Tabs(ref from) = path[path.len() - 1].widget else {
            return;
        };
        let source_window = self.windows[source as usize].native;
        let (sx, sy) = source_window.position();
        let (lw, lh) = source_window.size();
        let (pw, ph) = source_window.pixel_size();
        let gx = sx + logical_coordinate(x, lw, pw);
        let gy = sy + logical_coordinate(y, lh, ph);
        if let Some(dock) = crate::dock::dock_of_strip(strip)
            && let Some(drop) = self.dock_drop(source, dock, key, (x, y), (gx, gy))
        {
            self.pending.push(UiMsg::Dock(dock.to_string(), drop));
            return;
        }
        if windowing::video_driver() != "wayland" {
            let mut target = 0;
            while target < self.windows.len() {
                if target != source as usize && self.floor_layer(target as i64) < 0 {
                    let native = self.windows[target].native;
                    let (tx, ty) = native.position();
                    let (tw, th) = native.size();
                    let (pixel_w, pixel_h) = native.pixel_size();
                    let px = pixel_coordinate((gx - tx) as f64, tw, pixel_w);
                    let py = pixel_coordinate((gy - ty) as f64, th, pixel_h);
                    for entry in tree::find_kind(
                        &self.windows[target].root,
                        &self.windows[target].node,
                        &self.windows[target].theme,
                        "tabs",
                    ) {
                        if let Widget::Tabs(to) = entry.widget
                            && to.workspace == from.workspace
                            && !to.workspace.is_empty()
                            && py >= entry.y
                            && py
                                < entry.y
                                    + self.windows[target].theme.control_height()
                                    + self.windows[target].theme.px(4)
                            && px >= entry.x
                            && px < entry.x + entry.node.rect.width
                        {
                            let mut at = 0;
                            while at < to.items.len() && at * 2 < entry.node.parts.len() {
                                let r = entry.node.parts[at * 2];
                                if px - entry.x < r.center_x() {
                                    break;
                                };
                                at += 1;
                            }
                            self.pending.push(UiMsg::TabMove(
                                strip.to_string(),
                                key.to_string(),
                                to.id,
                                at as i64,
                            ));
                            return;
                        }
                    }
                }
                target += 1;
            }
        }
        self.pending
            .push(UiMsg::TabDetach(strip.to_string(), key.to_string(), gx, gy))
    }

    /// The dock group and zone under a tab dropped at window pixel `local`
    /// of `source`, or at global logical `global` over another window.
    fn dock_drop(
        &self,
        source: i64,
        dock: &str,
        pane: &str,
        local: (i64, i64),
        global: (i64, i64),
    ) -> Option<crate::dock::Drop> {
        use crate::dock::Zone;
        // Other windows' positions are unknown to clients under Wayland.
        let others = windowing::video_driver() != "wayland";
        for (w, window) in self.windows.iter().enumerate() {
            let (px, py) = if w as i64 == source {
                local
            } else if others && self.floor_layer(w as i64) < 0 {
                let (tx, ty) = window.native.position();
                let (tw, th) = window.native.size();
                let (pixel_w, pixel_h) = window.native.pixel_size();
                (
                    pixel_coordinate((global.0 - tx) as f64, tw, pixel_w),
                    pixel_coordinate((global.1 - ty) as f64, th, pixel_h),
                )
            } else {
                continue;
            };
            for entry in tree::find_kind(&window.root, &window.node, &window.theme, "tabs") {
                let Widget::Tabs(group) = &entry.widget else {
                    continue;
                };
                let (gw, gh) = (entry.node.rect.width, entry.node.rect.height);
                if crate::dock::dock_of_strip(&group.id) != Some(dock)
                    || px < entry.x
                    || py < entry.y
                    || px >= entry.x + gw
                    || py >= entry.y + gh
                {
                    continue;
                }
                let strip = window.theme.control_height() + window.theme.px(4);
                // A quarter of the group from an edge places beside that edge;
                // the strip and the middle place among the tabs.
                let fx = (px - entry.x) as f64 / gw.max(1) as f64;
                let fy = (py - entry.y - strip) as f64 / (gh - strip).max(1) as f64;
                let edges = [
                    (fx, Zone::Left),
                    (1.0 - fx, Zone::Right),
                    (fy, Zone::Top),
                    (1.0 - fy, Zone::Bottom),
                ];
                let zone = if py < entry.y + strip {
                    Zone::Center
                } else {
                    edges
                        .into_iter()
                        .filter(|(distance, _)| *distance < 0.25)
                        .min_by(|a, b| a.0.total_cmp(&b.0))
                        .map_or(Zone::Center, |(_, zone)| zone)
                };
                return Some(crate::dock::Drop {
                    pane: pane.to_string(),
                    group: group.id.clone(),
                    zone,
                });
            }
        }
        None
    }

    fn resize_completion(&mut self, w: i64, owner: &str) {
        let cs = self.windows[w as usize]
            .states
            .choice(&format!("{}#completion", owner));
        let mut i = 0;
        while i < self.windows[w as usize].popups.len() {
            let p = self.windows[w as usize].popups[i].clone();
            if p.request.owner == owner {
                let mut req = p.request.clone();
                req.size = crate::widgets::completion::list_size(
                    &cs,
                    &self.windows[w as usize].theme,
                    req.anchor.width,
                );
                self.open_popup(w, &req);
                return;
            }
            i += 1;
        }
    }

    // What the inspector shows about window `w`.
    fn inspector_snapshot(&self, w: i64) -> crate::devtools::inspector::Snapshot {
        let win = &self.windows[w as usize];
        // The view's own layout: with the inspector open the window splits,
        // and the split's first pane holds the view.
        let content = if win.node_inspected && !win.node.children.is_empty() {
            &win.node.children[0]
        } else {
            &win.node
        };
        crate::devtools::inspector::Snapshot {
            states: win.states.clone(),
            log: self.log.clone(),
            focus: win.focus.clone(),
            hover: win.hover.clone(),
            capture: win.capture.clone(),
            pressed: win.pressed.clone(),
            popups: win.popups.len() as i64,
            dialogs: win.dialogs.iter().map(|d| d.id.clone()).collect::<Vec<_>>(),
            frames: win.frames,
            causes: win.layout_causes.clone(),
            theme: win.theme.clone(),
            commands: win.commands.commands.clone(),
            scopes: self.focused_scopes(w),
            semantics: tree::semantics(
                &win.view,
                content,
                &win.states,
                &win.theme,
                &win.focus,
                &win.title,
            ),
            node: content.clone(),
        }
    }

    // Marks window `w` for layout, noting `cause` for the inspector.
    fn invalidate(&mut self, w: i64, cause: &str) {
        self.windows[w as usize].layout_dirty = true;
        let entry = format!("{} ms: {}", self.now(), cause);
        self.windows[w as usize].layout_causes.push(entry);
        if self.windows[w as usize].layout_causes.len() > 20 {
            let _ = self.windows[w as usize].layout_causes.remove(0);
        }
    }

    // Deterministic stepped sessions answer inline. Production run() uses
    // the bounded worker; pre-run requests can be held for that worker.
    fn run_job(&mut self, job: &crate::services::jobs::Job) {
        use crate::services::jobs::{DEFAULT_DEADLINE, QUEUE_BYTE_LIMIT, QUEUE_LIMIT};
        if self.jobs.is_none() && !self.holds_jobs && !self.inline_work {
            match crate::services::jobs::Worker::new(self.mailbox()) {
                Ok(worker) => self.attach_worker(worker),
                Err(error) => {
                    self.pending.push(job.failure(error));
                    return;
                }
            }
        }
        if let Some(queue) = &self.jobs {
            let deadline = match job {
                crate::services::jobs::Job::Work(work) => {
                    std::time::Duration::from_millis(work.timeout_ms.clamp(1, 600_000) as u64)
                }
                _ => DEFAULT_DEADLINE,
            };
            if let Err(e) = queue.submit(job, deadline) {
                if e == crate::services::jobs::SubmitError::Full
                    && let crate::services::jobs::Job::RichLayout(request) = job
                {
                    crate::rich::layout_cache::defer(&self.budget, request);
                } else {
                    self.pending.push(job.failure(e));
                }
            }
        } else if self.holds_jobs {
            let bytes: Option<usize> = self.held_jobs.iter().map(|(job, _)| job.bytes()).sum();
            match (bytes, job.bytes()) {
                (Some(held), Some(bytes))
                    if self.held_jobs.len() < QUEUE_LIMIT && held + bytes <= QUEUE_BYTE_LIMIT =>
                {
                    match self.budget.reserve(bytes) {
                        Ok(lease) => self.held_jobs.push((job.clone(), lease)),
                        Err(error) => self.pending.push(job.failure(error)),
                    }
                }
                _ => self
                    .pending
                    .push(job.failure("pre-run background queue is full or oversized")),
            }
        } else if job.bytes().is_none() {
            self.pending
                .push(job.failure("background request exceeds its byte limit"));
        } else {
            self.pending.push(crate::services::jobs::answer(job));
        }
    }

    fn attach_worker(&mut self, jobs: crate::services::jobs::Worker) {
        self.jobs = Some(jobs);
        for (job, lease) in std::mem::take(&mut self.held_jobs) {
            drop(lease);
            self.run_job(&job);
        }
    }

    /// Keeps jobs asked for before `run` (an application's `init` loading
    /// its data) for `run`'s worker, so the window shows and takes input
    /// while they run. A session driven without `run` answers them as it
    /// settles.
    pub fn hold_jobs_for_run(&mut self) {
        self.holds_jobs = true;
    }

    // The desktop's settings applied to the base theme (`with_preferences`).
    // Sessions under test keep the defaults.
    fn apply_system_preferences(&mut self) {
        if !FOLLOW_DESKTOP_PREFERENCES {
            return;
        };
        let theme = with_preferences(
            self.base_theme.clone(),
            services::system_preferences(),
            services::system_prefers_dark(),
            self.follow_system,
            self.explicit_text_scale,
            self.explicit_motion,
        );
        if theme != self.base_theme {
            self.base_theme = theme;
            self.retheme()
        }
    }

    fn set_theme_kind(&mut self, kind: crate::style::ThemeKind) {
        self.base_theme = self.base_theme.with_kind(kind);
        self.retheme()
    }

    // Applies the base theme to every window, keeping each window's scale.
    fn retheme(&mut self) {
        let mut w = 0;
        while w < self.windows.len() {
            let scale = self.windows[w].theme.scale;
            let mut t = self.base_theme.clone();
            t.scale = scale;
            self.windows[w].theme = t.clone();
            self.frame_theme(w as i64);
            self.invalidate(w as i64, "theme changed");
            self.close_all_popups(w as i64);
            w += 1;
        }
    }

    // Draws window `w`'s title bar in the application's theme when the
    // application chose one, and in the desktop's while it follows it.
    fn frame_theme(&mut self, w: i64) {
        let theme = if self.follow_system {
            None
        } else if self.base_theme.is_dark() {
            Some(windowing::Theme::Dark)
        } else {
            Some(windowing::Theme::Light)
        };
        // Window systems that draw no themed frame keep their own.
        let _ = self.windows[w as usize].native.set_frame_theme(theme);
    }

    fn close_requested(&mut self, w: i64) {
        let modal = self.floor_layer(w);
        if modal >= 0 {
            self.dialog_cancel(w, modal);
            return;
        };
        if self.windows[w as usize].close_policy == ClosePolicy::Ask {
            self.pending
                .push(UiMsg::CloseRequested(self.windows[w as usize].key.clone()));
            return;
        }
        self.close_window(w)
    }

    /// Closes window `w`; its open dialogs answer `Disposed`.
    pub fn close_window(&mut self, w: i64) {
        if let Err(e) = self.reconcile_tabs() {
            self.log_line(&format!("cannot close window: {}", e));
            return;
        };
        let win = self.windows[w as usize].clone();
        if let Some(worker) = &self.jobs {
            worker.cancel_owner(&win.key);
            for id in win.states.ids() {
                worker.cancel_owner(id);
            }
            for dialog in &win.dialogs {
                worker.cancel_owner(&dialog.id);
            }
        }
        self.held_jobs.retain(|(job, _)| {
            job.owner() != win.key
                && !win.states.ids().any(|id| id == job.owner())
                && !win.dialogs.iter().any(|dialog| dialog.id == job.owner())
        });
        for d in &win.dialogs {
            if let Some(value) = crate::dialogs::pickers::rollback(d, Outcome::Disposed) {
                self.pending.push(UiMsg::DialogPreview(d.id.clone(), value))
            };
            self.pending
                .push(UiMsg::Dialog(d.id.clone(), Outcome::Disposed))
        }
        if let Err(e) = services::save_window_placement(&self.app_id, &win.key, placement_of(&win))
        {
            self.warn(&format!(
                "could not save where window \"{}\" was: {}",
                win.key, e
            ))
        };
        let pending = &mut self.pending;
        self.native_dialogs.retain(|_, (owner, id)| {
            let gone = *owner == win.key;
            if gone {
                pending.push(UiMsg::Dialog(id.clone(), Outcome::Disposed))
            }
            !gone
        });
        self.timers.retain(|t| t.window != win.key);
        self.renderers.remove(&win.key);
        self.frame_leases.remove(&win.key);
        self.descriptions.remove(&win.key);
        win.native.close();
        let _ = self.windows.remove(w as usize);
        self.a11y.window_closed(&win.key);
        if let Some(callback) = &mut self.disposed {
            callback(&win.key);
        }
    }

    fn request_quit(&mut self) {
        if self
            .windows
            .iter()
            .any(|win| win.close_policy == ClosePolicy::Ask)
        {
            self.pending.push(UiMsg::QuitRequested)
        } else {
            self.quit = true
        }
    }

    // ---- dialogs ----

    /// Shows a dialog in window `w`. Native file dialogs are tracked until
    /// the platform answers; toolkit dialogs take the window modal.
    pub fn open_dialog_in(&mut self, w: i64, req: &DialogRequest) {
        self.open_dialog_as(w, req, Modality::Window)
    }

    /// Shows a dialog in window `w` blocking what `modality` says. Native
    /// file dialogs are modal to their window whatever it says.
    pub fn open_dialog_as(&mut self, w: i64, req: &DialogRequest, modality: Modality) {
        let id = req.id();
        // A progress update replaces the open dialog of the same id.
        let mut i = 0;
        while i < self.windows[w as usize].dialogs.len() {
            if self.windows[w as usize].dialogs[i].id == id {
                let mut inst = self.windows[w as usize].dialogs[i].clone();
                inst.update_request(req);
                self.windows[w as usize].dialogs[i] = inst.clone();
                self.relayout_dialog(w, i as i64);
                return;
            }
            i += 1;
        }
        match host::native_plan(req) {
            host::Plan::Native(title, location, filters, many, mode) => {
                let mut wf = vec![];
                for (name, pattern) in filters {
                    wf.push(windowing::dialog::Filter { name, pattern })
                }
                let fd = windowing::dialog::FileDialog {
                    title,
                    location,
                    filters: wf,
                    many,
                    parent: Some(self.windows[w as usize].native),
                };
                let opened = match mode {
                    0 => self.app.open_file_dialog(&fd),
                    1 => self.app.save_file_dialog(&fd),
                    _ => self.app.open_folder_dialog(&fd),
                };
                match opened {
                    Ok(native) => {
                        let k: u64 = native.into();
                        self.native_dialogs
                            .insert(k as i64, (self.windows[w as usize].key.clone(), id));
                    }
                    Err(e) => self
                        .pending
                        .push(UiMsg::Dialog(id, Outcome::Failed(format!("{}", e)))),
                }
                return;
            }
            host::Plan::Refuse(why) => {
                self.pending
                    .push(UiMsg::Dialog(id, Outcome::Unsupported(why)));
                return;
            }
            host::Plan::Toolkit => (),
        }
        // A file browser needs room the window that asked may not have, so it
        // opens in a window of its own, modal to that one.
        let (w, own_window) = if matches!(
            req,
            DialogRequest::File { .. } | DialogRequest::Shortcut { .. }
        ) {
            let shortcut = matches!(req, DialogRequest::Shortcut { .. });
            match self.dialog_window(w, &req.title(), shortcut) {
                Ok(d) => (d, true),
                Err(e) => {
                    self.pending
                        .push(UiMsg::Dialog(id, Outcome::Failed(e.to_string())));
                    return;
                }
            }
        } else {
            (w, false)
        };
        let f = self.windows[w as usize].focus.clone();
        let mut inst = host::Instance::new(req, &self.app_id);
        inst.modality = if own_window {
            Modality::Window
        } else {
            modality
        };
        inst.fills_window = own_window;
        // Modeless dialogs close in any order, so each keeps its own opener.
        if modality == Modality::Modeless {
            inst.opener = f.to_string()
        } else {
            self.windows[w as usize].focus_stack.push(f.to_string())
        };
        self.close_all_popups(w);
        self.windows[w as usize].dialogs.push(inst);
        let k = self.windows[w as usize].dialogs.len() - 1;
        self.relayout_dialog(w, k as i64);
        self.mark_tree(w);
        let first = host::initial_focus(&self.windows[w as usize].dialogs[k]);
        let chain = tree::focus_chain(
            &self.windows[w as usize].dialogs[k].root,
            &self.windows[w as usize].dialogs[k].node,
            "",
        );
        let target = if !first.is_empty() && chain.contains(&first) {
            first
        } else if chain.is_empty() {
            String::new()
        } else {
            chain[0].clone()
        };
        self.windows[w as usize].focus = String::new();
        if !target.is_empty() {
            self.set_focus(w, &target)
        };
        self.windows[w as usize].focus_visible = self.windows[w as usize].keyboard_input;
        self.speak(&host::announcement(&self.windows[w as usize].dialogs[k]));
        if modality == Modality::Application {
            let _ = self.windows[w as usize].native.focus();
        }
    }

    // The window file dialogs of window `owner` open in, made and shown the
    // first time. It is modal to `owner` and keeps the size the user gave it.
    fn dialog_window(
        &mut self,
        owner: i64,
        title: &str,
        shortcut: bool,
    ) -> Result<i64, errors::Error> {
        let owner_key = self.windows[owner as usize].key.clone();
        let prefix = if shortcut {
            SHORTCUT_DIALOG_WINDOW
        } else {
            FILE_DIALOG_WINDOW
        };
        let key = format!("{prefix}{owner_key}");
        let found = self.find(&key);
        if found >= 0 {
            return Ok(found);
        }
        let parent = self.windows[owner as usize].native;
        let (width, height) = if shortcut {
            (560, 360)
        } else {
            FILE_DIALOG_SIZE
        };
        self.prepare_window_as(
            &key,
            title,
            width,
            height,
            WindowStyle::default(),
            Some(parent),
        )?;
        let d = self.find(&key);
        self.windows[d as usize].owner = owner_key;
        let focus = self.windows[owner as usize].focus.clone();
        if let Some(opener) = self.path_in_layers(owner, &focus).last() {
            self.windows[d as usize].theme = opener.theme.clone();
        }
        let (min_w, min_h) = if shortcut {
            (480, 340)
        } else {
            FILE_DIALOG_MIN
        };
        self.set_min_size(&key, min_w, min_h)?;
        self.show_window(&key)?;
        Ok(d)
    }

    // Rebuilds and lays out dialog `k` of window `w`.
    fn relayout_dialog(&mut self, w: i64, k: i64) {
        let mut inst = self.windows[w as usize].dialogs[k as usize].clone();
        let theme = self.windows[w as usize].theme.clone();
        inst.root = host::view(&inst, &theme);
        let mut st = self.take_states(w);
        tree::sync_models(&inst.root, &mut st, &theme);
        self.windows[w as usize].states = st;
        let size = Size::new(
            self.windows[w as usize].node.rect.width.max(1),
            self.windows[w as usize].node.rect.height.max(1),
        );
        let mut lcx = LayoutCx::new(&theme, &self.windows[w as usize].states, size);
        if inst.fills_window {
            inst.node = inst
                .root
                .layout(Constraints::tight(size), &mut lcx)
                .at(0, 0);
            self.windows[w as usize].dialogs[k as usize] = inst.clone();
            self.windows[w as usize].dirty = true;
            return;
        }
        // A dialog keeps a margin of the window around it, however tall its body.
        let margin = theme.px(16);
        let room = Size::new(
            (size.width - margin * 2).max(1),
            (size.height - margin * 2).max(1),
        );
        let natural = inst.root.layout(Constraints::loose(room), &mut lcx);
        let ds = natural.size();
        let x = (size.width - ds.width) / 2;
        let y = margin + ((room.height - ds.height) / 3).max(0);
        inst.node = natural.at(x, y);
        self.windows[w as usize].dialogs[k as usize] = inst.clone();
        self.windows[w as usize].dirty = true;
    }

    // Routes a message from a dialog's controls to its controller; answers
    // whether the message belonged to a dialog.
    fn dialog_message(&mut self, m: &UiMsg) -> bool {
        let id = msg_id(m);
        if !id.starts_with("dlg:") {
            return false;
        }
        let mut w = 0;
        while w < self.windows.len() {
            let mut k = 0;
            while k < self.windows[w].dialogs.len() {
                let inst = self.windows[w].dialogs[k].clone();
                if id.starts_with(&format!("dlg:{}:", inst.id)) {
                    let mut st = self.take_states(w as i64);
                    let (next, outcome, effects_out) = host::update(inst.clone(), m, &mut st);
                    self.windows[w].states = st;
                    self.dialog_stepped(w as i64, k as i64, &next, outcome, &effects_out);
                    return true;
                }
                k += 1;
            }
            w += 1;
        }
        true
    }

    // Stores dialog `k`'s new state and carries out what its step asked for:
    // its effects, then closing with `outcome` or laying it out again.
    fn dialog_stepped(
        &mut self,
        w: i64,
        k: i64,
        next: &host::Instance,
        outcome: Option<Outcome>,
        effects_out: &[Effect],
    ) {
        self.windows[w as usize].dialogs[k as usize] = next.clone();
        for e in effects_out {
            match e {
                Effect::Focus(target) => effects::perform(Effect::FocusIn(
                    self.windows[w as usize].key.clone(),
                    target.to_string(),
                )),
                _ => effects::perform(e.clone()),
            }
        }
        match outcome {
            Some(o) => self.finish_dialog(w, k, &o),
            None => {
                self.relayout_dialog(w, k);
                self.mark_tree(w)
            }
        }
    }

    // Hands the application's answer to check `request` to the dialog `id`.
    fn dialog_checked(&mut self, id: &str, request: i64, error: &str) {
        let mut w = 0;
        while w < self.windows.len() {
            let mut k = 0;
            while k < self.windows[w].dialogs.len() {
                if self.windows[w].dialogs[k].id == id {
                    let (next, outcome, fx) = crate::dialogs::pickers::check_answered(
                        self.windows[w].dialogs[k].clone(),
                        request,
                        error,
                    );
                    self.dialog_stepped(w as i64, k as i64, &next, outcome, &fx);
                    return;
                }
                k += 1;
            }
            w += 1;
        }
    }

    // Closes dialog `k` with exactly one outcome and restores focus.
    fn finish_dialog(&mut self, w: i64, k: i64, outcome: &Outcome) {
        let held = self.layer_of(w, &self.windows[w as usize].focus) == k;
        let inst = self.windows[w as usize].dialogs.remove(k as usize);
        self.windows[w as usize].popups = vec![];
        self.windows[w as usize].layer_focus.remove(&inst.id);
        self.mark_tree(w);
        // A modeless dialog closing without focus leaves focus where it is.
        if inst.modality != Modality::Modeless || held {
            let restore = if inst.modality == Modality::Modeless {
                inst.opener.clone()
            } else {
                self.windows[w as usize]
                    .focus_stack
                    .pop()
                    .unwrap_or("".to_string())
            };
            self.windows[w as usize].focus = String::new();
            if !restore.is_empty() && !self.path_in_layers(w, &restore).is_empty() {
                self.set_focus(w, &restore)
            } else {
                self.update_text_input(w)
            }
        }
        self.windows[w as usize].dirty = true;
        self.log_line(&format!("dialog {} closed", inst.id));
        // A dialog window closes after this answer is delivered; closing it
        // here would move the indexes of the windows the caller still uses.
        let owner = self.windows[w as usize].owner.clone();
        if !owner.is_empty() && self.windows[w as usize].dialogs.is_empty() {
            effects::perform(Effect::CloseWindow(self.windows[w as usize].key.clone()));
            let o = self.find(&owner);
            if o >= 0 {
                let _ = self.windows[o as usize].native.focus();
            }
        }
        // The command palette runs the command it picked.
        if inst.id == "gui:palette" {
            if let Outcome::Action(cmd) = outcome {
                self.run_command(w, cmd)
            }
            return;
        }
        if let Some(value) = crate::dialogs::pickers::rollback(&inst, outcome.clone()) {
            self.pending
                .push(UiMsg::DialogPreview(inst.id.clone(), value))
        }
        self.pending.push(UiMsg::Dialog(inst.id, outcome.clone()))
    }

    fn dialog_cancel(&mut self, w: i64, k: i64) {
        if k < 0 {
            return;
        };
        let inst = &self.windows[w as usize].dialogs[k as usize];
        let (next, outcome) = host::request_cancel(inst.clone());
        self.windows[w as usize].dialogs[k as usize] = next;
        if let Some(o) = outcome {
            self.finish_dialog(w, k, &o)
        } else {
            self.relayout_dialog(w, k)
        }
    }

    fn dialog_default(&mut self, w: i64, k: i64) {
        if k < 0 {
            return;
        };
        let inst = &self.windows[w as usize].dialogs[k as usize];
        let target = host::default_action(inst);
        if !target.is_empty() {
            self.pending.push(UiMsg::Click(target))
        }
    }

    /// Closes the dialog `id` (a finished progress dialog) with `outcome`.
    pub fn close_dialog(&mut self, id: &str, outcome: &Outcome) {
        let mut w = 0;
        while w < self.windows.len() {
            let mut k = 0;
            while k < self.windows[w].dialogs.len() {
                if self.windows[w].dialogs[k].id == id {
                    self.finish_dialog(w as i64, k as i64, outcome);
                    return;
                }
                k += 1;
            }
            w += 1;
        }
    }

    fn native_dialog_closed(
        &mut self,
        native: windowing::dialog::DialogId,
        choice: windowing::dialog::DialogChoice,
    ) {
        let k: u64 = native.into();
        let Some((_, id)) = self.native_dialogs.remove(&(k as i64)) else {
            return;
        };
        let outcome = match choice {
            windowing::dialog::DialogChoice::Chosen { paths, .. } => Outcome::Paths(paths),
            windowing::dialog::DialogChoice::Failed { reason } => Outcome::Failed(reason),
            windowing::dialog::DialogChoice::Cancelled => Outcome::Cancelled,
        };
        self.pending.push(UiMsg::Dialog(id, outcome))
    }

    // ---- rendering ----

    fn settle_layout(&mut self, w: i64) -> Result<(), errors::Error> {
        if self.windows[w as usize].layout_dirty {
            self.layout(w)
        }
        Ok(())
    }

    fn layout(&mut self, w: i64) {
        let (pw, ph) = self.windows[w as usize].native.pixel_size();
        let size = Size::new(pw.max(1), ph.max(1));
        let theme = self.windows[w as usize].theme.clone();
        self.windows[w as usize].root = if self.inspector {
            crate::devtools::inspector::wrap(
                self.windows[w as usize].view.clone(),
                &self.inspector_snapshot(w),
            )
        } else {
            self.windows[w as usize].view.clone()
        };
        let root = self.windows[w as usize].root.clone();
        let mut st = self.take_states(w);
        tree::sync_models(&root, &mut st, &theme);
        self.windows[w as usize].states = st;
        let mut lcx = LayoutCx::new(&theme, &self.windows[w as usize].states, size);
        self.windows[w as usize].node = root.layout(Constraints::tight(size), &mut lcx);
        self.check_unique_ids(w);
        self.check_named_commands(w);
        if self.windows[w as usize].grow_base > 0 {
            let loose = Constraints {
                min_width: size.width,
                max_width: size.width,
                min_height: 0,
                max_height: UNBOUNDED,
            };
            let mut probe = LayoutCx::new(&theme, &self.windows[w as usize].states, size);
            self.windows[w as usize].content_height = root.layout(loose, &mut probe).size().height;
        }
        self.windows[w as usize].node_inspected = self.inspector;
        self.windows[w as usize].layout_dirty = false;
        // Animated widgets get frames while they are laid out.
        let key = self.windows[w as usize].key.clone();
        let wants = !lcx.animating.is_empty() && !theme.reduced_motion;
        let has = self
            .timers
            .iter()
            .any(|t| t.window == *key && t.owner.is_empty() && t.tag == "gui:frame");
        if wants && !has {
            self.timers.push(Timer {
                deadline: self.now() + FRAME_MS,
                window: key.to_string(),
                owner: "".to_string(),
                tag: "gui:frame".to_string(),
                repeat: FRAME_MS,
            })
        } else if !wants && has {
            self.timers = self
                .timers
                .iter()
                .filter(|t| !(t.window == *key && t.owner.is_empty() && t.tag == "gui:frame"))
                .cloned()
                .collect::<Vec<_>>();
        }
        let mut k = 0;
        while k < self.windows[w as usize].dialogs.len() {
            self.relayout_dialog(w, k as i64);
            k += 1;
        }
        self.windows[w as usize].dirty = true;
    }

    /// Lays out (if needed) and paints window `w`.
    pub fn render(&mut self, w: i64) -> Result<(), errors::Error> {
        if self.windows[w as usize].native.is_minimized() || self.windows[w as usize].occluded {
            return Ok(());
        }
        let (pw, ph) = self.windows[w as usize].native.pixel_size();
        if pw <= 0 || ph <= 0 {
            return Ok(());
        }
        let frame = usize::try_from(pw)
            .ok()
            .and_then(|width| {
                usize::try_from(ph)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| errors::new("framebuffer dimensions overflow their byte budget"))?;
        if frame > FRAME_BYTE_LIMIT {
            return Err(errors::new("framebuffer exceeds its byte budget"));
        }
        // The inspector's panel describes the window as it is now.
        if self.windows[w as usize].layout_dirty
            || self.inspector
            || self.windows[w as usize].node.rect.width != pw
            || self.windows[w as usize].node.rect.height != ph
        {
            self.layout(w)
        }
        self.check_memory()?;
        let key = self.windows[w as usize].key.clone();
        let caches = self
            .renderers
            .get(&key)
            .map_or_else(crate::render::retained::Budgets::default, |renderer| {
                renderer.retained.budgets
            });
        let charge = frame
            .saturating_mul(4)
            .saturating_add(caches.scene)
            .saturating_add(caches.layers.saturating_mul(2));
        match self.frame_leases.get_mut(&key) {
            Some(lease) => lease.resize(charge).map_err(errors::new)?,
            None => {
                let lease = self.budget.reserve(charge).map_err(errors::new)?;
                self.frame_leases.insert(key.clone(), lease);
            }
        }
        let recording = self
            .renderers
            .entry(key.clone())
            .or_default()
            .retained
            .recording(pw, ph);
        let mut recorded = self.paint_window(w, recording);
        if let Some(error) = recorded.paint_error.take() {
            return Err(errors::new(error));
        }
        let painted = match recorded.take_scene() {
            Some(scene) => Painted::Scene(scene),
            None => {
                // A scene larger than its budget still paints correctly
                // without retaining an unbounded command list.
                let mut canvas = self.paint_window(w, Canvas::new(pw, ph));
                if let Some(error) = canvas.paint_error.take() {
                    return Err(errors::new(error));
                }
                Painted::Frame(canvas)
            }
        };
        let native = self.windows[w as usize].native;
        let damage = self
            .renderers
            .entry(key)
            .or_default()
            .present(native, pw, ph, painted)?;
        self.windows[w as usize].dirty = false;
        if !damage.is_empty() {
            self.windows[w as usize].frames += 1;
        }
        if self.a11y.active() {
            let sem = self.semantics(w);
            self.windows[w as usize].semantics = sem.clone();
            let (x, y) = self.windows[w as usize].native.position();
            self.a11y.origin(&self.windows[w as usize].key, x, y);
            self.a11y.update(&self.windows[w as usize].key, &sem)
        }
        Ok(())
    }

    fn paint_window(&self, w: i64, mut canvas: Canvas) -> Canvas {
        let (pw, ph) = (canvas.width, canvas.height);
        let win = &self.windows[w as usize];
        let theme = &win.theme;

        canvas.fill(theme.background);
        let mut cx = PaintCx::new(&canvas, theme, &win.states);
        cx.focus = win.focus.clone();
        cx.hover = win.hover.last().cloned().unwrap_or_default();
        cx.pressed = win.pressed.clone();
        cx.focus_visible = win.focus_visible;
        cx.active = win.active;
        // An inactive window takes no typing, so it shows no caret.
        cx.caret_on = win.caret_on && win.active;
        cx.now = self.now();
        cx.registry = win.commands.commands.clone();
        cx.mnemonics = win.mnemonics;
        let root = &win.root;
        cx.origin_x = win.node.rect.x;
        cx.origin_y = win.node.rect.y;
        root.paint(&win.node, &mut cx);
        for d in &win.dialogs {
            if d.modality != Modality::Modeless {
                cx.canvas.rect(0, 0, pw, ph, theme.scrim)
            };
            let r = d.node.rect;
            let (drop, blur) = theme.shadow_of(theme.shape.dialog_elevation);
            cx.canvas.shadow(
                r.x,
                r.y + drop,
                r.width,
                r.height,
                theme.radius(),
                blur,
                theme.shadow,
            );
            cx.origin_x = r.x;
            cx.origin_y = r.y;
            d.root.paint(&d.node, &mut cx)
        }
        for p in &win.popups {
            let path = self.path_in_layers(w, &p.request.owner);
            if path.is_empty() {
                continue;
            };
            let owner = &path[path.len() - 1];
            cx.origin_x = owner.x;
            cx.origin_y = owner.y;
            cx.theme = owner.theme.clone();
            owner.widget.popup_paint(&p.request, p.rect, &mut cx);
            cx.theme = theme.clone();
        }
        cx.canvas
    }

    /// Sets per-window retained-scene and raster/GPU-layer accounting budgets.
    /// Each cache is independently bounded; persistent surfaces are not caches.
    pub fn set_render_budgets(
        &mut self,
        w: i64,
        scene: usize,
        layers: usize,
    ) -> Result<(), errors::Error> {
        let key = self
            .windows
            .get(w as usize)
            .ok_or_else(|| errors::new("no such rendering window"))?
            .key
            .clone();
        let (width, height) = self.windows[w as usize].native.pixel_size();
        let frame = (width.max(0) as usize)
            .saturating_mul(height.max(0) as usize)
            .saturating_mul(4);
        let charge = frame
            .saturating_mul(4)
            .saturating_add(scene)
            .saturating_add(layers.saturating_mul(2));
        match self.frame_leases.get_mut(&key) {
            Some(lease) => lease.resize(charge).map_err(errors::new)?,
            None => {
                self.frame_leases.insert(
                    key.clone(),
                    self.budget.reserve(charge).map_err(errors::new)?,
                );
            }
        }
        let renderer = self.renderers.entry(key).or_default();
        renderer.purge();
        renderer.invalidate();
        renderer.retained.budgets = crate::render::retained::Budgets { scene, layers };
        self.windows[w as usize].dirty = true;
        Ok(())
    }

    /// Rendering counters for the last frame, including byte accounting.
    pub fn rendering_stats(&self, w: i64) -> Option<&crate::render::retained::Stats> {
        self.windows
            .get(w as usize)
            .and_then(|win| self.renderers.get(&win.key))
            .map(|r| &r.retained.stats)
    }

    /// The accessibility tree of window `w`; modal dialogs are marked so.
    pub fn semantics(&self, w: i64) -> AccessNode {
        let win = &self.windows[w as usize];
        let mut root = tree::semantics(
            &win.root,
            &win.node,
            &win.states,
            &win.theme,
            &win.focus,
            &win.title,
        );
        // Screen readers present focus only inside the active window.
        root.active = win.active;
        for d in &win.dialogs {
            let mut dn = tree::semantics(
                &d.root,
                &d.node,
                &win.states,
                &win.theme,
                &win.focus,
                &d.title(),
            );
            dn.role = host::dialog_role(d);
            dn.modal = d.modality != Modality::Modeless;
            dn.id = format!("dlg:{}", d.id);
            root.children.push(dn)
        }
        for p in &win.popups {
            let path = self.path_in_layers(w, &p.request.owner);
            if path.is_empty() {
                continue;
            };
            if let Some(n) = path[path.len() - 1].widget.popup_semantics(
                &p.request,
                &win.states,
                &win.commands.commands,
            ) {
                let mut panel = n;
                panel.bounds = p.rect;
                root.children.push(panel)
            }
        }
        root
    }

    // ---- test and automation helpers ----

    /// The current tree of the main window, after pending work settles.
    pub fn accessibility_status(&self) -> String {
        self.a11y.status()
    }
    pub fn accessibility_endpoint(&self) -> String {
        self.a11y.endpoint()
    }

    pub fn root(&self) -> Widget {
        if self.windows.is_empty() {
            Widget::Empty
        } else {
            self.windows[0].root.clone()
        }
    }

    /// Sends a platform event as if it had arrived, and settles.
    pub fn inject(&mut self, e: &windowing::Event) -> Result<(), errors::Error> {
        self.handle(e)?;
        self.settle()
    }

    /// Delivers the main window gaining (`true`) or losing keyboard focus
    /// to another window, and settles.
    pub fn set_active(&mut self, active: bool) -> Result<(), errors::Error> {
        if self.windows.is_empty() {
            return Ok(());
        };
        let window = self.windows[0].native.id();
        self.inject(&if active {
            windowing::Event::FocusGained { window }
        } else {
            windowing::Event::FocusLost { window }
        })
    }

    // The window the drivers below act on, as input would reach it: the
    // main window's open dialog window, else the main window.
    fn driven(&self) -> i64 {
        let main = &self.windows[0].key;
        self.windows
            .iter()
            .rposition(|win| win.owner == *main && !win.dialogs.is_empty())
            .map_or(0, |d| d as i64)
    }

    /// The window-pixel rectangle of widget `id` in the main window.
    pub fn rect_of(&self, id: &str) -> Option<Rect> {
        if self.windows.is_empty() {
            return None;
        };
        let path = self.path_in_layers(self.driven(), id);
        if path.is_empty() {
            None
        } else {
            Some(path[path.len() - 1].rect())
        }
    }

    /// Clicks the centre of widget `id` in the main window.
    pub fn click(&mut self, id: &str) -> Result<bool, errors::Error> {
        self.settle()?;
        let Some(r) = self.rect_of(id) else {
            return Ok(false);
        };
        let x = r.center_x();
        let y = r.center_y();
        self.pointer_down(self.driven(), x, y, MouseButton::Left, 1, Mods::default());
        self.pointer_up(self.driven(), x, y, MouseButton::Left, Mods::default());
        self.settle()?;
        Ok(true)
    }

    /// Clicks window pixel (`x`, `y`) of the main window.
    pub fn click_at(&mut self, x: i64, y: i64) -> Result<(), errors::Error> {
        self.settle()?;
        self.pointer_down(self.driven(), x, y, MouseButton::Left, 1, Mods::default());
        self.pointer_up(self.driven(), x, y, MouseButton::Left, Mods::default());
        self.settle()
    }

    /// Moves the pointer, no button held, to main-window pixels (`x`, `y`).
    pub fn hover(&mut self, x: i64, y: i64) -> Result<(), errors::Error> {
        self.settle()?;
        self.pointer_move(self.driven(), x, y, false, Mods::default());
        self.settle()
    }

    /// Turns the wheel `dy` notches (positive away from the user, which
    /// scrolls up) over main-window pixels (`x`, `y`).
    pub fn scroll_at(&mut self, x: i64, y: i64, dy: f64) -> Result<(), errors::Error> {
        self.settle()?;
        self.wheel(self.driven(), x, y, 0.0, dy, Mods::default());
        self.settle()
    }

    /// The system cursor the session last asked the platform for.
    pub fn cursor(&self) -> Option<windowing::SystemCursor> {
        self.cursor_kind
    }

    /// Drags the pointer in main-window pixels through the normal capture path.
    pub fn drag(&mut self, x: i64, y: i64, to_x: i64, to_y: i64) -> Result<(), errors::Error> {
        self.settle()?;
        self.pointer_down(self.driven(), x, y, MouseButton::Left, 1, Mods::default());
        self.settle()?;
        self.pointer_move(self.driven(), to_x, to_y, true, Mods::default());
        self.settle()?;
        self.pointer_up(
            self.driven(),
            to_x,
            to_y,
            MouseButton::Left,
            Mods::default(),
        );
        self.settle()
    }

    /// Runs an assistive-technology `action` ("press", "set-value", ...) on
    /// the semantic node `target` of widget `owner` in the main window, as the
    /// platform accessibility bridge delivers it.
    pub fn accessible_action(
        &mut self,
        owner: &str,
        target: &str,
        action: &str,
        value: &str,
    ) -> Result<(), errors::Error> {
        self.settle()?;
        self.send_to(
            0,
            owner,
            &Event::Accessible {
                target: target.to_string(),
                action: action.to_string(),
                value: value.to_string(),
                start: 0,
                end: 0,
            },
        );
        self.settle()
    }

    /// Presses `key` with `mods` in the main window without releasing it;
    /// `repeat` marks an auto-repeat.
    pub fn hold_key(&mut self, key: Key, mods: Mods, repeat: bool) -> Result<(), errors::Error> {
        self.settle()?;
        self.key_down(self.driven(), key, mods, repeat)?;
        self.settle()
    }

    /// Releases `key` in the main window.
    pub fn release_key(&mut self, key: Key, mods: Mods) -> Result<(), errors::Error> {
        self.settle()?;
        self.key_up(self.driven(), key, mods);
        self.settle()
    }

    /// Presses the left button at main-window pixels (`x`, `y`) and holds it.
    pub fn press_pointer(&mut self, x: i64, y: i64) -> Result<(), errors::Error> {
        self.settle()?;
        self.pointer_down(self.driven(), x, y, MouseButton::Left, 1, Mods::default());
        self.settle()
    }

    /// Clicks main-window pixels (`x`, `y`) with `mods` held, as a
    /// Shift-click or Ctrl-click does.
    pub fn click_with(&mut self, x: i64, y: i64, mods: Mods) -> Result<(), errors::Error> {
        self.settle()?;
        self.pointer_down(self.driven(), x, y, MouseButton::Left, 1, mods);
        self.settle()?;
        self.pointer_up(self.driven(), x, y, MouseButton::Left, mods);
        self.settle()
    }

    /// Moves the pointer to main-window pixels (`x`, `y`), `held` with the left button down.
    pub fn move_pointer(&mut self, x: i64, y: i64, held: bool) -> Result<(), errors::Error> {
        self.settle()?;
        self.pointer_move(self.driven(), x, y, held, Mods::default());
        self.settle()
    }

    /// Releases the left button at main-window pixels (`x`, `y`).
    pub fn release_pointer(&mut self, x: i64, y: i64) -> Result<(), errors::Error> {
        self.settle()?;
        self.pointer_up(self.driven(), x, y, MouseButton::Left, Mods::default());
        self.settle()
    }

    /// Presses and releases `key` with `mods` in the main window.
    pub fn press(&mut self, key: Key, mods: Mods) -> Result<(), errors::Error> {
        self.settle()?;
        self.key_down(self.driven(), key, mods, false)?;
        // A key that closed the window is released over nothing.
        if !self.windows.is_empty() {
            self.key_up(self.driven(), key, mods)
        };
        self.settle()
    }

    /// Commits `text` as typed input in the main window. Like the platform,
    /// it delivers text only while the focused widget has turned text input
    /// on, and fails otherwise.
    pub fn type_text(&mut self, text: &str) -> Result<(), errors::Error> {
        self.settle()?;
        self.require_text_input("type_text")?;
        self.send_to_focus(self.driven(), &Event::Text(text.to_string()));
        self.settle()
    }

    fn require_text_input(&self, what: &str) -> Result<(), errors::Error> {
        let w = self.driven();
        if w < 0 || !self.windows[w as usize].native.is_text_input_active() {
            return Err(errors::new(format!(
                "{what}: the focused widget takes no typed text, so the window's text input is off"
            )));
        }
        Ok(())
    }

    /// Sends an input-method preedit to the main window, which the platform
    /// only does while text input is on.
    pub fn compose(&mut self, text: &str, cursor: i64) -> Result<(), errors::Error> {
        self.settle()?;
        self.require_text_input("compose")?;
        self.send_to_focus(
            self.driven(),
            &Event::Composing {
                text: text.to_string(),
                cursor,
                selection: 0,
            },
        );
        self.settle()
    }

    /// Focuses widget `id` in the main window; false when it is not focusable.
    pub fn focus(&mut self, id: &str) -> Result<bool, errors::Error> {
        self.settle()?;
        if self.windows.is_empty() {
            return Ok(false);
        };
        let layer = self.layer_of(self.driven(), id);
        if !self.reachable(self.driven(), layer) {
            return Ok(false);
        };
        let (root, node) = self.layer_tree(self.driven(), layer);
        let chain = tree::focus_chain(&root, &node, "");
        if !chain.iter().any(|c| c == id) {
            return Ok(false);
        };
        self.set_focus(self.driven(), id);
        self.settle()?;
        Ok(true)
    }

    /// The focused widget id of the main window.
    pub fn focused(&self) -> String {
        if self.windows.is_empty() {
            String::new()
        } else {
            self.windows[self.driven() as usize].focus.clone()
        }
    }

    /// Advances the session clock by `ms`, firing each deadline in order on
    /// a clock that moves only by those steps, so a repeating timer fires
    /// once per period however long its handlers take.
    pub fn advance(&mut self, ms: i64) -> Result<(), errors::Error> {
        let start = self.now();
        self.clock_held = Some(start);
        let stepped = self.step_held_clock(ms.max(0));
        let end = self.clock_held.take().unwrap_or(start);
        // The real clock continues from where the advance left the session's.
        self.clock_skew += end - self.now();
        stepped
    }

    fn step_held_clock(&mut self, mut left: i64) -> Result<(), errors::Error> {
        loop {
            let now = self.now();
            let next = self
                .timers
                .iter()
                .filter(|t| !self.visual_timer_paused(t))
                .map(|t| (t.deadline - now).max(0))
                .filter(|offset| *offset <= left)
                .min();
            let step = next.unwrap_or(left);
            self.clock_held = Some(now + step);
            left -= step;
            self.fire_timers()?;
            self.settle()?;
            if next.is_none() || left == 0 {
                return Ok(());
            }
        }
    }

    /// Open dialogs of the main window, by id.
    pub fn dialog_ids(&self) -> Vec<String> {
        if self.windows.is_empty() {
            vec![]
        } else {
            let main = &self.windows[0].key;
            self.windows
                .iter()
                .filter(|win| win.key == *main || win.owner == *main)
                .flat_map(|win| win.dialogs.iter().map(|d| d.id.clone()))
                .collect::<Vec<_>>()
        }
    }

    /// Open popups of the main window: (owner, level).
    pub fn popup_owners(&self) -> Vec<(String, i64)> {
        if self.windows.is_empty() {
            vec![]
        } else {
            self.windows[self.driven() as usize]
                .popups
                .iter()
                .map(|p| (p.request.owner.clone(), p.request.level))
                .collect::<Vec<_>>()
        }
    }

    /// Queues a message as if a widget had emitted it.
    pub fn emit(&mut self, m: &UiMsg) {
        let bytes = crate::memory::measure(m);
        if self.pending.len() >= 256 {
            self.resource_error = "pending message queue is full".into();
            return;
        }
        match self.budget.reserve(bytes) {
            Ok(lease) => {
                self.pending.push(m.clone());
                self.pending_leases.push(lease);
            }
            Err(error) => self.resource_error = error,
        }
    }

    /// Runs settled work: messages, effects, mailbox posts, and repaints.
    pub fn flush(&mut self) -> Result<(), errors::Error> {
        self.settle()
    }

    /// Writes a PNG of the main window to `file`.
    pub fn screenshot(&mut self, file: &str) -> Result<(), errors::Error> {
        let c = self.capture()?;
        Ok(fs::write(file, crate::render::to_png(&c))?)
    }

    /// The canvas pixels of the main window's last frame, by repainting.
    pub fn capture(&mut self) -> Result<Canvas, errors::Error> {
        self.capture_window(0)
    }

    /// The canvas pixels of window `w`'s last frame, by repainting.
    pub fn capture_window(&mut self, w: i64) -> Result<Canvas, errors::Error> {
        self.settle()?;
        if w < 0 || w >= self.windows.len() as i64 {
            return Err(errors::new(format!("no window {}", w)));
        };
        let native = self.windows[w as usize].native;
        if let Some(renderer) = self.renderers.get_mut(&self.windows[w as usize].key)
            && let Some(pixels) = renderer.gpu_capture()?
        {
            let mut canvas = Canvas::new(renderer.retained.width, renderer.retained.height);
            canvas.pixels = pixels;
            return Ok(canvas);
        }
        let frame = native.capture()?;
        let mut c = Canvas::new(frame.width, frame.height);
        let (ro, go, bo) = frame.format.rgb_offsets();
        let mut y = 0;
        while y < frame.height {
            let mut x = 0;
            while x < frame.width {
                let s = y * frame.stride + x * 4;
                let d = (y * frame.width + x) * 4;
                c.pixels[d as usize] = frame.pixels[(s + ro as i64) as usize];
                c.pixels[(d + 1) as usize] = frame.pixels[(s + go as i64) as usize];
                c.pixels[(d + 2) as usize] = frame.pixels[(s + bo as i64) as usize];
                c.pixels[(d + 3) as usize] = 255;
                x += 1;
            }
            y += 1;
        }
        Ok(c)
    }
}

fn is_motion(e: &windowing::Event) -> bool {
    matches!(e, windowing::Event::PointerMoved { .. })
}

/// The widget id a message names.
pub fn msg_id(m: &UiMsg) -> String {
    match m {
        UiMsg::Click(id)
        | UiMsg::Toggle(id, _)
        | UiMsg::Check(id, _)
        | UiMsg::Choose(id, _)
        | UiMsg::Text(id, _)
        | UiMsg::Caret(id, _, _)
        | UiMsg::DocumentChanged(id, _, _)
        | UiMsg::Submit(id, _)
        | UiMsg::Select(id, _)
        | UiMsg::SelectMany(id, _)
        | UiMsg::Value(id, _)
        | UiMsg::Range(id, _, _)
        | UiMsg::Color(id, _)
        | UiMsg::Date(id, _)
        | UiMsg::Tab(id, _)
        | UiMsg::TabAction(id, _, _)
        | UiMsg::TabDrop(id, _, _, _)
        | UiMsg::TabMove(id, _, _, _)
        | UiMsg::TabDetach(id, _, _, _)
        | UiMsg::Expand(id, _, _)
        | UiMsg::Activate(id, _)
        | UiMsg::CellEdit(id, _, _, _)
        | UiMsg::Sort(id, _, _)
        | UiMsg::ColumnResize(id, _, _)
        | UiMsg::Reorder(id, _, _)
        | UiMsg::Split(id, _)
        | UiMsg::DragFinished(id, _)
        | UiMsg::Dropped(id, _)
        | UiMsg::CompletionRequest(id, _, _, _)
        | UiMsg::Custom(id, _)
        | UiMsg::Worked(id, _, _, _)
        | UiMsg::Listed(id, _, _)
        | UiMsg::FontsLoaded(id, _)
        | UiMsg::AppsLoaded(id, _) => id.to_string(),
        _ => String::new(),
    }
}

// The first menu bar in `w`, depth first.
fn find_menu_bar(w: &Widget) -> Option<menus::MenuBar> {
    if let Widget::MenuBar(b) = w {
        return Some(b.clone());
    }
    for child in w.children() {
        if let Some(b) = find_menu_bar(child) {
            return Some(b);
        }
    }
    None
}

// Placements store sizes in interface units, so a window keeps its
// apparent size when the display scale changes between runs.
fn placement_of(win: &WindowState) -> services::WindowPlacement {
    let (x, y) = win.native.position();
    let (_lw, lh) = win.native.size();
    let (pw, pixel_height) = win.native.pixel_size();
    // A window that grows for its popups keeps its base height.
    let ph = if win.grow_base > 0 {
        window_extent(win.grow_base, pixel_height, lh)
    } else {
        pixel_height
    };
    let scale = win.theme.scale.max(0.5);
    services::WindowPlacement {
        x,
        y,
        width: (pw as f64 / scale).round() as i64,
        height: (ph as f64 / scale).round() as i64,
        maximized: win.native.is_maximized(),
    }
}

// Requested window sizes are pixels at the interface scale; SDL sizes are
// logical units whose pixel density is independent of the interface scale.
pub fn window_extent(pixels: i64, logical: i64, physical: i64) -> i64 {
    (pixels as f64 * logical as f64 / physical.max(1) as f64).round() as i64
}

impl std::fmt::Display for WindowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Window({})", self.key)
    }
}

impl std::fmt::Debug for WindowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Window {{ key: {:?}, title: {:?}, focus: {:?} }}",
            self.key, self.title, self.focus
        )
    }
}

impl std::fmt::Display for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Session({})", self.app_id)
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Session {{ app_id: {:?}, windows: {} }}",
            self.app_id,
            self.windows.len()
        )
    }
}

impl std::fmt::Display for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Shell")
    }
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Shell")
    }
}

#[cfg(test)]
mod popup_tests {

    use crate::widgets::cx::UiMsg;
    use crate::widgets::{Widget, menus};
    use windowing::{Key, Mods};

    fn view(_m: &i64) -> Widget {
        Widget::column(
            &[Widget::button("target", "Target").context_menu(
                "ctx",
                &[
                    menus::labeled("edit.copy", "Copy"),
                    menus::separator(),
                    menus::labeled("edit.paste", "Paste"),
                ],
            )],
            8,
        )
        .padding(8)
    }

    #[test]
    fn an_open_context_menu_is_in_the_semantic_tree() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Popups",
                "dev.gossamer.gui-popup-test",
                || 0,
                |m: i64, _msg: UiMsg| m,
                view,
            )
            .size(420, 320)
            .start()
            .unwrap();
            assert!(s.focus("target").unwrap());
            assert!(
                !s.semantics(0).dump().contains("menuitem"),
                "{}",
                s.semantics(0).dump()
            );
            s.press(
                Key::F10,
                Mods {
                    shift: true,
                    ..Mods::default()
                },
            )
            .unwrap();
            assert_eq!(
                s.popup_owners()
                    .iter()
                    .map(|p| p.0.clone())
                    .collect::<Vec<_>>(),
                vec!["ctx"]
            );
            let dump = s.semantics(0).dump();
            assert!(
                dump.contains("menu\n") || dump.contains("menu ["),
                "{}",
                dump
            );
            assert!(
                dump.contains("menuitem \"Copy\"") && dump.contains("menuitem \"Paste\""),
                "{}",
                dump
            );
            s.press(Key::Escape, Mods::default()).unwrap();
            assert!(
                !s.semantics(0).dump().contains("menuitem"),
                "{}",
                s.semantics(0).dump()
            );
            s.shutdown()
        })
    }
}

#[cfg(test)]
mod dialog_scope_tests {
    use crate::app::effects::Effect;
    use crate::dialogs::Outcome;
    use crate::dialogs::request;
    use crate::dialogs::request::Modality;
    use crate::paths;
    use crate::strext::StrExt;
    use crate::widgets::Widget;
    use crate::widgets::cx::UiMsg;
    use std::{env, fs};
    use windowing::{Key, Mods};

    #[derive(Debug, Clone, Default)]
    struct Log {
        events: Vec<String>,
    }

    fn log_update(m: Log, msg: UiMsg) -> Log {
        let mut n = m;
        match msg {
            UiMsg::Click(id) => n.events.push(id.to_string()),
            UiMsg::Dialog(id, outcome) => n.events.push(format!("{}={:?}", id, outcome)),
            UiMsg::Listed(id, request, found) => {
                n.events
                    .push(format!("{}#{}={}", id, request, found.entries.len()))
            }
            UiMsg::FontsLoaded(id, _) => n.events.push(format!("{} fonts", id)),
            UiMsg::DialogCheck(id, request, step, _) => {
                n.events.push(format!("check {}#{} {}", id, request, step))
            }
            UiMsg::DialogCheckStopped(id, request) => {
                n.events.push(format!("stopped {}#{}", id, request))
            }
            _ => (),
        }
        n
    }

    fn log_view(m: &Log) -> Widget {
        Widget::column(
            &[
                Widget::text_field("name", "", "Name"),
                Widget::button("go", "Go"),
                Widget::label(&format!("log {}", m.events.join("; "))),
            ],
            8,
        )
        .padding(8)
    }

    fn tools_view(_m: &Log, _key: &str) -> Widget {
        Widget::button("tool", "Tool")
    }

    fn events(s: &crate::Session) -> String {
        let dump = s.semantics(0).dump();
        let at = dump.find("log ").unwrap_or(0);
        let rest = dump.substring((at + 4) as i64, dump.byte_len());
        rest.substring(
            0,
            (rest.find("\"").unwrap_or(rest.byte_len() as usize)) as i64,
        )
        .to_string()
    }

    #[test]
    fn shortcut_dialog_captures_ctrl_home_and_keeps_actions_visible_and_clickable() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Shortcuts",
                "dev.gossamer.gui-shortcut-test",
                Log::default,
                log_update,
                log_view,
            )
            .size(300, 120)
            .windows(tools_view)
            .start()
            .unwrap();
            s.open_window("settings", "Settings", 480, 270).unwrap();
            s.set_focus(1, "tool");
            let req = request::DialogRequest::Shortcut {
                id: "capture".to_string(),
                title: "Launch shortcut".to_string(),
                initial: "Ctrl+Space".to_string(),
                reserved: vec!["Ctrl+Q".to_string()],
            };
            let inject = |s: &mut crate::Session, key, mods| {
                let d = s.windows.len() - 1;
                s.inject(&windowing::Event::KeyDown {
                    window: s.windows[d].native.id(),
                    key,
                    scancode: windowing::Scancode::Unknown,
                    mods,
                    repeat: false,
                })
                .unwrap();
            };
            let click = |s: &mut crate::Session, part: &str| {
                let d = s.windows.len() as i64 - 1;
                let path = s.path_in_layers(d, &format!("dlg:capture:{part}"));
                let target = path.last().unwrap();
                let x = target.x + target.node.rect.width / 2;
                let y = target.y + target.node.rect.height / 2;
                for parent in &path {
                    let rect = crate::geometry::Rect::new(
                        parent.x,
                        parent.y,
                        parent.node.rect.width,
                        parent.node.rect.height,
                    );
                    assert!(
                        rect.contains(x, y),
                        "{part} outside hit-test ancestor: {rect:?}"
                    );
                }
                s.pointer_down(d, x, y, windowing::MouseButton::Left, 1, Mods::default());
                s.pointer_up(d, x, y, windowing::MouseButton::Left, Mods::default());
                s.flush().unwrap();
            };
            s.open_dialog_in(1, &req);
            s.flush().unwrap();
            assert_eq!(s.windows.len(), 3);
            let d = 2;
            assert_eq!(s.windows[d].owner, "settings");
            assert_eq!(s.windows[d].focus, "dlg:capture:capture");
            s.windows[d].native.set_size(480, 340).unwrap();
            s.windows[d].native.sync().unwrap();
            s.invalidate(d as i64, "test minimum shortcut size");
            s.flush().unwrap();
            for part in ["capture", "action:clear", "action:ok", "action:cancel"] {
                let path = s.path_in_layers(d as i64, &format!("dlg:capture:{part}"));
                let target = path.last().unwrap();
                assert!(target.x >= 0 && target.y >= 0);
                assert!(target.x + target.node.rect.width <= s.windows[d].node.rect.width);
                assert!(target.y + target.node.rect.height <= s.windows[d].node.rect.height);
                if let Widget::Button(button) = &target.widget {
                    let width = crate::text::layout::measure(
                        &button.label,
                        &target.theme.font(crate::style::TextRole::Body),
                    )
                    .0;
                    assert!(
                        width <= target.node.rect.width as f64,
                        "button label overflows {part}"
                    );
                }
            }
            let ctrl = Mods {
                ctrl: true,
                ..Mods::default()
            };
            inject(&mut s, Key::LeftCtrl, ctrl);
            assert_eq!(s.windows[d].dialogs[0].text, "Ctrl+Space");
            inject(&mut s, Key::Home, ctrl);
            let ctrl_home = crate::input::Shortcut::parse("Ctrl+Home").unwrap().label();
            assert_eq!(s.windows[d].dialogs[0].text, ctrl_home);
            if let Ok(path) = env::var("FEATHER_SHORTCUT_SCREENSHOT") {
                fs::write(
                    path,
                    crate::render::to_png(&s.capture_window(d as i64).unwrap()),
                )
                .unwrap();
            }
            click(&mut s, "action:ok");
            assert_eq!(s.windows.len(), 2);
            assert!(
                s.semantics(0).dump().contains(&ctrl_home),
                "{}",
                s.semantics(0).dump()
            );
            s.open_dialog_in(1, &req);
            s.flush().unwrap();
            inject(&mut s, Key::Q, ctrl);
            assert!(!s.windows[d].dialogs[0].error.is_empty());
            click(&mut s, "action:ok");
            assert_eq!(
                s.windows.len(),
                3,
                "a conflicting shortcut must not be saved"
            );
            click(&mut s, "action:clear");
            assert!(s.windows[d].dialogs[0].text.is_empty());
            assert!(s.windows[d].dialogs[0].error.is_empty());
            click(&mut s, "capture");
            inject(&mut s, Key::Home, ctrl);
            inject(&mut s, Key::Return, Mods::default());
            assert_eq!(
                s.windows.len(),
                2,
                "Enter should save the recorded shortcut"
            );
            s.open_dialog_in(1, &req);
            s.flush().unwrap();
            inject(&mut s, Key::Escape, Mods::default());
            assert_eq!(s.windows.len(), 2);
            assert!(s.semantics(0).dump().contains("Cancelled"));
            s.shutdown();
        })
    }

    #[test]
    fn escape_cancels_a_dialog_whose_focused_field_has_a_selection() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Escape",
                "dev.gossamer.gui-escape-test",
                Log::default,
                log_update,
                log_view,
            )
            .size(640, 520)
            .start()
            .unwrap();
            for req in [
                request::color(
                    "color",
                    "Choose a color",
                    crate::style::color::Color::rgb(30, 120, 200),
                ),
                request::prompt("rename", "Rename", "New name", "Meeting notes.txt"),
            ] {
                let id = req.id();
                crate::perform(Effect::OpenDialog(req));
                s.flush().unwrap();
                assert_eq!(s.dialog_ids(), vec![id.clone()]);
                let all = if crate::input::platform_is_macos() {
                    Mods {
                        gui: true,
                        ..Mods::default()
                    }
                } else {
                    Mods {
                        ctrl: true,
                        ..Mods::default()
                    }
                };
                s.press(Key::A, all).unwrap();
                s.press(Key::Escape, Mods::default()).unwrap();
                s.flush().unwrap();
                assert!(s.dialog_ids().is_empty(), "{id} stayed open");
            }
            s.shutdown()
        })
    }

    #[test]
    fn file_dialogs_open_in_a_window_of_their_own_sized_for_browsing() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Small",
                "dev.gossamer.gui-file-window-test",
                Log::default,
                log_update,
                log_view,
            )
            .size(300, 120)
            .start()
            .unwrap();
            // Automatic choosers use the toolkit's own browser without a desktop.
            for req in [
                request::open_file("open", "Open", &[]),
                request::toolkit_file(request::select_folder("folder", "Choose Folder")),
            ] {
                let id = req.id();
                crate::perform(Effect::OpenDialog(req));
                s.flush().unwrap();
                assert_eq!(s.windows.len(), 2, "{id}");
                let d = &s.windows[1];
                assert_eq!(d.owner, "main");
                let (width, height) = super::FILE_DIALOG_SIZE;
                let theme = &d.theme;
                assert!(
                    d.node.rect.width >= theme.px(width) - 1,
                    "{:?}",
                    d.node.rect
                );
                assert!(
                    d.node.rect.height >= theme.px(height) - 1,
                    "{:?}",
                    d.node.rect
                );
                // The dialog fills its window, which the small window could not hold.
                assert_eq!(d.dialogs[0].node.rect.size(), d.node.rect.size());
                assert!(d.node.rect.width > s.windows[0].node.rect.width);
                assert!(s.blocked(0));
                assert_eq!(s.dialog_ids(), vec![id.clone()]);
                s.press(Key::Escape, Mods::default()).unwrap();
                s.flush().unwrap();
                assert_eq!(s.windows.len(), 1, "{id}");
                assert!(!s.blocked(0));
                assert!(s.dialog_ids().is_empty());
            }
            s.shutdown()
        })
    }

    #[test]
    fn theme_tokens_reach_every_window_and_outlast_a_theme_change() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Scopes",
                "dev.gossamer.gui-dialog-scope-test",
                Log::default,
                log_update,
                log_view,
            )
            .size(640, 480)
            .start()
            .unwrap();
            let mut tokens = crate::style::ThemeTokens::standard();
            tokens.shape.radius = 11;
            tokens.motion.caret_blink = 700;
            crate::perform(Effect::SetThemeTokens(tokens.clone()));
            crate::perform(Effect::SetTheme(crate::style::ThemeKind::Dark));
            s.flush().unwrap();
            assert!(s.windows[0].theme.is_dark());
            assert_eq!(s.windows[0].theme.tokens(), tokens);
            assert!(s.focus("name").unwrap());
            assert!(
                s.timers
                    .iter()
                    .any(|t| t.tag == "gui:caret" && t.repeat == 700)
            );
            s.shutdown()
        })
    }

    fn prefs(
        high: Option<bool>,
        motion: Option<bool>,
        accent: Option<(u8, u8, u8)>,
        text: Option<f64>,
    ) -> windowing::system::Preferences {
        windowing::system::Preferences {
            high_contrast: high,
            reduced_motion: motion,
            accent,
            text_scale: text,
            menu_font_size: None,
        }
    }

    #[test]
    fn desktop_preferences_pick_the_palette_motion_accent_and_text_size() {
        let base = crate::style::Theme::light();
        let t = super::with_preferences(
            base.clone(),
            prefs(Some(true), Some(true), Some((61, 174, 233)), Some(1.5)),
            Some(true),
            true,
            false,
            false,
        );
        assert_eq!(t.kind, crate::style::ThemeKind::HighContrastDark);
        assert!(t.reduced_motion);
        assert_eq!(t.text_scale, 1.5);
        // A high-contrast palette keeps its accent.
        assert_eq!(t.accent, crate::style::Theme::high_contrast_dark().accent);
        let accented = super::with_preferences(
            base.clone(),
            prefs(Some(false), None, Some((61, 174, 233)), None),
            Some(false),
            true,
            false,
            false,
        );
        assert_eq!(accented.kind, crate::style::ThemeKind::Light);
        assert!(accented.accent != base.accent);
        assert!(accented.accent_text.contrast(accented.accent) >= 4.5);
        assert!(accented.accent.contrast(accented.background) >= 3.0);
        assert!(accented.selection_text.contrast(accented.selection) >= 4.5);
        // An application's own theme, text size and motion choices win.
        let mut chosen = crate::style::Theme::dark();
        chosen.text_scale = 1.25;
        let kept = super::with_preferences(
            chosen.clone(),
            prefs(Some(true), Some(true), Some((61, 174, 233)), Some(2.0)),
            Some(false),
            false,
            true,
            true,
        );
        assert_eq!(kept, chosen);
        // Unknown preferences change nothing.
        assert_eq!(
            super::with_preferences(
                base.clone(),
                prefs(None, None, None, None),
                None,
                true,
                false,
                false
            ),
            base
        )
    }

    #[test]
    fn the_desktop_menu_font_size_sets_menu_text_and_leaves_body_text() {
        let base = crate::style::Theme::light();
        let mut desktop = prefs(None, None, None, None);
        desktop.menu_font_size = Some(11.0);
        let t = super::with_preferences(base.clone(), desktop, None, false, false, false);
        let menu = t.font(crate::style::TextRole::Menu);
        assert!((menu.size - 11.0 * 96.0 / 72.0 * t.scale * t.text_scale).abs() < 1e-9);
        assert_eq!(
            t.font(crate::style::TextRole::Body),
            base.font(crate::style::TextRole::Body)
        );
        // A size no desktop uses is ignored.
        desktop.menu_font_size = Some(0.0);
        let kept = super::with_preferences(base.clone(), desktop, None, false, false, false);
        assert_eq!(kept, base);
    }

    #[test]
    fn a_pale_desktop_accent_is_deepened_until_controls_show() {
        for t in [crate::style::Theme::light(), crate::style::Theme::dark()] {
            for rgb in [(255, 255, 160), (20, 20, 30), (61, 174, 233)] {
                let (r, g, b) = rgb;
                let a = t.with_accent(crate::style::color::Color::rgb(r, g, b));
                assert!(
                    a.accent.contrast(a.background) >= 3.0,
                    "{:?} on {:?}",
                    rgb,
                    t.kind
                );
                assert!(
                    a.accent_text.contrast(a.accent) >= 4.5,
                    "{:?} on {:?}",
                    rgb,
                    t.kind
                );
                assert!(
                    a.selection_text.contrast(a.selection) >= 4.5,
                    "{:?} on {:?}",
                    rgb,
                    t.kind
                )
            }
        }
    }

    #[test]
    fn a_listing_worker_answers_through_the_mailbox_without_holding_up_the_session() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let folder = paths::join(&config, "listed");
            fs::create_dir_all(paths::join(&folder, "inner")).unwrap();
            let mut s = crate::application(
                "Scopes",
                "dev.gossamer.gui-dialog-scope-test",
                Log::default,
                log_update,
                log_view,
            )
            .size(640, 480)
            .start()
            .unwrap();
            s.jobs = Some(crate::services::jobs::Worker::new(s.mailbox()).unwrap());
            crate::perform(Effect::ListDirectory(
                crate::services::listing::ListRequest::new("probe", 7, &folder),
            ));
            s.flush().unwrap();
            let mut steps = 0;
            while !events(&s).contains("probe#7=1") && steps < 200 {
                let _ = s.step().unwrap();
                steps += 1;
            }
            s.jobs = None;
            assert!(events(&s).contains("probe#7=1"), "{}", events(&s));
            s.shutdown()
        })
    }

    #[test]
    fn jobs_asked_for_before_the_loop_wait_for_its_worker() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let folder = paths::join(&config, "held");
            fs::create_dir_all(paths::join(&folder, "inner")).unwrap();
            let mut s = crate::application(
                "Scopes",
                "dev.gossamer.gui-dialog-scope-test",
                Log::default,
                log_update,
                log_view,
            )
            .size(640, 480)
            .start()
            .unwrap();
            s.hold_jobs_for_run();
            crate::perform(Effect::ListDirectory(
                crate::services::listing::ListRequest::new("probe", 8, &folder),
            ));
            s.flush().unwrap();
            assert!(!events(&s).contains("probe#8"), "{}", events(&s));
            s.attach_worker(crate::services::jobs::Worker::new(s.mailbox()).unwrap());
            let mut steps = 0;
            while !events(&s).contains("probe#8=1") && steps < 200 {
                let _ = s.step().unwrap();
                steps += 1;
            }
            s.jobs = None;
            assert!(events(&s).contains("probe#8=1"), "{}", events(&s));
            s.shutdown()
        })
    }

    fn wizard() -> crate::dialogs::request::DialogRequest {
        let name = crate::dialogs::request::PreferenceField {
            key: "name".to_string(),
            label: "Name".to_string(),
            kind: crate::dialogs::request::PreferenceKind::Text,
            value: "repo".to_string(),
            default: "".to_string(),
            help: "".to_string(),
        };
        let pages = vec![
            crate::dialogs::request::WizardPage {
                key: "name".to_string(),
                title: "Name".to_string(),
                description: "".to_string(),
                fields: vec![name],
                skip_if: "".to_string(),
            },
            crate::dialogs::request::WizardPage {
                key: "done".to_string(),
                title: "Done".to_string(),
                description: "".to_string(),
                fields: vec![],
                skip_if: "".to_string(),
            },
        ];
        request::checked(request::wizard("setup", "Set up", &pages))
    }

    #[test]
    fn a_checked_wizard_waits_for_the_application_and_recovers_from_a_failure() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Scopes",
                "dev.gossamer.gui-dialog-scope-test",
                Log::default,
                log_update,
                log_view,
            )
            .size(800, 640)
            .start()
            .unwrap();
            crate::perform(Effect::OpenDialog(wizard()));
            s.flush().unwrap();
            assert!(s.click("dlg:setup:action:next").unwrap());
            assert!(events(&s).ends_with("check setup#1 next"), "{}", events(&s));
            let dump = s.semantics(0).dump();
            assert!(
                dump.contains("Checking")
                    && dump.contains("button \"Stop\"")
                    && !dump.contains("button \"Next\""),
                "{}",
                dump
            );
            // A failure is shown and the page stays for another try.
            crate::perform(Effect::DialogChecked(
                "setup".to_string(),
                1,
                "That name is taken".to_string(),
            ));
            s.flush().unwrap();
            assert!(
                s.semantics(0).dump().contains("That name is taken"),
                "{}",
                s.semantics(0).dump()
            );
            assert!(s.click("dlg:setup:action:next").unwrap());
            assert!(s.click("dlg:setup:action:stop").unwrap());
            assert!(
                events(&s).ends_with("check setup#2 next; stopped setup#2"),
                "{}",
                events(&s)
            );
            // An answer to the stopped check changes nothing.
            crate::perform(Effect::DialogChecked(
                "setup".to_string(),
                2,
                "".to_string(),
            ));
            s.flush().unwrap();
            assert!(
                s.semantics(0).dump().contains("button \"Next\""),
                "{}",
                s.semantics(0).dump()
            );
            assert!(s.click("dlg:setup:action:next").unwrap());
            crate::perform(Effect::DialogChecked(
                "setup".to_string(),
                3,
                "".to_string(),
            ));
            s.flush().unwrap();
            assert!(
                s.semantics(0).dump().contains("button \"Finish\""),
                "{}",
                s.semantics(0).dump()
            );
            assert!(s.click("dlg:setup:action:finish").unwrap());
            assert!(
                events(&s).ends_with("check setup#4 finish"),
                "{}",
                events(&s)
            );
            crate::perform(Effect::DialogChecked(
                "setup".to_string(),
                4,
                "".to_string(),
            ));
            s.flush().unwrap();
            assert!(s.dialog_ids().is_empty(), "{:?}", s.dialog_ids());
            assert!(events(&s).contains("setup=Values"), "{}", events(&s));
            s.shutdown()
        })
    }

    #[test]
    fn sessions_under_test_run_without_a_display() {
        crate::testing::on_ui_thread(|| {
            let mut s = super::start("Headless", "dev.gossamer.gui-headless-test").unwrap();
            let driver = windowing::video_driver();
            s.shutdown();
            if env::var("SDL_VIDEO_DRIVER").is_err() {
                assert_eq!(driver, "offscreen")
            } else {
                assert!(!driver.is_empty())
            }
        })
    }

    #[test]
    fn a_modeless_dialog_leaves_its_window_usable_and_f6_moves_between_them() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Scopes",
                "dev.gossamer.gui-dialog-scope-test",
                Log::default,
                log_update,
                log_view,
            )
            .size(640, 480)
            .start()
            .unwrap();
            assert!(s.focus("name").unwrap());
            crate::perform(Effect::OpenDialogWith(
                "".to_string(),
                request::prompt("find", "Find", "Text", ""),
                Modality::Modeless,
            ));
            s.flush().unwrap();
            let inside = s.focused();
            assert!(inside.starts_with("dlg:find:"), "{}", inside);
            let dump = s.semantics(0).dump();
            assert!(
                dump.contains("dialog \"Find\"") && !dump.contains("[modal]"),
                "{}",
                dump
            );
            // The window's controls still take the pointer and the keyboard.
            assert!(s.click("go").unwrap());
            assert_eq!(s.focused(), "go");
            // Escape outside the dialog leaves it open; F6 returns to it.
            s.press(Key::Escape, Mods::default()).unwrap();
            assert_eq!(s.dialog_ids(), vec!["find"]);
            s.press(Key::F6, Mods::default()).unwrap();
            assert_eq!(s.focused(), inside);
            s.press(Key::F6, Mods::default()).unwrap();
            assert_eq!(s.focused(), "go");
            s.press(
                Key::F6,
                Mods {
                    shift: true,
                    ..Mods::default()
                },
            )
            .unwrap();
            assert_eq!(s.focused(), inside);
            // Escape inside closes it, and focus returns to where it opened from.
            s.press(Key::Escape, Mods::default()).unwrap();
            assert!(s.dialog_ids().is_empty(), "{:?}", s.dialog_ids());
            assert_eq!(s.focused(), "name");
            s.press(Key::F6, Mods::default()).unwrap();
            assert_eq!(s.focused(), "name");
            s.shutdown()
        })
    }

    #[test]
    fn a_window_modal_dialog_blocks_its_window_and_an_application_modal_one_every_window() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Scopes",
                "dev.gossamer.gui-dialog-scope-test",
                Log::default,
                log_update,
                log_view,
            )
            .windows(tools_view)
            .size(640, 480)
            .start()
            .unwrap();
            crate::perform(Effect::OpenDialog(request::info(
                "note",
                "Note",
                "Window modal",
            )));
            s.flush().unwrap();
            assert!(
                !s.click("go").unwrap() || !events(&s).contains("go"),
                "{}",
                events(&s)
            );
            assert!(
                s.semantics(0).dump().contains("[modal]"),
                "{}",
                s.semantics(0).dump()
            );
            s.close_dialog("note", &Outcome::Cancelled);
            s.flush().unwrap();
            crate::perform(Effect::OpenWindow {
                key: "tools".to_string(),
                title: "Tools".to_string(),
                width: 200,
                height: 120,
            });
            crate::perform(Effect::OpenDialogWith(
                "tools".to_string(),
                request::confirm(
                    "ask",
                    "Replace file?",
                    "A file with this name already exists.",
                    "Replace",
                    true,
                ),
                Modality::Application,
            ));
            s.flush().unwrap();
            // Nothing in the main window reacts while the tools window asks.
            let before = events(&s);
            let _ = s.click("go").unwrap();
            let focused = s.focused();
            s.press(Key::Tab, Mods::default()).unwrap();
            assert_eq!(events(&s), before);
            assert_eq!(s.focused(), focused);
            s.close_dialog("ask", &Outcome::Cancelled);
            s.flush().unwrap();
            assert!(s.click("go").unwrap());
            assert!(events(&s).ends_with("go"), "{}", events(&s));
            s.shutdown()
        })
    }
}

#[cfg(test)]
mod growth_tests {

    use crate::widgets::Widget;
    use crate::widgets::completion::{Completion, MatchMode};
    use crate::widgets::cx::UiMsg;
    use crate::widgets::textinput::{InputKind, TextInput};
    use windowing::{Key, Mods};

    fn update(m: String, msg: UiMsg) -> String {
        match msg {
            UiMsg::Text(_, text) => text,
            _ => m,
        }
    }

    fn view(m: &str) -> Widget {
        let mut t = TextInput::new("run", m, "Run", InputKind::Line);
        t.completion = Some(Completion::local(
            &Widget::items(&[
                ("a".to_string(), "Alpha".to_string()),
                ("b".to_string(), "Beta".to_string()),
                ("c".to_string(), "Gamma".to_string()),
            ]),
            MatchMode::Substring,
        ));
        t.error = if m == "zz" {
            "Nothing matches".to_string()
        } else {
            "".to_string()
        };
        Widget::input(&t).padding(8)
    }

    fn height(s: &crate::Session) -> i64 {
        s.windows[0].native.pixel_size().1
    }

    #[test]
    fn a_window_shows_only_once_it_has_its_final_size_and_first_frame() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("Shown", "dev.gossamer.gui-shown-test").unwrap();
            s.set_shell(|_m| (), |_k| view(""), |_k| Vec::new());
            s.prepare_window("main", "Shown", 300, 120).unwrap();
            let win = s.windows[0].native;
            assert!(!win.is_visible());
            let scale = s.windows[0].theme.scale;
            let (pw, ph) = win.pixel_size();
            assert_eq!(
                (pw, ph),
                (
                    (300.0 * scale).round() as i64,
                    (120.0 * scale).round() as i64
                )
            );
            s.show_window("main").unwrap();
            assert!(win.is_visible());
            assert_eq!(s.windows[0].node.rect.width, pw);
            s.shutdown()
        })
    }

    #[test]
    fn a_dismissible_window_closes_on_escape_over_its_popup_and_when_it_loses_focus() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Run",
                "dev.gossamer.gui-dismiss-test",
                String::new,
                update,
                |m: &String| view(m),
            )
            .size(400, 52)
            .frameless()
            .dismissible()
            .focus("run")
            .start()
            .unwrap();
            s.type_text("a").unwrap();
            s.flush().unwrap();
            assert!(!s.popup_owners().is_empty());
            s.press(Key::Escape, Mods::default()).unwrap();
            assert!(!s.step().unwrap());
            s.shutdown();
            let mut t = crate::application(
                "Run",
                "dev.gossamer.gui-dismiss-test",
                String::new,
                update,
                |m: &String| view(m),
            )
            .size(400, 52)
            .dismissible()
            .focus("run")
            .start()
            .unwrap();
            // Losing focus it never had keeps it; losing it after having it closes it.
            t.set_active(false).unwrap();
            assert_eq!(t.windows.len(), 1);
            t.set_active(true).unwrap();
            t.set_active(false).unwrap();
            assert!(!t.step().unwrap());
            t.shutdown()
        })
    }

    #[test]
    fn loading_completion_keeps_the_previous_height_until_new_results_arrive() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Run",
                "dev.gossamer.gui-completion-height-test",
                String::new,
                update,
                |m: &String| {
                    let mut input = TextInput::new("run", m, "Run", InputKind::Line);
                    input.completion = Some(Completion::asynchronous(0));
                    Widget::input(&input).padding(8).align(0.0, 0.0)
                },
            )
            .size(400, 52)
            .grows_for_popups()
            .focus("run")
            .start()
            .unwrap();
            assert_eq!(
                s.windows[0].node.rect.height,
                height(&s),
                "initial growth must relayout the first frame"
            );
            s.type_text("a").unwrap();
            s.advance(1).unwrap();
            let request = s.windows[0].states.choice("run#completion").request;
            let items = Widget::items(&[
                ("a".to_string(), "Alpha".to_string()),
                ("b".to_string(), "Beta".to_string()),
                ("c".to_string(), "Gamma".to_string()),
                ("d".to_string(), "Delta".to_string()),
            ]);
            crate::perform(crate::app::effects::Effect::Complete(
                "run".to_string(),
                request,
                items.clone(),
                String::new(),
            ));
            s.flush().unwrap();
            let expanded = height(&s);
            let popup_height = s.windows[0].popups[0].request.size.height;
            s.type_text("b").unwrap();
            s.advance(1).unwrap();
            let loading = s.windows[0].states.choice("run#completion");
            assert!(loading.loading);
            assert!(
                loading.items.is_empty(),
                "stale results must not be selectable"
            );
            assert_eq!(s.windows[0].popups[0].request.size.height, popup_height);
            assert_eq!(
                height(&s),
                expanded,
                "loading must not shrink and re-expand the window"
            );
            crate::perform(crate::app::effects::Effect::Complete(
                "run".to_string(),
                loading.request,
                items[..1].to_vec(),
                String::new(),
            ));
            s.flush().unwrap();
            let contracted = height(&s);
            assert!(
                contracted < expanded,
                "finished results may contract to their real height"
            );
            crate::perform(crate::app::effects::Effect::Complete(
                "run".to_string(),
                request,
                items,
                String::new(),
            ));
            s.flush().unwrap();
            assert_eq!(
                height(&s),
                contracted,
                "stale responses must not resize the window"
            );
            s.shutdown();
        })
    }

    #[test]
    fn a_run_box_grows_for_its_proposals_and_its_error_and_shrinks_back() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Run",
                "dev.gossamer.gui-growth-test",
                String::new,
                update,
                |m: &String| view(m),
            )
            .size(400, 52)
            .grows_for_popups()
            .focus("run")
            .start()
            .unwrap();
            let closed = height(&s);
            s.type_text("a").unwrap();
            s.flush().unwrap();
            let popup = s.windows[0].popups[0].clone();
            assert!(height(&s) > closed, "{} <= {}", height(&s), closed);
            // The list keeps the size it asked for and ends inside the window.
            assert_eq!(popup.rect.height, popup.request.size.height);
            assert!(
                popup.rect.bottom() <= height(&s),
                "{:?} in {}",
                popup.rect,
                height(&s)
            );
            s.press(Key::Escape, Mods::default()).unwrap();
            s.flush().unwrap();
            assert!(s.popup_owners().is_empty());
            assert_eq!(height(&s), closed);
            // The field's error line below it gets room too.
            let primary = Mods {
                ctrl: !crate::input::platform_is_macos(),
                gui: crate::input::platform_is_macos(),
                ..Mods::default()
            };
            s.press(Key::A, primary).unwrap();
            s.type_text("zz").unwrap();
            s.flush().unwrap();
            assert!(s.popup_owners().is_empty());
            assert!(height(&s) > closed, "{} <= {}", height(&s), closed);
            s.shutdown()
        })
    }
}

// The commands `w` names: menu bar, context menu, and button menu entries, and
// command buttons.
fn named_commands(w: &Widget, out: &mut Vec<String>) {
    match w {
        Widget::MenuBar(b) => {
            for m in &b.menus {
                menu_commands(&m.items, out)
            }
        }
        Widget::ContextMenu(c) => menu_commands(&c.items, out),
        Widget::Button(b) => {
            if !b.command.is_empty() {
                out.push(b.command.clone())
            };
            menu_commands(&b.menu, out)
        }
        _ => (),
    }
    for c in w.children() {
        named_commands(c, out)
    }
}

fn menu_commands(items: &[menus::MenuItem], out: &mut Vec<String>) {
    for it in items {
        match it {
            menus::MenuItem::Action { command, .. } | menus::MenuItem::Radio { command, .. } => {
                out.push(command.to_string())
            }
            menus::MenuItem::Submenu { items, .. } => menu_commands(items, out),
            menus::MenuItem::Separator | menus::MenuItem::Header(_) => (),
        }
    }
}

#[cfg(test)]
mod warning_tests {

    use crate::widgets::Widget;
    use crate::widgets::cx::UiMsg;
    use crate::widgets::menus;

    fn view(_m: &i64) -> Widget {
        Widget::column(
            &[
                Widget::menu_bar(
                    "menu",
                    &[menus::Menu::new(
                        "&File",
                        &[menus::item("app.sav"), menus::item("gui.quit")],
                    )],
                ),
                Widget::text_field("name", "", "First"),
                Widget::text_field("name", "", "Second"),
                Widget::command_button("gui.quit", "Quit"),
            ],
            8,
        )
    }

    #[test]
    fn duplicate_ids_and_unknown_commands_are_reported_once() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Warnings",
                "dev.gossamer.gui-warning-test",
                || 0,
                |m, _msg: UiMsg| m,
                view,
            )
            .size(300, 200)
            .start()
            .unwrap();
            s.flush().unwrap();
            s.emit(&UiMsg::Text("name".to_string(), "a".to_string()));
            s.flush().unwrap();
            let warnings = s
                .log
                .iter()
                .filter(|l| l.starts_with("warning:"))
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(
                warnings,
                vec![
                    "warning: window \"main\" has two widgets with the id \"name\"; give each its own id or `.key(..)`",
                    "warning: window \"main\" names the command \"app.sav\", which no command declares",
                ]
            );
            s.shutdown()
        })
    }
}

#[cfg(test)]
mod retained_render_tests {
    use super::*;
    #[test]
    fn occlusion_pauses_visual_work_restore_repaints_and_low_memory_purges_layers() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _cleanup = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let mut s = start("Retained tests", "dev.gossamer.retained-tests").unwrap();
            s.set_shell(|_| {}, |_| Widget::label("Retained"), |_| Vec::new());
            s.open_window("main", "Retained", 320, 200).unwrap();
            let id = s.windows[0].native.id();
            let before = s.windows[0].frames;
            s.timers.push(Timer {
                deadline: s.now(),
                window: "main".into(),
                owner: String::new(),
                tag: "gui:frame".into(),
                repeat: 33,
            });
            s.inject(&windowing::Event::Occluded { window: id })
                .unwrap();
            assert_eq!(s.next_deadline(), -1);
            s.advance(1000).unwrap();
            assert_eq!(s.windows[0].frames, before);
            s.inject(&windowing::Event::Restored { window: id })
                .unwrap();
            assert!(!s.windows[0].occluded);
            s.inject(&windowing::Event::RedrawRequested { window: id })
                .unwrap();
            assert!(s.windows[0].frames > before);
            assert!(s.rendering_stats(0).unwrap().scene_bytes > 0);
            s.inject(&windowing::Event::LowMemory).unwrap();
            assert_eq!(s.rendering_stats(0).unwrap().scene_bytes, 0);
            s.set_render_budgets(0, 0, 0).unwrap();
            s.flush().unwrap();
            assert_eq!(s.rendering_stats(0).unwrap().layer_bytes, 0);
            s.inject(&windowing::Event::RenderReset { window: id })
                .unwrap();
            s.close_window(0);
            assert!(s.renderers.is_empty());
            s.shutdown();
        })
    }
}

#[cfg(test)]
mod reservation_render_tests {
    #[test]
    fn repaint_reuses_an_existing_frame_reservation_at_the_session_limit() {
        crate::testing::on_ui_thread(|| {
            let mut session = crate::application(
                "Frame admission",
                "test.frame-admission",
                || (),
                |_, _: crate::widgets::cx::UiMsg| (),
                |_| crate::widgets::Widget::label("steady"),
            )
            .size(320, 200)
            .start()
            .unwrap();
            session.flush().unwrap();
            session
                .set_memory_limit(session.resource_usage().total)
                .unwrap();
            session.render(0).unwrap();
            session.shutdown();
        })
    }
}

#[cfg(test)]
mod advance_tests {
    use super::*;

    #[test]
    fn advancing_the_clock_fires_a_repeating_timer_once_per_period() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _cleanup = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let mut s = crate::application(
                "Ticks",
                "dev.gossamer.gui-advance-test",
                || 0_i64,
                // A handler slower than the period must not earn extra ticks.
                |n, msg| match msg {
                    UiMsg::Timer(tag) if tag == "tick" => {
                        std::thread::sleep(std::time::Duration::from_millis(150));
                        n + 1
                    }
                    _ => n,
                },
                |n| Widget::label(&format!("ticks {n}")),
            )
            .size(200, 100)
            .start()
            .unwrap();
            effects::perform(effects::Effect::Timer("tick".into(), 100, true));
            s.flush().unwrap();
            s.advance(1_000).unwrap();
            assert!(s.semantics(0).dump().contains("ticks 10"));
            s.advance(50).unwrap();
            assert!(s.semantics(0).dump().contains("ticks 10"));
            s.advance(50).unwrap();
            assert!(s.semantics(0).dump().contains("ticks 11"));
            s.shutdown()
        })
    }
}

#[cfg(test)]
mod window_command_tests {
    use super::*;

    #[test]
    fn a_dialog_shows_its_focus_ring_only_when_the_keyboard_opened_it() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _cleanup = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let mut s = start("Focus rings", "dev.gossamer.focus-rings").unwrap();
            s.set_shell(|_| {}, |_| Widget::label("Content"), |_| Vec::new());
            s.open_window("main", "Main", 320, 200).unwrap();
            let info = |id: &str| crate::dialogs::request::info(id, "Info", "Text");
            s.click_at(10, 10).unwrap();
            s.open_dialog_in(0, &info("pointer"));
            assert!(!s.windows[0].focus_visible);
            s.close_dialog("pointer", &crate::dialogs::Outcome::Accepted);
            s.press(windowing::Key::A, windowing::Mods::default())
                .unwrap();
            s.open_dialog_in(0, &info("keyboard"));
            assert!(s.windows[0].focus_visible);
            s.shutdown()
        })
    }

    #[test]
    fn each_window_builds_its_commands_for_its_own_key() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _cleanup = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let mut s = start("Window commands", "dev.gossamer.window-commands").unwrap();
            s.set_shell(
                |_| {},
                Widget::label,
                |key| vec![Command::new("which", key).checked(key == "second")],
            );
            s.open_window("main", "Main", 320, 200).unwrap();
            s.open_window("second", "Second", 320, 200).unwrap();
            for (w, key) in [(0, "main"), (1, "second")] {
                let which = s.windows[w].commands.get("which").unwrap();
                assert_eq!(which.label, key);
                assert_eq!(which.checked, i64::from(key == "second"));
            }
            s.shutdown()
        })
    }
}
