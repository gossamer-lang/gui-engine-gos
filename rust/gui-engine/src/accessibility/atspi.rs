#[cfg(target_os = "linux")]
mod linux {
    // AT-SPI publishes the current semantic tree over the desktop accessibility
    // bus. zbus reads and writes the socket on its own executor; a reader
    // thread hands method calls to the session's UI thread, which answers them
    // from the snapshot it owns.
    use std::collections::HashMap;
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use parking_lot::Mutex;
    use windowing::Waker;
    use zbus::blocking::{Connection, MessageIterator, connection};
    use zbus::message::{Message, Type as MessageType};
    use zbus::zvariant::OwnedObjectPath;

    use crate::accessibility::AccessNode;
    use crate::accessibility::Role;
    use crate::accessibility::live;
    use crate::accessibility::protocol;
    use crate::accessibility::provider::{self, NativeAction, Published, ROOT, Snapshot};
    use crate::accessibility::wire::{self, Wire};

    // Messages one poll answers before yielding to the frame; the rest wake it again.
    const POLL_BATCH: usize = 64;
    const ADDRESS_TIMEOUT: Duration = Duration::from_millis(300);

    pub struct Adapter {
        pub app_id: String,
        pub snapshot: Snapshot,
        pub status: String,
        connection: Option<Connection>,
        pub desktop_name: String,
        pub desktop_path: String,
        pub application_id: i64,
        pub name: String,
        actions: Vec<NativeAction>,
        pacer: live::Pacer,
        started: Instant,
        inbox: Option<Receiver<Message>>,
        reader: Option<JoinHandle<()>>,
        waker: Arc<Mutex<Option<Waker>>>,
        embed: Option<NonZeroU32>,
        /// Watches the desktop's accessibility switch while no assistive
        /// technology is enabled, so the bus is joined once one is.
        status_watch: Option<StatusWatch>,
        /// Joined the bus since the session last asked, so it describes
        /// every window.
        joined: bool,
    }

    struct StatusWatch {
        connection: Connection,
        thread: JoinHandle<()>,
        enabled: Arc<AtomicBool>,
    }

    impl Adapter {
        pub fn detached(app_id: &str) -> Adapter {
            Adapter {
                app_id: app_id.to_string(),
                snapshot: Snapshot::new(app_id),
                status: "Native accessibility unavailable on this platform".to_string(),
                connection: None,
                desktop_name: String::new(),
                desktop_path: String::new(),
                application_id: 0,
                name: String::new(),
                actions: vec![],
                pacer: live::Pacer::new(),
                started: Instant::now(),
                inbox: None,
                reader: None,
                waker: Arc::new(Mutex::new(None)),
                embed: None,
                status_watch: None,
                joined: false,
            }
        }

        pub fn connect(app_id: &str) -> Adapter {
            let mut a = Adapter::detached(app_id);
            a.open();
            a
        }

        pub fn listening(&self) -> bool {
            !self.name.is_empty()
        }

        pub fn endpoint(&self) -> String {
            self.name.clone()
        }

        /// Wakes the session through `waker` whenever bus traffic waits for
        /// `poll`; false without a connection.
        pub fn watch(&mut self, waker: Waker) -> bool {
            *self.waker.lock() = Some(waker);
            self.connection.is_some() || self.status_watch.is_some()
        }

        /// Whether bus traffic wakes the session, so it may wait for events
        /// without polling the bus.
        pub fn watched(&self) -> bool {
            (self.connection.is_some() || self.status_watch.is_some())
                && self.waker.lock().is_some()
        }

        pub fn window_opened(&mut self, _key: &str, _title: &str, _window: &windowing::Window) {}

        /// True once after the bus is joined late, when assistive technology
        /// was enabled after the session started.
        pub fn activated(&mut self) -> bool {
            std::mem::take(&mut self.joined)
        }

        pub fn window_closed(&mut self, key: &str) {
            let old = self.snapshot.objects.clone();
            self.snapshot.close(key);
            self.changes(&old)
        }

