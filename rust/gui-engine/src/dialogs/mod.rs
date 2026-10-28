pub mod files;
pub mod host;
pub mod outcome;
pub mod pickers;
pub mod request;

// dialogs: the dialog catalog and its shared contract (typed requests,
// exactly one asynchronous outcome, modal focus scopes, button roles).

pub type Outcome = crate::dialogs::outcome::Outcome;
