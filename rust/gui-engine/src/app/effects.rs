// effects: what an application's update asks the toolkit to do beyond
// changing its model. `perform` queues an effect on the UI thread; the
// session applies the queue after each update, in order. Effects keep
// `update` free of session handles, so it stays a testable function.

use std::cell::RefCell;

use windowing;

use crate::commands::Command;
use crate::dialogs::request::DialogRequest;
use crate::services::desktop::Urgency;
use crate::style::{Density, ThemeKind};
use crate::widgets::completion::CompletionItem;
use crate::widgets::menus::MenuItem;

/// One request from application code.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Effect {
    /// Runs revisioned file IO, analysis or completion on the bounded worker.
    Work(crate::services::work::Work),
    CancelWork(String),
    /// Queues a message for the next update, without reentrant delivery.
    Emit(crate::widgets::cx::UiMsg),
    /// Shows a dialog in the main window (or the window named).
    OpenDialog(DialogRequest),
    OpenDialogIn(String, DialogRequest),
    /// Shows a dialog in the window named ("" for the main window) with
    /// the modality given.
    OpenDialogWith(String, DialogRequest, crate::dialogs::request::Modality),
    /// Opens another window built by the application's window view.
    OpenWindow {
        key: String,
        title: String,
        width: i64,
        height: i64,
    },
    CloseWindow(String),
    SetWindowPosition(String, i64, i64),
    /// Keeps a window at least this many interface units wide and tall,
    /// growing it now when its current or remembered size is smaller.
    SetMinSize(String, i64, i64),
    /// Ends the session (after asking documents, when close is deferred).
    Quit,
    /// Moves keyboard focus to the widget `id` in the main window.
    Focus(String),
    /// Moves focus inside a named window.
    FocusIn(String, String),
    SetTitle(String, String),
    /// Delivers `UiMsg::Timer(tag)` after `delay` ms (repeating when asked).
    Timer(String, i64, bool),
    CancelTimer(String),
    /// Answers a `UiMsg::CompletionRequest`: owner id, request id, items,
    /// and an error text ("" on success).
    Complete(String, i64, Vec<CompletionItem>, String),
    /// Answers `UiMsg::DialogCheck(id, request, ..)`: "" lets the step take
    /// effect, anything else is shown as the reason it did not.
    DialogChecked(String, i64, String),
    /// Reads a folder without holding up the UI; answered by
    /// `UiMsg::Listed(owner, request, listing)`.
    ListDirectory(crate::services::listing::ListRequest),
    /// Builds the installed-font catalog without holding up the UI; answered
    /// by `UiMsg::FontsLoaded(owner, records)`.
    LoadFonts(String),
    /// Builds the installed-application catalog off the UI thread; answered
    /// by `UiMsg::AppsLoaded(owner, entries)`. Entries carry no icon pixels;
    /// `services::apps::with_icons` loads them.
    LoadApps(String),
    /// Starts an application on files off the UI thread; answered by
    /// `UiMsg::Task(owner, Done(name))` or `Failed(why)`.
    LaunchApp(String, crate::services::apps::AppEntry, Vec<String>),
    /// Speaks `text` through assistive technology.
    Announce(String),
    /// Shows a toast in the main window, replacing one with the same id.
    Toast(crate::widgets::feedback::Toast),
    /// Closes the toast `id` without reporting anything.
    DismissToast(String),
    SetClipboard(String),
    OpenUrl(String),
    RevealFile(String),
    SetTheme(ThemeKind),
    /// Follows the desktop's light/dark setting again.
    FollowSystemTheme,
    /// Sets the text size; the desktop's text size no longer applies.
    SetTextScale(f64),
    /// Stops (`true`) or allows animation; the desktop's motion preference
    /// no longer applies.
    SetReducedMotion(bool),
    /// Draws every window as if its display's scale were `Some(scale)`
    /// (previews, mixed-DPI checks); `None` follows each display again.
    SetDisplayScale(Option<f64>),
    SetDensity(Density),
    /// Replaces the typography, shape, motion and icon tokens of every
    /// window; they persist across theme kind changes.
    SetThemeTokens(crate::style::ThemeTokens),
    SetRtl(bool),
    /// Installs a translation catalog and rebuilds all windows with its direction.
    SetLocale(crate::localization::Catalog),
    /// Replaces the extra application commands (beyond `App::commands`).
    Commands(Vec<Command>),
    /// Scrolls every scroll area so the widget `id` shows.
    ScrollTo(String),
    /// Selects UTF-8 bytes `anchor..caret` of the text input `id` in a
    /// window and scrolls the caret into view. Offsets inside a grapheme
    /// move to its start; offsets past the end move to the end.
    SelectText {
        window: String,
        id: String,
        anchor: i64,
        caret: i64,
    },
    /// Shows a desktop notification. Answers `UiMsg::Task(id, Done(""))`, or
    /// `Failed` with the reason where the platform has no notification service.
    Notify {
        id: String,
        title: String,
        body: String,
        urgency: Urgency,
    },
    /// Shows the application's tray icon, replacing any shown, with a menu
    /// whose items run commands. A failure answers `UiMsg::Task(id, Failed(..))`.
    SetTray {
        id: String,
        icon: Option<windowing::TrayIcon>,
        tooltip: String,
        items: Vec<MenuItem>,
    },
    /// Removes the tray icon.
    CloseTray,
    /// Drags `items` out of a window while the pointer is held; answers
    /// `UiMsg::DragFinished(id, operation)`.
    StartDrag {
        window: String,
        id: String,
        items: Vec<windowing::DragItem>,
        operation: windowing::DragOperation,
    },
    /// Replaces UTF-8 bytes `start..end` of the text input `id` in a window
    /// with `text` as one undoable edit, leaving the caret after it.
    /// Read-only inputs ignore it.
    ReplaceText {
        window: String,
        id: String,
        start: i64,
        end: i64,
        text: String,
    },
}

