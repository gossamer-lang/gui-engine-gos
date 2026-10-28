//! Bounded shared-worker geometry for styled paragraphs. Small paragraphs can
//! paint immediately; larger ones display an estimate until exact geometry arrives.
use super::Span;
use crate::{
    memory::{Budget, ByteCache, Lease},
    services::jobs::Context,
    text::layout::{self, LayoutOptions, TextLayout, TextStyle},
};
use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Key {
    source: Arc<str>,
    spans: Arc<[Span]>,
    style: String,
}
#[derive(Debug)]
struct Life(AtomicBool);
impl PartialEq for Life {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub owner: String,
    pub style: TextStyle,
    pub options: LayoutOptions,
    #[serde(skip)]
    key: Option<Key>,
    #[serde(skip)]
    life: Option<Arc<Life>>,
}
struct Entry {
    key: Key,
    life: Arc<Life>,
    error: Option<String>,
    _lease: Lease,
    pending: bool,
    view: Option<(i64, i64, i64)>,
}
#[derive(Default)]
struct State {
    owners: BTreeMap<(usize, String), Entry>,
    pending: BTreeMap<(usize, String), Request>,
}
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
static CACHE: LazyLock<Mutex<ByteCache<Key, Arc<TextLayout>>>> =
    LazyLock::new(|| Mutex::new(ByteCache::new(64 * 1024 * 1024)));
fn source_bytes(key: &Key) -> usize {
    key.source.len() + crate::memory::measure(&key.spans) + key.style.capacity() + 1024
}
pub(crate) fn bytes(request: &Request) -> usize {
    request.key.as_ref().map_or(0, source_bytes)
}
fn error(message: impl Into<String>) -> Arc<TextLayout> {
    let message = message.into();
    let caption = format!(
        "Preview unavailable: {}",
        message.chars().take(160).collect::<String>()
    );
    let mut result = layout::layout(
        &caption,
        &TextStyle::sans(14.0),
        LayoutOptions::wrapped(480.0),
    );
    result.error = Some(message);
    Arc::new(result)
}

fn estimate(text: &str, style: &TextStyle, options: LayoutOptions) -> Arc<TextLayout> {
    let (_, _, height) = layout::metrics(style);
    let columns = if options.max_width > 0.0 {
        (options.max_width / (style.size * 0.5).max(1.0)).max(1.0)
    } else {
        f64::INFINITY
    };
    let rows = text
        .split('\n')
        .map(|line| ((line.chars().count() as f64) / columns).ceil().max(1.0))
        .sum::<f64>();
    Arc::new(TextLayout {
        height: rows * height,
        width: options.max_width.max(0.0),
        line_height: height,
        source_length: text.len() as i64,
        ..Default::default()
    })
}
pub(crate) fn get(
    owner: &str,
    text: &str,
    spans: &[Span],
    style: TextStyle,
    options: LayoutOptions,
) -> Arc<TextLayout> {
    if text.len() > super::PARAGRAPH_LIMIT || spans.len() > super::SPAN_LIMIT || owner.len() > 4096
    {
        return error("rich paragraph exceeds its layout limits");
    }
    let budget = crate::app::effects::current_budget();
    let identity = (budget.identity(), owner.to_owned());
    let style_key = serde_json::to_string(&(style.clone(), options)).unwrap();
    {
        let mut state = STATE.lock();
        if let Some(entry) = state.owners.get(&identity)
            && entry.key.source.as_ref() == text
            && entry.key.spans.as_ref() == spans
            && entry.key.style == style_key
        {
            if let Some(layout) = CACHE.lock().get(&entry.key) {
                return layout.clone();
            }
            if let Some(why) = &entry.error {
                return error(why);
            }
            if entry.pending {
                return estimate(text, &style, options);
            }
        }
        if let Some(previous) = state.owners.remove(&identity) {
            previous.life.0.store(false, Ordering::Release);
        }
        state.pending.remove(&identity);
    }
    let owned = text.len() + crate::memory::measure(&spans) + owner.len() * 2 + 8192;
    let reservation = match budget.reserve(owned) {
        Ok(lease) => lease,
        Err(why) => return error(why),
    };
    let key = Key {
        source: Arc::from(text),
        spans: Arc::from(spans),
        style: style_key,
    };
    let cached = CACHE.lock().get(&key).cloned();
    if text.len() <= 4096 && spans.len() <= 64 {
        if let Some(cached) = cached {
            return cached;
        }
        let layout = match layout::try_layout_rich(text, &style, options, spans) {
            Ok(layout) => Arc::new(layout),
            Err(why) => return error(why),
        };
        let bytes = source_bytes(&key) + layout.owned_bytes();
        CACHE.lock().insert(key, layout.clone(), bytes);
        return layout;
    }
    let mut state = STATE.lock();
    if state.owners.len() >= 1024 {
        return error("too many rich paragraph owners");
    }
    let life = Arc::new(Life(AtomicBool::new(true)));
    state.owners.insert(
        identity.clone(),
        Entry {
            key: key.clone(),
            life: life.clone(),
            error: None,
            _lease: reservation,
            pending: cached.is_none(),
            view: None,
        },
    );
    if let Some(cached) = cached {
        return cached;
    }
    state.pending.insert(
        identity,
        Request {
            owner: owner.into(),
            style: style.clone(),
            options,
            key: Some(key),
            life: Some(life),
        },
    );
    drop(state);
    estimate(text, &style, options)
}

