//! The scene canvas widget. Clicking selects the topmost object (Shift adds
//! or removes it), dragging moves the selection or, from the corner handle,
//! resizes it, and dragging empty space or with the middle button pans; the
//! wheel zooms about the pointer. From the keyboard, Page Down and Page Up
//! (Home and End) walk the objects and announce them, arrows move the
//! selection (Shift moves farther), Delete removes it, Escape clears it, and
//! Ctrl+Z and Ctrl+Y undo and redo. Every object is an accessible child.

use serde::{Deserialize, Serialize};
use windowing::{Key, MouseButton};

use super::render::{self, Affine};
use super::{Camera, Info, Node, Shape};
use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Rect, Size};
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node as LayoutNode, PaintCx, UiMsg};
use crate::widgets::state::StateStore;

/// A view of scene `scene`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub id: String,
    pub scene: i64,
    pub label: String,
    pub width: i64,
    pub height: i64,
}

pub fn layout_scene(v: &View, c: Constraints, cx: &mut LayoutCx) -> LayoutNode {
    LayoutNode::leaf(c.constrain(Size::new(
        cx.theme.px(v.width.max(64)),
        cx.theme.px(v.height.max(48)),
    )))
}

pub fn paint_scene(v: &View, node: &LayoutNode, cx: &mut PaintCx) {
    let r = Rect::from_size(node.size());
    cx.fill(r, cx.theme.field);
    // Scene units are interface units: the scene draws at a scale of one
    // and the layer scales it to the window's pixels.
    let ui = cx.theme.scale;
    let mut flat = cx.theme.clone();
    flat.scale = 1.0;
    if let Ok(ops) = super::read(v.scene, |scene| render::render(scene, &flat, true)) {
        let at = cx.abs(r);
        let layer = crate::render::display::DrawOp::Layer {
            ops,
            transform: crate::render::retained::Transform {
                x: 0.0,
                y: 0.0,
                scale_x: ui,
                scale_y: ui,
            },
        };
        let clipped = [
            crate::render::display::DrawOp::Clip(r),
            layer,
            crate::render::display::DrawOp::Unclip,
        ];
        crate::render::display::draw(&mut cx.canvas, &clipped, at.x, at.y);
    }
    if cx.focus == v.id && cx.focus_visible {
        cx.focus_ring(r, 0);
    }
}

/// How an object reads aloud: its name, kind and place among its siblings.
fn describe(node: &Node, index: usize, count: usize) -> String {
    let kind = match &node.shape {
        Shape::Rect { .. } => "rectangle",
        Shape::Ellipse { .. } => "ellipse",
        Shape::Path(_) => "shape",
        Shape::Text { .. } => "text",
        Shape::Image { .. } => "image",
        Shape::Group { .. } => "group",
    };
    let name = if node.label.is_empty() {
        "Unnamed"
    } else {
        &node.label
    };
    format!("{name}, {kind}, {} of {count}", index + 1)
}

fn changed(v: &View, info: Info, cx: &mut EventCx) {
    cx.emit(&UiMsg::Scene(v.id.clone(), info));
}

fn select(v: &View, objects: &[String], cx: &mut EventCx) {
    let before = super::read(v.scene, |s| s.selection.clone()).unwrap_or_default();
    if let Ok(info) = super::select(v.scene, objects)
        && info.selection != before
    {
        changed(v, info, cx);
    }
}

/// Selects the object `step` places from the current one in top-level
/// order, brings it into view and announces it.
fn walk(v: &View, size: Size, step: i64, to_end: Option<bool>, cx: &mut EventCx) {
    let Ok(Some((id, text, bounds))) = super::read(v.scene, |scene| {
        let count = scene.nodes.len();
        if count == 0 {
            return None;
        }
        let current = scene
            .selection
            .first()
            .and_then(|s| scene.nodes.iter().position(|n| &n.id == s));
        let index = match (to_end, current) {
            (Some(false), _) => 0,
            (Some(true), _) => count - 1,
            (None, None) => {
                if step < 0 {
                    count - 1
                } else {
                    0
                }
            }
            (None, Some(i)) => (i as i64 + step).rem_euclid(count as i64) as usize,
        };
        let n = &scene.nodes[index];
        let m = Affine::camera(&scene.camera).after(&Affine::of(&n.transform));
        Some((
            n.id.clone(),
            describe(n, index, count),
            render::bounds(n, &m),
        ))
    }) else {
        return;
    };
    select(v, std::slice::from_ref(&id), cx);
    if bounds.x + bounds.width < 0
        || bounds.y + bounds.height < 0
        || bounds.x > size.width
        || bounds.y > size.height
    {
        let _ = super::read(v.scene, |s| s.camera).map(|c| {
            let _ = super::set_camera(
                v.scene,
                Camera {
                    x: c.x + (bounds.x as f64 - size.width as f64 / 4.0) / c.zoom,
                    y: c.y + (bounds.y as f64 - size.height as f64 / 4.0) / c.zoom,
                    zoom: c.zoom,
                },
            );
        });
    }
    cx.announce(&text);
}

