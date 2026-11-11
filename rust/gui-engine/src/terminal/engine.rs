//! VT parsing and screen state: a WezTerm terminal core that reflows its
//! lines when it resizes and packs scrollback lines as they leave the screen,
//! behind filtered control strings, byte-admitted storage and bounded device
//! replies.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;
use wezterm_term::color::{ColorAttribute, ColorPalette};
use wezterm_term::{
    Alert, AlertHandler, CellAttributes, Clipboard, ClipboardSelection, CursorVisibility,
    Intensity, Line, MouseEvent, SemanticType, StableRowIndex, Terminal, TerminalConfiguration,
    TerminalSize, Underline,
};

use crate::memory::{Budget, Lease};

/// One terminal's parser, screen and scrollback, charged to a session budget.
pub struct Engine {
    term: Terminal,
    config: Arc<Config>,
    notes: Arc<Mutex<Notifications>>,
    /// The top row shown while the view is scrolled back, as a row index that
    /// stays with its line as output scrolls; None shows the live screen.
    view_top: Option<StableRowIndex>,
    filter: Filter,
    lease: Lease,
    budget: Budget,
    max_rows: u16,
    max_cols: u16,
    history: usize,
    byte_limit: usize,
    charged: usize,
    /// The session's charge for the images the screen holds.
    images: Lease,
    /// The pixel size of a cell as the view draws it; zero until it has.
    cell: (u16, u16),
}

/// The most memory one terminal's images may take, while the session's
/// budget has room for them.
pub(crate) const IMAGE_LIMIT: usize = 32 * 1024 * 1024;
/// The largest paste a terminal takes; it reaches the program as fast as the
/// program reads it.
pub(crate) const PASTE_LIMIT: usize = 16 * 1024 * 1024;

/// What the terminal core asks of its host: the scrollback to keep, the
/// palette its view shows, which a program's color queries answer from, and
/// the memory its images may take.
#[derive(Debug)]
struct Config {
    history: usize,
    /// The memory the history may hold; past it the oldest rows go first.
    history_bytes: AtomicUsize,
    palette: Mutex<ColorPalette>,
    image_limit: AtomicUsize,
}

impl TerminalConfiguration for Config {
    fn scrollback_size(&self) -> usize {
        self.history
    }

    fn scrollback_bytes(&self) -> usize {
        self.history_bytes.load(Ordering::Relaxed)
    }

    fn color_palette(&self) -> ColorPalette {
        self.palette.lock().clone()
    }

    fn enable_kitty_graphics(&self) -> bool {
        true
    }

    fn enable_kitty_keyboard(&self) -> bool {
        true
    }

    fn image_memory_limit(&self) -> usize {
        self.image_limit.load(Ordering::Relaxed)
    }
}

/// The most desktop notifications a terminal holds before the session
/// hears of them; a program that sends more loses the oldest.
const TOAST_LIMIT: usize = 8;

/// Receives what the terminal reports while it parses: titles, bells and
/// desktop notifications.
struct Alerts(Arc<Mutex<Notifications>>);

impl AlertHandler for Alerts {
    fn alert(&mut self, alert: Alert) {
        let mut notes = self.0.lock();
        match alert {
            Alert::WindowTitleChanged(title) => notes.set_title(&title),
            Alert::Bell => notes.bells = notes.bells.saturating_add(1),
            Alert::ToastNotification { title, body, .. } => {
                if notes.toasts.len() == TOAST_LIMIT {
                    notes.toasts.pop_front();
                }
                let title = printable(title.as_deref().unwrap_or_default(), 256);
                let body = printable(&body, 1024);
                notes.toasts.push_back((title, body));
            }
            // Colors are the view's, and the session already redraws on every
            // change.
            _ => {}
        }
    }
}

/// `text` without control characters, at most `limit` characters long.
fn printable(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect()
}

/// Takes what a program writes to the clipboard (OSC 52). Nothing reads the
/// clipboard back: the core has no path for a program to query it.
struct Clip(Arc<Mutex<Notifications>>);

impl Clipboard for Clip {
    fn set_contents(
        &self,
        selection: ClipboardSelection,
        data: Option<String>,
    ) -> anyhow::Result<()> {
        if let Some(text) = data {
            let mut notes = self.0.lock();
            match selection {
                ClipboardSelection::Clipboard => notes.clipboard = Some(text),
                ClipboardSelection::PrimarySelection => notes.primary = Some(text),
            }
        }
        Ok(())
    }
}

/// Receives the replies a program asked for, to write back to it.
struct Replies(Arc<Mutex<Notifications>>);

