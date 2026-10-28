// collections: virtualized lists, icon grids, tables, and trees. Views
// never materialise one widget per item: the application gives a count and
// a row function, the view asks only for the rows in the viewport plus a
// small overscan, and painting, hit testing, and accessibility all work
// from row indices mapped to stable keys. Selection, focus, the range
// anchor, and edits are tracked by key, so inserting, removing, or
// reordering items never moves the selection to a different item.
//
// Variable row heights use a prefix-sum extent table rebuilt only when the
// application bumps `revision` (or the count changes). The changes that
// produced each revision (`changes`, from a `models::ModelLog`) let the view
// keep its top visible row and its focus on the same items across inserts,
// removals and moves; the session replays them once per revision (`sync`). Keyboard: Up/Down,
// Page Up/Down, Home/End move focus; Space toggles in multiple selection;
// Shift extends from the anchor; Ctrl moves focus without selecting;
// Enter (or a double click) activates; letters jump by typeahead.
use crate::strext::StrExt;
use std::collections::HashMap;
use windowing;
use windowing::{Key, MouseButton};

use crate::accessibility::{AccessNode, Role};
use crate::editing::EditState;
use crate::geometry::{Constraints, Rect, Size, UNBOUNDED};
use crate::models::{ModelChange, ModelLog, position_after};
use crate::render::icons::Icon;
use crate::style::color::Color;
use crate::style::{TextRole, Theme};
use crate::text::layout;
use crate::text::layout::{LayoutOptions, TextAlign};
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx, UiMsg};
use crate::widgets::state::{StateStore, WState};

/// Answers the items a drag of the selected rows (by id) offers.
pub type DragOffer = dyn Fn(Vec<String>) -> Vec<windowing::DragItem>;

/// One row of a list or icon grid.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Row {
    pub key: String,
    pub label: String,
    pub detail: String,
    pub icon: Option<crate::render::icons::Icon>,
    pub disabled: bool,
    /// Tree depth (for trees) and whether the item can expand.
    pub depth: i64,
    pub expandable: bool,
    pub expanded: bool,
    /// Table cells, by column order.
    pub cells: Vec<String>,
}

impl Row {
    pub fn new(key: &str, label: &str) -> Row {
        Row {
            key: key.to_string(),
            label: label.to_string(),
            detail: String::new(),
            icon: None,
            disabled: false,
            depth: 0,
            expandable: false,
            expanded: false,
            cells: vec![],
        }
    }
}

/// How a collection lays out its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ViewMode {
    List,
    IconGrid,
}

/// A table column.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Column {
    pub key: String,
    pub title: String,
    /// Width in interface units.
    pub width: i64,
    pub min_width: i64,
    pub sortable: bool,
    pub editable: bool,
    pub align: TextAlign,
}

impl Column {
    pub fn new(key: &str, title: &str, width: i64) -> Column {
        Column {
            key: key.to_string(),
            title: title.to_string(),
            width,
            min_width: 40,
            sortable: true,
            editable: false,
            align: TextAlign::Start,
        }
    }
}

/// A virtualized list, icon grid, or table.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Collection {
    pub id: String,
    pub count: i64,
    pub row: crate::Callback<dyn Fn(i64) -> Row>,
    /// Row height in interface units, or `height_of` for variable rows.
    pub height_of: Option<crate::Callback<dyn Fn(i64) -> i64>>,
    /// Bump when items change so cached extents are rebuilt.
    pub revision: i64,
    /// The changes that produced recent revisions, numbered by revision
    /// (`ModelLog::changes`); without them a new revision keeps the scroll
    /// offset as it is.
    pub changes: Vec<(i64, ModelChange)>,
    pub selected: Vec<String>,
    pub multiple: bool,
    pub mode: ViewMode,
    pub columns: Vec<Column>,
    /// (column key, ascending) the table is sorted by, or ("", true).
    pub sort: (String, bool),
    pub label: String,
    pub empty_text: String,
    /// Visible rows requested (natural height), 0 for 8.
    pub rows: i64,
    /// Show a tree's expand arrows and indentation.
    pub tree: bool,
    /// Lets the pointer drag rows out: the dragged row keys (the selection,
    /// when a selected row is dragged) to the items offered. Answered by
    /// `UiMsg::DragFinished(id, operation)`.
    pub drag: Option<crate::Callback<DragOffer>>,
    /// Draws a list row's content (after its tree arrow and icon) as the
    /// widget it answers, in place of the label and detail. The widget is
    /// drawn only: the row keeps selection, focus, keyboard and accessibility,
    /// and its text takes the row's selected or disabled color.
    pub delegate: Option<crate::Callback<dyn Fn(Row) -> crate::widgets::Widget>>,
}

impl Collection {
    /// A list of `count` rows, `row(i)` describing each, with nothing
    /// selected, fixed-height rows, and no columns.
    pub fn new(id: &str, count: i64, row: impl Fn(i64) -> Row + 'static) -> Collection {
        Collection {
            id: id.to_string(),
            count,
            row: crate::Callback::new(std::rc::Rc::new(row)),
            height_of: None,
            revision: 0,
            changes: vec![],
            selected: vec![],
            multiple: false,
            mode: ViewMode::List,
            columns: vec![],
            sort: (String::new(), true),
            label: String::new(),
            empty_text: String::new(),
            rows: 0,
            tree: false,
            drag: None,
            delegate: None,
        }
    }

    /// Rows `height_of(i)` interface units tall instead of one fixed height.
    pub fn height_of(self, height_of: impl Fn(i64) -> i64 + 'static) -> Collection {
        let mut c = self;
        c.height_of = Some(crate::Callback::new(std::rc::Rc::new(height_of)));
        c
    }

    /// The items' revision and the changes that produced recent ones.
    pub fn revision(self, revision: i64, changes: &[(i64, ModelChange)]) -> Collection {
        let mut c = self;
        c.revision = revision;
        c.changes = changes.to_vec();
        c
    }

    /// The selected rows' keys.
    pub fn selected(self, keys: &[String]) -> Collection {
        let mut c = self;
        c.selected = keys.to_vec();
        c
    }

    /// Whether several rows can be selected at once.
    pub fn multiple(self, allowed: bool) -> Collection {
        let mut c = self;
        c.multiple = allowed;
        c
    }

    pub fn mode(self, mode: ViewMode) -> Collection {
        let mut c = self;
        c.mode = mode;
        c
    }

    /// A table of `columns`.
    pub fn columns(self, columns: &[Column]) -> Collection {
        let mut c = self;
        c.columns = columns.to_vec();
        c
    }

    /// The column the table is sorted by and whether ascending.
    pub fn sorted_by(self, key: &str, ascending: bool) -> Collection {
        let mut c = self;
        c.sort = (key.to_string(), ascending);
        c
    }

    /// The accessible name.
    pub fn label(self, label: &str) -> Collection {
        let mut c = self;
        c.label = label.to_string();
        c
    }

    /// What shows when there are no rows.
    pub fn empty_text(self, text: &str) -> Collection {
        let mut c = self;
        c.empty_text = text.to_string();
        c
    }

    /// How many rows tall it asks to be.
    pub fn rows(self, rows: i64) -> Collection {
        let mut c = self;
        c.rows = rows;
        c
    }

    /// Whether rows show as a tree, with expand arrows and indentation.
    pub fn tree(self, tree: bool) -> Collection {
        let mut c = self;
        c.tree = tree;
        c
    }

    /// Lets the pointer drag rows out as `items(keys)`.
    pub fn draggable(
        self,
        items: impl Fn(Vec<String>) -> Vec<windowing::DragItem> + 'static,
    ) -> Collection {
        let mut c = self;
        c.drag = Some(crate::Callback::new(std::rc::Rc::new(items)));
        c
    }

    /// Draws each list row's content as `delegate(row)`.
    pub fn delegate(
        self,
        delegate: impl Fn(Row) -> crate::widgets::Widget + 'static,
    ) -> Collection {
        let mut c = self;
        c.delegate = Some(crate::Callback::new(std::rc::Rc::new(delegate)));
        c
    }
}

impl PartialEq for Collection {
    fn eq(&self, other: &Collection) -> bool {
        self.id == other.id
            && self.count == other.count
            && self.revision == other.revision
            && self.selected == other.selected
            && self.columns == other.columns
            && self.sort == other.sort
    }
}

impl std::fmt::Display for Collection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Collection({}, {} rows)", self.id, self.count)
    }
}

impl std::fmt::Debug for Collection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = { self.to_string() };
        f.write_str(&text)
    }
}

