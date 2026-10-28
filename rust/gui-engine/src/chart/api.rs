//! The chart operations Gossamer reaches through one JSON entry point.

use serde::Serialize;

use super::{SeriesStyle, Spec, Window};

#[derive(Serialize)]
enum Answer {
    Id(i64),
    Window(Window),
    Rows(Vec<(String, String, String)>),
    Text(String),
    Item(Box<crate::print::Item>),
    Done,
}

fn args<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// Runs chart operation `method` on chart `id` with JSON `text` arguments,
/// answering JSON.
pub fn call(method: &str, id: i64, text: &str) -> Result<String, String> {
    if text.len() > 256 * 1024 * 1024 {
        return Err("chart request exceeds its byte budget".into());
    }
    let theme = crate::style::Theme::light();
    let answer = match method {
        "open" => Answer::Id(super::create(
            args::<Spec>(text)?,
            crate::app::effects::current_budget(),
        )?),
        "add_series" => {
            let (style, points): (SeriesStyle, Vec<(f64, f64)>) = args(text)?;
            Answer::Id(super::add_series(id, style, &points)?)
        }
        "set_points" => {
            let (series, points): (i64, Vec<(f64, f64)>) = args(text)?;
            super::set_points(id, series, &points)?;
            Answer::Done
        }
        "append" => {
            let (series, points): (i64, Vec<(f64, f64)>) = args(text)?;
            super::append(id, series, &points)?;
            Answer::Done
        }
        "set_hidden" => {
            let (series, hidden): (i64, bool) = args(text)?;
            super::set_hidden(id, series, hidden)?;
            Answer::Done
        }
        "zoom_to" => {
            super::set_window(id, Some(args::<Window>(text)?))?;
            Answer::Done
        }
        "fit" => {
            super::set_window(id, None)?;
            Answer::Done
        }
        "window" => Answer::Window(super::window(id)?),
        "table" => Answer::Rows(super::table(id, args(text)?)?),
        "summary" => Answer::Text(super::read(id, super::widget::summary)?),
        "export_png" => {
            let (path, width, height): (String, i64, i64) = args(text)?;
            let png = super::export_png(id, width, height, &theme)?;
            std::fs::write(&path, png).map_err(|e| format!("{path}: {e}"))?;
            Answer::Done
        }
        "drawing" => {
            let (width, height): (i64, i64) = args(text)?;
            if !(16..=8192).contains(&width) || !(16..=8192).contains(&height) {
                return Err("a chart drawing is 16 to 8192 units on a side".into());
            }
            let ops = super::read(id, |m| super::render::render(m, width, height, &theme))?;
            Answer::Item(Box::new(crate::print::Item::Drawing {
                ops,
                width: width as f64,
                height: height as f64,
            }))
        }
        "close" => {
            super::close(id);
            Answer::Done
        }
        _ => return Err(format!("unknown chart operation {method}")),
    };
    serde_json::to_string(&answer).map_err(|e| e.to_string())
}
