// containers: widgets that arrange other widgets. Flex rows and columns
// (with grow weights, main and cross alignment, baselines, and mirroring
// in right-to-left layouts), grids and forms, stacks, wrapping flows,
// scroll areas, split panes, and the modifier box every builder modifier
// produces (padding, margins, size limits, alignment, backgrounds,
// borders, tooltips, visibility, disabled subtrees, theme overrides).
use crate::strext::StrExt;
use windowing;
use windowing::{Key, MouseButton};

use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Insets, Rect, Size, UNBOUNDED};
use crate::style::TextRole;
use crate::text::layout;
use crate::text::layout::LayoutOptions;
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx, Placement, PopupRequest, UiMsg};
use crate::widgets::state::{ScrollState, StateStore, WState};
use crate::widgets::{Axis, CrossAlign, MainAlign, Modifiers, Visibility, Widget, paint_children};

// ---- flex ----

/// A row or column of children.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Flex {
    pub axis: Axis,
    pub children: Vec<Widget>,
    /// Gap between children, interface units.
    pub spacing: i64,
    pub main: MainAlign,
    pub cross: CrossAlign,
}

impl Flex {
    pub fn new(axis: Axis, children: &[Widget], spacing: i64) -> Flex {
        let cross = if axis == Axis::Vertical {
            CrossAlign::Start
        } else {
            CrossAlign::Center
        };
        Flex {
            axis,
            children: children.to_vec(),
            spacing,
            main: MainAlign::Start,
            cross,
        }
    }
}

fn main_of(axis: Axis, s: Size) -> i64 {
    if axis == Axis::Horizontal {
        s.width
    } else {
        s.height
    }
}

fn cross_of(axis: Axis, s: Size) -> i64 {
    if axis == Axis::Horizontal {
        s.height
    } else {
        s.width
    }
}

fn make_constraints(
    axis: Axis,
    main_min: i64,
    main_max: i64,
    cross_min: i64,
    cross_max: i64,
) -> Constraints {
    if axis == Axis::Horizontal {
        Constraints::new(main_min, main_max, cross_min, cross_max)
    } else {
        Constraints::new(cross_min, cross_max, main_min, main_max)
    }
}

pub fn layout_flex(f: &Flex, c: Constraints, cx: &mut LayoutCx) -> Node {
    let axis = f.axis;
    let horizontal = axis == Axis::Horizontal;
    let spacing = cx.theme.space(1) * f.spacing / 4;
    let max_main = if horizontal {
        c.max_width
    } else {
        c.max_height
    };
    let max_cross = if horizontal {
        c.max_height
    } else {
        c.max_width
    };
    let bounded_main = max_main < UNBOUNDED;
    let bounded_cross = max_cross < UNBOUNDED;
    let stretch = f.cross == CrossAlign::Stretch && bounded_cross;
    let n = f.children.len();
    let mut nodes: Vec<Node> = vec![];
    let mut i = 0;
    while i < n {
        nodes.push(Node::default());
        i += 1;
    }
    let mut visible = 0;
    let mut total_grow = 0;
    i = 0;
    while i < n {
        if !f.children[i].is_collapsed() {
            visible += 1;
            total_grow += f.children[i].grow_weight();
        }
        i += 1;
    }
    let gaps = if visible > 1 {
        spacing * (visible - 1)
    } else {
        0
    };
    let cross_min = if stretch { max_cross } else { 0 };
    let mut used = gaps;
    // Fixed-size children first.
    i = 0;
    while i < n {
        let child = &f.children[i];
        if child.is_collapsed() {
            nodes[i] = child.layout(Constraints::tight(Size::zero()), cx);
        } else if child.grow_weight() == 0 || !bounded_main {
            let room = if bounded_main {
                (max_main - used).max(0)
            } else {
                UNBOUNDED
            };
            nodes[i] = child.layout(make_constraints(axis, 0, room, cross_min, max_cross), cx);
            used += main_of(axis, nodes[i].size());
        }
        i += 1;
    }
    // Growing children share what is left.
    if bounded_main && total_grow > 0 {
        let free = (max_main - used).max(0);
        let mut handed = 0;
        let mut grown = 0;
        i = 0;
        while i < n {
            let child = &f.children[i];
            let w = child.grow_weight();
            if w > 0 && !child.is_collapsed() {
                grown += w;
                let share = if grown == total_grow {
                    free - handed
                } else {
                    free * w / total_grow
                };
                handed += share;
                let least = if child.fits_content() { 0 } else { share };
                nodes[i] = child.layout(
                    make_constraints(axis, least, share, cross_min, max_cross),
                    cx,
                );
                used += main_of(axis, nodes[i].size());
            }
            i += 1;
        }
    }
    let mut cross_size = 0;
    let mut max_ascent = 0;
    for node in &nodes {
        cross_size = cross_size.max(cross_of(axis, node.size()));
        if node.baseline > max_ascent {
            max_ascent = node.baseline
        }
    }
    if stretch {
        cross_size = max_cross
    };
    let mut main_size = used;
    if bounded_main && f.main != MainAlign::Start && total_grow == 0 {
        main_size = max_main;
    }
    let min_main = if horizontal {
        c.min_width
    } else {
        c.min_height
    };
    main_size = main_size.max(min_main);
    let min_cross = if horizontal {
        c.min_height
    } else {
        c.min_width
    };
    cross_size = cross_size.max(min_cross);
    if f.cross == CrossAlign::Baseline && horizontal {
        // Baseline rows grow to fit the deepest descender below the shared baseline.
        let mut deepest = 0;
        for node in &nodes {
            let b = if node.baseline >= 0 {
                node.baseline
            } else {
                node.rect.height
            };
            deepest = deepest.max(max_ascent - b + node.rect.height);
        }
        cross_size = cross_size.max(deepest);
    }
    let mut start = 0;
    let mut between = spacing;
    if total_grow == 0 && bounded_main {
        let free = main_size - used;
        match f.main {
            MainAlign::Start => (),
            MainAlign::Center => start = free / 2,
            MainAlign::End => start = free,
            MainAlign::SpaceBetween => {
                if visible > 1 {
                    between = spacing + free / (visible - 1)
                }
            }
            MainAlign::SpaceAround => {
                if visible > 0 {
                    let each = free / visible;
                    start = each / 2;
                    between = spacing + each;
                }
            }
        }
    }
    let mut pos = start;
    let mut placed = vec![];
    i = 0;
    while i < n {
        let node = &nodes[i];
        if f.children[i].is_collapsed() {
            placed.push(node.at(0, 0));
            i += 1;
            continue;
        }
        let size = node.size();
        let cs = cross_of(axis, size);
        let cross_pos = match f.cross {
            CrossAlign::Start | CrossAlign::Stretch => 0,
            CrossAlign::Center => (cross_size - cs) / 2,
            CrossAlign::End => cross_size - cs,
            CrossAlign::Baseline => {
                if node.baseline >= 0 {
                    max_ascent - node.baseline
                } else {
                    (cross_size - cs) / 2
                }
            }
        };
        let p = if horizontal {
            node.at(pos, cross_pos)
        } else {
            node.at(cross_pos, pos)
        };
        placed.push(p);
        pos += main_of(axis, size) + between;
        i += 1;
    }
    let size = if horizontal {
        c.constrain(Size::new(main_size, cross_size))
    } else {
        c.constrain(Size::new(cross_size, main_size))
    };
    // Right to left, the start edge is the right one: a row reverses, and a
    // column's start-aligned children sit against the right edge.
    if cx.theme.rtl {
        let mut k = 0;
        while k < placed.len() {
            let r = placed[k].rect;
            placed[k] = placed[k].at(size.width - r.x - r.width, r.y);
            k += 1;
        }
    }
    let mut out = Node::leaf(size);
    out.children = placed;
    if horizontal && f.cross == CrossAlign::Baseline {
        out.baseline = max_ascent;
    } else if horizontal {
        let mut b = -1;
        for p in &out.children {
            if p.baseline >= 0 {
                b = b.max(p.baseline + p.rect.y)
            }
        }
        out.baseline = b;
    } else if !out.children.is_empty() && out.children[0].baseline >= 0 {
        out.baseline = out.children[0].baseline + out.children[0].rect.y;
    }
    out
}

