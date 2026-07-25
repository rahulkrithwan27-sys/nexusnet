//! Content-addressed deduplication.
//!
//! Network traffic repeats itself: the same asset, the same config blob, the
//! same response to a polling client. Sending it once and thereafter sending a
//! short digest is the single largest bandwidth saving available, larger than
//! any compression ratio, because the best case is not "smaller" but "nothing
//! at all".
//!
//! [`Deduplicator`] tracks what a peer has already received. The first time a
//! payload is offered it returns the bytes; every subsequent time it returns a
//! [`Reference`] the peer can resolve from its own store.
//!
//! ## On the digest
//!
//! This uses a 128-bit `FxHash`-style digest, which is fast and dependency-free
//! but **not cryptographic**. It is appropriate when both peers are trusted and
//! the concern is accidental collision, not a deliberate one. An attacker who
//! can choose payloads could construct a collision and cause the wrong content
//! to be served. Before deduplicating across a trust boundary, switch to a
//! cryptographic digest — that belongs with the encryption work in Phase 4,
//! where a hash primitive is already a dependency.

use std::collections::HashMap;

use bytes::Bytes;

use crate::lru::LruCache;

/// A 128-bit content digest.
///
/// Not cryptographically secure; see the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(u128);

impl Digest {
    /// Computes the digest of `data`.
    ///
    /// Two independent 64-bit accumulators are combined so that the 128-bit
    /// result is not merely a padded 64-bit hash; the second is seeded and
    /// stepped differently from the first.
    #[must_use]
    pub fn of(data: &[u8]) -> Self {
        const SEED_A: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        const SEED_B: u64 = 0x2545_F491_4F6C_DD1D;
        const PRIME_A: u64 = 0x0100_0000_01b3;
        const PRIME_B: u64 = 0x9E37_79B9_7F4A_7C15;

        let mut a = SEED_A ^ (data.len() as u64);
        let mut b = SEED_B.wrapping_add(data.len() as u64);

        for (index, &byte) in data.iter().enumerate() {
            a = (a ^ u64::from(byte)).wrapping_mul(PRIME_A);
            a = a.rotate_left(13);

            b = b.wrapping_add(u64::from(byte).wrapping_mul(index as u64 | 1));
            b = (b ^ (b >> 29)).wrapping_mul(PRIME_B);
        }

        // Final avalanche so small inputs still spread across the whole width.
        a ^= a >> 33;
        a = a.wrapping_mul(0xff51_afd7_ed55_8ccd);
        a ^= a >> 33;

        b ^= b >> 31;
        b = b.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        b ^= b >> 31;

        Self((u128::from(a) << 64) | u128::from(b))
    }

    /// Returns the digest as a 128-bit integer.
    #[must_use]
    pub const fn as_u128(self) -> u128 {
        self.0
    }

    /// Returns the digest as 16 big-endian bytes, ready for the wire.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 16] {
        self.0.to_be_bytes()
    }

    /// Reconstructs a digest from 16 big-endian bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(u128::from_be_bytes(bytes))
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// What to transmit for a given payload.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reference {
    /// The peer has not seen this content; send the bytes.
    Content {
        /// The content digest, so the peer can index what it receives.
        digest: Digest,
        /// The payload itself.
        data: Bytes,
    },
    /// The peer already holds this content; send only the digest.
    Cached {
        /// The digest identifying content the peer already has.
        digest: Digest,
        /// The size the peer will reconstruct, useful for accounting.
        original_len: usize,
    },
}

impl Reference {
    /// Returns the content digest either way.
    #[must_use]
    pub const fn digest(&self) -> Digest {
        match self {
            Self::Content { digest, .. } | Self::Cached { digest, .. } => *digest,
        }
    }

    /// Returns `true` when only a digest needs to travel.
    #[must_use]
    pub const fn is_cached(&self) -> bool {
        matches!(self, Self::Cached { .. })
    }

    /// Returns the number of bytes this reference puts on the wire.
    ///
    /// A cached reference costs only the digest.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        match self {
            Self::Content { data, .. } => data.len() + 16,
            Self::Cached { .. } => 16,
        }
    }

    /// Returns the bytes saved compared with sending the payload in full.
    #[must_use]
    pub fn bytes_saved(&self) -> usize {
        match self {
            Self::Content { .. } => 0,
            Self::Cached { original_len, .. } => original_len.saturating_sub(16),
        }
    }
}

/// A snapshot of deduplication activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DedupStats {
    /// Payloads offered for transmission.
    pub offered: u64,
    /// Payloads that were already known to the peer.
    pub deduplicated: u64,
    /// Total bytes saved by sending digests instead of content.
    pub bytes_saved: u64,
    /// Distinct payloads currently tracked.
    pub tracked: usize,
}

