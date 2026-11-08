//! Terminals: VT screen state with bounded scrollback, an optional program on
//! a native pseudo-terminal, and the widget that shows and drives one.
//!
//! A terminal is a session-owned handle. Its program's output arrives on IO
//! threads, which only wake the session; the session's UI thread parses it
//! while settling, so screen state is only ever touched on that thread.

pub mod api;
pub mod engine;
pub mod frame;
mod images;
pub mod keyboard;
mod layer;
pub mod links;
mod modes;
mod shapes;
mod transport;
pub mod widget;

#[cfg(windows)]
mod conpty;
#[cfg(unix)]
mod pty;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};

use encoding_rs::{Decoder, Encoding};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::memory::{Budget, ByteCache, Lease};
use crate::text::layout::TextLayout;
use crate::widgets::cx::UiMsg;
use engine::Engine;

#[cfg(windows)]
use conpty::Pty as Transport;
#[cfg(unix)]
use pty::Pty as Transport;

const HANDLE_LIMIT: usize = 256;
const ENTRY_BYTES: usize = 16 * 1024;
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
    /// The link under the pointer when a button last pressed on the screen,
    /// or empty when the press was off any link.
    LinkPointed(String),
    /// The user asked to open a link: Ctrl+click, or Command+click on macOS.
    LinkActivated(String),
    /// The program sent a desktop notification (OSC 9 or 777): its title,
    /// empty when it gave none, and its body.
    Notify(String, String),
    /// The program wrote this text to the clipboard (OSC 52).
    Clipboard(String),
    /// The program wrote this text to the primary selection (OSC 52).
    PrimarySelection(String),
    /// The user asked to open a file path the screen shows: its absolute
    /// path and the line and column after it, 0 when it names none.
    PathActivated(String, i64, i64),
    /// A paste of several lines or of control characters waits for the
    /// application to confirm it, which then pastes the text itself.
    PasteHeld(String),
    /// The terminal's view took keyboard focus, as a click on it gives it.
    Focused,
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
    /// Rows of history kept above the screen.
    pub history: i64,
    pub selection: Option<(Cell, Cell)>,
    pub reserved_bytes: i64,
    /// The program's process id, 0 without a program.
    pub pid: i64,
    /// A job other than the program runs in the foreground, as a shell's
    /// command does; closing the terminal would end it.
    #[serde(default)]
    pub busy: bool,
    /// The program name of the foreground job while `busy`, where the system
    /// names it.
    #[serde(default)]
    pub job: String,
    /// Input bytes waiting for the program, and output waiting to be parsed:
    /// none, since output is parsed as it is read.
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
    /// Set after a key is encoded whose text a platform may also deliver;
    /// it lasts until that key's release or the next key's press.
    suppress_text: bool,
    /// The view size the screen last refused, so each refusal is reported
    /// once, and the report waiting for the next pump.
    refused: Option<(u16, u16)>,
    refusal: Option<String>,
    /// A paste the program has not read all of yet.
    paste: Option<Paste>,
    /// Output parsed since the last pump that the screen shows, which the
    /// session repaints for.
    output_shown: bool,
    /// The revision the last pump saw, and the one last given to the
    /// compactor.
    pumped: u64,
    compacted: u64,
    /// Copy mode or link hints, which take the view's keys while on.
    mode: Option<modes::Mode>,
    /// The program's character set, or `None` for UTF-8, which passes
    /// through unchanged.
    charset: Option<Charset>,
    /// Layouts of the screen's text runs, sized to the screen by the widget
    /// that paints it.
    layouts: ByteCache<String, TextLayout>,
    /// The font registry generation `layouts` were made in.
    layouts_generation: u64,
    /// The rows the widget recorded when it last painted the screen.
    rows: widget::RowCache,
    /// The text as pixels kept between paints.
    layer: layer::Layer,
    /// The links the screen showed when the widget last painted it.
    links: Vec<links::Link>,
    /// The link under the pointer, which the screen underlines.
    hovered: Option<Arc<str>>,
    /// The link under the pointer when it last pressed a button, which the
    /// application hears of for its context menu.
    pointed: Option<Arc<str>>,
    /// The program is drawing a synchronized frame, so its output is shown
    /// once the frame ends.
    synchronized: bool,
    /// Whether the program last heard the view had focus.
    focused: bool,
    /// Stretches of the buffer the application highlights, in buffer rows.
    highlights: Vec<Span>,
    /// The pixels of the images the screen last showed, so a paint draws
    /// the same pixels the last one did.
    pictures: images::Pictures,
    /// When an animated image the screen shows next changes frame.
    animation_due: Option<i64>,
    /// Whether new output, and typing, bring a view scrolled back to the
    /// live screen; the widget sets them from its options.
    scroll_on_output: bool,
    scroll_on_keystroke: bool,
    /// The bells the widget has answered, and when its flash ends.
    bells_flashed: u64,
    flash_until: i64,
    /// The pointer is dragging the scrollbar's thumb.
    scrollbar_drag: bool,
}

