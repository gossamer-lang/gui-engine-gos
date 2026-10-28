//! Image viewing: PNG, JPEG, GIF (first frame) and SVG decoded off the UI
//! thread with cancellation, EXIF orientation applied to JPEGs, and a
//! byte-bounded cache of decoded images keyed by file identity, so a
//! replaced file decodes afresh. The viewer widget fits, shows actual size,
//! zooms, pans and rotates.
//!
//! A viewer is a session-owned handle. Loading a new image supersedes the
//! one in flight, whose result is then discarded.

pub mod api;
pub mod widget;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread::JoinHandle;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::memory::{Budget, ByteCache, Lease};
use crate::render::Pixels;
use crate::widgets::cx::UiMsg;

const HANDLE_LIMIT: usize = 256;
/// The largest image decoded, in pixels.
pub const PIXEL_LIMIT: usize = 64 * 1024 * 1024;
const FILE_LIMIT: usize = 128 * 1024 * 1024;
const CACHE_BYTES: usize = 256 * 1024 * 1024;
const PREVIEW_SIDE: i64 = 2048;
const SVG_SIDE: i64 = 2048;

/// The formats the viewer decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    Svg,
}

/// Decoded straight RGBA pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub width: i64,
    pub height: i64,
    pub rgba: Pixels,
    pub format: Format,
}

/// What a viewer shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum State {
    Empty,
    Loading(String),
    Ready {
        width: i64,
        height: i64,
        format: Format,
    },
    Failed(String),
    Cancelled,
}

/// How the image is scaled into the view.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Zoom {
    /// Shrunk to fit the view, never enlarged.
    Fit,
    /// Image pixels per device pixel.
    Scale(f64),
}

/// A viewer's state as the application reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    pub path: String,
    pub zoom: Zoom,
    /// Quarter turns clockwise the user added.
    pub turns: i64,
}

fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(Format::Gif)
    } else {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
        (head.contains("<svg") || head.trim_start().starts_with("<?xml")).then_some(Format::Svg)
    }
}

