// Folder watches for `Effect::WatchFolder`: each posts
// `UiMsg::FolderChanged(id)` to the session when a file in its folder is
// made, removed, renamed or written, from the watcher's own thread, which
// lives only as long as the watch.

use notify::{RecursiveMode, Watcher};

use crate::app::mailbox::Mailbox;
use crate::widgets::cx::UiMsg;

/// One folder watch; dropping it ends the watch and its thread.
pub struct FolderWatch {
    _watcher: notify::RecommendedWatcher,
}

/// Watches folder `path` for `id`, posting through `mailbox`.
pub fn watch(id: String, path: &str, mailbox: Mailbox) -> notify::Result<FolderWatch> {
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        // Access events report reads and opens, which change nothing.
        if let Ok(e) = event
            && !e.kind.is_access()
        {
            mailbox.post(&UiMsg::FolderChanged(id.clone()));
        }
    })?;
    let folder = std::path::Path::new(path);
    watcher.watch(folder, RecursiveMode::NonRecursive)?;
    watch_files(&mut watcher, folder);
    Ok(FolderWatch { _watcher: watcher })
}

// kqueue reports a folder's entries changing but not a file in it written in
// place; the watcher watches files made later itself, so the ones already
// there are watched here.
#[cfg(target_os = "freebsd")]
fn watch_files(watcher: &mut notify::RecommendedWatcher, folder: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            // A file removed meanwhile is simply not watched.
            let _ = watcher.watch(&entry.path(), RecursiveMode::NonRecursive);
        }
    }
}

// The other systems' watchers report a file in the folder written in place.
#[cfg(not(target_os = "freebsd"))]
fn watch_files(_watcher: &mut notify::RecommendedWatcher, _folder: &std::path::Path) {}
