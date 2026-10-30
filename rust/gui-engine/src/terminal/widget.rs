//! The terminal widget: draws a terminal's screen on a monospace grid, sends
//! keys, text and pastes to its program, selects with the pointer, and
//! exposes the screen as accessible text.

use serde::{Deserialize, Serialize};
use windowing::{Key, MouseButton};

use std::sync::Arc;

use super::frame::{Color as CellColor, Frame};
use super::keyboard;
use super::links::Link;
use super::{Cell, Event as TerminalEvent, with_entry};
use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Rect, Size};
use crate::memory::ByteCache;
use crate::style::color::Color;
use crate::style::{TextRole, Theme};
use crate::text::layout::{self, LayoutOptions, TextStyle};
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx, UiMsg};
use crate::widgets::state::StateStore;

/// A view of terminal `terminal`, which keeps its screen while the view
/// comes and goes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub id: String,
    /// The terminal handle this view shows.
    pub terminal: i64,
    /// The accessible name; the program's title is its description.
    pub label: String,
    /// The size in cells the view takes where its parent sets no bound; it
    /// fills any bounded space, and the screen follows the size it is given.
    pub rows: i64,
    pub cols: i64,
    /// The font family the view prefers; empty takes the bundled
    /// [`DEFAULT_FONT`]. A family that is not installed falls back to the
    /// bundled monospace face, so every platform keeps one cell width.
    #[serde(default)]
    pub font: String,
    /// The text size in points; zero takes [`DEFAULT_POINTS`].
    #[serde(default)]
    pub font_size: f64,
}

impl View {
    pub fn new(id: &str, terminal: i64, label: &str) -> View {
        View {
            id: id.to_string(),
            terminal,
            label: label.to_string(),
            rows: 24,
            cols: 80,
            font: String::new(),
            font_size: 0.0,
        }
    }
}

pub(super) const PAD: i64 = 4;
const WHEEL_ROWS: i64 = 3;
/// Layout bytes kept per screen cell: room for two screens of runs at about
/// 160 bytes a cell (a glyph's layout plus each run's own), the screen the
/// last paint drew and the one a paint draws, so rows that scroll or stay
/// put are laid out once.
pub(super) const LAYOUT_BYTES_PER_CELL: usize = 2 * 160;

/// The family a terminal draws in when its view names none: IBM Plex Mono,
/// bundled with the toolkit (SIL Open Font License 1.1).
pub const DEFAULT_FONT: &str = "IBM Plex Mono";

/// The text size, in points, a terminal draws at when its view sets none.
pub const DEFAULT_POINTS: f64 = 10.0;

const PLEX_MONO: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf");
const PLEX_MONO_BOLD: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-Bold.ttf");

/// Registers the bundled terminal faces with the font registry once.
fn ensure_default_font() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        for data in [PLEX_MONO, PLEX_MONO_BOLD] {
            // The faces are fixed assets well inside the registry's budget;
            // were one refused, the family check below falls back to the
            // bundled monospace face.
            let _ = crate::text::fonts::register_static(data);
        }
    });
}

/// The grid's cell width and line height in pixels, and the text style, for
/// a view in a theme.
pub(super) fn metrics(v: &View, theme: &Theme) -> (f64, f64, TextStyle) {
    let mut style = theme.font(TextRole::Mono);
    let points = if v.font_size > 0.0 {
        v.font_size
    } else {
        DEFAULT_POINTS
    };
    // A point is 96/72 interface units.
    style.size = points * 96.0 / 72.0 * theme.scale * theme.text_scale;
    ensure_default_font();
    let family = if v.font.is_empty() {
        DEFAULT_FONT
    } else {
        &v.font
    };
    if crate::text::fonts::family_available(family) {
        style.family = crate::text::fonts::Family::Named(family.to_string());
    }
    let (width, _) = layout::measure("M", &style);
    let (_, _, line) = layout::metrics(&style);
    (width.max(1.0), line.ceil().max(1.0), style)
}

