// state: per-widget state that outlives a view rebuild. The application
// owns values (a field's text, a checkbox's state); the toolkit owns
// interaction state (caret, selection, scroll offset, open popup, undo
// history) here, keyed by widget id. Each id carries a generation: an id
// that leaves the tree for a frame and comes back is a new widget, so
// late results addressed to the old generation are discarded.

use crate::editing::EditState;
use crate::widgets::collections::CollectionState;
use crate::widgets::completion::ChoiceState;
use crate::widgets::menus::MenuState;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const STATE_BYTE_LIMIT: usize = 256 * 1024 * 1024;

/// A widget's identity: its id and the generation it was created in.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct WidgetId {
    pub key: String,
    pub generation: i64,
}

/// Scroll position and scrollbar drag of a scroll area.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct ScrollState {
    pub x: i64,
    pub y: i64,
    /// Pointer offset into the thumb while dragging it, or -1.
    pub drag: i64,
    pub drag_horizontal: bool,
}

/// Drag state of sliders, dials, splitters, and similar pointer gestures.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DragState {
    pub active: bool,
    /// Which part is dragged (a range slider's low or high thumb).
    pub part: i64,
    pub start_x: i64,
    pub start_y: i64,
    pub start_value: f64,
}

impl Default for DragState {
    fn default() -> DragState {
        DragState {
            active: false,
            part: 0,
            start_x: 0,
            start_y: 0,
            start_value: 0.0,
        }
    }
}

/// One widget's stored state.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WState {
    Edit(EditState),
    Scroll(ScrollState),
    Drag(DragState),
    Choice(ChoiceState),
    Collection(CollectionState),
    Menu(MenuState),
    /// Free-form state for custom widgets and small controls.
    Fields(HashMap<String, String>),
}

/// Every stored widget state of one window.
#[derive(Debug, Clone)]
pub struct StateStore {
    entries: HashMap<String, Arc<WState>>,
    metadata: HashMap<String, Arc<HashMap<String, String>>>,
    generations: HashMap<String, i64>,
    seen: HashSet<String>,
    next_generation: i64,
    charges: HashMap<(String, bool), usize>,
    budget: crate::memory::Budget,
    leases: HashMap<(String, u8), Arc<crate::memory::Lease>>,
    bytes: usize,
    identity_bytes: usize,
    error: String,
}

impl Default for StateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StateStore {
    pub fn new() -> StateStore {
        Self::with_budget(crate::memory::Budget::new(STATE_BYTE_LIMIT))
    }

    pub(crate) fn with_budget(budget: crate::memory::Budget) -> StateStore {
        StateStore {
            entries: HashMap::new(),
            metadata: HashMap::new(),
            generations: HashMap::new(),
            seen: HashSet::new(),
            next_generation: 1,
            charges: HashMap::new(),
            budget,
            leases: HashMap::new(),
            bytes: 0,
            identity_bytes: 0,
            error: String::new(),
        }
    }

    pub fn get(&self, id: &str) -> Option<WState> {
        match self.entries.get(id).map(Arc::as_ref) {
            Some(state) => Some(state.clone()),
            None => self
                .metadata
                .get(id)
                .map(|fields| WState::Fields(fields.as_ref().clone())),
        }
    }

    pub fn set(&mut self, id: &str, state: WState) {
        if let Err(error) = self.try_set(id, state) {
            self.error = error;
        }
    }

