//! Docking: panes arranged in tab groups and splits across a main window and
//! floating windows, with edge and center placement, reordering, detaching,
//! keyboard moves and versioned persistence that repairs what it restores.
//!
//! The application owns its pane contents and keeps a [`Layout`] in its
//! model; [`Layout::handle`] folds the toolkit's tab, split and drop messages
//! into it and [`view`] builds the window's widgets. Groups are tab strips
//! sharing the dock's workspace, so a pane keeps its widget state as it moves
//! between groups and windows.

pub mod api;

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::widgets::cx::UiMsg;
use crate::widgets::tabs::{TabItem, Tabs};
use crate::widgets::{Axis, Widget};

/// The persistence format [`Layout::save`] writes.
pub const VERSION: i64 = 2;
const MIN_RATIO: f64 = 0.05;
const PANE_LIMIT: usize = 4096;
const SOURCE_LIMIT: usize = 4 * 1024 * 1024;

/// Where a pane lands relative to a group: beside one of its edges, or
/// among its tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Zone {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

/// A tab dropped on a dock group, which the session reports as
/// `UiMsg::Dock(dock, drop)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drop {
    pub pane: String,
    /// The tab strip id of the group it landed on.
    pub group: String,
    pub zone: Zone,
}

/// A split or a tab group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Split {
        id: i64,
        axis: Axis,
        /// The first child's share of the extent.
        ratio: f64,
        first: Box<Node>,
        second: Box<Node>,
    },
    Group {
        id: i64,
        panes: Vec<String>,
        active: String,
    },
}

/// A window of its own holding part of the layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Floating {
    /// The window key the application opens it under.
    pub window: String,
    pub root: Node,
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

/// A whole dock: the main window's tree and its floating windows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub version: i64,
    pub root: Node,
    pub floating: Vec<Floating>,
    /// The next split, group or window number.
    pub next: i64,
}

/// What handling a message changed beyond the layout itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Change {
    /// The layout changed; the view should show it.
    Layout,
    /// A floating window needs opening under this key.
    Opened(Floating),
    /// The floating window with this key closed.
    Closed(String),
    /// The user closed this pane; the application releases its content.
    PaneClosed(String),
    /// Keyboard focus belongs on this group's tab strip, showing this pane.
    Focus { strip: String, pane: String },
}

/// Which window holds a group: the main one or a floating one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Home {
    Main,
    Floating(usize),
}

