pub mod buffer;
pub mod document_api;
pub mod documents;
pub mod search;
use buffer::Buffer;

// editing: the one editable-text controller every text control shares
// (fields, search, password, text areas, editable combos, and the code
// editor's line editing). Offsets are UTF-8 byte offsets that always sit
// on extended grapheme cluster boundaries. Input-method preedit lives
// outside the committed text until the platform commits it, and every
// edit is a revisioned transaction recorded in a bounded undo history.

use crate::strext::StrExt;
use crate::unicode;

use crate::services::clipboard;
use crate::text::layout::{TextLayout, grapheme_boundaries};
use crate::text::offsets;
use crate::text::offsets::{ByteOffset, ScalarIndex};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Delta {
    pub start: usize,
    pub removed: usize,
    pub inserted: usize,
}
impl Delta {
    fn inverse(&self) -> Self {
        Self {
            start: self.start,
            removed: self.inserted,
            inserted: self.removed,
        }
    }
}

/// One undoable state.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Deserialize)]
pub struct Snapshot {
    pub text: Buffer,
    #[serde(default)]
    pub cost: usize,
    #[serde(skip)]
    pub deltas: Vec<Delta>,
    pub anchor: i64,
    pub caret: i64,
}

impl serde::Serialize for Snapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if crate::memory::measuring() {
            use serde::ser::SerializeTupleStruct;
            return serializer
                .serialize_tuple_struct(
                    "__gui_owned_bytes",
                    self.cost
                        .saturating_add(self.deltas.capacity() * size_of::<Delta>()),
                )?
                .end();
        }
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Snapshot", 4)?;
        state.serialize_field("text", &self.text)?;
        state.serialize_field("anchor", &self.anchor)?;
        state.serialize_field("caret", &self.caret)?;
        state.serialize_field("cost", &self.cost)?;
        state.end()
    }
}

/// How an edit groups with the one before it in undo history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum EditKind {
    #[default]
    #[serde(rename = "NoEdit", alias = "None")]
    None,
    Insert,
    Delete,
    Other,
}

/// Editing state of one text control.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EditState {
    /// The text as edited here; may run ahead of the application's value
    /// until the next view shows the change.
    pub text: Buffer,
    #[serde(skip)]
    pub document: i64,
    #[serde(skip)]
    pub rich_committed: bool,
    #[serde(skip)]
    pub layout_key: u64,
    #[serde(skip)]
    pub follow_caret: bool,
    #[serde(skip)]
    pub changes: Vec<Delta>,
    /// The application's value last seen, to tell external changes apart.
    pub external: std::sync::Arc<str>,
    pub anchor: i64,
    pub caret: i64,
    pub preedit: String,
    /// Caret position inside the preedit, in bytes.
    pub preedit_caret: i64,
    pub undo: Vec<Snapshot>,
    pub redo: Vec<Snapshot>,
    pub last_kind: EditKind,
    pub last_time: i64,
    pub revision: i64,
    /// The x a vertical move aims for, kept across consecutive moves.
    pub goal_x: f64,
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub selecting: bool,
    /// Position in the field's history list while browsing it, or -1.
    pub history_index: i64,
    pub revealed: bool,
    pub initialized: bool,
}

impl Default for EditState {
    fn default() -> EditState {
        EditState {
            text: Buffer::default(),
            document: 0,
            rich_committed: false,
            layout_key: 0,
            follow_caret: true,
            changes: Vec::new(),
            external: std::sync::Arc::from(""),
            anchor: 0,
            caret: 0,
            preedit: String::new(),
            preedit_caret: 0,
            undo: vec![],
            redo: vec![],
            last_kind: EditKind::default(),
            last_time: 0,
            revision: 0,
            goal_x: 0.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            selecting: false,
            history_index: 0,
            revealed: false,
            initialized: false,
        }
    }
}

/// Most undo entries kept per control.
pub const UNDO_LIMIT: i64 = 200;
/// Combined owned storage for undo and redo in one control. Oversized snapshots
/// are not retained; editing the live document still succeeds.
pub const UNDO_BYTE_LIMIT: usize = 64 * 1024 * 1024;
/// Typing pauses longer than this start a new undo group (ms).
pub const GROUP_PAUSE: i64 = 1000;

