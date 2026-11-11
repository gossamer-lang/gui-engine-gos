//! Recorded paint with bounded, revision-keyed raster tiles, painted into the
//! pixels that persist in the window's own surface.
use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Canvas, Coverage, Surface, revision_hasher, swap_red_blue};
use crate::geometry::Rect;
use crate::style::color::Color;

#[derive(Debug, Clone, PartialEq)]
pub enum Primitive {
    Rect(i64, i64, i64, i64, Color),
    Circle(i64, i64, i64, Color),
    CircleStroke(i64, i64, i64, i64, Color),
    RoundRect(i64, i64, i64, i64, i64, i64, Color),
    Line(i64, i64, i64, i64, f64, Color),
    Coverage(i64, i64, i64, i64, Coverage, Color),
    Image(i64, i64, i64, i64, Arc<[u8]>, i64, i64),
    /// Pixels drawn by reference, as `Image` draws its own copy.
    Shared(i64, i64, i64, i64, super::Pixels, i64, i64),
    Layer(i64, i64, i64, i64, Arc<[u8]>, i64, i64),
    /// A widget's own pixels, drawn by reference at their size.
    Surface(i64, i64, super::Surface),
}

#[derive(Debug, Clone)]
pub struct Command {
    pub paint: Primitive,
    pub clip: Rect,
    pub opacity: i64,
    pub bounds: Rect,
    revision: u64,
}

// Revisions differ for nearly every command that differs, so they are
// compared before the payloads.
impl PartialEq for Command {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision
            && self.bounds == other.bounds
            && self.opacity == other.opacity
            && self.clip == other.clip
            && self.paint == other.paint
    }
}

impl Command {
    pub fn new(paint: Primitive, clip: Rect, opacity: i64) -> Self {
        let opacity = opacity.clamp(0, 255);
        let bounds = paint.bounds().intersect(clip);
        let mut h = revision_hasher();
        // Hash the exact binary payload; floating point widths use their bits.
        paint.hash_into(&mut h);
        [clip.x, clip.y, clip.width, clip.height, opacity].hash(&mut h);
        Self {
            paint,
            clip,
            opacity,
            bounds,
            revision: h.finish(),
        }
    }

    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match &self.paint {
                Primitive::Coverage(_, _, _, _, p, _) => p.len(),
                Primitive::Image(_, _, _, _, p, _, _) | Primitive::Layer(_, _, _, _, p, _, _) => {
                    p.len()
                }
                _ => 0,
            }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Isolates a primitive's source color and coverage for GPU composition.
    /// Rasterizing only this tile bounds temporary allocations even for paths
    /// whose unclipped coordinates extend far beyond the window.
    pub fn layer_pixels(&self, bounds: Rect) -> Vec<u8> {
        let mut black = Canvas::new(bounds.width, bounds.height);
        let mut white = black.clone();
        white.fill(Color::white());
        self.replay(&mut black, -bounds.x, -bounds.y);
        self.replay(&mut white, -bounds.x, -bounds.y);
        isolate(black.pixels, &white.pixels)
    }

    pub(crate) fn replay(&self, canvas: &mut Canvas, dx: i64, dy: i64) {
        self.replay_within(canvas, dx, dy, self.clip);
    }

    /// Like `replay`, but paints only inside `within` (frame coordinates).
    pub(crate) fn replay_within(&self, canvas: &mut Canvas, dx: i64, dy: i64, within: Rect) {
        let old = canvas.clip;
        let opacity = canvas.opacity;
        canvas.push_clip(self.clip.intersect(within).offset(dx, dy));
        canvas.opacity = self.opacity;
        match &self.paint {
            Primitive::Rect(x, y, w, h, c) => canvas.rect(x + dx, y + dy, *w, *h, *c),
            Primitive::Circle(x, y, r, c) => canvas.circle(x + dx, y + dy, *r, *c),
            Primitive::CircleStroke(x, y, r, t, c) => {
                canvas.circle_stroke(x + dx, y + dy, *r, *t, *c)
            }
            Primitive::RoundRect(x, y, w, h, r, t, c) => {
                canvas.round_rect_coverage(x + dx, y + dy, *w, *h, *r, *t, *c)
            }
            Primitive::Line(x, y, a, b, w, c) => {
                canvas.line_aa_w(x + dx, y + dy, a + dx, b + dy, *w, *c)
            }
            Primitive::Coverage(x, y, w, h, p, c) => {
                canvas.coverage_shared(x + dx, y + dy, *w, *h, p, *c)
            }
            Primitive::Layer(x, y, w, h, p, sw, sh) => {
                canvas.layer_scaled(x + dx, y + dy, *w, *h, p, *sw, *sh)
            }
            Primitive::Image(x, y, w, h, p, sw, sh) => {
                canvas.image_scaled(x + dx, y + dy, *w, *h, p, *sw, *sh)
            }
            Primitive::Shared(x, y, w, h, p, sw, sh) => {
                canvas.pixels_scaled(x + dx, y + dy, *w, *h, p, *sw, *sh)
            }
            Primitive::Surface(x, y, s) => canvas.surface(x + dx, y + dy, s),
        }
        canvas.clip = old;
        canvas.opacity = opacity;
    }
}

impl Primitive {
    fn bounds(&self) -> Rect {
        match self {
            Self::Rect(x, y, w, h, _)
            | Self::Coverage(x, y, w, h, _, _)
            | Self::Image(x, y, w, h, _, _, _)
            | Self::Shared(x, y, w, h, _, _, _)
            | Self::Layer(x, y, w, h, _, _, _) => Rect::new(*x, *y, *w, *h),
            Self::Surface(x, y, s) => Rect::new(*x, *y, s.width(), s.height()),
            Self::RoundRect(x, y, w, h, _, _, _) => Rect::new(x - 2, y - 2, w + 4, h + 4),
            Self::Circle(x, y, r, _) | Self::CircleStroke(x, y, r, _, _) => {
                Rect::new(x - r - 1, y - r - 1, r * 2 + 3, r * 2 + 3)
            }
            Self::Line(x, y, a, b, w, _) => {
                let pad = (w.abs() / 2.0).ceil() as i64 + 2;
                Rect::new(
                    x.min(a) - pad,
                    y.min(b) - pad,
                    (a - x).abs() + 2 * pad + 1,
                    (b - y).abs() + 2 * pad + 1,
                )
            }
        }
    }

    fn hash_into(&self, h: &mut impl Hasher) {
        std::mem::discriminant(self).hash(h);
        let color = |c: &Color, h: &mut _| [c.r, c.g, c.b, c.a].hash(h);
        match self {
            Self::Rect(x, y, w, t, c) => {
                [x, y, w, t].hash(h);
                color(c, h);
            }
            Self::Circle(x, y, r, c) => {
                [x, y, r].hash(h);
                color(c, h);
            }
            Self::CircleStroke(x, y, r, t, c) => {
                [x, y, r, t].hash(h);
                color(c, h);
            }
            Self::RoundRect(x, y, w, t, r, b, c) => {
                [x, y, w, t, r, b].hash(h);
                color(c, h);
            }
            Self::Line(x, y, a, b, w, c) => {
                [x, y, a, b].hash(h);
                w.to_bits().hash(h);
                color(c, h);
            }
            Self::Coverage(x, y, w, t, p, c) => {
                [x, y, w, t].hash(h);
                p.hash(h);
                color(c, h);
            }
            Self::Image(x, y, w, t, p, sw, sh) | Self::Layer(x, y, w, t, p, sw, sh) => {
                [x, y, w, t, sw, sh].hash(h);
                p.hash(h);
            }
            Self::Shared(x, y, w, t, p, sw, sh) => {
                [x, y, w, t, sw, sh].hash(h);
                p.hash(h);
            }
            Self::Surface(x, y, s) => {
                [x, y].hash(h);
                s.hash(h);
            }
        }
    }
}

