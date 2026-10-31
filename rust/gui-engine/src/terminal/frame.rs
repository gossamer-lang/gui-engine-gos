//! Immutable byte-admitted viewport snapshots and accessible text coordinates.
use super::engine::{Engine, Slot, cell_text, color, slots, styles};
use super::links::{self, Link};
use crate::memory::{Budget, Lease};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}
/// A cell's text: nothing, one ASCII character, or a longer grapheme. Most
/// cells are blank or ASCII, so a frame stores those without allocating.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CellText {
    #[default]
    Empty,
    Ascii(u8),
    Other(Box<str>),
}
impl CellText {
    fn new(text: &str) -> CellText {
        match text.as_bytes() {
            [] => CellText::Empty,
            [byte] if byte.is_ascii() => CellText::Ascii(*byte),
            _ => CellText::Other(text.into()),
        }
    }
    pub fn as_str(&self) -> &str {
        match self {
            CellText::Empty => "",
            // An ASCII byte is always a whole UTF-8 character.
            CellText::Ascii(byte) => std::str::from_utf8(std::slice::from_ref(byte)).unwrap_or(""),
            CellText::Other(text) => text,
        }
    }
    pub fn is_empty(&self) -> bool {
        matches!(self, CellText::Empty)
    }
    pub fn len(&self) -> usize {
        self.as_str().len()
    }
    pub fn is_ascii(&self) -> bool {
        match self {
            CellText::Other(text) => text.is_ascii(),
            _ => true,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub text: CellText,
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
    links: Vec<Link>,
    text: String,
    offsets: Vec<Vec<usize>>,
    _lease: Lease,
}
impl Frame {
    pub(crate) fn capture(engine: &Engine, budget: &Budget) -> Result<Self, String> {
        let (rows, cols) = engine.size();
        let bytes = usize::from(rows) * usize::from(cols) * 160 + usize::from(rows) * 128 + 4096;
        if bytes > 32 * 1024 * 1024 {
            return Err("terminal viewport exceeds frame budget".into());
        }
        let lease = budget.reserve(bytes)?;
        let mut cells = Vec::with_capacity(rows.into());
        let mut offsets = Vec::with_capacity(rows.into());
        let mut wrapped = Vec::with_capacity(rows.into());
        let mut marked = Vec::new();
        let mut text = String::new();
        engine.with_visible_lines(|lines| {
            for (row, line) in lines.iter().enumerate() {
                let mut cells_of_row = Vec::with_capacity(cols.into());
                let mut positions = Vec::with_capacity(usize::from(cols) + 1);
                let mut previous = text.len();
                let mut uris: Vec<Option<&str>> = Vec::with_capacity(cols.into());
                for slot in slots(line, cols) {
                    let (shown, continuation, wide, attrs) = match slot {
                        Slot::Cell(cell, attrs, wide) => {
                            (cell_text(cell, attrs), false, wide, Some(attrs))
                        }
                        Slot::Continuation => ("", true, false, None),
                        Slot::Blank => ("", false, false, None),
                    };
                    positions.push(if continuation { previous } else { text.len() });
                    if !continuation {
                        previous = text.len();
                        if shown.is_empty() {
                            text.push(' ');
                        } else {
                            text.push_str(shown);
                        }
                    }
                    let uri = match slot {
                        Slot::Continuation => uris.last().copied().flatten(),
                        _ => attrs.and_then(|a| a.hyperlink()).map(|h| h.uri()),
                    };
                    uris.push(uri);
                    let (bold, dim, italic, underline, inverse) =
                        attrs.map(styles).unwrap_or_default();
                    cells_of_row.push(Cell {
                        text: CellText::new(shown),
                        continuation,
                        wide,
                        fg: attrs.map_or(Color::Default, |a| color(a.foreground())),
                        bg: attrs.map_or(Color::Default, |a| color(a.background())),
                        bold,
                        italic,
                        underline,
                        inverse,
                        dim,
                    });
                }
                marked.extend(links::marked(row as u16, &uris));
                positions.push(text.len());
                if row + 1 < usize::from(rows) {
                    text.push('\n');
                }
                wrapped.push(line.last_cell_was_wrapped());
                cells.push(cells_of_row);
                offsets.push(positions);
            }
        });
        let mut found = links::found(&cells, &wrapped, &marked);
        marked.append(&mut found);
        let (cursor_row, cursor_col) = engine.cursor();
        Ok(Self {
            rows,
            cols,
            cursor: (cursor_row.min(rows - 1), cursor_col),
            hide_cursor: engine.hide_cursor(),
            alternate: engine.alternate_screen(),
            cells,
            wrapped,
            links: marked,
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
    /// The links on the screen, marked by the program or found in its text.
    pub fn links(&self) -> &[Link] {
        &self.links
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
