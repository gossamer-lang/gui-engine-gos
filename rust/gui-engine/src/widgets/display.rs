// display: widgets that present without taking values: labels (plain,
// wrapping, selectable, links), spacers, separators, images, and icons.
use windowing::Key;

use crate::accessibility::{AccessNode, Role};
use crate::editing::EditState;
use crate::geometry::{Constraints, Rect, Size};
use crate::services::clipboard;
use crate::style::TextRole;
use crate::style::color::Color;
use crate::text::layout;
use crate::text::layout::{LayoutOptions, TextAlign, TextLayout, Wrap};
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx, UiMsg};
use crate::widgets::state::{StateStore, WState};
use crate::widgets::{Tone, buttons};

/// Text in a typographic role.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Label {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub spans: Vec<crate::rich::Span>,
    #[serde(default)]
    pub rich_document: i64,
    #[serde(default)]
    pub heading_level: i64,
    pub role: TextRole,
    pub tone: Tone,
    pub wrap: bool,
    pub align: TextAlign,
    /// Lines kept before eliding with an ellipsis (0 keeps all).
    pub max_lines: i64,
    pub selectable: bool,
    pub link: bool,
}

impl Label {
    pub fn new(text: &str, role: TextRole) -> Label {
        Label {
            id: String::new(),
            text: text.to_string(),
            spans: Vec::new(),
            rich_document: 0,
            heading_level: 0,
            role,
            tone: Tone::Normal,
            wrap: false,
            align: TextAlign::Start,
            max_lines: 0,
            selectable: false,
            link: false,
        }
    }
}

fn options_for(l: &Label, width: i64, rtl: bool) -> LayoutOptions {
    let mut o = if l.wrap && width >= 0 {
        LayoutOptions::wrapped(width as f64)
    } else {
        LayoutOptions::single_line()
    };
    o.align = l.align;
    o.max_lines = l.max_lines;
    // Cutting to `max_lines` needs the text broken into lines at the width,
    // as `LayoutOptions::elided` does.
    if !l.wrap && l.max_lines > 0 && width >= 0 {
        o.max_width = width as f64;
        o.wrap = Wrap::Anywhere;
    }
    if rtl {
        o.direction = crate::text::bidi::Direction::Auto;
    }
    o
}

/// The text layout a label paints with inside `width` pixels.
pub fn label_layout(l: &Label, theme: &crate::style::Theme, width: i64) -> TextLayout {
    (*shared_label_layout(l, theme, width)).clone()
}
fn shared_label_layout(
    l: &Label,
    theme: &crate::style::Theme,
    width: i64,
) -> std::sync::Arc<TextLayout> {
    if !l.spans.is_empty() {
        return crate::rich::layout_cache::get(
            &l.id,
            &l.text,
            &l.spans,
            theme.font(l.role),
            options_for(l, width, theme.rtl),
        );
    }
    std::sync::Arc::new(layout::layout(
        &l.text,
        &theme.font(l.role),
        options_for(l, width, theme.rtl),
    ))
}

pub fn layout_label(l: &Label, c: Constraints, cx: &mut LayoutCx) -> Node {
    let width = if c.has_bounded_width() {
        c.max_width
    } else {
        -1
    };
    let tl = shared_label_layout(l, &cx.theme, width);
    let mut w = tl.width.ceil() as i64;
    if l.wrap && width >= 0 && tl.lines.len() > 1 {
        w = width;
    }
    let h = tl.height.ceil() as i64;
    let mut node = Node::leaf(c.constrain(Size::new(w, h)));
    node.baseline = tl.ascent.ceil() as i64;
    node
}