impl EditState {
    /// Bytes owned by history, including vector capacities. The live text,
    /// external application value and IME preedit are accounted separately.
    pub fn history_bytes(&self) -> usize {
        (self.undo.capacity() + self.redo.capacity()) * size_of::<Snapshot>()
            + self
                .undo
                .iter()
                .chain(&self.redo)
                .map(|s| {
                    s.cost
                        .saturating_add(s.deltas.capacity() * size_of::<Delta>())
                })
                .sum::<usize>()
    }

    fn trim_history(&mut self) {
        while self.undo.len() + self.redo.len() > UNDO_LIMIT as usize
            || self.history_bytes() > UNDO_BYTE_LIMIT
        {
            if !self.undo.is_empty() {
                self.undo.remove(0);
            } else if !self.redo.is_empty() {
                self.redo.remove(0);
            } else {
                break;
            }
            self.undo.shrink_to_fit();
            self.redo.shrink_to_fit();
        }
    }

    /// Brings the state in line with the application's current `value`:
    /// adopts it when it changed outside this controller.
    pub fn sync(&mut self, value: &str) {
        if !self.initialized {
            self.text = value.into();
            self.layout_key = self.text.identity().wrapping_mul(2).wrapping_add(1);
            self.external = std::sync::Arc::from(value);
            self.anchor = value.byte_len();
            self.caret = value.byte_len();
            self.history_index = -1;
            self.goal_x = -1.0;
            self.initialized = true;
            return;
        }
        if value != self.external.as_ref() {
            self.external = std::sync::Arc::from(value);
            if self.text != value {
                self.text = value.into();
                self.preedit = "".to_string();
                self.anchor = snap(value, self.anchor.min(value.byte_len()));
                self.caret = snap(value, self.caret.min(value.byte_len()));
                self.last_kind = EditKind::None;
            }
        }
    }

    /// The selection as an ordered byte range, start first.
    pub fn selection(&self) -> (i64, i64) {
        (self.anchor.min(self.caret), self.anchor.max(self.caret))
    }

    /// Whether the selection covers any text.
    pub fn has_selection(&self) -> bool {
        self.anchor != self.caret
    }