/// Axis-aligned subtree transform. Negative scales reflect about the origin.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Transform {
    pub x: f64,
    pub y: f64,
    pub scale_x: f64,
    pub scale_y: f64,
}
impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
        }
    }
}

/// Commands a recording made, where a later recording of the frame that
/// follows can copy them from with [`Canvas::replay_span`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordedSpan {
    pub(crate) scene: u64,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

// Names each recording a window retains, across every window, so a span is
// copied only from the scene it was recorded into.
static NEXT_SCENE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    pub commands: Vec<Command>,
}
impl Scene {
    pub fn bytes(&self) -> usize {
        self.commands.capacity() * std::mem::size_of::<Command>()
            + self
                .commands
                .iter()
                .map(|c| c.bytes() - std::mem::size_of::<Command>())
                .sum::<usize>()
    }
    pub fn replay(&self, canvas: &mut Canvas) {
        for c in &self.commands {
            c.replay(canvas, 0, 0);
        }
    }
    /// Appends a transformed subtree, preserving the caller's clip and opacity.
    /// Masks are sampled in premultiplied color by the image primitive.
    pub fn append(
        &mut self,
        scene: &Scene,
        transform: Transform,
        clip: Rect,
        opacity: i64,
    ) -> Result<(), crate::errors::Error> {
        if ![
            transform.x,
            transform.y,
            transform.scale_x,
            transform.scale_y,
        ]
        .iter()
        .all(|v| v.is_finite())
            || transform.scale_x == 0.0
            || transform.scale_y == 0.0
        {
            return Err(crate::errors::new(
                "subtree transform must be finite with nonzero scales",
            ));
        }
        // A bounded subtree becomes one image layer. Its local raster is exact;
        // transforms then use the same image sampling as custom-widget images.
        let bounds = scene.commands.iter().map(|c| c.bounds).reduce(union);
        let Some(bounds) = bounds.filter(|r| r.width > 0 && r.height > 0) else {
            return Ok(());
        };
        if bounds.width.saturating_mul(bounds.height) > (Budgets::default().scene / 8) as i64 {
            return Err(crate::errors::new(
                "transformed subtree exceeds the raster budget",
            ));
        }
        let mut black = Canvas::new(bounds.width, bounds.height);
        let mut white = black.clone();
        white.fill(Color::white());
        for c in &scene.commands {
            c.replay(&mut black, -bounds.x, -bounds.y);
            c.replay(&mut white, -bounds.x, -bounds.y);
        }
        let mut rgba = isolate(black.pixels, &white.pixels);
        if transform.scale_x < 0.0 {
            for row in rgba.chunks_exact_mut(bounds.width as usize * 4) {
                let n = row.len() / 4;
                for x in 0..n / 2 {
                    for k in 0..4 {
                        row.swap(x * 4 + k, (n - 1 - x) * 4 + k);
                    }
                }
            }
        }
        if transform.scale_y < 0.0 {
            let stride = bounds.width as usize * 4;
            for y in 0..bounds.height as usize / 2 {
                for k in 0..stride {
                    rgba.swap(
                        y * stride + k,
                        (bounds.height as usize - 1 - y) * stride + k,
                    );
                }
            }
        }
        let x0 = transform.x + bounds.x as f64 * transform.scale_x;
        let x1 = x0 + bounds.width as f64 * transform.scale_x;
        let y0 = transform.y + bounds.y as f64 * transform.scale_y;
        let y1 = y0 + bounds.height as f64 * transform.scale_y;
        let dest = Rect::new(
            x0.min(x1).floor() as i64,
            y0.min(y1).floor() as i64,
            (x1 - x0).abs().ceil() as i64,
            (y1 - y0).abs().ceil() as i64,
        );
        self.commands.push(Command::new(
            Primitive::Layer(
                dest.x,
                dest.y,
                dest.width,
                dest.height,
                rgba.into(),
                bounds.width,
                bounds.height,
            ),
            clip,
            opacity,
        ));
        Ok(())
    }
}

fn isolate(mut black: Vec<u8>, white: &[u8]) -> Vec<u8> {
    for (p, q) in black
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(white.as_chunks::<4>().0.iter())
    {
        let alpha = 255 - u16::from(q[0].saturating_sub(p[0]));
        for channel in &mut p[..3] {
            *channel = (u16::from(*channel) * 255)
                .checked_div(alpha)
                .unwrap_or(0)
                .min(255) as u8;
        }
        p[3] = alpha as u8;
    }
    black
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect::new(
        x,
        y,
        (a.x + a.width).max(b.x + b.width) - x,
        (a.y + a.height).max(b.y + b.height) - y,
    )
}
fn nonempty(r: Rect) -> bool {
    r.width > 0 && r.height > 0
}

/// Per-window isolated-subtree cache. Equality of a handle means identity of
/// the cache; paint content remains represented by Scene and Command values.
#[derive(Debug, Clone, Default)]
pub struct SubtreeCache(Arc<parking_lot::Mutex<Subtrees>>);
impl PartialEq for SubtreeCache {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
#[derive(Debug, Default)]
struct Subtrees {
    entries: Vec<Subtree>,
    bytes: usize,
    limit: usize,
    clock: u64,
    hits: u64,
    /// Entries last used before this clock are dropped by the next sweep.
    swept: u64,
}
#[derive(Debug)]
struct Subtree {
    revision: u64,
    source: Scene,
    transform: Transform,
    clip: Rect,
    opacity: i64,
    command: Command,
    bytes: usize,
    used: u64,
}
impl Subtrees {
    fn evict(&mut self, required: usize) {
        while self.bytes.saturating_add(required) > self.limit && !self.entries.is_empty() {
            let i = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.used)
                .unwrap()
                .0;
            self.bytes -= self.entries.remove(i).bytes;
        }
    }
}
impl SubtreeCache {
    fn set_limit(&self, limit: usize) {
        let mut data = self.0.lock();
        data.limit = limit;
        data.hits = 0;
        data.evict(0);
    }
    fn trim_limit(&self, limit: usize) {
        let mut data = self.0.lock();
        data.limit = limit;
        data.evict(0);
    }
    fn bytes(&self) -> usize {
        self.0.lock().bytes
    }
    fn clear(&self) {
        let mut data = self.0.lock();
        data.entries.clear();
        data.entries.shrink_to_fit();
        data.bytes = 0;
        data.hits = 0;
    }
    fn take_hits(&self) -> u64 {
        std::mem::take(&mut self.0.lock().hits)
    }
    fn sweep(&self) -> bool {
        let mut data = self.0.lock();
        let next = data.clock + 1;
        let mark = std::mem::replace(&mut data.swept, next);
        data.entries.retain(|e| e.used >= mark);
        if data.entries.is_empty() {
            data.entries = Vec::new();
        }
        data.bytes = data.entries.iter().map(|e| e.bytes).sum();
        !data.entries.is_empty()
    }
}
impl Scene {
    pub fn append_cached(
        &mut self,
        source: &Scene,
        transform: Transform,
        clip: Rect,
        opacity: i64,
        cache: &SubtreeCache,
    ) -> Result<(), crate::errors::Error> {
        let mut hash = revision_hasher();
        for command in &source.commands {
            command.revision.hash(&mut hash);
        }
        [
            transform.x,
            transform.y,
            transform.scale_x,
            transform.scale_y,
        ]
        .map(f64::to_bits)
        .hash(&mut hash);
        [clip.x, clip.y, clip.width, clip.height, opacity].hash(&mut hash);
        let revision = hash.finish();
        let mut data = cache.0.lock();
        data.clock += 1;
        let clock = data.clock;
        if let Some(index) = data.entries.iter().position(|e| {
            e.revision == revision
                && e.source == *source
                && e.transform == transform
                && e.clip == clip
                && e.opacity == opacity
        }) {
            data.entries[index].used = clock;
            self.commands.push(data.entries[index].command.clone());
            data.hits += 1;
            return Ok(());
        }
        let before = self.commands.len();
        self.append(source, transform, clip, opacity)?;
        if self.commands.len() == before {
            return Ok(());
        }
        let command = self.commands.last().unwrap();
        let bytes = source
            .bytes()
            .saturating_add(command.bytes())
            .saturating_add(std::mem::size_of::<Subtree>());
        if bytes <= data.limit {
            data.evict(bytes);
            data.bytes += bytes;
            data.entries.push(Subtree {
                revision,
                source: source.clone(),
                transform,
                clip,
                opacity,
                command: command.clone(),
                bytes,
                used: clock,
            });
        }
        Ok(())
    }
}

