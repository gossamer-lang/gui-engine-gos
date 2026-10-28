//! Immutable byte-admitted viewport snapshots and accessible text coordinates.
use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor};

use super::engine::{Listener, cell_text};
use crate::memory::{Budget, Lease};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}
impl From<TermColor> for Color {
    fn from(value: TermColor) -> Self {
        use NamedColor::*;
        match value {
            TermColor::Spec(rgb) => Self::Rgb(rgb.r, rgb.g, rgb.b),
            TermColor::Indexed(i) => Self::Indexed(i),
            TermColor::Named(named) => match named {
                Foreground | Background | Cursor | BrightForeground | DimForeground => {
                    Self::Default
                }
                // A dim color is its base color; the cell's DIM flag dims it.
                DimBlack => Self::Indexed(0),
                DimRed => Self::Indexed(1),
                DimGreen => Self::Indexed(2),
                DimYellow => Self::Indexed(3),
                DimBlue => Self::Indexed(4),
                DimMagenta => Self::Indexed(5),
                DimCyan => Self::Indexed(6),
                DimWhite => Self::Indexed(7),
                base => Self::Indexed(base as u8),
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub continuation: bool,
    pub wide: bool,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub dim: bool,
}
pub struct Frame {
    rows: u16,
    cols: u16,
    cursor: (u16, u16),
    hide_cursor: bool,
    alternate: bool,
    cells: Vec<Vec<Cell>>,
    wrapped: Vec<bool>,
    text: String,
    offsets: Vec<Vec<usize>>,
    _lease: Lease,
}
impl Frame {
    pub(crate) fn capture(term: &Term<Listener>, budget: &Budget) -> Result<Self, String> {
        let (rows, cols) = (term.screen_lines() as u16, term.columns() as u16);
        let bytes = usize::from(rows) * usize::from(cols) * 160 + usize::from(rows) * 128 + 4096;
        if bytes > 32 * 1024 * 1024 {
            return Err("terminal viewport exceeds frame budget".into());
        }
        let lease = budget.reserve(bytes)?;
        let grid = term.grid();
        let offset = grid.display_offset() as i32;
        let mut cells = Vec::with_capacity(rows.into());
        let mut offsets = Vec::with_capacity(rows.into());
        let mut wrapped = Vec::with_capacity(rows.into());
        let mut text = String::new();
        for row in 0..rows {
            let line = &grid[Line(i32::from(row) - offset)];
            let mut cells_of_row = Vec::with_capacity(cols.into());
            let mut positions = Vec::with_capacity(usize::from(cols) + 1);
            let mut previous = text.len();
            for col in 0..cols {
                let cell = &line[Column(usize::from(col))];
                let continuation = cell.flags.contains(Flags::WIDE_CHAR_SPACER);
                let shown = cell_text(cell);
                positions.push(if continuation { previous } else { text.len() });
                if !continuation {
                    previous = text.len();
                    if shown.is_empty() {
                        text.push(' ');
                    } else {
                        text.push_str(&shown);
                    }
                }
                cells_of_row.push(Cell {
                    text: shown,
                    continuation,
                    wide: cell.flags.contains(Flags::WIDE_CHAR),
                    fg: cell.fg.into(),
                    bg: cell.bg.into(),
                    bold: cell.flags.contains(Flags::BOLD),
                    italic: cell.flags.contains(Flags::ITALIC),
                    underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                    inverse: cell.flags.contains(Flags::INVERSE),
                    dim: cell.flags.contains(Flags::DIM),
                });
            }
            positions.push(text.len());
            if row + 1 < rows {
                text.push('\n');
            }
            wrapped.push(
                line[Column(usize::from(cols) - 1)]
                    .flags
                    .contains(Flags::WRAPLINE),
            );
            cells.push(cells_of_row);
            offsets.push(positions);
        }
        let point = grid.cursor.point;
        let cursor_row = (point.line.0 + offset).clamp(0, i32::from(rows) - 1) as u16;
        Ok(Self {
            rows,
            cols,
            cursor: (cursor_row, point.column.0 as u16),
            hide_cursor: !term
                .mode()
                .contains(alacritty_terminal::term::TermMode::SHOW_CURSOR)
                || offset > 0,
            alternate: term
                .mode()
                .contains(alacritty_terminal::term::TermMode::ALT_SCREEN),
            cells,
            wrapped,
            text,
            offsets,
            _lease: lease,
        })
    }
    pub fn rows(&self) -> u16 {
        self.rows
    }
    pub fn cols(&self) -> u16 {
        self.cols
    }
    pub fn cursor(&self) -> (u16, u16) {
        self.cursor
    }
    pub fn hide_cursor(&self) -> bool {
        self.hide_cursor
    }
    pub fn alternate(&self) -> bool {
        self.alternate
    }
    pub fn cells(&self) -> &[Vec<Cell>] {
        &self.cells
    }
    pub fn wrapped(&self) -> &[bool] {
        &self.wrapped
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn byte_offset(&self, row: u16, col: u16) -> usize {
        self.offsets[usize::from(row.min(self.rows - 1))][usize::from(col.min(self.cols))]
    }
    pub fn position(&self, byte: usize) -> (u16, u16) {
        let byte = byte.min(self.text.len());
        let row = self
            .offsets
            .partition_point(|line| line[0] <= byte)
            .saturating_sub(1);
        let line = &self.offsets[row];
        let mut col = line
            .partition_point(|&offset| offset <= byte)
            .saturating_sub(1)
            .min(usize::from(self.cols));
        while col > 0 && col < usize::from(self.cols) && self.cells[row][col].continuation {
            col -= 1;
        }
        (row as u16, col as u16)
    }
    pub fn selection(&self, anchor: (u16, u16), caret: (u16, u16)) -> &str {
        let a = self.byte_offset(anchor.0, anchor.1);
        let b = self.byte_offset(caret.0, caret.1);
        &self.text[a.min(b)..a.max(b)]
    }
}
#[cfg(test)]
mod tests {
    use super::super::engine::Engine;
    use super::*;
    #[test]
    fn viewport_has_stable_utf8_accessible_offsets_and_immutable_styles() {
        let budget = Budget::new(4 * 1024 * 1024);
        let mut e = Engine::new(3, 12, 10, 2 * 1024 * 1024, Budget::new(4 * 1024 * 1024)).unwrap();
        e.process("a界e\u{301}\r\n\x1b[38;2;12;34;56mcolor".as_bytes())
            .unwrap();
        let frame = e.frame().unwrap();
        assert_eq!(frame.byte_offset(0, 1), 1);
        assert_eq!(frame.byte_offset(0, 2), 1);
        assert_eq!(frame.byte_offset(0, 3), 4);
        assert_eq!(frame.byte_offset(0, 4), 7);
        assert_eq!(frame.selection((0, 1), (0, 4)), "界e\u{301}");
        assert_eq!(frame.position(2), (0, 1));
        assert_eq!(frame.cells[1][0].fg, Color::Rgb(12, 34, 56));
        e.process(b"\x1b[2Jchanged").unwrap();
        assert!(frame.text().starts_with("a界e\u{301}"));
        let own = Engine::new(3, 12, 10, 2 * 1024 * 1024, budget.clone()).unwrap();
        let held = own.frame().unwrap();
        let with_frame = budget.usage().0;
        drop(held);
        assert!(budget.usage().0 < with_frame);
    }
    #[test]
    fn frame_admission_fails_before_copying_and_scrollback_view_is_explicit() {
        let mut e = Engine::new(4, 20, 10, 2 * 1024 * 1024, Budget::new(4 * 1024 * 1024)).unwrap();
        for i in 0..20 {
            e.process(format!("row{i}\r\n").as_bytes()).unwrap();
        }
        e.scroll(5);
        let frame = e.frame().unwrap();
        assert!(!frame.text().contains("row19"));
        assert!(frame.hide_cursor());
        // An engine whose budget it fills leaves no room for a frame.
        let charge = e.charged_bytes();
        let full = Engine::new(4, 20, 10, 2 * 1024 * 1024, Budget::new(charge + 16)).unwrap();
        assert!(full.frame().is_err());
    }
}