    /// The selected part of the text.
    pub fn selected_text(&self) -> String {
        let (a, b) = self.selection();
        self.text.slice(a as usize, b as usize)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.snapshot(),
            cost: 0,
            deltas: Vec::new(),
            anchor: self.anchor,
            caret: self.caret,
        }
    }

    // Records the state before an edit, merging with the previous entry
    // when it continues the same kind of typing.
    fn record(&mut self, kind: EditKind, now: i64, cost: usize) {
        let cost = cost.min(self.text.owned_charge());
        let continues = kind != EditKind::Other
            && kind == self.last_kind
            && now - self.last_time < GROUP_PAUSE
            && !self.undo.is_empty();
        if !continues {
            if cost + size_of::<Snapshot>() <= UNDO_BYTE_LIMIT {
                let mut snapshot = self.snapshot();
                snapshot.cost = cost;
                self.undo.push(snapshot);
            } else {
                // An unrecorded edit is a history boundary: do not let a later
                // undo jump across it into unrelated older text.
                self.undo = vec![];
            }
        }
        if continues && let Some(snapshot) = self.undo.last_mut() {
            snapshot.cost = snapshot
                .cost
                .saturating_add(cost)
                .min(snapshot.text.owned_charge());
        }
        self.redo = vec![];
        self.trim_history();
        self.last_kind = kind;
        self.last_time = now;
    }

    /// Replaces the selection with `insert` as one transaction. Answers
    /// whether the text changed. `max_graphemes` (0 = none) bounds the result.
    pub fn replace_selection(
        &mut self,
        insert: &str,
        kind: EditKind,
        now: i64,
        max_graphemes: i64,
    ) -> bool {
        let (a, b) = self.selection();
        let mut piece = insert.to_string();
        if max_graphemes > 0 {
            let current = unicode::grapheme_count(&self.text)
                - unicode::grapheme_count(self.text.substring(a, b));
            let room = (max_graphemes - current).max(0);
            if unicode::grapheme_count(&piece) > room {
                piece = take_graphemes(&piece, room);
            }
        }
        if piece.is_empty() && a == b {
            return false;
        }
        // Ropey nodes have bounded fanout and 1 KiB leaves. Reserve a
        // conservative path-copy allowance plus removed/inserted payloads.
        let cost = ((b - a).max(0) as usize)
            .saturating_add(piece.len())
            .saturating_mul(4)
            .saturating_add(128 * 1024);
        self.record(kind, now, cost);
        let delta = Delta {
            start: a as usize,
            removed: (b - a) as usize,
            inserted: piece.len(),
        };
        if let Some(snapshot) = self.undo.last_mut() {
            snapshot.deltas.push(delta.clone());
        }
        self.changes = vec![delta];
        self.text.replace(a as usize, b as usize, &piece);
        self.trim_history();
        self.caret = a + piece.byte_len();
        self.anchor = self.caret;
        self.revision += 1;
        self.goal_x = -1.0;
        true
    }

    /// Commits input-method or typed text at the caret.
    pub fn insert(&mut self, text: &str, now: i64, max_graphemes: i64) -> bool {
        self.preedit = "".to_string();
        let kind = if text == " " || text.contains("\n") {
            EditKind::Other
        } else {
            EditKind::Insert
        };
        self.replace_selection(text, kind, now, max_graphemes)
    }

    /// Deletes the selection, or the grapheme (or word, with `word`)
    /// before the caret.
    pub fn backspace(&mut self, word: bool, now: i64) -> bool {
        if !self.has_selection() {
            if self.caret == 0 {
                return false;
            }
            self.anchor = if word {
                self.text.word_target(self.caret.max(0) as usize, -1) as i64
            } else {
                self.text.boundary(self.caret.max(0) as usize, -1) as i64
            };
        }
        self.replace_selection("", EditKind::Delete, now, 0)
    }

    /// Deletes the selection, or the grapheme (or word) after the caret.
    pub fn delete_forward(&mut self, word: bool, now: i64) -> bool {
        if !self.has_selection() {
            if self.caret >= self.text.byte_len() {
                return false;
            }
            self.anchor = if word {
                self.text.word_target(self.caret.max(0) as usize, 1) as i64
            } else {
                self.text.boundary(self.caret.max(0) as usize, 1) as i64
            };
        }
        self.replace_selection("", EditKind::Delete, now, 0)
    }

    /// Moves the caret to `to`, extending the selection when `extend`.
    pub fn move_to(&mut self, to: i64, extend: bool) {
        self.caret = self.text.snap(to.max(0) as usize) as i64;
        if !extend {
            self.anchor = self.caret;
        }
        self.last_kind = EditKind::None;
    }

    /// One grapheme (or word) left or right in logical order. Without
    /// `extend`, a selection collapses to its side first.
    pub fn step(&mut self, dir: i64, word: bool, extend: bool) {
        if !extend && self.has_selection() && !word {
            let (a, b) = self.selection();
            self.move_to(if dir < 0 { a } else { b }, false);
            return;
        }
        let to = if dir < 0 {
            if word {
                self.text.word_target(self.caret.max(0) as usize, -1) as i64
            } else {
                self.text.boundary(self.caret.max(0) as usize, -1) as i64
            }
        } else if word {
            self.text.word_target(self.caret.max(0) as usize, 1) as i64
        } else {
            self.text.boundary(self.caret.max(0) as usize, 1) as i64
        };
        self.move_to(to, extend);
        self.goal_x = -1.0;
    }

    /// One caret stop visually left or right through `layout` (bidi aware).
    pub fn visual_step(&mut self, layout: &TextLayout, dir: i64, extend: bool) {
        if !extend && self.has_selection() {
            let (a, b) = self.selection();
            let xa = layout.caret_x(a);
            let xb = layout.caret_x(b);
            let left_edge = if xa <= xb { a } else { b };
            let right_edge = if xa <= xb { b } else { a };
            self.move_to(if dir < 0 { left_edge } else { right_edge }, false);
            return;
        }
        let to = layout.visual_step(self.caret, dir);
        self.move_to(to, extend);
        self.goal_x = -1.0;
    }

    /// One line up or down through `layout`, keeping the goal column.
    pub fn vertical(&mut self, layout: &TextLayout, dir: i64, extend: bool) {
        if self.text.len() > 32 * 1024 {
            let goal = if self.goal_x < 0.0 {
                layout.caret_x(self.caret)
            } else {
                self.goal_x
            };
            if let Some(to) = crate::text::document_layout::vertical(self, dir, goal) {
                self.move_to(to, extend);
                self.goal_x = goal;
                return;
            }
            let line = self.text.line_of(self.caret.max(0) as usize);
            let target = (line as i64 + dir)
                .clamp(0, self.text.line_count().saturating_sub(1) as i64)
                as usize;
            let column = self.text.scalar_of(self.caret.max(0) as usize)
                - self.text.scalar_of(self.text.line_start(line));
            let start = self.text.line_start(target);
            let end = self.text.line_start(target + 1);
            let to = self
                .text
                .byte_of_scalar(self.text.scalar_of(start) + column)
                .min(end);
            self.move_to(to as i64, extend);
            return;
        }
        if self.goal_x < 0.0 {
            self.goal_x = layout.caret_x(self.caret);
        }
        let goal = self.goal_x;
        let to = layout.vertical_step(self.caret, dir, goal);
        self.move_to(to, extend);
        self.goal_x = goal;
    }

    /// To the start or end of the caret's visual line.
    pub fn line_edge(&mut self, layout: &TextLayout, end: bool, extend: bool) {
        if layout.lines.is_empty() {
            self.move_to(if end { self.text.byte_len() } else { 0 }, extend);
            return;
        }
        let line = &layout.lines[(layout.line_of(self.caret)) as usize];
        self.move_to(if end { line.end } else { line.start }, extend);
        self.goal_x = -1.0;
    }

    /// Selects the whole text, caret at the end.
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.byte_len();
        self.last_kind = EditKind::None;
    }

    /// Selects the word around `offset` (double-click).
    pub fn select_word(&mut self, offset: i64) {
        let (start, end) = self.text.word_range(offset.max(0) as usize);
        self.anchor = start as i64;
        self.caret = end as i64;
    }

    /// Selects the line around `offset` (triple-click).
    pub fn select_line(&mut self, offset: i64) {
        let line = self.text.line_of(offset.max(0) as usize);
        let start = self.text.line_start(line) as i64;
        let mut end = self.text.line_start(line + 1) as i64;
        if end > start && self.text.slice(end as usize - 1, end as usize) == "\n" {
            end -= 1;
        }
        self.anchor = start;
        self.caret = end;
    }

    /// Restores the text and selection before the last edit; answers whether there was one.
    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        if prev.cost + size_of::<Snapshot>() <= UNDO_BYTE_LIMIT {
            let mut snapshot = self.snapshot();
            snapshot.cost = prev.cost;
            snapshot.deltas = prev.deltas.clone();
            self.redo.push(snapshot);
        } else {
            self.redo = vec![];
        }
        self.trim_history();
        self.changes = prev.deltas.iter().rev().map(Delta::inverse).collect();
        self.text = prev.text;
        self.anchor = prev.anchor;
        self.caret = prev.caret;
        self.last_kind = EditKind::None;
        self.revision += 1;
        true
    }

    /// Reapplies the last undone edit; answers whether there was one.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        if next.cost + size_of::<Snapshot>() <= UNDO_BYTE_LIMIT {
            let mut snapshot = self.snapshot();
            snapshot.cost = next.cost;
            snapshot.deltas = next.deltas.clone();
            self.undo.push(snapshot);
        } else {
            self.undo = vec![];
        }
        self.trim_history();
        self.changes = next.deltas;
        self.text = next.text;
        self.anchor = next.anchor;
        self.caret = next.caret;
        self.last_kind = EditKind::None;
        self.revision += 1;
        true
    }

    /// Copies the selection; answers whether anything was copied.
    pub fn copy(&self) -> bool {
        if !self.has_selection() {
            return false;
        }
        clipboard::set_text(&self.selected_text());
        true
    }

    /// Copies the selection and deletes it as its own undo entry.
    pub fn cut(&mut self, now: i64) -> bool {
        if !self.copy() {
            return false;
        }
        self.replace_selection("", EditKind::Other, now, 0)
    }

    /// Pastes clipboard text as one undo group. Single-line controls turn
    /// newlines into spaces.
    pub fn paste(&mut self, single_line: bool, now: i64, max_graphemes: i64) -> bool {
        let mut text = clipboard::text();
        if text.is_empty() {
            return false;
        }
        if single_line {
            text = text
                .replace("\r\n", " ")
                .replace("\n", " ")
                .replace("\r", " ");
        }
        self.preedit = "".to_string();
        self.replace_selection(&text, EditKind::Other, now, max_graphemes)
    }

    /// Sets the input-method preedit shown at the caret (not committed).
    /// `cursor` is the caret inside `text`; one outside it, such as SDL's
    /// -1 for "not set", places the caret after the preedit.
    pub fn compose(&mut self, text: &str, cursor: ScalarIndex) {
        self.preedit = text.to_string();
        self.preedit_caret = match offsets::byte_of_scalar(text, cursor) {
            Ok(ByteOffset(b)) => b,
            Err(_) => text.byte_len(),
        };
    }

    /// Text as displayed: committed text with the preedit spliced in at
    /// the caret, and the preedit's byte range in it.
    pub fn display(&self) -> (String, i64, i64) {
        if self.preedit.is_empty() {
            return (self.text.to_string(), -1, -1);
        }
        let (a, b) = self.selection();
        let shown = self.text.substring(0, a).to_owned()
            + &self.preedit
            + self.text.substring(b, self.text.byte_len());
        (shown, a, a + self.preedit.byte_len())
    }
}

