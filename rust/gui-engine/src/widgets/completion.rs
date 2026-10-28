// completion: one completion controller for every control that proposes
// values (autocomplete fields, editable combos, the code editor, the
// browser address bar, the launcher, the command palette).
//
// Contract:
// - Items have stable keys; the highlight follows the key as results
//   change, never a row position.
// - Requests carry an id and the text revision they were made for.
//   Results for anything but the latest request, or for a disposed
//   widget, are dropped even if a provider ignored cancellation.
// - Local sources answer immediately; async sources are debounced and
//   bounded (`limit` items, `DOC_LIMIT` detail characters).
// - Matching normalises only comparison keys (case folding); match ranges
//   index the original label.
// - Ranking is deterministic: score, then label, then key.
// - Enter or a click accepts; Tab accepts only when `accept_tab`; Escape
//   closes without touching typed text; blur never accepts.
// - No requests run while an input method is composing.

use crate::strext::StrExt;
use crate::unicode;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;
use windowing::Key;

use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Rect, Size};
use crate::render::icons::Icon;
use crate::style::{TextRole, Theme};
use crate::text::folding::{FoldedText, folded_chars};
use crate::text::layout;
use crate::text::layout::LayoutOptions;
use crate::widgets::cx::{EventCx, PaintCx};

/// Detail text kept per item.
pub const DOC_LIMIT: i64 = 2000;

/// One proposal.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompletionItem {
    pub key: String,
    pub label: String,
    /// Text inserted on acceptance ("" inserts the label).
    pub insert: String,
    pub detail: String,
    pub icon: Option<crate::render::icons::Icon>,
    /// Decoded application icon, preferred over the built-in glyph.
    #[serde(default)]
    pub image: Option<crate::services::apps::AppIcon>,
    pub category: String,
    pub disabled: bool,
    pub score: f64,
    /// Byte ranges of `label` that matched the query.
    pub matches: Vec<(i64, i64)>,
}

impl Default for CompletionItem {
    fn default() -> CompletionItem {
        CompletionItem {
            key: String::new(),
            label: String::new(),
            insert: String::new(),
            detail: String::new(),
            icon: None,
            image: None,
            category: String::new(),
            disabled: false,
            score: 0.0,
            matches: vec![],
        }
    }
}

impl CompletionItem {
    pub fn new(key: &str, label: &str) -> CompletionItem {
        CompletionItem {
            key: key.to_string(),
            label: label.to_string(),
            ..CompletionItem::default()
        }
    }

    pub fn with_detail(&self, detail: &str) -> CompletionItem {
        let mut i = self.clone();
        i.detail = if detail.len() > DOC_LIMIT as usize {
            detail
                .substring(0, detail.byte_len().min(DOC_LIMIT * 4))
                .to_string()
        } else {
            detail.to_string()
        };
        i
    }

    pub fn inserted(&self) -> String {
        if self.insert.is_empty() {
            self.label.clone()
        } else {
            self.insert.clone()
        }
    }
}

/// How a local source matches the query against labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MatchMode {
    Prefix,
    Substring,
    /// Characters in order, not necessarily adjacent.
    Fuzzy,
}

/// Where proposals come from.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub enum Source {
    /// A fixed list matched inside the widget.
    Local(std::sync::Arc<[CompletionItem]>, MatchMode),
    /// A function answering synchronously from the query.
    Provider(crate::Callback<dyn Fn(String) -> Vec<CompletionItem>>),
    /// The application answers `UiMsg::CompletionRequest` with
    /// `Effect::Complete` after `debounce` ms.
    Async(i64),
}

impl PartialEq for Source {
    fn eq(&self, other: &Source) -> bool {
        match (self, other) {
            (Source::Local(a, m), Source::Local(b, n)) => a.len() == b.len() && m == n,
            (Source::Async(a), Source::Async(b)) => a == b,
            _ => false,
        }
    }
}

/// Completion behavior of one control.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Completion {
    pub source: Source,
    /// Characters typed before proposals appear.
    pub min_chars: i64,
    pub limit: i64,
    pub accept_tab: bool,
    /// Replace only the word at the caret (editors) instead of the whole text.
    pub word: bool,
    /// Show the top proposal's remainder as uncommitted ghost text.
    pub ghost: bool,
}

impl Completion {
    pub fn local(items: &[CompletionItem], mode: MatchMode) -> Completion {
        Completion {
            source: Source::Local(std::sync::Arc::from(items.to_vec()), mode),
            min_chars: 1,
            limit: 200,
            accept_tab: false,
            word: false,
            ghost: false,
        }
    }

