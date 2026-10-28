// Bounded, nonblocking mail from other threads into a session. The inbox owns
// closure of the queue, so surviving mailbox clones cannot retain new messages
// after shutdown or wake a finalized platform session.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

use crate::widgets::cx::UiMsg;
use parking_lot::{Condvar, Mutex};

const MESSAGE_LIMIT: usize = 256;
const BYTE_LIMIT: usize = 256 * 1024 * 1024;
const ENTRY_LIMIT: usize = 128 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Delivery {
    pub cancelled: Arc<Cancellation>,
    pub deadline: Instant,
}

impl Delivery {
    pub fn active(&self) -> bool {
        !self.cancelled.is_set() && Instant::now() < self.deadline
    }
}

/// A cancellation flag that also wakes every sender blocked on mailbox space
/// for the delivery it guards.
#[derive(Default)]
pub(crate) struct Cancellation {
    flag: AtomicBool,
    waiting: Mutex<Vec<Weak<Shared>>>,
}

impl Cancellation {
    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        let waiting = std::mem::take(&mut *self.waiting.lock());
        for shared in waiting.iter().filter_map(Weak::upgrade) {
            // Taking the queue lock orders this wake after the sender's check.
            let _queue = shared.queue.lock();
            shared.space.notify_all();
        }
    }
}

struct Envelope {
    msg: UiMsg,
    bytes: usize,
    delivery: Option<Delivery>,
    lease: crate::memory::Lease,
}

#[derive(Default)]
struct Queue {
    entries: VecDeque<Envelope>,
    bytes: usize,
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    space: Condvar,
    budget: crate::memory::Budget,
}
impl Shared {
    fn new(budget: crate::memory::Budget) -> Self {
        Self {
            queue: Mutex::new(Queue::default()),
            space: Condvar::new(),
            budget,
        }
    }
}

/// Why a mailbox could not accept a message. Callers can retry `Full` after
/// allowing the UI to drain. `TooLarge` requires a smaller message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PostError {
    #[error("the mailbox is full")]
    Full,
    #[error("the message exceeds the mailbox byte limit")]
    TooLarge,
    #[error("the session has ended")]
    Closed,
}

// Count serialized bytes without allocating a second copy of the message.
// Eight times this size plus envelope storage conservatively accounts for
// cloned strings/vectors, including small and empty catalog entries.
pub(crate) fn message_bytes(msg: &impl serde::Serialize, limit: usize) -> Option<usize> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes = self.bytes.saturating_add(buf.len());
            if self.bytes > self.limit {
                return Err(std::io::Error::other("message too large"));
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter {
        bytes: 0,
        limit: limit.saturating_sub(512) / 8,
    };
    serde_json::to_writer(&mut counter, msg).ok()?;
    Some(counter.bytes * 8 + 512)
}

/// The posting end, safe to clone into any thread.
#[derive(Clone)]
pub struct Mailbox {
    shared: Arc<Shared>,
    waker: windowing::Waker,
}

impl Mailbox {
    pub(crate) fn budget(&self) -> crate::memory::Budget {
        self.shared.budget.clone()
    }
    /// Best-effort delivery. Use `try_post` when acceptance must be observed.
    pub fn post(&self, msg: &UiMsg) {
        let _ = self.try_post(msg);
    }

    /// Accepts and wakes without blocking, or returns an explicit error before
    /// cloning the message. A successful post can still be discarded on shutdown.
    pub fn try_post(&self, msg: &UiMsg) -> Result<(), PostError> {
        let bytes = message_bytes(msg, ENTRY_LIMIT).ok_or(PostError::TooLarge)?;
        let mut queue = self.shared.queue.lock();
        self.enqueue(&mut queue, msg, bytes, None)
    }