pub fn paint_label(l: &Label, node: &Node, cx: &mut PaintCx) {
    let r = Rect::from_size(node.size());
    let tl = shared_label_layout(l, &cx.theme, r.width);
    let hover = l.link && cx.is_hover(&l.id);
    let pressed = l.link && crate::widgets::buttons::held(&l.id, cx);
    let color = if cx.disabled {
        cx.theme.text_disabled
    } else if pressed {
        cx.theme.accent.mix(Color::black(), 0.25)
    } else if hover {
        cx.theme.accent
    } else {
        l.tone.resolve(&cx.theme)
    };
    let x = cx.origin_x;
    let y = cx.origin_y;
    // Box alignment: a label wider than its text honours its alignment.
    let mut dx = 0.0;
    if !l.wrap {
        let slack = (r.width as f64 - tl.width).max(0.0);
        dx = match l.align {
            TextAlign::Center => slack / 2.0,
            TextAlign::Right => slack,
            TextAlign::End => {
                if cx.theme.rtl {
                    0.0
                } else {
                    slack
                }
            }
            TextAlign::Start => {
                if cx.theme.rtl {
                    slack
                } else {
                    0.0
                }
            }
            TextAlign::Left => 0.0,
        };
    }
    if l.selectable {
        let st = cx.states.edit(&l.id);
        if st.initialized && st.has_selection() {
            let (a, b) = st.selection();
            let sel = if cx.active {
                cx.theme.selection
            } else {
                cx.theme.selection_inactive
            };
            for rect in tl.selection_rects(a, b) {
                cx.canvas.rect(
                    x + dx as i64 + rect.x,
                    y + rect.y,
                    rect.width,
                    rect.height,
                    sel.with_alpha(110),
                )
            }
        }
    }
    let style = cx.theme.font(l.role);
    if l.spans.is_empty() {
        layout::draw(&mut cx.canvas, &tl, x as f64 + dx, y as f64, &style, color);
    } else {
        layout::draw_rich(
            &mut cx.canvas,
            &tl,
            x as f64 + dx,
            y as f64,
            &style,
            color,
            &l.spans,
            l.rich_document,
            &cx.theme,
        );
    }
    if l.link {
        // Links underline so color is not the only cue; the pointer on one
        // thickens it.
        let thickness = if hover || pressed {
            cx.theme.px(2).max(2)
        } else {
            cx.theme.px(1).max(1)
        };
        for line in &tl.lines {
            let uy = y + line.baseline.ceil() as i64 + cx.theme.px(2);
            cx.canvas.rect(
                x + dx as i64 + line.left as i64,
                uy,
                line.width.ceil() as i64,
                thickness,
                color,
            )
        }
    }
    if (l.link || l.selectable) && cx.is_focused(&l.id) {
        let rad = cx.theme.px(3);
        cx.focus_ring(r, rad)
    }
}

