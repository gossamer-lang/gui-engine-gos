//! Scene canvases: a retained tree of shapes, text, images and groups with
//! transforms and clipping, edited by selection, moving, resizing and
//! deletion with bounded undo, viewed through a pannable, zoomable camera.
//!
//! A scene is a session-owned handle. [`render::render`] draws it as drawing
//! operations, so the widget, PNG export and printing show the same picture;
//! the widget exposes every object to assistive technology.

pub mod api;
pub mod render;
pub mod widget;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::memory::{Budget, ByteCache, Lease};
use crate::render::vector::Path;
use crate::style::color::Color;

const HANDLE_LIMIT: usize = 256;
const NODE_LIMIT: usize = 100_000;
const HISTORY_LIMIT: usize = 100;
const HISTORY_BYTES: usize = 64 * 1024 * 1024;
const GEOMETRY_BYTES: usize = 8 * 1024 * 1024;

/// What a node draws.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Shape {
    Rect {
        width: f64,
        height: f64,
        radius: f64,
    },
    Ellipse {
        width: f64,
        height: f64,
    },
    /// A path in the node's own coordinates.
    Path(Path),
    /// Text with its top-left at the node's origin. Text does not rotate.
    Text {
        text: String,
        size: f64,
    },
    /// Pixels drawn at `width` x `height`. Images do not rotate.
    Image {
        rgba: Vec<u8>,
        pixels_wide: i64,
        pixels_high: i64,
        width: f64,
        height: f64,
    },
    /// Children drawn in order, optionally clipped to `width` x `height`.
    Group {
        width: f64,
        height: f64,
        clip: bool,
        children: Vec<Node>,
    },
}

/// Position, rotation (degrees, about the origin) and scale.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transform {
    pub x: f64,
    pub y: f64,
    pub rotation: f64,
    pub scale: f64,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
            scale: 1.0,
        }
    }
}

/// One object of a scene.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Node {
    /// Unique within the scene.
    pub id: String,
    /// The accessible name; empty marks the object decorative.
    pub label: String,
    pub shape: Shape,
    pub transform: Transform,
    pub fill: Option<Color>,
    /// Stroke color and width.
    pub stroke: Option<(Color, f64)>,
    /// 0..=1.
    pub opacity: f64,
    pub visible: bool,
    /// A locked object cannot be selected or moved by the pointer.
    pub locked: bool,
}

impl Default for Node {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            shape: Shape::Rect {
                width: 0.0,
                height: 0.0,
                radius: 0.0,
            },
            transform: Transform::default(),
            fill: None,
            stroke: None,
            opacity: 1.0,
            visible: true,
            locked: false,
        }
    }
}

/// How the view looks at the scene: scene point (`x`, `y`) at the view's
/// top-left, magnified `zoom` times.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            zoom: 1.0,
        }
    }
}

/// A scene's content and view.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    pub nodes: Vec<Node>,
    pub selection: Vec<String>,
    pub camera: Camera,
    pub revision: u64,
}

/// What an edit changed, for the application.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub revision: i64,
    pub selection: Vec<String>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub nodes: i64,
}

struct Entry {
    scene: Scene,
    undo: Vec<Arc<Vec<Node>>>,
    redo: Vec<Arc<Vec<Node>>>,
    history_bytes: usize,
    /// The nodes as a pointer gesture found them, committed as one step.
    gesture: Option<Arc<Vec<Node>>>,
    budget: Budget,
    lease: Lease,
    /// Flattened path outlines for hit testing, by node id and revision.
    geometry: render::Outlines,
}

static NEXT: AtomicI64 = AtomicI64::new(1);
static SCENES: LazyLock<Mutex<HashMap<i64, Entry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn count(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|n| {
            1 + if let Shape::Group { children, .. } = &n.shape {
                count(children)
            } else {
                0
            }
        })
        .sum()
}

fn ids<'a>(nodes: &'a [Node], out: &mut Vec<&'a str>) {
    for n in nodes {
        out.push(&n.id);
        if let Shape::Group { children, .. } = &n.shape {
            ids(children, out);
        }
    }
}