        pub fn origin(&mut self, key: &str, x: i64, y: i64) {
            self.snapshot.origins.insert(key.to_string(), (x, y));
        }

        pub fn update(&mut self, key: &str, tree: &AccessNode) {
            let old = self.snapshot.objects.clone();
            self.snapshot.publish(key, tree);
            self.changes(&old)
        }

        fn changes(&mut self, old: &HashMap<String, Published>) {
            let now = self.now_ms();
            // Assistive technology presents focus only inside the active
            // window, so activation is announced before the focus it holds.
            for (path, item) in &self.snapshot.objects {
                let was = old.get(path).is_some_and(|before| before.node.active);
                if item.node.active != was {
                    let active = item.node.active;
                    self.event(
                        path,
                        "StateChanged",
                        "active",
                        i64::from(active),
                        &Wire::Signed(0),
                        "i",
                    );
                    self.signal(
                        path,
                        "org.a11y.atspi.Event.Window",
                        if active { "Activate" } else { "Deactivate" },
                        "",
                        0,
                        Wire::Text(item.node.name.clone()),
                        "s",
                    );
                }
            }
            for (path, item) in &self.snapshot.objects {
                let previous = old.get(path);
                if previous.is_none() {
                    self.cache_signal("AddAccessible", &[protocol::cache_entry(self, item)]);
                }
                // A live region speaks its text when it appears and when the
                // text changes, at its politeness, paced so a region that
                // changes faster than speech is heard at its latest text.
                let n = &item.node;
                if !n.live.is_empty() && !n.name.is_empty() {
                    let changed = previous.is_none_or(|before| before.node.name != n.name);
                    if changed
                        && let Some(s) = self.pacer.offer(path, &n.name, live::level(&n.live), now)
                    {
                        self.speak(&s);
                    }
                }
                let children = previous.map(|p| p.children.as_slice()).unwrap_or_default();
                for (i, child) in item.children.iter().enumerate() {
                    if !children.contains(child) {
                        self.event(
                            path,
                            "ChildrenChanged",
                            "add",
                            i as i64,
                            &self.reference(child),
                            "(so)",
                        );
                    }
                }
                for (i, child) in children.iter().enumerate() {
                    if !item.children.contains(child) {
                        self.event(
                            path,
                            "ChildrenChanged",
                            "remove",
                            i as i64,
                            &self.reference(child),
                            "(so)",
                        );
                    }
                }
                let Some(before) = previous else { continue };
                let b = &before.node;
                if b.name != n.name {
                    self.event(
                        path,
                        "PropertyChange",
                        "accessible-name",
                        0,
                        &Wire::Text(n.name.clone()),
                        "s",
                    );
                }
                if b.focused != n.focused {
                    self.event(
                        path,
                        "StateChanged",
                        "focused",
                        i64::from(n.focused),
                        &Wire::Signed(0),
                        "i",
                    );
                }
                let states = [
                    ("checked", b.checked == 1, n.checked == 1),
                    ("selected", b.selected, n.selected),
                    ("enabled", !b.disabled, !n.disabled),
                    ("invalid-entry", b.invalid, n.invalid),
                ];
                for (state, was, now) in states {
                    if was != now {
                        self.event(
                            path,
                            "StateChanged",
                            state,
                            i64::from(now),
                            &Wire::Signed(0),
                            "i",
                        );
                    }
                }
                if b.caret != n.caret && n.caret >= 0 {
                    let offset = provider::node_character_offset(n, n.caret);
                    self.event(path, "TextCaretMoved", "", offset, &Wire::Signed(0), "i");
                }
                if b.value != n.value && n.role != Role::PasswordField {
                    if n.text_start >= 0 {
                        self.text_event(path, "delete", &b.value);
                        self.text_event(path, "insert", &n.value);
                    }
                    self.event(
                        path,
                        "PropertyChange",
                        "accessible-value",
                        0,
                        &Wire::Text(n.value.clone()),
                        "s",
                    );
                }
            }
            for path in old.keys() {
                if !self.snapshot.objects.contains_key(path) {
                    self.pacer.forget(path);
                    self.cache_signal("RemoveAccessible", &[self.reference(path)]);
                }
            }
        }

