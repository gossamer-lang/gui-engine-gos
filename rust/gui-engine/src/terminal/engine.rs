//! VT parsing and screen state: an Alacritty terminal grid that reflows its
//! lines when it resizes, behind filtered control strings, byte-admitted
//! storage and bounded device replies.

use std::sync::Arc;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell as TermCell, Flags};
use alacritty_terminal::term::{Config, Osc52, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use parking_lot::Mutex;

use crate::memory::{Budget, Lease};

/// One terminal's parser, screen and scrollback, charged to a session budget.
pub struct Engine {
    term: Term<Listener>,
    processor: Processor,
    notes: Arc<Mutex<Notifications>>,
    /// The size the listener reports when a program asks for it.
    size: Arc<Mutex<(u16, u16)>>,
    filter: Filter,
    lease: Lease,
    budget: Budget,
    max_rows: u16,
    max_cols: u16,
    history: usize,
    byte_limit: usize,
    charged: usize,
}

/// A grid size in the form the terminal resizes to.
struct GridSize {
    rows: u16,
    cols: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        usize::from(self.rows)
    }
    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }
    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

/// Receives what the terminal reports while it parses: titles, bells and
/// the replies a program asked for.
#[derive(Clone)]
pub(crate) struct Listener {
    notes: Arc<Mutex<Notifications>>,
    size: Arc<Mutex<(u16, u16)>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let mut notes = self.notes.lock();
        match event {
            TermEvent::Title(title) => notes.set_title(&title),
            TermEvent::ResetTitle => notes.set_title(""),
            TermEvent::Bell => notes.bells = notes.bells.saturating_add(1),
            TermEvent::PtyWrite(text) => notes.reply(text),
            TermEvent::TextAreaSizeRequest(format) => {
                let (rows, cols) = *self.size.lock();
                notes.reply(format(WindowSize {
                    num_lines: rows,
                    num_cols: cols,
                    cell_width: 0,
                    cell_height: 0,
                }));
            }
            // Clipboard access is off, colors are the view's, and the session
            // already redraws on every change.
            _ => {}
        }
    }
}

#[derive(Default)]
struct Filter {
    state: FilterState,
    string: Vec<u8>,
    overflow: bool,
    dropped: usize,
}

/// Where the filter is in an escape sequence it intercepts.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum FilterState {
    #[default]
    Ground,
    Escape,
    Osc,
    OscEscape,
    Ignored,
    IgnoredEscape,
}

