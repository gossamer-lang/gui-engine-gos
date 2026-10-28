//! The scene operations Gossamer reaches through one JSON entry point.

use serde::Serialize;

use super::{Camera, Info, Node};

#[derive(Serialize)]
enum Answer {
    Id(i64),
    Info(Info),
    Nodes(Vec<Node>),
    Hit(Option<String>),
    Item(Box<crate::print::Item>),
    Done,
}

fn args<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// The scene drawn at `width` x `height` without selection marks.
fn picture(
    id: i64,
    width: i64,
    height: i64,
) -> Result<Vec<crate::render::display::DrawOp>, String> {
    if !(16..=8192).contains(&width) || !(16..=8192).contains(&height) {
        return Err("a scene picture is 16 to 8192 units on a side".into());
    }
    let theme = crate::style::Theme::light();
    super::read(id, |scene| {
        let mut ops = vec![crate::render::display::DrawOp::Clip(
            crate::geometry::Rect::new(0, 0, width, height),
        )];
        ops.extend(super::render::render(scene, &theme, false));
        ops.push(crate::render::display::DrawOp::Unclip);
        ops
    })
}

/// Runs scene operation `method` on scene `id` with JSON `text` arguments,
/// answering JSON.
pub fn call(method: &str, id: i64, text: &str) -> Result<String, String> {
    if text.len() > 256 * 1024 * 1024 {
        return Err("scene request exceeds its byte budget".into());
    }
    let answer = match method {
        "open" => Answer::Id(super::create(
            args(text)?,
            crate::app::effects::current_budget(),
        )?),
        "add" => {
            let (parent, index, node): (String, i64, Node) = args(text)?;
            Answer::Info(super::add(id, &parent, usize::try_from(index).ok(), node)?)
        }
        "replace" => Answer::Info(super::replace(id, args(text)?)?),
        "remove" => Answer::Info(super::remove(id, &args::<Vec<String>>(text)?)?),
        "restack" => {
            let (object, index): (String, i64) = args(text)?;
            Answer::Info(super::restack(id, &object, index.max(0) as usize)?)
        }
        "translate" => {
            let (objects, dx, dy): (Vec<String>, f64, f64) = args(text)?;
            Answer::Info(super::translate(id, &objects, dx, dy)?)
        }
        "resize" => {
            let (object, width, height): (String, f64, f64) = args(text)?;
            Answer::Info(super::resize(id, &object, width, height)?)
        }
        "undo" => Answer::Info(super::undo(id)?),
        "redo" => Answer::Info(super::redo(id)?),
        "select" => Answer::Info(super::select(id, &args::<Vec<String>>(text)?)?),
        "camera" => {
            super::set_camera(id, args::<Camera>(text)?)?;
            Answer::Done
        }
        "info" => Answer::Info(super::info_of(id)?),
        "nodes" => Answer::Nodes(super::read(id, |s| s.nodes.clone())?),
        "hit" => {
            let (x, y, deep): (f64, f64, bool) = args(text)?;
            Answer::Hit(super::hit_at(id, x, y, deep)?)
        }
        "export_png" => {
            let (path, width, height): (String, i64, i64) = args(text)?;
            let ops = picture(id, width, height)?;
            let mut canvas = crate::render::Canvas::try_new(width, height)?;
            canvas.fill(crate::style::color::Color::white());
            crate::render::display::draw(&mut canvas, &ops, 0, 0);
            let png = crate::render::png::encode(width, height, &canvas.pixels)
                .ok_or("the scene image did not encode")?;
            std::fs::write(&path, png).map_err(|e| format!("{path}: {e}"))?;
            Answer::Done
        }
        "drawing" => {
            let (width, height): (i64, i64) = args(text)?;
            Answer::Item(Box::new(crate::print::Item::Drawing {
                ops: picture(id, width, height)?,
                width: width as f64,
                height: height as f64,
            }))
        }
        "close" => {
            super::close(id);
            Answer::Done
        }
        _ => return Err(format!("unknown scene operation {method}")),
    };
    serde_json::to_string(&answer).map_err(|e| e.to_string())
}
