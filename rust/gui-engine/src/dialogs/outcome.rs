// outcome: how a dialog ended. Every dialog request answers exactly one
// outcome, asynchronously, through `UiMsg::Dialog(id, outcome)`.

use crate::style::color::Color;
use std::collections::HashMap;

/// A dialog's single terminal answer.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Outcome {
    /// The affirmative button of an acknowledgement or confirmation.
    Accepted,
    /// A typed action from a dialog with several choices (Save, Discard,
    /// Retry, a wizard's Finish).
    Action(String),
    Text(String),
    /// Named field values from a form or preferences dialog.
    Values(HashMap<String, String>),
    Paths(Vec<String>),
    /// The keys a multiple-choice input checked, in the options' order.
    Keys(Vec<String>),
    Color(Color),
    /// A font as `family|style|size`.
    Font(String),
    /// An ISO-8601 date, time, or date-time.
    Date(String),
    /// A shortcut in `Shortcut::parse` text form ("" when cleared).
    Shortcut(String),
    /// The user dismissed the dialog (Cancel, Escape, the close button).
    Cancelled,
    /// The owner window or widget went away before an answer.
    Disposed,
    /// The requested backend or feature is not available here.
    Unsupported(String),
    /// The backend failed; the text says why.
    Failed(String),
}