// ---- grid ----

/// Cells in columns, row-major.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Grid {
    pub children: Vec<Widget>,
    pub columns: i64,
    pub column_gap: i64,
    pub row_gap: i64,
    /// The last column takes the remaining width.
    pub stretch_last: bool,
}

pub fn layout_grid(g: &Grid, c: Constraints, cx: &mut LayoutCx) -> Node {
    let cols = g.columns.max(1);
    let n = g.children.len();
    let rows = n.div_ceil(cols as usize);
    let cgap = cx.theme.space(1) * g.column_gap / 4;
    let rgap = cx.theme.space(1) * g.row_gap / 4;
    let mut widths = vec![0; cols as usize];
    let mut i = 0;
    while i < n {
        let col = i % cols as usize;
        if !(g.stretch_last && col == (cols - 1) as usize) {
            // Intrinsic width: a filling child (an aligned label) would claim
            // any bounded width offered here.
            let natural =
                g.children[i].layout(Constraints::loose(Size::new(UNBOUNDED, UNBOUNDED)), cx);
            widths[col] = widths[col].max(natural.rect.width.min(c.max_width));
        }
        i += 1;
    }
    let fixed = widths.iter().sum::<i64>() + cgap * (cols - 1);
    if g.stretch_last {
        let last = if c.has_bounded_width() {
            (c.max_width - fixed).max(0)
        } else {
            0
        };
        let mut natural_last = 0;
        i = (cols - 1) as usize;
        while i < n {
            let nl = g.children[i].layout(Constraints::loose(Size::new(UNBOUNDED, UNBOUNDED)), cx);
            natural_last = natural_last.max(nl.rect.width);
            i += cols as usize;
        }
        widths[(cols - 1) as usize] = if c.has_bounded_width() {
            last
        } else {
            natural_last
        }
    } else if c.has_bounded_width() && fixed > c.max_width {
        // Too wide: shrink the widest column first.
        let mut over = fixed - c.max_width;
        while over > 0 {
            let mut widest = 0;
            let mut k = 1;
            while k < cols {
                if widths[k as usize] > widths[widest] {
                    widest = k as usize
                };
                k += 1;
            }
            let cut = over.min(widths[widest] / 2).max(1);
            widths[widest] -= cut;
            over -= cut;
            if widths[widest] <= 0 {
                break;
            }
        }
    }
    let mut nodes = vec![];
    i = 0;
    while i < n {
        let w = widths[i % cols as usize];
        nodes.push(g.children[i].layout(Constraints::new(w, w, 0, UNBOUNDED), cx));
        i += 1;
    }
    // Rows align on baselines, so a label stays beside its field's text when
    // the field grows a line beneath it.
    let mut heights = vec![0; rows];
    let mut baselines = vec![-1; rows];
    let mut dys = vec![0; n];
    for row in 0..rows {
        let members: Vec<i64> = ((row * cols as usize) as i64
            ..((row * cols as usize + cols as usize).min(n)) as i64)
            .collect();
        let (height, baseline, offsets) = align_line(&nodes, &members);
        heights[row] = height;
        baselines[row] = baseline;
        for (k, &i) in members.iter().enumerate() {
            dys[i as usize] = offsets[k]
        }
    }
    let mut lefts = vec![0; cols as usize];
    let mut x = 0;
    i = 0;
    while i < cols as usize {
        lefts[i] = x;
        x += widths[i] + cgap;
        i += 1;
    }
    let total_w = (x - cgap).max(0);
    let mut tops = vec![0; rows];
    let mut y = 0;
    i = 0;
    while i < rows {
        tops[i] = y;
        y += heights[i] + rgap;
        i += 1;
    }
    let total_h = (y - rgap).max(0);
    let size = c.constrain(Size::new(total_w, total_h));
    let mut placed = vec![];
    i = 0;
    while i < n {
        let col = i % cols as usize;
        let row = i / cols as usize;
        let node = &nodes[i];
        let ty = tops[row] + dys[i];
        let mut tx = lefts[col];
        if cx.theme.rtl {
            tx = size.width - tx - node.rect.width;
        }
        placed.push(node.at(tx, ty));
        i += 1;
    }
    let mut out = Node::leaf(size);
    out.children = placed;
    if rows > 0 {
        out.baseline = baselines[0]
    };
    out
}