/// The rows and columns that fit a view of `size`.
fn grid(v: &View, theme: &Theme, size: Size) -> (i64, i64) {
    let (cw, lh, _) = metrics(v, theme);
    let pad = theme.px(PAD);
    let cols = ((size.width - 2 * pad) as f64 / cw).floor() as i64;
    let rows = ((size.height - 2 * pad) as f64 / lh).floor() as i64;
    (rows.clamp(1, 256), cols.clamp(1, 512))
}

/// The cell under a local point, clamped to the screen; a point in a cell's
/// right half selects up to its far edge.
fn cell_at(v: &View, theme: &Theme, x: i64, y: i64, rows: i64, cols: i64) -> Cell {
    let (cw, lh, _) = metrics(v, theme);
    let pad = theme.px(PAD) as f64;
    let col = ((x as f64 - pad) / cw).round() as i64;
    let row = ((y as f64 - pad) / lh).floor() as i64;
    Cell {
        row: row.clamp(0, rows - 1),
        col: col.clamp(0, cols),
    }
}

/// The cell a local point falls in, or None off the screen's cells.
fn cell_under(v: &View, theme: &Theme, x: i64, y: i64) -> Option<(u16, u16)> {
    let (cw, lh, _) = metrics(v, theme);
    let pad = theme.px(PAD) as f64;
    let col = ((x as f64 - pad) / cw).floor();
    let row = ((y as f64 - pad) / lh).floor();
    (col >= 0.0 && row >= 0.0 && col < f64::from(u16::MAX) && row < f64::from(u16::MAX))
        .then_some((row as u16, col as u16))
}

/// The link at a local point, as the screen last painted it.
fn link_under(v: &View, theme: &Theme, x: i64, y: i64) -> Option<Arc<str>> {
    let (row, col) = cell_under(v, theme, x, y)?;
    with_entry(v.terminal, |e| {
        e.links
            .iter()
            .find(|l| l.contains(row, col))
            .map(|l| l.uri.clone())
    })
    .flatten()
}

/// Whether a press with `mods` opens the link under it: Command on macOS,
/// where Ctrl+click is a right click, and Ctrl elsewhere.
fn opens_link(mods: windowing::Mods) -> bool {
    if cfg!(target_os = "macos") {
        mods.gui
    } else {
        mods.ctrl
    }
}

/// Underlines the link under the pointer, and tells whether that changed.
fn hover(v: &View, link: Option<Arc<str>>, cx: &mut EventCx) {
    if link.is_some() {
        cx.cursor = Some(windowing::SystemCursor::Pointer);
    }
    let changed = with_entry(v.terminal, |e| {
        let changed = e.hovered != link;
        e.hovered = link;
        changed
    });
    if changed == Some(true) {
        cx.redraw = true;
    }
}

/// Records the link under a press, and tells the application when it is
/// another than the last press found.
fn point(v: &View, link: Option<Arc<str>>, cx: &mut EventCx) {
    let changed = with_entry(v.terminal, |e| {
        let changed = e.pointed != link;
        e.pointed = link.clone();
        changed
    });
    if changed == Some(true) {
        let uri = link.as_deref().unwrap_or_default().to_string();
        cx.emit(&UiMsg::Terminal(
            v.terminal,
            TerminalEvent::LinkPointed(uri),
        ));
    }
}

fn hovered_at(hovered: &[&Link], row: u16, col: usize) -> bool {
    hovered.iter().any(|l| l.contains(row, col as u16))
}

pub fn layout_terminal(v: &View, c: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    let (cw, lh, _) = metrics(v, theme);
    let pad = theme.px(PAD);
    let width = (v.cols.max(1) as f64 * cw).ceil() as i64 + 2 * pad;
    let height = (v.rows.max(1) as f64 * lh).ceil() as i64 + 2 * pad;
    Node::leaf(c.biggest(Size::new(width, height)))
}

