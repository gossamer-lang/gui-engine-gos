//! Queues, wakes and admission shared by the native terminal transports.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use parking_lot::{Condvar, Mutex};

pub(crate) const STACK: usize = 256 * 1024;
pub(crate) const CHUNK: usize = 8192;
/// The most output one pass of a reader collects before queueing it.
pub(crate) const BATCH: usize = 64 * 1024;
pub(crate) const INPUT: usize = 64 * 1024;
pub(crate) const CHUNK_LIMIT: usize = 64;
const TRANSPORT_LIMIT: usize = 16;

pub(crate) struct State {
    pub(crate) chunks: VecDeque<Vec<u8>>,
    pub(crate) bytes: usize,
    pub(crate) closed: bool,
    /// The consumer holds a chunk it popped and has not finished.
    pub(crate) in_flight: bool,
}

/// A byte- and chunk-bounded queue between a native IO thread and its owner.
pub(crate) struct Queue {
    pub(crate) state: Mutex<State>,
    pub(crate) changed: Condvar,
    limit: usize,
    /// Set while the owner waits for room; the consumer wakes it after a pop.
    room_wanted: AtomicBool,
}

impl Queue {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(State {
                chunks: VecDeque::new(),
                bytes: 0,
                closed: false,
                in_flight: false,
            }),
            changed: Condvar::new(),
            limit,
            room_wanted: AtomicBool::new(false),
        }
    }

    /// Hands `bytes` to `write` at once when nothing waits before them, the
    /// queue's lock keeping them in order, and queues what it leaves;
    /// `write` answers how many it took. Answers whether any were queued.
    pub(crate) fn write_or_push(
        &self,
        bytes: &[u8],
        write: impl FnOnce(&[u8]) -> usize,
    ) -> Result<bool, String> {
        if bytes.len() > self.limit {
            return Err("terminal chunk exceeds its queue budget".into());
        }
        let mut state = self.state.lock();
        if state.closed {
            return Err("terminal queue is closed".into());
        }
        let mut rest = bytes;
        if state.chunks.is_empty() && !state.in_flight {
            rest = &bytes[write(bytes).min(bytes.len())..];
            if rest.is_empty() {
                return Ok(false);
            }
        }
        if state.bytes + rest.len() > self.limit || state.chunks.len() >= CHUNK_LIMIT {
            return Err("terminal input queue is full".into());
        }
        state.bytes += rest.len();
        state.chunks.push_back(rest.to_vec());
        self.changed.notify_all();
        Ok(true)
    }

    /// The next chunk; blocks while the queue is open and empty. The chunk
    /// stays in flight until `finished`.
    pub(crate) fn pop(&self) -> Option<Vec<u8>> {
        let mut state = self.state.lock();
        while !state.closed && state.chunks.is_empty() {
            self.changed.wait(&mut state);
        }
        let bytes = state.chunks.pop_front()?;
        state.bytes -= bytes.len();
        state.in_flight = true;
        self.changed.notify_all();
        Some(bytes)
    }

    /// The consumer wrote the chunk it popped last.
    pub(crate) fn finished(&self) {
        self.state.lock().in_flight = false;
    }

    pub(crate) fn close(&self) {
        let mut state = self.state.lock();
        state.closed = true;
        state.chunks.clear();
        state.bytes = 0;
        self.changed.notify_all();
    }

    pub(crate) fn bytes(&self) -> usize {
        self.state.lock().bytes
    }

    /// The bytes a push could add now without waiting.
    pub(crate) fn room(&self) -> usize {
        let state = self.state.lock();
        if state.closed || state.chunks.len() >= CHUNK_LIMIT {
            0
        } else {
            self.limit - state.bytes
        }
    }

    /// Asks the consumer to say, through `take_room_wanted`, when it takes a chunk.
    pub(crate) fn want_room(&self) {
        self.room_wanted.store(true, Ordering::Release);
    }

    /// Whether the owner waits for room; it waits once per `want_room`.
    pub(crate) fn take_room_wanted(&self) -> bool {
        self.room_wanted.swap(false, Ordering::AcqRel)
    }
}

/// How a transport's program ended, as its reader learns the end of its
/// output and its waiter the exit status.
#[derive(Default)]
pub(crate) struct Ending {
    state: Mutex<(bool, Option<Result<u32, String>>)>,
}

impl Ending {
    pub(crate) fn finish(&self, eof: bool, exit: Option<Result<u32, String>>) {
        let mut state = self.state.lock();
        state.0 |= eof;
        if exit.is_some() {
            state.1 = exit;
        }
    }
}

/// Whether a transport's output ended, and its program's exit once known.
pub struct Drain {
    pub eof: bool,
    pub exit: Option<Result<u32, String>>,
}

/// At most one outstanding wake per transport; taking the program's state
/// (`drain`) rearms it. Output parsed before a wake is in the registry by the
/// time the wake's pump looks, as both take the registry lock.
pub(crate) struct Notify {
    pending: AtomicBool,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Notify {
    pub(crate) fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            pending: AtomicBool::new(false),
            wake,
        }
    }

    pub(crate) fn fire(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            (self.wake)();
        }
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
}

