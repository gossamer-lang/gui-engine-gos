//! Native documents keep persistent snapshots inside the toolkit so bindings
//! exchange handles and revisions instead of copying an entire file per edit.
#[path = "persistence.rs"]
pub mod persistence;
use super::{EditState, buffer::Buffer};
use crate::memory::{Budget, Lease};
use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicI64, Ordering},
    },
};
const DOCUMENT_LIMIT: usize = 16 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Affinity {
    Before,
    After,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Anchor {
    pub byte: usize,
    pub affinity: Affinity,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Info {
    pub revision: i64,
    pub bytes: usize,
    pub lines: usize,
    pub dirty: bool,
    pub anchor: i64,
    pub caret: i64,
}
struct Document {
    state: EditState,
    saved: Buffer,
    budget: Budget,
    lease: Lease,
    anchors: BTreeMap<i64, Anchor>,
    next_anchor: i64,
    analysis: Arc<crate::text::document_syntax::Analysis>,
    analysis_lease: Lease,
    analysis_revision: i64,
}
static NEXT: AtomicI64 = AtomicI64::new(1);
static DOCUMENTS: LazyLock<Mutex<BTreeMap<i64, Document>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
fn charge(state: &EditState) -> usize {
    state
        .text
        .owned_charge()
        .saturating_add(state.history_bytes())
        .saturating_add(state.changes.capacity() * std::mem::size_of::<super::Delta>())
        .saturating_add(state.external.len())
        .saturating_add(state.preedit.capacity())
        .saturating_add(16384)
}
fn info_of(doc: &Document) -> Info {
    Info {
        revision: doc.state.revision,
        bytes: doc.state.text.len(),
        lines: doc.state.text.line_count(),
        dirty: !doc.state.text.same_revision(&doc.saved),
        anchor: doc.state.anchor,
        caret: doc.state.caret,
    }
}
pub fn create(text: &str, budget: Budget) -> Result<i64, String> {
    if text.len() > DOCUMENT_LIMIT {
        return Err("document exceeds its byte budget".into());
    }
    let mut lease = budget.reserve(text.len().saturating_mul(6).saturating_add(32768))?;
    let mut state = EditState::default();
    state.sync(text);
    // A document opens at its start, unlike a field's value, which a caret
    // at the end continues.
    state.anchor = 0;
    state.caret = 0;
    state.external = Arc::from("");
    lease.resize(charge(&state).saturating_add(state.text.owned_charge()))?;
    let mut documents = DOCUMENTS.lock();
    if documents.len() >= 1024 {
        return Err("document registry exceeds its handle budget".into());
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    state.document = id;
    state.layout_key = (id as u64).wrapping_mul(2);
    documents.insert(
        id,
        Document {
            saved: state.text.snapshot(),
            state,
            budget: budget.clone(),
            lease,
            anchors: BTreeMap::new(),
            next_anchor: 1,
            analysis: Arc::new(crate::text::document_syntax::Analysis::empty("")),
            analysis_lease: budget.reserve(0)?,
            analysis_revision: -1,
        },
    );
    Ok(id)
}
pub fn info(id: i64) -> Result<Info, String> {
    DOCUMENTS
        .lock()
        .get(&id)
        .map(info_of)
        .ok_or_else(|| "document is closed".into())
}
pub fn state(id: i64) -> Result<EditState, String> {
    DOCUMENTS
        .lock()
        .get(&id)
        .map(|doc| doc.state.clone())
        .ok_or_else(|| "document is closed".into())
}
pub fn snapshot(id: i64) -> Result<(Buffer, i64), String> {
    DOCUMENTS
        .lock()
        .get(&id)
        .map(|doc| (doc.state.text.snapshot(), doc.state.revision))
        .ok_or_else(|| "document is closed".into())
}
pub fn read(id: i64, start: usize, end: usize) -> Result<String, String> {
    let (text, _) = snapshot(id)?;
    Ok(text.slice(start, end))
}
pub fn close(id: i64) {
    DOCUMENTS.lock().remove(&id);
    crate::text::document_layout::release((id as u64).wrapping_mul(2));
    crate::text::document_syntax::release(id);
}
pub(crate) fn close_budget(budget: &Budget) {
    DOCUMENTS.lock().retain(|_, doc| &doc.budget != budget);
}
// Ordered transactions cover replace-all, typing groups and their undo/redo.
// Prepare their position map before taking the registry lock. Each live anchor
// then needs one binary search instead of a pass over every replacement.
struct AnchorStep {
    start: usize,
    end: usize,
    new_start: usize,
    new_end: usize,
    component_start: usize,
    component_end: usize,
}
struct AnchorTransform {
    steps: Vec<AnchorStep>,
    descending: bool,
    removed: usize,
    inserted: usize,
}
impl AnchorTransform {
    fn new(changes: &[super::Delta]) -> Option<Self> {
        if changes.len() < 2 {
            return None;
        }
        let descending = changes.windows(2).all(|pair| {
            pair[1]
                .start
                .checked_add(pair[1].removed)
                .is_some_and(|end| pair[0].start >= end)
        });
        if !descending
            && !changes.windows(2).all(|pair| {
                pair[0]
                    .start
                    .checked_add(pair[0].inserted)
                    .is_some_and(|end| pair[1].start >= end)
            })
        {
            return None;
        }
        let mut steps: Vec<AnchorStep> = Vec::with_capacity(changes.len());
        let (mut removed, mut inserted) = (0usize, 0usize);
        for index in 0..changes.len() {
            let change = &changes[if descending {
                changes.len() - 1 - index
            } else {
                index
            }];
            let start = if descending {
                change.start
            } else {
                change.start.checked_add(removed)?.checked_sub(inserted)?
            };
            let end = start.checked_add(change.removed)?;
            if let Some(previous) = steps.last()
                && start < previous.end
            {
                return None;
            }
            let new_start = start.checked_sub(removed)?.checked_add(inserted)?;
            let component_start = steps
                .last()
                .filter(|previous| previous.end == start)
                .map_or(index, |previous| previous.component_start);
            steps.push(AnchorStep {
                start,
                end,
                new_start,
                new_end: new_start.checked_add(change.inserted)?,
                component_start,
                component_end: index,
            });
            removed = removed.checked_add(change.removed)?;
            inserted = inserted.checked_add(change.inserted)?;
        }
        for index in (0..steps.len() - 1).rev() {
            if steps[index].end == steps[index + 1].start {
                steps[index].component_end = steps[index + 1].component_end;
            }
        }
        Some(Self {
            steps,
            descending,
            removed,
            inserted,
        })
    }
    fn map(&self, byte: usize, affinity: Affinity) -> usize {
        if self.descending {
            let upper = self.steps.partition_point(|step| step.start <= byte);
            if upper == 0 {
                return byte;
            }
            let step = &self.steps[upper - 1];
            if byte > step.end {
                return step.new_end + byte - step.end;
            }
            match affinity {
                Affinity::Before => self.steps[step.component_start].new_start,
                Affinity::After => step.new_end,
            }
        } else {
            let index = self.steps.partition_point(|step| step.end < byte);
            if index == self.steps.len() {
                return byte - self.removed + self.inserted;
            }
            let step = &self.steps[index];
            if byte < step.start {
                return step.new_start - (step.start - byte);
            }
            match affinity {
                Affinity::Before => step.new_start,
                Affinity::After => self.steps[step.component_end].new_end,
            }
        }
    }
}

pub fn commit(id: i64, expected: i64, state: EditState) -> Result<Info, String> {
    commit_checked(id, expected, state, || true)
}
fn commit_checked(
    id: i64,
    expected: i64,
    state: EditState,
    active: impl Fn() -> bool,
) -> Result<Info, String> {
    if state.text.len() > DOCUMENT_LIMIT {
        return Err("document exceeds its byte budget".into());
    }
    let _mapping_lease = if state.revision != expected && state.changes.len() > 1 {
        let budget = DOCUMENTS
            .lock()
            .get(&id)
            .ok_or("document is closed")?
            .budget
            .clone();
        Some(
            budget.reserve(
                state
                    .changes
                    .len()
                    .saturating_mul(std::mem::size_of::<AnchorStep>())
                    .saturating_add(128),
            )?,
        )
    } else {
        None
    };
    let mapping = (state.revision != expected)
        .then(|| AnchorTransform::new(&state.changes))
        .flatten();
    let mut documents = DOCUMENTS.lock();
    let doc = documents.get_mut(&id).ok_or("document is closed")?;
    if doc.state.revision != expected {
        return Err("stale document transaction".into());
    }
    if !active() {
        return Err("replacement cancelled or expired".into());
    }
    doc.lease.resize(
        charge(&state)
            .saturating_add(doc.saved.owned_charge())
            .saturating_add(doc.anchors.len() * 128),
    )?;
    if state.revision != expected {
        for anchor in doc.anchors.values_mut() {
            if let Some(mapping) = &mapping {
                anchor.byte = mapping.map(anchor.byte, anchor.affinity);
                continue;
            }
            for delta in &state.changes {
                let end = delta.start.saturating_add(delta.removed);
                if anchor.byte > end {
                    anchor.byte = anchor.byte - delta.removed + delta.inserted;
                } else if anchor.byte >= delta.start {
                    anchor.byte = delta.start
                        + if anchor.affinity == Affinity::After {
                            delta.inserted
                        } else {
                            0
                        };
                }
            }
        }
    }
    doc.state = state;
    Ok(info_of(doc))
}
/// Applies an atomic revision-checked byte-range transaction. Anchors inside
/// replacement collapse to its boundary according to their insertion affinity.
pub fn replace(
    id: i64,
    expected: i64,
    start: usize,
    end: usize,
    text: &str,
) -> Result<Info, String> {
    let budget = DOCUMENTS
        .lock()
        .get(&id)
        .ok_or("document is closed")?
        .budget
        .clone();
    let _temporary = budget.reserve(text.len().saturating_mul(4).saturating_add(128 * 1024))?;
    let mut state = state(id)?;
    state.changes.clear();
    if state.revision != expected {
        return Err("stale document transaction".into());
    }
    let start = state.text.snap(start.min(state.text.len()));
    let end = state.text.snap(end.max(start).min(state.text.len()));
    if state
        .text
        .len()
        .saturating_sub(end - start)
        .saturating_add(text.len())
        > DOCUMENT_LIMIT
    {
        return Err("document exceeds its byte budget".into());
    }
    state.anchor = start as i64;
    state.caret = end as i64;
    state.replace_selection(text, super::EditKind::Other, 0, 0);
    commit(id, expected, state)
}

pub fn anchor(id: i64, byte: usize, affinity: Affinity) -> Result<i64, String> {
    let mut documents = DOCUMENTS.lock();
    let doc = documents.get_mut(&id).ok_or("document is closed")?;
    if doc.anchors.len() >= 4096 {
        return Err("document anchor budget exhausted".into());
    }
    doc.lease.resize(
        charge(&doc.state)
            .saturating_add(doc.saved.owned_charge())
            .saturating_add((doc.anchors.len() + 1) * 128),
    )?;
    let key = doc.next_anchor;
    doc.next_anchor += 1;
    doc.anchors.insert(
        key,
        Anchor {
            byte: doc.state.text.snap(byte),
            affinity,
        },
    );
    Ok(key)
}
pub fn resolve(id: i64, anchor: i64) -> Result<Anchor, String> {
    DOCUMENTS
        .lock()
        .get(&id)
        .and_then(|doc| doc.anchors.get(&anchor))
        .cloned()
        .ok_or_else(|| "anchor is closed".into())
}
pub fn release_anchor(id: i64, anchor: i64) {
    if let Some(doc) = DOCUMENTS.lock().get_mut(&id) {
        doc.anchors.remove(&anchor);
    }
}
pub fn mark_saved(id: i64, snapshot: Buffer) -> Result<(), String> {
    let mut documents = DOCUMENTS.lock();
    let doc = documents.get_mut(&id).ok_or("document is closed")?;
    doc.lease.resize(
        charge(&doc.state)
            .saturating_add(snapshot.owned_charge())
            .saturating_add(doc.anchors.len() * 128),
    )?;
    doc.saved = snapshot;
    Ok(())
}
pub fn freeze(id: i64) -> Result<i64, String> {
    let mut documents = DOCUMENTS.lock();
    let doc = documents.get(&id).ok_or("document is closed")?;
    let mut state = EditState::default();
    state.text = doc.state.text.snapshot();
    state.initialized = true;
    state.revision = doc.state.revision;
    let budget = doc.budget.clone();
    let saved = state.text.snapshot();
    let lease = budget.reserve(charge(&state).saturating_add(saved.owned_charge()))?;
    if documents.len() >= 1024 {
        return Err("document registry exceeds its handle budget".into());
    }
    let key = NEXT.fetch_add(1, Ordering::Relaxed);
    documents.insert(
        key,
        Document {
            state,
            saved,
            budget: budget.clone(),
            lease,
            anchors: BTreeMap::new(),
            next_anchor: 1,
            analysis: Arc::new(crate::text::document_syntax::Analysis::empty("")),
            analysis_lease: budget.reserve(0)?,
            analysis_revision: -1,
        },
    );
    Ok(key)
}
/// Shares the immutable native snapshot for a concurrent reader.
pub fn reader(id: i64) -> Result<Arc<Buffer>, String> {
    Ok(Arc::new(snapshot(id)?.0))
}

pub fn analysis(id: i64) -> Option<Arc<crate::text::document_syntax::Analysis>> {
    DOCUMENTS.lock().get(&id).map(|doc| doc.analysis.clone())
}
pub fn set_analysis(
    id: i64,
    revision: i64,
    analysis: crate::text::document_syntax::Analysis,
) -> Result<(), String> {
    let mut documents = DOCUMENTS.lock();
    let doc = documents.get_mut(&id).ok_or("document is closed")?;
    if doc.state.revision != revision {
        return Err("stale document analysis".into());
    }
    doc.analysis_lease.resize(analysis.owned_bytes())?;
    doc.analysis = Arc::new(analysis);
    doc.analysis_revision = revision;
    Ok(())
}

pub fn paint_analysis(id: i64) -> Option<Arc<crate::text::document_syntax::Analysis>> {
    let documents = DOCUMENTS.lock();
    let doc = documents.get(&id)?;
    (doc.analysis_revision == doc.state.revision).then(|| doc.analysis.clone())
}

pub fn replace_many(
    id: i64,
    expected: i64,
    ranges: &[crate::text::offsets::ByteRange],
    replacement: &str,
    active: impl Fn() -> bool,
) -> Result<Info, String> {
    let mut state = state(id)?;
    if state.revision != expected {
        return Err("stale document replacement".into());
    }
    let mut previous = 0usize;
    let mut removed = 0usize;
    for range in ranges {
        if !active() {
            return Err("replacement cancelled or expired".into());
        }
        let start = usize::try_from(range.start.0).map_err(|_| "invalid replacement range")?;
        let end = usize::try_from(range.end.0).map_err(|_| "invalid replacement range")?;
        if start < previous
            || end < start
            || end > state.text.len()
            || state.text.snap(start) != start
            || state.text.snap(end) != end
        {
            return Err("replacement ranges must be ordered grapheme ranges".into());
        }
        removed = removed.saturating_add(end - start);
        previous = end;
    }
    let inserted = ranges
        .len()
        .checked_mul(replacement.len())
        .ok_or("replacement exceeds byte budget")?;
    if state
        .text
        .len()
        .saturating_sub(removed)
        .saturating_add(inserted)
        > DOCUMENT_LIMIT
    {
        return Err("replacement exceeds document byte budget".into());
    }
    if ranges.is_empty() {
        return info(id);
    }
    let budget = DOCUMENTS
        .lock()
        .get(&id)
        .ok_or("document is closed")?
        .budget
        .clone();
    let _temporary = budget.reserve(
        state
            .text
            .len()
            .saturating_add(inserted)
            .saturating_mul(2)
            .saturating_add(ranges.len().saturating_mul(32))
            .saturating_add(128 * 1024),
    )?;
    let cost = removed
        .saturating_add(inserted)
        .saturating_mul(4)
        .saturating_add(ranges.len().saturating_mul(128 * 1024));
    state.record(super::EditKind::Other, 0, cost);
    state.changes.clear();
    for range in ranges.iter().rev() {
        if !active() {
            return Err("replacement cancelled or expired".into());
        }
        state
            .text
            .replace(range.start.0 as usize, range.end.0 as usize, replacement);
        state.changes.push(super::Delta {
            start: range.start.0 as usize,
            removed: (range.end.0 - range.start.0) as usize,
            inserted: replacement.len(),
        });
    }
    if let Some(snapshot) = state.undo.last_mut() {
        snapshot.deltas = state.changes.clone();
    }
    state.trim_history();
    state.caret = state.text.byte_len();
    state.anchor = state.caret;
    state.revision += 1;
    if !active() {
        return Err("replacement cancelled or expired".into());
    }
    commit_checked(id, expected, state, active)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn sequential_anchor(
        mut byte: usize,
        affinity: Affinity,
        changes: &[super::super::Delta],
    ) -> usize {
        for change in changes {
            let end = change.start + change.removed;
            if byte > end {
                byte = byte - change.removed + change.inserted;
            } else if byte >= change.start {
                byte = change.start
                    + if affinity == Affinity::After {
                        change.inserted
                    } else {
                        0
                    };
            }
        }
        byte
    }
    #[test]
    fn a_created_document_opens_with_the_caret_at_its_start() {
        let id = create("first line\nsecond line\n", Budget::new(1 << 20)).unwrap();
        let opened = info(id).unwrap();
        assert_eq!((opened.anchor, opened.caret), (0, 0));
        assert!(!opened.dirty);
        close(id);
    }
    #[test]
    fn prepared_anchor_maps_preserve_adjacent_and_zero_length_boundary_semantics() {
        use super::super::Delta;
        for a_start in 0..4 {
            for a_removed in 0..3 {
                for gap in 0..3 {
                    for b_removed in 0..3 {
                        for a_inserted in 0..3 {
                            for b_inserted in 0..3 {
                                let b_start = a_start + a_removed + gap;
                                let descending = vec![
                                    Delta {
                                        start: b_start,
                                        removed: b_removed,
                                        inserted: b_inserted,
                                    },
                                    Delta {
                                        start: a_start,
                                        removed: a_removed,
                                        inserted: a_inserted,
                                    },
                                ];
                                let ascending = vec![
                                    descending[1].clone(),
                                    Delta {
                                        start: a_start + a_inserted + gap,
                                        removed: b_removed,
                                        inserted: b_inserted,
                                    },
                                ];
                                for changes in [descending, ascending] {
                                    let map = AnchorTransform::new(&changes).unwrap();
                                    for byte in 0..12 {
                                        for affinity in [Affinity::Before, Affinity::After] {
                                            assert_eq!(
                                                map.map(byte, affinity),
                                                sequential_anchor(byte, affinity, &changes),
                                                "byte={byte}, affinity={affinity:?}, changes={changes:?}"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn cancellation_at_the_commit_boundary_leaves_text_and_anchors_unchanged() {
        use crate::text::offsets::{ByteOffset, ByteRange};
        let id = create("a a", Budget::new(16 * 1024 * 1024)).unwrap();
        let _cleanup = scopeguard::guard((), |_| close(id));
        let marker = anchor(id, 2, Affinity::After).unwrap();
        let ranges = vec![
            ByteRange {
                start: ByteOffset(0),
                end: ByteOffset(1),
            },
            ByteRange {
                start: ByteOffset(2),
                end: ByteOffset(3),
            },
        ];
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let result = replace_many(id, 0, &ranges, "bbb", || {
            calls.fetch_add(1, Ordering::Relaxed) < 2 * ranges.len() + 1
        });
        assert!(result.unwrap_err().contains("cancelled"));
        assert_eq!(read(id, 0, 3).unwrap(), "a a");
        assert_eq!(info(id).unwrap().revision, 0);
        assert_eq!(resolve(id, marker).unwrap().byte, 2);
    }

    #[test]
    fn bulk_replacement_maps_thousands_of_anchors_and_accounts_delta_storage() {
        use crate::text::offsets::{ByteOffset, ByteRange};
        let budget = Budget::new(128 * 1024 * 1024);
        let id = create(&"a ".repeat(100_000), budget.clone()).unwrap();
        let _cleanup = scopeguard::guard((), |_| close(id));
        let mut anchors = Vec::new();
        for index in 0..2048 {
            let word = index * 37;
            anchors.push((anchor(id, word * 2, Affinity::Before).unwrap(), word * 3));
            anchors.push((anchor(id, word * 2, Affinity::After).unwrap(), word * 3 + 2));
        }
        let ranges: Vec<_> = (0..100_000)
            .map(|word| ByteRange {
                start: ByteOffset(word * 2),
                end: ByteOffset(word * 2 + 1),
            })
            .collect();
        replace_many(id, 0, &ranges, "bb", || true).unwrap();
        for (anchor, expected) in anchors {
            assert_eq!(resolve(id, anchor).unwrap().byte, expected);
        }
        let state = state(id).unwrap();
        assert!(!state.undo.is_empty());
        assert!(state.history_bytes() >= 100_000 * std::mem::size_of::<super::super::Delta>());
        assert!(state.history_bytes() <= super::super::UNDO_BYTE_LIMIT);
        assert!(
            charge(&state)
                >= state.history_bytes()
                    + state.changes.capacity() * std::mem::size_of::<super::super::Delta>()
        );
    }

    #[test]
    fn anchors_affinity_stale_transactions_save_markers_and_undo_are_consistent() {
        let budget = Budget::new(128 * 1024 * 1024);
        let id = create("a👩\u{200d}💻b", budget.clone()).unwrap();
        let before = anchor(id, 1, Affinity::Before).unwrap();
        let after = anchor(id, 1, Affinity::After).unwrap();
        let old = reader(id).unwrap();
        replace(id, 0, 1, 1, "界").unwrap();
        assert_eq!(resolve(id, before).unwrap().byte, 1);
        assert_eq!(resolve(id, after).unwrap().byte, 4);
        assert!(replace(id, 0, 0, 1, "stale").is_err());
        assert_eq!(old.to_string(), "a👩\u{200d}💻b");
        let saved = snapshot(id).unwrap().0;
        mark_saved(id, saved).unwrap();
        assert!(!info(id).unwrap().dirty);
        replace(id, 1, 0, 1, "A").unwrap();
        assert!(info(id).unwrap().dirty);
        let mut current = state(id).unwrap();
        assert!(current.undo());
        commit(id, 2, current).unwrap();
        assert!(!info(id).unwrap().dirty);
        close(id);
        drop(old);
        assert_eq!(budget.usage().0, 0);
    }
    #[test]
    fn session_checkpoints_restore_dirty_text_history_views_and_release_rejected_results() {
        let budget = Budget::new(512 * 1024 * 1024);
        crate::app::effects::set_budget(budget.clone());
        let context = crate::services::jobs::Context::standalone(Duration::from_secs(10));
        let id = create("first\nשלום", budget.clone()).unwrap();
        replace(id, 0, 0, 5, "second").unwrap();
        let mut edited = state(id).unwrap();
        edited.scroll_y = 18.0;
        edited.scroll_x = 12.0;
        commit(id, 1, edited).unwrap();
        let path = std::env::temp_dir().join(format!("gos-session-{}.bin", std::process::id()));
        let _cleanup = scopeguard::guard((), |_| {
            let _ = std::fs::remove_file(&path);
        });
        persistence::save(
            path.to_str().unwrap(),
            &[persistence::Record::new("doc1", id)],
            "{\"window\":\"main\"}",
            &context,
            &mut |_, _| {},
        )
        .unwrap();
        let (records, meta) =
            persistence::restore(path.to_str().unwrap(), &context, &mut |_, _| {}).unwrap();
        assert_eq!(meta, "{\"window\":\"main\"}");
        let restored = records[0].document;
        let mut view = state(restored).unwrap();
        assert_eq!(view.text, "second\nשלום");
        assert_eq!((view.scroll_x, view.scroll_y), (12.0, 18.0));
        assert!(info(restored).unwrap().dirty);
        assert!(view.undo());
        assert_eq!(view.text, "first\nשלום");
        commit(restored, 1, view).unwrap();
        assert!(!info(restored).unwrap().dirty);
        drop(records);
        assert!(info(restored).is_err());
        let before = budget.usage().0;
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
        assert!(persistence::restore(path.to_str().unwrap(), &context, &mut |_, _| {}).is_err());
        assert_eq!(budget.usage().0, before);
        close(id);
        assert_eq!(budget.usage().0, 0);
        crate::app::effects::set_budget(Budget::new(crate::app::effects::EFFECT_BYTE_LIMIT));
    }
}
