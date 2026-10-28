//! Terminals: VT screen state with bounded scrollback, an optional program on
//! a native pseudo-terminal, and the widget that shows and drives one.
//!
//! A terminal is a session-owned handle. Its program's output arrives on IO
//! threads, which only wake the session; the session's UI thread parses it
//! while settling, so screen state is only ever touched on that thread.

pub mod api;
pub mod engine;
pub mod frame;
pub mod keyboard;
mod transport;
pub mod widget;

#[cfg(windows)]
mod conpty;
#[cfg(unix)]
mod pty;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::memory::{Budget, Lease};
use crate::widgets::cx::UiMsg;
use engine::Engine;

#[cfg(windows)]
use conpty::Pty as Transport;
#[cfg(unix)]
use pty::Pty as Transport;

const HANDLE_LIMIT: usize = 256;
const ENTRY_BYTES: usize = 16 * 1024;
const PUMP_BYTES: usize = 256 * 1024;
const FEED_LIMIT: usize = 1024 * 1024;

static NEXT: AtomicI64 = AtomicI64::new(1);
static TERMINALS: LazyLock<Mutex<HashMap<i64, Entry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A terminal's screen size and storage limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub rows: i64,
    pub cols: i64,
    /// Rows of history kept above the screen.
    pub scrollback: i64,
    /// The most bytes the screen and its history may hold. The default holds
    /// the largest screen (256 x 512) with the default history, so the screen
    /// can always follow its view; it is a ceiling, and the session is
    /// charged only for the size in use.
    pub byte_limit: i64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            rows: 24,
            cols: 80,
            scrollback: 2000,
            byte_limit: 64 * 1024 * 1024,
        }
    }
}

/// A program to run on a terminal, executed directly rather than through a
/// shell.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
    /// The working directory; empty keeps the session's.
    pub cwd: String,
    /// Variables set for the program on top of the inherited environment.
    pub env: Vec<(String, String)>,
}

/// A cell position: a row of the visible screen and a column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub row: i64,
    pub col: i64,
}

/// What a terminal's program did that its application hears about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// The program set the window title.
    Title(String),
    /// The program rang the bell.
    Bell,
    /// The program exited with this status (128 + signal when killed).
    Exited(i64),
    /// The program could not be waited for.
    Failed(String),
    /// The screen could not take the size of its view within its byte limit,
    /// and keeps the size it has.
    Refused(String),
}

/// The state of a terminal an application reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub rows: i64,
    pub cols: i64,
    pub cursor: Cell,
    pub cursor_visible: bool,
    pub alternate_screen: bool,
    /// The visible screen, one line per row.
    pub text: String,
    pub title: String,
    pub running: bool,
    pub exit: Option<i64>,
    /// Rows the view is scrolled back into history.
    pub scrolled: i64,
    pub selection: Option<(Cell, Cell)>,
    pub reserved_bytes: i64,
    /// The program's process id, 0 without a program.
    pub pid: i64,
    /// Input bytes waiting for the program and output waiting to be parsed.
    pub queued_input: i64,
    pub queued_output: i64,
}

struct Entry {
    engine: Engine,
    transport: Option<Transport>,
    budget: Budget,
    _lease: Lease,
    selection: Option<(Cell, Cell)>,
    scrolled: usize,
    exit: Option<Result<u32, String>>,
    exit_reported: bool,
    title_seen: u64,
    bells_seen: u64,
    /// Changes whenever the screen, its scrolling or its selection does.
    revision: u64,
    /// Set after a key with Alt is encoded, whose text a platform may also
    /// deliver.
    suppress_text: bool,
    /// The view size the screen last refused, so each refusal is reported
    /// once, and the report waiting for the next pump.
    refused: Option<(u16, u16)>,
    refusal: Option<String>,
}

fn dimension(value: i64, limit: i64, what: &str) -> Result<u16, String> {
    if !(1..=limit).contains(&value) {
        return Err(format!("terminal {what} must be 1..={limit}"));
    }
    Ok(value as u16)
}

fn with<T>(id: i64, f: impl FnOnce(&mut Entry) -> Result<T, String>) -> Result<T, String> {
    let mut terminals = TERMINALS.lock();
    let entry = terminals.get_mut(&id).ok_or("the terminal is closed")?;
    f(entry)
}

