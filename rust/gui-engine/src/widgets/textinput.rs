// textinput: single-line fields, text areas, password and search fields,
// masked and validated inputs, and history recall, all over the shared
// editing controller. Typing never comes from key-down events: committed
// text arrives as `Event::Text` and composition as `Event::Composing`, so
// dead keys, AltGr, and input methods behave the platform's way.
use crate::strext::StrExt;
use crate::unicode;
use windowing::{Key, MouseButton};

use crate::accessibility::{AccessNode, Role};
use crate::editing;
use crate::editing::{EditKind, EditState, Validity};
use crate::geometry::{Constraints, Rect, Size};
use crate::render::icons::Icon;
use crate::services::clipboard;
use crate::style::{TextRole, Theme};
use crate::text::layout;
use crate::text::layout::{LayoutOptions, TextLayout, TextStyle};
use crate::text::offsets::ScalarIndex;
use crate::widgets::completion;
use crate::widgets::completion::{ChoiceState, Completion, CompletionItem, Source};
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx, Placement, PopupRequest, UiMsg};
use crate::widgets::state::{StateStore, WState};

/// What kind of text input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InputKind {
    Line,
    Area,
    Password,
    Search,
}

/// A text input. The application owns `text`; edits are reported as
/// `UiMsg::Text(id, text)`.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct TextInput {
    pub id: String,
    pub text: String,
    /// Native document handle. Positive handles report DocumentChanged and
    /// retain text/history without crossing the binding on every input event.
    #[serde(default)]
    pub document: i64,
    #[serde(default)]
    pub rich_document: i64,
    #[serde(default)]
    pub rich_block: i64,
    pub placeholder: String,
    pub kind: InputKind,
    /// Visible lines of a text area.
    pub rows: i64,
    pub read_only: bool,
    /// Most graphemes accepted (0 = unlimited).
    pub max_length: i64,
    /// Input mask: `9` digit, `a` letter, `*` any; other characters are
    /// inserted literally.
    pub mask: String,
    /// Judges the draft; invalid drafts stay editable and are marked.
    pub validate: Option<crate::Callback<dyn Fn(String) -> Validity>>,
    /// An error to show under the field (from the application).
    pub error: String,
    pub help: String,
    /// Accessible name (defaults to the placeholder).
    pub label: String,
    /// Previous entries recalled with Up/Down (oldest first).
    pub history: Vec<String>,
    pub completion: Option<Completion>,
    /// Optional leading content; completion navigation previews the highlighted item.
    #[serde(default)]
    pub leading_icon: Option<Icon>,
    #[serde(default)]
    pub leading_image: Option<crate::services::apps::AppIcon>,
    /// Source spans for the current committed text, in UTF-8 bytes.
    pub syntax: Vec<crate::text::syntax::Span>,
    pub mono: bool,
    /// Soft-wrap a text area (otherwise it scrolls horizontally).
    pub wrap: bool,
    /// Tab inserts a tab character in a text area instead of moving focus.
    pub tab_inserts: bool,
    pub required: bool,
    /// A dropdown button lists every completion item (an editable combo).
    pub dropdown: bool,
    /// Only completion items are accepted: leaving the field with other
    /// text restores the last accepted item.
    pub strict: bool,
}

impl PartialEq for TextInput {
    fn eq(&self, other: &TextInput) -> bool {
        self.id == other.id
            && self.text == other.text
            && self.document == other.document
            && self.rich_document == other.rich_document
            && self.rich_block == other.rich_block
            && self.placeholder == other.placeholder
            && self.kind == other.kind
            && self.rows == other.rows
            && self.read_only == other.read_only
            && self.error == other.error
            && self.help == other.help
            && self.label == other.label
            && self.mask == other.mask
            && self.max_length == other.max_length
            && self.leading_icon == other.leading_icon
            && self.leading_image == other.leading_image
    }
}

impl TextInput {
    pub fn new(id: &str, text: &str, placeholder: &str, kind: InputKind) -> TextInput {
        TextInput {
            id: id.to_string(),
            text: text.to_string(),
            document: 0,
            rich_document: 0,
            rich_block: 0,
            placeholder: placeholder.to_string(),
            kind,
            rows: 4,
            read_only: false,
            max_length: 0,
            mask: String::new(),
            validate: None,
            error: String::new(),
            help: String::new(),
            label: String::new(),
            history: vec![],
            completion: None,
            leading_icon: None,
            leading_image: None,
            syntax: vec![],
            mono: false,
            wrap: true,
            tab_inserts: false,
            required: false,
            dropdown: false,
            strict: false,
        }
    }

    pub fn multiline(&self) -> bool {
        self.kind == InputKind::Area
    }
}

fn style_of(t: &TextInput, theme: &Theme) -> TextStyle {
    if t.mono {
        theme.font(TextRole::Mono)
    } else {
        theme.font(TextRole::Body)
    }
}

fn trailing_button(t: &TextInput) -> bool {
    (t.kind == InputKind::Search && !t.text.is_empty())
        || t.kind == InputKind::Password
        || t.dropdown
}

fn caption(t: &TextInput, validity: &str) -> String {
    if !t.error.is_empty() {
        return t.error.clone();
    };
    if !validity.is_empty() {
        return validity.to_string();
    };
    t.help.clone()
}

/// The field box, inner text rectangle, and trailing button rectangle.
fn geometry(t: &TextInput, theme: &Theme, width: i64) -> (Rect, Rect, Rect) {
    let pad = theme.padding();
    let style = style_of(t, theme);
    let (_, _, lh) = layout::metrics(&style);
    let box_h = if t.multiline() {
        (lh * t.rows.max(1) as f64) as i64 + pad * 2
    } else {
        theme.control_height()
    };
    let field = Rect::new(0, 0, width, box_h);
    let button = if trailing_button(t) {
        theme.control_height()
    } else {
        0
    };
    let inner_h = if t.multiline() {
        box_h - pad * 2
    } else {
        lh as i64
    };
    let inner_y = if t.multiline() {
        pad
    } else {
        (box_h - inner_h) / 2
    };
    let leading = if t.leading_icon.is_some() || t.leading_image.is_some() {
        theme.px(28)
    } else {
        0
    };
    let inner = Rect::new(
        pad + leading,
        inner_y,
        (width - pad * 2 - button - leading).max(1),
        inner_h,
    );
    let btn = if button > 0 {
        Rect::new(width - button, 0, button, box_h)
    } else {
        Rect::new(0, 0, 0, 0)
    };
    (field, inner, btn)
}

pub fn layout_input(t: &TextInput, c: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    let natural_w = theme.px(if t.multiline() { 420 } else { 280 });
    let width = if c.min_width > 0 && c.min_width == c.max_width {
        c.max_width
    } else {
        natural_w.min(c.max_width).max(c.min_width)
    };
    let (mut field, mut inner, btn) = geometry(t, theme, width);
    let validity = cx.states.field(&t.id, "validity", "");
    let cap = caption(t, &validity);
    let mut cap_h = 0;
    if !cap.is_empty() {
        let small = theme.font(TextRole::Small);
        let l = layout::layout(&cap, &small, LayoutOptions::wrapped(width as f64));
        cap_h = theme.px(4) + l.height.ceil() as i64;
    }
    // A text area given more height than its rows fills it.
    if t.multiline()
        && c.min_height > field.height + cap_h
        && c.min_height < crate::geometry::UNBOUNDED
    {
        let extra = c.min_height - field.height - cap_h;
        field.height += extra;
        inner.height += extra;
    }
    // A text area given less height than its rows shrinks, down to one line,
    // and scrolls its text.
    if t.multiline() && field.height + cap_h > c.max_height {
        let line = layout::metrics(&style_of(t, theme)).2.ceil() as i64;
        let less = (field.height + cap_h - c.max_height).min((inner.height - line).max(0));
        field.height -= less;
        inner.height -= less;
    }
    let mut h = field.height;
    let mut cap_rect = Rect::new(0, 0, 0, 0);
    if cap_h > 0 {
        cap_rect = Rect::new(0, h + theme.px(4), width, cap_h - theme.px(4));
        h = cap_rect.bottom();
    }
    let mut node = Node::leaf(c.constrain(Size::new(width, h)));
    node.parts = vec![field, inner, btn, cap_rect];
    let (a, _, _) = layout::metrics(&style_of(t, theme));
    node.baseline = inner.y + a as i64;
    node
}

/// The text layout of what the field shows (bullets for passwords,
/// preedit spliced in) and the preedit byte range in it.
fn shown_layout(
    t: &TextInput,
    st: &EditState,
    theme: &Theme,
    inner_w: i64,
    inner_h: i64,
) -> (std::sync::Arc<TextLayout>, i64, i64) {
    if t.rich_document > 0
        && st.preedit.is_empty()
        && let Ok(snapshot) = crate::rich::registry::snapshot(t.rich_document)
        && let Some(block) = snapshot.content.find(t.rich_block)
        && block.text.same_revision(&st.text)
    {
        let options = if t.multiline() && t.wrap {
            LayoutOptions::wrapped(inner_w as f64)
        } else {
            LayoutOptions::single_line()
        };
        let source = st.text.to_string();
        let full = crate::rich::layout_cache::get(
            &t.id,
            &source,
            &block.spans,
            style_of(t, theme),
            options,
        );
        crate::rich::layout_cache::remember_view(
            &crate::app::effects::current_budget(),
            &t.id,
            inner_w,
            inner_h,
            theme.px(4),
        );
        if !full.lines.is_empty() && full.error.is_none() {
            return (full, -1, -1);
        }
        let (fallback, a, b) = viewport_layout(t, st, theme, inner_w, inner_h);
        return (std::sync::Arc::new(fallback), a, b);
    }
    if st.text.len() > 32 * 1024 && t.kind != InputKind::Password {
        if st.preedit.is_empty() {
            let style = style_of(t, theme);
            let options = if t.multiline() && t.wrap {
                LayoutOptions::wrapped(inner_w as f64)
            } else {
                LayoutOptions::single_line()
            };
            if let Some(view) = crate::text::document_layout::viewport(
                &t.id,
                st,
                style,
                options,
                Rect::new(st.scroll_x as i64, st.scroll_y as i64, inner_w, inner_h),
            ) {
                return (std::sync::Arc::new(view), -1, -1);
            }
        }
        let (layout, a, b) = viewport_layout(t, st, theme, inner_w, inner_h);
        return (std::sync::Arc::new(layout), a, b);
    }
    let (shown, pa, pb) = st.display();
    let display = if t.kind == InputKind::Password && !st.revealed {
        bullets(&shown)
    } else {
        shown
    };
    let style = style_of(t, theme);
    let opts = if t.multiline() && t.wrap {
        LayoutOptions::wrapped(inner_w as f64)
    } else {
        LayoutOptions::single_line()
    };
    (
        std::sync::Arc::new(layout::layout(&display, &style, opts)),
        pa,
        pb,
    )
}

