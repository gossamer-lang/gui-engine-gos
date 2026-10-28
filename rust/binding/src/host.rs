// Gossamer closures the toolkit calls back: registered once, each runs
// through the dispatch of the binding call in progress on this thread.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

use gossamer_binding::{NativeDispatch, PersistentCallback, Value};

thread_local! {
    static CURRENT: Cell<Option<NonNull<dyn NativeDispatch + 'static>>> = const { Cell::new(None) };
    // Kept until released, never dropped with the thread: the thread is the
    // program's, and by its teardown the runtime a release returns a callback
    // to is gone.
    static CALLBACKS: ManuallyDrop<RefCell<HashMap<i64, PersistentCallback>>> =
        ManuallyDrop::new(RefCell::new(HashMap::new()));
    static NEXT: Cell<i64> = const { Cell::new(1) };
}

/// Publishes a binding call's dispatch for the toolkit's callbacks while
/// the guard lives; dropping it restores the enclosing call's dispatch.
pub struct Scope {
    previous: Option<NonNull<dyn NativeDispatch + 'static>>,
}

impl Scope {
    pub fn enter(d: &mut dyn NativeDispatch) -> Scope {
        let short: NonNull<dyn NativeDispatch + '_> = NonNull::from(d);
        // SAFETY: only the lifetime bound changes. The pointer is read only
        // while this guard lives, which is inside the call that lent `d`.
        let long: NonNull<dyn NativeDispatch + 'static> = unsafe { std::mem::transmute(short) };
        Scope {
            previous: CURRENT.with(|c| c.replace(Some(long))),
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|c| c.set(self.previous));
    }
}

/// Keeps `f` for later calls and answers its id.
pub fn register(f: PersistentCallback) -> i64 {
    assert!(
        count() < 8192,
        "hosted callbacks exceed their registration byte budget"
    );
    let id = NEXT.with(|n| n.replace(n.get() + 1));
    CALLBACKS.with(|c| c.borrow_mut().insert(id, f));
    id
}

/// Lets callback `id` go.
pub fn release(id: i64) {
    CALLBACKS.with(|c| c.borrow_mut().remove(&id));
}

/// Runs callback `id` on JSON `args`, answering its JSON result.
pub fn call(id: i64, args: &str) -> Result<String, String> {
    let f = CALLBACKS
        .with(|c| c.borrow().get(&id).cloned())
        .ok_or_else(|| format!("callback {id} is not registered"))?;
    let Some(mut d) = CURRENT.with(Cell::get) else {
        return Err(format!("callback {id} ran outside a binding call"));
    };
    // SAFETY: a live `Scope` on this thread published `d`, and the call that
    // lent it is still running: callbacks run synchronously inside it.
    let d = unsafe { d.as_mut() };
    match f.invoke(d, vec![Value::String(args.into())]) {
        Ok(Value::String(s)) => Ok(s.as_str().to_string()),
        Ok(Value::Unit) => Ok("null".to_string()),
        Ok(other) => Err(format!("callback {id} answered {other:?}, not JSON text")),
        Err(e) => Err(e.to_string()),
    }
}

pub fn count() -> usize {
    CALLBACKS.with(|callbacks| callbacks.borrow().len())
}