pub(crate) fn take_requests(budget: &Budget) -> Vec<Request> {
    let mut state = STATE.lock();
    let keys = state
        .pending
        .keys()
        .filter(|(id, _)| *id == budget.identity())
        .cloned()
        .collect::<Vec<_>>();
    keys.into_iter()
        .filter_map(|key| state.pending.remove(&key))
        .collect()
}
pub(crate) fn run(request: &Request, context: &Context) -> Result<(), String> {
    let key = request.key.as_ref().ok_or("rich layout has no source")?;
    let active = || {
        context.active()
            && request
                .life
                .as_ref()
                .is_some_and(|life| life.0.load(Ordering::Acquire))
    };
    if !active() {
        return Err("rich layout cancelled".into());
    }
    let made = layout::rich_paragraph(
        &key.source,
        &request.style,
        request.options,
        &key.spans,
        context,
    )?;
    if !active() {
        return Err("stale rich layout".into());
    }
    let made = Arc::new(made);
    let bytes = source_bytes(key) + made.owned_bytes();
    if bytes > 64 * 1024 * 1024 {
        return Err("rich paragraph geometry exceeds its byte budget".into());
    }
    if !CACHE.lock().insert(key.clone(), made, bytes) {
        return Err("rich paragraph geometry exceeds its cache budget".into());
    }
    let mut state = STATE.lock();
    for entry in state.owners.values_mut() {
        if &entry.key == key
            && request
                .life
                .as_ref()
                .is_some_and(|life| Arc::ptr_eq(life, &entry.life))
        {
            entry.pending = false;
        }
    }
    Ok(())
}
pub(crate) fn failed(request: &Request, why: &str) {
    let mut state = STATE.lock();
    for entry in state.owners.values_mut() {
        if request.key.as_ref() == Some(&entry.key)
            && request
                .life
                .as_ref()
                .is_some_and(|life| Arc::ptr_eq(life, &entry.life))
        {
            entry.pending = false;
            entry.error = Some(why.chars().take(4096).collect());
        }
    }
}
pub(crate) fn release_owner(budget: &Budget, owner: &str) {
    let key = (budget.identity(), owner.to_owned());
    let mut state = STATE.lock();
    state.pending.remove(&key);
    if let Some(entry) = state.owners.remove(&key) {
        entry.life.0.store(false, Ordering::Release);
    }
}
pub(crate) fn close_budget(budget: &Budget) {
    let mut state = STATE.lock();
    state.pending.retain(|(id, _), _| *id != budget.identity());
    state.owners.retain(|(id, _), entry| {
        if *id == budget.identity() {
            entry.life.0.store(false, Ordering::Release);
            false
        } else {
            true
        }
    });
}
pub(crate) fn purge() {
    let mut state = STATE.lock();
    for entry in state.owners.values() {
        entry.life.0.store(false, Ordering::Release);
    }
    state.owners.clear();
    state.pending.clear();
    CACHE.lock().clear();
}

