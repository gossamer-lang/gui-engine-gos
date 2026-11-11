//! GPU composition with software fallback, sharing the exact raster reference.
use super::retained::{Command, Primitive, Retained, Scene, Target, overlay_of, ring_hole};
use crate::{errors, geometry::Rect};

// Rings get their own layers where the window supports it. GUI_RINGS=0 turns
// this off, so rings are painted into the window's surface like elsewhere.
fn rings_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("GUI_RINGS").as_deref() != Ok("0"))
}

fn native_rect(r: Rect) -> windowing::display::Rect {
    windowing::display::Rect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

// Software is the permanent default. Only recognized explicit opt-ins may
// create a GPU; typos/empty values must never silently enable acceleration.
fn gpu_requested(mode: Option<&str>, driver: &str) -> bool {
    match mode {
        Some("gpu") => true,
        Some("auto") => matches!(driver, "x11" | "wayland" | "windows" | "cocoa"),
        _ => false,
    }
}

// A thread repays its start-up cost once it converts this many pixels.
const PIXELS_PER_WORKER: i64 = 1 << 18;

/// Copies damaged canvas pixels into a native surface's channel order.
struct Conversion<'a> {
    canvas: &'a super::Canvas,
    damage: &'a [Rect],
    stride: i64,
    offsets: (usize, usize, usize),
}

impl Conversion<'_> {
    /// Converts the damage within `band`, the surface rows from `top` down.
    fn band(&self, top: i64, band: &mut [u8]) {
        let (ro, go, bo) = self.offsets;
        let c = self.canvas;
        let rows = band.len() as i64 / self.stride;
        for r in self.damage {
            let len = r.width as usize * 4;
            for y in r.y.max(top)..(r.y + r.height).min(top + rows) {
                let src = &c.pixels[((y * c.width + r.x) * 4) as usize..][..len];
                let dst = &mut band[((y - top) * self.stride + r.x * 4) as usize..][..len];
                for (d, s) in dst
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(src.as_chunks::<4>().0)
                {
                    d[ro] = s[0];
                    d[go] = s[1];
                    d[bo] = s[2];
                    d[3] = s[3];
                }
            }
        }
    }
}

/// What a window painted for one frame.
pub enum Painted {
    /// The recorded scene, diffed against the one retained.
    Scene(Scene),
    /// The whole frame, painted at once because its scene outgrew its budget.
    Frame(super::Canvas),
}

fn native(damage: &[Rect]) -> Vec<windowing::display::Rect> {
    damage
        .iter()
        .map(|r| windowing::display::Rect {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        })
        .collect()
}

// The frame pixels `damage` repaints, counted as the software raster counts
// the parts it paints, so every backend reports the same damage.
fn damage_pixels(damage: &[Rect], screen: Rect) -> u64 {
    damage
        .iter()
        .map(|r| r.intersect(screen))
        .filter(|r| r.width > 0 && r.height > 0)
        .map(|r| (r.width * r.height) as u64)
        .sum()
}

// Copies `damage` of a whole-frame canvas into the window's surface.
fn convert(
    window: windowing::Window,
    canvas: &super::Canvas,
    damage: &[Rect],
) -> Result<(), errors::Error> {
    window.paint_surface(&native(damage), |pixels, width, height, stride, format| {
        let screen = Rect::new(0, 0, width.min(canvas.width), height.min(canvas.height));
        let damage: Vec<Rect> = damage
            .iter()
            .map(|r| r.intersect(screen))
            .filter(|r| r.width > 0 && r.height > 0)
            .collect();
        let area: i64 = damage.iter().map(|r| r.width * r.height).sum();
        let workers = super::pool::parallelism()
            .min((area / PIXELS_PER_WORKER) as usize)
            .max(1);
        let rows = (height as usize).div_ceil(workers).max(1);
        let to_native = Conversion {
            canvas,
            damage: &damage,
            stride,
            offsets: format.rgb_offsets(),
        };
        let bands = pixels
            .chunks_mut(stride as usize * rows)
            .zip((0..).step_by(rows));
        if workers == 1 {
            bands.for_each(|(band, top)| to_native.band(top as i64, band));
        } else {
            // Bands are disjoint rows of the surface.
            std::thread::scope(|scope| {
                for (band, top) in bands {
                    let to_native = &to_native;
                    scope.spawn(move || to_native.band(top as i64, band));
                }
            });
        }
    })?;
    Ok(())
}