    pub fn provider(f: impl Fn(String) -> Vec<CompletionItem> + 'static) -> Completion {
        Completion {
            source: Source::Provider(crate::Callback::new(std::rc::Rc::new(f))),
            min_chars: 1,
            limit: 200,
            accept_tab: false,
            word: false,
            ghost: false,
        }
    }

    pub fn asynchronous(debounce: i64) -> Completion {
        Completion {
            source: Source::Async(debounce),
            min_chars: 1,
            limit: 200,
            accept_tab: false,
            word: false,
            ghost: false,
        }
    }
}

/// Immutable proposal storage keeps state snapshots from copying a large model.
/// Its cached charge also makes session admission independent of item count.
#[derive(Debug, Clone, Default)]
pub struct SharedItems(std::sync::Arc<ItemStorage>);
#[derive(Debug, Default)]
struct ItemStorage {
    items: Vec<CompletionItem>,
    bytes: usize,
}
impl From<Vec<CompletionItem>> for SharedItems {
    fn from(items: Vec<CompletionItem>) -> Self {
        let bytes = size_of::<ItemStorage>()
            + 2 * size_of::<usize>()
            + items.capacity() * size_of::<CompletionItem>()
            + items.iter().map(CompletionItem::owned_bytes).sum::<usize>();
        Self(std::sync::Arc::new(ItemStorage { items, bytes }))
    }
}
impl std::ops::Deref for SharedItems {
    type Target = [CompletionItem];
    fn deref(&self) -> &Self::Target {
        &self.0.items
    }
}
impl PartialEq for SharedItems {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0) || self.0.items == other.0.items
    }
}
impl PartialEq<Vec<CompletionItem>> for SharedItems {
    fn eq(&self, other: &Vec<CompletionItem>) -> bool {
        &self.0.items == other
    }
}
impl<'a> IntoIterator for &'a SharedItems {
    type Item = &'a CompletionItem;
    type IntoIter = std::slice::Iter<'a, CompletionItem>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl serde::Serialize for SharedItems {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if crate::memory::measuring() {
            use serde::ser::SerializeTupleStruct;
            serializer
                .serialize_tuple_struct("__gui_owned_bytes", self.0.bytes)?
                .end()
        } else {
            self.0.items.serialize(serializer)
        }
    }
}
impl<'de> serde::Deserialize<'de> for SharedItems {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Vec::<CompletionItem>::deserialize(deserializer)?.into())
    }
}

/// Popup and result state shared by completion and choice controls.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct ChoiceState {
    pub open: bool,
    pub items: SharedItems,
    /// Unfiltered source used to reconcile an open choice after model changes.
    pub model_items: SharedItems,
    pub highlight: String,
    /// First visible row.
    pub first: i64,
    pub query: String,
    pub request: i64,
    pub revision: i64,
    pub loading: bool,
    pub error: String,
    pub typeahead: String,
    pub typeahead_time: i64,
    /// The committed key when the popup opened, for Escape rollback.
    pub rollback: String,
    /// Separate filter text for searchable pickers.
    pub filter: String,
    pub filter_caret: i64,
}

/// Case-folded comparison key.
pub fn fold(s: &str) -> String {
    unicode::fold_case(s)
}

