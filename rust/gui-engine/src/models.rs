// models: what a collection view needs from an application's items beyond
// a count and a row function. A `ModelLog` numbers each change to a list by
// the revision it produced, so a view that last saw revision r replays only
// what came after it and keeps its scroll position, focus and range anchor
// on the same items; a `SortFilter` is a sorted, filtered view of a source
// list by position.

/// The most changes a `ModelLog` keeps; a view further behind resets.
pub const LOG_LIMIT: i64 = 256;

/// One change to a list. Positions are those of the list before the change.
#[derive(Clone, Debug, PartialEq, Eq, Copy, serde::Serialize, serde::Deserialize)]
pub enum ModelChange {
    /// `count` items were inserted at `at`.
    Inserted(i64, i64),
    /// The `count` items starting at `at` were removed.
    Removed(i64, i64),
    /// The `count` items starting at `from` now start at `to`, a position in
    /// the list without them.
    Moved(i64, i64, i64),
    /// Anything may have changed.
    Reset,
}

/// Where the item at position `i` before `change` is after it; -1 when the
/// change removed it, and `i` unchanged for a reset.
pub fn position_after(i: i64, change: ModelChange) -> i64 {
    match change {
        ModelChange::Inserted(at, count) => {
            if i >= at {
                i + count
            } else {
                i
            }
        }
        ModelChange::Removed(at, count) => {
            if i < at {
                i
            } else if i < at + count {
                -1
            } else {
                i - count
            }
        }
        ModelChange::Moved(from, count, to) => {
            if i >= from && i < from + count {
                return to + (i - from);
            };
            let without = if i >= from + count { i - count } else { i };
            if without >= to {
                without + count
            } else {
                without
            }
        }
        ModelChange::Reset => i,
    }
}

/// The changes an application made to one list, newest last, each with the
/// revision it produced. `revision` starts at 0 and counts every change.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelLog {
    pub revision: i64,
    pub changes: Vec<(i64, ModelChange)>,
}

impl ModelLog {
    pub fn new() -> ModelLog {
        ModelLog {
            revision: 0,
            changes: vec![],
        }
    }

    /// Records that `count` items were inserted at `at`.
    pub fn inserted(&mut self, at: i64, count: i64) {
        self.record(ModelChange::Inserted(at, count))
    }

    /// Records that the `count` items starting at `at` were removed.
    pub fn removed(&mut self, at: i64, count: i64) {
        self.record(ModelChange::Removed(at, count))
    }

    /// Records that the `count` items starting at `from` now start at `to`
    /// (a position in the list without them).
    pub fn moved(&mut self, from: i64, count: i64, to: i64) {
        self.record(ModelChange::Moved(from, count, to))
    }

    /// Records that anything may have changed (a reload, a new sort order).
    pub fn reset(&mut self) {
        self.record(ModelChange::Reset)
    }

    fn record(&mut self, change: ModelChange) {
        self.revision += 1;
        self.changes.push((self.revision, change));
        if self.changes.len() > LOG_LIMIT as usize {
            let _ = self.changes.remove(0);
        }
    }

    /// The changes after `revision`, oldest first, or `None` when the log no
    /// longer reaches back that far (the reader should reset).
    pub fn since(&self, revision: i64) -> Option<Vec<ModelChange>> {
        if revision >= self.revision {
            return Some(vec![]);
        };
        let mut out = vec![];
        let mut first = true;
        for (r, change) in &self.changes {
            if *r <= revision {
                continue;
            };
            if first && *r != revision + 1 {
                return None;
            };
            first = false;
            out.push(*change)
        }
        if out.len() as i64 == self.revision - revision {
            Some(out)
        } else {
            None
        }
    }
}

/// A sorted, filtered view of a source list: view position `i` shows source
/// position `source_of(i)`. Rebuild it when the source changes; it answers
/// the change a view should replay (`ModelChange::Reset`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SortFilter {
    /// Source positions in view order.
    pub order: Vec<i64>,
}

pub const MODEL_BYTE_LIMIT: usize = 128 * 1024 * 1024;
impl SortFilter {
    /// The `count` source items that `keep` accepts, ordered by `sort_key`
    /// (ascending, or descending when `ascending` is false); items with equal
    /// keys keep their source order.
    pub fn new(
        count: i64,
        keep: impl Fn(i64) -> bool,
        sort_key: impl Fn(i64) -> String,
        ascending: bool,
    ) -> SortFilter {
        Self::try_new(count, keep, sort_key, ascending).unwrap_or_else(|error| panic!("{error}"))
    }