/// Interaction state of a collection.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CollectionState {
    pub scroll_y: i64,
    pub scroll_x: i64,
    pub focus: String,
    pub anchor: String,
    pub typeahead: String,
    pub typeahead_time: i64,
    /// Prefix sums of row heights (pixels), length count + 1.
    pub extents: Vec<i64>,
    pub extents_revision: i64,
    pub extents_count: i64,
    pub extents_scale: f64,
    /// The collection revision the scroll position and focus were last
    /// brought up to, or -1 before the first.
    pub revision: i64,
    /// Where the focused row was, a hint for finding it again.
    pub focus_index: i64,
    /// Column widths (pixels) and display order, by key.
    pub widths: HashMap<String, i64>,
    pub order: Vec<String>,
    /// Pointer drag: 1 header resize, 2 header reorder, 3 a row that may be
    /// dragged out; column key; pointer start.
    pub drag_kind: i64,
    pub drag_column: String,
    pub drag_start: i64,
    pub drag_width: i64,
    /// The cell being edited: (row key, column key), or ("", "").
    pub edit_row: String,
    pub edit_column: String,
    pub last_click_key: String,
    pub last_click_time: i64,
    /// The row a press armed for dragging out, where it was pressed, and
    /// whether the press deferred selecting it to the release.
    pub drag_row: String,
    pub drag_row_index: i64,
    pub drag_row_y: i64,
    pub select_on_release: bool,
}

impl Default for CollectionState {
    fn default() -> CollectionState {
        CollectionState {
            scroll_y: 0,
            scroll_x: 0,
            focus: String::new(),
            anchor: String::new(),
            typeahead: String::new(),
            typeahead_time: 0,
            extents: vec![],
            extents_revision: -1,
            extents_count: -1,
            extents_scale: 0.0,
            revision: -1,
            focus_index: -1,
            widths: HashMap::new(),
            order: vec![],
            drag_kind: 0,
            drag_column: String::new(),
            drag_start: 0,
            drag_width: 0,
            edit_row: String::new(),
            edit_column: String::new(),
            last_click_key: String::new(),
            last_click_time: 0,
            drag_row: String::new(),
            drag_row_index: -1,
            drag_row_y: 0,
            select_on_release: false,
        }
    }
}

/// Rows painted beyond the viewport on each side.
pub const OVERSCAN: i64 = 4;

fn default_row_height(c: &Collection, theme: &Theme) -> i64 {
    if c.mode == ViewMode::IconGrid {
        grid_shape(c, theme, 0).2
    } else {
        theme.row_height()
    }
}

fn header_height(c: &Collection, theme: &Theme) -> i64 {
    if c.columns.is_empty() {
        0
    } else {
        theme.row_height()
    }
}

// Grid geometry: (columns, cell width, cell height). A cell holds its icon
// and two lines of about ten characters at the theme's text size.
fn grid_shape(_c: &Collection, theme: &Theme, width: i64) -> (i64, i64, i64) {
    let small = theme.font(TextRole::Small);
    let (_, _, lh) = layout::metrics(&small);
    let (line_w, _) = layout::measure("0000000000", &small);
    let cell_w = theme.px(112).max(line_w.ceil() as i64 + theme.px(16));
    let cell_h = theme
        .px(112)
        .max(theme.px(theme.shape.grid_icon) + theme.px(24) + 2 * lh.ceil() as i64);
    let cols = (width / cell_w).max(1);
    (cols, width / cols, cell_h)
}

/// The state with extents valid for this collection at this scale.
pub fn with_extents(c: &Collection, st: CollectionState, theme: &Theme) -> CollectionState {
    let mut s = st;
    if c.height_of.is_none() || c.mode == ViewMode::IconGrid {
        return s;
    }
    if s.extents_revision == c.revision
        && s.extents_count == c.count
        && s.extents_scale == theme.scale
    {
        return s;
    }
    let Some(ref h) = c.height_of else { return s };
    let mut ext = vec![0; (c.count + 1) as usize];
    let mut i = 0;
    while i < c.count {
        ext[(i + 1) as usize] = ext[i as usize] + theme.px(h(i)).max(1);
        i += 1;
    }
    s.extents = ext;
    s.extents_revision = c.revision;
    s.extents_count = c.count;
    s.extents_scale = theme.scale;
    s
}

/// Total content height in pixels.
pub fn content_height(c: &Collection, s: &CollectionState, theme: &Theme, width: i64) -> i64 {
    if c.mode == ViewMode::IconGrid {
        let (cols, _, cell) = grid_shape(c, theme, width);
        return ((c.count + cols - 1) / cols) * cell;
    }
    if c.height_of.is_some() && s.extents.len() == (c.count + 1) as usize {
        return s.extents[c.count as usize];
    }
    c.count * default_row_height(c, theme)
}

/// The top (content pixels) and height of row `i` (list modes).
pub fn row_span(c: &Collection, s: &CollectionState, theme: &Theme, i: i64) -> (i64, i64) {
    if c.height_of.is_some() && s.extents.len() == (c.count + 1) as usize && i >= 0 && i < c.count {
        return (
            s.extents[i as usize],
            s.extents[(i + 1) as usize] - s.extents[i as usize],
        );
    }
    let h = default_row_height(c, theme);
    (i * h, h)
}

/// The row index at content offset `y` (list modes), or -1.
pub fn row_at(c: &Collection, s: &CollectionState, theme: &Theme, y: i64) -> i64 {
    if y < 0 || c.count == 0 {
        return -1;
    }
    if c.height_of.is_some() && s.extents.len() == (c.count + 1) as usize {
        // Binary search the prefix sums.
        let mut lo = 0;
        let mut hi = c.count - 1;
        while lo <= hi {
            let mid = (lo + hi) / 2;
            if y < s.extents[mid as usize] {
                hi = mid - 1;
            } else if y >= s.extents[(mid + 1) as usize] {
                lo = mid + 1;
            } else {
                return mid;
            }
        }
        return -1;
    }
    let i = y / default_row_height(c, theme).max(1);
    if i >= c.count { -1 } else { i }
}

/// The index range of rows to paint for a viewport: [first, end).
pub fn visible_range(
    c: &Collection,
    s: &CollectionState,
    theme: &Theme,
    viewport: Size,
) -> (i64, i64) {
    if c.count == 0 {
        return (0, 0);
    }
    if c.mode == ViewMode::IconGrid {
        let (cols, _, cell) = grid_shape(c, theme, viewport.width);
        let first_row = (s.scroll_y / cell.max(1) - 1).max(0);
        let last_row = (s.scroll_y + viewport.height) / cell.max(1) + 1;
        return (first_row * cols, ((last_row + 1) * cols).min(c.count));
    }
    let first = row_at(c, s, theme, s.scroll_y).max(0);
    let mut last = row_at(c, s, theme, s.scroll_y + viewport.height);
    if last < 0 {
        last = c.count - 1
    };
    (
        (first - OVERSCAN).max(0),
        (last + 1 + OVERSCAN).min(c.count),
    )
}

// The first row at least partly in view and how far the view starts into
// it, from the extents the state was last laid out with.
fn top_row(c: &Collection, s: &CollectionState, theme: &Theme) -> (i64, i64) {
    if c.mode == ViewMode::IconGrid {
        return (0, s.scroll_y);
    }
    if s.extents.len() > 1 {
        let last = s.extents.len() - 2;
        let mut lo = 0;
        let mut hi = last;
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if s.extents[mid] <= s.scroll_y {
                lo = mid
            } else {
                hi = mid - 1
            }
        }
        return (lo as i64, s.scroll_y - s.extents[lo]);
    }
    let h = default_row_height(c, theme).max(1);
    (s.scroll_y / h, s.scroll_y % h)
}

// Where position `i` is after `change`; a removed item's place passes to
// the item after it.
fn follow(i: i64, change: ModelChange) -> i64 {
    let p = position_after(i, change);
    if p >= 0 {
        return p;
    };
    match change {
        ModelChange::Removed(at, _) => at,
        _ => i,
    }
}

