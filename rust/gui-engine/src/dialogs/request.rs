// request: typed dialog requests. Each names the dialog with an id that
// comes back in exactly one `UiMsg::Dialog(id, outcome)`.

use crate::editing::Validity;
use crate::strext::StrExt;
use crate::style::color::Color;
use std::collections::HashMap;

/// How serious a message is; shown with an icon and in text, never by
/// color alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Severity {
    Info,
    Warning,
    Error,
    Confirmation,
}

/// What a toolkit dialog blocks while it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Modality {
    /// The window it opens in.
    Window,
    /// Every window of the application.
    Application,
    /// Nothing: the window stays usable, and F6 moves focus between the
    /// window and its modeless dialogs.
    Modeless,
}

/// The role of a dialog button: it decides placement, Enter/Escape
/// behavior, and styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ButtonRole {
    /// Accepts (OK, Save, Open); Enter activates the default one.
    Accept,
    /// Rejects without discarding (Cancel); Escape and the close box use it.
    Reject,
    /// Destructive (Discard, Delete); never the default, never Escape.
    Destructive,
    /// Applies without closing.
    Apply,
    Help,
    /// Anything else (Retry, Copy details).
    Other,
}

/// One dialog button: a typed action id, its label, and its role.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DialogAction {
    pub id: String,
    pub label: String,
    pub role: ButtonRole,
}

impl DialogAction {
    pub fn new(id: &str, label: &str, role: ButtonRole) -> DialogAction {
        DialogAction {
            id: id.to_string(),
            label: crate::localization::tr(label),
            role,
        }
    }
}

/// What an input dialog collects.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum InputSpec {
    Text,
    Multiline,
    Password,
    /// A number within bounds; `step` for the spin arrows.
    Number(f64, f64, f64),
    /// One (or several, when `multiple`) of the (key, label) options.
    Choice(Vec<(String, String)>, bool),
}

impl PartialEq for InputSpec {
    fn eq(&self, other: &InputSpec) -> bool {
        match (self, other) {
            (InputSpec::Text, InputSpec::Text)
            | (InputSpec::Multiline, InputSpec::Multiline)
            | (InputSpec::Password, InputSpec::Password) => true,
            (InputSpec::Number(a, b, c), InputSpec::Number(d, e, f)) => a == d && b == e && c == f,
            (InputSpec::Choice(a, m), InputSpec::Choice(b, n)) => a == b && m == n,
            _ => false,
        }
    }
}

/// Which file dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FileMode {
    Open,
    OpenMany,
    Save,
    Folder,
    FolderMany,
}

/// Which implementation shows a file dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FileBackend {
    /// Native when it can meet every requirement, else the toolkit dialog.
    Auto,
    /// The platform dialog; unmet requirements answer `Unsupported`.
    Native,
    /// The toolkit's own dialog with favorites and recent locations.
    Toolkit,
}

/// A named set of file patterns (`*.txt`, `*.gos`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileFilter {
    pub name: String,
    pub patterns: Vec<String>,
}

/// What a date/time picker edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DateMode {
    Date,
    Time,
    DateTime,
}

/// One page of a preferences dialog: fields of (key, label, kind, value).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PreferencePage {
    pub key: String,
    pub title: String,
    pub fields: Vec<PreferenceField>,
}

/// One preference.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PreferenceField {
    pub key: String,
    pub label: String,
    pub kind: PreferenceKind,
    pub value: String,
    pub default: String,
    pub help: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PreferenceKind {
    Toggle,
    Text,
    Number(f64, f64),
    /// A whole number from the first bound to the second.
    Integer(i64, i64),
    Choice(Vec<(String, String)>),
    Color,
}

/// One page of a wizard; `skip_if` names a field whose "true" value skips
/// the page (conditional steps).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WizardPage {
    pub key: String,
    pub title: String,
    pub description: String,
    pub fields: Vec<PreferenceField>,
    pub skip_if: String,
}