// Shape only visible logical lines. The rope supplies line/byte indexes in
// logarithmic time; unchanged paragraphs continue to hit the shared shape cache.
fn viewport_layout(
    t: &TextInput,
    st: &EditState,
    theme: &Theme,
    inner_w: i64,
    inner_h: i64,
) -> (TextLayout, i64, i64) {
    let style = style_of(t, theme);
    let (ascent, descent, height) = layout::metrics(&style);
    let first = ((st.scroll_y / height.max(1.0)).floor().max(0.0) as usize).saturating_sub(2);
    let rows = ((inner_h.max(1) as f64 / height.max(1.0)).ceil() as usize).clamp(4, 256) + 4;
    let last = (first + rows).min(st.text.line_count());
    let mut out = TextLayout {
        ascent,
        descent,
        line_height: height,
        height: st.text.line_count() as f64 * height,
        source_length: st.text.byte_len(),
        ..TextLayout::default()
    };
    let mut pa = -1;
    let mut pb = -1;
    let mut next_top = first as f64 * height;
    for line in first..last {
        let line_start = st.text.line_start(line);
        let line_end = st.text.line_start(line + 1);
        let caret = st.caret.max(0) as usize;
        let mut start = line_start;
        let mut end = line_end;
        if end - start > 4096 {
            let wanted = if caret >= start && caret <= end {
                caret
            } else {
                start + (st.scroll_x / (style.size * 0.6).max(1.0)) as usize
            };
            start = st.text.snap(wanted.saturating_sub(512).max(line_start));
            end = st.text.snap((start + 4096).min(line_end));
        }
        let mut text = st.text.slice(start, end);
        while text.ends_with(['\n', '\r']) {
            text.pop();
        }
        if !st.preedit.is_empty() && caret >= start && caret <= end {
            let (a, b) = st.selection();
            if a >= start as i64 && b <= (start + text.len()) as i64 {
                text.replace_range((a as usize - start)..(b as usize - start), &st.preedit);
                pa = a;
                pb = a + st.preedit.len() as i64;
            }
        }
        let opts = if t.wrap {
            LayoutOptions::wrapped(inner_w as f64)
        } else {
            LayoutOptions::single_line()
        };
        let mut part = layout::layout(&text, &style, opts);
        let top = next_top;
        next_top += part.height;
        out.height = out.height.max(next_top);
        let left = if start > line_start {
            let space = layout::layout(" ", &style, LayoutOptions::single_line()).width;
            (st.text.scalar_of(start) - st.text.scalar_of(line_start)) as f64 * space
        } else {
            0.0
        };
        out.width = out.width.max(left + part.width);
        for visual in &mut part.lines {
            visual.start += start as i64;
            visual.end += start as i64;
            visual.top += top;
            visual.baseline += top;
            visual.left += left;
            for glyph in &mut visual.glyphs {
                glyph.start += start as i64;
                glyph.end += start as i64;
            }
        }
        out.lines.extend(part.lines);
        out.boundaries
            .extend(part.boundaries.into_iter().map(|byte| byte + start as i64));
    }
    out.boundaries.sort_unstable();
    out.boundaries.dedup();
    (out, pa, pb)
}

// One bullet per grapheme, so caret offsets map through `bullet_offset`.
fn bullets(text: &str) -> String {
    let mut out = String::new();
    for _ in unicode::graphemes(text) {
        out.push('\u{2022}')
    }
    out
}

// Maps a byte offset in the real text to the bullet text and back.
fn to_shown(t: &TextInput, st: &EditState, offset: i64) -> i64 {
    if t.kind != InputKind::Password || st.revealed {
        return offset;
    }
    let (shown, _, _) = st.display();
    unicode::grapheme_count(shown.substring(0, offset.min(shown.byte_len()))) * 3
}

fn from_shown(t: &TextInput, st: &EditState, offset: i64) -> i64 {
    if t.kind != InputKind::Password || st.revealed {
        return offset;
    }
    let n = offset / 3;
    let mut at = 0;
    for (k, g) in unicode::graphemes(&st.text).enumerate() {
        if k as i64 >= n {
            break;
        };
        at += g.byte_len();
    }
    at
}

fn state_of(t: &TextInput, states: &StateStore) -> EditState {
    let mut state = if t.rich_document > 0 {
        crate::rich::editor::state(t.rich_document, t.rich_block, states.edit(&t.id))
            .unwrap_or_else(|_| states.edit(&t.id))
    } else if t.document > 0 {
        editing::documents::state(t.document).unwrap_or_else(|_| states.edit(&t.id))
    } else {
        let mut state = states.edit(&t.id);
        state.sync(&t.text);
        state
    };
    if state.layout_key == 0 {
        state.layout_key = state.text.identity().wrapping_mul(2).wrapping_add(1);
    }
    if t.rich_document > 0 {
        crate::rich::layout_cache::adjust_state(
            &state.text,
            &mut state.scroll_x,
            &mut state.scroll_y,
            state.caret,
            state.follow_caret,
            &states.budget(),
            &t.id,
        );
    } else {
        crate::text::document_layout::adjust_state(&mut state);
    }
    state
}

// Preview selection without accepting it or changing the user's query.
fn leading_content(
    t: &TextInput,
    states: &StateStore,
) -> (Option<crate::services::apps::AppIcon>, Option<Icon>) {
    let choice = states.choice(&format!("{}#completion", t.id));
    if t.completion.is_some()
        && choice.open
        && let Some(item) = choice.current()
    {
        return (item.image, item.icon.or(t.leading_icon.clone()));
    }
    (t.leading_image.clone(), t.leading_icon.clone())
}

