//! Concrete strategies derived from network conditions.
//!
//! Each type here answers one question a caller has to decide anyway, and
//! answers it from measurement rather than a constant:
//!
//! * [`CompressionStrategy`] — compress, how hard, and above what size.
//! * [`CacheStrategy`] — cache, how much, and for how long.
//! * [`DeltaSyncStrategy`] — send differences instead of whole payloads.
//!
//! All three are plain values. They describe what a caller *should* do; nothing
//! here performs any of it.

use std::time::Duration;

use crate::quality::NetworkQuality;

/// The smallest payload worth compressing, in bytes.
///
/// Below this, codec framing overhead exceeds any plausible saving.
pub const MIN_COMPRESSIBLE: usize = 128;

/// The smallest payload worth diffing against a cached base.
///
/// Delta encoding has its own overhead, and below this the difference is
/// unlikely to be smaller than simply resending.
pub const MIN_DELTA_PAYLOAD: usize = 512;

/// How to compress outbound payloads.
///
/// # Examples
///
/// ```
/// use nexusnet_optimizer::{CompressionStrategy, NetworkQuality};
///
/// // A struggling link should trade CPU for bytes.
/// let poor = CompressionStrategy::for_quality(NetworkQuality::Poor);
/// assert!(poor.enabled);
/// assert!(poor.level >= 80);
///
/// // A fast one should not bother.
/// let excellent = CompressionStrategy::for_quality(NetworkQuality::Excellent);
/// assert!(!excellent.enabled);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CompressionStrategy {
    /// Whether to compress at all.
    pub enabled: bool,
    /// The level on the abstract 0–100 scale `nexusnet-compression` uses.
    pub level: u8,
    /// Payloads smaller than this are sent uncompressed regardless.
    pub min_payload: usize,
}

impl CompressionStrategy {
    /// Returns a strategy suited to `quality`.
    ///
    /// The trade is time, not space: compressing pays exactly when the
    /// transmission time saved exceeds the CPU time spent. A degraded link has
    /// slow transmission, so it can afford a lot of CPU; a fast one cannot.
    #[must_use]
    pub const fn for_quality(quality: NetworkQuality) -> Self {
        let (enabled, level) = match quality {
            NetworkQuality::Critical => (true, 95),
            NetworkQuality::Poor => (true, 85),
            NetworkQuality::Fair => (true, 55),
            NetworkQuality::Good => (true, 25),
            // On a fast link most payloads transmit in less time than
            // compressing them would take.
            NetworkQuality::Excellent => (false, 0),
        };

        Self {
            enabled,
            level,
            min_payload: MIN_COMPRESSIBLE,
        }
    }

    /// A strategy that never compresses.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            level: 0,
            min_payload: MIN_COMPRESSIBLE,
        }
    }

    /// Returns `true` if a payload of `len` bytes should be compressed.
    #[must_use]
    pub const fn applies_to(&self, len: usize) -> bool {
        self.enabled && len >= self.min_payload
    }

    /// Returns a copy with a different minimum payload size.
    #[must_use]
    pub const fn with_min_payload(mut self, min_payload: usize) -> Self {
        self.min_payload = min_payload;
        self
    }
}

impl Default for CompressionStrategy {
    fn default() -> Self {
        Self::for_quality(NetworkQuality::default())
    }
}

/// How to use the local cache.
///
/// Caching trades memory for bandwidth, so the worse the link, the more memory
/// is worth spending and the longer entries are worth keeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CacheStrategy {
    /// Whether caching is worth its memory under these conditions.
    pub enabled: bool,
    /// Suggested cache capacity in bytes.
    pub capacity_bytes: usize,
    /// Suggested time to live for entries.
    pub ttl: Duration,
    /// Entries smaller than this are not worth tracking.
    pub min_entry_size: usize,
}

impl CacheStrategy {
    /// Returns a strategy suited to `quality`.
    ///
    /// A degraded link keeps entries far longer: re-fetching is expensive, so
    /// slightly stale data is usually the better trade. A fast link can afford
    /// freshness.
    #[must_use]
    pub const fn for_quality(quality: NetworkQuality) -> Self {
        let (capacity_bytes, ttl_secs) = match quality {
            NetworkQuality::Critical => (64 * 1024 * 1024, 3600),
            NetworkQuality::Poor => (32 * 1024 * 1024, 900),
            NetworkQuality::Fair => (16 * 1024 * 1024, 300),
            NetworkQuality::Good => (8 * 1024 * 1024, 120),
            NetworkQuality::Excellent => (4 * 1024 * 1024, 60),
        };

        Self {
            enabled: true,
            capacity_bytes,
            ttl: Duration::from_secs(ttl_secs),
            min_entry_size: 64,
        }
    }

