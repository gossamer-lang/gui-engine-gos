//! Queues, wakes and admission shared by the native terminal transports.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use parking_lot::{Condvar, Mutex};

pub(crate) const STACK: usize = 256 * 1024;
pub(crate) const CHUNK: usize = 8192;
/// The most output that can wait for the parser. When this much is waiting,
/// the reader stops reading until the parser catches up.
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
    peek(ending)
}

/// The program's state, leaving the owner's wake off.
pub(crate) fn peek(ending: &Ending) -> Drain {
    let state = ending.state.lock();
    Drain {
        eof: state.0,
        exit: state.1.clone(),
    }
}

/// Receives the terminal's output, on the parser thread.
pub(crate) type Sink = Box<dyn Fn(&[u8]) + Send + Sync>;

/// Output read and not yet taken by the parser.
struct Backlog {
    /// At most `BATCH` bytes. The capacity is reserved up front, so reading
    /// never allocates.
    bytes: Vec<u8>,
    /// The parser takes the backlog that is left, then ends.
    finishing: bool,
    /// The transport closes; output not yet parsed is dropped.
    closed: bool,
    /// The parser hands back memory others freed for it before it waits.
    release: bool,
    /// The parser waits for output.
    parked: bool,
}

/// The state a transport's reader and its parser thread share.
pub(crate) struct Handoff {
    state: Mutex<Backlog>,
    arrived: Condvar,
    room: Condvar,
}

impl Handoff {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(Backlog {
                bytes: Vec::with_capacity(BATCH),
                finishing: false,
                closed: false,
                release: false,
                parked: false,
            }),
            arrived: Condvar::new(),
            room: Condvar::new(),
        }
    }

    /// Drops the backlog and ends the parser; a reader waiting for room
    /// stops waiting.
    pub(crate) fn close(&self) {
        let mut state = self.state.lock();
        state.closed = true;
        state.bytes = Vec::new();
        self.arrived.notify_all();
        self.room.notify_all();
    }

    /// Has the parser hand back the memory others freed for it.
    pub(crate) fn release(&self) {
        self.state.lock().release = true;
        self.arrived.notify_all();
    }
}

/// Passes output from the reader thread to the parser thread. While output
/// streams, the reader only reads and copies. A pseudo-terminal holds very
/// little (a kilobyte on macOS), and the program writing to it is stalled
/// until the reader reads again, so the reader must get back to `read`
/// quickly. The parser takes everything waiting at once, so the cost of
/// waking it and starting a parse is paid once per burst, not once per read.
pub(crate) struct Output {
    handoff: Arc<Handoff>,
    parser: Option<std::thread::JoinHandle<()>>,
    sink: Arc<Sink>,
    notify: Arc<Notify>,
    /// Reads pushed since output was last handed over.
    reads: usize,
    /// Whether the reader had to wait for output since it last handed
    /// output over.
    rested: bool,
    /// A buffer for output the reader parses itself. It is swapped with the
    /// backlog, so both keep their capacity and reading never allocates.
    taken: Vec<u8>,
}

impl Output {
    /// Starts the parser thread, which passes output to `sink` and calls
    /// `notify` after each batch.
    pub(crate) fn new(
        sink: Sink,
        notify: Arc<Notify>,
        handoff: Arc<Handoff>,
    ) -> Result<Self, String> {
        let sink = Arc::new(sink);
        let (shared, parse_sink, parse_notify) = (handoff.clone(), sink.clone(), notify.clone());
        let parser = std::thread::Builder::new()
            .name("gui-term-parse".into())
            .stack_size(STACK)
            .spawn(move || {
                interactive_thread();
                parse(&shared, &parse_sink, &parse_notify)
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            handoff,
            parser: Some(parser),
            sink,
            notify,
            reads: 0,
            rested: true,
            taken: Vec::with_capacity(BATCH),
        })
    }

    /// Queues `bytes` for the parser without waking it; `idle` or `wake`
    /// does that. If `BATCH` bytes are already waiting, wakes the parser and
    /// waits until it makes room, so a slow parser slows the program down.
    /// Returns false once the transport is closed.
    pub(crate) fn push(&mut self, mut bytes: &[u8]) -> bool {
        let mut state = self.handoff.state.lock();
        loop {
            if state.closed {
                return false;
            }
            let room = (BATCH - state.bytes.len()).min(bytes.len());
            state.bytes.extend_from_slice(&bytes[..room]);
            bytes = &bytes[room..];
            if bytes.is_empty() {
                self.reads += 1;
                return true;
            }
            self.handoff.arrived.notify_one();
            self.handoff.room.wait(&mut state);
        }
    }

    /// Wakes the parser for the output pushed since the last wake.
    // Only the Windows reader uses this: its reads block, so it hands output
    // over while more may be waiting. The Unix reader uses `idle`.
    #[cfg(any(windows, test))]
    pub(crate) fn wake(&mut self) {
        // The parser checks for output and starts waiting while holding the
        // backlog's lock, which every push also took, so no wake is lost.
        if std::mem::take(&mut self.reads) > 0 {
            self.handoff.arrived.notify_one();
        }
    }

