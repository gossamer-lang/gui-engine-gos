//! The dock operations Gossamer reaches through one JSON entry point. The
//! layout travels with each call, so the application's model owns it.

use std::collections::HashMap;

use serde::Serialize;

use super::{Change, Layout, Pane, Zone};
use crate::widgets::cx::UiMsg;

#[derive(Serialize)]
enum Answer {
    Layout(Layout),
    Changed(Layout, Vec<Change>),
    Restored(Layout, Vec<String>),
    Text(String),
    Group(Option<i64>),
    Pane(Option<String>),
    Panes(Vec<String>),
    View(Box<crate::widgets::Widget>),
}

fn args<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// Runs dock operation `method` with JSON `text` arguments, answering JSON.
pub fn call(method: &str, text: &str) -> Result<String, String> {
    if text.len() > 64 * 1024 * 1024 {
        return Err("dock request exceeds its byte budget".into());
    }
    let answer = match method {
        "new" => Answer::Layout(Layout::new(&args::<Vec<String>>(text)?)),
        "restore" => {
            let (source, known): (String, Vec<String>) = args(text)?;
            let (layout, repairs) = Layout::restore(&source, &known);
            Answer::Restored(layout, repairs)
        }
        "save" => Answer::Text(args::<Layout>(text)?.save()),
        "handle" => {
            let (mut layout, dock, msg): (Layout, String, UiMsg) = args(text)?;
            let changes = layout.handle(&dock, &msg);
            Answer::Changed(layout, changes)
        }
        "command" => {
            let (mut layout, dock, pane, command): (Layout, String, String, String) = args(text)?;
            let changes = layout.command(&dock, &pane, &command);
            Answer::Changed(layout, changes)
        }
        "place" => {
            let (mut layout, pane, group, zone): (Layout, String, i64, Zone) = args(text)?;
            let closed = layout.place(&pane, group, zone, None)?;
            Answer::Changed(layout, super::changed(closed))
        }
        "set_share" => {
            let (mut layout, pane, share): (Layout, String, f64) = args(text)?;
            layout.set_share(&pane, share)?;
            Answer::Layout(layout)
        }
        "place_window" => {
            let (mut layout, window, x, y, w, h): (Layout, String, i64, i64, i64, i64) =
                args(text)?;
            layout.place_window(&window, (x, y, w, h));
            Answer::Layout(layout)
        }
        "add" => {
            let (mut layout, pane): (Layout, String) = args(text)?;
            layout.add(&pane)?;
            Answer::Layout(layout)
        }
        "panes" => Answer::Panes(args::<Layout>(text)?.panes()),
        "group_of" => {
            let (layout, pane): (Layout, String) = args(text)?;
            Answer::Group(layout.group_of(&pane))
        }
        "next_group" => {
            let (layout, pane, backward): (Layout, String, bool) = args(text)?;
            Answer::Pane(layout.next_group(&pane, backward))
        }
        "view" => {
            let (layout, dock, window, panes): (Layout, String, String, Vec<(String, Pane)>) =
                args(text)?;
            let panes: HashMap<String, Pane> = panes.into_iter().collect();
            Answer::View(Box::new(super::view(&layout, &dock, &window, &panes)))
        }
        _ => return Err(format!("unknown dock operation {method}")),
    };
    serde_json::to_string(&answer).map_err(|e| e.to_string())
}
