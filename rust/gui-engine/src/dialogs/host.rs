// host: the toolkit dialog host. Each open dialog is an `Instance`: its
// request, its drafts (never the caller's committed values), and the
// widget tree built from them. Controls inside a dialog use ids prefixed
// `dlg:<id>:`, so the session routes their messages here instead of to
// the application. `update` answers at most one outcome, which closes the
// dialog; Escape and the close box answer the cancel policy, Enter the
// enabled default, and neither ever implies a destructive choice.

use crate::strext::StrExt;
use std::collections::HashMap;
use windowing;

use crate::accessibility::Role;
use crate::app::effects::Effect;
use crate::commands::Command;
use crate::dialogs::Outcome;
use crate::dialogs::request::{
    ButtonRole, DialogAction, DialogRequest, FileBackend, FileMode, InputSpec, Modality, Severity,
};
use crate::editing::Validity;
use crate::input::platform_is_macos;
use crate::render::icons::Icon;
use crate::style::{TextRole, Theme};
use crate::widgets::completion::{CompletionItem, MatchMode, rank};
use crate::widgets::cx::{Node, UiMsg};
use crate::widgets::state::StateStore;
use crate::widgets::textinput::{InputKind, TextInput};
use crate::widgets::{CustomInput, Tone, Widget};

/// An open toolkit dialog.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Instance {
    pub files: Vec<crate::dialogs::files::Entry>,
    pub id: String,
    pub request: DialogRequest,
    pub root: Widget,
    pub node: Node,
    pub app_id: String,
    /// The input draft, palette query, or shortcut being captured.
    pub text: String,
    pub error: String,
    pub expanded: bool,
    pub values: HashMap<String, String>,
    pub page: i64,
    pub selected: String,
    pub modality: Modality,
    /// Where focus was when a modeless dialog opened; it returns there when
    /// the dialog closes holding focus.
    pub opener: String,
    /// Whether the dialog fills a window of its own rather than floating
    /// over the window that asked for it.
    pub fills_window: bool,
}

impl Instance {
    pub fn new(req: &DialogRequest, app_id: &str) -> Instance {
        let text = match req {
            DialogRequest::Input { initial, .. } | DialogRequest::Shortcut { initial, .. } => {
                initial
            }
            _ => &String::new(),
        };
        let inst = Instance {
            files: vec![],
            id: req.id(),
            request: req.clone(),
            root: Widget::Empty,
            node: Node::default(),
            app_id: app_id.to_string(),
            text: text.to_string(),
            error: String::new(),
            expanded: false,
            values: HashMap::new(),
            page: 0,
            selected: String::new(),
            modality: Modality::Window,
            opener: String::new(),
            fills_window: false,
        };
        crate::dialogs::pickers::initialize(inst)
    }

    pub fn title(&self) -> String {
        self.request.title()
    }

    /// Replaces the request (progress updates) keeping drafts.
    pub fn update_request(&mut self, req: &DialogRequest) {
        self.request = req.clone();
    }

    fn cid(&self, part: &str) -> String {
        format!("dlg:{}:{}", self.id, part)
    }
}

/// How a request is shown.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Plan {
    Toolkit,
    /// A native file dialog: title, location, filters (name, `;`-joined
    /// extensions), several files, and kind (0 open, 1 save, 2 folder).
    Native(String, String, Vec<(String, String)>, bool, i64),
    Refuse(String),
}

/// Chooses the backend for a request (native file dialogs when allowed
/// and able to meet the requirements).
pub fn native_plan(req: &DialogRequest) -> Plan {
    match req {
        DialogRequest::File {
            mode,
            title,
            directory,
            name,
            filters,
            backend,
            needs_favorites,
            ..
        } => {
            let multiple_folders = *mode == FileMode::FolderMany;
            let native_ok = !needs_favorites && !multiple_folders;
            let use_native = match backend {
                FileBackend::Native => {
                    if !native_ok {
                        let why = if *needs_favorites {
                            "the native file dialog cannot show application favorites"
                        } else {
                            "the native folder dialog cannot select several folders"
                        };
                        return Plan::Refuse(why.to_string());
                    }
                    true
                }
                // A headless session has no desktop to host a native chooser.
                FileBackend::Auto => native_ok && windowing::video_driver() != "offscreen",
                FileBackend::Toolkit => false,
            };
            if !use_native {
                return Plan::Toolkit;
            }
            let mut wfilters = vec![];
            for f in filters {
                wfilters.push((
                    f.name.clone(),
                    f.patterns
                        .iter()
                        .map(|p| p.replace("*.", ""))
                        .collect::<Vec<_>>()
                        .join(";"),
                ))
            }
            let location = if name.is_empty() {
                directory.clone()
            } else if directory.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", directory, name)
            };
            let kind = match mode {
                FileMode::Open | FileMode::OpenMany => 0,
                FileMode::Save => 1,
                _ => 2,
            };
            Plan::Native(
                title.to_string(),
                location,
                wfilters,
                *mode == FileMode::OpenMany,
                kind,
            )
        }
        _ => Plan::Toolkit,
    }
}