/// Scores `label` against `query` (0 = no match) and answers the matched
/// byte ranges of `label`. Both are compared case-folded; a range covers whole
/// characters of `label` even where a query ends inside a character's folding.
pub fn match_label(label: &str, query: &str, mode: MatchMode) -> (f64, Vec<(i64, i64)>) {
    if query.is_empty() {
        return (1.0, vec![]);
    }
    let lchars: Vec<char> = label.chars().collect();
    let l = FoldedText::of_chars(label);
    let q = folded_chars(query);
    let m = q.len();
    // Folded position `k` starts a word of the label.
    let word_start = |k: i64| -> bool {
        let u = l.unit[k as usize];
        l.at_boundary(k)
            && (u == 0
                || !unicode::is_letter(lchars[(u - 1) as usize])
                || unicode::is_upper(lchars[u as usize]))
    };
    match mode {
        MatchMode::Prefix => {
            if !l.matches_at(&q, 0) {
                return (0.0, vec![]);
            };
            (
                100.0 - lchars.len() as f64 * 0.01,
                vec![l.span(0, m as i64)],
            )
        }
        MatchMode::Substring => {
            let mut s = 0;
            while s + m <= l.len() as usize {
                if l.at_boundary(s as i64)
                    && l.at_boundary((s + m) as i64)
                    && l.matches_at(&q, s as i64)
                {
                    let u = l.unit[s];
                    let bonus = if u == 0 {
                        50.0
                    } else if !unicode::is_letter(lchars[(u - 1) as usize]) {
                        25.0
                    } else {
                        0.0
                    };
                    return (
                        50.0 + bonus - u as f64 * 0.1 - lchars.len() as f64 * 0.01,
                        vec![l.span(s as i64, (s + m) as i64)],
                    );
                }
                s += 1;
            }
            (0.0, vec![])
        }
        MatchMode::Fuzzy => {
            let mut ranges: Vec<(i64, i64)> = vec![];
            let mut score = 0.0;
            let mut li = 0;
            let mut last = -2;
            for c in q {
                while li < l.len() && l.chars[li as usize] != c {
                    li += 1
                }
                if li == l.len() {
                    return (0.0, vec![]);
                };
                // Consecutive hits and word starts score higher.
                score += if li == last + 1 { 3.0 } else { 1.0 };
                if word_start(li) {
                    score += 2.0
                };
                let (start, end) = l.span(li, li + 1);
                let n = ranges.len();
                if n > 0 && ranges[n - 1].1 >= start {
                    ranges[n - 1] = (ranges[n - 1].0, end)
                } else {
                    ranges.push((start, end))
                }
                last = li;
                li += 1;
            }
            (score - lchars.len() as f64 * 0.01, ranges)
        }
    }
}

/// Filters and ranks `items` for `query`, keeping at most `limit`.
pub fn rank(
    items: &[CompletionItem],
    query: &str,
    mode: MatchMode,
    limit: i64,
) -> Vec<CompletionItem> {
    rank_cancellable(items, query, mode, limit, || true, &mut |_, _| {}).unwrap_or_default()
}

pub const RESULT_BYTE_LIMIT: usize = 64 * 1024 * 1024;

pub fn rank_cancellable(
    items: &[CompletionItem],
    query: &str,
    mode: MatchMode,
    limit: i64,
    active: impl Fn() -> bool,
    progress: &mut impl FnMut(i64, i64),
) -> Result<Vec<CompletionItem>, String> {
    if query.len() > 4096 {
        return Err("completion query exceeds its byte budget".into());
    }
    let limit = if limit <= 0 {
        100_000
    } else {
        limit.min(100_000) as usize
    };
    struct Ranked(CompletionItem);
    impl PartialEq for Ranked {
        fn eq(&self, other: &Self) -> bool {
            self.cmp(other).is_eq()
        }
    }
    impl Eq for Ranked {}
    impl PartialOrd for Ranked {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for Ranked {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            other
                .0
                .score
                .total_cmp(&self.0.score)
                .then_with(|| self.0.label.cmp(&other.0.label))
                .then_with(|| self.0.key.cmp(&other.0.key))
        }
    }
    let mut kept = std::collections::BinaryHeap::<Ranked>::new();
    let mut bytes = 0usize;
    for (i, item) in items.iter().enumerate() {
        if !active() {
            return Err("completion cancelled or expired".into());
        }
        if item.label.len() > 64 * 1024 {
            return Err("completion label exceeds its byte budget".into());
        }
        let (score, ranges) = match_label(&item.label, query, mode);
        if score > 0.0 {
            let mut matched = item.clone();
            matched.score += score;
            matched.matches = ranges;
            let cost = matched.owned_bytes() + 2 * size_of::<CompletionItem>();
            if cost > RESULT_BYTE_LIMIT {
                return Err("completion item exceeds its byte budget".into());
            }
            bytes = bytes.saturating_add(cost);
            kept.push(Ranked(matched));
            if kept.len() > limit {
                bytes -= kept.pop().unwrap().0.owned_bytes() + 2 * size_of::<CompletionItem>();
            }
            if bytes > RESULT_BYTE_LIMIT {
                return Err("completion results exceed their byte budget".into());
            }
        }
        if i % 256 == 0 {
            progress(i as i64, items.len() as i64);
        }
    }
    Ok(kept
        .into_sorted_vec()
        .into_iter()
        .map(|ranked| ranked.0)
        .collect())
}

