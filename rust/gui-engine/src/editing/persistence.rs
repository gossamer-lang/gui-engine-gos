//! Versioned, bounded session checkpoints. History stores replacement patches
//! rather than serializing each shared rope snapshot as a complete string.
use super::*;
use crate::{
    editing::{Delta, Snapshot, UNDO_BYTE_LIMIT, UNDO_LIMIT},
    services::jobs::Context,
};
pub const CHECKPOINT_LIMIT: usize = 128 * 1024 * 1024;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub key: String,
    pub document: i64,
    #[serde(skip)]
    guard: Option<Arc<Restored>>,
}
impl PartialEq for Record {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.document == other.document
    }
}
impl Record {
    pub(crate) fn temporary(key: impl Into<String>, document: i64) -> Self {
        Self {
            key: key.into(),
            document,
            guard: Some(Arc::new(Restored {
                documents: vec![document],
                adopted: std::sync::atomic::AtomicBool::new(false),
            })),
        }
    }
    pub fn new(key: impl Into<String>, document: i64) -> Self {
        Self {
            key: key.into(),
            document,
            guard: None,
        }
    }
}
#[derive(Debug)]
struct Restored {
    documents: Vec<i64>,
    adopted: std::sync::atomic::AtomicBool,
}
impl Drop for Restored {
    fn drop(&mut self) {
        if !self.adopted.load(Ordering::Acquire) {
            for id in &self.documents {
                close(*id);
            }
        }
    }
}
pub(crate) fn adopt(records: &[Record]) {
    for record in records {
        if let Some(guard) = &record.guard {
            guard.adopted.store(true, Ordering::Release);
        }
    }
}