        fn now_ms(&self) -> i64 {
            self.started.elapsed().as_millis() as i64
        }

        fn speak(&self, spoken: &live::Spoken) {
            let text = Wire::Text(spoken.text.clone());
            self.event(&spoken.path, "Announcement", "", spoken.level, &text, "s")
        }

        pub fn focus_changed(&mut self, _key: &str, _id: &str) {}

        pub fn announce(&mut self, text: &str) {
            self.event(
                ROOT,
                "Announcement",
                "",
                1,
                &Wire::Text(text.to_string()),
                "s",
            );
        }

        pub fn poll(&mut self) -> Vec<NativeAction> {
            if self
                .status_watch
                .as_ref()
                .is_some_and(|w| w.enabled.load(Ordering::Acquire))
            {
                self.stop_status_watch();
                self.open();
                self.joined = self.listening();
            }
            let mut answered = 0;
            while answered < POLL_BATCH {
                let Some(inbox) = &self.inbox else { break };
                match inbox.try_recv() {
                    Ok(message) => self.receive(&message),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.reset();
                        break;
                    }
                }
                answered += 1;
            }
            if answered == POLL_BATCH
                && let Some(w) = &*self.waker.lock()
            {
                let _ = w.wake();
            }
            for spoken in self.pacer.due(self.now_ms()) {
                self.speak(&spoken);
            }
            std::mem::take(&mut self.actions)
        }

        fn receive(&mut self, message: &Message) {
            match message.message_type() {
                MessageType::MethodCall => self.dispatch(message),
                MessageType::MethodReturn
                    if self.embed.is_some() && message.header().reply_serial() == self.embed =>
                {
                    if let Ok((name, path)) =
                        message.body().deserialize::<(String, OwnedObjectPath)>()
                    {
                        self.desktop_name = name;
                        self.desktop_path = path.to_string();
                    }
                }
                _ => {}
            }
        }

        /// Closes the connection and ends its reader.
        pub fn disconnect(&mut self) {
            self.reset();
        }

        fn stop_status_watch(&mut self) {
            if let Some(w) = self.status_watch.take() {
                // Closing the connection ends the signal stream and its thread.
                let _ = w.connection.close();
                let _ = w.thread.join();
            }
        }

        fn reset(&mut self) {
            self.stop_status_watch();
            if let Some(c) = self.connection.take() {
                let _ = c.close();
            }
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
            self.inbox = None;
            self.embed = None;
            self.name.clear();
            self.actions.clear();
            self.pacer = live::Pacer::new();
            self.snapshot = Snapshot::new(&self.app_id);
        }