/// A group's place in its window as fractions of the window's extent.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Area {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Node {
    fn group(id: i64, panes: Vec<String>) -> Node {
        let active = panes.first().cloned().unwrap_or_default();
        Node::Group { id, panes, active }
    }

    fn groups<'a>(&'a self, area: Area, out: &mut Vec<(i64, &'a [String], Area)>) {
        match self {
            Node::Group { id, panes, .. } => out.push((*id, panes, area)),
            Node::Split {
                axis,
                ratio,
                first,
                second,
                ..
            } => {
                let (a, b) = match axis {
                    Axis::Horizontal => (
                        Area {
                            w: area.w * ratio,
                            ..area
                        },
                        Area {
                            x: area.x + area.w * ratio,
                            w: area.w * (1.0 - ratio),
                            ..area
                        },
                    ),
                    Axis::Vertical => (
                        Area {
                            h: area.h * ratio,
                            ..area
                        },
                        Area {
                            y: area.y + area.h * ratio,
                            h: area.h * (1.0 - ratio),
                            ..area
                        },
                    ),
                };
                first.groups(a, out);
                second.groups(b, out);
            }
        }
    }

    fn find_group(&mut self, group: i64) -> Option<&mut Node> {
        match self {
            Node::Group { id, .. } if *id == group => Some(self),
            Node::Group { .. } => None,
            Node::Split { first, second, .. } => first
                .find_group(group)
                .or_else(move || second.find_group(group)),
        }
    }

    fn find_split(&mut self, split: i64) -> Option<&mut f64> {
        match self {
            Node::Group { .. } => None,
            Node::Split {
                id,
                ratio,
                first,
                second,
                ..
            } => {
                if *id == split {
                    Some(ratio)
                } else {
                    first
                        .find_split(split)
                        .or_else(move || second.find_split(split))
                }
            }
        }
    }

    /// Removes `pane`, collapsing a group it leaves empty into its sibling.
    /// Answers whether the pane was here and whether this node is now empty.
    fn remove(&mut self, pane: &str) -> (bool, bool) {
        match self {
            Node::Group { panes, active, .. } => {
                let Some(at) = panes.iter().position(|p| p == pane) else {
                    return (false, panes.is_empty());
                };
                panes.remove(at);
                if active == pane {
                    *active = panes
                        .get(at.min(panes.len().saturating_sub(1)))
                        .cloned()
                        .unwrap_or_default();
                }
                (true, panes.is_empty())
            }
            Node::Split { first, second, .. } => {
                let (found, empty) = first.remove(pane);
                if found {
                    if empty {
                        *self = std::mem::replace(second, Node::group(0, Vec::new()));
                    }
                    return (true, false);
                }
                let (found, empty) = second.remove(pane);
                if found && empty {
                    *self = std::mem::replace(first, Node::group(0, Vec::new()));
                }
                (found, false)
            }
        }
    }

    /// The ratio of the innermost split with the group holding `pane` as a
    /// direct child, and whether that group is the split's first child.
    fn share_of(&mut self, pane: &str) -> Option<(&mut f64, bool)> {
        match self {
            Node::Group { .. } => None,
            Node::Split {
                ratio,
                first,
                second,
                ..
            } => {
                let holds = |n: &Node| matches!(n, Node::Group { panes, .. } if panes.iter().any(|p| p == pane));
                if holds(first) {
                    Some((ratio, true))
                } else if holds(second) {
                    Some((ratio, false))
                } else {
                    first.share_of(pane).or_else(move || second.share_of(pane))
                }
            }
        }
    }

    fn panes(&self, out: &mut Vec<String>) {
        match self {
            Node::Group { panes, .. } => out.extend(panes.iter().cloned()),
            Node::Split { first, second, .. } => {
                first.panes(out);
                second.panes(out);
            }
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self, Node::Group { panes, .. } if panes.is_empty())
    }
}

const WHOLE: Area = Area {
    x: 0.0,
    y: 0.0,
    w: 1.0,
    h: 1.0,
};

impl Layout {
    /// One group holding `panes` in the main window.
    pub fn new(panes: &[String]) -> Layout {
        Layout {
            version: VERSION,
            root: Node::group(1, panes.to_vec()),
            floating: Vec::new(),
            next: 2,
        }
    }

    fn fresh(&mut self) -> i64 {
        let id = self.next;
        self.next += 1;
        id
    }

    fn root_of(&mut self, home: Home) -> &mut Node {
        match home {
            Home::Main => &mut self.root,
            Home::Floating(i) => &mut self.floating[i].root,
        }
    }