struct Encoder(Vec<u8>);
impl Encoder {
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.0.len().saturating_add(bytes.len()) > CHECKPOINT_LIMIT {
            return Err("session checkpoint exceeds its byte budget".into());
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }
    fn number(&mut self, value: u64) -> Result<(), String> {
        self.bytes(&value.to_le_bytes())
    }
    fn text(&mut self, text: &str) -> Result<(), String> {
        self.number(text.len() as u64)?;
        self.bytes(text.as_bytes())
    }
    fn buffer(&mut self, buffer: &Buffer) -> Result<(), String> {
        self.number(buffer.len() as u64)?;
        for chunk in buffer.chunks() {
            self.bytes(chunk.as_bytes())?;
        }
        Ok(())
    }
}
struct Decoder<'a>(&'a [u8]);
impl<'a> Decoder<'a> {
    fn bytes(&mut self, length: usize) -> Result<&'a [u8], String> {
        let bytes = self.0.get(..length).ok_or("truncated session checkpoint")?;
        self.0 = &self.0[length..];
        Ok(bytes)
    }
    fn number(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }
    fn text(&mut self, limit: usize) -> Result<String, String> {
        let length = usize::try_from(self.number()?).map_err(|_| "checkpoint length overflow")?;
        if length > limit {
            return Err("checkpoint field exceeds its byte budget".into());
        }
        String::from_utf8(self.bytes(length)?.to_vec()).map_err(|error| error.to_string())
    }
}
struct Patch {
    start: usize,
    removed: String,
    inserted: String,
    anchor: i64,
    caret: i64,
}
fn patch(
    old: &Buffer,
    new: &Buffer,
    anchor: i64,
    caret: i64,
    context: &Context,
) -> Result<Patch, String> {
    let mut start = 0;
    let mut suffix = 0;
    if !old.same_revision(new) {
        for (a, b) in old.chars().zip(new.chars()) {
            if a != b {
                break;
            }
            start += a.len_utf8();
            if start % 65536 == 0 {
                context.check()?;
            }
        }
        for (a, b) in old.chars_rev().zip(new.chars_rev()) {
            if a != b || start + suffix + a.len_utf8() > old.len().min(new.len()) {
                break;
            }
            suffix += a.len_utf8();
            if suffix % 65536 == 0 {
                context.check()?;
            }
        }
    } else {
        start = old.len();
    }
    Ok(Patch {
        start,
        removed: old.slice(start, old.len() - suffix),
        inserted: new.slice(start, new.len() - suffix),
        anchor,
        caret,
    })
}
fn encode_patch(out: &mut Encoder, patch: &Patch) -> Result<(), String> {
    out.number(patch.start as u64)?;
    out.text(&patch.removed)?;
    out.text(&patch.inserted)?;
    out.number(patch.anchor as u64)?;
    out.number(patch.caret as u64)
}
fn decode_patch(input: &mut Decoder<'_>) -> Result<Patch, String> {
    let start = usize::try_from(input.number()?).map_err(|_| "patch offset overflow")?;
    Ok(Patch {
        start,
        removed: input.text(DOCUMENT_LIMIT)?,
        inserted: input.text(DOCUMENT_LIMIT)?,
        anchor: input.number()? as i64,
        caret: input.number()? as i64,
    })
}
pub fn save(
    path: &str,
    records: &[Record],
    metadata: &str,
    context: &Context,
    progress: &mut impl FnMut(i64, i64),
) -> Result<(), String> {
    if records.len() > 64 || metadata.len() > 1024 * 1024 {
        return Err("session checkpoint metadata exceeds its budget".into());
    }
    let mut out = Encoder(Vec::new());
    out.bytes(b"GOSDOC01")?;
    out.text(metadata)?;
    out.number(records.len() as u64)?;
    let mut keys = std::collections::HashSet::new();
    for (index, record) in records.iter().enumerate() {
        context.check()?;
        if record.key.len() > 4096 || !keys.insert(&record.key) {
            return Err("invalid checkpoint document key".into());
        }
        let (state, saved) = {
            let documents = DOCUMENTS.lock();
            let doc = documents
                .get(&record.document)
                .ok_or("checkpoint document is closed")?;
            (doc.state.clone(), doc.saved.clone())
        };
        out.text(&record.key)?;
        out.buffer(&state.text)?;
        out.buffer(&saved)?;
        out.number(state.anchor.max(0) as u64)?;
        out.number(state.caret.max(0) as u64)?;
        out.number(state.scroll_x.to_bits())?;
        out.number(state.scroll_y.to_bits())?;
        out.number(state.revision.max(0) as u64)?;
        out.number(u64::from(state.follow_caret))?;
        let mut undo = Vec::new();
        let mut bytes = 0usize;
        for position in (0..state.undo.len()).rev() {
            let old = &state.undo[position];
            let next = state
                .undo
                .get(position + 1)
                .map_or(&state.text, |snapshot| &snapshot.text);
            let patch = patch(&old.text, next, old.anchor, old.caret, context)?;
            bytes = bytes.saturating_add(patch.removed.len() + patch.inserted.len() + 128 * 1024);
            if bytes > UNDO_BYTE_LIMIT {
                break;
            }
            undo.push(patch);
        }
        undo.reverse();
        out.number(undo.len() as u64)?;
        for patch in &undo {
            encode_patch(&mut out, patch)?;
        }
        let mut redo = Vec::new();
        let mut previous = &state.text;
        for next in state.redo.iter().rev() {
            let patch = patch(previous, &next.text, next.anchor, next.caret, context)?;
            bytes = bytes.saturating_add(patch.removed.len() + patch.inserted.len() + 128 * 1024);
            if bytes > UNDO_BYTE_LIMIT {
                break;
            }
            redo.push(patch);
            previous = &next.text;
        }
        out.number(redo.len() as u64)?;
        for patch in &redo {
            encode_patch(&mut out, patch)?;
        }
        progress(index as i64 + 1, records.len() as i64);
    }
    context.check()?;
    crate::services::work::write_bytes(path, &out.0, context, progress)
}
fn apply(buffer: &mut Buffer, patch: &Patch, inverse: bool) -> Result<(), String> {
    let (remove, insert) = if inverse {
        (&patch.inserted, &patch.removed)
    } else {
        (&patch.removed, &patch.inserted)
    };
    let end = patch
        .start
        .checked_add(remove.len())
        .ok_or("patch offset overflow")?;
    if end > buffer.len()
        || buffer.slice(patch.start, end) != *remove
        || buffer.char_boundary(patch.start) != patch.start
        || buffer.char_boundary(end) != end
    {
        return Err("invalid checkpoint history patch".into());
    }
    if buffer.len() - remove.len() + insert.len() > DOCUMENT_LIMIT {
        return Err("checkpoint history exceeds document budget".into());
    }
    buffer.replace(patch.start, end, insert);
    Ok(())
}
pub fn restore(
    path: &str,
    context: &Context,
    progress: &mut impl FnMut(i64, i64),
) -> Result<(Vec<Record>, String), String> {
    let bytes =
        crate::services::bounded_io::read(std::path::Path::new(path), CHECKPOINT_LIMIT, &|| {
            context.active()
        })?;
    let mut input = Decoder(&bytes);
    if input.bytes(8)? != b"GOSDOC01" {
        return Err("unsupported session checkpoint version".into());
    }
    let metadata = input.text(1024 * 1024)?;
    let count = input.number()?;
    if count > 64 {
        return Err("checkpoint exceeds its document count budget".into());
    }
    let mut restored = scopeguard::guard(Vec::<Record>::new(), |records| {
        for record in records {
            close(record.document);
        }
    });
    let mut keys = std::collections::HashSet::new();
    for index in 0..count {
        context.check()?;
        let key = input.text(4096)?;
        if !keys.insert(key.clone()) {
            return Err("duplicate checkpoint document key".into());
        }
        let text = input.text(DOCUMENT_LIMIT)?;
        let saved = input.text(DOCUMENT_LIMIT)?;
        let anchor = input.number()? as i64;
        let caret = input.number()? as i64;
        let scroll_x = f64::from_bits(input.number()?);
        let scroll_y = f64::from_bits(input.number()?);
        let revision = input.number()? as i64;
        let follow = input.number()?;
        if follow > 1 {
            return Err("invalid checkpoint viewport policy".into());
        }
        if anchor < 0
            || caret < 0
            || anchor as usize > text.len()
            || caret as usize > text.len()
            || revision < 0
            || !scroll_x.is_finite()
            || !scroll_y.is_finite()
            || scroll_x < 0.0
            || scroll_y < 0.0
        {
            return Err("invalid checkpoint view state".into());
        }
        let mut state = EditState {
            text: Buffer::from(text),
            initialized: true,
            anchor,
            caret,
            scroll_x,
            scroll_y,
            revision,
            goal_x: -1.0,
            history_index: -1,
            follow_caret: follow == 1,
            ..EditState::default()
        };
        let mut patches = Vec::new();
        let undo = input.number()?;
        if undo > UNDO_LIMIT as u64 {
            return Err("checkpoint undo count exceeds budget".into());
        }
        for _ in 0..undo {
            patches.push(decode_patch(&mut input)?);
        }
        let mut before = state.text.snapshot();
        for patch in patches.iter().rev() {
            context.check()?;
            apply(&mut before, patch, true)?;
            let cost = (patch.removed.len() + patch.inserted.len())
                .saturating_mul(4)
                .saturating_add(128 * 1024)
                .min(before.owned_charge());
            state.undo.push(Snapshot {
                text: before.snapshot(),
                anchor: patch.anchor,
                caret: patch.caret,
                cost,
                deltas: vec![Delta {
                    start: patch.start,
                    removed: patch.removed.len(),
                    inserted: patch.inserted.len(),
                }],
            });
        }
        state.undo.reverse();
        let redo = input.number()?;
        if undo + redo > UNDO_LIMIT as u64 {
            return Err("checkpoint history count exceeds budget".into());
        }
        let mut after = state.text.snapshot();
        for _ in 0..redo {
            context.check()?;
            let patch = decode_patch(&mut input)?;
            apply(&mut after, &patch, false)?;
            let cost = (patch.removed.len() + patch.inserted.len())
                .saturating_mul(4)
                .saturating_add(128 * 1024)
                .min(after.owned_charge());
            state.redo.push(Snapshot {
                text: after.snapshot(),
                anchor: patch.anchor,
                caret: patch.caret,
                cost,
                deltas: vec![Delta {
                    start: patch.start,
                    removed: patch.removed.len(),
                    inserted: patch.inserted.len(),
                }],
            });
        }
        state.redo.reverse();
        state.trim_history();
        let saved = state
            .undo
            .iter()
            .chain(&state.redo)
            .map(|snapshot| &snapshot.text)
            .chain(std::iter::once(&state.text))
            .find(|text| *text == &saved)
            .map_or_else(|| Buffer::from(saved), Buffer::snapshot);
        let document = create("", context.budget.clone())?;
        restored.push(Record::new(key, document));
        state.document = document;
        state.layout_key = (document as u64).wrapping_mul(2);
        commit(document, 0, state)?;
        mark_saved(document, saved)?;
        progress(index as i64 + 1, count as i64);
    }
    context.check()?;
    if !input.0.is_empty() {
        return Err("trailing checkpoint data".into());
    }
    let mut records = scopeguard::ScopeGuard::into_inner(restored);
    let guard = Arc::new(Restored {
        documents: records.iter().map(|record| record.document).collect(),
        adopted: std::sync::atomic::AtomicBool::new(false),
    });
    for record in &mut records {
        record.guard = Some(guard.clone());
    }
    Ok((records, metadata))
}
