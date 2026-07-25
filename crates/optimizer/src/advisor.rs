//! Turning network estimates into concrete sending decisions.
//!
//! [`Optimizer`] is the layer that answers the practical questions: how big
//! should the next payload be, how hard should it be compressed, how long
//! before a retry. It answers them from measurements rather than from fixed
//! constants, which is the whole point of an adaptive framework.
//!
//! Every recommendation is *advice*. Nothing here sends anything or mutates
//! another subsystem; a caller is free to ignore it. That keeps the policy
//! testable in isolation and keeps the mechanism crates independent of it.

use std::time::Duration;

use crate::estimate::{BandwidthEstimator, RttEstimator};

/// How aggressively to compress, on the abstract 0–100 scale that
/// `nexusnet-compression` uses.
///
/// Named rather than numeric so the intent survives a change to the underlying
/// scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum CompressionAdvice {
    /// Do not compress. The link is fast enough that compression costs more
    /// time than it saves.
    None,
    /// Compress lightly, favouring speed.
    Fast,
    /// The balanced default.
    Balanced,
    /// Compress hard, favouring size. Worth it when bandwidth is the
    /// bottleneck.
    Maximum,
}

impl CompressionAdvice {
    /// Returns the level on the 0–100 scale used by `nexusnet-compression`.
    #[must_use]
    pub const fn level(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Fast => 15,
            Self::Balanced => 50,
            Self::Maximum => 90,
        }
    }

    /// Returns `true` unless this advises skipping compression.
    #[must_use]
    pub const fn should_compress(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// A complete set of sending recommendations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Recommendation {
    /// The payload size to aim for, in bytes.
    pub payload_size: usize,
    /// How hard to compress.
    pub compression: CompressionAdvice,
    /// How long to wait before treating a send as lost.
    pub retry_timeout: Duration,
    /// How many bytes may be in flight before waiting for acknowledgement.
    pub in_flight_bytes: u64,
    /// Whether these figures rest on enough measurement to be trusted.
    ///
    /// When `false`, the values are defaults rather than conclusions; a caller
    /// may prefer its own constants until confidence arrives.
    pub confident: bool,
}

/// The smallest payload the optimizer will recommend.
///
/// Below roughly this size, per-frame overhead dominates and throughput
/// collapses regardless of link speed.
pub const MIN_PAYLOAD: usize = 1024;

/// The largest payload the optimizer will recommend.
///
/// Beyond this, a single lost payload costs too much to retransmit and latency
/// suffers on any link that is not exceptionally fast.
pub const MAX_PAYLOAD: usize = 1024 * 1024;

/// The payload size used before measurements exist.
///
/// Chosen to sit just under a typical Ethernet MTU once framing and headers are
/// accounted for, so an unmeasured connection does not immediately fragment.
pub const DEFAULT_PAYLOAD: usize = 1400;

/// Bandwidth below which compressing hard is worthwhile: 256 KiB/s.
///
/// On a slow link, CPU spent shrinking a payload is repaid many times over by
/// the transmission time saved.
pub const SLOW_LINK_BYTES_PER_SECOND: f64 = 256.0 * 1024.0;

/// Bandwidth above which compression stops paying: 32 MiB/s.
///
/// On a link this fast, most payloads transmit in less time than compressing
/// them would take.
pub const FAST_LINK_BYTES_PER_SECOND: f64 = 32.0 * 1024.0 * 1024.0;

/// Produces sending advice from observed network conditions.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use nexusnet_optimizer::{CompressionAdvice, Optimizer};
///
/// let mut optimizer = Optimizer::new();
///
/// // A slow, high-latency link: compress hard, send larger payloads.
/// for _ in 0..10 {
///     optimizer.record_delivery(16 * 1024, Duration::from_secs(1));
///     optimizer.record_rtt(Duration::from_millis(400));
/// }
///
/// let advice = optimizer.recommend();
/// assert_eq!(advice.compression, CompressionAdvice::Maximum);
/// assert!(advice.confident);
/// ```
#[derive(Debug, Clone, Default)]
pub struct Optimizer {
    bandwidth: BandwidthEstimator,
    rtt: RttEstimator,
}