pub fn label_event(l: &Label, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    if l.id.is_empty() || cx.disabled {
        return;
    }
    if l.link {
        let area = cx.to_window(Rect::from_size(node.size()));
        match ev {
            Event::PointerDown { button, .. } if *button == windowing::MouseButton::Left => {
                buttons::press_begin(&l.id, st, cx)
            }
            Event::PointerUp { x, y, button, .. }
                if *button == windowing::MouseButton::Left
                    && buttons::press_end(&l.id, area.contains(*x, *y), st, cx) =>
            {
                cx.emit(&UiMsg::Click(l.id.clone()))
            }
            Event::FocusOut | Event::CaptureLost => buttons::press_cancel(&l.id, st, cx),
            Event::PointerMove { x, y, .. } => {
                cx.cursor = Some(windowing::SystemCursor::Pointer);
                buttons::press_track(&l.id, area.contains(*x, *y), st, cx)
            }
            Event::KeyDown { key, .. } if (*key == Key::Return || *key == Key::Space) => {
                cx.emit(&UiMsg::Click(l.id.clone()));
                cx.consume()
            }
            _ => (),
        }
        return;
    }
    if let Event::Accessible {
        target,
        action,
        start,
        end,
        ..
    } = ev
    {
        if let Some(index) = target
            .strip_prefix(&format!("{}#link:", l.id))
            .and_then(|s| s.parse::<usize>().ok())
            && action == "press"
            && let Some(span) = l.spans.get(index)
            && !span.marks.link.is_empty()
        {
            cx.emit(&UiMsg::RichLink(l.id.clone(), span.marks.link.clone()));
            cx.consume();
            return;
        }
        if l.selectable
            && target == &l.id
            && action == "select-text"
            && *start >= 0
            && *end >= *start
        {
            let mut selection = st.edit(&l.id);
            selection.sync(&l.text);
            selection.anchor = selection.text.byte_of_scalar(*start as usize) as i64;
            selection.caret = selection.text.byte_of_scalar(*end as usize) as i64;
            st.set(&l.id, WState::Edit(selection));
            cx.redraw = true;
            cx.consume();
            return;
        }
    }
    if !l.selectable {
        return;
    }
    let mut s = st.edit(&l.id);
    s.sync(&l.text);
    let tl = shared_label_layout(l, &cx.theme, node.rect.width);
    let ox = cx.origin_x;
    let oy = cx.origin_y;
    match ev {
        Event::PointerDown {
            x, y, clicks, mods, ..
        } => {
            cx.focus_on(&l.id);
            cx.capture_pointer(&l.id);
            let at = tl.hit((x - ox) as f64, (y - oy) as f64);
            if *clicks >= 3 {
                s.select_all()
            } else if *clicks == 2 {
                s.select_word(at)
            } else {
                s.move_to(at, mods.shift)
            }
            s.selecting = true;
            cx.consume()
        }
        Event::PointerMove { x, y, pressed, .. } => {
            cx.cursor = Some(windowing::SystemCursor::Text);
            if *pressed && s.selecting {
                s.move_to(tl.hit((x - ox) as f64, (y - oy) as f64), true);
                cx.redraw = true;
            }
        }
        Event::PointerUp { x, y, button, .. } => {
            if *button == windowing::MouseButton::Left && !s.has_selection() {
                let byte = tl.hit((x - ox) as f64, (y - oy) as f64).max(0) as usize;
                if let Some(span) = l.spans.iter().find(|span| {
                    span.start <= byte && byte < span.end && !span.marks.link.is_empty()
                }) {
                    cx.emit(&UiMsg::RichLink(l.id.clone(), span.marks.link.clone()));
                    cx.consume();
                }
            }
            s.selecting = false;
            cx.release_pointer();
            if s.has_selection() {
                clipboard::set_primary_text(&s.selected_text())
            }
        }
        Event::KeyDown { key, mods, .. } => {
            let primary = mods.shortcut();
            if *key == Key::Return
                && let Some(span) = l.spans.iter().find(|span| {
                    span.start <= s.caret.max(0) as usize
                        && (s.caret.max(0) as usize) < span.end
                        && !span.marks.link.is_empty()
                })
            {
                cx.emit(&UiMsg::RichLink(l.id.clone(), span.marks.link.clone()));
                cx.consume();
            } else if primary && *key == Key::A {
                s.select_all();
                cx.redraw = true;
                cx.consume()
            } else if primary && *key == Key::C {
                s.copy();
                cx.consume()
            } else if *key == Key::Left || *key == Key::Right {
                let dir = if *key == Key::Left { -1 } else { 1 };
                // With the word modifier (Option on macOS, Ctrl elsewhere) the
                // caret moves by words, in logical order.
                let word = if crate::input::platform_is_macos() {
                    mods.alt
                } else {
                    mods.ctrl
                };
                if word {
                    s.step(if cx.theme.rtl { -dir } else { dir }, true, mods.shift)
                } else {
                    s.visual_step(&tl, dir, mods.shift)
                };
                cx.redraw = true;
                cx.consume()
            } else if *key == Key::Up || *key == Key::Down {
                s.vertical(&tl, if *key == Key::Up { -1 } else { 1 }, mods.shift);
                cx.redraw = true;
                cx.consume()
            } else if *key == Key::Home || *key == Key::End {
                if primary {
                    s.move_to(
                        if *key == Key::Home {
                            0
                        } else {
                            s.text.byte_len()
                        },
                        mods.shift,
                    )
                } else {
                    s.line_edge(&tl, *key == Key::End, mods.shift)
                };
                cx.redraw = true;
                cx.consume()
            }
        }
        Event::Command(name) => {
            if name == "edit.copy" {
                s.copy();
                cx.consume()
            };
            if name == "edit.select-all" {
                s.select_all();
                cx.redraw = true;
                cx.consume()
            }
        }
        Event::FocusOut => {
            s.anchor = s.caret;
            cx.redraw = true;
        }
        _ => (),
    }
    st.set(&l.id, WState::Edit(s))
}