// ---- stack ----

/// Layers painted bottom to top, sharing one area.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stack {
    pub layers: Vec<Widget>,
}

pub fn layout_stack(s: &Stack, c: Constraints, cx: &mut LayoutCx) -> Node {
    let mut size = Size::zero();
    for layer in &s.layers {
        let n = layer.layout(c.loosen(), cx);
        size.width = size.width.max(n.rect.width);
        size.height = size.height.max(n.rect.height);
    }
    let size = c.constrain(size);
    let mut out = Node::leaf(size);
    for layer in &s.layers {
        out.children
            .push(layer.layout(Constraints::tight(size), cx))
    }
    out
}

// ---- flow ----

/// Children that wrap into lines.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Flow {
    pub children: Vec<Widget>,
    pub spacing: i64,
    pub line_spacing: i64,
}

// How cells sharing a line align: those with a baseline on their tallest
// ascent, as one group centered in the line, and the rest each centered.
// Answers the line's height, its baseline (-1 without one), and each
// member's offset from the line's top.
fn align_line(nodes: &[Node], members: &[i64]) -> (i64, i64, Vec<i64>) {
    let mut ascent = -1;
    for &i in members {
        ascent = ascent.max(nodes[i as usize].baseline)
    }
    let mut group = 0;
    let mut height = 0;
    for &i in members {
        let n = &nodes[i as usize];
        if n.baseline >= 0 {
            group = group.max(ascent - n.baseline + n.rect.height)
        } else {
            height = height.max(n.rect.height)
        }
    }
    height = height.max(group);
    let lead = (height - group) / 2;
    let offsets = members
        .iter()
        .map(|&i| {
            let i = i as usize;
            if nodes[i].baseline >= 0 {
                lead + ascent - nodes[i].baseline
            } else {
                (height - nodes[i].rect.height) / 2
            }
        })
        .collect::<Vec<_>>();
    (
        height,
        if ascent >= 0 { lead + ascent } else { -1 },
        offsets,
    )
}

pub fn layout_flow(f: &Flow, c: Constraints, cx: &mut LayoutCx) -> Node {
    let gap = cx.theme.space(1) * f.spacing / 4;
    let lgap = cx.theme.space(1) * f.line_spacing / 4;
    let limit = if c.has_bounded_width() {
        c.max_width
    } else {
        UNBOUNDED
    };
    let mut nodes = vec![];
    for child in &f.children {
        nodes.push(child.layout(Constraints::loose(Size::new(limit, UNBOUNDED)), cx))
    }
    // Break into lines, then align each line's items with align_line.
    let mut lines: Vec<Vec<i64>> = vec![];
    let mut line: Vec<i64> = vec![];
    let mut xs = vec![];
    let mut x = 0;
    let mut widest = 0;
    for (i, n) in nodes.iter().enumerate() {
        let s = n.size();
        if x > 0 && x + s.width > limit {
            lines.push(line);
            line = vec![];
            x = 0;
        }
        xs.push(x);
        line.push(i as i64);
        x += s.width + gap;
        widest = widest.max(x - gap);
    }
    if !line.is_empty() {
        lines.push(line)
    };
    let mut placed = vec![];
    let mut y = 0;
    let mut first_baseline = -1;
    for (k, members) in lines.iter().enumerate() {
        let (height, baseline, offsets) = align_line(&nodes, members);
        for (j, &i) in members.iter().enumerate() {
            placed.push(nodes[i as usize].at(xs[i as usize], y + offsets[j]))
        }
        if k == 0 {
            first_baseline = baseline
        };
        y += height + if k + 1 < lines.len() { lgap } else { 0 };
    }
    let size = c.constrain(Size::new(widest, y));
    if cx.theme.rtl {
        let mut k = 0;
        while k < placed.len() {
            let r = placed[k].rect;
            placed[k] = placed[k].at(size.width - r.x - r.width, r.y);
            k += 1;
        }
    }
    let mut out = Node::leaf(size);
    out.children = placed;
    out.baseline = first_baseline;
    out
}

// ---- scroll ----

/// A clipped viewport that scrolls its child.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Scroll {
    pub id: String,
    pub child: Box<Widget>,
    pub horizontal: bool,
    pub vertical: bool,
    /// As tall as the content, up to the height offered, instead of filling it.
    pub fit: bool,
}

/// Scrollbar thickness, pixels.
fn bar(cx_theme: &crate::style::Theme) -> i64 {
    cx_theme.px(10)
}