impl std::io::Write for Replies {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().reply(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Filter {
    state: FilterState,
    string: Vec<u8>,
    /// The string held is an OSC, which BEL also ends.
    osc: bool,
    overflow: bool,
    dropped: usize,
}

/// Where the filter is in an escape sequence it intercepts.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum FilterState {
    #[default]
    Ground,
    Escape,
    /// Inside an OSC, DCS or APC string, held until it ends.
    Text,
    TextEscape,
    Ignored,
    IgnoredEscape,
}

/// The title, bell count, notifications, clipboard text and pending replies a
/// terminal has reported.
#[derive(Default, Clone)]
pub struct Notifications {
    pub title: String,
    /// Increments whenever the program sets a title.
    pub title_revision: u64,
    pub bells: u64,
    /// Desktop notifications (OSC 9 and 777), as title and body.
    toasts: std::collections::VecDeque<(String, String)>,
    /// The last text the program wrote to the clipboard and to the primary
    /// selection.
    clipboard: Option<String>,
    primary: Option<String>,
    replies: std::collections::VecDeque<Vec<u8>>,
    reply_bytes: usize,
    pub dropped_replies: usize,
}
impl Notifications {
    fn reply(&mut self, bytes: &[u8]) {
        if self.replies.len() >= 32 || self.reply_bytes + bytes.len() > 4096 {
            self.dropped_replies += 1;
            return;
        }
        self.reply_bytes += bytes.len();
        self.replies.push_back(bytes.to_vec());
    }
    fn set_title(&mut self, title: &str) {
        self.title = title
            .chars()
            .filter(|c| !c.is_control())
            .take(512)
            .collect();
        self.title_revision = self.title_revision.wrapping_add(1);
    }
}
// Where the first ESC at or after `from` is, found a word at a time. A word's
// lowest flagged byte is an ESC; a borrow can only flag bytes above one.
fn next_escape(input: &[u8], from: usize) -> Option<usize> {
    const ONES: u64 = u64::from_le_bytes([1; 8]);
    const HIGHS: u64 = u64::from_le_bytes([0x80; 8]);
    let rest = &input[from..];
    let (words, tail) = rest.as_chunks::<8>();
    for (n, word) in words.iter().enumerate() {
        let x = u64::from_le_bytes(*word) ^ (ONES * 0x1b);
        let escapes = x.wrapping_sub(ONES) & !x & HIGHS;
        if escapes != 0 {
            return Some(from + n * 8 + escapes.trailing_zeros() as usize / 8);
        }
    }
    let done = words.len() * 8;
    tail.iter()
        .position(|&b| b == 0x1b)
        .map(|n| from + done + n)
}

const OSC_LIMIT: usize = 4096;
/// The longest image a program may send in one string: a sixel (DCS), a
/// kitty graphics command (APC) or an iTerm2 file (OSC 1337).
const IMAGE_STRING_LIMIT: usize = 16 * 1024 * 1024;
/// The longest clipboard write (OSC 52) the filter holds: base64 of about
/// 768 KiB of text.
const CLIPBOARD_LIMIT: usize = 1024 * 1024;
impl Filter {
    /// The longest the string being held may grow.
    fn string_limit(&self) -> usize {
        let held = &self.string;
        if held.starts_with(b"\x1b]52;") {
            CLIPBOARD_LIMIT
        } else if held.starts_with(b"\x1bP")
            || held.starts_with(b"\x1b_")
            || held.starts_with(b"\x1b]1337;File=")
        {
            IMAGE_STRING_LIMIT
        } else {
            OSC_LIMIT
        }
    }

    /// Forgets the string held, returning the room an image took.
    fn end_string(&mut self) {
        self.string.clear();
        if self.string.capacity() > OSC_LIMIT {
            self.string = Vec::new();
        }
    }

    /// Whether `pass` hands `input` on as it is: from the ground state, each
    /// ESC in it comes before a byte that begins no string and no new escape.
    fn passes_whole(&self, input: &[u8]) -> bool {
        const ONES: u64 = u64::from_le_bytes([1; 8]);
        const HIGHS: u64 = u64::from_le_bytes([0x80; 8]);
        if self.state != FilterState::Ground {
            return false;
        }
        // An ESC holds the input back when a string, another ESC or the end
        // of the input follows it.
        let holds = |at: usize| {
            input[at] == 0x1b
                && matches!(
                    input.get(at + 1),
                    Some(b']' | b'P' | b'_' | b'X' | b'^' | 0x1b) | None
                )
        };
        // Every ESC of a word is checked in one pass over the input. A borrow
        // can flag bytes above an ESC that are not one, which `holds` skips.
        let (words, _) = input.as_chunks::<8>();
        for (n, word) in words.iter().enumerate() {
            let x = u64::from_le_bytes(*word) ^ (ONES * 0x1b);
            let mut escapes = x.wrapping_sub(ONES) & !x & HIGHS;
            while escapes != 0 {
                if holds(n * 8 + escapes.trailing_zeros() as usize / 8) {
                    return false;
                }
                escapes &= escapes - 1;
            }
        }
        !(words.len() * 8..input.len()).any(holds)
    }

