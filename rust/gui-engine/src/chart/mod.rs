//! Charts: series of points on scaled axes, drawn with legends, hit testing,
//! pan and zoom, an accessible data summary and keyboard reading of values.
//!
//! A chart is a session-owned handle. Series may grow from any thread (a
//! live feed appends as data arrives); each series keeps at most its point
//! cap, dropping its oldest points, and the session repaints after appends.
//! [`render`] produces drawing operations, so the widget, PNG export and the
//! printing path draw the same picture.

pub mod api;
pub mod render;
pub mod widget;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::memory::{Budget, Lease};
use crate::style::color::Color;

const HANDLE_LIMIT: usize = 256;
const SERIES_LIMIT: usize = 64;
/// The most points one series keeps.
pub const POINT_LIMIT: usize = 2_000_000;
const POINT_BYTES: usize = 16;

/// How values map to an axis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Scale {
    Linear,
    /// Base-10 logarithmic; only positive values show.
    Log,
    /// Seconds since 1970-01-01 UTC, labelled as dates and times.
    Time,
    /// Whole-number positions naming these categories.
    Category(Vec<String>),
}

/// One axis of a chart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Axis {
    pub label: String,
    pub scale: Scale,
}

impl Default for Axis {
    fn default() -> Self {
        Self {
            label: String::new(),
            scale: Scale::Linear,
        }
    }
}

/// How a series draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Line,
    Area,
    Bar,
    Scatter,
}

/// A series' appearance and point cap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeriesStyle {
    pub name: String,
    pub kind: Kind,
    /// None picks the palette color for the series' position.
    pub color: Option<Color>,
    /// The most points kept; appends beyond it drop the oldest.
    pub capacity: i64,
}

impl Default for SeriesStyle {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: Kind::Line,
            color: None,
            capacity: 100_000,
        }
    }
}

/// A chart's title, axes and legend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spec {
    pub title: String,
    pub x: Axis,
    pub y: Axis,
    pub legend: bool,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            title: String::new(),
            x: Axis::default(),
            y: Axis::default(),
            legend: true,
        }
    }
}

/// The visible ranges; none fits the data.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub x0: f64,
    pub x1: f64,
    pub y0: f64,
    pub y1: f64,
}

/// A series' data and appearance.
#[derive(Debug, Clone)]
pub struct Series {
    pub style: SeriesStyle,
    pub points: VecDeque<(f64, f64)>,
    pub hidden: bool,
    /// Whether x never decreases, so lookups by x can bisect.
    pub sorted: bool,
}

fn nondecreasing<'a>(points: impl Iterator<Item = &'a (f64, f64)>) -> bool {
    let mut last = f64::NEG_INFINITY;
    points.into_iter().all(|(x, _)| {
        let ok = *x >= last;
        last = *x;
        ok
    })
}

/// A point the pointer or keyboard picked out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub series: i64,
    pub name: String,
    pub index: i64,
    pub x: f64,
    pub y: f64,
}

/// A chart's state, shared by the widget, exports and appends.
#[derive(Debug, Clone)]
pub struct Model {
    pub spec: Spec,
    pub series: Vec<Series>,
    pub window: Option<Window>,
    /// The point the keyboard reads: (series, index).
    pub cursor: Option<(usize, usize)>,
    pub hover: Option<Hit>,
    pub revision: u64,
}

struct Entry {
    model: Model,
    budget: Budget,
    lease: Lease,
    waker: Option<windowing::Waker>,
    changed: Arc<AtomicBool>,
}

static NEXT: AtomicI64 = AtomicI64::new(1);
static CHARTS: LazyLock<Mutex<HashMap<i64, Entry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn bytes(model: &Model) -> usize {
    model
        .series
        .iter()
        .map(|s| s.style.capacity.max(0) as usize * POINT_BYTES + s.style.name.len() + 256)
        .sum::<usize>()
        + crate::memory::measure(&model.spec)
        + 4096
}