pub fn paint_input(t: &TextInput, node: &Node, cx: &mut PaintCx) {
    if node.parts.len() < 4 {
        return;
    }
    let visible = cx
        .abs(Rect::from_size(node.size()))
        .intersect(cx.canvas.clip);
    if visible.width <= 0 || visible.height <= 0 {
        return;
    }
    let theme = cx.theme.clone();
    let field = node.parts[0];
    let inner = node.parts[1];
    let btn = node.parts[2];
    let cap_rect = node.parts[3];
    let st = state_of(t, &cx.states);
    let focused = cx.is_focused(&t.id);
    let validity = cx.states.field(&t.id, "validity", "");
    let invalid = !t.error.is_empty() || !validity.is_empty();
    let radius = theme.radius();
    let bg = if t.read_only || cx.disabled {
        theme.surface_alt
    } else {
        theme.field
    };
    cx.fill_round(field, radius, bg);
    let edge = if invalid {
        theme.danger
    } else if focused {
        theme.accent
    } else {
        theme.border_strong.with_alpha(180)
    };
    let ew = if focused || invalid {
        theme.border_width() * 2
    } else {
        theme.border_width()
    };
    cx.stroke_round(field, radius, ew, edge);
    if t.leading_icon.is_some() || t.leading_image.is_some() {
        let size = theme.px(20);
        let area = cx.abs(field);
        let rect = Rect::new(
            area.x + theme.padding(),
            area.y + (area.height - size) / 2,
            size,
            size,
        );
        let (image, glyph) = leading_content(t, &cx.states);
        if let Some(image) = image {
            cx.app_icon(&image, rect);
        } else if let Some(glyph) = glyph {
            cx.icon(&glyph, rect.x, rect.y, size, theme.text_muted);
        }
    }
    let abs_inner = cx.abs(inner);
    let (tl, pa, pb) = shown_layout(t, &st, &theme, inner.width, inner.height);
    let style = style_of(t, &theme);
    let ox = abs_inner.x as f64 - st.scroll_x;
    let oy = abs_inner.y as f64 - st.scroll_y;
    let saved = cx.canvas.clip;
    cx.canvas.push_clip(abs_inner.intersect(saved));
    let empty = st.text.is_empty() && st.preedit.is_empty();
    if empty && !t.placeholder.is_empty() {
        let pl = layout::layout(&t.placeholder, &style, LayoutOptions::single_line());
        layout::draw(
            &mut cx.canvas,
            &pl,
            abs_inner.x as f64,
            abs_inner.y as f64,
            &style,
            theme.text_muted,
        )
    }
    // Selection under the text.
    if st.has_selection() {
        let (a, b) = st.selection();
        let sel = if focused && cx.active {
            theme.selection
        } else {
            theme.selection_inactive
        };
        for r in tl.selection_rects(to_shown(t, &st, a), to_shown(t, &st, b)) {
            cx.canvas
                .rect(ox as i64 + r.x, oy as i64 + r.y, r.width, r.height, sel)
        }
    }
    let fg = if cx.disabled {
        theme.text_disabled
    } else {
        theme.text
    };
    let native_analysis = editing::documents::paint_analysis(t.document);
    let native_spans = native_analysis.as_ref().map(|analysis| {
        crate::text::document_syntax::paint_spans(t.document, &t.id, analysis, &tl)
    });
    let source_spans = native_spans.as_deref().unwrap_or(t.syntax.as_slice());
    let first_byte = tl.lines.first().map_or(0, |line| line.start);
    let last_byte = tl.lines.last().map_or(0, |line| line.end);
    let begin = source_spans.partition_point(|span| span.end <= first_byte);
    let end = source_spans.partition_point(|span| span.start <= last_byte);
    let visible_spans = &source_spans[begin.min(end)..end];
    let rich = if t.rich_document > 0 && st.preedit.is_empty() {
        crate::rich::registry::snapshot(t.rich_document).ok()
    } else {
        None
    };
    if let Some(block) = rich
        .as_ref()
        .and_then(|snapshot| snapshot.content.find(t.rich_block))
    {
        layout::draw_rich(
            &mut cx.canvas,
            &tl,
            ox,
            oy,
            &style,
            fg,
            &block.spans,
            t.rich_document,
            &theme,
        );
    } else if !visible_spans.is_empty()
        && st.preedit.is_empty()
        && t.kind != InputKind::Password
        && !cx.disabled
    {
        layout::draw_syntax(
            &mut cx.canvas,
            &tl,
            ox,
            oy,
            &style,
            fg,
            visible_spans,
            &theme,
        )
    } else {
        layout::draw(&mut cx.canvas, &tl, ox, oy, &style, fg)
    }
    if st.has_selection() && focused && cx.active {
        let (a, b) = st.selection();
        for r in tl.selection_rects(to_shown(t, &st, a), to_shown(t, &st, b)) {
            let inner_saved = cx.canvas.clip;
            cx.canvas.push_clip(Rect::new(
                ox as i64 + r.x,
                oy as i64 + r.y,
                r.width,
                r.height,
            ));
            layout::draw(&mut cx.canvas, &tl, ox, oy, &style, theme.selection_text);
            cx.canvas.pop_clip(inner_saved)
        }
    }
    // Composition: underline the preedit range.
    if pa >= 0 {
        for r in tl.selection_rects(to_shown(t, &st, pa), to_shown(t, &st, pb)) {
            cx.canvas.rect(
                ox as i64 + r.x,
                oy as i64 + r.bottom() - theme.px(2),
                r.width,
                theme.px(2),
                theme.accent,
            )
        }
    }
    // Ghost completion text after the caret.
    let cs = cx.states.choice(&format!("{}#completion", t.id));
    if let Some(ref comp) = t.completion
        && comp.ghost
        && focused
        && cs.open
        && !st.has_selection()
        && st.caret == st.text.byte_len()
        && let Some(item) = cs.current()
    {
        let ins = item.inserted();
        if ins.byte_len() > st.text.byte_len() && fold_prefix(&ins, &st.text) {
            let rest = ins.substring(st.text.byte_len(), ins.byte_len());
            let gl = layout::layout(rest, &style, LayoutOptions::single_line());
            let gx = ox + tl.caret_x(tl.text.byte_len());
            layout::draw(&mut cx.canvas, &gl, gx, oy, &style, theme.text_muted)
        }
    }
    // Caret.
    if focused && cx.caret_on && !t.read_only && !cx.disabled {
        let caret_at = if pa >= 0 {
            pa + st.preedit_caret
        } else {
            st.caret
        };
        let shown_caret = if pa >= 0 {
            caret_at
        } else {
            to_shown(t, &st, caret_at)
        };
        let r = tl.caret_rect(shown_caret);
        cx.canvas.rect(
            ox as i64 + r.x,
            oy as i64 + r.y,
            theme.px(2).max(1),
            r.height,
            theme.text,
        )
    }
    cx.canvas.pop_clip(saved);
    if btn.width > 0 {
        let a = cx.abs(btn);
        let s = theme.px(18);
        let glyph = if t.dropdown {
            Icon::ChevronDown
        } else if t.kind == InputKind::Search {
            Icon::Close
        } else if st.revealed {
            Icon::EyeOff
        } else {
            Icon::Eye
        };
        let color = if cx.is_hover(&t.id) {
            theme.text
        } else {
            theme.text_muted
        };
        cx.icon(
            &glyph,
            a.x + (a.width - s) / 2,
            a.y + (a.height - s) / 2,
            s,
            color,
        )
    }
    if t.kind == InputKind::Search && btn.width == 0 && empty {
        let a = cx.abs(field);
        let s = theme.px(18);
        cx.icon(
            &Icon::Search,
            a.right() - s - theme.padding(),
            a.y + (a.height - s) / 2,
            s,
            theme.text_muted,
        )
    }
    if cap_rect.height > 0 {
        let cap = caption(t, &validity);
        let small = theme.font(TextRole::Small);
        let l = layout::layout(&cap, &small, LayoutOptions::wrapped(cap_rect.width as f64));
        let a = cx.abs(cap_rect);
        let color = if invalid {
            theme.danger
        } else {
            theme.text_muted
        };
        layout::draw(&mut cx.canvas, &l, a.x as f64, a.y as f64, &small, color)
    }
    if focused {
        cx.focus_ring(field, radius)
    }
}

fn fold_prefix(full: &str, prefix: &str) -> bool {
    completion::fold(full).starts_with(&completion::fold(prefix))
}

// Keeps the caret inside the visible text rectangle.
fn scroll_to_caret(t: &TextInput, s: &mut EditState, theme: &Theme, inner: Rect) {
    if s.text.len() > 32 * 1024 && t.multiline() {
        let (_, _, height) = layout::metrics(&style_of(t, theme));
        let y = s.text.line_of(s.caret.max(0) as usize) as f64 * height;
        if y < s.scroll_y {
            s.scroll_y = y;
        }
        if y + height > s.scroll_y + inner.height as f64 {
            s.scroll_y = (y + height - inner.height as f64).max(0.0);
        }
    }
    let (tl, _, _) = shown_layout(t, s, theme, inner.width, inner.height);
    let r = tl.caret_rect(to_shown(t, s, s.caret));
    let margin = theme.px(4) as f64;
    let x = r.x as f64;
    if x - s.scroll_x > inner.width as f64 - margin {
        s.scroll_x = x - inner.width as f64 + margin;
    }
    if x - s.scroll_x < 0.0 {
        s.scroll_x = (x - margin).max(0.0);
    }
    if t.multiline() {
        let y = r.y as f64;
        if y + r.height as f64 - s.scroll_y > inner.height as f64 {
            s.scroll_y = y + r.height as f64 - inner.height as f64;
        }
        if y - s.scroll_y < 0.0 {
            s.scroll_y = y;
        }
        let max_y = (tl.height - inner.height as f64).max(0.0);
        s.scroll_y = s.scroll_y.max(0.0).min(max_y);
    } else {
        s.scroll_y = 0.0;
        let max_x = (tl.width - inner.width as f64 + margin).max(0.0);
        s.scroll_x = s.scroll_x.max(0.0).min(max_x);
    }
}

// Applies the mask and reports a text change.
fn changed(t: &TextInput, s: &mut EditState, st: &mut StateStore, cx: &mut EventCx) {
    if !t.mask.is_empty() {
        let mut raw = String::new();
        for c in s.text.chars() {
            if !t.mask.contains(c) || c == '9' || c == 'a' || c == '*' {
                raw.push(c)
            }
        }
        let (masked, _ok) = editing::apply_mask(&t.mask, &raw);
        if masked != s.text {
            s.text = masked.into();
            s.caret = s.text.byte_len();
            s.anchor = s.caret;
        }
    }
    let mut validity = String::new();
    if let Some(ref v) = t.validate {
        validity = match v(s.text.to_string()) {
            Validity::Valid | Validity::Intermediate(_) => "".to_string(),
            Validity::Invalid(why) => why,
        };
    }
    st.set_field(&t.id, "validity", &validity);
    if t.rich_document > 0 {
        match crate::rich::editor::commit(t.rich_document, t.rich_block, s) {
            Ok(true) => cx.emit(&UiMsg::RichChanged(
                t.id.clone(),
                t.rich_document,
                s.revision,
            )),
            Ok(false) => (),
            Err(error) => {
                st.set_field(&t.id, "validity", &error);
                if let Ok(previous) =
                    crate::rich::editor::state(t.rich_document, t.rich_block, s.clone())
                {
                    *s = previous;
                }
            }
        }
    } else if t.document > 0 {
        let expected =
            editing::documents::info(t.document).map_or(s.revision - 1, |info| info.revision);
        match editing::documents::commit(t.document, expected, s.clone()) {
            Ok(info) => cx.emit(&UiMsg::DocumentChanged(
                t.id.clone(),
                t.document,
                info.revision,
            )),
            Err(error) => {
                st.set_field(&t.id, "validity", &error);
                if let Ok(previous) = editing::documents::state(t.document) {
                    *s = previous;
                }
            }
        }
    } else {
        cx.emit(&UiMsg::Text(t.id.clone(), s.text.to_string()));
    }
    s.history_index = -1;
}

fn clear_text_drag(id: &str, st: &mut StateStore) {
    st.set_field(id, "drag-x", "");
    st.set_field(id, "drag-y", "");
    st.set_field(id, "drag-at", "")
}

fn inner_rect(node: &Node) -> Rect {
    if node.parts.len() >= 2 {
        node.parts[1]
    } else {
        Rect::from_size(node.size())
    }
}

fn field_rect(node: &Node) -> Rect {
    if !node.parts.is_empty() {
        node.parts[0]
    } else {
        Rect::from_size(node.size())
    }
}