pub fn layout_scroll(s: &Scroll, c: Constraints, cx: &mut LayoutCx) -> Node {
    let child_c = Constraints {
        min_width: if s.horizontal { 0 } else { c.min_width },
        max_width: if s.horizontal { UNBOUNDED } else { c.max_width },
        min_height: 0,
        max_height: if s.vertical { UNBOUNDED } else { c.max_height },
    };
    let child = s.child.layout(child_c, cx);
    let content = child.size();
    // A viewport spans the width offered. It takes the height offered only
    // when it scrolls vertically, since a column offers every child the
    // height left over, and a fitting viewport stops at its content's height;
    // otherwise it is as tall as its content, plus the scrollbar's track when
    // that content overflows, so the thumb never covers it.
    let width = if c.has_bounded_width() {
        c.max_width
    } else {
        content.width
    };
    let height = if s.vertical {
        if c.has_bounded_height() {
            if s.fit {
                content.height.min(c.max_height)
            } else {
                c.max_height
            }
        } else {
            content.height.min(cx.theme.px(400))
        }
    } else if s.horizontal && content.width > width {
        content.height + bar(&cx.theme)
    } else {
        content.height
    };
    let size = c.constrain(Size::new(width, height));
    let st = clamp_scroll(cx.states.scroll(&s.id), content, size);
    let mut out = Node::leaf(size);
    out.content = content;
    out.children = vec![child.at(-st.x, -st.y)];
    out
}

/// A scroll state with offsets clamped to the content.
pub fn clamp_scroll(st: ScrollState, content: Size, viewport: Size) -> ScrollState {
    let mut s = st;
    s.x = s.x.max(0).min((content.width - viewport.width).max(0));
    s.y = s.y.max(0).min((content.height - viewport.height).max(0));
    s
}

// The vertical thumb's rectangle (local), or an empty one.
fn v_thumb(node: &Node, st: ScrollState, b: i64) -> Rect {
    let vh = node.rect.height;
    let ch = node.content.height;
    if ch <= vh || vh <= 0 {
        return Rect::new(0, 0, 0, 0);
    }
    let thumb = (vh * vh / ch).max(b * 2);
    let max_off = ch - vh;
    let ty = st.y * (vh - thumb) / max_off.max(1);
    Rect::new(node.rect.width - b, ty, b, thumb)
}

fn h_thumb(node: &Node, st: ScrollState, b: i64) -> Rect {
    let vw = node.rect.width;
    let cw = node.content.width;
    if cw <= vw || vw <= 0 {
        return Rect::new(0, 0, 0, 0);
    }
    let thumb = (vw * vw / cw).max(b * 2);
    let max_off = cw - vw;
    let tx = st.x * (vw - thumb) / max_off.max(1);
    Rect::new(tx, node.rect.height - b, thumb, b)
}

pub fn paint_scroll(s: &Scroll, node: &Node, cx: &mut PaintCx) {
    let r = cx.abs(Rect::from_size(node.size()));
    let saved = cx.canvas.clip;
    cx.canvas.push_clip(r);
    paint_children(&[&*s.child], node, cx);
    cx.canvas.pop_clip(saved);
    let st = clamp_scroll(cx.states.scroll(&s.id), node.content, node.size());
    let b = bar(&cx.theme);
    let color = if st.drag >= 0 {
        cx.theme.text_muted
    } else {
        cx.theme.border_strong.with_alpha(160)
    };
    let vt = v_thumb(node, st, b);
    if !vt.is_empty() {
        cx.fill_round(
            Rect::new(vt.x + b / 4, vt.y + 2, b / 2, vt.height - 4),
            b / 4,
            color,
        )
    }
    let ht = h_thumb(node, st, b);
    if !ht.is_empty() {
        cx.fill_round(
            Rect::new(ht.x + 2, ht.y + b / 4, ht.width - 4, b / 2),
            b / 4,
            color,
        )
    }
}

pub fn scroll_event(s: &Scroll, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    let mut state = clamp_scroll(st.scroll(&s.id), node.content, node.size());
    let before = state;
    let b = bar(&cx.theme);
    let ox = cx.origin_x;
    let oy = cx.origin_y;
    let local_x = |x: i64| -> i64 { x - ox };
    let local_y = |y: i64| -> i64 { y - oy };
    let line = cx.theme.px(48);
    match ev {
        Event::Scroll { dx, dy, mods, .. } => {
            let mut vy = dy;
            let mut vx = dx;
            if mods.shift && *vx == 0.0 {
                vx = vy;
                vy = &0.0;
            }
            if s.vertical && *vy != 0.0 {
                state.y -= (vy * line as f64) as i64;
            }
            if s.horizontal && *vx != 0.0 {
                state.x += (vx * line as f64) as i64;
            }
            state = clamp_scroll(state, node.content, node.size());
            // Hand the wheel to an outer scroller once this one is at its end.
            if state != before {
                cx.consume()
            }
        }
        Event::PointerDown { x, y, .. } => {
            let lx = local_x(*x);
            let ly = local_y(*y);
            let vt = v_thumb(node, state, b);
            let ht = h_thumb(node, state, b);
            if !vt.is_empty() && lx >= node.rect.width - b {
                if vt.contains(lx, ly) {
                    state.drag = ly - vt.y;
                } else {
                    // Clicking the track pages toward the pointer.
                    state.y += if ly < vt.y {
                        -node.rect.height
                    } else {
                        node.rect.height
                    };
                    state.drag = -1;
                }
                state.drag_horizontal = false;
                cx.capture_pointer(&s.id);
                cx.consume()
            } else if !ht.is_empty() && ly >= node.rect.height - b {
                if ht.contains(lx, ly) {
                    state.drag = lx - ht.x;
                } else {
                    state.x += if lx < ht.x {
                        -node.rect.width
                    } else {
                        node.rect.width
                    };
                    state.drag = -1;
                }
                state.drag_horizontal = true;
                cx.capture_pointer(&s.id);
                cx.consume()
            }
        }
        Event::PointerMove { x, y, pressed, .. } if *pressed && state.drag >= 0 => {
            if state.drag_horizontal {
                let ht = h_thumb(node, state, b);
                let track = node.rect.width - ht.width;
                let pos = local_x(*x) - state.drag;
                state.x = pos * (node.content.width - node.rect.width) / track.max(1);
            } else {
                let vt = v_thumb(node, state, b);
                let track = node.rect.height - vt.height;
                let pos = local_y(*y) - state.drag;
                state.y = pos * (node.content.height - node.rect.height) / track.max(1);
            }
            cx.consume()
        }
        Event::PointerUp { .. } if (state.drag >= 0 || cx.capture.is_some()) => {
            state.drag = -1;
            cx.release_pointer()
        }
        Event::KeyDown { key, mods, .. } => {
            let page = (node.rect.height - line).max(line);
            if *key == Key::PageDown {
                state.y += page;
                cx.consume()
            } else if *key == Key::PageUp {
                state.y -= page;
                cx.consume()
            } else if *key == Key::Home && mods.ctrl {
                state.y = 0;
                cx.consume()
            } else if *key == Key::End && mods.ctrl {
                state.y = node.content.height;
                cx.consume()
            }
        }
        _ => (),
    }
    state = clamp_scroll(state, node.content, node.size());
    if state != before {
        cx.redraw = true;
        if state.x != before.x || state.y != before.y {
            cx.relayout = true;
        }
    }
    st.set(&s.id, WState::Scroll(state))
}

