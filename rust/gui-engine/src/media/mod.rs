//! Media hosts: audio and video played by GStreamer 1.x, loaded at run time
//! as an optional dependency (Linux packages; the GStreamer runtime on
//! Windows and macOS). Without it a player reports itself unavailable.
//!
//! A player is a session-owned handle around a `playbin` pipeline. Decoded
//! video frames arrive as RGBA on GStreamer's streaming thread, which keeps
//! only the newest frame and wakes the session; a bus thread reports state,
//! end of stream and errors. Closing a player stops the pipeline and joins
//! its thread, after which no callback runs.

pub mod api;
mod gst;
pub mod widget;

use std::collections::HashMap;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread::JoinHandle;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::memory::{Budget, Lease};
use crate::render::Pixels;
use crate::widgets::cx::UiMsg;
use gst::Pointer;

const HANDLE_LIMIT: usize = 32;
const FRAME_LIMIT: usize = 64 * 1024 * 1024;

/// Where a player's sound goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioOutput {
    /// The system's audio device.
    #[default]
    System,
    /// Nowhere, keeping playback's timing: for tests and muted previews.
    Discard,
}

/// What a player is doing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum State {
    /// GStreamer is not installed or did not start.
    Unavailable(String),
    Idle,
    Loading,
    Paused,
    Playing,
    Ended,
    Failed(String),
}

/// A player's state as the application reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    pub source: String,
    pub position_ms: i64,
    /// -1 while unknown, such as for a live stream.
    pub duration_ms: i64,
    /// 0..=1.
    pub volume: f64,
    pub muted: bool,
    /// The video's size, 0 x 0 for sound only.
    pub width: i64,
    pub height: i64,
    pub frames: i64,
}

/// The newest decoded frame.
#[derive(Debug, Default)]
pub struct Frame {
    pub width: i64,
    pub height: i64,
    pub rgba: Pixels,
    pub count: i64,
    /// The frame before, whose buffer the next frame reuses once the scene
    /// that drew it lets go.
    spare: Pixels,
}

/// What GStreamer's threads share with the UI side.
struct Shared {
    frame: Mutex<Frame>,
    waker: Option<windowing::Waker>,
    changed: AtomicBool,
    /// Events from the bus thread, oldest first.
    events: Mutex<Vec<BusEvent>>,
}

