//! The network optimizer: measurement in, a complete plan out.
//!
//! [`NetworkOptimizer`] observes bandwidth, latency, and loss, grades the link,
//! and produces an [`OptimizationPlan`] covering every adaptive decision the
//! framework makes — payload size, compression, caching, delta synchronization,
//! retry timing, and how much to keep in flight.
//!
//! ## Advice, not action
//!
//! Nothing here sends, compresses, or caches anything, and this crate depends
//! on no other NexusNet crate. It reads measurements and returns values. That
//! keeps the policy testable in isolation and lets the mechanism crates stay
//! independent of how the decisions are made.

use std::time::Duration;

use crate::advisor::{DEFAULT_PAYLOAD, MAX_PAYLOAD, MIN_PAYLOAD};
use crate::estimate::{BandwidthEstimator, RttEstimator};
use crate::quality::{LossEstimator, NetworkQuality};
use crate::strategy::{CacheStrategy, CompressionStrategy, DeltaSyncStrategy};

/// A complete set of adaptive decisions for the current conditions.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct OptimizationPlan {
    /// The graded network quality the plan was derived from.
    pub quality: NetworkQuality,
    /// The payload size to aim for, in bytes.
    pub payload_size: usize,
    /// How to compress outbound payloads.
    pub compression: CompressionStrategy,
    /// How to use the local cache.
    pub cache: CacheStrategy,
    /// Whether to send differences rather than whole payloads.
    pub delta_sync: DeltaSyncStrategy,
    /// How long to wait before treating a send as lost.
    pub retry_timeout: Duration,
    /// How many bytes to keep in flight before waiting for acknowledgement.
    pub in_flight_bytes: u64,
    /// Whether the plan rests on enough measurement to be trusted.
    ///
    /// When `false`, these are defaults rather than conclusions. Acting hard on
    /// one or two samples is how an adaptive system starts oscillating.
    pub confident: bool,
}

impl OptimizationPlan {
    /// Returns `true` if the plan calls for conserving bandwidth.
    #[must_use]
    pub const fn conserves_bandwidth(&self) -> bool {
        self.compression.enabled || self.delta_sync.enabled
    }
}

/// A snapshot of optimizer activity.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[non_exhaustive]
pub struct OptimizationMetrics {
    /// Delivery samples recorded.
    pub delivery_samples: u64,
    /// Round-trip time samples recorded.
    pub rtt_samples: u64,
    /// Loss observations recorded.
    pub loss_samples: u64,
    /// Times the graded quality changed.
    ///
    /// Frequent changes suggest an unstable link, or thresholds too close
    /// together for the traffic being measured.
    pub quality_changes: u64,
    /// The current graded quality.
    pub quality: NetworkQuality,
    /// The worst quality observed.
    pub worst_quality: NetworkQuality,
    /// The current bandwidth estimate in bytes per second, if any.
    pub bytes_per_second: Option<f64>,
    /// The current smoothed round-trip time, if any.
    pub smoothed_rtt: Option<Duration>,
    /// The current smoothed loss ratio, if any.
    pub loss_ratio: Option<f64>,
    /// Whether the estimates rest on enough samples to be trusted.
    pub confident: bool,
}

impl OptimizationMetrics {
    /// Returns the total samples of every kind.
    #[must_use]
    pub const fn total_samples(&self) -> u64 {
        self.delivery_samples + self.rtt_samples + self.loss_samples
    }

    /// Returns `true` if the link is currently graded as degraded.
    #[must_use]
    pub const fn is_degraded(&self) -> bool {
        self.quality.is_degraded()
    }

    /// Returns `true` if conditions have ever been worse than they are now.
    ///
    /// Distinguishes a link recovering from one that was always this bad, which
    /// matters when deciding whether to keep conserving.
    #[must_use]
    pub fn has_recovered(&self) -> bool {
        self.quality > self.worst_quality
    }
}

