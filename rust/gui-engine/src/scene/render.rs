//! Scene geometry: affine transforms, outlines, bounds, drawing operations
//! and hit testing.

use super::{Camera, Node, Scene, Shape};
use crate::geometry::Rect;
use crate::memory::ByteCache;
use crate::render::display::DrawOp;
use crate::render::vector::Path;
use crate::style::Theme;
use crate::style::color::Color;
use crate::text::font::{CLOSE, CUBIC, LINE, MOVE, QUAD};
use crate::text::layout::{self, TextStyle};

/// Flattened path outlines by object id and revision, for hit testing.
pub(crate) type Outlines = ByteCache<(String, u64), std::sync::Arc<Vec<Vec<(f64, f64)>>>>;

/// x' = a x + c y + e, y' = b x + d y + f.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn of(t: &super::Transform) -> Affine {
        let (sin, cos) = t.rotation.to_radians().sin_cos();
        Affine {
            a: cos * t.scale,
            b: sin * t.scale,
            c: -sin * t.scale,
            d: cos * t.scale,
            e: t.x,
            f: t.y,
        }
    }

    pub fn camera(c: &Camera) -> Affine {
        Affine {
            a: c.zoom,
            b: 0.0,
            c: 0.0,
            d: c.zoom,
            e: -c.x * c.zoom,
            f: -c.y * c.zoom,
        }
    }

    /// This transform applied after `inner`.
    pub fn after(&self, inner: &Affine) -> Affine {
        Affine {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            e: self.a * inner.e + self.c * inner.f + self.e,
            f: self.b * inner.e + self.d * inner.f + self.f,
        }
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    pub fn inverse(&self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Affine {
            a,
            b,
            c,
            d,
            e: -(a * self.e + c * self.f),
            f: -(b * self.e + d * self.f),
        })
    }

    /// How much the transform scales lengths.
    pub fn magnitude(&self) -> f64 {
        (self.a * self.d - self.b * self.c).abs().sqrt()
    }
}

fn rounded_rect(w: f64, h: f64, radius: f64) -> Path {
    let r = radius.clamp(0.0, w.min(h) / 2.0);
    let k = r * 0.552_284_75;
    let mut p = Path::new();
    p.move_to(r, 0.0);
    p.line_to(w - r, 0.0);
    p.cubic_to(w - r + k, 0.0, w, r - k, w, r);
    p.line_to(w, h - r);
    p.cubic_to(w, h - r + k, w - r + k, h, w - r, h);
    p.line_to(r, h);
    p.cubic_to(r - k, h, 0.0, h - r + k, 0.0, h - r);
    p.line_to(0.0, r);
    p.cubic_to(0.0, r - k, r - k, 0.0, r, 0.0);
    p.close();
    p
}

fn ellipse(w: f64, h: f64) -> Path {
    let (rx, ry) = (w / 2.0, h / 2.0);
    let (kx, ky) = (rx * 0.552_284_75, ry * 0.552_284_75);
    let mut p = Path::new();
    p.move_to(w, ry);
    p.cubic_to(w, ry + ky, rx + kx, h, rx, h);
    p.cubic_to(rx - kx, h, 0.0, ry + ky, 0.0, ry);
    p.cubic_to(0.0, ry - ky, rx - kx, 0.0, rx, 0.0);
    p.cubic_to(rx + kx, 0.0, w, ry - ky, w, ry);
    p.close();
    p
}

/// A shape's outline in its own coordinates.
pub fn shape_path(shape: &Shape) -> Option<Path> {
    match shape {
        Shape::Rect {
            width,
            height,
            radius,
        } => Some(rounded_rect(*width, *height, *radius)),
        Shape::Ellipse { width, height } => Some(ellipse(*width, *height)),
        Shape::Path(path) => Some(path.clone()),
        _ => None,
    }
}

fn arity(op: f64) -> usize {
    match op {
        MOVE | LINE => 2,
        QUAD => 4,
        CUBIC => 6,
        _ => 0,
    }
}