        fn open(&mut self) {
            let mode = std::env::var("GUI_ACCESSIBILITY").unwrap_or_else(|_| "auto".to_string());
            let video = windowing::video_driver();
            if mode == "off" || (mode != "on" && (video == "offscreen" || video == "dummy")) {
                self.status = "Accessibility disabled".to_string();
                return;
            }
            let Ok(session) = connection::Builder::session()
                .map(|b| b.method_timeout(ADDRESS_TIMEOUT))
                .and_then(|b| b.build())
            else {
                self.status = "No session bus".to_string();
                return;
            };
            // As other toolkits do, the tree is published only while the
            // desktop says assistive technology is on: without a reader,
            // publishing every frame costs work nothing receives.
            if mode != "on" && !assistive_technology_enabled(&session) {
                self.status = match self.watch_status(session) {
                    Ok(()) => "Assistive technology not enabled".to_string(),
                    Err(status) => status.to_string(),
                };
                return;
            }
            let address = match bus_address(&session) {
                Ok(address) => address,
                Err(status) => {
                    self.status = status.to_string();
                    return;
                }
            };
            let Ok(c) = connection::Builder::address(address.as_str()).and_then(|b| b.build())
            else {
                self.status = "Cannot connect accessibility bus".to_string();
                return;
            };
            // The stream exists before the first call goes out, so no reply is missed.
            let messages = MessageIterator::from(&c);
            let (inbox, received) = mpsc::channel();
            let waker = self.waker.clone();
            let reader = std::thread::Builder::new()
                .name("gui-atspi".to_string())
                .spawn(move || read(messages, inbox, waker));
            let Ok(reader) = reader else {
                self.status = "Cannot start the accessibility reader".to_string();
                return;
            };
            self.name = c.unique_name().map(|n| n.to_string()).unwrap_or_default();
            let embed = wire::body(&[self.reference(ROOT)]).ok().and_then(|body| {
                let call = Message::method_call(ROOT, "Embed")
                    .and_then(|m| m.destination("org.a11y.atspi.Registry"))
                    .and_then(|m| m.interface("org.a11y.atspi.Socket"))
                    .and_then(|m| m.build(&body))
                    .ok()?;
                c.send(&call).ok()?;
                Some(call.primary_header().serial_num())
            });
            self.embed = embed;
            self.connection = Some(c);
            self.inbox = Some(received);
            self.reader = Some(reader);
            self.status = "Connected to AT-SPI accessibility bus".to_string();
        }

        pub fn reference(&self, path: &str) -> Wire {
            if path.is_empty() {
                Wire::Fields(vec![
                    Wire::Text(String::new()),
                    Wire::Path("/org/a11y/atspi/null".to_string()),
                ])
            } else {
                Wire::Fields(vec![
                    Wire::Text(self.name.clone()),
                    Wire::Path(path.to_string()),
                ])
            }
        }

        fn emit(&self, path: &str, interface: &str, member: &str, values: &[Wire]) {
            let Some(c) = &self.connection else { return };
            let Ok(body) = wire::body(values) else { return };
            let _ = c.emit_signal(None::<&str>, path, interface, member, &body);
        }

        fn cache_signal(&self, member: &str, values: &[Wire]) {
            self.emit(
                "/org/a11y/atspi/cache",
                "org.a11y.atspi.Cache",
                member,
                values,
            );
        }

        fn text_event(&self, path: &str, kind: &str, text: &str) {
            if text.is_empty() {
                return;
            }
            let length = provider::character_offset(text, text.len() as i64);
            let values = [
                Wire::Text(kind.to_string()),
                Wire::Signed(0),
                Wire::Signed(length),
                Wire::Variant("s", vec![Wire::Text(text.to_string())]),
                Wire::Items("{sv}", vec![]),
            ];
            self.emit(path, "org.a11y.atspi.Event.Object", "TextChanged", &values);
        }

        fn event(
            &self,
            path: &str,
            member: &str,
            detail: &str,
            value: i64,
            payload: &Wire,
            signature: &'static str,
        ) {
            self.signal(
                path,
                "org.a11y.atspi.Event.Object",
                member,
                detail,
                value,
                payload.clone(),
                signature,
            )
        }

        #[allow(clippy::too_many_arguments)] // Each argument is one field of the AT-SPI event signal.
        fn signal(
            &self,
            path: &str,
            interface: &str,
            member: &str,
            detail: &str,
            value: i64,
            payload: Wire,
            signature: &'static str,
        ) {
            let values = [
                Wire::Text(detail.to_string()),
                Wire::Signed(value),
                Wire::Signed(0),
                Wire::Variant(signature, vec![payload]),
                Wire::Items("{sv}", vec![]),
            ];
            self.emit(path, interface, member, &values);
        }