fn with<T>(id: i64, f: impl FnOnce(&mut Entry) -> Result<T, String>) -> Result<T, String> {
    let mut charts = CHARTS.lock();
    let entry = charts.get_mut(&id).ok_or("the chart is closed")?;
    f(entry)
}

/// Marks a chart changed and wakes its session to repaint.
fn touched(entry: &mut Entry) {
    entry.model.revision = entry.model.revision.wrapping_add(1);
    if !entry.changed.swap(true, Ordering::AcqRel)
        && let Some(waker) = &entry.waker
    {
        let _ = waker.wake();
    }
}

/// Opens a chart in the running session.
pub fn create(spec: Spec, budget: Budget) -> Result<i64, String> {
    if let Scale::Category(names) = &spec.x.scale
        && names.len() > 100_000
    {
        return Err("too many categories".into());
    }
    let model = Model {
        spec,
        series: Vec::new(),
        window: None,
        cursor: None,
        hover: None,
        revision: 0,
    };
    let lease = budget.reserve(bytes(&model))?;
    let mut charts = CHARTS.lock();
    if charts.len() >= HANDLE_LIMIT {
        return Err("the chart registry is full".into());
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    charts.insert(
        id,
        Entry {
            model,
            budget,
            lease,
            waker: crate::app::effects::current_waker(),
            changed: Arc::new(AtomicBool::new(false)),
        },
    );
    Ok(id)
}

fn finite(points: &[(f64, f64)]) -> Result<(), String> {
    if points.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
        return Err("chart points must be finite".into());
    }
    Ok(())
}

/// Adds a series, answering its index.
pub fn add_series(id: i64, style: SeriesStyle, points: &[(f64, f64)]) -> Result<i64, String> {
    finite(points)?;
    if !(1..=POINT_LIMIT as i64).contains(&style.capacity) {
        return Err(format!("a series keeps 1 to {POINT_LIMIT} points"));
    }
    with(id, |entry| {
        if entry.model.series.len() >= SERIES_LIMIT {
            return Err("the chart holds its most series".into());
        }
        let capacity = style.capacity as usize;
        let mut model = entry.model.clone();
        let start = points.len().saturating_sub(capacity);
        model.series.push(Series {
            style,
            points: points[start..].iter().copied().collect(),
            hidden: false,
            sorted: nondecreasing(points[start..].iter()),
        });
        entry.lease.resize(bytes(&model))?;
        entry.model.series = model.series;
        touched(entry);
        Ok(entry.model.series.len() as i64 - 1)
    })
}

/// Replaces a series' points.
pub fn set_points(id: i64, series: i64, points: &[(f64, f64)]) -> Result<(), String> {
    finite(points)?;
    with(id, |entry| {
        let s = entry
            .model
            .series
            .get_mut(series as usize)
            .ok_or("no such series")?;
        let start = points.len().saturating_sub(s.style.capacity as usize);
        s.points = points[start..].iter().copied().collect();
        s.sorted = nondecreasing(s.points.iter());
        entry.model.cursor = None;
        touched(entry);
        Ok(())
    })
}

/// Appends points to a series from any thread; the oldest beyond its cap
/// are dropped.
pub fn append(id: i64, series: i64, points: &[(f64, f64)]) -> Result<(), String> {
    finite(points)?;
    with(id, |entry| {
        let s = entry
            .model
            .series
            .get_mut(series as usize)
            .ok_or("no such series")?;
        let capacity = s.style.capacity as usize;
        let start = points.len().saturating_sub(capacity);
        for point in &points[start..] {
            if s.points.len() == capacity {
                s.points.pop_front();
            }
            if s.points.back().is_some_and(|last| point.0 < last.0) {
                s.sorted = false;
            }
            s.points.push_back(*point);
        }
        touched(entry);
        Ok(())
    })
}