pub fn label_semantics(l: &Label) -> Option<AccessNode> {
    let role = if l.link {
        Role::Link
    } else if l.role == TextRole::Heading || l.role == TextRole::Title || l.role == TextRole::Hero {
        Role::Heading
    } else if l.wrap {
        Role::Paragraph
    } else {
        Role::Label
    };
    let mut n = AccessNode::new(role, &l.text);
    n.id = l.id.clone();
    n.focusable = l.link || l.selectable;
    n.read_only = true;
    if l.link {
        n.actions.push("press".to_string())
    };
    if role == Role::Heading {
        n.level = if (1..=6).contains(&l.heading_level) {
            l.heading_level
        } else {
            match l.role {
                TextRole::Hero | TextRole::Title => 1,
                _ => 2,
            }
        };
    }
    Some(n)
}

/// Empty space.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Spacer {
    pub units: i64,
    /// Takes free space in a flex container.
    pub flexible: bool,
}

pub fn layout_spacer(s: Spacer, c: Constraints, cx: &mut LayoutCx) -> Node {
    let v = cx.theme.px(s.units);
    Node::leaf(c.constrain(Size::new(v, v)))
}

/// A thin rule between groups.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Separator {
    pub vertical: bool,
}

pub fn layout_separator(s: Separator, c: Constraints, cx: &mut LayoutCx) -> Node {
    let t = cx.theme.border_width();
    let pad = cx.theme.px(4);
    if s.vertical {
        Node::leaf(c.constrain(Size::new(
            t + pad * 2,
            if c.has_bounded_height() {
                c.max_height
            } else {
                cx.theme.px(24)
            },
        )))
    } else {
        Node::leaf(c.constrain(Size::new(
            if c.has_bounded_width() {
                c.max_width
            } else {
                cx.theme.px(24)
            },
            t + pad * 2,
        )))
    }
}

pub fn paint_separator(s: Separator, node: &Node, cx: &mut PaintCx) {
    let t = cx.theme.border_width();
    let r = node.rect;
    let color = cx.theme.border;
    if s.vertical {
        cx.fill(Rect::new((r.width - t) / 2, 0, t, r.height), color)
    } else {
        cx.fill(Rect::new(0, (r.height - t) / 2, r.width, t), color)
    }
}

/// How an image fits the space it is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Fit {
    /// Whole image visible, aspect kept.
    Contain,
    /// Space filled, aspect kept, overflow clipped.
    Cover,
    /// Natural size.
    Natural,
}

/// An RGBA image.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Image {
    pub rgba: Vec<u8>,
    pub width: i64,
    pub height: i64,
    /// Alternative text; empty marks the image decorative.
    pub alt: String,
    pub fit: Fit,
    /// Shown instead of the image when it failed to load.
    pub error: String,
}

pub fn layout_image(i: &Image, c: Constraints, cx: &mut LayoutCx) -> Node {
    if !i.error.is_empty() || i.width <= 0 || i.height <= 0 {
        let s = cx.theme.px(48);
        return Node::leaf(c.constrain(Size::new(s, s)));
    }
    let natural = Size::new(cx.theme.px(i.width), cx.theme.px(i.height));
    if i.fit == Fit::Natural || (!c.has_bounded_width() && !c.has_bounded_height()) {
        return Node::leaf(c.constrain(natural));
    }
    let max_w = if c.has_bounded_width() {
        c.max_width
    } else {
        natural.width
    };
    let max_h = if c.has_bounded_height() {
        c.max_height
    } else {
        natural.height
    };
    let k = (max_w as f64 / natural.width as f64)
        .min(max_h as f64 / natural.height as f64)
        .min(1.0);
    Node::leaf(c.constrain(Size::new(
        (natural.width as f64 * k) as i64,
        (natural.height as f64 * k) as i64,
    )))
}

pub fn paint_image(i: &Image, node: &Node, cx: &mut PaintCx) {
    let r = cx.abs(Rect::from_size(node.size()));
    if !i.error.is_empty() || i.width <= 0 || i.height <= 0 {
        let border = cx.theme.border;
        cx.canvas
            .round_rect_stroke(r.x, r.y, r.width, r.height, cx.theme.px(4), 1, border);
        let muted = cx.theme.text_muted;
        cx.icon(
            &crate::render::icons::Icon::Error,
            r.x + (r.width - r.height / 2) / 2,
            r.y + r.height / 4,
            r.height / 2,
            muted,
        );
        return;
    }
    let k = match i.fit {
        Fit::Cover => (r.width as f64 / i.width as f64).max(r.height as f64 / i.height as f64),
        _ => (r.width as f64 / i.width as f64).min(r.height as f64 / i.height as f64),
    };
    let w = (i.width as f64 * k) as i64;
    let h = (i.height as f64 * k) as i64;
    let saved = cx.canvas.clip;
    cx.canvas.push_clip(r);
    cx.canvas.image_scaled(
        r.x + (r.width - w) / 2,
        r.y + (r.height - h) / 2,
        w,
        h,
        &i.rgba,
        i.width,
        i.height,
    );
    cx.canvas.pop_clip(saved)
}