impl CompletionItem {
    pub(crate) fn owned_bytes(&self) -> usize {
        self.key.capacity()
            + self.label.capacity()
            + self.insert.capacity()
            + self.detail.capacity()
            + self.category.capacity()
            + self.matches.capacity() * size_of::<(i64, i64)>()
            + self.image.as_ref().map_or(0, |image| image.rgba.capacity())
            + match &self.icon {
                Some(Icon::Custom(name)) => name.capacity(),
                _ => 0,
            }
    }
}

/// Sorts by score (descending), then label, then key.
pub fn sort_ranked(items: &mut [CompletionItem]) {
    // Extract a key once per item instead of calling a Gossamer comparator
    // for every comparison in a large catalog.
    items.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.key.cmp(&b.key))
    })
}

impl ChoiceState {
    /// Index of the highlighted item, or -1.
    pub fn highlight_index(&self) -> i64 {
        let mut i = 0;
        while i < self.items.len() {
            if self.items[i].key == self.highlight {
                return i as i64;
            };
            i += 1;
        }
        -1
    }

    /// Replaces the results, keeping the highlight by key when the item is
    /// still present and otherwise moving it to the first enabled item.
    pub fn set_items(&mut self, items: &[CompletionItem]) {
        self.set_shared_items(items.to_vec().into());
    }

    pub fn set_shared_items(&mut self, items: SharedItems) {
        self.items = items;
        self.loading = false;
        self.error = "".to_string();
        if self.highlight_index() < 0
            || (self.highlight_index() >= 0 && self.items[self.highlight_index() as usize].disabled)
        {
            self.highlight = "".to_string();
            for it in &self.items {
                if !it.disabled {
                    self.highlight = it.key.clone();
                    break;
                }
            }
        }
        self.first = self.first.min(self.items.len() as i64 - 1).max(0);
    }

    /// Moves the highlight `step` enabled items (wrapping at the ends
    /// unless `clamp`), keeping it visible within `rows` rows.
    pub fn move_highlight(&mut self, step: i64, rows: i64, clamp: bool) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let mut i = self.highlight_index();
        let dir = if step >= 0 { 1 } else { -1 };
        let mut remaining = step.abs().max(1);
        let mut guard = 0;
        while remaining > 0 && guard < n * 2 {
            let next = i + dir;
            if next < 0 || next >= n as i64 {
                if clamp {
                    break;
                };
                i = if next < 0 { (n - 1) as i64 } else { 0 };
            } else {
                i = next;
            }
            if !self.items[i as usize].disabled {
                remaining -= 1
            };
            guard += 1;
        }
        if i < 0 {
            i = 0
        };
        if self.items[i as usize].disabled {
            return;
        };
        self.highlight = self.items[i as usize].key.clone();
        self.reveal(i, rows)
    }

    pub fn highlight_edge(&mut self, last: bool, rows: i64) {
        let n = self.items.len();
        let n = n as i64;
        let mut i = if last { n - 1 } else { 0 };
        while i >= 0 && i < n && self.items[i as usize].disabled {
            i += if last { -1 } else { 1 };
        }
        if i >= 0 && i < n {
            self.highlight = self.items[i as usize].key.clone();
            self.reveal(i, rows)
        }
    }

    fn reveal(&mut self, i: i64, rows: i64) {
        if i < self.first {
            self.first = i
        };
        if i >= self.first + rows {
            self.first = i - rows + 1
        };
        self.first = self.first.max(0);
    }

    /// The highlighted item, when one is highlighted and enabled.
    pub fn current(&self) -> Option<CompletionItem> {
        let i = self.highlight_index();
        if self.loading || !self.error.is_empty() || i < 0 || self.items[i as usize].disabled {
            None
        } else {
            Some(self.items[i as usize].clone())
        }
    }

    /// Typeahead: the next enabled item whose label starts with the
    /// accumulated letters (reset after a pause of `pause` ms).
    pub fn typeahead_key(&mut self, c: &str, now: i64, pause: i64) -> Option<String> {
        if now - self.typeahead_time > pause {
            self.typeahead = "".to_string();
        }
        self.typeahead_time = now;
        // Repeating one letter cycles through items starting with it.
        let cycling = self.typeahead.len() == 1 && self.typeahead == c;
        if !cycling {
            self.typeahead += c;
        }
        let needle = fold(&self.typeahead);
        let n = self.items.len();
        let start = self.highlight_index();
        let mut k = 1;
        if !cycling {
            k = 0
        };
        while k <= n {
            let i = (start.max(0) + k as i64) % n.max(1) as i64;
            let item = &self.items[i as usize];
            if !item.disabled && fold(&item.label).starts_with(&needle) {
                self.highlight = item.key.clone();
                return Some(item.key.clone());
            }
            k += 1;
        }
        None
    }
}