fn check(nodes: &[Node]) -> Result<(), String> {
    if count(nodes) > NODE_LIMIT {
        return Err(format!("a scene holds at most {NODE_LIMIT} objects"));
    }
    let mut all = Vec::new();
    ids(nodes, &mut all);
    let mut seen = std::collections::HashSet::new();
    if let Some(repeated) = all.iter().find(|id| id.is_empty() || !seen.insert(**id)) {
        return Err(format!(
            "object ids must be unique and not empty: {repeated:?}"
        ));
    }
    fn finite(nodes: &[Node]) -> bool {
        nodes.iter().all(|n| {
            let t = n.transform;
            [t.x, t.y, t.rotation, t.scale, n.opacity]
                .iter()
                .all(|v| v.is_finite())
                && t.scale > 0.0
                && match &n.shape {
                    Shape::Image {
                        rgba,
                        pixels_wide,
                        pixels_high,
                        ..
                    } => {
                        *pixels_wide > 0
                            && *pixels_high > 0
                            && rgba.len() == (*pixels_wide * *pixels_high * 4) as usize
                    }
                    Shape::Group { children, .. } => finite(children),
                    _ => true,
                }
        })
    }
    if !finite(nodes) {
        return Err("object transforms must be finite with a positive scale, and images must match their size".into());
    }
    Ok(())
}

fn bytes(scene: &Scene) -> usize {
    crate::memory::measure(&scene.nodes) + 4096
}

fn with<T>(id: i64, f: impl FnOnce(&mut Entry) -> Result<T, String>) -> Result<T, String> {
    let mut scenes = SCENES.lock();
    let entry = scenes.get_mut(&id).ok_or("the scene is closed")?;
    f(entry)
}

fn info(entry: &Entry) -> Info {
    Info {
        revision: entry.scene.revision as i64,
        selection: entry.scene.selection.clone(),
        can_undo: !entry.undo.is_empty(),
        can_redo: !entry.redo.is_empty(),
        nodes: count(&entry.scene.nodes) as i64,
    }
}

/// Applies `edit` to the scene's nodes as one undoable step.
fn edit(
    entry: &mut Entry,
    edit: impl FnOnce(&mut Vec<Node>) -> Result<(), String>,
) -> Result<Info, String> {
    let before = Arc::new(entry.scene.nodes.clone());
    let mut nodes = entry.scene.nodes.clone();
    edit(&mut nodes)?;
    check(&nodes)?;
    if nodes == *before {
        return Ok(info(entry));
    }
    entry.lease.resize(crate::memory::measure(&nodes) + 4096)?;
    remember(entry, before);
    entry.scene.nodes = nodes;
    settle(entry);
    Ok(info(entry))
}

/// Records `before` as the step undo returns to, within the history bounds.
fn remember(entry: &mut Entry, before: Arc<Vec<Node>>) {
    entry.history_bytes += crate::memory::measure(&*before);
    entry.undo.push(before);
    while entry.undo.len() > HISTORY_LIMIT || entry.history_bytes > HISTORY_BYTES {
        let dropped = entry.undo.remove(0);
        entry.history_bytes -= crate::memory::measure(&*dropped);
    }
    for redo in entry.redo.drain(..) {
        entry.history_bytes -= crate::memory::measure(&*redo);
    }
}

/// Starts a pointer gesture: its changes become one undoable step.
pub fn begin_gesture(id: i64) -> Result<(), String> {
    with(id, |entry| {
        entry.gesture = Some(Arc::new(entry.scene.nodes.clone()));
        Ok(())
    })
}

/// Changes the scene during a gesture, from the gesture's starting nodes.
pub fn gesture(id: i64, change: impl FnOnce(&mut Vec<Node>)) -> Result<(), String> {
    with(id, |entry| {
        let start = entry.gesture.clone().ok_or("no gesture is under way")?;
        let mut nodes = (*start).clone();
        change(&mut nodes);
        check(&nodes)?;
        entry.scene.nodes = nodes;
        entry.scene.revision += 1;
        Ok(())
    })
}

/// Ends a gesture, recording it when it changed anything.
pub fn end_gesture(id: i64) -> Result<Option<Info>, String> {
    with(id, |entry| {
        let Some(start) = entry.gesture.take() else {
            return Ok(None);
        };
        if *start == entry.scene.nodes {
            return Ok(None);
        }
        entry.lease.resize(bytes(&entry.scene))?;
        remember(entry, start);
        settle(entry);
        Ok(Some(info(entry)))
    })
}