pub fn image_semantics(i: &Image) -> Option<AccessNode> {
    if i.alt.is_empty() && i.error.is_empty() {
        return None;
    }
    Some(AccessNode::new(
        Role::Image,
        &if i.error.is_empty() {
            i.alt.clone()
        } else {
            crate::localization::message(
                "image",
                "{image} (failed to load: {error})",
                1,
                &crate::hmap! { "image" => i.alt.clone(), "error" => i.error.clone() },
            )
        },
    ))
}

/// A built-in vector icon.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Icon {
    pub icon: crate::render::icons::Icon,
    pub units: i64,
    pub tone: Tone,
    /// Accessible name; empty marks the icon decorative.
    pub label: String,
}

pub fn layout_icon(i: &Icon, c: Constraints, cx: &mut LayoutCx) -> Node {
    let s = cx.theme.px(i.units);
    Node::leaf(c.constrain(Size::new(s, s)))
}

pub fn paint_icon(i: &Icon, node: &Node, cx: &mut PaintCx) {
    let r = cx.abs(Rect::from_size(node.size()));
    let s = r.width.min(r.height);
    let color = if cx.disabled {
        cx.theme.text_disabled
    } else {
        i.tone.resolve(&cx.theme)
    };
    cx.icon(
        &i.icon,
        r.x + (r.width - s) / 2,
        r.y + (r.height - s) / 2,
        s,
        color,
    )
}

pub fn icon_semantics(i: &Icon) -> Option<AccessNode> {
    if i.label.is_empty() {
        return None;
    }
    Some(AccessNode::new(Role::Image, &i.label))
}

/// A count or status pill.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Badge {
    pub text: String,
    pub tone: Tone,
    /// Accessible name; empty reads `text`.
    pub label: String,
}

pub fn layout_badge(b: &Badge, c: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    let style = theme.font(TextRole::Small);
    let (tw, _) = layout::measure(&b.text, &style);
    let (a, _, lh) = layout::metrics(&style);
    let h = lh.ceil() as i64 + theme.px(4);
    let mut node = Node::leaf(c.constrain(Size::new((tw.ceil() as i64 + theme.px(12)).max(h), h)));
    node.baseline = theme.px(2) + a as i64;
    node
}

pub fn paint_badge(b: &Badge, node: &Node, cx: &mut PaintCx) {
    let theme = cx.theme.clone();
    let r = Rect::from_size(node.size());
    let (fill, fg) = badge_colors(&theme, b.tone, cx.disabled);
    cx.fill_round(r, r.height / 2, fill);
    let style = theme.font(TextRole::Small);
    let l = layout::layout(&b.text, &style, LayoutOptions::single_line());
    let a = cx.abs(r);
    layout::draw(
        &mut cx.canvas,
        &l,
        (a.x + (a.width - l.width.ceil() as i64) / 2) as f64,
        (a.y + (a.height - l.height.ceil() as i64) / 2) as f64,
        &style,
        fg,
    )
}

// The fill and text colors. The text is the theme's text or background
// color, whichever reads better, when that reaches 4.5:1 (WCAG AA) on the
// fill; otherwise white or black, one of which always does.
fn badge_colors(theme: &crate::style::Theme, tone: Tone, disabled: bool) -> (Color, Color) {
    let fill = if disabled {
        theme.text_disabled
    } else {
        tone.resolve(theme)
    };
    let themed = if theme.text.contrast(fill) >= theme.background.contrast(fill) {
        theme.text
    } else {
        theme.background
    };
    if themed.contrast(fill) >= 4.5 {
        return (fill, themed);
    }
    let plain = if Color::white().contrast(fill) >= Color::black().contrast(fill) {
        Color::white()
    } else {
        Color::black()
    };
    (fill, plain)
}