#[derive(Default)]
pub struct WindowRenderer {
    pub retained: Retained,
    gpu: Option<windowing::compositor::Compositor>,
    retry: u32,
    // The overlays the window shows in layers of their own.
    overlays: Vec<windowing::surface::Overlay>,
    // The overlay cells rendered most recently, the newest last.
    cells: Vec<OverlayCell>,
}

// An overlay's area rendered the way the software painter renders it, with
// a hash of everything it was rendered from.
struct OverlayCell {
    content: u64,
    overlay: windowing::surface::Overlay,
}

// How many rendered cells are kept. This is enough for cursors blinking in a
// few panes to show again without being rendered again.
const KEPT_CELLS: usize = 4;

// Whether `c` paints anything in `area`.
fn reaches(c: &Command, area: Rect) -> bool {
    let r = c.bounds.intersect(area);
    r.width > 0 && r.height > 0
}

// The overlays `scene` draws, ready for the window to show. Each overlay's
// area is rendered in software with everything painted below it and then the
// overlay itself, so its pixels are exactly what the window's surface would
// show there. Rings are drawn in full, as they are when ring layers are off,
// because the overlay's layer covers the ring layers too. An area is rendered
// again only when something it is rendered from has changed. A cursor that
// blinks back on finds its earlier pixels and reuses them.
fn overlays_of(cells: &mut Vec<OverlayCell>, scene: &Scene) -> Vec<windowing::surface::Overlay> {
    use std::hash::{Hash, Hasher};
    let mut lifted = Vec::new();
    for (i, top) in scene.commands.iter().enumerate() {
        let Some((area, _)) = overlay_of(top) else {
            continue;
        };
        // If something drawn later covers an overlay, the overlay was already
        // turned into a plain rect when the scene was taken (see
        // `Scene::lower_covered_overlays`). So only commands drawn before an
        // overlay can reach its area.
        let below = || scene.commands[..i].iter().filter(|c| reaches(c, area));
        let mut h = super::revision_hasher();
        [area.x, area.y, area.width, area.height].hash(&mut h);
        top.revision().hash(&mut h);
        for c in below() {
            let (b, clip) = (c.bounds, c.clip);
            let placed = [
                b.x,
                b.y,
                b.width,
                b.height,
                clip.x,
                clip.y,
                clip.width,
                clip.height,
            ];
            (c.revision(), placed, c.opacity).hash(&mut h);
        }
        let content = h.finish();
        if let Some(at) = cells.iter().position(|cell| cell.content == content) {
            let cell = cells.remove(at);
            lifted.push(cell.overlay.clone());
            cells.push(cell);
            continue;
        }
        let mut canvas = super::Canvas::over(area.width, area.height, Vec::new());
        for c in below().chain(std::iter::once(top)) {
            c.replay_within(&mut canvas, -area.x, -area.y, area);
        }
        let mut h = super::revision_hasher();
        canvas.pixels.hash(&mut h);
        let overlay = windowing::surface::Overlay {
            rect: native_rect(area),
            pixels: canvas.pixels.into(),
            key: h.finish(),
        };
        lifted.push(overlay.clone());
        cells.push(OverlayCell { content, overlay });
        if cells.len() > KEPT_CELLS {
            cells.remove(0);
        }
    }
    lifted
}
impl WindowRenderer {
    pub fn invalidate(&mut self) {
        self.retained.invalidate();
        self.gpu = None;
        self.retry = 0;
    }
    /// Device loss presents the reference in software before a bounded retry.
    pub fn device_lost(&mut self) {
        self.invalidate();
        self.retry = 60;
    }
    pub fn purge(&mut self) {
        self.retained.purge();
        self.gpu = None;
        self.retry = 60;
    }
    /// Shows a frame `width` x `height` in `window` and answers the regions
    /// it changed. A scene is diffed against the retained one and only its
    /// damage is painted, straight into the window's surface.
    pub fn present(
        &mut self,
        window: windowing::Window,
        width: i64,
        height: i64,
        painted: Painted,
    ) -> Result<Vec<Rect>, errors::Error> {
        let mode = std::env::var("GUI_RENDERER").ok();
        let wants_gpu = gpu_requested(mode.as_deref(), &windowing::video_driver());
        if !wants_gpu {
            self.gpu = None;
            self.retry = 0;
        }
        if let Some(gpu) = &mut self.gpu {
            gpu.trim_layers(self.retained.budgets.layers);
        }
        if self
            .gpu
            .as_ref()
            .is_some_and(|gpu| gpu.size() != (width, height))
        {
            self.gpu = None;
        }
        let mut full = false;
        if self.gpu.is_none() && wants_gpu && self.retry == 0 {
            match windowing::compositor::Compositor::new(window, width, height) {
                Ok(gpu) => {
                    self.gpu = Some(gpu);
                    full = true;
                }
                Err(_) => self.retry = 60,
            }
        }
        let screen = Rect::new(0, 0, width, height);
        let scene = match painted {
            Painted::Scene(scene) => scene,
            Painted::Frame(canvas) => {
                self.retained.purge();
                self.retained.width = width;
                self.retained.height = height;
                let damage = vec![screen];
                self.retained.stats.damage_pixels = damage_pixels(&damage, screen);
                if let Some(gpu) = &mut self.gpu {
                    if gpu.present(&canvas.pixels, &native(&damage), false).is_ok() {
                        self.retained.stats.gpu_layer_bytes = gpu.cache_bytes();
                        return Ok(damage);
                    }
                    self.gpu = None;
                    self.retry = 60;
                }
                convert(window, &canvas, &damage)?;
                self.overlays.clear();
                return Ok(damage);
            }
        };
        self.retained.rings = self.gpu.is_none() && rings_enabled() && window.shows_rings();
        let mut damage = self.retained.damage(width, height, &scene);
        if full {
            damage = vec![screen];
        }
        if damage.is_empty() {
            // Nothing else changed, but a ring may have scrolled or changed
            // rows, or an overlay changed (a cursor blinked or moved), so
            // their layers still need updating.
            if self.retained.rings {
                let overlays = overlays_of(&mut self.cells, &scene);
                if scene.commands.iter().any(|c| ring_hole(c).is_some())
                    || overlays != self.overlays
                {
                    self.software(window, &scene, None, Some(overlays))?;
                }
            }
            self.retained.commit(scene);
            self.retained.stats.gpu_layer_bytes = self.gpu.as_ref().map_or(0, |g| g.cache_bytes());
            return Ok(damage);
        }
        if let Some(gpu) = &mut self.gpu {
            if Self::compose(gpu, &scene, self.retained.budgets.layers, &damage).is_ok() {
                self.retained.stats.damage_pixels = damage_pixels(&damage, screen);
                self.retained.stats.gpu_layer_bytes = gpu.cache_bytes();
                self.retained.commit(scene);
                return Ok(damage);
            }
            // Device loss discards all GPU objects. Software presents this
            // frame in full; a later update retries GPU creation without a loop.
            self.gpu = None;
            self.retry = 60;
            let damage = vec![screen];
            self.software(window, &scene, Some(&damage), None)?;
            self.retained.commit(scene);
            return Ok(damage);
        }
        self.retry = self.retry.saturating_sub(1);
        self.software(window, &scene, Some(&damage), None)?;
        self.retained.commit(scene);
        Ok(damage)
    }
    fn compose(
        gpu: &mut windowing::compositor::Compositor,
        scene: &Scene,
        layers: usize,
        damage: &[Rect],
    ) -> Result<(), windowing::Error> {
        for d in damage {
            gpu.begin_damage(windowing::display::Rect {
                x: d.x,
                y: d.y,
                width: d.width,
                height: d.height,
            })?;
            for command in &scene.commands {
                let clip = command.bounds.intersect(*d);
                if clip.width <= 0 || clip.height <= 0 {
                    continue;
                }
                gpu.clip(windowing::display::Rect {
                    x: clip.x,
                    y: clip.y,
                    width: clip.width,
                    height: clip.height,
                })?;
                if let Primitive::Rect(x, y, w, h, color) = command.paint {
                    let alpha = (color.a as i64 * command.opacity / 255).clamp(0, 255) as u8;
                    gpu.fill(
                        windowing::display::Rect {
                            x,
                            y,
                            width: w,
                            height: h,
                        },
                        [color.r, color.g, color.b, alpha],
                        alpha < 255,
                    )?;
                } else {
                    let bounds = command.bounds;
                    // Each cache entry and temporary raster is at most 64 KiB.
                    for y in (bounds.y..bounds.y + bounds.height).step_by(128) {
                        for x in (bounds.x..bounds.x + bounds.width).step_by(128) {
                            let tile = Rect::new(
                                x,
                                y,
                                128.min(bounds.x + bounds.width - x),
                                128.min(bounds.y + bounds.height - y),
                            );
                            if tile.intersect(clip).width <= 0 || tile.intersect(clip).height <= 0 {
                                continue;
                            }
                            gpu.layer(
                                command.revision(),
                                windowing::display::Rect {
                                    x,
                                    y,
                                    width: tile.width,
                                    height: tile.height,
                                },
                                layers,
                                || command.layer_pixels(tile),
                            )?;
                        }
                    }
                }
            }
        }
        gpu.finish(false)?;
        Ok(())
    }

