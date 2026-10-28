//! Session-owned rich documents, immutable snapshots and bounded image assets.
use super::{
    markup::{self, Format},
    *,
};
use crate::memory::{Budget, ByteCache, Lease};
use parking_lot::Mutex;
use std::{collections::HashMap, sync::LazyLock};
const HANDLE_LIMIT: usize = 1024;
const ASSET_LIMIT: usize = 64 * 1024 * 1024;
static NEXT: AtomicI64 = AtomicI64::new(1);
static DOCUMENTS: LazyLock<Mutex<HashMap<i64, Entry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
struct Entry {
    document: Document,
    budget: Budget,
    lease: Lease,
    origin: i64,
    readonly: bool,
    assets: ByteCache<String, Arc<crate::render::png::Image>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub marks: Marks,
    pub revision: i64,
    pub dirty: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub readonly: bool,
    pub blocks: usize,
    pub selection: Selection,
}
fn info_of(entry: &Entry) -> Info {
    Info {
        marks: entry.document.current_marks(),
        revision: entry.document.revision,
        dirty: entry.document.dirty(),
        can_undo: !entry.document.undo.is_empty(),
        can_redo: !entry.document.redo.is_empty(),
        readonly: entry.readonly,
        blocks: entry.document.content.blocks.len(),
        selection: entry.document.selection.clone(),
    }
}
fn add(
    document: Document,
    budget: Budget,
    origin: Option<i64>,
    readonly: bool,
) -> Result<i64, String> {
    let lease = budget.reserve(document.owned_bytes() + 512)?;
    let mut documents = DOCUMENTS.lock();
    if documents.len() >= HANDLE_LIMIT {
        return Err("rich document handle registry is full".into());
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    documents.insert(
        id,
        Entry {
            document,
            budget,
            lease,
            origin: origin.unwrap_or(id),
            readonly,
            assets: ByteCache::new(ASSET_LIMIT),
        },
    );
    Ok(id)
}
pub fn create(content: Content, budget: Budget) -> Result<i64, String> {
    add(Document::new(content)?, budget, None, false)
}
pub fn import(
    format: Format,
    source: &str,
    budget: Budget,
    active: impl Fn() -> bool,
) -> Result<i64, String> {
    if source.len() > SOURCE_LIMIT {
        return Err("rich import exceeds its source budget".into());
    }
    let _scratch = budget.reserve(
        source
            .len()
            .saturating_mul(16)
            .saturating_add(32 * 1024 * 1024),
    )?;
    let imported = markup::import(format, source, &active)?;
    if !active() {
        return Err("rich import cancelled".into());
    }
    create(imported.content, budget)
}
pub fn info(id: i64) -> Result<Info, String> {
    DOCUMENTS
        .lock()
        .get(&id)
        .map(info_of)
        .ok_or("rich document is closed".into())
}
pub(crate) fn view(id: i64) -> Result<(Snapshot, Info), String> {
    DOCUMENTS
        .lock()
        .get(&id)
        .map(|entry| (entry.document.freeze(), info_of(entry)))
        .ok_or("rich document is closed".into())
}
pub fn snapshot(id: i64) -> Result<Snapshot, String> {
    DOCUMENTS
        .lock()
        .get(&id)
        .map(|e| e.document.freeze())
        .ok_or("rich document is closed".into())
}
pub fn freeze(id: i64) -> Result<i64, String> {
    let (document, budget, origin) = {
        let documents = DOCUMENTS.lock();
        let source = documents.get(&id).ok_or("rich document is closed")?;
        let frozen = source.document.freeze();
        let document = Document {
            typing_marks: frozen.typing_marks,
            content: frozen.content.clone(),
            saved: frozen.content,
            selection: frozen.selection,
            revision: source.document.revision,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
        };
        (document, source.budget.clone(), source.origin)
    };
    add(document, budget, Some(origin), true)
}
pub fn close(id: i64) {
    DOCUMENTS.lock().remove(&id);
}
pub(crate) fn close_budget(budget: &Budget) {
    DOCUMENTS.lock().retain(|_, e| &e.budget != budget);
}
pub fn select(id: i64, selection: Selection) -> Result<(), String> {
    let mut documents = DOCUMENTS.lock();
    let entry = documents.get_mut(&id).ok_or("rich document is closed")?;
    entry.document.select(selection)
}
pub fn mark_saved(id: i64, snapshot: i64) -> Result<(), String> {
    let mut documents = DOCUMENTS.lock();
    let frozen = documents.get(&snapshot).ok_or("rich snapshot is closed")?;
    if !frozen.readonly {
        return Err("save marker needs an immutable rich snapshot".into());
    }
    let origin = frozen.origin;
    let frozen = frozen.document.freeze();
    let target = documents.get_mut(&id).ok_or("rich document is closed")?;
    if target.readonly || target.origin != origin {
        return Err("save marker belongs to another rich document".into());
    }
    let mut candidate = target.document.clone();
    candidate.mark_saved(&frozen);
    target
        .lease
        .resize(candidate.owned_bytes() + target.assets.bytes() + 512)?;
    target.document = candidate;
    Ok(())
}
fn change(
    id: i64,
    expected: i64,
    active: impl Fn() -> bool,
    edit: impl FnOnce(&mut Document) -> Result<(), String>,
) -> Result<Info, String> {
    let (mut candidate, budget) = {
        let documents = DOCUMENTS.lock();
        let source = documents.get(&id).ok_or("rich document is closed")?;
        if source.readonly {
            return Err("rich snapshot is immutable".into());
        }
        if source.document.revision != expected {
            return Err("stale rich transaction".into());
        }
        (source.document.clone(), source.budget.clone())
    };
    let _scratch = budget.reserve(
        candidate
            .content
            .owned_bytes()
            .saturating_mul(2)
            .saturating_add(512 * 1024),
    )?;
    edit(&mut candidate)?;
    let mut documents = DOCUMENTS.lock();
    let target = documents.get_mut(&id).ok_or("rich document is closed")?;
    if target.document.revision != expected {
        return Err("stale rich transaction".into());
    }
    if !active() {
        return Err("rich transaction cancelled".into());
    }
    target
        .lease
        .resize(candidate.owned_bytes() + target.assets.bytes() + 512)?;
    target.document = candidate;
    Ok(info_of(target))
}
pub fn transact(
    id: i64,
    expected: i64,
    edits: &[Edit],
    active: impl Fn() -> bool,
) -> Result<Info, String> {
    change(id, expected, &active, |doc| {
        doc.transact(expected, edits, &active).map(|_| ())
    })
}
pub fn undo(id: i64, expected: i64) -> Result<Info, String> {
    change(
        id,
        expected,
        || true,
        |doc| {
            doc.undo();
            Ok(())
        },
    )
}
pub fn redo(id: i64, expected: i64) -> Result<Info, String> {
    change(
        id,
        expected,
        || true,
        |doc| {
            doc.redo();
            Ok(())
        },
    )
}
pub fn export(
    id: i64,
    expected: i64,
    format: Format,
    active: impl Fn() -> bool,
) -> Result<String, String> {
    let (content, budget) = {
        let documents = DOCUMENTS.lock();
        let source = documents.get(&id).ok_or("rich document is closed")?;
        if source.document.revision != expected {
            return Err("stale rich export".into());
        }
        (source.document.content.clone(), source.budget.clone())
    };
    let _scratch = budget.reserve(
        content
            .owned_bytes()
            .saturating_mul(4)
            .saturating_add(16 * 1024 * 1024),
    )?;
    markup::export(format, &content, active)
}
pub fn set_image(
    id: i64,
    source: &str,
    png: &[u8],
    active: impl Fn() -> bool,
) -> Result<(), String> {
    if !safe_url(source) || png.len() > 32 * 1024 * 1024 {
        return Err("invalid or oversized rich image input".into());
    }
    let budget = DOCUMENTS
        .lock()
        .get(&id)
        .ok_or("rich document is closed")?
        .budget
        .clone();
    let _scratch = budget.reserve(64 * 1024 * 1024)?;
    if !active() {
        return Err("rich image cancelled".into());
    }
    let image = crate::render::png::decode_with_limit(png, 16 * 1024 * 1024)
        .ok_or("rich image decode failed or exceeds 16 MiB")?;
    if !active() {
        return Err("rich image cancelled".into());
    }
    let mut documents = DOCUMENTS.lock();
    let target = documents.get_mut(&id).ok_or("rich document is closed")?;
    let bytes = image.rgba.capacity() + source.len() + 512;
    let total = (target.assets.bytes() + bytes + 4096).min(ASSET_LIMIT);
    target
        .lease
        .resize(target.document.owned_bytes() + total + 512)?;
    target.assets.insert(source.into(), Arc::new(image), bytes);
    target
        .lease
        .resize(target.document.owned_bytes() + target.assets.bytes() + 512)?;
    Ok(())
}
pub fn image(id: i64, source: &str) -> Option<Arc<crate::render::png::Image>> {
    DOCUMENTS
        .lock()
        .get_mut(&id)?
        .assets
        .get(&source.to_owned())
        .cloned()
}
/// Result ownership survives queue clones. A cancelled or undelivered result
/// closes its native handle; actual shell delivery adopts the session-owned id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub document: i64,
    #[serde(skip)]
    guard: Option<Arc<Unclaimed>>,
}
impl PartialEq for Record {
    fn eq(&self, other: &Self) -> bool {
        self.document == other.document
    }
}
#[derive(Debug)]
struct Unclaimed {
    id: i64,
    adopted: std::sync::atomic::AtomicBool,
}
impl Drop for Unclaimed {
    fn drop(&mut self) {
        if !self.adopted.load(Ordering::Acquire) {
            close(self.id);
        }
    }
}
impl Record {
    pub(crate) fn pending(document: i64) -> Self {
        Self {
            document,
            guard: Some(Arc::new(Unclaimed {
                id: document,
                adopted: std::sync::atomic::AtomicBool::new(false),
            })),
        }
    }
    pub fn adopt(&self) {
        if let Some(guard) = &self.guard {
            guard.adopted.store(true, Ordering::Release);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handles_charge_session_budgets_snapshots_are_immutable_and_disposal_releases_everything() {
        let budget = Budget::new(256 * 1024 * 1024);
        let id = import(
            Format::Markdown,
            "# Title\n\nUnicode שלום **bold**",
            budget.clone(),
            || true,
        )
        .unwrap();
        assert!(budget.usage().0 > 0);
        let saved = freeze(id).unwrap();
        let block = snapshot(id).unwrap().content.blocks[0].id;
        assert!(transact(saved, 0, &[Edit::Remove { block }], || true).is_err());
        transact(
            id,
            0,
            &[Edit::Replace {
                block,
                start: 0,
                end: 0,
                text: "New ".into(),
                marks: None,
            }],
            || true,
        )
        .unwrap();
        mark_saved(id, saved).unwrap();
        assert!(info(id).unwrap().dirty);
        undo(id, 1).unwrap();
        assert!(!info(id).unwrap().dirty);
        close(saved);
        close(id);
        assert_eq!(budget.usage().0, 0);
    }
    #[test]
    fn dropped_worker_results_close_native_documents_and_low_memory_rejection_rolls_back() {
        let budget = Budget::new(256 * 1024 * 1024);
        let id = create(Content::empty(), budget.clone()).unwrap();
        let record = Record::pending(id);
        let clone = record.clone();
        drop(record);
        assert!(info(id).is_ok());
        drop(clone);
        assert!(info(id).is_err());
        assert_eq!(budget.usage().0, 0);
        let id = create(Content::empty(), budget.clone()).unwrap();
        budget.set_limit(budget.usage().0).unwrap();
        let block = snapshot(id).unwrap().content.blocks[0].id;
        assert!(
            transact(
                id,
                0,
                &[Edit::Replace {
                    block,
                    start: 0,
                    end: 0,
                    text: "x".into(),
                    marks: None
                }],
                || true
            )
            .is_err()
        );
        assert_eq!(info(id).unwrap().revision, 0);
        close(id);
        assert_eq!(budget.usage().0, 0);
    }
}
