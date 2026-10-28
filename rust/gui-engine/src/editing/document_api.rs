//! Bounded JSON adapter for native document handles. Full text is copied only
//! when a caller explicitly asks to read a range or creates an initial document.
use super::documents;
use serde::de::DeserializeOwned;
#[derive(serde::Serialize)]
enum Answer {
    Id(i64),
    Info(documents::Info),
    Text(String),
    Position(usize, usize),
    Anchor(documents::Anchor),
    Done,
    Flag(bool),
}
fn args<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|error| error.to_string())
}
fn out(value: impl serde::Serialize) -> Result<String, String> {
    serde_json::to_string(&value).map_err(|error| error.to_string())
}
pub fn call(method: &str, id: i64, text: &str) -> Result<String, String> {
    match method {
        "new" => {
            let text: String = args(text)?;
            out(Answer::Id(documents::create(
                &text,
                crate::app::effects::current_budget(),
            )?))
        }
        "layout_ready" => {
            let state = documents::state(id)?;
            out(Answer::Flag(crate::text::document_layout::ready(
                state.layout_key,
                state.text.identity(),
            )))
        }
        "info" => out(Answer::Info(documents::info(id)?)),
        "read" => {
            let (start, end): (usize, usize) = args(text)?;
            out(Answer::Text(documents::read(id, start, end)?))
        }
        "freeze" => out(Answer::Id(documents::freeze(id)?)),
        "close" => {
            documents::close(id);
            out(Answer::Done)
        }
        "replace" => {
            let (revision, start, end, text): (i64, usize, usize, String) = args(text)?;
            out(Answer::Info(documents::replace(
                id, revision, start, end, &text,
            )?))
        }
        "saved" => {
            let snapshot: i64 = args(text)?;
            documents::mark_saved(id, documents::snapshot(snapshot)?.0)?;
            out(Answer::Done)
        }
        "position" => {
            let byte: usize = args(text)?;
            let (text, _) = documents::snapshot(id)?;
            let line = text.line_of(byte);
            let column = text.scalar_of(byte) - text.scalar_of(text.line_start(line));
            out(Answer::Position(line + 1, column + 1))
        }
        "line" => {
            let line: usize = args(text)?;
            let (text, _) = documents::snapshot(id)?;
            out(Answer::Position(
                text.line_start(line),
                text.line_start(line + 1),
            ))
        }
        "anchor" => {
            let (byte, affinity): (usize, documents::Affinity) = args(text)?;
            out(Answer::Id(documents::anchor(id, byte, affinity)?))
        }
        "resolve" => {
            let anchor: i64 = args(text)?;
            out(Answer::Anchor(documents::resolve(id, anchor)?))
        }
        "release_anchor" => {
            let anchor: i64 = args(text)?;
            documents::release_anchor(id, anchor);
            out(Answer::Done)
        }
        _ => Err("unknown document operation".into()),
    }
}