/// `st` brought up to `c.revision`: the changes since the revision it last
/// saw are replayed so the top visible row and the focus stay on the same
/// items, and variable row extents are rebuilt once. Without the changes (or
/// after a reset) the scroll offset stays, clamped to the new content.
pub fn sync(c: &Collection, st: CollectionState, theme: &Theme) -> CollectionState {
    let mut s = st;
    if s.revision == c.revision {
        return with_extents(c, s, theme);
    }
    let first_sync = s.revision < 0;
    let replay = if first_sync {
        None
    } else {
        ModelLog {
            revision: c.revision,
            changes: c.changes.clone(),
        }
        .since(s.revision)
    };
    let (top, offset) = top_row(c, &s, theme);
    let mut new_top = top;
    let mut focus_at = s.focus_index;
    let mut anchored = false;
    let mut tracked = false;
    if let Some(changes) = replay {
        anchored = c.mode == ViewMode::List;
        tracked = focus_at >= 0;
        for change in changes {
            if change == ModelChange::Reset {
                anchored = false;
                tracked = false
            };
            new_top = follow(new_top, change);
            if focus_at >= 0 {
                focus_at = follow(focus_at, change)
            }
        }
    }
    s.revision = c.revision;
    s = with_extents(c, s, theme);
    if anchored && c.count > 0 {
        let t = new_top.min(c.count - 1).max(0);
        let (row_top, height) = row_span(c, &s, theme, t);
        s.scroll_y = row_top + offset.min(height - 1).max(0);
    }
    s.scroll_y = s
        .scroll_y
        .min((content_height(c, &s, theme, 0) - 1).max(0))
        .max(0);
    if !s.focus.is_empty() {
        let row = &c.row;
        let hint = focus_at.max(0).min((c.count - 1).max(0));
        if c.count == 0 {
            s.focus = String::new();
            s.focus_index = -1;
        } else if row(hint).key == s.focus {
            s.focus_index = hint;
        } else {
            // Followed through every change, a mismatch means the row went.
            let found = if tracked {
                -1
            } else {
                index_of_key(c, &s.focus, hint)
            };
            // A removed row hands focus to the row now in its place.
            let at = if found >= 0 { found } else { hint };
            s.focus = row(at).key;
            s.focus_index = at;
        }
    }
    s
}

fn index_of_key(c: &Collection, key: &str, near: i64) -> i64 {
    if key.is_empty() {
        return -1;
    };
    // Look near the last known position first, then everywhere.
    let row = &c.row;
    let lo = (near - 64).max(0);
    let hi = (near + 64).min(c.count);
    let mut i = lo;
    while i < hi {
        if row(i).key == key {
            return i;
        };
        i += 1;
    }
    i = 0;
    while i < c.count {
        if row(i).key == key {
            return i;
        };
        i += 1;
    }
    -1
}

pub fn layout_collection(c: &Collection, cs: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    let rows = if c.rows > 0 { c.rows } else { 8 };
    let natural_h = header_height(c, theme) + rows * default_row_height(c, theme);
    let mut natural_w = theme.px(320);
    if !c.columns.is_empty() {
        natural_w = c.columns.iter().map(|col| theme.text_px(col.width)).sum();
    }
    let w = if cs.has_bounded_width() {
        cs.max_width
    } else {
        natural_w
    };
    let h = if cs.has_bounded_height() {
        cs.max_height.min(UNBOUNDED)
    } else {
        natural_h
    };
    let mut n = Node::leaf(cs.constrain(Size::new(w, h)));
    let s = with_extents(c, cx.states.collection(&c.id), theme);
    n.content = Size::new(w, content_height(c, &s, theme, w));
    n
}

fn columns_in_order(c: &Collection, s: &CollectionState, theme: &Theme) -> Vec<(Column, i64)> {
    let mut ordered: Vec<Column> = vec![];
    for key in &s.order {
        for col in &c.columns {
            if col.key == *key {
                ordered.push(col.clone())
            }
        }
    }
    for col in &c.columns {
        if !s.order.contains(&col.key) {
            ordered.push(col.clone())
        }
    }
    ordered
        .iter()
        .map(|col| {
            let w = s
                .widths
                .get(&col.key)
                .copied()
                .unwrap_or_else(|| theme.text_px(col.width));
            (col.clone(), w.max(theme.text_px(col.min_width)))
        })
        .collect::<Vec<_>>()
}

// How a list row shares `room` pixels between its label and its detail:
// both whole when they fit; otherwise the detail takes what the label leaves
// but at least a third of the row, the label the rest, and a detail with less
// than `least` pixels is left out. Answers (label, detail) widths.
fn row_widths(
    item: &Row,
    room: i64,
    body: &crate::text::layout::TextStyle,
    small: &crate::text::layout::TextStyle,
    gap: i64,
    least: i64,
) -> (i64, i64) {
    let space = room.max(0);
    if item.detail.is_empty() {
        return (space, 0);
    };
    let (lw, _) = layout::measure(&item.label, body);
    let (dw, _) = layout::measure(&item.detail, small);
    let label = lw.ceil() as i64;
    let detail = dw.ceil() as i64;
    if label + gap + detail <= space {
        return (label, detail);
    };
    let share = detail.min((space - label - gap).max(space / 3));
    if share < least {
        return (space, 0);
    };
    (space - share - gap, share)
}

// Paints a delegate's widget for one row in `area` (canvas pixels), its
// text in the row's color `fg`.
fn paint_delegate(w: &crate::widgets::Widget, area: Rect, fg: Color, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let mut row_theme = theme.clone();
    row_theme.text = fg;
    if fg != theme.text {
        row_theme.text_muted = fg
    };
    let size = Size::new(area.width.max(0), area.height);
    let mut lcx = LayoutCx::new(&row_theme, &cx.states, size);
    let node = w.layout(Constraints::tight(size), &mut lcx);
    let ox = cx.origin_x;
    let oy = cx.origin_y;
    cx.origin_x = area.x;
    cx.origin_y = area.y;
    cx.theme = row_theme.clone();
    w.paint(&node, cx);
    cx.theme = theme.clone();
    cx.origin_x = ox;
    cx.origin_y = oy;
}