/// `offset` moved down onto a grapheme boundary of `text`.
pub fn snap(text: &str, offset: i64) -> i64 {
    let mut best = 0;
    for b in grapheme_boundaries(text) {
        if b <= offset { best = b } else { break }
    }
    best
}

pub fn prev_boundary(text: &str, offset: i64) -> i64 {
    let mut best = 0;
    for b in grapheme_boundaries(text) {
        if b < offset { best = b } else { break }
    }
    best
}

pub fn next_boundary(text: &str, offset: i64) -> i64 {
    for b in grapheme_boundaries(text) {
        if b > offset {
            return b;
        }
    }
    text.byte_len()
}

/// Word boundaries, including 0 and the length: grapheme runs of word
/// characters (letters, digits, marks, underscore), spaces, and punctuation
/// each form one segment, and every ideograph is a word of its own.
pub fn word_boundaries(text: &str) -> Vec<i64> {
    let mut out = vec![0];
    let mut at = 0;
    let mut last = -1;
    for g in unicode::graphemes(text) {
        let class = word_class(g);
        if at > 0 && (class != last || class == 3) {
            out.push(at)
        }
        at += g.byte_len();
        last = class;
    }
    if at > 0 {
        out.push(at)
    };
    out
}

// 0 word character, 1 space, 2 punctuation or symbol, 3 ideograph.
fn word_class(g: &str) -> i64 {
    if let Some(c) = g.chars().next() {
        let v = c as i64;
        if (0x3400..=0x9fff).contains(&v)
            || (0x3040..=0x30ff).contains(&v)
            || (0xf900..=0xfaff).contains(&v)
            || (0x20000..=0x3fffd).contains(&v)
        {
            return 3;
        }
        if unicode::is_letter(c) || unicode::is_number(c) || c == '_' {
            return 0;
        };
        if unicode::is_space(c) {
            return 1;
        };
        return 2;
    }
    2
}