pub fn input_event(t: &TextInput, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    let mutation = match ev {
        Event::Text(text) | Event::ReplaceRange { text, .. } | Event::Drop { text, .. } => {
            Some(text.len())
        }
        Event::Composing { text, .. } => Some(text.len()),
        Event::Command(command)
            if matches!(
                command.as_str(),
                "edit.paste" | "edit.cut" | "edit.delete" | "edit.undo" | "edit.redo"
            ) =>
        {
            Some(if command == "edit.paste" {
                16 * 1024 * 1024
            } else {
                0
            })
        }
        Event::KeyDown { key, mods, .. }
            if matches!(
                key,
                Key::Backspace | Key::Delete | Key::Return | Key::KpEnter | Key::Tab
            ) || mods.shortcut() && matches!(key, Key::V | Key::X | Key::Z | Key::Y) =>
        {
            Some(if *key == Key::V { 16 * 1024 * 1024 } else { 4 })
        }
        _ => None,
    };
    let _temporary = if let Some(bytes) = mutation {
        match st
            .budget()
            .reserve(bytes.saturating_mul(4).saturating_add(128 * 1024))
        {
            Ok(lease) => Some(lease),
            Err(error) => {
                st.set_field(&t.id, "validity", &error);
                cx.redraw = true;
                cx.consume();
                return;
            }
        }
    } else {
        None
    };
    let mut s = state_of(t, st);
    s.changes.clear();
    let theme = cx.theme.clone();
    let inner = inner_rect(node);
    let abs_inner = cx.to_window(inner);
    let field_abs = cx.to_window(field_rect(node));
    let single = !t.multiline();
    let (tl, _, _) = shown_layout(t, &s, &theme, inner.width, inner.height);
    let local = |x: i64, y: i64| -> (f64, f64) {
        (
            (x - abs_inner.x) as f64 + s.scroll_x,
            (y - abs_inner.y) as f64 + s.scroll_y,
        )
    };
    let now = cx.now;
    let editable = !t.read_only && !cx.disabled;
    let text_before = s.text.clone();
    let selection_before = (s.anchor, s.caret);
    let mut moved = false;
    if cx.disabled {
        return;
    }
    match ev {
        Event::SetSelection { anchor, caret } => {
            s.anchor = s.text.snap(*anchor.max(&0) as usize) as i64;
            s.caret = s.text.snap(*caret.max(&0) as usize) as i64;
            s.goal_x = -1.0;
            moved = true;
            cx.consume()
        }
        Event::ReplaceRange { start, end, text } => {
            if !editable {
                return;
            };
            s.anchor = s.text.snap(*start.max(&0) as usize) as i64;
            s.caret = s.text.snap(*end.max(start).max(&0) as usize) as i64;
            s.preedit = "".to_string();
            s.replace_selection(
                &if single {
                    text.replace("\n", " ").replace("\r", "")
                } else {
                    text.to_string()
                },
                EditKind::Other,
                now,
                t.max_length,
            );
            moved = true;
            cx.consume()
        }
        Event::Accessible {
            action,
            value,
            start,
            end,
            ..
        } => {
            let scalar_byte = |offset: i64| {
                if offset < 0 {
                    s.text.len()
                } else {
                    s.text.byte_of_scalar(offset as usize)
                }
            };
            let start_byte = scalar_byte(*start);
            let end_byte = scalar_byte(*end);
            let a = s.text.snap(start_byte) as i64;
            let down = s.text.snap(end_byte);
            let b = if down == end_byte {
                down
            } else {
                s.text.boundary(down, 1)
            } as i64;
            if action == "select-text" {
                s.anchor = a;
                s.caret = b;
            } else if editable && (action == "set-text" || action == "replace-text") {
                s.anchor = if action == "set-text" { 0 } else { a };
                s.caret = if action == "set-text" {
                    s.text.byte_len()
                } else {
                    b
                };
                s.preedit = "".to_string();
                s.insert(
                    &if single {
                        value.replace("\n", " ").replace("\r", "")
                    } else {
                        value.to_string()
                    },
                    cx.now,
                    t.max_length,
                );
            } else {
                return;
            }
            moved = true;
            cx.consume()
        }
        Event::PointerDown {
            x,
            y,
            button,
            clicks,
            mods,
        } => {
            cx.focus_on(&t.id);
            let btn = if node.parts.len() >= 3 {
                cx.to_window(node.parts[2])
            } else {
                Rect::new(0, 0, 0, 0)
            };
            if btn.width > 0 && btn.contains(*x, *y) {
                if t.dropdown {
                    let cs_id = format!("{}#completion", t.id);
                    if st.choice(&cs_id).open {
                        let mut cs = st.choice(&cs_id);
                        cs.open = false;
                        st.set(&cs_id, WState::Choice(cs));
                        cx.close_popup(&t.id, 0)
                    } else {
                        request_all(t, &mut s, st, cx, field_abs)
                    }
                } else if t.kind == InputKind::Search {
                    s.select_all();
                    s.replace_selection("", EditKind::Other, now, 0);
                } else {
                    s.revealed = !s.revealed;
                }
                cx.consume()
            } else if *button == MouseButton::Middle {
                if editable {
                    let (lx, ly) = local(*x, *y);
                    s.move_to(from_shown(t, &s, tl.hit(lx, ly)), false);
                    let p = clipboard::primary_text();
                    if !p.is_empty() {
                        s.insert(
                            &if single {
                                p.replace("\n", " ")
                            } else {
                                p.clone()
                            },
                            now,
                            t.max_length,
                        );
                    }
                }
                cx.consume()
            } else if *button == MouseButton::Left {
                let (lx, ly) = local(*x, *y);
                let at = from_shown(t, &s, tl.hit(lx, ly));
                let (a, b) = s.selection();
                if *clicks == 1
                    && !mods.shift
                    && t.kind != InputKind::Password
                    && a < b
                    && at >= a
                    && at < b
                {
                    // A press inside the selection may drag it out; a release
                    // without moving places the caret there instead.
                    st.set_field(&t.id, "drag-x", &format!("{}", x));
                    st.set_field(&t.id, "drag-y", &format!("{}", y));
                    st.set_field(&t.id, "drag-at", &format!("{}", at));
                    cx.capture_pointer(&t.id);
                    cx.consume()
                } else if *clicks >= 3 {
                    if single {
                        s.select_all()
                    } else {
                        s.select_line(at)
                    }
                } else if *clicks == 2 {
                    s.select_word(at)
                } else {
                    s.move_to(at, mods.shift)
                }
                s.selecting = true;
                s.goal_x = -1.0;
                cx.capture_pointer(&t.id);
                cx.consume()
            }
        }
        Event::PointerMove { x, y, pressed, .. } => {
            cx.cursor = Some(windowing::SystemCursor::Text);
            if let Some(ax) = st.field(&t.id, "drag-x", "").to_i64()
                && let Some(ay) = st.field(&t.id, "drag-y", "").to_i64()
            {
                let reach = theme.px(4);
                if *pressed && ((x - ax).abs() > reach || (y - ay).abs() > reach) {
                    clear_text_drag(&t.id, st);
                    cx.release_pointer();
                    cx.start_drag = Some((
                        t.id.clone(),
                        vec![
                            windowing::DragItem {
                                mime: "text/plain;charset=utf-8".to_string(),
                                data: s.selected_text().into_bytes(),
                            },
                            windowing::DragItem {
                                mime: "text/plain".to_string(),
                                data: s.selected_text().into_bytes(),
                            },
                        ],
                        windowing::DragOperation::Copy,
                    ));
                    cx.consume()
                }
            } else if *pressed && s.selecting {
                let (lx, ly) = local(*x, *y);
                s.move_to(from_shown(t, &s, tl.hit(lx, ly)), true);
                moved = true;
                cx.redraw = true;
            }
        }
        Event::PointerUp { .. } => {
            if let Some(at) = st.field(&t.id, "drag-at", "").to_i64() {
                clear_text_drag(&t.id, st);
                cx.release_pointer();
                s.move_to(at, false);
                moved = true;
            } else if s.selecting {
                s.selecting = false;
                cx.release_pointer();
                if s.has_selection() && t.kind != InputKind::Password {
                    clipboard::set_primary_text(&s.selected_text())
                }
            }
        }
        Event::Scroll { dx, dy, .. } if t.multiline() => {
            s.follow_caret = false;
            if !t.wrap {
                s.scroll_x = (s.scroll_x - dx * theme.px(48) as f64)
                    .max(0.0)
                    .min((tl.width - inner.width as f64).max(0.0));
            }
            let max_y = (tl.height - inner.height as f64).max(0.0);
            let before = s.scroll_y;
            s.scroll_y = (s.scroll_y - dy * theme.px(48) as f64).max(0.0).min(max_y);
            if s.scroll_y != before {
                cx.redraw = true;
                cx.consume()
            }
        }
        Event::Text(text) if editable => {
            let insert = if single {
                text.replace("\n", " ").replace("\r", "")
            } else {
                text.to_string()
            };
            if s.insert(&insert, now, t.max_length) {
                moved = true;
            }
            cx.consume()
        }
        Event::Drop { x, y, text, .. } if editable && !text.is_empty() => {
            let (lx, ly) = local(*x, *y);
            s.move_to(from_shown(t, &s, tl.hit(lx, ly)), false);
            if s.insert(
                &if single {
                    text.replace("\r\n", " ")
                        .replace("\n", " ")
                        .replace("\r", "")
                } else {
                    text.to_string()
                },
                now,
                t.max_length,
            ) {
                moved = true;
            }
            cx.focus_on(&t.id);
            cx.consume()
        }
        Event::Composing { text, cursor, .. } if editable => {
            s.compose(text, ScalarIndex(*cursor));
            cx.redraw = true;
            cx.consume()
        }
        Event::KeyDown { key, mods, .. } => {
            // Keys the input method is using stay with it.
            if !s.preedit.is_empty() {
                if *key == Key::Escape {
                    s.preedit.clear();
                    s.preedit_caret = 0;
                    cx.redraw = true;
                }
                if t.document > 0
                    && let Ok(info) = editing::documents::info(t.document)
                {
                    let _ = editing::documents::commit(t.document, info.revision, s.clone());
                }
                cx.consume();
                st.set(&t.id, WState::Edit(s));
                return;
            }
            let primary = mods.shortcut();
            let word = if crate::input::platform_is_macos() {
                mods.alt
            } else {
                mods.ctrl
            };
            let cs_id = format!("{}#completion", t.id);
            let mut cs = st.choice(&cs_id);
            // An open completion list takes navigation and acceptance keys.
            if cs.open {
                if *key == Key::Escape {
                    cs.open = false;
                    st.set(&cs_id, WState::Choice(cs));
                    cx.close_popup(&t.id, 0);
                    cx.consume();
                    st.set(&t.id, WState::Edit(s));
                    return;
                }
                let accept_key = *key == Key::Return
                    || *key == Key::KpEnter
                    || (*key == Key::Tab && !mods.shift && accepts_tab(t));
                if accept_key {
                    if let Some(item) = cs.current() {
                        accept(t, &mut s, &item, st, cx);
                        cx.consume();
                        st.set(&t.id, WState::Edit(s));
                        return;
                    }
                    if !cs.error.is_empty() && *key == Key::Return {
                        request(t, &mut s, st, cx, true, field_abs);
                        cx.consume();
                        st.set(&t.id, WState::Edit(s));
                        return;
                    }
                }
                if (*key == Key::Up
                    || *key == Key::Down
                    || *key == Key::PageUp
                    || *key == Key::PageDown)
                    && completion::list_key(&mut cs, *key, cx)
                {
                    st.set(&cs_id, WState::Choice(cs));
                    cx.consume();
                    st.set(&t.id, WState::Edit(s));
                    return;
                }
            }
            match key {
                Key::Left | Key::Right => {
                    let dir = if *key == Key::Left { -1 } else { 1 };
                    if word {
                        // Word moves are logical, in reading direction.
                        let logical = if !tl.lines.is_empty() && tl.lines[0].rtl {
                            -dir
                        } else {
                            dir
                        };
                        s.step(logical, true, mods.shift)
                    } else {
                        s.visual_step(&tl, dir, mods.shift);
                        if t.kind == InputKind::Password && !s.revealed {
                            s.move_to(from_shown(t, &s, to_shown(t, &s, s.caret)), mods.shift)
                        }
                    }
                    moved = true;
                    cx.consume()
                }
                Key::Up | Key::Down => {
                    let dir = if *key == Key::Up { -1 } else { 1 };
                    if t.multiline() {
                        s.vertical(&tl, dir, mods.shift);
                        moved = true;
                        cx.consume()
                    } else if !t.history.is_empty() && editable {
                        recall(t, &mut s, dir, now);
                        moved = true;
                        cx.consume()
                    } else if t.completion.is_some() && *key == Key::Down && editable {
                        if t.dropdown {
                            request_all(t, &mut s, st, cx, field_abs)
                        } else {
                            request(t, &mut s, st, cx, true, field_abs)
                        };
                        cx.consume()
                    }
                }
                Key::Home | Key::End => {
                    if primary || single {
                        s.move_to(
                            if *key == Key::Home {
                                0
                            } else {
                                s.text.byte_len()
                            },
                            mods.shift,
                        )
                    } else if !t.wrap && s.text.len() > 32 * 1024 {
                        let line = s.text.line_of(s.caret.max(0) as usize);
                        let start = s.text.line_start(line);
                        let mut end = s.text.line_start(line + 1);
                        while end > start
                            && matches!(s.text.slice(end - 1, end).as_str(), "\n" | "\r")
                        {
                            end -= 1;
                        }
                        s.move_to(
                            if *key == Key::End {
                                end as i64
                            } else {
                                start as i64
                            },
                            mods.shift,
                        );
                    } else {
                        s.line_edge(&tl, *key == Key::End, mods.shift)
                    }
                    moved = true;
                    cx.consume()
                }
                Key::PageUp | Key::PageDown => {
                    if t.multiline() {
                        let lines = (inner.height as f64 / tl.line_height.max(1.0)) as i64;
                        let mut k = 0;
                        while k < lines.max(1) {
                            s.vertical(&tl, if *key == Key::PageUp { -1 } else { 1 }, mods.shift);
                            k += 1;
                        }
                        moved = true;
                        cx.consume()
                    }
                }
                Key::Backspace => {
                    if editable && s.backspace(word, now) {
                        moved = true
                    };
                    cx.consume()
                }
                Key::Delete => {
                    if editable && s.delete_forward(word, now) {
                        moved = true
                    };
                    cx.consume()
                }
                Key::Return | Key::KpEnter => {
                    if t.multiline() && t.rich_document > 0 && editable && !mods.shift {
                        crate::rich::editor::remember_selection(t.rich_document, t.rich_block, &s);
                        match crate::rich::editor::enter(t.rich_document) {
                            Ok(info) => {
                                s.rich_committed = true;
                                cx.emit(&UiMsg::RichChanged(
                                    t.id.clone(),
                                    t.rich_document,
                                    info.revision,
                                ));
                                cx.redraw = true;
                            }
                            Err(error) => st.set_field(&t.id, "validity", &error),
                        }
                        cx.consume();
                    } else if t.multiline() {
                        if editable && s.insert("\n", now, t.max_length) {
                            moved = true
                        };
                        cx.consume()
                    } else {
                        cx.emit(&UiMsg::Submit(t.id.clone(), s.text.to_string()));
                        cx.consume()
                    }
                }
                Key::Tab => {
                    if t.rich_document > 0 && editable && !mods.ctrl {
                        crate::rich::editor::remember_selection(t.rich_document, t.rich_block, &s);
                        if let Ok(info) = crate::rich::editor::indent(t.rich_document, mods.shift) {
                            s.rich_committed = true;
                            cx.emit(&UiMsg::RichChanged(
                                t.id.clone(),
                                t.rich_document,
                                info.revision,
                            ));
                            cx.consume();
                            cx.redraw = true;
                        }
                    } else if t.multiline()
                        && t.tab_inserts
                        && !mods.ctrl
                        && !mods.shift
                        && editable
                    {
                        if s.insert("\t", now, t.max_length) {
                            moved = true
                        };
                        cx.consume()
                    }
                }
                // Escape clears a search; otherwise it goes on to the dialog
                // or popup around the field.
                Key::Escape => {
                    if t.kind == InputKind::Search && !s.text.is_empty() && editable {
                        s.select_all();
                        s.replace_selection("", EditKind::Other, now, 0);
                        moved = true;
                        cx.consume()
                    }
                }
                Key::Space => {
                    if primary && t.completion.is_some() {
                        request(t, &mut s, st, cx, true, field_abs);
                        cx.consume()
                    }
                }
                _ => {
                    if primary && edit_shortcut(t, &mut s, *key, mods.shift, now, editable, cx) {
                        moved = true;
                        cx.consume()
                    }
                }
            }
        }
        Event::Command(name) => {
            let used = match name.as_str() {
                "edit.copy" if t.kind != InputKind::Password => s.copy(),
                "edit.cut" => editable && t.kind != InputKind::Password && s.cut(now),
                "edit.paste" => editable && s.paste(single, now, t.max_length),
                "edit.select-all" => {
                    s.select_all();
                    true
                }
                "edit.undo" => edit_shortcut(t, &mut s, Key::Z, false, now, editable, cx),
                "edit.redo" => edit_shortcut(t, &mut s, Key::Y, false, now, editable, cx),
                "edit.delete" => editable && s.has_selection() && s.delete_forward(false, now),
                _ => false,
            };
            if used {
                moved = true;
                cx.consume()
            }
        }
        Event::FocusIn => {
            // A strict combo restores the last listed item it held.
            if t.strict && matches_item(t, &s.text) {
                st.set_field(&t.id, "accepted", &s.text)
            };
            cx.redraw = true;
        }
        Event::FocusOut => {
            // Losing focus cancels a composition; it never accepts a proposal.
            s.preedit = "".to_string();
            if t.strict && matches_item(t, &s.text) {
                st.set_field(&t.id, "accepted", &s.text)
            } else if t.strict {
                let back = st.field(&t.id, "accepted", "");
                if back != s.text {
                    s.select_all();
                    s.replace_selection(&back, EditKind::Other, now, 0);
                }
            }
            s.selecting = false;
            let cs_id = format!("{}#completion", t.id);
            let mut cs = st.choice(&cs_id);
            if cs.open {
                cs.open = false;
                st.set(&cs_id, WState::Choice(cs));
                cx.close_popup(&t.id, 0)
            }
            cx.redraw = true;
        }
        Event::Timer(tag) if tag == "complete" => request(t, &mut s, st, cx, false, field_abs),
        _ => (),
    }
    if !s.text.same_revision(&text_before) {
        changed(t, &mut s, st, cx);
        if s.preedit.is_empty() {
            schedule_completion(t, &mut s, st, cx, field_abs)
        }
    }
    if moved || !s.text.same_revision(&text_before) {
        s.follow_caret = true;
        scroll_to_caret(t, &mut s, &theme, inner);
        cx.redraw = true;
    }
    if (s.anchor, s.caret) != selection_before {
        cx.emit(&UiMsg::Caret(t.id.clone(), s.anchor, s.caret))
    }
    // Keep the input method's candidate window at the caret.
    if cx.is_focused(&t.id) || cx.request_focus == Some(t.id.clone()) {
        let (tl2, _, _) = shown_layout(t, &s, &theme, inner.width, inner.height);
        let r = tl2.caret_rect(to_shown(t, &s, s.caret));
        let caret_x = r.x - s.scroll_x as i64;
        let line = Rect::new(
            abs_inner.x,
            abs_inner.y + r.y - s.scroll_y as i64,
            abs_inner.width.max(1),
            r.height,
        );
        cx.ime = Some((line, caret_x.max(0).min(abs_inner.width)));
    }
    if t.rich_document == 0
        && t.document > 0
        && let Ok(info) = editing::documents::info(t.document)
    {
        let _ = editing::documents::commit(t.document, info.revision, s.clone());
    }
    if t.rich_document > 0 {
        if !s.rich_committed {
            crate::rich::editor::remember_selection(t.rich_document, t.rich_block, &s);
        }
        s.undo.clear();
        s.redo.clear();
    }
    st.set(&t.id, WState::Edit(s))
}