    // Paints `damage` of `scene` into the window's surface. Where the window
    // supports rings, also shows the scene's rings below the surface and its
    // overlays above it (`overlays` when the caller already has them). With
    // `damage` of `None`, only the rings and overlays are updated.
    fn software(
        &mut self,
        window: windowing::Window,
        scene: &Scene,
        damage: Option<&[Rect]>,
        overlays: Option<Vec<windowing::surface::Overlay>>,
    ) -> Result<(), errors::Error> {
        let shows_rings = self.retained.rings;
        let overlays = match overlays {
            Some(overlays) => overlays,
            None if shows_rings => overlays_of(&mut self.cells, scene),
            None => Vec::new(),
        };
        let retained = &mut self.retained;
        let paint = |pixels: &mut [u8],
                     width: i64,
                     height: i64,
                     stride: i64,
                     format: windowing::surface::PixelFormat| {
            retained.raster(
                scene,
                damage.unwrap_or(&[]),
                Target {
                    pixels,
                    width,
                    height,
                    stride,
                    offsets: format.rgb_offsets(),
                },
            )
        };
        if !shows_rings {
            if let Some(damage) = damage {
                // A plain paint takes down any rings and overlays shown.
                window.paint_surface(&native(damage), paint)?;
                self.overlays.clear();
            }
            return Ok(());
        }
        let found: Vec<(Rect, &super::Surface)> =
            scene.commands.iter().filter_map(ring_hole).collect();
        let held: Vec<_> = found.iter().map(|(_, s)| s.pixels()).collect();
        // The rows each ring changed, relative to the ring's first row.
        let written: Vec<Option<Vec<windowing::display::Rect>>> = found
            .iter()
            .map(|(_, s)| {
                let turn = s.turn();
                let stretch = Rect::new(0, turn.top, s.width(), turn.len);
                s.written().map(|rows| {
                    rows.iter()
                        .map(|r| r.intersect(stretch))
                        .filter(|r| r.width > 0 && r.height > 0)
                        .map(|r| native_rect(r.offset(0, -turn.top)))
                        .collect()
                })
            })
            .collect();
        let rings: Vec<windowing::surface::Ring> = found
            .iter()
            .zip(&held)
            .zip(&written)
            .filter_map(|(((hole, s), pixels), written)| {
                let turn = s.turn();
                let stride = s.width() * 4;
                let rows = pixels
                    .get((turn.top * stride) as usize..((turn.top + turn.len) * stride) as usize)?;
                let (id, previous) = s.identity();
                Some(windowing::surface::Ring {
                    key: s.storage(),
                    rect: native_rect(*hole),
                    pixels: rows,
                    stride,
                    shift: turn.shift,
                    id,
                    previous,
                    written: written.as_deref(),
                })
            })
            .collect();
        let damage = damage.map(native);
        window.paint_surface_rings(damage.as_deref(), &rings, &overlays, paint)?;
        self.overlays = overlays;
        Ok(())
    }
    pub fn gpu_capture(&mut self) -> Result<Option<Vec<u8>>, errors::Error> {
        let Some(gpu) = &mut self.gpu else {
            return Ok(None);
        };
        Ok(gpu.finish(true)?)
    }
    pub fn gpu_name(&self) -> Option<&str> {
        self.gpu.as_ref().map(|g| g.name())
    }
}