    pub fn try_new(
        count: i64,
        keep: impl Fn(i64) -> bool,
        sort_key: impl Fn(i64) -> String,
        ascending: bool,
    ) -> Result<SortFilter, String> {
        if count > 4_000_000 {
            return Err("model index exceeds its byte budget".into());
        }
        let mut charged = 0usize;
        let mut keyed: Vec<(String, i64)> = vec![];
        let mut i = 0;
        while i < count {
            if keep(i) {
                let key = sort_key(i);
                charged = charged.saturating_add(key.capacity() + 2 * size_of::<(String, i64)>());
                if charged > MODEL_BYTE_LIMIT {
                    return Err("model sort keys exceed their byte budget".into());
                }
                keyed.push((key, i))
            };
            i += 1;
        }
        if ascending {
            keyed.sort_by(|a, b| {
                if a.0 < b.0 {
                    (-1).cmp(&0)
                } else if a.0 > b.0 {
                    (1).cmp(&0)
                } else if a.1 < b.1 {
                    (-1).cmp(&0)
                } else if a.1 > b.1 {
                    (1).cmp(&0)
                } else {
                    (0).cmp(&0)
                }
            })
        } else {
            keyed.sort_by(|a, b| {
                if a.0 > b.0 {
                    (-1).cmp(&0)
                } else if a.0 < b.0 {
                    (1).cmp(&0)
                } else if a.1 < b.1 {
                    (-1).cmp(&0)
                } else if a.1 > b.1 {
                    (1).cmp(&0)
                } else {
                    (0).cmp(&0)
                }
            })
        }
        Ok(SortFilter {
            order: keyed.into_iter().map(|entry| entry.1).collect(),
        })
    }

    /// Every source item in source order.
    pub fn identity(count: i64) -> SortFilter {
        assert!(count <= 4_000_000, "model index exceeds its byte budget");
        SortFilter {
            order: (0..count).collect(),
        }
    }

    pub fn count(&self) -> i64 {
        self.order.len() as i64
    }

    /// The source position shown at view position `i`, or -1.
    pub fn source_of(&self, i: i64) -> i64 {
        if i < 0 || i >= self.order.len() as i64 {
            -1
        } else {
            self.order[i as usize]
        }
    }

    /// The view position of source position `source`, or -1 when filtered out.
    pub fn view_of(&self, source: i64) -> i64 {
        self.order
            .iter()
            .position(|s| *s == source)
            .map_or(-1, |at| at as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::{LOG_LIMIT, ModelChange, ModelLog, SortFilter, position_after};

    #[test]
    fn positions_follow_inserts_removals_and_moves() {
        assert_eq!(position_after(4, ModelChange::Inserted(2, 3)), 7);
        assert_eq!(position_after(1, ModelChange::Inserted(2, 3)), 1);
        assert_eq!(position_after(2, ModelChange::Inserted(2, 3)), 5);
        assert_eq!(position_after(5, ModelChange::Removed(2, 3)), 2);
        assert_eq!(position_after(3, ModelChange::Removed(2, 3)), -1);
        assert_eq!(position_after(1, ModelChange::Removed(2, 3)), 1);
        // Items 1 and 2 move to the end of [0 1 2 3 4]: [0 3 4 1 2].
        assert_eq!(position_after(1, ModelChange::Moved(1, 2, 3)), 3);
        assert_eq!(position_after(2, ModelChange::Moved(1, 2, 3)), 4);
        assert_eq!(position_after(3, ModelChange::Moved(1, 2, 3)), 1);
        assert_eq!(position_after(0, ModelChange::Moved(1, 2, 3)), 0);
        // Item 3 moves to the front of [0 1 2 3]: [3 0 1 2].
        assert_eq!(position_after(3, ModelChange::Moved(3, 1, 0)), 0);
        assert_eq!(position_after(0, ModelChange::Moved(3, 1, 0)), 1)
    }

    #[test]
    fn a_log_replays_what_came_after_a_revision_and_resets_past_its_limit() {
        let mut log = ModelLog::new();
        log.inserted(0, 2);
        log.removed(1, 1);
        log.moved(0, 1, 1);
        assert_eq!(log.revision, 3);
        assert_eq!(log.since(3), Some(vec![]));
        assert_eq!(
            log.since(1),
            Some(vec![
                ModelChange::Removed(1, 1),
                ModelChange::Moved(0, 1, 1)
            ])
        );
        let mut long = ModelLog::new();
        let mut i = 0;
        while i < LOG_LIMIT + 5 {
            long.inserted(0, 1);
            i += 1;
        }
        assert_eq!(long.since(2), None);
        assert_eq!(
            long.since(long.revision - 1),
            Some(vec![ModelChange::Inserted(0, 1)])
        )
    }

    #[test]
    fn a_sort_filter_keeps_matching_items_in_key_order_and_is_stable() {
        let names = ["pear", "apple", "fig", "apple", "kiwi"];
        let p = SortFilter::new(
            names.len() as i64,
            |i| names[i as usize] != "fig",
            |i| names[i as usize].to_string(),
            true,
        );
        assert_eq!(p.order, vec![1, 3, 4, 0]);
        assert_eq!(p.count(), 4);
        assert_eq!(p.source_of(2), 4);
        assert_eq!(p.view_of(2), -1);
        assert_eq!(p.view_of(0), 3);
        let down = SortFilter::new(
            names.len() as i64,
            |_| true,
            |i| names[i as usize].to_string(),
            false,
        );
        assert_eq!(down.order, vec![0, 4, 2, 1, 3]);
        assert_eq!(SortFilter::identity(3).order, vec![0, 1, 2])
    }
}