/// The accessibility role of a dialog.
pub fn dialog_role(inst: &Instance) -> Role {
    match inst.request {
        // A question that can destroy work interrupts like a warning does.
        DialogRequest::Message {
            severity,
            ref actions,
            ..
        } => {
            if severity == Severity::Error
                || severity == Severity::Warning
                || actions.iter().any(|a| a.role == ButtonRole::Destructive)
            {
                Role::AlertDialog
            } else {
                Role::Dialog
            }
        }
        _ => Role::Dialog,
    }
}

/// What opening `inst` says to assistive technology: its title, and for an
/// alert its message as well, since an alert interrupts to be read.
pub fn announcement(inst: &Instance) -> String {
    let title = crate::localization::message(
        "dialog",
        "{title} dialog",
        1,
        &crate::hmap! { "title" => inst.title() },
    );
    match inst.request {
        DialogRequest::Message { ref message, .. } => {
            if dialog_role(inst) == Role::AlertDialog && !message.is_empty() {
                format!("{}. {}", title, message)
            } else {
                title
            }
        }
        _ => title,
    }
}

/// The request for the command palette over `commands`.
pub fn palette_request(commands: &[Command]) -> DialogRequest {
    let mut options = vec![];
    for c in commands {
        let label = if c.enabled {
            c.label.clone()
        } else {
            format!("{} (unavailable)", c.label)
        };
        let shortcut = c.shortcut_label();
        options.push((
            c.id.clone(),
            if shortcut.is_empty() {
                label
            } else {
                format!("{}\u{2003}{}", label, shortcut)
            },
        ))
    }
    crate::dialogs::request::choose(
        "gui:palette",
        "Command Palette",
        "Type a command",
        &options,
        false,
    )
}

// Buttons in platform order: destructive and help first, then on Windows
// accept before reject, elsewhere reject before accept.
fn ordered(actions: &[DialogAction]) -> Vec<DialogAction> {
    let of = |roles: &[ButtonRole]| {
        actions
            .iter()
            .filter(|a| roles.contains(&a.role))
            .cloned()
            .collect::<Vec<_>>()
    };
    let accept = of(&[ButtonRole::Accept, ButtonRole::Apply]);
    let reject = of(&[ButtonRole::Reject]);
    let mut out = of(&[ButtonRole::Destructive, ButtonRole::Help]);
    out.extend(of(&[ButtonRole::Other]));
    if !platform_is_macos() && is_windows() {
        out.extend(accept);
        out.extend(reject);
    } else {
        out.extend(reject);
        out.extend(accept);
    }
    out
}

#[cfg(target_os = "windows")]
fn is_windows() -> bool {
    true
}

#[cfg(not(target_os = "windows"))]
fn is_windows() -> bool {
    false
}

fn actions_of(inst: &Instance) -> Vec<DialogAction> {
    match inst.request {
        DialogRequest::Message { ref actions, .. } => actions.to_vec(),
        DialogRequest::Input { .. } => vec![
            DialogAction::new("ok", "OK", ButtonRole::Accept),
            DialogAction::new("cancel", "Cancel", ButtonRole::Reject),
        ],
        DialogRequest::Shortcut { .. } => vec![
            DialogAction::new("clear", "Clear", ButtonRole::Other),
            DialogAction::new("ok", "Set", ButtonRole::Accept),
            DialogAction::new("cancel", "Cancel", ButtonRole::Reject),
        ],
        DialogRequest::Progress { cancellable, .. } => {
            if cancellable {
                vec![DialogAction::new("cancel", "Cancel", ButtonRole::Reject)]
            } else {
                vec![]
            }
        }
        DialogRequest::About { .. } => vec![DialogAction::new("ok", "Close", ButtonRole::Accept)],
        _ => crate::dialogs::pickers::actions(inst),
    }
}

/// The control the dialog focuses first.
pub fn initial_focus(inst: &Instance) -> String {
    match inst.request {
        DialogRequest::Input { .. } => inst.cid("field"),
        DialogRequest::Shortcut { .. } => inst.cid("capture"),
        DialogRequest::Message { ref actions, .. } => {
            // The safe default: the first accept that is not destructive.
            for a in actions {
                if a.role == ButtonRole::Accept {
                    return inst.cid(&format!("action:{}", a.id));
                }
            }
            for a in actions {
                if a.role == ButtonRole::Reject {
                    return inst.cid(&format!("action:{}", a.id));
                }
            }
            String::new()
        }
        _ => crate::dialogs::pickers::initial_focus(inst),
    }
}

/// The control Enter activates when the focused control does not use it.
pub fn default_action(inst: &Instance) -> String {
    for a in actions_of(inst) {
        if a.role == ButtonRole::Accept {
            return inst.cid(&format!("action:{}", a.id));
        }
    }
    String::new()
}

