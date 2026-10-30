//! Byte accounting for rebuildable caches. Limits cover owned payload capacities
//! and conservative tree-node storage, not allocator metadata or caller clones.

use std::collections::BTreeMap;
use std::mem::size_of;

pub(crate) type SharedCache<K, V> = std::sync::LazyLock<parking_lot::Mutex<ByteCache<K, V>>>;

struct Entry<V> {
    value: V,
    bytes: usize,
    used: u64,
}

/// Least-recently-used storage with a hard accounted-byte limit. Callers supply
/// the capacities of heap allocations owned by the key and value. Oversized
/// entries are returned to the caller's normal path without being retained.
pub(crate) struct ByteCache<K, V> {
    entries: BTreeMap<std::sync::Arc<K>, Entry<V>>,
    ages: BTreeMap<u64, std::sync::Arc<K>>,
    limit: usize,
    bytes: usize,
    clock: u64,
    evictions: u64,
}

impl<K: Ord + Clone, V> ByteCache<K, V> {
    pub fn new(limit: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            ages: BTreeMap::new(),
            limit,
            bytes: 0,
            clock: 0,
            evictions: 0,
        }
    }

    fn tick(&mut self) -> u64 {
        if self.clock == u64::MAX {
            let old = std::mem::take(&mut self.ages);
            self.clock = 0;
            for (_, key) in old {
                self.clock += 1;
                self.entries.get_mut(key.as_ref()).unwrap().used = self.clock;
                self.ages.insert(self.clock, key);
            }
        }
        self.clock += 1;
        self.clock
    }
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let clock = self.tick();
        let entry = self.entries.get_mut(key)?;
        let owned = self.ages.remove(&entry.used).expect("cache age exists");
        entry.used = clock;
        self.ages.insert(clock, owned);
        Some(&entry.value)
    }
    pub fn insert(&mut self, key: K, value: V, heap_bytes: usize) -> bool {
        // Both indexes share one key allocation. Charge complete sparse B-tree
        // nodes for the entry and age indexes, including Arc storage.
        let bytes = heap_bytes
            .saturating_add(size_of::<K>() + 32)
            .saturating_add(
                (size_of::<std::sync::Arc<K>>()
                    + size_of::<Entry<V>>()
                    + size_of::<u64>()
                    + size_of::<std::sync::Arc<K>>())
                .saturating_mul(12)
                    + 128,
            );
        self.remove(&key);
        if bytes > self.limit {
            return false;
        }
        while self.bytes > self.limit - bytes {
            let (_, oldest) = self.ages.pop_first().expect("nonempty cache over limit");
            self.bytes -= self.entries.remove(oldest.as_ref()).unwrap().bytes;
            self.evictions += 1;
        }
        let used = self.tick();
        let key = std::sync::Arc::new(key);
        self.entries
            .insert(key.clone(), Entry { value, bytes, used });
        self.ages.insert(used, key);
        self.bytes += bytes;
        true
    }
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let entry = self.entries.remove(key)?;
        self.ages.remove(&entry.used);
        self.bytes -= entry.bytes;
        Some(entry.value)
    }
    /// Changes the byte limit, evicting the least recently used entries until
    /// the cache fits it.
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
        while self.bytes > self.limit {
            let Some((_, oldest)) = self.ages.pop_first() else {
                break;
            };
            if let Some(entry) = self.entries.remove(oldest.as_ref()) {
                self.bytes -= entry.bytes;
            }
            self.evictions += 1;
        }
    }
    /// A point in the cache's use order; entries got or inserted after it
    /// are newer than it.
    pub fn mark(&self) -> u64 {
        self.clock
    }
    /// Evicts every entry not got or inserted since `mark`.
    pub fn evict_unused_since(&mut self, mark: u64) {
        while let Some(entry) = self.ages.first_entry() {
            if *entry.key() > mark {
                break;
            }
            let oldest = entry.remove();
            if let Some(entry) = self.entries.remove(oldest.as_ref()) {
                self.bytes -= entry.bytes;
            }
            self.evictions += 1;
        }
    }
    pub fn clear(&mut self) {
        self.entries = BTreeMap::new();
        self.ages = BTreeMap::new();
        self.bytes = 0;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn evictions(&self) -> u64 {
        self.evictions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_replacement_oversize_and_disposal() {
        let mut cache = ByteCache::new(4096);
        assert!(cache.insert(1, "a", 600));
        let entry_bytes = cache.bytes();
        assert!(cache.insert(2, "b", 600));
        assert_eq!(cache.get(&1), Some(&"a"));
        // Exactly two entries fit.
        cache.limit = 2 * entry_bytes;
        assert!(cache.insert(3, "c", 600));
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.get(&1), Some(&"a"));
        assert_eq!(cache.evictions(), 1);
        assert!(cache.insert(1, "d", 0));
        assert!(cache.bytes() < 2 * entry_bytes);
        assert!(!cache.insert(4, "large", usize::MAX));
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert_eq!((cache.len(), cache.bytes()), (0, 0));
    }

    #[test]
    fn lowering_the_limit_evicts_the_least_recently_used_first() {
        let mut cache = ByteCache::new(1 << 20);
        for key in 0..10 {
            assert!(cache.insert(key, key, 600));
        }
        let entry = cache.bytes() / 10;
        assert_eq!(cache.get(&0), Some(&0));
        cache.set_limit(3 * entry);
        assert_eq!(cache.len(), 3);
        assert!(cache.bytes() <= 3 * entry);
        assert_eq!(cache.get(&0), Some(&0));
        assert_eq!(cache.get(&9), Some(&9));
        assert_eq!(cache.get(&1), None);
        cache.set_limit(0);
        assert_eq!((cache.len(), cache.bytes()), (0, 0));
    }

    #[test]
    fn repeated_sessions_never_accumulate_cache_storage() {
        let mut cache = ByteCache::new(8192);
        for session in 0..100 {
            for key in 0..200 {
                cache.insert(key, vec![session; key * 10], key * 10);
                assert!(cache.bytes() <= 8192);
            }
            cache.clear();
            assert_eq!(cache.bytes(), 0);
        }
    }
}

