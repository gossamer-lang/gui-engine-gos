pub mod display;
pub mod icons;
pub mod png;
pub mod presenter;
pub mod retained;
pub mod svg;
pub mod vector;

// render: immediate drawing onto an RGBA canvas and blitting a canvas into
// a window framebuffer. Pure software: clipped and faded fills, anti-aliased
// shapes and paths, gradients, shadows, and scaled RGBA images. Colors are
// straight alpha; every primitive multiplies the color alpha by the paint
// opacity. The surface boundary (`blit`) maps RGBA onto the frame byte
// order and stride the window reports.

use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use windowing;

use crate::geometry::Rect;
use crate::style::color::Color;

static NEXT_PIXELS: AtomicU64 = AtomicU64::new(1);

/// RGBA bytes shared, unchanged, by their owner and the scenes that draw
/// them. Each distinct content gets its own identity, which scenes compare
/// and hash in place of the bytes.
#[derive(Debug, Clone)]
pub struct Pixels {
    id: u64,
    rgba: Arc<[u8]>,
}

impl Pixels {
    pub fn new(rgba: impl Into<Arc<[u8]>>) -> Self {
        Self {
            id: NEXT_PIXELS.fetch_add(1, Ordering::Relaxed),
            rgba: rgba.into(),
        }
    }

    /// Replaces the content with `rgba`, reusing this buffer when nothing
    /// else holds it and it has the same length.
    pub fn replace(&mut self, rgba: &[u8]) {
        match Arc::get_mut(&mut self.rgba) {
            Some(own) if own.len() == rgba.len() => {
                own.copy_from_slice(rgba);
                self.id = NEXT_PIXELS.fetch_add(1, Ordering::Relaxed);
            }
            _ => *self = Self::new(rgba),
        }
    }
}

impl Default for Pixels {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl PartialEq for Pixels {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Deref for Pixels {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.rgba
    }
}

impl std::hash::Hash for Pixels {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.id.hash(h)
    }
}

/// A software drawing surface in RGBA order, 4 bytes per pixel, rows
/// packed (stride = width * 4). Widgets draw in their own coordinates;
/// the session blits the finished canvas into the window.
///
/// Drawing honours two paint state fields: `clip` (only the pixels
/// inside it change) and `opacity` (0 to 255; 255 is opaque). Every draw
/// call goes through both, so nesting widgets can clip children and
/// fade layers without each widget knowing.
#[derive(Debug, Clone, PartialEq)]
pub struct Canvas {
    pub pixels: Vec<u8>,
    pub width: i64,
    pub height: i64,
    pub clip: Rect,
    /// 0..=255; 255 paints solid, less blends with what is under it.
    pub opacity: i64,
    pub(crate) subtrees: Option<retained::SubtreeCache>,
    pub(crate) paint_error: Option<String>,
    pub(crate) recording: Option<retained::Scene>,
    pub(crate) recorded_bytes: usize,
    pub(crate) recording_limit: usize,
    pub(crate) recording_overflow: bool,
}

impl Canvas {
    /// An opaque canvas of `width` x `height` pixels, clipped to
    /// itself.
    pub fn new(width: i64, height: i64) -> Canvas {
        Self::try_new(width, height).unwrap_or_else(|error| Canvas {
            pixels: Vec::new(),
            width: 0,
            height: 0,
            clip: Rect::new(0, 0, 0, 0),
            opacity: 255,
            subtrees: None,
            paint_error: Some(error),
            recording: None,
            recorded_bytes: 0,
            recording_limit: 0,
            recording_overflow: false,
        })
    }

    pub fn try_new(width: i64, height: i64) -> Result<Canvas, String> {
        let bytes = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .filter(|&bytes| bytes <= 64 * 1024 * 1024)
            .ok_or_else(|| "canvas exceeds its pixel byte budget".to_owned())?;
        Ok(Canvas {
            pixels: vec![0u8; bytes],
            width,
            height,
            clip: Rect::new(0, 0, width, height),
            opacity: 255,
            subtrees: None,
            paint_error: None,
            recording: None,
            recorded_bytes: 0,
            recording_limit: 0,
            recording_overflow: false,
        })
    }

    /// A blank `width` x `height` canvas drawn into `pixels`, reusing their
    /// allocation.
    pub(crate) fn over(width: i64, height: i64, mut pixels: Vec<u8>) -> Canvas {
        let bytes = (width.max(0) * height.max(0) * 4) as usize;
        pixels.clear();
        pixels.resize(bytes, 0);
        Canvas {
            pixels,
            width,
            height,
            clip: Rect::new(0, 0, width, height),
            opacity: 255,
            subtrees: None,
            paint_error: None,
            recording: None,
            recorded_bytes: 0,
            recording_limit: 0,
            recording_overflow: false,
        }
    }

    /// Records primitives without allocating a framebuffer or rasterizing glyphs.
    pub fn recording(width: i64, height: i64, budget: usize) -> Self {
        let mut canvas = Self::new(0, 0);
        canvas.width = width;
        canvas.height = height;
        canvas.clip = Rect::new(0, 0, width, height);
        canvas.recording = Some(retained::Scene::default());
        canvas.recording_limit = budget;
        canvas
    }

    pub fn take_scene(&mut self) -> Option<retained::Scene> {
        let scene = self.recording.take();
        if self.recording_overflow { None } else { scene }
    }

    fn record(&mut self, payload: usize, make: impl FnOnce() -> retained::Primitive) -> bool {
        let Some(scene) = &mut self.recording else {
            return false;
        };
        let unit = std::mem::size_of::<retained::Command>();
        let bytes = unit.saturating_add(payload);
        let payload_bytes = self
            .recorded_bytes
            .saturating_sub(scene.commands.len() * unit)
            .saturating_add(payload);
        let max_capacity = self.recording_limit.saturating_sub(payload_bytes) / unit;
        if self.recording_overflow
            || payload_bytes > self.recording_limit
            || scene.commands.len() + 1 > max_capacity
        {
            self.recording_overflow = true;
            return true;
        }
        if scene.commands.capacity() < scene.commands.len() + 1 {
            let capacity = (scene.commands.capacity().max(2) * 2)
                .min(max_capacity)
                .max(scene.commands.len() + 1);
            scene
                .commands
                .reserve_exact(capacity - scene.commands.len());
        }
        let command = retained::Command::new(make(), self.clip, self.opacity);
        if command.bounds.width > 0 && command.bounds.height > 0 {
            self.recorded_bytes += bytes;
            scene.commands.push(command);
        }
        true
    }

    /// Fill every pixel with `color`, ignoring clip and opacity.
    pub fn fill(&mut self, color: Color) {
        if self.recording.is_some() {
            let clip = self.clip;
            let opacity = self.opacity;
            self.clip = Rect::new(0, 0, self.width, self.height);
            self.opacity = 255;
            self.rect(0, 0, self.width, self.height, color);
            self.clip = clip;
            self.opacity = opacity;
            return;
        }
        clear(&mut self.pixels, color)
    }