pub fn transform_path(path: &Path, m: &Affine) -> Path {
    let mut out = Path::new();
    let mut i = 0;
    while i < path.cmds.len() {
        let op = path.cmds[i];
        let n = arity(op);
        if i + n >= path.cmds.len() && n > 0 {
            break;
        }
        out.cmds.push(op);
        for k in 0..n / 2 {
            let (x, y) = m.apply(path.cmds[i + 1 + 2 * k], path.cmds[i + 2 + 2 * k]);
            out.cmds.push(x);
            out.cmds.push(y);
        }
        i += 1 + n;
    }
    out
}

/// A path as polylines, one per subpath, with curves cut into segments.
pub fn flatten(path: &Path) -> Vec<Vec<(f64, f64)>> {
    const STEPS: usize = 16;
    let mut out: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut at = (0.0, 0.0);
    let mut start = (0.0, 0.0);
    let c = &path.cmds;
    let mut i = 0;
    while i < c.len() {
        let op = c[i];
        let n = arity(op);
        if i + n >= c.len() && n > 0 {
            break;
        }
        let p = |k: usize| (c[i + 1 + 2 * k], c[i + 2 + 2 * k]);
        match op {
            MOVE => {
                at = p(0);
                start = at;
                out.push(vec![at]);
            }
            LINE => {
                if out.is_empty() {
                    out.push(vec![at]);
                }
                at = p(0);
                if let Some(last) = out.last_mut() {
                    last.push(at);
                }
            }
            QUAD | CUBIC => {
                let (c1, c2, end) = if op == QUAD {
                    let (q, e) = (p(0), p(1));
                    (
                        (
                            at.0 + 2.0 / 3.0 * (q.0 - at.0),
                            at.1 + 2.0 / 3.0 * (q.1 - at.1),
                        ),
                        (e.0 + 2.0 / 3.0 * (q.0 - e.0), e.1 + 2.0 / 3.0 * (q.1 - e.1)),
                        e,
                    )
                } else {
                    (p(0), p(1), p(2))
                };
                let from = at;
                if out.is_empty() {
                    out.push(vec![from]);
                }
                for s in 1..=STEPS {
                    let t = s as f64 / STEPS as f64;
                    let u = 1.0 - t;
                    let x = u * u * u * from.0
                        + 3.0 * u * u * t * c1.0
                        + 3.0 * u * t * t * c2.0
                        + t * t * t * end.0;
                    let y = u * u * u * from.1
                        + 3.0 * u * u * t * c1.1
                        + 3.0 * u * t * t * c2.1
                        + t * t * t * end.1;
                    if let Some(last) = out.last_mut() {
                        last.push((x, y));
                    }
                }
                at = end;
            }
            CLOSE => {
                if let Some(last) = out.last_mut() {
                    last.push(start);
                }
                at = start;
            }
            _ => {}
        }
        i += 1 + n;
    }
    out.retain(|p| p.len() >= 2);
    out
}

/// Nonzero winding of a point against closed polylines.
fn inside(polylines: &[Vec<(f64, f64)>], x: f64, y: f64) -> bool {
    let mut winding = 0;
    for line in polylines {
        let closed = line.iter().copied().chain(std::iter::once(line[0]));
        let mut previous = line[0];
        for p in closed.skip(1) {
            if previous.1 <= y {
                if p.1 > y && cross(previous, p, (x, y)) > 0.0 {
                    winding += 1;
                }
            } else if p.1 <= y && cross(previous, p, (x, y)) < 0.0 {
                winding -= 1;
            }
            previous = p;
        }
    }
    winding != 0
}

fn cross(a: (f64, f64), b: (f64, f64), p: (f64, f64)) -> f64 {
    (b.0 - a.0) * (p.1 - a.1) - (p.0 - a.0) * (b.1 - a.1)
}

fn text_style(size: f64) -> TextStyle {
    TextStyle::sans(size.clamp(1.0, 4096.0))
}