/// A stretch of a terminal's buffer, its end excluded, in rows counted from
/// the oldest history row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: Cell,
    pub end: Cell,
}

/// A non-UTF-8 character set the program reads and writes: its output is
/// decoded to UTF-8 for the screen, and input is encoded from UTF-8.
struct Charset {
    encoding: &'static Encoding,
    /// Holds a multibyte sequence split across reads.
    decoder: Decoder,
}

impl Charset {
    fn decode(&mut self, bytes: &[u8]) -> String {
        let capacity = self
            .decoder
            .max_utf8_buffer_length(bytes.len())
            .unwrap_or(bytes.len().saturating_mul(3));
        let mut text = String::with_capacity(capacity);
        // The buffer holds the longest decoding, so one call consumes `bytes`.
        let _ = self.decoder.decode_to_string(bytes, &mut text, false);
        text
    }

    // Characters the set cannot represent are sent as `?`.
    fn encode(&self, bytes: &[u8]) -> Vec<u8> {
        let text = String::from_utf8_lossy(bytes);
        let mut encoder = self.encoding.new_encoder();
        let mut rest: &str = &text;
        let mut out = Vec::new();
        loop {
            if let Some(more) = encoder.max_buffer_length_from_utf8_without_replacement(rest.len())
            {
                out.reserve(more);
            }
            let (result, read) =
                encoder.encode_from_utf8_to_vec_without_replacement(rest, &mut out, true);
            rest = &rest[read..];
            match result {
                encoding_rs::EncoderResult::InputEmpty => return out,
                encoding_rs::EncoderResult::Unmappable(_) => out.push(b'?'),
                encoding_rs::EncoderResult::OutputFull => {}
            }
        }
    }
}

/// The character set `label` names (a WHATWG label such as "ISO-8859-1",
/// "windows-1251" or "Shift_JIS"), or `None` for UTF-8. Sets a terminal
/// cannot carry are refused: UTF-16, whose bytes are not ASCII, and
/// ISO-2022-JP, whose escapes collide with control sequences.
fn charset(label: &str) -> Result<Option<Charset>, String> {
    let encoding = Encoding::for_label(label.trim().as_bytes())
        .ok_or_else(|| format!("unknown character set {label}"))?;
    if encoding == encoding_rs::UTF_8 {
        return Ok(None);
    }
    if encoding.output_encoding() != encoding || encoding == encoding_rs::ISO_2022_JP {
        return Err(format!(
            "a terminal cannot use the {} character set",
            encoding.name()
        ));
    }
    Ok(Some(Charset {
        encoding,
        decoder: encoding.new_decoder_without_bom_handling(),
    }))
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
            paste: None,
            output_shown: false,
            pumped: 0,
            compacted: 0,
            mode: None,
            charset: None,
            layouts: ByteCache::new(0),
            layouts_generation: 0,
            rows: widget::RowCache::default(),
            layer: layer::Layer::default(),
            links: Vec::new(),
            hovered: None,
            pointed: None,
            synchronized: false,
            focused: false,
            highlights: Vec::new(),
            pictures: images::Pictures::default(),
            animation_due: None,
            scroll_on_output: false,
            scroll_on_keystroke: true,
            bells_flashed: 0,
            flash_until: 0,
            scrollbar_drag: false,
        },
    );
    Ok(id)
}