pub fn paint_collection(c: &Collection, node: &Node, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let r = Rect::from_size(node.size());
    let s = with_extents(c, cx.states.collection(&c.id), &theme);
    let focused = cx.is_focused(&c.id);
    cx.fill_round(r, theme.radius(), theme.field);
    let abs = cx.abs(r);
    let saved = cx.canvas.clip;
    cx.canvas.push_clip(abs.inset(1).intersect(saved));
    let header = header_height(c, &theme);
    let body = theme.font(TextRole::Body);
    let small = theme.font(TextRole::Small);
    let cols = columns_in_order(c, &s, &theme);
    if header > 0 {
        cx.canvas
            .rect(abs.x, abs.y, abs.width, header, theme.surface_alt);
        let mut x = abs.x - s.scroll_x;
        for (col, w) in &cols {
            let l = layout::layout(
                &col.title,
                &body.with_bold(true),
                LayoutOptions::single_line(),
            );
            layout::draw(
                &mut cx.canvas,
                &l,
                (x + theme.px(10)) as f64,
                (abs.y + (header - l.height.ceil() as i64) / 2) as f64,
                &body.with_bold(true),
                theme.text,
            );
            if c.sort.0 == col.key {
                let isz = theme.px(theme.shape.small_icon);
                cx.icon(
                    if c.sort.1 {
                        &Icon::SortAsc
                    } else {
                        &Icon::SortDesc
                    },
                    x + w - isz - theme.px(8),
                    abs.y + (header - isz) / 2,
                    isz,
                    theme.text_muted,
                )
            }
            cx.canvas.rect(
                x + w - 1,
                abs.y + theme.px(8),
                1,
                header - theme.px(16),
                theme.border,
            );
            x += w;
        }
        cx.canvas
            .rect(abs.x, abs.y + header - 1, abs.width, 1, theme.border)
    }
    let viewport = Size::new(r.width, r.height - header);
    let (first, end) = visible_range(c, &s, &theme, viewport);
    let row = &c.row;
    let selected_bg = if focused && cx.active {
        theme.selection
    } else {
        theme.selection_inactive
    };
    let body_clip = Rect::new(
        abs.x + 1,
        abs.y + header,
        abs.width - 2,
        abs.height - header - 1,
    )
    .intersect(saved);
    cx.canvas.push_clip(body_clip);
    if c.count == 0 {
        let l = layout::layout(&c.empty_text, &body, LayoutOptions::single_line());
        layout::draw(
            &mut cx.canvas,
            &l,
            (abs.x + (abs.width - l.width.ceil() as i64) / 2) as f64,
            (abs.y + header + theme.px(24)) as f64,
            &body,
            theme.text_muted,
        )
    }
    let mut i = first;
    while i < end {
        let item = row(i);
        let sel = c.selected.contains(&item.key);
        let is_focus = focused && item.key == s.focus;
        let fg = if item.disabled {
            theme.text_disabled
        } else if sel {
            if focused && cx.active {
                theme.selection_text
            } else {
                theme.text
            }
        } else {
            theme.text
        };
        if c.mode == ViewMode::IconGrid {
            let (ncols, cw, ch) = grid_shape(c, &theme, r.width);
            let gx = abs.x + (i % ncols) * cw;
            let gy = abs.y + header + (i / ncols) * ch - s.scroll_y;
            let cell = Rect::new(
                gx + theme.px(4),
                gy + theme.px(4),
                cw - theme.px(8),
                ch - theme.px(8),
            );
            if sel {
                cx.canvas.round_rect(
                    cell.x,
                    cell.y,
                    cell.width,
                    cell.height,
                    theme.px(6),
                    selected_bg,
                )
            };
            let isz = theme.px(theme.shape.grid_icon);
            cx.icon(
                &item.icon.unwrap_or(Icon::File),
                cell.x + (cell.width - isz) / 2,
                cell.y + theme.px(8),
                isz,
                fg,
            );
            let mut o = LayoutOptions::wrapped((cell.width - theme.px(4)) as f64);
            o.align = TextAlign::Center;
            o.max_lines = 2;
            let l = layout::layout(&item.label, &small, o);
            layout::draw(
                &mut cx.canvas,
                &l,
                (cell.x + theme.px(2)) as f64,
                (cell.y + isz + theme.px(12)) as f64,
                &small,
                fg,
            );
            if is_focus {
                cx.canvas.round_rect_stroke(
                    cell.x,
                    cell.y,
                    cell.width,
                    cell.height,
                    theme.px(6),
                    theme.focus_width(),
                    theme.focus,
                )
            }
        } else {
            let (top, h) = row_span(c, &s, &theme, i);
            let ry = abs.y + header + top - s.scroll_y;
            if sel {
                cx.canvas.rect(abs.x + 1, ry, abs.width - 2, h, selected_bg)
            } else if i % 2 == 1 && !cols.is_empty() {
                cx.canvas.rect(
                    abs.x + 1,
                    ry,
                    abs.width - 2,
                    h,
                    theme.surface_alt.with_alpha(110),
                )
            };
            let mut x = abs.x + theme.px(10) - s.scroll_x;
            if c.tree {
                x += item.depth * theme.px(20);
                if item.expandable {
                    let isz = theme.px(theme.shape.small_icon);
                    let arrow = if item.expanded {
                        Icon::ChevronDown
                    } else if theme.rtl {
                        Icon::ChevronLeft
                    } else {
                        Icon::ChevronRight
                    };
                    cx.icon(&arrow, x, ry + (h - isz) / 2, isz, fg)
                }
                x += theme.px(theme.shape.small_icon + 6);
            }
            if let Some(own) = &item.icon {
                let isz = theme.px(theme.shape.icon);
                cx.icon(own, x, ry + (h - isz) / 2, isz, fg);
                x += theme.px(theme.shape.icon + 8);
            }
            if cols.is_empty() && c.delegate.is_some() {
                if let Some(ref d) = c.delegate {
                    paint_delegate(
                        &d(item),
                        Rect::new(x, ry, abs.right() - x - theme.px(8), h),
                        fg,
                        cx,
                    )
                }
            } else if cols.is_empty() {
                let (label_w, detail_w) = row_widths(
                    &item,
                    abs.right() - theme.px(12) - x,
                    &body,
                    &small,
                    theme.px(12),
                    theme.px(40),
                );
                let l = layout::layout(&item.label, &body, LayoutOptions::elided(label_w as f64));
                layout::draw(
                    &mut cx.canvas,
                    &l,
                    x as f64,
                    (ry + (h - l.height.ceil() as i64) / 2) as f64,
                    &body,
                    fg,
                );
                if detail_w > 0 {
                    let d = layout::layout(
                        &item.detail,
                        &small,
                        LayoutOptions::elided(detail_w as f64),
                    );
                    let muted = if sel && focused {
                        theme.selection_text
                    } else {
                        theme.text_muted
                    };
                    layout::draw(
                        &mut cx.canvas,
                        &d,
                        (abs.right() - theme.px(12) - d.width.ceil() as i64) as f64,
                        (ry + (h - d.height.ceil() as i64) / 2) as f64,
                        &small,
                        muted,
                    )
                }
            } else {
                let mut cxp = abs.x - s.scroll_x;
                for (k, (col, w)) in cols.iter().enumerate() {
                    let ci = col_index(c, &col.key);
                    let text = if ci < item.cells.len() as i64 {
                        &item.cells[ci as usize]
                    } else if ci == 0 {
                        &item.label
                    } else {
                        &String::new()
                    };
                    let editing = s.edit_row == item.key && s.edit_column == col.key;
                    let cell_clip = Rect::new(cxp, ry, w - 1, h).intersect(body_clip);
                    cx.canvas.push_clip(cell_clip);
                    if editing {
                        let e = cx.states.edit(&format!("{}#edit", c.id));
                        cx.canvas.rect(cxp + 1, ry + 1, w - 3, h - 2, theme.field);
                        cx.canvas.round_rect_stroke(
                            cxp + 1,
                            ry + 1,
                            w - 3,
                            h - 2,
                            theme.px(3),
                            theme.border_width(),
                            theme.accent,
                        );
                        let l = layout::layout(&e.text, &body, LayoutOptions::single_line());
                        let tx = cxp + theme.px(8);
                        let ty = ry + (h - l.height.ceil() as i64) / 2;
                        layout::draw(&mut cx.canvas, &l, tx as f64, ty as f64, &body, theme.text);
                        if cx.caret_on {
                            let cr = l.caret_rect(e.caret);
                            cx.canvas
                                .rect(tx + cr.x, ty + cr.y, theme.px(2), cr.height, theme.text)
                        }
                    } else {
                        let base_x = if k == 0 {
                            x - abs.x + s.scroll_x - theme.px(10)
                        } else {
                            0
                        };
                        let l = layout::layout(
                            text,
                            &body,
                            LayoutOptions::elided((w - theme.px(20) - base_x).max(1) as f64),
                        );
                        let slack = (w - theme.px(20) - base_x - l.width.ceil() as i64).max(0);
                        let tx = cxp
                            + theme.px(10)
                            + base_x
                            + match col.align {
                                TextAlign::Center => slack / 2,
                                TextAlign::End | TextAlign::Right => slack,
                                _ => 0,
                            };
                        layout::draw(
                            &mut cx.canvas,
                            &l,
                            tx as f64,
                            (ry + (h - l.height.ceil() as i64) / 2) as f64,
                            &body,
                            fg,
                        )
                    }
                    cx.canvas.pop_clip(body_clip);
                    cxp += w;
                }
            }
            if is_focus && cx.focus_visible {
                cx.canvas
                    .rect(abs.x + 1, ry, theme.focus_width(), h, theme.focus);
                cx.canvas.round_rect_stroke(
                    abs.x + 1,
                    ry,
                    abs.width - 2,
                    h,
                    0,
                    theme.border_width(),
                    theme.focus,
                )
            }
        }
        i += 1;
    }
    cx.canvas.pop_clip(cx.canvas.clip);
    cx.canvas.pop_clip(saved);
    // Scrollbar.
    let total = content_height(c, &s, &theme, r.width);
    if total > viewport.height && viewport.height > 0 {
        let b = theme.px(10);
        let thumb = (viewport.height * viewport.height / total).max(b * 2);
        let ty = abs.y
            + header
            + s.scroll_y * (viewport.height - thumb) / (total - viewport.height).max(1);
        cx.canvas.round_rect(
            abs.right() - b + b / 4,
            ty + 2,
            b / 2,
            thumb - 4,
            b / 4,
            theme.border_strong.with_alpha(170),
        )
    }
    cx.canvas.round_rect_stroke(
        abs.x,
        abs.y,
        abs.width,
        abs.height,
        theme.radius(),
        theme.border_width(),
        if focused { theme.accent } else { theme.border },
    );
    if focused {
        let rad = theme.radius();
        cx.focus_ring(r, rad)
    }
}

fn col_index(c: &Collection, key: &str) -> i64 {
    let mut i = 0;
    while i < c.columns.len() {
        if c.columns[i].key == key {
            return i as i64;
        };
        i += 1;
    }
    0
}

fn clamp_scroll(c: &Collection, s: &mut CollectionState, theme: &Theme, viewport: Size) {
    let total = content_height(c, s, theme, viewport.width);
    s.scroll_y = s.scroll_y.max(0).min((total - viewport.height).max(0));
    s.scroll_x = s.scroll_x.max(0);
}

// Scrolls so row `i` is fully visible.
fn reveal(c: &Collection, s: &mut CollectionState, theme: &Theme, viewport: Size, i: i64) {
    if i < 0 {
        return;
    };
    if c.mode == ViewMode::IconGrid {
        let (cols, _, cell) = grid_shape(c, theme, viewport.width);
        let top = (i / cols) * cell;
        if top < s.scroll_y {
            s.scroll_y = top
        };
        if top + cell > s.scroll_y + viewport.height {
            s.scroll_y = top + cell - viewport.height
        };
        return;
    }
    let (top, h) = row_span(c, s, theme, i);
    if top < s.scroll_y {
        s.scroll_y = top
    };
    if top + h > s.scroll_y + viewport.height {
        s.scroll_y = top + h - viewport.height
    }
}

