// bridge: the session's connection to platform accessibility. The
// session reports window lifecycle, focus, tree updates, and
// announcements here; the platform adapter (AT-SPI on Linux, see
// `atspi`; windowing's UI Automation and NSAccessibility providers on
// Windows and macOS, see `native`) exports them when an assistive
// technology is listening.
// Without a listener the bridge only keeps the latest tree, so semantic
// state is still inspectable by tests and the inspector.

use crate::accessibility::AccessNode;
use crate::accessibility::atspi;
use std::collections::HashMap;
use windowing;

/// The accessibility bridge of one session.
pub struct Bridge {
    pub app_id: String,
    pub trees: HashMap<String, AccessNode>,
    pub focus: HashMap<String, String>,
    pub spoken: Vec<String>,
    adapter: atspi::Adapter,
}

impl Bridge {
    pub fn new(app_id: &str) -> Bridge {
        Bridge {
            app_id: app_id.to_string(),
            trees: HashMap::new(),
            focus: HashMap::new(),
            spoken: vec![],
            adapter: atspi::Adapter::connect(app_id),
        }
    }

    /// Whether trees should be produced after each frame (a platform
    /// listener is connected, or tests enabled recording).
    pub fn status(&self) -> String {
        self.adapter.status.clone()
    }
    pub fn endpoint(&self) -> String {
        self.adapter.endpoint()
    }

    pub fn active(&self) -> bool {
        self.adapter.listening()
    }

    /// Wakes the session through `waker` for platform accessibility
    /// traffic; false when nothing is connected.
    pub fn watch(&mut self, waker: windowing::Waker) -> bool {
        self.adapter.watch(waker)
    }

    /// Whether a watch wakes the session, so it need not poll the platform.
    pub fn watched(&self) -> bool {
        self.adapter.watched()
    }

    pub fn window_opened(&mut self, key: &str, title: &str, window: windowing::Window) {
        self.adapter.window_opened(key, title, &window)
    }

    /// True once, when a platform reader first asks for the windows; the
    /// session then describes every window.
    pub fn activated(&mut self) -> bool {
        self.adapter.activated()
    }

    pub fn window_closed(&mut self, key: &str) {
        let _ = self.trees.remove(key);
        let _ = self.focus.remove(key);
        self.adapter.window_closed(key)
    }

    pub fn origin(&mut self, key: &str, x: i64, y: i64) {
        self.adapter.origin(key, x, y)
    }

    pub fn update(&mut self, key: &str, tree: &AccessNode) {
        self.trees.insert(key.to_string(), tree.clone());
        self.adapter.update(key, tree)
    }

    pub fn focus_changed(&mut self, key: &str, id: &str) {
        self.focus.insert(key.to_string(), id.to_string());
        self.adapter.focus_changed(key, id)
    }

    /// Queues a polite announcement.
    pub fn announce(&mut self, text: &str) {
        self.spoken.push(text.to_string());
        if self.spoken.len() > 50 {
            let _ = self.spoken.remove(0);
        }
        self.adapter.announce(text)
    }

    /// Services pending requests from assistive technology (actions,
    /// queries); answers the action requests to route: (window, id, action).
    pub fn poll(&mut self) -> Vec<crate::accessibility::provider::NativeAction> {
        self.adapter.poll()
    }

    pub fn shutdown(&mut self) {
        self.adapter.disconnect();
        self.trees = HashMap::new();
        self.focus = HashMap::new();
        self.spoken = vec![];
    }
}
