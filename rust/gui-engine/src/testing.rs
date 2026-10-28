// SDL takes the first thread that starts it as the main thread and expects
// video on that thread from then on; on macOS a mismatch is an assertion.
// libtest runs each test on a thread of its own, so every test that drives
// a session runs its body on one long-lived UI thread instead.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel, sync_channel};
use std::thread::{self, ThreadId};

use parking_lot::Mutex;

type Job = Box<dyn FnOnce() + Send>;

struct UiThread {
    id: ThreadId,
    jobs: Mutex<Sender<Job>>,
}

static UI_THREAD: OnceLock<UiThread> = OnceLock::new();

fn ui_thread() -> &'static UiThread {
    UI_THREAD.get_or_init(|| {
        let (jobs, queue) = channel::<Job>();
        let handle = thread::Builder::new()
            .name("gui-test-ui".into())
            .spawn(move || {
                while let Ok(job) = queue.recv() {
                    job();
                }
            })
            .expect("spawning the test UI thread");
        UiThread {
            id: handle.thread().id(),
            jobs: Mutex::new(jobs),
        }
    })
}

/// Whether the caller is the UI thread that test sessions run on.
pub fn is_ui_thread() -> bool {
    UI_THREAD
        .get()
        .is_some_and(|ui| ui.id == thread::current().id())
}

/// Runs `body` on the test UI thread and answers its result; a panic in
/// `body` is raised again on the caller's thread with the same payload.
pub fn on_ui_thread<R: Send + 'static>(body: impl FnOnce() -> R + Send + 'static) -> R {
    if is_ui_thread() {
        return body();
    }
    let (done, outcome) = sync_channel(1);
    let job: Job = Box::new(move || {
        let _ = done.send(catch_unwind(AssertUnwindSafe(body)));
    });
    ui_thread()
        .jobs
        .lock()
        .send(job)
        .expect("the test UI thread runs for the whole process");
    match outcome
        .recv()
        .expect("the test UI thread answers every job it takes")
    {
        Ok(value) => value,
        // A message panics anew so the caller's test output shows it.
        Err(payload) => match payload.downcast::<String>() {
            Ok(message) => panic!("{message}"),
            Err(payload) => match payload.downcast::<&str>() {
                Ok(message) => panic!("{message}"),
                Err(payload) => resume_unwind(payload),
            },
        },
    }
}