pub fn badge_semantics(b: &Badge) -> Option<AccessNode> {
    Some(AccessNode::new(
        Role::Label,
        if b.label.is_empty() {
            &b.text
        } else {
            &b.label
        },
    ))
}

/// The text layout cache is shared; this exposes edit state syncing for
/// tests of selectable labels.
pub fn selection_of(st: &StateStore, id: &str) -> (i64, i64) {
    let s: EditState = st.edit(id);
    s.selection()
}

/// Rich text exposes read-only text, selection and actionable inline links/images.
pub fn rich_label_semantics(
    l: &Label,
    node: &Node,
    states: &StateStore,
    theme: &crate::style::Theme,
) -> Option<AccessNode> {
    let mut semantic = label_semantics(l)?;
    if l.spans.is_empty() {
        return Some(semantic);
    }
    semantic.value = l.text.clone();
    semantic.multiline = l.wrap;
    let state = states.edit(&l.id);
    semantic.text_start = state.anchor;
    semantic.text_end = state.caret;
    semantic.caret = state.caret;
    let geometry = shared_label_layout(l, theme, node.rect.width);
    if let Some(error) = &geometry.error {
        semantic.description = error.clone();
        return Some(semantic);
    }
    // Walk shaped glyphs once. Calling range_rects separately for every link
    // rescans a long paragraph quadratically on the UI thread.
    let Ok(_temporary) = crate::app::effects::current_budget()
        .reserve(l.spans.len().saturating_mul(size_of::<Option<Rect>>()))
    else {
        semantic.description =
            "Rich inline accessibility geometry exceeds the session budget".into();
        return Some(semantic);
    };
    let mut bounds: Vec<Option<Rect>> = vec![None; l.spans.len()];
    for line in &geometry.lines {
        for glyph in &line.glyphs {
            let index = l
                .spans
                .partition_point(|span| span.end <= glyph.start.max(0) as usize);
            if let Some(span) = l.spans.get(index)
                && (span.image.is_some() || !span.marks.link.is_empty())
            {
                let x = (line.left + glyph.x).floor() as i64;
                let right = (line.left + glyph.x + glyph.advance + 0.5).floor() as i64;
                let rect = Rect::new(x, line.top as i64, (right - x).max(1), line.height as i64);
                bounds[index] = Some(bounds[index].map_or(rect, |current| current.union(rect)));
            }
        }
    }
    for (index, span) in l.spans.iter().enumerate() {
        let mut child = if let Some(image) = &span.image {
            if image.alt.is_empty() {
                continue;
            }
            AccessNode::new(Role::Image, &image.alt)
        } else if !span.marks.link.is_empty() {
            let mut link =
                AccessNode::new(Role::Link, l.text.get(span.start..span.end).unwrap_or(""));
            link.id = format!("{}#link:{index}", l.id);
            link.value = span.marks.link.clone();
            link.actions.push("press".into());
            link
        } else {
            continue;
        };
        if let Some(rect) = bounds[index] {
            child.bounds = rect;
        }
        semantic.children.push(child);
    }
    Some(semantic)
}

#[cfg(test)]
mod tests {
    use super::badge_colors;

    use crate::style::Theme;
    use crate::widgets::Tone;
    use crate::widgets::Widget;
    use windowing::Key;

    #[test]
    fn a_text_line_ends_in_an_ellipsis_where_it_does_not_fit() {
        let Widget::Text(l) = Widget::text_line(
            "A rather long application description",
            crate::style::TextRole::Body,
            Tone::Muted,
        ) else {
            return;
        };
        let tight = super::label_layout(&l, &crate::style::Theme::light(), 80);
        assert!(tight.elided && tight.width <= 80.0, "{}", tight.width);
        assert!(!super::label_layout(&l, &crate::style::Theme::light(), 2000).elided)
    }