fn select_index(
    c: &Collection,
    s: &mut CollectionState,
    i: i64,
    shift: bool,
    ctrl: bool,
    toggle: bool,
    cx: &mut EventCx,
) {
    if i < 0 || i >= c.count {
        return;
    };
    let row = &c.row;
    let item = row(i);
    s.focus = item.key.clone();
    s.focus_index = i;
    if ctrl && !toggle {
        cx.redraw = true;
        return;
    }
    if c.multiple && shift && !s.anchor.is_empty() {
        let a = index_of_key(c, &s.anchor, i);
        let lo = a.min(i).max(0);
        let hi = a.max(i);
        let mut keys = vec![];
        let mut k = lo;
        while k <= hi {
            let it = row(k);
            if !it.disabled {
                keys.push(it.key.clone())
            };
            k += 1;
        }
        cx.emit(&UiMsg::SelectMany(c.id.clone(), keys));
        return;
    }
    s.anchor = item.key.clone();
    if item.disabled {
        return;
    };
    if c.multiple && toggle {
        let mut next = c.selected.clone();
        if next.contains(&item.key) {
            next = next
                .iter()
                .filter(|k| **k != item.key)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            next.push(item.key)
        };
        cx.emit(&UiMsg::SelectMany(c.id.clone(), next.to_vec()))
    } else if c.multiple {
        cx.emit(&UiMsg::SelectMany(c.id.clone(), vec![item.key]))
    } else if !c.selected.contains(&item.key) {
        cx.emit(&UiMsg::Select(c.id.clone(), item.key))
    }
    cx.announce(&item.label)
}

pub fn collection_event(
    c: &Collection,
    node: &Node,
    ev: &Event,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    if cx.disabled {
        return;
    }
    let theme = cx.theme.clone();
    let mut s = with_extents(c, st.collection(&c.id), &theme);
    let header = header_height(c, &theme);
    let viewport = Size::new(node.rect.width, node.rect.height - header);
    let ox = cx.origin_x;
    let oy = cx.origin_y;
    let focus_index = index_of_key(c, &s.focus, 0);
    let row = &c.row;
    let edit_id = format!("{}#edit", c.id);
    // An open cell editor takes text and editing keys first.
    if !s.edit_row.is_empty() {
        let mut e = st.edit(&edit_id);
        let mut done = false;
        match ev {
            Event::Text(t) => {
                e.insert(t, cx.now, 0);
                cx.redraw = true;
                cx.consume()
            }
            Event::KeyDown { key, mods, .. } => {
                match key {
                    Key::Return | Key::KpEnter | Key::Tab => {
                        cx.emit(&UiMsg::CellEdit(
                            c.id.clone(),
                            s.edit_row.clone(),
                            s.edit_column.clone(),
                            e.text.to_string(),
                        ));
                        done = true;
                    }
                    Key::Escape => done = true,
                    Key::Left => e.step(-1, mods.ctrl, mods.shift),
                    Key::Right => e.step(1, mods.ctrl, mods.shift),
                    Key::Home => e.move_to(0, mods.shift),
                    Key::End => e.move_to(e.text.byte_len(), mods.shift),
                    Key::Backspace => {
                        let _ = e.backspace(mods.ctrl, cx.now);
                    }
                    Key::Delete => {
                        let _ = e.delete_forward(mods.ctrl, cx.now);
                    }
                    _ => {
                        if mods.shortcut() && *key == Key::A {
                            e.select_all()
                        }
                    }
                }
                cx.redraw = true;
                cx.consume()
            }
            Event::FocusOut | Event::PointerDown { .. } => {
                // Leaving the editor commits it, before the row could move.
                cx.emit(&UiMsg::CellEdit(
                    c.id.clone(),
                    s.edit_row.clone(),
                    s.edit_column.clone(),
                    e.text.to_string(),
                ));
                done = true;
            }
            _ => (),
        }
        st.set(&edit_id, WState::Edit(e));
        if done {
            s.edit_row = String::new();
            s.edit_column = String::new();
            st.remove(&edit_id);
            st.set(&c.id, WState::Collection(s.clone()));
            if cx.handled {
                return;
            }
        } else {
            st.set(&c.id, WState::Collection(s));
            return;
        }
    }
    match ev {
        Event::PointerDown {
            x,
            y,
            button,
            clicks,
            mods,
        } => {
            cx.focus_on(&c.id);
            let lx = x - ox;
            let ly = y - oy;
            if header > 0 && ly < header {
                // Header: resize at an edge, otherwise sort or start reordering.
                let mut cxp = -s.scroll_x;
                for (col, w) in columns_in_order(c, &s, &theme) {
                    if (lx - (cxp + w)).abs() <= theme.px(4) {
                        s.drag_kind = 1;
                        s.drag_column = col.key;
                        s.drag_start = lx;
                        s.drag_width = w;
                        cx.capture_pointer(&c.id);
                        break;
                    }
                    if lx >= cxp && lx < cxp + w {
                        s.drag_kind = 2;
                        s.drag_column = col.key;
                        s.drag_start = lx;
                        cx.capture_pointer(&c.id);
                        break;
                    }
                    cxp += w;
                }
                st.set(&c.id, WState::Collection(s));
                cx.consume();
                return;
            }
            let i = if c.mode == ViewMode::IconGrid {
                let (cols, cw, ch) = grid_shape(c, &theme, node.rect.width);
                let gi = ((ly - header + s.scroll_y) / ch.max(1)) * cols + lx / cw.max(1);
                if gi < c.count && lx / cw.max(1) < cols {
                    gi
                } else {
                    -1
                }
            } else {
                row_at(c, &s, &theme, ly - header + s.scroll_y)
            };
            if i >= 0 && *button == MouseButton::Left {
                let item = row(i);
                // Tree arrow toggles expansion.
                if c.tree && item.expandable {
                    let ax = theme.px(10) + item.depth * theme.px(20);
                    if lx >= ax && lx < ax + theme.px(theme.shape.small_icon + 6) {
                        cx.emit(&UiMsg::Expand(c.id.clone(), item.key, !item.expanded));
                        st.set(&c.id, WState::Collection(s));
                        cx.consume();
                        return;
                    }
                }
                let double = *clicks >= 2
                    || (s.last_click_key == item.key && cx.now - s.last_click_time < 450);
                s.last_click_key = item.key.clone();
                s.last_click_time = cx.now;
                if double && !item.disabled {
                    // Double click activates, or edits an editable cell.
                    let mut edited = false;
                    if !c.columns.is_empty() {
                        let mut cxp = -s.scroll_x;
                        for (col, w) in columns_in_order(c, &s, &theme) {
                            if lx >= cxp && lx < cxp + w && col.editable {
                                start_edit(c, &mut s, st, &item, &col.key);
                                edited = true;
                                cx.redraw = true;
                            }
                            cxp += w;
                        }
                    }
                    if !edited {
                        cx.emit(&UiMsg::Activate(c.id.clone(), item.key))
                    }
                } else if c.drag.is_some() && !item.disabled {
                    // Pressing a selected row keeps the selection, so all of it can be dragged.
                    let keep =
                        c.selected.contains(&item.key) && !mods.shift && !(mods.ctrl || mods.gui);
                    if !keep {
                        select_index(c, &mut s, i, mods.shift, false, mods.ctrl || mods.gui, cx)
                    };
                    s.drag_kind = 3;
                    s.drag_row = item.key;
                    s.drag_row_index = i;
                    s.drag_start = lx;
                    s.drag_row_y = ly;
                    s.select_on_release = keep;
                    cx.capture_pointer(&c.id)
                } else {
                    select_index(c, &mut s, i, mods.shift, false, mods.ctrl || mods.gui, cx)
                }
            }
            st.set(&c.id, WState::Collection(s));
            cx.consume()
        }
        Event::PointerMove { x, y, pressed, .. } => {
            if *pressed && s.drag_kind == 3 {
                let reach = theme.px(4);
                if (x - ox - s.drag_start).abs() > reach || (y - oy - s.drag_row_y).abs() > reach {
                    let keys = if c.selected.contains(&s.drag_row) && c.multiple {
                        c.selected.clone()
                    } else {
                        vec![s.drag_row.clone()]
                    };
                    if let Some(ref items) = c.drag {
                        cx.start_drag = Some((
                            c.id.clone(),
                            items(keys.to_vec()),
                            windowing::DragOperation::Copy,
                        ));
                    }
                    s.drag_kind = 0;
                    s.select_on_release = false;
                    cx.release_pointer();
                    st.set(&c.id, WState::Collection(s));
                    cx.consume()
                }
            } else if *pressed && s.drag_kind == 1 {
                let w = (s.drag_width + x - ox - s.drag_start).max(theme.px(40));
                s.widths.insert(s.drag_column.to_string(), w);
                cx.redraw = true;
                st.set(&c.id, WState::Collection(s));
                cx.consume()
            } else if s.drag_kind == 0 && header > 0 {
                cx.cursor = None;
            }
        }
        Event::PointerUp { x, .. } => {
            if s.drag_kind == 3 && s.select_on_release {
                // A press on a selected row that did not drag selects just that row.
                let i = index_of_key(c, &s.drag_row, s.drag_row_index);
                if i >= 0 {
                    select_index(c, &mut s, i, false, false, false, cx)
                }
            }
            if s.drag_kind == 1 {
                let w = match s.widths.get(&s.drag_column) {
                    Some(v) => v,
                    None => &s.drag_width,
                };
                cx.emit(&UiMsg::ColumnResize(
                    c.id.clone(),
                    s.drag_column.clone(),
                    (*w as f64 / theme.scale) as i64,
                ))
            } else if s.drag_kind == 2 {
                let moved = x - ox - s.drag_start;
                if moved.abs() < theme.px(6) {
                    let col = &c.columns[(col_index(c, &s.drag_column)) as usize];
                    if col.sortable {
                        let asc = if c.sort.0 == col.key { !c.sort.1 } else { true };
                        cx.emit(&UiMsg::Sort(c.id.clone(), col.key.clone(), asc))
                    }
                } else {
                    // Drop the column at the pointer's slot.
                    let mut order = columns_in_order(c, &s, &theme)
                        .iter()
                        .map(|p| p.0.key.clone())
                        .collect::<Vec<_>>();
                    let mut target = 0;
                    let mut cxp = -s.scroll_x;
                    let lx = x - ox;
                    for (_, w) in columns_in_order(c, &s, &theme) {
                        if lx > cxp + w / 2 {
                            target += 1
                        };
                        cxp += w;
                    }
                    order = order
                        .iter()
                        .filter(|k| **k != s.drag_column)
                        .cloned()
                        .collect::<Vec<_>>();
                    let at = target.min(order.len());
                    order.insert(at, s.drag_column.clone());
                    s.order = order;
                    cx.emit(&UiMsg::Reorder(
                        c.id.clone(),
                        s.drag_column.clone(),
                        at as i64,
                    ))
                }
            }
            if s.drag_kind != 0 {
                s.drag_kind = 0;
                cx.release_pointer()
            }
            st.set(&c.id, WState::Collection(s))
        }
        Event::Scroll { dx, dy, mods, .. } => {
            let before = s.scroll_y;
            let before_x = s.scroll_x;
            if mods.shift || *dx != 0.0 {
                s.scroll_x = (s.scroll_x
                    + ((if *dx != 0.0 { dx } else { dy }) * theme.px(48) as f64) as i64)
                    .max(0);
            } else {
                s.scroll_y -= (dy * theme.px(48) as f64) as i64;
            }
            clamp_scroll(c, &mut s, &theme, viewport);
            if s.scroll_y != before || s.scroll_x != before_x {
                cx.redraw = true;
                cx.consume()
            }
            st.set(&c.id, WState::Collection(s))
        }
        Event::KeyDown { key, mods, .. } => {
            let per_page = (viewport.height / default_row_height(c, &theme).max(1)).max(1);
            let cols = if c.mode == ViewMode::IconGrid {
                grid_shape(c, &theme, node.rect.width).0
            } else {
                1
            };
            let cur = if focus_index < 0 { -1 } else { focus_index };
            let back = if theme.rtl { Key::Right } else { Key::Left };
            let fwd = if theme.rtl { Key::Left } else { Key::Right };
            let mut target = -2;
            match key {
                Key::Down => {
                    target = if cur < 0 {
                        0
                    } else {
                        (cur + cols).min(c.count - 1)
                    }
                }
                Key::Up => target = if cur < 0 { 0 } else { (cur - cols).max(0) },
                Key::PageDown => target = (cur.max(0) + per_page * cols).min(c.count - 1),
                Key::PageUp => target = (cur.max(0) - per_page * cols).max(0),
                Key::Home => target = 0,
                Key::End => target = c.count - 1,
                Key::Return | Key::KpEnter => {
                    if cur >= 0 {
                        let item = row(cur);
                        let editable = c.columns.iter().find(|col| col.editable);
                        if mods.alt || editable.is_none() {
                            if !item.disabled {
                                cx.emit(&UiMsg::Activate(c.id.clone(), item.key))
                            }
                        } else {
                            cx.emit(&UiMsg::Activate(c.id.clone(), item.key))
                        }
                    }
                    cx.consume()
                }
                Key::F2 => {
                    if cur >= 0 {
                        for col in &c.columns {
                            if col.editable && s.edit_row.is_empty() {
                                start_edit(c, &mut s, st, &row(cur), &col.key);
                                cx.redraw = true;
                            }
                        }
                    }
                    cx.consume()
                }
                Key::Space => {
                    if cur >= 0 {
                        select_index(c, &mut s, cur, false, false, c.multiple, cx)
                    }
                    cx.consume()
                }
                _ => {
                    if c.tree && (*key == fwd || *key == back) && cur >= 0 {
                        let item = row(cur);
                        if *key == fwd {
                            if item.expandable && !item.expanded {
                                cx.emit(&UiMsg::Expand(c.id.clone(), item.key, true))
                            } else if item.expandable && cur + 1 < c.count {
                                target = cur + 1
                            }
                        } else if item.expandable && item.expanded {
                            cx.emit(&UiMsg::Expand(c.id.clone(), item.key, false))
                        } else {
                            // To the parent: the nearest earlier row one level up.
                            let mut p = cur - 1;
                            while p >= 0 && row(p).depth >= item.depth {
                                p -= 1
                            }
                            if p >= 0 {
                                target = p
                            }
                        }
                        cx.consume()
                    } else if c.mode == ViewMode::IconGrid && (*key == fwd || *key == back) {
                        target = if *key == fwd {
                            (cur + 1).min(c.count - 1)
                        } else {
                            (cur - 1).max(0)
                        };
                    } else if mods.shortcut() && *key == Key::A && c.multiple {
                        let mut keys = vec![];
                        let mut k = 0;
                        while k < c.count {
                            let it = row(k);
                            if !it.disabled {
                                keys.push(it.key.clone())
                            };
                            k += 1;
                        }
                        cx.emit(&UiMsg::SelectMany(c.id.clone(), keys));
                        cx.consume()
                    }
                }
            }
            if target >= 0 && target < c.count {
                select_index(c, &mut s, target, mods.shift, mods.ctrl, false, cx);
                reveal(c, &mut s, &theme, viewport, target);
                clamp_scroll(c, &mut s, &theme, viewport);
                cx.consume()
            }
            st.set(&c.id, WState::Collection(s))
        }
        Event::Text(t) => {
            // Typeahead over labels, starting after the focused row.
            if cx.now - s.typeahead_time > 900 {
                s.typeahead = "".to_string()
            };
            s.typeahead_time = cx.now;
            s.typeahead += t;
            let needle = crate::widgets::completion::fold(&s.typeahead);
            let mut k = 1;
            let start = focus_index.max(0);
            let n = c.count;
            let mut found = -1;
            if s.typeahead.len() > 1 {
                k = 0
            };
            while k <= n.min(100_000) && n > 0 {
                let i = (start + k) % n;
                let it = row(i);
                if !it.disabled && crate::widgets::completion::fold(&it.label).starts_with(&needle)
                {
                    found = i;
                    break;
                }
                k += 1;
            }
            if found >= 0 {
                select_index(c, &mut s, found, false, false, false, cx);
                reveal(c, &mut s, &theme, viewport, found);
                clamp_scroll(c, &mut s, &theme, viewport)
            }
            st.set(&c.id, WState::Collection(s));
            cx.consume()
        }
        Event::FocusIn => {
            if s.focus.is_empty() && c.count > 0 {
                s.focus = if c.selected.is_empty() {
                    row(0).key
                } else {
                    c.selected[0].clone()
                };
                st.set(&c.id, WState::Collection(s))
            }
            cx.redraw = true;
        }
        _ => (),
    }
}