/// What Escape or the close box answers: the reject action, or
/// `Cancelled`; `None` when the dialog cannot be dismissed (a progress
/// dialog without cancel).
pub fn cancel_outcome(inst: &Instance) -> Option<Outcome> {
    match inst.request {
        DialogRequest::Progress { cancellable, .. } => {
            if cancellable {
                Some(Outcome::Cancelled)
            } else {
                None
            }
        }
        _ => Some(Outcome::Cancelled),
    }
}

fn severity_icon(s: Severity) -> (Icon, Tone, String) {
    match s {
        Severity::Info => (
            Icon::Info,
            Tone::Info,
            crate::localization::tr("Information"),
        ),
        Severity::Warning => (
            Icon::Warning,
            Tone::Warning,
            crate::localization::tr("Warning"),
        ),
        Severity::Error => (Icon::Error, Tone::Danger, crate::localization::tr("Error")),
        Severity::Confirmation => (
            Icon::Info,
            Tone::Accent,
            crate::localization::tr("Question"),
        ),
    }
}

fn button_for(inst: &Instance, a: &DialogAction) -> Widget {
    let id = inst.cid(&format!("action:{}", a.id));
    match a.role {
        ButtonRole::Accept => {
            let mut b = crate::widgets::buttons::Button::new(
                &id,
                &a.label,
                crate::widgets::buttons::ButtonKind::Primary,
            );
            b.is_default = true;
            Widget::Button(b).disabled(
                matches!(inst.request, DialogRequest::Shortcut { .. }) && !inst.error.is_empty(),
            )
        }
        ButtonRole::Destructive => Widget::danger_button(&id, &a.label),
        _ => Widget::button(&id, &a.label),
    }
}

/// The dialog panel: title bar, body, and button row.
pub fn panel(inst: &Instance, body: Widget) -> Widget {
    let close = Widget::tool_button(
        &inst.cid("close"),
        &Icon::Close,
        &crate::localization::tr("Close"),
    );
    let title = Widget::text(&inst.title(), TextRole::Heading, Tone::Normal).grow(1);
    let header = Widget::row(&[title, close], 8);
    let file = matches!(inst.request, DialogRequest::File { .. })
        || matches!(inst.request, DialogRequest::Color { .. })
        || matches!(inst.request, DialogRequest::Font { .. })
        || matches!(inst.request, DialogRequest::Date { .. })
        || matches!(inst.request, DialogRequest::Preferences { .. })
        || matches!(inst.request, DialogRequest::Wizard { .. });
    // The browser already scrolls its places and entries. Keep its toolbar in
    // the bounded body so focusing the list cannot scroll navigation away.
    let browser = matches!(inst.request, DialogRequest::File { .. });
    let shortcut = matches!(inst.request, DialogRequest::Shortcut { .. });
    let mut rows = vec![
        header,
        if browser || shortcut {
            body.grow(1)
        } else if file {
            Widget::scroll_to_fit(&inst.cid("body"), body).grow(1)
        } else {
            body
        },
    ];
    let actions = ordered(&actions_of(inst));
    if !actions.is_empty() {
        let mut buttons = vec![Widget::stretch()];
        for a in actions {
            buttons.push(button_for(inst, &a))
        }
        rows.push(Widget::row(&buttons, 8))
    }
    let panel = Widget::flex(
        crate::widgets::Axis::Vertical,
        &rows,
        16,
        crate::widgets::MainAlign::Start,
        crate::widgets::CrossAlign::Stretch,
    )
    .padding(20)
    .background(Tone::Surface);
    // A dialog in a window of its own takes the whole window, whose frame
    // already sets it apart; one over its window is a bordered panel.
    let panel = if inst.fills_window {
        panel
    } else {
        panel.bordered().min_width(360).max_width(760)
    }
    .key(&format!("dlg:{}", inst.id))
    .role(Role::Dialog)
    .accessible(&inst.title())
    .described(match &inst.request {
        DialogRequest::Message { message, .. } => message,
        _ => "",
    });
    // The browser lists a folder in a fixed area; the other bodies take their
    // content's height and scroll only when the window is shorter.
    if browser && !inst.fills_window {
        panel.height(600)
    } else {
        panel
    }
}