pub fn scroll_semantics(s: &Scroll, node: &Node, st: &StateStore) -> Option<AccessNode> {
    let mut n = AccessNode::new(Role::ScrollArea, "");
    n.id = s.id.clone();
    let state = clamp_scroll(st.scroll(&s.id), node.content, node.size());
    let max = (node.content.height - node.rect.height).max(0);
    if max > 0 {
        n.range(0.0, max as f64, state.y as f64)
    }
    n.actions.push("scroll".to_string());
    Some(n)
}

/// Adjusts every scroll state on `path` so `target` (window pixels) shows.
pub fn reveal_in(
    st: &mut StateStore,
    scroll_id: &str,
    viewport: Rect,
    content: Size,
    target: Rect,
) {
    let mut s = st.scroll(scroll_id);
    if target.y < viewport.y {
        s.y -= viewport.y - target.y;
    } else if target.bottom() > viewport.bottom() {
        s.y += (target.bottom() - viewport.bottom()).min(target.y - viewport.y);
    }
    if target.x < viewport.x {
        s.x -= viewport.x - target.x;
    } else if target.right() > viewport.right() {
        s.x += (target.right() - viewport.right()).min(target.x - viewport.x);
    }
    st.set(
        scroll_id,
        WState::Scroll(clamp_scroll(s, content, viewport.size())),
    )
}

// ---- split ----

/// Two panes with a draggable divider.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Split {
    pub id: String,
    pub first: Box<Widget>,
    pub second: Box<Widget>,
    pub axis: Axis,
    pub ratio: f64,
    /// Minimum pane sizes, interface units.
    pub min_first: i64,
    pub min_second: i64,
}

fn split_ratio(s: &Split, st: &StateStore) -> f64 {
    let d = st.drag(&s.id);
    if d.active { d.start_value } else { s.ratio }
}

pub fn layout_split(s: &Split, c: Constraints, cx: &mut LayoutCx) -> Node {
    let handle = cx.theme.px(6);
    let size = c.biggest(Size::new(cx.theme.px(600), cx.theme.px(400)));
    let horizontal = s.axis == Axis::Horizontal;
    let total = if horizontal { size.width } else { size.height };
    let avail = (total - handle).max(0);
    let min_a = cx.theme.px(s.min_first).min(avail / 2);
    let min_b = cx.theme.px(s.min_second).min(avail / 2);
    let ratio = split_ratio(s, &cx.states).clamp(0.0, 1.0);
    let a = ((avail as f64 * ratio) as i64)
        .max(min_a)
        .min(avail - min_b);
    let b = avail - a;
    let mut out = Node::leaf(size);
    if horizontal {
        out.children.push(
            s.first
                .layout(Constraints::tight(Size::new(a, size.height)), cx)
                .at(0, 0),
        );
        out.children.push(
            s.second
                .layout(Constraints::tight(Size::new(b, size.height)), cx)
                .at(a + handle, 0),
        );
        out.parts.push(Rect::new(a, 0, handle, size.height))
    } else {
        out.children.push(
            s.first
                .layout(Constraints::tight(Size::new(size.width, a)), cx)
                .at(0, 0),
        );
        out.children.push(
            s.second
                .layout(Constraints::tight(Size::new(size.width, b)), cx)
                .at(0, a + handle),
        );
        out.parts.push(Rect::new(0, a, size.width, handle))
    }
    out
}

pub fn paint_split(s: &Split, node: &Node, cx: &mut PaintCx) {
    let r = cx.abs(Rect::from_size(node.size()));
    let saved = cx.canvas.clip;
    let ox = cx.origin_x;
    let oy = cx.origin_y;
    let mut i = 0;
    let kids = [s.first.clone(), s.second.clone()];
    while i < 2 && i < node.children.len() {
        let cn = &node.children[i];
        let cr = cn.rect.offset(ox, oy);
        cx.canvas.push_clip(cr.intersect(r));
        cx.origin_x = cr.x;
        cx.origin_y = cr.y;
        kids[i].paint(cn, cx);
        cx.canvas.pop_clip(saved);
        i += 1;
    }
    cx.origin_x = ox;
    cx.origin_y = oy;
    if node.parts.is_empty() {
        return;
    }
    let h = node.parts[0];
    let lit = cx.is_hover(&s.id) || cx.states.drag(&s.id).active;
    let color = if lit {
        cx.theme.accent
    } else {
        cx.theme.border
    };
    let t = cx.theme.border_width();
    if s.axis == Axis::Horizontal {
        cx.fill(Rect::new(h.x + (h.width - t) / 2, h.y, t, h.height), color)
    } else {
        cx.fill(Rect::new(h.x, h.y + (h.height - t) / 2, h.width, t), color)
    }
    if cx.is_focused(&s.id) {
        cx.focus_ring(h, 0)
    }
}

