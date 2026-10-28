//! The printing operations Gossamer reaches through one JSON entry point.

use serde::Serialize;

use super::job::{self, Source, Target};
use super::native::{self, Destination};
use super::{Item, PageSetup, document};

#[derive(Serialize)]
enum Answer {
    Id(i64),
    Pages(i64),
    Text(String),
    Destinations(Vec<Destination>),
    Done,
}

fn args<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// Runs printing operation `method` with JSON `text` arguments, answering
/// JSON.
pub fn call(method: &str, text: &str) -> Result<String, String> {
    if text.len() > 512 * 1024 * 1024 {
        return Err("print request exceeds its byte budget".into());
    }
    let answer = match method {
        "open" => {
            let (items, setup): (Vec<Item>, PageSetup) = args(text)?;
            Answer::Id(document::open(
                items,
                setup,
                crate::app::effects::current_budget(),
            )?)
        }
        "setup" => {
            let (id, setup): (i64, PageSetup) = args(text)?;
            Answer::Pages(document::set_setup(id, setup)?)
        }
        "count" => Answer::Pages(document::pages(args(text)?)?.0.len() as i64),
        "text" => {
            let (id, page): (i64, i64) = args(text)?;
            Answer::Text(document::page_text(id, page)?)
        }
        "close" => {
            document::close(args(text)?);
            Answer::Done
        }
        "start" => {
            let (source, title, target): (Source, String, Target) = args(text)?;
            Answer::Id(job::start(source, title, target)?)
        }
        "cancel" => {
            job::cancel(args(text)?)?;
            Answer::Done
        }
        "withdraw" => {
            let (server, spooled): (String, String) = args(text)?;
            job::withdraw(&server, &spooled)?;
            Answer::Done
        }
        "destinations" => Answer::Destinations(native::destinations(&args::<String>(text)?)?),
        _ => return Err(format!("unknown print operation {method}")),
    };
    serde_json::to_string(&answer).map_err(|e| e.to_string())
}