/// A shape's own bounding box: (x0, y0, x1, y1) in its coordinates.
pub fn local_bounds(shape: &Shape) -> (f64, f64, f64, f64) {
    match shape {
        Shape::Rect { width, height, .. }
        | Shape::Ellipse { width, height }
        | Shape::Image { width, height, .. }
        | Shape::Group { width, height, .. } => (0.0, 0.0, *width, *height),
        // A curve lies within its control points, so their box contains it.
        Shape::Path(path) => {
            let mut points = Vec::new();
            let mut i = 0;
            while i < path.cmds.len() {
                let n = arity(path.cmds[i]);
                if n > 0 && i + n >= path.cmds.len() {
                    break;
                }
                points.extend(
                    (0..n / 2).map(|k| (path.cmds[i + 1 + 2 * k], path.cmds[i + 2 + 2 * k])),
                );
                i += 1 + n;
            }
            if points.is_empty() {
                return (0.0, 0.0, 0.0, 0.0);
            }
            points.iter().fold(
                (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
                |(a, b, c, d), p| (a.min(p.0), b.min(p.1), c.max(p.0), d.max(p.1)),
            )
        }
        Shape::Text { text, size } => {
            let (w, _) = layout::measure(text, &text_style(*size));
            let (_, _, line) = layout::metrics(&text_style(*size));
            (0.0, 0.0, w, line)
        }
    }
}

/// A node's bounding box under `m`, in view pixels.
pub fn bounds(node: &Node, m: &Affine) -> Rect {
    let (x0, y0, x1, y1) = local_bounds(&node.shape);
    let corners = [
        m.apply(x0, y0),
        m.apply(x1, y0),
        m.apply(x0, y1),
        m.apply(x1, y1),
    ];
    let (a, b, c, d) = corners.iter().fold(
        (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
        |(a, b, c, d), p| (a.min(p.0), b.min(p.1), c.max(p.0), d.max(p.1)),
    );
    Rect::new(
        a.floor() as i64,
        b.floor() as i64,
        (c - a).ceil().max(1.0) as i64,
        (d - b).ceil().max(1.0) as i64,
    )
}

fn faded(color: Color, opacity: f64) -> Color {
    color.with_alpha((f64::from(color.a) * opacity.clamp(0.0, 1.0)).round() as i64)
}

fn draw_node(node: &Node, outer: &Affine, ops: &mut Vec<DrawOp>) {
    if !node.visible || node.opacity <= 0.0 {
        return;
    }
    let m = outer.after(&Affine::of(&node.transform));
    let opacity = node.opacity.clamp(0.0, 1.0);
    if let Some(path) = shape_path(&node.shape) {
        let path = transform_path(&path, &m);
        if let Some(fill) = node.fill {
            ops.push(DrawOp::Fill {
                path: path.clone(),
                color: faded(fill, opacity),
            });
        }
        if let Some((color, width)) = node.stroke {
            ops.push(DrawOp::StrokePath {
                path,
                width: (width * m.magnitude()).max(0.5),
                color: faded(color, opacity),
            });
        }
        return;
    }
    match &node.shape {
        Shape::Text { text, size } => {
            let (x, y) = m.apply(0.0, 0.0);
            ops.push(DrawOp::Text {
                x: x.round() as i64,
                y: y.round() as i64,
                text: text.clone(),
                style: text_style(size * m.magnitude()),
                color: faded(node.fill.unwrap_or_else(Color::black), opacity),
            });
        }
        Shape::Image {
            rgba,
            pixels_wide,
            pixels_high,
            ..
        } => {
            if opacity < 1.0 {
                ops.push(DrawOp::Opacity((opacity * 255.0).round() as i64));
            }
            ops.push(DrawOp::Image {
                rect: bounds(node, outer),
                rgba: rgba.clone(),
                width: *pixels_wide,
                height: *pixels_high,
            });
            if opacity < 1.0 {
                ops.push(DrawOp::EndOpacity);
            }
        }
        Shape::Group { clip, children, .. } => {
            if *clip {
                ops.push(DrawOp::Clip(bounds(node, outer)));
            }
            if opacity < 1.0 {
                ops.push(DrawOp::Opacity((opacity * 255.0).round() as i64));
            }
            for child in children {
                draw_node(child, &m, ops);
            }
            if opacity < 1.0 {
                ops.push(DrawOp::EndOpacity);
            }
            if *clip {
                ops.push(DrawOp::Unclip);
            }
        }
        _ => {}
    }
}

/// The transform from a node's parent to the view, by id.
pub fn parent_transform(nodes: &[Node], id: &str, outer: &Affine) -> Option<Affine> {
    for n in nodes {
        if n.id == id {
            return Some(*outer);
        }
        if let Shape::Group { children, .. } = &n.shape
            && let Some(found) =
                parent_transform(children, id, &outer.after(&Affine::of(&n.transform)))
        {
            return Some(found);
        }
    }
    None
}

/// The resize handle of the selection's single sized object, in view pixels.
pub fn resize_handle(scene: &Scene, theme: &Theme) -> Option<Rect> {
    let [only] = scene.selection.as_slice() else {
        return None;
    };
    let node = super::find(&scene.nodes, only)?;
    if matches!(node.shape, Shape::Path(_) | Shape::Text { .. }) {
        return None;
    }
    let m = parent_transform(&scene.nodes, only, &Affine::camera(&scene.camera))?;
    let b = bounds(node, &m.after(&Affine::of(&node.transform)));
    let s = theme.px(8);
    Some(Rect::new(
        b.x + b.width - s / 2,
        b.y + b.height - s / 2,
        s,
        s,
    ))
}

/// The scene drawn in view pixels; `selection` draws selection outlines and
/// handles, which exports leave out.
pub fn render(scene: &Scene, theme: &Theme, selection: bool) -> Vec<DrawOp> {
    let camera = Affine::camera(&scene.camera);
    let mut ops = Vec::new();
    for node in &scene.nodes {
        draw_node(node, &camera, &mut ops);
    }
    if selection {
        for id in &scene.selection {
            let (Some(node), Some(m)) = (
                super::find(&scene.nodes, id),
                parent_transform(&scene.nodes, id, &camera),
            ) else {
                continue;
            };
            let b = bounds(node, &m.after(&Affine::of(&node.transform)));
            ops.push(DrawOp::Stroke {
                rect: Rect::new(b.x - 2, b.y - 2, b.width + 4, b.height + 4),
                radius: 0,
                width: theme.px(2),
                color: theme.accent,
            });
        }
        if let Some(handle) = resize_handle(scene, theme) {
            ops.push(DrawOp::FillRect {
                rect: handle,
                color: theme.accent,
            });
        }
    }
    ops
}

fn hit_node(
    node: &Node,
    outer: &Affine,
    x: f64,
    y: f64,
    revision: u64,
    outlines: &mut Outlines,
    deep: bool,
) -> Option<String> {
    if !node.visible || node.locked {
        return None;
    }
    let m = outer.after(&Affine::of(&node.transform));
    let (lx, ly) = m.inverse()?.apply(x, y);
    let (x0, y0, x1, y1) = local_bounds(&node.shape);
    let within = lx >= x0 && lx <= x1 && ly >= y0 && ly <= y1;
    match &node.shape {
        Shape::Group { clip, children, .. } => {
            if *clip && !within {
                return None;
            }
            for child in children.iter().rev() {
                if let Some(hit) = hit_node(child, &m, x, y, revision, outlines, deep) {
                    return Some(if deep { hit } else { node.id.clone() });
                }
            }
            None
        }
        Shape::Ellipse { width, height } => {
            let (rx, ry) = (width / 2.0, height / 2.0);
            let (dx, dy) = ((lx - rx) / rx.max(1e-9), (ly - ry) / ry.max(1e-9));
            (dx * dx + dy * dy <= 1.0).then(|| node.id.clone())
        }
        // Only a point within the control-point bounds needs the outline.
        Shape::Path(path) => {
            if !within {
                return None;
            }
            let key = (node.id.clone(), revision);
            let outline = match outlines.get(&key) {
                Some(found) => found.clone(),
                None => {
                    let flat = std::sync::Arc::new(flatten(path));
                    let size =
                        flat.iter().map(|p| p.len() * 16 + 32).sum::<usize>() + node.id.len();
                    outlines.insert(key, flat.clone(), size);
                    flat
                }
            };
            inside(&outline, lx, ly).then(|| node.id.clone())
        }
        _ => within.then(|| node.id.clone()),
    }
}

/// The topmost visible, unlocked object under a view pixel: the top-level
/// object, or with `deep` the innermost.
pub(crate) fn hit(
    scene: &Scene,
    outlines: &mut Outlines,
    x: f64,
    y: f64,
    deep: bool,
) -> Option<String> {
    let camera = Affine::camera(&scene.camera);
    scene
        .nodes
        .iter()
        .rev()
        .find_map(|n| hit_node(n, &camera, x, y, scene.revision, outlines, deep))
}