impl DedupStats {
    /// Returns the fraction of offered payloads that were deduplicated.
    ///
    /// Returns `0.0` before anything has been offered.
    #[must_use]
    pub fn dedup_ratio(&self) -> f64 {
        if self.offered == 0 {
            0.0
        } else {
            self.deduplicated as f64 / self.offered as f64
        }
    }
}

/// Tracks which payloads a peer already holds.
///
/// # Examples
///
/// ```
/// use bytes::Bytes;
/// use nexusnet_cache::Deduplicator;
///
/// let mut dedup = Deduplicator::new(1024);
/// // Comfortably above the 64-byte threshold below which a digest saves nothing.
/// let payload = Bytes::from(vec![b'x'; 4096]);
///
/// // First time: the content must travel.
/// let first = dedup.offer(payload.clone());
/// assert!(!first.is_cached());
///
/// // Second time: only the digest, saving 4096 - 16 bytes.
/// let second = dedup.offer(payload);
/// assert!(second.is_cached());
/// assert_eq!(second.wire_len(), 16);
/// assert_eq!(first.digest(), second.digest());
/// ```
#[derive(Debug)]
pub struct Deduplicator {
    seen: LruCache<Digest, usize>,
    offered: u64,
    deduplicated: u64,
    bytes_saved: u64,
    min_size: usize,
}

/// The default minimum payload size worth deduplicating.
///
/// A digest is 16 bytes, so deduplicating anything smaller than this cannot
/// save meaningfully and only adds bookkeeping.
pub const DEFAULT_MIN_DEDUP_SIZE: usize = 64;

impl Deduplicator {
    /// Creates a deduplicator remembering up to `capacity` distinct payloads.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            seen: LruCache::new(capacity),
            offered: 0,
            deduplicated: 0,
            bytes_saved: 0,
            min_size: DEFAULT_MIN_DEDUP_SIZE,
        }
    }

    /// Sets the smallest payload worth deduplicating.
    #[must_use]
    pub const fn with_min_size(mut self, min_size: usize) -> Self {
        self.min_size = min_size;
        self
    }

    /// Returns a snapshot of deduplication activity.
    #[must_use]
    pub fn stats(&self) -> DedupStats {
        DedupStats {
            offered: self.offered,
            deduplicated: self.deduplicated,
            bytes_saved: self.bytes_saved,
            tracked: self.seen.len(),
        }
    }

    /// Decides how `data` should be transmitted.
    ///
    /// Payloads below the minimum size are always sent as content, since a
    /// digest would not be smaller.
    pub fn offer(&mut self, data: Bytes) -> Reference {
        self.offered += 1;
        let digest = Digest::of(&data);

        if data.len() < self.min_size {
            return Reference::Content { digest, data };
        }

        if let Some(&original_len) = self.seen.get(&digest) {
            self.deduplicated += 1;
            let reference = Reference::Cached {
                digest,
                original_len,
            };
            self.bytes_saved += reference.bytes_saved() as u64;

            return reference;
        }

        self.seen.insert(digest, data.len());

        Reference::Content { digest, data }
    }

    /// Returns `true` if the peer is known to hold this content.
    #[must_use]
    pub fn contains(&self, digest: Digest) -> bool {
        self.seen.contains_key(&digest)
    }

    /// Forgets everything, as after a peer reconnects with a cold cache.
    pub fn reset(&mut self) {
        self.seen.clear();
    }
}

/// The receiving side of deduplication: resolves digests back to content.
///
/// A sender's [`Deduplicator`] and a receiver's [`DedupStore`] must agree about
/// what the receiver holds. If they disagree — after a restart, say — the
/// receiver reports a miss and the sender should resend the content.
#[derive(Debug, Default)]
pub struct DedupStore {
    contents: HashMap<Digest, Bytes>,
}