    pub fn try_set(&mut self, id: &str, state: WState) -> Result<(), String> {
        if id.is_empty() {
            return Ok(());
        }
        let metadata = matches!(state, WState::Fields(_));
        let key = (id.to_owned(), metadata);
        let previous = self.charges.get(&key).copied().unwrap_or(0);
        let hidden = match &state {
            WState::Edit(edit) => {
                edit.changes.capacity() * std::mem::size_of::<crate::editing::Delta>()
            }
            _ => 0,
        };
        let bytes = crate::memory::measure(&state)
            .saturating_add(hidden)
            .saturating_add(id.len() * 4 + 256);
        if id.len() > 4096
            || bytes > STATE_BYTE_LIMIT
            || self.bytes.saturating_add(self.identity_bytes) - previous > STATE_BYTE_LIMIT - bytes
        {
            return Err(format!(
                "state for {id} exceeds the window state byte budget"
            ));
        }
        let lease_key = (id.to_owned(), u8::from(metadata));
        if let Some(lease) = self.leases.get_mut(&lease_key).and_then(Arc::get_mut) {
            lease.resize(bytes)?;
        } else {
            let lease = self.budget.reserve(bytes)?;
            self.leases.insert(lease_key, Arc::new(lease));
        }
        self.bytes = self.bytes - previous + bytes;
        self.charges.insert(key, bytes);
        if let WState::Fields(fields) = state {
            let compact = fields
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            self.metadata.insert(id.to_owned(), Arc::new(compact));
        } else {
            let mut compact = state.clone();
            if let WState::Collection(collection) = &mut compact {
                collection.widths = collection
                    .widths
                    .iter()
                    .map(|(key, value)| (key.clone(), *value))
                    .collect();
            }
            self.entries.insert(id.to_owned(), Arc::new(compact));
        }
        Ok(())
    }

    pub(crate) fn budget(&self) -> crate::memory::Budget {
        self.budget.clone()
    }
    pub fn owned_bytes(&self) -> usize {
        self.bytes.saturating_add(self.identity_bytes)
    }