    /// Records that the reader had to wait for this output.
    pub(crate) fn rested(&mut self) {
        self.rested = true;
    }

    /// Called when the reader has nothing more to read. Usually wakes the
    /// parser for the queued output. One case is parsed here instead: a
    /// single read after a wait (such as the echo of a keystroke) while the
    /// parser is idle. Parsing it here skips waking the parser, so it reaches
    /// the screen sooner. Returns true if it parsed here; the reader then
    /// reads once more before waiting, and anything that arrived meanwhile
    /// is treated as a stream and goes to the parser.
    pub(crate) fn idle(&mut self) -> bool {
        let reads = std::mem::take(&mut self.reads);
        let rested = std::mem::replace(&mut self.rested, false);
        if reads == 0 {
            return false;
        }
        let mut state = self.handoff.state.lock();
        if !(rested && reads == 1 && state.parked) || state.bytes.is_empty() {
            drop(state);
            self.handoff.arrived.notify_one();
            return false;
        }
        // The parser is idle with all earlier output parsed, and only this
        // reader adds output, so parsing here keeps the output in order.
        std::mem::swap(&mut self.taken, &mut state.bytes);
        drop(state);
        (self.sink)(&self.taken);
        self.notify.fire();
        self.taken.clear();
        true
    }

    /// Parses the output handed off and not yet parsed, then ends the parser.
    pub(crate) fn finish(mut self) {
        {
            let mut state = self.handoff.state.lock();
            state.finishing = true;
            self.handoff.arrived.notify_all();
        }
        if let Some(parser) = self.parser.take() {
            let _ = parser.join();
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Some(parser) = self.parser.take() {
            self.handoff.close();
            let _ = parser.join();
        }
    }
}

/// Runs the calling thread, which carries a terminal's output to the screen,
/// at the priority of the work a person waits on.
pub(crate) fn interactive_thread() {
    // macOS schedules threads by quality-of-service class; one the program
    // starts has the default class, below the main thread it feeds, and on a
    // machine with few cores waits behind it between pseudo-terminal reads.
    #[cfg(target_vendor = "apple")]
    // SAFETY: sets only the calling thread's own class; no pointers.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

/// The parser thread: takes the backlog whole and parses it, until the
/// transport closes, or finishes and the backlog is empty.
fn parse(handoff: &Handoff, sink: &Sink, notify: &Notify) {
    let mut taken = Vec::new();
    loop {
        let mut state = handoff.state.lock();
        if state.closed {
            return;
        }
        if state.bytes.is_empty() {
            // Nothing left to parse: free the parse buffer before waiting.
            taken = Vec::new();
            if state.finishing {
                return;
            }
            state.release = false;
            drop(state);
            let bound = windowing::idle::thread_idle();
            let mut state = handoff.state.lock();
            if state.bytes.is_empty() && !(state.closed || state.finishing || state.release) {
                state.parked = true;
                match bound {
                    Some(bound) => {
                        handoff.arrived.wait_for(&mut state, bound);
                    }
                    None => handoff.arrived.wait(&mut state),
                }
                state.parked = false;
            }
            continue;
        }
        taken.extend_from_slice(&state.bytes);
        state.bytes.clear();
        handoff.room.notify_one();
        drop(state);
        sink(&taken);
        notify.fire();
        taken.clear();
    }
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
    use std::sync::{Arc, mpsc};

    use parking_lot::Mutex;

    use super::{BATCH, Handoff, Notify, Output, Queue, Sink, names_another_terminal};

    fn output(sink: Sink) -> (Output, Arc<Handoff>) {
        let handoff = Arc::new(Handoff::new());
        let notify = Arc::new(Notify::new(Arc::new(|| {})));
        (Output::new(sink, notify, handoff.clone()).unwrap(), handoff)
    }

    impl super::Backlog {
        // The parser waits, with all output parsed.
        fn waits_empty(&self) -> bool {
            self.parked && self.bytes.is_empty()
        }
    }

    // A sink that records each batch it parses and the thread it ran on.
    type Parsed = Arc<Mutex<Vec<(Vec<u8>, String)>>>;

    fn recording() -> (Sink, Parsed) {
        let batches = Arc::new(Mutex::new(Vec::new()));
        let seen = batches.clone();
        let sink = Box::new(move |bytes: &[u8]| {
            let name = std::thread::current()
                .name()
                .unwrap_or_default()
                .to_string();
            seen.lock().push((bytes.to_vec(), name));
        });
        (sink, batches)
    }

    // A sink whose first parse tells `entered` it began, then waits for
    // `open` before it records anything.
    struct Gate {
        entered: mpsc::Receiver<()>,
        open: mpsc::Sender<()>,
        batches: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    fn gated() -> (Sink, Gate) {
        let (enter, entered) = mpsc::channel();
        let (open, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(Some((enter, gate)));
        let batches = Arc::new(Mutex::new(Vec::new()));
        let seen = batches.clone();
        let sink = Box::new(move |bytes: &[u8]| {
            if let Some((enter, gate)) = gate.lock().take() {
                let _ = enter.send(());
                let _ = gate.recv();
            }
            seen.lock().push(bytes.to_vec());
        });
        (
            sink,
            Gate {
                entered,
                open,
                batches,
            },
        )
    }

    #[test]
    fn output_is_parsed_whole_and_in_order_on_the_parser_thread() {
        let (sink, batches) = recording();
        let (mut output, _) = output(sink);
        let mut sent = Vec::new();
        for n in 0..200u32 {
            let chunk = n.to_string().into_bytes();
            assert!(output.push(&chunk));
            if n % 7 == 0 {
                output.wake();
            }
            sent.extend_from_slice(&chunk);
        }
        output.finish();
        let batches = batches.lock();
        let parsed: Vec<u8> = batches
            .iter()
            .flat_map(|(bytes, _)| bytes.clone())
            .collect();
        assert_eq!(parsed, sent);
        assert!(batches.iter().all(|(_, name)| name == "gui-term-parse"));
    }

    #[test]
    fn output_before_a_pause_is_parsed_on_the_reader_while_the_parser_waits() {
        let (sink, batches) = recording();
        let (mut output, handoff) = output(sink);
        for chunk in [b"ab", b"cd"] {
            // The parser parks once it finds nothing to parse.
            while !handoff.state.lock().waits_empty() {
                std::thread::yield_now();
            }
            assert!(output.push(chunk));
            assert!(output.idle());
            assert!(!output.idle());
            output.rested();
        }
        output.finish();
        let reader = std::thread::current()
            .name()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            *batches.lock(),
            vec![(b"ab".to_vec(), reader.clone()), (b"cd".to_vec(), reader)]
        );
    }

    #[test]
    fn output_after_a_parse_on_the_reader_or_of_several_reads_goes_to_the_parser() {
        let (sink, batches) = recording();
        let (mut output, handoff) = output(sink);
        let parked = || {
            while !handoff.state.lock().waits_empty() {
                std::thread::yield_now();
            }
        };
        parked();
        assert!(output.push(b"a"));
        assert!(output.idle());
        // Output came while the reader parsed: the stream goes on.
        assert!(output.push(b"b"));
        assert!(!output.idle());
        output.rested();
        parked();
        for chunk in [b"c", b"d"] {
            assert!(output.push(chunk));
        }
        assert!(!output.idle());
        output.finish();
        let reader = std::thread::current()
            .name()
            .unwrap_or_default()
            .to_string();
        let parser = "gui-term-parse".to_string();
        assert_eq!(
            *batches.lock(),
            vec![
                (b"a".to_vec(), reader),
                (b"b".to_vec(), parser.clone()),
                (b"cd".to_vec(), parser)
            ]
        );
    }

    #[test]
    fn output_read_during_a_parse_is_taken_in_one_batch() {
        let (sink, gate) = gated();
        let (mut output, _) = output(sink);
        assert!(output.push(b"a"));
        output.wake();
        gate.entered.recv().unwrap();
        // The parser is busy, so output at a pause still goes to it.
        for chunk in [b"b", b"c", b"d"] {
            assert!(output.push(chunk));
            output.idle();
        }
        gate.open.send(()).unwrap();
        output.finish();
        assert_eq!(*gate.batches.lock(), vec![b"a".to_vec(), b"bcd".to_vec()]);
    }

    #[test]
    fn a_parser_behind_holds_the_reader_to_a_batch() {
        let (sink, gate) = gated();
        let (mut output, _) = output(sink);
        let sent: Vec<u8> = (0..3 * BATCH + 5).map(|n| n as u8).collect();
        assert!(output.push(&sent[..1]));
        output.wake();
        gate.entered.recv().unwrap();
        let reader = {
            let sent = sent.clone();
            std::thread::spawn(move || (output.push(&sent[1..]), output))
        };
        gate.open.send(()).unwrap();
        let (pushed, output) = reader.join().unwrap();
        assert!(pushed);
        output.finish();
        let batches = gate.batches.lock();
        assert!(batches.iter().all(|batch| batch.len() <= BATCH));
        assert_eq!(batches.concat(), sent);
    }

    #[test]
    fn closing_releases_a_reader_waiting_for_room() {
        let (sink, gate) = gated();
        let (mut output, handoff) = output(sink);
        assert!(output.push(b"a"));
        output.wake();
        gate.entered.recv().unwrap();
        let reader = std::thread::spawn(move || (output.push(&vec![0; 2 * BATCH]), output));
        handoff.close();
        let (pushed, mut output) = reader.join().unwrap();
        assert!(!pushed);
        assert!(!output.push(b"b"));
        gate.open.send(()).unwrap();
        drop(output);
    }

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
