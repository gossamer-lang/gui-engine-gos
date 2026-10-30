// Sessions by id, and the session methods a Gossamer program calls, each
// taking its arguments as a JSON array and answering JSON.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::mem::ManuallyDrop;

use gui_engine::app::Session;
use gui_engine::errors;
use serde::Serialize;
use serde::de::DeserializeOwned;

thread_local! {
    // A session the program leaves open is not shut down with the thread:
    // by then the runtime its callbacks run in is gone.
    static SESSIONS: ManuallyDrop<RefCell<HashMap<i64, Session>>> =
        ManuallyDrop::new(RefCell::new(HashMap::new()));
    // The update, view, and commands callbacks each session was given.
    static SHELLS: RefCell<HashMap<i64, Vec<i64>>> = RefCell::new(HashMap::new());
    static NEXT: Cell<i64> = const { Cell::new(1) };
}

/// Keeps `s` and answers its id.
pub fn add(s: Session) -> i64 {
    let id = NEXT.with(|n| n.replace(n.get() + 1));
    SESSIONS.with(|m| m.borrow_mut().insert(id, s));
    id
}

/// Runs `f` on session `id`. The session is out of the table while `f`
/// runs, so a callback reaching the same session finds it busy instead of
/// aliasing it.
pub fn with<T>(id: i64, f: impl FnOnce(&mut Session) -> Result<T, String>) -> Result<T, String> {
    let mut s = SESSIONS
        .with(|m| m.borrow_mut().remove(&id))
        .ok_or_else(|| format!("session {id} is closed or busy"))?;
    let out = f(&mut s);
    SESSIONS.with(|m| m.borrow_mut().insert(id, s));
    out
}

/// Ends session `id` and drops it, letting go of its shell's callbacks.
pub fn close(id: i64) {
    crate::outside::close_mailboxes(id);
    if let Some(mut s) = SESSIONS.with(|m| m.borrow_mut().remove(&id)) {
        s.shutdown();
    }
    if let Some(shell) = SHELLS.with_borrow_mut(|m| m.remove(&id)) {
        for callback in shell {
            crate::host::release(callback)
        }
    }
}

fn args<T: DeserializeOwned>(a: &str) -> Result<T, String> {
    serde_json::from_str(a).map_err(|e| format!("bad arguments: {e}"))
}

fn out(v: impl Serialize) -> Result<String, String> {
    serde_json::to_string(&v).map_err(|e| e.to_string())
}

// The answer of a method that answers nothing.
fn done() -> Result<String, String> {
    Ok("null".to_string())
}

fn res(r: Result<impl Serialize, errors::Error>) -> Result<String, String> {
    out(r.map_err(|e| e.to_string())?)
}

fn window(s: &Session, w: i64) -> Result<&gui_engine::app::WindowState, String> {
    usize::try_from(w)
        .ok()
        .and_then(|i| s.windows.get(i))
        .ok_or_else(|| format!("window {w} is not open"))
}

// Activation is explicit: ordinary applications should keep using focus(),
// while a launcher must take keyboard focus after its user asks to open it.
fn activate_window(native: windowing::Window) -> Result<(), String> {
    native.focus().map_err(|e| e.to_string())?;
    native.sync().map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    if windowing::video_driver() == "x11" && native.is_visible() {
        use x11_dl::xlib;
        let windowing::surface::RawHandle::Xlib {
            display, window, ..
        } = native.raw_handle().map_err(|e| e.to_string())?
        else {
            return Err("the window has no X11 handle".to_string());
        };
        let x = xlib::Xlib::open().map_err(|e| e.to_string())?;
        let display = display.cast::<xlib::Display>();
        let mut data = xlib::ClientMessageData::new();
        data.set_long(0, 2); // A direct user activation, like a task switcher.
        // SAFETY: SDL owns this live display and mapped window. All event
        // fields are initialized, and XSync completes the focus request.
        unsafe {
            let mut event = xlib::XEvent {
                client_message: xlib::XClientMessageEvent {
                    type_: xlib::ClientMessage,
                    serial: 0,
                    send_event: 1,
                    display,
                    window,
                    message_type: (x.XInternAtom)(display, c"_NET_ACTIVE_WINDOW".as_ptr(), 0),
                    format: 32,
                    data,
                },
            };
            (x.XSendEvent)(
                display,
                (x.XDefaultRootWindow)(display),
                0,
                xlib::SubstructureRedirectMask | xlib::SubstructureNotifyMask,
                &mut event,
            );
            (x.XRaiseWindow)(display, window);
            (x.XSetInputFocus)(display, window, xlib::RevertToParent, xlib::CurrentTime);
            (x.XSync)(display, 0);
        }
    }
    Ok(())
}