// ---- popup list painting ----

// The widest item text by (fingerprint of the measured texts, item count,
// font size in 64ths, bold, scale, details): measuring every label is costly
// for long lists, and the labels rarely change between layouts. The
// fingerprint is a sum of per-item hashes, so reordering (ranking) the same
// items keeps it.
static WIDEST: crate::Cache<(u64, i64, i64, bool, i64, bool), f64> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// FNV-1a over the UTF-8 bytes.
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The width of the widest item's text in pixels: its label, plus its detail
/// when `details` and some item has one.
pub fn widest(items: &[CompletionItem], theme: &Theme, details: bool) -> f64 {
    let style = theme.font(TextRole::Body);
    let with_details = details && items.iter().any(|it| !it.detail.is_empty());
    let mut fingerprint: u64 = 0;
    for it in items {
        let mut h = fnv1a(&it.label);
        if with_details {
            h = (h ^ fnv1a(&it.detail)).wrapping_mul(1_099_511_628_211)
        };
        fingerprint = fingerprint.wrapping_add(h);
    }
    let key = (
        fingerprint,
        items.len() as i64,
        (style.size * 64.0) as i64,
        style.bold,
        theme.px(100),
        with_details,
    );
    if let Some(w) = WIDEST.lock().get(&key) {
        return *w;
    };
    let small = theme.font(TextRole::Small);
    let mut w = 0.0_f64;
    for it in items {
        let (tw, _) = layout::measure(&it.label, &style);
        let dw = if with_details && !it.detail.is_empty() {
            layout::measure(&it.detail, &small).0
        } else {
            0.0
        };
        w = w.max(tw + dw);
    }
    if WIDEST.lock().len() >= 256 {
        *WIDEST.lock() = HashMap::new()
    };
    WIDEST.lock().insert(key, w);
    w
}

/// Rows a popup list shows before scrolling.
pub const VISIBLE_ROWS: i64 = 8;

/// The pixel size of a popup list for `state` at least `min_width` wide.
pub fn list_size(state: &ChoiceState, theme: &Theme, min_width: i64) -> Size {
    let row = theme.row_height();
    let rows = state.items.len().min(VISIBLE_ROWS as usize)
        + if state.loading || !state.error.is_empty() {
            1
        } else {
            0
        };
    let w = min_width.max((widest(&state.items, theme, true)).ceil() as i64 + theme.px(64));
    Size::new(
        w.min(theme.px(640)),
        (rows.max(1) * row as usize + theme.px(8) as usize) as i64,
    )
}

