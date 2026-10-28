// The accessibility adapter for Windows and macOS: windowing's providers
// (UI Automation, NSAccessibility) answer the screen reader, and this adapter
// keeps them fed. Each update publishes the window's tree into the same
// `Snapshot` the AT-SPI bridge uses, so identities, owners and action checks
// are shared, then hands windowing the published objects as a flat list:
// an object's stable path number is its element id. Requests the providers
// queued come back through `poll` as the same `NativeAction`s AT-SPI makes.

#[cfg(not(target_os = "linux"))]
mod platform {
    use crate::accessibility::AccessNode;
    use crate::accessibility::provider::{NativeAction, Published, Snapshot};
    use std::collections::HashMap;

    use windowing::access;
    use windowing::access::AccessElement;

    const PATH_PREFIX: &str = "/org/a11y/atspi/accessible/n";

    pub struct Adapter {
        pub app_id: String,
        pub snapshot: Snapshot,
        pub status: String,
        windows: HashMap<String, windowing::Window>,
        was_listening: bool,
    }

    // The element id of the published object at `path`; 0 for the
    // application root, which no window shows.
    fn element_id(path: &str) -> u64 {
        path.strip_prefix(PATH_PREFIX)
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0)
    }

    fn element(p: &Published) -> AccessElement {
        let n = &p.node;
        AccessElement {
            id: element_id(&p.object),
            role: n.role.name().to_string(),
            name: n.name.clone(),
            description: n.description.clone(),
            value: n.value.clone(),
            text_source: n.text_source.as_ref().map(|source| {
                let buffer = source.0.clone();
                access::TextSource {
                    revision: buffer.identity(),
                    read: std::sync::Arc::new(move || buffer.to_string()),
                }
            }),
            x: n.bounds.x,
            y: n.bounds.y,
            width: n.bounds.width,
            height: n.bounds.height,
            focusable: n.focusable,
            focused: n.focused,
            disabled: n.disabled,
            checked: n.checked,
            selected: n.selected,
            expanded: n.expanded,
            pressed: n.pressed,
            read_only: n.read_only,
            required: n.required,
            invalid: n.invalid,
            multiline: n.multiline,
            modal: n.modal,
            level: n.level,
            pos_in_set: n.pos_in_set,
            set_size: n.set_size,
            has_range: n.has_range,
            range_min: n.range_min,
            range_max: n.range_max,
            range_now: n.range_now,
            actions: n.actions.clone(),
            live: n.live.clone(),
            children: p
                .children
                .iter()
                .map(|c| element_id(c))
                .filter(|&id| id != 0)
                .collect(),
        }
    }

    impl Adapter {
        pub fn connect(app_id: &str) -> Adapter {
            let status = if access::has_provider() {
                if windowing::video_driver() == "windows" {
                    "UI Automation provider ready"
                } else {
                    "NSAccessibility provider ready"
                }
            } else {
                "Native accessibility unavailable on this platform"
            };
            Adapter {
                app_id: app_id.to_string(),
                snapshot: Snapshot::new(app_id),
                status: status.to_string(),
                windows: HashMap::new(),
                was_listening: false,
            }
        }

        pub fn listening(&self) -> bool {
            access::has_provider() && access::listening()
        }
        pub fn endpoint(&self) -> String {
            String::new()
        }

        /// True once, when a screen reader first reads the windows, so the
        /// session describes every window from then on.
        pub fn activated(&mut self) -> bool {
            if self.was_listening || !self.listening() {
                return false;
            }
            self.was_listening = true;
            true
        }

        pub fn window_opened(&mut self, key: &str, _title: &str, window: &windowing::Window) {
            self.windows.insert(key.to_string(), *window);
        }

        pub fn window_closed(&mut self, key: &str) {
            self.snapshot.close(key);
            let _ = self.windows.remove(key);
        }

        pub fn origin(&mut self, key: &str, x: i64, y: i64) {
            self.snapshot.origins.insert(key.to_string(), (x, y));
        }

        pub fn update(&mut self, key: &str, tree: &AccessNode) {
            let _ = self.snapshot.publish(key, tree);
            let Some(&window) = self.windows.get(key) else {
                return;
            };
            let Some(root) = self.snapshot.roots.get(key) else {
                return;
            };
            // The window's own element first, then the rest breadth first.
            let mut elements: Vec<AccessElement> = vec![];
            let mut pending = std::collections::VecDeque::from([root]);
            while let Some(path) = pending.pop_front() {
                if let Some(p) = self.snapshot.objects.get(path) {
                    pending.extend(&p.children);
                    elements.push(element(p))
                }
            }
            if let Err(e) = access::update(window, elements) {
                self.status = format!("accessibility update failed: {e}");
            }
        }

        pub fn focus_changed(&mut self, key: &str, id: &str) {
            let Some(&window) = self.windows.get(key) else {
                return;
            };
            let Some(path) = self.snapshot.related(key, id) else {
                return;
            };
            let _ = access::focus(window, element_id(&path));
        }

        pub fn announce(&mut self, text: &str) {
            // One window speaks for the application; the screen reader
            // voices an announcement once whichever window raised it.
            if let Some(&window) = self.windows.values().next() {
                let _ = access::announce(window, text, false);
            }
        }

        pub fn poll(&mut self) -> Vec<NativeAction> {
            access::take_requests()
                .into_iter()
                .filter_map(|r| {
                    let path = format!("{PATH_PREFIX}{}", r.id);
                    self.snapshot.request(&path, &r.action, &r.value, 0, 0)
                })
                .collect()
        }

        pub fn disconnect(&mut self) {
            self.windows.clear();
        }

        /// The providers wake the session themselves when a request arrives.
        pub fn watch(&mut self, _waker: windowing::Waker) -> bool {
            true
        }

        /// The providers wake the session themselves when a request arrives.
        pub fn watched(&self) -> bool {
            true
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub type Adapter = platform::Adapter;