impl DedupStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of stored payloads.
    #[must_use]
    pub fn len(&self) -> usize {
        self.contents.len()
    }

    /// Returns `true` if nothing is stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.contents.is_empty()
    }

    /// Resolves a received reference to its content.
    ///
    /// Returns `None` for a cached reference this store does not hold, which
    /// means the sender's view is stale and the content must be requested.
    pub fn resolve(&mut self, reference: &Reference) -> Option<Bytes> {
        match reference {
            Reference::Content { digest, data } => {
                self.contents.insert(*digest, data.clone());
                Some(data.clone())
            }
            Reference::Cached { digest, .. } => self.contents.get(digest).cloned(),
        }
    }

    /// Removes everything.
    pub fn clear(&mut self) {
        self.contents.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_content_hashes_identically() {
        let a = Digest::of(b"the same bytes");
        let b = Digest::of(b"the same bytes");
        assert_eq!(a, b);
    }

    #[test]
    fn different_content_hashes_differently() {
        assert_ne!(Digest::of(b"alpha"), Digest::of(b"beta"));
        assert_ne!(Digest::of(b""), Digest::of(b"\0"));
        // Transpositions must change the digest, which a plain sum would not.
        assert_ne!(Digest::of(b"ab"), Digest::of(b"ba"));
    }

    #[test]
    fn digests_survive_a_wire_round_trip() {
        let digest = Digest::of(b"payload");
        assert_eq!(Digest::from_bytes(digest.to_bytes()), digest);
        assert_eq!(digest.to_string().len(), 32);
    }

    #[test]
    fn digest_spreads_across_the_full_width() {
        // A padded 64-bit hash would leave one half constant across inputs.
        let mut high_bits = std::collections::HashSet::new();
        let mut low_bits = std::collections::HashSet::new();

        for i in 0..64_u32 {
            let value = Digest::of(&i.to_be_bytes()).as_u128();
            high_bits.insert((value >> 64) as u64);
            low_bits.insert(value as u64);
        }

        assert!(high_bits.len() > 60, "high half barely varies");
        assert!(low_bits.len() > 60, "low half barely varies");
    }

    #[test]
    fn repeated_payloads_are_deduplicated() {
        let mut dedup = Deduplicator::new(16);
        let payload = Bytes::from(vec![b'x'; 1000]);

        let first = dedup.offer(payload.clone());
        assert!(!first.is_cached());
        assert_eq!(first.bytes_saved(), 0);

        let second = dedup.offer(payload.clone());
        assert!(second.is_cached());
        assert_eq!(second.bytes_saved(), 1000 - 16);
        assert_eq!(second.wire_len(), 16);

        let stats = dedup.stats();
        assert_eq!(stats.offered, 2);
        assert_eq!(stats.deduplicated, 1);
        assert!((stats.dedup_ratio() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn small_payloads_are_not_deduplicated() {
        let mut dedup = Deduplicator::new(16);
        let tiny = Bytes::from_static(b"ok");

        assert!(!dedup.offer(tiny.clone()).is_cached());
        assert!(
            !dedup.offer(tiny).is_cached(),
            "a digest is not smaller than a two-byte payload"
        );
    }

    #[test]
    fn distinct_payloads_are_not_confused() {
        let mut dedup = Deduplicator::new(16);
        let first = Bytes::from(vec![b'a'; 200]);
        let second = Bytes::from(vec![b'b'; 200]);

        assert!(!dedup.offer(first).is_cached());
        assert!(!dedup.offer(second).is_cached());
        assert_eq!(dedup.stats().deduplicated, 0);
    }

    #[test]
    fn resetting_forgets_everything() {
        let mut dedup = Deduplicator::new(16);
        let payload = Bytes::from(vec![b'z'; 500]);

        dedup.offer(payload.clone());
        assert!(dedup.offer(payload.clone()).is_cached());

        dedup.reset();
        assert!(
            !dedup.offer(payload).is_cached(),
            "after a reset the peer is assumed to hold nothing"
        );
    }

    #[test]
    fn the_store_resolves_both_kinds_of_reference() {
        let mut dedup = Deduplicator::new(16);
        let mut store = DedupStore::new();
        let payload = Bytes::from(vec![b'q'; 300]);

        let first = dedup.offer(payload.clone());
        assert_eq!(store.resolve(&first), Some(payload.clone()));

        let second = dedup.offer(payload.clone());
        assert!(second.is_cached());
        assert_eq!(
            store.resolve(&second),
            Some(payload),
            "a cached reference resolves from what was stored earlier"
        );
    }

    #[test]
    fn a_stale_reference_reports_a_miss() {
        let mut dedup = Deduplicator::new(16);
        let mut store = DedupStore::new();
        let payload = Bytes::from(vec![b'r'; 300]);

        dedup.offer(payload.clone());
        let cached = dedup.offer(payload);

        // The receiver restarted and lost its store.
        store.clear();
        assert_eq!(
            store.resolve(&cached),
            None,
            "a miss must be reported so the sender can resend"
        );
    }

    #[test]
    fn capacity_bounds_what_is_remembered() {
        let mut dedup = Deduplicator::new(2);

        for i in 0..5_u8 {
            dedup.offer(Bytes::from(vec![i; 200]));
        }

        assert!(
            dedup.stats().tracked <= 2,
            "tracked {} exceeds capacity",
            dedup.stats().tracked
        );
    }
}