    /// A strategy that caches nothing.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            capacity_bytes: 0,
            ttl: Duration::ZERO,
            min_entry_size: usize::MAX,
        }
    }

    /// Returns `true` if an entry of `len` bytes is worth caching.
    #[must_use]
    pub const fn applies_to(&self, len: usize) -> bool {
        self.enabled && len >= self.min_entry_size
    }
}

impl Default for CacheStrategy {
    fn default() -> Self {
        Self::for_quality(NetworkQuality::default())
    }
}

/// Whether to send differences instead of whole payloads.
///
/// Delta synchronization is the largest available bandwidth saving when it
/// applies, because an unchanged payload costs nothing but a digest. It is also
/// the most expensive to get wrong: both peers must agree on the base version,
/// and a stale base means resending everything plus the wasted attempt.
// `max_delta_ratio` is an `f64`, so `Eq` is unavailable: float comparison has
// no total order. `PartialEq` is what callers actually need here.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct DeltaSyncStrategy {
    /// Whether delta synchronization is worth attempting.
    pub enabled: bool,
    /// Payloads smaller than this are sent whole.
    pub min_payload: usize,
    /// How old a base version may be before it is refreshed outright.
    pub max_base_age: Duration,
    /// If a computed delta exceeds this fraction of the original, send the
    /// original instead.
    ///
    /// A delta that saves almost nothing still costs both peers the work of
    /// producing and applying it.
    pub max_delta_ratio: f64,
}

impl DeltaSyncStrategy {
    /// Returns a strategy suited to `quality`.
    ///
    /// Delta sync is enabled only from [`Fair`](NetworkQuality::Fair) down. On
    /// a fast link the CPU and bookkeeping cost more than the bytes saved, and
    /// a stale base can cost a whole extra round trip — which a fast link
    /// notices more than the bandwidth.
    #[must_use]
    pub fn for_quality(quality: NetworkQuality) -> Self {
        let enabled = quality.is_degraded();

        let (max_base_age_secs, max_delta_ratio) = match quality {
            NetworkQuality::Critical => (3600, 0.9),
            NetworkQuality::Poor => (900, 0.8),
            NetworkQuality::Fair => (300, 0.7),
            NetworkQuality::Good | NetworkQuality::Excellent => (60, 0.5),
        };

        Self {
            enabled,
            min_payload: MIN_DELTA_PAYLOAD,
            max_base_age: Duration::from_secs(max_base_age_secs),
            max_delta_ratio,
        }
    }

    /// A strategy that never sends deltas.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            min_payload: MIN_DELTA_PAYLOAD,
            max_base_age: Duration::ZERO,
            max_delta_ratio: 0.0,
        }
    }

    /// Returns `true` if a payload of `len` bytes should be diffed.
    #[must_use]
    pub const fn applies_to(&self, len: usize) -> bool {
        self.enabled && len >= self.min_payload
    }

    /// Returns `true` if a delta of `delta_len` is worth sending in place of an
    /// original of `original_len`.
    ///
    /// Returns `false` for a zero-length original, since no saving is possible.
    #[must_use]
    pub fn accepts_delta(&self, delta_len: usize, original_len: usize) -> bool {
        if !self.enabled || original_len == 0 {
            return false;
        }

        (delta_len as f64 / original_len as f64) <= self.max_delta_ratio
    }

    /// Returns `true` if a base captured `age` ago may still be diffed against.
    #[must_use]
    pub fn accepts_base_age(&self, age: Duration) -> bool {
        self.enabled && age <= self.max_base_age
    }
}

