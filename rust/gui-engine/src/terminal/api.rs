//! The terminal operations Gossamer reaches through one JSON entry point.

use serde::Serialize;

use super::{Cell, Command, Options, Snapshot, Span};

#[derive(Serialize)]
enum Answer {
    Id(i64),
    Rows(i64),
    Text(String),
    Snapshot(Snapshot),
    Lines(Vec<super::engine::Row>),
    Zones(Vec<super::engine::Zone>),
    Done,
}

fn args<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// Runs terminal operation `method` on terminal `id` with JSON `text`
/// arguments, answering JSON.
pub fn call(method: &str, id: i64, text: &str) -> Result<String, String> {
    if text.len() > 2 * 1024 * 1024 {
        return Err("terminal request exceeds its byte budget".into());
    }
    let answer = match method {
        "open" => {
            let options: Options = args(text)?;
            Answer::Id(super::create(
                &options,
                crate::app::effects::current_budget(),
            )?)
        }
        "spawn" => {
            let command: Command = args(text)?;
            super::spawn(id, &command)?;
            Answer::Done
        }
        "feed" => {
            let bytes: String = args(text)?;
            super::feed(id, bytes.as_bytes())?;
            Answer::Done
        }
        "input" => {
            let bytes: String = args(text)?;
            super::input(id, bytes.as_bytes())?;
            Answer::Done
        }
        "paste" => {
            let pasted: String = args(text)?;
            super::paste(id, &pasted)?;
            Answer::Done
        }
        "resize" => {
            let (rows, cols): (i64, i64) = args(text)?;
            super::resize(id, rows, cols)?;
            Answer::Done
        }
        "scroll" => {
            let rows: i64 = args(text)?;
            Answer::Rows(super::scroll(id, rows)?)
        }
        "select" => {
            let range: (Cell, Cell) = args(text)?;
            super::select(id, Some(range))?;
            Answer::Done
        }
        "deselect" => {
            super::select(id, None)?;
            Answer::Done
        }
        "signal" => {
            let signum: i64 = args(text)?;
            super::signal(id, signum)?;
            Answer::Done
        }
        "encoding" => {
            let label: String = args(text)?;
            super::set_encoding(id, &label)?;
            Answer::Done
        }
        "rows" => Answer::Lines(super::rows(id)?),
        "cwd" => Answer::Text(super::cwd(id)?),
        "zones" => Answer::Zones(super::zones(id)?),
        "highlight" => {
            let spans: Vec<Span> = args(text)?;
            super::highlight(id, spans)?;
            Answer::Done
        }
        "copy" => Answer::Text(super::copy(id)?),
        "snapshot" => Answer::Snapshot(super::snapshot(id)?),
        "close" => {
            super::close(id);
            Answer::Done
        }
        _ => return Err(format!("unknown terminal operation {method}")),
    };
    serde_json::to_string(&answer).map_err(|e| e.to_string())
}
