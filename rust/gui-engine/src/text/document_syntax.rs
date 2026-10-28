//! Native analysis retains a compact lexical checkpoint per logical line.
//! Token data is interned in a byte-bounded process cache and regenerated on
//! demand, so a large file does not retain duplicate full token vectors.
use super::{
    layout::TextLayout,
    syntax::{self, SourceLine, Span},
};
use crate::{editing::buffer::Buffer, memory::ByteCache, services::jobs::Context};
use parking_lot::Mutex;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, LazyLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
const INDEX_LIMIT: usize = 64 * 1024 * 1024;
const WORD_LIMIT: usize = 8 * 1024 * 1024;
const TOKEN_LIMIT: usize = 64 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy)]
struct Row {
    id: u64,
    outgoing: i64,
}
struct Cached {
    id: u64,
    line: SourceLine,
}
type Key = (String, i64, String);
struct Cache {
    tokens: ByteCache<Key, Arc<Cached>>,
    ids: ByteCache<u64, Weak<Cached>>,
    pending: BTreeMap<(i64, i64, String), BTreeSet<usize>>,
    failed: ByteCache<(i64, i64, usize), ()>,
}
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| {
    Mutex::new(Cache {
        tokens: ByteCache::new(TOKEN_LIMIT),
        ids: ByteCache::new(64 * 1024 * 1024),
        pending: BTreeMap::new(),
        failed: ByteCache::new(16 * 1024 * 1024),
    })
});
#[derive(Clone)]
pub struct Analysis {
    pub source: Buffer,
    pub language: String,
    pub revision: i64,
    pub scanned: i64,
    pub words: Vec<String>,
    pub truncated: bool,
    rows: Vec<Row>,
    counts: BTreeMap<String, u32>,
    word_bytes: usize,
}
impl Analysis {
    pub fn empty(language: &str) -> Self {
        Self {
            source: Buffer::default(),
            language: language.into(),
            revision: -1,
            scanned: 0,
            words: Vec::new(),
            truncated: false,
            rows: Vec::new(),
            counts: BTreeMap::new(),
            word_bytes: 0,
        }
    }
    pub fn owned_bytes(&self) -> usize {
        self.source.owned_charge()
            + self.language.capacity()
            + self.rows.capacity() * size_of::<Row>()
            + self.word_bytes
            + self.words.capacity() * size_of::<String>()
            + self.words.iter().map(String::capacity).sum::<usize>()
    }
}
fn line_text(source: &Buffer, line: usize) -> String {
    source
        .slice(source.line_start(line), source.line_start(line + 1))
        .trim_end_matches(['\n', '\r', '\u{0085}', '\u{2028}', '\u{2029}'])
        .into()
}
fn intern(
    text: String,
    language: &str,
    incoming: i64,
    context: &Context,
) -> Result<Arc<Cached>, String> {
    context.check()?;
    if text.len() > syntax::LINE_BYTE_LIMIT {
        return Err("syntax line exceeds its byte budget".into());
    }
    let key = (language.into(), incoming, text);
    if let Some(cached) = CACHE.lock().tokens.get(&key).cloned() {
        return Ok(cached);
    }
    let line = syntax::lex_line(&key.2, language, incoming);
    context.check()?;
    let bytes = line.owned_bytes() + key.0.capacity() + key.2.capacity() + size_of::<Cached>();
    if bytes > TOKEN_LIMIT - 4096 {
        return Err("syntax line tokens exceed their byte budget".into());
    }
    let mut cache = CACHE.lock();
    if let Some(cached) = cache.tokens.get(&key).cloned() {
        return Ok(cached);
    }
    let cached = Arc::new(Cached {
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        line,
    });
    if !cache.tokens.insert(key, cached.clone(), bytes) {
        return Err("syntax token exceeds its cache budget".into());
    }
    cache.ids.insert(cached.id, Arc::downgrade(&cached), 0);
    Ok(cached)
}
fn words(analysis: &mut Analysis, line: &SourceLine, add: bool) {
    for word in &line.words {
        if add {
            if let Some(count) = analysis.counts.get_mut(word) {
                *count = count.saturating_add(1);
            } else {
                let charge = word.len() * 2 + 160;
                if analysis.word_bytes + charge <= WORD_LIMIT {
                    analysis.word_bytes += charge;
                    analysis.counts.insert(word.clone(), 1);
                } else {
                    analysis.truncated = true;
                }
            }
        } else if let Some(count) = analysis.counts.get_mut(word) {
            *count -= 1;
            if *count == 0 {
                analysis.counts.remove(word);
                analysis.word_bytes -= word.len() * 2 + 160;
            }
        }
    }
}
fn suffix_line(source: &Buffer, suffix: usize) -> usize {
    let at = source.len() - suffix;
    let line = source.line_of(at);
    (line + usize::from(source.line_start(line) != at)).min(source.line_count())
}
pub fn update(
    source: Buffer,
    revision: i64,
    language: &str,
    previous: Option<&Analysis>,
    context: &Context,
    progress: &mut impl FnMut(i64, i64),
) -> Result<Analysis, String> {
    let count = source.line_count();
    if count.saturating_mul(size_of::<Row>()) > INDEX_LIMIT {
        return Err("native syntax index exceeds its byte budget".into());
    }
    let old = previous
        .filter(|old| old.language == language && old.rows.len() == old.source.line_count());
    if let Some(old) = old
        && old.source.same_revision(&source)
    {
        let mut made = old.clone();
        made.scanned = 0;
        made.revision = revision;
        return Ok(made);
    }
    let mut first = 0;
    let mut old_tail = 0;
    let mut new_tail = count;
    if let Some(old) = old {
        let mut prefix = 0;
        let mut suffix = 0;
        for (index, (a, b)) in old.source.chars().zip(source.chars()).enumerate() {
            if index % 1024 == 0 {
                context.check()?;
            }
            if a != b {
                break;
            }
            prefix += a.len_utf8();
        }
        for (index, (a, b)) in old.source.chars_rev().zip(source.chars_rev()).enumerate() {
            if index % 1024 == 0 {
                context.check()?;
            }
            if a != b || prefix + suffix + a.len_utf8() > old.source.len().min(source.len()) {
                break;
            }
            suffix += a.len_utf8();
        }
        first = source.line_of(prefix).min(old.source.line_of(prefix));
        old_tail = suffix_line(&old.source, suffix);
        new_tail = suffix_line(&source, suffix);
        let shared = (old.source.line_count() - old_tail).min(count - new_tail);
        old_tail = old.source.line_count() - shared;
        new_tail = count - shared;
    }
    let mut made = Analysis::empty(language);
    made.source = source;
    made.revision = revision;
    made.rows = Vec::with_capacity(count);
    if let Some(old) = old {
        made.counts = old.counts.clone();
        made.word_bytes = old.word_bytes;
        made.truncated = old.truncated;
        made.rows.extend_from_slice(&old.rows[..first]);
        for line in first..old_tail {
            context.check()?;
            let incoming = if line == 0 {
                0
            } else {
                old.rows[line - 1].outgoing
            };
            let cached = intern(line_text(&old.source, line), language, incoming, context)?;
            words(&mut made, &cached.line, false);
        }
    }
    let mut line = first;
    while line < count {
        context.check()?;
        let incoming = made.rows.last().map_or(0, |row| row.outgoing);
        if let Some(old) = old
            && line >= new_tail
        {
            let old_line = old_tail + line - new_tail;
            let old_incoming = if old_line == 0 {
                0
            } else {
                old.rows[old_line - 1].outgoing
            };
            if old_incoming == incoming {
                made.rows.extend_from_slice(&old.rows[old_line..]);
                break;
            }
            let cached = intern(
                line_text(&old.source, old_line),
                language,
                old_incoming,
                context,
            )?;
            words(&mut made, &cached.line, false);
        }
        let cached = intern(line_text(&made.source, line), language, incoming, context)?;
        words(&mut made, &cached.line, true);
        made.rows.push(Row {
            id: cached.id,
            outgoing: cached.line.outgoing,
        });
        made.scanned += 1;
        line += 1;
        if line % 256 == 0 {
            progress(line as i64, count as i64);
        }
    }
    made.words = made.counts.keys().cloned().collect();
    if made.owned_bytes() > 128 * 1024 * 1024 {
        return Err("native syntax analysis exceeds its byte budget".into());
    }
    context.check()?;
    progress(count as i64, count as i64);
    Ok(made)
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub document: i64,
    pub revision: i64,
    pub owner: String,
    pub lines: Vec<usize>,
}
pub(crate) fn take_requests() -> Vec<Request> {
    std::mem::take(&mut CACHE.lock().pending)
        .into_iter()
        .map(|((document, revision, owner), lines)| Request {
            document,
            revision,
            owner,
            lines: lines.into_iter().collect(),
        })
        .collect()
}
pub(crate) fn release(document: i64) {
    CACHE.lock().pending.retain(|(id, _, _), _| *id != document);
}
pub(crate) fn purge() {
    let mut cache = CACHE.lock();
    cache.tokens.clear();
    cache.ids.clear();
    cache.pending.clear();
    cache.failed.clear();
}
pub(crate) fn fill(request: &Request, context: &Context) -> Result<(), String> {
    let analysis = crate::editing::documents::paint_analysis(request.document)
        .ok_or("analysis revision is no longer current")?;
    if analysis.revision != request.revision {
        return Ok(());
    }
    for &line in &request.lines {
        context.check()?;
        let Some(row) = analysis.rows.get(line) else {
            continue;
        };
        let incoming = if line == 0 {
            0
        } else {
            analysis.rows[line - 1].outgoing
        };
        let cached = intern(
            line_text(&analysis.source, line),
            &analysis.language,
            incoming,
            context,
        )?;
        // Recreated entries retain the logical id recorded by the immutable
        // index, independently of content interning and cache eviction.
        CACHE.lock().ids.insert(row.id, Arc::downgrade(&cached), 0);
    }
    Ok(())
}
pub(crate) fn paint_spans(
    document: i64,
    owner: &str,
    analysis: &Analysis,
    view: &TextLayout,
) -> Vec<Span> {
    let mut lines = BTreeMap::<usize, Option<Arc<Cached>>>::new();
    {
        let mut cache = CACHE.lock();
        for visual in &view.lines {
            let line = analysis.source.line_of(visual.start.max(0) as usize);
            if let std::collections::btree_map::Entry::Vacant(entry) = lines.entry(line) {
                let cached = analysis
                    .rows
                    .get(line)
                    .and_then(|row| cache.ids.get(&row.id))
                    .and_then(Weak::upgrade);
                if cached.is_none()
                    && cache
                        .failed
                        .get(&(document, analysis.revision, line))
                        .is_none()
                    && cache.pending.len() < 64
                    && owner.len() <= 4096
                {
                    let requested = cache
                        .pending
                        .entry((document, analysis.revision, owner.into()))
                        .or_default();
                    if requested.len() < 512 {
                        requested.insert(line);
                    }
                }
                entry.insert(cached);
            }
        }
    }
    let mut visible = BTreeMap::new();
    for visual in &view.lines {
        let logical = analysis.source.line_of(visual.start.max(0) as usize);
        let Some(Some(cached)) = lines.get(&logical) else {
            continue;
        };
        let origin = analysis.source.line_start(logical) as i64;
        for glyph in &visual.glyphs {
            let first = cached
                .line
                .spans
                .partition_point(|span| span.end <= glyph.start - origin);
            let end = cached
                .line
                .spans
                .partition_point(|span| span.start < glyph.end - origin);
            for span in &cached.line.spans[first..end] {
                if visible.len() >= 262144 {
                    break;
                }
                visible.insert(
                    (span.start + origin, span.end + origin),
                    Span {
                        start: span.start + origin,
                        end: span.end + origin,
                        kind: span.kind,
                    },
                );
            }
        }
    }
    visible.into_values().collect()
}