impl Default for DeltaSyncStrategy {
    fn default() -> Self {
        Self::for_quality(NetworkQuality::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_intensifies_as_the_link_degrades() {
        let mut previous = 0_u8;

        // Walk from best to worst; the level must never decrease.
        for quality in [
            NetworkQuality::Excellent,
            NetworkQuality::Good,
            NetworkQuality::Fair,
            NetworkQuality::Poor,
            NetworkQuality::Critical,
        ] {
            let strategy = CompressionStrategy::for_quality(quality);
            assert!(
                strategy.level >= previous,
                "{quality} should compress at least as hard as the grade above it"
            );
            previous = strategy.level;
        }
    }

    #[test]
    fn a_fast_link_skips_compression() {
        let strategy = CompressionStrategy::for_quality(NetworkQuality::Excellent);
        assert!(!strategy.enabled);
        assert!(!strategy.applies_to(1_000_000));
    }

    #[test]
    fn a_failing_link_compresses_hard() {
        let strategy = CompressionStrategy::for_quality(NetworkQuality::Critical);
        assert!(strategy.enabled);
        assert!(strategy.level >= 90);
        assert!(strategy.applies_to(4096));
    }

    #[test]
    fn tiny_payloads_are_never_compressed() {
        let strategy = CompressionStrategy::for_quality(NetworkQuality::Critical);
        assert!(!strategy.applies_to(8), "framing would exceed the saving");
        assert!(strategy.applies_to(MIN_COMPRESSIBLE));
    }

    #[test]
    fn the_compression_threshold_is_adjustable() {
        let strategy =
            CompressionStrategy::for_quality(NetworkQuality::Poor).with_min_payload(4096);

        assert!(!strategy.applies_to(1024));
        assert!(strategy.applies_to(4096));
    }

    #[test]
    fn a_disabled_compression_strategy_never_applies() {
        let strategy = CompressionStrategy::disabled();
        assert!(!strategy.enabled);
        assert!(!strategy.applies_to(usize::MAX));
    }

    #[test]
    fn cache_generosity_grows_as_the_link_degrades() {
        let excellent = CacheStrategy::for_quality(NetworkQuality::Excellent);
        let critical = CacheStrategy::for_quality(NetworkQuality::Critical);

        assert!(
            critical.capacity_bytes > excellent.capacity_bytes,
            "a slow link should spend more memory to avoid re-fetching"
        );
        assert!(
            critical.ttl > excellent.ttl,
            "a slow link should tolerate staler data"
        );
    }

    #[test]
    fn cache_ttl_is_monotonic_across_grades() {
        let mut previous = Duration::ZERO;

        for quality in [
            NetworkQuality::Excellent,
            NetworkQuality::Good,
            NetworkQuality::Fair,
            NetworkQuality::Poor,
            NetworkQuality::Critical,
        ] {
            let ttl = CacheStrategy::for_quality(quality).ttl;
            assert!(ttl >= previous, "{quality} shortened the time to live");
            previous = ttl;
        }
    }

    #[test]
    fn small_entries_are_not_cached() {
        let strategy = CacheStrategy::for_quality(NetworkQuality::Poor);
        assert!(!strategy.applies_to(1));
        assert!(strategy.applies_to(4096));

        let disabled = CacheStrategy::disabled();
        assert!(!disabled.applies_to(usize::MAX));
    }

    #[test]
    fn delta_sync_activates_only_when_bandwidth_is_scarce() {
        assert!(!DeltaSyncStrategy::for_quality(NetworkQuality::Excellent).enabled);
        assert!(!DeltaSyncStrategy::for_quality(NetworkQuality::Good).enabled);
        assert!(DeltaSyncStrategy::for_quality(NetworkQuality::Fair).enabled);
        assert!(DeltaSyncStrategy::for_quality(NetworkQuality::Poor).enabled);
        assert!(DeltaSyncStrategy::for_quality(NetworkQuality::Critical).enabled);
    }

    #[test]
    fn small_payloads_are_sent_whole() {
        let strategy = DeltaSyncStrategy::for_quality(NetworkQuality::Poor);
        assert!(!strategy.applies_to(64));
        assert!(strategy.applies_to(MIN_DELTA_PAYLOAD));
    }

    #[test]
    fn a_delta_that_saves_little_is_rejected() {
        let strategy = DeltaSyncStrategy::for_quality(NetworkQuality::Fair);

        // 60% of the original is within the 70% ceiling.
        assert!(strategy.accepts_delta(600, 1000));
        // 90% is not worth the work on both sides.
        assert!(!strategy.accepts_delta(900, 1000));
    }

    #[test]
    fn a_degraded_link_tolerates_a_weaker_delta() {
        let fair = DeltaSyncStrategy::for_quality(NetworkQuality::Fair);
        let critical = DeltaSyncStrategy::for_quality(NetworkQuality::Critical);

        assert!(
            critical.max_delta_ratio > fair.max_delta_ratio,
            "when bytes are scarce, even a modest saving is worth having"
        );
        assert!(critical.accepts_delta(850, 1000));
        assert!(!fair.accepts_delta(850, 1000));
    }

    #[test]
    fn a_zero_length_original_cannot_be_improved() {
        let strategy = DeltaSyncStrategy::for_quality(NetworkQuality::Critical);
        assert!(!strategy.accepts_delta(0, 0));
    }

    #[test]
    fn a_stale_base_is_refused() {
        let strategy = DeltaSyncStrategy::for_quality(NetworkQuality::Fair);

        assert!(strategy.accepts_base_age(Duration::from_secs(60)));
        assert!(!strategy.accepts_base_age(Duration::from_secs(3600)));
    }

    #[test]
    fn a_disabled_delta_strategy_refuses_everything() {
        let strategy = DeltaSyncStrategy::disabled();

        assert!(!strategy.applies_to(usize::MAX));
        assert!(!strategy.accepts_delta(1, 1_000_000));
        assert!(!strategy.accepts_base_age(Duration::ZERO));
    }
}
