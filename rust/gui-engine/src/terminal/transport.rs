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
pub(crate) const OUTPUT: usize = 256 * 1024;
const CHUNK_LIMIT: usize = 64;
const TRANSPORT_LIMIT: usize = 16;

pub(crate) struct State {
    pub(crate) chunks: VecDeque<Vec<u8>>,
    pub(crate) bytes: usize,
    pub(crate) closed: bool,
    pub(crate) eof: bool,
    pub(crate) exit: Option<Result<u32, String>>,
}

/// A byte- and chunk-bounded queue between a native IO thread and its owner.
pub(crate) struct Queue {
    pub(crate) state: Mutex<State>,
    pub(crate) changed: Condvar,
    limit: usize,
}

impl Queue {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(State {
                chunks: VecDeque::new(),
                bytes: 0,
                closed: false,
                eof: false,
                exit: None,
            }),
            changed: Condvar::new(),
            limit,
        }
    }

    /// Queues `bytes`, waiting for space when `wait` is set. Answers whether
    /// the queue was empty, which is when its reader needs a wake.
    pub(crate) fn push(&self, bytes: Vec<u8>, wait: bool) -> Result<bool, String> {
        if bytes.len() > self.limit {
            return Err("terminal chunk exceeds its queue budget".into());
        }
        let mut state = self.state.lock();
        while !state.closed
            && (state.bytes + bytes.len() > self.limit || state.chunks.len() >= CHUNK_LIMIT)
        {
            if !wait {
                return Err("terminal input queue is full".into());
            }
            self.changed.wait(&mut state);
        }
        if state.closed {
            return Err("terminal queue is closed".into());
        }
        let wake = state.chunks.is_empty();
        state.bytes += bytes.len();
        state.chunks.push_back(bytes);
        self.changed.notify_all();
        Ok(wake)
    }

    /// Waits until the owner has taken every queued chunk; false once the
    /// queue is closed.
    pub(crate) fn wait_empty(&self) -> bool {
        let mut state = self.state.lock();
        while !state.closed && !state.chunks.is_empty() {
            self.changed.wait(&mut state);
        }
        !state.closed
    }

    /// The next chunk; blocks while the queue is open and empty.
    pub(crate) fn pop(&self) -> Option<Vec<u8>> {
        let mut state = self.state.lock();
        while !state.closed && state.chunks.is_empty() {
            self.changed.wait(&mut state);
        }
        let bytes = state.chunks.pop_front()?;
        state.bytes -= bytes.len();
        self.changed.notify_all();
        Some(bytes)
    }

    pub(crate) fn close(&self) {
        let mut state = self.state.lock();
        state.closed = true;
        state.chunks.clear();
        state.bytes = 0;
        self.changed.notify_all();
    }

    pub(crate) fn finish(&self, eof: bool, exit: Option<Result<u32, String>>) {
        let mut state = self.state.lock();
        state.eof |= eof;
        if exit.is_some() {
            state.exit = exit;
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        self.state.lock().bytes
    }
}

/// Output a transport delivered, with its end-of-stream and exit status.
pub struct Drain {
    pub more: bool,
    pub bytes: Vec<u8>,
    pub eof: bool,
    pub exit: Option<Result<u32, String>>,
}

/// At most one outstanding wake per transport. Draining rearms it under the
/// output lock, then wakes again if output it left behind still needs one.
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

/// Takes up to `limit` bytes of queued output.
pub(crate) fn drain(output: &Queue, notify: &Notify, limit: usize) -> Drain {
    let mut state = output.state.lock();
    let mut bytes = Vec::new();
    let limit = limit.min(OUTPUT);
    while bytes.len() < limit {
        let Some(mut chunk) = state.chunks.pop_front() else {
            break;
        };
        let take = chunk.len().min(limit - bytes.len());
        let tail = chunk.split_off(take);
        state.bytes -= take;
        bytes.extend_from_slice(&chunk);
        if !tail.is_empty() {
            state.chunks.push_front(tail);
        }
    }
    notify.pending.store(false, Ordering::Release);
    let result = Drain {
        more: !state.chunks.is_empty(),
        bytes,
        eof: state.eof,
        exit: state.exit.clone(),
    };
    output.changed.notify_all();
    drop(state);
    if result.more {
        notify.fire();
    }
    result
}

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

pub(crate) fn pty_size(rows: u16, cols: u16) -> Result<portable_pty::PtySize, String> {
    if rows == 0 || rows > 256 || cols == 0 || cols > 512 {
        return Err("invalid terminal dimensions".into());
    }
    Ok(portable_pty::PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}