    /// The bytes of `input` the parser may see, in order: OSC, DCS and APC
    /// strings only when complete and bounded, and no SOS or PM payloads.
    fn pass(&mut self, input: &[u8], out: &mut Vec<u8>) {
        use FilterState::*;
        let mut at = 0;
        while at < input.len() {
            // Text between sequences passes as it is, a stretch at a time.
            if self.state == Ground {
                let end = next_escape(input, at).unwrap_or(input.len());
                out.extend_from_slice(&input[at..end]);
                at = end;
                if at == input.len() {
                    break;
                }
            }
            let byte = input[at];
            at += 1;
            self.state = match self.state {
                Ground if byte == 0x1b => Escape,
                Ground => {
                    out.push(byte);
                    Ground
                }
                Escape => match byte {
                    0x1b => Escape,
                    b']' | b'P' | b'_' => {
                        self.string.clear();
                        self.string.extend_from_slice(&[0x1b, byte]);
                        self.osc = byte == b']';
                        self.overflow = false;
                        Text
                    }
                    b'X' | b'^' => {
                        self.dropped += 1;
                        Ignored
                    }
                    _ => {
                        out.extend_from_slice(&[0x1b, byte]);
                        Ground
                    }
                },
                Text | TextEscape if byte == 0x18 || byte == 0x1a => {
                    self.end_string();
                    Ground
                }
                Text | TextEscape => {
                    // BEL ends only an OSC; every string ends at ST.
                    let done =
                        (self.osc && byte == 7) || (self.state == TextEscape && byte == b'\\');
                    if !self.overflow {
                        if self.string.len() < self.string_limit() {
                            self.string.push(byte);
                        } else {
                            self.string.clear();
                            self.overflow = true;
                            self.dropped += 1;
                        }
                    }
                    if done {
                        if !self.overflow {
                            out.extend_from_slice(&self.string);
                        }
                        self.end_string();
                        Ground
                    } else if byte == 0x1b {
                        TextEscape
                    } else {
                        Text
                    }
                }
                Ignored | IgnoredEscape
                    if byte == 0x18
                        || byte == 0x1a
                        || (self.state == IgnoredEscape && byte == b'\\') =>
                {
                    Ground
                }
                Ignored | IgnoredEscape if byte == 0x1b => IgnoredEscape,
                Ignored | IgnoredEscape => Ignored,
            };
        }
    }
}

/// What a terminal is charged, and the part of it its history may hold.
struct Charge {
    bytes: usize,
    history: usize,
}

fn charge(rows: u16, cols: u16, history: usize, byte_limit: usize) -> Result<Charge, String> {
    if rows == 0 || cols == 0 || rows > 256 || cols > 512 || history > 100_000 {
        return Err("terminal dimensions/history exceed limits".into());
    }
    // The main and alternate screens and the copy a resize makes while it
    // reflows, at full width as cells, plus their line headers. Storage keeps
    // its high-water dimensions, so a shrink returns nothing.
    let cell = std::mem::size_of::<wezterm_term::Cell>();
    let line = std::mem::size_of::<Line>();
    let rows = usize::from(rows);
    let cols = usize::from(cols);
    let screens = (rows * 3)
        .checked_mul(cols)
        .and_then(|n| n.checked_mul(cell))
        .and_then(|n| n.checked_add(rows * 2 * line))
        .and_then(|n| n.checked_add(64 * 1024))
        .ok_or("terminal storage overflow")?;
    if screens > byte_limit {
        return Err("terminal screen exceeds its byte budget".into());
    }
    // The history holds what its rows take at full width as cells, or what
    // the limit leaves, whichever is less: past that the oldest rows go
    // first, so a long history is bounded by memory rather than refused.
    let full = history
        .checked_mul(cols * cell + line)
        .ok_or("terminal storage overflow")?;
    let history = full.min(byte_limit - screens);
    Ok(Charge {
        bytes: screens + history,
        history,
    })
}

/// One row of a terminal's buffer as text.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Row {
    pub text: String,
    /// The row runs on into the next: the program's line continues there.
    pub wrapped: bool,
}

/// One column of a line: the cell that starts there, or the second column of
/// a wide cell to its left.
#[derive(Clone, Copy)]
pub(crate) enum Slot<'a> {
    Cell(&'a str, &'a CellAttributes, bool),
    Continuation,
    Blank,
}

/// The columns of `line` across a `cols`-wide screen; a line keeps no cells
/// past its last written one, and those columns are blank.
pub(crate) fn slots(line: &Line, cols: u16) -> Vec<Slot<'_>> {
    let cols = usize::from(cols);
    let mut out = vec![Slot::Blank; cols];
    for cell in line.visible_cells() {
        let at = cell.cell_index();
        if at >= cols {
            break;
        }
        let width = cell.width().max(1);
        out[at] = Slot::Cell(cell.str(), cell.attrs(), width > 1);
        for spare in out.iter_mut().take(cols).skip(at + 1).take(width - 1) {
            *spare = Slot::Continuation;
        }
    }
    out
}

/// One cell's text as shown: its character and any combining marks on it,
/// bounded so a run of marks cannot grow a frame without limit; blank for a
/// space, a tab's cell, or hidden text.
pub(crate) fn cell_text<'a>(text: &'a str, attrs: &CellAttributes) -> &'a str {
    if attrs.invisible() || text == " " || text == "\t" {
        return "";
    }
    match text.char_indices().nth(17) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

/// A visible row as text: a wide character is one character over two cells,
/// and blank cells are spaces.
fn row_text(line: &Line, cols: u16) -> String {
    let mut text = String::new();
    for slot in slots(line, cols) {
        match slot {
            Slot::Cell(cell, attrs, _) => {
                let shown = cell_text(cell, attrs);
                if shown.is_empty() {
                    text.push(' ');
                } else {
                    text.push_str(shown);
                }
            }
            Slot::Continuation => {}
            Slot::Blank => text.push(' '),
        }
    }
    text
}

/// A color as the frame reports it.
pub(crate) fn color(value: ColorAttribute) -> super::frame::Color {
    use super::frame::Color;
    match value {
        ColorAttribute::TrueColorWithPaletteFallback(rgb, _)
        | ColorAttribute::TrueColorWithDefaultFallback(rgb) => {
            let (r, g, b, _) = rgb.to_srgb_u8();
            Color::Rgb(r, g, b)
        }
        ColorAttribute::PaletteIndex(i) => Color::Indexed(i),
        ColorAttribute::Default => Color::Default,
    }
}

/// Whether a cell's attributes ask for bold, dim, italic, underline, inverse.
pub(crate) fn styles(attrs: &CellAttributes) -> (bool, bool, bool, bool, bool) {
    (
        attrs.intensity() == Intensity::Bold,
        attrs.intensity() == Intensity::Half,
        attrs.italic(),
        attrs.underline() != Underline::None,
        attrs.reverse(),
    )
}

impl Engine {
    /// A `rows` x `cols` screen keeping `history` scrollback rows, refused
    /// when its storage would exceed `byte_limit` or the budget.
    pub fn new(
        rows: u16,
        cols: u16,
        history: usize,
        byte_limit: usize,
        budget: Budget,
    ) -> Result<Self, String> {
        let Charge {
            bytes,
            history: history_bytes,
        } = charge(rows, cols, history, byte_limit)?;
        let lease = budget.reserve(bytes)?;
        let notes = Arc::new(Mutex::new(Notifications::default()));
        let config = Arc::new(Config {
            history,
            history_bytes: AtomicUsize::new(history_bytes),
            palette: Mutex::new(ColorPalette::default()),
            image_limit: AtomicUsize::new(IMAGE_LIMIT),
        });
        let images = budget.reserve(0)?;
        let mut term = Terminal::new(
            terminal_size(rows, cols, (0, 0)),
            config.clone(),
            "xterm-256color",
            env!("CARGO_PKG_VERSION"),
            Box::new(Replies(notes.clone())),
        );
        term.set_notification_handler(Box::new(Alerts(notes.clone())));
        let clipboard: Arc<dyn Clipboard> = Arc::new(Clip(notes.clone()));
        term.set_clipboard(&clipboard);
        Ok(Self {
            charged: bytes,
            term,
            config,
            notes,
            view_top: None,
            filter: Filter::default(),
            lease,
            budget,
            max_rows: rows,
            max_cols: cols,
            history,
            byte_limit,
            images,
            cell: (0, 0),
        })
    }
    /// Stores the history rows well above the screen compressed; cheap when
    /// none are new.
    pub fn compact_history(&mut self) {
        self.term.compact_history();
    }