/// Paints a popup list of `state` into `rect` (window pixels).
pub fn paint_list(
    state: &ChoiceState,
    rect: Rect,
    selected: &str,
    empty_text: &str,
    cx: &mut PaintCx,
) {
    let theme = cx.theme.clone();
    let radius = theme.radius();
    let (drop, blur) = theme.shadow_of(theme.shape.popup_elevation);
    cx.canvas.shadow(
        rect.x,
        rect.y + drop,
        rect.width,
        rect.height,
        radius,
        blur,
        theme.shadow,
    );
    cx.canvas
        .round_rect(rect.x, rect.y, rect.width, rect.height, radius, theme.field);
    cx.canvas.round_rect_stroke(
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        radius,
        theme.border_width(),
        theme.border,
    );
    let row = theme.row_height();
    let pad = theme.px(4);
    let saved = cx.canvas.clip;
    cx.canvas.push_clip(rect.inset(1));
    let mut y = rect.y + pad;
    let body = theme.font(TextRole::Body);
    let small = theme.font(TextRole::Small);
    if state.items.is_empty() && !state.loading && state.error.is_empty() {
        let l = layout::layout(empty_text, &body, LayoutOptions::single_line());
        layout::draw(
            &mut cx.canvas,
            &l,
            (rect.x + theme.px(12)) as f64,
            (y + (row - l.height.ceil() as i64) / 2) as f64,
            &body,
            theme.text_muted,
        )
    }
    let mut i = state.first.max(0);
    let end = (state.first + VISIBLE_ROWS).min(state.items.len() as i64);
    while i < end {
        let it = &state.items[i as usize];
        let lit = it.key == state.highlight;
        if lit {
            cx.canvas.round_rect(
                rect.x + pad,
                y,
                rect.width - pad * 2,
                row,
                theme.px(5),
                theme.selection,
            )
        }
        let fg = if it.disabled {
            theme.text_disabled
        } else if lit {
            theme.selection_text
        } else {
            theme.text
        };
        let mut x = rect.x + theme.px(12);
        if it.key == selected && !selected.is_empty() {
            cx.icon(
                &Icon::Check,
                x,
                y + (row - theme.px(18)) / 2,
                theme.px(18),
                fg,
            )
        }
        x += theme.px(24);
        if let Some(ref image) = it.image {
            cx.app_icon(
                image,
                Rect::new(x, y + (row - theme.px(20)) / 2, theme.px(20), theme.px(20)),
            );
            x += theme.px(28);
        } else if let Some(ref own) = it.icon {
            cx.icon(own, x, y + (row - theme.px(20)) / 2, theme.px(20), fg);
            x += theme.px(28);
        }
        let l = layout::layout(&it.label, &body, LayoutOptions::single_line());
        let ty = y + (row - l.height.ceil() as i64) / 2;
        // Matched ranges show as a band under the text, not color alone.
        for (a, b) in &it.matches {
            for r in l.selection_rects(*a, *b) {
                let band = if lit {
                    theme.selection_text.with_alpha(70)
                } else {
                    theme.accent.with_alpha(60)
                };
                cx.canvas.rect(
                    x + r.x,
                    ty + r.height - theme.px(3),
                    r.width,
                    theme.px(3),
                    band,
                )
            }
        }
        layout::draw(&mut cx.canvas, &l, x as f64, ty as f64, &body, fg);
        if !it.detail.is_empty() {
            let d = layout::layout(&it.detail, &small, LayoutOptions::single_line());
            let dx = rect.right() - theme.px(12) - d.width.ceil() as i64;
            if dx > x + l.width.ceil() as i64 + theme.px(12) {
                let muted = if lit {
                    theme.selection_text
                } else {
                    theme.text_muted
                };
                layout::draw(
                    &mut cx.canvas,
                    &d,
                    dx as f64,
                    (y + (row - d.height.ceil() as i64) / 2) as f64,
                    &small,
                    muted,
                )
            }
        }
        y += row;
        i += 1;
    }
    if state.loading || !state.error.is_empty() {
        let text = if state.loading {
            crate::localization::tr("Loading\u{2026}")
        } else {
            crate::localization::message(
                "completion",
                "{error} (Enter to retry)",
                1,
                &crate::hmap! { "error" => state.error.clone() },
            )
        };
        let tone = if state.loading {
            theme.text_muted
        } else {
            theme.danger
        };
        let l = layout::layout(&text, &small, LayoutOptions::single_line());
        layout::draw(
            &mut cx.canvas,
            &l,
            (rect.x + theme.px(12)) as f64,
            (y + (row - l.height.ceil() as i64) / 2) as f64,
            &small,
            tone,
        )
    }
    if state.items.len() > VISIBLE_ROWS as usize {
        let track = rect.height - pad * 2;
        let thumb = (track * VISIBLE_ROWS / state.items.len() as i64).max(theme.px(20));
        let ty = rect.y
            + pad
            + (track - thumb) * state.first
                / ((state.items.len() - VISIBLE_ROWS as usize).max(1)) as i64;
        cx.canvas.round_rect(
            rect.right() - theme.px(6),
            ty,
            theme.px(4),
            thumb,
            theme.px(2),
            theme.border_strong,
        )
    }
    cx.canvas.pop_clip(saved)
}

/// The item key at window point (`x`, `y`) of a popup list, or "".
pub fn list_hit(state: &ChoiceState, rect: Rect, theme: &Theme, x: i64, y: i64) -> String {
    if !rect.contains(x, y) {
        return "".to_string();
    }
    let row = theme.row_height();
    let pad = theme.px(4);
    let i = state.first + (y - rect.y - pad) / row;
    if y - rect.y - pad < 0
        || i < 0
        || i >= state.items.len() as i64
        || i >= state.first + VISIBLE_ROWS
    {
        return "".to_string();
    }
    state.items[i as usize].key.clone()
}