    pub fn take_error(&mut self) -> Option<String> {
        if self.error.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.error))
        }
    }

    pub fn purge_caches(&mut self) {
        let ids: Vec<_> = self.entries.keys().cloned().collect();
        for id in ids {
            let Some(mut state) = self.get(&id) else {
                continue;
            };
            match &mut state {
                WState::Edit(edit) => {
                    edit.undo = Vec::new();
                    edit.redo = Vec::new();
                }
                WState::Collection(collection) => {
                    collection.extents = Vec::new();
                    collection.extents_revision = -1;
                }
                WState::Choice(choice) => {
                    choice.items = Default::default();
                    choice.model_items = Default::default();
                    choice.open = false;
                    choice.loading = false;
                }
                _ => continue,
            }
            self.set(&id, state);
        }
    }

    fn compact_empty_maps(&mut self) {
        if self.entries.is_empty() {
            self.entries = HashMap::new();
        }
        if self.metadata.is_empty() {
            self.metadata = HashMap::new();
        }
        if self.charges.is_empty() {
            self.charges = HashMap::new();
        }
        if self.leases.is_empty() {
            self.leases = HashMap::new();
        }
        if self.generations.is_empty() {
            self.generations = HashMap::new();
        }
        if self.seen.is_empty() {
            self.seen = HashSet::new();
        }
    }

    pub fn remove(&mut self, id: &str) {
        if let Some(state) = self.entries.remove(id)
            && let WState::Edit(edit) = state.as_ref()
        {
            crate::text::document_layout::release(edit.layout_key);
        }
        self.metadata.remove(id);
        for metadata in [false, true] {
            self.leases.remove(&(id.to_owned(), u8::from(metadata)));
            self.bytes -= self.charges.remove(&(id.to_owned(), metadata)).unwrap_or(0);
        }
        self.compact_empty_maps();
    }

    /// The generation of `id`, assigning one the first time it is seen.
    pub fn generation(&mut self, id: &str) -> i64 {
        if let Some(g) = self.generations.get(id) {
            return *g;
        }
        if id.len() > 4096
            || self.owned_bytes().saturating_add(id.len() * 8 + 256) > STATE_BYTE_LIMIT
        {
            self.error = "widget identities exceed the window state byte budget".into();
            return 0;
        }
        let lease = match self.budget.reserve(id.len() * 8 + 256) {
            Ok(lease) => lease,
            Err(error) => {
                self.error = error;
                return 0;
            }
        };
        self.leases.insert((id.to_owned(), 2), Arc::new(lease));
        let g = self.next_generation;
        self.next_generation += 1;
        self.generations.insert(id.to_string(), g);
        self.identity_bytes += id.len() * 8 + 256;
        g
    }

    /// The identity of `id` now.
    pub fn widget_id(&mut self, id: &str) -> WidgetId {
        WidgetId {
            key: id.to_string(),
            generation: self.generation(id),
        }
    }

    /// Whether `wid` still names a live widget of the same generation.
    pub fn is_live(&self, wid: &WidgetId) -> bool {
        match self.generations.get(&wid.key) {
            Some(g) => *g == wid.generation,
            None => false,
        }
    }

    /// Records that `id` is in this frame's tree.
    pub fn mark(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        self.seen.insert(id.to_string());
        let _ = self.generation(id);
    }

    pub fn begin_frame(&mut self) {
        self.seen = HashSet::new();
    }

    /// Drops the state and generation of every id not marked this frame and
    /// answers those ids, so their timers, popups, and tasks can be cancelled.
    pub fn end_frame(&mut self) -> Vec<String> {
        let mut gone = vec![];
        for id in self.generations.keys() {
            if !self.seen.contains(id) {
                gone.push(id.to_string())
            }
        }
        for id in &gone {
            self.leases.remove(&(id.to_owned(), 2));
            if self.generations.remove(id).is_some() {
                self.identity_bytes -= id.len() * 8 + 256;
            }
            self.remove(id);
        }
        if self.entries.capacity() > self.entries.len() * 2 + 32 {
            self.entries.shrink_to_fit();
        }
        if self.metadata.capacity() > self.metadata.len() * 2 + 32 {
            self.metadata.shrink_to_fit();
        }
        if self.generations.capacity() > self.generations.len() * 2 + 32 {
            self.generations.shrink_to_fit();
        }
        if self.charges.capacity() > self.charges.len() * 2 + 32 {
            self.charges.shrink_to_fit();
        }
        gone
    }

    /// Moves a page's state and generations without recreating its editing history.
    pub fn transfer_to(&mut self, target: &mut StateStore, ids: &[String]) {
        let additional: usize = ids
            .iter()
            .map(|id| {
                [false, true]
                    .iter()
                    .map(|&metadata| {
                        self.charges
                            .get(&(id.clone(), metadata))
                            .copied()
                            .unwrap_or(0)
                            .saturating_sub(
                                target
                                    .charges
                                    .get(&(id.clone(), metadata))
                                    .copied()
                                    .unwrap_or(0),
                            )
                    })
                    .sum::<usize>()
            })
            .sum();
        if target.owned_bytes().saturating_add(additional) > STATE_BYTE_LIMIT {
            target.error = "tab transfer exceeds the window state byte budget".into();
            return;
        }
        let mut replacements = HashMap::new();
        if self.budget != target.budget {
            for id in ids {
                for role in 0..=2 {
                    let key = (id.clone(), role);
                    if let Some(lease) = self.leases.get(&key) {
                        match target.budget.reserve(lease.bytes()) {
                            Ok(reserved) => {
                                replacements.insert(key, Arc::new(reserved));
                            }
                            Err(error) => {
                                target.error = error;
                                return;
                            }
                        }
                    }
                }
            }
        }
        for id in ids {
            for role in 0..=2 {
                if let Some(lease) = self.leases.remove(&(id.clone(), role)) {
                    let lease = replacements.remove(&(id.clone(), role)).unwrap_or(lease);
                    target.leases.insert((id.clone(), role), lease);
                }
            }
            for metadata in [false, true] {
                if let Some(bytes) = self.charges.remove(&(id.clone(), metadata)) {
                    self.bytes -= bytes;
                    target.bytes -= target
                        .charges
                        .insert((id.clone(), metadata), bytes)
                        .unwrap_or(0);
                    target.bytes += bytes;
                }
            }
            if let Some(fields) = self.metadata.remove(id) {
                target.metadata.insert(id.to_string(), fields);
            }
            if let Some(state) = self.entries.remove(id) {
                target.entries.insert(id.to_string(), state);
            }
            if let Some(generation) = self.generations.remove(id) {
                self.identity_bytes -= id.len() * 8 + 256;
                if target
                    .generations
                    .insert(id.to_string(), generation)
                    .is_none()
                {
                    target.identity_bytes += id.len() * 8 + 256;
                }
                target.next_generation = target.next_generation.max(generation + 1);
            }
            self.seen.remove(id);
            target.seen.insert(id.to_string());
        }
        self.compact_empty_maps();
    }

    pub(crate) fn ids(&self) -> impl Iterator<Item = &str> {
        self.generations.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> i64 {
        let mut count = self.entries.len();
        for id in self.metadata.keys() {
            if !self.entries.contains_key(id) {
                count += 1
            }
        }
        count as i64
    }

    // Typed accessors: each answers the default state when none is stored.

    pub fn edit(&self, id: &str) -> EditState {
        match self.entries.get(id).map(Arc::as_ref) {
            Some(WState::Edit(s)) => {
                if s.document > 0 {
                    crate::editing::documents::state(s.document).unwrap_or_else(|_| s.clone())
                } else {
                    s.clone()
                }
            }
            _ => EditState::default(),
        }
    }

    pub fn scroll(&self, id: &str) -> ScrollState {
        match self.entries.get(id).map(Arc::as_ref) {
            Some(WState::Scroll(s)) => *s,
            _ => ScrollState {
                x: 0,
                y: 0,
                drag: -1,
                drag_horizontal: false,
            },
        }
    }

    pub fn drag(&self, id: &str) -> DragState {
        match self.entries.get(id).map(Arc::as_ref) {
            Some(WState::Drag(s)) => *s,
            _ => DragState::default(),
        }
    }

    pub fn choice(&self, id: &str) -> ChoiceState {
        match self.entries.get(id).map(Arc::as_ref) {
            Some(WState::Choice(s)) => s.clone(),
            _ => ChoiceState::default(),
        }
    }

    pub fn collection(&self, id: &str) -> CollectionState {
        match self.entries.get(id).map(Arc::as_ref) {
            Some(WState::Collection(s)) => s.clone(),
            _ => CollectionState::default(),
        }
    }

    pub fn menu(&self, id: &str) -> MenuState {
        match self.entries.get(id).map(Arc::as_ref) {
            Some(WState::Menu(s)) => s.clone(),
            _ => MenuState::default(),
        }
    }

    pub fn fields(&self, id: &str) -> HashMap<String, String> {
        self.metadata
            .get(id)
            .map(|fields| fields.as_ref().clone())
            .unwrap_or_default()
    }

    /// One free-form field of `id`, or `default`.
    pub fn field(&self, id: &str, name: &str, default: &str) -> String {
        match self.fields(id).get(name) {
            Some(v) => v.to_string(),
            None => default.to_string(),
        }
    }

    pub fn set_field(&mut self, id: &str, name: &str, value: &str) {
        let mut m = self.fields(id);
        m.insert(name.to_string(), value.to_string());
        self.set(id, WState::Fields(m))
    }
}