/// Drops selected ids that no longer exist and advances the revision.
fn settle(entry: &mut Entry) {
    let mut all = Vec::new();
    ids(&entry.scene.nodes, &mut all);
    let all: std::collections::HashSet<String> = all.into_iter().map(String::from).collect();
    entry.scene.selection.retain(|id| all.contains(id));
    entry.scene.revision += 1;
}

pub(crate) fn find_mut<'a>(nodes: &'a mut [Node], id: &str) -> Option<&'a mut Node> {
    for n in nodes {
        if n.id == id {
            return Some(n);
        }
        if let Shape::Group { children, .. } = &mut n.shape
            && let Some(found) = find_mut(children, id)
        {
            return Some(found);
        }
    }
    None
}

pub fn find<'a>(nodes: &'a [Node], id: &str) -> Option<&'a Node> {
    for n in nodes {
        if n.id == id {
            return Some(n);
        }
        if let Shape::Group { children, .. } = &n.shape
            && let Some(found) = find(children, id)
        {
            return Some(found);
        }
    }
    None
}

/// The children list holding `id`, and its index there.
fn parent_of<'a>(nodes: &'a mut Vec<Node>, id: &str) -> Option<(&'a mut Vec<Node>, usize)> {
    if let Some(at) = nodes.iter().position(|n| n.id == id) {
        return Some((nodes, at));
    }
    for n in nodes.iter_mut() {
        if let Shape::Group { children, .. } = &mut n.shape
            && let Some(found) = parent_of(children, id)
        {
            return Some(found);
        }
    }
    None
}

/// Opens a scene holding `nodes`.
pub fn create(nodes: Vec<Node>, budget: Budget) -> Result<i64, String> {
    check(&nodes)?;
    let scene = Scene {
        nodes,
        ..Scene::default()
    };
    let lease = budget.reserve(bytes(&scene))?;
    let mut scenes = SCENES.lock();
    if scenes.len() >= HANDLE_LIMIT {
        return Err("the scene registry is full".into());
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    scenes.insert(
        id,
        Entry {
            scene,
            undo: Vec::new(),
            redo: Vec::new(),
            history_bytes: 0,
            gesture: None,
            budget,
            lease,
            geometry: ByteCache::new(GEOMETRY_BYTES),
        },
    );
    Ok(id)
}

/// Adds `node` to `parent`'s children (or the top level for ""), at `index`
/// or last.
pub fn add(id: i64, parent: &str, index: Option<usize>, node: Node) -> Result<Info, String> {
    with(id, |entry| {
        edit(entry, |nodes| {
            let list = if parent.is_empty() {
                nodes
            } else {
                match find_mut(nodes, parent).map(|n| &mut n.shape) {
                    Some(Shape::Group { children, .. }) => children,
                    _ => return Err("the parent is not a group".into()),
                }
            };
            let at = index.unwrap_or(list.len()).min(list.len());
            list.insert(at, node);
            Ok(())
        })
    })
}

/// Replaces an object, keeping its place.
pub fn replace(id: i64, node: Node) -> Result<Info, String> {
    with(id, |entry| {
        edit(entry, |nodes| {
            let slot = find_mut(nodes, &node.id).ok_or("no such object")?;
            *slot = node;
            Ok(())
        })
    })
}

/// Removes objects and their children.
pub fn remove(id: i64, objects: &[String]) -> Result<Info, String> {
    with(id, |entry| {
        edit(entry, |nodes| {
            for object in objects {
                let (list, at) = parent_of(nodes, object).ok_or("no such object")?;
                list.remove(at);
            }
            Ok(())
        })
    })
}

/// Moves an object to `index` among its siblings: its stacking order.
pub fn restack(id: i64, object: &str, index: usize) -> Result<Info, String> {
    with(id, |entry| {
        edit(entry, |nodes| {
            let (list, at) = parent_of(nodes, object).ok_or("no such object")?;
            let node = list.remove(at);
            let to = index.min(list.len());
            list.insert(to, node);
            Ok(())
        })
    })
}

/// Moves objects by (`dx`, `dy`) scene units as one step.
pub fn translate(id: i64, objects: &[String], dx: f64, dy: f64) -> Result<Info, String> {
    with(id, |entry| {
        edit(entry, |nodes| {
            for object in objects {
                let n = find_mut(nodes, object).ok_or("no such object")?;
                n.transform.x += dx;
                n.transform.y += dy;
            }
            Ok(())
        })
    })
}

/// Sets a sized object's width and height.
pub fn resize(id: i64, object: &str, width: f64, height: f64) -> Result<Info, String> {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return Err("sizes must be positive".into());
    }
    with(id, |entry| {
        edit(entry, |nodes| {
            match &mut find_mut(nodes, object).ok_or("no such object")?.shape {
                Shape::Rect {
                    width: w,
                    height: h,
                    ..
                }
                | Shape::Ellipse {
                    width: w,
                    height: h,
                }
                | Shape::Image {
                    width: w,
                    height: h,
                    ..
                }
                | Shape::Group {
                    width: w,
                    height: h,
                    ..
                } => {
                    (*w, *h) = (width, height);
                    Ok(())
                }
                _ => Err("the object has no size to set".into()),
            }
        })
    })
}