fn is_word_segment(text: &str, start: i64, end: i64) -> bool {
    let piece = text.substring(start, end);
    for c in piece.chars() {
        if unicode::is_letter(c) || unicode::is_number(c) || c == '_' {
            return true;
        }
    }
    false
}

/// The start of the word before `offset` (skipping spaces and punctuation).
pub fn prev_word(text: &str, offset: i64) -> i64 {
    let b = word_boundaries(text);
    let mut i = b.len() - 1;
    while i > 0 {
        if b[i - 1] < offset && is_word_segment(text, b[i - 1], b[i]) {
            return b[i - 1];
        }
        i -= 1;
    }
    0
}

/// The end of the word after `offset`.
pub fn next_word(text: &str, offset: i64) -> i64 {
    let b = word_boundaries(text);
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i + 1] > offset && is_word_segment(text, b[i], b[i + 1]) {
            return b[i + 1];
        }
        i += 1;
    }
    text.byte_len()
}

fn take_graphemes(text: &str, n: i64) -> String {
    let mut out = String::new();
    for (k, g) in unicode::graphemes(text).enumerate() {
        if k as i64 >= n {
            break;
        };
        out += g;
    }
    out
}

/// How a validator judges a draft value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Validity {
    Valid,
    /// Could become valid with more typing (an incomplete number).
    Intermediate(String),
    Invalid(String),
}