/// An open popup list for assistive technology: a list box `list_id` named
/// `name` holding the rows the popup shows, each with its position in the
/// whole set, its detail as its description, and the highlight as its
/// selection; the row keyed `checked` is checked.
pub fn list_semantics(
    state: &ChoiceState,
    list_id: &str,
    name: &str,
    item_prefix: &str,
    checked: &str,
) -> AccessNode {
    let mut lb = AccessNode::new(Role::ListBox, name);
    lb.id = list_id.to_string();
    lb.set_size = state.items.len() as i64;
    let mut i = state.first.max(0);
    let end = (state.first + VISIBLE_ROWS).min(state.items.len() as i64);
    while i < end {
        let it = &state.items[i as usize];
        let mut item = AccessNode::new(Role::ListItem, &it.label);
        item.id = format!("{}#{}", item_prefix, it.key);
        item.description = it.detail.clone();
        item.selected = it.key == state.highlight;
        item.checked = if !checked.is_empty() && it.key == checked {
            1
        } else {
            -1
        };
        item.disabled = it.disabled;
        item.pos_in_set = i + 1;
        item.set_size = state.items.len() as i64;
        lb.children.push(item);
        i += 1;
    }
    lb
}

/// Handles keys shared by popup lists. Answers true when it used the key.
pub fn list_key(state: &mut ChoiceState, key: Key, cx: &mut EventCx) -> bool {
    match key {
        Key::Down => {
            state.move_highlight(1, VISIBLE_ROWS, false);
            cx.redraw = true;
            true
        }
        Key::Up => {
            state.move_highlight(-1, VISIBLE_ROWS, false);
            cx.redraw = true;
            true
        }
        Key::PageDown => {
            state.move_highlight(VISIBLE_ROWS - 1, VISIBLE_ROWS, true);
            cx.redraw = true;
            true
        }
        Key::PageUp => {
            state.move_highlight(1 - VISIBLE_ROWS, VISIBLE_ROWS, true);
            cx.redraw = true;
            true
        }
        _ => false,
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = {
            match self {
                Source::Local(items, _) => format!("Local({} items)", items.len()),
                Source::Provider(_) => "Provider".to_string(),
                Source::Async(ms) => format!("Async({} ms)", ms),
            }
        };
        f.write_str(&text)
    }
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = { self.to_string() };
        f.write_str(&text)
    }
}

