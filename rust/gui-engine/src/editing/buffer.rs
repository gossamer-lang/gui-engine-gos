//! Persistent UTF-8 rope. A clone shares immutable tree nodes; editing copies
//! only the affected leaf and its path. Line and scalar indexes live in the tree.
use ropey::Rope;
use serde::{Deserialize, Serialize, Serializer};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(1);
#[derive(Debug)]
struct Data {
    rope: Rope,
    flat: OnceLock<String>,
    identity: u64,
    max_line: usize,
}
#[derive(Debug, Clone)]
pub struct Buffer(Arc<Data>);
impl Default for Buffer {
    fn default() -> Self {
        Self::from("")
    }
}
impl From<&str> for Buffer {
    fn from(text: &str) -> Self {
        Self::of(Rope::from_str(text))
    }
}
impl From<String> for Buffer {
    fn from(text: String) -> Self {
        Self::from(text.as_str())
    }
}
impl Buffer {
    fn of(rope: Rope) -> Self {
        Self(Arc::new(Data {
            max_line: rope.lines().map(|line| line.len_bytes()).max().unwrap_or(0),
            rope,
            flat: OnceLock::new(),
            identity: NEXT.fetch_add(1, Ordering::Relaxed),
        }))
    }
    pub fn chunks(&self) -> ropey::iter::Chunks<'_> {
        self.0.rope.chunks()
    }
    pub fn chars(&self) -> ropey::iter::Chars<'_> {
        self.0.rope.chars()
    }
    pub fn chars_rev(&self) -> ropey::iter::Chars<'_> {
        self.0.rope.chars_at(self.0.rope.len_chars()).reversed()
    }
    pub fn len(&self) -> usize {
        self.0.rope.len_bytes()
    }
    pub fn byte_len(&self) -> i64 {
        self.len() as i64
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn max_line_bytes(&self) -> usize {
        self.0.max_line
    }
    pub fn identity(&self) -> u64 {
        self.0.identity
    }
    pub fn line_count(&self) -> usize {
        self.0.rope.len_lines()
    }
    pub fn line_of(&self, byte: usize) -> usize {
        self.0.rope.byte_to_line(byte.min(self.len()))
    }
    pub fn line_start(&self, line: usize) -> usize {
        self.0.rope.line_to_byte(line.min(self.line_count()))
    }
    pub fn scalar_of(&self, byte: usize) -> usize {
        self.0.rope.byte_to_char(byte.min(self.len()))
    }
    pub fn byte_of_scalar(&self, scalar: usize) -> usize {
        self.0
            .rope
            .char_to_byte(scalar.min(self.0.rope.len_chars()))
    }
    pub fn char_boundary(&self, byte: usize) -> usize {
        self.byte_of_scalar(self.scalar_of(byte))
    }
    pub fn slice(&self, start: usize, end: usize) -> String {
        let start = self.char_boundary(start.min(self.len()));
        let end = self.char_boundary(end.max(start).min(self.len()));
        self.0.rope.byte_slice(start..end).to_string()
    }
    pub fn replace(&mut self, start: usize, end: usize, text: &str) {
        let start = self.scalar_of(start);
        let end = self.scalar_of(end.max(self.byte_of_scalar(start)));
        let mut rope = self.0.rope.clone();
        rope.remove(start..end);
        rope.insert(start, text);
        let first = rope.char_to_line(start);
        let last = rope.char_to_line(start + text.chars().count());
        let max_line = self
            .0
            .max_line
            .max(text.split('\n').map(str::len).max().unwrap_or(0))
            .max(rope.line(first).len_bytes())
            .max(rope.line(last).len_bytes());
        *self = Self(Arc::new(Data {
            rope,
            flat: OnceLock::new(),
            identity: NEXT.fetch_add(1, Ordering::Relaxed),
            max_line,
        }));
    }
    /// A snapshot without retaining a compatibility flattened string.
    pub fn snapshot(&self) -> Self {
        Self(Arc::new(Data {
            rope: self.0.rope.clone(),
            flat: OnceLock::new(),
            identity: self.0.identity,
            max_line: self.0.max_line,
        }))
    }
    pub fn owned_charge(&self) -> usize {
        self.len()
            .saturating_mul(2)
            .saturating_add(4096)
            .saturating_add(self.0.flat.get().map_or(0, String::capacity))
    }
    pub fn capacity(&self) -> usize {
        self.owned_charge()
    }
    pub fn flattened(&self) -> bool {
        self.0.flat.get().is_some()
    }
    pub fn same_revision(&self, other: &Self) -> bool {
        self.0.identity == other.0.identity
    }
    /// Finds grapheme boundaries in a bounded neighborhood using the rope's
    /// leaf chunks. Unicode's cursor requests more context when a cluster spans leaves.
    pub fn boundary(&self, byte: usize, direction: i8) -> usize {
        use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};
        let at = self.char_boundary(byte.min(self.len()));
        let mut cursor = GraphemeCursor::new(at, self.len(), true);
        let mut offset = if direction < 0 {
            at.saturating_sub(1)
        } else {
            at.min(self.len().saturating_sub(1))
        };
        if self.is_empty() {
            return 0;
        }
        loop {
            let (chunk, start, _, _) = self.0.rope.chunk_at_byte(offset);
            let result = if direction < 0 {
                cursor.prev_boundary(chunk, start)
            } else {
                cursor.next_boundary(chunk, start)
            };
            match result {
                Ok(Some(found)) => return found,
                Ok(None) => return if direction < 0 { 0 } else { self.len() },
                Err(GraphemeIncomplete::PrevChunk) => offset = start.saturating_sub(1),
                Err(GraphemeIncomplete::NextChunk) => {
                    offset = (start + chunk.len()).min(self.len())
                }
                Err(GraphemeIncomplete::PreContext(end)) => {
                    let (context, start, _, _) = self.0.rope.chunk_at_byte(end.saturating_sub(1));
                    cursor.provide_context(&context[..end - start], start);
                }
                Err(GraphemeIncomplete::InvalidOffset) => return at,
            }
        }
    }
    pub fn word_target(&self, byte: usize, direction: i8) -> usize {
        let mut at = self.char_boundary(byte.min(self.len()));
        let mut chars = self.0.rope.chars_at(self.scalar_of(at));
        let mut found = false;
        loop {
            let next = if direction < 0 {
                chars.prev()
            } else {
                chars.next()
            };
            let Some(scalar) = next else {
                return at;
            };
            let class = super::word_class(&scalar.to_string());
            let word = class == 0 || class == 3 || crate::unicode::is_mark(scalar);
            if found && !word {
                return at;
            }
            if direction < 0 {
                at -= scalar.len_utf8();
            } else {
                at += scalar.len_utf8();
            }
            if word {
                if class == 3 {
                    return at;
                }
                found = true;
            }
        }
    }
    pub fn word_range(&self, byte: usize) -> (usize, usize) {
        let byte = self.char_boundary(byte.min(self.len()));
        if byte == self.len() {
            return (byte, byte);
        }
        let scalar = self.0.rope.char(self.scalar_of(byte));
        let class = super::word_class(&scalar.to_string());
        if class == 3 {
            return (byte, byte + scalar.len_utf8());
        }
        let mut start = byte;
        let mut end = byte;
        let mut before = self.0.rope.chars_at(self.scalar_of(byte));
        while let Some(scalar) = before.prev() {
            if super::word_class(&scalar.to_string()) != class && !crate::unicode::is_mark(scalar) {
                break;
            }
            start -= scalar.len_utf8();
        }
        for scalar in self.0.rope.chars_at(self.scalar_of(byte)) {
            if super::word_class(&scalar.to_string()) != class && !crate::unicode::is_mark(scalar) {
                break;
            }
            end += scalar.len_utf8();
        }
        (self.snap(start), self.snap(end))
    }
    pub fn snap(&self, byte: usize) -> usize {
        let at = self.char_boundary(byte.min(self.len()));
        if at == 0 {
            0
        } else {
            let before = self.boundary(at, -1);
            let after = self.boundary(before, 1);
            if after == at { at } else { before }
        }
    }
}
impl std::ops::Deref for Buffer {
    type Target = str;
    fn deref(&self) -> &str {
        self.0.flat.get_or_init(|| self.0.rope.to_string())
    }
}
impl std::fmt::Display for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for chunk in self.0.rope.chunks() {
            f.write_str(chunk)?;
        }
        Ok(())
    }
}
impl PartialEq for Buffer {
    fn eq(&self, other: &Self) -> bool {
        self.same_revision(other) || self.0.rope == other.0.rope
    }
}
impl Eq for Buffer {}
impl PartialEq<str> for Buffer {
    fn eq(&self, text: &str) -> bool {
        self.0.rope == text
    }
}
impl PartialEq<&str> for Buffer {
    fn eq(&self, text: &&str) -> bool {
        self == *text
    }
}
impl PartialEq<String> for Buffer {
    fn eq(&self, text: &String) -> bool {
        self == text.as_str()
    }
}
impl PartialEq<Buffer> for String {
    fn eq(&self, text: &Buffer) -> bool {
        text == self
    }
}
impl Serialize for Buffer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if crate::memory::measuring() {
            use serde::ser::SerializeTupleStruct;
            serializer
                .serialize_tuple_struct("__gui_owned_bytes", self.owned_charge())?
                .end()
        } else {
            serializer.collect_str(self)
        }
    }
}
impl<'de> Deserialize<'de> for Buffer {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serialization_streams_unicode_without_retaining_a_flattened_copy() {
        let text = "Unicode שלום 👩‍💻\n".repeat(10_000);
        let buffer = Buffer::from(text.as_str());
        assert!(!buffer.flattened());
        let encoded = serde_json::to_string(&buffer).unwrap();
        assert_eq!(serde_json::from_str::<String>(&encoded).unwrap(), text);
        assert!(!buffer.flattened());
    }
    #[test]
    fn immutable_snapshots_survive_unicode_transactions_and_concurrent_readers() {
        let source = "a👩\u{200d}💻e\u{301} שלום\n".repeat(4000);
        let mut buffer = Buffer::from(source.as_str());
        let snapshot = buffer.snapshot();
        let reader = std::thread::spawn(move || {
            assert_eq!(snapshot.to_string(), source);
            assert!(!snapshot.flattened());
        });
        for index in 0..200 {
            let at = buffer.snap((index * 97) % buffer.len());
            let end = buffer.boundary(at, 1);
            buffer.replace(at, end, "界\n");
            assert_eq!(buffer.char_boundary(at), at);
            assert!(!buffer.flattened());
        }
        reader.join().unwrap();
        assert!(buffer.line_count() > 4000);
    }
    #[test]
    fn grapheme_cursor_crosses_rope_leaves_without_splitting_clusters() {
        let source = format!(
            "{}👩\u{200d}💻e\u{301}{}",
            "x".repeat(1020),
            "z".repeat(1020)
        );
        let buffer = Buffer::from(source.as_str());
        let at = 1020;
        let next = buffer.boundary(at, 1);
        assert_eq!(buffer.slice(at, next), "👩\u{200d}💻");
        for inside in at + 1..next {
            assert_eq!(buffer.snap(inside), at);
        }
        assert_eq!(buffer.boundary(next, -1), at);
        assert_eq!(buffer.slice(next, buffer.boundary(next, 1)), "e\u{301}");
        assert!(!buffer.flattened());
    }
}