pub fn split_event(s: &Split, node: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    if node.parts.is_empty() {
        return;
    }
    let handle = node.parts[0];
    let horizontal = s.axis == Axis::Horizontal;
    let mut d = st.drag(&s.id);
    let total = if horizontal {
        node.rect.width
    } else {
        node.rect.height
    };
    let avail = (total - handle.width.min(handle.height)).max(1);
    match ev {
        Event::PointerDown { x, y, .. } => {
            let lx = x - cx.origin_x;
            let ly = y - cx.origin_y;
            if handle.contains(lx, ly) {
                d.active = true;
                d.start_value = split_ratio(s, st);
                cx.focus_on(&s.id);
                cx.capture_pointer(&s.id);
                cx.consume()
            }
        }
        Event::PointerMove { x, y, pressed, .. } => {
            let lx = x - cx.origin_x;
            let ly = y - cx.origin_y;
            if handle.contains(lx, ly) || d.active {
                cx.cursor = Some(if horizontal {
                    windowing::SystemCursor::ResizeEw
                } else {
                    windowing::SystemCursor::ResizeNs
                });
            }
            if *pressed && d.active {
                let pos = if horizontal { lx } else { ly };
                d.start_value = (pos as f64 / avail as f64).clamp(0.0, 1.0);
                cx.emit(&UiMsg::Split(s.id.clone(), d.start_value));
                cx.consume()
            }
        }
        Event::PointerUp { .. } if d.active => {
            d.active = false;
            cx.release_pointer();
            cx.emit(&UiMsg::Split(s.id.clone(), d.start_value))
        }
        Event::KeyDown { key, .. } if cx.is_focused(&s.id) => {
            let step = if (horizontal && *key == Key::Left) || (!horizontal && *key == Key::Up) {
                -0.02
            } else if (horizontal && *key == Key::Right) || (!horizontal && *key == Key::Down) {
                0.02
            } else {
                0.0
            };
            if step != 0.0 {
                cx.emit(&UiMsg::Split(
                    s.id.clone(),
                    (s.ratio + step).clamp(0.0, 1.0),
                ));
                cx.consume()
            }
        }
        _ => (),
    }
    st.set(&s.id, WState::Drag(d))
}

pub fn split_semantics(s: &Split, node: &Node) -> Option<AccessNode> {
    let mut n = AccessNode::control(Role::Splitter, &s.id, "", "");
    n.range(0.0, 100.0, (s.ratio * 100.0).round());
    n.actions.push("increment".to_string());
    n.actions.push("decrement".to_string());
    if !node.parts.is_empty() {
        n.bounds = node.parts[0];
    }
    Some(n)
}

// ---- modifier box ----

/// What a drag source offers when the pointer drags it away.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DragSource {
    pub items: Vec<windowing::DragItem>,
    pub operation: windowing::DragOperation,
}

/// A widget plus modifiers (see `Modifiers`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecoratedBox {
    pub child: Box<Widget>,
    pub mods: Modifiers,
}

fn units_insets(theme: &crate::style::Theme, i: Insets) -> Insets {
    let r = Insets::new(
        theme.px(i.left),
        theme.px(i.top),
        theme.px(i.right),
        theme.px(i.bottom),
    );
    if theme.rtl { r.mirrored() } else { r }
}

pub fn layout_decorated(d: &DecoratedBox, c: Constraints, cx: &mut LayoutCx) -> Node {
    let m = &d.mods;
    if m.visibility == Visibility::Collapsed {
        let mut out = Node::leaf(Size::zero());
        out.children.push(Node::leaf(Size::zero()));
        return out;
    }
    let saved = &cx.theme.clone();
    if let Some(ref change) = m.theme {
        cx.theme = change.apply(cx.theme.clone());
    }
    let theme = cx.theme.clone();
    let margin = units_insets(&theme, m.margin);
    let padding = units_insets(&theme, m.padding);
    let mut inner = c.deflate(margin);
    // Explicit sizes and limits (interface units).
    if m.width >= 0 {
        let w = theme.px(m.width).max(inner.min_width).min(inner.max_width);
        inner.min_width = w;
        inner.max_width = w;
    }
    if m.height >= 0 {
        let h = theme
            .px(m.height)
            .max(inner.min_height)
            .min(inner.max_height);
        inner.min_height = h;
        inner.max_height = h;
    }
    if m.min_width >= 0 {
        inner.min_width = inner
            .min_width
            .max(theme.px(m.min_width))
            .min(inner.max_width)
    };
    if m.min_height >= 0 {
        inner.min_height = inner
            .min_height
            .max(theme.px(m.min_height))
            .min(inner.max_height)
    };
    if m.max_width >= 0 {
        inner.max_width = inner
            .max_width
            .min(theme.px(m.max_width))
            .max(inner.min_width)
    };
    if m.max_height >= 0 {
        inner.max_height = inner
            .max_height
            .min(theme.px(m.max_height))
            .max(inner.min_height)
    };
    let aligned = m.align_x >= 0.0 || m.align_y >= 0.0;
    let mut child_c = inner.deflate(padding);
    if aligned {
        child_c = child_c.loosen();
    }
    if m.aspect > 0.0 && child_c.has_bounded_width() {
        let h = (child_c.max_width as f64 / m.aspect) as i64;
        child_c.max_height = child_c.max_height.min(h);
        child_c.min_height = child_c.min_height.min(child_c.max_height);
    }
    let child = d.child.layout(child_c, cx);
    let content = child.size().inflate(padding);
    let mut box_size = inner.constrain(content);
    if aligned {
        // An aligned child sits in all the space it was offered.
        if m.align_x >= 0.0 && inner.has_bounded_width() {
            box_size.width = inner.max_width
        };
        if m.align_y >= 0.0 && inner.has_bounded_height() {
            box_size.height = inner.max_height
        }
    }
    let free_w = box_size.width - padding.horizontal() - child.rect.width;
    let free_h = box_size.height - padding.vertical() - child.rect.height;
    let ax = if m.align_x >= 0.0 {
        if theme.rtl {
            1.0 - m.align_x
        } else {
            m.align_x
        }
    } else {
        0.0
    };
    let ay = if m.align_y >= 0.0 { m.align_y } else { 0.0 };
    let cx0 = margin.left + padding.left + (free_w.max(0) as f64 * ax) as i64;
    let cy0 = margin.top + padding.top + (free_h.max(0) as f64 * ay) as i64;
    let total = Size::new(
        box_size.width + margin.horizontal(),
        box_size.height + margin.vertical(),
    );
    let mut out = Node::leaf(c.constrain(total));
    out.parts.push(Rect::new(
        margin.left,
        margin.top,
        box_size.width,
        box_size.height,
    ));
    out.baseline = if child.baseline >= 0 {
        child.baseline + cy0
    } else {
        -1
    };
    out.children.push(child.at(cx0, cy0));
    cx.theme = saved.clone();
    out
}