pub(crate) fn remember_view(budget: &Budget, owner: &str, width: i64, height: i64, margin: i64) {
    if let Some(entry) = STATE
        .lock()
        .owners
        .get_mut(&(budget.identity(), owner.to_owned()))
    {
        entry.view = Some((width, height, margin));
    }
}
pub(crate) fn adjust_state(
    text: &crate::editing::buffer::Buffer,
    scroll_x: &mut f64,
    scroll_y: &mut f64,
    caret: i64,
    follow: bool,
    budget: &Budget,
    owner: &str,
) {
    if !follow || owner.is_empty() {
        return;
    }
    let state = STATE.lock();
    let Some(entry) = state.owners.get(&(budget.identity(), owner.to_owned())) else {
        return;
    };
    if text != entry.key.source.as_ref() {
        return;
    }
    let Some((width, height, margin)) = entry.view else {
        return;
    };
    let Some(layout) = CACHE.lock().get(&entry.key).cloned() else {
        return;
    };
    let rect = layout.caret_rect(caret);
    let margin = margin as f64;
    if rect.x as f64 - *scroll_x > width as f64 - margin {
        *scroll_x = rect.x as f64 - width as f64 + margin;
    }
    if rect.x as f64 - *scroll_x < 0.0 {
        *scroll_x = (rect.x as f64 - margin).max(0.0);
    }
    if rect.bottom() as f64 - *scroll_y > height as f64 {
        *scroll_y = rect.bottom() as f64 - height as f64;
    }
    if (rect.y as f64) < *scroll_y {
        *scroll_y = rect.y as f64;
    }
    *scroll_x = scroll_x
        .max(0.0)
        .min((layout.width - width as f64 + margin).max(0.0));
    *scroll_y = scroll_y
        .max(0.0)
        .min((layout.height - height as f64).max(0.0));
}
pub(crate) fn defer(budget: &Budget, request: &Request) {
    let mut state = STATE.lock();
    let key = (budget.identity(), request.owner.clone());
    if let Some(entry) = state.owners.get(&key)
        && request
            .life
            .as_ref()
            .is_some_and(|life| Arc::ptr_eq(life, &entry.life) && life.0.load(Ordering::Acquire))
    {
        state.pending.insert(key, request.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disposed_paragraphs_cancel_their_inflight_geometry_and_release_session_storage() {
        let old = crate::app::effects::current_budget();
        let budget = Budget::new(128 * 1024 * 1024);
        crate::app::effects::set_budget(budget.clone());
        let _restore = scopeguard::guard(old, crate::app::effects::set_budget);
        let text = "paragraph ".repeat(1000);
        let spans = vec![Span {
            start: 0,
            end: text.len(),
            marks: crate::rich::Marks::default(),
            image: None,
        }];
        let _estimate = get(
            "disposal-regression",
            &text,
            &spans,
            TextStyle::sans(18.0),
            LayoutOptions::wrapped(300.0),
        );
        let requests = take_requests(&budget);
        release_owner(&budget, "disposal-regression");
        for request in requests {
            assert!(
                run(
                    &request,
                    &Context::standalone(std::time::Duration::from_secs(1))
                )
                .is_err()
            );
        }
        assert_eq!(budget.usage().0, 0);
    }
}