/// The directory terminal `id`'s program works in: the one it last reported,
/// else the foreground job's, else the program's; empty when none is known.
pub fn cwd(id: i64) -> Result<String, String> {
    with(id, |entry| {
        Ok(entry
            .engine
            .current_dir()
            .or_else(|| entry.transport.as_ref().and_then(Transport::cwd))
            .unwrap_or_default())
    })
}

/// The stretches of terminal `id`'s buffer the shell marked as prompts,
/// inputs and outputs, oldest first.
pub fn zones(id: i64) -> Result<Vec<engine::Zone>, String> {
    with(id, |entry| Ok(entry.engine.zones()))
}

/// The most stretches one terminal highlights at once.
const HIGHLIGHT_LIMIT: usize = 10_000;

/// Highlights `spans` of terminal `id`'s buffer, replacing any it had.
pub fn set_highlights(id: i64, spans: Vec<Span>) -> Result<(), String> {
    if spans.len() > HIGHLIGHT_LIMIT {
        return Err(format!(
            "a terminal highlights at most {HIGHLIGHT_LIMIT} stretches"
        ));
    }
    with(id, |entry| {
        if entry.highlights != spans {
            entry.highlights = spans;
            entry.revision += 1;
        }
        Ok(())
    })
}

/// Writes the replies the program asked for to it.
fn send_replies(entry: &mut Entry) {
    let replies = entry.engine.take_replies();
    if !replies.is_empty()
        && let Some(transport) = &entry.transport
    {
        let _ = transport.write(&replies);
    }
}

/// Reports a pointer event to terminal `id`'s program.
fn mouse(entry: &mut Entry, event: wezterm_term::MouseEvent) {
    entry.engine.mouse(event);
    send_replies(entry);
}

/// Tells terminal `id`'s program whether its view has focus, once per change.
fn focus(entry: &mut Entry, focused: bool) {
    if entry.focused != focused {
        entry.focused = focused;
        entry.engine.focus(focused);
        send_replies(entry);
    }
}

/// Every row of terminal `id`'s buffer, its oldest history row first; the
/// last `Snapshot::rows` rows are the screen.
pub fn rows(id: i64) -> Result<Vec<engine::Row>, String> {
    with(id, |entry| Ok(entry.engine.rows()))
}

/// Sends signal `signum` to terminal `id`'s foreground job (POSIX only).
pub fn signal(id: i64, signum: i64) -> Result<(), String> {
    let signum = i32::try_from(signum).map_err(|_| format!("{signum} is not a signal number"))?;
    with(id, |entry| {
        entry
            .transport
            .as_ref()
            .ok_or("the terminal runs no program")?
            .signal(signum)
    })
}

/// Sets the character set terminal `id`'s program reads and writes, from
/// the next output on; "UTF-8" restores the default.
pub fn set_encoding(id: i64, label: &str) -> Result<(), String> {
    let chosen = charset(label)?;
    with(id, |entry| {
        entry.charset = chosen;
        Ok(())
    })
}