// The sixteen named colors for a dark background (Tango, with a deeper
// green) and for a light one, each set chosen to read on its background.
const ANSI_DARK: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (204, 0, 0),
    (42, 113, 54),
    (196, 160, 0),
    (52, 101, 164),
    (117, 80, 123),
    (6, 152, 154),
    (211, 215, 207),
    (85, 87, 83),
    (239, 41, 41),
    (102, 165, 124),
    (252, 233, 79),
    (115, 159, 207),
    (173, 127, 168),
    (52, 226, 226),
    (238, 238, 236),
];
const ANSI_LIGHT: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 49, 49),
    (0, 128, 0),
    (148, 152, 0),
    (4, 81, 165),
    (188, 5, 188),
    (5, 152, 188),
    (85, 85, 85),
    (102, 102, 102),
    (205, 49, 49),
    (20, 140, 20),
    (128, 128, 0),
    (4, 81, 165),
    (188, 5, 188),
    (5, 152, 188),
    (165, 165, 165),
];

/// The WCAG contrast ratio body text needs; text below it against its
/// background is moved toward black or white until it reaches it.
const MIN_CONTRAST: f64 = 4.5;

fn indexed(i: u8, dark: bool) -> Color {
    match i {
        0..=15 => {
            let palette = if dark { &ANSI_DARK } else { &ANSI_LIGHT };
            let (r, g, b) = palette[usize::from(i)];
            Color::rgb(r, g, b)
        }
        16..=231 => {
            let level = |n: u8| if n == 0 { 0 } else { 55 + 40 * n };
            let n = i - 16;
            Color::rgb(level(n / 36), level((n / 6) % 6), level(n % 6))
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            Color::rgb(v, v, v)
        }
    }
}

fn resolve(color: &CellColor, default: Color, dark: bool) -> Color {
    match color {
        CellColor::Default => default,
        CellColor::Indexed(i) => indexed(*i, dark),
        CellColor::Rgb(r, g, b) => Color::rgb(*r, *g, *b),
    }
}

/// `fg`, moved the least way toward black or white that reads on `bg` at
/// [`MIN_CONTRAST`]. Text a program hides by drawing it in its background
/// color stays hidden.
fn legible(fg: Color, bg: Color) -> Color {
    if fg == bg || fg.contrast(bg) >= MIN_CONTRAST {
        return fg;
    }
    let toward = if Color::black().contrast(bg) >= Color::white().contrast(bg) {
        Color::black()
    } else {
        Color::white()
    };
    // Contrast grows monotonically along the mix, so halving the interval
    // finds its least sufficient step; sixteen halvings are finer than one
    // level of an 8-bit channel.
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..16 {
        let mid = (low + high) / 2.0;
        if fg.mix(toward, mid).contrast(bg) >= MIN_CONTRAST {
            high = mid;
        } else {
            low = mid;
        }
    }
    fg.mix(toward, high)
}

fn in_selection(selection: Option<(Cell, Cell)>, row: i64, col: i64) -> bool {
    let Some((a, b)) = selection else {
        return false;
    };
    let (start, end) = if (a.row, a.col) <= (b.row, b.col) {
        (a, b)
    } else {
        (b, a)
    };
    (row, col) >= (start.row, start.col) && (row, col) < (end.row, end.col)
}