fn accepts_tab(t: &TextInput) -> bool {
    match t.completion {
        Some(ref c) => c.accept_tab,
        None => false,
    }
}

fn edit_shortcut(
    t: &TextInput,
    s: &mut EditState,
    key: Key,
    shift: bool,
    now: i64,
    editable: bool,
    cx: &mut EventCx,
) -> bool {
    if t.rich_document > 0 && editable {
        let answer = if key == Key::Z || key == Key::Y {
            Some(crate::rich::editor::undo(
                t.rich_document,
                t.rich_block,
                s,
                key == Key::Y || shift,
            ))
        } else if matches!(key, Key::B | Key::I | Key::U) {
            Some(crate::rich::editor::format(
                t.rich_document,
                t.rich_block,
                s,
                match key {
                    Key::B => crate::rich::Mark::Strong,
                    Key::I => crate::rich::Mark::Emphasis,
                    _ => crate::rich::Mark::Underline,
                },
            ))
        } else {
            None
        };
        if let Some(answer) = answer {
            if let Ok(revision) = answer {
                cx.emit(&UiMsg::RichChanged(t.id.clone(), t.rich_document, revision));
            }
            cx.redraw = true;
            return true;
        }
    }
    let single = !t.multiline();
    match key {
        Key::A => {
            s.select_all();
            true
        }
        Key::C => t.kind != InputKind::Password && s.copy(),
        Key::X => editable && t.kind != InputKind::Password && s.cut(now),
        Key::V => editable && s.paste(single, now, t.max_length),
        Key::Z => editable && if shift { s.redo() } else { s.undo() },
        Key::Y => editable && s.redo(),
        _ => false,
    }
}