fn start_edit(
    c: &Collection,
    s: &mut CollectionState,
    st: &mut StateStore,
    item: &Row,
    column: &str,
) {
    let ci = col_index(c, column);
    let text = if ci < item.cells.len() as i64 {
        &item.cells[ci as usize]
    } else {
        &item.label
    };
    s.edit_row = item.key.clone();
    s.edit_column = column.to_string();
    let mut e = EditState::default();
    e.sync(text);
    e.select_all();
    st.set(&format!("{}#edit", c.id), WState::Edit(e))
}

pub fn collection_semantics(
    c: &Collection,
    node: &Node,
    st: &StateStore,
    theme: &Theme,
) -> Option<AccessNode> {
    let role = if c.tree {
        Role::Tree
    } else if !c.columns.is_empty() {
        Role::Table
    } else if c.mode == ViewMode::IconGrid {
        Role::Grid
    } else {
        Role::ListBox
    };
    let mut n = AccessNode::control(role, &c.id, &c.label, "");
    n.set_size = c.count;
    let s = with_extents(c, st.collection(&c.id), theme);
    let header = header_height(c, theme);
    let viewport = Size::new(node.rect.width, node.rect.height - header);
    let (first, end) = visible_range(c, &s, theme, viewport);
    let row = &c.row;
    if !c.columns.is_empty() {
        let mut hrow = AccessNode::new(Role::Row, "");
        for col in &c.columns {
            let mut h = AccessNode::new(Role::ColumnHeader, &col.title);
            h.id = format!("{}#col:{}", c.id, col.key);
            if c.sort.0 == col.key {
                h.value = if c.sort.1 {
                    "ascending".to_string()
                } else {
                    "descending".to_string()
                }
            };
            if col.sortable {
                h.actions.push("sort".to_string())
            };
            hrow.children.push(h)
        }
        n.children.push(hrow)
    }
    // Only realised rows are exposed, each with its position in the set.
    let mut i = first;
    while i < end {
        let item = row(i);
        let item_role = if c.tree {
            Role::TreeItem
        } else if !c.columns.is_empty() {
            Role::Row
        } else {
            Role::ListItem
        };
        let mut it = AccessNode::new(item_role, &item.label);
        it.id = format!("{}#{}", c.id, item.key);
        it.selected = c.selected.contains(&item.key);
        it.focused = s.focus == item.key;
        it.disabled = item.disabled;
        it.pos_in_set = i + 1;
        it.set_size = c.count;
        if c.tree {
            it.level = item.depth + 1;
            if item.expandable {
                it.expanded = if item.expanded { 1 } else { 0 };
                it.actions.push("toggle-expanded".to_string())
            }
        }
        let (top, h) = row_span(c, &s, theme, i);
        it.bounds = Rect::new(0, header + top - s.scroll_y, node.rect.width, h);
        for cell in item.cells {
            it.children.push(AccessNode::new(Role::Cell, &cell))
        }
        it.actions.push("select".to_string());
        it.actions.push("activate".to_string());
        n.children.push(it);
        i += 1;
    }
    Some(n)
}