    /// Does one block of `compact_history`'s work; false once none is left.
    pub fn compact_history_step(&mut self) -> bool {
        self.term.compact_history_step()
    }

    /// Stores the history rows a read inflated compressed again.
    pub fn recompress_read_history(&mut self) {
        self.term.recompress_read_history();
    }

    /// The bytes the history's rows take, as its byte bound counts them.
    pub fn history_bytes(&self) -> usize {
        self.term.screen().history_bytes()
    }

    /// Parses program output into the screen.
    pub fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > 1024 * 1024 {
            return Err("terminal feed exceeds 1 MiB".into());
        }
        let filtered;
        let passed = if self.filter.passes_whole(bytes) {
            bytes
        } else {
            let mut out = Vec::with_capacity(bytes.len());
            self.filter.pass(bytes, &mut out);
            filtered = out;
            &filtered
        };
        let alternate = self.term.is_alt_screen_active();
        self.term.advance_bytes(passed);
        self.charge_images();
        // The other screen has rows of its own, so a view into the one left
        // behind no longer applies.
        if self.term.is_alt_screen_active() != alternate {
            self.view_top = None;
        }
        Ok(())
    }
    /// Charges the session for the images the screen holds. Past what the
    /// session can take, the screen's image limit falls to its charge, so its
    /// next image takes the place of older ones.
    fn charge_images(&mut self) {
        let held = self.term.image_memory();
        if held == self.images.bytes() {
            return;
        }
        let limit = if self.images.resize(held).is_ok() {
            IMAGE_LIMIT
        } else {
            self.images.bytes()
        };
        self.config.image_limit.store(limit, Ordering::Relaxed);
    }
    /// The bytes the screen's images take.
    pub fn image_bytes(&self) -> usize {
        self.images.bytes()
    }
    /// Resizes the screen, rewrapping its lines and history to the new width;
    /// storage keeps the largest size it has reached. `cell` is the pixel size
    /// a cell is drawn at, which places images and which programs may ask.
    pub fn resize(&mut self, rows: u16, cols: u16, cell: (u16, u16)) -> Result<(), String> {
        charge(rows, cols, self.history, self.byte_limit)?;
        let Charge { bytes, history } = charge(
            rows.max(self.max_rows),
            cols.max(self.max_cols),
            self.history,
            self.byte_limit,
        )?;
        self.lease.resize(bytes)?;
        self.charged = bytes;
        self.config.history_bytes.store(history, Ordering::Relaxed);
        let back = self.scrollback();
        self.term.resize(terminal_size(rows, cols, cell));
        self.cell = cell;
        // Rewrapping renumbers the rows, so the view keeps its distance from
        // the live screen.
        self.scroll(back);
        self.max_rows = self.max_rows.max(rows);
        self.max_cols = self.max_cols.max(cols);
        Ok(())
    }
    /// An immutable snapshot of the visible screen.
    pub fn frame(&self) -> Result<super::frame::Frame, String> {
        super::frame::Frame::capture(self, &self.budget)
    }
    /// The pixel size of a cell as the view last drew it.
    pub fn cell(&self) -> (u16, u16) {
        self.cell
    }
    /// The screen's rows and columns.
    pub fn size(&self) -> (u16, u16) {
        let screen = self.term.screen();
        (screen.physical_rows as u16, screen.physical_cols as u16)
    }
    /// How many rows back into the scrollback the view shows.
    pub fn scrollback(&self) -> usize {
        let Some(top) = self.view_top else { return 0 };
        let live = self.term.screen().visible_row_to_stable_row(0);
        (live - top).clamp(0, self.history() as isize) as usize
    }
    /// The cursor's row and column on the visible screen.
    pub fn cursor(&self) -> (u16, u16) {
        let cursor = self.term.cursor_pos();
        let row = cursor.y + self.scrollback() as i64;
        (row.max(0) as u16, cursor.x as u16)
    }
    /// Whether the cursor is hidden, by the program or by scrolling back.
    pub fn hide_cursor(&self) -> bool {
        self.term.cursor_pos().visibility == CursorVisibility::Hidden || self.scrollback() > 0
    }
    pub fn alternate_screen(&self) -> bool {
        self.term.is_alt_screen_active()
    }
    /// Whether arrow keys send application sequences.
    pub fn application_cursor(&self) -> bool {
        self.term.application_cursor_keys_enabled()
    }
    /// The flags of kitty's keyboard protocol while the program asked for it.
    pub fn kitty_keyboard(&self) -> Option<wezterm_term::input_types::KittyKeyboardFlags> {
        self.term.kitty_keyboard_flags()
    }
    /// Rows of history kept above the screen.
    pub fn history(&self) -> usize {
        let screen = self.term.screen();
        screen.scrollback_rows() - screen.physical_rows
    }
    /// Shows the screen `offset` rows back into the scrollback.
    pub fn scroll(&mut self, offset: usize) {
        let target = offset.min(self.history());
        self.view_top =
            (target > 0).then(|| self.term.screen().visible_row_to_stable_row(0) - target as isize);
    }
    /// Calls `f` with the lines the view shows, top first.
    /// The stable row of the first line `with_visible_lines` passes.
    pub(crate) fn visible_stable_top(&self) -> isize {
        let screen = self.term.screen();
        let end = screen.scrollback_rows() - self.scrollback();
        screen.phys_to_stable_row_index(end - screen.physical_rows)
    }
    pub(crate) fn with_visible_lines(&self, f: impl FnMut(&[&Line])) {
        let screen = self.term.screen();
        let end = screen.scrollback_rows() - self.scrollback();
        screen.with_phys_lines(end - screen.physical_rows..end, f);
    }
    /// The visible screen as text: rows without trailing blanks, a wrapped
    /// row running on into the next.
    pub fn text(&self) -> String {
        let (rows, cols) = self.size();
        let mut out = String::new();
        self.with_visible_lines(|lines| {
            for (row, line) in lines.iter().enumerate() {
                let text = row_text(line, cols);
                if line.last_cell_was_wrapped() {
                    out.push_str(&text);
                } else {
                    out.push_str(text.trim_end());
                    if row + 1 < usize::from(rows) {
                        out.push('\n');
                    }
                }
            }
        });
        out.truncate(out.trim_end().len());
        out
    }
    /// Every row of the buffer, the oldest history row first and the
    /// screen's last row last, wherever the view is scrolled. A wide
    /// character is one character over two cells, and a row that wraps
    /// keeps its trailing blanks.
    pub fn rows(&self) -> Vec<Row> {
        let screen = self.term.screen();
        let cols = screen.physical_cols as u16;
        let mut out = Vec::with_capacity(screen.scrollback_rows());
        screen.for_each_phys_line(|_, line| {
            let mut text = row_text(line, cols);
            let wrapped = line.last_cell_was_wrapped();
            if !wrapped {
                text.truncate(text.trim_end().len());
            }
            out.push(Row { text, wrapped });
        });
        out
    }
    /// The text between two visible cell positions, end excluded, as a copy
    /// would take it: a line's trailing blanks are left out, a tab copies as
    /// a tab, and a wrapped line runs on without a line break.
    pub fn copy(&self, start: (u16, u16), end: (u16, u16)) -> String {
        if end <= start {
            return String::new();
        }
        let (_, cols) = self.size();
        let last = if end.1 == 0 {
            (end.0 - 1, cols - 1)
        } else {
            (end.0, end.1 - 1)
        };
        let mut out = String::new();
        self.with_visible_lines(|lines| {
            out = self.copy_lines(
                &lines[usize::from(start.0)..=usize::from(last.0)],
                start.1,
                last.1,
            );
        });
        out
    }
    /// The text between two positions in stable rows (see
    /// `stable_top`), end excluded, as `copy` takes it; the rows need not be
    /// on screen.
    pub fn copy_stable(&self, start: (isize, u16), end: (isize, u16)) -> String {
        if end <= start {
            return String::new();
        }
        let (_, cols) = self.size();
        let last = if end.1 == 0 {
            (end.0 - 1, cols - 1)
        } else {
            (end.0, end.1 - 1)
        };
        let screen = self.term.screen();
        let range = screen.stable_range(&(start.0..last.0 + 1));
        let mut out = String::new();
        screen.with_phys_lines(range, |lines| {
            out = self.copy_lines(lines, start.1, last.1);
        });
        out
    }
    // The text of `lines`, from column `from` of the first to column `to` of
    // the last, both included.
    fn copy_lines(&self, lines: &[&Line], from: u16, to: u16) -> String {
        let (_, cols) = self.size();
        let tabs = self.term.tab_stops_by_column();
        let mut out = String::new();
        let count = lines.len();
        for (row, line) in lines.iter().enumerate() {
            let slots = slots(line, cols);
            let from = if row == 0 { from } else { 0 };
            let to = if row + 1 == count { to } else { cols - 1 };
            let length = slots
                .iter()
                .rposition(|s| matches!(s, Slot::Cell(t, _, _) if *t != " "))
                .map_or(0, |at| at + 1)
                .min(usize::from(to) + 1);
            let mut from = usize::from(from);
            // A selection that starts on a wide character's second column
            // takes the whole character.
            if from > 0 && matches!(slots.get(from), Some(Slot::Continuation)) {
                from -= 1;
            }
            let mut tab_mode = false;
            for (col, slot) in slots.iter().enumerate().take(length).skip(from) {
                let text = match slot {
                    Slot::Cell(text, _, _) => *text,
                    Slot::Continuation => continue,
                    Slot::Blank => " ",
                };
                // A tab's blank cells up to the next tab stop are the tab.
                if tab_mode {
                    if tabs.get(col).copied().unwrap_or(false) || text != " " {
                        tab_mode = false;
                    } else {
                        continue;
                    }
                }
                if text == "\t" {
                    tab_mode = true;
                }
                out.push_str(text);
            }
            if to == cols - 1 && !line.last_cell_was_wrapped() {
                out.push('\n');
            }
        }
        out.strip_suffix('\n').map(str::to_owned).unwrap_or(out)
    }
    /// The stable row the view shows at its top: a row keeps its number as
    /// output scrolls it into the history, until the history drops it.
    pub fn stable_top(&self) -> isize {
        self.view_top
            .unwrap_or_else(|| self.term.screen().visible_row_to_stable_row(0))
    }
    /// The stable row of the live screen's top.
    pub fn stable_live(&self) -> isize {
        self.term.screen().visible_row_to_stable_row(0)
    }
    /// The stable row of the oldest history row kept.
    pub fn stable_first(&self) -> isize {
        self.stable_live() - self.history() as isize
    }
    /// The text of stable row `row`, empty where the buffer keeps none.
    pub fn stable_row_text(&self, row: isize) -> String {
        let (_, cols) = self.size();
        let screen = self.term.screen();
        let range = screen.stable_range(&(row..row + 1));
        let mut out = String::new();
        screen.with_phys_lines(range, |lines| {
            if let Some(line) = lines.first() {
                out = row_text(line, cols);
            }
        });
        out
    }
    /// `text` as program input, without escape and NUL characters, and what
    /// ends it: bracketed paste's closing mark when the program asked for it.
    pub fn paste(&self, text: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
        if text.len() > PASTE_LIMIT {
            return Err("terminal paste exceeds 16 MiB".into());
        }
        let clean = text.replace(['\x1b', '\0'], "");
        Ok(if self.term.bracketed_paste_enabled() {
            (
                format!("\x1b[200~{clean}").into_bytes(),
                b"\x1b[201~".to_vec(),
            )
        } else {
            (clean.into_bytes(), Vec::new())
        })
    }
    pub fn charged_bytes(&self) -> usize {
        self.charged
    }
    /// The title and bell count reported so far.
    pub fn notifications(&self) -> Notifications {
        let notes = self.notes.lock();
        Notifications {
            title: notes.title.clone(),
            title_revision: notes.title_revision,
            bells: notes.bells,
            dropped_replies: notes.dropped_replies,
            ..Notifications::default()
        }
    }
    /// Device replies the program requested, to write back to it.
    pub fn take_replies(&mut self) -> Vec<u8> {
        let mut notes = self.notes.lock();
        let mut out = Vec::with_capacity(notes.reply_bytes);
        for reply in notes.replies.drain(..) {
            out.extend(reply);
        }
        notes.reply_bytes = 0;
        out
    }
    /// Control strings dropped for being unsupported or oversized.
    pub fn dropped_controls(&self) -> usize {
        self.filter.dropped
    }
    /// Makes `palette` the one the screen shows and a program's color queries
    /// answer from; colors the program set itself stay over it.
    pub fn set_palette(&mut self, palette: &ColorPalette) {
        let mut held = self.config.palette.lock();
        if *held != *palette {
            *held = palette.clone();
        }
    }
    /// The colors the screen shows: the view's palette with any the program
    /// set (OSC 4, 10, 11, 12) over it.
    pub fn palette(&self) -> ColorPalette {
        self.term.palette()
    }
    /// Whether the program asked for the mouse.
    pub fn mouse_grabbed(&self) -> bool {
        self.term.is_mouse_grabbed()
    }
    /// Reports a pointer event to the program as it asked (or as cursor keys
    /// for the wheel on the alternate screen); the bytes wait in the replies.
    pub fn mouse(&mut self, event: MouseEvent) {
        // An event the program asked for nothing about is not reported.
        let _ = self.term.mouse_event(event);
    }
    /// Tells the program the view gained or lost focus, when it asked
    /// (DECSET 1004).
    pub fn focus(&mut self, focused: bool) {
        self.term.focus_changed(focused);
    }
    /// Whether the program is drawing a synchronized frame (DECSET 2026).
    pub fn synchronized(&self) -> bool {
        self.term.synchronized_output()
    }
    /// Ends a synchronized frame the program left open.
    pub fn end_synchronized(&mut self) {
        self.term.end_synchronized_output();
    }
    /// The directory the program last reported (OSC 7), as a path.
    pub fn current_dir(&self) -> Option<String> {
        self.term.get_current_dir().and_then(directory_of)
    }
    /// The shell's prompts, inputs and outputs that OSC 133 marked, oldest
    /// first, in buffer rows counted from the oldest history row.
    pub fn zones(&mut self) -> Vec<Zone> {
        let first = self.term.screen().phys_to_stable_row_index(0);
        let row = |stable: StableRowIndex| (stable - first).max(0) as i64;
        self.term
            .get_semantic_zones()
            .unwrap_or_default()
            .into_iter()
            .map(|z| Zone {
                kind: match z.semantic_type {
                    SemanticType::Prompt => ZoneKind::Prompt,
                    SemanticType::Input => ZoneKind::Input,
                    SemanticType::Output => ZoneKind::Output,
                },
                start: super::Cell {
                    row: row(z.start_y),
                    col: z.start_x as i64,
                },
                end: super::Cell {
                    row: row(z.end_y),
                    col: z.end_x as i64,
                },
            })
            .collect()
    }
    /// The desktop notifications the program sent since the last call.
    pub fn take_toasts(&mut self) -> Vec<(String, String)> {
        self.notes.lock().toasts.drain(..).collect()
    }
    /// The text the program last wrote to the clipboard and to the primary
    /// selection since the last call.
    pub fn take_clipboard(&mut self) -> (Option<String>, Option<String>) {
        let mut notes = self.notes.lock();
        (notes.clipboard.take(), notes.primary.take())
    }
}

