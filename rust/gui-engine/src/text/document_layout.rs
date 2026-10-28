//! Native viewport layout. Paragraph geometry is computed on the shared worker,
//! retained by bytes, then cropped before returning to the UI. Sparse measured
//! row counts refine the virtual scroll range without shaping the whole file.
use super::layout::{self, LayoutOptions, TextLayout, TextStyle};
use crate::{
    editing::buffer::Buffer,
    geometry::Rect,
    memory::{ByteCache, Lease},
    services::jobs::Context,
};
use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Debug)]
struct Life(AtomicBool);
impl PartialEq for Life {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub key: u64,
    pub owner: String,
    pub revision: i64,
    pub identity: u64,
    pub style: TextStyle,
    pub options: LayoutOptions,
    pub area: Rect,
    pub caret: i64,
    pub follow: bool,
    #[serde(skip)]
    source: Option<Buffer>,
    #[serde(skip)]
    life: Option<Arc<Life>>,
}
struct Entry {
    root: Buffer,
    request: Option<Request>,
    view: Option<TextLayout>,
    rows: BTreeMap<usize, usize>,
    navigation: BTreeMap<usize, std::sync::Weak<TextLayout>>,
    life: Arc<Life>,
    lease: Lease,
    suggestion: Option<(Rect, Rect, i64, i64, u64)>,
}
#[derive(Default)]
struct State {
    entries: BTreeMap<u64, Entry>,
    pending: BTreeMap<u64, Request>,
}
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
static PARAGRAPHS: LazyLock<Mutex<ByteCache<String, Arc<TextLayout>>>> =
    LazyLock::new(|| Mutex::new(ByteCache::new(256 * 1024 * 1024)));