/// Builds the dialog's widget tree from its drafts.
pub fn view(inst: &Instance, theme: &Theme) -> Widget {
    let body = match inst.request {
        DialogRequest::Message {
            severity,
            ref message,
            ref detail,
            ..
        } => {
            let (icon, tone, word) = severity_icon(severity);
            let ic = crate::widgets::display::Icon {
                icon,
                units: 40,
                tone,
                label: word,
            };
            let mut col = vec![Widget::paragraph(message)];
            if !detail.is_empty() {
                let toggle_label = if inst.expanded {
                    crate::localization::tr("Hide details")
                } else {
                    crate::localization::tr("Show details")
                };
                col.push(Widget::row(
                    &[
                        Widget::flat_button(&inst.cid("details"), &toggle_label),
                        Widget::flat_button(
                            &inst.cid("copy"),
                            &crate::localization::tr("Copy details"),
                        ),
                    ],
                    8,
                ));
                if inst.expanded {
                    col.push(
                        Widget::scroll(
                            &inst.cid("detail-scroll"),
                            Widget::selectable(&inst.cid("detail"), detail),
                        )
                        .max_height(220)
                        .bordered()
                        .padding(8),
                    )
                }
            }
            Widget::flex(
                crate::widgets::Axis::Horizontal,
                &[Widget::Icon(ic), Widget::column(&col, 10).grow(1)],
                16,
                crate::widgets::MainAlign::Start,
                crate::widgets::CrossAlign::Start,
            )
        }
        DialogRequest::Input {
            ref label,
            ref spec,
            ..
        } => input_body(inst, label, spec),
        DialogRequest::Shortcut { .. } => {
            let shown = if inst.text.is_empty() {
                "No shortcut"
            } else {
                &inst.text
            };
            let capture_id = inst.cid("capture");
            let input_id = capture_id.clone();
            let capture = crate::widgets::Custom {
                id: capture_id,
                focusable: true,
                role: Role::TextField,
                label: format!("Record shortcut: {shown}"),
                measure: crate::Callback::new(std::rc::Rc::new(move |c, t| {
                    c.constrain(crate::geometry::Size::new(t.px(360), t.px(64)))
                })),
                draw: crate::Callback::new(std::rc::Rc::new(move |_s, _t| vec![])),
                input: crate::Callback::new(std::rc::Rc::new(move |input| match input {
                    CustomInput::KeyPress(key, mods) => captured_key(key, mods)
                        .map(|text| vec![UiMsg::Custom(input_id.clone(), text)])
                        .unwrap_or_default(),
                    _ => vec![],
                })),
                captures_input: false,
            };
            let mut rows = vec![
                Widget::paragraph(
                    "Press the new shortcut, then choose Set. Click the box to record again.",
                ),
                Widget::stack(&[
                    Widget::text_line(shown, TextRole::Heading, Tone::Accent)
                        .padding(16)
                        .bordered(),
                    Widget::Custom(capture),
                ])
                .height(64),
                Widget::muted("Enter saves. Escape cancels."),
            ];
            if !inst.error.is_empty() {
                let mut error = crate::widgets::display::Label::new(&inst.error, TextRole::Small);
                error.wrap = true;
                error.tone = Tone::Danger;
                rows.push(Widget::Text(error));
            }
            Widget::flex(
                crate::widgets::Axis::Vertical,
                &rows,
                12,
                crate::widgets::MainAlign::Start,
                crate::widgets::CrossAlign::Stretch,
            )
        }
        DialogRequest::Progress {
            ref message, value, ..
        } => {
            let mut col = vec![Widget::paragraph(message)];
            col.push(crate::widgets::feedback::progress(&inst.cid("bar"), value));
            Widget::column(&col, 12)
        }
        // The dialog's heading, "About {name}", already names the application.
        DialogRequest::About {
            ref version,
            ref description,
            ref credits,
            ref licenses,
            ref diagnostics,
            ref links,
            ..
        } => {
            let mut col = vec![
                Widget::muted(&crate::localization::message(
                    "about",
                    "Version {version}",
                    1,
                    &crate::hmap! { "version" => version.to_string() },
                )),
                Widget::paragraph(description),
            ];
            if !credits.is_empty() {
                col.push(Widget::paragraph(credits))
            };
            for (label, url) in links {
                col.push(Widget::link(&inst.cid(&format!("link:{}", url)), label))
            }
            if !licenses.is_empty() {
                col.push(
                    Widget::scroll(
                        &inst.cid("licenses"),
                        Widget::selectable(&inst.cid("license-text"), licenses),
                    )
                    .max_height(160)
                    .bordered()
                    .padding(8),
                )
            }
            if !diagnostics.is_empty() {
                col.push(Widget::row(
                    &[
                        Widget::selectable(&inst.cid("diagnostics"), diagnostics).grow(1),
                        Widget::flat_button(&inst.cid("copy"), "Copy"),
                    ],
                    8,
                ))
            }
            Widget::column(&col, 8)
        }
        _ => crate::dialogs::pickers::body(inst, theme),
    };
    panel(inst, body)
}

// The options of a choice input that match `query`, best first.
fn shown_choices(options: &[(String, String)], query: &str) -> Vec<CompletionItem> {
    let mut items = vec![];
    for (key, text) in options {
        items.push(CompletionItem::new(key, text))
    }
    rank(&items, query, MatchMode::Fuzzy, 200)
}

