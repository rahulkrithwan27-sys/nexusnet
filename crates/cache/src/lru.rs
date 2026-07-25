//! An LRU cache with optional per-entry expiry and byte-aware capacity.
//!
//! Two limits matter for a network cache, and most implementations only offer
//! one. Bounding the *number* of entries says nothing about memory when entry
//! sizes vary by four orders of magnitude, which they do for network payloads.
//! Bounding *bytes* alone lets a flood of tiny entries balloon the bookkeeping.
//! [`LruCache`] enforces both, and evicts by least-recent use until each is
//! satisfied.

use std::borrow::Borrow;
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::time::{Duration, Instant};

/// Measures how much of a cache's byte budget a value consumes.
///
/// Implemented for the common payload types; implement it for your own value
/// type to make byte-aware capacity meaningful.
pub trait Weight {
    /// Returns this value's size in bytes.
    fn weight(&self) -> usize;
}

impl Weight for Vec<u8> {
    fn weight(&self) -> usize {
        self.len()
    }
}

impl Weight for bytes::Bytes {
    fn weight(&self) -> usize {
        self.len()
    }
}

impl Weight for String {
    fn weight(&self) -> usize {
        self.len()
    }
}

impl Weight for &[u8] {
    fn weight(&self) -> usize {
        self.len()
    }
}

/// Fixed-size values weigh their own size; useful when the cache stores
/// metadata (such as a length) rather than a payload.
macro_rules! impl_weight_for_scalar {
    ($($ty:ty),* $(,)?) => {
        $(
            impl Weight for $ty {
                fn weight(&self) -> usize {
                    std::mem::size_of::<Self>()
                }
            }
        )*
    };
}

impl_weight_for_scalar!(usize, u8, u16, u32, u64, u128, i32, i64, ());

/// Why an entry left the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EvictionReason {
    /// Displaced because the cache exceeded its entry or byte capacity.
    Capacity,
    /// Removed because its time to live had elapsed.
    Expired,
    /// Removed explicitly by the caller.
    Removed,
}

/// A snapshot of cache activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct CacheStats {
    /// Lookups that found a live entry.
    pub hits: u64,
    /// Lookups that found nothing.
    pub misses: u64,
    /// Entries evicted to stay within capacity.
    pub evictions: u64,
    /// Entries dropped because they had expired.
    pub expirations: u64,
    /// Entries currently held.
    pub entries: usize,
    /// Bytes currently held, by the [`Weight`] of each value.
    pub bytes: usize,
}

impl CacheStats {
    /// Returns the fraction of lookups that were hits.
    ///
    /// Returns `0.0` before any lookup, rather than dividing by zero.
    #[must_use]
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// An entry together with its bookkeeping.
#[derive(Debug)]
struct Entry<V> {
    value: V,
    weight: usize,
    /// Monotonic counter recording the last use; higher is more recent.
    last_used: u64,
    expires_at: Option<Instant>,
}

impl<V> Entry<V> {
    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|deadline| now >= deadline)
    }
}

/// An LRU cache with optional expiry and byte-aware capacity.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use nexusnet_cache::LruCache;
///
/// let mut cache: LruCache<String, Vec<u8>> = LruCache::new(2);
///
/// cache.insert("a".to_owned(), vec![1, 2, 3]);
/// cache.insert("b".to_owned(), vec![4, 5, 6]);
///
/// // Touching "a" makes "b" the least recently used.
/// assert!(cache.get("a").is_some());
/// cache.insert("c".to_owned(), vec![7, 8, 9]);
///
/// assert!(cache.get("a").is_some());
/// assert!(cache.get("b").is_none(), "the least recently used entry is evicted");
/// assert!(cache.get("c").is_some());
/// ```
#[derive(Debug)]
pub struct LruCache<K, V> {
    entries: HashMap<K, Entry<V>>,
    /// Maps use-order to key, so the least recently used entry is the first
    /// element rather than the result of a full scan. Without this, every
    /// eviction would be O(n).
    order: BTreeMap<u64, K>,
    max_entries: usize,
    max_bytes: Option<usize>,
    default_ttl: Option<Duration>,
    bytes: usize,
    /// How many entries carry an expiry. When zero, expiry scanning is skipped
    /// entirely — otherwise every insert would pay an O(n) sweep.
    ttl_entries: usize,
    clock: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
    expirations: u64,
}