// Structural accounting avoids allocating JSON or scanning string contents on
// the UI thread. Enum storage is deliberately conservative so boxed variants
// and sparse containers are covered even though serde erases Box/Arc wrappers.
thread_local! { static MEASURING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub(crate) fn measuring() -> bool {
    MEASURING.with(std::cell::Cell::get)
}

#[derive(Debug, thiserror::Error)]
#[error("resource structure exceeds the accounting depth limit")]
struct MeasureError;
impl serde::ser::Error for MeasureError {
    fn custom<T: std::fmt::Display>(_: T) -> Self {
        Self
    }
}
struct Meter {
    depth: usize,
}
struct Compound {
    depth: usize,
    heap: usize,
    sequence: bool,
    map: bool,
}

/// A conservative charge for compact owned values. Callers clone incoming
/// descriptions before retaining them, so unused caller capacities and closure
/// captures remain owned by the caller. Opaque callback environments are outside
/// this contract; callback handles and their registry entries are bounded.
pub(crate) fn measure(value: &impl serde::Serialize) -> usize {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            MEASURING.with(|flag| flag.set(self.0));
        }
    }
    let _restore = Restore(MEASURING.with(|flag| flag.replace(true)));
    value
        .serialize(Meter { depth: 0 })
        .unwrap_or(usize::MAX)
        .saturating_add(size_of_val(value))
}