/// A dialog to show.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DialogRequest {
    /// An information, warning, error, or question message with typed
    /// actions. `detail` is expandable, selectable, and copyable.
    Message {
        id: String,
        severity: Severity,
        title: String,
        message: String,
        detail: String,
        actions: Vec<DialogAction>,
    },
    Input {
        id: String,
        title: String,
        label: String,
        initial: String,
        spec: InputSpec,
        validate: Option<crate::Callback<dyn Fn(String) -> Validity>>,
    },
    Color {
        id: String,
        title: String,
        initial: Color,
        alpha: bool,
    },
    Font {
        id: String,
        title: String,
        initial: String,
    },
    Date {
        id: String,
        title: String,
        initial: String,
        mode: DateMode,
        min: String,
        max: String,
    },
    Shortcut {
        id: String,
        title: String,
        initial: String,
        reserved: Vec<String>,
    },
    /// Progress with optional cancellation; update it with a new request
    /// of the same id, close it with `close_dialog`.
    Progress {
        id: String,
        title: String,
        message: String,
        value: f64,
        cancellable: bool,
    },
    File {
        id: String,
        mode: FileMode,
        title: String,
        directory: String,
        name: String,
        filters: Vec<FileFilter>,
        backend: FileBackend,
        /// Keys the remembered directory, filter, and recent locations.
        purpose: String,
        /// Needs the application-managed favorites sidebar.
        needs_favorites: bool,
    },
    /// `checked` dialogs ask the application before Apply, OK, Next or
    /// Finish takes effect (see `checked`).
    Preferences {
        id: String,
        title: String,
        pages: Vec<PreferencePage>,
        checked: bool,
    },
    Wizard {
        id: String,
        title: String,
        pages: Vec<WizardPage>,
        checked: bool,
    },
    /// A few fields over Cancel and an accept button labelled `accept`,
    /// answered as `Outcome::Values` keyed by field.
    Form {
        id: String,
        title: String,
        fields: Vec<PreferenceField>,
        accept: String,
    },
    About {
        id: String,
        name: String,
        version: String,
        description: String,
        credits: String,
        licenses: String,
        diagnostics: String,
        links: Vec<(String, String)>,
    },
}

impl DialogRequest {
    /// The request's id.
    pub fn id(&self) -> String {
        match self {
            DialogRequest::Message { id, .. }
            | DialogRequest::Input { id, .. }
            | DialogRequest::Color { id, .. }
            | DialogRequest::Font { id, .. }
            | DialogRequest::Date { id, .. }
            | DialogRequest::Shortcut { id, .. }
            | DialogRequest::Progress { id, .. }
            | DialogRequest::File { id, .. }
            | DialogRequest::Preferences { id, .. }
            | DialogRequest::Wizard { id, .. }
            | DialogRequest::Form { id, .. }
            | DialogRequest::About { id, .. } => id.clone(),
        }
    }

    /// The title the dialog's frame shows.
    pub fn title(&self) -> String {
        match self {
            DialogRequest::Message { title, .. }
            | DialogRequest::Input { title, .. }
            | DialogRequest::Color { title, .. }
            | DialogRequest::Font { title, .. }
            | DialogRequest::Date { title, .. }
            | DialogRequest::Shortcut { title, .. }
            | DialogRequest::Progress { title, .. }
            | DialogRequest::File { title, .. }
            | DialogRequest::Preferences { title, .. }
            | DialogRequest::Wizard { title, .. }
            | DialogRequest::Form { title, .. } => title.clone(),
            DialogRequest::About { name, .. } => crate::localization::message(
                "about",
                "About {name}",
                1,
                &crate::hmap! { "name" => name.to_string() },
            ),
        }
    }
}

// ---- convenience constructors ----

fn message(
    id: &str,
    severity: Severity,
    title: &str,
    text: &str,
    actions: &[DialogAction],
) -> DialogRequest {
    DialogRequest::Message {
        id: id.to_string(),
        severity,
        title: title.to_string(),
        message: text.to_string(),
        detail: String::new(),
        actions: actions.to_vec(),
    }
}

/// An acknowledgement: a message and OK.
pub fn info(id: &str, title: &str, text: &str) -> DialogRequest {
    message(
        id,
        Severity::Info,
        title,
        text,
        &[DialogAction::new("ok", "OK", ButtonRole::Accept)],
    )
}

/// A consequence the user should weigh, with Continue and Cancel.
pub fn warning(id: &str, title: &str, text: &str) -> DialogRequest {
    message(
        id,
        Severity::Warning,
        title,
        text,
        &[
            DialogAction::new("continue", "Continue", ButtonRole::Accept),
            DialogAction::new("cancel", "Cancel", ButtonRole::Reject),
        ],
    )
}