impl std::fmt::Display for Completion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Completion({})", self.source)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_loading_and_error_states_each_occupy_one_row() {
        let theme = crate::style::Theme::light();
        let mut state = super::ChoiceState::default();
        let one_row = theme.row_height() + theme.px(8);
        assert_eq!(super::list_size(&state, &theme, 100).height, one_row);
        state.loading = true;
        assert_eq!(super::list_size(&state, &theme, 100).height, one_row);
        state.loading = false;
        state.error = "unavailable".to_string();
        assert_eq!(super::list_size(&state, &theme, 100).height, one_row);
    }

    use super::{ChoiceState, CompletionItem, SharedItems, match_label, rank};

    #[test]
    fn choice_snapshots_share_models_and_charge_their_retained_capacity() {
        let mut item = CompletionItem::new("key", "label");
        item.detail = String::with_capacity(4096);
        let items = SharedItems::from(vec![item]);
        let cloned = items.clone();
        assert!(std::sync::Arc::ptr_eq(&items.0, &cloned.0));
        assert!(crate::memory::measure(&items) >= 4096);
        assert_eq!(
            serde_json::to_value(&items).unwrap(),
            serde_json::to_value(&items.0.items).unwrap()
        );
        let decoded: SharedItems =
            serde_json::from_value(serde_json::to_value(&items).unwrap()).unwrap();
        assert_eq!(decoded, items);
    }

    fn items(labels: &[String]) -> Vec<CompletionItem> {
        labels
            .iter()
            .enumerate()
            .map(|(i, l)| CompletionItem::new(&format!("k{i}"), l))
            .collect()
    }

    #[test]
    fn prefix_substring_and_fuzzy_match() {
        assert!(match_label("Apple", "ap", super::MatchMode::Prefix).0 > 0.0);
        assert_eq!(match_label("Apple", "pl", super::MatchMode::Prefix).0, 0.0);
        let (s, r) = match_label("Pineapple", "APP", super::MatchMode::Substring);
        assert!(s > 0.0);
        assert_eq!(r, vec![(4, 7)]);
        let (f, fr) = match_label("open_file_dialog", "ofd", super::MatchMode::Fuzzy);
        assert!(f > 0.0);
        assert_eq!(fr.len(), 3)
    }

    #[test]
    fn unicode_folding_maps_ranges_to_the_original() {
        let (s, r) = match_label("\u{C9}mile", "\u{E9}mi", super::MatchMode::Prefix);
        assert!(s > 0.0);
        assert_eq!(r, vec![(0, 4)]);
        // `İ` folds to two characters; ranges still cover whole label characters.
        let (p, pr) = match_label("\u{130}stanbul", "\u{130}st", super::MatchMode::Prefix);
        assert!(p > 0.0);
        assert_eq!(pr, vec![(0, 4)]);
        let (partial, partial_range) = match_label("\u{130}stanbul", "i", super::MatchMode::Prefix);
        assert!(partial > 0.0);
        assert_eq!(partial_range, vec![(0, 2)]);
        let (sub, sub_range) = match_label(
            "Grand \u{130}stanbul",
            "\u{130}STAN",
            super::MatchMode::Substring,
        );
        assert!(sub > 0.0);
        assert_eq!(sub_range, vec![(6, 12)]);
        // A substring may not begin or end inside one character's folding.
        assert_eq!(
            match_label("x\u{130}", "\u{307}", super::MatchMode::Substring).0,
            0.0
        );
        let (f, fr) = match_label("\u{130}zmir Ankara", "\u{130}a", super::MatchMode::Fuzzy);
        assert!(f > 0.0);
        assert_eq!(fr, vec![(0, 2), (7, 8)]);
        let (st, st_range) = match_label("Stra\u{DF}e", "STRASSE", super::MatchMode::Prefix);
        assert!(st > 0.0);
        assert_eq!(st_range, vec![(0, 7)]);
        let (mid, mid_range) =
            match_label("Gro\u{DF}e Stra\u{DF}e", "SSE", super::MatchMode::Substring);
        assert!(mid > 0.0);
        assert_eq!(mid_range, vec![(3, 6)])
    }

    #[test]
    fn ranking_is_deterministic_with_duplicate_labels() {
        let mut list = items(&["beta".to_string(), "alpha".to_string(), "alpha".to_string()]);
        list[2].key = "a2".to_string();
        let ranked = rank(&list, "", super::MatchMode::Prefix, 10);
        assert_eq!(
            ranked.iter().map(|i| i.key.clone()).collect::<Vec<_>>(),
            vec!["a2", "k1", "k0"]
        )
    }

    #[test]
    fn highlight_follows_the_key_across_results() {
        let mut s = ChoiceState::default();
        s.set_items(&items(&[
            "one".to_string(),
            "two".to_string(),
            "three".to_string(),
        ]));
        assert_eq!(s.highlight, "k0");
        s.move_highlight(1, 8, false);
        assert_eq!(s.highlight, "k1");
        let mut reordered = items(&["zero".to_string(), "one".to_string(), "two".to_string()]);
        reordered[1].key = "kx".to_string();
        reordered[2].key = "k1".to_string();
        s.set_items(&reordered);
        assert_eq!(s.highlight, "k1");
        assert_eq!(s.highlight_index(), 2)
    }

    #[test]
    fn disabled_items_are_skipped() {
        let mut list = items(&["a".to_string(), "b".to_string(), "c".to_string()]);
        list[1].disabled = true;
        let mut s = ChoiceState::default();
        s.set_items(&list);
        s.move_highlight(1, 8, false);
        assert_eq!(s.highlight, "k2");
        s.move_highlight(1, 8, false);
        assert_eq!(s.highlight, "k0")
    }

    #[test]
    fn typeahead_accumulates_and_cycles() {
        let mut s = ChoiceState::default();
        s.set_items(&items(&[
            "banana".to_string(),
            "blueberry".to_string(),
            "cherry".to_string(),
            "bread".to_string(),
        ]));
        assert_eq!(s.typeahead_key("b", 0, 1000), Some("k0".to_string()));
        assert_eq!(s.typeahead_key("l", 100, 1000), Some("k1".to_string()));
        assert_eq!(s.typeahead_key("c", 5000, 1000), Some("k2".to_string()));
        assert_eq!(s.typeahead_key("b", 9000, 1000), Some("k3".to_string()));
        assert_eq!(s.typeahead_key("b", 9100, 1000), Some("k0".to_string()))
    }

    #[test]
    fn a_hundred_thousand_items_rank_with_a_bound() {
        let mut big = vec![];
        let mut i = 0;
        while i < 100_000 {
            big.push(CompletionItem::new(
                &format!("{}", i),
                &format!("item {}", i),
            ));
            i += 1;
        }
        let r = rank(&big, "item 9999", super::MatchMode::Prefix, 50);
        assert!(r.len() <= 50);
        assert_eq!(r[0].label, "item 9999")
    }
}