/// Caps both retained command storage and cached raster layers per window.
#[derive(Debug, Clone, Copy)]
pub struct Budgets {
    pub scene: usize,
    pub layers: usize,
}
impl Default for Budgets {
    fn default() -> Self {
        Self {
            scene: 32 * 1024 * 1024,
            layers: 16 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Stats {
    pub damage_pixels: u64,
    pub rasterized_pixels: u64,
    pub cache_hits: u64,
    pub layer_bytes: usize,
    pub scene_bytes: usize,
    pub gpu_layer_bytes: usize,
}
struct Tile {
    rect: Rect,
    revision: u64,
    pixels: Vec<u8>,
    used: u64,
}

// How many recent contents of each tile position are remembered.
const SEEN: usize = 4;

/// The pixels a frame is rasterized into, such as a window's surface: rows
/// `stride` bytes apart, each pixel four bytes holding red, green and blue at
/// `offsets` and their coverage in the fourth byte, premultiplied.
pub struct Target<'a> {
    pub pixels: &'a mut [u8],
    pub width: i64,
    pub height: i64,
    pub stride: i64,
    pub offsets: (usize, usize, usize),
}

pub struct Retained {
    /// The size of the frame the retained scene was last shown at.
    pub width: i64,
    pub height: i64,
    scene: Arc<Scene>,
    /// The recording `scene` holds, or 0 when it holds none.
    scene_id: u64,
    /// The recording the next commit retains.
    pending_id: u64,
    tiles: Vec<Tile>,
    /// Tiles last used before this frame are dropped by the next sweep.
    swept: u64,
    /// The latest content revisions each tile position showed, newest last.
    /// A raster is cached only for content that comes back after other
    /// content replaced it there: content that recurs (a blinking cursor, a
    /// menu closing) pays for its pixels, while content that only moves on (a
    /// terminal's output) or is repainted in place (an exposed or restyled
    /// window) does not.
    seen: HashMap<(i64, i64), Vec<u64>>,
    pub(crate) subtrees: SubtreeCache,
    pub budgets: Budgets,
    clock: u64,
    valid: bool,
    /// The last raster reached only part of the frame.
    partial: bool,
    /// One tile's pixels for each raster worker to draw a tile it will not
    /// keep. They belong to the thread that rasters the frame, so a worker
    /// allocates no tile: a thread's allocator keeps the pages it used after
    /// the thread ends, until another thread reclaims them.
    scratch: Vec<Vec<u8>>,
    pub stats: Stats,
    /// The window shows rings in their own layers. The scrolling part of a
    /// ring surface is left transparent so its layers show through, and
    /// scrolling or changing rows there causes no damage (see [`ring_hole`]).
    pub rings: bool,
    /// Whether the kept pixels were painted with rings turned on.
    rings_painted: bool,
}
impl Default for Retained {
    fn default() -> Self {
        Self::new()
    }
}
impl Retained {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            scene: Arc::default(),
            scene_id: 0,
            pending_id: 0,
            tiles: vec![],
            swept: 0,
            seen: HashMap::new(),
            subtrees: SubtreeCache::default(),
            budgets: Budgets::default(),
            clock: 0,
            valid: false,
            partial: false,
            rings: false,
            rings_painted: false,
            scratch: vec![],
            stats: Stats::default(),
        }
    }
    pub fn purge(&mut self) {
        self.subtrees.clear();
        self.tiles = Vec::new();
        self.seen = HashMap::new();
        self.scene = Arc::default();
        self.scene_id = 0;
        self.pending_id = 0;
        self.valid = false;
        self.stats = Stats::default();
    }
    /// Recording inherits the window's bounded subtree cache. At most half
    /// the layer budget can be occupied by isolated subtrees; raster tiles
    /// use the remainder, including unused subtree space. The recording may
    /// copy spans of the scene retained now, which the next commit replaces.
    pub fn recording(&mut self, width: i64, height: i64) -> Canvas {
        self.subtrees.set_limit(self.budgets.layers / 2);
        let mut canvas = Canvas::recording(width, height, self.budgets.scene);
        canvas.subtrees = Some(self.subtrees.clone());
        self.pending_id = NEXT_SCENE.fetch_add(1, Ordering::Relaxed);
        canvas.recording_id = self.pending_id;
        if self.scene_id != 0 {
            canvas.previous = Some((self.scene_id, Arc::clone(&self.scene)));
        }
        canvas
    }
    pub fn scene(&self) -> &Scene {
        &self.scene
    }
    /// Drops the raster tiles and subtrees no frame has reused since the
    /// previous sweep; content that comes back later rasters again. Answers
    /// whether any remain for a later sweep.
    pub fn sweep(&mut self) -> bool {
        let mark = std::mem::replace(&mut self.swept, self.clock + 1);
        self.tiles.retain(|t| t.used >= mark);
        if self.tiles.is_empty() {
            self.tiles = Vec::new();
        }
        let subtrees = self.subtrees.sweep();
        self.stats.layer_bytes =
            self.tiles.iter().map(|t| t.pixels.len()).sum::<usize>() + self.subtrees.bytes();
        subtrees || !self.tiles.is_empty()
    }
    /// Whether raster tiles or subtrees are cached for a later frame.
    pub fn caches_layers(&self) -> bool {
        !self.tiles.is_empty() || self.subtrees.bytes() > 0
    }
    pub fn invalidate(&mut self) {
        self.valid = false;
    }
    /// The regions of a `width` x `height` frame that showing `scene` in
    /// place of the retained one changes; removed and reordered operations
    /// damage both their old and new bounds. Starts the frame's statistics.
    pub fn damage(&mut self, width: i64, height: i64, scene: &Scene) -> Vec<Rect> {
        self.subtrees.trim_limit(self.budgets.layers / 2);
        self.clock += 1;
        self.stats = Stats::default();
        self.stats.cache_hits += self.subtrees.take_hits();
        let screen = Rect::new(0, 0, width, height);
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            self.valid = false;
            self.tiles.clear();
            self.seen.clear();
        }
        // Turning rings on or off changes the pixels, so repaint everything.
        if self.rings != self.rings_painted {
            self.rings_painted = self.rings;
            self.valid = false;
            self.tiles.clear();
        }
        if !self.valid {
            return vec![screen];
        }
        let rings = self.rings;
        let prefix = self
            .scene
            .commands
            .iter()
            .zip(&scene.commands)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = self.scene.commands[prefix..]
            .iter()
            .rev()
            .zip(scene.commands[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let mut damage = vec![];
        let old = &self.scene.commands[prefix..self.scene.commands.len() - suffix];
        let new = &scene.commands[prefix..scene.commands.len() - suffix];
        // Commands that stay in the same place of the same order paint the
        // same pixels, unless one that differs covers them.
        // A ring that only scrolled or changed rows in its scrolling part
        // causes no damage: its own layers show the change.
        if old.len() == new.len() {
            for (a, b) in old.iter().zip(new) {
                if a == b || (rings && ring_unchanged(a, b)) {
                    continue;
                }
                if let Some(changed) = rewritten(a, b) {
                    for rect in changed {
                        add_damage(&mut damage, rect.intersect(b.bounds).intersect(screen));
                    }
                    continue;
                }
                add_damage(&mut damage, a.bounds.intersect(screen));
                add_damage(&mut damage, b.bounds.intersect(screen));
            }
        } else {
            // Collect the ring commands first, so matching them stays cheap
            // even when the scene has thousands of commands.
            let (old_rings, new_rings): (Vec<&Command>, Vec<&Command>) = if rings {
                (
                    old.iter().filter(|c| ring_hole(c).is_some()).collect(),
                    new.iter().filter(|c| ring_hole(c).is_some()).collect(),
                )
            } else {
                (vec![], vec![])
            };
            let unchanged_ring = |c: &Command, others: &[&Command], old_first: bool| {
                ring_hole(c).is_some()
                    && others.iter().any(|o| {
                        if old_first {
                            ring_unchanged(c, o)
                        } else {
                            ring_unchanged(o, c)
                        }
                    })
            };
            for c in old {
                if rings && unchanged_ring(c, &new_rings, true) {
                    continue;
                }
                add_damage(&mut damage, c.bounds.intersect(screen));
            }
            for c in new {
                if rings && unchanged_ring(c, &old_rings, false) {
                    continue;
                }
                add_damage(&mut damage, c.bounds.intersect(screen));
            }
        }
        damage
    }
    /// Paints `damage` of `scene` into `target`, which keeps the pixels
    /// outside it from earlier frames. Cached tiles are keyed by the full
    /// paint revision, position and background, never by widget identity.
    pub fn raster(&mut self, scene: &Scene, damage: &[Rect], target: Target<'_>) {
        let (width, height) = (self.width, self.height);
        let layer_budget = self.budgets.layers.saturating_sub(self.subtrees.bytes());
        let mut total = self.tiles.iter().map(|t| t.pixels.len()).sum::<usize>();
        while total > layer_budget {
            let i = self
                .tiles
                .iter()
                .enumerate()
                .min_by_key(|(_, t)| t.used)
                .unwrap()
                .0;
            total -= self.tiles.remove(i).pixels.len();
        }
        // Tiles worth keeping get their own pixels up front, while the layer
        // budget has room; the rest draw into a worker's scratch tile.
        let mut room = layer_budget;
        let mut reaching = tile_commands(scene, width, height);
        let columns = tile_count(width);
        let mut bands: Vec<Vec<Job>> = vec![];
        for y in (0..height).step_by(TILE as usize) {
            let mut band = vec![];
            for x in (0..width).step_by(TILE as usize) {
                let tile = Rect::new(x, y, TILE.min(width - x), TILE.min(height - y));
                let parts: Vec<Rect> = damage
                    .iter()
                    .map(|d| d.intersect(tile))
                    .filter(|r| nonempty(*r))
                    .collect();
                if parts.is_empty() {
                    continue;
                }
                let commands =
                    std::mem::take(&mut reaching[((y / TILE) * columns + x / TILE) as usize]);
                let mut h = revision_hasher();
                [x, y, tile.width, tile.height].hash(&mut h);
                for c in &commands {
                    c.revision.hash(&mut h);
                }
                let revision = h.finish();
                let cached = self
                    .tiles
                    .iter()
                    .position(|t| t.rect == tile && t.revision == revision);
                let mut admit = false;
                if let Some(i) = cached {
                    self.stats.cache_hits += 1;
                    self.tiles[i].used = self.clock;
                } else {
                    let history = self.seen.entry((x, y)).or_default();
                    admit = history.iter().rev().skip(1).any(|r| *r == revision);
                    history.retain(|r| *r != revision);
                    if history.len() == SEEN {
                        history.remove(0);
                    }
                    history.push(revision);
                }
                let bytes = (tile.width * tile.height * 4) as usize;
                let kept = (admit && bytes <= room).then(|| {
                    room -= bytes;
                    vec![0u8; bytes]
                });
                band.push(Job {
                    tile,
                    parts,
                    revision,
                    commands,
                    cached,
                    kept,
                });
            }
            bands.push(band);
        }
        let stride = target.stride.max(0) as usize;
        let tiles = &self.tiles;
        let clock = self.clock;
        let mut work: Vec<(Band, Vec<Job>)> = target
            .pixels
            .chunks_mut((stride * TILE as usize).max(1))
            .zip(bands)
            .zip((0..height).step_by(TILE as usize))
            .filter(|((_, jobs), _)| !jobs.is_empty())
            .map(|((pixels, jobs), y)| {
                let band = Band {
                    y,
                    rows: (pixels.len() / stride.max(1)) as i64,
                    pixels,
                    stride,
                    width: target.width,
                    offsets: target.offsets,
                };
                (band, jobs)
            })
            .collect();
        let rings = self.rings;
        let raster = |(work, scratch): (&mut [(Band, Vec<Job>)], &mut Vec<u8>)| -> Banded {
            let mut done = Banded::default();
            for (band, jobs) in work {
                band_raster(band, jobs, tiles, clock, scratch, rings, &mut done);
            }
            done
        };
        // Bands own disjoint rows of the frame, so the bands with tiles to
        // rasterize do so side by side on the pool's workers.
        let fresh = work
            .iter()
            .filter(|(_, jobs)| jobs.iter().any(|j| j.cached.is_none()))
            .count();
        let workers = super::pool::parallelism().min(fresh).max(1);
        let per = work.len().div_ceil(workers).max(1);
        let chunks = work.len().div_ceil(per).max(1);
        let scratch = &mut self.scratch;
        while scratch.len() < chunks {
            scratch.push(Vec::with_capacity((TILE * TILE * 4) as usize));
        }
        let results: Vec<Banded> = super::pool::scope(
            work.chunks_mut(per)
                .zip(scratch.iter_mut())
                .map(|chunk| || raster(chunk))
                .collect(),
        );
        for done in results {
            self.stats.rasterized_pixels += done.rasterized_pixels;
            self.stats.damage_pixels += done.damage_pixels;
            for fresh in done.tiles {
                // A tile position keeps rasters of as many contents as it
                // remembers, its least recently used one giving way.
                let here = self.tiles.iter().filter(|t| t.rect == fresh.rect).count();
                if here >= SEEN
                    && let Some(i) = self
                        .tiles
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| t.rect == fresh.rect)
                        .min_by_key(|(_, t)| t.used)
                        .map(|(i, _)| i)
                {
                    total -= self.tiles.remove(i).pixels.len();
                }
                while total + fresh.pixels.len() > layer_budget {
                    let i = self
                        .tiles
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, t)| t.used)
                        .unwrap()
                        .0;
                    total -= self.tiles.remove(i).pixels.len();
                }
                total += fresh.pixels.len();
                self.tiles.push(fresh);
            }
        }
        self.partial = target.width != width || target.height != height;
    }
    /// Retains `scene` as what the frame shows, when it fits its budget.
    pub fn commit(&mut self, scene: Scene) {
        self.stats.layer_bytes =
            self.tiles.iter().map(|t| t.pixels.len()).sum::<usize>() + self.subtrees.bytes();
        self.stats.scene_bytes = scene.bytes();
        let fits = self.stats.scene_bytes <= self.budgets.scene;
        // A target of another size than the frame holds only part of it,
        // so the next frame repaints in full.
        self.valid = fits && !std::mem::take(&mut self.partial);
        let recorded = std::mem::take(&mut self.pending_id);
        (self.scene, self.scene_id) = if fits {
            (Arc::new(scene), recorded)
        } else {
            (Arc::default(), 0)
        };
        if !fits {
            self.stats.scene_bytes = 0;
        }
    }
}
// Fixed-size layers bound work and allocations even for a large primitive.
const TILE: i64 = 128;

