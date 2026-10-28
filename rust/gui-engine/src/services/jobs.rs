// jobs: work the toolkit keeps off the UI thread. Inside `Session::run` a
// worker goroutine answers each job by posting a message; a session stepped
// on its own answers it at the next settle, in the same order.

use crate::app::mailbox::{Cancellation, Delivery, PostError, message_bytes};
use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app::mailbox::Mailbox;
use crate::services::{apps, listing};
use crate::text::fonts;
use crate::widgets::cx::{TaskOutcome, UiMsg};

/// One piece of background work and the message that answers it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Job {
    Work(super::work::Work),
    RichLayout(crate::rich::layout_cache::Request),
    DocumentLayout(crate::text::document_layout::Request),
    SyntaxLines(crate::text::document_syntax::Request),
    FontFace(String, crate::text::catalog::FontRecord, bool),
    /// Answered by `UiMsg::Listed`.
    List(listing::ListRequest),
    /// Builds the installed-font catalog for the owner; answered by
    /// `UiMsg::FontsLoaded`.
    Fonts(String),
    /// Builds the installed-application catalog; answered by
    /// `UiMsg::AppsLoaded`.
    Apps(String),
    /// Starts an application on files; answered by `UiMsg::Task(owner,
    /// Done(name))` or `Failed(why)`.
    Launch(String, apps::AppEntry, Vec<String>),
}

/// The message that answers `job`, doing its work now.
pub fn answer(job: &Job) -> UiMsg {
    match job {
        Job::RichLayout(request) => {
            match crate::rich::layout_cache::run(request, &Context::standalone(DEFAULT_DEADLINE)) {
                Ok(()) => UiMsg::RichLayoutReady(request.owner.clone()),
                Err(error) => {
                    crate::rich::layout_cache::failed(request, &error);
                    job.failure(error)
                }
            }
        }
        Job::SyntaxLines(request) => match crate::text::document_syntax::fill(
            request,
            &Context::standalone(DEFAULT_DEADLINE),
        ) {
            Ok(()) => UiMsg::LayoutReady(request.owner.clone()),
            Err(error) => job.failure(error),
        },
        Job::DocumentLayout(request) => {
            match crate::text::document_layout::run(request, &Context::standalone(DEFAULT_DEADLINE))
            {
                Ok(()) => UiMsg::LayoutReady(request.owner.clone()),
                Err(error) => job.failure(error),
            }
        }
        Job::FontFace(owner, record, bold) => {
            match fonts::prepare_installed(record, *bold, &|| true) {
                Ok(_) => UiMsg::FontReady(record.family.clone()),
                Err(error) => UiMsg::Task(owner.clone(), TaskOutcome::Failed(error)),
            }
        }
        Job::Work(work) => work.answer(&Context::standalone(DEFAULT_DEADLINE), &mut |_, _| {}),
        Job::List(req) => UiMsg::Listed(req.owner.clone(), req.request, listing::list(req)),
        Job::Fonts(owner) => UiMsg::FontsLoaded(owner.to_string(), fonts::scan_installed()),
        Job::Apps(owner) => UiMsg::AppsLoaded(owner.to_string(), apps::installed()),
        Job::Launch(owner, entry, files) => UiMsg::Task(
            owner.to_string(),
            match apps::launch(entry, files) {
                Ok(()) => TaskOutcome::Done(entry.name.clone()),
                Err(e) => TaskOutcome::Failed(format!("{}", e)),
            },
        ),
    }
}

/// Maximum queued plus running requests, and their accounted bytes. Results
/// have the mailbox's separate byte cap. Submission never blocks the UI.
pub const QUEUE_LIMIT: usize = 64;
pub const QUEUE_BYTE_LIMIT: usize = 768 * 1024 * 1024;
pub const JOB_BYTE_LIMIT: usize = 512 * 1024 * 1024;
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(30);

impl Job {
    pub fn owner(&self) -> &str {
        match self {
            Self::Work(work) => &work.owner,
            Self::RichLayout(request) => &request.owner,
            Self::DocumentLayout(request) => &request.owner,
            Self::SyntaxLines(request) => &request.owner,
            Self::FontFace(owner, _, _) => owner,
            Self::List(req) => &req.owner,
            Self::Fonts(owner) | Self::Apps(owner) | Self::Launch(owner, _, _) => owner,
        }
    }