fn extra(rows: &BTreeMap<usize, usize>, line: usize) -> usize {
    rows.range(..line)
        .map(|(_, rows)| rows.saturating_sub(1))
        .sum()
}
fn top(rows: &BTreeMap<usize, usize>, line: usize, height: f64) -> f64 {
    (line + extra(rows, line)) as f64 * height
}
fn line_at(rows: &BTreeMap<usize, usize>, count: usize, y: f64, height: f64) -> usize {
    let (mut low, mut high) = (0, count);
    while low < high {
        let mid = (low + high) / 2;
        if top(rows, mid, height) <= y {
            low = mid + 1
        } else {
            high = mid
        }
    }
    low.saturating_sub(1).min(count.saturating_sub(1))
}
pub(crate) fn viewport(
    owner: &str,
    state: &crate::editing::EditState,
    style: TextStyle,
    options: LayoutOptions,
    area: Rect,
) -> Option<TextLayout> {
    let key = state.layout_key;
    if key == 0 || owner.len() > 4096 {
        return None;
    }
    let mut cache = STATE.lock();
    if !cache.entries.contains_key(&key) {
        if cache.entries.len() >= 1024 {
            let oldest = *cache.entries.keys().next()?;
            if let Some(old) = cache.entries.remove(&oldest) {
                old.life.0.store(true, Ordering::Release);
            }
            cache.pending.remove(&oldest);
        }
        let lease = crate::app::effects::current_budget()
            .reserve(state.text.owned_charge() + 64 * 1024)
            .ok()?;
        cache.entries.insert(
            key,
            Entry {
                root: state.text.snapshot(),
                request: None,
                view: None,
                rows: BTreeMap::new(),
                navigation: BTreeMap::new(),
                life: Arc::new(Life(AtomicBool::new(false))),
                lease,
                suggestion: None,
            },
        );
    }
    let entry = cache.entries.get_mut(&key)?;
    if !entry.root.same_revision(&state.text) {
        if state.changes.len() == 1 {
            let delta = &state.changes[0];
            let first = entry.root.line_of(delta.start);
            let last = entry.root.line_of(delta.start + delta.removed);
            let shift = state.text.line_count() as i64 - entry.root.line_count() as i64;
            entry.rows = std::mem::take(&mut entry.rows)
                .into_iter()
                .filter_map(|(line, rows)| {
                    if line < first {
                        Some((line, rows))
                    } else if line > last {
                        Some(((line as i64 + shift).max(0) as usize, rows))
                    } else {
                        None
                    }
                })
                .collect();
        } else {
            entry.rows.clear();
        }
        entry.navigation.clear();
        entry.root = state.text.snapshot();
        entry.view = None;
        entry.request = None;
    }
    let request = Request {
        key,
        owner: owner.into(),
        revision: state.revision,
        identity: state.text.identity(),
        style,
        options,
        area,
        caret: state.caret,
        follow: state.follow_caret,
        source: Some(state.text.snapshot()),
        life: Some(entry.life.clone()),
    };
    if let Some(old) = &entry.request {
        if old.style != request.style || old.options != request.options {
            entry.rows.clear();
            entry.navigation.clear();
        }
        if old == &request {
            return entry.view.clone();
        }
    }
    entry.request = Some(request.clone());
    entry.view = None;
    cache.pending.insert(key, request);
    None
}
pub(crate) fn take_requests() -> Vec<Request> {
    std::mem::take(&mut STATE.lock().pending)
        .into_values()
        .collect()
}
pub(crate) fn release(key: u64) {
    let mut state = STATE.lock();
    state.pending.remove(&key);
    if let Some(entry) = state.entries.remove(&key) {
        entry.life.0.store(true, Ordering::Release);
    }
}
pub(crate) fn purge() {
    let mut state = STATE.lock();
    for entry in state.entries.values() {
        entry.life.0.store(true, Ordering::Release);
    }
    state.entries.clear();
    state.pending.clear();
    PARAGRAPHS.lock().clear();
}
pub(crate) fn source_bytes(request: &Request) -> usize {
    request.source.as_ref().map_or(0, Buffer::owned_charge)
}
pub(crate) fn bytes(request: &Request) -> usize {
    request
        .source
        .as_ref()
        .map_or(0, Buffer::max_line_bytes)
        .saturating_mul(256)
        .clamp(8 * 1024 * 1024, 512 * 1024 * 1024)
}
pub(crate) fn adjust_state(state: &mut crate::editing::EditState) {
    if !state.follow_caret || !state.preedit.is_empty() {
        return;
    }
    let cache = STATE.lock();
    let Some(entry) = cache.entries.get(&state.layout_key) else {
        return;
    };
    if let Some((old, new, revision, caret, identity)) = entry.suggestion
        && state.revision == revision
        && state.caret == caret
        && state.text.identity() == identity
        && state.scroll_x as i64 == old.x
        && state.scroll_y as i64 == old.y
    {
        state.scroll_x = new.x as f64;
        state.scroll_y = new.y as f64;
    }
}
fn paragraph(
    source: &Buffer,
    line: usize,
    request: &Request,
    context: &Context,
) -> Result<Arc<TextLayout>, String> {
    let start = source.line_start(line);
    let end = source.line_start(line + 1);
    let text = source.slice(start, end);
    let text = text.trim_end_matches(['\n', '\r', '\u{0085}', '\u{2028}', '\u{2029}']);
    let key = format!("{}#{}#{}", request.style.key(), request.options.key(), text);
    if let Some(cached) = PARAGRAPHS.lock().get(&key).cloned() {
        return Ok(cached);
    }
    let made = Arc::new(layout::document_paragraph(
        text,
        &request.style,
        request.options,
        context,
    )?);
    let bytes = key.capacity() + made.owned_bytes();
    PARAGRAPHS.lock().insert(key, made.clone(), bytes);
    Ok(made)
}
pub(crate) fn failed(request: &Request, error: &str) {
    let mut state = STATE.lock();
    if let Some(entry) = state.entries.get_mut(&request.key)
        && entry.request.as_ref() == Some(request)
    {
        entry.view = Some(TextLayout {
            error: Some(error.chars().take(4096).collect()),
            ..TextLayout::default()
        });
    }
}
pub(crate) fn run(request: &Request, context: &Context) -> Result<(), String> {
    let source = request
        .source
        .as_ref()
        .ok_or("layout request has no native snapshot")?;
    let life = request.life.as_ref().ok_or("layout request is closed")?;
    let active = || context.active() && !life.0.load(Ordering::Acquire);
    if !active() {
        return Err("document layout cancelled".into());
    }
    let mut rows = {
        let state = STATE.lock();
        let entry = state
            .entries
            .get(&request.key)
            .ok_or("layout owner is closed")?;
        if entry.request.as_ref() != Some(request) {
            return Err("stale viewport request".into());
        }
        entry.rows.clone()
    };
    let mut navigation = BTreeMap::new();
    let (ascent, descent, height) = layout::metrics(&request.style);
    let mut adjusted = request.clone();
    if request.follow {
        let logical = source.line_of(request.caret.max(0) as usize);
        let shape = paragraph(source, logical, request, context)?;
        navigation.insert(logical, Arc::downgrade(&shape));
        rows.insert(logical, shape.lines.len().max(1));
        let caret = shape.caret_rect(request.caret - source.line_start(logical) as i64);
        let y = top(&rows, logical, height) + caret.y as f64;
        if y < adjusted.area.y as f64 {
            adjusted.area.y = y as i64;
        }
        if y + caret.height as f64 > adjusted.area.bottom() as f64 {
            adjusted.area.y =
                (y + caret.height as f64 - adjusted.area.height as f64).max(0.0) as i64;
        }
        if request.options.wrap != layout::Wrap::NoWrap {
            adjusted.area.x = 0;
        } else {
            if caret.x < adjusted.area.x {
                adjusted.area.x = caret.x.max(0);
            }
            if caret.x + 8 > adjusted.area.right() {
                adjusted.area.x = (caret.x + 8 - adjusted.area.width).max(0);
            }
        }
    }
    let first =
        line_at(&rows, source.line_count(), adjusted.area.y as f64, height).saturating_sub(2);
    let mut out = TextLayout {
        ascent,
        descent,
        line_height: height,
        source_length: source.byte_len(),
        ..TextLayout::default()
    };
    let bottom = adjusted.area.y as f64 + adjusted.area.height as f64 + height * 2.0;
    for line in first..source.line_count() {
        if !active() {
            return Err("document layout cancelled".into());
        }
        let start = source.line_start(line);
        let paragraph = paragraph(source, line, request, context)?;
        navigation.insert(line, Arc::downgrade(&paragraph));
        rows.insert(line, paragraph.lines.len().max(1));
        let y = top(&rows, line, height);
        out.width = out.width.max(paragraph.width);
        for visual in paragraph.lines.iter().filter(|visual| {
            visual.top + y + visual.height >= adjusted.area.y as f64 - height
                && visual.top + y <= bottom
        }) {
            let mut visible = visual.clone();
            // Glyphs are in visual order, including decreasing source offsets
            // in RTL runs. Keep only viewport glyphs and their grapheme stops.
            visible.glyphs.retain(|glyph| {
                glyph.x + visual.left + glyph.advance.max(request.style.size)
                    >= adjusted.area.x as f64 - request.style.size * 2.0
                    && glyph.x + visual.left
                        <= adjusted.area.right() as f64 + request.style.size * 2.0
            });
            visible.glyphs.shrink_to_fit();
            visible.start += start as i64;
            visible.end += start as i64;
            visible.top += y;
            visible.baseline += y;
            for glyph in &mut visible.glyphs {
                glyph.start += start as i64;
                glyph.end += start as i64;
                let a = paragraph
                    .boundaries
                    .partition_point(|&byte| byte < glyph.start - start as i64);
                let b = paragraph
                    .boundaries
                    .partition_point(|&byte| byte <= glyph.end - start as i64);
                out.boundaries.extend(
                    paragraph.boundaries[a..b]
                        .iter()
                        .map(|byte| byte + start as i64),
                );
            }
            out.boundaries.push(visible.start);
            out.boundaries.push(visible.end);
            out.lines.push(visible);
        }
        if y + paragraph.height > bottom {
            break;
        }
    }
    while rows.len() > 16384 {
        let key = *rows.keys().next().unwrap();
        rows.remove(&key);
    }
    out.height = top(&rows, source.line_count(), height);
    out.boundaries.sort_unstable();
    out.boundaries.dedup();
    if out.owned_bytes() > 16 * 1024 * 1024 {
        return Err("viewport geometry exceeds its byte budget".into());
    }
    let mut state = STATE.lock();
    let entry = state
        .entries
        .get_mut(&request.key)
        .ok_or("layout owner is closed")?;
    if !active() || entry.request.as_ref() != Some(request) {
        return Err("stale viewport layout".into());
    }
    entry
        .lease
        .resize(entry.root.owned_charge() + rows.len() * 128 + out.owned_bytes() + 64 * 1024)?;
    entry.rows = rows;
    entry.navigation = navigation;
    entry.view = Some(out);
    entry.suggestion = Some((
        request.area,
        adjusted.area,
        request.revision,
        request.caret,
        request.identity,
    ));
    entry.request = Some(adjusted);
    Ok(())
}