#[cfg(test)]
mod tests {
    use super::{Conversion, damage_pixels, gpu_requested, overlays_of};
    use crate::geometry::Rect;
    use crate::render::Canvas;
    use crate::style::color::Color;

    #[test]
    fn an_overlay_shows_its_cell_as_painted_and_renders_again_only_when_it_changes() {
        let scene = |cursor: bool, far: u8| {
            let mut c = Canvas::recording(200, 100, 1024 * 1024);
            c.fill(Color::rgb(30, 30, 30));
            c.rect(20, 20, 60, 20, Color::rgb(200, 40, 40));
            // Something away from the cursor that changes on its own.
            c.rect(150, 70, 10, 10, Color::rgb(far, far, far));
            if cursor {
                c.overlay(30, 22, 8, 16, Color::rgba(255, 255, 0, 160));
            }
            c.take_scene().unwrap()
        };
        let mut cells = Vec::new();
        let lifted = overlays_of(&mut cells, &scene(true, 0));
        assert_eq!(lifted.len(), 1);
        // The cell holds exactly what the whole frame shows there.
        let mut whole = Canvas::new(200, 100);
        scene(true, 0).replay(&mut whole);
        let o = &lifted[0];
        assert_eq!(
            (o.rect.x, o.rect.y, o.rect.width, o.rect.height),
            (30, 22, 8, 16)
        );
        for y in 0..16 {
            let row = &whole.pixels[(((22 + y) * 200 + 30) * 4) as usize..][..8 * 4];
            assert_eq!(&o.pixels[(y * 8 * 4) as usize..][..8 * 4], row);
        }
        // Blinking off and on, or a change elsewhere, renders nothing new.
        assert!(overlays_of(&mut cells, &scene(false, 0)).is_empty());
        let again = overlays_of(&mut cells, &scene(true, 90));
        assert_eq!(again, lifted);
        assert!(std::sync::Arc::ptr_eq(&again[0].pixels, &o.pixels));
        assert_eq!(cells.len(), 1);
    }