    fn homes(&self) -> Vec<(Home, &Node)> {
        std::iter::once((Home::Main, &self.root))
            .chain(
                self.floating
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (Home::Floating(i), &f.root)),
            )
            .collect()
    }

    /// Every group with its window and area, main window first.
    fn group_areas(&self) -> Vec<(Home, i64, Vec<String>, Area)> {
        let mut out = Vec::new();
        for (home, root) in self.homes() {
            let mut groups = Vec::new();
            root.groups(WHOLE, &mut groups);
            out.extend(
                groups
                    .into_iter()
                    .map(|(id, panes, area)| (home, id, panes.to_vec(), area)),
            );
        }
        out
    }

    fn home_of_group(&self, group: i64) -> Option<Home> {
        self.group_areas()
            .into_iter()
            .find(|(_, id, _, _)| *id == group)
            .map(|(home, ..)| home)
    }

    /// The group holding `pane`.
    pub fn group_of(&self, pane: &str) -> Option<i64> {
        self.group_areas()
            .into_iter()
            .find(|(_, _, panes, _)| panes.iter().any(|p| p == pane))
            .map(|(_, id, ..)| id)
    }

    /// Every pane, main window first, in tab order.
    pub fn panes(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (_, root) in self.homes() {
            root.panes(&mut out);
        }
        out
    }

    /// Takes `pane` out of the layout; a floating window it leaves empty
    /// closes, and its key is answered.
    fn take(&mut self, pane: &str) -> Option<String> {
        let mut emptied = None;
        for i in 0..self.floating.len() {
            let (found, _) = self.floating[i].root.remove(pane);
            if found {
                if self.floating[i].root.is_empty() {
                    emptied = Some(i);
                }
                break;
            }
        }
        if emptied.is_none() {
            self.root.remove(pane);
        }
        emptied.map(|i| self.floating.remove(i).window)
    }

    /// Places `pane` against `group` at `zone`; among the group's tabs it goes
    /// to `index`, or last. Answers the floating window the move emptied.
    pub fn place(
        &mut self,
        pane: &str,
        group: i64,
        zone: Zone,
        index: Option<usize>,
    ) -> Result<Option<String>, String> {
        let source = self.group_of(pane).ok_or("the pane is not docked")?;
        let target_home = self.home_of_group(group).ok_or("no such dock group")?;
        let source_size = self
            .group_areas()
            .into_iter()
            .find(|(_, id, ..)| *id == source)
            .map_or(0, |(_, _, panes, _)| panes.len());
        if source == group && (zone == Zone::Center || source_size == 1) {
            if let Some(index) = index {
                self.reorder(group, pane, index)?;
            }
            return Ok(None);
        }
        let closed = self.take(pane);
        // A floating window that closed shifts the ones after it.
        let target_home = match (target_home, &closed) {
            (Home::Floating(_), Some(_)) => self
                .home_of_group(group)
                .ok_or("the target group closed with its window")?,
            _ => target_home,
        };
        let fresh = if zone == Zone::Center {
            0
        } else {
            self.fresh()
        };
        let split = if zone == Zone::Center {
            0
        } else {
            self.fresh()
        };
        let node = self
            .root_of(target_home)
            .find_group(group)
            .ok_or("no such dock group")?;
        match zone {
            Zone::Center => {
                if let Node::Group { panes, active, .. } = node {
                    let at = index.unwrap_or(panes.len()).min(panes.len());
                    panes.insert(at, pane.to_string());
                    *active = pane.to_string();
                }
            }
            edge => {
                let existing = std::mem::replace(node, Node::group(0, Vec::new()));
                let added = Node::group(fresh, vec![pane.to_string()]);
                let axis = match edge {
                    Zone::Left | Zone::Right => Axis::Horizontal,
                    _ => Axis::Vertical,
                };
                let (first, second) = match edge {
                    Zone::Left | Zone::Top => (added, existing),
                    _ => (existing, added),
                };
                *node = Node::Split {
                    id: split,
                    axis,
                    ratio: 0.5,
                    first: Box::new(first),
                    second: Box::new(second),
                };
            }
        }
        Ok(closed)
    }

    /// Docks a new pane among the tabs of the main window's first group and
    /// shows it.
    pub fn add(&mut self, pane: &str) -> Result<(), String> {
        if self.group_of(pane).is_some() {
            return Err("the pane is already docked".into());
        }
        if self.panes().len() >= PANE_LIMIT {
            return Err("the dock holds its most panes".into());
        }
        let mut groups = Vec::new();
        self.root.groups(WHOLE, &mut groups);
        let first = groups[0].0;
        if let Some(Node::Group { panes, active, .. }) = self.root.find_group(first) {
            panes.push(pane.to_string());
            *active = pane.to_string();
        }
        Ok(())
    }

    /// Moves `pane` to `index` among its group's tabs.
    pub fn reorder(&mut self, group: i64, pane: &str, index: usize) -> Result<(), String> {
        let home = self.home_of_group(group).ok_or("no such dock group")?;
        let Some(Node::Group { panes, .. }) = self.root_of(home).find_group(group) else {
            return Err("no such dock group".into());
        };
        let from = panes
            .iter()
            .position(|p| p == pane)
            .ok_or("the pane is not in that group")?;
        let moved = panes.remove(from);
        panes.insert(index.min(panes.len()), moved);
        Ok(())
    }

    /// Shows `pane` in its group.
    pub fn activate(&mut self, pane: &str) -> Result<(), String> {
        let group = self.group_of(pane).ok_or("the pane is not docked")?;
        let home = self.home_of_group(group).ok_or("no such dock group")?;
        if let Some(Node::Group { active, .. }) = self.root_of(home).find_group(group) {
            *active = pane.to_string();
        }
        Ok(())
    }

    /// Removes `pane`; answers the floating window that closed with it.
    pub fn close(&mut self, pane: &str) -> Result<Option<String>, String> {
        self.group_of(pane).ok_or("the pane is not docked")?;
        Ok(self.take(pane))
    }

    /// Moves `pane` into a new floating window at the given position and
    /// size, answering the window, and the window it emptied if any.
    pub fn detach(
        &mut self,
        pane: &str,
        (x, y, width, height): (i64, i64, i64, i64),
    ) -> Result<(Floating, Option<String>), String> {
        self.group_of(pane).ok_or("the pane is not docked")?;
        if self.panes().len() == 1 {
            return Err("the pane is the only one docked".into());
        }
        let closed = self.take(pane);
        let number = self.fresh();
        let group = self.fresh();
        let floating = Floating {
            window: format!("dock-window-{number}"),
            root: Node::group(group, vec![pane.to_string()]),
            x,
            y,
            width: width.max(160),
            height: height.max(120),
        };
        self.floating.push(floating.clone());
        Ok((floating, closed))
    }

    /// Returns a floating window's panes to the main window's first group.
    pub fn reattach(&mut self, window: &str) -> Result<(), String> {
        let at = self
            .floating
            .iter()
            .position(|f| f.window == window)
            .ok_or("no such floating window")?;
        let floating = self.floating.remove(at);
        let mut panes = Vec::new();
        floating.root.panes(&mut panes);
        let mut groups = Vec::new();
        self.root.groups(WHOLE, &mut groups);
        let first = groups[0].0;
        if let Some(Node::Group {
            panes: into,
            active,
            ..
        }) = self.root.find_group(first)
        {
            if let Some(last) = panes.last() {
                *active = last.clone();
            }
            into.extend(panes);
        }
        Ok(())
    }

    /// Records where a floating window is, so a restored layout reopens it
    /// there.
    pub fn place_window(&mut self, window: &str, (x, y, width, height): (i64, i64, i64, i64)) {
        if let Some(floating) = self.floating.iter_mut().find(|f| f.window == window) {
            (floating.x, floating.y) = (x, y);
            (floating.width, floating.height) = (width.max(160), height.max(120));
        }
    }

    /// Sets a split's ratio, kept clear of either edge.
    pub fn set_ratio(&mut self, split: i64, ratio: f64) -> Result<(), String> {
        for i in 0..=self.floating.len() {
            let root = if i == 0 {
                &mut self.root
            } else {
                &mut self.floating[i - 1].root
            };
            if let Some(slot) = root.find_split(split) {
                *slot = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
                return Ok(());
            }
        }
        Err("no such dock split".into())
    }

    /// Gives the group holding `pane` this share of the split it sits in,
    /// kept clear of either edge.
    pub fn set_share(&mut self, pane: &str, share: f64) -> Result<(), String> {
        if !share.is_finite() {
            return Err("the share is not a number".into());
        }
        let share = share.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
        for i in 0..=self.floating.len() {
            let root = if i == 0 {
                &mut self.root
            } else {
                &mut self.floating[i - 1].root
            };
            if let Some((ratio, first)) = root.share_of(pane) {
                *ratio = if first { share } else { 1.0 - share };
                return Ok(());
            }
        }
        Err(if self.group_of(pane).is_some() {
            "the pane's group fills its window".into()
        } else {
            "the pane is not docked".into()
        })
    }

    /// The active pane of the group after (or before) `pane`'s, cycling
    /// through every window's groups.
    pub fn next_group(&self, pane: &str, backward: bool) -> Option<String> {
        let groups: Vec<_> = self
            .group_areas()
            .into_iter()
            .filter(|(_, _, panes, _)| !panes.is_empty())
            .collect();
        let at = groups
            .iter()
            .position(|(_, _, panes, _)| panes.iter().any(|p| p == pane))?;
        let n = groups.len();
        let next = if backward {
            (at + n - 1) % n
        } else {
            (at + 1) % n
        };
        let id = groups[next].1;
        self.homes().into_iter().find_map(|(_, root)| {
            let mut root = root.clone();
            match root.find_group(id) {
                Some(Node::Group { active, .. }) => Some(active.clone()),
                _ => None,
            }
        })
    }

    /// Moves `pane` one step toward `zone` from the keyboard: into the
    /// nearest group on that side of its own in the same window, or, with
    /// none there, into a new group along that edge.
    pub fn nudge(&mut self, pane: &str, zone: Zone) -> Result<Option<String>, String> {
        let source = self.group_of(pane).ok_or("the pane is not docked")?;
        let areas = self.group_areas();
        let (home, _, _, from) = areas
            .iter()
            .find(|(_, id, ..)| *id == source)
            .cloned()
            .ok_or("no such dock group")?;
        let overlaps = |a0: f64, a1: f64, b0: f64, b1: f64| a0 < b1 - 1e-9 && b0 < a1 - 1e-9;
        let neighbor = areas
            .iter()
            .filter(|(h, id, ..)| *h == home && *id != source)
            .filter(|(_, _, _, to)| match zone {
                Zone::Left => {
                    to.x + to.w <= from.x + 1e-9
                        && overlaps(from.y, from.y + from.h, to.y, to.y + to.h)
                }
                Zone::Right => {
                    to.x >= from.x + from.w - 1e-9
                        && overlaps(from.y, from.y + from.h, to.y, to.y + to.h)
                }
                Zone::Top => {
                    to.y + to.h <= from.y + 1e-9
                        && overlaps(from.x, from.x + from.w, to.x, to.x + to.w)
                }
                Zone::Bottom => {
                    to.y >= from.y + from.h - 1e-9
                        && overlaps(from.x, from.x + from.w, to.x, to.x + to.w)
                }
                Zone::Center => false,
            })
            .min_by(|(.., a), (.., b)| {
                let distance = |to: &Area| (to.x - from.x).abs() + (to.y - from.y).abs();
                distance(a).total_cmp(&distance(b))
            })
            .map(|(_, id, ..)| *id);
        match neighbor {
            Some(group) => self.place(pane, group, Zone::Center, None),
            None => self.place(pane, source, zone, None),
        }
    }

    /// The layout's persistence form.
    pub fn save(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Restores a saved layout for the panes the application has now: it
    /// migrates older versions, drops unknown and repeated panes, adds known
    /// panes the save lacks, clamps ratios and renumbers repeated ids. A save
    /// that cannot be read starts over from [`Layout::new`]. Answers the
    /// repairs it made.
    pub fn restore(source: &str, known: &[String]) -> (Layout, Vec<String>) {
        let mut repairs = Vec::new();
        let parsed = if source.len() > SOURCE_LIMIT {
            Err("the saved layout exceeds its size limit".to_string())
        } else {
            parse(source)
        };
        let mut layout = match parsed {
            Ok((layout, migrated)) => {
                if migrated {
                    repairs.push("migrated a version 1 layout".to_string());
                }
                layout
            }
            Err(why) => {
                repairs.push(format!("started over: {why}"));
                return (Layout::new(known), repairs);
            }
        };
        let known_set: HashSet<&String> = known.iter().collect();
        let mut seen = HashSet::new();
        let mut ids = HashSet::new();
        let mut next = layout.next.max(1);
        let mut repair = |node: &mut Node, repairs: &mut Vec<String>| {
            sanitize(node, &known_set, &mut seen, &mut ids, &mut next, repairs)
        };
        repair(&mut layout.root, &mut repairs);
        let mut windows = HashSet::new();
        layout.floating.retain_mut(|f| {
            repair(&mut f.root, &mut repairs);
            let keep = !f.root.is_empty() && windows.insert(f.window.clone());
            if !keep {
                repairs.push(format!("dropped floating window {}", f.window));
            }
            f.width = f.width.clamp(160, 16_384);
            f.height = f.height.clamp(120, 16_384);
            keep
        });
        let missing: Vec<String> = known
            .iter()
            .filter(|p| !seen.contains(*p))
            .cloned()
            .collect();
        if !missing.is_empty() {
            repairs.push(format!("added {} pane(s) the save lacked", missing.len()));
            let mut groups = Vec::new();
            layout.root.groups(WHOLE, &mut groups);
            let first = groups[0].0;
            if let Some(Node::Group { panes, active, .. }) = layout.root.find_group(first) {
                if active.is_empty() {
                    *active = missing[0].clone();
                }
                panes.extend(missing);
            }
        }
        layout.next = next;
        layout.version = VERSION;
        (layout, repairs)
    }

    /// Folds a toolkit message about dock `dock` into the layout. `window`
    /// sizes a pane detached by the message.
    pub fn handle(&mut self, dock: &str, msg: &UiMsg) -> Vec<Change> {
        let group = |strip: &str| group_id(dock, strip);
        let result: Result<Vec<Change>, String> = match msg {
            UiMsg::Tab(strip, pane) if group(strip).is_some() => {
                self.activate(pane).map(|_| vec![Change::Layout])
            }
            UiMsg::Reorder(strip, pane, index) => match group(strip) {
                Some(g) => self
                    .reorder(g, pane, (*index).max(0) as usize)
                    .map(|_| vec![Change::Layout]),
                None => Ok(Vec::new()),
            },
            UiMsg::TabMove(from, pane, to, index) if group(from).is_some() => match group(to) {
                Some(g) => self
                    .place(pane, g, Zone::Center, Some((*index).max(0) as usize))
                    .map(changed),
                None => Ok(Vec::new()),
            },
            UiMsg::TabDetach(strip, pane, x, y) if group(strip).is_some() => self
                .detach(pane, (*x, *y, 640, 480))
                .map(|(opened, closed)| {
                    let mut changes = changed(closed);
                    changes.push(Change::Opened(opened));
                    changes
                }),
            UiMsg::TabAction(strip, pane, action) if group(strip).is_some() => {
                self.tab_action(pane, action)
            }
            UiMsg::Split(split, ratio) => match split_id(dock, split) {
                Some(s) => self.set_ratio(s, *ratio).map(|_| vec![Change::Layout]),
                None => Ok(Vec::new()),
            },
            UiMsg::Dock(target, drop) if target == dock => match group(&drop.group) {
                Some(g) => self.place(&drop.pane, g, drop.zone, None).map(changed),
                None => Ok(Vec::new()),
            },
            UiMsg::CloseRequested(window) if self.floating.iter().any(|f| &f.window == window) => {
                self.reattach(window)
                    .map(|_| vec![Change::Layout, Change::Closed(window.clone())])
            }
            _ => Ok(Vec::new()),
        };
        result.unwrap_or_default()
    }

    fn tab_action(&mut self, pane: &str, action: &str) -> Result<Vec<Change>, String> {
        match action {
            "close" => self.close(pane).map(|closed| {
                let mut changes = changed(closed);
                changes.push(Change::PaneClosed(pane.to_string()));
                changes
            }),
            "detach" => self
                .detach(pane, (80, 80, 640, 480))
                .map(|(opened, closed)| {
                    let mut changes = changed(closed);
                    changes.push(Change::Opened(opened));
                    changes
                }),
            "move-left" | "move-right" => {
                let group = self.group_of(pane).ok_or("the pane is not docked")?;
                let panes = self
                    .group_areas()
                    .into_iter()
                    .find(|(_, id, ..)| *id == group)
                    .map(|(_, _, panes, _)| panes)
                    .unwrap_or_default();
                let at = panes.iter().position(|p| p == pane).unwrap_or(0);
                let to = if action == "move-left" {
                    at.saturating_sub(1)
                } else {
                    at + 1
                };
                self.reorder(group, pane, to).map(|_| vec![Change::Layout])
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Runs a dock keyboard command of dock `dock` on the focused pane.
    pub fn command(&mut self, dock: &str, pane: &str, command: &str) -> Vec<Change> {
        let zone = match command {
            "dock.move-left" => Some(Zone::Left),
            "dock.move-right" => Some(Zone::Right),
            "dock.move-up" => Some(Zone::Top),
            "dock.move-down" => Some(Zone::Bottom),
            _ => None,
        };
        let result = match (command, zone) {
            (_, Some(zone)) => self.nudge(pane, zone).map(changed),
            ("dock.next-group" | "dock.previous-group", _) => {
                let backward = command == "dock.previous-group";
                match self.next_group(pane, backward) {
                    Some(next) => self.group_of(&next).map_or(Ok(Vec::new()), |group| {
                        Ok(vec![Change::Focus {
                            strip: group_strip(dock, group),
                            pane: next,
                        }])
                    }),
                    None => Ok(Vec::new()),
                }
            }
            ("dock.detach", _) => self.tab_action(pane, "detach"),
            ("dock.reattach", _) => {
                let window = self.floating.iter().find_map(|f| {
                    let mut panes = Vec::new();
                    f.root.panes(&mut panes);
                    panes.iter().any(|p| p == pane).then(|| f.window.clone())
                });
                match window {
                    Some(window) => self
                        .reattach(&window)
                        .map(|_| vec![Change::Layout, Change::Closed(window)]),
                    None => Ok(Vec::new()),
                }
            }
            _ => Ok(Vec::new()),
        };
        result.unwrap_or_default()
    }
}

fn changed(closed: Option<String>) -> Vec<Change> {
    let mut changes = vec![Change::Layout];
    if let Some(window) = closed {
        changes.push(Change::Closed(window));
    }
    changes
}

/// The dock keyboard commands; add them to the application's commands.
pub fn commands() -> Vec<crate::commands::Command> {
    use crate::commands::Command;
    use crate::localization::tr;
    let category = tr("Dock");
    vec![
        Command::new("dock.move-left", &tr("Move Pane Left"))
            .key("Primary+Alt+Left")
            .category(&category),
        Command::new("dock.move-right", &tr("Move Pane Right"))
            .key("Primary+Alt+Right")
            .category(&category),
        Command::new("dock.move-up", &tr("Move Pane Up"))
            .key("Primary+Alt+Up")
            .category(&category),
        Command::new("dock.move-down", &tr("Move Pane Down"))
            .key("Primary+Alt+Down")
            .category(&category),
        Command::new("dock.next-group", &tr("Next Pane Group"))
            .key("Primary+Alt+PageDown")
            .category(&category),
        Command::new("dock.previous-group", &tr("Previous Pane Group"))
            .key("Primary+Alt+PageUp")
            .category(&category),
        Command::new("dock.detach", &tr("Move Pane to New Window")).category(&category),
        Command::new("dock.reattach", &tr("Return Pane to Main Window")).category(&category),
    ]
}

/// The tab strip id of `group` in dock `dock`.
pub fn group_strip(dock: &str, group: i64) -> String {
    format!("{dock}:group:{group}")
}

fn group_id(dock: &str, strip: &str) -> Option<i64> {
    strip
        .strip_prefix(dock)?
        .strip_prefix(":group:")?
        .parse()
        .ok()
}

fn split_id(dock: &str, split: &str) -> Option<i64> {
    split
        .strip_prefix(dock)?
        .strip_prefix(":split:")?
        .parse()
        .ok()
}

/// The dock a group's tab strip belongs to.
pub(crate) fn dock_of_strip(strip: &str) -> Option<&str> {
    let (dock, group) = strip.rsplit_once(":group:")?;
    group.parse::<i64>().ok()?;
    Some(dock)
}

/// The layout a save holds, and whether it was migrated from version 1.
fn parse(source: &str) -> Result<(Layout, bool), String> {
    let value: serde_json::Value = serde_json::from_str(source).map_err(|e| e.to_string())?;
    match value.get("version").and_then(serde_json::Value::as_i64) {
        Some(VERSION) => serde_json::from_value(value)
            .map(|layout| (layout, false))
            .map_err(|e| e.to_string()),
        // Version 1 saved a row of tab groups.
        Some(1) => {
            let groups: Vec<Vec<String>> =
                serde_json::from_value(value.get("groups").cloned().unwrap_or_default())
                    .map_err(|e| e.to_string())?;
            let mut layout = Layout::new(&[]);
            let mut nodes: Vec<Node> = groups
                .into_iter()
                .map(|panes| Node::group(layout.fresh(), panes))
                .collect();
            let Some(mut root) = nodes.pop() else {
                return Ok((layout, true));
            };
            while let Some(left) = nodes.pop() {
                let count = 1.0 + split_count(&root) as f64;
                root = Node::Split {
                    id: layout.fresh(),
                    axis: Axis::Horizontal,
                    ratio: 1.0 / (count + 1.0),
                    first: Box::new(left),
                    second: Box::new(root),
                };
            }
            layout.root = root;
            Ok((layout, true))
        }
        Some(other) => Err(format!("unknown layout version {other}")),
        None => Err("the save has no version".into()),
    }
}

fn split_count(node: &Node) -> usize {
    match node {
        Node::Group { .. } => 0,
        Node::Split { first, second, .. } => 1 + split_count(first) + split_count(second),
    }
}

fn sanitize(
    node: &mut Node,
    known: &HashSet<&String>,
    seen: &mut HashSet<String>,
    ids: &mut HashSet<i64>,
    next: &mut i64,
    repairs: &mut Vec<String>,
) {
    let mut renumber = |id: &mut i64, repairs: &mut Vec<String>| {
        if *id <= 0 || !ids.insert(*id) {
            repairs.push(format!("renumbered repeated id {id}"));
            *id = *next;
            *next += 1;
            ids.insert(*id);
        }
        *next = (*next).max(*id + 1);
    };
    match node {
        Node::Group { id, panes, active } => {
            renumber(id, repairs);
            let before = panes.len();
            panes
                .retain(|p| known.contains(p) && seen.len() < PANE_LIMIT && seen.insert(p.clone()));
            if panes.len() != before {
                repairs.push(format!(
                    "dropped {} unknown or repeated pane(s)",
                    before - panes.len()
                ));
            }
            if !panes.contains(active) {
                *active = panes.first().cloned().unwrap_or_default();
            }
        }
        Node::Split {
            id,
            ratio,
            first,
            second,
            ..
        } => {
            renumber(id, repairs);
            if !ratio.is_finite() || *ratio < MIN_RATIO || *ratio > 1.0 - MIN_RATIO {
                repairs.push(format!("clamped split ratio {ratio}"));
                *ratio = if ratio.is_finite() {
                    ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO)
                } else {
                    0.5
                };
            }
            sanitize(first, known, seen, ids, next, repairs);
            sanitize(second, known, seen, ids, next, repairs);
            // A split one of whose sides lost every pane becomes the other.
            if first.is_empty() {
                *node = std::mem::replace(second, Node::group(0, Vec::new()));
            } else if second.is_empty() {
                *node = std::mem::replace(first, Node::group(0, Vec::new()));
            }
        }
    }
}

/// A pane's title and content for [`view`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    pub title: String,
    pub content: Widget,
    /// Whether the user may close it from its tab.
    pub closable: bool,
}

/// The widgets of dock `dock` in `window`: "" names the main window, any
/// other key a floating window.
pub fn view(layout: &Layout, dock: &str, window: &str, panes: &HashMap<String, Pane>) -> Widget {
    let root = if window.is_empty() {
        Some(&layout.root)
    } else {
        layout
            .floating
            .iter()
            .find(|f| f.window == window)
            .map(|f| &f.root)
    };
    match root {
        Some(root) => node_view(root, dock, panes),
        None => Widget::Empty,
    }
}

fn node_view(node: &Node, dock: &str, panes: &HashMap<String, Pane>) -> Widget {
    match node {
        Node::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => Widget::split(
            &format!("{dock}:split:{id}"),
            node_view(first, dock, panes),
            node_view(second, dock, panes),
            *axis,
            *ratio,
        ),
        Node::Group {
            id,
            panes: keys,
            active,
        } => {
            let items: Vec<TabItem> = keys
                .iter()
                .map(|key| {
                    let pane = panes.get(key);
                    TabItem::new(key, pane.map_or(key.as_str(), |p| p.title.as_str()))
                        .closable(pane.is_some_and(|p| p.closable))
                })
                .collect();
            let pages: HashMap<String, Widget> = keys
                .iter()
                .filter_map(|key| panes.get(key).map(|p| (key.clone(), p.content.clone())))
                .collect();
            Widget::Tabs(Tabs {
                id: group_strip(dock, *id),
                items,
                active: active.clone(),
                content: Box::new(pages.get(active).cloned().unwrap_or(Widget::Empty)),
                pages,
                workspace: dock.to_string(),
                add_button: false,
                detachable: true,
                joined: false,
            })
        }
    }
}

#[cfg(test)]
mod tests;
