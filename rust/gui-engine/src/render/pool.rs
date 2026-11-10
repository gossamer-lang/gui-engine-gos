//! Raster workers that stay parked between frames, so a frame's parts run
//! side by side without starting a thread each.

use std::collections::VecDeque;
use std::panic::{self, AssertUnwindSafe};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex, MutexGuard};

type Task = Box<dyn FnOnce() + Send + 'static>;

/// Workers besides the calling thread.
const WORKERS: usize = 15;

#[derive(Default)]
struct Pool {
    state: Mutex<State>,
    ready: Condvar,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Task>,
    workers: usize,
}

/// Counts the tasks of one `scope` still running.
struct Latch {
    left: Mutex<usize>,
    done: Condvar,
}

impl Latch {
    fn count_down(&self) {
        let mut left = self.left.lock();
        *left -= 1;
        if *left == 0 {
            self.done.notify_all();
        }
    }

    fn wait(&self) {
        let mut left = self.left.lock();
        while *left > 0 {
            self.done.wait(&mut left);
        }
    }
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(Pool::default)
}

/// The threads the machine runs at once, as the process first found it.
/// Asking the system reads files on Linux (the cgroup's CPU quota), too
/// slow to repeat for every frame.
pub(crate) fn parallelism() -> usize {
    static PARALLELISM: OnceLock<usize> = OnceLock::new();
    *PARALLELISM.get_or_init(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
}

/// Runs every task, the first on the calling thread and the others on the
/// pool's workers, answering their results in order once all have finished.
/// A panic in a task resumes on the caller after every task has finished.
pub(crate) fn scope<T: Send, F: FnOnce() -> T + Send>(tasks: Vec<F>) -> Vec<T> {
    let mut tasks = tasks.into_iter();
    let Some(first) = tasks.next() else {
        return Vec::new();
    };
    let rest: Vec<F> = tasks.collect();
    if rest.is_empty() {
        return vec![first()];
    }
    let slots: Vec<Mutex<Option<std::thread::Result<T>>>> =
        rest.iter().map(|_| Mutex::new(None)).collect();
    let latch = Latch {
        left: Mutex::new(rest.len()),
        done: Condvar::new(),
    };
    let pool = pool();
    {
        let mut state = pool.state.lock();
        for (task, slot) in rest.into_iter().zip(&slots) {
            let latch = &latch;
            let job: Box<dyn FnOnce() + Send + '_> = Box::new(move || {
                *slot.lock() = Some(panic::catch_unwind(AssertUnwindSafe(task)));
                latch.count_down();
            });
            // SAFETY: this function waits on `latch` before it returns or
            // unwinds, and the job counts it down last, so every borrow the job
            // holds outlives the job.
            let job: Task = unsafe { std::mem::transmute(job) };
            state.queue.push_back(job);
        }
        grow(&mut state, pool);
    }
    for _ in 1..tasks_queued(&slots) {
        pool.ready.notify_one();
    }
    let first = panic::catch_unwind(AssertUnwindSafe(first));
    // The caller takes queued parts too, so the scope finishes even when no
    // worker could start.
    loop {
        let job = pool.state.lock().queue.pop_front();
        match job {
            Some(job) => job(),
            None => break,
        }
    }
    latch.wait();
    let mut out = Vec::with_capacity(slots.len() + 1);
    let mut failure = None;
    for result in std::iter::once(first).chain(slots.into_iter().map(|slot| {
        slot.into_inner()
            .unwrap_or_else(|| Err(Box::new("a raster part did not run")))
    })) {
        match result {
            Ok(value) => out.push(value),
            Err(panic) => failure = failure.or(Some(panic)),
        }
    }
    if let Some(panic) = failure {
        panic::resume_unwind(panic);
    }
    out
}

// One wake per queued part, besides the one the caller runs.
fn tasks_queued<T>(slots: &[T]) -> usize {
    slots.len() + 1
}

// Starts workers until the queued parts each have one, up to the limit.
fn grow(state: &mut MutexGuard<'_, State>, pool: &'static Pool) {
    while state.workers < state.queue.len().min(WORKERS) {
        let started = std::thread::Builder::new()
            .name("gui-raster".into())
            .spawn(move || work(pool));
        if started.is_err() {
            return;
        }
        state.workers += 1;
    }
}

/// How long a worker waits for work before it ends, handing back its stack
/// and heap; output that streams keeps the workers it needs busy.
const LINGER: Duration = Duration::from_secs(5);

fn work(pool: &'static Pool) {
    let mut state = pool.state.lock();
    let mut idle_since = Instant::now();
    loop {
        if let Some(job) = state.queue.pop_front() {
            MutexGuard::unlocked(&mut state, job);
            idle_since = Instant::now();
            continue;
        }
        let left = LINGER.saturating_sub(idle_since.elapsed());
        if left.is_zero() {
            state.workers -= 1;
            return;
        }
        // Raster parts free their scratch rows, which stay in this thread's
        // heap until it hands them back.
        let due = MutexGuard::unlocked(&mut state, windowing::idle::thread_idle);
        if !state.queue.is_empty() {
            continue;
        }
        pool.ready
            .wait_for(&mut state, due.map_or(left, |due| due.min(left)));
    }
}

#[cfg(test)]
mod tests {
    use super::scope;

    #[test]
    fn scope_answers_every_result_in_order() {
        let mut parts = vec![0_u64; 64];
        let tasks: Vec<_> = parts
            .chunks_mut(4)
            .enumerate()
            .map(|(i, chunk)| {
                move || {
                    chunk.fill(i as u64);
                    i
                }
            })
            .collect();
        assert_eq!(scope(tasks), (0..16).collect::<Vec<_>>());
        assert!(
            parts
                .chunks(4)
                .enumerate()
                .all(|(i, c)| c.iter().all(|&v| v == i as u64))
        );
    }

    #[test]
    fn a_panicking_part_resumes_after_the_others_finish() {
        let done = std::sync::atomic::AtomicUsize::new(0);
        let tasks: Vec<Box<dyn FnOnce() + Send + '_>> = (0..8)
            .map(|i| {
                let done = &done;
                Box::new(move || {
                    if i == 3 {
                        panic!("part 3");
                    }
                    done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }) as Box<dyn FnOnce() + Send + '_>
            })
            .collect();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scope(tasks)));
        assert!(caught.is_err());
        assert_eq!(done.load(std::sync::atomic::Ordering::SeqCst), 7);
    }
}