pub fn paint_terminal(v: &View, node: &Node, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let r = Rect::from_size(node.size());
    let background = theme.field;
    let foreground = theme.text;
    let dark = background.luminance() < 0.5;
    cx.fill(r, background);
    let (rows, cols) = grid(v, &theme, node.size());
    let captured = with_entry(v.terminal, |entry| {
        // The screen follows the view's size; the program hears of it.
        super::follow_view(entry, rows as u16, cols as u16);
        let frame = entry.engine.frame()?;
        entry.links = frame.links().to_vec();
        if entry
            .hovered
            .as_ref()
            .is_some_and(|uri| !entry.links.iter().any(|l| &l.uri == uri))
        {
            entry.hovered = None;
        }
        Ok::<_, String>((frame, entry.selection, entry.hovered.clone()))
    });
    let Some(Ok((frame, selection, hovered))) = captured else {
        return;
    };
    let hovered: Vec<&Link> = frame
        .links()
        .iter()
        .filter(|l| hovered.as_ref() == Some(&l.uri))
        .collect();
    let (cw, lh, style) = metrics(v, &theme);
    let pad = theme.px(PAD);
    let origin = cx.abs(r);
    let focused = cx.focus == v.id;
    // Run layouts are kept per terminal, sized to its screen: output brings
    // rows that never come back, which the shared layout cache would hold to
    // its whole budget.
    let generation = crate::text::fonts::generation();
    let mut layouts = with_entry(v.terminal, |entry| {
        // Faces that arrived since may draw these runs differently.
        if entry.layouts_generation != generation {
            entry.layouts.clear();
            entry.layouts_generation = generation;
        }
        std::mem::replace(&mut entry.layouts, ByteCache::new(0))
    })
    .unwrap_or_else(|| ByteCache::new(0));
    layouts
        .set_limit(usize::from(frame.rows()) * usize::from(frame.cols()) * LAYOUT_BYTES_PER_CELL);
    let painted_from = layouts.mark();
    for (row, cells) in frame.cells().iter().enumerate() {
        let screen_row = row as u16;
        let row = row as i64;
        let y = pad + (row as f64 * lh) as i64;
        let mut col = 0;
        while col < cells.len() {
            let cell = &cells[col];
            // The second column of a wide cell is drawn with the cell.
            if cell.continuation {
                col += 1;
                continue;
            }
            let selected = in_selection(selection, row, col as i64);
            let lit = hovered_at(&hovered, screen_row, col);
            let (mut fg, mut bg) = (
                resolve(&cell.fg, foreground, dark),
                resolve(&cell.bg, background, dark),
            );
            if cell.inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            fg = legible(fg, bg);
            if selected {
                fg = theme.selection_text;
                bg = theme.selection;
            }
            if cell.dim {
                fg = fg.mix(bg, 0.4);
            }
            // ASCII cells of one style draw as one run; any other cell draws
            // alone at its column, so fallback fonts cannot shift the grid.
            let ascii =
                |c: &super::frame::Cell| !c.continuation && c.text.len() <= 1 && c.text.is_ascii();
            let mut end = col + 1;
            if ascii(cell) {
                while end < cells.len()
                    && ascii(&cells[end])
                    && cells[end].fg == cell.fg
                    && cells[end].bg == cell.bg
                    && cells[end].inverse == cell.inverse
                    && cells[end].bold == cell.bold
                    && cells[end].italic == cell.italic
                    && cells[end].underline == cell.underline
                    && cells[end].dim == cell.dim
                    && in_selection(selection, row, end as i64) == selected
                    && hovered_at(&hovered, screen_row, end) == lit
                {
                    end += 1;
                }
            }
            let span = if cell.wide { 2 } else { end - col };
            let x = pad + (col as f64 * cw) as i64;
            let width = (span as f64 * cw).ceil() as i64;
            if bg != background {
                cx.fill(Rect::new(x, y, width, lh as i64), bg);
            }
            let text: String = cells[col..end]
                .iter()
                .filter(|c| !c.continuation)
                .map(|c| {
                    if c.text.is_empty() {
                        " "
                    } else {
                        c.text.as_str()
                    }
                })
                .collect();
            // Blank cells draw no ink, and backgrounds and underlines fill
            // their cells, so a run is laid out only up to its last glyph.
            let text = text.trim_end_matches(' ');
            if !text.trim().is_empty() {
                let mut run = style.clone();
                run.bold = cell.bold;
                run.italic = cell.italic;
                let mut key = format!("{}#{}", run.key(), text);
                lay_out(&mut layouts, &key, text, &run);
                let mut at = ((origin.x + x) as f64, (origin.y + y) as f64);
                // A glyph wider than its cells, such as an emoji a program
                // counts as one cell, is drawn smaller and centered on its
                // row, so it stays inside them.
                if !ascii(cell)
                    && let Some(drawn) = layouts.get(&key).map(|l| l.width)
                    && drawn > width as f64
                {
                    run.size *= width as f64 / drawn;
                    key = format!("{}#{}", run.key(), text);
                    lay_out(&mut layouts, &key, text, &run);
                    if let Some(height) = layouts.get(&key).map(|l| l.height) {
                        at.1 += ((lh - height) / 2.0).max(0.0);
                    }
                }
                match layouts.get(&key) {
                    Some(laid) => layout::draw(&mut cx.canvas, laid, at.0, at.1, &run, fg),
                    // A run too large for the cache is laid out for this paint.
                    None => {
                        let laid =
                            layout::layout_uncached(text, &run, LayoutOptions::single_line());
                        layout::draw(&mut cx.canvas, &laid, at.0, at.1, &run, fg);
                    }
                }
            }
            if cell.underline || lit {
                cx.fill(
                    Rect::new(x, y + lh as i64 - theme.px(2), width, theme.px(1).max(1)),
                    fg,
                );
            }
            col = end.max(col + 1);
        }
    }
    // A row the screen no longer shows only comes back by scrolling back,
    // which lays it out again, so the cache keeps the runs this paint drew.
    layouts.evict_unused_since(painted_from);
    with_entry(v.terminal, |entry| entry.layouts = layouts);
    // The cursor shows focus, filled while the terminal has it and hollow
    // otherwise, so the terminal draws no focus ring around its screen.
    paint_cursor(&frame, focused, cx, (cw, lh, pad));
}