// Whether a multiple-choice input has `key` checked.
fn picked(inst: &Instance, key: &str) -> bool {
    inst.values
        .get(&format!("pick:{}", key))
        .map_or("", String::as_str)
        == "true"
}

fn input_body(inst: &Instance, label: &str, spec: &InputSpec) -> Widget {
    match spec {
        InputSpec::Choice(options, multiple) => {
            // A filtered list: the query narrows; a single choice picks the
            // highlight (Enter, a click, OK), a multiple choice checks rows.
            let shown = shown_choices(options, &inst.text);
            let mut field =
                TextInput::new(&inst.cid("field"), &inst.text, label, InputKind::Search);
            field.label = label.to_string();
            let mut rows = vec![];
            let mut first = true;
            for it in shown {
                if *multiple {
                    rows.push(Widget::checkbox(
                        &inst.cid(&format!("pick:{}", it.key)),
                        &it.label,
                        picked(inst, &it.key),
                    ))
                } else {
                    let highlighted =
                        it.key == inst.selected || (inst.selected.is_empty() && first);
                    let b = Widget::flat_button(&inst.cid(&format!("pick:{}", it.key)), &it.label);
                    rows.push(if highlighted {
                        b.background(Tone::Selection)
                    } else {
                        b
                    })
                }
                first = false;
            }
            if rows.is_empty() {
                rows.push(Widget::muted("No matches"))
            }
            Widget::column(
                &[
                    Widget::input(&field),
                    Widget::scroll(&inst.cid("list"), Widget::column(&rows, 2)).max_height(320),
                ],
                8,
            )
        }
        _ => {
            let kind = match spec {
                InputSpec::Multiline => InputKind::Area,
                InputSpec::Password => InputKind::Password,
                _ => InputKind::Line,
            };
            let mut field = TextInput::new(&inst.cid("field"), &inst.text, "", kind);
            field.label = label.to_string();
            field.error = inst.error.clone();
            if let InputSpec::Number(min, max, _) = spec {
                field.help = crate::localization::message(
                    "input",
                    "A number from {min} to {max}",
                    1,
                    &crate::hmap! { "min" => format!("{}", min), "max" => format!("{}", max) },
                );
            }
            Widget::column(&[Widget::label(label), Widget::input(&field)], 6)
        }
    }
}

fn captured_key(key: windowing::Key, mods: windowing::Mods) -> Option<String> {
    use windowing::Key;
    if matches!(
        key,
        Key::LeftCtrl
            | Key::RightCtrl
            | Key::LeftShift
            | Key::RightShift
            | Key::LeftAlt
            | Key::RightAlt
            | Key::LeftGui
            | Key::RightGui
            | Key::LeftMeta
            | Key::RightMeta
            | Key::LeftHyper
            | Key::RightHyper
            | Key::Mode
            | Key::Level5Shift
            | Key::Unknown
    ) {
        return None;
    }
    // Plain Enter accepts, Escape cancels, and Tab/Shift+Tab traverse controls.
    if (key == Key::Tab && !mods.ctrl && !mods.alt && !mods.gui)
        || (matches!(key, Key::Return | Key::Escape)
            && !mods.ctrl
            && !mods.alt
            && !mods.gui
            && !mods.shift)
    {
        return None;
    }
    Some(key_text(key, mods))
}

fn key_text(k: windowing::Key, m: windowing::Mods) -> String {
    let mut stroke = crate::input::KeyStroke::new(k);
    stroke.ctrl = m.ctrl;
    stroke.alt = m.alt;
    stroke.shift = m.shift;
    stroke.meta = m.gui;
    stroke.label()
}

fn validate_input(inst: &Instance) -> Result<Outcome, String> {
    match inst.request {
        DialogRequest::Input {
            ref spec,
            ref validate,
            ..
        } => {
            if let Some(v) = validate {
                match v(inst.text.clone()) {
                    Validity::Valid => (),
                    Validity::Intermediate(why) | Validity::Invalid(why) => return Err(why),
                }
            }
            match spec {
                InputSpec::Number(min, max, _) => match inst.text.trim().to_f64() {
                    Some(v) => {
                        if v < *min || v > *max {
                            Err(crate::localization::message(
                                "input",
                                "Enter a number from {min} to {max}",
                                1,
                                &crate::hmap! { "min" => format!("{}", min), "max" => format!("{}", max) },
                            ))
                        } else {
                            Ok(Outcome::Text(inst.text.trim().to_string()))
                        }
                    }
                    None => Err("Enter a number".to_string()),
                },
                InputSpec::Choice(options, true) => Ok(Outcome::Keys(
                    options
                        .iter()
                        .filter(|o| picked(inst, &o.0))
                        .cloned()
                        .collect::<Vec<_>>()
                        .iter()
                        .map(|o| o.0.clone())
                        .collect::<Vec<_>>(),
                )),
                InputSpec::Choice(options, false) => {
                    if !inst.selected.is_empty() {
                        return Ok(Outcome::Action(inst.selected.clone()));
                    };
                    let shown = shown_choices(options, &inst.text);
                    if shown.is_empty() {
                        Err("No option matches".to_string())
                    } else {
                        Ok(Outcome::Action(shown[0].key.clone()))
                    }
                }
                _ => Ok(Outcome::Text(inst.text.clone())),
            }
        }
        _ => Ok(Outcome::Accepted),
    }
}

