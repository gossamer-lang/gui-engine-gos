//! The terminal widget: draws a terminal's screen on a monospace grid, sends
//! keys, text and pastes to its program, selects with the pointer, and
//! exposes the screen as accessible text.

use serde::{Deserialize, Serialize};
use wezterm_term::color::{ColorPalette, SrgbaTuple};
use wezterm_term::{KeyModifiers, MouseEvent, MouseEventKind};
use windowing::{Key, MouseButton};

use std::sync::Arc;

use super::frame::{Color as CellColor, Frame, RowVersion};
use super::images;
use super::keyboard;
use super::layer::LayerContext;
use super::links::{Link, Place, place};
use super::shapes;
use super::{Cell, Event as TerminalEvent, Span, with_entry};
use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Rect, Size};
use crate::memory::ByteCache;
use crate::render::retained::RecordedSpan;
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
    /// The colors the screen shows; `None` takes the theme's, with named
    /// colors chosen for its light or dark background.
    #[serde(default)]
    pub scheme: Option<Scheme>,
    /// How the view draws and handles its terminal beyond font and colors.
    #[serde(default)]
    pub options: ViewOptions,
}

/// How a view draws and handles its terminal beyond its font and colors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewOptions {
    /// The cursor's shape: "block", "bar" or "underline".
    pub cursor: String,
    /// Whether the cursor blinks.
    pub blink: bool,
    /// Bold text in the eight standard colors takes their bright forms.
    pub bold_bright: bool,
    /// Letters too close to their background move toward a readable color.
    pub legible: bool,
    /// Space around the screen, in interface units.
    pub padding: i64,
    /// When the scrollbar shows: "always", "scrolling" (while the view is
    /// scrolled back) or "never".
    pub scrollbar: String,
    /// Which edge the scrollbar lies along: "right" or "left".
    pub scrollbar_side: String,
    /// New output brings a view scrolled back to the live screen.
    pub scroll_on_output: bool,
    /// Typing brings a view scrolled back to the live screen.
    pub scroll_on_keystroke: bool,
    /// Characters a double click selects along with letters and digits.
    pub word_chars: String,
    /// On macOS, Option sends Meta (Escape and the key) rather than the
    /// character it types.
    pub option_as_meta: bool,
    /// A selection is copied to the clipboard as well as the primary
    /// selection.
    pub copy_on_select: bool,
    /// A paste of several lines or of control characters is held for the
    /// application to confirm (`Event::PasteHeld`).
    pub confirm_paste: bool,
    /// The bell flashes the screen.
    pub flash_bell: bool,
    /// How opaque the default background is, from 0 to 1; below 1 the
    /// window must be see-through (`Session::set_transparent`) to show what
    /// lies behind it.
    pub opacity: f64,
    /// The height of a row as a multiple of the font's line height, from 0.8
    /// to 3; text sits in the middle of a taller row.
    pub line_height: f64,
    /// The font's weight on the CSS scale (400 regular); bold text is 300
    /// heavier, from 700 to 900. The family's nearest faces draw it.
    pub font_weight: i64,
    /// Whether letters of one style join into the font's ligatures, such as
    /// a coding font's arrows.
    pub ligatures: bool,
}

impl Default for ViewOptions {
    fn default() -> ViewOptions {
        ViewOptions {
            cursor: "block".to_string(),
            blink: true,
            bold_bright: false,
            legible: true,
            padding: PAD,
            scrollbar: "never".to_string(),
            scrollbar_side: "right".to_string(),
            scroll_on_output: false,
            scroll_on_keystroke: true,
            word_chars: WORD_CHARS.to_string(),
            option_as_meta: true,
            copy_on_select: false,
            confirm_paste: false,
            flash_bell: false,
            opacity: 1.0,
            line_height: 1.0,
            font_weight: 400,
            ligatures: true,
        }
    }
}

/// What a double click selects besides letters and digits, as in GNOME
/// Terminal: enough for paths, URLs and addresses.
pub const WORD_CHARS: &str = "-#%&+,./:=?@_~";

// The space around the screen of `v`, in pixels.
fn pad_of(v: &View, theme: &Theme) -> i64 {
    theme.px(v.options.padding.clamp(0, 64))
}

// The width the scrollbar of `v` takes from the screen, in pixels: only an
// always-shown one takes room; one shown while scrolling lies over it.
fn scrollbar_room(v: &View, theme: &Theme) -> i64 {
    if v.options.scrollbar == "always" {
        theme.px(SCROLLBAR)
    } else {
        0
    }
}

// The screen's left edge in the view, in pixels: past the padding, and past
// an always-shown scrollbar on the left.
fn left_of(v: &View, theme: &Theme) -> i64 {
    let lead = if v.options.scrollbar_side == "left" {
        scrollbar_room(v, theme)
    } else {
        0
    };
    pad_of(v, theme) + lead
}

/// A terminal's colors, each `#rrggbb`: the default text and background,
/// the cursor, the selection and its text, and the sixteen named colors.
/// A color that does not parse takes the theme's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scheme {
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub selection: String,
    pub selection_text: String,
    pub ansi: Vec<String>,
}

/// The color `#rrggbb` names.
fn hex(text: &str) -> Option<Color> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some(Color::rgb(channel(0)?, channel(2)?, channel(4)?))
}

fn tuple(color: Color) -> SrgbaTuple {
    SrgbaTuple::from((color.r, color.g, color.b))
}

fn shown(color: SrgbaTuple) -> Color {
    let (r, g, b, _) = color.to_srgb_u8();
    Color::rgb(r, g, b)
}

/// The palette a view shows in `theme`: its scheme's colors over the theme's
/// own, the 256-color cube and gray ramp after the named colors.
fn palette(v: &View, theme: &Theme) -> ColorPalette {
    let scheme = v.scheme.clone().unwrap_or_default();
    let pick = |text: &str, fallback: Color| hex(text).unwrap_or(fallback);
    let background = pick(&scheme.background, theme.field);
    let foreground = pick(&scheme.foreground, theme.text);
    let dark = background.luminance() < 0.5;
    let mut out = ColorPalette::default();
    for (i, slot) in out.colors.0.iter_mut().enumerate() {
        let i = i as u8;
        let named = scheme.ansi.get(usize::from(i)).and_then(|text| hex(text));
        *slot = tuple(named.unwrap_or_else(|| indexed(i, dark)));
    }
    out.foreground = tuple(foreground);
    out.background = tuple(background);
    // The theme names no caret color, so the cursor takes a neutral gray that
    // shows on light and dark backgrounds alike.
    out.cursor_bg = tuple(pick(&scheme.cursor, Color::rgb(134, 129, 129)));
    out.cursor_border = out.cursor_bg;
    out.cursor_fg = out.background;
    out.selection_bg = tuple(pick(&scheme.selection, theme.selection));
    out.selection_fg = tuple(pick(&scheme.selection_text, theme.selection_text));
    out
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
            scheme: None,
            options: ViewOptions::default(),
        }
    }
}

pub(super) const PAD: i64 = 4;
// The scrollbar's width in interface units.
const SCROLLBAR: i64 = 10;
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
    style.ligatures = v.options.ligatures;
    ensure_default_font();
    let family = if v.font.is_empty() {
        DEFAULT_FONT
    } else {
        &v.font
    };
    if crate::text::fonts::family_available(family) {
        let weighted = crate::text::fonts::weighted_family(family, v.options.font_weight);
        style.family = crate::text::fonts::Family::Named(weighted);
    }
    let (width, _) = layout::measure("M", &style);
    let (_, _, line) = layout::metrics(&style);
    let spaced = (line * line_factor(v)).ceil().max(1.0);
    (width.max(1.0), spaced, style)
}

fn line_factor(v: &View) -> f64 {
    if v.options.line_height.is_finite() {
        v.options.line_height.clamp(0.8, 3.0)
    } else {
        1.0
    }
}

// How far text sits below the top of its row, centering it in a row taller
// (or shorter) than the font's line.
fn text_offset(style: &TextStyle, lh: f64) -> f64 {
    let (_, _, line) = layout::metrics(style);
    ((lh - line.ceil()) / 2.0).floor()
}

/// The rows and columns that fit a view of `size`.
fn grid(v: &View, theme: &Theme, size: Size) -> (i64, i64) {
    let (cw, lh, _) = metrics(v, theme);
    let pad = pad_of(v, theme);
    let cols = ((size.width - 2 * pad - scrollbar_room(v, theme)) as f64 / cw).floor() as i64;
    let rows = ((size.height - 2 * pad) as f64 / lh).floor() as i64;
    (rows.clamp(1, 256), cols.clamp(1, 512))
}

