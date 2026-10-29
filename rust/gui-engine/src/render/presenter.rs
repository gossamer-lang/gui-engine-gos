//! GPU composition with software fallback, sharing the exact raster reference.
use super::retained::{Primitive, Retained, Scene, Target};
use crate::{errors, geometry::Rect};

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
                    d[3] = 255;
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
        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
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
                return Ok(damage);
            }
        };
        let mut damage = self.retained.damage(width, height, &scene);
        if full {
            damage = vec![screen];
        }
        if damage.is_empty() {
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
            self.software(window, &scene, &damage)?;
            self.retained.commit(scene);
            return Ok(damage);
        }
        self.retry = self.retry.saturating_sub(1);
        self.software(window, &scene, &damage)?;
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

    // Paints `damage` of `scene` straight into the window's surface.
    fn software(
        &mut self,
        window: windowing::Window,
        scene: &Scene,
        damage: &[Rect],
    ) -> Result<(), errors::Error> {
        let retained = &mut self.retained;
        window.paint_surface(&native(damage), |pixels, width, height, stride, format| {
            retained.raster(
                scene,
                damage,
                Target {
                    pixels,
                    width,
                    height,
                    stride,
                    offsets: format.rgb_offsets(),
                },
            )
        })?;
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
    use super::{Conversion, damage_pixels, gpu_requested};
    use crate::geometry::Rect;
    use crate::render::Canvas;

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
                        255,
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