    // Supersession is per owner and operation. Launch requests are independent
    // side effects and must not silently cancel an earlier launch.
    fn key(&self) -> Option<(String, u64)> {
        let kind = match self {
            Self::Work(work) => work.kind(),
            Self::RichLayout(_) => 33,
            Self::DocumentLayout(_) => 18,
            Self::SyntaxLines(_) => 19,
            Self::FontFace(..) => 4,
            Self::List(_) => 0,
            Self::Fonts(_) => 1,
            Self::Apps(_) => 2,
            Self::Launch(..) => return None,
        };
        Some((self.owner().to_owned(), kind))
    }

    pub(crate) fn bytes(&self) -> Option<usize> {
        let mut bytes = message_bytes(self, JOB_BYTE_LIMIT)?;
        if let Self::DocumentLayout(request) = self {
            bytes = bytes.saturating_add(crate::text::document_layout::source_bytes(request));
        }
        if let Self::RichLayout(request) = self {
            bytes = bytes.saturating_add(crate::rich::layout_cache::bytes(request));
        }
        (bytes <= JOB_BYTE_LIMIT).then_some(bytes)
    }
    fn peak_bytes(&self) -> Option<usize> {
        let bytes = message_bytes(self, JOB_BYTE_LIMIT)?;
        Some(match self {
            Self::RichLayout(request) => bytes.max(
                crate::rich::layout_cache::bytes(request)
                    .saturating_mul(8)
                    .saturating_add(64 * 1024 * 1024)
                    .min(JOB_BYTE_LIMIT),
            ),
            Self::SyntaxLines(_) => bytes.max(64 * 1024 * 1024),
            Self::DocumentLayout(request) => {
                bytes.max(crate::text::document_layout::bytes(request))
            }
            Self::Work(work) => match &work.operation {
                super::work::Operation::ImportRich(..) | super::work::Operation::ExportRich(..) => {
                    bytes.max(256 * 1024 * 1024)
                }
                super::work::Operation::ReadRichImage(..) => bytes.max(128 * 1024 * 1024),
                super::work::Operation::ReadText(_) | super::work::Operation::ReadDocument(_) => {
                    bytes.max(super::work::TEXT_LIMIT * 2)
                }
                super::work::Operation::SaveSession(..)
                | super::work::Operation::RestoreSession(..) => bytes.max(256 * 1024 * 1024),
                super::work::Operation::SearchDocument(..)
                | super::work::Operation::ReplaceDocument(..) => bytes.max(64 * 1024 * 1024),
                super::work::Operation::AnalyzeDocument(id, _) => bytes.max(
                    crate::editing::documents::snapshot(*id)
                        .map_or(0, |(source, _)| {
                            source
                                .line_count()
                                .saturating_mul(16)
                                .saturating_add(source.max_line_bytes().saturating_mul(64))
                                .saturating_add(32 * 1024 * 1024)
                        })
                        .min(256 * 1024 * 1024),
                ),
                super::work::Operation::CompleteDocument(_, _, _, _, limit) => bytes.max(
                    (if *limit <= 0 {
                        100_000
                    } else {
                        *limit as usize
                    })
                    .saturating_mul(4096)
                    .clamp(8 * 1024 * 1024, 256 * 1024 * 1024),
                ),
                super::work::Operation::WriteDocument(id, _) => bytes.max(
                    crate::editing::documents::info(*id)
                        .map_or(0, |info| info.bytes.saturating_mul(2)),
                ),
                super::work::Operation::Analyze(text, _) => {
                    bytes.max(text.len().saturating_mul(256).min(128 * 1024 * 1024))
                }
                _ => bytes,
            },
            Self::Fonts(_) | Self::Apps(_) | Self::FontFace(..) => bytes.max(64 * 1024 * 1024),
            Self::List(_) => bytes.max(4 * 1024 * 1024),
            _ => bytes,
        })
    }

    pub(crate) fn failure(&self, why: impl ToString) -> UiMsg {
        match self {
            Self::RichLayout(request) => {
                crate::rich::layout_cache::failed(request, &why.to_string());
                UiMsg::RichLayoutReady(request.owner.clone())
            }
            Self::SyntaxLines(request) => {
                crate::text::document_syntax::failed(request);
                UiMsg::Worked(
                    request.owner.clone(),
                    -7,
                    request.revision,
                    super::work::Outcome::Failed(why.to_string()),
                )
            }
            Self::DocumentLayout(request) => {
                crate::text::document_layout::failed(request, &why.to_string());
                UiMsg::LayoutReady(request.owner.clone())
            }
            Self::Work(work) => work.message(super::work::Outcome::Failed(why.to_string())),
            Self::List(req) => UiMsg::Listed(
                req.owner.clone(),
                req.request,
                listing::Listing {
                    directory: req.directory.clone(),
                    entries: vec![],
                    total: 0,
                    error: why.to_string(),
                },
            ),
            _ => UiMsg::Task(
                self.owner().to_owned(),
                TaskOutcome::Failed(why.to_string()),
            ),
        }
    }
}

