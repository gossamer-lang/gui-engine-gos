// background: work that may take long but must not compete with drawing or a
// terminal's output, run on a thread of its own at background priority.

/// Runs `work` on a thread of its own at background priority and answers its
/// result; the calling thread waits for it. Where no thread can be started,
/// the work runs on the calling thread at its priority.
pub(crate) fn run<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    let pending = parking_lot::Mutex::new(Some(work));
    let ran = std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("gui-background".into())
            .spawn_scoped(scope, || {
                lower_current_thread();
                pending.lock().take().map(|work| work())
            })
            .ok()?;
        match handle.join() {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    });
    match (ran, pending.into_inner()) {
        (Some(result), _) => result,
        (None, Some(work)) => work(),
        // The thread answers `None` only when it found no work to take, and
        // only it takes the work, which it then runs.
        (None, None) => unreachable!("background work neither ran nor remained"),
    }
}

// macOS: the utility class yields to user-interactive and default threads.
#[cfg(target_vendor = "apple")]
fn lower_current_thread() {
    // SAFETY: sets the calling thread's own QoS class; no pointers are passed.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

// Linux: a nice value applies to the thread whose id is given.
#[cfg(target_os = "linux")]
fn lower_current_thread() {
    // SAFETY: lowers the calling thread's own priority; no pointers are passed.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10);
    }
}

// Windows: below normal yields to the interface and output threads.
#[cfg(windows)]
fn lower_current_thread() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    // SAFETY: GetCurrentThread answers a pseudo-handle for the calling thread
    // that needs no closing.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

// FreeBSD and other systems: a nice value applies to the whole process and
// the per-thread idle class needs privileges, so the work runs at normal
// priority.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
fn lower_current_thread() {}

#[cfg(test)]
mod tests {
    #[test]
    fn background_work_answers_its_result_on_another_thread() {
        let caller = std::thread::current().id();
        let (answer, ran_on) = super::run(|| (6 * 7, std::thread::current().id()));
        assert_eq!(answer, 42);
        assert_ne!(ran_on, caller);
    }
}