/// A failure with the actions the caller supports (default: Close).
pub fn error(id: &str, title: &str, text: &str, detail: &str, retry: bool) -> DialogRequest {
    let mut actions = vec![];
    if retry {
        actions.push(DialogAction::new("retry", "Retry", ButtonRole::Accept))
    };
    actions.push(DialogAction::new(
        "close",
        "Close",
        if retry {
            ButtonRole::Reject
        } else {
            ButtonRole::Accept
        },
    ));
    DialogRequest::Message {
        id: id.to_string(),
        severity: Severity::Error,
        title: title.to_string(),
        message: text.to_string(),
        detail: detail.to_string(),
        actions,
    }
}

/// A yes/no style confirmation with caller labels; the safe choice is
/// the default and Escape answers `Cancelled`.
pub fn confirm(
    id: &str,
    title: &str,
    text: &str,
    accept: &str,
    destructive: bool,
) -> DialogRequest {
    message(
        id,
        Severity::Confirmation,
        title,
        text,
        &[
            DialogAction::new(
                "accept",
                accept,
                if destructive {
                    ButtonRole::Destructive
                } else {
                    ButtonRole::Accept
                },
            ),
            DialogAction::new("cancel", "Cancel", ButtonRole::Reject),
        ],
    )
}

/// Replace an existing file?
pub fn confirm_overwrite(id: &str, name: &str) -> DialogRequest {
    message(
        id,
        Severity::Warning,
        &crate::localization::tr("Replace file?"),
        &crate::localization::message(
            "files",
            "\u{201C}{name}\u{201D} already exists. Replacing it overwrites its contents.",
            1,
            &crate::hmap! { "name" => name.to_string() },
        ),
        &[
            DialogAction::new("replace", "Replace", ButtonRole::Destructive),
            DialogAction::new("cancel", "Cancel", ButtonRole::Reject),
        ],
    )
}

/// Delete permanently?
pub fn confirm_delete(id: &str, name: &str) -> DialogRequest {
    message(
        id,
        Severity::Warning,
        &crate::localization::tr("Delete?"),
        &crate::localization::message(
            "files",
            "\u{201C}{name}\u{201D} will be deleted. This cannot be undone.",
            1,
            &crate::hmap! { "name" => name.to_string() },
        ),
        &[
            DialogAction::new("delete", "Delete", ButtonRole::Destructive),
            DialogAction::new("cancel", "Cancel", ButtonRole::Reject),
        ],
    )
}

/// Save changes before closing? Save, Discard, or Cancel; closing the
/// prompt never means Discard.
pub fn confirm_unsaved(id: &str, name: &str) -> DialogRequest {
    message(
        id,
        Severity::Warning,
        &crate::localization::tr("Save changes?"),
        &crate::localization::message(
            "files",
            "\u{201C}{name}\u{201D} has changes that are not saved.",
            1,
            &crate::hmap! { "name" => name.to_string() },
        ),
        &[
            DialogAction::new("save", "Save", ButtonRole::Accept),
            DialogAction::new("discard", "Don\u{2019}t Save", ButtonRole::Destructive),
            DialogAction::new("cancel", "Cancel", ButtonRole::Reject),
        ],
    )
}

/// A one-line text prompt.
pub fn prompt(id: &str, title: &str, label: &str, initial: &str) -> DialogRequest {
    DialogRequest::Input {
        id: id.to_string(),
        title: title.to_string(),
        label: label.to_string(),
        initial: initial.to_string(),
        spec: InputSpec::Text,
        validate: None,
    }
}

/// A filtered list of (key, label) options. A single choice answers
/// `Outcome::Action(key)`; with `multiple`, rows are checked and OK answers
/// `Outcome::Keys` in the options' order.
pub fn choose(
    id: &str,
    title: &str,
    label: &str,
    options: &[(String, String)],
    multiple: bool,
) -> DialogRequest {
    DialogRequest::Input {
        id: id.to_string(),
        title: title.to_string(),
        label: label.to_string(),
        initial: "".to_string(),
        spec: InputSpec::Choice(options.to_vec(), multiple),
        validate: None,
    }
}

