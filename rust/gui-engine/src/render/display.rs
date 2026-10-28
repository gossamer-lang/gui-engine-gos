// display: a retained list of draw operations in local pixels. Custom
// widgets, charts, and scene canvases describe their pictures with it, so
// drawing stays testable as data and the renderer stays the only code
// that touches pixels.

use crate::geometry::Rect;
use crate::render::Canvas;
use crate::render::vector::Path;
use crate::style::color::Color;
use crate::text::font::{CUBIC, LINE, MOVE, QUAD};
use crate::text::layout;
use crate::text::layout::{LayoutOptions, TextStyle};

/// One drawing operation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DrawOp {
    FillRect {
        rect: Rect,
        color: Color,
    },
    RoundRect {
        rect: Rect,
        radius: i64,
        color: Color,
    },
    Stroke {
        rect: Rect,
        radius: i64,
        width: i64,
        color: Color,
    },
    Line {
        x0: i64,
        y0: i64,
        x1: i64,
        y1: i64,
        width: f64,
        color: Color,
    },
    Circle {
        cx: i64,
        cy: i64,
        r: i64,
        color: Color,
    },
    Fill {
        path: Path,
        color: Color,
    },
    StrokePath {
        path: Path,
        width: f64,
        color: Color,
    },
    /// Text with its line box's top-left at (x, y).
    Text {
        x: i64,
        y: i64,
        text: String,
        style: TextStyle,
        color: Color,
    },
    Image {
        rect: Rect,
        rgba: Vec<u8>,
        width: i64,
        height: i64,
    },
    /// Restricts the following operations to `rect` until `Unclip`.
    Clip(Rect),
    Unclip,
    /// Multiplies the following operations' opacity (0..=255) until `EndOpacity`.
    Opacity(i64),
    EndOpacity,
    /// An isolated subtree, transformed about its local origin.
    Layer {
        ops: Vec<DrawOp>,
        transform: super::retained::Transform,
    },
}

/// Executes `ops` with their local origin at (`ox`, `oy`) on `canvas`.
/// Draws `ops` as an isolated subtree under `transform`, as a
/// [`DrawOp::Layer`] holding them would.
pub fn draw_transformed(
    canvas: &mut Canvas,
    ops: &[DrawOp],
    transform: super::retained::Transform,
) {
    let mut recording = Canvas::recording(
        canvas.width,
        canvas.height,
        super::retained::Budgets::default().scene,
    );
    recording.subtrees = canvas.subtrees.clone();
    draw(&mut recording, ops, 0, 0);
    if let Some(error) = recording.paint_error.take() {
        canvas.paint_error = Some(error);
        return;
    }
    let Some(scene) = recording.take_scene() else {
        canvas.paint_error = Some("subtree exceeds the display-list budget".into());
        return;
    };
    let mut layer = super::retained::Scene::default();
    let result = if let Some(cache) = &canvas.subtrees {
        layer.append_cached(&scene, transform, canvas.clip, canvas.opacity, cache)
    } else {
        layer.append(&scene, transform, canvas.clip, canvas.opacity)
    };
    match result {
        Ok(()) => layer.replay(canvas),
        Err(error) => canvas.paint_error = Some(error.to_string()),
    }
}

pub fn draw(canvas: &mut Canvas, ops: &[DrawOp], ox: i64, oy: i64) {
    let mut clips: Vec<Rect> = vec![];
    let mut opacities: Vec<i64> = vec![];
    for op in ops {
        match op {
            DrawOp::Layer { ops, transform } => {
                let mut transform = *transform;
                transform.x += ox as f64;
                transform.y += oy as f64;
                draw_transformed(canvas, ops, transform);
            }
            DrawOp::FillRect { rect, color } => {
                canvas.rect(rect.x + ox, rect.y + oy, rect.width, rect.height, *color)
            }
            DrawOp::RoundRect {
                rect,
                radius,
                color,
            } => canvas.round_rect(
                rect.x + ox,
                rect.y + oy,
                rect.width,
                rect.height,
                *radius,
                *color,
            ),
            DrawOp::Stroke {
                rect,
                radius,
                width,
                color,
            } => canvas.round_rect_stroke(
                rect.x + ox,
                rect.y + oy,
                rect.width,
                rect.height,
                *radius,
                *width,
                *color,
            ),
            DrawOp::Line {
                x0,
                y0,
                x1,
                y1,
                width,
                color,
            } => canvas.line_aa_w(x0 + ox, y0 + oy, x1 + ox, y1 + oy, *width, *color),
            DrawOp::Circle { cx, cy, r, color } => canvas.circle(cx + ox, cy + oy, *r, *color),
            DrawOp::Fill { path, color } => {
                canvas.fill_path(&translated(path.clone(), ox, oy), *color)
            }
            DrawOp::StrokePath { path, width, color } => {
                canvas.stroke_path(&translated(path.clone(), ox, oy), *width, *color)
            }
            DrawOp::Text {
                x,
                y,
                text,
                style,
                color,
            } => {
                let l = layout::layout(text, style, LayoutOptions::single_line());
                layout::draw(canvas, &l, (x + ox) as f64, (y + oy) as f64, style, *color)
            }
            DrawOp::Image {
                rect,
                rgba,
                width,
                height,
            } => canvas.image_scaled(
                rect.x + ox,
                rect.y + oy,
                rect.width,
                rect.height,
                rgba,
                *width,
                *height,
            ),
            DrawOp::Clip(r) => {
                clips.push(canvas.clip);
                canvas.push_clip(r.offset(ox, oy))
            }
            DrawOp::Unclip => {
                if let Some(prev) = clips.pop() {
                    canvas.pop_clip(prev)
                }
            }
            DrawOp::Opacity(a) => {
                opacities.push(canvas.opacity);
                canvas.push_opacity(*a)
            }
            DrawOp::EndOpacity => {
                if let Some(prev) = opacities.pop() {
                    canvas.pop_opacity(prev)
                }
            }
        }
    }
    // Unbalanced clips and opacities end with the list.
    while let Some(prev) = clips.pop() {
        canvas.pop_clip(prev)
    }
    while let Some(prev) = opacities.pop() {
        canvas.pop_opacity(prev)
    }
}

/// `path` moved by (`dx`, `dy`).
pub fn translated(path: Path, dx: i64, dy: i64) -> Path {
    if dx == 0 && dy == 0 {
        return path;
    }
    let fx = dx as f64;
    let fy = dy as f64;
    let mut out = Path::new();
    let c = path.cmds;
    let mut i = 0;
    while i < c.len() {
        let op = c[i];
        out.cmds.push(op);
        let n = match op {
            MOVE | LINE => 2,
            QUAD => 4,
            CUBIC => 6,
            _ => 0,
        };
        let mut k = 0;
        while k < n {
            out.cmds
                .push(c[i + 1 + k] + if k % 2 == 0 { fx } else { fy });
            k += 1;
        }
        i += 1 + n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{DrawOp, draw};
    use crate::geometry::Rect;
    use crate::render::Canvas;
    use crate::style::color::Color;

    #[test]
    fn clips_restrict_and_unwind() {
        let mut c = Canvas::new(10, 10);
        let ops = vec![
            DrawOp::Clip(Rect::new(0, 0, 2, 2)),
            DrawOp::FillRect {
                rect: Rect::new(0, 0, 10, 10),
                color: Color::white(),
            },
        ];
        draw(&mut c, &ops, 0, 0);
        assert_eq!(c.pixels[0], 255);
        assert_eq!(c.pixels[(5 * 10 + 5) * 4], 0);
        assert_eq!(c.clip, Rect::new(0, 0, 10, 10))
    }
}
