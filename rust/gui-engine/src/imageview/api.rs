//! The image viewer operations Gossamer reaches through one JSON entry point.

use serde::Serialize;

use super::{Status, Zoom};

#[derive(Serialize)]
enum Answer {
    Id(i64),
    Status(Status),
    Reloaded(bool),
    Done,
}

fn args<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// Runs viewer operation `method` on viewer `id` with JSON `text` arguments,
/// answering JSON.
pub fn call(method: &str, id: i64, text: &str) -> Result<String, String> {
    if text.len() > 64 * 1024 {
        return Err("image viewer request exceeds its byte budget".into());
    }
    let answer = match method {
        "open" => Answer::Id(super::open(crate::app::effects::current_budget())?),
        "load" => {
            super::load(id, &args::<String>(text)?)?;
            Answer::Done
        }
        "reload" => Answer::Reloaded(super::reload(id)?),
        "cancel" => {
            super::cancel(id)?;
            Answer::Done
        }
        "zoom" => {
            super::set_zoom(id, args::<Zoom>(text)?)?;
            Answer::Done
        }
        "turn" => {
            super::turn(id, args(text)?)?;
            Answer::Done
        }
        "status" => Answer::Status(super::status_of(id)?),
        "close" => {
            super::close(id);
            Answer::Done
        }
        _ => return Err(format!("unknown image viewer operation {method}")),
    };
    serde_json::to_string(&answer).map_err(|e| e.to_string())
}