/// Asks whether `origin` may use `what` (notifications, location, ...).
/// Answers `Outcome::Action("allow")` or `Action("deny")`; Escape denies.
pub fn permission(id: &str, origin: &str, what: &str) -> DialogRequest {
    let tr = crate::localization::tr;
    message(
        id,
        Severity::Confirmation,
        &tr("Permission requested"),
        &crate::localization::message(
            "permission",
            "{origin} asks to use {what}.",
            1,
            &crate::hmap! { "origin" => origin.to_string(), "what" => what.to_string() },
        ),
        &[
            DialogAction::new("deny", "Don't Allow", ButtonRole::Reject),
            DialogAction::new("allow", "Allow", ButtonRole::Accept),
        ],
    )
}

/// A go-to dialog for a document of `lines` lines. It answers
/// `Outcome::Text` holding "line" or "line:column", both counted from 1;
/// `parse_location` reads it back.
pub fn go_to_line(id: &str, lines: i64, initial: &str) -> DialogRequest {
    let label = crate::localization::message(
        "goto",
        "Line (1 to {count}), or line:column",
        lines,
        &HashMap::new(),
    );
    DialogRequest::Input {
        id: id.to_string(),
        title: crate::localization::tr("Go to Line"),
        label,
        initial: initial.to_string(),
        spec: InputSpec::Text,
        validate: Some(crate::Callback::new(std::rc::Rc::new(move |text| {
            location_validity(&text, lines)
        }))),
    }
}

/// The 1-based line and column of a "line" or "line:column" answer; the
/// column is 1 when absent. None when either part is not a positive integer.
pub fn parse_location(text: &str) -> Option<(i64, i64)> {
    let parts = text.trim().split(":").collect::<Vec<_>>();
    if parts.len() > 2 {
        return None;
    }
    let line = parts[0].trim().to_i64()?;
    let column = if parts.len() == 2 {
        parts[1].trim().to_i64()?
    } else {
        1
    };
    if line < 1 || column < 1 {
        None
    } else {
        Some((line, column))
    }
}

fn location_validity(text: &str, lines: i64) -> Validity {
    if text.trim().is_empty() {
        return Validity::Intermediate(crate::localization::tr("Enter a line number"));
    }
    match parse_location(text) {
        Some((line, _)) => {
            if line <= lines {
                Validity::Valid
            } else {
                Validity::Invalid(crate::localization::message(
                    "goto",
                    "The document has {count} lines",
                    lines,
                    &HashMap::new(),
                ))
            }
        }
        None => Validity::Invalid(crate::localization::tr(
            "Enter a line number, or line:column",
        )),
    }
}

pub fn color(id: &str, title: &str, initial: Color) -> DialogRequest {
    DialogRequest::Color {
        id: id.to_string(),
        title: title.to_string(),
        initial,
        alpha: true,
    }
}

/// Open one file.
pub fn open_file(id: &str, title: &str, filters: &[FileFilter]) -> DialogRequest {
    DialogRequest::File {
        id: id.to_string(),
        mode: FileMode::Open,
        title: title.to_string(),
        directory: String::new(),
        name: String::new(),
        filters: filters.to_vec(),
        backend: FileBackend::Auto,
        purpose: String::new(),
        needs_favorites: false,
    }
}

/// Choose where to save.
pub fn save_file(id: &str, title: &str, name: &str, filters: &[FileFilter]) -> DialogRequest {
    DialogRequest::File {
        id: id.to_string(),
        mode: FileMode::Save,
        title: title.to_string(),
        directory: String::new(),
        name: name.to_string(),
        filters: filters.to_vec(),
        backend: FileBackend::Auto,
        purpose: String::new(),
        needs_favorites: false,
    }
}

/// Choose a folder.
pub fn select_folder(id: &str, title: &str) -> DialogRequest {
    DialogRequest::File {
        id: id.to_string(),
        mode: FileMode::Folder,
        title: title.to_string(),
        directory: String::new(),
        name: String::new(),
        filters: vec![],
        backend: FileBackend::Auto,
        purpose: String::new(),
        needs_favorites: false,
    }
}

pub fn about(id: &str, name: &str, version: &str, description: &str) -> DialogRequest {
    DialogRequest::About {
        id: id.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        description: description.to_string(),
        credits: String::new(),
        licenses: String::new(),
        diagnostics: String::new(),
        links: vec![],
    }
}