/// The title, bell count and pending replies a terminal has reported.
#[derive(Default, Clone)]
pub struct Notifications {
    pub title: String,
    /// Increments whenever the program sets a title.
    pub title_revision: u64,
    pub bells: u64,
    replies: std::collections::VecDeque<Vec<u8>>,
    reply_bytes: usize,
    pub dropped_replies: usize,
}
impl Notifications {
    fn reply(&mut self, text: String) {
        if self.replies.len() >= 32 || self.reply_bytes + text.len() > 4096 {
            self.dropped_replies += 1;
            return;
        }
        self.reply_bytes += text.len();
        self.replies.push_back(text.into_bytes());
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
const OSC_LIMIT: usize = 4096;
impl Filter {
    /// The bytes of `input` the parser may see, in order: OSC strings only
    /// when complete and bounded, and no DCS, APC, SOS or PM payloads.
    fn pass(&mut self, input: &[u8], out: &mut Vec<u8>) {
        use FilterState::*;
        for &byte in input {
            self.state = match self.state {
                Ground if byte == 0x1b => Escape,
                Ground => {
                    out.push(byte);
                    Ground
                }
                Escape => match byte {
                    0x1b => Escape,
                    b']' => {
                        self.string.clear();
                        self.string.extend_from_slice(b"\x1b]");
                        self.overflow = false;
                        Osc
                    }
                    b'P' | b'X' | b'^' | b'_' => {
                        self.dropped += 1;
                        Ignored
                    }
                    _ => {
                        out.extend_from_slice(&[0x1b, byte]);
                        Ground
                    }
                },
                Osc | OscEscape if byte == 0x18 || byte == 0x1a => {
                    self.string.clear();
                    Ground
                }
                Osc | OscEscape => {
                    let done = byte == 7 || (self.state == OscEscape && byte == b'\\');
                    if !self.overflow {
                        if self.string.len() < OSC_LIMIT {
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
                        self.string.clear();
                        Ground
                    } else if byte == 0x1b {
                        OscEscape
                    } else {
                        Osc
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

fn charge(rows: u16, cols: u16, history: usize) -> Result<usize, String> {
    if rows == 0 || cols == 0 || rows > 256 || cols > 512 || history > 100_000 {
        return Err("terminal dimensions/history exceed limits".into());
    }
    // Charges what the terminal can hold: the main and alternate grids, the
    // copy a resize makes while it reflows, and every history row at full
    // width, plus the history ring's row headers at twice their count for its
    // growth. Storage keeps its high-water dimensions, so a shrink returns
    // nothing.
    let cell = std::mem::size_of::<TermCell>();
    let row_header = std::mem::size_of::<Vec<TermCell>>() + 8;
    let cells = (usize::from(rows) * 3 + history)
        .checked_mul(usize::from(cols))
        .and_then(|n| n.checked_mul(cell));
    cells
        .and_then(|n| n.checked_add(history * 2 * row_header))
        .and_then(|n| n.checked_add(64 * 1024))
        .ok_or_else(|| "terminal storage overflow".into())
}

/// One cell's text: its character and any combining marks on it, bounded so
/// a flood of marks cannot grow it without limit.
pub(crate) fn cell_text(cell: &TermCell) -> String {
    let mut text = String::with_capacity(4);
    if cell
        .flags
        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER | Flags::HIDDEN)
    {
        return text;
    }
    // A tab's first cell keeps '\t' for copying; on screen it is blank like
    // the rest of the tab's cells.
    if cell.c != ' ' && cell.c != '\t' {
        text.push(cell.c);
    }
    for mark in cell.zerowidth().unwrap_or_default().iter().take(16) {
        text.push(*mark);
    }
    text
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
        let bytes = charge(rows, cols, history)?;
        if bytes > byte_limit {
            return Err("terminal grid/history exceeds its byte budget".into());
        }
        let lease = budget.reserve(bytes)?;
        let notes = Arc::new(Mutex::new(Notifications::default()));
        let size = Arc::new(Mutex::new((rows, cols)));
        let listener = Listener {
            notes: notes.clone(),
            size: size.clone(),
        };
        let config = Config {
            scrolling_history: history,
            osc52: Osc52::Disabled,
            ..Config::default()
        };
        Ok(Self {
            charged: bytes,
            term: Term::new(config, &GridSize { rows, cols }, listener),
            processor: Processor::new(),
            notes,
            size,
            filter: Filter::default(),
            lease,
            budget,
            max_rows: rows,
            max_cols: cols,
            history,
            byte_limit,
        })
    }
    /// Parses program output into the screen.
    pub fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > 1024 * 1024 {
            return Err("terminal feed exceeds 1 MiB".into());
        }
        let mut passed = Vec::with_capacity(bytes.len());
        self.filter.pass(bytes, &mut passed);
        self.processor.advance(&mut self.term, &passed);
        Ok(())
    }
    /// Resizes the screen, rewrapping its lines and history to the new width;
    /// storage keeps the largest size it has reached.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        charge(rows, cols, self.history)?;
        let bytes = charge(
            rows.max(self.max_rows),
            cols.max(self.max_cols),
            self.history,
        )?;
        if bytes > self.byte_limit {
            return Err("terminal resize exceeds its byte budget".into());
        }
        self.lease.resize(bytes)?;
        self.charged = bytes;
        self.term.resize(GridSize { rows, cols });
        *self.size.lock() = (rows, cols);
        self.max_rows = self.max_rows.max(rows);
        self.max_cols = self.max_cols.max(cols);
        Ok(())
    }
    /// An immutable snapshot of the visible screen.
    pub fn frame(&self) -> Result<super::frame::Frame, String> {
        super::frame::Frame::capture(&self.term, &self.budget)
    }
    /// The screen's rows and columns.
    pub fn size(&self) -> (u16, u16) {
        (self.term.screen_lines() as u16, self.term.columns() as u16)
    }
    /// How many rows back into the scrollback the view shows.
    pub fn scrollback(&self) -> usize {
        self.term.grid().display_offset()
    }
    /// The cursor's row and column on the visible screen.
    pub fn cursor(&self) -> (u16, u16) {
        let point = self.term.grid().cursor.point;
        let row = point.line.0 + self.scrollback() as i32;
        (row.max(0) as u16, point.column.0 as u16)
    }
    /// Whether the cursor is hidden, by the program or by scrolling back.
    pub fn hide_cursor(&self) -> bool {
        !self.term.mode().contains(TermMode::SHOW_CURSOR) || self.scrollback() > 0
    }
    pub fn alternate_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }
    /// Whether arrow keys send application sequences.
    pub fn application_cursor(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }
    /// Shows the screen `offset` rows back into the scrollback.
    pub fn scroll(&mut self, offset: usize) {
        let target = offset.min(self.term.grid().history_size());
        let delta = target as i64 - self.scrollback() as i64;
        if delta != 0 {
            self.term.scroll_display(Scroll::Delta(delta as i32));
        }
    }
    /// The grid point at a visible row and column.
    fn point(&self, row: u16, col: u16) -> Point {
        Point::new(
            Line(i32::from(row) - self.scrollback() as i32),
            Column(usize::from(col)),
        )
    }
    /// The visible screen as text: rows without trailing blanks, a wrapped
    /// row running on into the next.
    pub fn text(&self) -> String {
        let (rows, cols) = self.size();
        let mut out = String::new();
        for row in 0..rows {
            let line = &self.term.grid()[self.point(row, 0).line];
            let mut text = String::new();
            for col in 0..cols {
                let cell = &line[Column(usize::from(col))];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let shown = cell_text(cell);
                if shown.is_empty() {
                    text.push(' ');
                } else {
                    text.push_str(&shown);
                }
            }
            let wrapped = line[Column(usize::from(cols) - 1)]
                .flags
                .contains(Flags::WRAPLINE);
            if wrapped {
                out.push_str(&text);
            } else {
                out.push_str(text.trim_end());
                if row + 1 < rows {
                    out.push('\n');
                }
            }
        }
        out.truncate(out.trim_end().len());
        out
    }
    /// The text between two visible cell positions, end excluded, as a copy
    /// would take it.
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
        self.term
            .bounds_to_string(self.point(start.0, start.1), self.point(last.0, last.1))
    }
    /// Input for pasting `text`, bracketed when the program asked for it.
    pub fn paste(&self, text: &str) -> Result<Vec<u8>, String> {
        if text.len() > 64 * 1024 {
            return Err("terminal paste exceeds 64 KiB".into());
        }
        let clean = text.replace(['\x1b', '\0'], "");
        Ok(if self.term.mode().contains(TermMode::BRACKETED_PASTE) {
            format!("\x1b[200~{clean}\x1b[201~").into_bytes()
        } else {
            clean.into_bytes()
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
}
#[cfg(test)]
mod tests {
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
        e.resize(86, 320).unwrap();
        e.resize(256, 512).unwrap();
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
        assert_eq!(frame.cells()[0][8].text, "c");
        assert_eq!(e.copy((0, 0), (0, 10)), "ab\tcd");
    }
    #[test]
    fn lines_rewrap_when_the_screen_narrows_and_rejoin_when_it_widens() {
        let budget = Budget::new(64 * 1024 * 1024);
        let mut e = Engine::new(4, 40, 100, 16 * 1024 * 1024, budget).unwrap();
        let long = format!("start-{}-end", "x".repeat(50));
        e.process(format!("{long}\r\nnext").as_bytes()).unwrap();
        e.resize(4, 20).unwrap();
        // The line rewraps into three rows of twenty; rows above the cursor's
        // move into the scrollback to keep it in place, as in VTE.
        let narrow = e.text();
        assert!(narrow.ends_with("-end\nnext"), "{narrow:?}");
        e.scroll(usize::MAX);
        let earlier = e.text();
        assert!(earlier.starts_with("start-"), "{earlier:?}");
        e.scroll(0);
        e.resize(4, 80).unwrap();
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
        assert_eq!(e.paste("one\ntwo").unwrap(), b"\x1b[200~one\ntwo\x1b[201~");
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
        assert!(e.filter.string.capacity() <= OSC_LIMIT * 2);
        e.process(b"\x07safe").unwrap();
        assert!(e.text().contains("safe"));
        e.process(b"\x1bP").unwrap();
        for _ in 0..100 {
            e.process(&[b'B'; 8192]).unwrap();
        }
        e.process(b"\x1b\\").unwrap();
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
    fn resize_admission_is_atomic_and_tracks_retained_high_water_capacity() {
        let budget = Budget::new(64 * 1024 * 1024);
        let mut e = Engine::new(24, 80, 1000, 16 * 1024 * 1024, budget).unwrap();
        e.process(b"preserved").unwrap();
        let old = e.charged_bytes();
        assert!(e.resize(256, 512).is_err());
        assert_eq!(e.size(), (24, 80));
        assert_eq!(e.charged_bytes(), old);
        e.resize(32, 100).unwrap();
        let high = e.charged_bytes();
        e.resize(8, 20).unwrap();
        assert_eq!(e.charged_bytes(), high);
        assert!(e.text().contains("preserved"));
    }
}