fn recall(t: &TextInput, s: &mut EditState, dir: i64, now: i64) {
    let n = t.history.len();
    let mut idx = s.history_index;
    if dir < 0 {
        idx = if idx < 0 {
            (n - 1) as i64
        } else {
            (idx - 1).max(0)
        };
    } else {
        if idx < 0 {
            return;
        };
        idx += 1;
    }
    let value = if idx >= n as i64 {
        ""
    } else {
        &t.history[idx as usize]
    };
    s.select_all();
    s.replace_selection(value, EditKind::Other, now, t.max_length);
    s.history_index = if idx >= n as i64 { -1 } else { idx };
}

// The text range a completion replaces: the word before the caret, or all.
fn completion_range(t: &TextInput, s: &EditState) -> (i64, i64) {
    let word = match t.completion {
        Some(ref c) => c.word,
        None => false,
    };
    if !word {
        return (0, s.text.byte_len());
    }
    let mut start = s.caret;
    while start > 0 && s.caret - start <= 4096 {
        let prev = s.text.boundary(start.max(0) as usize, -1) as i64;
        let piece = s.text.slice(prev as usize, start as usize);
        let mut ok = false;
        for c in piece.chars() {
            if unicode::is_letter(c) || unicode::is_number(c) || c == '_' {
                ok = true
            }
        }
        if !ok {
            break;
        };
        start = prev;
    }
    (start, s.caret)
}

fn schedule_completion(
    t: &TextInput,
    s: &mut EditState,
    st: &mut StateStore,
    cx: &mut EventCx,
    field: Rect,
) {
    let Some(ref comp) = t.completion else { return };
    let (a, b) = completion_range(t, s);
    let query = s.text.slice(a as usize, b as usize);
    let cs_id = format!("{}#completion", t.id);
    let mut cs = st.choice(&cs_id);
    if unicode::grapheme_count(&query) < comp.min_chars {
        if cs.open {
            cs.open = false;
            cs.request += 1;
            st.set(&cs_id, WState::Choice(cs));
            cx.close_popup(&t.id, 0)
        }
        return;
    }
    match comp.source {
        Source::Async(debounce) => {
            cx.cancel_timer(&t.id, "complete");
            cx.start_timer(&t.id, "complete", debounce)
        }
        _ => request(t, s, st, cx, false, field),
    }
}

fn background_completion(
    t: &TextInput,
    s: &EditState,
    cs: &mut ChoiceState,
    items: &std::sync::Arc<[CompletionItem]>,
    query: &str,
    mode: completion::MatchMode,
    limit: i64,
) {
    cs.items = Default::default();
    cs.loading = true;
    cs.error.clear();
    let work = crate::services::work::Work {
        owner: t.id.clone(),
        request: cs.request,
        revision: s.revision,
        timeout_ms: 30_000,
        operation: crate::services::work::Operation::Complete(
            items.clone(),
            query.to_owned(),
            mode,
            limit,
        ),
    };
    if let Err(error) = crate::app::effects::try_perform(crate::app::effects::Effect::Work(work)) {
        cs.loading = false;
        cs.error = error;
    }
}

// Asks the completion source for proposals for the text at the caret.
fn request(
    t: &TextInput,
    s: &mut EditState,
    st: &mut StateStore,
    cx: &mut EventCx,
    explicit: bool,
    field: Rect,
) {
    let Some(ref comp) = t.completion else { return };
    let (a, b) = completion_range(t, s);
    let query = s.text.slice(a as usize, b as usize);
    let cs_id = format!("{}#completion", t.id);
    let mut cs = st.choice(&cs_id);
    cs.query = query.to_string();
    cs.revision = s.revision;
    cs.request += 1;
    match comp.source {
        Source::Local(ref items, mode) => {
            if items.len() > 256
                && !cfg!(test)
                && std::env::var("GUI_TEST_INLINE_WORK").as_deref() != Ok("1")
            {
                background_completion(t, s, &mut cs, items, &query, mode, comp.limit);
            } else {
                cs.set_items(&completion::rank(items, &query, mode, comp.limit))
            }
        }
        Source::Provider(ref f) => {
            let mut found = f(query.to_string());
            completion::sort_ranked(&mut found);
            if comp.limit > 0 && found.len() > comp.limit as usize {
                found = found[0_usize..comp.limit as usize].to_vec()
            };
            cs.set_items(&found)
        }
        Source::Async(_) => {
            // Results for an earlier query are not choices for this one; the
            // highlighted key stays, so it survives into matching results.
            cs.items = Default::default();
            cs.loading = true;
            cs.error = "".to_string();
            cx.emit(&UiMsg::CompletionRequest(
                t.id.clone(),
                cs.request,
                query.to_string(),
                s.revision,
            ))
        }
    }
    let show = !cs.items.is_empty() || cs.loading || explicit;
    cs.open = show;
    st.set(&cs_id, WState::Choice(cs.clone()));
    if show {
        open_list(t, &cs, cx, field)
    } else {
        cx.close_popup(&t.id, 0)
    }
}

fn open_list(t: &TextInput, cs: &ChoiceState, cx: &mut EventCx, field: Rect) {
    let size = completion::list_size(cs, &cx.theme, field.width);
    cx.open_popup(&PopupRequest::new(
        &t.id,
        0,
        field,
        size,
        Placement::Below,
        false,
        true,
    ))
}

/// Delivers asynchronous completion results; answers false when they are
/// stale (a newer request, or a revision the text has moved past).
pub fn deliver(
    st: &mut StateStore,
    id: &str,
    request: i64,
    items: &[CompletionItem],
    error: &str,
) -> bool {
    let cs_id = format!("{}#completion", id);
    let mut cs = st.choice(&cs_id);
    if request != cs.request || !cs.open {
        return false;
    }
    if st.edit(id).revision != cs.revision {
        return false;
    }
    if error.is_empty() {
        let mut sorted = items.to_vec();
        completion::sort_ranked(&mut sorted);
        cs.set_items(&sorted)
    } else {
        // The error row is the list's only row, so Enter retries.
        cs.items = Default::default();
        cs.loading = false;
        cs.error = error.to_string();
    }
    st.set(&cs_id, WState::Choice(cs));
    true
}

// Whether `text` is one of a strict combo's item labels.
fn matches_item(t: &TextInput, text: &str) -> bool {
    let Some(ref comp) = t.completion else {
        return true;
    };
    let Source::Local(ref items, _) = comp.source else {
        return true;
    };
    if text.is_empty() {
        return true;
    };
    items
        .iter()
        .any(|it| completion::fold(&it.label) == completion::fold(text))
}

// Opens the list with every item, as a combo's dropdown button does.
fn request_all(
    t: &TextInput,
    s: &mut EditState,
    st: &mut StateStore,
    cx: &mut EventCx,
    field: Rect,
) {
    let Some(ref comp) = t.completion else { return };
    let cs_id = format!("{}#completion", t.id);
    let mut cs = st.choice(&cs_id);
    cs.request += 1;
    cs.revision = s.revision;
    match comp.source {
        Source::Local(ref items, mode) => {
            if items.len() > 256
                && !cfg!(test)
                && std::env::var("GUI_TEST_INLINE_WORK").as_deref() != Ok("1")
            {
                background_completion(t, s, &mut cs, items, "", mode, comp.limit);
            } else {
                cs.set_items(&completion::rank(items, "", mode, comp.limit))
            }
        }
        Source::Provider(ref f) => cs.set_items(&f("".to_string())),
        Source::Async(_) => {
            cs.loading = true;
            cx.emit(&UiMsg::CompletionRequest(
                t.id.clone(),
                cs.request,
                "".to_string(),
                s.revision,
            ))
        }
    }
    // The current item stays highlighted.
    for it in &cs.items {
        if completion::fold(&it.label) == completion::fold(&s.text) {
            cs.highlight = it.key.clone()
        }
    }
    cs.open = true;
    st.set(&cs_id, WState::Choice(cs.clone()));
    open_list(t, &cs, cx, field)
}