/// Applies an input mask: `9` a digit, `a` a letter, `*` anything; other
/// characters are literals inserted automatically. Answers the masked text
/// and whether the input fit the mask.
pub fn apply_mask(mask: &str, input: &str) -> (String, bool) {
    let mut out = String::new();
    let chars: Vec<char> = input.chars().collect();
    let mut ci = 0;
    let mut ok = true;
    for m in mask.chars() {
        if ci >= chars.len() {
            break;
        }
        match m {
            '9' | 'a' | '*' => {
                // Skip input that does not fit this slot.
                while ci < chars.len() {
                    let c = chars[ci];
                    ci += 1;
                    let fits = match m {
                        '9' => c.is_ascii_digit(),
                        'a' => unicode::is_letter(c),
                        _ => true,
                    };
                    if fits {
                        out.push(c);
                        break;
                    }
                    ok = false;
                }
            }
            literal => {
                out.push(literal);
                if chars[ci] == literal {
                    ci += 1
                }
            }
        }
    }
    if ci < chars.len() {
        ok = false;
    }
    (out, ok)
}

#[cfg(test)]
mod tests {
    #[test]
    fn idle_edit_kind_avoids_the_gossamer_prelude_collision_and_accepts_legacy_states() {
        assert_eq!(
            serde_json::to_string(&super::EditKind::None).unwrap(),
            "\"NoEdit\""
        );
        assert_eq!(
            serde_json::from_str::<super::EditKind>("\"None\"").unwrap(),
            super::EditKind::None
        );
        assert_eq!(
            serde_json::from_str::<super::EditKind>("\"NoEdit\"").unwrap(),
            super::EditKind::None
        );
    }

    use super::{EditState, Validity, apply_mask, next_word, prev_word};

    use crate::text::offsets::ScalarIndex;

    fn state(text: &str) -> EditState {
        let mut s = EditState::default();
        s.sync(text);
        s
    }

    #[test]
    fn typing_groups_into_one_undo_step() {
        let mut s = state("");
        s.insert("a", 0, 0);
        s.insert("b", 10, 0);
        s.insert("c", 20, 0);
        assert_eq!(s.text, "abc");
        assert!(s.undo());
        assert_eq!(s.text, "");
        assert!(s.redo());
        assert_eq!(s.text, "abc")
    }

    #[test]
    fn a_pause_starts_a_new_group() {
        let mut s = state("");
        s.insert("a", 0, 0);
        s.insert("b", 5000, 0);
        assert!(s.undo());
        assert_eq!(s.text, "a")
    }

    #[test]
    fn backspace_removes_whole_graphemes() {
        let mut s = state("ae\u{0301}");
        s.backspace(false, 0);
        assert_eq!(s.text, "a");
        let mut e = state("x\u{1F468}\u{200D}\u{1F469}");
        e.backspace(false, 0);
        assert_eq!(e.text, "x")
    }

    #[test]
    fn words_skip_punctuation() {
        assert_eq!(prev_word("hello, world", 12), 7);
        assert_eq!(prev_word("hello, world", 7), 0);
        assert_eq!(next_word("hello, world", 0), 5);
        assert_eq!(next_word("hello, world", 5), 12)
    }

    #[test]
    fn selection_replacement_and_step_collapse() {
        let mut s = state("hello");
        s.move_to(1, false);
        s.move_to(4, true);
        assert_eq!(s.selected_text(), "ell");
        s.step(-1, false, false);
        assert_eq!(s.caret, 1);
        s.move_to(4, true);
        s.insert("i", 0, 0);
        assert_eq!(s.text, "hio")
    }