// Lays out `text` in `run` under `key` unless the cache holds it.
fn lay_out(
    layouts: &mut ByteCache<String, layout::TextLayout>,
    key: &str,
    text: &str,
    run: &TextStyle,
) {
    if layouts.get(&key.to_string()).is_none() {
        let laid = layout::layout_uncached(text, run, LayoutOptions::single_line());
        let bytes = key.len() + laid.owned_bytes();
        layouts.insert(key.to_string(), laid, bytes);
    }
}

fn paint_cursor(frame: &Frame, focused: bool, cx: &mut PaintCx, (cw, lh, pad): (f64, f64, i64)) {
    if frame.hide_cursor() {
        return;
    }
    let (row, col) = frame.cursor();
    let x = pad + (f64::from(col) * cw) as i64;
    let y = pad + (f64::from(row) * lh) as i64;
    let cell = Rect::new(x, y, cw.ceil() as i64, lh as i64);
    // The theme names no caret color, so the cursor takes a neutral gray that
    // shows on light and dark backgrounds alike.
    let color = Color::rgb(134, 129, 129);
    if !focused {
        cx.stroke_round(cell, 0, 1, color);
    } else if cx.caret_on {
        cx.fill(cell, color.with_alpha(160));
    }
}

/// The key a terminal encodes, or None for a key it leaves to the window.
fn encode_key(key: Key, mods: windowing::Mods) -> Option<keyboard::Key> {
    use keyboard::Key as K;
    Some(match key {
        Key::Return | Key::KpEnter => K::Enter,
        Key::Backspace => K::Backspace,
        Key::Tab if !mods.ctrl => K::Tab,
        Key::Escape => K::Escape,
        Key::Up => K::Up,
        Key::Down => K::Down,
        Key::Left => K::Left,
        Key::Right => K::Right,
        Key::Home => K::Home,
        Key::End => K::End,
        Key::Insert => K::Insert,
        Key::Delete => K::Delete,
        Key::PageUp => K::PageUp,
        Key::PageDown => K::PageDown,
        Key::F1 => K::Function(1),
        Key::F2 => K::Function(2),
        Key::F3 => K::Function(3),
        Key::F4 => K::Function(4),
        Key::F5 => K::Function(5),
        Key::F7 => K::Function(7),
        Key::F8 => K::Function(8),
        Key::F9 => K::Function(9),
        Key::F10 => K::Function(10),
        Key::F11 => K::Function(11),
        Key::F12 => K::Function(12),
        // Printable keys arrive as text unless Ctrl or Alt modifies them.
        _ if mods.ctrl || mods.alt => K::Character(printable(key, mods.shift)?),
        _ => return None,
    })
}