    pub(crate) fn try_post_guarded(
        &self,
        msg: &UiMsg,
        delivery: &Delivery,
    ) -> Result<(), PostError> {
        if !delivery.active() {
            return Err(PostError::Closed);
        }
        let bytes = message_bytes(msg, ENTRY_LIMIT).ok_or(PostError::TooLarge)?;
        self.enqueue(
            &mut self.shared.queue.lock(),
            msg,
            bytes,
            Some(delivery.clone()),
        )
    }

    fn enqueue(
        &self,
        queue: &mut Queue,
        msg: &UiMsg,
        bytes: usize,
        delivery: Option<Delivery>,
    ) -> Result<(), PostError> {
        if queue.closed {
            return Err(PostError::Closed);
        }
        if queue.entries.len() >= MESSAGE_LIMIT || queue.bytes + bytes > BYTE_LIMIT {
            return Err(PostError::Full);
        }
        let lease = self
            .shared
            .budget
            .reserve(bytes)
            .map_err(|_| PostError::Full)?;
        queue.entries.push_back(Envelope {
            msg: msg.clone(),
            bytes,
            delivery,
            lease,
        });
        queue.bytes += bytes;
        // Keep closure and wake atomic with respect to Inbox::close.
        let _ = self.waker.wake();
        Ok(())
    }

    /// Background workers backpressure here without holding the UI thread.
    /// Cancellation, expiry and inbox closure also release the wait.
    pub(crate) fn post_guarded(&self, msg: &UiMsg, delivery: &Delivery) -> Result<(), PostError> {
        let bytes = message_bytes(msg, ENTRY_LIMIT).ok_or(PostError::TooLarge)?;
        // Registered outside the queue lock: `Cancellation::cancel` takes the
        // queue lock while it holds no registration lock.
        delivery
            .cancelled
            .waiting
            .lock()
            .push(Arc::downgrade(&self.shared));
        let result = {
            let mut queue = self.shared.queue.lock();
            loop {
                if !delivery.active() {
                    break Err(PostError::Closed);
                }
                match self.enqueue(&mut queue, msg, bytes, Some(delivery.clone())) {
                    Err(PostError::Full) => {
                        self.shared.space.wait_until(&mut queue, delivery.deadline);
                    }
                    result => break result,
                }
            }
        };
        let shared = Arc::downgrade(&self.shared);
        delivery
            .cancelled
            .waiting
            .lock()
            .retain(|waiting| !waiting.ptr_eq(&shared));
        result
    }
}

/// The session's receiving end. Dropping it releases all queued messages.
pub struct Inbox {
    shared: Arc<Shared>,
    received: i64,
}

impl Default for Inbox {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for Inbox {
    fn drop(&mut self) {
        self.close();
    }
}

impl Inbox {
    pub fn new() -> Inbox {
        Self::with_budget(crate::memory::Budget::new(BYTE_LIMIT))
    }
    pub(crate) fn with_budget(budget: crate::memory::Budget) -> Inbox {
        Inbox {
            shared: Arc::new(Shared::new(budget)),
            received: 0,
        }
    }

    pub fn mailbox(&self, waker: windowing::Waker) -> Mailbox {
        Mailbox {
            shared: Arc::clone(&self.shared),
            waker,
        }
    }

    /// Message count and accounted bytes waiting for the UI thread.
    pub fn usage(&self) -> (usize, usize) {
        let queue = self.shared.queue.lock();
        (queue.entries.len(), queue.bytes)
    }

    pub fn close(&mut self) {
        let mut queue = self.shared.queue.lock();
        queue.closed = true;
        queue.entries = VecDeque::new();
        queue.bytes = 0;
        self.shared.space.notify_all();
    }

    /// Already posted messages, oldest first; superseded or expired job results
    /// are rejected here even if cancellation raced with the worker's post.
    pub fn drain(&mut self) -> Vec<UiMsg> {
        self.drain_leased()
            .into_iter()
            .map(|(message, _)| message)
            .collect()
    }