    #[test]
    fn badge_text_reads_on_every_tone_in_every_theme() {
        let themes = vec![
            ("light", Theme::light()),
            ("dark", Theme::dark()),
            ("high contrast light", Theme::high_contrast_light()),
            ("high contrast dark", Theme::high_contrast_dark()),
        ];
        let tones = vec![
            ("normal", Tone::Normal),
            ("accent", Tone::Accent),
            ("danger", Tone::Danger),
            ("success", Tone::Success),
            ("warning", Tone::Warning),
            ("info", Tone::Info),
        ];
        for (name, theme) in themes {
            for &(tone_name, tone) in &tones {
                let (fill, fg) = badge_colors(&theme, tone, false);
                let ratio = fg.contrast(fill);
                assert!(
                    ratio >= 4.5,
                    "{} badge in {}: {:.2}",
                    tone_name,
                    name,
                    ratio
                )
            }
        }
    }

    fn link_view(m: &[String]) -> crate::widgets::Widget {
        crate::widgets::Widget::column(
            &[
                crate::widgets::Widget::link("docs", "Read the docs"),
                crate::widgets::Widget::label("plain"),
                crate::widgets::Widget::label(&format!("log {}", m.join("; "))),
            ],
            8,
        )
        .padding(8)
    }

    fn link_update(mut m: Vec<String>, msg: crate::widgets::cx::UiMsg) -> Vec<String> {
        if let crate::widgets::cx::UiMsg::Click(id) = msg {
            m.push(id)
        };
        m
    }

    #[test]
    fn a_link_opens_only_on_a_release_over_where_it_was_pressed() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Links",
                "dev.gossamer.gui-link-test",
                Vec::new,
                link_update,
                |m: &Vec<String>| link_view(m),
            )
            .size(420, 260)
            .start()
            .unwrap();
            let r = s.rect_of("docs").unwrap();
            let x = r.center_x();
            let y = r.center_y();
            s.press_pointer(x, y + 120).unwrap();
            s.release_pointer(x, y).unwrap();
            s.press_pointer(x, y).unwrap();
            s.move_pointer(x, y + 120, true).unwrap();
            s.release_pointer(x, y + 120).unwrap();
            s.click_at(x, y).unwrap();
            assert!(
                s.semantics(0).dump().contains("log docs\""),
                "{}",
                s.semantics(0).dump()
            );
            s.shutdown()
        })
    }

    fn select_view(_m: &i64) -> crate::widgets::Widget {
        crate::widgets::Widget::column(
            &[crate::widgets::Widget::selectable(
                "quote",
                "Hello brave new world",
            )],
            8,
        )
        .padding(8)
    }

    fn copied(s: &mut crate::Session) -> String {
        let mut mods = windowing::Mods::default();
        if crate::input::platform_is_macos() {
            mods.gui = true
        } else {
            mods.ctrl = true
        };
        s.press(Key::C, mods).unwrap();
        crate::services::clipboard::text()
    }

    #[test]
    fn a_selectable_label_selects_by_keyboard_and_copies() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            crate::services::clipboard::use_memory(true);
            let mut s = crate::application(
                "Select",
                "dev.gossamer.gui-select-label-test",
                || 0,
                |m: i64, _msg: crate::widgets::cx::UiMsg| m,
                select_view,
            )
            .size(420, 200)
            .start()
            .unwrap();
            assert!(s.focus("quote").unwrap());
            let mut word = windowing::Mods {
                shift: true,
                ..windowing::Mods::default()
            };
            if crate::input::platform_is_macos() {
                word.alt = true
            } else {
                word.ctrl = true
            };
            s.press(Key::Home, windowing::Mods::default()).unwrap();
            s.press(Key::Right, word).unwrap();
            assert_eq!(copied(&mut s), "Hello");
            s.press(
                Key::End,
                windowing::Mods {
                    shift: true,
                    ..windowing::Mods::default()
                },
            )
            .unwrap();
            assert_eq!(copied(&mut s), "Hello brave new world");
            s.press(Key::Home, windowing::Mods::default()).unwrap();
            s.press(
                Key::Right,
                windowing::Mods {
                    shift: true,
                    ..windowing::Mods::default()
                },
            )
            .unwrap();
            assert_eq!(copied(&mut s), "H");
            // A double click inside the first word selects it.
            let r = s.rect_of("quote").unwrap();
            s.click_at(r.x + 6, r.center_y()).unwrap();
            s.click_at(r.x + 6, r.center_y()).unwrap();
            assert_eq!(copied(&mut s), "Hello");
            s.shutdown()
        })
    }
}