pub(crate) fn vertical(state: &crate::editing::EditState, dir: i64, x: f64) -> Option<i64> {
    let cache = STATE.lock();
    let entry = cache.entries.get(&state.layout_key)?;
    if !entry.root.same_revision(&state.text) {
        return None;
    }
    let mut logical = state.text.line_of(state.caret.max(0) as usize);
    let mut shape = entry.navigation.get(&logical)?.upgrade()?;
    let mut target = shape.line_of(state.caret - state.text.line_start(logical) as i64) + dir;
    loop {
        if target < 0 {
            if logical == 0 {
                return Some(0);
            }
            logical -= 1;
            shape = entry.navigation.get(&logical)?.upgrade()?;
            target += shape.lines.len() as i64;
        } else if target >= shape.lines.len() as i64 {
            target -= shape.lines.len() as i64;
            logical += 1;
            if logical >= state.text.line_count() {
                return Some(state.text.byte_len());
            }
            shape = entry.navigation.get(&logical)?.upgrade()?;
        } else {
            return Some(state.text.line_start(logical) as i64 + shape.hit_line(target, x));
        }
    }
}

pub fn ready(key: u64, identity: u64) -> bool {
    STATE.lock().entries.get(&key).is_some_and(|entry| {
        entry.root.identity() == identity
            && entry.view.as_ref().is_some_and(|view| view.error.is_none())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn wrapped_unicode_viewports_and_long_lines_use_exact_background_geometry() {
        let budget = crate::memory::Budget::new(1024 * 1024 * 1024);
        crate::app::effects::set_budget(budget.clone());
        for text in [
            "let α = \"שלום e\u{301} 👩\u{200d}💻\"; ".repeat(2000),
            "x".repeat(1024 * 1024),
        ] {
            let mut state = crate::editing::EditState::default();
            state.sync(&text);
            state.scroll_y = 0.0;
            let style = TextStyle::mono(14.0);
            let options = LayoutOptions::wrapped(600.0);
            let area = Rect::new(0, 0, 600, 360);
            assert!(viewport("layout-test", &state, style.clone(), options, area).is_none());
            let requests = take_requests();
            assert_eq!(requests.len(), 1);
            let context = Context::standalone(Duration::from_secs(60));
            let begin = Instant::now();
            run(&requests[0], &context).unwrap();
            adjust_state(&mut state);
            assert!(state.scroll_y > 360.0);
            let actual = viewport(
                "layout-test",
                &state,
                style.clone(),
                options,
                Rect::new(state.scroll_x as i64, state.scroll_y as i64, 600, 360),
            )
            .unwrap();
            let caret = actual.caret_rect(state.caret);
            assert!(caret.y as f64 >= state.scroll_y - 1.0);
            assert!(caret.y as f64 + caret.height as f64 <= state.scroll_y + 362.0);
            assert!(actual.owned_bytes() < 2 * 1024 * 1024);
            assert!(!state.text.flattened());
            let before = state.caret;
            state.vertical(&actual, -3, false);
            assert!(state.caret < before);
            println!(
                "background paragraph bytes={} elapsed_ms={} viewport_bytes={}",
                text.len(),
                begin.elapsed().as_millis(),
                actual.owned_bytes()
            );
            release(state.layout_key);
        }
        purge();
        assert_eq!(budget.usage().0, 0);
        crate::app::effects::set_budget(crate::memory::Budget::new(
            crate::app::effects::EFFECT_BYTE_LIMIT,
        ));
    }
}