/// Tiles of `TILE` pixels across `extent` pixels.
fn tile_count(extent: i64) -> i64 {
    (extent + TILE - 1).div_euclid(TILE).max(0)
}

/// The commands of `scene` that reach each tile of a `width` x `height`
/// frame, row by row, each tile's in scene order.
fn tile_commands(scene: &Scene, width: i64, height: i64) -> Vec<Vec<&Command>> {
    let (columns, rows) = (tile_count(width), tile_count(height));
    let mut reaching = vec![Vec::new(); (columns * rows) as usize];
    let screen = Rect::new(0, 0, width, height);
    for c in &scene.commands {
        let b = c.bounds.intersect(screen);
        if !nonempty(b) {
            continue;
        }
        for ty in b.y / TILE..=(b.y + b.height - 1) / TILE {
            for tx in b.x / TILE..=(b.x + b.width - 1) / TILE {
                reaching[(ty * columns + tx) as usize].push(c);
            }
        }
    }
    reaching
}

/// A damaged tile: its damaged parts, the commands that reach it, and the
/// cached raster of the same revision, if any.
struct Job<'a> {
    tile: Rect,
    parts: Vec<Rect>,
    revision: u64,
    commands: Vec<&'a Command>,
    cached: Option<usize>,
    /// The pixels a raster worth keeping goes into: the tile showed this
    /// content before, and the layer budget had room for it.
    kept: Option<Vec<u8>>,
}