/// Folds one message from the dialog's controls into the instance.
/// Answers the updated instance, the outcome when the dialog ends, and
/// effects to perform (clipboard, opening links).
pub fn update(
    inst: Instance,
    m: &UiMsg,
    st: &mut StateStore,
) -> (Instance, Option<Outcome>, Vec<Effect>) {
    let mut next = inst;
    let mut fx = vec![];
    let prefix = format!("dlg:{}:", next.id);
    let id = crate::app::msg_id(m);
    let part = id.substring(prefix.byte_len(), id.byte_len());
    if part == "close" {
        let (next, outcome) = request_cancel(next);
        return (next, outcome, fx);
    }
    match m {
        UiMsg::Click(_) => {
            if part == "details" {
                next.expanded = !next.expanded;
                return (next, None, fx);
            }
            if part == "copy" {
                let text = match &next.request {
                    DialogRequest::Message {
                        message, detail, ..
                    } => format!("{}\n\n{}", message, detail),
                    DialogRequest::About {
                        name,
                        version,
                        diagnostics,
                        ..
                    } => format!("{} {}\n{}", name, version, diagnostics),
                    _ => String::new(),
                };
                fx.push(Effect::SetClipboard(text));
                fx.push(Effect::Announce("Copied".to_string()));
                return (next, None, fx);
            }
            if part.starts_with("link:") {
                fx.push(Effect::OpenUrl(
                    part.substring(5, part.byte_len()).to_string(),
                ));
                return (next, None, fx);
            }
            if part.starts_with("pick:") {
                let key = part.substring(5, part.byte_len());
                return (next, Some(Outcome::Action(key.to_string())), fx);
            }
            if part.starts_with("action:") {
                let action = part.substring(7, part.byte_len());
                return finish_action(next, action, &fx);
            }
            let (n, o, e) = crate::dialogs::pickers::click(next, part, st);
            for x in e {
                fx.push(x)
            }
            return (n, o, fx);
        }
        UiMsg::Text(_, text) if part == "field" => {
            next.text = text.to_string();
            next.error = String::new();
            next.selected = String::new();
            return (next, None, fx);
        }
        UiMsg::Submit(_, text) if part == "field" => {
            next.text = text.to_string();
            return finish_action(next, "ok", &fx);
        }
        UiMsg::Custom(_, value) if part == "capture" => {
            if value == "Esc" {
                let (next, outcome) = request_cancel(next);
                return (next, outcome, fx);
            }
            let mut conflict = String::new();
            if let DialogRequest::Shortcut { reserved, .. } = &next.request
                && reserved.contains(value)
            {
                conflict = crate::localization::message(
                    "shortcut",
                    "{shortcut} is reserved",
                    1,
                    &crate::hmap! { "shortcut" => value.to_string() },
                )
            }
            next.error = conflict;
            if next.error.is_empty() {
                next.text = value.to_string()
            };
            return (next, None, fx);
        }
        _ => (),
    }
    crate::dialogs::pickers::message(next, m, part, st)
}

// The outcome of an action button.
fn finish_action(
    inst: Instance,
    action: &str,
    fx: &[Effect],
) -> (Instance, Option<Outcome>, Vec<Effect>) {
    let mut next = inst.clone();
    let mut role = ButtonRole::Other;
    let mut found = false;
    for a in actions_of(&inst) {
        if a.id == action {
            role = a.role;
            found = true
        }
    }
    if !found {
        return (next, None, fx.to_vec());
    };
    if role == ButtonRole::Reject {
        return (next, Some(Outcome::Cancelled), fx.to_vec());
    }
    match inst.request {
        DialogRequest::Message { actions, .. } => {
            if actions.len() == 1 && role == ButtonRole::Accept {
                return (next, Some(Outcome::Accepted), fx.to_vec());
            }
            (next, Some(Outcome::Action(action.to_string())), fx.to_vec())
        }
        DialogRequest::Input { .. } => match validate_input(&inst) {
            Ok(o) => (next, Some(o), fx.to_vec()),
            Err(why) => {
                next.error = why;
                (next, None, fx.to_vec())
            }
        },
        DialogRequest::Shortcut { .. } => {
            if action == "clear" {
                next.text.clear();
                next.error.clear();
                let mut effects = fx.to_vec();
                effects.push(Effect::Focus(next.cid("capture")));
                return (next, None, effects);
            }
            if !next.error.is_empty() {
                return (next, None, fx.to_vec());
            }
            (
                next,
                Some(Outcome::Shortcut(inst.text.clone())),
                fx.to_vec(),
            )
        }
        DialogRequest::About { .. } => (next, Some(Outcome::Accepted), fx.to_vec()),
        DialogRequest::Progress { .. } => (next, Some(Outcome::Cancelled), fx.to_vec()),
        _ => crate::dialogs::pickers::action(next, action, fx),
    }
}