    #[test]
    fn external_changes_are_adopted_and_clamped() {
        let mut s = state("abcdef");
        s.move_to(6, false);
        s.sync("ab");
        assert_eq!(s.text, "ab");
        assert_eq!(s.caret, 2);
        // A value the controller itself produced is not an external change.
        s.insert("c", 0, 0);
        s.sync("ab");
        assert_eq!(s.text, "abc")
    }

    #[test]
    fn preedit_stays_out_of_committed_text() {
        let mut s = state("ab");
        s.compose("\u{304B}", ScalarIndex(1));
        let (shown, from, to) = s.display();
        assert_eq!(shown, "ab\u{304B}");
        assert_eq!((from, to), (2, 5));
        assert_eq!(s.text, "ab");
        s.insert("\u{6F22}", 0, 0);
        assert_eq!(s.text, "ab\u{6F22}");
        assert!(s.preedit.is_empty())
    }

    #[test]
    fn preedit_caret_converts_scalars_and_defaults_to_the_end() {
        let mut s = state("");
        s.compose("\u{304B}\u{1F600}x", ScalarIndex(2));
        assert_eq!(s.preedit_caret, 7);
        s.compose("\u{304B}\u{1F600}x", ScalarIndex(-1));
        assert_eq!(s.preedit_caret, 8);
        s.compose("\u{304B}", ScalarIndex(5));
        assert_eq!(s.preedit_caret, 3)
    }

    #[test]
    fn max_length_counts_graphemes() {
        let mut s = state("ab");
        s.insert("cde", 0, 4);
        assert_eq!(s.text, "abcd");
        assert!(!s.insert("x", 0, 4))
    }

    #[test]
    fn masks_insert_literals_and_reject_misfits() {
        assert_eq!(
            apply_mask("9999-99-99", "20261016"),
            ("2026-10-16".to_string(), true)
        );
        let (shown, ok) = apply_mask("99", "a1");
        assert_eq!(shown, "1");
        assert!(!ok)
    }

    #[test]
    fn validity_compares() {
        assert!(Validity::Valid != Validity::Invalid("x".to_string()))
    }
}

#[cfg(test)]
mod byte_budget_tests {
    #[test]
    fn history_admission_includes_delta_vector_capacity() {
        let mut state = super::EditState::default();
        let mut snapshot = state.snapshot();
        snapshot.cost = 1;
        snapshot.deltas =
            Vec::with_capacity(super::UNDO_BYTE_LIMIT / std::mem::size_of::<super::Delta>() + 1);
        assert!(crate::memory::measure(&snapshot) > super::UNDO_BYTE_LIMIT);
        state.undo.push(snapshot);
        assert!(state.history_bytes() > super::UNDO_BYTE_LIMIT);
        state.trim_history();
        assert!(state.undo.is_empty());
        state.changes = Vec::with_capacity(4096);
        let mut store = crate::widgets::state::StateStore::new();
        store
            .try_set("field", crate::widgets::state::WState::Edit(state))
            .unwrap();
        assert!(store.owned_bytes() >= 4096 * std::mem::size_of::<super::Delta>());
    }

    use super::*;

    #[test]
    fn undo_and_redo_share_a_byte_budget_and_oversized_edits_still_work() {
        let mut state = EditState::default();
        state.sync(&"x".repeat(1024 * 1024));
        for i in 0..30 {
            state.replace_selection("y", EditKind::Other, i, 0);
            assert!(state.history_bytes() <= UNDO_BYTE_LIMIT);
        }
        while state.undo() {
            assert!(state.history_bytes() <= UNDO_BYTE_LIMIT);
        }
        while state.redo() {
            assert!(state.history_bytes() <= UNDO_BYTE_LIMIT);
        }
        let text = "z".repeat(10 * 1024 * 1024);
        state.sync(&text);
        state.caret = text.len() as i64;
        state.anchor = state.caret;
        assert!(state.insert("!", 100, 0));
        assert!(state.history_bytes() <= UNDO_BYTE_LIMIT);
        assert!(state.text.ends_with('!'));
        assert!(state.undo());
    }
}
