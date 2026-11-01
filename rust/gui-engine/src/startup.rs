// A program's link order can lay the code it runs only while starting
// between the two markers below; once the first frame is shown, those pages
// leave the program's resident memory and come back only if called again.

// The markers' bodies differ so that merging identical functions cannot give
// them one address.

/// Opens the run of startup-only code a link order lays out.
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn gui_engine_startup_code_begin() {
    std::hint::black_box(0u8);
}

/// Closes the run of startup-only code a link order lays out.
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn gui_engine_startup_code_end() {
    std::hint::black_box(1u8);
}

/// Ends startup by releasing the startup-only code; a session calls it once,
/// after its first frame reaches a window. Profiles split startup from the
/// rest of a session where it is entered, so it keeps its name.
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn gui_engine_startup_complete() {
    let begin: extern "C" fn() = gui_engine_startup_code_begin;
    let end: extern "C" fn() = gui_engine_startup_code_end;
    release(begin as usize, end as usize);
}

/// The whole pages strictly inside `begin..end`, as a start and a length.
/// Without a link order the markers sit side by side and no page qualifies.
#[cfg(any(target_os = "linux", test))]
fn pages_between(begin: usize, end: usize, page: usize) -> Option<(usize, usize)> {
    let start = begin.checked_next_multiple_of(page)?;
    let stop = end - end % page;
    (stop > start).then(|| (start, stop - start))
}

#[cfg(target_os = "linux")]
fn release(begin: usize, end: usize) {
    // SAFETY: sysconf has no preconditions.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let Ok(page) = usize::try_from(page) else {
        return;
    };
    let Some((start, len)) = pages_between(begin, end, page) else {
        return;
    };
    let at = start as *mut libc::c_void;
    // SAFETY: the range is whole pages of this program's own code, mapped
    // privately from its file and never written, so dropping them loses
    // nothing: the kernel reads a page back from the file on its next use.
    // Both calls are advice; a kernel that refuses one leaves the code as it
    // was, so their results change nothing.
    unsafe {
        libc::madvise(at, len, libc::MADV_COLD);
        libc::madvise(at, len, libc::MADV_DONTNEED);
    }
}

// Link orders are profiled on Linux only, so elsewhere no startup-only run
// exists to release.
#[cfg(not(target_os = "linux"))]
fn release(_begin: usize, _end: usize) {}

#[cfg(test)]
mod tests {
    use super::pages_between;

    #[test]
    fn only_whole_pages_inside_the_markers_are_released() {
        assert_eq!(
            pages_between(0x1010, 0x4020, 0x1000),
            Some((0x2000, 0x2000))
        );
        assert_eq!(
            pages_between(0x2000, 0x3000, 0x1000),
            Some((0x2000, 0x1000))
        );
    }

    #[test]
    fn markers_within_one_page_release_nothing() {
        assert_eq!(pages_between(0x1010, 0x1020, 0x1000), None);
        assert_eq!(pages_between(0x1ff0, 0x2010, 0x1000), None);
    }

    #[test]
    fn markers_out_of_order_release_nothing() {
        assert_eq!(pages_between(0x5000, 0x1000, 0x1000), None);
    }
}