fn printable(key: Key, shift: bool) -> Option<char> {
    let letters = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    let digits = [
        Key::Num0,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
    ];
    if let Some(i) = letters.iter().position(|k| *k == key) {
        let c = (b'a' + i as u8) as char;
        return Some(if shift { c.to_ascii_uppercase() } else { c });
    }
    if let Some(i) = digits.iter().position(|k| *k == key) {
        return Some((b'0' + i as u8) as char);
    }
    (key == Key::Space).then_some(' ')
}

// A key whose Ctrl+Shift chord the terminal leaves to the application:
// letters, digits, PageUp and PageDown.
fn application_chord(key: Key) -> bool {
    printable(key, false).is_some_and(|c| c != ' ') || key == Key::PageUp || key == Key::PageDown
}

fn ime_area(v: &View, cx: &mut EventCx) {
    let cursor = with_entry(v.terminal, |e| e.engine.cursor());
    if let Some((row, col)) = cursor {
        let (cw, lh, _) = metrics(v, &cx.theme);
        let pad = cx.theme.px(PAD);
        let x = pad + (f64::from(col) * cw) as i64;
        let y = pad + (f64::from(row) * lh) as i64;
        cx.ime = Some((
            cx.to_window(Rect::new(x, y, cw.ceil() as i64, lh as i64)),
            0,
        ));
    }
}

fn redraw(cx: &mut EventCx) {
    cx.redraw = true;
    cx.consume();
}

pub fn terminal_event(v: &View, node: &Node, ev: &Event, _st: &mut StateStore, cx: &mut EventCx) {
    let ev = &crate::widgets::cx::local_event(ev, cx);
    let size = node.size();
    let (rows, cols) = grid(v, &cx.theme, size);
    let focused = cx.is_focused(&v.id);
    match ev {
        Event::PointerDown {
            x,
            y,
            button,
            clicks,
            mods,
        } => {
            cx.focus_on(&v.id);
            let link = link_under(v, &cx.theme, *x, *y);
            point(v, link.clone(), cx);
            if let (MouseButton::Left, Some(uri)) = (button, &link)
                && opens_link(*mods)
            {
                cx.emit(&UiMsg::Terminal(
                    v.terminal,
                    TerminalEvent::LinkActivated(uri.to_string()),
                ));
                return cx.consume();
            }
            // A right press goes on to the context menu around the terminal.
            if *button == MouseButton::Right {
                return;
            }
            let at = cell_at(v, &cx.theme, *x, *y, rows, cols);
            match button {
                MouseButton::Left if *clicks >= 3 => {
                    let line = (
                        Cell {
                            row: at.row,
                            col: 0,
                        },
                        Cell {
                            row: at.row,
                            col: cols,
                        },
                    );
                    let _ = super::select(v.terminal, Some(line));
                }
                MouseButton::Left => {
                    let _ = super::select(v.terminal, Some((at, at)));
                    cx.capture_pointer(&v.id);
                }
                MouseButton::Middle => {
                    let _ = super::paste(v.terminal, &crate::services::clipboard::primary_text());
                }
                _ => {}
            }
            redraw(cx);
        }
        Event::PointerMove {
            x,
            y,
            pressed: true,
            ..
        } => {
            let at = cell_at(v, &cx.theme, *x, *y, rows, cols);
            let anchor = with_entry(v.terminal, |e| e.selection.map(|(a, _)| a)).flatten();
            if let Some(anchor) = anchor {
                let _ = super::select(v.terminal, Some((anchor, at)));
                redraw(cx);
            }
        }
        Event::PointerMove {
            x,
            y,
            pressed: false,
            ..
        } => hover(v, link_under(v, &cx.theme, *x, *y), cx),
        Event::PointerLeave => hover(v, None, cx),
        Event::PointerUp { .. } => {
            cx.release_pointer();
            let collapsed = with_entry(v.terminal, |e| e.selection.is_some_and(|(a, b)| a == b));
            if collapsed == Some(true) {
                let _ = super::select(v.terminal, None);
            } else if let Ok(text) = super::copy(v.terminal)
                && !text.is_empty()
            {
                crate::services::clipboard::set_primary_text(&text);
            }
            redraw(cx);
        }
        Event::Scroll { dy, .. } => {
            let alternate = with_entry(v.terminal, |e| e.engine.alternate_screen());
            if alternate == Some(false) && *dy != 0.0 {
                let rows = if *dy > 0.0 { WHEEL_ROWS } else { -WHEEL_ROWS };
                let _ = super::scroll(v.terminal, rows);
                redraw(cx);
            }
        }
        Event::KeyDown { key, mods, .. } if focused => key_down(v, *key, *mods, rows, cx),
        Event::Text(text) if focused => {
            let suppressed = with_entry(v.terminal, |e| std::mem::take(&mut e.suppress_text));
            if suppressed != Some(true) && super::input(v.terminal, text.as_bytes()).is_ok() {
                ime_area(v, cx);
                redraw(cx);
            }
        }
        Event::Command(name) if focused => {
            let used = match name.as_str() {
                "edit.copy" => copy_selection(v),
                "edit.paste" => {
                    super::paste(v.terminal, &crate::services::clipboard::text()).is_ok()
                }
                "edit.select-all" => super::select(
                    v.terminal,
                    Some((
                        Cell::default(),
                        Cell {
                            row: rows - 1,
                            col: cols,
                        },
                    )),
                )
                .is_ok(),
                // A menu opened from the keyboard is about no link; the
                // command goes on to the context menu around the terminal.
                "gui.context-menu" => {
                    point(v, None, cx);
                    false
                }
                _ => false,
            };
            if used {
                redraw(cx);
            }
        }
        Event::FocusIn => {
            ime_area(v, cx);
            cx.redraw = true;
        }
        Event::FocusOut => cx.redraw = true,
        _ => {}
    }
}