        fn dispatch(&mut self, message: &Message) {
            let header = message.header();
            let text = |s: Option<String>| s.unwrap_or_default();
            let path = text(header.path().map(|p| p.to_string()));
            let interface = text(header.interface().map(|i| i.to_string()));
            let member = text(header.member().map(|m| m.to_string()));
            let result = wire::arguments(&message.body())
                .ok()
                .and_then(|args| protocol::query(self, &path, &interface, &member, &args));
            let Some(c) = &self.connection else { return };
            let _ = match result.map(|values| wire::body(&values)) {
                Some(Ok(body)) if body.fields().is_empty() => c.reply(&header, &()),
                Some(Ok(body)) => c.reply(&header, &body),
                _ => c.reply_error(
                    &header,
                    "org.freedesktop.DBus.Error.InvalidArgs",
                    &"Unknown object, interface, method or invalid arguments",
                ),
            };
        }

        pub fn queue(&mut self, path: &str, kind: &str, value: &str, start: i64, end: i64) -> bool {
            if self.actions.len() >= 256 {
                return false;
            }
            match self.snapshot.request(path, kind, value, start, end) {
                Some(action) => {
                    self.actions.push(action);
                    true
                }
                None => false,
            }
        }
    }

    impl Drop for Adapter {
        fn drop(&mut self) {
            self.reset();
        }
    }

    const STATUS: &str = "org.a11y.Status";
    // The switches screen readers and other assistive technology turn on.
    const SWITCHES: [&str; 2] = ["IsEnabled", "ScreenReaderEnabled"];

    // Whether the desktop's accessibility service says assistive technology
    // is on. A desktop without the service answers no.
    fn assistive_technology_enabled(session: &Connection) -> bool {
        SWITCHES.iter().any(|switch| {
            session
                .call_method(
                    Some("org.a11y.Bus"),
                    "/org/a11y/bus",
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &(STATUS, *switch),
                )
                .ok()
                .and_then(|reply| {
                    reply
                        .body()
                        .deserialize::<zbus::zvariant::OwnedValue>()
                        .ok()
                })
                .and_then(|value| bool::try_from(value).ok())
                .unwrap_or(false)
        })
    }

    impl Adapter {
        // Subscribes before returning, so a switch turned on from now on is
        // seen; a thread reads the signals and wakes the session.
        fn watch_status(&mut self, session: Connection) -> Result<(), &'static str> {
            let rule = zbus::MatchRule::builder()
                .msg_type(MessageType::Signal)
                .sender("org.a11y.Bus")
                .and_then(|b| b.path("/org/a11y/bus"))
                .and_then(|b| b.interface("org.freedesktop.DBus.Properties"))
                .and_then(|b| b.member("PropertiesChanged"))
                .and_then(|b| b.add_arg(STATUS))
                .map(|b| b.build())
                .map_err(|_| "Cannot watch the accessibility status")?;
            let signals = MessageIterator::for_match_rule(rule, &session, Some(4))
                .map_err(|_| "Cannot watch the accessibility status")?;
            let enabled = Arc::new(AtomicBool::new(false));
            let seen = enabled.clone();
            let waker = self.waker.clone();
            let thread = std::thread::Builder::new()
                .name("gui-a11y-status".to_string())
                .spawn(move || {
                    for message in signals.flatten() {
                        type Changed = (
                            String,
                            HashMap<String, zbus::zvariant::OwnedValue>,
                            Vec<String>,
                        );
                        let Ok((_, changed, _)) = message.body().deserialize::<Changed>() else {
                            continue;
                        };
                        let on = SWITCHES.iter().any(|switch| {
                            changed
                                .get(*switch)
                                .and_then(|v| v.try_clone().ok())
                                .and_then(|v| bool::try_from(v).ok())
                                .unwrap_or(false)
                        });
                        if on {
                            seen.store(true, Ordering::Release);
                            if let Some(w) = &*waker.lock() {
                                let _ = w.wake();
                            }
                            return;
                        }
                    }
                })
                .map_err(|_| "Cannot watch the accessibility status")?;
            // A switch turned on between the first read and the subscription.
            if assistive_technology_enabled(&session) {
                enabled.store(true, Ordering::Release);
            }
            self.status_watch = Some(StatusWatch {
                connection: session,
                thread,
                enabled,
            });
            Ok(())
        }
    }

    // The accessibility bus address the session bus's a11y service hands out.
    fn bus_address(session: &Connection) -> Result<String, &'static str> {
        let reply = session
            .call_method(
                Some("org.a11y.Bus"),
                "/org/a11y/bus",
                Some("org.a11y.Bus"),
                "GetAddress",
                &(),
            )
            .map_err(|_| "Accessibility bus unavailable")?;
        reply
            .body()
            .deserialize::<String>()
            .map_err(|_| "Accessibility bus unavailable")
    }

    // Forwards calls and replies until the connection closes; the session
    // answers them on its own thread.
    fn read(messages: MessageIterator, inbox: Sender<Message>, waker: Arc<Mutex<Option<Waker>>>) {
        for message in messages.flatten() {
            if !matches!(
                message.message_type(),
                MessageType::MethodCall | MessageType::MethodReturn
            ) {
                continue;
            }
            if inbox.send(message).is_err() {
                return;
            }
            if let Some(w) = &*waker.lock() {
                let _ = w.wake();
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::io::{BufRead, BufReader};
        use std::process::{Child, Command, Stdio};

        use super::*;

        // A message bus of the test's own, killed when it is dropped.
        struct Bus {
            daemon: Child,
            address: String,
        }

        impl Bus {
            fn start() -> Bus {
                let mut daemon = Command::new("dbus-daemon")
                    .args(["--session", "--nofork", "--print-address=1"])
                    .stdout(Stdio::piped())
                    .spawn()
                    .expect("dbus-daemon runs");
                let mut address = String::new();
                BufReader::new(daemon.stdout.take().expect("piped stdout"))
                    .read_line(&mut address)
                    .expect("the daemon prints its address");
                Bus {
                    daemon,
                    address: address.trim().to_string(),
                }
            }

            fn connect(&self) -> Connection {
                connection::Builder::address(self.address.as_str())
                    .and_then(|b| b.build())
                    .expect("a connection to the test bus")
            }
        }

        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.daemon.kill();
                let _ = self.daemon.wait();
            }
        }

        // Answers `Get` for the status switches, all off, as a desktop with
        // no assistive technology does.
        fn serve_status(service: &Connection) -> JoinHandle<()> {
            service
                .request_name("org.a11y.Bus")
                .expect("the service name");
            let calls = MessageIterator::from(service);
            let replies = service.clone();
            std::thread::spawn(move || {
                for call in calls.flatten() {
                    if call.message_type() == MessageType::MethodCall
                        && call.header().member().is_some_and(|m| m == "Get")
                    {
                        let off = zbus::zvariant::Value::from(false);
                        let _ = replies.reply(&call.header(), &off);
                    }
                }
            })
        }

        #[test]
        fn the_bus_is_joined_only_once_the_desktop_turns_assistive_technology_on() {
            let bus = Bus::start();
            let service = bus.connect();
            let server = serve_status(&service);
            let session = bus.connect();
            assert!(!assistive_technology_enabled(&session));
            let mut adapter = Adapter::detached("test");
            adapter.watch_status(session).expect("the status watch");
            let mut changed = HashMap::new();
            changed.insert("ScreenReaderEnabled", zbus::zvariant::Value::from(true));
            service
                .emit_signal(
                    None::<&str>,
                    "/org/a11y/bus",
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    &(STATUS, changed, Vec::<String>::new()),
                )
                .expect("the change is sent");
            let watch = adapter.status_watch.take().expect("a watch while off");
            // The watcher ends once it has seen a switch turn on.
            watch.thread.join().expect("the watcher ends");
            assert!(watch.enabled.load(Ordering::Acquire));
            let _ = watch.connection.close();
            let _ = service.close();
            server.join().expect("the service ends");
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub use crate::accessibility::native::Adapter;
#[cfg(target_os = "linux")]
pub use linux::Adapter;