    /// Shrink the clip to the intersection with `rect`. Pair every
    /// push with `pop_clip`.
    pub fn push_clip(&mut self, rect: Rect) {
        let c = self.clip;
        let x0 = maxi(c.x, rect.x);
        let y0 = maxi(c.y, rect.y);
        let x1 = mini(c.x + c.width, rect.x + rect.width);
        let y1 = mini(c.y + c.height, rect.y + rect.height);
        self.clip = Rect::new(x0, y0, maxi(x1 - x0, 0), maxi(y1 - y0, 0));
    }

    pub fn pop_clip(&mut self, restore: Rect) {
        self.clip = restore;
    }

    pub fn push_opacity(&mut self, alpha: i64) {
        self.opacity = clamp255(self.opacity * alpha / 255);
    }

    pub fn pop_opacity(&mut self, restore: i64) {
        self.opacity = restore;
    }

    /// Fill the rectangle at `(x, y)` size `(w, h)` in canvas pixels,
    /// clipped and faded by the paint state.
    pub fn rect(&mut self, x: i64, y: i64, w: i64, h: i64, color: Color) {
        if self.record(0, || retained::Primitive::Rect(x, y, w, h, color)) {
            return;
        }
        let c = self.clip;
        let op = self.op(color);
        blend_rect(&mut self.pixels, self.width * 4, c, op, x, y, w, h, color)
    }

    /// A rectangle outline one pixel thick, inside the given bounds.
    pub fn stroke_rect(&mut self, x: i64, y: i64, w: i64, h: i64, color: Color) {
        self.rect(x, y, w, 1, color);
        self.rect(x, y + h - 1, w, 1, color);
        self.rect(x, y + 1, 1, h - 2, color);
        self.rect(x + w - 1, y + 1, 1, h - 2, color)
    }