/// Parses output terminal `id`'s program wrote, answering what the program
/// asked of the terminal.
fn take_output(entry: &mut Entry, bytes: &[u8]) {
    let processed = match &mut entry.charset {
        Some(charset) => entry.engine.process(charset.decode(bytes).as_bytes()),
        None => entry.engine.process(bytes),
    };
    if processed.is_ok() {
        entry.revision += 1;
        if entry.scroll_on_output && entry.scrolled != 0 {
            entry.scrolled = 0;
            entry.engine.scroll(0);
        }
        // A synchronized frame shows once the program ends it.
        let synchronized = entry.engine.synchronized();
        entry.output_shown |= !synchronized;
        entry.synchronized = synchronized;
    }
    send_replies(entry);
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
        let cell = entry.engine.cell();
        // The transport parses the output it reads on its own threads, under
        // the registry lock, while the session's thread paints what came
        // before.
        let sink: transport::Sink = Box::new(move |bytes: &[u8]| {
            if let Some(entry) = TERMINALS.lock().get_mut(&id) {
                take_output(entry, bytes);
            }
        });
        entry.transport = Some(Transport::spawn(
            command,
            rows,
            cols,
            cell,
            &entry.budget,
            wake,
            sink,
        )?);
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

/// Sends typed `bytes` as `input` does, answering whether the view changed:
/// typing scrolls it back to the live screen and drops the selection.
pub(crate) fn typed(id: i64, bytes: &[u8]) -> Result<bool, String> {
    with(id, |entry| {
        let before = entry.revision;
        send(entry, bytes)?;
        Ok(entry.revision != before)
    })
}

fn send(entry: &mut Entry, bytes: &[u8]) -> Result<(), String> {
    let transport = entry
        .transport
        .as_ref()
        .ok_or("the terminal runs no program")?;
    let bytes = match &entry.charset {
        Some(charset) => charset.encode(bytes),
        None => bytes.to_vec(),
    };
    if let Some(paste) = &mut entry.paste {
        // Escape or Ctrl+C stops a paste still going out; other input waits
        // for it to finish.
        if bytes == b"\x1b" || bytes == b"\x03" {
            paste.sent = paste.body.len();
        }
        if paste.after.len() + bytes.len() > transport::INPUT {
            return Err("terminal input waiting for a paste exceeds 64 KiB".into());
        }
        paste.after.extend_from_slice(&bytes);
        feed_paste(entry);
    } else {
        transport.write(&bytes)?;
    }
    settle_input(entry);
    Ok(())
}

/// Pastes `text`, bracketed when the program asked for bracketed paste.
pub fn paste(id: i64, text: &str) -> Result<(), String> {
    with(id, |entry| {
        if entry.paste.is_some() {
            return Err("an earlier paste is still being sent".into());
        }
        entry
            .transport
            .as_ref()
            .ok_or("the terminal runs no program")?;
        let (body, end) = entry.engine.paste(text)?;
        let (body, end) = match &entry.charset {
            Some(charset) => (charset.encode(&body), charset.encode(&end)),
            None => (body, end),
        };
        let lease = entry.budget.reserve(body.len() + end.len())?;
        entry.paste = Some(Paste {
            body,
            sent: 0,
            end,
            after: Vec::new(),
            _lease: lease,
        });
        feed_paste(entry);
        settle_input(entry);
        Ok(())
    })
}

/// A paste on its way to the program, which reads it as fast as it can: the
/// input queue takes it a part at a time, so keys typed meanwhile follow it.
struct Paste {
    body: Vec<u8>,
    sent: usize,
    /// Bracketed paste's closing mark, sent even when the paste is cancelled.
    end: Vec<u8>,
    /// Input typed while the paste was going out, which follows it.
    after: Vec<u8>,
    _lease: Lease,
}

/// Queues as much of the pending paste as the input queue has room for.
fn feed_paste(entry: &mut Entry) {
    let (Some(paste), Some(transport)) = (&mut entry.paste, &entry.transport) else {
        return;
    };
    loop {
        let room = transport.input_room().min(transport::CHUNK * 8);
        let left = paste.body.len() - paste.sent;
        if left == 0 {
            let tail = [
                std::mem::take(&mut paste.end),
                std::mem::take(&mut paste.after),
            ]
            .concat();
            if tail.len() > room {
                paste.end = tail;
                transport.want_input_room();
                return;
            }
            if transport.write(&tail).is_err() {
                transport.want_input_room();
                paste.end = tail;
                return;
            }
            entry.paste = None;
            return;
        }
        if room == 0 {
            transport.want_input_room();
            return;
        }
        let take = left.min(room);
        if transport
            .write(&paste.body[paste.sent..paste.sent + take])
            .is_err()
        {
            transport.want_input_room();
            return;
        }
        paste.sent += take;
    }
}

/// Stops the pending paste where the program has got to: its closing mark and
/// the input typed meanwhile still go out.
pub fn cancel_paste(id: i64) -> Result<(), String> {
    with(id, |entry| {
        if let Some(paste) = &mut entry.paste {
            paste.sent = paste.body.len();
        }
        feed_paste(entry);
        Ok(())
    })
}

/// Pastes `text` from the terminal's own view: a paste that fails is reported
/// from the next pump as a refusal, since the view has no caller to answer.
pub(crate) fn paste_from_view(id: i64, text: &str) -> bool {
    match paste(id, text) {
        Ok(()) => true,
        Err(why) => {
            with_entry(id, |entry| {
                entry.refusal = Some(format!("Couldn't paste: {why}"))
            });
            false
        }
    }
}

/// Starts copy mode on terminal `id`, its cursor where the program's is:
/// keys move it over the screen and the history, select, and copy.
pub fn copy_mode(id: i64) -> Result<(), String> {
    with(id, |entry| {
        let (row, col) = entry.engine.cursor();
        let cursor = (entry.engine.stable_top() + row as isize, col);
        entry.mode = Some(modes::Mode::Copy(modes::Copy {
            cursor,
            anchor: None,
        }));
        entry.selection = None;
        entry.revision += 1;
        Ok(())
    })
}

/// Labels the links on terminal `id`'s screen for keys to open them, or to
/// copy them when `copy` is set. Answers how many there are; with none, no
/// mode starts.
pub fn hints(id: i64, copy: bool) -> Result<i64, String> {
    with(id, |entry| {
        let links = entry.links.clone();
        if links.is_empty() {
            return Ok(0);
        }
        let labels = modes::hint_labels(links.len())
            .into_iter()
            .zip(links)
            .collect();
        entry.mode = Some(modes::Mode::Hints(modes::Hints {
            copy,
            labels,
            typed: String::new(),
        }));
        entry.revision += 1;
        Ok(entry.links.len() as i64)
    })
}

/// Ends copy mode or the hints on terminal `id`.
pub fn leave_mode(id: i64) -> Result<(), String> {
    with(id, |entry| {
        leave(entry);
        Ok(())
    })
}

fn leave(entry: &mut Entry) {
    if entry.mode.take().is_some() {
        entry.selection = None;
        entry.revision += 1;
    }
}

/// The keyboard mode terminal `id` is in: "copy", "hints", or empty.
pub fn mode(id: i64) -> String {
    with_entry(id, |entry| match entry.mode {
        Some(modes::Mode::Copy(_)) => "copy",
        Some(modes::Mode::Hints(_)) => "hints",
        None => "",
    })
    .unwrap_or("")
    .to_string()
}

/// Whether a paste to terminal `id`'s program is still going out.
pub fn pasting(id: i64) -> bool {
    with_entry(id, |entry| entry.paste.is_some()).unwrap_or(false)
}

/// What sending input does to the view besides the bytes.
fn settle_input(entry: &mut Entry) {
    // The user is answering what the screen shows, so a frame the program
    // left open (it may have stopped) is shown as it stands.
    if entry.synchronized {
        entry.engine.end_synchronized();
        entry.synchronized = false;
        entry.revision += 1;
    }
    if entry.scrolled != 0 && entry.scroll_on_keystroke {
        entry.scrolled = 0;
        entry.engine.scroll(0);
        entry.revision += 1;
    }
    if entry.selection.is_some() {
        entry.selection = None;
        entry.revision += 1;
    }
}

/// Resizes the screen and tells the program.
pub fn resize(id: i64, rows: i64, cols: i64) -> Result<(), String> {
    let rows = dimension(rows, 256, "rows")?;
    let cols = dimension(cols, 512, "columns")?;
    with(id, |entry| {
        let cell = entry.engine.cell();
        resize_entry(entry, rows, cols, cell)
    })
}

/// Resizes the screen to its view, whose cells are `cell` pixels, keeping a
/// refusal to report from the pump: the screen then keeps its size, and the
/// application hears why.
fn follow_view(entry: &mut Entry, rows: u16, cols: u16, cell: (u16, u16)) {
    match resize_entry(entry, rows, cols, cell) {
        Ok(()) => entry.refused = None,
        Err(why) if entry.refused != Some((rows, cols)) => {
            entry.refused = Some((rows, cols));
            entry.refusal = Some(format!("the screen cannot take {rows} x {cols}: {why}"));
        }
        Err(_) => {}
    }
}

fn resize_entry(entry: &mut Entry, rows: u16, cols: u16, cell: (u16, u16)) -> Result<(), String> {
    if entry.engine.size() == (rows, cols) && entry.engine.cell() == cell {
        return Ok(());
    }
    entry.engine.resize(rows, cols, cell)?;
    if let Some(transport) = &entry.transport {
        transport.resize(rows, cols, cell)?;
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

/// Writes terminal `id`'s text to the file at `path` as UTF-8: the selection
/// when `selection` is set, otherwise every row it keeps, oldest first. A
/// row that wraps runs on into the next one, so the program's lines come back
/// whole; trailing blank rows are left out. Answers the bytes written.
pub fn save_text(id: i64, path: &str, selection: bool) -> Result<i64, String> {
    use std::io::Write;
    let text = with(id, |entry| {
        Ok(if selection {
            selected_text(entry)
        } else {
            let mut out = String::new();
            for row in entry.engine.rows() {
                out.push_str(&row.text);
                if !row.wrapped {
                    out.push('\n');
                }
            }
            out.truncate(out.trim_end_matches(['\n', ' ']).len());
            out.push('\n');
            out
        })
    })?;
    let mut file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|e| format!("{path}: {e}"))?;
    Ok(text.len() as i64)
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
            .map_or((0, (0, 0)), |t| (t.pid(), (t.queued_input(), 0)));
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
            history: entry.engine.history() as i64,
            selection: entry.selection,
            reserved_bytes: (entry.engine.charged_bytes() + entry.engine.image_bytes()) as i64,
            pid: i64::from(pid),
            busy: entry
                .transport
                .as_ref()
                .is_some_and(|t| entry.exit.is_none() && t.busy()),
            job: entry
                .transport
                .as_ref()
                .filter(|_| entry.exit.is_none())
                .and_then(Transport::job)
                .unwrap_or_default(),
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
/// When the next animated image any terminal shows changes frame.
pub(crate) fn animation_due() -> Option<i64> {
    TERMINALS
        .lock()
        .values()
        .filter_map(|entry| entry.animation_due)
        .min()
}

/// Takes what the terminals of `budget` did since the last pump. When `held`,
/// a window already waits to paint output: the transports' wakes stay taken,
/// so more output does not wake the session before that frame, which takes
/// the programs' ends and events.
pub(crate) fn pump(budget: &Budget, held: bool) -> (bool, Vec<UiMsg>) {
    let mut changed = false;
    let mut events = Vec::new();
    let mut terminals = TERMINALS.lock();
    for (id, entry) in terminals.iter_mut().filter(|(_, e)| e.budget == *budget) {
        if let Some(why) = entry.refusal.take() {
            changed = true;
            events.push(UiMsg::Terminal(*id, Event::Refused(why)));
        }
        if entry.paste.is_some() {
            feed_paste(entry);
        }
        changed |= std::mem::take(&mut entry.output_shown);
        entry.pumped = entry.revision;
        let Some(transport) = &entry.transport else {
            continue;
        };
        if held || !transport.has_pending_output() {
            continue;
        }
        // The transport parsed the output; what is left is its end and the
        // program's exit.
        let drained = transport.drain();
        for (title, body) in entry.engine.take_toasts() {
            events.push(UiMsg::Terminal(*id, Event::Notify(title, body)));
        }
        let (clipboard, primary) = entry.engine.take_clipboard();
        if let Some(text) = clipboard {
            events.push(UiMsg::Terminal(*id, Event::Clipboard(text)));
        }
        if let Some(text) = primary {
            events.push(UiMsg::Terminal(*id, Event::PrimarySelection(text)));
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
        // A program that ends inside a synchronized frame leaves it to show.
        if drained.eof && entry.synchronized {
            entry.engine.end_synchronized();
            entry.synchronized = false;
            changed = true;
        }
        // An exit is reported once the output before it has been parsed.
        if !entry.exit_reported
            && drained.eof
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

/// Whether terminal `id`'s output wake is taken: output since the last pump
/// that took it does not wake the session.
#[cfg(all(test, unix))]
pub(crate) fn output_wake_taken(id: i64) -> bool {
    with_entry(id, |e| {
        e.transport.as_ref().is_some_and(|t| t.has_pending_output())
    })
    .unwrap_or(false)
}

/// Readies the terminals for the session's thread to wait: their history
/// rows its reads inflated, drawing or searching, are compressed again. Once
/// the windows show all the output there is (`presented`), each terminal
/// that took none since the last pump has its history compacted off this
/// thread; output that keeps arriving holds this off, so a flood's rows are
/// not compressed only to be dropped.
pub fn before_wait(presented: bool) {
    let compactor = compactor();
    for (id, entry) in TERMINALS.lock().iter_mut() {
        entry.engine.recompress_read_history();
        if presented && entry.revision == entry.pumped && entry.compacted != entry.revision {
            entry.compacted = entry.revision;
            match compactor {
                Some(compactor) => compactor.request(*id, entry.revision),
                None => entry.engine.compact_history(),
            }
        }
    }
}

// Compacts the histories `before_wait` hands it on a thread of its own, a
// block at a time, stopping at the first output since the request.
struct Compactor {
    queue: Mutex<VecDeque<(i64, u64)>>,
    ready: parking_lot::Condvar,
}

impl Compactor {
    fn request(&self, id: i64, revision: u64) {
        self.queue.lock().push_back((id, revision));
        self.ready.notify_one();
    }

    fn run(&self) {
        loop {
            let (id, revision) = {
                let mut queue = self.queue.lock();
                loop {
                    if let Some(request) = queue.pop_front() {
                        break request;
                    }
                    match windowing::idle::thread_idle() {
                        Some(due) => {
                            self.ready.wait_for(&mut queue, due);
                        }
                        None => self.ready.wait(&mut queue),
                    }
                }
            };
            loop {
                let mut terminals = TERMINALS.lock();
                let Some(entry) = terminals.get_mut(&id) else {
                    break;
                };
                if entry.revision != revision || !entry.engine.compact_history_step() {
                    // The rows compacted away belong to the reader, which
                    // made them, to hand back.
                    if let Some(transport) = &entry.transport {
                        transport.release_memory();
                    }
                    break;
                }
            }
        }
    }
}

static COMPACTOR: Compactor = Compactor {
    queue: Mutex::new(VecDeque::new()),
    ready: parking_lot::Condvar::new(),
};

// The compactor, its thread started on first use; `None` when the thread
// could not start, and the session's thread compacts instead.
fn compactor() -> Option<&'static Compactor> {
    static STARTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let started = STARTED.get_or_init(|| {
        std::thread::Builder::new()
            .name("gui-term-compact".into())
            .stack_size(256 * 1024)
            .spawn(|| COMPACTOR.run())
            .is_ok()
    });
    started.then_some(&COMPACTOR)
}

/// Runs `f` on a terminal's entry for the widget, which draws and drives it.
fn with_entry<T>(id: i64, f: impl FnOnce(&mut Entry) -> T) -> Option<T> {
    TERMINALS.lock().get_mut(&id).map(f)
}

/// Lets go of the text pixels of terminals no paint drew since the last
/// sweep; answers whether any terminal still holds some.
pub fn sweep_layers() -> bool {
    let mut held = false;
    for entry in TERMINALS.lock().values_mut() {
        held |= entry.layer.sweep();
    }
    held
}

/// Whether any terminal keeps its text as pixels for a later sweep.
pub fn holds_layers() -> bool {
    TERMINALS.lock().values().any(|entry| entry.layer.holds())
}

/// Terminals open across every session, for disposal checks.
pub fn open_count() -> usize {
    TERMINALS.lock().len()
}

#[cfg(test)]
mod tests;