impl Optimizer {
    /// Creates an optimizer with no measurements.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bandwidth: BandwidthEstimator::new(),
            rtt: RttEstimator::new(),
        }
    }

    /// Records that `bytes` were delivered in `elapsed`.
    pub fn record_delivery(&mut self, bytes: u64, elapsed: Duration) {
        self.bandwidth.sample(bytes, elapsed);
    }

    /// Records a round-trip time measurement.
    pub fn record_rtt(&mut self, rtt: Duration) {
        self.rtt.sample(rtt);
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

    /// Returns `true` once both estimators have enough samples.
    #[must_use]
    pub const fn is_confident(&self) -> bool {
        self.bandwidth.is_confident() && self.rtt.samples() >= 4
    }

    /// Returns the current recommendations.
    #[must_use]
    pub fn recommend(&self) -> Recommendation {
        Recommendation {
            payload_size: self.payload_size(),
            compression: self.compression(),
            retry_timeout: self.rtt.retransmit_timeout(),
            in_flight_bytes: self.in_flight_bytes(),
            confident: self.is_confident(),
        }
    }

    /// Recommends a payload size from the bandwidth-delay product.
    ///
    /// The reasoning: a payload should be small enough to transmit quickly
    /// relative to the round trip, so that loss is cheap to recover and latency
    /// stays low. Aiming at roughly an eighth of the bandwidth-delay product
    /// keeps several payloads in flight rather than one large one.
    fn payload_size(&self) -> usize {
        let (Some(rate), Some(rtt)) = (self.bandwidth.bytes_per_second(), self.rtt.smoothed_rtt())
        else {
            return DEFAULT_PAYLOAD;
        };

        let bandwidth_delay_product = rate * rtt.as_secs_f64();
        let target = bandwidth_delay_product / 8.0;

        if target.is_finite() && target > 0.0 {
            (target as usize).clamp(MIN_PAYLOAD, MAX_PAYLOAD)
        } else {
            DEFAULT_PAYLOAD
        }
    }

    /// Recommends a compression level from the link speed.
    ///
    /// The trade is time: compression is worth it exactly when the transmission
    /// time saved exceeds the CPU time spent. That makes it a function of
    /// bandwidth, not of payload content.
    fn compression(&self) -> CompressionAdvice {
        let Some(rate) = self.bandwidth.bytes_per_second() else {
            // Without measurement, the balanced default is the safe choice.
            return CompressionAdvice::Balanced;
        };

        if rate <= SLOW_LINK_BYTES_PER_SECOND {
            CompressionAdvice::Maximum
        } else if rate >= FAST_LINK_BYTES_PER_SECOND {
            CompressionAdvice::None
        } else if rate >= FAST_LINK_BYTES_PER_SECOND / 4.0 {
            CompressionAdvice::Fast
        } else {
            CompressionAdvice::Balanced
        }
    }

    /// Recommends how many bytes to keep in flight.
    ///
    /// This is the bandwidth-delay product: the amount of data needed to keep
    /// the link busy for one round trip. Sending less underuses the link;
    /// sending much more only fills buffers and adds latency.
    fn in_flight_bytes(&self) -> u64 {
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

    /// Discards all measurements, as after a route change or reconnection.
    pub fn reset(&mut self) {
        self.bandwidth.reset();
        self.rtt.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `count` identical samples of a given rate and round-trip time.
    fn train(optimizer: &mut Optimizer, bytes_per_second: u64, rtt: Duration, count: usize) {
        for _ in 0..count {
            optimizer.record_delivery(bytes_per_second, Duration::from_secs(1));
            optimizer.record_rtt(rtt);
        }
    }

    #[test]
    fn an_unmeasured_link_returns_safe_defaults() {
        let optimizer = Optimizer::new();
        let advice = optimizer.recommend();

        assert_eq!(advice.payload_size, DEFAULT_PAYLOAD);
        assert_eq!(advice.compression, CompressionAdvice::Balanced);
        assert!(!advice.confident, "no samples means no confidence");
    }

    #[test]
    fn confidence_requires_samples_from_both_estimators() {
        let mut optimizer = Optimizer::new();

        for _ in 0..10 {
            optimizer.record_delivery(1000, Duration::from_secs(1));
        }
        assert!(
            !optimizer.is_confident(),
            "bandwidth alone is not enough to be confident"
        );

        for _ in 0..10 {
            optimizer.record_rtt(Duration::from_millis(50));
        }
        assert!(optimizer.is_confident());
    }

    #[test]
    fn a_slow_link_is_told_to_compress_hard() {
        let mut optimizer = Optimizer::new();
        train(&mut optimizer, 32 * 1024, Duration::from_millis(300), 10);

        let advice = optimizer.recommend();
        assert_eq!(advice.compression, CompressionAdvice::Maximum);
        assert!(advice.compression.should_compress());
        assert_eq!(advice.compression.level(), 90);
    }

    #[test]
    fn a_very_fast_link_is_told_not_to_compress() {
        let mut optimizer = Optimizer::new();
        train(
            &mut optimizer,
            64 * 1024 * 1024,
            Duration::from_millis(1),
            10,
        );

        let advice = optimizer.recommend();
        assert_eq!(advice.compression, CompressionAdvice::None);
        assert!(!advice.compression.should_compress());
    }

    #[test]
    fn a_middling_link_gets_a_middling_level() {
        let mut optimizer = Optimizer::new();
        train(
            &mut optimizer,
            2 * 1024 * 1024,
            Duration::from_millis(40),
            10,
        );

        let advice = optimizer.recommend();
        assert!(
            matches!(
                advice.compression,
                CompressionAdvice::Balanced | CompressionAdvice::Fast
            ),
            "got {:?}",
            advice.compression
        );
    }

    #[test]
    fn payload_size_grows_with_the_bandwidth_delay_product() {
        let mut slow = Optimizer::new();
        train(&mut slow, 64 * 1024, Duration::from_millis(50), 10);

        let mut fast = Optimizer::new();
        train(&mut fast, 16 * 1024 * 1024, Duration::from_millis(50), 10);

        assert!(
            fast.recommend().payload_size > slow.recommend().payload_size,
            "a fatter pipe should carry larger payloads"
        );
    }

    #[test]
    fn payload_size_stays_within_bounds() {
        // An absurdly fast, high-latency link would suggest a huge payload.
        let mut huge = Optimizer::new();
        train(
            &mut huge,
            10 * 1024 * 1024 * 1024,
            Duration::from_secs(2),
            10,
        );
        assert_eq!(huge.recommend().payload_size, MAX_PAYLOAD);

        // A very slow link would suggest an unusably small one.
        let mut tiny = Optimizer::new();
        train(&mut tiny, 100, Duration::from_millis(10), 10);
        assert_eq!(tiny.recommend().payload_size, MIN_PAYLOAD);
    }

    #[test]
    fn in_flight_bytes_track_the_bandwidth_delay_product() {
        let mut optimizer = Optimizer::new();
        // 1 MiB/s with a 200ms round trip is roughly 200 KiB in flight.
        train(&mut optimizer, 1024 * 1024, Duration::from_millis(200), 20);

        let advice = optimizer.recommend();
        let expected = (1024.0 * 1024.0 * 0.2) as u64;
        let difference = advice.in_flight_bytes.abs_diff(expected);

        assert!(
            difference < expected / 4,
            "expected about {expected} bytes in flight, got {}",
            advice.in_flight_bytes
        );
    }

    #[test]
    fn the_retry_timeout_follows_the_round_trip_time() {
        let mut quick = Optimizer::new();
        train(&mut quick, 1024 * 1024, Duration::from_millis(20), 20);

        let mut slow = Optimizer::new();
        train(&mut slow, 1024 * 1024, Duration::from_millis(800), 20);

        assert!(
            slow.recommend().retry_timeout > quick.recommend().retry_timeout,
            "a slower path must wait longer before retrying"
        );
    }

    #[test]
    fn advice_adapts_when_conditions_change() {
        let mut optimizer = Optimizer::new();

        // Start on a fast link.
        train(
            &mut optimizer,
            64 * 1024 * 1024,
            Duration::from_millis(5),
            20,
        );
        assert_eq!(optimizer.recommend().compression, CompressionAdvice::None);

        // The link degrades badly and stays down.
        train(&mut optimizer, 16 * 1024, Duration::from_millis(500), 80);
        assert_eq!(
            optimizer.recommend().compression,
            CompressionAdvice::Maximum,
            "sustained degradation should change the advice"
        );
    }

    #[test]
    fn resetting_returns_to_defaults() {
        let mut optimizer = Optimizer::new();
        train(&mut optimizer, 16 * 1024, Duration::from_millis(300), 10);
        assert!(optimizer.is_confident());

        optimizer.reset();
        assert!(!optimizer.is_confident());
        assert_eq!(optimizer.recommend().payload_size, DEFAULT_PAYLOAD);
    }

    #[test]
    fn compression_levels_are_ordered() {
        assert!(CompressionAdvice::None < CompressionAdvice::Fast);
        assert!(CompressionAdvice::Fast < CompressionAdvice::Balanced);
        assert!(CompressionAdvice::Balanced < CompressionAdvice::Maximum);

        assert!(CompressionAdvice::Maximum.level() > CompressionAdvice::Balanced.level());
        assert_eq!(CompressionAdvice::None.level(), 0);
    }
}