// Replaces the completion range with the accepted item: one undo step.
fn accept(
    t: &TextInput,
    s: &mut EditState,
    item: &CompletionItem,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    let (a, b) = completion_range(t, s);
    s.anchor = a;
    s.caret = b;
    s.replace_selection(&item.inserted(), EditKind::Other, cx.now, t.max_length);
    let cs_id = format!("{}#completion", t.id);
    let mut cs = st.choice(&cs_id);
    cs.open = false;
    cs.request += 1;
    st.set(&cs_id, WState::Choice(cs));
    cx.close_popup(&t.id, 0);
    changed(t, s, st, cx);
    cx.emit(&UiMsg::Select(t.id.clone(), item.key.clone()));
    st.set_field(&t.id, "accepted", &s.text);
    cx.announce(&crate::localization::message(
        "completion",
        "{item} inserted",
        1,
        &crate::hmap! { "item" => item.label.clone() },
    ))
}

pub fn popup_paint(t: &TextInput, rect: Rect, cx: &mut PaintCx) {
    let cs = cx.states.choice(&format!("{}#completion", t.id));
    completion::paint_list(&cs, rect, "", &crate::localization::tr("No matches"), cx)
}

pub fn popup_event(t: &TextInput, rect: Rect, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    let cs_id = format!("{}#completion", t.id);
    let mut cs = st.choice(&cs_id);
    match ev {
        Event::PointerMove { x, y, .. } => {
            let key = completion::list_hit(&cs, rect, &cx.theme, *x, *y);
            if !key.is_empty() && key != cs.highlight {
                cs.highlight = key;
                cx.redraw = true;
            }
        }
        Event::PointerUp { x, y, .. } => {
            let key = completion::list_hit(&cs, rect, &cx.theme, *x, *y);
            if !key.is_empty() {
                cs.highlight = key;
                st.set(&cs_id, WState::Choice(cs.clone()));
                if let Some(item) = cs.current() {
                    let mut s = state_of(t, st);
                    accept(t, &mut s, &item, st, cx);
                    st.set(&t.id, WState::Edit(s))
                }
                cx.consume();
                return;
            }
        }
        Event::Scroll { dy, .. } => {
            let max_first = cs.items.len() - (completion::VISIBLE_ROWS) as usize;
            cs.first = (cs.first - dy.round() as i64 * 3)
                .max(0)
                .min(max_first as i64);
            cx.redraw = true;
        }
        _ => (),
    }
    cx.consume();
    st.set(&cs_id, WState::Choice(cs))
}

pub fn input_semantics(t: &TextInput, st: &StateStore) -> Option<AccessNode> {
    let role = match t.kind {
        InputKind::Line => Role::TextField,
        InputKind::Area => Role::TextArea,
        InputKind::Password => Role::PasswordField,
        InputKind::Search => Role::SearchField,
    };
    let name = if t.label.is_empty() {
        &t.placeholder
    } else {
        &t.label
    };
    let mut n = AccessNode::control(role, &t.id, name, "focus");
    let s = state_of(t, st);
    // Passwords never expose their value.
    if t.kind != InputKind::Password {
        if s.text.len() > 32 * 1024 {
            n.text_source = Some(crate::accessibility::TextSnapshot(s.text.snapshot()));
        } else {
            n.value = s.text.to_string();
        }
        let (a, b) = s.selection();
        n.text_start = a;
        n.text_end = b;
        n.caret = s.caret;
    }
    n.read_only = t.read_only;
    n.required = t.required;
    n.multiline = t.multiline();
    let validity = st.field(&t.id, "validity", "");
    n.invalid = !t.error.is_empty() || !validity.is_empty();
    n.description = caption(t, &validity);
    if t.completion.is_some() {
        let cs = st.choice(&format!("{}#completion", t.id));
        n.expanded = if cs.open { 1 } else { 0 };
        n.controls = format!("{}#completion", t.id);
        if cs.open {
            n.children.push(completion::list_semantics(
                &cs,
                &format!("{}#completion", t.id),
                name,
                &t.id,
                "",
            ))
        }
    }
    Some(n)
}

/// The state's selection for tests: (anchor, caret).
pub fn selection(st: &StateStore, id: &str) -> (i64, i64) {
    let s = st.edit(id);
    (s.anchor, s.caret)
}

/// Floors a layout x to a pixel; exposed for the code editor's gutter.
pub fn pixel(x: f64) -> i64 {
    x.floor() as i64
}

impl std::fmt::Display for TextInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TextInput({})", self.id)
    }
}

impl std::fmt::Debug for TextInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TextInput {{ id: {:?}, text: {:?} }}",
            self.id, self.text
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{InputKind, TextInput, input_event, layout_input};
    use crate::editing::EditState;
    use crate::geometry::{Constraints, Rect, Size};

    use crate::style::Theme;
    use crate::widgets::cx::{Event, EventCx, LayoutCx, UiMsg};
    use crate::widgets::state::StateStore;

    #[test]
    fn application_icons_paint_in_results_and_follow_the_highlight_in_the_entry() {
        use crate::render::Canvas;
        use crate::render::icons::Icon;
        use crate::services::apps::AppIcon;
        use crate::widgets::completion::{ChoiceState, Completion, CompletionItem};
        use crate::widgets::cx::PaintCx;
        use crate::widgets::state::WState;
        let theme = Theme::light();
        let mut input = TextInput::new("run", "a", "Run", InputKind::Line);
        input.completion = Some(Completion::asynchronous(0));
        input.leading_icon = Some(Icon::Search);
        let mut first = CompletionItem::new("one", "Alpha");
        first.image = Some(AppIcon {
            rgba: vec![240, 30, 20, 255],
            width: 1,
            height: 1,
        });
        let mut second = CompletionItem::new("two", "Another");
        second.image = Some(AppIcon {
            rgba: vec![20, 50, 240, 255],
            width: 1,
            height: 1,
        });
        let mut missing = CompletionItem::new("missing", "Absent icon");
        missing.icon = Some(Icon::Grid);
        let mut choice = ChoiceState {
            open: true,
            ..ChoiceState::default()
        };
        choice.set_items(&[first, second, missing]);
        let mut states = StateStore::new();
        states.set("run#completion", WState::Choice(choice.clone()));
        let mut layout = LayoutCx::new(&theme, &states, Size::new(320, 80));
        let node = layout_input(&input, Constraints::loose(layout.window), &mut layout);
        let expected_geometry = node.clone();
        for expected in [[240, 30, 20, 255], [20, 50, 240, 255]] {
            let mut paint = PaintCx::new(&Canvas::new(320, 240), &theme, &states);
            super::paint_input(&input, &node, &mut paint);
            let x = theme.padding() + theme.px(10);
            let y = node.parts[0].height / 2;
            let offset = ((y * paint.canvas.width + x) * 4) as usize;
            assert_eq!(&paint.canvas.pixels[offset..offset + 4], &expected);
            let mut event = EventCx::new(&theme, "run", 0, Rect::new(0, 0, 320, 80));
            input_event(
                &input,
                &node,
                &Event::KeyDown {
                    key: windowing::Key::Down,
                    mods: windowing::Mods::default(),
                    repeat: false,
                },
                &mut states,
                &mut event,
            );
            assert_eq!(
                states.edit("run").text,
                "a",
                "preview must not accept the result"
            );
        }
        let (image, glyph) = super::leading_content(&input, &states);
        assert!(image.is_none());
        assert_eq!(glyph, Some(Icon::Grid));
        let mut layout = LayoutCx::new(&theme, &states, Size::new(320, 80));
        assert_eq!(
            layout_input(&input, Constraints::loose(layout.window), &mut layout),
            expected_geometry
        );
        let mut paint = PaintCx::new(&Canvas::new(320, 240), &theme, &states);
        crate::widgets::completion::paint_list(
            &choice,
            Rect::new(0, 0, 320, 200),
            "",
            "No matches",
            &mut paint,
        );
        let x = theme.px(12 + 24 + 10);
        let y = theme.px(4) + theme.row_height() / 2;
        let offset = ((y * paint.canvas.width + x) * 4) as usize;
        assert_eq!(
            &paint.canvas.pixels[offset..offset + 4],
            &[240, 30, 20, 255]
        );
    }

    #[test]
    fn a_text_area_given_less_height_than_its_rows_shrinks_to_fit() {
        let mut t = TextInput::new("notes", "one\ntwo\nthree", "", InputKind::Area);
        t.rows = 12;
        let theme = Theme::default();
        let mut l = LayoutCx::new(&theme, &StateStore::new(), Size::new(320, 400));
        let roomy = layout_input(&t, Constraints::loose(l.window), &mut l);
        assert!(roomy.parts[0].height > 100);
        let node = layout_input(&t, Constraints::new(0, 320, 0, 100), &mut l);
        assert_eq!(node.rect.height, 100);
        assert!(
            node.parts[0].bottom() <= 100,
            "the field paints past its node: {:?}",
            node.parts[0]
        );
        assert!(node.parts[1].bottom() <= node.parts[0].bottom());
        let squeezed = layout_input(&t, Constraints::new(0, 320, 0, 4), &mut l);
        assert!(
            squeezed.parts[1].height > 0,
            "the text keeps at least one line"
        );
    }

    // The input-method area for a focused field after End: the caret's line
    // and the caret's pixel offset along it.
    fn ime_after_end(text: &str) -> (Rect, i64) {
        let t = TextInput::new("f", text, "", InputKind::Line);
        let theme = Theme::default();
        let mut l = LayoutCx::new(&theme, &StateStore::new(), Size::new(320, 80));
        let node = layout_input(&t, Constraints::loose(l.window), &mut l);
        let mut st = StateStore::new();
        let mut cx = EventCx::new(&theme, "f", 0, Rect::new(0, 0, 320, 80));
        input_event(
            &t,
            &node,
            &Event::KeyDown {
                key: windowing::Key::End,
                mods: windowing::Mods::default(),
                repeat: false,
            },
            &mut st,
            &mut cx,
        );
        cx.ime.unwrap_or((Rect::new(0, 0, 0, 0), -1))
    }

    // Delivers `ev` to a focused line input holding `text` and answers its
    // edit state and the messages it emitted.
    fn deliver(t: &TextInput, ev: &Event) -> (EditState, Vec<UiMsg>) {
        let theme = Theme::default();
        let mut l = LayoutCx::new(&theme, &StateStore::new(), Size::new(320, 80));
        let node = layout_input(t, Constraints::loose(l.window), &mut l);
        let mut st = StateStore::new();
        let mut cx = EventCx::new(&theme, &t.id, 0, Rect::new(0, 0, 320, 80));
        input_event(t, &node, ev, &mut st, &mut cx);
        (st.edit(&t.id), cx.messages)
    }

    // Selects "hello" in a focused field, presses inside it, moves `by`
    // pixels with the button held and releases; answers the drag requested
    // and the field's final state.
    fn drag_selection(
        by: i64,
    ) -> (
        Option<(String, Vec<windowing::DragItem>, windowing::DragOperation)>,
        EditState,
    ) {
        let t = TextInput::new("f", "hello world", "", InputKind::Line);
        let theme = Theme::default();
        let mut l = LayoutCx::new(&theme, &StateStore::new(), Size::new(320, 80));
        let node = layout_input(&t, Constraints::loose(l.window), &mut l);
        let mut st = StateStore::new();
        let mut cx = EventCx::new(&theme, "f", 0, Rect::new(0, 0, 320, 80));
        let none = windowing::Mods::default();
        input_event(
            &t,
            &node,
            &Event::SetSelection {
                anchor: 0,
                caret: 5,
            },
            &mut st,
            &mut cx,
        );
        let inner = node.parts[1];
        let x = inner.x + theme.px(6);
        let y = inner.y + inner.height / 2;
        input_event(
            &t,
            &node,
            &Event::PointerDown {
                x,
                y,
                button: windowing::MouseButton::Left,
                clicks: 1,
                mods: none,
            },
            &mut st,
            &mut cx,
        );
        input_event(
            &t,
            &node,
            &Event::PointerMove {
                x: x + by,
                y,
                pressed: true,
                mods: none,
            },
            &mut st,
            &mut cx,
        );
        input_event(
            &t,
            &node,
            &Event::PointerUp {
                x: x + by,
                y,
                button: windowing::MouseButton::Left,
                mods: none,
            },
            &mut st,
            &mut cx,
        );
        (cx.start_drag, st.edit("f"))
    }

    #[test]
    fn dragging_inside_the_selection_offers_its_text() {
        let (started, s) = drag_selection(30);
        let Some((id, items, _)) = started else {
            panic!("the drag offered nothing");
        };
        assert_eq!(id, "f");
        assert_eq!(items[0].data, b"hello");
        assert_eq!((s.anchor, s.caret), (0, 5))
    }

    #[test]
    fn a_click_inside_the_selection_places_the_caret() {
        let (started, s) = drag_selection(0);
        assert!(started.is_none());
        assert_eq!(s.anchor, s.caret);
        assert!(s.caret < 5)
    }

    #[test]
    fn programmatic_selection_snaps_to_graphemes_and_reports_the_caret() {
        // "e" plus a combining acute is one grapheme spanning bytes 0..3.
        let t = TextInput::new("f", "e\u{301}xy", "", InputKind::Line);
        let (s, messages) = deliver(
            &t,
            &Event::SetSelection {
                anchor: 2,
                caret: 4,
            },
        );
        assert_eq!((s.anchor, s.caret), (0, 4));
        assert_eq!(messages, vec![UiMsg::Caret("f".to_string(), 0, 4)]);
        let (s, _) = deliver(
            &t,
            &Event::SetSelection {
                anchor: 99,
                caret: -3,
            },
        );
        assert_eq!((s.anchor, s.caret), (5, 0))
    }

    #[test]
    fn programmatic_replacement_is_one_undoable_edit() {
        let t = TextInput::new("f", "a cat", "", InputKind::Line);
        let (s, messages) = deliver(
            &t,
            &Event::ReplaceRange {
                start: 2,
                end: 5,
                text: "dog".to_string(),
            },
        );
        assert_eq!(s.text, "a dog");
        assert_eq!(s.caret, 5);
        assert_eq!(
            messages,
            vec![UiMsg::Text("f".to_string(), "a dog".to_string())]
        );
        let mut undone = s;
        assert!(undone.undo());
        assert_eq!(undone.text, "a cat");
        let mut locked = t;
        locked.read_only = true;
        let (r, quiet) = deliver(
            &locked,
            &Event::ReplaceRange {
                start: 2,
                end: 5,
                text: "dog".to_string(),
            },
        );
        assert_eq!(r.text, "");
        assert!(quiet.is_empty())
    }

    #[test]
    fn ime_cursor_is_the_caret_pixel_offset_along_its_line() {
        let (wide, at_wide) = ime_after_end("WWW");
        let (narrow, at_narrow) = ime_after_end("iii");
        assert!(at_wide > at_narrow);
        assert!(at_narrow > 3);
        assert!(at_wide < wide.width);
        assert_eq!(wide.x, narrow.x);
        assert_eq!(wide.width, narrow.width);
        let (_, at_start) = ime_after_end("");
        assert_eq!(at_start, 0)
    }
}