/// Cancellation and deadline shared with a running operation and its queued
/// result. Supersession invalidates results already in the mailbox too.
#[derive(Clone)]
pub struct Context {
    delivery: Delivery,
    pub(crate) budget: crate::memory::Budget,
}
impl Context {
    pub fn standalone(timeout: Duration) -> Self {
        Self {
            budget: crate::app::effects::current_budget(),
            delivery: Delivery {
                cancelled: Arc::new(Cancellation::default()),
                deadline: Instant::now() + timeout,
            },
        }
    }
    pub fn cancelled(&self) -> bool {
        self.delivery.cancelled.is_set()
    }
    pub fn check(&self) -> Result<(), String> {
        if self.cancelled() {
            Err("background work cancelled".into())
        } else if !self.active() {
            Err("background deadline exceeded".into())
        } else {
            Ok(())
        }
    }
    pub fn active(&self) -> bool {
        self.delivery.active()
    }
    pub fn cancel(&self) {
        self.delivery.cancelled.cancel();
    }
}

struct Request {
    job: Job,
    context: Context,
    bytes: usize,
    _lease: crate::memory::Lease,
}
struct CachedAnalysis {
    document: Arc<crate::text::syntax::Document>,
    _lease: crate::memory::Lease,
}

struct State {
    queue: VecDeque<Request>,
    active: Option<(String, Context)>,
    latest: HashMap<(String, u64), Context>,
    bytes: usize,
    output_bytes: usize,
    stopped: bool,
    sequence: u64,
    /// Times the worker has blocked waiting for a request.
    parks: u64,
    analyses: crate::memory::ByteCache<String, Arc<CachedAnalysis>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            active: None,
            latest: HashMap::new(),
            bytes: 0,
            output_bytes: 0,
            stopped: false,
            sequence: 0,
            parks: 0,
            analyses: crate::memory::ByteCache::new(64 * 1024 * 1024),
        }
    }
}
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
    /// Signalled each time the worker blocks with nothing to do.
    idle: Condvar,
    budget: crate::memory::Budget,
    registry: Mutex<Option<crate::memory::Lease>>,
}
impl Default for Shared {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            ready: Condvar::new(),
            idle: Condvar::new(),
            budget: crate::memory::Budget::new(QUEUE_BYTE_LIMIT * 2),
            registry: Mutex::new(None),
        }
    }
}