// Effects are asked for and applied on the UI thread; workers reach the
// session through a `Mailbox` instead.
pub const EFFECT_BYTE_LIMIT: usize = 128 * 1024 * 1024;
struct Queued {
    effect: Effect,
    bytes: usize,
    lease: crate::memory::Lease,
}
#[derive(Default)]
struct Queue {
    items: Vec<Queued>,
    bytes: usize,
}
thread_local! {
    static QUEUE: RefCell<Queue> = RefCell::new(Queue::default());
    static BUDGET: RefCell<crate::memory::Budget> = RefCell::new(crate::memory::Budget::new(EFFECT_BYTE_LIMIT));
    static WAKER: RefCell<Option<windowing::Waker>> = const { RefCell::new(None) };
    static MAILBOX: RefCell<Option<crate::app::mailbox::Mailbox>> = const { RefCell::new(None) };
}

/// Installs the running session's mailbox for background work to answer through.
pub(crate) fn set_mailbox(mailbox: Option<crate::app::mailbox::Mailbox>) {
    MAILBOX.with_borrow_mut(|current| *current = mailbox);
}

/// The mailbox of the session running on this thread.
pub(crate) fn current_mailbox() -> Option<crate::app::mailbox::Mailbox> {
    MAILBOX.with_borrow(Clone::clone)
}

/// Installs the running session's waker, which IO threads use to wake it.
pub(crate) fn set_waker(waker: Option<windowing::Waker>) {
    WAKER.with_borrow_mut(|current| *current = waker);
}

/// The waker of the session running on this thread.
pub(crate) fn current_waker() -> Option<windowing::Waker> {
    WAKER.with_borrow(Clone::clone)
}

pub(crate) fn set_budget(budget: crate::memory::Budget) {
    BUDGET.with_borrow_mut(|current| *current = budget);
}

pub fn current_budget() -> crate::memory::Budget {
    BUDGET.with_borrow(Clone::clone)
}

pub fn try_perform(effect: Effect) -> Result<(), String> {
    let bytes = crate::memory::measure(&effect);
    QUEUE.with_borrow_mut(|queue| {
        if queue.items.len() >= 256 || bytes > EFFECT_BYTE_LIMIT.saturating_sub(queue.bytes) {
            return Err("effect queue exceeds its byte budget".into());
        }
        let lease = BUDGET.with_borrow(|budget| budget.reserve(bytes))?;
        queue.items.push(Queued {
            effect: effect.clone(),
            bytes,
            lease,
        });
        queue.bytes += bytes;
        Ok(())
    })
}

pub fn perform(effect: Effect) {
    if let Err(error) = try_perform(effect) {
        panic!("{error}");
    }
}

pub(crate) fn take_leased() -> Vec<(Effect, crate::memory::Lease)> {
    QUEUE.with_borrow_mut(|queue| {
        let items = std::mem::take(&mut queue.items);
        let bytes = items.iter().map(|item| item.bytes).sum::<usize>();
        debug_assert_eq!(bytes, queue.bytes);
        queue.bytes = 0;
        items
            .into_iter()
            .map(|item| (item.effect, item.lease))
            .collect()
    })
}

pub fn take() -> Vec<Effect> {
    take_leased()
        .into_iter()
        .map(|(effect, _)| effect)
        .collect()
}
pub fn pending() -> i64 {
    QUEUE.with_borrow(|queue| queue.items.len() as i64)
}

impl std::fmt::Display for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Effect")
    }
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Effect")
    }
}