impl std::fmt::Display for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Instance({})", self.id)
    }
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Instance {{ id: {:?} }}", self.id)
    }
}

impl std::fmt::Display for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = {
            match self {
                Plan::Toolkit => "Toolkit",
                Plan::Native(_, _, _, _, k) => &format!("Native({})", k),
                Plan::Refuse(why) => &format!("Refuse({})", why),
            }
        };
        f.write_str(text)
    }
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = { self.to_string() };
        f.write_str(&text)
    }
}

/// Dirty settings require an explicit discard; repeated Escape keeps editing.
pub fn request_cancel(inst: Instance) -> (Instance, Option<Outcome>) {
    if matches!(inst.request, DialogRequest::Preferences { .. })
        && crate::dialogs::pickers::dirty(&inst)
    {
        let mut n = inst;
        n.values
            .insert("discard-review".to_string(), "true".to_string());
        (n, None)
    } else {
        (inst.clone(), cancel_outcome(&inst))
    }
}

#[cfg(test)]
mod tests {
    use super::{Instance, announcement, dialog_role, finish_action, ordered, update};
    use crate::accessibility::Role;

    use crate::dialogs::request;
    use crate::dialogs::request::DialogAction;
    use crate::widgets::cx::UiMsg;
    use crate::widgets::state::StateStore;