impl PartialEq for StateStore {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
            && self.metadata == other.metadata
            && self.generations == other.generations
            && self.seen == other.seen
            && self.next_generation == other.next_generation
    }
}

#[cfg(test)]
mod tests {
    use super::{ScrollState, StateStore, WState};

    #[test]
    fn unmarked_ids_are_disposed_and_regenerated() {
        let mut s = StateStore::new();
        s.begin_frame();
        s.mark("a");
        s.set(
            "a",
            WState::Scroll(ScrollState {
                x: 1,
                y: 2,
                drag: -1,
                drag_horizontal: false,
            }),
        );
        let first = s.widget_id("a");
        assert!(s.end_frame().is_empty());
        assert!(s.is_live(&first));
        s.begin_frame();
        let gone = s.end_frame();
        assert_eq!(gone, vec!["a"]);
        assert!(!s.is_live(&first));
        assert_eq!(s.scroll("a").y, 0);
        s.begin_frame();
        s.mark("a");
        let second = s.widget_id("a");
        assert!(second.generation != first.generation)
    }

    #[test]
    fn fields_store_free_form_values() {
        let mut s = StateStore::new();
        s.set_field("w", "open", "yes");
        assert_eq!(s.field("w", "open", "no"), "yes");
        assert_eq!(s.field("w", "other", "no"), "no")
    }
    #[test]
    fn metadata_coexists_with_interaction_state_moves_and_disposes() {
        let mut source = StateStore::new();
        let mut target = StateStore::new();
        source.mark("field");
        source.set_field("field", "validity", "Invalid date");
        source.set("field", WState::Edit(crate::editing::EditState::default()));
        assert_eq!(source.field("field", "validity", ""), "Invalid date");
        source.set_field("field", "hint", "YYYY-MM-DD");
        assert!(matches!(source.get("field"), Some(WState::Edit(_))));
        source.transfer_to(&mut target, &["field".to_string()]);
        assert_eq!(source.field("field", "validity", ""), "");
        assert_eq!(target.field("field", "validity", ""), "Invalid date");
        assert!(matches!(target.get("field"), Some(WState::Edit(_))));
        target.begin_frame();
        target.end_frame();
        assert_eq!(target.len(), 0);
        assert_eq!(target.field("field", "validity", ""), "")
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn windows_share_one_budget_and_disposed_states_return_their_reservations() {
        let budget = crate::memory::Budget::new(24_576);
        let mut first = StateStore::with_budget(budget.clone());
        let mut second = StateStore::with_budget(budget.clone());
        let mut edit = crate::editing::EditState::default();
        edit.sync(&"x".repeat(4000));
        first.try_set("first", WState::Edit(edit.clone())).unwrap();
        assert!(
            second
                .try_set("second", WState::Edit(edit.clone()))
                .is_err()
        );
        assert!(second.get("second").is_none());
        let retained = first.clone();
        drop(first);
        assert!(budget.usage().0 > 0);
        drop(retained);
        second.try_set("second", WState::Edit(edit)).unwrap();
        second.remove("second");
        assert_eq!(budget.usage().0, 0);
    }

    #[test]
    fn transfer_between_accounts_reserves_the_destination_and_releases_the_source() {
        let source_budget = crate::memory::Budget::new(16_384);
        let target_budget = crate::memory::Budget::new(16_384);
        let mut source = StateStore::with_budget(source_budget.clone());
        let mut target = StateStore::with_budget(target_budget.clone());
        source.mark("field");
        source.set_field("field", "draft", "retained");
        let identity = source.widget_id("field");
        source.transfer_to(&mut target, &["field".into()]);
        assert!(target.is_live(&identity));
        assert_eq!(target.field("field", "draft", ""), "retained");
        assert_eq!(source_budget.usage().0, 0);
        assert!(target_budget.usage().0 > 0);
        target.begin_frame();
        target.end_frame();
        assert_eq!(target_budget.usage().0, 0);
    }

    #[test]
    fn native_sparse_metadata_is_compacted_before_retention() {
        let budget = crate::memory::Budget::new(16_384);
        let mut store = StateStore::with_budget(budget.clone());
        let mut fields = HashMap::with_capacity(100_000);
        fields.insert("key".into(), "value".into());
        store.try_set("field", WState::Fields(fields)).unwrap();
        assert!(store.metadata["field"].capacity() < 16);
        assert!(budget.usage().0 < 16_384);
        drop(store);
        assert_eq!(budget.usage().0, 0);
    }
}