/// Calls session method `method` with the JSON array `a`.
pub fn call(s: &mut Session, method: &str, a: &str) -> Result<String, String> {
    match method {
        "now" => out(s.now()),
        "open_window" | "prepare_window" => {
            let (key, title, w, h): (String, String, i64, i64) = args(a)?;
            res(if method == "open_window" {
                s.open_window(&key, &title, w, h)
            } else {
                s.prepare_window(&key, &title, w, h)
            })
        }
        "prepare_window_with" => {
            let (key, title, w, h, style) = args(a)?;
            let (key, title): (String, String) = (key, title);
            res(s.prepare_window_with(&key, &title, w, h, style))
        }
        "show_window" => res(s.show_window(&args::<(String,)>(a)?.0)),
        "set_icon" => res(s.set_icon(&args::<(gui_engine::services::apps::AppIcon,)>(a)?.0)),
        "set_min_size" => {
            let (key, w, h): (String, i64, i64) = args(a)?;
            res(s.set_min_size(&key, w, h))
        }
        "grow_for_popups" => {
            let (key, h): (String, i64) = args(a)?;
            res(s.grow_for_popups(&key, h))
        }
        "set_close_policy" => {
            let (key, policy): (String, _) = args(a)?;
            s.set_close_policy(&key, policy);
            done()
        }
        "step" => res(s.step()),
        "flush" => res(s.flush()),
        "hold_jobs_for_run" => {
            s.hold_jobs_for_run();
            done()
        }
        "run_command" => {
            let (w, id): (i64, String) = args(a)?;
            window(s, w)?;
            s.run_command(w, &id);
            done()
        }
        "set_focus" => {
            let (w, id): (i64, String) = args(a)?;
            window(s, w)?;
            s.set_focus(w, &id);
            done()
        }
        "spoken" => out(s.spoken()),
        "rebuild" => {
            let w = args::<(i64,)>(a)?.0;
            window(s, w)?;
            s.rebuild(w);
            done()
        }
        "close_window" => {
            let w = args::<(i64,)>(a)?.0;
            window(s, w)?;
            s.close_window(w);
            done()
        }
        "open_dialog_in" => {
            let (w, req): (i64, _) = args(a)?;
            window(s, w)?;
            s.open_dialog_in(w, &req);
            done()
        }
        "open_dialog_as" => {
            let (w, req, modality): (i64, _, _) = args(a)?;
            window(s, w)?;
            s.open_dialog_as(w, &req, modality);
            done()
        }
        "close_dialog" => {
            let (id, outcome): (String, _) = args(a)?;
            s.close_dialog(&id, &outcome);
            done()
        }
        "render" => {
            let w = args::<(i64,)>(a)?.0;
            window(s, w)?;
            res(s.render(w))
        }
        "semantics" => {
            let w = args::<(i64,)>(a)?.0;
            window(s, w)?;
            out(s.semantics(w))
        }
        "accessibility_status" => out(s.accessibility_status()),
        "accessibility_endpoint" => out(s.accessibility_endpoint()),
        "root" => out(s.root()),
        "handle" => res(s.handle(&args::<(windowing::Event,)>(a)?.0)),
        "inject" => res(s.inject(&args::<(windowing::Event,)>(a)?.0)),
        "set_active" => res(s.set_active(args::<(bool,)>(a)?.0)),
        "rect_of" => out(s.rect_of(&args::<(String,)>(a)?.0)),
        "click" => res(s.click(&args::<(String,)>(a)?.0)),
        "click_at" | "right_click_at" | "hover" | "press_pointer" | "release_pointer" => {
            let (x, y): (i64, i64) = args(a)?;
            res(match method {
                "click_at" => s.click_at(x, y),
                "right_click_at" => s.right_click_at(x, y),
                "hover" => s.hover(x, y),
                "press_pointer" => s.press_pointer(x, y),
                _ => s.release_pointer(x, y),
            })
        }
        "move_pointer" => {
            let (x, y, held): (i64, i64, bool) = args(a)?;
            res(s.move_pointer(x, y, held))
        }
        "scroll_at" => {
            let (x, y, dy): (i64, i64, f64) = args(a)?;
            res(s.scroll_at(x, y, dy))
        }
        "cursor" => out(s.cursor()),
        "drag" => {
            let (x, y, tx, ty): (i64, i64, i64, i64) = args(a)?;
            res(s.drag(x, y, tx, ty))
        }
        "accessible_action" => {
            let (owner, target, action, value): (String, String, String, String) = args(a)?;
            res(s.accessible_action(&owner, &target, &action, &value))
        }
        "hold_key" => {
            let (key, mods, repeat) = args(a)?;
            res(s.hold_key(key, mods, repeat))
        }
        "release_key" | "press" => {
            let (key, mods) = args(a)?;
            res(if method == "press" {
                s.press(key, mods)
            } else {
                s.release_key(key, mods)
            })
        }
        "type_text" => res(s.type_text(&args::<(String,)>(a)?.0)),
        "compose" => {
            let (text, cursor): (String, i64) = args(a)?;
            res(s.compose(&text, cursor))
        }
        "focus" => res(s.focus(&args::<(String,)>(a)?.0)),
        "focused" => out(s.focused()),
        "advance" => res(s.advance(args::<(i64,)>(a)?.0)),
        "dialog_ids" => out(s.dialog_ids()),
        "popup_owners" => out(s.popup_owners()),
        "emit" => {
            s.emit(&args::<(gui_engine::widgets::cx::UiMsg,)>(a)?.0);
            done()
        }
        "screenshot" => res(s.screenshot(&args::<(String,)>(a)?.0)),
        "capture_window" => res(s
            .capture_window(args::<(i64,)>(a)?.0)
            .map(|c| (c.width, c.height))),
        "hide_window" | "focus_window" => {
            let native = window(s, args::<(i64,)>(a)?.0)?.native;
            if method == "hide_window" {
                native.hide().map_err(|e| e.to_string())?;
            } else {
                native.focus().map_err(|e| e.to_string())?;
            }
            done()
        }
        "activate_window" => {
            activate_window(window(s, args::<(i64,)>(a)?.0)?.native)?;
            done()
        }
        "window_visible" => out(window(s, args::<(i64,)>(a)?.0)?.native.is_visible()),
        "resource_usage" => out(s.resource_usage()),
        "set_memory_limit" => res(s.set_memory_limit(args::<(usize,)>(a)?.0)),
        "window_count" => out(s.windows.len() as i64),
        "window_pixel_size" => out(window(s, args::<(i64,)>(a)?.0)?.native.pixel_size()),
        "window_size" => out(window(s, args::<(i64,)>(a)?.0)?.node.rect.size()),
        "document_handle" => {
            let (w, id): (i64, String) = args(a)?;
            let win = window(s, w)?;
            let path = gui_engine::widgets::tree::path_to(&win.root, &win.node, &win.theme, &id);
            let handle = path
                .last()
                .and_then(|entry| {
                    if let gui_engine::widgets::Widget::TextInput(input) = &entry.widget {
                        Some(input.document)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            out(handle)
        }
        "edit_state" => {
            let (w, id): (i64, String) = args(a)?;
            out(window(s, w)?.states.edit(&id))
        }
        "part_rect" => {
            let (w, id, part): (i64, String, i64) = args(a)?;
            let win = window(s, w)?;
            let path = gui_engine::widgets::tree::path_to(&win.root, &win.node, &win.theme, &id);
            out(path.last().and_then(|e| {
                let parts = &e.node.parts;
                usize::try_from(part)
                    .ok()
                    .and_then(|k| parts.get(k))
                    .map(|r| r.offset(e.x, e.y))
            }))
        }
        "log" => out(&s.log),
        "window_theme" => out(&window(s, args::<(i64,)>(a)?.0)?.theme),
        "window_id" => out(window(s, args::<(i64,)>(a)?.0)?.native.id().0),
        "rendering_stats" => {
            let w = args::<(i64,)>(a)?.0;
            out(s.rendering_stats(w).cloned().unwrap_or_default())
        }
        "set_render_budgets" => {
            let (w, scene, layers): (i64, usize, usize) = args(a)?;
            res(s.set_render_budgets(w, scene, layers))
        }
        "window_frames" => out(window(s, args::<(i64,)>(a)?.0)?.frames),
        "resize_window" => {
            let (w, width, height): (i64, i64, i64) = args(a)?;
            let native = window(s, w)?.native;
            native.set_size(width, height).map_err(|e| e.to_string())?;
            native.sync().map_err(|e| e.to_string())?;
            done()
        }
        "choice_state" => {
            let (w, id): (i64, String) = args(a)?;
            out(window(s, w)?.states.choice(&id))
        }
        "dialog_draft" => {
            let id: String = args::<(String,)>(a)?.0;
            let inst = s
                .windows
                .iter()
                .flat_map(|win| win.dialogs.iter())
                .find(|d| d.id == id)
                .ok_or_else(|| format!("dialog {id} is not open"))?;
            out((&inst.text, &inst.values))
        }
        _ => Err(format!("unknown session method {method}")),
    }
}

/// Records the callbacks session `id`'s shell runs, which `close` releases.
pub fn keep_shell(id: i64, shell: [i64; 3]) {
    if let Some(previous) = SHELLS.with_borrow_mut(|m| m.insert(id, shell.to_vec())) {
        for callback in previous {
            crate::host::release(callback);
        }
    }
}

pub fn keep_callback(id: i64, callback: i64) {
    SHELLS.with_borrow_mut(|shells| shells.entry(id).or_default().push(callback));
}
