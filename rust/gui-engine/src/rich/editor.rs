//! The existing text controller edits one structured leaf without a second undo
//! history. IME/preedit and view scroll remain control-local; committed content,
//! selection and formatting belong to the native structured document.
use super::*;
use crate::{editing::EditState, text::layout};
pub(crate) fn state(document: i64, block: i64, mut cached: EditState) -> Result<EditState, String> {
    let (snapshot, info) = registry::view(document)?;
    let leaf = snapshot
        .content
        .find(block)
        .ok_or("rich editor block is closed")?;
    if leaf.kind.container() || leaf.kind == Kind::Rule {
        return Err("rich editor requires a text block".into());
    }
    let changed = !cached.text.same_revision(&leaf.text);
    if changed {
        cached.text = leaf.text.snapshot();
        cached.preedit.clear();
        cached.preedit_caret = 0;
    }
    if changed || cached.revision != info.revision || !cached.initialized {
        if info.selection.anchor.block == block && info.selection.caret.block == block {
            cached.anchor = info.selection.anchor.byte as i64;
            cached.caret = info.selection.caret.byte as i64;
        }
        cached.anchor = cached.text.snap(cached.anchor.max(0) as usize) as i64;
        cached.caret = cached.text.snap(cached.caret.max(0) as usize) as i64;
    }
    cached.undo.clear();
    cached.redo.clear();
    cached.document = 0;
    cached.initialized = true;
    cached.revision = info.revision;
    cached.rich_committed = false;
    Ok(cached)
}
pub(crate) fn remember_selection(document: i64, block: i64, state: &EditState) {
    // A key-up or focus-out may reach the old tree after a split/undo has
    // selected a different leaf. That stale control must not steal selection.
    if registry::info(document).is_ok_and(|info| info.selection.caret.block != block) {
        return;
    }
    let _ = registry::select(
        document,
        Selection {
            anchor: Position {
                block,
                byte: state.anchor.max(0) as usize,
            },
            caret: Position {
                block,
                byte: state.caret.max(0) as usize,
            },
        },
    );
}
pub(crate) fn commit(document: i64, block: i64, state: &mut EditState) -> Result<bool, String> {
    if state.rich_committed {
        return Ok(false);
    }
    let (original, info) = registry::view(document)?;
    let old = original
        .content
        .find(block)
        .ok_or("rich editor block is closed")?;
    if old.text.same_revision(&state.text) {
        remember_selection(document, block, state);
        return Ok(false);
    }
    if state.revision != info.revision + 1 {
        return Err("stale rich editor transaction".into());
    }
    let before = old.text.to_string();
    let after = state.text.to_string();
    let a = layout::grapheme_boundaries(&before);
    let b = layout::grapheme_boundaries(&after);
    let prefix = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let first = a
        .partition_point(|&byte| byte <= prefix as i64)
        .saturating_sub(1);
    let last = b
        .partition_point(|&byte| byte <= prefix as i64)
        .saturating_sub(1);
    let prefix = (a[first] as usize).min(b[last] as usize);
    let suffix = before.as_bytes()[prefix..]
        .iter()
        .rev()
        .zip(after.as_bytes()[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let mut old_end = before.len() - suffix;
    let mut new_end = after.len() - suffix;
    while a.binary_search(&(old_end as i64)).is_err() || b.binary_search(&(new_end as i64)).is_err()
    {
        old_end += 1;
        new_end += 1;
    }
    registry::transact(
        document,
        info.revision,
        &[Edit::Replace {
            block,
            start: prefix,
            end: old_end,
            text: after[prefix..new_end].into(),
            marks: None,
        }],
        || true,
    )?;
    let selection = Selection {
        anchor: Position {
            block,
            byte: state.anchor.max(0) as usize,
        },
        caret: Position {
            block,
            byte: state.caret.max(0) as usize,
        },
    };
    registry::select(document, selection)?;
    *state = self::state(document, block, state.clone())?;
    Ok(true)
}
pub(crate) fn format(
    document: i64,
    block: i64,
    state: &EditState,
    mark: Mark,
) -> Result<i64, String> {
    remember_selection(document, block, state);
    let info = registry::info(document)?;
    let enabled = !match mark {
        Mark::Strong => info.marks.strong,
        Mark::Emphasis => info.marks.emphasis,
        Mark::Underline => info.marks.underline,
        Mark::Strike => info.marks.strike,
        Mark::Code => info.marks.code,
    };
    let (start, end) = state.selection();
    Ok(registry::transact(
        document,
        info.revision,
        &[Edit::SetMark {
            block,
            start: start.max(0) as usize,
            end: end.max(0) as usize,
            mark,
            enabled,
        }],
        || true,
    )?
    .revision)
}
pub(crate) fn undo(
    document: i64,
    block: i64,
    state: &mut EditState,
    redo: bool,
) -> Result<i64, String> {
    let info = registry::info(document)?;
    let info = if redo {
        registry::redo(document, info.revision)?
    } else {
        registry::undo(document, info.revision)?
    };
    let target = if registry::snapshot(document)?.content.find(block).is_some() {
        block
    } else {
        info.selection.caret.block
    };
    *state = self::state(document, target, state.clone())?;
    state.rich_committed = true;
    Ok(info.revision)
}

/// Split a selected paragraph, preserving its inline styles and list structure.
/// The entire change is one history entry and the caret moves to the new leaf.
pub fn enter(document: i64) -> Result<registry::Info, String> {
    let (snapshot, info) = registry::view(document)?;
    let selection = &info.selection;
    if selection.anchor.block != selection.caret.block {
        return Err("split requires a single text block selection".into());
    }
    let id = selection.caret.block;
    let leaf = snapshot
        .content
        .find(id)
        .ok_or("selected rich block is closed")?;
    if leaf.kind.container() || leaf.kind == Kind::Rule {
        return Err("split requires a text block".into());
    }
    let start = selection.anchor.byte.min(selection.caret.byte);
    let end = selection.anchor.byte.max(selection.caret.byte);
    let mut edits = Vec::new();
    if matches!(leaf.kind, Kind::Code(_)) {
        edits.push(Edit::Replace {
            block: id,
            start,
            end,
            text: "\n".into(),
            marks: Some(Marks::default()),
        });
        return registry::transact(document, info.revision, &edits, || true);
    }
    let path = path(&snapshot.content, id).ok_or("selected rich block is closed")?;
    let (parent, index) = parent_index(&snapshot.content, &path);
    let mut right = leaf.sliced(end, leaf.text.len());
    right.id = next_id();
    if matches!(right.kind, Kind::Heading(_)) {
        right.kind = Kind::Paragraph;
    }
    let right_id = right.id;
    edits.push(Edit::Replace {
        block: id,
        start,
        end: leaf.text.len(),
        text: String::new(),
        marks: None,
    });
    if parent.kind == Kind::Item && path.len() >= 2 {
        let (list, item_index) = parent_index(&snapshot.content, &path[..path.len() - 1]);
        if matches!(list.kind, Kind::List(_)) {
            if leaf.text.is_empty() && parent.children.len() == 1 {
                return indent(document, true);
            }
            let mut item = Block::new(Kind::Item);
            item.children.push(Arc::new(right));
            let item_id = item.id;
            edits.push(Edit::Insert {
                parent: list.id,
                index: item_index + 1,
                block: item,
            });
            for (offset, child) in parent.children.iter().skip(index + 1).enumerate() {
                edits.push(Edit::Move {
                    block: child.id,
                    parent: item_id,
                    index: offset + 1,
                });
            }
        } else {
            edits.push(Edit::Insert {
                parent: parent.id,
                index: index + 1,
                block: right,
            });
        }
    } else {
        edits.push(Edit::Insert {
            parent: parent.id,
            index: index + 1,
            block: right,
        });
    }
    registry::transact(document, info.revision, &edits, || true)?;
    registry::select(
        document,
        Selection {
            anchor: Position {
                block: right_id,
                byte: 0,
            },
            caret: Position {
                block: right_id,
                byte: 0,
            },
        },
    )?;
    registry::info(document)
}
// Root is represented by a temporary id-zero container, never inserted.
fn path(content: &Content, id: i64) -> Option<Vec<usize>> {
    fn visit(blocks: &[Arc<Block>], id: i64, indices: &mut Vec<usize>) -> bool {
        for (index, block) in blocks.iter().enumerate() {
            indices.push(index);
            if block.id == id || visit(&block.children, id, indices) {
                return true;
            }
            indices.pop();
        }
        false
    }
    let mut indices = Vec::new();
    visit(&content.blocks, id, &mut indices).then_some(indices)
}
fn parent_index(content: &Content, path: &[usize]) -> (Block, usize) {
    let mut root = Block::new(Kind::Quote);
    root.id = 0;
    root.children = content.blocks.clone();
    for &index in &path[..path.len() - 1] {
        root = (*root.children[index]).clone();
    }
    (root, *path.last().unwrap())
}
/// Indent a list item under its predecessor, or outdent into the containing list.
/// At the top level outdent promotes the item while preserving surrounding lists.
pub fn indent(document: i64, outdent: bool) -> Result<registry::Info, String> {
    let (snapshot, info) = registry::view(document)?;
    let mut indices = path(&snapshot.content, info.selection.caret.block)
        .ok_or("selected rich block is closed")?;
    let mut item = None;
    while !indices.is_empty() {
        let (parent, index) = parent_index(&snapshot.content, &indices);
        if parent.children[index].kind == Kind::Item {
            item = Some((indices.clone(), parent, index));
            break;
        }
        indices.pop();
    }
    let (item_path, list, index) = item.ok_or("indentation requires a list item")?;
    if !matches!(list.kind, Kind::List(_)) {
        return Err("item requires a list parent".into());
    }
    let item = &list.children[index];
    let mut edits = Vec::new();
    if !outdent {
        if index == 0 {
            return Err("first list item has no predecessor".into());
        }
        let previous = &list.children[index - 1];
        let nested = previous.children.last().filter(|b| b.kind == list.kind);
        let target = if let Some(nested) = nested {
            nested.id
        } else {
            let nested = Block::new(list.kind.clone());
            let id = nested.id;
            edits.push(Edit::Insert {
                parent: previous.id,
                index: previous.children.len(),
                block: nested,
            });
            id
        };
        edits.push(Edit::Move {
            block: item.id,
            parent: target,
            index: nested.map_or(0, |list| list.children.len()),
        });
    } else {
        let list_path = &item_path[..item_path.len() - 1];
        let (parent, list_index) = parent_index(&snapshot.content, list_path);
        if parent.kind == Kind::Item && list_path.len() >= 2 {
            let (outer, parent_index) =
                parent_index(&snapshot.content, &list_path[..list_path.len() - 1]);
            if !matches!(outer.kind, Kind::List(_)) {
                return Err("invalid nested list".into());
            }
            // Following siblings remain nested under the outdented item.
            if index + 1 < list.children.len() {
                let nested = Block::new(list.kind.clone());
                let target = nested.id;
                edits.push(Edit::Insert {
                    parent: item.id,
                    index: item.children.len(),
                    block: nested,
                });
                for (offset, next) in list.children.iter().skip(index + 1).enumerate() {
                    edits.push(Edit::Move {
                        block: next.id,
                        parent: target,
                        index: offset,
                    });
                }
            }
            edits.push(Edit::Move {
                block: item.id,
                parent: outer.id,
                index: parent_index + 1,
            });
        } else {
            let mut insertion = list_index + 1;
            for child in &item.children {
                edits.push(Edit::Move {
                    block: child.id,
                    parent: parent.id,
                    index: insertion,
                });
                insertion += 1;
            }
            if index + 1 < list.children.len() {
                let mut tail = Block::new(list.kind.clone());
                if let Kind::List(Some(start)) = list.kind {
                    tail.kind = Kind::List(Some(start + index as i64 + 1));
                }
                let target = tail.id;
                edits.push(Edit::Insert {
                    parent: parent.id,
                    index: insertion,
                    block: tail,
                });
                for (offset, next) in list.children.iter().skip(index + 1).enumerate() {
                    edits.push(Edit::Move {
                        block: next.id,
                        parent: target,
                        index: offset,
                    });
                }
            }
            edits.push(Edit::Remove { block: item.id });
        }
        if list.children.len() == 1 || index == 0 {
            edits.push(Edit::Remove { block: list.id });
        }
    }
    registry::transact(document, info.revision, &edits, || true)
}

/// Wrap the current text block in a quotation or list, preserving its identity.
pub fn wrap(document: i64, kind: Kind) -> Result<registry::Info, String> {
    if !matches!(kind, Kind::Quote | Kind::List(_)) {
        return Err("wrap requires quote or list".into());
    }
    let (snapshot, info) = registry::view(document)?;
    let id = info.selection.caret.block;
    let path = path(&snapshot.content, id).ok_or("selected rich block is closed")?;
    let (parent, index) = parent_index(&snapshot.content, &path);
    let mut wrapper = Block::new(kind.clone());
    let wrapper_id = wrapper.id;
    let target = if matches!(kind, Kind::List(_)) {
        let item = Block::new(Kind::Item);
        let id = item.id;
        wrapper.children.push(Arc::new(item));
        id
    } else {
        wrapper_id
    };
    registry::transact(
        document,
        info.revision,
        &[
            Edit::Insert {
                parent: parent.id,
                index,
                block: wrapper,
            },
            Edit::Move {
                block: id,
                parent: target,
                index: 0,
            },
        ],
        || true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Budget;
    #[test]
    fn paragraph_and_list_commands_preserve_styles_selection_and_atomic_history() {
        let budget = Budget::new(256 * 1024 * 1024);
        let id = registry::import(
            markup::Format::Markdown,
            "**first** tail\n\n- one\n- two\n- three\n",
            budget.clone(),
            || true,
        )
        .unwrap();
        let original = registry::snapshot(id).unwrap();
        let first = original.content.blocks[0].id;
        let select = |block, byte| {
            registry::select(
                id,
                Selection {
                    anchor: Position { block, byte },
                    caret: Position { block, byte },
                },
            )
            .unwrap()
        };
        select(first, 5);
        enter(id).unwrap();
        let split = registry::snapshot(id).unwrap();
        assert_eq!(split.content.blocks[0].text, "first");
        assert_eq!(split.content.blocks[1].text, " tail");
        assert!(split.content.blocks[0].spans[0].marks.strong);
        assert_eq!(
            registry::info(id).unwrap().selection.caret.block,
            split.content.blocks[1].id
        );
        registry::undo(id, registry::info(id).unwrap().revision).unwrap();
        assert_eq!(registry::snapshot(id).unwrap().content, original.content);
        let two = original.content.blocks[1].children[1].children[0].id;
        select(two, 1);
        indent(id, false).unwrap();
        let nested = registry::snapshot(id).unwrap();
        assert_eq!(nested.content.blocks[1].children.len(), 2);
        assert_eq!(
            nested.content.blocks[1].children[0].children[1].children[0].children[0].id,
            two
        );
        indent(id, true).unwrap();
        assert_eq!(registry::snapshot(id).unwrap().content, original.content);
        enter(id).unwrap();
        let split = registry::snapshot(id).unwrap();
        assert_eq!(split.content.blocks[1].children.len(), 4);
        assert_eq!(split.content.blocks[1].children[1].children[0].text, "t");
        assert_eq!(split.content.blocks[1].children[2].children[0].text, "wo");
        indent(id, true).unwrap();
        let promoted = registry::snapshot(id).unwrap();
        assert_eq!(promoted.content.blocks[2].text, "wo");
        assert!(matches!(promoted.content.blocks[3].kind, Kind::List(_)));
        let text = registry::export(
            id,
            registry::info(id).unwrap().revision,
            markup::Format::Markdown,
            || true,
        )
        .unwrap();
        assert!(text.contains("wo"));
        registry::close(id);
        assert_eq!(budget.usage().0, 0);
    }
    #[test]
    fn code_inline_marks_fail_without_silent_export_loss() {
        let id = registry::import(
            markup::Format::Markdown,
            "```rust\nhello\n```",
            Budget::new(128 * 1024 * 1024),
            || true,
        )
        .unwrap();
        let block = registry::snapshot(id).unwrap().content.blocks[0].id;
        assert!(
            registry::transact(
                id,
                0,
                &[Edit::SetMark {
                    block,
                    start: 0,
                    end: 0,
                    mark: Mark::Strong,
                    enabled: true
                }],
                || true
            )
            .is_err()
        );
        assert_eq!(registry::info(id).unwrap().revision, 0);
        registry::close(id);
    }
    #[test]
    fn typing_ime_formatting_and_undo_share_the_document_history() {
        let budget = Budget::new(256 * 1024 * 1024);
        let id = registry::create(Content::empty(), budget.clone()).unwrap();
        let block = registry::snapshot(id).unwrap().content.blocks[0].id;
        let mut field = state(id, block, EditState::default()).unwrap();
        format(id, block, &field, Mark::Strong).unwrap();
        assert!(!registry::info(id).unwrap().dirty);
        assert!(field.insert("👩‍💻", 1, 0));
        assert!(commit(id, block, &mut field).unwrap());
        assert!(
            registry::snapshot(id)
                .unwrap()
                .content
                .find(block)
                .unwrap()
                .spans[0]
                .marks
                .strong
        );
        let before = registry::snapshot(id).unwrap();
        field.compose("候補", crate::text::offsets::ScalarIndex(1));
        assert_eq!(registry::snapshot(id).unwrap().content, before.content);
        field.insert("e\u{301}", 2, 0);
        commit(id, block, &mut field).unwrap();
        undo(id, block, &mut field, false).unwrap();
        assert_eq!(field.text, "👩‍💻");
        assert!(!commit(id, block, &mut field).unwrap());
        undo(id, block, &mut field, true).unwrap();
        assert_eq!(field.text, "👩‍💻e\u{301}");
        registry::close(id);
        assert_eq!(budget.usage().0, 0);
    }
    #[test]
    fn invalid_or_stale_controller_transactions_leave_committed_content_intact() {
        let budget = Budget::new(256 * 1024 * 1024);
        let id = registry::create(
            Content {
                blocks: vec![Arc::new(Block::paragraph("original"))],
            },
            budget.clone(),
        )
        .unwrap();
        let block = registry::snapshot(id).unwrap().content.blocks[0].id;
        let mut field = state(id, block, EditState::default()).unwrap();
        field.insert("local", 1, 0);
        registry::transact(
            id,
            0,
            &[Edit::Replace {
                block,
                start: 0,
                end: 0,
                text: "remote ".into(),
                marks: None,
            }],
            || true,
        )
        .unwrap();
        assert!(commit(id, block, &mut field).is_err());
        assert_eq!(
            registry::snapshot(id).unwrap().content.plain_text(),
            "remote original"
        );
        registry::close(id);
        assert_eq!(budget.usage().0, 0);
    }
}