impl std::fmt::Display for DialogRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DialogRequest({})", self.id())
    }
}

impl std::fmt::Debug for DialogRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DialogRequest {{ id: {:?}, title: {:?} }}",
            self.id(),
            self.title()
        )
    }
}

/// Uses the toolkit file browser, including its persistent locations sidebar.
pub fn toolkit_file(req: DialogRequest) -> DialogRequest {
    match req {
        DialogRequest::File {
            id,
            mode,
            title,
            directory,
            name,
            filters,
            purpose,
            ..
        } => DialogRequest::File {
            id: id.to_string(),
            mode,
            title: title.to_string(),
            directory: directory.to_string(),
            name: name.to_string(),
            filters: filters.to_vec(),
            backend: FileBackend::Toolkit,
            purpose: purpose.to_string(),
            needs_favorites: true,
        },
        _ => req,
    }
}

/// A family/style/size picker with reversible live preview.
pub fn font(id: &str, title: &str, initial: &str) -> DialogRequest {
    DialogRequest::Font {
        id: id.to_string(),
        title: title.to_string(),
        initial: initial.to_string(),
    }
}
pub fn date(
    id: &str,
    title: &str,
    initial: &str,
    mode: DateMode,
    min: &str,
    max: &str,
) -> DialogRequest {
    DialogRequest::Date {
        id: id.to_string(),
        title: title.to_string(),
        initial: initial.to_string(),
        mode,
        min: min.to_string(),
        max: max.to_string(),
    }
}
pub fn preferences(id: &str, title: &str, pages: &[PreferencePage]) -> DialogRequest {
    DialogRequest::Preferences {
        id: id.to_string(),
        title: title.to_string(),
        pages: pages.to_vec(),
        checked: false,
    }
}
/// A form of `fields` whose accept button reads `accept`.
pub fn form(id: &str, title: &str, fields: &[PreferenceField], accept: &str) -> DialogRequest {
    DialogRequest::Form {
        id: id.to_string(),
        title: title.to_string(),
        fields: fields.to_vec(),
        accept: accept.to_string(),
    }
}
pub fn wizard(id: &str, title: &str, pages: &[WizardPage]) -> DialogRequest {
    DialogRequest::Wizard {
        id: id.to_string(),
        title: title.to_string(),
        pages: pages.to_vec(),
        checked: false,
    }
}

/// `req` (preferences or a wizard) asking the application before a step
/// takes effect: Apply, OK, Next or Finish sends `UiMsg::DialogCheck(id,
/// request, step, values)` and waits, showing it is working, until
/// `Effect::DialogChecked(id, request, error)` answers. "" lets the step
/// proceed; an error is shown and the drafts stay for another try. Stop (or
/// Cancel) abandons the wait, reported as `UiMsg::DialogCheckStopped`.
pub fn checked(req: DialogRequest) -> DialogRequest {
    match req {
        DialogRequest::Preferences {
            id, title, pages, ..
        } => DialogRequest::Preferences {
            id: id.to_string(),
            title: title.to_string(),
            pages: pages.to_vec(),
            checked: true,
        },
        DialogRequest::Wizard {
            id, title, pages, ..
        } => DialogRequest::Wizard {
            id: id.to_string(),
            title: title.to_string(),
            pages: pages.to_vec(),
            checked: true,
        },
        _ => req,
    }
}

#[cfg(test)]
mod tests {
    use super::{location_validity, parse_location};
    use crate::editing::Validity;

    #[test]
    fn locations_read_a_line_and_an_optional_column() {
        assert_eq!(parse_location("12"), Some((12, 1)));
        assert_eq!(parse_location(" 3 : 7 "), Some((3, 7)));
        assert_eq!(parse_location("0"), None);
        assert_eq!(parse_location("2:0"), None);
        assert_eq!(parse_location("2.5"), None);
        assert_eq!(parse_location("1:2:3"), None);
        assert_eq!(parse_location("x"), None)
    }

    #[test]
    fn locations_past_the_last_line_are_invalid() {
        assert_eq!(location_validity("4", 4), Validity::Valid);
        assert_eq!(
            location_validity("5", 4),
            Validity::Invalid("The document has 4 lines".to_string())
        );
        assert_eq!(
            location_validity("", 4),
            Validity::Intermediate("Enter a line number".to_string())
        )
    }
}