pub fn paint_decorated(d: &DecoratedBox, node: &Node, cx: &mut PaintCx) {
    let m = &d.mods;
    if m.visibility != Visibility::Visible {
        return;
    }
    let saved_theme = &cx.theme.clone();
    let saved_disabled = cx.disabled;
    if let Some(ref change) = m.theme {
        cx.theme = change.apply(cx.theme.clone());
    }
    if m.disabled {
        cx.disabled = true
    };
    let frame = if node.parts.is_empty() {
        Rect::from_size(node.size())
    } else {
        node.parts[0]
    };
    let radius = if m.radius >= 0 {
        cx.theme.px(m.radius)
    } else if m.background.is_some() || m.border {
        cx.theme.radius()
    } else {
        0
    };
    if m.elevation > 0 {
        let r = cx.abs(frame);
        let shadow = cx.theme.shadow;
        cx.canvas.shadow(
            r.x,
            r.y + cx.theme.px(2),
            r.width,
            r.height,
            radius,
            cx.theme.px(4 * m.elevation),
            shadow,
        )
    }
    if let Some(tone) = m.background {
        let color = tone.resolve(&cx.theme);
        cx.fill_round(frame, radius, color)
    }
    if m.border {
        let bw = cx.theme.border_width();
        let color = cx.theme.border;
        cx.stroke_round(frame, radius, bw, color)
    }
    paint_children(&[&*d.child], node, cx);
    cx.theme = saved_theme.clone();
    cx.disabled = saved_disabled;
}

pub fn decorated_event(
    d: &DecoratedBox,
    node: &Node,
    ev: &Event,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    let m = &d.mods;
    if m.key.is_empty() {
        return;
    }
    if let Some(ref source) = m.drag {
        drag_source_event(&m.key, source, ev, st, cx)
    }
    match ev {
        Event::PointerMove { .. }
            if !m.tooltip.is_empty() && st.field(&m.key, "tip", "").is_empty() =>
        {
            st.set_field(&m.key, "tip", "waiting");
            cx.start_timer(&m.key, "tooltip", cx.theme.motion.tooltip_delay)
        }
        Event::Timer(tag) if tag == "tooltip" && st.field(&m.key, "tip", "") == "waiting" => {
            st.set_field(&m.key, "tip", "shown");
            let anchor = cx.to_window(if node.parts.is_empty() {
                Rect::from_size(node.size())
            } else {
                node.parts[0]
            });
            let size = tooltip_size(&m.tooltip, &cx.theme);
            cx.open_popup(&PopupRequest::new(
                &m.key,
                0,
                anchor,
                size,
                Placement::Below,
                false,
                true,
            ));
            cx.announce(&m.tooltip)
        }
        Event::PointerLeave | Event::PointerDown { .. } | Event::KeyDown { .. }
            if !st.field(&m.key, "tip", "").is_empty() =>
        {
            st.set_field(&m.key, "tip", "");
            cx.cancel_timer(&m.key, "tooltip");
            cx.close_popup(&m.key, 0)
        }
        Event::Drop { paths, .. } if m.drop_target && !paths.is_empty() => {
            cx.emit(&UiMsg::Dropped(m.key.clone(), paths.to_vec()));
            cx.consume()
        }
        _ => (),
    }
}

// A left press arms the source; a move past a few units, button still held,
// asks the session to start the platform drag, which then owns the pointer.
fn drag_source_event(
    key: &str,
    source: &DragSource,
    ev: &Event,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    match ev {
        Event::PointerDown { x, y, button, .. } if *button == MouseButton::Left => {
            st.set_field(key, "drag-x", &format!("{}", x));
            st.set_field(key, "drag-y", &format!("{}", y));
            cx.capture_pointer(key)
        }
        Event::PointerMove { x, y, pressed, .. } => {
            let Some(ax) = st.field(key, "drag-x", "").to_i64() else {
                return;
            };
            let Some(ay) = st.field(key, "drag-y", "").to_i64() else {
                return;
            };
            let reach = cx.theme.px(4);
            if *pressed && ((x - ax).abs() > reach || (y - ay).abs() > reach) {
                disarm(key, st, cx);
                cx.start_drag = Some((key.to_string(), source.items.clone(), source.operation));
                cx.consume()
            }
        }
        Event::PointerUp { .. } | Event::CaptureLost if !st.field(key, "drag-x", "").is_empty() => {
            disarm(key, st, cx)
        }
        _ => (),
    }
}

