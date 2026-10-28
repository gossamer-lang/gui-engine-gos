//! Paginated documents kept on the native side for previewing, exporting and
//! printing, and the widget that shows one page. Pages never cross into the
//! application as pixels: the preview draws them where it paints.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::{Item, Page, PageSetup};
use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Rect, Size};
use crate::memory::{Budget, Lease};
use crate::render::display::DrawOp;
use crate::render::retained::Transform;
use crate::style::color::Color;
use crate::widgets::cx::{LayoutCx, Node, PaintCx};

const HANDLE_LIMIT: usize = 256;

static NEXT: AtomicI64 = AtomicI64::new(1);
static DOCUMENTS: LazyLock<Mutex<HashMap<i64, Entry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct Entry {
    items: Arc<Vec<Item>>,
    setup: PageSetup,
    pages: Arc<Vec<Page>>,
    budget: Budget,
    lease: Lease,
}

fn charge(items: &[Item], pages: &[Page]) -> usize {
    crate::memory::measure(&items)
        .saturating_add(crate::memory::measure(&pages))
        .saturating_add(4096)
}

/// Lays `items` out with `setup` and keeps the pages, answering a handle.
pub fn open(items: Vec<Item>, setup: PageSetup, budget: Budget) -> Result<i64, String> {
    let pages = super::paginate(&items, &setup, &|| false)?;
    let lease = budget.reserve(charge(&items, &pages))?;
    let mut documents = DOCUMENTS.lock();
    if documents.len() >= HANDLE_LIMIT {
        return Err("the print document registry is full".into());
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    documents.insert(
        id,
        Entry {
            items: Arc::new(items),
            setup,
            pages: Arc::new(pages),
            budget,
            lease,
        },
    );
    Ok(id)
}

/// Lays the document out again with `setup`, answering its page count.
pub fn set_setup(id: i64, setup: PageSetup) -> Result<i64, String> {
    let items = DOCUMENTS
        .lock()
        .get(&id)
        .map(|e| e.items.clone())
        .ok_or("the print document is closed")?;
    let pages = super::paginate(&items, &setup, &|| false)?;
    let mut documents = DOCUMENTS.lock();
    let entry = documents
        .get_mut(&id)
        .ok_or("the print document is closed")?;
    entry.lease.resize(charge(&items, &pages))?;
    entry.setup = setup;
    entry.pages = Arc::new(pages);
    Ok(entry.pages.len() as i64)
}

/// The document's pages and setup, shared with a job.
pub fn pages(id: i64) -> Result<(Arc<Vec<Page>>, PageSetup), String> {
    DOCUMENTS
        .lock()
        .get(&id)
        .map(|e| (e.pages.clone(), e.setup.clone()))
        .ok_or("the print document is closed".into())
}

/// The text page `page` (from 1) shows, line by line.
pub fn page_text(id: i64, page: i64) -> Result<String, String> {
    let (pages, _) = pages(id)?;
    let index = usize::try_from(page - 1)
        .ok()
        .filter(|i| *i < pages.len())
        .ok_or("no such page")?;
    let mut lines: Vec<(i64, i64, &str)> = pages[index]
        .ops
        .iter()
        .filter_map(|op| match op {
            DrawOp::Text { x, y, text, .. } => Some((*y, *x, text.as_str())),
            _ => None,
        })
        .collect();
    lines.sort();
    let mut out = String::new();
    let mut row = None;
    for (y, _, text) in lines {
        if row.is_some_and(|r| r != y) {
            out.push('\n');
        } else if row.is_some() {
            out.push(' ');
        }
        out.push_str(text);
        row = Some(y);
    }
    Ok(out)
}

pub fn close(id: i64) {
    let entry = DOCUMENTS.lock().remove(&id);
    drop(entry);
}

pub(crate) fn close_budget(budget: &Budget) {
    let closed: Vec<Entry> = {
        let mut documents = DOCUMENTS.lock();
        let ids: Vec<i64> = documents
            .iter()
            .filter(|(_, e)| e.budget == *budget)
            .map(|(id, _)| *id)
            .collect();
        ids.iter().filter_map(|id| documents.remove(id)).collect()
    };
    drop(closed);
}

/// One page of a print document, drawn at `scale` pixels per point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preview {
    pub id: String,
    pub document: i64,
    /// The page shown, from 1.
    pub page: i64,
    pub scale: f64,
}

/// The document's pages and the index of the one shown.
fn shown(p: &Preview) -> Option<(Arc<Vec<Page>>, usize)> {
    let documents = DOCUMENTS.lock();
    let entry = documents.get(&p.document)?;
    let last = entry.pages.len().checked_sub(1)?;
    let index = usize::try_from(p.page - 1).unwrap_or(0).min(last);
    Some((entry.pages.clone(), index))
}

fn scale(p: &Preview, ui: f64) -> f64 {
    p.scale.clamp(0.05, 8.0) * ui
}

pub fn layout_preview(p: &Preview, c: Constraints, cx: &mut LayoutCx) -> Node {
    let size = shown(p).map_or(Size::new(0, 0), |(pages, index)| {
        let page = &pages[index];
        let s = scale(p, cx.theme.scale);
        Size::new(
            (page.width * s).ceil() as i64,
            (page.height * s).ceil() as i64,
        )
    });
    Node::leaf(c.constrain(size))
}

pub fn paint_preview(p: &Preview, node: &Node, cx: &mut PaintCx) {
    let Some((pages, index)) = shown(p) else {
        return;
    };
    let r = Rect::from_size(node.size());
    cx.fill(r, Color::white());
    let s = scale(p, cx.theme.scale);
    let at = cx.abs(r);
    let transform = Transform {
        x: at.x as f64,
        y: at.y as f64,
        scale_x: s,
        scale_y: s,
    };
    crate::render::display::draw_transformed(&mut cx.canvas, &pages[index].ops, transform);
    let border = cx.theme.border;
    cx.stroke_round(r, 0, 1, border);
}

pub fn preview_semantics(p: &Preview, node: &Node) -> Option<AccessNode> {
    let (pages, _) = shown(p)?;
    let count = pages.len();
    let page = p.page.clamp(1, count as i64);
    let mut n = AccessNode::new(Role::Document, &format!("Page {page} of {count}"));
    n.id = p.id.clone();
    n.read_only = true;
    n.multiline = true;
    n.bounds = Rect::from_size(node.size());
    n.value = page_text(p.document, page).unwrap_or_default();
    Some(n)
}