/// The cell under a local point, clamped to the screen; a point in a cell's
/// right half selects up to its far edge.
fn cell_at(v: &View, theme: &Theme, x: i64, y: i64, rows: i64, cols: i64) -> Cell {
    let (cw, lh, _) = metrics(v, theme);
    let pad = pad_of(v, theme) as f64;
    let left = left_of(v, theme) as f64;
    let col = ((x as f64 - left) / cw).round() as i64;
    let row = ((y as f64 - pad) / lh).floor() as i64;
    Cell {
        row: row.clamp(0, rows - 1),
        col: col.clamp(0, cols),
    }
}

/// The cell a local point falls in, or None off the screen's cells.
fn cell_under(v: &View, theme: &Theme, x: i64, y: i64) -> Option<(u16, u16)> {
    let (cw, lh, _) = metrics(v, theme);
    let pad = pad_of(v, theme) as f64;
    let left = left_of(v, theme) as f64;
    let col = ((x as f64 - left) / cw).floor();
    let row = ((y as f64 - pad) / lh).floor();
    (col >= 0.0 && row >= 0.0 && col < f64::from(u16::MAX) && row < f64::from(u16::MAX))
        .then_some((row as u16, col as u16))
}

/// A link at a point: a URI, or a file path that exists, with its place.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Uri(Arc<str>),
    Path(Arc<str>, Place),
}

impl Target {
    /// The text the screen underlines while the pointer is over it.
    fn shown(&self) -> Arc<str> {
        match self {
            Target::Uri(uri) | Target::Path(uri, _) => uri.clone(),
        }
    }

    /// The URI the application hears of for its context menu.
    fn uri(&self) -> String {
        match self {
            Target::Uri(uri) => uri.to_string(),
            Target::Path(_, place) => format!("file://{}", place.path.replace('%', "%25")),
        }
    }
}

/// The file `text` names, from the directory terminal `id`'s program works
/// in, when it exists; read only for the link under the pointer.
fn locate(id: i64, text: &str) -> Option<Place> {
    let mut found = place(text);
    let path = std::path::Path::new(&found.path);
    let full = if let Some(rest) = found.path.strip_prefix("~/") {
        std::path::PathBuf::from(std::env::var_os("HOME")?).join(rest)
    } else if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::PathBuf::from(super::cwd(id).ok().filter(|d| !d.is_empty())?).join(path)
    };
    std::fs::metadata(&full).ok()?;
    found.path = full.to_str()?.to_string();
    Some(found)
}