impl Meter {
    fn compound(self, heap: usize, sequence: bool, map: bool) -> Result<Compound, MeasureError> {
        if self.depth > 128 {
            return Err(MeasureError);
        }
        Ok(Compound {
            depth: self.depth + 1,
            heap,
            sequence,
            map,
        })
    }
}
macro_rules! scalar {
    ($($method:ident: $ty:ty),*) => { $(fn $method(self, _: $ty) -> Result<usize, MeasureError> { Ok(0) })* };
}
impl serde::Serializer for Meter {
    type Ok = usize;
    type Error = MeasureError;
    type SerializeSeq = Compound;
    type SerializeTuple = Compound;
    type SerializeTupleStruct = Compound;
    type SerializeTupleVariant = Compound;
    type SerializeMap = Compound;
    type SerializeStruct = Compound;
    type SerializeStructVariant = Compound;
    scalar!(serialize_bool: bool, serialize_i8: i8, serialize_i16: i16, serialize_i32: i32,
        serialize_i64: i64, serialize_u8: u8, serialize_u16: u16, serialize_u32: u32,
        serialize_u64: u64, serialize_f32: f32, serialize_f64: f64, serialize_char: char,
        serialize_i128: i128, serialize_u128: u128);
    fn serialize_str(self, value: &str) -> Result<usize, MeasureError> {
        Ok(value.len())
    }
    fn serialize_bytes(self, value: &[u8]) -> Result<usize, MeasureError> {
        Ok(value.len())
    }
    fn serialize_none(self) -> Result<usize, MeasureError> {
        Ok(0)
    }
    fn serialize_some<T: serde::Serialize + ?Sized>(
        self,
        value: &T,
    ) -> Result<usize, MeasureError> {
        if self.depth > 128 {
            return Err(MeasureError);
        }
        value.serialize(Meter {
            depth: self.depth + 1,
        })
    }
    fn serialize_unit(self) -> Result<usize, MeasureError> {
        Ok(0)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<usize, MeasureError> {
        Ok(0)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<usize, MeasureError> {
        Ok(1024)
    }
    fn serialize_newtype_struct<T: serde::Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<usize, MeasureError> {
        if self.depth > 128 {
            return Err(MeasureError);
        }
        Ok(value
            .serialize(Meter {
                depth: self.depth + 1,
            })?
            .saturating_add(size_of_val(value)))
    }
    fn serialize_newtype_variant<T: serde::Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        value: &T,
    ) -> Result<usize, MeasureError> {
        if self.depth > 128 {
            return Err(MeasureError);
        }
        Ok(value
            .serialize(Meter {
                depth: self.depth + 1,
            })?
            .saturating_add(size_of_val(value))
            .saturating_add(1024))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Compound, MeasureError> {
        self.compound(32, true, false)
    }
    fn serialize_tuple(self, _: usize) -> Result<Compound, MeasureError> {
        self.compound(0, true, false)
    }
    fn serialize_tuple_struct(
        self,
        name: &'static str,
        size: usize,
    ) -> Result<Compound, MeasureError> {
        self.compound(
            if name == "__gui_owned_bytes" { size } else { 0 },
            true,
            false,
        )
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Compound, MeasureError> {
        self.compound(1024, true, false)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Compound, MeasureError> {
        self.compound(64, true, true)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Compound, MeasureError> {
        self.compound(0, false, false)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Compound, MeasureError> {
        self.compound(1024, false, false)
    }
}
impl Compound {
    fn add<T: serde::Serialize + ?Sized>(&mut self, value: &T) -> Result<(), MeasureError> {
        let heap = value.serialize(Meter { depth: self.depth })?;
        self.heap = self.heap.saturating_add(heap);
        if self.sequence {
            self.heap = self
                .heap
                .saturating_add(size_of_val(value).saturating_mul(4));
        }
        if self.map {
            self.heap = self.heap.saturating_add(32);
        }
        Ok(())
    }
}
macro_rules! compound {
    ($trait:ident, $method:ident $(, $key:ident: $ty:ty)?) => {
        impl serde::ser::$trait for Compound {
            type Ok = usize; type Error = MeasureError;
            fn $method<T: serde::Serialize + ?Sized>(&mut self, $( $key: $ty,)? value: &T) -> Result<(), MeasureError> { $(let _ = $key;)? self.add(value) }
            fn end(self) -> Result<usize, MeasureError> { Ok(self.heap) }
        }
    };
}
compound!(SerializeSeq, serialize_element);
compound!(SerializeTuple, serialize_element);
compound!(SerializeTupleStruct, serialize_field);
compound!(SerializeTupleVariant, serialize_field);
compound!(SerializeStruct, serialize_field, key: &'static str);
compound!(SerializeStructVariant, serialize_field, key: &'static str);
impl serde::ser::SerializeMap for Compound {
    type Ok = usize;
    type Error = MeasureError;
    fn serialize_key<T: serde::Serialize + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), MeasureError> {
        self.add(value)
    }
    fn serialize_value<T: serde::Serialize + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), MeasureError> {
        self.add(value)
    }
    fn end(self) -> Result<usize, MeasureError> {
        Ok(self.heap)
    }
}

/// A shared storage budget. Reservations travel with retained values so caches,
/// workers, inboxes and all windows compete for the same session allowance.
#[derive(Clone, Debug)]
pub struct Budget(std::sync::Arc<parking_lot::Mutex<BudgetState>>);
#[derive(Debug)]
struct BudgetState {
    used: usize,
    limit: usize,
}
#[derive(Debug)]
pub(crate) struct Lease {
    budget: Budget,
    bytes: usize,
}
impl Budget {
    pub fn new(limit: usize) -> Self {
        Self(std::sync::Arc::new(parking_lot::Mutex::new(BudgetState {
            used: 0,
            limit,
        })))
    }
    pub(crate) fn identity(&self) -> usize {
        std::sync::Arc::as_ptr(&self.0) as usize
    }
    pub fn usage(&self) -> (usize, usize) {
        let state = self.0.lock();
        (state.used, state.limit)
    }
    pub fn set_limit(&self, limit: usize) -> Result<(), String> {
        let mut state = self.0.lock();
        if state.used > limit {
            return Err("retained resources exceed the requested byte budget".into());
        }
        state.limit = limit;
        Ok(())
    }
    pub(crate) fn reserve(&self, bytes: usize) -> Result<Lease, String> {
        let mut state = self.0.lock();
        if bytes > state.limit.saturating_sub(state.used) {
            return Err("aggregate session byte budget exhausted".into());
        }
        state.used += bytes;
        Ok(Lease {
            budget: self.clone(),
            bytes,
        })
    }
}
impl Lease {
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
    pub(crate) fn resize(&mut self, bytes: usize) -> Result<(), String> {
        let mut state = self.budget.0.lock();
        if bytes > self.bytes && bytes - self.bytes > state.limit.saturating_sub(state.used) {
            return Err("aggregate session byte budget exhausted".into());
        }
        state.used = state.used - self.bytes + bytes;
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.budget.0.lock().used -= self.bytes;
    }
}
impl PartialEq for Budget {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}
impl PartialEq for Lease {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

#[cfg(test)]
mod reservation_tests {
    use super::*;
    #[test]
    fn shared_reservations_are_atomic_resize_without_losing_old_capacity_and_release() {
        let budget = Budget::new(100);
        let mut first = budget.reserve(60).unwrap();
        let second = budget.reserve(30).unwrap();
        assert!(budget.reserve(11).is_err());
        assert!(first.resize(71).is_err());
        assert_eq!(budget.usage(), (90, 100));
        first.resize(20).unwrap();
        assert_eq!(budget.usage().0, 50);
        drop(second);
        first.resize(100).unwrap();
        assert!(budget.set_limit(99).is_err());
        drop(first);
        assert_eq!(budget.usage(), (0, 100));
    }

    #[test]
    fn concurrent_reservations_cannot_overcommit_the_shared_budget() {
        let budget = Budget::new(4096);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let budget = budget.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                let lease = budget.reserve(1024).ok();
                barrier.wait();
                assert!(budget.usage().0 <= 4096);
                barrier.wait();
                usize::from(lease.is_some())
            }));
        }
        let accepted: usize = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .sum();
        assert_eq!(accepted, 4);
        assert_eq!(budget.usage().0, 0);
    }
}
