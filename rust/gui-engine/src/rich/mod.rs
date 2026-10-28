//! Persistent structured documents. Block identities survive transactions;
//! positions are UTF-8 bytes at grapheme boundaries in textual leaf blocks.
pub mod api;
pub mod editor;
pub mod layout_cache;
pub mod markup;
pub mod preview;
pub mod registry;
use crate::editing::buffer::Buffer;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};
pub const SOURCE_LIMIT: usize = 8 * 1024 * 1024;
pub const PARAGRAPH_LIMIT: usize = 256 * 1024;
pub const NODE_LIMIT: usize = 10_000;
pub const SPAN_LIMIT: usize = 65_536;
pub const DEPTH_LIMIT: usize = 32;
pub const HISTORY_LIMIT: usize = 64 * 1024 * 1024;
static NEXT_BLOCK: AtomicI64 = AtomicI64::new(1);
fn next_id() -> i64 {
    NEXT_BLOCK.fetch_add(1, Ordering::Relaxed)
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Marks {
    pub strong: bool,
    pub emphasis: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    pub link: String,
    pub title: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Image {
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
    pub source: String,
    pub alt: String,
    pub title: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub marks: Marks,
    pub image: Option<Image>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Kind {
    Paragraph,
    Heading(i64),
    Code(String),
    Quote,
    List(Option<i64>),
    Item,
    Rule,
}
impl Kind {
    pub fn container(&self) -> bool {
        matches!(self, Self::Quote | Self::List(_) | Self::Item)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub id: i64,
    pub kind: Kind,
    pub text: Buffer,
    pub spans: Vec<Span>,
    pub children: Vec<Arc<Block>>,
}
impl Block {
    pub fn new(kind: Kind) -> Self {
        Self {
            id: next_id(),
            kind,
            text: Buffer::default(),
            spans: Vec::new(),
            children: Vec::new(),
        }
    }
    pub fn paragraph(text: &str) -> Self {
        let mut block = Self::new(Kind::Paragraph);
        block.append(text, &Marks::default(), None);
        block
    }
    pub fn append(&mut self, text: &str, marks: &Marks, image: Option<Image>) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.replace(start, start, text);
        if let Some(last) = self.spans.last_mut()
            && last.marks == *marks
            && last.image.is_none()
            && image.is_none()
        {
            last.end = self.text.len();
            return;
        }
        self.spans.push(Span {
            start,
            end: self.text.len(),
            marks: marks.clone(),
            image,
        });
    }
    pub fn owned_bytes(&self) -> usize {
        self.text.capacity()
            + self.spans.capacity() * size_of::<Span>()
            + self.children.capacity() * size_of::<Arc<Block>>()
            + 256
            + self
                .spans
                .iter()
                .map(|s| {
                    s.marks.link.capacity()
                        + s.marks.title.capacity()
                        + s.image.as_ref().map_or(0, |i| {
                            i.source.capacity() + i.alt.capacity() + i.title.capacity()
                        })
                })
                .sum::<usize>()
            + self.children.iter().map(|b| b.owned_bytes()).sum::<usize>()
    }
    fn sliced(&self, start: usize, end: usize) -> Self {
        let mut out = self.clone();
        out.text = Buffer::from(self.text.slice(start, end));
        out.spans.clear();
        for span in &self.spans {
            let a = span.start.max(start);
            let b = span.end.min(end);
            if a < b {
                out.spans.push(Span {
                    start: a - start,
                    end: b - start,
                    ..span.clone()
                });
            }
        }
        out
    }
    fn replace_text(
        &mut self,
        start: usize,
        end: usize,
        insert: &str,
        marks: Option<Marks>,
    ) -> Result<(), String> {
        check_range(&self.text, start, end)?;
        if self.kind.container() || self.kind == Kind::Rule {
            return Err("block does not accept text".into());
        }
        if self.text.len() - (end - start) + insert.len() > SOURCE_LIMIT {
            return Err("rich text exceeds source budget".into());
        }
        let inherited = marks.unwrap_or_else(|| {
            self.spans
                .iter()
                .find(|span| span.start < start && start <= span.end)
                .or_else(|| self.spans.first())
                .map_or_else(Marks::default, |s| s.marks.clone())
        });
        let prefix = self.sliced(0, start);
        let suffix = self.sliced(end, self.text.len());
        self.text = prefix.text;
        self.spans = prefix.spans;
        self.append(insert, &inherited, None);
        let base = self.text.len();
        self.text.replace(base, base, &suffix.text.to_string());
        for span in suffix.spans {
            self.spans.push(Span {
                start: span.start + base,
                end: span.end + base,
                ..span
            });
        }
        let mut covered = 0;
        let mut normalized = Vec::new();
        for span in self.spans.drain(..) {
            let start = span.start.max(covered);
            if start >= span.end {
                continue;
            }
            let end = if boundary(&self.text, span.end) {
                span.end
            } else {
                self.text.boundary(span.end.saturating_sub(1), 1)
            };
            covered = end;
            normalized.push(Span { start, end, ..span });
        }
        self.spans = normalized;
        self.normalize();
        Ok(())
    }
    pub fn normalize(&mut self) {
        let mut result: Vec<Span> = Vec::new();
        for span in self.spans.drain(..) {
            if span.start == span.end {
                continue;
            }
            if let Some(last) = result.last_mut()
                && last.end == span.start
                && last.marks == span.marks
                && last.image.is_none()
                && span.image.is_none()
            {
                last.end = span.end;
            } else {
                result.push(span)
            }
        }
        self.spans = result;
    }
    fn format(
        &mut self,
        start: usize,
        end: usize,
        mark: Mark,
        enabled: bool,
    ) -> Result<(), String> {
        check_range(&self.text, start, end)?;
        let mut result = Vec::new();
        for span in &self.spans {
            let a = span.start.max(start);
            let b = span.end.min(end);
            if a >= b {
                result.push(span.clone());
                continue;
            }
            if span.start < a {
                result.push(Span {
                    end: a,
                    ..span.clone()
                });
            }
            let mut middle = Span {
                start: a,
                end: b,
                ..span.clone()
            };
            match mark {
                Mark::Strong => middle.marks.strong = enabled,
                Mark::Emphasis => middle.marks.emphasis = enabled,
                Mark::Underline => middle.marks.underline = enabled,
                Mark::Strike => middle.marks.strike = enabled,
                Mark::Code => middle.marks.code = enabled,
            }
            result.push(middle);
            if b < span.end {
                result.push(Span {
                    start: b,
                    ..span.clone()
                });
            }
        }
        self.spans = result;
        self.normalize();
        Ok(())
    }
}
fn boundary(text: &Buffer, byte: usize) -> bool {
    byte <= text.len() && (byte == 0 || text.boundary(byte - 1, 1) == byte)
}
fn check_range(text: &Buffer, start: usize, end: usize) -> Result<(), String> {
    if start > end || !boundary(text, start) || !boundary(text, end) {
        Err("range is not on UTF-8 grapheme boundaries".into())
    } else {
        Ok(())
    }
}
pub fn safe_url(value: &str) -> bool {
    if value.len() > 4096 || value.chars().any(|c| c.is_control()) || value.trim() != value {
        return false;
    }
    if let Some((scheme, _)) = value.split_once(':') {
        matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto" | "file" | "asset"
        )
    } else {
        true
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Content {
    pub blocks: Vec<Arc<Block>>,
}
impl Content {
    pub fn empty() -> Self {
        Self {
            blocks: vec![Arc::new(Block::paragraph(""))],
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        fn visit(
            block: &Block,
            depth: usize,
            ids: &mut HashSet<i64>,
            counts: &mut [usize; 3],
        ) -> Result<(), String> {
            counts[0] += 1;
            counts[1] = counts[1].saturating_add(block.text.len()).saturating_add(
                block
                    .spans
                    .iter()
                    .map(|span| {
                        span.marks.link.len()
                            + span.marks.title.len()
                            + span.image.as_ref().map_or(0, |image| {
                                image.source.len() + image.alt.len() + image.title.len()
                            })
                    })
                    .sum::<usize>(),
            );
            if let Kind::Code(language) = &block.kind
                && (language.len() > 256 || language.chars().any(|c| c.is_control()))
            {
                return Err("invalid code language".into());
            }
            if matches!(block.kind, Kind::Code(_))
                && block
                    .spans
                    .iter()
                    .any(|span| span.marks != Marks::default() || span.image.is_some())
            {
                return Err("code blocks require plain inline content".into());
            }
            if block.kind == Kind::Rule && (!block.text.is_empty() || !block.spans.is_empty()) {
                return Err("rule cannot contain text".into());
            }
            counts[2] = counts[2].saturating_add(block.spans.len());
            if depth > DEPTH_LIMIT
                || counts[0] > NODE_LIMIT
                || counts[1] > SOURCE_LIMIT
                || counts[2] > SPAN_LIMIT
            {
                return Err("rich document exceeds structure/source limits".into());
            }
            if block.text.len() > PARAGRAPH_LIMIT {
                return Err("rich paragraph exceeds 256 KiB".into());
            }
            if block.id <= 0 || !ids.insert(block.id) {
                return Err("rich block identities must be unique and positive".into());
            }
            if matches!(block.kind,Kind::Heading(level) if !(1..=6).contains(&level)) {
                return Err("invalid heading level".into());
            }
            if matches!(block.kind,Kind::List(Some(start)) if !(1..=1_000_000_000).contains(&start))
            {
                return Err("invalid list ordinal".into());
            }
            if block.kind.container() {
                if !block.text.is_empty() || !block.spans.is_empty() {
                    return Err("container has inline content".into());
                }
                if let Kind::List(_) = block.kind
                    && block.children.iter().any(|child| child.kind != Kind::Item)
                {
                    return Err("list children must be items".into());
                }
            } else if !block.children.is_empty() {
                return Err("leaf block has children".into());
            }
            let mut offset = 0;
            for span in &block.spans {
                if span.start != offset
                    || span.start >= span.end
                    || !boundary(&block.text, span.start)
                    || !boundary(&block.text, span.end)
                {
                    return Err("inline spans must cover text on grapheme boundaries".into());
                }
                if !safe_url(&span.marks.link) || span.marks.title.len() > 4096 {
                    return Err("unsafe or oversized link".into());
                }
                if let Some(image) = &span.image
                    && (!safe_url(&image.source)
                        || !(0..=4096).contains(&image.width)
                        || !(0..=4096).contains(&image.height)
                        || image.alt.len() > 64 * 1024
                        || image.title.len() > 4096
                        || block.text.slice(span.start, span.end) != "\u{fffc}")
                {
                    return Err("invalid image object".into());
                }
                offset = span.end;
            }
            if offset != block.text.len() {
                return Err("inline spans do not cover text".into());
            }
            for child in &block.children {
                visit(child, depth + 1, ids, counts)?;
            }
            Ok(())
        }
        let mut ids = HashSet::new();
        let mut counts = [0; 3];
        if self.blocks.is_empty() {
            return Err("rich document needs a block".into());
        }
        for block in &self.blocks {
            visit(block, 1, &mut ids, &mut counts)?;
        }
        Ok(())
    }
    pub fn owned_bytes(&self) -> usize {
        self.blocks.capacity() * size_of::<Arc<Block>>()
            + self.blocks.iter().map(|b| b.owned_bytes()).sum::<usize>()
            + 128
    }
    pub fn shared_block(&self, id: i64) -> Option<Arc<Block>> {
        fn find(blocks: &[Arc<Block>], id: i64) -> Option<Arc<Block>> {
            for block in blocks {
                if block.id == id {
                    return Some(block.clone());
                }
                if let Some(found) = find(&block.children, id) {
                    return Some(found);
                }
            }
            None
        }
        find(&self.blocks, id)
    }
    pub fn find(&self, id: i64) -> Option<&Block> {
        fn find(blocks: &[Arc<Block>], id: i64) -> Option<&Block> {
            for block in blocks {
                if block.id == id {
                    return Some(block);
                }
                if let Some(found) = find(&block.children, id) {
                    return Some(found);
                }
            }
            None
        }
        find(&self.blocks, id)
    }
    pub fn leaves(&self) -> Vec<&Block> {
        fn visit<'a>(blocks: &'a [Arc<Block>], out: &mut Vec<&'a Block>) {
            for block in blocks {
                if block.kind.container() {
                    visit(&block.children, out)
                } else {
                    out.push(block)
                }
            }
        }
        let mut out = Vec::new();
        visit(&self.blocks, &mut out);
        out
    }
    pub fn plain_text(&self) -> String {
        let mut output = String::new();
        for (index, block) in self.leaves().into_iter().enumerate() {
            if index > 0 {
                output.push('\n')
            }
            for span in &block.spans {
                if let Some(image) = &span.image {
                    output.push_str(&image.alt)
                } else {
                    output.push_str(&block.text.slice(span.start, span.end))
                }
            }
        }
        output
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Position {
    pub block: i64,
    pub byte: usize,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Selection {
    pub anchor: Position,
    pub caret: Position,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Mark {
    Strong,
    Emphasis,
    Underline,
    Strike,
    Code,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Edit {
    Replace {
        block: i64,
        start: usize,
        end: usize,
        text: String,
        marks: Option<Marks>,
    },
    SetMark {
        block: i64,
        start: usize,
        end: usize,
        mark: Mark,
        enabled: bool,
    },
    Link {
        block: i64,
        start: usize,
        end: usize,
        url: String,
        title: String,
    },
    SetSpans {
        block: i64,
        spans: Vec<Span>,
    },
    SetKind {
        block: i64,
        kind: Kind,
    },
    Insert {
        parent: i64,
        index: usize,
        block: Block,
    },
    Remove {
        block: i64,
    },
    Move {
        block: i64,
        parent: i64,
        index: usize,
    },
    Split {
        block: i64,
        at: usize,
    },
    Join {
        first: i64,
        second: i64,
    },
}
fn modify(
    blocks: &mut [Arc<Block>],
    id: i64,
    action: &mut impl FnMut(&mut Block) -> Result<(), String>,
) -> Result<bool, String> {
    for block in blocks {
        if block.id == id {
            action(Arc::make_mut(block))?;
            return Ok(true);
        }
        if block.kind.container() && modify(&mut Arc::make_mut(block).children, id, action)? {
            return Ok(true);
        }
    }
    Ok(false)
}
fn remove(blocks: &mut Vec<Arc<Block>>, id: i64) -> Option<Arc<Block>> {
    if let Some(index) = blocks.iter().position(|b| b.id == id) {
        return Some(blocks.remove(index));
    }
    for block in blocks {
        if let Some(found) = remove(&mut Arc::make_mut(block).children, id) {
            return Some(found);
        }
    }
    None
}
fn insert(
    content: &mut Content,
    parent: i64,
    index: usize,
    block: Arc<Block>,
) -> Result<(), String> {
    if parent == 0 {
        if index > content.blocks.len() {
            return Err("block index out of range".into());
        }
        content.blocks.insert(index, block);
        return Ok(());
    }
    if !modify(&mut content.blocks, parent, &mut |parent| {
        if !parent.kind.container() {
            return Err("parent is not a container".into());
        }
        if index > parent.children.len() {
            return Err("block index out of range".into());
        }
        parent.children.insert(index, block.clone());
        Ok(())
    })? {
        return Err("parent block does not exist".into());
    }
    Ok(())
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub typing_marks: Option<Marks>,
    pub content: Arc<Content>,
    pub selection: Selection,
}
#[derive(Clone, Debug)]
pub struct Document {
    pub typing_marks: Option<Marks>,
    pub content: Arc<Content>,
    pub selection: Selection,
    pub revision: i64,
    saved: Arc<Content>,
    undo: VecDeque<Snapshot>,
    redo: VecDeque<Snapshot>,
}
impl Document {
    pub fn new(content: Content) -> Result<Self, String> {
        content.validate()?;
        let first = content.leaves().first().map_or(0, |b| b.id);
        let content = Arc::new(content);
        let p = Position {
            block: first,
            byte: 0,
        };
        Ok(Self {
            typing_marks: None,
            content: content.clone(),
            saved: content,
            selection: Selection {
                anchor: p.clone(),
                caret: p,
            },
            revision: 0,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
        })
    }
    pub fn current_marks(&self) -> Marks {
        if let Some(marks) = &self.typing_marks {
            return marks.clone();
        }
        let position = &self.selection.caret;
        self.content
            .find(position.block)
            .and_then(|block| {
                block
                    .spans
                    .iter()
                    .find(|span| span.start < position.byte && position.byte <= span.end)
                    .or_else(|| block.spans.first())
            })
            .map_or_else(Marks::default, |span| span.marks.clone())
    }
    pub fn dirty(&self) -> bool {
        !Arc::ptr_eq(&self.saved, &self.content)
    }
    pub fn freeze(&self) -> Snapshot {
        Snapshot {
            typing_marks: self.typing_marks.clone(),
            content: self.content.clone(),
            selection: self.selection.clone(),
        }
    }
    pub fn mark_saved(&mut self, snapshot: &Snapshot) {
        self.saved = snapshot.content.clone()
    }
    pub fn history_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(&self.redo)
            .map(|s| {
                s.content.owned_bytes()
                    + s.typing_marks
                        .as_ref()
                        .map_or(0, |marks| marks.link.capacity() + marks.title.capacity())
                    + size_of::<Snapshot>()
            })
            .sum()
    }
    pub fn owned_bytes(&self) -> usize {
        self.content.owned_bytes()
            + self.saved.owned_bytes()
            + self.history_bytes()
            + self
                .typing_marks
                .as_ref()
                .map_or(0, |marks| marks.link.capacity() + marks.title.capacity())
            + 512
    }
    pub fn select(&mut self, selection: Selection) -> Result<(), String> {
        for p in [&selection.anchor, &selection.caret] {
            let block = self
                .content
                .find(p.block)
                .ok_or("selection block does not exist")?;
            if block.kind.container() || !boundary(&block.text, p.byte) {
                return Err("invalid rich selection".into());
            }
        }
        if self.selection != selection {
            self.typing_marks = None;
        }
        self.selection = selection;
        Ok(())
    }
    pub fn transact(
        &mut self,
        expected: i64,
        edits: &[Edit],
        active: impl Fn() -> bool,
    ) -> Result<i64, String> {
        if expected != self.revision {
            return Err("stale rich transaction".into());
        }
        if edits.len() > 1024 {
            return Err("too many rich edits".into());
        }
        if edits.is_empty() {
            return Ok(self.revision);
        }
        let mut candidate = (*self.content).clone();
        let mut selection = self.selection.clone();
        let mut typing_marks = self.typing_marks.clone();
        for edit in edits {
            if !active() {
                return Err("rich transaction cancelled".into());
            }
            match edit {
                Edit::Insert {
                    parent,
                    index,
                    block,
                } => {
                    let mut block = block.clone();
                    if block.id == 0 {
                        block.id = next_id()
                    }
                    insert(&mut candidate, *parent, *index, Arc::new(block))?;
                }
                Edit::Remove { block } => {
                    remove(&mut candidate.blocks, *block).ok_or("block does not exist")?;
                }
                Edit::Move {
                    block,
                    parent,
                    index,
                } => {
                    let block =
                        remove(&mut candidate.blocks, *block).ok_or("block does not exist")?;
                    insert(&mut candidate, *parent, *index, block)?;
                }
                Edit::Join { first, second } => {
                    if first == second {
                        return Err("cannot join a block to itself".into());
                    }
                    let base = candidate
                        .find(*first)
                        .ok_or("joined block does not exist")?
                        .text
                        .len();
                    for position in [&mut selection.anchor, &mut selection.caret] {
                        if position.block == *second {
                            position.block = *first;
                            position.byte += base;
                        }
                    }
                    let right = candidate
                        .find(*second)
                        .ok_or("joined block does not exist")?
                        .clone();
                    if right.kind.container() || right.kind == Kind::Rule {
                        return Err("cannot join container/rule".into());
                    }
                    if !modify(&mut candidate.blocks, *first, &mut |left| {
                        if left.kind.container() || left.kind == Kind::Rule {
                            return Err("cannot join container/rule".into());
                        }
                        let base = left.text.len();
                        left.text.replace(base, base, &right.text.to_string());
                        left.spans.extend(right.spans.iter().map(|s| Span {
                            start: s.start + base,
                            end: s.end + base,
                            ..s.clone()
                        }));
                        left.normalize();
                        Ok(())
                    })? {
                        return Err("joined block does not exist".into());
                    }
                    remove(&mut candidate.blocks, *second).ok_or("joined block does not exist")?;
                }
                Edit::Split { block, at } => {
                    let right_id = next_id();
                    for position in [&mut selection.anchor, &mut selection.caret] {
                        if position.block == *block && position.byte > *at {
                            position.block = right_id;
                            position.byte -= *at;
                        }
                    }
                    fn split(
                        blocks: &mut Vec<Arc<Block>>,
                        id: i64,
                        at: usize,
                        right_id: i64,
                    ) -> Result<bool, String> {
                        for index in 0..blocks.len() {
                            if blocks[index].id == id {
                                let original = &blocks[index];
                                if original.kind.container() || original.kind == Kind::Rule {
                                    return Err("cannot split container/rule".into());
                                }
                                check_range(&original.text, at, at)?;
                                let left = original.sliced(0, at);
                                let mut right = original.sliced(at, original.text.len());
                                right.id = right_id;
                                if matches!(right.kind, Kind::Heading(_)) {
                                    right.kind = Kind::Paragraph
                                }
                                blocks[index] = Arc::new(left);
                                blocks.insert(index + 1, Arc::new(right));
                                return Ok(true);
                            }
                            if split(
                                &mut Arc::make_mut(&mut blocks[index]).children,
                                id,
                                at,
                                right_id,
                            )? {
                                return Ok(true);
                            }
                        }
                        Ok(false)
                    }
                    if !split(&mut candidate.blocks, *block, *at, right_id)? {
                        return Err("split block does not exist".into());
                    }
                }
                _ => {
                    if let Edit::Replace {
                        block,
                        start,
                        end,
                        text,
                        ..
                    } = edit
                    {
                        for position in [&mut selection.anchor, &mut selection.caret] {
                            if position.block == *block {
                                if position.byte > *end {
                                    position.byte = position.byte - (*end - *start) + text.len();
                                } else if position.byte >= *start {
                                    position.byte = *start + text.len();
                                }
                            }
                        }
                    }
                    let id = match edit {
                        Edit::Replace { block, .. }
                        | Edit::SetMark { block, .. }
                        | Edit::Link { block, .. }
                        | Edit::SetKind { block, .. }
                        | Edit::SetSpans { block, .. } => *block,
                        _ => unreachable!(),
                    };
                    if !modify(&mut candidate.blocks, id, &mut |block| match edit {
                        Edit::Replace {
                            start,
                            end,
                            text,
                            marks,
                            ..
                        } => block.replace_text(
                            *start,
                            *end,
                            text,
                            marks.clone().or_else(|| {
                                (self.selection.anchor.block == id
                                    && self.selection.caret.block == id
                                    && *start
                                        == self
                                            .selection
                                            .anchor
                                            .byte
                                            .min(self.selection.caret.byte)
                                    && *end
                                        == self
                                            .selection
                                            .anchor
                                            .byte
                                            .max(self.selection.caret.byte))
                                .then(|| typing_marks.clone())
                                .flatten()
                            }),
                        ),
                        Edit::SetMark {
                            start,
                            end,
                            mark,
                            enabled,
                            ..
                        } => {
                            if matches!(block.kind, Kind::Code(_)) {
                                return Err("code blocks do not accept inline formatting".into());
                            }
                            if start == end {
                                check_range(&block.text, *start, *end)?;
                                let mut marks = typing_marks.clone().unwrap_or_else(|| {
                                    block
                                        .spans
                                        .iter()
                                        .find(|span| span.start < *start && *start <= span.end)
                                        .or_else(|| block.spans.first())
                                        .map_or_else(Marks::default, |span| span.marks.clone())
                                });
                                match mark {
                                    Mark::Strong => marks.strong = *enabled,
                                    Mark::Emphasis => marks.emphasis = *enabled,
                                    Mark::Underline => marks.underline = *enabled,
                                    Mark::Strike => marks.strike = *enabled,
                                    Mark::Code => marks.code = *enabled,
                                }
                                typing_marks = Some(marks);
                                Ok(())
                            } else {
                                block.format(*start, *end, mark.clone(), *enabled)
                            }
                        }
                        Edit::SetSpans { spans, .. } => {
                            block.spans = spans.clone();
                            block.normalize();
                            Ok(())
                        }
                        Edit::SetKind { kind, .. } => {
                            block.kind = kind.clone();
                            Ok(())
                        }
                        Edit::Link {
                            start,
                            end,
                            url,
                            title,
                            ..
                        } => {
                            if !safe_url(url) || title.len() > 4096 {
                                return Err("unsafe link".into());
                            }
                            check_range(&block.text, *start, *end)?;
                            let mut spans = Vec::new();
                            for span in &block.spans {
                                let a = span.start.max(*start);
                                let b = span.end.min(*end);
                                if a >= b {
                                    spans.push(span.clone());
                                    continue;
                                }
                                if span.start < a {
                                    spans.push(Span {
                                        end: a,
                                        ..span.clone()
                                    })
                                }
                                let mut middle = Span {
                                    start: a,
                                    end: b,
                                    ..span.clone()
                                };
                                middle.marks.link = url.clone();
                                middle.marks.title = title.clone();
                                spans.push(middle);
                                if b < span.end {
                                    spans.push(Span {
                                        start: b,
                                        ..span.clone()
                                    })
                                }
                            }
                            block.spans = spans;
                            block.normalize();
                            Ok(())
                        }
                        _ => unreachable!(),
                    })? {
                        return Err("block does not exist".into());
                    }
                }
            }
        }
        if candidate.blocks.is_empty() {
            candidate = Content::empty()
        }
        candidate.validate()?;
        if !active() {
            return Err("rich transaction cancelled".into());
        }
        if candidate == *self.content {
            self.typing_marks = typing_marks;
            return Ok(self.revision);
        }
        let snapshot = self.freeze();
        self.content = Arc::new(candidate);
        self.selection = selection;
        self.typing_marks = typing_marks;
        self.undo.push_back(snapshot);
        self.redo.clear();
        self.trim();
        self.revision += 1;
        // Preserve valid selections and clamp ranges when their leaf was edited.
        let first = self.content.leaves().first().map_or(0, |block| block.id);
        for p in [&mut self.selection.anchor, &mut self.selection.caret] {
            if let Some(block) = self.content.find(p.block) {
                p.byte = p.byte.min(block.text.len());
                while !boundary(&block.text, p.byte) {
                    p.byte = p.byte.saturating_sub(1)
                }
            } else {
                p.block = first;
                p.byte = 0
            }
        }
        Ok(self.revision)
    }
    fn trim(&mut self) {
        while self.undo.len() + self.redo.len() > 200 || self.history_bytes() > HISTORY_LIMIT {
            if self.undo.pop_front().is_none() {
                self.redo.pop_front();
            }
        }
    }
    pub fn undo(&mut self) -> bool {
        if let Some(previous) = self.undo.pop_back() {
            self.redo.push_back(self.freeze());
            self.content = previous.content;
            self.selection = previous.selection;
            self.typing_marks = previous.typing_marks;
            self.revision += 1;
            self.trim();
            true
        } else {
            false
        }
    }
    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo.pop_back() {
            self.undo.push_back(self.freeze());
            self.content = next.content;
            self.selection = next.selection;
            self.typing_marks = next.typing_marks;
            self.revision += 1;
            self.trim();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use markup::{Format, export, import};
    fn semantic(content: &Content) -> serde_json::Value {
        let mut value = serde_json::to_value(content).unwrap();
        fn strip(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(fields) => {
                    fields.remove("id");
                    for value in fields.values_mut() {
                        strip(value)
                    }
                }
                serde_json::Value::Array(values) => {
                    for value in values {
                        strip(value)
                    }
                }
                _ => (),
            }
        }
        strip(&mut value);
        value
    }
    #[test]
    fn markdown_and_html_roundtrip_structures_unicode_links_images_and_code() {
        let source = "# Heading שלום\n\nA **bold** and *italic* [link](https://example.com) and ![diagram](asset:diagram).\n\n3. first\n4. second\n   - nested\n\n> quoted ~~deleted~~\n\n```rust\nlet a = \"<&>\";\n```\n\n---\n";
        let original = import(Format::Markdown, source, || true).unwrap().content;
        assert!(original.plain_text().contains("diagram"));
        for format in [Format::Markdown, Format::Html] {
            let text = export(format.clone(), &original, || true).unwrap();
            let restored = import(format, &text, || true).unwrap().content;
            assert_eq!(semantic(&original), semantic(&restored), "{text}");
        }
    }
    #[test]
    fn unicode_graphemes_formatting_atomic_errors_snapshots_and_history() {
        let mut doc = Document::new(Content {
            blocks: vec![Arc::new(Block::paragraph("a👩‍💻e\u{301}שלום"))],
        })
        .unwrap();
        let id = doc.content.blocks[0].id;
        let snapshot = doc.freeze();
        assert!(
            doc.transact(
                0,
                &[Edit::Replace {
                    block: id,
                    start: 2,
                    end: 2,
                    text: "bad".into(),
                    marks: None
                }],
                || true
            )
            .is_err()
        );
        assert_eq!(doc.revision, 0);
        let end = 1 + "👩‍💻".len();
        doc.transact(
            0,
            &[
                Edit::SetMark {
                    block: id,
                    start: 1,
                    end,
                    mark: Mark::Strong,
                    enabled: true,
                },
                Edit::Replace {
                    block: id,
                    start: 0,
                    end: 1,
                    text: "z".into(),
                    marks: None,
                },
            ],
            || true,
        )
        .unwrap();
        assert!(doc.dirty());
        assert_eq!(snapshot.content.plain_text(), "a👩‍💻e\u{301}שלום");
        assert!(doc.undo());
        assert!(!doc.dirty());
        assert!(doc.redo());
        assert_eq!(doc.content.plain_text(), "z👩‍💻e\u{301}שלום");
        let frozen = doc.freeze();
        doc.mark_saved(&frozen);
        assert!(!doc.dirty());
        assert!(
            doc.transact(
                doc.revision,
                &[Edit::SetMark {
                    block: id,
                    start: 0,
                    end: 1,
                    mark: Mark::Underline,
                    enabled: true
                }],
                || false
            )
            .is_err()
        );
        assert!(!doc.dirty());
    }
    #[test]
    fn structure_split_move_and_join_are_undoable() {
        let mut doc = Document::new(Content {
            blocks: vec![Arc::new(Block::paragraph("first second"))],
        })
        .unwrap();
        let first = doc.content.blocks[0].id;
        doc.transact(
            0,
            &[Edit::Split {
                block: first,
                at: 6,
            }],
            || true,
        )
        .unwrap();
        assert_eq!(doc.content.blocks.len(), 2);
        let second = doc.content.blocks[1].id;
        doc.transact(
            1,
            &[Edit::Move {
                block: second,
                parent: 0,
                index: 0,
            }],
            || true,
        )
        .unwrap();
        assert_eq!(doc.content.blocks[0].id, second);
        assert!(doc.undo());
        doc.transact(doc.revision, &[Edit::Join { first, second }], || true)
            .unwrap();
        assert_eq!(doc.content.plain_text(), "first second");
        assert_eq!(doc.content.blocks[0].id, first);
    }
    #[test]
    fn html_rejects_execution_attributes_dtd_unsafe_urls_and_unknown_elements() {
        for source in [
            "<script>alert(1)</script>",
            "<p onclick=\"x\">hi</p>",
            "<p><a href=\"javascript:alert(1)\">x</a></p>",
            "<!DOCTYPE x [<!ENTITY a 'x'>]><p>&a;</p>",
            "<table/>",
            "<pre><code>one</code><code>two</code></pre>",
            "<pre><code class=\"discard-me\">text</code></pre>",
            "<p><br>lost text</br></p>",
            "<hr>lost text</hr>",
            "<p xmlns=\"urn:unsupported\">text</p>",
            "<p><img src=\"data:text/html,hi\"/></p>",
        ] {
            assert!(
                import(Format::Html, source, || true).is_err(),
                "accepted {source}"
            );
        }
        assert!(import(Format::Markdown, "[bad](javascript:alert)", || true).is_err());
        assert!(import(Format::Markdown, "hi", || false).is_err());
    }
    #[test]
    fn underline_space_styles_and_empty_paragraphs_roundtrip_without_silent_loss() {
        let original = import(
            Format::Html,
            "<p/><p><u> a </u><strong>b</strong></p><p/>",
            || true,
        )
        .unwrap()
        .content;
        for format in [Format::Html, Format::Markdown] {
            let exported = export(format.clone(), &original, || true).unwrap();
            let restored = import(format, &exported, || true).unwrap().content;
            assert_eq!(semantic(&original), semantic(&restored), "{exported}");
        }
    }
    #[test]
    fn history_is_bounded_after_repeated_large_format_and_text_edits() {
        let mut doc = Document::new(Content {
            blocks: vec![Arc::new(Block::paragraph(&"source ".repeat(30_000)))],
        })
        .unwrap();
        let id = doc.content.blocks[0].id;
        for _ in 0..250 {
            doc.transact(
                doc.revision,
                &[Edit::Replace {
                    block: id,
                    start: 0,
                    end: 0,
                    text: "x".into(),
                    marks: None,
                }],
                || true,
            )
            .unwrap();
            assert!(doc.history_bytes() <= HISTORY_LIMIT);
        }
        let revision = doc.revision;
        assert!(
            doc.transact(revision - 1, &[Edit::Remove { block: id }], || true)
                .is_err()
        );
        assert_eq!(doc.revision, revision);
    }
}