/// Opens a terminal charged to `budget`.
pub fn create(options: &Options, budget: Budget) -> Result<i64, String> {
    let rows = dimension(options.rows, 256, "rows")?;
    let cols = dimension(options.cols, 512, "columns")?;
    if !(0..=100_000).contains(&options.scrollback) || options.byte_limit <= 0 {
        return Err(
            "terminal scrollback must be 0..=100000 rows within a positive byte limit".into(),
        );
    }
    let lease = budget.reserve(ENTRY_BYTES)?;
    let engine = Engine::new(
        rows,
        cols,
        options.scrollback as usize,
        options.byte_limit as usize,
        budget.clone(),
    )?;
    let mut terminals = TERMINALS.lock();
    if terminals.len() >= HANDLE_LIMIT {
        return Err("the terminal registry is full".into());
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    terminals.insert(
        id,
        Entry {
            engine,
            transport: None,
            budget,
            _lease: lease,
            selection: None,
            scrolled: 0,
            exit: None,
            exit_reported: false,
            title_seen: 0,
            bells_seen: 0,
            revision: 0,
            suppress_text: false,
            refused: None,
            refusal: None,
        },
    );
    Ok(id)
}

/// Runs `command` on terminal `id`, which must not already run a program.
pub fn spawn(id: i64, command: &Command) -> Result<(), String> {
    let waker = crate::app::effects::current_waker();
    let wake: Arc<dyn Fn() + Send + Sync> = match waker {
        Some(waker) => Arc::new(move || {
            let _ = waker.wake();
        }),
        None => Arc::new(|| {}),
    };
    with(id, |entry| {
        if entry.transport.is_some() {
            return Err("the terminal already runs a program".into());
        }
        let (rows, cols) = entry.engine.size();
        entry.transport = Some(Transport::spawn(command, rows, cols, &entry.budget, wake)?);
        entry.exit = None;
        entry.exit_reported = false;
        Ok(())
    })
}

/// Parses `bytes` as program output: a recorded stream, or output an
/// application produces itself.
pub fn feed(id: i64, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > FEED_LIMIT {
        return Err("terminal feed exceeds 1 MiB".into());
    }
    with(id, |entry| {
        entry.engine.process(bytes)?;
        entry.revision += 1;
        Ok(())
    })
}

/// Sends `bytes` to the program as typed input, showing the live screen.
pub fn input(id: i64, bytes: &[u8]) -> Result<(), String> {
    with(id, |entry| send(entry, bytes))
}

fn send(entry: &mut Entry, bytes: &[u8]) -> Result<(), String> {
    let transport = entry
        .transport
        .as_ref()
        .ok_or("the terminal runs no program")?;
    transport.write(bytes)?;
    if entry.scrolled != 0 || entry.selection.is_some() {
        entry.scrolled = 0;
        entry.engine.scroll(0);
        entry.selection = None;
        entry.revision += 1;
    }
    Ok(())
}

/// Pastes `text`, bracketed when the program asked for bracketed paste.
pub fn paste(id: i64, text: &str) -> Result<(), String> {
    with(id, |entry| {
        let bytes = entry.engine.paste(text)?;
        send(entry, &bytes)
    })
}

/// Resizes the screen and tells the program.
pub fn resize(id: i64, rows: i64, cols: i64) -> Result<(), String> {
    let rows = dimension(rows, 256, "rows")?;
    let cols = dimension(cols, 512, "columns")?;
    with(id, |entry| resize_entry(entry, rows, cols))
}

/// Resizes the screen to its view, keeping a refusal to report from the pump:
/// the screen then keeps its size, and the application hears why.
fn follow_view(entry: &mut Entry, rows: u16, cols: u16) {
    match resize_entry(entry, rows, cols) {
        Ok(()) => entry.refused = None,
        Err(why) if entry.refused != Some((rows, cols)) => {
            entry.refused = Some((rows, cols));
            entry.refusal = Some(format!("the screen cannot take {rows} x {cols}: {why}"));
        }
        Err(_) => {}
    }
}

fn resize_entry(entry: &mut Entry, rows: u16, cols: u16) -> Result<(), String> {
    if entry.engine.size() == (rows, cols) {
        return Ok(());
    }
    entry.engine.resize(rows, cols)?;
    if let Some(transport) = &entry.transport {
        transport.resize(rows, cols)?;
    }
    entry.selection = None;
    entry.revision += 1;
    Ok(())
}

/// Scrolls the view `rows` rows back into history (negative moves forward),
/// answering how far back it now is.
pub fn scroll(id: i64, rows: i64) -> Result<i64, String> {
    with(id, |entry| {
        let target = (entry.scrolled as i64).saturating_add(rows).max(0) as usize;
        entry.engine.scroll(target);
        let reached = entry.engine.scrollback();
        if reached != entry.scrolled {
            entry.scrolled = reached;
            entry.revision += 1;
        }
        Ok(reached as i64)
    })
}

/// Selects the cells from `anchor` to `caret`; `None` clears the selection.
pub fn select(id: i64, range: Option<(Cell, Cell)>) -> Result<(), String> {
    with(id, |entry| {
        let (rows, cols) = entry.engine.size();
        let clamp = |cell: Cell| Cell {
            row: cell.row.clamp(0, i64::from(rows) - 1),
            col: cell.col.clamp(0, i64::from(cols)),
        };
        let range = range.map(|(a, b)| (clamp(a), clamp(b)));
        if entry.selection != range {
            entry.selection = range;
            entry.revision += 1;
        }
        Ok(())
    })
}

/// The selected text, empty without a selection.
pub fn copy(id: i64) -> Result<String, String> {
    with(id, |entry| Ok(selected_text(entry)))
}

fn selected_text(entry: &Entry) -> String {
    let Some((a, b)) = entry.selection else {
        return String::new();
    };
    let (start, end) = if (a.row, a.col) <= (b.row, b.col) {
        (a, b)
    } else {
        (b, a)
    };
    entry.engine.copy(
        (start.row as u16, start.col as u16),
        (end.row as u16, end.col as u16),
    )
}

/// The terminal's state.
pub fn snapshot(id: i64) -> Result<Snapshot, String> {
    with(id, |entry| {
        let engine = &entry.engine;
        let (rows, cols) = engine.size();
        let (row, col) = engine.cursor();
        let (pid, (queued_input, queued_output)) = entry
            .transport
            .as_ref()
            .map_or((0, (0, 0)), |t| (t.pid(), t.queued_bytes()));
        Ok(Snapshot {
            rows: rows.into(),
            cols: cols.into(),
            cursor: Cell {
                row: row.into(),
                col: col.into(),
            },
            cursor_visible: !engine.hide_cursor(),
            alternate_screen: engine.alternate_screen(),
            text: entry.engine.text(),
            title: entry.engine.notifications().title.clone(),
            running: entry.transport.is_some() && entry.exit.is_none(),
            exit: match &entry.exit {
                Some(Ok(code)) => Some(i64::from(*code)),
                _ => None,
            },
            scrolled: entry.scrolled as i64,
            selection: entry.selection,
            reserved_bytes: entry.engine.charged_bytes() as i64,
            pid: i64::from(pid),
            queued_input: queued_input as i64,
            queued_output: queued_output as i64,
        })
    })
}

/// Ends the terminal's program, if any, and releases the terminal.
pub fn close(id: i64) {
    let entry = TERMINALS.lock().remove(&id);
    // The program and its IO threads end outside the registry lock.
    drop(entry);
}

/// Releases every terminal charged to a session's budget.
pub(crate) fn close_budget(budget: &Budget) {
    let closed: Vec<Entry> = {
        let mut terminals = TERMINALS.lock();
        let ids: Vec<i64> = terminals
            .iter()
            .filter(|(_, entry)| entry.budget == *budget)
            .map(|(id, _)| *id)
            .collect();
        ids.iter().filter_map(|id| terminals.remove(id)).collect()
    };
    drop(closed);
}

/// Parses output waiting for `budget`'s terminals on the session's thread,
/// answering whether any screen changed and the events applications hear.
pub(crate) fn pump(budget: &Budget) -> (bool, Vec<UiMsg>) {
    let mut changed = false;
    let mut events = Vec::new();
    let mut terminals = TERMINALS.lock();
    for (id, entry) in terminals.iter_mut().filter(|(_, e)| e.budget == *budget) {
        if let Some(why) = entry.refusal.take() {
            changed = true;
            events.push(UiMsg::Terminal(*id, Event::Refused(why)));
        }
        let Some(transport) = &entry.transport else {
            continue;
        };
        if !transport.has_pending_output() {
            continue;
        }
        let drained = transport.drain(PUMP_BYTES);
        if !drained.bytes.is_empty() {
            if entry.engine.process(&drained.bytes).is_ok() {
                entry.revision += 1;
                changed = true;
            }
            let replies = entry.engine.take_replies();
            if !replies.is_empty() {
                let _ = transport.write(&replies);
            }
        }
        let notes = entry.engine.notifications();
        if notes.title_revision != entry.title_seen {
            entry.title_seen = notes.title_revision;
            events.push(UiMsg::Terminal(*id, Event::Title(notes.title.clone())));
        }
        if notes.bells != entry.bells_seen {
            entry.bells_seen = notes.bells;
            events.push(UiMsg::Terminal(*id, Event::Bell));
        }
        if drained.exit.is_some() {
            entry.exit = drained.exit.clone();
        }
        // An exit is reported once the output before it has been parsed.
        if !entry.exit_reported
            && drained.eof
            && !drained.more
            && let Some(exit) = &entry.exit
        {
            entry.exit_reported = true;
            changed = true;
            events.push(UiMsg::Terminal(
                *id,
                match exit {
                    Ok(code) => Event::Exited(i64::from(*code)),
                    Err(error) => Event::Failed(error.clone()),
                },
            ));
        }
    }
    (changed, events)
}

/// Runs `f` on a terminal's entry for the widget, which draws and drives it.
fn with_entry<T>(id: i64, f: impl FnOnce(&mut Entry) -> T) -> Option<T> {
    TERMINALS.lock().get_mut(&id).map(f)
}

/// Terminals open across every session, for disposal checks.
pub fn open_count() -> usize {
    TERMINALS.lock().len()
}

#[cfg(test)]
mod tests;