/// How the program stands; rearms the owner's wake.
pub(crate) fn drain(ending: &Ending, notify: &Notify) -> Drain {
    notify.pending.store(false, Ordering::Release);
    let state = ending.state.lock();
    Drain {
        eof: state.0,
        exit: state.1.clone(),
    }
}

/// Where a transport's reader hands what it reads, on the reader's own
/// thread, so output is parsed while the session's thread paints.
pub(crate) type Sink = Box<dyn Fn(&[u8]) + Send>;

/// Bounds native IO threads and descriptors process-wide, beside the byte
/// admission each transport makes.
pub(crate) struct Slot;

static TRANSPORTS: AtomicUsize = AtomicUsize::new(0);

impl Slot {
    pub(crate) fn acquire() -> Result<Self, String> {
        TRANSPORTS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < TRANSPORT_LIMIT).then_some(used + 1)
            })
            .map_err(|_| "terminal transport limit reached".to_string())?;
        Ok(Self)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        TRANSPORTS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Validates a program and its arguments before any native resource exists.
pub(crate) fn check_command(program: &str, args: &[String]) -> Result<(), String> {
    if program.is_empty()
        || program.len() > 4096
        || program.contains('\0')
        || args.iter().map(|s| s.len() + 1).sum::<usize>() > 64 * 1024
        || args.iter().any(|s| s.contains('\0'))
    {
        return Err("invalid terminal command".into());
    }
    Ok(())
}

// Variables by which another terminal names itself. A program started here
// would inherit them from the terminal that started this one and take this
// terminal for that one: `kitty @` would drive the other kitty, and an image
// tool would speak the other terminal's protocol.
const FOREIGN_TERMINAL: &[&str] = &[
    "COLORFGBG",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "MLTERM",
    "TERMINAL_EMULATOR",
    "TERMINOLOGY",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "VTE_VERSION",
    "WINDOWID",
    "WT_PROFILE_ID",
    "WT_SESSION",
    "XTERM_LOCALE",
    "XTERM_SHELL",
    "XTERM_VERSION",
];
const FOREIGN_TERMINAL_PREFIXES: &[&str] = &[
    "ALACRITTY_",
    "GHOSTTY_",
    "ITERM_",
    "KITTY_",
    "KONSOLE_",
    "WEZTERM_",
];

// Whether the inherited variable `name` names another terminal. Windows
// compares names without case.
fn names_another_terminal(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    FOREIGN_TERMINAL.contains(&name.as_str())
        || FOREIGN_TERMINAL_PREFIXES
            .iter()
            .any(|p| name.starts_with(p))
}

/// The environment a program in this terminal starts with: the inherited one
/// without another terminal's names for itself, this terminal's own, then
/// `command`'s variables.
pub(crate) fn environment(builder: &mut portable_pty::CommandBuilder, command: &super::Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(names_another_terminal) {
            builder.env_remove(&key);
        }
    }
    builder.env("TERM", "xterm-256color");
    builder.env("COLORTERM", "truecolor");
    for (key, value) in &command.env {
        builder.env(key, value);
    }
}

/// The size a program reads from its terminal: `rows` by `cols` cells of
/// `cell` pixels each.
pub(crate) fn pty_size(
    rows: u16,
    cols: u16,
    cell: (u16, u16),
) -> Result<portable_pty::PtySize, String> {
    if rows == 0 || rows > 256 || cols == 0 || cols > 512 {
        return Err("invalid terminal dimensions".into());
    }
    Ok(portable_pty::PtySize {
        rows,
        cols,
        pixel_width: cols.saturating_mul(cell.0),
        pixel_height: rows.saturating_mul(cell.1),
    })
}

#[cfg(test)]
mod tests {
    use super::{Queue, names_another_terminal};

    #[test]
    fn input_goes_out_directly_only_with_nothing_before_it() {
        let queue = Queue::new(1024);
        let mut direct = Vec::new();
        // Nothing waits: the write takes all of it.
        assert!(
            !queue
                .write_or_push(b"ab", |b| {
                    direct.extend_from_slice(b);
                    b.len()
                })
                .unwrap()
        );
        // The kernel takes part: the rest waits for the writer thread.
        assert!(queue.write_or_push(b"cdef", |_| 2).unwrap());
        assert_eq!(direct, b"ab");
        // Queued input, and then a chunk the writer holds, keep later input
        // behind them.
        assert!(
            queue
                .write_or_push(b"g", |_| panic!("written ahead"))
                .unwrap()
        );
        assert_eq!(queue.pop().unwrap(), b"ef");
        assert_eq!(queue.pop().unwrap(), b"g");
        assert!(
            queue
                .write_or_push(b"h", |_| panic!("written ahead"))
                .unwrap()
        );
        assert_eq!(queue.pop().unwrap(), b"h");
        queue.finished();
        assert!(!queue.write_or_push(b"i", |b| b.len()).unwrap());
    }

    #[test]
    fn another_terminals_names_for_itself_are_left_behind() {
        for name in [
            "KITTY_PID",
            "KITTY_WINDOW_ID",
            "TERM_PROGRAM",
            "VTE_VERSION",
            "wt_session",
        ] {
            assert!(names_another_terminal(name), "{name}");
        }
        for name in ["TERM", "COLORTERM", "HOME", "PATH", "KITTYCAT"] {
            assert!(!names_another_terminal(name), "{name}");
        }
    }
}