/// The link at a local point, as the screen last painted it.
fn link_under(v: &View, theme: &Theme, x: i64, y: i64) -> Option<Target> {
    let (row, col) = cell_under(v, theme, x, y)?;
    let (uri, path) = with_entry(v.terminal, |e| {
        e.links
            .iter()
            .find(|l| l.contains(row, col))
            .map(|l| (l.uri.clone(), l.path))
    })
    .flatten()?;
    if !path {
        return Some(Target::Uri(uri));
    }
    let place = locate(v.terminal, &uri)?;
    Some(Target::Path(uri, place))
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
fn hover(v: &View, target: Option<Target>, cx: &mut EventCx) {
    if target.is_some() {
        cx.cursor = Some(windowing::SystemCursor::Pointer);
    }
    let link = target.as_ref().map(Target::shown);
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
fn point(v: &View, target: Option<&Target>, cx: &mut EventCx) {
    let uri = target.map(Target::uri).unwrap_or_default();
    let link: Option<Arc<str>> = (!uri.is_empty()).then(|| Arc::from(uri.as_str()));
    let changed = with_entry(v.terminal, |e| {
        let changed = e.pointed != link;
        e.pointed = link;
        changed
    });
    if changed == Some(true) {
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
    let pad = pad_of(v, theme);
    let width = (v.cols.max(1) as f64 * cw).ceil() as i64 + 2 * pad + scrollbar_room(v, theme);
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

fn resolve(color: &CellColor, default: Color, palette: &ColorPalette) -> Color {
    match color {
        CellColor::Default => default,
        CellColor::Indexed(i) => shown(palette.colors.0[usize::from(*i)]),
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

/// The columns of each screen row that highlights cover, as `start..end`
/// ranges, for a screen whose top row is buffer row `top`.
fn highlighted_rows(highlights: &[Span], top: i64, rows: i64, cols: i64) -> Vec<Vec<(i64, i64)>> {
    let mut out = vec![Vec::new(); rows.max(0) as usize];
    for span in highlights {
        let first = span.start.row.max(top);
        let last = span.end.row.min(top + rows - 1);
        for row in first..=last {
            let from = if row == span.start.row {
                span.start.col
            } else {
                0
            };
            let to = if row == span.end.row {
                span.end.col
            } else {
                cols
            };
            if from < to {
                out[(row - top) as usize].push((from, to));
            }
        }
    }
    out
}

fn in_highlight(ranges: &[(i64, i64)], col: i64) -> bool {
    ranges.iter().any(|(from, to)| (*from..*to).contains(&col))
}

// The columns `0..cols` of screen row `row` that `in_selection` holds, as
// `start..end`.
fn selected_columns(selection: Option<(Cell, Cell)>, row: i64, cols: i64) -> Option<(i64, i64)> {
    let (a, b) = selection?;
    let (start, end) = if (a.row, a.col) <= (b.row, b.col) {
        (a, b)
    } else {
        (b, a)
    };
    if row < start.row || row > end.row {
        return None;
    }
    let from = if row == start.row {
        start.col.max(0)
    } else {
        0
    };
    let to = if row == end.row {
        end.col.min(cols)
    } else {
        cols
    };
    (from < to).then_some((from, to))
}

/// What every row of one paint draws with besides its own cells.
#[derive(Clone, Copy)]
pub(super) struct RowPaint<'a> {
    v: &'a View,
    theme: &'a Theme,
    colors: &'a ColorPalette,
    style: &'a TextStyle,
    selection: Option<(Cell, Cell)>,
    highlighted: &'a [Vec<(i64, i64)>],
    hovered: &'a [&'a Link],
    foreground: Color,
    background: Color,
    highlight_bg: Color,
    selection_bg: Color,
    selection_fg: Color,
    cw: f64,
    pub(super) lh: f64,
    rise: f64,
    pad: i64,
    left: i64,
    pub(super) origin: Rect,
}

impl RowPaint<'_> {
    /// The top and height, in the view, of screen row `row`. Cells take
    /// whole-pixel edges from the fractional grid, so each meets its
    /// neighbors with neither a gap nor an overlap.
    pub(super) fn band(&self, row: i64) -> (i64, i64) {
        let y = self.pad + (row as f64 * self.lh) as i64;
        (y, self.pad + ((row + 1) as f64 * self.lh) as i64 - y)
    }
}

/// Draws screen row `row`, whose cells are `cells`, answering the keys of the
/// run layouts it drew with.
pub(super) fn draw_row(
    p: &RowPaint,
    layouts: &mut ByteCache<String, layout::TextLayout>,
    cx: &mut PaintCx,
    row: i64,
    cells: &[super::frame::Cell],
) -> Vec<String> {
    let RowPaint {
        v,
        theme,
        colors,
        style,
        selection,
        highlighted,
        hovered,
        foreground,
        background,
        highlight_bg,
        selection_bg,
        selection_fg,
        cw,
        lh,
        rise,
        left,
        origin,
        ..
    } = *p;
    let screen_row = row as u16;
    let mut runs = Vec::new();
    // The layout key of each bold and italic style, made once per row.
    let mut style_keys: [Option<String>; 4] = Default::default();
    let (y, height) = p.band(row);
    let mut col = 0;
    while col < cells.len() {
        let cell = &cells[col];
        // The second column of a wide cell is drawn with the cell.
        if cell.continuation {
            col += 1;
            continue;
        }
        let selected = in_selection(selection, row, col as i64);
        let marks = &highlighted[row as usize];
        let marked = in_highlight(marks, col as i64);
        let lit = hovered_at(hovered, screen_row, col);
        let ink = match cell.fg {
            CellColor::Indexed(i) if v.options.bold_bright && cell.bold && i < 8 => {
                CellColor::Indexed(i + 8)
            }
            ref other => other.clone(),
        };
        let (mut fg, mut bg) = (
            resolve(&ink, foreground, colors),
            resolve(&cell.bg, background, colors),
        );
        if cell.inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        if marked {
            bg = highlight_bg;
        }
        // Blocks and box pieces build pictures, as chafa's symbols do, from
        // colors meant to sit close together; only letters are made legible.
        if v.options.legible && !shapes::is_shape(cell.text.as_str()) {
            fg = legible(fg, bg);
        }
        if selected {
            fg = selection_fg;
            bg = selection_bg;
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
                && in_highlight(marks, end as i64) == marked
                && hovered_at(hovered, screen_row, end) == lit
            {
                end += 1;
            }
        }
        let span = if cell.wide { 2 } else { end - col };
        let x = left + (col as f64 * cw) as i64;
        let width = left + ((col + span) as f64 * cw) as i64 - x;
        if bg != background {
            cx.fill(Rect::new(x, y, width, height), bg);
        }
        // A run's layout key is its style's key, `#`, then its text, so the
        // text is read from the key rather than copied apart from it.
        let style_key = style_keys[usize::from(cell.bold) * 2 + usize::from(cell.italic)]
            .get_or_insert_with(|| {
                let mut run = style.clone();
                run.bold = cell.bold;
                run.italic = cell.italic;
                run.key()
            });
        let prefix = style_key.len() + 1;
        let mut key = String::with_capacity(prefix + end - col);
        key.push_str(style_key);
        key.push('#');
        for c in cells[col..end].iter().filter(|c| !c.continuation) {
            key.push_str(if c.text.is_empty() {
                " "
            } else {
                c.text.as_str()
            });
        }
        // Blank cells draw no ink, and backgrounds and underlines fill
        // their cells, so a run is laid out only up to its last glyph.
        key.truncate(key.trim_end_matches(' ').len().max(prefix));
        let text = &key[prefix..];
        let shape = Rect::new(origin.x + x, origin.y + y, width, height);
        let drawn = !ascii(cell) && shapes::draw(&mut cx.canvas, text, shape, fg, bg);
        if !drawn && !text.trim().is_empty() {
            let mut run = style.clone();
            run.bold = cell.bold;
            run.italic = cell.italic;
            lay_out(layouts, &key, text, &run);
            let mut at = ((origin.x + x) as f64, (origin.y + y) as f64 + rise);
            // A glyph wider than its cells, such as an emoji a program
            // counts as one cell, is drawn smaller and centered on its
            // row, so it stays inside them.
            let mut drawn_key = None;
            if !ascii(cell)
                && let Some(drawn) = layouts.get(&key).map(|l| l.width)
                && drawn > width as f64
            {
                run.size *= width as f64 / drawn;
                let smaller = format!("{}#{}", run.key(), text);
                lay_out(layouts, &smaller, text, &run);
                if let Some(height) = layouts.get(&smaller).map(|l| l.height) {
                    at.1 += ((lh - height) / 2.0).max(0.0);
                }
                drawn_key = Some(smaller);
            }
            let shown = drawn_key.as_ref().unwrap_or(&key);
            match layouts.get(shown) {
                Some(laid) => layout::draw(&mut cx.canvas, laid, at.0, at.1, &run, fg),
                // A run too large for the cache is laid out for this paint.
                None => {
                    let laid = layout::layout_uncached(text, &run, LayoutOptions::single_line());
                    layout::draw(&mut cx.canvas, &laid, at.0, at.1, &run, fg);
                }
            }
            runs.push(key);
            runs.extend(drawn_key);
        }
        if cell.underline || lit {
            cx.fill(
                Rect::new(x, y + height - theme.px(2), width, theme.px(1).max(1)),
                fg,
            );
        }
        col = end.max(col + 1);
    }
    runs
}

/// What every row of a paint draws with besides its own cells and marks. A
/// row recorded with another context would not draw as this paint does.
#[derive(Debug, PartialEq)]
struct RowContext {
    palette: ColorPalette,
    style: TextStyle,
    /// Cell width, line height and text rise, by their bits.
    metrics: [u64; 3],
    origin: Rect,
    /// Left and top padding.
    inset: (i64, i64),
    /// One and two pixels at the theme's scale, for underlines.
    rule: (i64, i64),
    clip: Rect,
    opacity: i64,
    bold_bright: bool,
    legible: bool,
    fonts: u64,
    cols: usize,
}

/// What one row draws: the version of its cells, and the columns the
/// selection, highlights and hovered links cover in it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RowKey {
    pub(super) version: RowVersion,
    selected: Option<(i64, i64)>,
    marks: Vec<(i64, i64)>,
    lit: Vec<(u16, u16)>,
}

/// Each screen row's recording in the paint before, which a row drawn the
/// same way again copies rather than drawing anew.
#[derive(Debug, Default)]
pub(super) struct RowCache {
    context: Option<RowContext>,
    rows: Vec<Option<RecordedRow>>,
}

/// One row as a paint drew it: what it showed, where its drawing is
/// recorded, and the keys of the run layouts it drew with.
#[derive(Debug)]
struct RecordedRow {
    key: RowKey,
    span: RecordedSpan,
    runs: Vec<String>,
}

impl RowCache {
    fn begin(&mut self, context: RowContext, rows: usize) {
        if self.context.as_ref() != Some(&context) {
            self.rows.clear();
            self.context = Some(context);
        }
        self.rows.resize_with(rows, || None);
    }

    // Whether most of the rows `keys` name sat in other rows of the paint
    // before: the output scrolled.
    fn scrolled(&self, keys: &[Option<RowKey>]) -> bool {
        let at: std::collections::HashMap<RowVersion, usize> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| row.as_ref().map(|row| (row.key.version, i)))
            .collect();
        let moved = keys
            .iter()
            .enumerate()
            .filter(|(i, key)| {
                key.as_ref()
                    .and_then(|key| at.get(&key.version))
                    .is_some_and(|j| j != i)
            })
            .count();
        moved * 2 > keys.len()
    }

    fn recorded(&self, row: usize, key: &RowKey) -> Option<&RecordedRow> {
        self.rows
            .get(row)?
            .as_ref()
            .filter(|recorded| recorded.key == *key)
    }

    fn store(&mut self, row: usize, recorded: Option<RecordedRow>) {
        if let Some(slot) = self.rows.get_mut(row) {
            *slot = recorded;
        }
    }

    // A replayed row's drawing now sits at `span` in this paint's recording.
    fn replayed(&mut self, row: usize, span: RecordedSpan) {
        if let Some(Some(recorded)) = self.rows.get_mut(row) {
            recorded.span = span;
        }
    }
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
    let (rows, cols) = grid(v, &theme, node.size());
    let wanted = palette(v, &theme);
    // Only a window with keyboard focus gives it to the terminal it focuses.
    let focused = cx.focus == v.id && cx.active;
    let (cw, lh, style) = metrics(v, &theme);
    // Programs place images in whole pixels per cell.
    let cell = (pixels_of(cw), pixels_of(lh));
    let captured = with_entry(v.terminal, |entry| {
        // The screen follows the view's size; the program hears of it.
        super::follow_view(entry, rows as u16, cols as u16, cell);
        // A paint follows every change of focus, so the program hears of
        // each one here.
        super::focus(entry, focused);
        entry.scroll_on_output = v.options.scroll_on_output;
        entry.scroll_on_keystroke = v.options.scroll_on_keystroke;
        // A bell rung since the last paint flashes the screen briefly.
        let bells = entry.engine.notifications().bells;
        if bells != entry.bells_flashed {
            entry.bells_flashed = bells;
            if v.options.flash_bell {
                entry.flash_until = cx.now + FLASH_MS;
            }
        }
        entry.engine.set_palette(&wanted);
        let colors = entry.engine.palette();
        let top = entry.engine.history() as i64 - entry.engine.scrollback() as i64;
        let highlighted = highlighted_rows(&entry.highlights, top, rows, cols);
        let frame = entry.engine.frame()?;
        entry.links = frame.links().to_vec();
        if entry
            .hovered
            .as_ref()
            .is_some_and(|uri| !entry.links.iter().any(|l| &l.uri == uri))
        {
            entry.hovered = None;
        }
        Ok::<_, String>((
            frame,
            entry.selection,
            entry.hovered.clone(),
            colors,
            highlighted,
            (
                entry.scrolled as i64,
                entry.engine.history() as i64,
                entry.flash_until,
            ),
        ))
    });
    let alpha = (v.options.opacity.clamp(0.0, 1.0) * 255.0).round() as i64;
    let Some(Ok((
        frame,
        selection,
        hovered,
        colors,
        highlighted,
        (scrolled, history, flash_until),
    ))) = captured
    else {
        cx.fill(r, shown(wanted.background).with_alpha(alpha));
        return;
    };
    let background = shown(colors.background);
    let foreground = shown(colors.foreground);
    let (selection_bg, selection_fg) = (shown(colors.selection_bg), shown(colors.selection_fg));
    // Highlights read as a weaker selection, so the selected match stands out.
    let highlight_bg = selection_bg.mix(background, 0.55);
    let hovered: Vec<&Link> = frame
        .links()
        .iter()
        .filter(|l| hovered.as_ref() == Some(&l.uri))
        .collect();
    let pad = pad_of(v, &theme);
    let left = left_of(v, &theme);
    let rise = text_offset(&style, lh);
    let origin = cx.abs(r);
    let generation = crate::text::fonts::generation();
    let mut layer =
        with_entry(v.terminal, |entry| std::mem::take(&mut entry.layer)).unwrap_or_default();
    // The layer holds opaque text alone: a translucent or faded terminal, or
    // one showing images, paints its rows into the window's recording.
    let layer_context = (alpha == 255
        && cx.canvas.opacity == 255
        && cx.canvas.is_recording()
        && frame.images().is_empty())
    .then(|| LayerContext {
        palette: colors.clone(),
        style: style.clone(),
        metrics: [cw.to_bits(), lh.to_bits(), rise.to_bits()],
        inset: (left, pad),
        rule: (theme.px(1), theme.px(2)),
        bold_bright: v.options.bold_bright,
        legible: v.options.legible,
        fonts: generation,
        size: (r.width, r.height),
        rows: frame.cells().len(),
        background,
    })
    .filter(|context| layer.usable(context));
    if layer_context.is_none() {
        cx.fill(r, background.with_alpha(alpha));
    }
    let mut pictures =
        with_entry(v.terminal, |entry| std::mem::take(&mut entry.pictures)).unwrap_or_default();
    let mut used = images::Used::default();
    let image_grid = images::Grid {
        x: (origin.x + left) as f64,
        y: (origin.y + pad) as f64,
        cell_width: cw,
        cell_height: lh,
        placed: cell,
    };
    let below = images::paint(
        &mut cx.canvas,
        frame.images(),
        true,
        image_grid,
        &mut pictures,
        &mut used,
        cx.now,
    );
    // Run layouts are kept per terminal, sized to its screen: output brings
    // rows that never come back, which the shared layout cache would hold to
    // its whole budget.
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
    let paint = RowPaint {
        v,
        theme: &theme,
        colors: &colors,
        style: &style,
        selection,
        highlighted: &highlighted,
        hovered: &hovered,
        foreground,
        background,
        highlight_bg,
        selection_bg,
        selection_fg,
        cw,
        lh,
        rise,
        pad,
        left,
        origin,
    };
    let keys: Vec<Option<RowKey>> = frame
        .cells()
        .iter()
        .enumerate()
        .map(|(index, cells)| {
            let screen_row = index as u16;
            frame.version(screen_row).map(|version| RowKey {
                version,
                selected: selected_columns(selection, index as i64, cells.len() as i64),
                marks: highlighted[index].clone(),
                lit: hovered
                    .iter()
                    .filter(|l| l.row == screen_row)
                    .map(|l| (l.start, l.end))
                    .collect(),
            })
        })
        .collect();
    let mut recorded =
        with_entry(v.terminal, |entry| std::mem::take(&mut entry.rows)).unwrap_or_default();
    // The layer costs a screen of pixels, so a terminal keeps one only while
    // its output scrolls.
    if layer.quiet(cx.now) {
        layer.release();
    }
    let layered = layer_context
        .as_ref()
        .filter(|_| layer.holds() || recorded.scrolled(&keys))
        .is_some_and(|context| {
            super::layer::paint(&mut layer, context, &paint, &keys, &frame, &mut layouts, cx)
        });
    if layered {
        // The window's recording holds no rows to copy from the next paint.
        recorded = RowCache::default();
    } else {
        if layer_context.is_some() {
            cx.fill(r, background.with_alpha(alpha));
        }
        recorded.begin(
            RowContext {
                palette: colors.clone(),
                style: style.clone(),
                metrics: [cw.to_bits(), lh.to_bits(), rise.to_bits()],
                origin,
                inset: (left, pad),
                rule: (theme.px(1), theme.px(2)),
                clip: cx.canvas.clip,
                opacity: cx.canvas.opacity,
                bold_bright: v.options.bold_bright,
                legible: v.options.legible,
                fonts: generation,
                cols: usize::from(frame.cols()),
            },
            frame.cells().len(),
        );
        for ((index, cells), key) in frame.cells().iter().enumerate().zip(keys) {
            let row = index as i64;
            if let Some(row_recording) = key.as_ref().and_then(|key| recorded.recorded(index, key))
                && let Some(span) = cx.canvas.replay_span(row_recording.span)
            {
                // A copied row is still on the screen, so its runs stay cached.
                for run in &row_recording.runs {
                    layouts.get(run);
                }
                recorded.replayed(index, span);
                continue;
            }
            let mark = cx.canvas.recorded_mark();
            let runs = draw_row(&paint, &mut layouts, cx, row, cells);
            recorded.store(
                index,
                key.zip(cx.canvas.recorded_span(mark))
                    .map(|(key, span)| RecordedRow { key, span, runs }),
            );
        }
    }
    // A row the screen no longer shows only comes back by scrolling back,
    // which lays it out again, so the cache keeps the runs this paint drew.
    layouts.evict_unused_since(painted_from);
    let above = images::paint(
        &mut cx.canvas,
        frame.images(),
        false,
        image_grid,
        &mut pictures,
        &mut used,
        cx.now,
    );
    pictures.keep(&used);
    // A held layer paints once more after its output goes still, so it lets
    // its pixels go whether or not anything else repaints the terminal.
    let release_due = layer.release_due();
    with_entry(v.terminal, |entry| {
        entry.layouts = layouts;
        entry.rows = recorded;
        entry.layer = layer;
        entry.pictures = pictures;
        entry.animation_due = [below, above, release_due].into_iter().flatten().min();
    });
    // The cursor shows focus, filled while the terminal has it and hollow
    // otherwise, so the terminal draws no focus ring around its screen.
    paint_cursor(
        &frame,
        focused,
        &v.options,
        shown(colors.cursor_bg),
        cx,
        (cw, lh, left, pad),
    );
    paint_mode(v, &frame, &style, cx, r, (cw, lh, left, pad));
    if let Some(bar) = scrollbar(v, &theme, r, rows, (scrolled, history)) {
        cx.fill_round(bar.track, theme.px(3), foreground.with_alpha(28));
        cx.fill_round(bar.thumb, theme.px(3), foreground.with_alpha(110));
    }
    if cx.now < flash_until {
        cx.fill(r, foreground.with_alpha(40));
        with_entry(v.terminal, |entry| {
            entry.animation_due = Some(
                entry
                    .animation_due
                    .map_or(flash_until, |d| d.min(flash_until)),
            )
        });
    }
}

// The bell's flash, in milliseconds.
const FLASH_MS: i64 = 150;

// A scrollbar's track and thumb, local to the view.
struct Scrollbar {
    track: Rect,
    thumb: Rect,
}

// Where the scrollbar of `v` in `area` lies, showing `rows` of a screen
// `scrolled` back into `history` rows; None while it does not show.
fn scrollbar(
    v: &View,
    theme: &Theme,
    area: Rect,
    rows: i64,
    (scrolled, history): (i64, i64),
) -> Option<Scrollbar> {
    let shows = match v.options.scrollbar.as_str() {
        "always" => true,
        "scrolling" => scrolled > 0,
        _ => false,
    };
    if !shows {
        return None;
    }
    let width = theme.px(SCROLLBAR);
    let inset = theme.px(2);
    let edge = if v.options.scrollbar_side == "left" {
        0
    } else {
        area.width - width
    };
    let track = Rect::new(
        edge + inset,
        inset,
        width - 2 * inset,
        area.height - 2 * inset,
    );
    let total = (history + rows).max(1);
    let length = (track.height * rows / total)
        .max(theme.px(24))
        .min(track.height);
    // The thumb sits at the bottom on the live screen and rises as the view
    // goes back.
    let travel = track.height - length;
    let y = track.y + travel - travel * scrolled / history.max(1);
    Some(Scrollbar {
        track,
        thumb: Rect::new(track.x, y, track.width, length),
    })
}

// The whole pixels a cell `length` long spans.
fn pixels_of(length: f64) -> u16 {
    length.round().clamp(1.0, f64::from(u16::MAX)) as u16
}

// Lays out `text` in `run` under `key` unless the cache holds it.
fn lay_out(
    layouts: &mut ByteCache<String, layout::TextLayout>,
    key: &String,
    text: &str,
    run: &TextStyle,
) {
    if layouts.get(key).is_none() {
        let laid = layout::layout_uncached(text, run, LayoutOptions::single_line());
        let bytes = key.len() + laid.owned_bytes();
        layouts.insert(key.clone(), laid, bytes);
    }
}

fn paint_cursor(
    frame: &Frame,
    focused: bool,
    options: &ViewOptions,
    color: Color,
    cx: &mut PaintCx,
    (cw, lh, left, pad): (f64, f64, i64, i64),
) {
    if frame.hide_cursor() {
        return;
    }
    let (row, col) = frame.cursor();
    let x = left + (f64::from(col) * cw) as i64;
    let y = pad + (f64::from(row) * lh) as i64;
    let cell = Rect::new(x, y, cw.ceil() as i64, lh as i64);
    let line = cx.theme.px(2).max(1);
    let shape = match options.cursor.as_str() {
        "bar" => Rect::new(x, y, line, cell.height),
        "underline" => Rect::new(x, y + cell.height - line, cell.width, line),
        _ => cell,
    };
    if !focused {
        if shape == cell {
            cx.stroke_round(cell, 0, 1, color);
        } else {
            cx.fill(shape, color.with_alpha(160));
        }
    } else if cx.caret_on || !options.blink {
        cx.fill(shape, color.with_alpha(160));
    }
}

/// The key a terminal encodes, or None for a key it leaves to the window.
// A key while copy mode or the hints are on goes to the mode; a Ctrl, Alt
// or Command chord goes on to the window's commands. Nothing reaches the
// program. Answers whether a mode is on.
fn mode_key(v: &View, key: Key, mods: windowing::Mods, rows: i64, cx: &mut EventCx) -> bool {
    let Some(copying) = with_entry(v.terminal, |e| match &e.mode {
        Some(super::modes::Mode::Copy(_)) => Some(true),
        Some(super::modes::Mode::Hints(_)) => Some(false),
        None => None,
    })
    .flatten() else {
        return false;
    };
    if copying && mods.ctrl && mods.shift && key == Key::C {
        yank(v);
        redraw(cx);
        return true;
    }
    if mods.ctrl || mods.alt || mods.gui {
        return true;
    }
    if copying {
        copy_mode_key(v, key, mods.shift, rows);
    } else {
        hint_key(v, key, mods.shift, cx);
    }
    redraw(cx);
    true
}

// The character a key types under `shift`, for a mode's bindings.
fn mode_char(key: Key, shift: bool) -> Option<char> {
    match key {
        Key::Char(c) => Some(if shift { c.to_ascii_uppercase() } else { c }),
        Key::Num4 if shift => Some('$'),
        _ => printable(key, shift),
    }
}

// Moves the copy-mode cursor, selects, or copies.
fn copy_mode_key(v: &View, key: Key, shift: bool, rows: i64) {
    use super::modes::{self, Mode};
    let c = mode_char(key, shift);
    let yanking = matches!(key, Key::Return | Key::KpEnter) || c == Some('y');
    if yanking {
        return yank(v);
    }
    with_entry(v.terminal, |e| {
        if key == Key::Escape || c == Some('q') {
            return super::leave(e);
        }
        let Some(Mode::Copy(m)) = &mut e.mode else {
            return;
        };
        let (_, cols) = e.engine.size();
        let first = e.engine.stable_first();
        let live = e.engine.stable_live();
        let rows = rows as isize;
        let last = live + rows - 1;
        let (mut row, mut col) = m.cursor;
        let text = |row: isize| e.engine.stable_row_text(row);
        match (key, c) {
            (Key::Left, _) | (_, Some('h')) => col = col.saturating_sub(1),
            (Key::Right, _) | (_, Some('l')) => col = (col + 1).min(cols - 1),
            (Key::Up, _) | (_, Some('k')) => row -= 1,
            (Key::Down, _) | (_, Some('j')) => row += 1,
            (Key::PageUp, _) => row -= rows,
            (Key::PageDown, _) => row += rows,
            (Key::Home, _) | (_, Some('0')) => col = 0,
            (Key::End, _) | (_, Some('$')) => col = modes::line_end(&text(row)),
            (_, Some('g')) => (row, col) = (first, 0),
            (_, Some('G')) => {
                let (cursor_row, cursor_col) = e.engine.cursor();
                (row, col) = (e.engine.stable_top() + cursor_row as isize, cursor_col);
            }
            (_, Some('w')) => match modes::next_word(&text(row), col) {
                Some(next) => col = next,
                None if row < last => (row, col) = (row + 1, 0),
                None => {}
            },
            (_, Some('b')) => match modes::previous_word(&text(row), col) {
                Some(previous) => col = previous,
                None if row > first => {
                    row -= 1;
                    col = modes::previous_word(&text(row), cols).unwrap_or(0);
                }
                None => {}
            },
            (Key::Space, _) | (_, Some('v')) => {
                m.anchor = match m.anchor {
                    Some(_) => None,
                    None => Some((row, col)),
                };
            }
            _ => {}
        }
        let row = row.clamp(first, last.max(first));
        m.cursor = (row, col);
        let anchor = m.anchor;
        // The view follows the cursor.
        let top = e.engine.stable_top();
        let target = if row < top {
            Some(live - row)
        } else if row > top + rows - 1 {
            Some(live - (row - rows + 1))
        } else {
            None
        };
        if let Some(back) = target {
            e.engine.scroll(back.max(0) as usize);
            e.scrolled = e.engine.scrollback();
        }
        // The selection shows where it crosses the view.
        let top = e.engine.stable_top();
        e.selection = anchor.map(|anchor| {
            let (a, b) = if anchor <= (row, col) {
                (anchor, (row, col))
            } else {
                ((row, col), anchor)
            };
            let shown = |(r, c): modes::Place, end: bool| {
                let view = r - top;
                if view < 0 {
                    Cell { row: 0, col: 0 }
                } else if view >= rows {
                    Cell {
                        row: rows as i64 - 1,
                        col: i64::from(cols),
                    }
                } else {
                    Cell {
                        row: view as i64,
                        col: i64::from(c) + i64::from(end),
                    }
                }
            };
            (shown(a, false), shown(b, true))
        });
        e.revision += 1;
    });
}

// Copies the copy-mode selection, or the cursor's line without one, and
// ends copy mode.
fn yank(v: &View) {
    use super::modes::Mode;
    let text = with_entry(v.terminal, |e| {
        let Some(Mode::Copy(m)) = &e.mode else {
            return String::new();
        };
        let text = match m.anchor {
            Some(anchor) => {
                let (a, b) = if anchor <= m.cursor {
                    (anchor, m.cursor)
                } else {
                    (m.cursor, anchor)
                };
                e.engine.copy_stable(a, (b.0, b.1 + 1))
            }
            None => e.engine.stable_row_text(m.cursor.0).trim_end().to_string(),
        };
        super::leave(e);
        text
    })
    .unwrap_or_default();
    if !text.is_empty() {
        crate::services::clipboard::set_text(&text);
    }
}

// Narrows the hints by a typed key, and opens or copies the link whose label
// it completes.
fn hint_key(v: &View, key: Key, shift: bool, cx: &mut EventCx) {
    use super::modes::{self, Mode, Pick};
    let chosen = with_entry(v.terminal, |e| {
        if key == Key::Escape {
            super::leave(e);
            return None;
        }
        let Some(Mode::Hints(h)) = &mut e.mode else {
            return None;
        };
        if key == Key::Backspace {
            h.typed.pop();
            e.revision += 1;
            return None;
        }
        let c = mode_char(key, shift)?.to_ascii_lowercase();
        let typed = format!("{}{c}", h.typed);
        let chosen = match modes::pick(h, &typed) {
            Pick::Chosen(link) => Some((h.copy, link.clone())),
            Pick::Partial => {
                h.typed = typed;
                None
            }
            Pick::Unknown => None,
        };
        if chosen.is_some() {
            super::leave(e);
        }
        e.revision += 1;
        chosen
    })
    .flatten();
    let Some((copy, link)) = chosen else {
        return;
    };
    if copy {
        crate::services::clipboard::set_text(&link.uri);
        return;
    }
    let event = if link.path {
        match locate(v.terminal, &link.uri) {
            Some(place) => TerminalEvent::PathActivated(place.path, place.line, place.col),
            None => return,
        }
    } else {
        TerminalEvent::LinkActivated(link.uri.to_string())
    };
    cx.emit(&UiMsg::Terminal(v.terminal, event));
}

// Draws what a mode shows over the screen: the copy-mode cursor and a frame
// round the view, or each link's label.
fn paint_mode(
    v: &View,
    frame: &Frame,
    style: &TextStyle,
    cx: &mut PaintCx,
    area: Rect,
    (cw, lh, left, pad): (f64, f64, i64, i64),
) {
    use super::modes::Mode;
    enum Shown {
        Copy(Option<(i64, u16)>),
        Hints(Vec<(String, u16, u16)>),
    }
    let shown = with_entry(v.terminal, |e| match &e.mode {
        Some(Mode::Copy(m)) => {
            let row = m.cursor.0 - e.engine.stable_top();
            Some(Shown::Copy(
                (0..i64::from(frame.rows()))
                    .contains(&(row as i64))
                    .then_some((row as i64, m.cursor.1)),
            ))
        }
        Some(Mode::Hints(h)) => Some(Shown::Hints(
            h.labels
                .iter()
                .filter(|(label, _)| label.starts_with(h.typed.as_str()))
                .map(|(label, link)| (label[h.typed.len()..].to_string(), link.row, link.start))
                .collect(),
        )),
        None => None,
    })
    .flatten();
    let Some(shown) = shown else {
        return;
    };
    let accent = cx.theme.accent;
    let line = cx.theme.px(2).max(1);
    let cell = |row: i64, col: u16, width: usize| {
        Rect::new(
            left + (f64::from(col) * cw) as i64,
            pad + (row as f64 * lh) as i64,
            (cw * width.max(1) as f64).ceil() as i64,
            lh as i64,
        )
    };
    match shown {
        Shown::Copy(cursor) => {
            cx.stroke_round(area, 0, line, accent);
            if let Some((row, col)) = cursor {
                let at = cell(row, col, 1);
                cx.fill(at, accent.with_alpha(110));
                cx.stroke_round(at, 0, 1, accent);
            }
        }
        Shown::Hints(labels) => {
            let run = style.clone();
            let origin = cx.abs(area);
            let ink = cx.theme.accent_text;
            for (label, row, col) in labels {
                let at = cell(i64::from(row), col, label.chars().count());
                cx.fill(at, accent);
                let laid = layout::layout_uncached(&label, &run, LayoutOptions::single_line());
                let rise = text_offset(style, lh);
                layout::draw(
                    &mut cx.canvas,
                    &laid,
                    (origin.x + at.x) as f64,
                    (origin.y + at.y) as f64 + rise,
                    &run,
                    ink,
                );
            }
        }
    }
}

fn kitty_flags(v: &View) -> Option<wezterm_term::input_types::KittyKeyboardFlags> {
    with_entry(v.terminal, |e| e.engine.kitty_keyboard()).flatten()
}

#[derive(Clone, Copy, PartialEq)]
enum KeyState {
    Down,
    Repeat,
    Up,
}

// Sends a key under kitty's keyboard protocol. Keys that type text without
// Ctrl or Alt keep arriving as text, as the protocol has it unless the
// program asked for every key as an escape code, so input methods and
// keyboard layouts go on working.
fn kitty_key(
    v: &View,
    key: Key,
    mods: windowing::Mods,
    state: KeyState,
    flags: wezterm_term::input_types::KittyKeyboardFlags,
    cx: &mut EventCx,
) {
    use wezterm_term::input_types::{KeyEvent, KeyboardLedStatus, KittyKeyboardFlags, Modifiers};
    let every_key = flags.contains(KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);
    let text_key = printable(key, false).is_some() || matches!(key, Key::Char(_));
    if text_key && !mods.ctrl && !mods.alt && !every_key {
        return;
    }
    let Some(code) = kitty_code(key) else {
        return;
    };
    let mut modifiers = Modifiers::NONE;
    for (on, bit) in [
        (mods.shift, Modifiers::SHIFT),
        (mods.alt, Modifiers::ALT),
        (mods.ctrl, Modifiers::CTRL),
        (mods.gui, Modifiers::SUPER),
    ] {
        if on {
            modifiers |= bit;
        }
    }
    let mut leds = KeyboardLedStatus::empty();
    if mods.caps_lock {
        leds |= KeyboardLedStatus::CAPS_LOCK;
    }
    if mods.num_lock {
        leds |= KeyboardLedStatus::NUM_LOCK;
    }
    let event = KeyEvent {
        key: code,
        modifiers,
        leds,
        repeat_count: if state == KeyState::Repeat { 2 } else { 1 },
        key_is_down: state != KeyState::Up,
        raw: None,
        #[cfg(windows)]
        win32_uni_char: None,
    };
    let bytes = event.encode_kitty(flags);
    if bytes.is_empty() {
        return;
    }
    if let Ok(changed) = super::typed(v.terminal, bytes.as_bytes()) {
        // The key's text, if the platform also sends it, was in the code.
        if text_key && state != KeyState::Up {
            with_entry(v.terminal, |e| e.suppress_text = true);
        }
        took_typing(v, changed, cx);
    }
}

fn kitty_code(key: Key) -> Option<wezterm_term::input_types::KeyCode> {
    use wezterm_term::input_types::KeyCode as K;
    if let Some(c) = printable(key, false) {
        return Some(K::Char(c));
    }
    Some(match key {
        Key::Char(c) => K::Char(c),
        Key::Return | Key::KpEnter => K::Char('\r'),
        Key::Tab => K::Char('\t'),
        // The protocol's Backspace and Delete are these characters.
        Key::Backspace => K::Char('\u{8}'),
        Key::Escape => K::Char('\x1b'),
        Key::Delete => K::Char('\u{7f}'),
        Key::Up => K::UpArrow,
        Key::Down => K::DownArrow,
        Key::Left => K::LeftArrow,
        Key::Right => K::RightArrow,
        Key::Home => K::Home,
        Key::End => K::End,
        Key::Insert => K::Insert,
        Key::PageUp => K::PageUp,
        Key::PageDown => K::PageDown,
        Key::F1 => K::Function(1),
        Key::F2 => K::Function(2),
        Key::F3 => K::Function(3),
        Key::F4 => K::Function(4),
        Key::F5 => K::Function(5),
        Key::F6 => K::Function(6),
        Key::F7 => K::Function(7),
        Key::F8 => K::Function(8),
        Key::F9 => K::Function(9),
        Key::F10 => K::Function(10),
        Key::F11 => K::Function(11),
        Key::F12 => K::Function(12),
        Key::LeftShift => K::LeftShift,
        Key::RightShift => K::RightShift,
        Key::LeftCtrl => K::LeftControl,
        Key::RightCtrl => K::RightControl,
        Key::LeftAlt => K::LeftAlt,
        Key::RightAlt => K::RightAlt,
        Key::LeftGui => K::LeftWindows,
        Key::RightGui => K::RightWindows,
        Key::CapsLock => K::CapsLock,
        _ => return None,
    })
}

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
// letters, digits, PageUp and PageDown, and any other a window command
// binds.
fn application_chord(key: Key, mods: windowing::Mods, cx: &EventCx) -> bool {
    printable(key, false).is_some_and(|c| c != ' ')
        || key == Key::PageUp
        || key == Key::PageDown
        || bound_chord(key, mods, cx)
}

// Whether an enabled command of the window is bound to this stroke.
fn bound_chord(key: Key, mods: windowing::Mods, cx: &EventCx) -> bool {
    let stroke = [(key, mods)];
    cx.registry
        .iter()
        .any(|c| c.enabled && c.shortcuts.iter().any(|s| s.progress(&stroke).0))
}

fn ime_area(v: &View, cx: &mut EventCx) {
    let cursor = with_entry(v.terminal, |e| e.engine.cursor());
    if let Some((row, col)) = cursor {
        let (cw, lh, _) = metrics(v, &cx.theme);
        let pad = pad_of(v, &cx.theme);
        let x = left_of(v, &cx.theme) + (f64::from(col) * cw) as i64;
        let y = pad + (f64::from(row) * lh) as i64;
        cx.ime = Some((
            cx.to_window(Rect::new(x, y, cw.ceil() as i64, lh as i64)),
            0,
        ));
    }
}

/// Whether pointer events go to the program: it asked for the mouse, the
/// view shows the live screen, and Shift does not keep them for selecting.
fn reporting(v: &View, mods: windowing::Mods) -> bool {
    !mods.shift
        && with_entry(v.terminal, |e| e.engine.mouse_grabbed() && e.scrolled == 0).unwrap_or(false)
}

fn modifiers(mods: windowing::Mods) -> KeyModifiers {
    let mut out = KeyModifiers::NONE;
    if mods.shift {
        out |= KeyModifiers::SHIFT;
    }
    if mods.alt {
        out |= KeyModifiers::ALT;
    }
    if mods.ctrl {
        out |= KeyModifiers::CTRL;
    }
    out
}

fn reported_button(button: MouseButton) -> Option<wezterm_term::MouseButton> {
    match button {
        MouseButton::Left => Some(wezterm_term::MouseButton::Left),
        MouseButton::Middle => Some(wezterm_term::MouseButton::Middle),
        MouseButton::Right => Some(wezterm_term::MouseButton::Right),
        _ => None,
    }
}

/// Reports a pointer event at a local point to the program, at the screen
/// cell under it.
fn report_pointer(
    v: &View,
    theme: &Theme,
    (x, y): (i64, i64),
    kind: MouseEventKind,
    button: wezterm_term::MouseButton,
    mods: windowing::Mods,
) {
    let (row, col) = cell_under(v, theme, x.max(0), y.max(0)).unwrap_or_default();
    let event = MouseEvent {
        kind,
        x: usize::from(col),
        y: i64::from(row),
        x_pixel_offset: 0,
        y_pixel_offset: 0,
        button,
        modifiers: modifiers(mods),
    };
    with_entry(v.terminal, |entry| super::mouse(entry, event));
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
            if *button == MouseButton::Left && drag_scrollbar(v, node, *y, cx, Some((*x, *y))) {
                return redraw(cx);
            }
            let link = link_under(v, &cx.theme, *x, *y);
            point(v, link.as_ref(), cx);
            if let (MouseButton::Left, Some(target)) = (button, &link)
                && opens_link(*mods)
            {
                let event = match target {
                    Target::Uri(uri) => TerminalEvent::LinkActivated(uri.to_string()),
                    Target::Path(_, place) => {
                        TerminalEvent::PathActivated(place.path.clone(), place.line, place.col)
                    }
                };
                cx.emit(&UiMsg::Terminal(v.terminal, event));
                return cx.consume();
            }
            if reporting(v, *mods)
                && let Some(reported) = reported_button(*button)
            {
                report_pointer(
                    v,
                    &cx.theme,
                    (*x, *y),
                    MouseEventKind::Press,
                    reported,
                    *mods,
                );
                cx.capture_pointer(&v.id);
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
                    selected(v);
                }
                MouseButton::Left if *clicks == 2 => {
                    if let Some(word) = word_at(v, at) {
                        let _ = super::select(v.terminal, Some(word));
                        selected(v);
                    }
                }
                MouseButton::Left => {
                    let _ = super::select(v.terminal, Some((at, at)));
                    cx.capture_pointer(&v.id);
                }
                MouseButton::Middle => {
                    paste(v, &crate::services::clipboard::primary_text(), cx);
                }
                _ => {}
            }
            redraw(cx);
        }
        Event::PointerMove { x, y, mods, .. } if reporting(v, *mods) => {
            let none = wezterm_term::MouseButton::None;
            report_pointer(v, &cx.theme, (*x, *y), MouseEventKind::Move, none, *mods);
            hover(v, link_under(v, &cx.theme, *x, *y), cx);
        }
        Event::PointerMove {
            y, pressed: true, ..
        } if with_entry(v.terminal, |e| e.scrollbar_drag).unwrap_or(false) => {
            drag_scrollbar(v, node, *y, cx, None);
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
        Event::PointerUp { x, y, button, mods } if reporting(v, *mods) => {
            cx.release_pointer();
            if let Some(reported) = reported_button(*button) {
                report_pointer(
                    v,
                    &cx.theme,
                    (*x, *y),
                    MouseEventKind::Release,
                    reported,
                    *mods,
                );
            }
            cx.consume();
        }
        Event::PointerUp { .. } => {
            cx.release_pointer();
            if with_entry(v.terminal, |e| std::mem::take(&mut e.scrollbar_drag)) == Some(true) {
                return redraw(cx);
            }
            let collapsed = with_entry(v.terminal, |e| e.selection.is_some_and(|(a, b)| a == b));
            if collapsed == Some(true) {
                let _ = super::select(v.terminal, None);
            } else {
                selected(v);
            }
            redraw(cx);
        }
        Event::Drop { paths, text, .. } if !paths.is_empty() || !text.is_empty() => {
            cx.focus_on(&v.id);
            let dropped = if paths.is_empty() {
                text.clone()
            } else {
                paths
                    .iter()
                    .map(|p| shell_quoted(p))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            paste(v, &dropped, cx);
            redraw(cx);
        }
        Event::Scroll { x, y, dy, mods, .. } => {
            let alternate = with_entry(v.terminal, |e| e.engine.alternate_screen());
            // A program that asked for the mouse hears the wheel; one on the
            // alternate screen that did not hears it as cursor keys.
            let program = reporting(v, *mods)
                || (alternate == Some(true)
                    && !mods.shift
                    && with_entry(v.terminal, |e| e.scrolled == 0).unwrap_or(false));
            if program && *dy != 0.0 {
                let wheel = if *dy > 0.0 {
                    wezterm_term::MouseButton::WheelUp(1)
                } else {
                    wezterm_term::MouseButton::WheelDown(1)
                };
                report_pointer(v, &cx.theme, (*x, *y), MouseEventKind::Press, wheel, *mods);
                cx.consume();
            } else if alternate == Some(false) && *dy != 0.0 {
                let rows = if *dy > 0.0 { WHEEL_ROWS } else { -WHEEL_ROWS };
                let _ = super::scroll(v.terminal, rows);
                redraw(cx);
            }
        }
        Event::KeyDown {
            key, mods, repeat, ..
        } if focused => {
            // A key's text arrives between its press and its release, so a
            // suppression set for an earlier key never reaches this one.
            with_entry(v.terminal, |e| e.suppress_text = false);
            key_down(v, *key, *mods, *repeat, rows, cx)
        }
        Event::KeyUp { key, mods, .. } if focused => {
            with_entry(v.terminal, |e| e.suppress_text = false);
            if let Some(flags) = kitty_flags(v) {
                kitty_key(v, *key, *mods, KeyState::Up, flags, cx);
            }
        }
        Event::Text(_) if focused && with_entry(v.terminal, |e| e.mode.is_some()) == Some(true) => {
            cx.consume();
        }
        Event::Text(text) if focused => {
            let suppressed = with_entry(v.terminal, |e| std::mem::take(&mut e.suppress_text));
            if suppressed != Some(true)
                && let Ok(changed) = super::typed(v.terminal, text.as_bytes())
            {
                took_typing(v, changed, cx);
            }
        }
        Event::Command(name) if focused => {
            let used = match name.as_str() {
                "edit.copy" => copy_selection(v),
                "edit.paste" => paste(v, &crate::services::clipboard::text(), cx),
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
            cx.emit(&UiMsg::Terminal(v.terminal, TerminalEvent::Focused));
        }
        Event::FocusOut => cx.redraw = true,
        _ => {}
    }
}

// The selection just made goes to the primary selection, and to the
// clipboard too when the view copies on select.
fn selected(v: &View) {
    if let Ok(text) = super::copy(v.terminal)
        && !text.is_empty()
    {
        crate::services::clipboard::set_primary_text(&text);
        if v.options.copy_on_select {
            crate::services::clipboard::set_text(&text);
        }
    }
}

// Pastes `text`, or holds it for the application to confirm when the view
// asks that of pastes that run commands or send control characters.
fn paste(v: &View, text: &str, cx: &mut EventCx) -> bool {
    if text.is_empty() {
        return false;
    }
    if v.options.confirm_paste && risky_paste(text) {
        cx.emit(&UiMsg::Terminal(
            v.terminal,
            TerminalEvent::PasteHeld(text.to_string()),
        ));
        return true;
    }
    super::paste_from_view(v.terminal, text)
}

/// Whether pasting `text` could run a command or send control characters:
/// it holds a line break, other than one that ends it, or a control
/// character other than a tab.
pub fn risky_paste(text: &str) -> bool {
    let body = text.strip_suffix('\n').unwrap_or(text);
    let body = body.strip_suffix('\r').unwrap_or(body);
    body.chars().any(|c| c.is_control() && c != '\t')
}

// `path` quoted for a POSIX shell, unless it needs no quoting.
fn shell_quoted(path: &str) -> String {
    let plain = !path.is_empty()
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,~".contains(c));
    if plain {
        path.to_string()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

// The word around cell `at` of the screen: letters, digits and the view's
// word characters, or None on a blank.
fn word_at(v: &View, at: Cell) -> Option<(Cell, Cell)> {
    let frame = with_entry(v.terminal, |e| e.engine.frame()).and_then(Result::ok)?;
    let cells = frame.cells().get(usize::try_from(at.row).ok()?)?;
    let word = |col: usize| {
        cells.get(col).is_some_and(|c| {
            c.continuation
                || c.text
                    .as_str()
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_alphanumeric() || v.options.word_chars.contains(ch))
        })
    };
    let col = usize::try_from(at.col)
        .ok()?
        .min(cells.len().saturating_sub(1));
    if !word(col) {
        return None;
    }
    let mut start = col;
    while start > 0 && word(start - 1) {
        start -= 1;
    }
    let mut end = col + 1;
    while end < cells.len() && word(end) {
        end += 1;
    }
    Some((
        Cell {
            row: at.row,
            col: start as i64,
        },
        Cell {
            row: at.row,
            col: end as i64,
        },
    ))
}

// Scrolls to where pointer row `y` falls on the scrollbar. A press (`at`)
// starts a drag when it lands on a shown scrollbar and answers whether it
// did; a move continues one.
fn drag_scrollbar(v: &View, node: &Node, y: i64, cx: &mut EventCx, at: Option<(i64, i64)>) -> bool {
    let theme = cx.theme.clone();
    let area = Rect::from_size(node.size());
    let (rows, _) = grid(v, &theme, node.size());
    let Some((scrolled, history)) = with_entry(v.terminal, |e| {
        (e.scrolled as i64, e.engine.history() as i64)
    }) else {
        return false;
    };
    let Some(bar) = scrollbar(v, &theme, area, rows, (scrolled, history)) else {
        return false;
    };
    if let Some((x, _)) = at {
        if x < bar.track.x - theme.px(2) {
            return false;
        }
        with_entry(v.terminal, |e| e.scrollbar_drag = true);
        cx.capture_pointer(&v.id);
    }
    let travel = (bar.track.height - bar.thumb.height).max(1);
    let from_top = (y - bar.track.y - bar.thumb.height / 2).clamp(0, travel);
    let target = history - history * from_top / travel;
    let _ = super::scroll(v.terminal, target - scrolled);
    true
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

fn key_down(v: &View, key: Key, mods: windowing::Mods, repeat: bool, rows: i64, cx: &mut EventCx) {
    // On macOS the Command layer belongs to the application, as in
    // Terminal.app; programs read Ctrl and Option.
    if cfg!(target_os = "macos") && mods.gui {
        return;
    }
    if mode_key(v, key, mods, rows, cx) {
        return;
    }
    // Ctrl+Shift+C and Ctrl+Shift+V copy and paste, leaving Ctrl+C and
    // Ctrl+V to the program.
    if mods.ctrl && mods.shift && key == Key::C {
        copy_selection(v);
        return redraw(cx);
    }
    if mods.ctrl && mods.shift && key == Key::V {
        paste(v, &crate::services::clipboard::text(), cx);
        return redraw(cx);
    }
    // The rest of the Ctrl+Shift layer belongs to the application, as in
    // other desktop terminals, so those strokes reach the window's commands.
    if mods.ctrl && mods.shift && application_chord(key, mods, cx) {
        return;
    }
    // A Ctrl or Alt chord the window binds to a command is the window's, as
    // GNOME Terminal takes Ctrl+= for zoom and Alt+1 for the first tab. Ctrl
    // and Alt with a letter stay the program's: they are its control codes
    // and Meta keys.
    if (mods.ctrl || mods.alt)
        && !printable(key, false).is_some_and(|c| c.is_ascii_alphabetic())
        && bound_chord(key, mods, cx)
    {
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
    // F11 is the window's where a command binds it, as fullscreen is in GNOME
    // Terminal and xfce4-terminal; elsewhere, as on macOS, it is the program's.
    if key == Key::F11 && !mods.ctrl && !mods.alt && !mods.shift && bound_chord(key, mods, cx) {
        return;
    }
    if mods.shift && !mods.ctrl && (key == Key::PageUp || key == Key::PageDown) {
        let page = if key == Key::PageUp { rows } else { -rows };
        let _ = super::scroll(v.terminal, page);
        return redraw(cx);
    }
    // Option types characters on macOS unless it is set to send Meta; the
    // text arrives as a Text event.
    if cfg!(target_os = "macos") && mods.alt && !mods.ctrl && !v.options.option_as_meta {
        return;
    }
    if let Some(flags) = kitty_flags(v) {
        let state = if repeat {
            KeyState::Repeat
        } else {
            KeyState::Down
        };
        return kitty_key(v, key, mods, state, flags, cx);
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
    if let Ok(changed) = super::typed(v.terminal, &bytes) {
        if mods.alt && !mods.ctrl {
            with_entry(v.terminal, |e| e.suppress_text = true);
        }
        took_typing(v, changed, cx);
    }
}

// The view repaints for typing only when typing changed it; what the program
// answers repaints it as output.
fn took_typing(v: &View, changed: bool, cx: &mut EventCx) {
    ime_area(v, cx);
    cx.typed = true;
    cx.consume();
    if changed {
        cx.redraw = true;
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
        "paste" => super::paste_from_view(v.terminal, &crate::services::clipboard::text()),
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
    fn a_font_weight_draws_the_family_at_that_weight_with_bold_heavier() {
        let theme = Theme::default();
        let mut view = View::new("term", 1, "Shell");
        view.options.font_weight = 300;
        let (_, _, style) = metrics(&view, &theme);
        let family = crate::text::fonts::Family::Named(format!("{DEFAULT_FONT} (weight 300)"));
        assert_eq!(style.family, family);
        let bold = crate::text::fonts::face(crate::text::fonts::primary(&family, true));
        assert_eq!(bold.weight, 700);
        assert!(bold.bold);
        assert!(style.ligatures);
        view.options.ligatures = false;
        assert!(!metrics(&view, &theme).2.ligatures);
    }

    #[test]
    fn an_always_shown_scrollbar_shows_without_history_on_the_side_chosen() {
        let theme = Theme::default();
        let area = Rect::new(0, 0, 400, 300);
        let mut view = View::new("term", 1, "Shell");
        view.options.scrollbar = "always".into();
        let right = scrollbar(&view, &theme, area, 20, (0, 0)).map(|b| b.track.x);
        assert_eq!(right, Some(400 - theme.px(SCROLLBAR) + theme.px(2)));
        view.options.scrollbar_side = "left".into();
        let left = scrollbar(&view, &theme, area, 20, (0, 0)).map(|b| b.track.x);
        assert_eq!(left, Some(theme.px(2)));
        view.options.scrollbar = "scrolling".into();
        assert!(scrollbar(&view, &theme, area, 20, (0, 100)).is_none());
    }

    #[test]
    fn a_left_scrollbar_moves_the_cells_past_it() {
        let theme = Theme::default();
        let mut view = View::new("term", 1, "Shell");
        view.options.scrollbar = "always".into();
        view.options.scrollbar_side = "left".into();
        let (cw, _, _) = metrics(&view, &theme);
        let first = pad_of(&view, &theme) + theme.px(SCROLLBAR);
        assert_eq!(
            cell_under(&view, &theme, first + 1, 10).map(|c| c.1),
            Some(0)
        );
        assert_eq!(
            cell_under(&view, &theme, first + cw.ceil() as i64 + 1, 10).map(|c| c.1),
            Some(1)
        );
        assert_eq!(cell_under(&view, &theme, first - 2, 10), None);
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
