// clipboard: text, primary selection, and MIME data through windowing's
// clipboard, or through an in-memory board for tests and headless runs.
// Every editing control goes through these functions, so one switch makes
// a whole session deterministic.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use windowing;

static MEMORY: AtomicBool = AtomicBool::new(false);
static BOARD: Mutex<String> = Mutex::new(String::new());
static PRIMARY: Mutex<String> = Mutex::new(String::new());

/// Routes clipboard calls to an in-memory board (`true`) or the system.
pub fn use_memory(memory: bool) {
    MEMORY.store(memory, Ordering::Relaxed);
    BOARD.lock().clear();
    PRIMARY.lock().clear();
}

pub fn is_memory() -> bool {
    MEMORY.load(Ordering::Relaxed)
}

/// The clipboard's text, or "" when it holds none or cannot be read.
pub fn text() -> String {
    if MEMORY.load(Ordering::Relaxed) {
        return BOARD.lock().to_string();
    }
    match windowing::clipboard::text() {
        Ok(t) => t,
        Err(_) => "".to_string(),
    }
}

/// Replaces the clipboard's contents with `text`. A system failure leaves
/// the clipboard unchanged and is not fatal to editing.
pub fn set_text(text: &str) {
    if text.len() > 16 * 1024 * 1024 {
        return;
    }
    if MEMORY.load(Ordering::Relaxed) {
        *BOARD.lock() = text.to_string();
        return;
    }
    if let Err(e) = windowing::clipboard::set_text(text) {
        eprintln!("gui: could not set the clipboard: {}", e)
    }
}

pub fn has_text() -> bool {
    if MEMORY.load(Ordering::Relaxed) {
        !BOARD.lock().is_empty()
    } else {
        windowing::clipboard::has_text()
    }
}

/// The X11/Wayland primary selection (middle-click paste), where supported.
pub fn primary_text() -> String {
    if MEMORY.load(Ordering::Relaxed) {
        return PRIMARY.lock().to_string();
    }
    match windowing::clipboard::primary_text() {
        Ok(t) => t,
        Err(_) => "".to_string(),
    }
}

pub fn set_primary_text(text: &str) {
    if text.len() > 16 * 1024 * 1024 {
        return;
    }
    if MEMORY.load(Ordering::Relaxed) {
        *PRIMARY.lock() = text.to_string();
        return;
    }
    let _ = windowing::clipboard::set_primary_text(text);
}

/// Offers several representations at once (e.g. text/plain and text/html).
pub fn set_data(items: &[(String, Vec<u8>)]) {
    if items.iter().fold(0usize, |sum, (mime, bytes)| {
        sum.saturating_add(mime.len()).saturating_add(bytes.len())
    }) > 16 * 1024 * 1024
    {
        return;
    }
    if MEMORY.load(Ordering::Relaxed) {
        for (mime, bytes) in items {
            if mime == "text/plain" || mime == "text/plain;charset=utf-8" {
                *BOARD.lock() = String::from_utf8(bytes.to_vec()).unwrap_or("".to_string());
            }
        }
        return;
    }
    if let Err(e) = windowing::clipboard::set_data(items.to_vec()) {
        eprintln!("gui: could not set clipboard data: {}", e)
    }
}

/// The clipboard data offered as `mime`, if any.
pub fn data(mime: &str) -> Option<Vec<u8>> {
    if MEMORY.load(Ordering::Relaxed) {
        if mime.starts_with("text/plain") && !BOARD.lock().is_empty() {
            return Some(BOARD.lock().as_bytes().to_vec());
        }
        return None;
    }
    windowing::clipboard::data(mime).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn memory_board_round_trips() {
        super::use_memory(true);
        super::set_text("hello");
        assert_eq!(super::text(), "hello");
        assert!(super::has_text());
        super::set_primary_text("p");
        assert_eq!(super::primary_text(), "p")
    }
}