impl Shared {
    fn touch(&self) {
        if !self.changed.swap(true, Ordering::AcqRel)
            && let Some(waker) = &self.waker
        {
            let _ = waker.wake();
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum BusEvent {
    Paused,
    Playing,
    Ended,
    Failed(String),
    Duration,
}

/// GStreamer objects are reference counted and thread-safe; the pointers
/// here are used only through GStreamer's thread-safe API.
struct Pipeline {
    playbin: Pointer,
    bus: Pointer,
}

// SAFETY: see `Pipeline`: every use goes through GStreamer's MT-safe calls.
unsafe impl Send for Pipeline {}

struct Entry {
    status: Status,
    pipeline: Option<Pipeline>,
    watcher: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    budget: Budget,
    _lease: Option<Lease>,
    output: AudioOutput,
    reported: Option<State>,
    reported_position: i64,
}

static NEXT: AtomicI64 = AtomicI64::new(1);
static PLAYERS: LazyLock<Mutex<HashMap<i64, Entry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn with<T>(id: i64, f: impl FnOnce(&mut Entry) -> Result<T, String>) -> Result<T, String> {
    let mut players = PLAYERS.lock();
    let entry = players.get_mut(&id).ok_or("the media player is closed")?;
    f(entry)
}

fn frame_from(api: &gst::Api, sample: Pointer, shared: &Shared) {
    if sample.is_null() {
        return;
    }
    // SAFETY: `sample` is a live GstSample this function owns and releases;
    // the buffer is mapped for reading and unmapped before the release.
    unsafe {
        let caps = (api.sample_caps)(sample);
        let structure = if caps.is_null() {
            std::ptr::null_mut()
        } else {
            (api.caps_structure)(caps, 0)
        };
        let (mut width, mut height): (c_int, c_int) = (0, 0);
        if !structure.is_null() {
            (api.structure_int)(structure, c"width".as_ptr(), &mut width);
            (api.structure_int)(structure, c"height".as_ptr(), &mut height);
        }
        let buffer = (api.sample_buffer)(sample);
        let bytes = (width.max(0) as usize)
            .saturating_mul(height.max(0) as usize)
            .saturating_mul(4);
        let mut map = gst::MapInfo::default();
        if !buffer.is_null()
            && bytes > 0
            && bytes <= FRAME_LIMIT
            && (api.buffer_map)(buffer, &mut map, gst::MAP_READ) != 0
        {
            if map.size >= bytes {
                let pixels = std::slice::from_raw_parts(map.data, bytes);
                let mut frame = shared.frame.lock();
                let frame = &mut *frame;
                std::mem::swap(&mut frame.rgba, &mut frame.spare);
                frame.rgba.replace(pixels);
                frame.width = i64::from(width);
                frame.height = i64::from(height);
                frame.count += 1;
            }
            (api.buffer_unmap)(buffer, &mut map);
        }
        (api.mini_object_unref)(sample);
    }
    shared.touch();
}

unsafe extern "C" fn on_sample(sink: Pointer, data: Pointer) -> c_int {
    if let Ok(api) = gst::api() {
        // SAFETY: `data` is the Shared the callbacks were registered with,
        // alive until their destroy notify runs.
        let shared = unsafe { &*data.cast::<Shared>() };
        // SAFETY: called by appsink, which has a sample ready.
        frame_from(api, unsafe { (api.pull_sample)(sink) }, shared);
    }
    0
}

unsafe extern "C" fn on_preroll(sink: Pointer, data: Pointer) -> c_int {
    if let Ok(api) = gst::api() {
        // SAFETY: as in `on_sample`.
        let shared = unsafe { &*data.cast::<Shared>() };
        // SAFETY: called by appsink, which has a preroll sample ready.
        frame_from(api, unsafe { (api.pull_preroll)(sink) }, shared);
    }
    0
}

unsafe extern "C" fn release(data: Pointer) {
    // SAFETY: `data` came from Arc::into_raw when the callbacks were set.
    drop(unsafe { Arc::from_raw(data.cast::<Shared>()) });
}

/// Watches a pipeline's bus until told to stop.
fn watch(api: &'static gst::Api, pipeline: Pipeline, shared: Arc<Shared>) {
    let mask = gst::MESSAGE_EOS
        | gst::MESSAGE_ERROR
        | gst::MESSAGE_STATE_CHANGED
        | gst::MESSAGE_DURATION_CHANGED
        | gst::MESSAGE_ASYNC_DONE
        | gst::MESSAGE_APPLICATION;
    loop {
        // SAFETY: the bus outlives this thread, which close joins first.
        let message = unsafe { (api.bus_pop)(pipeline.bus, gst::CLOCK_TIME_NONE, mask) };
        // A flushing bus answers nothing: its pipeline has stopped.
        if message.is_null() {
            return;
        }
        // SAFETY: `message` is live until unreffed at the end of the iteration.
        let (kind, source) = unsafe { (gst::message_type(message), gst::message_source(message)) };
        let event = match kind {
            gst::MESSAGE_APPLICATION => {
                // SAFETY: the message is owned here.
                unsafe { (api.mini_object_unref)(message) };
                return;
            }
            gst::MESSAGE_EOS => Some(BusEvent::Ended),
            gst::MESSAGE_ERROR => {
                let mut error = std::ptr::null_mut();
                let mut debug = std::ptr::null_mut();
                // SAFETY: an error message carries a GError and debug text.
                unsafe { (api.parse_error)(message, &mut error, &mut debug) };
                let text = gst::take_error(api, error);
                // SAFETY: the debug string is owned here (g_free accepts NULL).
                unsafe { (api.free)(debug.cast()) };
                Some(BusEvent::Failed(text))
            }
            gst::MESSAGE_STATE_CHANGED if source == pipeline.playbin => {
                let (mut old, mut new, mut pending) = (0, 0, 0);
                // SAFETY: a state-changed message carries three states.
                unsafe { (api.parse_state_changed)(message, &mut old, &mut new, &mut pending) };
                match new {
                    gst::STATE_PLAYING => Some(BusEvent::Playing),
                    gst::STATE_PAUSED => Some(BusEvent::Paused),
                    _ => None,
                }
            }
            gst::MESSAGE_DURATION_CHANGED | gst::MESSAGE_ASYNC_DONE => Some(BusEvent::Duration),
            _ => None,
        };
        // SAFETY: the message is owned here.
        unsafe { (api.mini_object_unref)(message) };
        if let Some(event) = event {
            let mut events = shared.events.lock();
            // A long-stopped session cannot make this queue grow without
            // bound: only the newest progress events matter.
            if events.len() >= 64 {
                events.remove(0);
            }
            events.push(event);
            drop(events);
            shared.touch();
        }
    }
}

fn unavailable(why: String) -> Status {
    Status {
        state: State::Unavailable(why),
        source: String::new(),
        position_ms: 0,
        duration_ms: -1,
        volume: 1.0,
        muted: false,
        width: 0,
        height: 0,
        frames: 0,
    }
}

/// Opens a player in the running session. Without GStreamer it opens in
/// the `Unavailable` state, which every later call reports.
pub fn open(output: AudioOutput, budget: Budget) -> Result<i64, String> {
    let status = match gst::api() {
        Ok(_) => Status {
            state: State::Idle,
            ..unavailable(String::new())
        },
        Err(why) => unavailable(why),
    };
    let mut players = PLAYERS.lock();
    if players.len() >= HANDLE_LIMIT {
        return Err("the media player registry is full".into());
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    players.insert(
        id,
        Entry {
            status,
            pipeline: None,
            watcher: None,
            shared: Arc::new(Shared {
                frame: Mutex::new(Frame::default()),
                waker: crate::app::effects::current_waker(),
                changed: AtomicBool::new(false),
                events: Mutex::new(Vec::new()),
            }),
            budget,
            _lease: None,
            output,
            reported: None,
            reported_position: 0,
        },
    );
    Ok(id)
}

fn available(entry: &Entry) -> Result<&'static gst::Api, String> {
    if let State::Unavailable(why) = &entry.status.state {
        return Err(why.clone());
    }
    gst::api()
}

/// Stops and releases a player's pipeline and its bus thread.
fn teardown(entry: &mut Entry) {
    let Ok(api) = gst::api() else {
        return;
    };
    if let Some(pipeline) = entry.pipeline.take() {
        // The stop message reaches the bus thread before the pipeline stops,
        // since a stopped pipeline's bus drops what is posted to it.
        // SAFETY: the bus is live; the message takes the structure.
        unsafe {
            let stop = (api.structure_new_empty)(c"gui-stop".as_ptr());
            (api.bus_post)(
                pipeline.bus,
                (api.message_new_application)(std::ptr::null_mut(), stop),
            );
        }
        if let Some(watcher) = entry.watcher.take() {
            let _ = watcher.join();
        }
        // SAFETY: going to NULL stops every streaming thread before it
        // returns; both references are owned here and released once.
        unsafe {
            (api.set_state)(pipeline.playbin, gst::STATE_NULL);
            (api.object_unref)(pipeline.bus);
            (api.object_unref)(pipeline.playbin);
        }
    }
    *entry.shared.frame.lock() = Frame::default();
    entry.shared.events.lock().clear();
}

/// Loads `source` (a file path or URI), ready to play from the start.
pub fn load(id: i64, source: &str) -> Result<(), String> {
    if source.is_empty() || source.contains('\0') || source.len() > 8192 {
        return Err("the media source must be a path or URI".into());
    }
    let local = !source.contains("://");
    if local {
        // GStreamer reports an unreadable file only as a failed state change.
        let meta = std::fs::metadata(source).map_err(|e| format!("cannot read {source}: {e}"))?;
        if meta.is_dir() {
            return Err(format!("{source} is a folder"));
        }
    }
    let mut players = PLAYERS.lock();
    let entry = players.get_mut(&id).ok_or("the media player is closed")?;
    let api = available(entry)?;
    teardown(entry);
    let uri = if !local {
        source.to_string()
    } else {
        let mut error = std::ptr::null_mut();
        // SAFETY: the path is NUL-terminated; the answer is owned and freed.
        let raw = unsafe { (api.filename_to_uri)(gst::c(source).as_ptr(), &mut error) };
        if raw.is_null() {
            return Err(gst::take_error(api, error));
        }
        // SAFETY: a non-null answer is a NUL-terminated string.
        let uri = unsafe { std::ffi::CStr::from_ptr(raw).to_string_lossy().into_owned() };
        // SAFETY: owned by this function.
        unsafe { (api.free)(raw.cast()) };
        uri
    };
    // SAFETY: element creation answers new floating references or NULL; the
    // sinks are handed to playbin, which owns them from then on.
    let (playbin, bus) = unsafe {
        let playbin = (api.factory_make)(c"playbin".as_ptr(), std::ptr::null());
        let sink = (api.factory_make)(c"appsink".as_ptr(), std::ptr::null());
        let audio = match entry.output {
            AudioOutput::System => (api.factory_make)(c"autoaudiosink".as_ptr(), std::ptr::null()),
            AudioOutput::Discard => (api.factory_make)(c"fakesink".as_ptr(), std::ptr::null()),
        };
        if playbin.is_null() || sink.is_null() || audio.is_null() {
            return Err("GStreamer lacks the playbin, appsink or audio sink element".into());
        }
        let caps = (api.caps_from_string)(c"video/x-raw,format=RGBA".as_ptr());
        gst::set_pointer(api, sink, c"caps", caps);
        (api.mini_object_unref)(caps);
        // Only the newest frame is kept: a slow UI drops frames, not memory.
        gst::set_int(api, sink, c"max-buffers", 1);
        gst::set_int(api, sink, c"drop", 1);
        gst::set_int(api, audio, c"sync", 1);
        let callbacks = gst::AppSinkCallbacks {
            eos: None,
            new_preroll: Some(on_preroll),
            new_sample: Some(on_sample),
            rest: [std::ptr::null_mut(); 8],
        };
        let shared = Arc::into_raw(entry.shared.clone())
            .cast_mut()
            .cast::<c_void>();
        (api.set_callbacks)(sink, &callbacks, shared, Some(release));
        gst::set_pointer(api, playbin, c"video-sink", sink);
        gst::set_pointer(api, playbin, c"audio-sink", audio);
        gst::set_pointer(
            api,
            playbin,
            c"uri",
            gst::c(&uri).as_ptr().cast_mut().cast(),
        );
        gst::set_double(api, playbin, c"volume", entry.status.volume);
        gst::set_int(api, playbin, c"mute", i32::from(entry.status.muted));
        (playbin, (api.get_bus)(playbin))
    };
    entry._lease = Some(entry.budget.reserve(FRAME_LIMIT / 4)?);
    let shared = entry.shared.clone();
    let pipeline = Pipeline { playbin, bus };
    let watched = Pipeline { playbin, bus };
    entry.watcher = Some(
        std::thread::Builder::new()
            .name("gui-media-bus".into())
            .spawn(move || watch(api, watched, shared))
            .map_err(|e| e.to_string())?,
    );
    entry.pipeline = Some(pipeline);
    entry.status = Status {
        state: State::Loading,
        source: source.to_string(),
        position_ms: 0,
        duration_ms: -1,
        frames: 0,
        width: 0,
        height: 0,
        ..entry.status.clone()
    };
    entry.reported = None;
    // SAFETY: the pipeline is live; pausing prerolls the first frame.
    unsafe { (api.set_state)(playbin, gst::STATE_PAUSED) };
    Ok(())
}

fn pipeline_of(entry: &Entry) -> Result<(&'static gst::Api, Pointer), String> {
    let api = available(entry)?;
    let pipeline = entry.pipeline.as_ref().ok_or("nothing is loaded")?;
    Ok((api, pipeline.playbin))
}

pub fn play(id: i64) -> Result<(), String> {
    with(id, |entry| {
        let (api, playbin) = pipeline_of(entry)?;
        if entry.status.state == State::Ended {
            // SAFETY: the pipeline is live.
            unsafe {
                (api.seek_simple)(
                    playbin,
                    gst::FORMAT_TIME,
                    gst::SEEK_FLUSH | gst::SEEK_ACCURATE,
                    0,
                )
            };
        }
        // SAFETY: the pipeline is live.
        unsafe { (api.set_state)(playbin, gst::STATE_PLAYING) };
        Ok(())
    })
}

pub fn pause(id: i64) -> Result<(), String> {
    with(id, |entry| {
        let (api, playbin) = pipeline_of(entry)?;
        // SAFETY: the pipeline is live.
        unsafe { (api.set_state)(playbin, gst::STATE_PAUSED) };
        Ok(())
    })
}

/// Moves playback to `position_ms`.
pub fn seek(id: i64, position_ms: i64) -> Result<(), String> {
    with(id, |entry| {
        let (api, playbin) = pipeline_of(entry)?;
        let target = position_ms.clamp(
            0,
            if entry.status.duration_ms > 0 {
                entry.status.duration_ms
            } else {
                i64::MAX / 1_000_000
            },
        );
        // A pipeline stays playing past its end; a seek from there waits
        // paused at the new place rather than playing on.
        if entry.status.state == State::Ended {
            // SAFETY: the pipeline is live.
            unsafe { (api.set_state)(playbin, gst::STATE_PAUSED) };
        }
        // SAFETY: the pipeline is live.
        let done = unsafe {
            (api.seek_simple)(
                playbin,
                gst::FORMAT_TIME,
                gst::SEEK_FLUSH | gst::SEEK_ACCURATE,
                target * 1_000_000,
            )
        };
        if done == 0 {
            return Err("the media cannot seek there".into());
        }
        entry.status.position_ms = target;
        if entry.status.state == State::Ended {
            entry.status.state = State::Paused;
        }
        Ok(())
    })
}

pub fn set_volume(id: i64, volume: f64) -> Result<(), String> {
    if !volume.is_finite() {
        return Err("volume must be 0 to 1".into());
    }
    with(id, |entry| {
        entry.status.volume = volume.clamp(0.0, 1.0);
        if let Ok((api, playbin)) = pipeline_of(entry) {
            // SAFETY: the pipeline is live.
            unsafe { gst::set_double(api, playbin, c"volume", entry.status.volume) };
        }
        entry.shared.touch();
        Ok(())
    })
}

pub fn set_muted(id: i64, muted: bool) -> Result<(), String> {
    with(id, |entry| {
        entry.status.muted = muted;
        if let Ok((api, playbin)) = pipeline_of(entry) {
            // SAFETY: the pipeline is live.
            unsafe { gst::set_int(api, playbin, c"mute", i32::from(muted)) };
        }
        entry.shared.touch();
        Ok(())
    })
}

/// Folds the threads' news into the status and reads the position.
fn refresh(entry: &mut Entry) {
    let events = std::mem::take(&mut *entry.shared.events.lock());
    for event in events {
        match event {
            BusEvent::Paused if !matches!(entry.status.state, State::Ended | State::Failed(_)) => {
                entry.status.state = State::Paused
            }
            BusEvent::Playing => entry.status.state = State::Playing,
            BusEvent::Ended => entry.status.state = State::Ended,
            BusEvent::Failed(why) => entry.status.state = State::Failed(why),
            _ => {}
        }
    }
    {
        let frame = entry.shared.frame.lock();
        entry.status.width = frame.width;
        entry.status.height = frame.height;
        entry.status.frames = frame.count;
    }
    if let Ok((api, playbin)) = pipeline_of(entry) {
        let (mut position, mut duration) = (0i64, 0i64);
        // SAFETY: the pipeline is live.
        unsafe {
            if (api.query_position)(playbin, gst::FORMAT_TIME, &mut position) != 0 {
                entry.status.position_ms = position / 1_000_000;
            }
            if (api.query_duration)(playbin, gst::FORMAT_TIME, &mut duration) != 0 && duration >= 0
            {
                entry.status.duration_ms = duration / 1_000_000;
            }
        }
        if entry.status.state == State::Ended && entry.status.duration_ms > 0 {
            entry.status.position_ms = entry.status.duration_ms;
        }
    }
}

pub fn status(id: i64) -> Result<Status, String> {
    with(id, |entry| {
        refresh(entry);
        Ok(entry.status.clone())
    })
}

/// Reads the newest frame in place, as drawing does.
pub(crate) fn frame<T>(id: i64, f: impl FnOnce(&Frame, &Status) -> T) -> Result<T, String> {
    with(id, |entry| {
        let frame = entry.shared.frame.lock();
        Ok(f(&frame, &entry.status))
    })
}

/// Releases a player; no callback runs after this returns.
pub fn close(id: i64) {
    let entry = PLAYERS.lock().remove(&id);
    if let Some(mut entry) = entry {
        teardown(&mut entry);
    }
}

pub(crate) fn close_budget(budget: &Budget) {
    let ids: Vec<i64> = PLAYERS
        .lock()
        .iter()
        .filter(|(_, e)| e.budget == *budget)
        .map(|(id, _)| *id)
        .collect();
    for id in ids {
        close(id);
    }
}

/// Whether the session's players changed, and the state changes its
/// application hears about.
pub(crate) fn pump(budget: &Budget) -> (bool, Vec<UiMsg>) {
    let mut changed = false;
    let mut events = Vec::new();
    for (id, entry) in PLAYERS
        .lock()
        .iter_mut()
        .filter(|(_, e)| e.budget == *budget)
    {
        if !entry.shared.changed.swap(false, Ordering::AcqRel) {
            continue;
        }
        changed = true;
        refresh(entry);
        let moved = entry.status.state == State::Playing
            && (entry.status.position_ms - entry.reported_position).abs() >= TICK_MS;
        if (entry.reported.as_ref() != Some(&entry.status.state) || moved)
            && entry.status.state != State::Loading
        {
            entry.reported = Some(entry.status.state.clone());
            entry.reported_position = entry.status.position_ms;
            events.push(UiMsg::Media(*id, entry.status.clone()));
        }
    }
    (changed, events)
}

/// How often playing media reports its position, in milliseconds.
pub const TICK_MS: i64 = 250;

/// Whether any of the session's players is playing.
pub(crate) fn playing(budget: &Budget) -> bool {
    PLAYERS
        .lock()
        .values()
        .any(|e| e.budget == *budget && e.status.state == State::Playing)
}

/// Marks the session's playing players changed, so the next pump reports
/// their positions.
pub(crate) fn tick(budget: &Budget) {
    for entry in PLAYERS
        .lock()
        .values()
        .filter(|e| e.budget == *budget && e.status.state == State::Playing)
    {
        entry.shared.touch();
    }
}

#[cfg(test)]
mod tests;
