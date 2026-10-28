// What reaches a session from outside its loop: workers' mailboxes, which
// any thread posts to, and the single-instance claim later launches reach.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};

use gui_engine::app::mailbox::Mailbox;
use gui_engine::widgets::cx::UiMsg;
use parking_lot::Mutex;
use windowing::instance::Listener;

static MAILBOXES: LazyLock<Mutex<HashMap<i64, (i64, Mailbox)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_MAILBOX: AtomicI64 = AtomicI64::new(1);

thread_local! {
    static LISTENERS: RefCell<HashMap<i64, Listener>> = RefCell::new(HashMap::new());
    static NEXT_LISTENER: Cell<i64> = const { Cell::new(1) };
}

/// Keeps `m` for posting from any thread and answers its id.
pub fn add_mailbox(session: i64, m: Mailbox) -> Result<i64, String> {
    let mut mailboxes = MAILBOXES.lock();
    if mailboxes.len() >= 1024
        || mailboxes
            .values()
            .filter(|(owner, _)| *owner == session)
            .count()
            >= 256
    {
        return Err("mailbox handles exceed their registration byte budget".into());
    }
    let id = NEXT_MAILBOX.fetch_add(1, Ordering::Relaxed);
    mailboxes.insert(id, (session, m));
    Ok(id)
}

pub fn close_mailboxes(session: i64) {
    MAILBOXES.lock().retain(|_, (owner, _)| *owner != session);
}

/// Delivers the JSON message `msg` through mailbox `id`.
pub fn post(id: i64, msg: &str) -> Result<(), String> {
    let msg: UiMsg = serde_json::from_str(msg).map_err(|e| format!("a posted message: {e}"))?;
    let m = MAILBOXES
        .lock()
        .get(&id)
        .map(|(_, mailbox)| mailbox.clone())
        .ok_or_else(|| format!("mailbox {id} is released"))?;
    m.try_post(&msg).map_err(|e| e.to_string())
}

/// Lets mailbox `id` go.
pub fn release_mailbox(id: i64) {
    MAILBOXES.lock().remove(&id);
}

/// Claims `app_id`'s single instance, handing the JSON list `files` to the
/// running one when there is one: "forwarded", "alone", or the id of this
/// process's listener.
pub fn claim(app_id: &str, files: &str) -> Result<String, String> {
    let files: Vec<String> = serde_json::from_str(files).map_err(|e| format!("files: {e}"))?;
    Ok(match gui_engine::launch(app_id, &files) {
        gui_engine::Launch::Failed(error) => return Err(error),
        gui_engine::Launch::Forwarded => "\"forwarded\"".to_string(),
        gui_engine::Launch::Alone => "\"alone\"".to_string(),
        gui_engine::Launch::Primary(l) => {
            let id = NEXT_LISTENER.with(|n| n.replace(n.get() + 1));
            LISTENERS.with_borrow_mut(|m| m.insert(id, l));
            id.to_string()
        }
    })
}

/// Takes listener `id` for a session to serve.
pub fn take_listener(id: i64) -> Result<Listener, String> {
    LISTENERS
        .with_borrow_mut(|m| m.remove(&id))
        .ok_or_else(|| format!("listener {id} is closed or already served"))
}

/// Gives up the claim of listener `id`, unless a session took it.
pub fn close_listener(id: i64) {
    if let Some(l) = LISTENERS.with_borrow_mut(|m| m.remove(&id)) {
        l.close()
    }
}