fn copy_selection(v: &View) -> bool {
    match super::copy(v.terminal) {
        Ok(text) if !text.is_empty() => {
            crate::services::clipboard::set_text(&text);
            true
        }
        _ => false,
    }
}

fn key_down(v: &View, key: Key, mods: windowing::Mods, rows: i64, cx: &mut EventCx) {
    // Ctrl+Shift+C and Ctrl+Shift+V copy and paste, leaving Ctrl+C and
    // Ctrl+V to the program.
    if mods.ctrl && mods.shift && key == Key::C {
        copy_selection(v);
        return redraw(cx);
    }
    if mods.ctrl && mods.shift && key == Key::V {
        let _ = super::paste(v.terminal, &crate::services::clipboard::text());
        return redraw(cx);
    }
    // The rest of the Ctrl+Shift layer belongs to the application, as in
    // other desktop terminals, so those strokes reach the window's commands.
    if mods.ctrl && mods.shift && application_chord(key) {
        return;
    }
    // F10 opens the window's menu bar, as in GNOME Terminal; Shift+F10 opens
    // a context menu. Where menus live in the system menu bar (macOS), plain
    // F10 belongs to the program, as in Terminal.app.
    if key == Key::F10
        && !mods.ctrl
        && !mods.alt
        && (mods.shift || !windowing::menu::has_native_menu_bar())
    {
        return;
    }
    // F11 makes the window fullscreen, as in GNOME Terminal and xfce4-terminal.
    if key == Key::F11 && !mods.ctrl && !mods.alt && !mods.shift {
        return;
    }
    if mods.shift && !mods.ctrl && (key == Key::PageUp || key == Key::PageDown) {
        let page = if key == Key::PageUp { rows } else { -rows };
        let _ = super::scroll(v.terminal, page);
        return redraw(cx);
    }
    let Some(encoded) = encode_key(key, mods) else {
        return;
    };
    let application_cursor =
        with_entry(v.terminal, |e| e.engine.application_cursor()).unwrap_or(false);
    let bytes = keyboard::encode(
        encoded,
        keyboard::Mods {
            shift: mods.shift,
            alt: mods.alt,
            ctrl: mods.ctrl,
        },
        application_cursor,
    );
    if bytes.is_empty() {
        return;
    }
    if super::input(v.terminal, &bytes).is_ok() {
        if mods.alt && !mods.ctrl {
            with_entry(v.terminal, |e| e.suppress_text = true);
        }
        ime_area(v, cx);
        redraw(cx);
    }
}