impl<K, V> LruCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Weight,
{
    /// Creates a cache holding at most `max_entries` entries.
    ///
    /// A capacity of zero is raised to one; a cache that can hold nothing would
    /// silently discard every insert.
    #[must_use]
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            max_entries: max_entries.max(1),
            max_bytes: None,
            default_ttl: None,
            bytes: 0,
            ttl_entries: 0,
            clock: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            expirations: 0,
        }
    }

    /// Also bounds the cache by total bytes.
    ///
    /// A single value larger than this budget is still stored — refusing it
    /// would turn a cache into a silent failure — but it will evict everything
    /// else to make room.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = Some(max_bytes);
        self
    }

    /// Applies a default time to live to entries inserted without one.
    #[must_use]
    pub fn with_default_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = Some(ttl);
        self
    }

    /// Returns the number of live entries, excluding expired ones.
    #[must_use]
    pub fn len(&self) -> usize {
        if self.ttl_entries == 0 {
            return self.entries.len();
        }

        let now = Instant::now();
        self.entries.values().filter(|e| !e.is_expired(now)).count()
    }

    /// Returns `true` if no live entry remains.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the total weight of stored values.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// Returns the entry capacity.
    #[must_use]
    pub const fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Returns a snapshot of cache activity.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            expirations: self.expirations,
            entries: self.entries.len(),
            bytes: self.bytes,
        }
    }

    /// Inserts a value, returning the previous one if the key was present.
    ///
    /// Uses the cache's default time to live, if one is configured.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.insert_inner(key, value, self.default_ttl)
    }

    /// Inserts a value that expires after `ttl`, overriding any default.
    pub fn insert_with_ttl(&mut self, key: K, value: V, ttl: Duration) -> Option<V> {
        self.insert_inner(key, value, Some(ttl))
    }

    fn insert_inner(&mut self, key: K, value: V, ttl: Option<Duration>) -> Option<V> {
        let weight = value.weight();
        let expires_at = ttl.map(|ttl| Instant::now() + ttl);

        self.clock += 1;
        let entry = Entry {
            value,
            weight,
            last_used: self.clock,
            expires_at,
        };

        if expires_at.is_some() {
            self.ttl_entries += 1;
        }
        self.order.insert(self.clock, key.clone());

        let previous = self.entries.insert(key, entry).map(|old| {
            self.bytes = self.bytes.saturating_sub(old.weight);
            self.order.remove(&old.last_used);
            if old.expires_at.is_some() {
                self.ttl_entries = self.ttl_entries.saturating_sub(1);
            }
            old.value
        });

        self.bytes += weight;
        self.enforce_capacity();

        previous
    }

    /// Returns a reference to a live value, marking it recently used.
    ///
    /// An expired entry is treated as absent and removed.
    pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let now = Instant::now();

        let expired = match self.entries.get(key) {
            Some(entry) => entry.is_expired(now),
            None => {
                self.misses += 1;
                return None;
            }
        };

        if expired {
            if let Some(entry) = self.entries.remove(key) {
                self.bytes = self.bytes.saturating_sub(entry.weight);
                self.order.remove(&entry.last_used);
                self.ttl_entries = self.ttl_entries.saturating_sub(1);
                self.expirations += 1;
            }
            self.misses += 1;
            return None;
        }

        self.clock += 1;
        let clock = self.clock;
        self.hits += 1;

        let entry = self.entries.get_mut(key)?;
        let previous_clock = entry.last_used;
        entry.last_used = clock;

        if let Some(owned_key) = self.order.remove(&previous_clock) {
            self.order.insert(clock, owned_key);
        }

        self.entries.get(key).map(|entry| &entry.value)
    }

    /// Returns `true` if a live entry exists, without marking it used.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let now = Instant::now();
        self.entries
            .get(key)
            .is_some_and(|entry| !entry.is_expired(now))
    }

    /// Removes an entry, returning its value if it was live.
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let entry = self.entries.remove(key)?;
        self.bytes = self.bytes.saturating_sub(entry.weight);
        self.order.remove(&entry.last_used);
        if entry.expires_at.is_some() {
            self.ttl_entries = self.ttl_entries.saturating_sub(1);
        }

        if entry.is_expired(Instant::now()) {
            self.expirations += 1;
            None
        } else {
            Some(entry.value)
        }
    }

    /// Removes every entry.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
        self.ttl_entries = 0;
    }

    /// Drops all expired entries, returning how many were removed.
    ///
    /// Expiry is otherwise lazy — an expired entry costs nothing until it is
    /// looked at — so call this periodically if entries may go untouched for a
    /// long time.
    pub fn purge_expired(&mut self) -> usize {
        if self.ttl_entries == 0 {
            return 0;
        }

        let now = Instant::now();
        let before = self.entries.len();
        let mut freed = 0_usize;
        let mut stale_clocks = Vec::new();

        self.entries.retain(|_, entry| {
            if entry.is_expired(now) {
                freed += entry.weight;
                stale_clocks.push(entry.last_used);
                false
            } else {
                true
            }
        });

        for clock in stale_clocks {
            self.order.remove(&clock);
        }

        let removed = before - self.entries.len();
        self.bytes = self.bytes.saturating_sub(freed);
        self.ttl_entries = self.ttl_entries.saturating_sub(removed);
        self.expirations += removed as u64;

        removed
    }

    /// Evicts until both the entry and byte limits are satisfied.
    fn enforce_capacity(&mut self) {
        // Expired entries are dead weight; clear them before evicting live ones.
        if self.over_capacity() {
            self.purge_expired();
        }

        while self.over_capacity() && self.entries.len() > 1 {
            let Some(victim) = self.least_recently_used() else {
                break;
            };

            if let Some(entry) = self.entries.remove(&victim) {
                self.bytes = self.bytes.saturating_sub(entry.weight);
                self.order.remove(&entry.last_used);
                if entry.expires_at.is_some() {
                    self.ttl_entries = self.ttl_entries.saturating_sub(1);
                }
                self.evictions += 1;
            }
        }
    }

    fn over_capacity(&self) -> bool {
        if self.entries.len() > self.max_entries {
            return true;
        }

        self.max_bytes.is_some_and(|max| self.bytes > max)
    }

    /// Returns the least recently used key in `O(log n)` via the order index.
    fn least_recently_used(&self) -> Option<K> {
        self.order.values().next().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(max: usize) -> LruCache<String, Vec<u8>> {
        LruCache::new(max)
    }

    #[test]
    fn stores_and_retrieves() {
        let mut cache = cache(4);
        cache.insert("key".to_owned(), vec![1, 2, 3]);

        assert_eq!(cache.get("key"), Some(&vec![1, 2, 3]));
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 0);
    }

    #[test]
    fn reports_misses() {
        let mut cache = cache(4);
        assert!(cache.get("absent").is_none());
        assert_eq!(cache.stats().misses, 1);
        assert!((cache.stats().hit_ratio() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn evicts_least_recently_used() {
        let mut cache = cache(2);
        cache.insert("a".to_owned(), vec![1]);
        cache.insert("b".to_owned(), vec![2]);

        // Touch "a" so "b" becomes the eviction candidate.
        assert!(cache.get("a").is_some());
        cache.insert("c".to_owned(), vec![3]);

        assert!(cache.contains_key("a"));
        assert!(!cache.contains_key("b"));
        assert!(cache.contains_key("c"));
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn replacing_a_key_does_not_double_count_bytes() {
        let mut cache = cache(4);
        cache.insert("k".to_owned(), vec![0; 100]);
        assert_eq!(cache.bytes(), 100);

        let previous = cache.insert("k".to_owned(), vec![0; 30]);
        assert_eq!(previous, Some(vec![0; 100]));
        assert_eq!(cache.bytes(), 30, "the old weight must be released");
    }

    #[test]
    fn byte_capacity_is_enforced() {
        let mut cache = LruCache::<String, Vec<u8>>::new(100).with_max_bytes(250);

        cache.insert("a".to_owned(), vec![0; 100]);
        cache.insert("b".to_owned(), vec![0; 100]);
        assert_eq!(cache.bytes(), 200);

        // Exceeding the byte budget evicts even though the entry count is fine.
        cache.insert("c".to_owned(), vec![0; 100]);
        assert!(cache.bytes() <= 250, "bytes were {}", cache.bytes());
        assert!(cache.stats().evictions >= 1);
    }

    #[test]
    fn an_oversized_value_is_still_stored() {
        let mut cache = LruCache::<String, Vec<u8>>::new(10).with_max_bytes(50);
        cache.insert("small".to_owned(), vec![0; 10]);
        cache.insert("huge".to_owned(), vec![0; 500]);

        assert!(
            cache.contains_key("huge"),
            "refusing an oversized value would be a silent failure"
        );
    }

    #[test]
    fn expired_entries_read_as_absent() {
        let mut cache = cache(4);
        cache.insert_with_ttl("k".to_owned(), vec![1], Duration::from_millis(20));
        assert!(cache.contains_key("k"));

        std::thread::sleep(Duration::from_millis(40));

        assert!(cache.get("k").is_none());
        assert_eq!(cache.stats().expirations, 1);
    }

    #[test]
    fn a_default_ttl_applies_to_inserts() {
        let mut cache =
            LruCache::<String, Vec<u8>>::new(4).with_default_ttl(Duration::from_millis(20));
        cache.insert("k".to_owned(), vec![1]);

        std::thread::sleep(Duration::from_millis(40));
        assert!(cache.get("k").is_none());
    }

    #[test]
    fn purging_removes_expired_entries_and_frees_bytes() {
        let mut cache = cache(10);
        cache.insert_with_ttl("short".to_owned(), vec![0; 50], Duration::from_millis(20));
        cache.insert_with_ttl("long".to_owned(), vec![0; 50], Duration::from_secs(30));
        assert_eq!(cache.bytes(), 100);

        std::thread::sleep(Duration::from_millis(40));

        assert_eq!(cache.purge_expired(), 1);
        assert_eq!(cache.bytes(), 50, "expired bytes must be released");
        assert!(cache.contains_key("long"));
    }

    #[test]
    fn removal_returns_the_value_and_frees_bytes() {
        let mut cache = cache(4);
        cache.insert("k".to_owned(), vec![0; 20]);

        assert_eq!(cache.remove("k"), Some(vec![0; 20]));
        assert_eq!(cache.bytes(), 0);
        assert!(cache.remove("k").is_none());
    }

    #[test]
    fn zero_capacity_is_corrected() {
        let mut cache = cache(0);
        assert_eq!(cache.max_entries(), 1);

        cache.insert("k".to_owned(), vec![1]);
        assert!(
            cache.contains_key("k"),
            "a cache must hold at least one entry"
        );
    }

    #[test]
    fn hit_ratio_is_computed() {
        let mut cache = cache(4);
        cache.insert("k".to_owned(), vec![1]);

        let _ = cache.get("k");
        let _ = cache.get("k");
        let _ = cache.get("absent");

        let stats = cache.stats();
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 1);
        assert!((stats.hit_ratio() - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn the_order_index_stays_consistent_under_churn() {
        // Exercises insert, hit, replace, remove, and eviction together; if the
        // order index drifted out of step with the entry map, eviction would
        // start choosing wrong or panicking.
        let mut cache = cache(8);

        for round in 0..200_u32 {
            let key = format!("k{}", round % 12);
            cache.insert(key.clone(), vec![0; 8]);

            if round % 3 == 0 {
                let _ = cache.get(&key);
            }
            if round % 7 == 0 {
                let _ = cache.remove(&format!("k{}", round % 5));
            }
        }

        assert!(cache.len() <= 8, "capacity exceeded: {}", cache.len());
        assert_eq!(
            cache.order.len(),
            cache.entries.len(),
            "the order index must hold exactly one entry per cached item"
        );

        // Every indexed clock must correspond to a live entry.
        for (clock, key) in &cache.order {
            let entry = cache.entries.get(key).expect("indexed key must exist");
            assert_eq!(entry.last_used, *clock, "index and entry disagree");
        }
    }

    #[test]
    fn eviction_order_is_strictly_by_last_use() {
        let mut cache = cache(3);
        cache.insert("a".to_owned(), vec![1]);
        cache.insert("b".to_owned(), vec![2]);
        cache.insert("c".to_owned(), vec![3]);

        // Use them in a deliberate order: b, a, c.
        let _ = cache.get("b");
        let _ = cache.get("a");
        let _ = cache.get("c");

        // "b" is now least recent and must go first.
        cache.insert("d".to_owned(), vec![4]);
        assert!(!cache.contains_key("b"));
        assert!(cache.contains_key("a") && cache.contains_key("c") && cache.contains_key("d"));

        // Then "a".
        cache.insert("e".to_owned(), vec![5]);
        assert!(!cache.contains_key("a"));
    }

    #[test]
    fn the_ttl_counter_tracks_expiring_entries() {
        // The counter gates expiry scanning; if it drifted above zero the cache
        // would sweep needlessly, and if it drifted below, expired entries
        // would never be collected.
        let mut cache = cache(16);

        cache.insert("plain".to_owned(), vec![1]);
        assert_eq!(
            cache.ttl_entries, 0,
            "an entry without a TTL must not count"
        );

        cache.insert_with_ttl("a".to_owned(), vec![1], Duration::from_secs(30));
        cache.insert_with_ttl("b".to_owned(), vec![1], Duration::from_secs(30));
        assert_eq!(cache.ttl_entries, 2);

        // Replacing a TTL entry with a plain one releases the count.
        cache.insert("a".to_owned(), vec![2]);
        assert_eq!(cache.ttl_entries, 1);

        // Explicit removal releases it too.
        assert!(cache.remove("b").is_some());
        assert_eq!(cache.ttl_entries, 0);

        // And clearing resets everything.
        cache.insert_with_ttl("c".to_owned(), vec![1], Duration::from_secs(30));
        cache.clear();
        assert_eq!(cache.ttl_entries, 0);
    }

    #[test]
    fn expiring_entries_are_collected_when_evicted() {
        let mut cache = cache(2);
        cache.insert_with_ttl("a".to_owned(), vec![1], Duration::from_secs(30));
        cache.insert_with_ttl("b".to_owned(), vec![1], Duration::from_secs(30));
        assert_eq!(cache.ttl_entries, 2);

        // Forces an eviction of a TTL-bearing entry.
        cache.insert("c".to_owned(), vec![1]);
        assert_eq!(
            cache.ttl_entries, 1,
            "evicting a TTL entry must release its count"
        );
    }

    #[test]
    fn clearing_empties_the_cache() {
        let mut cache = cache(4);
        cache.insert("a".to_owned(), vec![0; 10]);
        cache.insert("b".to_owned(), vec![0; 10]);

        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.bytes(), 0);
    }
}