pub fn accessible(
    c: &Collection,
    node: &Node,
    key: &str,
    action: &str,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    if action == "sort" && key.starts_with("col:") {
        let column = key.substring(4, key.byte_len());
        for col in &c.columns {
            if col.key == column && col.sortable {
                cx.emit(&UiMsg::Sort(
                    c.id.clone(),
                    column.to_string(),
                    if c.sort.0 == column { !c.sort.1 } else { true },
                ))
            }
        }
        return;
    }
    let index = index_of_key(c, key, 0);
    if index < 0 {
        return;
    };
    let row = &c.row;
    let item = row(index);
    if item.disabled {
        return;
    };
    if action == "toggle-expanded" {
        if item.expandable {
            cx.emit(&UiMsg::Expand(
                c.id.clone(),
                key.to_string(),
                !item.expanded,
            ))
        };
        return;
    };
    let mut state = with_extents(c, st.collection(&c.id), &cx.theme);
    select_index(c, &mut state, index, false, false, false, cx);
    reveal(c, &mut state, &cx.theme, node.size(), index);
    st.set(&c.id, WState::Collection(state));
    if action == "activate" {
        cx.emit(&UiMsg::Activate(c.id.clone(), key.to_string()))
    };
    cx.redraw = true;
}

#[cfg(test)]
mod tests {
    use super::{
        Collection, CollectionState, Row, ViewMode, collection_event, layout_collection, row_at,
        row_span, visible_range, with_extents,
    };
    use crate::geometry::{Constraints, Rect, Size};

    use crate::style::Theme;
    use crate::widgets::cx::{Event, EventCx, LayoutCx, UiMsg};
    use crate::widgets::state::StateStore;
    use std::sync::atomic::{AtomicI64, Ordering};

    #[test]
    fn an_icon_grid_cell_holds_its_icon_and_two_label_lines_at_large_text() {
        for text_scale in [1.0, 2.0] {
            let theme = Theme {
                text_scale,
                ..Theme::default()
            };
            let c = big(1, false).mode(ViewMode::IconGrid);
            let (_, _, line) =
                crate::text::layout::metrics(&theme.font(crate::style::TextRole::Small));
            let cell = super::content_height(&c, &CollectionState::default(), &theme, 2000);
            assert!(
                cell >= theme.px(theme.shape.grid_icon) + 2 * line as i64,
                "{} at text size {}",
                cell,
                text_scale
            )
        }
    }

    fn big(count: i64, variable: bool) -> Collection {
        let c = Collection::new("list", count, |i| {
            Row::new(&format!("k{}", i), &format!("Row {}", i))
        })
        .revision(1, &[])
        .label("Rows")
        .empty_text("Nothing")
        .rows(8);
        if variable {
            c.height_of(|i: i64| if i % 3 == 0 { 60 } else { 30 })
        } else {
            c
        }
    }

    fn of_keys(keys: &[String], log: &crate::models::ModelLog, variable: bool) -> Collection {
        let mut c = big(keys.len() as i64, variable).revision(log.revision, &log.changes);
        let keys = keys.to_vec();
        c.row = crate::Callback::new(std::rc::Rc::new(move |i: i64| {
            let key = &keys[i as usize];
            Row::new(key, &format!("Row {key}"))
        }));
        c
    }

    fn keys(n: i64) -> Vec<String> {
        (0..n).map(|i| format!("k{}", i)).collect()
    }

    #[test]
    fn an_insert_or_removal_above_the_view_keeps_the_top_row_and_focus_in_place() {
        let theme = Theme::light();
        let h = theme.row_height();
        let mut log = crate::models::ModelLog::new();
        let mut items = keys(100);
        let mut s = super::sync(
            &of_keys(&items, &log, false),
            CollectionState::default(),
            &theme,
        );
        s.scroll_y = 10 * h + 5;
        s.focus = "k12".to_string();
        s.focus_index = 12;
        // Three rows arrive at the top: k10 stays the top row, k12 keeps focus.
        let mut grown = vec!["n0".to_string(), "n1".to_string(), "n2".to_string()];
        grown.extend(items);
        items = grown;
        log.inserted(0, 3);
        s = super::sync(&of_keys(&items, &log, false), s, &theme);
        assert_eq!(s.scroll_y, 13 * h + 5);
        assert_eq!(s.focus_index, 15);
        assert_eq!(s.focus, "k12");
        // Five rows above go: the view moves up with them.
        items = items
            .iter()
            .filter(|k| !["k0", "k1", "k2", "k3", "k4"].contains(&k.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        log.removed(3, 5);
        s = super::sync(&of_keys(&items, &log, false), s, &theme);
        assert_eq!(s.scroll_y, 8 * h + 5);
        assert_eq!(s.focus_index, 10);
        // The focused row goes: the row now in its place takes focus.
        items = items
            .iter()
            .filter(|k| *k != "k12")
            .cloned()
            .collect::<Vec<_>>();
        log.removed(10, 1);
        s = super::sync(&of_keys(&items, &log, false), s, &theme);
        assert_eq!(s.focus, "k13");
        assert_eq!(s.focus_index, 10)
    }

    #[test]
    fn a_move_carries_the_top_row_and_variable_heights_rebuild_once() {
        let theme = Theme::light();
        let mut log = crate::models::ModelLog::new();
        let mut items = keys(30);
        let mut s = super::sync(
            &of_keys(&items, &log, true),
            CollectionState::default(),
            &theme,
        );
        let (top6, _) = row_span(&of_keys(&items, &log, true), &s, &theme, 6);
        s.scroll_y = top6 + 7;
        // Rows 5..8 move to the end: k6 is still the top row, now at 27.
        let moved = vec![items[5].clone(), items[6].clone(), items[7].clone()];
        let mut rest = items
            .iter()
            .filter(|k| !moved.contains(k))
            .cloned()
            .collect::<Vec<_>>();
        for k in moved {
            rest.push(k)
        }
        items = rest;
        log.moved(5, 3, 27);
        let c = of_keys(&items, &log, true);
        s = super::sync(&c, s, &theme);
        assert_eq!(s.extents_revision, log.revision);
        let (top, _) = row_span(&c, &s, &theme, 28);
        assert_eq!(s.scroll_y, top + 7)
    }

    #[test]
    fn a_reset_or_a_revision_beyond_the_log_keeps_the_offset_clamped() {
        let theme = Theme::light();
        let h = theme.row_height();
        let mut log = crate::models::ModelLog::new();
        let mut s = super::sync(
            &of_keys(&keys(100), &log, false),
            CollectionState::default(),
            &theme,
        );
        s.scroll_y = 40 * h;
        log.reset();
        s = super::sync(&of_keys(&keys(100), &log, false), s, &theme);
        assert_eq!(s.scroll_y, 40 * h);
        // Ten rows left: the offset clamps to the content.
        let mut far = crate::models::ModelLog::new();
        far.revision = log.revision + 1000;
        s = super::sync(&of_keys(&keys(10), &far, false), s, &theme);
        assert!(s.scroll_y < 10 * h, "{}", s.scroll_y);
        assert_eq!(s.revision, far.revision)
    }

    struct Feed {
        items: Vec<String>,
        log: crate::models::ModelLog,
    }

    fn feed_update(m: Feed, msg: UiMsg) -> Feed {
        let mut n = m;
        if let UiMsg::Click(id) = msg
            && id == "arrive"
        {
            let mut grown = vec!["new0".to_string(), "new1".to_string(), "new2".to_string()];
            grown.extend(n.items);
            n.items = grown;
            n.log.inserted(0, 3)
        }
        n
    }

    fn feed_view(m: &Feed) -> crate::widgets::Widget {
        let mut c = of_keys(&m.items, &m.log, false);
        c.rows = 6;
        crate::widgets::Widget::column(
            &[
                crate::widgets::Widget::button("arrive", "New posts"),
                crate::widgets::Widget::Collection(c),
            ],
            8,
        )
        .padding(8)
    }

    fn visible_rows(s: &crate::Session) -> Vec<String> {
        let list = s.semantics(0).find("list").unwrap();
        list.children
            .iter()
            .map(|n| n.name.clone())
            .collect::<Vec<_>>()
    }

    #[test]
    fn rows_arriving_above_a_scrolled_list_leave_what_the_reader_sees_in_place() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Feed",
            "dev.gossamer.gui-feed-test",
            || Feed {
                items: keys(200),
                log: crate::models::ModelLog::new(),
            },
            feed_update,
            feed_view,
        )
        .size(480, 420)
        .start()
        .unwrap();
        let r = s.rect_of("list").unwrap();
        s.scroll_at(r.center_x(), r.center_y(), -6.0).unwrap();
        let before = visible_rows(&s);
        assert!(!before.contains(&"Row k0".to_string()), "{:?}", before);
        assert!(s.click("arrive").unwrap());
        assert_eq!(visible_rows(&s), before);
        s.shutdown()
    }

