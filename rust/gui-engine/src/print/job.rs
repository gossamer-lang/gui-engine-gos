//! Background print and export jobs. Pagination, PDF output and spooling run
//! on a thread of their own, check cancellation between pages, and answer
//! the session through its mailbox as `UiMsg::Print(job, event)`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::{Item, PageSetup, native};
use crate::app::mailbox::{Cancellation, Delivery, Mailbox};
use crate::memory::Budget;
use crate::widgets::cx::UiMsg;

const JOB_LIMIT: usize = 8;
const STACK: usize = 8 * 1024 * 1024;

/// What a job prints: items laid out on the job's thread, or the pages an
/// open print document holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Source {
    Items(Vec<Item>, PageSetup),
    Document(i64),
}

/// Where a job's pages go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Target {
    /// A PDF file at this path.
    Pdf(String),
    Printer(native::Options),
}

/// What a job reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// The document was laid out on this many pages.
    Paginated(i64),
    /// The PDF was written to this path with this many bytes.
    Exported {
        path: String,
        bytes: i64,
    },
    /// The printer's spooler accepted the job under this id.
    Submitted {
        job: String,
        pages: i64,
    },
    Failed(String),
    Cancelled,
}

struct Entry {
    cancelled: Arc<AtomicBool>,
    /// The spooler's server and job id, once it accepted the job.
    spooled: Arc<Mutex<Option<(String, String)>>>,
    budget: Budget,
    thread: Option<JoinHandle<()>>,
}

static NEXT: AtomicI64 = AtomicI64::new(1);
static JOBS: LazyLock<Mutex<HashMap<i64, Entry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn answer(mailbox: &Mailbox, job: i64, event: Event) {
    let delivery = Delivery {
        cancelled: Arc::new(Cancellation::default()),
        deadline: Instant::now() + Duration::from_secs(60),
    };
    let _ = mailbox.post_guarded(&UiMsg::Print(job, event), &delivery);
}

/// Starts laying out `items` and sending them to `target` in the running
/// session, answering the job's id.
pub fn start(source: Source, title: String, target: Target) -> Result<i64, String> {
    let mailbox =
        crate::app::effects::current_mailbox().ok_or("printing needs a running session")?;
    if let Target::Printer(options) = &target {
        options.check()?;
    }
    if let Target::Pdf(path) = &target
        && (path.is_empty() || path.contains('\0'))
    {
        return Err("the PDF needs a file path".into());
    }
    let budget = crate::app::effects::current_budget();
    let source = match source {
        Source::Items(items, setup) => Prepared::Items(items, setup),
        Source::Document(id) => {
            let (pages, setup) = super::document::pages(id)?;
            Prepared::Pages(pages, setup)
        }
    };
    let lease = budget.reserve(
        crate::memory::measure(&source)
            .saturating_mul(4)
            .saturating_add(64 * 1024 * 1024),
    )?;
    let mut jobs = JOBS.lock();
    if jobs.len() >= JOB_LIMIT {
        return Err("too many print jobs are running".into());
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let cancelled = Arc::new(AtomicBool::new(false));
    let spooled = Arc::new(Mutex::new(None));
    let (stop, spool, thread_budget) = (cancelled.clone(), spooled.clone(), budget.clone());
    let thread = std::thread::Builder::new()
        .name("gui-print".into())
        .stack_size(STACK)
        .spawn(move || {
            let _lease = lease;
            crate::app::effects::set_budget(thread_budget);
            let is_cancelled = || stop.load(Ordering::Acquire);
            let result = run(
                id,
                &mailbox,
                &source,
                &title,
                &target,
                &is_cancelled,
                &spool,
            );
            let event = match result {
                Ok(event) => event,
                Err(_) if is_cancelled() => Event::Cancelled,
                Err(error) => Event::Failed(error),
            };
            JOBS.lock().remove(&id);
            answer(&mailbox, id, event);
        })
        .map_err(|e| e.to_string())?;
    jobs.insert(
        id,
        Entry {
            cancelled,
            spooled,
            budget,
            thread: Some(thread),
        },
    );
    Ok(id)
}

#[derive(Serialize)]
enum Prepared {
    Items(Vec<Item>, PageSetup),
    Pages(Arc<Vec<super::Page>>, PageSetup),
}

fn run(
    id: i64,
    mailbox: &Mailbox,
    source: &Prepared,
    title: &str,
    target: &Target,
    cancelled: &dyn Fn() -> bool,
    spooled: &Mutex<Option<(String, String)>>,
) -> Result<Event, String> {
    let (pages, setup) = match source {
        Prepared::Items(items, setup) => {
            (Arc::new(super::paginate(items, setup, cancelled)?), setup)
        }
        Prepared::Pages(pages, setup) => (pages.clone(), setup),
    };
    answer(mailbox, id, Event::Paginated(pages.len() as i64));
    match target {
        Target::Pdf(path) => {
            let bytes = super::pdf::write(&pages, title, cancelled)?;
            if cancelled() {
                return Err("export cancelled".into());
            }
            std::fs::write(path, &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(Event::Exported {
                path: path.clone(),
                bytes: bytes.len() as i64,
            })
        }
        Target::Printer(options) => {
            let media = setup.paper.media();
            let job = native::submit(
                &pages,
                || super::pdf::write(&pages, title, cancelled),
                &media,
                options,
                cancelled,
            )?;
            *spooled.lock() = Some((options.server.clone(), job.clone()));
            // A cancellation that raced the spooler withdraws the job it made.
            if cancelled() {
                let _ = native::cancel(&options.server, &job);
                return Err("printing cancelled".into());
            }
            Ok(Event::Submitted {
                job,
                pages: pages.len() as i64,
            })
        }
    }
}

/// Cancels job `id`: layout and output stop at the next page, and a job the
/// spooler already holds is withdrawn.
pub fn cancel(id: i64) -> Result<(), String> {
    let spooled = {
        let jobs = JOBS.lock();
        let entry = jobs.get(&id).ok_or("the print job has finished")?;
        entry.cancelled.store(true, Ordering::Release);
        entry.spooled.lock().clone()
    };
    match spooled {
        Some((server, job)) => native::cancel(&server, &job),
        None => Ok(()),
    }
}

/// Withdraws a job the spooler of `server` accepted earlier.
pub fn withdraw(server: &str, spooled: &str) -> Result<(), String> {
    native::cancel(server, spooled)
}

/// Cancels and joins every job a session started.
pub(crate) fn close_budget(budget: &Budget) {
    let threads: Vec<JoinHandle<()>> = {
        let mut jobs = JOBS.lock();
        jobs.values_mut()
            .filter(|entry| entry.budget == *budget)
            .filter_map(|entry| {
                entry.cancelled.store(true, Ordering::Release);
                entry.thread.take()
            })
            .collect()
    };
    for thread in threads {
        let _ = thread.join();
    }
}