/// A bounded worker owned by one session. Idle workers sleep on a condition
/// variable. Drop cancels all work and joins the thread before releasing it.
pub struct Worker {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SubmitError {
    #[error("the background queue is full")]
    Full,
    #[error("the background request exceeds its byte limit")]
    TooLarge,
    #[error("the background service has stopped")]
    Stopped,
}

impl Worker {
    pub fn new(mail: Mailbox) -> std::io::Result<Self> {
        let budget = mail.budget();
        let registry = budget.reserve(1024 * 1024).map_err(std::io::Error::other)?;
        let shared = Arc::new(Shared {
            budget,
            registry: Mutex::new(Some(registry)),
            ..Shared::default()
        });
        let runner = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("gui-jobs".into())
            .spawn(move || run_worker(&runner, &mail))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub fn submit(&self, job: &Job, timeout: Duration) -> Result<Context, SubmitError> {
        if job.owner().len() > 4096 {
            return Err(SubmitError::TooLarge);
        }
        let bytes = job.bytes().ok_or(SubmitError::TooLarge)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(SubmitError::TooLarge)?;
        let mut state = self.shared.state.lock();
        if state.stopped {
            return Err(SubmitError::Stopped);
        }
        // Once a result has been consumed, only this registry holds its token.
        state
            .latest
            .retain(|_, context| Arc::strong_count(&context.delivery.cancelled) > 1);
        let key = job.key();
        let replaced_bytes: usize = state
            .queue
            .iter()
            .filter(|r| key.is_some() && r.job.key() == key)
            .map(|r| r.bytes)
            .sum();
        let replaced_count = state
            .queue
            .iter()
            .filter(|r| key.is_some() && r.job.key() == key)
            .count();
        if state.queue.len() - replaced_count + usize::from(state.active.is_some()) >= QUEUE_LIMIT
            || state.bytes - replaced_bytes + bytes > QUEUE_BYTE_LIMIT
            || (key
                .as_ref()
                .is_none_or(|key| !state.latest.contains_key(key))
                && state.latest.len() >= QUEUE_LIMIT)
        {
            return Err(SubmitError::Full);
        }
        let lease = self
            .shared
            .budget
            .reserve(bytes)
            .map_err(|_| SubmitError::Full)?;
        let context = Context {
            budget: self.shared.budget.clone(),
            delivery: Delivery {
                cancelled: Arc::new(Cancellation::default()),
                deadline,
            },
        };
        let supersedes = key.is_some();
        let key = key.unwrap_or_else(|| {
            state.sequence = state
                .sequence
                .checked_add(1)
                .expect("job sequence exhausted");
            (job.owner().to_owned(), state.sequence + 3)
        });
        if let Some(old) = state.latest.insert(key.clone(), context.clone()) {
            old.cancel();
        }
        if supersedes {
            state.queue.retain(|r| r.job.key().as_ref() != Some(&key));
            state.bytes -= replaced_bytes;
        }
        state.bytes += bytes;
        let request = Request {
            job: job.clone(),
            context: context.clone(),
            bytes,
            _lease: lease,
        };
        if matches!(
            job,
            Job::DocumentLayout(_) | Job::SyntaxLines(_) | Job::RichLayout(_)
        ) {
            state.queue.push_front(request);
        } else {
            state.queue.push_back(request);
        }
        self.shared.ready.notify_one();
        Ok(context)
    }

    pub fn cancel_owner(&self, owner: &str) {
        crate::rich::layout_cache::release_owner(&self.shared.budget, owner);
        let mut state = self.shared.state.lock();
        for ((key, _), context) in &state.latest {
            if key == owner {
                context.cancel();
            }
        }
        if let Some((key, context)) = &state.active
            && key == owner
        {
            context.cancel();
        }
        let mut removed = 0;
        state.queue.retain(|r| {
            if r.job.owner() == owner {
                r.context.cancel();
                removed += r.bytes;
                false
            } else {
                true
            }
        });
        state.bytes -= removed;
        state.latest.retain(|(key, _), _| key != owner);
        state.analyses.remove(&owner.to_owned());
    }

    /// Queued plus running request count and charged bytes.
    pub fn usage(&self) -> (usize, usize) {
        let state = self.shared.state.lock();
        (
            state.queue.len() + usize::from(state.active.is_some()),
            state.bytes.saturating_add(state.output_bytes),
        )
    }

    pub fn shutdown(&mut self) {
        {
            let mut state = self.shared.state.lock();
            state.stopped = true;
            for context in state.latest.values() {
                context.cancel();
            }
            if let Some((_, context)) = &state.active {
                context.cancel();
            }
            for r in &state.queue {
                r.context.cancel();
            }
            let removed: usize = state.queue.iter().map(|r| r.bytes).sum();
            state.bytes -= removed;
            state.queue = VecDeque::new();
            state.latest = HashMap::new();
            state.analyses.clear();
            self.shared.ready.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.shared.registry.lock().take();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_worker(shared: &Shared, mail: &Mailbox) {
    run_worker_with(shared, mail, |job, context| {
        let mut last = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let mut progress = |done, total| {
            if context.active() && last.elapsed() >= Duration::from_millis(50) {
                let message = match job {
                    Job::Work(work) => work.message(super::work::Outcome::Progress(done, total)),
                    _ => UiMsg::Worked(
                        job.owner().to_owned(),
                        0,
                        0,
                        super::work::Outcome::Progress(done, total),
                    ),
                };
                let _ = mail.try_post_guarded(&message, &context.delivery);
                last = Instant::now();
            }
        };
        match job {
            Job::RichLayout(request) => match crate::rich::layout_cache::run(request, context) {
                Ok(()) => UiMsg::RichLayoutReady(request.owner.clone()),
                Err(error) => {
                    crate::rich::layout_cache::failed(request, &error);
                    job.failure(error)
                }
            },
            Job::SyntaxLines(request) => match crate::text::document_syntax::fill(request, context)
            {
                Ok(()) => UiMsg::LayoutReady(request.owner.clone()),
                Err(error) => job.failure(error),
            },
            Job::DocumentLayout(request) => {
                match crate::text::document_layout::run(request, context) {
                    Ok(()) => UiMsg::LayoutReady(request.owner.clone()),
                    Err(error) => job.failure(error),
                }
            }
            Job::Work(work) => {
                let previous = shared.state.lock().analyses.get(&work.owner).cloned();
                let result = work.answer_with_previous(
                    context,
                    previous.as_ref().map(|cached| cached.document.as_ref()),
                    &mut progress,
                );
                if context.active()
                    && let UiMsg::Worked(_, _, _, super::work::Outcome::Analysis(document)) =
                        &result
                {
                    let bytes = document.owned_bytes();
                    if let Ok(lease) = shared.budget.reserve(bytes) {
                        let cached = Arc::new(CachedAnalysis {
                            document: document.clone(),
                            _lease: lease,
                        });
                        let mut state = shared.state.lock();
                        // Cancellation and cache insertion share this lock. An owner
                        // removed during analysis cannot regain retained state.
                        if !state.stopped && context.active() {
                            state.analyses.insert(work.owner.clone(), cached, bytes);
                        }
                    }
                }
                result
            }
            Job::List(req) => UiMsg::Listed(
                req.owner.clone(),
                req.request,
                listing::list_cancellable(req, || context.active()),
            ),
            Job::FontFace(_, record, bold) => {
                match fonts::prepare_installed(record, *bold, &|| context.active()) {
                    Ok(_) => UiMsg::FontReady(record.family.clone()),
                    Err(error) => job.failure(error),
                }
            }
            Job::Fonts(owner) => {
                match fonts::scan_installed_cancellable(&|| context.active(), &mut progress) {
                    Ok(records) => UiMsg::FontsLoaded(owner.clone(), records),
                    Err(why) => job.failure(why),
                }
            }
            Job::Apps(owner) => {
                match apps::installed_cancellable(&|| context.active(), &mut progress) {
                    Ok(entries) => UiMsg::AppsLoaded(owner.clone(), entries),
                    Err(why) => job.failure(why),
                }
            }
            _ => answer(job),
        }
    });
}

fn run_worker_with(
    shared: &Shared,
    mail: &Mailbox,
    mut answer: impl FnMut(&Job, &Context) -> UiMsg,
) {
    loop {
        let request = {
            let mut state = shared.state.lock();
            while state.queue.is_empty() && !state.stopped {
                state.parks += 1;
                shared.idle.notify_all();
                shared.ready.wait(&mut state);
            }
            if state.stopped {
                return;
            }
            let request = state.queue.pop_front().unwrap();
            state.active = Some((request.job.owner().to_owned(), request.context.clone()));
            request
        };
        if !request.context.delivery.cancelled.is_set() {
            let peak = request
                .job
                .peak_bytes()
                .unwrap_or(JOB_BYTE_LIMIT)
                .saturating_sub(request.bytes);
            let reservation = shared.budget.reserve(peak);
            shared.state.lock().output_bytes = reservation.as_ref().map_or(0, |_| peak);
            let result = if let Err(error) = &reservation {
                request.job.failure(error)
            } else if request.context.active() {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    answer(&request.job, &request.context)
                }))
                .unwrap_or_else(|_| request.job.failure("background operation panicked"))
            } else {
                request.job.failure("background deadline exceeded")
            };
            let result = if Instant::now() >= request.context.delivery.deadline {
                request.job.failure("background deadline exceeded")
            } else {
                result
            };
            // The work deadline decides success above. Give terminal status its
            // own delivery interval, still invalidated by cancellation or a new
            // request, so expiry does not leave the application waiting forever.
            let delivery = Delivery {
                cancelled: Arc::clone(&request.context.delivery.cancelled),
                deadline: Instant::now() + DEFAULT_DEADLINE,
            };
            if mail.post_guarded(&result, &delivery) == Err(PostError::TooLarge) {
                let failure = request
                    .job
                    .failure("background result exceeds the mailbox byte limit");
                let _ = mail.post_guarded(&failure, &delivery);
            }
        }
        let mut state = shared.state.lock();
        state.bytes -= request.bytes;
        state.active = None;
        state.output_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::mailbox::Inbox;
    use std::sync::mpsc;

    fn job(owner: &str, revision: i64) -> Job {
        Job::List(listing::ListRequest::new(owner, revision, "."))
    }

    #[test]
    fn bounded_queue_supersedes_inflight_and_queued_work_and_cleans_up() {
        let mut session = crate::start("Job races", "test.jobs-races").unwrap();
        let mut inbox = Inbox::new();
        let mail = inbox.mailbox(session.waker());
        let shared = Arc::new(Shared::default());
        let runner = Arc::clone(&shared);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            run_worker_with(&runner, &mail, |job, _| {
                if matches!(job, Job::List(req) if req.request == 1) {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                }
                job.failure("completed")
            })
        });
        let mut worker = Worker {
            shared,
            thread: Some(thread),
        };
        let old = worker.submit(&job("owner", 1), DEFAULT_DEADLINE).unwrap();
        started_rx.recv().unwrap();
        let replaced = worker.submit(&job("owner", 2), DEFAULT_DEADLINE).unwrap();
        let newest = worker.submit(&job("owner", 3), DEFAULT_DEADLINE).unwrap();
        assert!(!old.active() && !replaced.active() && newest.active());
        assert_eq!(worker.usage().0, 2);
        for owner in 0..QUEUE_LIMIT - 2 {
            worker
                .submit(&job(&format!("other-{owner}"), 4), DEFAULT_DEADLINE)
                .unwrap();
        }
        assert!(matches!(
            worker.submit(&job("overflow", 4), DEFAULT_DEADLINE),
            Err(SubmitError::Full)
        ));
        assert!(worker.usage().1 <= QUEUE_BYTE_LIMIT);
        worker.cancel_owner("owner");
        assert!(!newest.active());
        release_tx.send(()).unwrap();
        worker.shutdown();
        assert_eq!(worker.usage(), (0, 0));
        assert!(matches!(
            worker.submit(&job("later", 4), DEFAULT_DEADLINE),
            Err(SubmitError::Stopped)
        ));
        assert!(
            inbox
                .drain()
                .iter()
                .all(|m| !matches!(m, UiMsg::Listed(owner, _, _) if owner == "owner"))
        );
        session.shutdown();
    }