/// The orientation (1..=8) an EXIF block records, or 1.
fn exif_orientation(exif: &[u8]) -> u16 {
    let tiff = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif);
    if tiff.len() < 8 {
        return 1;
    }
    let little = &tiff[..2] == b"II";
    let u16_at = |at: usize| -> Option<u16> {
        let b = tiff.get(at..at + 2)?;
        Some(if little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b = tiff.get(at..at + 4)?;
        let b = [b[0], b[1], b[2], b[3]];
        Some(if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    let Some(ifd) = u32_at(4).map(|o| o as usize) else {
        return 1;
    };
    let count = u16_at(ifd).unwrap_or(0) as usize;
    (0..count.min(512))
        .find_map(|i| {
            let entry = ifd + 2 + i * 12;
            (u16_at(entry)? == 0x0112)
                .then(|| u16_at(entry + 8))
                .flatten()
        })
        .filter(|o| (1..=8).contains(o))
        .unwrap_or(1)
}

/// The image turned and mirrored as EXIF `orientation` says it was taken.
fn orient(image: Image, orientation: u16) -> Image {
    if orientation <= 1 {
        return image;
    }
    let (w, h) = (image.width as usize, image.height as usize);
    let transposed = orientation >= 5;
    let (ow, oh) = if transposed { (h, w) } else { (w, h) };
    let mut out = vec![0u8; image.rgba.len()];
    for y in 0..oh {
        for x in 0..ow {
            // The source pixel each output pixel takes, by orientation.
            let (sx, sy) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                _ => (w - 1 - y, x),
            };
            let (to, from) = ((y * ow + x) * 4, (sy * w + sx) * 4);
            out[to..to + 4].copy_from_slice(&image.rgba[from..from + 4]);
        }
    }
    Image {
        width: ow as i64,
        height: oh as i64,
        rgba: Pixels::new(out),
        format: image.format,
    }
}

/// The image turned clockwise by `turns` quarter turns.
pub fn rotate(image: &Image, turns: i64) -> Image {
    let orientation = match turns.rem_euclid(4) {
        1 => 6,
        2 => 3,
        3 => 8,
        _ => 1,
    };
    orient(image.clone(), orientation)
}

fn checked(width: i64, height: i64) -> Result<(), String> {
    if width <= 0 || height <= 0 {
        return Err("the image has no pixels".into());
    }
    if (width as usize).saturating_mul(height as usize) > PIXEL_LIMIT {
        return Err(format!(
            "the image is {width} x {height}, beyond the viewer's {} megapixels",
            PIXEL_LIMIT / (1024 * 1024)
        ));
    }
    Ok(())
}

/// Decodes `bytes`, refusing images beyond [`PIXEL_LIMIT`].
pub fn decode(bytes: &[u8], cancelled: &dyn Fn() -> bool) -> Result<Image, String> {
    let format = sniff(bytes).ok_or("the file is not a PNG, JPEG, GIF or SVG image")?;
    let image = match format {
        Format::Png => {
            let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
            let reader = decoder.read_info().map_err(|e| format!("PNG: {e}"))?;
            let info = reader.info();
            checked(i64::from(info.width), i64::from(info.height))?;
            crate::render::png::decode_with_limit(bytes, PIXEL_LIMIT * 4)
                .map(|i| Image {
                    width: i.width,
                    height: i.height,
                    rgba: Pixels::new(i.rgba),
                    format,
                })
                .ok_or("PNG: the image data is damaged")?
        }
        Format::Jpeg => {
            use zune_core::bytestream::ZCursor;
            use zune_core::colorspace::ColorSpace;
            use zune_core::options::DecoderOptions;
            let options = DecoderOptions::default()
                .jpeg_set_out_colorspace(ColorSpace::RGBA)
                .set_max_width(16_384)
                .set_max_height(16_384);
            let mut decoder =
                zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
            decoder
                .decode_headers()
                .map_err(|e| format!("JPEG: {e:?}"))?;
            let info = decoder.info().ok_or("JPEG: no image header")?;
            checked(i64::from(info.width), i64::from(info.height))?;
            let orientation = decoder.exif().map_or(1, |exif| exif_orientation(exif));
            if cancelled() {
                return Err("decoding cancelled".into());
            }
            let rgba = decoder.decode().map_err(|e| format!("JPEG: {e:?}"))?;
            let image = Image {
                width: i64::from(info.width),
                height: i64::from(info.height),
                rgba: Pixels::new(rgba),
                format,
            };
            if image.rgba.len() != (image.width * image.height * 4) as usize {
                return Err("JPEG: the decoded pixels do not match the image size".into());
            }
            orient(image, orientation)
        }
        Format::Gif => {
            let mut options = gif::DecodeOptions::new();
            options.set_color_output(gif::ColorOutput::RGBA);
            let mut decoder = options
                .read_info(std::io::Cursor::new(bytes))
                .map_err(|e| format!("GIF: {e}"))?;
            let (w, h) = (i64::from(decoder.width()), i64::from(decoder.height()));
            checked(w, h)?;
            let frame = decoder
                .read_next_frame()
                .map_err(|e| format!("GIF: {e}"))?
                .ok_or("GIF: no frames")?;
            // The first frame is drawn into a canvas of the image's size.
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            for row in 0..i64::from(frame.height) {
                let y = i64::from(frame.top) + row;
                if y >= h {
                    break;
                }
                for col in 0..i64::from(frame.width) {
                    let x = i64::from(frame.left) + col;
                    if x >= w {
                        break;
                    }
                    let from = ((row * i64::from(frame.width) + col) * 4) as usize;
                    let to = ((y * w + x) * 4) as usize;
                    rgba[to..to + 4].copy_from_slice(&frame.buffer[from..from + 4]);
                }
            }
            Image {
                width: w,
                height: h,
                rgba: Pixels::new(rgba),
                format,
            }
        }
        Format::Svg => {
            let text = std::str::from_utf8(bytes).map_err(|_| "SVG: the file is not UTF-8 text")?;
            let picture =
                crate::render::svg::rasterize_cancellable(text, SVG_SIDE, &|| !cancelled())
                    .map_err(|e| format!("SVG: {e}"))?;
            Image {
                width: picture.width,
                height: picture.height,
                rgba: Pixels::new(picture.rgba),
                format,
            }
        }
    };
    if cancelled() {
        return Err("decoding cancelled".into());
    }
    Ok(image)
}

/// A copy no larger than `side` on either edge, for drawing a large image
/// small without resampling all of it every frame.
pub fn preview(image: &Image, side: i64) -> Option<Image> {
    if image.width <= side && image.height <= side {
        return None;
    }
    let scale = side as f64 / image.width.max(image.height) as f64;
    let (w, h) = (
        ((image.width as f64 * scale).round() as i64).max(1),
        ((image.height as f64 * scale).round() as i64).max(1),
    );
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    // Box filter: each output pixel averages the source pixels it covers.
    for y in 0..h {
        let (y0, y1) = (
            (y * image.height / h),
            ((y + 1) * image.height / h).max(y * image.height / h + 1),
        );
        for x in 0..w {
            let (x0, x1) = (
                (x * image.width / w),
                ((x + 1) * image.width / w).max(x * image.width / w + 1),
            );
            let mut sum = [0u64; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = ((sy * image.width + sx) * 4) as usize;
                    for (total, value) in sum.iter_mut().zip(&image.rgba[p..p + 4]) {
                        *total += u64::from(*value);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            let o = ((y * w + x) * 4) as usize;
            for (out, total) in rgba[o..o + 4].iter_mut().zip(sum) {
                *out = (total / n) as u8;
            }
        }
    }
    Some(Image {
        width: w,
        height: h,
        rgba: Pixels::new(rgba),
        format: image.format,
    })
}

/// A file's identity: its path, length and modification time.
type Identity = (String, u64, i128);

static CACHE: LazyLock<Mutex<ByteCache<Identity, Arc<Image>>>> =
    LazyLock::new(|| Mutex::new(ByteCache::new(CACHE_BYTES)));

fn identity(path: &str) -> Result<Identity, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128);
    Ok((path.to_string(), meta.len(), modified))
}

/// The decoded-image cache's bytes and entries.
pub fn cache_usage() -> (usize, usize) {
    let cache = CACHE.lock();
    (cache.bytes(), cache.len())
}

struct Entry {
    state: State,
    path: String,
    image: Option<Arc<Image>>,
    preview: Option<Arc<Image>>,
    zoom: Zoom,
    /// The view's center in image pixels; None centers the image.
    center: Option<(f64, f64)>,
    turns: i64,
    generation: u64,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    budget: Budget,
    lease: Option<Lease>,
    waker: Option<windowing::Waker>,
    changed: bool,
    reported: bool,
}

static NEXT: AtomicI64 = AtomicI64::new(1);
static VIEWERS: LazyLock<Mutex<HashMap<i64, Entry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn with<T>(id: i64, f: impl FnOnce(&mut Entry) -> Result<T, String>) -> Result<T, String> {
    let mut viewers = VIEWERS.lock();
    let entry = viewers.get_mut(&id).ok_or("the image viewer is closed")?;
    f(entry)
}

fn status(entry: &Entry) -> Status {
    Status {
        state: entry.state.clone(),
        path: entry.path.clone(),
        zoom: entry.zoom,
        turns: entry.turns,
    }
}

/// Opens an empty viewer in the running session.
pub fn open(budget: Budget) -> Result<i64, String> {
    let mut viewers = VIEWERS.lock();
    if viewers.len() >= HANDLE_LIMIT {
        return Err("the image viewer registry is full".into());
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    viewers.insert(
        id,
        Entry {
            state: State::Empty,
            path: String::new(),
            image: None,
            preview: None,
            zoom: Zoom::Fit,
            center: None,
            turns: 0,
            generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            worker: None,
            budget,
            lease: None,
            waker: crate::app::effects::current_waker(),
            changed: false,
            reported: true,
        },
    );
    Ok(id)
}

/// Stores a finished decode unless a newer load superseded it.
fn finish(id: i64, generation: u64, result: Result<Arc<Image>, String>, cancelled: bool) {
    let mut viewers = VIEWERS.lock();
    let Some(entry) = viewers.get_mut(&id) else {
        return;
    };
    if entry.generation != generation {
        return;
    }
    entry.state = match result {
        Ok(image) => {
            let turned = if entry.turns.rem_euclid(4) == 0 {
                image
            } else {
                Arc::new(rotate(&image, entry.turns))
            };
            let preview = preview(&turned, PREVIEW_SIDE).map(Arc::new);
            let bytes = turned.rgba.len() + preview.as_ref().map_or(0, |p| p.rgba.len()) + 4096;
            match entry.budget.reserve(bytes) {
                Ok(lease) => {
                    entry.lease = Some(lease);
                    let state = State::Ready {
                        width: turned.width,
                        height: turned.height,
                        format: turned.format,
                    };
                    entry.image = Some(turned);
                    entry.preview = preview;
                    state
                }
                Err(error) => State::Failed(error),
            }
        }
        Err(_) if cancelled => State::Cancelled,
        Err(error) => State::Failed(error),
    };
    entry.changed = true;
    entry.reported = false;
    if let Some(waker) = &entry.waker {
        let _ = waker.wake();
    }
}

/// Starts decoding `path` in the background; a load in flight is
/// superseded. Answers at once.
pub fn load(id: i64, path: &str) -> Result<(), String> {
    if path.is_empty() || path.contains('\0') {
        return Err("the image needs a file path".into());
    }
    let (generation, cancel) = with(id, |entry| {
        entry.cancel.store(true, Ordering::Release);
        entry.generation += 1;
        entry.cancel = Arc::new(AtomicBool::new(false));
        entry.state = State::Loading(path.to_string());
        entry.path = path.to_string();
        entry.image = None;
        entry.preview = None;
        entry.lease = None;
        entry.center = None;
        entry.zoom = Zoom::Fit;
        entry.changed = true;
        entry.reported = false;
        Ok((entry.generation, entry.cancel.clone()))
    })?;
    let file = path.to_string();
    let worker = std::thread::Builder::new()
        .name("gui-image-decode".into())
        .spawn(move || {
            let stop = || cancel.load(Ordering::Acquire);
            let result = identity(&file).and_then(|key| {
                if let Some(hit) = CACHE.lock().get(&key).cloned() {
                    return Ok(hit);
                }
                let bytes = crate::services::bounded_io::read(
                    std::path::Path::new(&file),
                    FILE_LIMIT,
                    &|| !stop(),
                )?;
                let image = Arc::new(decode(&bytes, &stop)?);
                // A file replaced while it decoded is cached under its new
                // identity by the load that reads it.
                if identity(&file).ok().as_ref() == Some(&key) {
                    let size = image.rgba.len() + file.len();
                    CACHE.lock().insert(key, image.clone(), size);
                }
                Ok(image)
            });
            finish(id, generation, result, stop());
        })
        .map_err(|e| e.to_string())?;
    let previous = with(id, |entry| Ok(entry.worker.replace(worker)))?;
    // The superseded decode sees its cancellation and ends on its own.
    drop(previous);
    Ok(())
}

/// Loads the file again when it changed since it was shown.
pub fn reload(id: i64) -> Result<bool, String> {
    let path = with(id, |entry| Ok(entry.path.clone()))?;
    if path.is_empty() {
        return Ok(false);
    }
    load(id, &path)?;
    Ok(true)
}

/// Stops a load in flight; the viewer then shows the load as cancelled.
pub fn cancel(id: i64) -> Result<(), String> {
    with(id, |entry| {
        entry.cancel.store(true, Ordering::Release);
        Ok(())
    })
}

pub fn set_zoom(id: i64, zoom: Zoom) -> Result<(), String> {
    if let Zoom::Scale(s) = zoom
        && !(s.is_finite() && (0.01..=64.0).contains(&s))
    {
        return Err("zoom must be 1% to 6400%".into());
    }
    with(id, |entry| {
        entry.zoom = zoom;
        if zoom == Zoom::Fit {
            entry.center = None;
        }
        entry.changed = true;
        Ok(())
    })
}

/// Turns the image clockwise by `turns` quarter turns.
pub fn turn(id: i64, turns: i64) -> Result<(), String> {
    with(id, |entry| {
        entry.turns = (entry.turns + turns).rem_euclid(4);
        if let Some(image) = &entry.image {
            let turned = Arc::new(rotate(image, turns));
            entry.preview = preview(&turned, PREVIEW_SIDE).map(Arc::new);
            entry.state = State::Ready {
                width: turned.width,
                height: turned.height,
                format: turned.format,
            };
            entry.image = Some(turned);
            entry.center = None;
        }
        entry.changed = true;
        Ok(())
    })
}

pub fn status_of(id: i64) -> Result<Status, String> {
    with(id, |entry| Ok(status(entry)))
}

/// Reads the viewer's image and view in place, as drawing does.
pub(crate) fn view_of<T>(
    id: i64,
    f: impl FnOnce(&Option<Arc<Image>>, &Option<Arc<Image>>, Zoom, Option<(f64, f64)>, &State) -> T,
) -> Result<T, String> {
    with(id, |entry| {
        Ok(f(
            &entry.image,
            &entry.preview,
            entry.zoom,
            entry.center,
            &entry.state,
        ))
    })
}

pub(crate) fn set_view(id: i64, zoom: Zoom, center: Option<(f64, f64)>) -> Result<(), String> {
    with(id, |entry| {
        entry.zoom = zoom;
        entry.center = center;
        entry.changed = true;
        Ok(())
    })
}

/// Releases a viewer, cancelling and joining its decode.
pub fn close(id: i64) {
    let entry = VIEWERS.lock().remove(&id);
    if let Some(mut entry) = entry {
        entry.cancel.store(true, Ordering::Release);
        if let Some(worker) = entry.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(crate) fn close_budget(budget: &Budget) {
    let ids: Vec<i64> = VIEWERS
        .lock()
        .iter()
        .filter(|(_, e)| e.budget == *budget)
        .map(|(id, _)| *id)
        .collect();
    for id in ids {
        close(id);
    }
}

/// Whether the session's viewers changed, and the load outcomes its
/// application hears about.
pub(crate) fn pump(budget: &Budget) -> (bool, Vec<UiMsg>) {
    let mut changed = false;
    let mut events = Vec::new();
    for (id, entry) in VIEWERS
        .lock()
        .iter_mut()
        .filter(|(_, e)| e.budget == *budget)
    {
        changed |= std::mem::take(&mut entry.changed);
        if !entry.reported && !matches!(entry.state, State::Loading(_)) {
            entry.reported = true;
            events.push(UiMsg::Image(*id, status(entry)));
        }
    }
    (changed, events)
}

#[cfg(test)]
mod tests;