/// What a stretch of the buffer is to the shell that marked it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ZoneKind {
    Prompt,
    Input,
    Output,
}

/// A stretch of the buffer the shell marked (OSC 133), its end excluded.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Zone {
    pub kind: ZoneKind,
    pub start: super::Cell,
    pub end: super::Cell,
}

/// The path an OSC 7 location names: a `file://host/path` URL, its escapes
/// decoded, or a bare absolute path.
fn directory_of(location: &str) -> Option<String> {
    let path = match location.strip_prefix("file://") {
        Some(rest) => &rest[rest.find('/')?..],
        None if location.starts_with('/') => location,
        None => return None,
    };
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        if bytes[at] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(at + 1).and_then(|b| hex(*b)),
                bytes.get(at + 2).and_then(|b| hex(*b)),
            )
        {
            out.push((hi * 16 + lo) as u8);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The size the terminal core takes, in cells and in the pixels of a `cell`
/// the view draws.
fn terminal_size(rows: u16, cols: u16, cell: (u16, u16)) -> TerminalSize {
    TerminalSize {
        rows: usize::from(rows),
        cols: usize::from(cols),
        pixel_width: usize::from(cols) * usize::from(cell.0),
        pixel_height: usize::from(rows) * usize::from(cell.1),
        dpi: 0,
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn input_the_filter_would_leave_alone_is_what_it_passes() {
        let pieces: [&[u8]; 12] = [
            b"text ",
            b"\x1b[31m",
            b"\x1b7",
            b"\x1b]0;title\x07",
            b"\x1bPq#0\x1b\\",
            b"\x1b_Gx\x1b\\",
            b"\x1bXsos\x1b\\",
            b"\x1b^pm\x1b\\",
            b"\x1b\x1b[m",
            b"\x1b",
            "é".as_bytes(),
            b"abcdefghij",
        ];
        for a in pieces {
            for b in pieces {
                for c in pieces {
                    let input = [a, b, c].concat();
                    let filter = super::Filter::default();
                    let mut out = Vec::new();
                    super::Filter::default().pass(&input, &mut out);
                    if filter.passes_whole(&input) {
                        assert_eq!(out, input, "{input:?}");
                    }
                    for from in 0..input.len() {
                        assert_eq!(
                            super::next_escape(&input, from),
                            input[from..]
                                .iter()
                                .position(|&b| b == 0x1b)
                                .map(|n| from + n),
                            "{input:?} from {from}"
                        );
                    }
                }
            }
        }
    }

    use super::*;
    #[test]
    fn the_default_limit_holds_the_largest_screen_with_the_default_history() {
        let defaults = super::super::Options::default();
        let budget = Budget::new(256 * 1024 * 1024);
        let mut e = Engine::new(
            24,
            80,
            defaults.scrollback as usize,
            defaults.byte_limit as usize,
            budget,
        )
        .unwrap();
        // A maximized window on a 4K display at 1.5x is about 86 x 320 cells.
        e.resize(86, 320, (0, 0)).unwrap();
        e.resize(256, 512, (0, 0)).unwrap();
        assert_eq!(e.size(), (256, 512));
    }
    #[test]
    fn tabs_show_as_blank_cells_and_copy_as_tabs() {
        let budget = Budget::new(64 * 1024 * 1024);
        let mut e = Engine::new(4, 40, 100, 16 * 1024 * 1024, budget).unwrap();
        e.process(b"ab\tcd\r\nlonger\tx").unwrap();
        assert_eq!(e.text(), "ab      cd\nlonger  x");
        let frame = e.frame().unwrap();
        assert!(frame.cells()[0][2..8].iter().all(|c| c.text.is_empty()));
        assert_eq!(frame.cells()[0][8].text.as_str(), "c");
        assert_eq!(e.copy((0, 0), (0, 10)), "ab\tcd");
    }
    #[test]
    fn lines_rewrap_when_the_screen_narrows_and_rejoin_when_it_widens() {
        let budget = Budget::new(64 * 1024 * 1024);
        let mut e = Engine::new(4, 40, 100, 16 * 1024 * 1024, budget).unwrap();
        let long = format!("start-{}-end", "x".repeat(50));
        e.process(format!("{long}\r\nnext").as_bytes()).unwrap();
        e.resize(4, 20, (0, 0)).unwrap();
        // The line rewraps into three rows of twenty; rows above the cursor's
        // move into the scrollback to keep it in place, as in VTE.
        let narrow = e.text();
        assert!(narrow.ends_with("-end\nnext"), "{narrow:?}");
        e.scroll(usize::MAX);
        let earlier = e.text();
        assert!(earlier.starts_with("start-"), "{earlier:?}");
        e.scroll(0);
        e.resize(4, 80, (0, 0)).unwrap();
        let wide = e.text();
        assert_eq!(wide.lines().next(), Some(long.as_str()), "{wide:?}");
    }
    #[test]
    fn streams_unicode_styles_cursor_alternate_screen_and_paste() {
        let budget = Budget::new(64 * 1024 * 1024);
        let mut e = Engine::new(8, 40, 100, 32 * 1024 * 1024, budget.clone()).unwrap();
        for byte in "hello e\u{301} 界\r\n\x1b[31;1mred\x1b[0m".bytes() {
            e.process(&[byte]).unwrap();
        }
        assert!(e.text().contains("hello e\u{301} 界"));
        let frame = e.frame().unwrap();
        let red = &frame.cells()[1][0];
        assert_eq!(red.fg, super::super::frame::Color::Indexed(1));
        assert!(red.bold);
        assert_eq!(e.cursor(), (1, 3));
        let original = e.text();
        e.process(b"\x1b[?1049h\x1b[2Jtemporary").unwrap();
        assert!(e.alternate_screen());
        e.process(b"\x1b[?1049l").unwrap();
        assert_eq!(e.text(), original);
        e.process(b"\x1b[?2004h").unwrap();
        assert_eq!(
            e.paste("one\ntwo").unwrap(),
            (b"\x1b[200~one\ntwo".to_vec(), b"\x1b[201~".to_vec())
        );
        drop(frame);
        drop(e);
        assert_eq!(budget.usage().0, 0);
    }
    #[test]
    fn osc_dcs_combining_and_scrollback_pressure_stay_bounded() {
        let budget = Budget::new(64 * 1024 * 1024);
        let mut e = Engine::new(4, 20, 12, 8 * 1024 * 1024, budget.clone()).unwrap();
        let bytes = e.charged_bytes();
        e.process(b"\x1b]52;c;").unwrap();
        for _ in 0..2000 {
            e.process(&[b'A'; 8192]).unwrap();
        }
        assert!(e.filter.string.capacity() <= CLIPBOARD_LIMIT * 2);
        e.process(b"\x07safe").unwrap();
        assert!(e.text().contains("safe"));
        e.process(b"\x1bP").unwrap();
        for _ in 0..IMAGE_STRING_LIMIT / 8192 + 1 {
            e.process(&[b'B'; 8192]).unwrap();
        }
        assert!(e.filter.string.capacity() <= IMAGE_STRING_LIMIT * 2);
        e.process(b"\x1b\\").unwrap();
        assert!(e.filter.string.capacity() <= OSC_LIMIT);
        e.process(format!("\r\nx{}", "\u{301}".repeat(100_000)).as_bytes())
            .unwrap();
        let frame = e.frame().unwrap();
        assert!(frame.cells().iter().flatten().all(|c| c.text.len() <= 40));
        drop(frame);
        for n in 0..1000 {
            e.process(format!("\r\nrow-{n}").as_bytes()).unwrap();
        }
        e.scroll(usize::MAX);
        assert_eq!(e.scrollback(), 12);
        assert_eq!(e.charged_bytes(), bytes);
        assert_eq!(budget.usage().0, bytes);
        assert!(e.dropped_controls() >= 2);
        drop(e);
        assert_eq!(budget.usage().0, 0);
    }
    #[test]
    fn device_replies_titles_and_request_floods_are_bounded() {
        let budget = Budget::new(8 * 1024 * 1024);
        let mut e = Engine::new(8, 20, 10, 4 * 1024 * 1024, budget).unwrap();
        e.process(b"\x1b]2;title\x07\x07\x1b[3;4H\x1b[6n\x1b[5n\x1b[18t")
            .unwrap();
        assert_eq!(e.notifications().title, "title");
        assert_eq!(e.notifications().bells, 1);
        assert_eq!(e.take_replies(), b"\x1b[3;4R\x1b[0n\x1b[8;8;20t");
        for _ in 0..10000 {
            e.process(b"\x1b[6n").unwrap();
        }
        assert_eq!(e.notes.lock().replies.len(), 32);
        assert!(e.notifications().dropped_replies > 0);
        assert!(e.take_replies().len() <= 4096);
    }
    #[test]
    fn synchronized_output_is_reported_and_followed() {
        let mut e = Engine::new(4, 20, 10, 4 * 1024 * 1024, Budget::new(8 * 1024 * 1024)).unwrap();
        e.process(b"\x1b[?2026$p").unwrap();
        assert_eq!(e.take_replies(), b"\x1b[?2026;2$y");
        e.process(b"\x1b[?2026h\x1b[?2026$p").unwrap();
        assert!(e.synchronized());
        assert_eq!(e.take_replies(), b"\x1b[?2026;1$y");
        e.end_synchronized();
        assert!(!e.synchronized());
    }

    #[test]
    fn programs_write_the_clipboard_and_send_notifications_but_never_read_the_clipboard() {
        let mut e = Engine::new(4, 20, 10, 4 * 1024 * 1024, Budget::new(8 * 1024 * 1024)).unwrap();
        // "copied" in base64, to the clipboard and then to the primary selection.
        e.process(b"\x1b]52;c;Y29waWVk\x07\x1b]52;p;cHJpbWFyeQ==\x07")
            .unwrap();
        assert_eq!(
            e.take_clipboard(),
            (Some("copied".into()), Some("primary".into()))
        );
        assert_eq!(e.take_clipboard(), (None, None));
        e.process(b"\x1b]52;c;?\x07").unwrap();
        assert!(e.take_replies().is_empty());
        e.process(b"\x1b]9;build done\x07\x1b]777;notify;Title;Body\x07")
            .unwrap();
        assert_eq!(
            e.take_toasts(),
            [
                (String::new(), "build done".to_string()),
                ("Title".to_string(), "Body".to_string())
            ]
        );
        for n in 0..20 {
            e.process(format!("\x1b]9;{n}\x07").as_bytes()).unwrap();
        }
        assert_eq!(e.take_toasts().len(), TOAST_LIMIT);
    }

    #[test]
    fn a_reported_directory_reads_as_a_path_and_marked_prompts_as_zones() {
        let mut e = Engine::new(6, 30, 20, 4 * 1024 * 1024, Budget::new(8 * 1024 * 1024)).unwrap();
        assert_eq!(e.current_dir(), None);
        e.process(b"\x1b]7;file://host/home/me/my%20dir\x07")
            .unwrap();
        assert_eq!(e.current_dir().as_deref(), Some("/home/me/my dir"));
        assert_eq!(directory_of("/plain/path").as_deref(), Some("/plain/path"));
        assert_eq!(directory_of("relative"), None);
        for n in 0..3 {
            e.process(
                format!("\x1b]133;A\x07$ \x1b]133;B\x07cmd{n}\r\n\x1b]133;C\x07out{n}\r\n")
                    .as_bytes(),
            )
            .unwrap();
        }
        let prompts: Vec<i64> = e
            .zones()
            .iter()
            .filter(|z| z.kind == ZoneKind::Prompt)
            .map(|z| z.start.row)
            .collect();
        assert_eq!(prompts, [0, 2, 4]);
        let rows = e.rows();
        assert!(rows[2].text.starts_with("$ cmd1"), "{rows:?}");
    }

    #[test]
    fn resize_admission_is_atomic_and_tracks_retained_high_water_capacity() {
        let budget = Budget::new(64 * 1024 * 1024);
        // The largest screens, three copies at full width, exceed this limit
        // on their own; the history only takes what the screens leave.
        let mut e = Engine::new(24, 80, 1000, 8 * 1024 * 1024, budget).unwrap();
        e.process(b"preserved").unwrap();
        let old = e.charged_bytes();
        assert!(e.resize(256, 512, (0, 0)).is_err());
        assert_eq!(e.size(), (24, 80));
        assert_eq!(e.charged_bytes(), old);
        e.resize(32, 100, (0, 0)).unwrap();
        let high = e.charged_bytes();
        e.resize(8, 20, (0, 0)).unwrap();
        assert_eq!(e.charged_bytes(), high);
        assert!(e.text().contains("preserved"));
    }
}