pub fn terminal_semantics(v: &View, node: &Node) -> Option<AccessNode> {
    let mut n = AccessNode::control(Role::Terminal, &v.id, &v.label, "");
    n.actions = vec!["copy".to_string(), "paste".to_string()];
    n.multiline = true;
    n.bounds = Rect::from_size(node.size());
    let read = with_entry(v.terminal, |entry| {
        let frame = entry.engine.frame().ok()?;
        let (row, col) = frame.cursor();
        let caret = frame.byte_offset(row, col) as i64;
        let selection = entry.selection.map(|(a, b)| {
            let a = frame.byte_offset(a.row as u16, a.col as u16) as i64;
            let b = frame.byte_offset(b.row as u16, b.col as u16) as i64;
            (a.min(b), a.max(b))
        });
        Some((
            frame.text().to_string(),
            entry.engine.notifications().title.clone(),
            caret,
            selection,
        ))
    });
    if let Some(Some((text, title, caret, selection))) = read {
        n.value = text;
        n.description = title;
        n.caret = caret;
        let (start, end) = selection.unwrap_or((caret, caret));
        n.text_start = start;
        n.text_end = end;
    }
    Some(n)
}

/// Runs an accessible action on the terminal.
pub fn terminal_action(v: &View, action: &str) -> bool {
    match action {
        "copy" => copy_selection(v),
        "paste" => super::paste(v.terminal, &crate::services::clipboard::text()).is_ok(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_color_reads_on_dark_and_light_backgrounds() {
        for (background, dark) in [
            (Color::rgb(30, 30, 30), true),
            (Color::rgb(250, 250, 250), false),
        ] {
            for i in 0..16 {
                let shown = legible(indexed(i, dark), background);
                assert!(
                    shown.contrast(background) >= MIN_CONTRAST,
                    "color {i} on {background:?}"
                );
            }
        }
    }

    #[test]
    fn legible_text_keeps_its_color_and_hidden_text_stays_hidden() {
        let white = Color::white();
        assert_eq!(legible(Color::black(), white), Color::black());
        assert_eq!(legible(white, white), white);
        let faint = Color::rgb(220, 220, 0);
        let moved = legible(faint, white);
        assert_ne!(moved, faint);
        assert!(moved.contrast(white) >= MIN_CONTRAST);
    }

    #[test]
    fn a_missing_font_falls_back_to_the_bundled_monospace_face_at_the_asked_size() {
        let theme = Theme::default();
        let mut view = View::new("term", 1, "Shell");
        view.font = "No Such Family 2026".into();
        view.font_size = 9.0;
        let (_, _, style) = metrics(&view, &theme);
        assert_eq!(style.family, crate::text::fonts::Family::Monospace);
        assert!((style.size - 12.0 * theme.scale * theme.text_scale).abs() < 1e-9);
        view.font = String::new();
        view.font_size = 0.0;
        let (_, _, default) = metrics(&view, &theme);
        assert_eq!(
            default.family,
            crate::text::fonts::Family::Named(DEFAULT_FONT.into())
        );
        let expected = DEFAULT_POINTS * 96.0 / 72.0 * theme.scale * theme.text_scale;
        assert!((default.size - expected).abs() < 1e-9);
    }

    #[test]
    fn the_view_fills_bounded_space_and_asks_for_its_cells_otherwise() {
        let theme = Theme::default();
        let states = crate::widgets::state::StateStore::default();
        let mut cx = LayoutCx::new(&theme, &states, Size::new(1200, 800));
        let view = View::new("term", 1, "Shell");
        let filled = layout_terminal(&view, Constraints::loose(Size::new(1200, 800)), &mut cx);
        assert_eq!(filled.size(), Size::new(1200, 800));
        let natural = layout_terminal(&view, Constraints::unbounded(), &mut cx);
        let (cols, rows) = (natural.size().width, natural.size().height);
        assert!(cols < 1200 && rows < 800 && cols > 0 && rows > 0);
    }
}