/// Observes network conditions and produces adaptive plans.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use nexusnet_optimizer::{NetworkOptimizer, NetworkQuality};
///
/// let mut optimizer = NetworkOptimizer::new();
///
/// // A slow, high-latency, lossy link.
/// for _ in 0..20 {
///     optimizer.record_delivery(24 * 1024, Duration::from_secs(1));
///     optimizer.record_rtt(Duration::from_millis(450));
///     optimizer.record_loss(90, 10);
/// }
///
/// let plan = optimizer.plan();
/// assert!(plan.quality.is_degraded());
/// assert!(plan.compression.enabled, "a scarce link should trade CPU for bytes");
/// assert!(plan.delta_sync.enabled);
/// assert!(plan.conserves_bandwidth());
/// ```
#[derive(Debug, Clone)]
pub struct NetworkOptimizer {
    bandwidth: BandwidthEstimator,
    rtt: RttEstimator,
    loss: LossEstimator,
    quality: NetworkQuality,
    worst_quality: NetworkQuality,
    delivery_samples: u64,
    rtt_samples: u64,
    loss_samples: u64,
    quality_changes: u64,
}

impl NetworkOptimizer {
    /// Creates an optimizer with no measurements.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bandwidth: BandwidthEstimator::new(),
            rtt: RttEstimator::new(),
            loss: LossEstimator::new(),
            // Assume a workable link until measurement says otherwise. Starting
            // pessimistic would compress hard and cache aggressively before
            // there is any evidence that it helps.
            quality: NetworkQuality::Good,
            worst_quality: NetworkQuality::Excellent,
            delivery_samples: 0,
            rtt_samples: 0,
            loss_samples: 0,
            quality_changes: 0,
        }
    }

    /// Records that `bytes` were delivered in `elapsed`.
    pub fn record_delivery(&mut self, bytes: u64, elapsed: Duration) {
        self.bandwidth.sample(bytes, elapsed);
        self.delivery_samples += 1;
        self.regrade();
    }

    /// Records a round-trip time measurement.
    pub fn record_rtt(&mut self, rtt: Duration) {
        self.rtt.sample(rtt);
        self.rtt_samples += 1;
        self.regrade();
    }

    /// Records the outcome of a batch of packets.
    pub fn record_loss(&mut self, delivered: u64, lost: u64) {
        self.loss.sample(delivered, lost);
        self.loss_samples += 1;
        self.regrade();
    }

    /// Returns the bandwidth estimator.
    #[must_use]
    pub const fn bandwidth(&self) -> &BandwidthEstimator {
        &self.bandwidth
    }

    /// Returns the round-trip time estimator.
    #[must_use]
    pub const fn rtt(&self) -> &RttEstimator {
        &self.rtt
    }

    /// Returns the loss estimator.
    #[must_use]
    pub const fn loss(&self) -> &LossEstimator {
        &self.loss
    }

    /// Returns the current graded quality.
    #[must_use]
    pub const fn quality(&self) -> NetworkQuality {
        self.quality
    }

    /// Returns `true` once every estimator has enough samples to be trusted.
    ///
    /// Loss is excluded from this requirement: a link that has simply not lost
    /// anything yet should not be treated as unmeasured forever.
    #[must_use]
    pub const fn is_confident(&self) -> bool {
        self.bandwidth.is_confident() && self.rtt.samples() >= 4
    }

    /// Returns a snapshot of optimizer activity.
    #[must_use]
    pub fn metrics(&self) -> OptimizationMetrics {
        OptimizationMetrics {
            delivery_samples: self.delivery_samples,
            rtt_samples: self.rtt_samples,
            loss_samples: self.loss_samples,
            quality_changes: self.quality_changes,
            quality: self.quality,
            worst_quality: self.worst_quality,
            bytes_per_second: self.bandwidth.bytes_per_second(),
            smoothed_rtt: self.rtt.smoothed_rtt(),
            loss_ratio: self.loss.ratio(),
            confident: self.is_confident(),
        }
    }

    /// Returns a complete plan for the current conditions.
    #[must_use]
    pub fn plan(&self) -> OptimizationPlan {
        OptimizationPlan {
            quality: self.quality,
            payload_size: self.payload_size(),
            compression: self.compression_strategy(),
            cache: CacheStrategy::for_quality(self.quality),
            delta_sync: DeltaSyncStrategy::for_quality(self.quality),
            retry_timeout: self.rtt.retransmit_timeout(),
            in_flight_bytes: self.in_flight_bytes(),
            confident: self.is_confident(),
        }
    }

    /// Returns the compression strategy alone.
    #[must_use]
    pub fn compression_strategy(&self) -> CompressionStrategy {
        CompressionStrategy::for_quality(self.quality)
    }

    /// Returns the cache strategy alone.
    #[must_use]
    pub fn cache_strategy(&self) -> CacheStrategy {
        CacheStrategy::for_quality(self.quality)
    }

    /// Returns the delta synchronization strategy alone.
    #[must_use]
    pub fn delta_sync_strategy(&self) -> DeltaSyncStrategy {
        DeltaSyncStrategy::for_quality(self.quality)
    }

    /// Returns the recommended payload size in bytes.
    ///
    /// Derived from the bandwidth-delay product: aiming at roughly an eighth of
    /// it keeps several payloads in flight rather than one large one, so a loss
    /// is cheap to recover and latency stays low.
    #[must_use]
    pub fn payload_size(&self) -> usize {
        let (Some(rate), Some(rtt)) = (self.bandwidth.bytes_per_second(), self.rtt.smoothed_rtt())
        else {
            return DEFAULT_PAYLOAD;
        };

        let target = (rate * rtt.as_secs_f64()) / 8.0;

        if target.is_finite() && target > 0.0 {
            (target as usize).clamp(MIN_PAYLOAD, MAX_PAYLOAD)
        } else {
            DEFAULT_PAYLOAD
        }
    }

    /// Returns how many bytes to keep in flight.
    ///
    /// This is the bandwidth-delay product: enough to keep the link busy for a
    /// round trip. Less underuses it; much more only fills buffers and adds
    /// latency.
    #[must_use]
    pub fn in_flight_bytes(&self) -> u64 {
        let (Some(rate), Some(rtt)) = (self.bandwidth.bytes_per_second(), self.rtt.smoothed_rtt())
        else {
            return (DEFAULT_PAYLOAD * 8) as u64;
        };

        let product = rate * rtt.as_secs_f64();

        if product.is_finite() && product > 0.0 {
            (product as u64).max(MIN_PAYLOAD as u64)
        } else {
            (DEFAULT_PAYLOAD * 8) as u64
        }
    }

    /// Discards every measurement, as after a route change or reconnection.
    ///
    /// The worst-observed grade is cleared too: conditions on a new path say
    /// nothing about the old one.
    pub fn reset(&mut self) {
        self.bandwidth.reset();
        self.rtt.reset();
        self.loss.reset();
        self.quality = NetworkQuality::Good;
        self.worst_quality = NetworkQuality::Excellent;
    }

    /// Recomputes the grade from whatever measurements exist.
    ///
    /// Only dimensions that have been measured contribute. Grading an
    /// unmeasured dimension as bad would make every new connection look broken.
    fn regrade(&mut self) {
        let mut grades = Vec::with_capacity(3);

        if let Some(rate) = self.bandwidth.bytes_per_second() {
            grades.push(NetworkQuality::from_bandwidth(rate));
        }
        if let Some(rtt) = self.rtt.smoothed_rtt() {
            grades.push(NetworkQuality::from_rtt(rtt));
        }
        // An unconfident loss estimate is noise: one loss in three packets is a
        // 33% ratio and means nothing.
        if self.loss.is_confident() {
            if let Some(ratio) = self.loss.ratio() {
                grades.push(NetworkQuality::from_loss(ratio));
            }
        }

        let Some(graded) = NetworkQuality::worst_of(&grades) else {
            return;
        };

        if graded != self.quality {
            self.quality_changes += 1;
            self.quality = graded;
        }

        self.worst_quality = self.worst_quality.min(graded);
    }
}