fn disarm(key: &str, st: &mut StateStore, cx: &mut EventCx) {
    st.set_field(key, "drag-x", "");
    st.set_field(key, "drag-y", "");
    cx.release_pointer()
}

fn tooltip_size(text: &str, theme: &crate::style::Theme) -> Size {
    let l = layout::layout(
        text,
        &theme.font(TextRole::Small),
        LayoutOptions::wrapped(theme.px(320) as f64),
    );
    let pad = theme.px(8);
    Size::new(
        l.width.ceil() as i64 + pad * 2,
        l.height.ceil() as i64 + pad * 2,
    )
}

/// Paints a tooltip popup of a decorated widget.
pub fn tooltip_paint(d: &DecoratedBox, rect: Rect, cx: &mut PaintCx) {
    let theme = &cx.theme;
    let pad = theme.px(8);
    let fg = if theme.is_dark() {
        theme.background
    } else {
        theme.surface
    };
    let bg = theme.text;
    let (drop, blur) = theme.shadow_of(theme.shape.tooltip_elevation);
    cx.canvas.shadow(
        rect.x,
        rect.y + drop,
        rect.width,
        rect.height,
        theme.px(4),
        blur,
        theme.shadow,
    );
    cx.canvas
        .round_rect(rect.x, rect.y, rect.width, rect.height, theme.px(4), bg);
    let style = theme.font(TextRole::Small);
    let l = layout::layout(
        &d.mods.tooltip,
        &style,
        LayoutOptions::wrapped(theme.px(320) as f64),
    );
    layout::draw(
        &mut cx.canvas,
        &l,
        (rect.x + pad) as f64,
        (rect.y + pad) as f64,
        &style,
        fg,
    )
}

pub fn decorated_semantics(d: &DecoratedBox) -> Option<AccessNode> {
    let m = &d.mods;
    // Without a role the modifiers describe the child; the tree builder
    // merges them into the child node instead.
    let role = m.role?;
    let mut n = AccessNode::new(role, &m.label);
    n.id = m.key.clone();
    n.description = if m.description.is_empty() {
        m.tooltip.clone()
    } else {
        m.description.clone()
    };
    n.disabled = m.disabled;
    Some(n)
}

#[cfg(test)]
mod session_tests {

    use crate::widgets::Widget;
    use crate::widgets::cx::UiMsg;

    fn view(_m: &i64) -> Widget {
        Widget::column(
            &[
                Widget::tool_button("save", &crate::render::icons::Icon::Save, "Save"),
                Widget::label("elsewhere"),
            ],
            8,
        )
        .padding(8)
    }

    fn tips(s: &crate::Session) -> i64 {
        (s.popup_owners()
            .iter()
            .filter(|p| p.0.starts_with("tip:"))
            .cloned()
            .collect::<Vec<_>>()
            .len()) as i64
    }

    #[test]
    fn a_tooltip_waits_hides_on_press_and_returns_only_after_the_pointer_leaves() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Tips",
                "dev.gossamer.gui-tip-test",
                || 0,
                |m: i64, _msg: UiMsg| m,
                view,
            )
            .size(420, 320)
            .start()
            .unwrap();
            let r = s.rect_of("save").unwrap();
            let x = r.center_x();
            let y = r.center_y();
            s.hover(x, y).unwrap();
            s.advance(300).unwrap();
            assert_eq!(tips(&s), 0);
            s.advance(400).unwrap();
            assert_eq!(tips(&s), 1);
            // A press hides it, and it stays hidden while the pointer stays.
            s.click_at(x, y).unwrap();
            assert_eq!(tips(&s), 0);
            s.hover(x + 1, y).unwrap();
            s.advance(1000).unwrap();
            assert_eq!(tips(&s), 0);
            // Leaving and coming back shows it again.
            s.hover(x, y + 200).unwrap();
            s.hover(x, y).unwrap();
            s.advance(700).unwrap();
            assert_eq!(tips(&s), 1);
            s.shutdown()
        })
    }
}

#[cfg(test)]
mod layout_tests {
    use crate::geometry::{Constraints, Size};

    use crate::style::Theme;
    use crate::widgets::Widget;
    use crate::widgets::cx::{LayoutCx, Node};
    use crate::widgets::state::StateStore;
    use crate::widgets::textinput::{InputKind, TextInput};

    fn form_rows(field: &Widget) -> (Node, Node) {
        let mut cx = LayoutCx::new(&Theme::default(), &StateStore::new(), Size::new(600, 400));
        let node = Widget::form(&[("Label".to_string(), field.clone())])
            .layout(Constraints::new(600, 600, 0, 400), &mut cx);
        (node.children[0].clone(), node.children[1].clone())
    }

    #[test]
    fn a_form_label_stays_beside_its_field_text_when_the_field_shows_an_error() {
        let mut port = TextInput::new("port", "80a", "", InputKind::Line);
        port.error = "A port is a whole number".to_string();
        let (label, field) = form_rows(&Widget::input(&port));
        assert!(label.baseline >= 0 && field.baseline >= 0);
        assert_eq!(label.rect.y + label.baseline, field.rect.y + field.baseline)
    }

    #[test]
    fn a_form_label_lines_up_with_a_segmented_choice() {
        let (label, field) = form_rows(&Widget::segmented(
            "density",
            &[
                ("compact".to_string(), "Compact".to_string()),
                ("normal".to_string(), "Normal".to_string()),
            ],
            "normal",
        ));
        assert_eq!(label.rect.y + label.baseline, field.rect.y + field.baseline)
    }

    #[test]
    fn a_form_label_centers_beside_a_control_without_text() {
        let (label, field) = form_rows(&Widget::slider("volume", 0.5, 0.0, 1.0, 0.1));
        let mid = |n: Node| n.rect.y * 2 + n.rect.height;
        assert!((mid(label) - mid(field)).abs() <= 2)
    }
}