/// Shows or hides a series, as clicking its legend entry does.
pub fn set_hidden(id: i64, series: i64, hidden: bool) -> Result<(), String> {
    with(id, |entry| {
        let s = entry
            .model
            .series
            .get_mut(series as usize)
            .ok_or("no such series")?;
        s.hidden = hidden;
        touched(entry);
        Ok(())
    })
}

/// Sets the visible ranges; none fits the data again.
pub fn set_window(id: i64, window: Option<Window>) -> Result<(), String> {
    if let Some(w) = window
        && !(w.x0.is_finite()
            && w.x1.is_finite()
            && w.y0.is_finite()
            && w.y1.is_finite()
            && w.x1 > w.x0
            && w.y1 > w.y0)
    {
        return Err("a chart window needs finite, increasing ranges".into());
    }
    with(id, |entry| {
        entry.model.window = window;
        touched(entry);
        Ok(())
    })
}

/// The visible ranges.
pub fn window(id: i64) -> Result<Window, String> {
    with(id, |entry| Ok(render::ranges(&entry.model)))
}

/// Reads the chart's state in place, as drawing does.
pub fn read<T>(id: i64, f: impl FnOnce(&Model) -> T) -> Result<T, String> {
    with(id, |entry| Ok(f(&entry.model)))
}

/// Changes the chart's state and repaints it.
fn change<T>(id: i64, f: impl FnOnce(&mut Model) -> T) -> Option<T> {
    let mut charts = CHARTS.lock();
    let entry = charts.get_mut(&id)?;
    let answer = f(&mut entry.model);
    touched(entry);
    Some(answer)
}

/// The chart's data as rows of text: series, x and y, for an accessible
/// table or a copy to the clipboard. At most `limit` rows per series.
pub fn table(id: i64, limit: i64) -> Result<Vec<(String, String, String)>, String> {
    with(id, |entry| {
        let model = &entry.model;
        let mut rows = Vec::new();
        for s in &model.series {
            for (x, y) in s.points.iter().take(limit.clamp(0, 100_000) as usize) {
                rows.push((
                    s.style.name.clone(),
                    render::format_value(&model.spec.x.scale, *x),
                    render::format_value(&model.spec.y.scale, *y),
                ));
            }
        }
        Ok(rows)
    })
}

/// The chart drawn at `width` x `height` pixels as PNG bytes.
pub fn export_png(
    id: i64,
    width: i64,
    height: i64,
    theme: &crate::style::Theme,
) -> Result<Vec<u8>, String> {
    if !(16..=8192).contains(&width) || !(16..=8192).contains(&height) {
        return Err("a chart image is 16 to 8192 pixels on a side".into());
    }
    let ops = read(id, |model| render::render(model, width, height, theme))?;
    let mut canvas = crate::render::Canvas::try_new(width, height)?;
    canvas.fill(theme.background);
    crate::render::display::draw(&mut canvas, &ops, 0, 0);
    crate::render::png::encode(width, height, &canvas.pixels)
        .ok_or("the chart image did not encode".into())
}

pub fn close(id: i64) {
    let entry = CHARTS.lock().remove(&id);
    drop(entry);
}

pub(crate) fn close_budget(budget: &Budget) {
    let closed: Vec<Entry> = {
        let mut charts = CHARTS.lock();
        let ids: Vec<i64> = charts
            .iter()
            .filter(|(_, e)| e.budget == *budget)
            .map(|(id, _)| *id)
            .collect();
        ids.iter().filter_map(|id| charts.remove(id)).collect()
    };
    drop(closed);
}

/// Whether any of the session's charts changed since the last call.
pub(crate) fn pump(budget: &Budget) -> bool {
    CHARTS
        .lock()
        .values()
        .filter(|e| e.budget == *budget)
        .fold(false, |changed, e| {
            e.changed.swap(false, Ordering::AcqRel) | changed
        })
}

#[cfg(test)]
mod tests;
