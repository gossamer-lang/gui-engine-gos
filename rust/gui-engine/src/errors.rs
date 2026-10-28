use std::fmt::Display;

/// A toolkit failure, its outermost context first.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

impl Error {
    /// The whole chain as one line.
    pub fn message(&self) -> &str {
        &self.0
    }
}

/// A failure described by `message`.
pub fn new(message: impl Into<String>) -> Error {
    Error(message.into())
}

/// `cause` seen from `context`, read as "context: cause".
pub fn wrap(cause: impl Display, context: impl Display) -> Error {
    Error(format!("{context}: {cause}"))
}

impl From<windowing::Error> for Error {
    fn from(e: windowing::Error) -> Error {
        Error(e.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Error {
        Error(e.to_string())
    }
}