/// One band of target rows, from frame row `y`, `rows` of them.
struct Band<'a> {
    y: i64,
    rows: i64,
    pixels: &'a mut [u8],
    stride: usize,
    width: i64,
    offsets: (usize, usize, usize),
}

/// What rasterizing some bands produced.
#[derive(Default)]
struct Banded {
    tiles: Vec<Tile>,
    rasterized_pixels: u64,
    damage_pixels: u64,
}

/// Paints the damaged parts of one band of tiles into the target's channel
/// order. New rasters of recurring content are kept while `room` lasts.
fn band_raster(
    band: &mut Band,
    jobs: &mut [Job],
    tiles: &[Tile],
    clock: u64,
    scratch: &mut Vec<u8>,
    rings: bool,
    done: &mut Banded,
) {
    let (ro, go, bo) = band.offsets;
    let inside = Rect::new(0, band.y, band.width, band.rows);
    for job in jobs {
        let tile = job.tile;
        if job.cached.is_none() && copy_surface(band, job, inside, rings, done) {
            continue;
        }
        let keep = job.kept.is_some();
        let raster: Cow<[u8]> = match job.cached {
            Some(i) => Cow::Borrowed(&tiles[i].pixels),
            None => {
                let pixels = job.kept.take().unwrap_or_else(|| std::mem::take(scratch));
                let mut canvas = Canvas::over(tile.width, tile.height, pixels);
                for c in &job.commands {
                    // Leave the ring's scrolling part transparent so its
                    // layers show through, but still paint anything on top.
                    // Paint the surface's fixed rows above and below it.
                    if rings && let Some((hole, surface)) = ring_hole(c) {
                        let top = hole.y - surface.turn().top;
                        let bottom = hole.y + hole.height;
                        let above = Rect::new(hole.x, top, hole.width, hole.y - top);
                        let below =
                            Rect::new(hole.x, bottom, hole.width, top + surface.height() - bottom);
                        for rows in [above, below].into_iter().filter(|r| nonempty(*r)) {
                            c.replay_within(&mut canvas, -tile.x, -tile.y, rows);
                        }
                        clear(&mut canvas, hole.intersect(c.clip).offset(-tile.x, -tile.y));
                        continue;
                    }
                    c.replay(&mut canvas, -tile.x, -tile.y);
                }
                done.rasterized_pixels += (tile.width * tile.height) as u64;
                Cow::Owned(canvas.pixels)
            }
        };
        for part in &job.parts {
            let part = part.intersect(inside);
            if !nonempty(part) {
                continue;
            }
            let len = part.width as usize * 4;
            for row in part.y..part.y + part.height {
                let src = ((row - tile.y) * tile.width + part.x - tile.x) as usize * 4;
                let dst = (row - band.y) as usize * band.stride + part.x as usize * 4;
                let into = band.pixels[dst..dst + len].as_chunks_mut::<4>().0;
                let from = raster[src..src + len].as_chunks::<4>().0;
                // The common byte orders as whole-pixel moves, which vectorize.
                match (ro, go, bo) {
                    (0, 1, 2) => {
                        for (d, s) in into.iter_mut().zip(from) {
                            *d = [s[0], s[1], s[2], s[3]];
                        }
                    }
                    (2, 1, 0) => swap_red_blue(into, from),
                    _ => {
                        for (d, s) in into.iter_mut().zip(from) {
                            d[ro] = s[0];
                            d[go] = s[1];
                            d[bo] = s[2];
                            d[3] = s[3];
                        }
                    }
                }
            }
            done.damage_pixels += (part.width * part.height) as u64;
        }
        if let Cow::Owned(pixels) = raster {
            if keep {
                done.tiles.push(Tile {
                    rect: tile,
                    revision: job.revision,
                    pixels,
                    used: clock,
                });
            } else {
                *scratch = pixels;
            }
        }
    }
}

