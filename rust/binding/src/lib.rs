//! The Gossamer binding of the GUI toolkit: sessions by id, their methods
//! over JSON, and Gossamer closures the toolkit calls back.

mod host;
mod outside;
mod session;

use gossamer_binding::{Bytes, PersistentCallback, register_module};
use gui_engine::app::Session;
use gui_engine::widgets::Widget;
use gui_engine::widgets::cx::UiMsg;

// A Gossamer closure the shell runs; its failure is a failure of the
// application's own code, so it ends the call as that code's panic would.
fn hosted(id: i64, args: &str) -> String {
    match host::call(id, args) {
        Ok(out) => out,
        Err(e) => panic!("{e}"),
    }
}

fn decode<T: serde::de::DeserializeOwned>(what: &str, text: &str) -> T {
    match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => panic!("the application's {what} is not valid: {e}"),
    }
}

register_module!(
    name: gui_engine_host,
    doc: "The GUI toolkit's sessions and callbacks.",

    cb_fn register(d, f: PersistentCallback) -> i64 {
        let _ = d;
        gui_engine::callback::set_host(host::call);
        host::register(f)
    }

    fn release(id: i64) {
        host::release(id)
    }

    fn start(name: String, app_id: String) -> Result<i64, String> {
        gui_engine::callback::set_host(host::call);
        gui_engine::app::start(&name, &app_id).map(session::add).map_err(|e| e.to_string())
    }

    cb_fn close(d, s: i64) {
        // Shutting down delivers the session's last messages to `update`.
        let _scope = host::Scope::enter(d);
        session::close(s)
    }

    cb_fn shell(d, s: i64, update: i64, view: i64, commands: i64) -> Result<(), String> {
        let _scope = host::Scope::enter(d);
        session::keep_shell(s, [update, view, commands]);
        session::with(s, |session: &mut Session| {
            session.set_shell(
                move |m: UiMsg| {
                    let msg = serde_json::to_string(&m).unwrap_or_else(|e| panic!("a message did not encode: {e}"));
                    hosted(update, &msg);
                },
                move |key: &str| decode::<Widget>("view", &hosted(view, &serde_json::to_string(key).unwrap_or_default())),
                move |key: &str| decode("commands", &hosted(commands, &serde_json::to_string(key).unwrap_or_default())),
            );
            Ok(())
        })
    }

    cb_fn on_window_disposed(d, s: i64, callback: i64) -> Result<(), String> {
        let _scope = host::Scope::enter(d);
        session::keep_callback(s, callback);
        session::with(s, |session| {
            session.on_window_disposed(move |key| { hosted(callback, &serde_json::to_string(key).unwrap()); });
            Ok(())
        })
    }

    fn callback_count() -> i64 { host::count() as i64 }

    cb_fn run(d, s: i64) -> Result<(), String> {
        let _scope = host::Scope::enter(d);
        let result=session::with(s, |session| session.run().map_err(|e| e.to_string()));
        outside::close_mailboxes(s);
        result
    }

    cb_fn call(d, s: i64, method: String, args: String) -> Result<String, String> {
        let _scope = host::Scope::enter(d);
        session::with(s, |session| session::call(session, &method, &args))
    }

    fn perform(effect: String) -> Result<(), String> {
        let effect = serde_json::from_str(&effect).map_err(|e| e.to_string())?;
        gui_engine::app::effects::try_perform(effect)
    }

    fn rich_preview(id: i64, prefix: String, start: i64, count: i64) -> Result<String, String> {
        let value = gui_engine::rich::preview::widget(id, &prefix, start.max(0) as usize, count.max(0) as usize)?;
        serde_json::to_string(&value).map_err(|e| e.to_string())
    }

    fn rich_input(id: i64, block: i64, owner: String) -> Result<String, String> {
        serde_json::to_string(&gui_engine::rich::preview::input(id, block, &owner)?).map_err(|e| e.to_string())
    }

    fn rich(method: String, id: i64, args: String) -> Result<String, String> {
        gui_engine::rich::api::call(&method, id, &args)
    }

    fn terminal(method: String, id: i64, args: String) -> Result<String, String> {
        gui_engine::terminal::api::call(&method, id, &args)
    }

    fn media(method: String, id: i64, args: String) -> Result<String, String> {
        gui_engine::media::api::call(&method, id, &args)
    }

    fn image_viewer(method: String, id: i64, args: String) -> Result<String, String> {
        gui_engine::imageview::api::call(&method, id, &args)
    }

    fn scene(method: String, id: i64, args: String) -> Result<String, String> {
        gui_engine::scene::api::call(&method, id, &args)
    }

    fn chart(method: String, id: i64, args: String) -> Result<String, String> {
        gui_engine::chart::api::call(&method, id, &args)
    }

    fn printing(method: String, args: String) -> Result<String, String> {
        gui_engine::print::api::call(&method, &args)
    }

    fn dock(method: String, args: String) -> Result<String, String> {
        gui_engine::dock::api::call(&method, &args)
    }

    fn document(method: String, id: i64, args: String) -> Result<String, String> {
        gui_engine::editing::document_api::call(&method, id, &args)
    }

    fn analyze(document: String, text: String) -> Result<String, String> {
        let mut document: gui_engine::text::syntax::Document = serde_json::from_str(&document).map_err(|e| e.to_string())?;
        document.update(&text);
        serde_json::to_string(&document).map_err(|e| e.to_string())
    }

    fn take_effects() -> String {
        serde_json::to_string(&gui_engine::app::effects::take()).unwrap_or_else(|e| panic!("effects did not encode: {e}"))
    }

    fn set_config_root(root: String) {
        gui_engine::services::set_config_root(&root)
    }

    fn use_system_fonts(enabled: bool) {
        gui_engine::text::fonts::use_system_fonts(enabled)
    }

    fn clipboard_use_memory(memory: bool) {
        gui_engine::services::clipboard::use_memory(memory)
    }

    fn app_icon_drawn(icon: String, size: i64, background: String, foreground: String) -> Result<String, String> {
        let icon = serde_json::from_str(&icon).map_err(|e| format!("icon: {e}"))?;
        let background = serde_json::from_str(&background).map_err(|e| format!("background: {e}"))?;
        let foreground = serde_json::from_str(&foreground).map_err(|e| format!("foreground: {e}"))?;
        let drawn = gui_engine::services::apps::AppIcon::drawn(&icon, size, background, foreground);
        serde_json::to_string(&drawn).map_err(|e| e.to_string())
    }

    fn apps_with_icons(entries: String, size: i64) -> Result<String, String> {
        let entries: Vec<gui_engine::services::apps::AppEntry> =
            serde_json::from_str(&entries).map_err(|e| e.to_string())?;
        serde_json::to_string(&gui_engine::services::apps::with_icons(&entries, size))
            .map_err(|e| e.to_string())
    }

    fn app_icon_png(icon: String) -> Result<Bytes, String> {
        let icon: gui_engine::services::apps::AppIcon =
            serde_json::from_str(&icon).map_err(|e| format!("icon: {e}"))?;
        Ok(Bytes::from(icon.to_png()))
    }

    fn localize(op: String, args: String) -> Result<String, String> {
        use gui_engine::localization as l10n;
        let json = |v: serde_json::Result<String>| v.map_err(|e| e.to_string());
        match op.as_str() {
            "install" => {
                let catalog: l10n::Catalog = serde_json::from_str(&args).map_err(|e| format!("catalog: {e}"))?;
                l10n::try_install(&catalog)?;
                Ok("null".to_string())
            }
            "locale" => json(serde_json::to_string(&l10n::locale())),
            "rtl" => json(serde_json::to_string(&l10n::rtl())),
            "tr" => {
                let source: String = serde_json::from_str(&args).map_err(|e| e.to_string())?;
                json(serde_json::to_string(&l10n::tr(&source)))
            }
            "message" => {
                let (context, source, count, values): (String, String, i64, std::collections::HashMap<String, String>) =
                    serde_json::from_str(&args).map_err(|e| e.to_string())?;
                json(serde_json::to_string(&l10n::message(&context, &source, count, &values)))
            }
            _ => Err(format!("unknown localization operation {op}")),
        }
    }

    fn mailbox(s: i64) -> Result<i64, String> {
        session::with(s, |session| outside::add_mailbox(s, session.mailbox()))
    }

    fn post(mailbox: i64, msg: String) -> Result<(), String> {
        outside::post(mailbox, &msg)
    }

    fn release_mailbox(mailbox: i64) {
        outside::release_mailbox(mailbox)
    }

    fn claim(app_id: String, files: String) -> Result<String, String> {
        outside::claim(&app_id, &files)
    }

    fn serve_instance(s: i64, listener: i64) -> Result<(), String> {
        let listener = outside::take_listener(listener)?;
        session::with(s, |session| {
            session.serve_instance(listener);
            Ok(())
        })
    }

    fn close_listener(listener: i64) {
        outside::close_listener(listener)
    }
);