    #[test]
    fn result_already_in_mailbox_is_cancelled_when_a_new_revision_arrives() {
        let mut session = crate::start("Stale job", "test.jobs-stale").unwrap();
        let mut inbox = Inbox::new();
        let mail = inbox.mailbox(session.waker());
        let shared = Arc::new(Shared::default());
        let runner = Arc::clone(&shared);
        let (done_tx, done_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            run_worker_with(&runner, &mail, |job, _| {
                done_tx.send(()).unwrap();
                job.failure("completed")
            })
        });
        let mut worker = Worker {
            shared,
            thread: Some(thread),
        };
        worker.submit(&job("owner", 1), DEFAULT_DEADLINE).unwrap();
        done_rx.recv().unwrap();
        // Wait for posting using a bounded deadline, independent of scheduling.
        let until = Instant::now() + Duration::from_secs(5);
        while inbox.usage().0 == 0 && Instant::now() < until {
            std::thread::yield_now();
        }
        assert_eq!(inbox.usage().0, 1);
        let current = worker.submit(&job("owner", 2), DEFAULT_DEADLINE).unwrap();
        let messages = inbox.drain();
        assert!(!messages.iter().any(|m| matches!(m, UiMsg::Listed(_, 1, _))));
        current.cancel();
        worker.shutdown();
        session.shutdown();
    }