    #[test]
    fn warnings_errors_and_destructive_questions_are_alerts_that_say_their_message() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let destructive = Instance::new(
            &request::confirm(
                "ask",
                "Replace file?",
                "It already exists.",
                "Replace",
                true,
            ),
            "t",
        );
        assert_eq!(dialog_role(&destructive), Role::AlertDialog);
        assert_eq!(
            announcement(&destructive),
            "Replace file? dialog. It already exists."
        );
        let plain = Instance::new(
            &request::confirm("ask", "Continue?", "Next step.", "Continue", false),
            "t",
        );
        assert_eq!(dialog_role(&plain), Role::Dialog);
        assert_eq!(announcement(&plain), "Continue? dialog");
        assert_eq!(
            dialog_role(&Instance::new(&request::info("i", "Saved", "Done."), "t")),
            Role::Dialog
        );
        assert_eq!(
            dialog_role(&Instance::new(
                &request::confirm_unsaved("u", "notes.txt"),
                "t"
            )),
            Role::AlertDialog
        )
    }

    #[test]
    fn unsaved_changes_answers_typed_actions_and_cancel() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let inst = Instance::new(&request::confirm_unsaved("close", "notes.txt"), "t");
        let mut st = StateStore::new();
        let (_, save, _) = update(
            inst.clone(),
            &UiMsg::Click("dlg:close:action:save".to_string()),
            &mut st,
        );
        assert_eq!(
            save,
            Some(crate::dialogs::Outcome::Action("save".to_string()))
        );
        let (_, discard, _) = update(
            inst.clone(),
            &UiMsg::Click("dlg:close:action:discard".to_string()),
            &mut st,
        );
        assert_eq!(
            discard,
            Some(crate::dialogs::Outcome::Action("discard".to_string()))
        );
        let (_, cancel, _) = update(
            inst.clone(),
            &UiMsg::Click("dlg:close:action:cancel".to_string()),
            &mut st,
        );
        assert_eq!(cancel, Some(crate::dialogs::Outcome::Cancelled));
        let (_, closed, _) = update(inst, &UiMsg::Click("dlg:close:close".to_string()), &mut st);
        assert_eq!(closed, Some(crate::dialogs::Outcome::Cancelled))
    }

    #[test]
    fn unsaved_changes_buttons_are_inside_their_hit_test_ancestors() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        for width in [480, 900] {
            for text_scale in [1.0, 2.0] {
                let theme = crate::style::Theme {
                    text_scale,
                    ..crate::style::Theme::default()
                };
                let size = crate::geometry::Size::new(width, 640);
                let inst = Instance::new(&request::confirm_unsaved("close:doc2", "notes.txt"), "t");
                let widget = super::view(&inst, &theme);
                let mut cx = crate::widgets::cx::LayoutCx::new(&theme, &StateStore::new(), size);
                let node = widget.layout(crate::geometry::Constraints::loose(size), &mut cx);
                for action in ["save", "discard", "cancel"] {
                    let id = format!("dlg:close:doc2:action:{}", action);
                    let path = crate::widgets::tree::path_to(&widget, &node, &theme, &id);
                    assert!(!path.is_empty());
                    let button = path[path.len() - 1].rect();
                    assert!(button.width > 0 && button.height > 0);
                    for parent in path {
                        assert!(parent.rect().contains(button.center_x(), button.center_y()))
                    }
                    let hit = crate::widgets::tree::hit_path(
                        &widget,
                        &node,
                        &theme,
                        button.center_x(),
                        button.center_y(),
                    );
                    assert!(!hit.is_empty());
                    assert_eq!(hit[hit.len() - 1].widget.id(), id)
                }
            }
        }
    }

    #[test]
    fn input_validates_before_accepting() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut req = request::prompt("n", "Count", "How many?", "abc");
        if let request::DialogRequest::Input {
            id,
            title,
            label,
            initial,
            ..
        } = req
        {
            req = request::DialogRequest::Input {
                id,
                title,
                label,
                initial,
                spec: request::InputSpec::Number(1.0, 10.0, 1.0),
                validate: None,
            };
        }
        let inst = Instance::new(&req, "t");
        let mut st = StateStore::new();
        let (bad, none, _) = update(inst, &UiMsg::Click("dlg:n:action:ok".to_string()), &mut st);
        assert_eq!(none, None);
        assert!(!bad.error.is_empty());
        let (fixed, _, _) = update(
            bad,
            &UiMsg::Text("dlg:n:field".to_string(), "7".to_string()),
            &mut st,
        );
        let (_, ok, _) = update(
            fixed,
            &UiMsg::Submit("dlg:n:field".to_string(), "7".to_string()),
            &mut st,
        );
        assert_eq!(ok, Some(crate::dialogs::Outcome::Text("7".to_string())))
    }

    #[test]
    fn choice_inputs_answer_the_highlight_or_the_checked_keys() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let options: Vec<(String, String)> = [("cut", "Cut"), ("copy", "Copy"), ("paste", "Paste")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut st = StateStore::new();
        let one = Instance::new(
            &request::choose("one", "Edit", "Command", &options, false),
            "t",
        );
        let (typed, _, _) = update(
            one.clone(),
            &UiMsg::Text("dlg:one:field".to_string(), "co".to_string()),
            &mut st,
        );
        let (_, picked, _) = update(
            typed,
            &UiMsg::Click("dlg:one:action:ok".to_string()),
            &mut st,
        );
        assert_eq!(
            picked,
            Some(crate::dialogs::Outcome::Action("copy".to_string()))
        );
        let (nothing, _, _) = update(
            one,
            &UiMsg::Text("dlg:one:field".to_string(), "zzz".to_string()),
            &mut st,
        );
        let (unmatched, none, _) = update(
            nothing,
            &UiMsg::Click("dlg:one:action:ok".to_string()),
            &mut st,
        );
        assert_eq!(none, None);
        assert!(!unmatched.error.is_empty());
        let many = Instance::new(
            &request::choose("many", "Edit", "Commands", &options, true),
            "t",
        );
        let (a, _, _) = update(
            many,
            &UiMsg::Toggle("dlg:many:pick:paste".to_string(), true),
            &mut st,
        );
        let (b, _, _) = update(
            a,
            &UiMsg::Toggle("dlg:many:pick:cut".to_string(), true),
            &mut st,
        );
        let (_, both, _) = update(
            b.clone(),
            &UiMsg::Click("dlg:many:action:ok".to_string()),
            &mut st,
        );
        assert_eq!(
            both,
            Some(crate::dialogs::Outcome::Keys(vec![
                "cut".to_string(),
                "paste".to_string()
            ]))
        );
        let (c, _, _) = update(
            b,
            &UiMsg::Toggle("dlg:many:pick:cut".to_string(), false),
            &mut st,
        );
        let (_, one_left, _) = update(
            c,
            &UiMsg::Submit("dlg:many:field".to_string(), "".to_string()),
            &mut st,
        );
        assert_eq!(
            one_left,
            Some(crate::dialogs::Outcome::Keys(vec!["paste".to_string()]))
        )
    }

    #[test]
    fn single_ok_messages_answer_accepted() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let inst = Instance::new(&request::info("i", "Done", "Saved."), "t");
        let (_, o, _) = finish_action(inst, "ok", &[]);
        assert_eq!(o, Some(crate::dialogs::Outcome::Accepted))
    }

    #[test]
    fn destructive_buttons_lead_and_accept_trails() {
        let order = ordered(&[
            DialogAction::new("save", "Save", crate::dialogs::request::ButtonRole::Accept),
            DialogAction::new(
                "discard",
                "Discard",
                crate::dialogs::request::ButtonRole::Destructive,
            ),
            DialogAction::new(
                "cancel",
                "Cancel",
                crate::dialogs::request::ButtonRole::Reject,
            ),
        ]);
        assert_eq!(order[0].id, "discard");
        assert_eq!(order[2].id, "save")
    }
}