/// Copies a tile's damage straight from an opaque surface on top of all else
/// that reaches the tile, which then needs no raster of its own. Answers
/// whether it did. With `rings`, a ring's scrolling part is made transparent
/// instead of copied.
fn copy_surface(band: &mut Band, job: &Job, inside: Rect, rings: bool, done: &mut Banded) -> bool {
    let Some(top) = job.commands.last() else {
        return false;
    };
    let Primitive::Surface(x, y, surface) = &top.paint else {
        return false;
    };
    let shown = Rect::new(*x, *y, surface.width, surface.height).intersect(top.clip);
    if !surface.opaque || top.opacity < 255 || shown.intersect(job.tile) != job.tile {
        return false;
    }
    let hole = ring_hole(top).filter(|_| rings).map(|(hole, _)| hole);
    let rgba = surface.rgba.read();
    let (ro, go, bo) = band.offsets;
    let (sr, sg, sb) = surface.offsets();
    for part in &job.parts {
        let part = part.intersect(inside);
        if !nonempty(part) {
            continue;
        }
        let len = part.width as usize * 4;
        for row in part.y..part.y + part.height {
            let src = (surface.stored_row(row - y) * surface.width + part.x - x) as usize * 4;
            let dst = (row - band.y) as usize * band.stride + part.x as usize * 4;
            let into = band.pixels[dst..dst + len].as_chunks_mut::<4>().0;
            // The hole is as wide as the surface, so each row is wholly in or out.
            if hole.is_some_and(|h| row >= h.y && row < h.y + h.height) {
                into.fill([0; 4]);
                continue;
            }
            let from = rgba[src..src + len].as_chunks::<4>().0;
            if (ro, go, bo) == (sr, sg, sb) {
                into.copy_from_slice(from);
            } else if (ro, bo) == (sb, sr) && go == sg {
                swap_red_blue(into, from);
            } else {
                for (d, s) in into.iter_mut().zip(from) {
                    d[ro] = s[sr];
                    d[go] = s[sg];
                    d[bo] = s[sb];
                    d[3] = s[3];
                }
            }
        }
        done.damage_pixels += (part.width * part.height) as u64;
    }
    true
}

/// The frame rectangles that differ when `b` draws a surface's content in
/// place of the content `a` drew there, where that is known.
fn rewritten(a: &Command, b: &Command) -> Option<Vec<Rect>> {
    let (Primitive::Surface(ax, ay, was), Primitive::Surface(x, y, now)) = (&a.paint, &b.paint)
    else {
        return None;
    };
    if (ax, ay) != (x, y) || a.clip != b.clip || a.opacity != b.opacity {
        return None;
    }
    let changed = now.changed_since(was)?;
    Some(changed.iter().map(|r| r.offset(*x, *y)).collect())
}

// Smallest scrolling area, in pixels, that gets its own ring layers. This is
// a guard against giving tiny surfaces layers, not a measured break-even;
// today only a terminal's text is drawn this way.
const RING_AREA: i64 = 256 * 256;

/// If `c` draws a ring, returns the window area its scrolling rows cover, and
/// the surface. A ring is an opaque surface with scrolling rows, drawn whole
/// and at full opacity. Its fixed rows above and below are painted as usual.
pub(crate) fn ring_hole(c: &Command) -> Option<(Rect, &Surface)> {
    let Primitive::Surface(x, y, surface) = &c.paint else {
        return None;
    };
    let turn = surface.turn();
    let shown = Rect::new(*x, *y, surface.width(), surface.height());
    let whole = c.clip.intersect(shown) == shown;
    let hole = Rect::new(*x, *y + turn.top, surface.width(), turn.len);
    (surface.is_opaque()
        && c.opacity >= 255
        && whole
        && turn.len > 0
        && turn.top >= 0
        && turn.top + turn.len <= surface.height()
        && hole.width * hole.height >= RING_AREA)
        .then_some((hole, surface))
}

/// Whether replacing ring `a` with ring `b` leaves the window's surface
/// unchanged: the hole is in the same place, and `b` changed only rows in its
/// scrolling part, which the ring's layers show.
fn ring_unchanged(a: &Command, b: &Command) -> bool {
    let (Some((was, old)), Some((hole, new))) = (ring_hole(a), ring_hole(b)) else {
        return false;
    };
    let turn = new.turn();
    let stretch = Rect::new(0, turn.top, new.width(), turn.len);
    was == hole
        && a.bounds == b.bounds
        && a.clip == b.clip
        && old.storage() == new.storage()
        && new.identity().1 == old.identity().0
        && new
            .written()
            .is_some_and(|rows| rows.iter().all(|r| stretch.intersect(*r) == *r))
}

/// Makes `rect` of `canvas` transparent.
fn clear(canvas: &mut Canvas, rect: Rect) {
    let r = rect.intersect(Rect::new(0, 0, canvas.width, canvas.height));
    if !nonempty(r) {
        return;
    }
    for row in r.y..r.y + r.height {
        let at = ((row * canvas.width + r.x) * 4) as usize;
        canvas.pixels[at..at + (r.width * 4) as usize].fill(0);
    }
}