    #[test]
    fn oversized_requests_are_rejected_before_cloning() {
        let mut session = crate::start("Job sizes", "test.jobs-sizes").unwrap();
        let mut worker = Worker::new(session.mailbox()).unwrap();
        assert!(matches!(
            worker.submit(&Job::Fonts("x".repeat(JOB_BYTE_LIMIT)), DEFAULT_DEADLINE),
            Err(SubmitError::TooLarge)
        ));
        assert_eq!(worker.usage(), (0, 0));
        worker.shutdown();
        session.shutdown();
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    use crate::app::mailbox::Inbox;

    #[test]
    fn an_expired_request_delivers_a_terminal_failure_without_running_the_provider() {
        let mut session = crate::start("Deadline", "test.jobs-deadline").unwrap();
        let mut inbox = Inbox::new();
        let mail = inbox.mailbox(session.waker());
        let shared = Arc::new(Shared::default());
        // Queue before starting the worker so there is no timing dependency.
        let mut worker = Worker {
            shared: Arc::clone(&shared),
            thread: None,
        };
        let request = Job::List(listing::ListRequest::new("expired", 7, "."));
        worker.submit(&request, Duration::ZERO).unwrap();
        worker.thread = Some(std::thread::spawn(move || {
            run_worker_with(&shared, &mail, |_, _| panic!("expired provider ran"))
        }));
        let until = Instant::now() + Duration::from_secs(5);
        while inbox.usage().0 == 0 && Instant::now() < until {
            std::thread::yield_now();
        }
        assert!(
            matches!(inbox.drain().as_slice(), [UiMsg::Listed(_, 7, result)] if result.error == "background deadline exceeded")
        );
        worker.shutdown();
        session.shutdown();
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    use crate::app::mailbox::{Inbox, PostError};
    #[test]
    fn shutdown_interrupts_a_worker_waiting_for_a_saturated_mailbox() {
        let mut session = crate::start("Backpressure shutdown", "test.jobs-backpressure").unwrap();
        let mut inbox = Inbox::new();
        let mail = inbox.mailbox(session.waker());
        let message = UiMsg::Custom("fill".into(), String::new());
        while mail.try_post(&message).is_ok() {}
        assert_eq!(mail.try_post(&message), Err(PostError::Full));
        let mut worker = Worker::new(mail).unwrap();
        let job = Job::List(listing::ListRequest::new("blocked", 1, "."));
        worker.submit(&job, DEFAULT_DEADLINE).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while worker.shared.state.lock().active.is_none() && Instant::now() < until {
            std::thread::yield_now();
        }
        assert!(worker.shared.state.lock().active.is_some());
        let start = Instant::now();
        worker.shutdown();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(worker.usage(), (0, 0));
        assert!(
            !inbox
                .drain()
                .iter()
                .any(|m| matches!(m, UiMsg::Listed(owner, _, _) if owner == "blocked"))
        );
        session.shutdown();
    }
}

#[cfg(test)]
mod idle_tests {
    use super::*;
    #[test]
    fn empty_worker_does_no_provider_work_and_shutdown_releases_reservations() {
        let mut session = crate::start("Idle worker", "test.jobs-idle").unwrap();
        let budget = session.mailbox().budget();
        let before = budget.usage().0;
        let mut worker = Worker::new(session.mailbox()).unwrap();
        let mut state = worker.shared.state.lock();
        while state.parks == 0 {
            worker.shared.idle.wait(&mut state);
        }
        // A worker with no requests blocks once and is never woken to poll.
        assert_eq!(state.parks, 1);
        assert!(state.active.is_none() && state.queue.is_empty() && state.latest.is_empty());
        assert_eq!(state.bytes, 0);
        drop(state);
        worker.shutdown();
        assert_eq!(budget.usage().0, before);
        session.shutdown();
    }
}

#[cfg(test)]
mod native_result_tests {
    use super::*;
    use crate::app::mailbox::Inbox;
    #[test]
    fn cancelling_an_already_posted_native_document_releases_its_handle_and_bytes() {
        let mut session = crate::start("Native result disposal", "test.native-result").unwrap();
        let budget = session.mailbox().budget();
        let mut inbox = Inbox::with_budget(budget.clone());
        let mut worker = Worker::new(inbox.mailbox(session.waker())).unwrap();
        let baseline = budget.usage().0;
        let path =
            std::env::temp_dir().join(format!("gui-native-cancel-{}.txt", std::process::id()));
        std::fs::write(&path, "native snapshot").unwrap();
        let cleanup = scopeguard::guard(path, |path| {
            let _ = std::fs::remove_file(path);
        });
        let job = Job::Work(super::super::work::Work {
            owner: "reader".into(),
            request: 1,
            revision: 0,
            timeout_ms: 10000,
            operation: super::super::work::Operation::ReadDocument(
                cleanup.to_string_lossy().into_owned(),
            ),
        });
        worker.submit(&job, Duration::from_secs(10)).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while (inbox.usage().0 == 0 || worker.usage().0 != 0) && Instant::now() < until {
            std::thread::yield_now();
        }
        assert!(inbox.usage().0 > 0);
        assert!(budget.usage().0 > baseline);
        worker.cancel_owner("reader");
        assert!(inbox.drain().is_empty());
        assert_eq!(budget.usage().0, baseline);
        worker.shutdown();
        session.shutdown();
    }
}
