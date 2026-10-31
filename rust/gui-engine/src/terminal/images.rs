//! Painting the images a terminal's cells show: each run of cells showing
//! one image in a row draws that image scaled to its whole extent, clipped to
//! the run, so the rows of an image meet without seams.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use wezterm_term::image::{ImageData, ImageDataType};

use super::frame::Piece;
use crate::geometry::Rect;
use crate::render::{Canvas, Pixels};

/// Frames an animation asks to show for less than this many milliseconds
/// show for `SHORT_FRAME_MS`, as browsers do for GIFs that ask for none.
const SHORTEST_FRAME_MS: i64 = 10;
const SHORT_FRAME_MS: i64 = 100;

/// The pixels a paint draws images from, kept between paints.
#[derive(Default)]
pub(crate) struct Pictures {
    /// Pixels by the address of the frame they share with the terminal core.
    pub(crate) pixels: HashMap<usize, Pixels>,
    /// When each animated image began playing, by content.
    pub(crate) animations: HashMap<[u8; 32], i64>,
}

/// Where a paint puts the screen: the canvas position of its first cell, the
/// size a cell is drawn at, and the cell size the terminal core placed
/// images with.
#[derive(Clone, Copy)]
pub(crate) struct Grid {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) cell_width: f64,
    pub(crate) cell_height: f64,
    pub(crate) placed: (u16, u16),
}

/// Draws the pieces whose z order puts them below the text (`below`) or
/// above it, at app time `now`; answers when an animated one next changes
/// frame.
pub(crate) fn paint(
    canvas: &mut Canvas,
    pieces: &[Piece],
    below: bool,
    grid: Grid,
    pictures: &mut Pictures,
    used: &mut Used,
    now: i64,
) -> Option<i64> {
    if grid.placed.0 == 0 || grid.placed.1 == 0 {
        return None;
    }
    let mut shown: Vec<&Piece> = pieces.iter().filter(|p| (p.z < 0) == below).collect();
    // A stable sort keeps each layer in reading order.
    shown.sort_by_key(|p| p.z);
    let mut due: Option<i64> = None;
    let mut at = 0;
    while at < shown.len() {
        let first = shown[at];
        let mut last = first;
        let mut end = at + 1;
        while end < shown.len() && continues(last, shown[end]) {
            last = shown[end];
            end += 1;
        }
        if let Some(picture) = picture(&first.image, pictures, used, now) {
            draw(canvas, first, last, grid, &picture);
            due = earlier(due, picture.due);
        }
        at = end;
    }
    due
}

/// What a paint drew from, so the pictures it no longer shows are let go.
#[derive(Default)]
pub(crate) struct Used {
    pixels: HashSet<usize>,
    animations: HashSet<[u8; 32]>,
}

impl Pictures {
    /// Keeps only what the last paint drew.
    pub(crate) fn keep(&mut self, used: &Used) {
        self.pixels.retain(|key, _| used.pixels.contains(key));
        self.animations
            .retain(|key, _| used.animations.contains(key));
    }
}

// Whether `next` is the cell after `previous` in a row, showing the next
// stretch of the same image.
fn continues(previous: &Piece, next: &Piece) -> bool {
    next.row == previous.row
        && next.col == previous.col + 1
        && next.z == previous.z
        && Arc::ptr_eq(&next.image, &previous.image)
        && (next.top_left.0 - previous.bottom_right.0).abs() < 1e-4
        && next.top_left.1 == previous.top_left.1
}

