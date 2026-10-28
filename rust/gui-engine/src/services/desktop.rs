// desktop: notifications and the system tray through windowing, or an
// in-memory record for tests and headless runs, the way `clipboard` switches.
// Tray menus are ordinary gui menus; their entries run registry commands.

use crate::errors;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use windowing;

use crate::commands::Command;
use crate::widgets::menus::{MenuItem, native_menu_entries};

static MEMORY: AtomicBool = AtomicBool::new(false);
static SENT: Mutex<Vec<String>> = Mutex::new(vec![]);

/// Routes notifications to an in-memory record (`true`) or the system.
pub fn use_memory(memory: bool) {
    MEMORY.store(memory, Ordering::Relaxed);
    *SENT.lock() = vec![];
}

/// The notifications recorded in memory mode, oldest first, as `title: body`.
pub fn sent() -> Vec<String> {
    SENT.lock().to_vec()
}

/// How urgent a notification is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

/// Shows a desktop notification. Where the platform has no notification
/// service the error says so; callers keep the information visible in the
/// window instead.
/// Shows a notification and calls `done` with the outcome, possibly from
/// another thread.
pub fn notify(
    app: &windowing::App,
    title: &str,
    body: &str,
    urgency: Urgency,
    done: impl FnOnce(Result<(), errors::Error>) + Send + 'static,
) {
    if MEMORY.load(Ordering::Relaxed) {
        SENT.lock().push(format!("{}: {}", title, body));
        return done(Ok(()));
    }
    let level = match urgency {
        Urgency::Low => windowing::Urgency::Low,
        Urgency::Normal => windowing::Urgency::Normal,
        Urgency::Critical => windowing::Urgency::Critical,
    };
    let notification = windowing::Notification {
        title: title.to_string(),
        body: body.to_string(),
        urgency: level,
        timeout_ms: -1,
    };
    app.notify_then(notification, move |sent| {
        done(sent.map_err(errors::Error::from))
    })
}

/// A tray icon whose menu runs registry commands.
#[derive(Debug, Clone, PartialEq)]
pub struct TrayHost {
    tray: windowing::Tray,
    commands: Vec<String>,
}

impl TrayHost {
    /// Puts an icon with `tooltip` and the menu `items` in the system tray.
    pub fn open(
        app: windowing::App,
        icon: Option<windowing::TrayIcon>,
        tooltip: &str,
        items: &[MenuItem],
        registry: &[Command],
    ) -> Result<TrayHost, errors::Error> {
        let tray = app.tray(icon.as_ref(), tooltip)?;
        let mut host = TrayHost {
            tray,
            commands: vec![],
        };
        host.set_menu(items, registry)?;
        Ok(host)
    }

    /// Replaces the menu, taking enabled and checked state from `registry`.
    pub fn set_menu(
        &mut self,
        items: &[MenuItem],
        registry: &[Command],
    ) -> Result<(), errors::Error> {
        let (entries, commands) = native_menu_entries(items, registry);
        self.tray.set_menu(&entries)?;
        self.commands = commands;
        Ok(())
    }

    /// The command a `MenuSelected` event from this tray runs, if it is one of its items.
    pub fn command_for(&self, source: windowing::MenuSource, item: u32) -> Option<String> {
        let windowing::MenuSource::Tray { tray } = source else {
            return None;
        };
        if tray != self.tray.id() || item == 0 || item as i64 > self.commands.len() as i64 {
            return None;
        }
        Some(self.commands[(item as i64 - 1) as usize].clone())
    }

    /// Removes the icon.
    pub fn close(&self) {
        self.tray.close()
    }
}

#[cfg(test)]
mod tests {
    use super::{Urgency, notify, sent, use_memory};
    use windowing;

    #[test]
    fn memory_mode_records_notifications_in_order() {
        crate::testing::on_ui_thread(|| {
            let app = windowing::init(
                "desktop test",
                &windowing::InitOptions {
                    driver: "offscreen".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
            let _defer = scopeguard::guard((), |_| {
                app.finalize();
            });
            use_memory(true);
            let _defer = scopeguard::guard((), |_| {
                use_memory(false);
            });
            let (tx, rx) = std::sync::mpsc::channel();
            let first = tx.clone();
            notify(&app, "Saved", "notes.txt", Urgency::Low, move |r| {
                first.send(r.is_ok()).unwrap()
            });
            notify(
                &app,
                "Build failed",
                "3 errors",
                Urgency::Critical,
                move |r| tx.send(r.is_ok()).unwrap(),
            );
            assert_eq!(rx.iter().collect::<Vec<_>>(), vec![true, true]);
            assert_eq!(sent(), vec!["Saved: notes.txt", "Build failed: 3 errors"])
        })
    }
}
