//! The video surface: the newest frame letterboxed in the widget, or what
//! the player is doing when there is none. Clicking or Space plays and
//! pauses, Left and Right seek five seconds, Up and Down change the volume,
//! M mutes and Home returns to the start. Applications add their own
//! transport controls from ordinary widgets; see `gui::media`.

use serde::{Deserialize, Serialize};
use windowing::{Key, MouseButton};

use super::{State, Status};
use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Rect, Size};
use crate::style::color::Color;
use crate::text::layout::{self, LayoutOptions};
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx};
use crate::widgets::state::StateStore;

/// A view of player `player`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub id: String,
    pub player: i64,
    pub label: String,
    pub width: i64,
    pub height: i64,
}

pub fn layout_media(v: &View, c: Constraints, cx: &mut LayoutCx) -> Node {
    Node::leaf(c.constrain(Size::new(
        cx.theme.px(v.width.max(64)),
        cx.theme.px(v.height.max(36)),
    )))
}

/// A time as minutes and seconds.
pub fn clock(ms: i64) -> String {
    if ms < 0 {
        return "--:--".into();
    }
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// What the player is doing, in words.
pub fn describe(status: &Status) -> String {
    let at = format!(
        "{} of {}",
        clock(status.position_ms),
        clock(status.duration_ms)
    );
    match &status.state {
        State::Unavailable(why) => format!("Media is unavailable: {why}"),
        State::Idle => "Nothing loaded".into(),
        State::Loading => "Loading".into(),
        State::Paused => format!("Paused at {at}"),
        State::Playing => format!("Playing, {at}"),
        State::Ended => "Ended".into(),
        State::Failed(why) => format!("Cannot play: {why}"),
    }
}

pub fn paint_media(v: &View, node: &Node, cx: &mut PaintCx) {
    let size = node.size();
    let r = Rect::from_size(size);
    cx.fill(r, Color::black());
    let at = cx.abs(r);
    let caption = super::frame(v.player, |frame, status| {
        if frame.width <= 0 || frame.height <= 0 {
            return Some(describe(status));
        }
        let scale =
            (size.width as f64 / frame.width as f64).min(size.height as f64 / frame.height as f64);
        let (w, h) = (
            (frame.width as f64 * scale).round() as i64,
            (frame.height as f64 * scale).round() as i64,
        );
        let saved = cx.canvas.clip;
        cx.canvas.clip = saved.intersect(at);
        cx.canvas.pixels_scaled(
            at.x + (size.width - w) / 2,
            at.y + (size.height - h) / 2,
            w.max(1),
            h.max(1),
            &frame.rgba,
            frame.width,
            frame.height,
        );
        cx.canvas.clip = saved;
        None
    });
    if let Ok(Some(text)) = caption {
        let style = cx.theme.font(crate::style::TextRole::Body);
        let laid = layout::layout(
            &text,
            &style,
            LayoutOptions::wrapped((size.width - cx.theme.px(24)).max(1) as f64),
        );
        layout::draw(
            &mut cx.canvas,
            &laid,
            (at.x + cx.theme.px(12)) as f64,
            (at.y + size.height / 2) as f64 - laid.height / 2.0,
            &style,
            Color::white(),
        );
    }
    if cx.focus == v.id && cx.focus_visible {
        cx.focus_ring(r, 0);
    }
}

fn toggle(v: &View) -> Option<String> {
    let status = super::status(v.player).ok()?;
    match status.state {
        State::Playing => super::pause(v.player).ok().map(|_| "Paused".into()),
        State::Paused | State::Ended => super::play(v.player).ok().map(|_| "Playing".into()),
        _ => None,
    }
}

pub fn media_event(v: &View, _node: &Node, ev: &Event, _st: &mut StateStore, cx: &mut EventCx) {
    let said = match ev {
        Event::PointerDown {
            button: MouseButton::Left,
            ..
        } => {
            cx.focus_on(&v.id);
            toggle(v)
        }
        Event::KeyDown { key, .. } if cx.is_focused(&v.id) => {
            let Ok(status) = super::status(v.player) else {
                return;
            };
            match key {
                Key::Space => toggle(v),
                Key::Left | Key::Right => {
                    let step = if *key == Key::Left { -5000 } else { 5000 };
                    super::seek(v.player, status.position_ms + step)
                        .ok()
                        .map(|_| super::widget::clock((status.position_ms + step).max(0)))
                }
                Key::Up | Key::Down => {
                    let volume =
                        (status.volume + if *key == Key::Up { 0.1 } else { -0.1 }).clamp(0.0, 1.0);
                    super::set_volume(v.player, volume)
                        .ok()
                        .map(|_| format!("Volume {:.0}%", volume * 100.0))
                }
                Key::M => super::set_muted(v.player, !status.muted).ok().map(|_| {
                    if status.muted {
                        "Sound on".into()
                    } else {
                        "Muted".into()
                    }
                }),
                Key::Home => super::seek(v.player, 0).ok().map(|_| "Start".into()),
                _ => return,
            }
        }
        _ => return,
    };
    if let Some(text) = said {
        cx.announce(&text);
    }
    cx.redraw = true;
    cx.consume();
}

pub fn media_semantics(v: &View, node: &Node) -> Option<AccessNode> {
    let mut n = AccessNode::control(Role::Group, &v.id, &v.label, "");
    n.bounds = Rect::from_size(node.size());
    n.actions = ["play-pause", "seek-forward", "seek-back", "mute"]
        .map(String::from)
        .to_vec();
    if let Ok(status) = super::status(v.player) {
        n.description = describe(&status);
        n.range(
            0.0,
            status.duration_ms.max(0) as f64 / 1000.0,
            status.position_ms as f64 / 1000.0,
        );
    }
    Some(n)
}

/// Runs an accessible action, answering what to announce.
pub fn media_action(v: &View, action: &str) -> Option<String> {
    let status = super::status(v.player).ok()?;
    match action {
        "play-pause" => toggle(v),
        "seek-forward" => super::seek(v.player, status.position_ms + 5000)
            .ok()
            .map(|_| "Forward".into()),
        "seek-back" => super::seek(v.player, status.position_ms - 5000)
            .ok()
            .map(|_| "Back".into()),
        "mute" => super::set_muted(v.player, !status.muted)
            .ok()
            .map(|_| "Mute toggled".into()),
        _ => None,
    }
}
