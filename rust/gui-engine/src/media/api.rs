//! The media operations Gossamer reaches through one JSON entry point.

use serde::Serialize;

use super::{AudioOutput, Status};

#[derive(Serialize)]
enum Answer {
    Id(i64),
    Status(Status),
    Done,
}

fn args<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// Runs media operation `method` on player `id` with JSON `text` arguments,
/// answering JSON.
pub fn call(method: &str, id: i64, text: &str) -> Result<String, String> {
    if text.len() > 64 * 1024 {
        return Err("media request exceeds its byte budget".into());
    }
    let answer = match method {
        "open" => Answer::Id(super::open(
            args::<AudioOutput>(text)?,
            crate::app::effects::current_budget(),
        )?),
        "load" => {
            super::load(id, &args::<String>(text)?)?;
            Answer::Done
        }
        "play" => {
            super::play(id)?;
            Answer::Done
        }
        "pause" => {
            super::pause(id)?;
            Answer::Done
        }
        "seek" => {
            super::seek(id, args(text)?)?;
            Answer::Done
        }
        "volume" => {
            super::set_volume(id, args(text)?)?;
            Answer::Done
        }
        "mute" => {
            super::set_muted(id, args(text)?)?;
            Answer::Done
        }
        "status" => Answer::Status(super::status(id)?),
        "close" => {
            super::close(id);
            Answer::Done
        }
        _ => return Err(format!("unknown media operation {method}")),
    };
    serde_json::to_string(&answer).map_err(|e| e.to_string())
}