fn earlier(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

struct Picture {
    pixels: Pixels,
    width: i64,
    height: i64,
    due: Option<i64>,
}

// The frame of `image` to show at `now`. An image the terminal core could
// not decode draws nothing.
fn picture(
    image: &ImageData,
    pictures: &mut Pictures,
    used: &mut Used,
    now: i64,
) -> Option<Picture> {
    let data = image.data();
    let (frame, width, height, due) = match &*data {
        ImageDataType::Rgba8 {
            data,
            width,
            height,
            ..
        } => (data, *width, *height, None),
        ImageDataType::AnimRgba8 {
            frames,
            durations,
            width,
            height,
            ..
        } => {
            let start = *pictures.animations.entry(image.hash()).or_insert(now);
            used.animations.insert(image.hash());
            let (index, due) = frame_at(durations, start, now);
            (frames.get(index)?, *width, *height, due)
        }
        _ => return None,
    };
    let key = Arc::as_ptr(frame).cast::<u8>() as usize;
    used.pixels.insert(key);
    let pixels = pictures
        .pixels
        .entry(key)
        .or_insert_with(|| Pixels::new(Arc::clone(frame)))
        .clone();
    Some(Picture {
        pixels,
        width: i64::from(width),
        height: i64::from(height),
        due,
    })
}

/// The frame an animation of `durations` shows at `now` when it began at
/// `start`, and when that frame gives way to the next.
fn frame_at(durations: &[Duration], start: i64, now: i64) -> (usize, Option<i64>) {
    let lengths: Vec<i64> = durations
        .iter()
        .map(|d| match i64::try_from(d.as_millis()).unwrap_or(i64::MAX) {
            ms if ms < SHORTEST_FRAME_MS => SHORT_FRAME_MS,
            ms => ms,
        })
        .collect();
    let total: i64 = lengths.iter().fold(0, |sum, ms| sum.saturating_add(*ms));
    if lengths.len() < 2 || total <= 0 {
        return (0, None);
    }
    let elapsed = (now - start).max(0);
    let mut into = elapsed % total;
    let mut ends = now - into;
    for (index, ms) in lengths.iter().enumerate() {
        ends += ms;
        if into < *ms {
            return (index, Some(ends));
        }
        into -= ms;
    }
    (lengths.len() - 1, Some(ends))
}

// Draws the stretch of the image from `first` to `last`, two cells of one
// row: the whole image scaled to the extent the first cell implies, clipped
// to the cells.
fn draw(canvas: &mut Canvas, first: &Piece, last: &Piece, grid: Grid, picture: &Picture) {
    let across = f64::from(first.bottom_right.0 - first.top_left.0);
    let down = f64::from(first.bottom_right.1 - first.top_left.1);
    if across <= 0.0 || down <= 0.0 {
        return;
    }
    // Padding is in the cell size the image was placed with.
    let scale_x = grid.cell_width / f64::from(grid.placed.0);
    let scale_y = grid.cell_height / f64::from(grid.placed.1);
    let (left, top, right, bottom) = first.padding;
    let cell_x = grid.x + f64::from(first.col) * grid.cell_width;
    let cell_y = grid.y + f64::from(first.row) * grid.cell_height;
    let inner_x = cell_x + f64::from(left) * scale_x;
    let inner_y = cell_y + f64::from(top) * scale_y;
    let inner_width = grid.cell_width - f64::from(left + right) * scale_x;
    let inner_height = grid.cell_height - f64::from(top + bottom) * scale_y;
    let width = inner_width / across;
    let height = inner_height / down;
    let x = inner_x - f64::from(first.top_left.0) * width;
    let y = inner_y - f64::from(first.top_left.1) * height;
    let run_right = grid.x + f64::from(last.col + 1) * grid.cell_width;
    let clip_left = cell_x.max(x);
    let clip_top = cell_y.max(y);
    let clip_right = run_right.min(x + width);
    let clip_bottom = (cell_y + grid.cell_height).min(y + height);
    if clip_right <= clip_left || clip_bottom <= clip_top {
        return;
    }
    let saved = canvas.clip;
    canvas.push_clip(Rect::new(
        clip_left.round() as i64,
        clip_top.round() as i64,
        (clip_right.round() - clip_left.round()) as i64,
        (clip_bottom.round() - clip_top.round()) as i64,
    ));
    canvas.pixels_scaled(
        x.round() as i64,
        y.round() as i64,
        width.round() as i64,
        height.round() as i64,
        &picture.pixels,
        picture.width,
        picture.height,
    );
    canvas.pop_clip(saved);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn an_animation_shows_each_frame_for_its_duration_and_loops() {
        let durations = [ms(100), ms(50), ms(200)];
        assert_eq!(frame_at(&durations, 1000, 1000), (0, Some(1100)));
        assert_eq!(frame_at(&durations, 1000, 1120), (1, Some(1150)));
        assert_eq!(frame_at(&durations, 1000, 1300), (2, Some(1350)));
        assert_eq!(frame_at(&durations, 1000, 1360), (0, Some(1450)));
    }

    #[test]
    fn frames_that_ask_for_no_time_show_for_a_tenth_of_a_second() {
        assert_eq!(frame_at(&[ms(0), ms(0)], 0, 150), (1, Some(200)));
        assert_eq!(frame_at(&[ms(40)], 0, 150), (0, None));
    }
}