impl Default for NetworkOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds identical samples describing a link of a given character.
    fn train(
        optimizer: &mut NetworkOptimizer,
        bytes_per_second: u64,
        rtt: Duration,
        loss_ratio: f64,
        rounds: usize,
    ) {
        let lost = (100.0 * loss_ratio) as u64;
        let delivered = 100 - lost;

        for _ in 0..rounds {
            optimizer.record_delivery(bytes_per_second, Duration::from_secs(1));
            optimizer.record_rtt(rtt);
            optimizer.record_loss(delivered, lost);
        }
    }

    #[test]
    fn a_new_optimizer_assumes_a_workable_link() {
        let optimizer = NetworkOptimizer::new();

        assert_eq!(optimizer.quality(), NetworkQuality::Good);
        assert!(!optimizer.is_confident());

        let plan = optimizer.plan();
        assert_eq!(plan.payload_size, DEFAULT_PAYLOAD);
        assert!(!plan.confident, "no samples means no confidence");
    }

    #[test]
    fn a_fast_clean_link_grades_excellent() {
        let mut optimizer = NetworkOptimizer::new();
        train(
            &mut optimizer,
            64 * 1024 * 1024,
            Duration::from_millis(10),
            0.0,
            30,
        );

        assert_eq!(optimizer.quality(), NetworkQuality::Excellent);
        assert!(optimizer.is_confident());
    }

    #[test]
    fn a_slow_lossy_link_grades_badly() {
        let mut optimizer = NetworkOptimizer::new();
        train(
            &mut optimizer,
            24 * 1024,
            Duration::from_millis(700),
            0.15,
            30,
        );

        assert_eq!(optimizer.quality(), NetworkQuality::Critical);
        assert!(optimizer.quality().is_degraded());
    }

    #[test]
    fn heavy_loss_overrides_excellent_bandwidth() {
        let mut optimizer = NetworkOptimizer::new();
        // A fat, fast pipe that drops a fifth of everything.
        train(
            &mut optimizer,
            64 * 1024 * 1024,
            Duration::from_millis(5),
            0.20,
            30,
        );

        assert_eq!(
            optimizer.quality(),
            NetworkQuality::Critical,
            "bandwidth must not disguise a link that loses a fifth of its packets"
        );
    }

    #[test]
    fn an_unmeasured_dimension_does_not_count_against_the_link() {
        let mut optimizer = NetworkOptimizer::new();

        // Bandwidth and latency only; no loss has been observed at all.
        for _ in 0..20 {
            optimizer.record_delivery(64 * 1024 * 1024, Duration::from_secs(1));
            optimizer.record_rtt(Duration::from_millis(5));
        }

        assert_eq!(
            optimizer.quality(),
            NetworkQuality::Excellent,
            "never having measured loss is not evidence of loss"
        );
    }

    #[test]
    fn a_thin_loss_sample_is_ignored() {
        let mut optimizer = NetworkOptimizer::new();

        for _ in 0..20 {
            optimizer.record_delivery(64 * 1024 * 1024, Duration::from_secs(1));
            optimizer.record_rtt(Duration::from_millis(5));
        }

        // One loss out of three packets is a 33% ratio and means nothing.
        optimizer.record_loss(2, 1);

        assert_eq!(
            optimizer.quality(),
            NetworkQuality::Excellent,
            "three packets is not enough to condemn a link"
        );
    }

    #[test]
    fn a_degraded_link_is_told_to_conserve() {
        let mut optimizer = NetworkOptimizer::new();
        train(
            &mut optimizer,
            48 * 1024,
            Duration::from_millis(400),
            0.05,
            30,
        );

        let plan = optimizer.plan();
        assert!(plan.quality.is_degraded());
        assert!(plan.compression.enabled);
        assert!(plan.compression.level >= 80);
        assert!(plan.delta_sync.enabled);
        assert!(plan.conserves_bandwidth());
        assert!(plan.cache.capacity_bytes >= 32 * 1024 * 1024);
    }

    #[test]
    fn a_fast_link_is_told_not_to_bother() {
        let mut optimizer = NetworkOptimizer::new();
        train(
            &mut optimizer,
            64 * 1024 * 1024,
            Duration::from_millis(5),
            0.0,
            30,
        );

        let plan = optimizer.plan();
        assert!(
            !plan.compression.enabled,
            "compressing would cost more time than it saves"
        );
        assert!(!plan.delta_sync.enabled);
        assert!(!plan.conserves_bandwidth());
    }

    #[test]
    fn payload_size_follows_the_bandwidth_delay_product() {
        let mut slow = NetworkOptimizer::new();
        train(&mut slow, 64 * 1024, Duration::from_millis(50), 0.0, 20);

        let mut fast = NetworkOptimizer::new();
        train(
            &mut fast,
            16 * 1024 * 1024,
            Duration::from_millis(50),
            0.0,
            20,
        );

        assert!(
            fast.plan().payload_size > slow.plan().payload_size,
            "a fatter pipe should carry larger payloads"
        );
    }

    #[test]
    fn payload_size_stays_within_bounds() {
        let mut huge = NetworkOptimizer::new();
        train(
            &mut huge,
            10 * 1024 * 1024 * 1024,
            Duration::from_secs(2),
            0.0,
            20,
        );
        assert_eq!(huge.plan().payload_size, MAX_PAYLOAD);

        let mut tiny = NetworkOptimizer::new();
        train(&mut tiny, 100, Duration::from_millis(10), 0.0, 20);
        assert_eq!(tiny.plan().payload_size, MIN_PAYLOAD);
    }

    #[test]
    fn in_flight_bytes_track_the_bandwidth_delay_product() {
        let mut optimizer = NetworkOptimizer::new();
        train(
            &mut optimizer,
            1024 * 1024,
            Duration::from_millis(200),
            0.0,
            30,
        );

        let expected = (1024.0 * 1024.0 * 0.2) as u64;
        let actual = optimizer.plan().in_flight_bytes;

        assert!(
            actual.abs_diff(expected) < expected / 4,
            "expected about {expected} bytes in flight, got {actual}"
        );
    }

    #[test]
    fn the_retry_timeout_follows_latency() {
        let mut quick = NetworkOptimizer::new();
        train(&mut quick, 1024 * 1024, Duration::from_millis(20), 0.0, 20);

        let mut slow = NetworkOptimizer::new();
        train(&mut slow, 1024 * 1024, Duration::from_millis(800), 0.0, 20);

        assert!(
            slow.plan().retry_timeout > quick.plan().retry_timeout,
            "a slower path must wait longer before retrying"
        );
    }

    #[test]
    fn the_plan_adapts_when_conditions_change() {
        let mut optimizer = NetworkOptimizer::new();

        train(
            &mut optimizer,
            64 * 1024 * 1024,
            Duration::from_millis(5),
            0.0,
            25,
        );
        assert!(!optimizer.plan().compression.enabled);

        // The link degrades badly and stays down.
        train(
            &mut optimizer,
            24 * 1024,
            Duration::from_millis(600),
            0.08,
            80,
        );

        let plan = optimizer.plan();
        assert!(
            plan.compression.enabled,
            "sustained degradation must change the plan"
        );
        assert!(plan.delta_sync.enabled);
    }

    #[test]
    fn metrics_track_samples_and_transitions() {
        let mut optimizer = NetworkOptimizer::new();
        train(
            &mut optimizer,
            64 * 1024 * 1024,
            Duration::from_millis(5),
            0.0,
            10,
        );

        let metrics = optimizer.metrics();
        assert_eq!(metrics.delivery_samples, 10);
        assert_eq!(metrics.rtt_samples, 10);
        assert_eq!(metrics.loss_samples, 10);
        assert_eq!(metrics.total_samples(), 30);
        assert!(
            metrics.quality_changes >= 1,
            "the grade moved off its default"
        );
        assert!(metrics.bytes_per_second.is_some());
        assert!(metrics.smoothed_rtt.is_some());
        assert!(metrics.confident);
        assert!(!metrics.is_degraded());
    }

    #[test]
    fn the_worst_grade_is_remembered() {
        let mut optimizer = NetworkOptimizer::new();

        // Start badly.
        train(
            &mut optimizer,
            16 * 1024,
            Duration::from_millis(800),
            0.2,
            30,
        );
        assert_eq!(optimizer.metrics().worst_quality, NetworkQuality::Critical);

        // Then recover.
        train(
            &mut optimizer,
            64 * 1024 * 1024,
            Duration::from_millis(5),
            0.0,
            200,
        );

        let metrics = optimizer.metrics();
        assert!(
            metrics.quality > NetworkQuality::Critical,
            "the link recovered, got {}",
            metrics.quality
        );
        assert_eq!(
            metrics.worst_quality,
            NetworkQuality::Critical,
            "the low-water mark must survive recovery"
        );
        assert!(metrics.has_recovered());
    }

    #[test]
    fn resetting_clears_measurements_and_history() {
        let mut optimizer = NetworkOptimizer::new();
        train(
            &mut optimizer,
            16 * 1024,
            Duration::from_millis(900),
            0.3,
            30,
        );
        assert!(optimizer.is_confident());

        optimizer.reset();

        assert!(!optimizer.is_confident());
        assert_eq!(optimizer.quality(), NetworkQuality::Good);
        assert_eq!(
            optimizer.metrics().worst_quality,
            NetworkQuality::Excellent,
            "a new path says nothing about the old one"
        );
        assert_eq!(optimizer.plan().payload_size, DEFAULT_PAYLOAD);
    }

    #[test]
    fn individual_strategies_match_the_plan() {
        let mut optimizer = NetworkOptimizer::new();
        train(
            &mut optimizer,
            48 * 1024,
            Duration::from_millis(400),
            0.02,
            30,
        );

        let plan = optimizer.plan();
        assert_eq!(optimizer.compression_strategy(), plan.compression);
        assert_eq!(optimizer.cache_strategy(), plan.cache);
        assert_eq!(optimizer.delta_sync_strategy(), plan.delta_sync);
    }
}