    #[test]
    fn damage_pixels_count_the_frame_area_each_region_repaints() {
        let screen = Rect::new(0, 0, 10, 10);
        let damage = [
            Rect::new(1, 1, 1, 18),
            Rect::new(8, 8, 4, 4),
            Rect::new(20, 0, 2, 2),
        ];
        assert_eq!(damage_pixels(&damage, screen), 9 + 4);
    }

    #[test]
    fn damage_converts_into_surface_order_band_by_band() {
        let mut canvas = Canvas::new(6, 5);
        for (i, v) in canvas.pixels.iter_mut().enumerate() {
            *v = i as u8;
        }
        let damage = [Rect::new(1, 0, 2, 3), Rect::new(4, 2, 2, 3)];
        let stride = 6 * 4 + 8;
        let to_native = Conversion {
            canvas: &canvas,
            damage: &damage,
            stride,
            offsets: (2, 1, 0),
        };
        let mut whole = vec![7u8; (stride * 5) as usize];
        to_native.band(0, &mut whole);
        let mut banded = vec![7u8; (stride * 5) as usize];
        for (top, band) in banded.chunks_mut(stride as usize * 2).enumerate() {
            to_native.band(top as i64 * 2, band);
        }
        assert_eq!(whole, banded);
        for y in 0..5i64 {
            for x in 0..6i64 {
                let at = (y * stride + x * 4) as usize;
                let src = ((y * 6 + x) * 4) as usize;
                let damaged = damage.iter().any(|r| {
                    (r.x..r.x + r.width).contains(&x) && (r.y..r.y + r.height).contains(&y)
                });
                let want = if damaged {
                    [
                        canvas.pixels[src + 2],
                        canvas.pixels[src + 1],
                        canvas.pixels[src],
                        canvas.pixels[src + 3],
                    ]
                } else {
                    [7; 4]
                };
                assert_eq!(whole[at..at + 4], want, "{x},{y}");
            }
        }
    }

    #[test]
    fn software_is_the_default_on_every_driver_and_unknown_overrides_fail_closed() {
        for driver in [
            "x11",
            "wayland",
            "windows",
            "cocoa",
            "offscreen",
            "unknown",
            "",
        ] {
            for mode in [
                None,
                Some(""),
                Some("software"),
                Some("Software"),
                Some("typo"),
                Some("software "),
            ] {
                assert!(
                    !gpu_requested(mode, driver),
                    "unexpected GPU: {mode:?} {driver}"
                );
            }
            assert!(gpu_requested(Some("gpu"), driver));
            assert_eq!(
                gpu_requested(Some("auto"), driver),
                matches!(driver, "x11" | "wayland" | "windows" | "cocoa")
            );
        }
    }
}