    fn delegated_view(m: &str) -> crate::widgets::Widget {
        let mut c = big(50, false);
        c.selected = if m.is_empty() {
            vec![]
        } else {
            vec![m.to_string()]
        };
        if m != "plain" {
            c = c.delegate(|row: Row| {
                crate::widgets::Widget::row(
                    &[
                        crate::widgets::Widget::label(&row.label),
                        crate::widgets::Widget::badge("New", crate::widgets::Tone::Accent, ""),
                    ],
                    8,
                )
            });
        }
        crate::widgets::Widget::Collection(c)
    }

    fn delegated_update(m: String, msg: UiMsg) -> String {
        match msg {
            UiMsg::Select(_, key) => key,
            UiMsg::Click(id) => id,
            _ => m,
        }
    }

    #[test]
    fn a_delegate_draws_row_content_while_the_row_keeps_input_and_semantics() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Delegates",
            "dev.gossamer.gui-delegate-test",
            String::new,
            delegated_update,
            |m: &String| delegated_view(m),
        )
        .size(480, 420)
        .start()
        .unwrap();
        let drawn = s.capture().unwrap();
        let names = s
            .semantics(0)
            .find("list")
            .unwrap()
            .children
            .iter()
            .map(|n| n.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(names[0], "Row 0");
        let r = s.rect_of("list").unwrap();
        let row_h = Theme::light().row_height();
        s.click_at(r.x + 40, r.y + row_h + row_h / 2).unwrap();
        assert!(
            s.semantics(0)
                .dump()
                .contains("listitem \"Row 1\" [focused selected"),
            "{}",
            s.semantics(0).dump()
        );
        s.emit(&UiMsg::Click("plain".to_string()));
        s.flush().unwrap();
        assert!(
            s.capture().unwrap().pixels != drawn.pixels,
            "the delegate drew nothing of its own"
        );
        s.shutdown()
    }

    static HEIGHTS_ASKED: AtomicI64 = AtomicI64::new(0);

    fn counted_view(_m: &i64) -> crate::widgets::Widget {
        let mut c = big(20_000, false);
        c = c.height_of(|i: i64| {
            let _ = HEIGHTS_ASKED.fetch_add(1, Ordering::Relaxed);
            if i % 2 == 0 { 30 } else { 50 }
        });
        crate::widgets::Widget::Collection(c)
    }

    #[test]
    fn variable_row_heights_are_measured_once_per_revision_not_per_frame() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::application(
            "Heights",
            "dev.gossamer.gui-heights-test",
            || 0,
            |m: i64, _msg: UiMsg| m,
            counted_view,
        )
        .size(480, 420)
        .start()
        .unwrap();
        let _ = s.capture().unwrap();
        let first = HEIGHTS_ASKED.load(Ordering::Relaxed);
        assert!(first >= 20_000, "{}", first);
        let r = s.rect_of("list").unwrap();
        s.scroll_at(r.center_x(), r.center_y(), -3.0).unwrap();
        let _ = s.capture().unwrap();
        let _ = s.capture().unwrap();
        assert_eq!(HEIGHTS_ASKED.load(Ordering::Relaxed), first);
        s.shutdown()
    }

    #[test]
    fn a_million_rows_realise_only_the_viewport() {
        let c = big(1_000_000, false);
        let s = CollectionState {
            scroll_y: 500_000 * Theme::light().row_height(),
            ..CollectionState::default()
        };
        let (first, end) = visible_range(&c, &s, &Theme::light(), Size::new(400, 440));
        assert!(end - first < 30);
        assert!(first >= 500_000 - super::OVERSCAN)
    }

    #[test]
    fn variable_heights_map_offsets_to_rows() {
        let c = big(10, true);
        let s = with_extents(&c, CollectionState::default(), &Theme::light());
        let (t3, h3) = row_span(&c, &s, &Theme::light(), 3);
        assert_eq!(h3, 60);
        assert_eq!(t3, 60 + 30 + 30);
        assert_eq!(row_at(&c, &s, &Theme::light(), t3 + 1), 3);
        assert_eq!(row_at(&c, &s, &Theme::light(), 10_000), -1)
    }

    fn draggable(selected: &[String]) -> Collection {
        big(20, false)
            .multiple(true)
            .selected(selected)
            .draggable(|keys: Vec<String>| {
                vec![windowing::DragItem {
                    mime: "text/plain".to_string(),
                    data: keys.join(",").into_bytes(),
                }]
            })
    }

    type Drag = (String, Vec<windowing::DragItem>, windowing::DragOperation);

    // Presses row `index`, moves `by` pixels with the button held, then
    // releases; answers the drag requested and the messages emitted.
    fn press_move_release(c: &Collection, index: i64, by: i64) -> (Option<Drag>, Vec<UiMsg>) {
        let theme = Theme::light();
        let mut l = LayoutCx::new(&theme, &StateStore::new(), Size::new(300, 400));
        let node = layout_collection(c, Constraints::tight(Size::new(300, 400)), &mut l);
        let mut st = StateStore::new();
        let mut cx = EventCx::new(&theme, &c.id, 0, Rect::new(0, 0, 300, 400));
        let y = index * theme.row_height() + theme.row_height() / 2;
        let none = windowing::Mods::default();
        collection_event(
            c,
            &node,
            &Event::PointerDown {
                x: 40,
                y,
                button: windowing::MouseButton::Left,
                clicks: 1,
                mods: none,
            },
            &mut st,
            &mut cx,
        );
        collection_event(
            c,
            &node,
            &Event::PointerMove {
                x: 40 + by,
                y,
                pressed: true,
                mods: none,
            },
            &mut st,
            &mut cx,
        );
        collection_event(
            c,
            &node,
            &Event::PointerUp {
                x: 40 + by,
                y,
                button: windowing::MouseButton::Left,
                mods: none,
            },
            &mut st,
            &mut cx,
        );
        (cx.start_drag, cx.messages)
    }

    #[test]
    fn dragging_a_row_offers_it_and_a_selected_row_offers_the_selection() {
        let (one, _) = press_move_release(&draggable(&[]), 2, 30);
        let Some((id, items, _)) = one else {
            panic!("the drag offered nothing");
        };
        assert_eq!(id, "list");
        assert_eq!(items[0].data, b"k2");
        let (many, _) =
            press_move_release(&draggable(&["k1".to_string(), "k2".to_string()]), 1, 30);
        let Some((_, items, _)) = many else {
            panic!("the drag offered nothing");
        };
        assert_eq!(items[0].data, b"k1,k2")
    }

    #[test]
    fn a_press_on_a_selected_row_without_moving_selects_only_that_row() {
        let (started, messages) =
            press_move_release(&draggable(&["k1".to_string(), "k2".to_string()]), 1, 0);
        assert!(started.is_none());
        assert!(
            messages.contains(&UiMsg::SelectMany(
                "list".to_string(),
                vec!["k1".to_string()]
            )) || messages.contains(&UiMsg::Select("list".to_string(), "k1".to_string()))
        )
    }
}