    /// A line from `(x0, y0)` to `(x1, y1)`, axis-aligned or diagonal
    /// (Bresenham). One pixel thick.
    pub fn line(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, color: Color) {
        let dx = (x1 - x0).abs();
        let dy = (y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx - dy;
        let mut x = x0;
        let mut y = y0;
        loop {
            self.rect(x, y, 1, 1, color);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = err * 2;
            if e2 > -dy {
                err -= dy;
                x += sx;
            }
            if e2 < dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Fill a circle centred at `(cx, cy)` with radius `r`,
    /// anti-aliased: edge pixels blend at partial coverage.
    pub fn circle(&mut self, cx: i64, cy: i64, r: i64, color: Color) {
        if self.record(0, || retained::Primitive::Circle(cx, cy, r, color)) {
            return;
        }
        let c = self.clip;
        let op = self.op(color);
        circle_kernel(&mut self.pixels, self.width * 4, c, op, cx, cy, r, color)
    }

    /// A circle outline `t` pixels thick, anti-aliased, centred at
    /// `(cx, cy)` with radius `r`.
    pub fn circle_stroke(&mut self, cx: i64, cy: i64, r: i64, t: i64, color: Color) {
        if self.record(0, || retained::Primitive::CircleStroke(cx, cy, r, t, color)) {
            return;
        }
        let c = self.clip;
        let op = self.op(color);
        circle_stroke_kernel(&mut self.pixels, self.width * 4, c, op, cx, cy, r, t, color)
    }

    /// A rectangle with rounded corners of radius `r`, anti-aliased,
    /// filled with `color`.
    pub fn round_rect(&mut self, x: i64, y: i64, w: i64, h: i64, r: i64, color: Color) {
        self.round_rect_coverage(x, y, w, h, r, 0, color)
    }

    /// A rounded-rectangle outline `t` pixels thick, anti-aliased,
    /// inside the bounds `(x, y, w, h)`.
    pub fn round_rect_stroke(
        &mut self,
        x: i64,
        y: i64,
        w: i64,
        h: i64,
        r: i64,
        t: i64,
        color: Color,
    ) {
        self.round_rect_coverage(x, y, w, h, r, t, color)
    }

    /// Shared scan for `round_rect` / `round_rect_stroke`: walks the
    /// shape's bounds and paints each pixel from its signed distance.
    /// `t` is 0 for a fill, the stroke thickness otherwise.
    fn round_rect_coverage(
        &mut self,
        x: i64,
        y: i64,
        w: i64,
        h: i64,
        r: i64,
        t: i64,
        color: Color,
    ) {
        if self.record(0, || {
            retained::Primitive::RoundRect(x, y, w, h, r, t, color)
        }) {
            return;
        }
        let c = self.clip;
        let op = self.op(color);
        round_rect_coverage_kernel(
            &mut self.pixels,
            self.width * 4,
            c,
            op,
            x,
            y,
            w,
            h,
            r,
            t,
            color,
        )
    }

    /// A straight line from `(x0, y0)` to `(x1, y1)`, one pixel wide,
    /// anti-aliased: endpoints anywhere, edges blended.
    pub fn line_aa(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, color: Color) {
        self.line_aa_w(x0, y0, x1, y1, 1.0, color)
    }

    /// A straight line `width` pixels wide, anti-aliased.
    pub fn line_aa_w(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, width: f64, color: Color) {
        if self.record(0, || {
            retained::Primitive::Line(x0, y0, x1, y1, width, color)
        }) {
            return;
        }
        let c = self.clip;
        let op = self.op(color);
        line_aa_w_kernel(
            &mut self.pixels,
            self.width * 4,
            c,
            op,
            x0,
            y0,
            x1,
            y1,
            width,
            color,
        )
    }

    /// Blend a coverage buffer (one 0..=255 alpha per pixel, rows
    /// packed, `w` wide) with its top-left at `(x, y)`, through the
    /// paint state. The text rasteriser's way onto the canvas.
    pub fn coverage(&mut self, x: i64, y: i64, w: i64, h: i64, cov: &[u8], color: Color) {
        if self.record(cov.len(), || {
            retained::Primitive::Coverage(x, y, w, h, cov.into(), color)
        }) {
            return;
        }
        let c = self.clip;
        let op = self.op(color);
        blend_coverage(
            &mut self.pixels,
            self.width * 4,
            c,
            op,
            x,
            y,
            w,
            h,
            cov,
            color,
        )
    }

    /// A vertical gradient from `top` to `bottom` over the rectangle.
    pub fn v_gradient(&mut self, x: i64, y: i64, w: i64, h: i64, top: Color, bottom: Color) {
        let mut row = 0;
        while row < h {
            let t = if h <= 1 {
                0.0
            } else {
                row as f64 / (h - 1) as f64
            };
            let c = Color::rgb(
                lerp(top.r, bottom.r, t),
                lerp(top.g, bottom.g, t),
                lerp(top.b, bottom.b, t),
            );
            self.rect(x, y + row, w, 1, c);
            row += 1;
        }
    }

    /// Draw an RGBA image (4 bytes per pixel, rows packed) with its
    /// top-left at `(x, y)`. Where the image's alpha is 255 the paint
    /// state's opacity still applies; partial image alpha blends.
    pub fn image(&mut self, x: i64, y: i64, width: i64, height: i64, rgba: &[u8]) {
        if self.record(rgba.len(), || {
            retained::Primitive::Image(x, y, width, height, rgba.into(), width, height)
        }) {
            return;
        }
        let clip = self.clip;
        image_rgba(
            &mut self.pixels,
            self.width * 4,
            clip,
            self.opacity,
            x,
            y,
            width,
            height,
            rgba,
        )
    }

    /// Draws an RGBA image scaled into the rectangle (`x`, `y`, `w`, `h`)
    /// with bilinear filtering.
    pub fn image_scaled(
        &mut self,
        x: i64,
        y: i64,
        w: i64,
        h: i64,
        rgba: &[u8],
        src_w: i64,
        src_h: i64,
    ) {
        if self.record(rgba.len(), || {
            retained::Primitive::Image(x, y, w, h, rgba.into(), src_w, src_h)
        }) {
            return;
        }
        self.paint_scaled(x, y, w, h, rgba, src_w, src_h)
    }

    fn paint_scaled(
        &mut self,
        x: i64,
        y: i64,
        w: i64,
        h: i64,
        rgba: &[u8],
        src_w: i64,
        src_h: i64,
    ) {
        if w <= 0 || h <= 0 || src_w <= 0 || src_h <= 0 {
            return;
        }
        if w == src_w && h == src_h {
            self.image(x, y, w, h, rgba);
            return;
        }
        let clip = self.clip;
        image_scaled_kernel(
            &mut self.pixels,
            self.width * 4,
            clip,
            self.opacity,
            x,
            y,
            w,
            h,
            rgba,
            src_w,
            src_h,
        )
    }

    /// Draws shared pixels scaled into the rectangle (`x`, `y`, `w`, `h`)
    /// with bilinear filtering. A recording holds them by reference.
    pub fn pixels_scaled(
        &mut self,
        x: i64,
        y: i64,
        w: i64,
        h: i64,
        image: &Pixels,
        src_w: i64,
        src_h: i64,
    ) {
        if self.record(0, || {
            retained::Primitive::Shared(x, y, w, h, image.clone(), src_w, src_h)
        }) {
            return;
        }
        self.paint_scaled(x, y, w, h, image, src_w, src_h)
    }

    /// Samples an isolated straight-alpha layer in premultiplied color,
    /// avoiding dark fringes around transparent pixels during transforms.
    pub fn layer_scaled(&mut self, x: i64, y: i64, w: i64, h: i64, rgba: &[u8], sw: i64, sh: i64) {
        if w <= 0 || h <= 0 || sw <= 0 || sh <= 0 {
            return;
        }
        if self.record(rgba.len(), || {
            retained::Primitive::Layer(x, y, w, h, rgba.into(), sw, sh)
        }) {
            return;
        }
        let bounds = Rect::new(x, y, w, h)
            .intersect(self.clip)
            .intersect(Rect::new(0, 0, self.width, self.height));
        for row in bounds.y..bounds.y + bounds.height {
            for col in bounds.x..bounds.x + bounds.width {
                let fx = (((col - x) as f64 + 0.5) * sw as f64 / w as f64 - 0.5)
                    .clamp(0.0, (sw - 1) as f64);
                let fy = (((row - y) as f64 + 0.5) * sh as f64 / h as f64 - 0.5)
                    .clamp(0.0, (sh - 1) as f64);
                let x0 = fx.floor() as i64;
                let y0 = fy.floor() as i64;
                let tx = fx - x0 as f64;
                let ty = fy - y0 as f64;
                let samples = [
                    (x0, y0, (1.0 - tx) * (1.0 - ty)),
                    ((x0 + 1).min(sw - 1), y0, tx * (1.0 - ty)),
                    (x0, (y0 + 1).min(sh - 1), (1.0 - tx) * ty),
                    ((x0 + 1).min(sw - 1), (y0 + 1).min(sh - 1), tx * ty),
                ];
                let mut sum = [0.0; 4];
                for (sx, sy, weight) in samples {
                    let p = ((sy * sw + sx) * 4) as usize;
                    let alpha = rgba[p + 3] as f64 / 255.0;
                    for k in 0..3 {
                        sum[k] += rgba[p + k] as f64 * alpha * weight;
                    }
                    sum[3] += alpha * weight;
                }
                if sum[3] > 0.0 {
                    self.rect(
                        col,
                        row,
                        1,
                        1,
                        Color {
                            r: (sum[0] / sum[3]).round() as u8,
                            g: (sum[1] / sum[3]).round() as u8,
                            b: (sum[2] / sum[3]).round() as u8,
                            a: (sum[3] * 255.0).round() as u8,
                        },
                    );
                }
            }
        }
    }

    // The paint opacity combined with a color's own alpha.
    fn op(&self, color: Color) -> i64 {
        self.opacity * color.a as i64 / 255
    }

    /// Fills a vector path (pixel coordinates) with anti-aliasing.
    pub fn fill_path(&mut self, path: &vector::Path, color: Color) {
        let m = vector::Mask::fill_within(
            &path.cmds,
            (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            vector::FillRule::NonZero,
            Some(self.clip),
        );
        self.mask(&m, color)
    }

    /// Strokes a vector path `width` pixels wide.
    pub fn stroke_path(&mut self, path: &vector::Path, width: f64, color: Color) {
        self.fill_path(&path.stroke(width), color)
    }

    /// Blends a coverage mask in `color`.
    pub fn mask(&mut self, m: &vector::Mask, color: Color) {
        if m.width > 0 && m.height > 0 {
            self.coverage(m.left, m.top, m.width, m.height, &m.coverage, color)
        }
    }

    /// A horizontal gradient from `left` to `right` over the rectangle.
    pub fn h_gradient(&mut self, x: i64, y: i64, w: i64, h: i64, left: Color, right: Color) {
        let mut col = 0;
        while col < w {
            let t = if w <= 1 {
                0.0
            } else {
                col as f64 / (w - 1) as f64
            };
            self.rect(x + col, y, 1, h, left.mix(right, t));
            col += 1;
        }
    }

    /// A soft drop shadow under the rounded rectangle, `blur` pixels wide.
    pub fn shadow(&mut self, x: i64, y: i64, w: i64, h: i64, radius: i64, blur: i64, color: Color) {
        if blur <= 0 {
            self.round_rect(x, y, w, h, radius, color);
            return;
        }
        // Concentric rounded rectangles approximate a Gaussian falloff.
        let steps = blur.min(12);
        let mut i = steps;
        while i >= 1 {
            let spread = blur * i / steps;
            let alpha = color.a as i64 * (steps - i + 1) / (steps * steps / 2 + 1);
            self.round_rect(
                x - spread,
                y - spread,
                w + spread * 2,
                h + spread * 2,
                radius + spread,
                color.with_alpha(alpha.min(255)),
            );
            i -= 1;
        }
    }

    /// The pixels inside `rect`, as packed RGBA rows.
    pub fn read_rect(&self, rect: Rect) -> Vec<u8> {
        let r = rect.intersect(Rect::new(0, 0, self.width, self.height));
        let mut out = vec![0u8; (r.width * r.height * 4) as usize];
        let mut row = 0;
        while row < r.height {
            let mut col = 0;
            while col < r.width {
                let s = ((r.y + row) * self.width + r.x + col) * 4;
                let d = (row * r.width + col) * 4;
                out[d as usize] = self.pixels[s as usize];
                out[(d + 1) as usize] = self.pixels[(s + 1) as usize];
                out[(d + 2) as usize] = self.pixels[(s + 2) as usize];
                out[(d + 3) as usize] = self.pixels[(s + 3) as usize];
                col += 1;
            }
            row += 1;
        }
        out
    }
}

/// `rgba` (straight alpha, `src_w` by `src_h`) shrunk to `w` by `h` by
/// averaging the area each output pixel covers, which keeps fine detail from
/// aliasing where bilinear sampling would skip pixels. Colors are averaged
/// weighted by alpha, so transparent pixels do not darken edges.
pub fn shrink(rgba: &[u8], src_w: i64, src_h: i64, w: i64, h: i64) -> Vec<u8> {
    let rows = average_axis(&premultiplied(rgba), src_w, src_h, w, true);
    let both = average_axis(&rows, w, src_h, h, false);
    let mut out = vec![0u8; (w * h * 4) as usize];
    for i in 0..w * h {
        let a = both[(i * 4 + 3) as usize];
        if a > 0.0 {
            for c in 0..3 {
                out[(i * 4 + c) as usize] =
                    (both[(i * 4 + c) as usize] / a * 255.0 + 0.5).min(255.0) as u8
            }
            out[(i * 4 + 3) as usize] = (a * 255.0 + 0.5).min(255.0) as u8
        }
    }
    out
}

// Straight-alpha RGBA bytes as premultiplied channels in 0..1.
fn premultiplied(rgba: &[u8]) -> Vec<f64> {
    let mut out = vec![0.0; rgba.len()];
    for i in 0..rgba.len() / 4 {
        let a = rgba[i * 4 + 3] as f64 / 255.0;
        for c in 0..3 {
            out[i * 4 + c] = rgba[i * 4 + c] as f64 / 255.0 * a
        }
        out[i * 4 + 3] = a
    }
    out
}

// Averages premultiplied pixels (`width` by `height`) along one axis down to
// `size` samples: across each row when `horizontal`, else down each column.
fn average_axis(px: &[f64], width: i64, height: i64, size: i64, horizontal: bool) -> Vec<f64> {
    let src = if horizontal { width } else { height };
    let lines = if horizontal { height } else { width };
    let out_w = if horizontal { size } else { width };
    let mut out = vec![
        0.0;
        ((if horizontal {
            size * height
        } else {
            width * size
        }) * 4) as usize
    ];
    let step = src as f64 / size as f64;
    for line in 0..lines {
        for t in 0..size {
            let from = t as f64 * step;
            let to = from + step;
            let mut k = from.floor() as i64;
            let mut sum = [0.0, 0.0, 0.0, 0.0];
            while (k as f64) < to && k < src {
                let weight = (to.min((k + 1) as f64) - from.max(k as f64)) / step;
                let at = if horizontal {
                    (line * width + k) * 4
                } else {
                    (k * width + line) * 4
                };
                for c in 0..4 {
                    sum[c] += px[(at + c as i64) as usize] * weight
                }
                k += 1;
            }
            let dst = if horizontal {
                (line * out_w + t) * 4
            } else {
                (t * out_w + line) * 4
            };
            for c in 0..4 {
                out[(dst + c) as usize] = sum[c as usize]
            }
        }
    }
    out
}

/// One output position's two source samples and the far sample's weight, in
/// 1/256ths.
struct Tap {
    near: usize,
    far: usize,
    weight: u32,
}

/// Bilinear taps for output positions `start..end` of a span placed at
/// `origin` that shows `source` samples over `span` positions.
fn taps(start: i64, end: i64, origin: i64, span: i64, source: i64) -> Vec<Tap> {
    let ratio = source as f64 / span as f64;
    (start..end)
        .map(|p| {
            let f = ((p - origin) as f64 + 0.5) * ratio - 0.5;
            let near = (f.floor() as i64).clamp(0, source - 1);
            Tap {
                near: near as usize,
                far: (near + 1).min(source - 1) as usize,
                weight: ((f - near as f64).clamp(0.0, 1.0) * 256.0).round() as u32,
            }
        })
        .collect()
}

fn image_scaled_kernel(
    dst: &mut [u8],
    stride: i64,
    clip: Rect,
    opacity: i64,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    rgba: &[u8],
    src_w: i64,
    src_h: i64,
) {
    let left = x.max(clip.x).max(0);
    let top = y.max(clip.y).max(0);
    let right = (x + w).min(clip.x + clip.width).min(stride / 4);
    let bottom = (y + h)
        .min(clip.y + clip.height)
        .min((dst.len() / stride as usize) as i64);
    if left >= right || top >= bottom {
        return;
    }
    let columns = taps(left, right, x, w, src_w);
    // Taps rise with the column, so the first and last bound the source span.
    let (first, last) = (columns[0].near, columns[columns.len() - 1].far);
    let src_stride = src_w as usize * 4;
    let span = (last - first + 1) * 4;
    let mut blended = vec![0u32; last - first + 1];
    let mut blended_from = None;
    for (row, tap) in (top..bottom).zip(taps(top, bottom, y, h, src_h)) {
        // Rows that sample the same two source lines alike share one blend.
        if blended_from != Some((tap.near, tap.weight)) {
            let line = |i: usize| {
                rgba[i * src_stride + first * 4..][..span]
                    .as_chunks::<4>()
                    .0
            };
            for ((out, n), f) in blended.iter_mut().zip(line(tap.near)).zip(line(tap.far)) {
                *out = lerp_pixel(u32::from_le_bytes(*n), u32::from_le_bytes(*f), tap.weight);
            }
            blended_from = Some((tap.near, tap.weight));
        }
        let start = (row * stride + left * 4) as usize;
        let out = &mut dst[start..start + columns.len() * 4];
        for (px, col) in out.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
            let near = blended[col.near - first];
            let far = blended[col.far - first];
            let [r, g, b, a] = lerp_pixel(near, far, col.weight).to_le_bytes();
            let alpha = opacity * i64::from(a) / 255;
            if alpha >= 255 {
                *px = [r, g, b, 255];
            } else if alpha > 0 {
                for (d, s) in px.iter_mut().zip([r, g, b]) {
                    *d = mix(*d, s, alpha);
                }
                px[3] = 255;
            }
        }
    }
}

/// Blends two RGBA pixels packed little-endian in a `u32`, taking `weight`
/// 256ths of `far`. Red and blue, then green and alpha, share one multiply
/// each: every 8-bit channel sits in a 16-bit lane its product cannot overflow.
fn lerp_pixel(near: u32, far: u32, weight: u32) -> u32 {
    const LANES: u32 = 0x00ff_00ff;
    const HALF: u32 = 0x0080_0080;
    let keep = 256 - weight;
    let rb = (((near & LANES) * keep + (far & LANES) * weight + HALF) >> 8) & LANES;
    let ga = ((((near >> 8) & LANES) * keep + ((far >> 8) & LANES) * weight + HALF) >> 8) & LANES;
    rb | (ga << 8)
}

// Copy image pixels through the clip in one mutable-buffer call. This avoids
// cloning the canvas or re-entering the rectangle painter for every wheel pixel.
fn image_rgba(
    dst: &mut [u8],
    stride: i64,
    clip: Rect,
    opacity: i64,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    rgba: &[u8],
) {
    if width <= 0 || height <= 0 || stride <= 0 {
        return;
    };
    let left = x.max(clip.x).max(0);
    let top = y.max(clip.y).max(0);
    let right = (x + width).min(clip.x + clip.width).min(stride / 4);
    let bottom = (y + height)
        .min(clip.y + clip.height)
        .min((dst.len() / stride as usize) as i64);
    let mut row = top;
    while row < bottom {
        let mut col = left;
        while col < right {
            let src = ((row - y) * width + col - x) * 4;
            let d = row * stride + col * 4;
            let a = clamp255(opacity * rgba[(src + 3) as usize] as i64 / 255);
            if a == 255 {
                dst[d as usize] = rgba[src as usize];
                dst[(d + 1) as usize] = rgba[(src + 1) as usize];
                dst[(d + 2) as usize] = rgba[(src + 2) as usize];
                dst[(d + 3) as usize] = 255;
            } else if a > 0 {
                dst[d as usize] = mix(dst[d as usize], rgba[src as usize], a);
                dst[(d + 1) as usize] = mix(dst[(d + 1) as usize], rgba[(src + 1) as usize], a);
                dst[(d + 2) as usize] = mix(dst[(d + 2) as usize], rgba[(src + 2) as usize], a);
                dst[(d + 3) as usize] = 255
            }
            col += 1;
        }
        row += 1;
    }
}

/// Copy `canvas` into `frame`, mapping RGBA to the frame's byte order.
/// The frame must match the canvas's size; the session checks before
/// calling, because a resized window takes a new framebuffer.
pub fn blit(canvas: &Canvas, frame: &mut windowing::Frame) {
    let (ro, go, bo) = frame.format.rgb_offsets();
    let width = mini(canvas.width, frame.width);
    let height = mini(canvas.height, frame.height);
    blit_rgba(
        &mut frame.pixels,
        frame.stride,
        &canvas.pixels,
        canvas.width * 4,
        width,
        height,
        ro as i64,
        go as i64,
        bo as i64,
    )
}

/// Copy `width` x `height` RGBA pixels from `src` (`src_stride` bytes a
/// row) into `dst` (`dst_stride` bytes a row), placing red, green, and
/// blue at the byte offsets `ro`, `go`, `bo` of each destination pixel
/// and its fourth byte opaque.
pub fn blit_rgba(
    dst: &mut [u8],
    dst_stride: i64,
    src: &[u8],
    src_stride: i64,
    width: i64,
    height: i64,
    ro: i64,
    go: i64,
    bo: i64,
) {
    let mut y = 0;
    while y < height {
        let mut s = y * src_stride;
        let mut d = y * dst_stride;
        let end = s + width * 4;
        while s < end {
            dst[(d + ro) as usize] = src[s as usize];
            dst[(d + go) as usize] = src[(s + 1) as usize];
            dst[(d + bo) as usize] = src[(s + 2) as usize];
            dst[(d + 3) as usize] = 255;
            s += 4;
            d += 4;
        }
        y += 1;
    }
}

/// Fill every pixel of `fb` with `color`. One call per frame start.
pub fn clear(fb: &mut [u8], color: Color) {
    let mut i = 0;
    let n = fb.len();
    while i + 4 <= n {
        fb[i] = color.r;
        fb[i + 1] = color.g;
        fb[i + 2] = color.b;
        fb[i + 3] = 255;
        i += 4;
    }
}

/// Fill the axis-aligned rectangle at `(x, y)` size `(w, h)`, clipped
/// to `clip`, blended at `opacity` over what is already there.
/// `stride` is the row length in bytes. Rows or columns outside the
/// surface are skipped, not an error.
pub fn blend_rect(
    fb: &mut [u8],
    stride: i64,
    clip: Rect,
    opacity: i64,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    color: Color,
) {
    if w <= 0 || h <= 0 || stride < 4 || opacity <= 0 {
        return;
    }
    let height = fb.len() / stride as usize;
    let x0 = maxi(maxi(x, clip.x), 0);
    let y0 = maxi(maxi(y, clip.y), 0);
    let x1 = mini(mini(x + w, clip.x + clip.width), stride / 4);
    let y1 = mini(mini(y + h, clip.y + clip.height), height as i64);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let solid = opacity >= 255;
    let mut row = y0;
    while row < y1 {
        let mut col = x0;
        while col < x1 {
            let o = row * stride + col * 4;
            if solid {
                fb[o as usize] = color.r;
                fb[(o + 1) as usize] = color.g;
                fb[(o + 2) as usize] = color.b
            } else {
                fb[o as usize] = mix(fb[o as usize], color.r, opacity);
                fb[(o + 1) as usize] = mix(fb[(o + 1) as usize], color.g, opacity);
                fb[(o + 2) as usize] = mix(fb[(o + 2) as usize], color.b, opacity)
            }
            fb[(o + 3) as usize] = 255;
            col += 1;
        }
        row += 1;
    }
}

/// Blend one pixel of `color` at `alpha` (0..=255) into `fb` (`stride`
/// bytes a row), through `opacity` and `clip`.
fn blend_px(
    fb: &mut [u8],
    stride: i64,
    clip: Rect,
    opacity: i64,
    x: i64,
    y: i64,
    alpha: i64,
    color: Color,
) {
    if x < clip.x || y < clip.y || x >= clip.x + clip.width || y >= clip.y + clip.height {
        return;
    }
    let a = clamp255(opacity * alpha / 255);
    if a <= 0 {
        return;
    }
    let o = y * stride + x * 4;
    if a >= 255 {
        fb[o as usize] = color.r;
        fb[(o + 1) as usize] = color.g;
        fb[(o + 2) as usize] = color.b
    } else {
        fb[o as usize] = mix(fb[o as usize], color.r, a);
        fb[(o + 1) as usize] = mix(fb[(o + 1) as usize], color.g, a);
        fb[(o + 2) as usize] = mix(fb[(o + 2) as usize], color.b, a)
    }
    fb[(o + 3) as usize] = 255
}

/// `Canvas::circle` over the pixels `fb` (`stride` bytes a row), clipped to
/// `clip` and faded by `opacity`.
fn circle_kernel(
    fb: &mut [u8],
    stride: i64,
    clip: Rect,
    opacity: i64,
    cx: i64,
    cy: i64,
    r: i64,
    color: Color,
) {
    if r <= 0 {
        return;
    }
    let rf = r as f64;
    let cxr = cx as f64;
    let cyr = cy as f64;
    let mut y = maxi(cy - r - 2, clip.y);
    let y_end = mini(cy + r + 2, clip.y + clip.height);
    while y < y_end {
        let dy = y as f64 + 0.5 - cyr;
        if y < clip.y - 1 || y > clip.y + clip.height {
            y += 1;
            continue;
        }
        let mut x = maxi(cx - r - 2, clip.x);
        let x_end = mini(cx + r + 2, clip.x + clip.width);
        while x < x_end {
            if x < clip.x - 1 || x > clip.x + clip.width {
                x += 1;
                continue;
            }
            let dx = x as f64 + 0.5 - cxr;
            let d = dx.hypot(dy) - rf;
            let a = clamp01(0.5 - d);
            if a > 0.0 {
                blend_px(fb, stride, clip, opacity, x, y, (a * 255.0) as i64, color)
            }
            x += 1;
        }
        y += 1;
    }
}

/// `Canvas::circle_stroke` over the pixels `fb` (`stride` bytes a row), clipped to
/// `clip` and faded by `opacity`.
fn circle_stroke_kernel(
    fb: &mut [u8],
    stride: i64,
    clip: Rect,
    opacity: i64,
    cx: i64,
    cy: i64,
    r: i64,
    t: i64,
    color: Color,
) {
    if r <= 0 || t <= 0 {
        return;
    }
    let rf = r as f64;
    let cxr = cx as f64;
    let cyr = cy as f64;
    let tf = t as f64;
    let mut y = maxi(cy - r - t - 2, clip.y);
    let y_end = mini(cy + r + t + 2, clip.y + clip.height);
    while y < y_end {
        if y < clip.y - 1 || y > clip.y + clip.height {
            y += 1;
            continue;
        }
        let dy = y as f64 + 0.5 - cyr;
        let mut x = maxi(cx - r - t - 2, clip.x);
        let x_end = mini(cx + r + t + 2, clip.x + clip.width);
        while x < x_end {
            if x < clip.x - 1 || x > clip.x + clip.width {
                x += 1;
                continue;
            }
            let dx = x as f64 + 0.5 - cxr;
            let d = (dx.hypot(dy) - rf).abs();
            let a = clamp01(tf / 2.0 + 0.5 - d);
            if a > 0.0 {
                blend_px(fb, stride, clip, opacity, x, y, (a * 255.0) as i64, color)
            }
            x += 1;
        }
        y += 1;
    }
}

/// `Canvas::round_rect_coverage` over the pixels `fb` (`stride` bytes a row), clipped to
/// `clip` and faded by `opacity`.
fn round_rect_coverage_kernel(
    fb: &mut [u8],
    stride: i64,
    clip: Rect,
    opacity: i64,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    r: i64,
    t: i64,
    color: Color,
) {
    if w <= 0 || h <= 0 {
        return;
    }
    let bx = w as f64 / 2.0;
    let by = h as f64 / 2.0;
    let mut rr = if r > w / 2 { w / 2 } else { r } as f64;
    if rr > h as f64 / 2.0 {
        rr = h as f64 / 2.0;
    }
    let tf = t as f64;
    let mut row = maxi(y - 2, clip.y);
    let row_end = mini(y + h + 2, clip.y + clip.height);
    while row < row_end {
        let mut col = maxi(x - 2, clip.x);
        let col_end = mini(x + w + 2, clip.x + clip.width);
        // The straight middle needs only spans, not a distance per pixel.
        if t <= 0 && row >= y + r && row < y + h - r {
            blend_rect(fb, stride, clip, opacity, x, row, w, 1, color);
            row += 1;
            continue;
        }
        while col < col_end {
            // A stroke cannot cover the box interior beyond its corner radius
            // and thickness. Jump over that span instead of evaluating a
            // distance and a square root for every untouched editor pixel.
            if t > 0
                && row as f64 >= y as f64 + rr + tf + 1.0
                && (row as f64) < (y + h) as f64 - rr - tf - 1.0
                && col as f64 >= x as f64 + rr + tf + 1.0
                && (col as f64) < (x + w) as f64 - rr - tf - 1.0
            {
                col = ((x + w) as f64 - rr - tf - 1.0) as i64;
                continue;
            }
            let px = col as f64 + 0.5 - x as f64 - bx;
            let py = row as f64 + 0.5 - y as f64 - by;
            let qx = px.abs() - bx + rr;
            let qy = py.abs() - by + rr;
            // Standard rounded-box SDF: the corner term keeps the
            // quadrant where both components are positive for the
            // hypot, and clamps the rest to inside-the-shape.
            let dx = maxi_f(qx, 0.0);
            let dy = maxi_f(qy, 0.0);
            let inside = mini_f(maxi_f(qx, qy), 0.0);
            let d = dx.hypot(dy) + inside - rr;
            let a = if t <= 0 {
                clamp01(0.5 - d)
            } else {
                clamp01(tf / 2.0 + 0.5 - d.abs())
            };
            if a > 0.0 {
                blend_px(
                    fb,
                    stride,
                    clip,
                    opacity,
                    col,
                    row,
                    (a * 255.0) as i64,
                    color,
                )
            }
            col += 1;
        }
        row += 1;
    }
}

/// `Canvas::line_aa_w` over the pixels `fb` (`stride` bytes a row), clipped to
/// `clip` and faded by `opacity`.
fn line_aa_w_kernel(
    fb: &mut [u8],
    stride: i64,
    clip: Rect,
    opacity: i64,
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
    width: f64,
    color: Color,
) {
    let ax = x0 as f64;
    let ay = y0 as f64;
    let bx = x1 as f64;
    let by = y1 as f64;
    let dx = bx - ax;
    let dy = by - ay;
    let len2 = dx * dx + dy * dy;
    let pad = (width / 2.0).ceil() as i64 + 2;
    let lox = maxi(mini(x0, x1) - pad, clip.x);
    let hix = mini(maxi(x0, x1) + pad, clip.x + clip.width - 1);
    let loy = maxi(mini(y0, y1) - pad, clip.y);
    let hiy = mini(maxi(y0, y1) + pad, clip.y + clip.height - 1);
    let hw = width / 2.0;
    let mut row = loy;
    while row <= hiy {
        let mut col = lox;
        while col <= hix {
            let px = col as f64 + 0.5;
            let py = row as f64 + 0.5;
            let mut t = 0.0;
            if len2 > 0.0 {
                t = clamp01(((px - ax) * dx + (py - ay) * dy) / len2);
            }
            let cx = ax + t * dx;
            let cy = ay + t * dy;
            let d = (px - cx).hypot(py - cy) - hw;
            let a = clamp01(0.5 - d);
            if a > 0.0 {
                blend_px(
                    fb,
                    stride,
                    clip,
                    opacity,
                    col,
                    row,
                    (a * 255.0) as i64,
                    color,
                )
            }
            col += 1;
        }
        row += 1;
    }
}

/// Blend the coverage buffer `cov` (one 0..=255 alpha per pixel, rows
/// packed, `w` wide) of `color` with its top-left at `(x, y)` into `fb`
/// (`stride` bytes a row), clipped to `clip` and faded by `opacity`.
pub fn blend_coverage(
    fb: &mut [u8],
    stride: i64,
    clip: Rect,
    opacity: i64,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    cov: &[u8],
    color: Color,
) {
    let y0 = maxi(y, clip.y);
    let y1 = mini(y + h, clip.y + clip.height);
    let x0 = maxi(x, clip.x);
    let x1 = mini(x + w, clip.x + clip.width);
    let mut row = y0;
    while row < y1 {
        let mut col = x0;
        let mut ci = (row - y) * w + (col - x);
        let mut o = row * stride + col * 4;
        while col < x1 {
            let a = clamp255(opacity * cov[ci as usize] as i64 / 255);
            if a >= 255 {
                fb[o as usize] = color.r;
                fb[(o + 1) as usize] = color.g;
                fb[(o + 2) as usize] = color.b
            } else if a > 0 {
                fb[o as usize] = mix(fb[o as usize], color.r, a);
                fb[(o + 1) as usize] = mix(fb[(o + 1) as usize], color.g, a);
                fb[(o + 2) as usize] = mix(fb[(o + 2) as usize], color.b, a)
            }
            col += 1;
            ci += 1;
            o += 4;
        }
        row += 1;
    }
}

/// The bare fill, opaque and unclipped, for callers that own the whole
/// surface. `Canvas::rect` is the clipped, fade-aware form.
pub fn fill_rect(fb: &mut [u8], stride: i64, x: i64, y: i64, w: i64, h: i64, color: Color) {
    blend_rect(
        fb,
        stride,
        Rect::new(0, 0, stride / 4, (fb.len() / stride as usize) as i64),
        255,
        x,
        y,
        w,
        h,
        color,
    )
}

/// One channel of `color` over `dst` at `opacity` (0..=255).
fn mix(dst: u8, src: u8, opacity: i64) -> u8 {
    let d = dst as i64;
    let s = src as i64;
    ((s * opacity + d * (255 - opacity)) / 255) as u8
}

fn lerp(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t).round() as u8
}

fn maxi_f(a: f64, b: f64) -> f64 {
    if a > b { a } else { b }
}

fn mini_f(a: f64, b: f64) -> f64 {
    if a < b { a } else { b }
}

/// `v` clamped into 0.0..=1.0; the anti-aliased edge coverage of a
/// signed distance that is 0 at the shape's edge.
fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

pub(crate) fn maxi(a: i64, b: i64) -> i64 {
    if a > b { a } else { b }
}

pub(crate) fn mini(a: i64, b: i64) -> i64 {
    if a < b { a } else { b }
}

pub(crate) fn clamp255(v: i64) -> i64 {
    maxi(0, mini(255, v))
}

/// The canvas encoded as a PNG file's bytes.
pub fn to_png(canvas: &Canvas) -> Vec<u8> {
    png::encode(canvas.width, canvas.height, &canvas.pixels).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{Canvas, blend_rect, clear, fill_rect};
    use crate::geometry::Rect;
    use crate::style::color::Color;

    #[test]
    fn shrinking_averages_area_and_ignores_transparent_color() {
        // A 2x2 image: opaque red, transparent black, opaque blue, opaque red.
        let px = [
            255u8, 0u8, 0u8, 255u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 255u8, 255u8, 255u8, 0u8, 0u8,
            255u8,
        ];
        let one = super::shrink(&px, 2, 2, 1, 1);
        // Three opaque pixels of four: red twice, blue once.
        assert_eq!(one, vec![170u8, 0u8, 85u8, 191u8]);
        let rows = super::shrink(&px, 2, 2, 1, 2);
        assert_eq!(rows, vec![255u8, 0u8, 0u8, 128u8, 128u8, 0u8, 128u8, 255u8])
    }

    #[test]
    fn clipped_large_primitives_finish_and_preserve_opacity() {
        let mut c = Canvas::new(4, 4);
        c.round_rect(
            -1_000_000,
            -1_000_000,
            2_000_000,
            2_000_000,
            4,
            Color::white(),
        );
        assert_eq!(c.pixels[0], 255);
        c.circle(0, 0, 1_000_000, Color::white());
        c.circle_stroke(0, 0, 1_000_000, 1, Color::black());
        c.line_aa(-1_000_000, 0, 1_000_000, 0, Color::black());
        c.opacity = 127;
        c.image(0, 0, 1, 1, &[255u8, 0u8, 0u8, 1u8]);
        assert_eq!(c.opacity, 127)
    }

    /// Bilinear sampling at the center of each covered pixel, in floating point.
    fn sampled(image: &[u8], sw: i64, sh: i64, w: i64, h: i64, col: i64, row: i64) -> [f64; 4] {
        let fx = ((col as f64 + 0.5) * sw as f64 / w as f64 - 0.5).clamp(0.0, (sw - 1) as f64);
        let fy = ((row as f64 + 0.5) * sh as f64 / h as f64 - 0.5).clamp(0.0, (sh - 1) as f64);
        let (x0, y0) = (fx.floor() as i64, fy.floor() as i64);
        let (x1, y1) = ((x0 + 1).min(sw - 1), (y0 + 1).min(sh - 1));
        let (tx, ty) = (fx - x0 as f64, fy - y0 as f64);
        let at = |x: i64, y: i64, k: usize| f64::from(image[((y * sw + x) * 4) as usize + k]);
        std::array::from_fn(|k| {
            let top = at(x0, y0, k) + (at(x1, y0, k) - at(x0, y0, k)) * tx;
            let bottom = at(x0, y1, k) + (at(x1, y1, k) - at(x0, y1, k)) * tx;
            top + (bottom - top) * ty
        })
    }

    #[test]
    fn scaled_images_sample_bilinearly_through_clip_and_opacity() {
        let (sw, sh) = (7i64, 5i64);
        let image: Vec<u8> = (0..sw * sh * 4)
            .map(|i| {
                if i % 4 == 3 {
                    255
                } else {
                    (i * 37 % 256) as u8
                }
            })
            .collect();
        for (w, h) in [(23, 17), (3, 2), (7, 5), (40, 4)] {
            let (x, y) = (2, 3);
            let mut c = Canvas::new(48, 24);
            c.push_clip(Rect::new(4, 0, 30, 18));
            c.image_scaled(x, y, w, h, &image, sw, sh);
            let shared = super::Pixels::new(image.clone());
            let mut by_reference = Canvas::new(48, 24);
            by_reference.push_clip(Rect::new(4, 0, 30, 18));
            by_reference.pixels_scaled(x, y, w, h, &shared, sw, sh);
            assert_eq!(c.pixels, by_reference.pixels, "{w}x{h}");
            for row in 0..24 {
                for col in 0..48 {
                    let p = &c.pixels[((row * 48 + col) * 4) as usize..][..4];
                    let inside = (4..34).contains(&col)
                        && row < 18
                        && (x..x + w).contains(&col)
                        && (y..y + h).contains(&row);
                    if !inside {
                        assert_eq!(p, [0, 0, 0, 0], "{w}x{h} outside at {col},{row}");
                        continue;
                    }
                    let want = sampled(&image, sw, sh, w, h, col - x, row - y);
                    for k in 0..3 {
                        assert!(
                            (f64::from(p[k]) - want[k]).abs() <= 1.5,
                            "{w}x{h} at {col},{row}: {p:?} vs {want:?}"
                        );
                    }
                }
            }
        }
        let mut faded = Canvas::new(2, 1);
        faded.opacity = 128;
        faded.image_scaled(0, 0, 2, 1, &[200, 100, 0, 255], 1, 1);
        assert_eq!(&faded.pixels[..4], [100, 50, 0, 255]);
        let mut clear = Canvas::new(2, 1);
        clear.image_scaled(0, 0, 2, 1, &[200, 100, 0, 0], 1, 1);
        assert_eq!(&clear.pixels[..4], [0, 0, 0, 0]);
    }

    #[test]
    fn replaced_pixels_reuse_an_unshared_buffer_under_a_new_identity() {
        let mut pixels = super::Pixels::new(vec![1u8; 8]);
        let before = pixels.clone();
        pixels.replace(&[2; 8]);
        assert_ne!(pixels, before);
        assert_eq!(*before, [1; 8]);
        assert_eq!(*pixels, [2; 8]);
        drop(before);
        let buffer = pixels.as_ptr();
        let earlier = pixels.clone();
        assert_eq!(pixels, earlier);
        drop(earlier);
        pixels.replace(&[3; 8]);
        assert_eq!(pixels.as_ptr(), buffer);
        assert_eq!(*pixels, [3; 8]);
        pixels.replace(&[4; 4]);
        assert_eq!(*pixels, [4; 4]);
    }

    fn fb() -> [u8; 16] {
        [0; 16] // 4x1 pixels, stride 16
    }

    #[test]
    fn clear_sets_every_byte_opaque() {
        let mut f = fb();
        clear(&mut f, Color::rgb(1, 2, 3));
        let mut i = 0;
        while i < 16 {
            assert_eq!(f[i], if i % 4 == 3 { 255 } else { (i % 4) as u8 + 1 });
            i += 1;
        }
    }

    #[test]
    fn fill_rect_clips_to_surface() {
        let mut f = fb();
        fill_rect(&mut f, 16, 2, 0, 10, 1, Color::white());
        assert_eq!((f[8], f[9], f[10], f[11]), (255, 255, 255, 255));
        assert_eq!((f[12], f[13], f[14], f[15]), (255, 255, 255, 255));
        assert_eq!(f[0], 0)
    }

    #[test]
    fn fill_rect_outside_draws_nothing() {
        let mut f = fb();
        fill_rect(&mut f, 16, 10, 10, 5, 5, Color::white());
        assert_eq!(f[0], 0)
    }

    #[test]
    fn canvas_clip_limits_drawing() {
        let mut c = Canvas::new(10, 10);
        c.push_clip(Rect::new(0, 0, 3, 3));
        c.rect(0, 0, 10, 10, Color::white());
        c.pop_clip(Rect::new(0, 0, 10, 10));
        // Inside the clip: white. Outside it: untouched.
        assert_eq!(c.pixels[0], 255);
        assert_eq!(c.pixels[(5 * 10 + 5) * 4], 0)
    }

    #[test]
    fn canvas_opacity_blends() {
        let mut c = Canvas::new(1, 1);
        c.rect(0, 0, 1, 1, Color::white());
        let restore = c.opacity;
        c.opacity = 128;
        c.rect(0, 0, 1, 1, Color::black());
        c.opacity = restore;
        // Half white over black is about 127.
        assert!(c.pixels[0] > 120 && c.pixels[0] < 135)
    }

    #[test]
    fn rounded_stroke_keeps_interior_and_clipped_pixels_untouched() {
        let mut c = Canvas::new(80, 60);
        c.fill(Color::black());
        c.opacity = 128;
        c.round_rect_stroke(8, 8, 64, 44, 6, 4, Color::white());
        assert_eq!(c.pixels[(30 * 80 + 40) * 4], 0);
        assert!(c.pixels[(30 * 80 + 8) * 4] >= 127);
        assert!(c.pixels[(8 * 80 + 40) * 4] >= 127);
        assert_eq!(c.pixels[(6 * 80 + 6) * 4], 0);
        let before = c.pixels.clone();
        c.push_clip(Rect::new(20, 20, 30, 20));
        c.round_rect_stroke(8, 8, 64, 44, 6, 4, Color::red());
        assert_eq!(c.pixels, before)
    }

    #[test]
    fn line_walks_its_endpoints() {
        let mut c = Canvas::new(10, 10);
        c.line(0, 0, 9, 9, Color::red());
        assert_eq!(c.pixels[0], 255);
        let o = (9 * 10 + 9) * 4;
        assert_eq!((c.pixels[o], c.pixels[o + 1]), (255, 0))
    }

    #[test]
    fn stroke_rect_outlines_without_filling() {
        let mut c = Canvas::new(5, 5);
        c.stroke_rect(0, 0, 5, 5, Color::white());
        assert_eq!(c.pixels[0], 255);
        // Centre stays empty.
        assert_eq!(c.pixels[(2 * 5 + 2) * 4], 0)
    }

    #[test]
    fn gradient_interpolates_top_to_bottom() {
        let mut c = Canvas::new(2, 10);
        c.v_gradient(0, 0, 2, 10, Color::black(), Color::white());
        assert_eq!(c.pixels[0], 0);
        assert!(c.pixels[9 * 2 * 4] > 230)
    }

    #[test]
    fn blend_rect_with_zero_opacity_draws_nothing() {
        let mut f = fb();
        blend_rect(
            &mut f,
            16,
            Rect::new(0, 0, 4, 1),
            0,
            0,
            0,
            4,
            1,
            Color::white(),
        );
        assert_eq!(f[0], 0)
    }
}

#[cfg(test)]
mod allocation_tests {
    use super::*;
    #[test]
    fn overflow_negative_and_oversized_framebuffers_are_rejected_before_allocation() {
        assert!(Canvas::try_new(-1, 20).is_err());
        assert!(Canvas::try_new(i64::MAX, i64::MAX).is_err());
        assert!(Canvas::try_new(16_384, 16_384).is_err());
        assert!(Canvas::try_new(3840, 2160).is_ok());
    }
}