#[cfg(test)]
mod document_tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn large_native_documents_edit_and_render_without_flattening_or_unbounded_history() {
        let theme = Theme::light();
        let budget = crate::memory::Budget::new(1024 * 1024 * 1024);
        for source in [
            "fn α() { let שלום = \"👩\u{200d}💻\" } // e\u{301}\n".repeat(250_000),
            "x".repeat(1024 * 1024),
        ] {
            assert!(source.len() <= 16 * 1024 * 1024);
            let document = editing::documents::create(&source, budget.clone()).unwrap();
            let mut input = TextInput::new("large", "", "Document", InputKind::Area);
            input.document = document;
            input.mono = true;
            input.wrap = false;
            let mut states = StateStore::with_budget(budget.clone());
            let mut layout = LayoutCx::new(&theme, &states, Size::new(1000, 700));
            let node = layout_input(&input, Constraints::tight(layout.window), &mut layout);
            let mut context = EventCx::new(&theme, "large", 0, Rect::new(0, 0, 1000, 700));
            input_event(
                &input,
                &node,
                &Event::SetSelection {
                    anchor: source.len() as i64,
                    caret: source.len() as i64,
                },
                &mut states,
                &mut context,
            );
            let mut worst = Duration::ZERO;
            for sample in 0..20 {
                let begin = Instant::now();
                let mut event = EventCx::new(&theme, "large", sample, Rect::new(0, 0, 1000, 700));
                input_event(
                    &input,
                    &node,
                    &Event::Text("a".into()),
                    &mut states,
                    &mut event,
                );
                let mut paint =
                    PaintCx::new(&crate::render::Canvas::new(1000, 700), &theme, &states);
                paint_input(&input, &node, &mut paint);
                assert!(paint.canvas.paint_error.is_none());
                worst = worst.max(begin.elapsed());
            }
            let state = editing::documents::state(document).unwrap();
            assert!(!state.text.flattened());
            assert!(state.history_bytes() <= editing::UNDO_BYTE_LIMIT);
            assert_eq!(state.text.len(), source.len() + 20);
            // Debug CPU and software painting are included. A tighter release
            // percentile gate is exercised by the native editor benchmark.
            assert!(
                worst < Duration::from_secs(2),
                "document event took {worst:?}"
            );
            let semantics = input_semantics(&input, &states).unwrap();
            assert!(semantics.text_source.is_some());
            assert!(semantics.value.is_empty());
            assert_eq!(
                crate::accessibility::provider::node_text_range(&semantics, 0, 3),
                source.chars().take(3).collect::<String>()
            );
            editing::documents::close(document);
            drop(states);
            drop(state);
            drop(semantics);
            assert_eq!(budget.usage().0, 0);
        }
    }
    #[test]
    fn native_document_composition_cancels_and_commits_without_mutating_snapshots() {
        let budget = crate::memory::Budget::new(1024 * 1024);
        let document = editing::documents::create("aéz", budget.clone()).unwrap();
        let mut input = TextInput::new("ime", "", "IME", InputKind::Area);
        input.document = document;
        let theme = Theme::light();
        let mut states = StateStore::with_budget(budget.clone());
        let mut layout = LayoutCx::new(&theme, &states, Size::new(320, 200));
        let node = layout_input(&input, Constraints::tight(layout.window), &mut layout);
        let mut send = |event: Event| {
            let mut context = EventCx::new(&theme, "ime", 0, Rect::new(0, 0, 320, 200));
            input_event(&input, &node, &event, &mut states, &mut context);
        };
        send(Event::SetSelection {
            anchor: 1,
            caret: 3,
        });
        let snapshot = editing::documents::reader(document).unwrap();
        send(Event::Composing {
            text: "漢".into(),
            cursor: 1,
            selection: 0,
        });
        assert_eq!(editing::documents::info(document).unwrap().revision, 0);
        assert_eq!(
            editing::documents::state(document).unwrap().preedit_caret,
            3
        );
        send(Event::KeyDown {
            key: Key::Escape,
            mods: windowing::Mods::default(),
            repeat: false,
        });
        assert!(
            editing::documents::state(document)
                .unwrap()
                .preedit
                .is_empty()
        );
        send(Event::Composing {
            text: "界".into(),
            cursor: 1,
            selection: 0,
        });
        send(Event::Text("界".into()));
        assert_eq!(snapshot.to_string(), "aéz");
        assert_eq!(editing::documents::state(document).unwrap().text, "a界z");
        let mut state = editing::documents::state(document).unwrap();
        assert!(state.undo());
        editing::documents::commit(document, 1, state).unwrap();
        assert!(!editing::documents::info(document).unwrap().dirty);
        editing::documents::close(document);
        drop(states);
        drop(snapshot);
        assert_eq!(budget.usage().0, 0);
    }
}