pub fn undo(id: i64) -> Result<Info, String> {
    with(id, |entry| {
        let Some(previous) = entry.undo.pop() else {
            return Ok(info(entry));
        };
        let current = Arc::new(std::mem::replace(
            &mut entry.scene.nodes,
            (*previous).clone(),
        ));
        entry.redo.push(current);
        entry.lease.resize(bytes(&entry.scene))?;
        settle(entry);
        Ok(info(entry))
    })
}

pub fn redo(id: i64) -> Result<Info, String> {
    with(id, |entry| {
        let Some(next) = entry.redo.pop() else {
            return Ok(info(entry));
        };
        let current = Arc::new(std::mem::replace(&mut entry.scene.nodes, (*next).clone()));
        entry.undo.push(current);
        entry.lease.resize(bytes(&entry.scene))?;
        settle(entry);
        Ok(info(entry))
    })
}

/// Selects these objects; unknown and locked ids are ignored.
pub fn select(id: i64, objects: &[String]) -> Result<Info, String> {
    with(id, |entry| {
        let chosen: Vec<String> = objects
            .iter()
            .filter(|o| find(&entry.scene.nodes, o).is_some_and(|n| !n.locked))
            .cloned()
            .collect();
        if chosen != entry.scene.selection {
            entry.scene.selection = chosen;
            entry.scene.revision += 1;
        }
        Ok(info(entry))
    })
}

pub fn set_camera(id: i64, camera: Camera) -> Result<(), String> {
    if !(camera.x.is_finite()
        && camera.y.is_finite()
        && camera.zoom.is_finite()
        && camera.zoom > 0.0)
    {
        return Err("the camera needs a finite position and positive zoom".into());
    }
    with(id, |entry| {
        entry.scene.camera = Camera {
            zoom: camera.zoom.clamp(0.01, 100.0),
            ..camera
        };
        entry.scene.revision += 1;
        Ok(())
    })
}

pub fn info_of(id: i64) -> Result<Info, String> {
    with(id, |entry| Ok(info(entry)))
}

/// Reads the scene in place, as drawing does.
pub fn read<T>(id: i64, f: impl FnOnce(&Scene) -> T) -> Result<T, String> {
    with(id, |entry| Ok(f(&entry.scene)))
}

/// The object under a view pixel; see [`render::hit`].
pub fn hit_at(id: i64, x: f64, y: f64, deep: bool) -> Result<Option<String>, String> {
    with(id, |entry| {
        Ok(render::hit(&entry.scene, &mut entry.geometry, x, y, deep))
    })
}

/// The geometry cache's bytes and entries, for budget checks.
pub fn cache_usage(id: i64) -> Result<(usize, usize), String> {
    with(id, |entry| {
        Ok((entry.geometry.bytes(), entry.geometry.len()))
    })
}

pub fn close(id: i64) {
    let entry = SCENES.lock().remove(&id);
    drop(entry);
}

pub(crate) fn close_budget(budget: &Budget) {
    let closed: Vec<Entry> = {
        let mut scenes = SCENES.lock();
        let ids: Vec<i64> = scenes
            .iter()
            .filter(|(_, e)| e.budget == *budget)
            .map(|(id, _)| *id)
            .collect();
        ids.iter().filter_map(|id| scenes.remove(id)).collect()
    };
    drop(closed);
}

#[cfg(test)]
mod tests;