pub(crate) fn failed(request: &Request) {
    let mut cache = CACHE.lock();
    for &line in &request.lines {
        cache
            .failed
            .insert((request.document, request.revision, line), (), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn compact_analysis_reuses_lines_propagates_state_and_removes_stale_words() {
        let context = Context::standalone(Duration::from_secs(20));
        let mut text =
            Buffer::from("let first = 1\n/* comment\nlet hidden = 2\n*/\nlet last = first\n");
        let initial = update(text.snapshot(), 0, "gos", None, &context, &mut |_, _| {}).unwrap();
        assert!(initial.words.contains(&"first".into()));
        assert!(!initial.words.contains(&"hidden".into()));
        text.replace(4, 9, "second");
        let next = update(
            text.snapshot(),
            1,
            "gos",
            Some(&initial),
            &context,
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(next.scanned, 1);
        assert!(next.words.contains(&"second".into()));
        assert!(next.words.contains(&"first".into()));
        let start = text.line_start(4);
        let end = text.line_start(5);
        text.replace(start, end, "let last = second\n");
        let next = update(
            text.snapshot(),
            2,
            "gos",
            Some(&next),
            &context,
            &mut |_, _| {},
        )
        .unwrap();
        assert!(!next.words.contains(&"first".into()));
        assert_eq!(next.scanned, 1);
        let start = text.line_start(1);
        text.replace(start, start + 2, "//");
        let next = update(
            text.snapshot(),
            3,
            "gos",
            Some(&next),
            &context,
            &mut |_, _| {},
        )
        .unwrap();
        assert!(next.words.contains(&"hidden".into()));
        assert!(next.scanned >= 2);
        let full = syntax::Document::new(&text.to_string(), "gos");
        for (index, line) in full.lines.iter().enumerate() {
            assert_eq!(next.rows[index].outgoing, line.outgoing);
        }
        purge();
    }
    #[test]
    fn a_ten_mebibyte_document_keeps_a_compact_index_and_incremental_last_line() {
        let context = Context::standalone(Duration::from_secs(30));
        use std::fmt::Write;
        let mut text = String::new();
        for row in 0..200_000 {
            match row % 4 {
                0 => writeln!(text, "fn α_{}() {{ let שלום = {row} }} // é 👩‍💻", row % 97),
                1 => writeln!(text, "// 日本語 تعليق строка {row} 👩‍💻 é source text"),
                2 => writeln!(
                    text,
                    "let vector_{} = #[{row}, {}, {}] // values",
                    row % 113,
                    row % 17,
                    row % 31
                ),
                _ => writeln!(
                    text,
                    "if ready_{} {{ println(\"مرحبا 世界 {row}\") }}",
                    row % 89
                ),
            }
            .unwrap();
        }
        let mut source = Buffer::from(text);
        assert!(source.len() >= 10 * 1024 * 1024);
        let initial = update(source.snapshot(), 0, "gos", None, &context, &mut |_, _| {}).unwrap();
        assert!(initial.owned_bytes() < 40 * 1024 * 1024);
        assert!(initial.words.len() < 512);
        let end = source.len();
        source.replace(end, end, "latest");
        let changed = update(
            source.snapshot(),
            1,
            "gos",
            Some(&initial),
            &context,
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(changed.scanned, 1);
        assert!(changed.words.contains(&"latest".into()));
        assert!(!changed.source.flattened());
        assert!(CACHE.lock().tokens.bytes() <= TOKEN_LIMIT);
        purge();
    }
}