#[derive(Serialize, Deserialize)]
enum Drag {
    Move {
        x: i64,
        y: i64,
    },
    Resize {
        x: i64,
        y: i64,
        width: f64,
        height: f64,
        factor: f64,
    },
    Pan {
        x: i64,
        y: i64,
        camera: Camera,
    },
}

fn edited(v: &View, result: Result<Info, String>, cx: &mut EventCx) {
    if let Ok(info) = result {
        changed(v, info, cx);
    }
}

pub fn scene_event(v: &View, node: &LayoutNode, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    // Pointer positions arrive in pixels; the scene works in interface units.
    let ui = cx.theme.scale.max(0.01);
    let units = |p: i64| (p as f64 / ui).round() as i64;
    let ev = &match &crate::widgets::cx::local_event(ev, cx) {
        Event::PointerDown {
            x,
            y,
            button,
            clicks,
            mods,
        } => Event::PointerDown {
            x: units(*x),
            y: units(*y),
            button: *button,
            clicks: *clicks,
            mods: *mods,
        },
        Event::PointerMove {
            x,
            y,
            pressed,
            mods,
        } => Event::PointerMove {
            x: units(*x),
            y: units(*y),
            pressed: *pressed,
            mods: *mods,
        },
        Event::PointerUp { x, y, button, mods } => Event::PointerUp {
            x: units(*x),
            y: units(*y),
            button: *button,
            mods: *mods,
        },
        Event::Scroll { x, y, dx, dy, mods } => Event::Scroll {
            x: units(*x),
            y: units(*y),
            dx: *dx,
            dy: *dy,
            mods: *mods,
        },
        other => other.clone(),
    };
    let full = node.size();
    let size = Size::new(units(full.width), units(full.height));
    let mut theme = cx.theme.clone();
    theme.scale = 1.0;
    let drag = || serde_json::from_str::<Drag>(&st.field(&v.id, "drag", "")).ok();
    match ev {
        Event::PointerDown {
            x, y, button, mods, ..
        } => {
            cx.focus_on(&v.id);
            let start = |d: &Drag, st: &mut StateStore| {
                st.set_field(&v.id, "drag", &serde_json::to_string(d).unwrap_or_default())
            };
            let handle = super::read(v.scene, |s| render::resize_handle(s, &theme))
                .ok()
                .flatten();
            if *button == MouseButton::Middle {
                if let Ok(camera) = super::read(v.scene, |s| s.camera) {
                    start(
                        &Drag::Pan {
                            x: *x,
                            y: *y,
                            camera,
                        },
                        st,
                    );
                }
            } else if let Some(h) = handle
                .filter(|h| *x >= h.x && *x < h.x + h.width && *y >= h.y && *y < h.y + h.height)
            {
                let _ = h;
                let sized = super::read(v.scene, |s| {
                    let id = s.selection.first()?;
                    let n = super::find(&s.nodes, id)?;
                    let (_, _, w, hgt) = render::local_bounds(&n.shape);
                    let m = render::parent_transform(&s.nodes, id, &Affine::camera(&s.camera))?
                        .after(&Affine::of(&n.transform));
                    Some((w, hgt, m.magnitude()))
                });
                if let Ok(Some((width, height, factor))) = sized {
                    let _ = super::begin_gesture(v.scene);
                    start(
                        &Drag::Resize {
                            x: *x,
                            y: *y,
                            width,
                            height,
                            factor,
                        },
                        st,
                    );
                }
            } else if let Ok(hit) = super::hit_at(v.scene, *x as f64, *y as f64, false) {
                match hit {
                    Some(id) => {
                        let selection =
                            super::read(v.scene, |s| s.selection.clone()).unwrap_or_default();
                        let next = if mods.shift {
                            if selection.contains(&id) {
                                selection.into_iter().filter(|s| *s != id).collect()
                            } else {
                                [selection, vec![id]].concat()
                            }
                        } else if selection.contains(&id) {
                            selection
                        } else {
                            vec![id]
                        };
                        select(v, &next, cx);
                        let _ = super::begin_gesture(v.scene);
                        start(&Drag::Move { x: *x, y: *y }, st);
                    }
                    None => {
                        if !mods.shift {
                            select(v, &[], cx);
                        }
                        if let Ok(camera) = super::read(v.scene, |s| s.camera) {
                            start(
                                &Drag::Pan {
                                    x: *x,
                                    y: *y,
                                    camera,
                                },
                                st,
                            );
                        }
                    }
                }
            }
            cx.capture_pointer(&v.id);
            cx.redraw = true;
            cx.consume();
        }
        Event::PointerMove {
            x,
            y,
            pressed: true,
            ..
        } => {
            let Some(d) = drag() else {
                return;
            };
            let zoom = super::read(v.scene, |s| s.camera.zoom).unwrap_or(1.0);
            match d {
                Drag::Move { x: sx, y: sy } => {
                    let (dx, dy) = ((*x - sx) as f64 / zoom, (*y - sy) as f64 / zoom);
                    let selection =
                        super::read(v.scene, |s| s.selection.clone()).unwrap_or_default();
                    let _ = super::gesture(v.scene, |nodes| {
                        for id in &selection {
                            if let Some(n) = super::find_mut(nodes, id) {
                                n.transform.x += dx;
                                n.transform.y += dy;
                            }
                        }
                    });
                }
                Drag::Resize {
                    x: sx,
                    y: sy,
                    width,
                    height,
                    factor,
                } => {
                    let (w, h) = (
                        (width + (*x - sx) as f64 / factor).max(1.0),
                        (height + (*y - sy) as f64 / factor).max(1.0),
                    );
                    let selection =
                        super::read(v.scene, |s| s.selection.clone()).unwrap_or_default();
                    let _ = super::gesture(v.scene, |nodes| {
                        if let Some(n) = selection.first().and_then(|id| super::find_mut(nodes, id))
                        {
                            match &mut n.shape {
                                Shape::Rect { width, height, .. }
                                | Shape::Ellipse { width, height }
                                | Shape::Image { width, height, .. }
                                | Shape::Group { width, height, .. } => {
                                    (*width, *height) = (w, h);
                                }
                                _ => {}
                            }
                        }
                    });
                }
                Drag::Pan {
                    x: sx,
                    y: sy,
                    camera,
                } => {
                    let _ = super::set_camera(
                        v.scene,
                        Camera {
                            x: camera.x - (*x - sx) as f64 / camera.zoom,
                            y: camera.y - (*y - sy) as f64 / camera.zoom,
                            zoom: camera.zoom,
                        },
                    );
                }
            }
            cx.redraw = true;
            cx.consume();
        }
        Event::PointerUp { .. } | Event::CaptureLost if drag().is_some() => {
            st.set_field(&v.id, "drag", "");
            cx.release_pointer();
            if let Ok(Some(info)) = super::end_gesture(v.scene) {
                changed(v, info, cx);
            }
            cx.redraw = true;
        }
        Event::Scroll { x, y, dy, .. } if *dy != 0.0 => {
            if let Ok(c) = super::read(v.scene, |s| s.camera) {
                let zoom = (c.zoom * 1.1f64.powf(*dy)).clamp(0.01, 100.0);
                let (px, py) = (*x as f64, *y as f64);
                let (sx, sy) = (c.x + px / c.zoom, c.y + py / c.zoom);
                let _ = super::set_camera(
                    v.scene,
                    Camera {
                        x: sx - px / zoom,
                        y: sy - py / zoom,
                        zoom,
                    },
                );
                cx.redraw = true;
                cx.consume();
            }
        }
        Event::KeyDown { key, mods, .. } if cx.is_focused(&v.id) => {
            let selection = super::read(v.scene, |s| s.selection.clone()).unwrap_or_default();
            let nudge = if mods.shift { 10.0 } else { 1.0 };
            match key {
                Key::PageDown => walk(v, size, 1, None, cx),
                Key::PageUp => walk(v, size, -1, None, cx),
                Key::Home => walk(v, size, 0, Some(false), cx),
                Key::End => walk(v, size, 0, Some(true), cx),
                Key::Left | Key::Right | Key::Up | Key::Down if !selection.is_empty() => {
                    let (dx, dy) = match key {
                        Key::Left => (-nudge, 0.0),
                        Key::Right => (nudge, 0.0),
                        Key::Up => (0.0, -nudge),
                        _ => (0.0, nudge),
                    };
                    edited(v, super::translate(v.scene, &selection, dx, dy), cx);
                }
                Key::Delete | Key::Backspace if !selection.is_empty() => {
                    edited(v, super::remove(v.scene, &selection), cx);
                    cx.announce(&format!("Deleted {}", selection.len()));
                }
                Key::Escape if !selection.is_empty() => select(v, &[], cx),
                Key::Z if mods.ctrl || mods.gui => edited(
                    v,
                    if mods.shift {
                        super::redo(v.scene)
                    } else {
                        super::undo(v.scene)
                    },
                    cx,
                ),
                Key::Y if mods.ctrl || mods.gui => edited(v, super::redo(v.scene), cx),
                Key::Num0 if mods.ctrl || mods.gui => {
                    let _ = super::set_camera(v.scene, Camera::default());
                }
                _ => return,
            }
            cx.redraw = true;
            cx.consume();
        }
        Event::Command(name) if cx.is_focused(&v.id) => {
            let selection = super::read(v.scene, |s| s.selection.clone()).unwrap_or_default();
            match name.as_str() {
                "edit.undo" => edited(v, super::undo(v.scene), cx),
                "edit.redo" => edited(v, super::redo(v.scene), cx),
                "edit.delete" if !selection.is_empty() => {
                    edited(v, super::remove(v.scene, &selection), cx)
                }
                "edit.select-all" => {
                    let all: Vec<String> = super::read(v.scene, |s| {
                        s.nodes
                            .iter()
                            .filter(|n| !n.locked)
                            .map(|n| n.id.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                    select(v, &all, cx);
                }
                _ => return,
            }
            cx.redraw = true;
            cx.consume();
        }
        Event::FocusIn | Event::FocusOut => cx.redraw = true,
        _ => {}
    }
}

fn object(
    node: &Node,
    outer: &Affine,
    prefix: &str,
    selection: &[String],
    index: usize,
    count: usize,
) -> AccessNode {
    let m = outer.after(&Affine::of(&node.transform));
    let role = if matches!(node.shape, Shape::Group { .. }) {
        Role::Group
    } else {
        Role::Image
    };
    let name = if node.label.is_empty() {
        "Unnamed"
    } else {
        &node.label
    };
    let mut n = AccessNode::new(role, name);
    n.id = format!("{prefix}#{}", node.id);
    n.description = describe(node, index, count);
    n.selected = selection.contains(&node.id);
    n.bounds = render::bounds(node, outer);
    n.pos_in_set = index as i64 + 1;
    n.set_size = count as i64;
    n.actions = vec!["select".to_string()];
    if let Shape::Group { children, .. } = &node.shape {
        let visible: Vec<&Node> = children.iter().filter(|c| c.visible).collect();
        n.children = visible
            .iter()
            .enumerate()
            .map(|(i, c)| object(c, &m, prefix, selection, i, visible.len()))
            .collect();
    }
    n
}

pub fn scene_semantics(v: &View, node: &LayoutNode, ui: f64) -> Option<AccessNode> {
    let mut n = AccessNode::control(Role::Canvas, &v.id, &v.label, "");
    n.bounds = Rect::from_size(node.size());
    if let Ok((children, description)) = super::read(v.scene, |s| {
        let camera = Affine::of(&super::Transform {
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
            scale: ui,
        })
        .after(&Affine::camera(&s.camera));
        let visible: Vec<&Node> = s.nodes.iter().filter(|n| n.visible).collect();
        let children: Vec<AccessNode> = visible
            .iter()
            .enumerate()
            .map(|(i, c)| object(c, &camera, &v.id, &s.selection, i, visible.len()))
            .collect();
        (
            children,
            format!("{} objects, {} selected", visible.len(), s.selection.len()),
        )
    }) {
        n.children = children;
        n.description = description;
    }
    Some(n)
}

/// Runs an accessible action on the canvas or on object `key`.
pub fn scene_action(v: &View, key: &str, action: &str, cx: &mut EventCx) {
    if action == "select" && !key.is_empty() {
        select(v, &[key.to_string()], cx);
        cx.redraw = true;
    }
}