    pub(crate) fn drain_leased(&mut self) -> Vec<(UiMsg, crate::memory::Lease)> {
        let mut queue = self.shared.queue.lock();
        let mut out = Vec::with_capacity(queue.entries.len());
        while let Some(envelope) = queue.entries.pop_front() {
            queue.bytes -= envelope.bytes;
            if envelope.delivery.as_ref().is_none_or(Delivery::active) {
                out.push((envelope.msg, envelope.lease));
            }
        }
        queue.entries = VecDeque::new();
        self.shared.space.notify_all();
        self.received += out.len() as i64;
        out
    }
}

impl std::fmt::Display for Mailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Mailbox")
    }
}
impl std::fmt::Debug for Mailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Mailbox")
    }
}
impl std::fmt::Display for Inbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Inbox({} received)", self.received)
    }
}
impl std::fmt::Debug for Inbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Inbox {{ received: {} }}", self.received)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturation_oversize_and_closed_session_are_explicit_and_release_bytes() {
        crate::testing::on_ui_thread(|| {
            let mut session = crate::start("Mailbox budget", "test.mailbox-budget").unwrap();
            let mut inbox = Inbox::new();
            let mail = inbox.mailbox(session.waker());
            let msg = UiMsg::Custom("owner".into(), "x".repeat(16 * 1024));
            let mut accepted = 0;
            while mail.try_post(&msg).is_ok() {
                accepted += 1;
            }
            assert!(accepted > 0 && accepted <= MESSAGE_LIMIT);
            assert!(inbox.usage().1 <= BYTE_LIMIT);
            assert_eq!(mail.try_post(&msg), Err(PostError::Full));
            assert_eq!(inbox.drain().len(), accepted);
            assert_eq!(inbox.usage(), (0, 0));
            assert_eq!(
                mail.try_post(&UiMsg::Custom("owner".into(), "x".repeat(ENTRY_LIMIT))),
                Err(PostError::TooLarge)
            );
            mail.try_post(&msg).unwrap();
            inbox.close();
            assert_eq!(inbox.usage(), (0, 0));
            assert_eq!(mail.try_post(&msg), Err(PostError::Closed));
            session.shutdown();
            assert_eq!(mail.try_post(&msg), Err(PostError::Closed));
        })
    }

    #[test]
    fn cancellation_after_post_rejects_the_already_queued_result() {
        crate::testing::on_ui_thread(|| {
            let mut session = crate::start("Mailbox race", "test.mailbox-race").unwrap();
            let mut inbox = Inbox::new();
            let mail = inbox.mailbox(session.waker());
            let delivery = Delivery {
                cancelled: Arc::new(Cancellation::default()),
                deadline: Instant::now() + std::time::Duration::from_secs(5),
            };
            mail.post_guarded(&UiMsg::Custom("old".into(), "result".into()), &delivery)
                .unwrap();
            delivery.cancelled.cancel();
            assert!(inbox.drain().is_empty());
            assert_eq!(inbox.usage(), (0, 0));
            session.shutdown();
        })
    }

    #[test]
    fn cancellation_wakes_a_sender_blocked_on_a_full_mailbox() {
        crate::testing::on_ui_thread(|| {
            let mut session = crate::start("Mailbox wake", "test.mailbox-wake").unwrap();
            let mut inbox = Inbox::new();
            let mail = inbox.mailbox(session.waker());
            let msg = UiMsg::Custom("owner".into(), String::new());
            while mail.try_post(&msg).is_ok() {}
            let delivery = Delivery {
                cancelled: Arc::new(Cancellation::default()),
                deadline: Instant::now() + std::time::Duration::from_secs(3600),
            };
            let (started, ready) = std::sync::mpsc::channel();
            let sender = {
                let delivery = delivery.clone();
                let msg = msg.clone();
                std::thread::spawn(move || {
                    started.send(()).unwrap();
                    mail.post_guarded(&msg, &delivery)
                })
            };
            ready.recv().unwrap();
            delivery.cancelled.cancel();
            assert_eq!(sender.join().unwrap(), Err(PostError::Closed));
            inbox.close();
            session.shutdown();
        })
    }
}