fn add_damage(regions: &mut Vec<Rect>, mut rect: Rect) {
    if !nonempty(rect) {
        return;
    }
    let mut i = 0;
    while i < regions.len() {
        let r = regions[i];
        if nonempty(r.intersect(rect)) {
            rect = union(r, rect);
            regions.remove(i);
            i = 0;
        } else {
            i += 1;
        }
    }
    regions.push(rect);
    if regions.len() > 32 {
        let all = regions.iter().copied().reduce(union).unwrap();
        regions.clear();
        regions.push(all);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Shows `scene` in `frame` as a window's surface would, answering the
    // damage.
    fn show(r: &mut Retained, frame: &mut Canvas, scene: Scene) -> Vec<Rect> {
        let damage = r.damage(frame.width, frame.height, &scene);
        let (width, height) = (frame.width, frame.height);
        r.raster(
            &scene,
            &damage,
            Target {
                pixels: &mut frame.pixels,
                width,
                height,
                stride: width * 4,
                offsets: (0, 1, 2),
            },
        );
        r.commit(scene);
        damage
    }
    fn picture(c: &mut Canvas, revision: i64) {
        c.fill(Color::rgb(240, 240, 240));
        c.push_clip(Rect::new(3, 5, 270, 170));
        c.round_rect(2, 4, 170, 140, 12, Color::rgb(20, 40, 60));
        c.round_rect_stroke(12, 15, 210, 110, 6, 3, Color::rgb(40, 90, 150));
        c.push_opacity(140);
        c.circle(140 + revision, 90, 30, Color::rgb(255, 20, 40));
        c.circle_stroke(250, 60, 10, 2, Color::white());
        c.line_aa_w(-10, 20, 300, 150, 2.5, Color::rgb(5, 100, 200));
        c.coverage(150, 100, 2, 2, &[0, 127, 255, 180], Color::white());
        c.image_scaled(180, 100, 5, 7, &[255, 0, 0, 255, 0, 0, 255, 100], 2, 1);
        c.h_gradient(30, 140, 200, 10, Color::rgb(20, 30, 40), Color::white());
        c.shadow(150, 20, 20, 20, 5, 7, Color::rgb(0, 0, 0));
    }
    fn recorded(width: i64, height: i64, revision: i64) -> Scene {
        let mut c = Canvas::recording(width, height, 1024 * 1024);
        picture(&mut c, revision);
        c.take_scene().unwrap()
    }
    fn pixel(c: &Canvas, x: i64, y: i64) -> [u8; 4] {
        let at = ((y * c.width + x) * 4) as usize;
        [
            c.pixels[at],
            c.pixels[at + 1],
            c.pixels[at + 2],
            c.pixels[at + 3],
        ]
    }
    #[test]
    fn a_ring_turn_damages_nothing_and_its_stretch_stays_clear_under_what_paints_above() {
        use crate::render::Turn;
        let (width, height, line) = (300, 260, 20);
        let turn = Turn {
            top: 10,
            len: 240,
            shift: 0,
        };
        let mut surface = Surface::default();
        surface.rewrite(width, height, true, turn, None, |c, _| {
            c.fill(Color::rgb(1, 2, 3))
        });
        surface.set_written(None);
        let scene = |s: &Surface| {
            let mut c = Canvas::recording(400, 300, 1024 * 1024);
            c.fill(Color::rgb(200, 200, 200));
            c.surface(50, 20, s);
            c.rect(60, 100, 10, 10, Color::rgb(255, 0, 0));
            c.take_scene().unwrap()
        };
        let mut r = Retained::new();
        r.rings = true;
        let mut frame = Canvas::new(400, 300);
        assert_eq!(
            show(&mut r, &mut frame, scene(&surface)),
            vec![Rect::new(0, 0, 400, 300)]
        );
        // The scrolling rows are transparent, except where something is
        // drawn on top. Fixed rows and everything else paint as usual.
        assert_eq!(pixel(&frame, 55, 40), [0, 0, 0, 0]);
        assert_eq!(pixel(&frame, 62, 102), [255, 0, 0, 255]);
        assert_eq!(pixel(&frame, 55, 25)[3], 255);
        assert_eq!(pixel(&frame, 10, 10), [200, 200, 200, 255]);
        // Scrolling and writing a scrolling row causes no damage.
        let turned = Turn {
            shift: line,
            ..turn
        };
        surface.rewrite(width, height, true, turned, None, |c, _| {
            c.rect(0, 10, width, line, Color::rgb(9, 9, 9))
        });
        surface.set_written(Some(vec![Rect::new(0, 10, width, line)]));
        let damage = show(&mut r, &mut frame, scene(&surface));
        assert!(damage.is_empty(), "{damage:?}");
        assert_eq!(pixel(&frame, 55, 40), [0, 0, 0, 0]);
        // Writing a fixed row does cause damage.
        surface.rewrite(width, height, true, turned, None, |c, _| {
            c.rect(0, 0, width, 10, Color::rgb(9, 9, 9))
        });
        surface.set_written(Some(vec![Rect::new(0, 0, width, 10)]));
        assert!(!show(&mut r, &mut frame, scene(&surface)).is_empty());
        // With rings off, everything is painted into the frame again.
        r.rings = false;
        assert_eq!(
            show(&mut r, &mut frame, scene(&surface)),
            vec![Rect::new(0, 0, 400, 300)]
        );
        assert_eq!(pixel(&frame, 55, 40)[3], 255);
        assert_eq!(pixel(&frame, 62, 102), [255, 0, 0, 255]);
    }
    #[test]
    fn incremental_pixels_match_immediate_paint_after_moves_removals_and_resize() {
        let mut retained = Retained::new();
        let mut frame = Canvas::new(0, 0);
        for (w, h, r) in [
            (300, 200, 0),
            (300, 200, 1),
            (300, 200, 40),
            (640, 480, 5),
            (300, 200, -100),
        ] {
            if frame.width != w || frame.height != h {
                frame = Canvas::new(w, h);
            }
            show(&mut retained, &mut frame, recorded(w, h, r));
            let mut reference = Canvas::new(w, h);
            picture(&mut reference, r);
            assert_eq!(frame.pixels, reference.pixels, "{w}x{h} revision {r}");
        }
        let mut empty = Canvas::recording(300, 200, 1024);
        empty.fill(Color::white());
        show(&mut retained, &mut frame, empty.take_scene().unwrap());
        assert!(frame.pixels.iter().all(|v| *v == 255));
    }
    #[test]
    fn frames_paint_into_a_padded_surface_in_its_own_channel_order() {
        let (w, h) = (300, 200);
        let stride = w * 4 + 12;
        let mut surface = vec![9u8; (stride * h) as usize];
        let mut r = Retained::new();
        for revision in [0, 1, 40] {
            let scene = recorded(w, h, revision);
            let damage = r.damage(w, h, &scene);
            r.raster(
                &scene,
                &damage,
                Target {
                    pixels: &mut surface,
                    width: w,
                    height: h,
                    stride,
                    offsets: (2, 1, 0),
                },
            );
            r.commit(scene);
            let mut reference = Canvas::new(w, h);
            picture(&mut reference, revision);
            for y in 0..h {
                let row = &surface[(y * stride) as usize..][..(stride) as usize];
                for x in 0..w {
                    let s = &reference.pixels[((y * w + x) * 4) as usize..][..3];
                    let d = &row[(x * 4) as usize..][..4];
                    assert_eq!([d[2], d[1], d[0], d[3]], [s[0], s[1], s[2], 255], "{x},{y}");
                }
                assert!(row[(w * 4) as usize..].iter().all(|b| *b == 9));
            }
        }
    }
    #[test]
    fn shared_pixels_record_by_reference_and_damage_only_when_replaced() {
        let (sw, sh) = (320, 180);
        let mut frame = crate::render::Pixels::new(vec![90u8; (sw * sh * 4) as usize]);
        let scene = |frame: &crate::render::Pixels| {
            let mut c = Canvas::recording(640, 480, 64 * 1024);
            c.fill(Color::black());
            c.pixels_scaled(0, 60, 640, 360, frame, sw, sh);
            c.take_scene().unwrap()
        };
        let first = scene(&frame);
        assert!(
            first.bytes() < 64 * 1024,
            "the scene holds the frame by reference"
        );
        let mut r = Retained::new();
        let mut shown = Canvas::new(640, 480);
        show(&mut r, &mut shown, first);
        assert!(show(&mut r, &mut shown, scene(&frame)).is_empty());
        frame.replace(&vec![200u8; (sw * sh * 4) as usize]);
        show(&mut r, &mut shown, scene(&frame));
        assert_eq!(r.stats.damage_pixels, 640 * 360);
        let mut reference = Canvas::new(640, 480);
        scene(&frame).replay(&mut reference);
        assert_eq!(shown.pixels, reference.pixels);
    }
    #[test]
    fn caret_damage_is_small_idle_does_no_work_and_pixels_persist() {
        let scene = |caret| {
            let mut c = Canvas::recording(640, 480, 4096);
            c.fill(Color::white());
            if caret {
                c.rect(20, 20, 1, 18, Color::rgb(0, 0, 0));
            }
            c.take_scene().unwrap()
        };
        let mut r = Retained::new();
        let mut frame = Canvas::new(640, 480);
        show(&mut r, &mut frame, scene(true));
        show(&mut r, &mut frame, scene(false));
        assert_eq!(r.stats.damage_pixels, 18);
        assert!(r.stats.rasterized_pixels < 640 * 480);
        assert!(show(&mut r, &mut frame, scene(false)).is_empty());
        assert_eq!(r.stats.rasterized_pixels, 0);
        assert_eq!(r.stats.damage_pixels, 0);
    }
    #[test]
    fn content_that_only_moves_on_is_never_cached() {
        let mut r = Retained::new();
        let mut shown = Canvas::new(300, 200);
        for frame in 0..20 {
            show(&mut r, &mut shown, recorded(300, 200, frame));
            assert_eq!(r.stats.layer_bytes, 0, "frame {frame}");
        }
        // Two states taking turns (a blinking cursor) settle into hits.
        for frame in 0..6 {
            show(&mut r, &mut shown, recorded(300, 200, 100 + frame % 2));
        }
        assert!(r.stats.cache_hits > 0);
    }
    #[test]
    fn a_sweep_keeps_layers_that_recur_and_drops_those_no_frame_reused() {
        let mut r = Retained::new();
        let mut shown = Canvas::new(300, 200);
        for frame in 0..6 {
            show(&mut r, &mut shown, recorded(300, 200, frame % 2));
        }
        assert!(r.caches_layers());
        let cached = r.stats.layer_bytes;
        // Reused since the window last painted, so the first sweep keeps them.
        assert!(r.sweep());
        assert_eq!(r.stats.layer_bytes, cached);
        // Two states still taking turns keep their layers through the next.
        show(&mut r, &mut shown, recorded(300, 200, 0));
        show(&mut r, &mut shown, recorded(300, 200, 1));
        assert!(r.sweep());
        assert_eq!(r.stats.layer_bytes, cached);
        // A window gone quiet holds nothing after a sweep it did not paint in.
        assert!(!r.sweep());
        assert_eq!(r.stats.layer_bytes, 0);
        assert!(!r.caches_layers());
        assert_eq!(r.tiles.capacity(), 0);
        // What comes back rasters again, the same as before.
        let mut reference = Canvas::new(300, 200);
        picture(&mut reference, 0);
        show(&mut r, &mut shown, recorded(300, 200, 0));
        assert_eq!(shown.pixels, reference.pixels);
    }
    #[test]
    fn byte_budgets_eviction_revision_reuse_and_purge_preserve_pixels() {
        let mut r = Retained::new();
        let mut frame = Canvas::new(300, 200);
        r.budgets.layers = 6 * 128 * 128 * 4;
        show(&mut r, &mut frame, recorded(300, 200, 0));
        // Content shown once, or repainted in place, is not cached.
        assert_eq!(r.stats.layer_bytes, 0);
        r.invalidate();
        show(&mut r, &mut frame, recorded(300, 200, 0));
        assert_eq!(r.stats.layer_bytes, 0);
        // Content that comes back after other content replaced it is.
        show(&mut r, &mut frame, recorded(300, 200, 1));
        show(&mut r, &mut frame, recorded(300, 200, 0));
        assert_eq!(r.stats.cache_hits, 0);
        assert!(r.stats.layer_bytes > 0 && r.stats.layer_bytes <= r.budgets.layers);
        show(&mut r, &mut frame, recorded(300, 200, 1));
        show(&mut r, &mut frame, recorded(300, 200, 0));
        assert!(r.stats.cache_hits > 0);
        r.budgets.layers = 0;
        r.invalidate();
        show(&mut r, &mut frame, recorded(300, 200, 0));
        assert_eq!(r.stats.layer_bytes, 0);
        r.budgets.scene = 0;
        show(&mut r, &mut frame, recorded(300, 200, 1));
        assert_eq!(r.stats.scene_bytes, 0);
        r.purge();
        assert!(r.tiles.is_empty());
        assert!(r.scene.commands.is_empty());
        assert_eq!(r.scene.bytes(), 0);
        assert_eq!(r.tiles.capacity(), 0);
        show(&mut r, &mut frame, recorded(300, 200, 1));
        let mut c = Canvas::new(300, 200);
        picture(&mut c, 1);
        assert_eq!(frame.pixels, c.pixels);
        let mut c = Canvas::recording(10, 10, 16);
        c.image(0, 0, 10, 10, &[0; 400]);
        assert_eq!(c.recorded_bytes, 0);
        assert!(c.take_scene().is_none());
    }
    #[test]
    fn subtree_translation_scale_reflection_clip_and_opacity_compose() {
        let mut local = Canvas::recording(4, 2, 4096);
        local.rect(0, 0, 2, 2, Color::rgb(255, 0, 0));
        local.rect(2, 0, 2, 2, Color::rgb(0, 0, 255));
        let scene = local.take_scene().unwrap();
        let mut transformed = Scene::default();
        transformed
            .append(
                &scene,
                Transform {
                    x: 10.0,
                    y: 2.0,
                    scale_x: -2.0,
                    scale_y: 2.0,
                },
                Rect::new(3, 0, 7, 8),
                128,
            )
            .unwrap();
        let mut c = Canvas::new(12, 8);
        c.fill(Color::white());
        transformed.replay(&mut c);
        let at = |x, y| &c.pixels[((y * 12 + x) * 4) as usize..((y * 12 + x) * 4 + 3) as usize];
        assert_eq!(at(2, 3), [255, 255, 255]);
        assert_eq!(at(3, 3), [127, 127, 255]);
        assert_eq!(at(9, 3), [255, 127, 127]);
        assert!(
            transformed
                .append(
                    &scene,
                    Transform {
                        scale_x: f64::NAN,
                        ..Transform::default()
                    },
                    Rect::new(0, 0, 10, 10),
                    255
                )
                .is_err()
        );
    }
    #[test]
    fn unchanged_transformed_subtrees_hit_before_raster_and_share_the_layer_budget() {
        let mut r = Retained::new();
        let mut frame = Canvas::new(128, 128);
        r.budgets.layers = 65536;
        let layer = |color| super::super::display::DrawOp::Layer {
            ops: vec![super::super::display::DrawOp::RoundRect {
                rect: Rect::new(0, 0, 12, 12),
                radius: 3,
                color,
            }],
            transform: Transform {
                x: 20.0,
                y: 20.0,
                scale_x: 2.0,
                scale_y: 2.0,
            },
        };
        for repeat in 0..2 {
            let mut c = r.recording(128, 128);
            c.fill(Color::white());
            super::super::display::draw(&mut c, &[layer(Color::rgb(100, 0, 0))], 0, 0);
            show(&mut r, &mut frame, c.take_scene().unwrap());
            if repeat == 1 {
                assert_eq!(r.stats.rasterized_pixels, 0);
                assert!(r.stats.cache_hits > 0);
            }
            assert!(r.stats.layer_bytes <= r.budgets.layers);
        }
        for color in 0..40 {
            let mut c = r.recording(128, 128);
            c.fill(Color::white());
            super::super::display::draw(&mut c, &[layer(Color::rgb(color, 0, 0))], 0, 0);
            show(&mut r, &mut frame, c.take_scene().unwrap());
            assert!(r.stats.layer_bytes <= r.budgets.layers);
        }
        r.budgets.layers = 0;
        let mut c = r.recording(128, 128);
        c.fill(Color::white());
        super::super::display::draw(&mut c, &[layer(Color::rgb(100, 0, 0))], 0, 0);
        show(&mut r, &mut frame, c.take_scene().unwrap());
        assert_eq!(r.stats.layer_bytes, 0);
        r.purge();
        assert_eq!(r.subtrees.bytes(), 0);
    }

    #[test]
    fn transparent_layers_use_premultiplied_sampling() {
        let mut c = Canvas::new(4, 1);
        c.fill(Color::white());
        c.layer_scaled(0, 0, 4, 1, &[255, 0, 0, 255, 0, 0, 0, 0], 2, 1);
        assert_eq!(c.pixels[4], 255);
        assert_eq!(c.pixels[8], 255);
        assert!(c.pixels[5] < c.pixels[9]);
    }
}
