//! Estimating network conditions from observed samples.
//!
//! Every adaptive decision downstream — how large a packet to send, how hard to
//! compress, how long to wait before retrying — depends on knowing roughly what
//! the network is doing. These estimators turn noisy per-sample measurements
//! into stable figures worth acting on.
//!
//! Both use exponentially weighted moving averages rather than a simple mean
//! over a window. An EWMA needs constant memory regardless of history, responds
//! smoothly to change, and forgets old conditions automatically — all of which
//! a sliding window gets wrong or gets right only by storing every sample.

use std::time::Duration;

/// The default smoothing factor for bandwidth samples.
///
/// Lower reacts faster and is noisier. An eighth is a common choice: it takes
/// roughly eight samples to converge, which is quick enough to follow a genuine
/// change and slow enough to ignore a single odd measurement.
pub const DEFAULT_SMOOTHING: f64 = 0.125;

/// Estimates available bandwidth from delivery samples.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use nexusnet_optimizer::BandwidthEstimator;
///
/// let mut estimator = BandwidthEstimator::new();
///
/// // 100 KiB delivered in 100ms is roughly 1 MiB/s.
/// for _ in 0..20 {
///     estimator.sample(100 * 1024, Duration::from_millis(100));
/// }
///
/// let estimate = estimator.bytes_per_second().expect("samples were recorded");
/// assert!((estimate - 1024.0 * 1024.0).abs() < 50_000.0);
/// ```
#[derive(Debug, Clone)]
pub struct BandwidthEstimator {
    smoothed: Option<f64>,
    peak: f64,
    smoothing: f64,
    samples: u64,
}

impl BandwidthEstimator {
    /// Creates an estimator with the default smoothing factor.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            smoothed: None,
            peak: 0.0,
            smoothing: DEFAULT_SMOOTHING,
            samples: 0,
        }
    }

    /// Creates an estimator with a specific smoothing factor.
    ///
    /// Clamped to `0.01..=1.0`. A factor of 1.0 tracks the latest sample
    /// exactly; values near zero barely move at all.
    #[must_use]
    pub fn with_smoothing(smoothing: f64) -> Self {
        Self {
            smoothing: smoothing.clamp(0.01, 1.0),
            ..Self::new()
        }
    }

    /// Records a delivery of `bytes` taking `elapsed`.
    ///
    /// Zero-length intervals are ignored rather than treated as infinite
    /// bandwidth, which would poison the estimate permanently.
    pub fn sample(&mut self, bytes: u64, elapsed: Duration) {
        let seconds = elapsed.as_secs_f64();
        if seconds <= 0.0 || bytes == 0 {
            return;
        }

        let rate = bytes as f64 / seconds;
        self.samples += 1;
        self.peak = self.peak.max(rate);

        self.smoothed = Some(match self.smoothed {
            Some(current) => current + self.smoothing * (rate - current),
            // The first sample is the estimate; averaging it against nothing
            // would start every connection at zero and climb slowly.
            None => rate,
        });
    }

    /// Returns the smoothed estimate in bytes per second, if any sample exists.
    #[must_use]
    pub fn bytes_per_second(&self) -> Option<f64> {
        self.smoothed
    }

    /// Returns the highest rate ever observed.
    ///
    /// Useful as an optimistic ceiling: the link demonstrably reached this once.
    #[must_use]
    pub const fn peak_bytes_per_second(&self) -> f64 {
        self.peak
    }

    /// Returns how many samples have been recorded.
    #[must_use]
    pub const fn samples(&self) -> u64 {
        self.samples
    }

    /// Returns `true` once enough samples exist to trust the estimate.
    ///
    /// Acting on one or two samples is how an adaptive system oscillates.
    #[must_use]
    pub const fn is_confident(&self) -> bool {
        self.samples >= 4
    }

    /// Discards all history, as after a route change or reconnection.
    pub fn reset(&mut self) {
        self.smoothed = None;
        self.peak = 0.0;
        self.samples = 0;
    }
}

impl Default for BandwidthEstimator {
    fn default() -> Self {
        Self::new()
    }
}

/// Estimates round-trip time and a retransmission timeout.
///
/// This follows the algorithm TCP uses (Jacobson/Karels): track both a smoothed
/// round-trip time and its variation, and derive the timeout from both. Using
/// the average alone produces a timeout that fires constantly on a jittery link
/// — the variation term is what makes it usable.
#[derive(Debug, Clone)]
pub struct RttEstimator {
    /// Smoothed round-trip time, in seconds.
    smoothed: Option<f64>,
    /// Mean deviation of the round-trip time, in seconds.
    variation: f64,
    min_rtt: Option<f64>,
    samples: u64,
}

/// The lower bound on a computed retransmission timeout.
///
/// Prevents a very fast local link from producing a timeout so short that
/// ordinary scheduling delay looks like packet loss.
pub const MIN_RETRANSMIT_TIMEOUT: Duration = Duration::from_millis(200);

/// The upper bound on a computed retransmission timeout.
pub const MAX_RETRANSMIT_TIMEOUT: Duration = Duration::from_secs(60);

impl RttEstimator {
    /// Creates an estimator with no samples.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            smoothed: None,
            variation: 0.0,
            min_rtt: None,
            samples: 0,
        }
    }

    /// Records a round-trip time measurement.
    pub fn sample(&mut self, rtt: Duration) {
        let seconds = rtt.as_secs_f64();
        if seconds <= 0.0 {
            return;
        }

        self.samples += 1;
        self.min_rtt = Some(self.min_rtt.map_or(seconds, |min| min.min(seconds)));

        match self.smoothed {
            Some(current) => {
                // The standard gains: 1/4 for variation, 1/8 for the average.
                let difference = (seconds - current).abs();
                self.variation = 0.75 * self.variation + 0.25 * difference;
                self.smoothed = Some(0.875 * current + 0.125 * seconds);
            }
            None => {
                // First measurement seeds both terms.
                self.smoothed = Some(seconds);
                self.variation = seconds / 2.0;
            }
        }
    }

    /// Returns the smoothed round-trip time.
    #[must_use]
    pub fn smoothed_rtt(&self) -> Option<Duration> {
        self.smoothed.map(Duration::from_secs_f64)
    }

    /// Returns the lowest round-trip time seen.
    ///
    /// The minimum approximates the path's latency without queueing, which is
    /// what congestion control compares against to detect buffer buildup.
    #[must_use]
    pub fn min_rtt(&self) -> Option<Duration> {
        self.min_rtt.map(Duration::from_secs_f64)
    }

    /// Returns the current variation estimate.
    #[must_use]
    pub fn variation(&self) -> Duration {
        Duration::from_secs_f64(self.variation)
    }

    /// Returns how many samples have been recorded.
    #[must_use]
    pub const fn samples(&self) -> u64 {
        self.samples
    }

    /// Returns the retransmission timeout: smoothed RTT plus four variations.
    ///
    /// Returns [`MIN_RETRANSMIT_TIMEOUT`] before any sample exists, and clamps
    /// the result to the configured bounds.
    #[must_use]
    pub fn retransmit_timeout(&self) -> Duration {
        let Some(smoothed) = self.smoothed else {
            return MIN_RETRANSMIT_TIMEOUT;
        };

        let timeout = Duration::from_secs_f64(smoothed + 4.0 * self.variation);
        timeout.clamp(MIN_RETRANSMIT_TIMEOUT, MAX_RETRANSMIT_TIMEOUT)
    }

    /// Discards all history.
    pub fn reset(&mut self) {
        self.smoothed = None;
        self.variation = 0.0;
        self.min_rtt = None;
        self.samples = 0;
    }
}

impl Default for RttEstimator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_estimator_reports_nothing() {
        let estimator = BandwidthEstimator::new();
        assert!(estimator.bytes_per_second().is_none());
        assert!(!estimator.is_confident());
    }

    #[test]
    fn the_first_sample_becomes_the_estimate() {
        let mut estimator = BandwidthEstimator::new();
        estimator.sample(1000, Duration::from_secs(1));

        let estimate = estimator.bytes_per_second().expect("one sample exists");
        assert!(
            (estimate - 1000.0).abs() < 1.0,
            "the first sample should be taken at face value, got {estimate}"
        );
    }

    #[test]
    fn repeated_samples_converge() {
        let mut estimator = BandwidthEstimator::new();
        for _ in 0..50 {
            estimator.sample(2000, Duration::from_secs(1));
        }

        let estimate = estimator.bytes_per_second().expect("samples exist");
        assert!((estimate - 2000.0).abs() < 1.0, "got {estimate}");
        assert!(estimator.is_confident());
    }

    #[test]
    fn a_single_outlier_does_not_dominate() {
        let mut estimator = BandwidthEstimator::new();
        for _ in 0..30 {
            estimator.sample(1000, Duration::from_secs(1));
        }

        // One wildly fast sample.
        estimator.sample(100_000, Duration::from_secs(1));

        let estimate = estimator.bytes_per_second().expect("samples exist");
        assert!(
            estimate < 20_000.0,
            "smoothing should damp an outlier, got {estimate}"
        );
        assert!(
            (estimator.peak_bytes_per_second() - 100_000.0).abs() < 1.0,
            "the peak should still record it"
        );
    }

    #[test]
    fn the_estimate_follows_a_sustained_change() {
        let mut estimator = BandwidthEstimator::new();
        for _ in 0..30 {
            estimator.sample(1000, Duration::from_secs(1));
        }

        // Bandwidth genuinely drops and stays down.
        for _ in 0..40 {
            estimator.sample(200, Duration::from_secs(1));
        }

        let estimate = estimator.bytes_per_second().expect("samples exist");
        assert!(
            (estimate - 200.0).abs() < 50.0,
            "a sustained change should be tracked, got {estimate}"
        );
    }

    #[test]
    fn degenerate_samples_are_ignored() {
        let mut estimator = BandwidthEstimator::new();
        estimator.sample(1000, Duration::ZERO);
        estimator.sample(0, Duration::from_secs(1));

        assert!(
            estimator.bytes_per_second().is_none(),
            "a zero interval would imply infinite bandwidth"
        );
        assert_eq!(estimator.samples(), 0);
    }

    #[test]
    fn resetting_clears_history() {
        let mut estimator = BandwidthEstimator::new();
        estimator.sample(1000, Duration::from_secs(1));
        estimator.reset();

        assert!(estimator.bytes_per_second().is_none());
        assert!((estimator.peak_bytes_per_second() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn an_unsampled_rtt_returns_the_floor() {
        let estimator = RttEstimator::new();
        assert!(estimator.smoothed_rtt().is_none());
        assert_eq!(estimator.retransmit_timeout(), MIN_RETRANSMIT_TIMEOUT);
    }

    #[test]
    fn a_stable_link_converges_to_its_rtt() {
        let mut estimator = RttEstimator::new();
        for _ in 0..40 {
            estimator.sample(Duration::from_millis(50));
        }

        let smoothed = estimator.smoothed_rtt().expect("samples exist");
        assert!(
            (smoothed.as_millis() as i64 - 50).abs() <= 2,
            "expected ~50ms, got {smoothed:?}"
        );

        // A stable link has little variation, so the timeout sits near the RTT.
        assert!(estimator.variation() < Duration::from_millis(10));
    }

    #[test]
    fn a_jittery_link_produces_a_wider_timeout() {
        let mut stable = RttEstimator::new();
        let mut jittery = RttEstimator::new();

        for i in 0..40 {
            stable.sample(Duration::from_millis(100));
            // Alternate wildly around the same mean.
            jittery.sample(Duration::from_millis(if i % 2 == 0 { 20 } else { 180 }));
        }

        assert!(
            jittery.retransmit_timeout() > stable.retransmit_timeout(),
            "variation must widen the timeout: jittery {:?} vs stable {:?}",
            jittery.retransmit_timeout(),
            stable.retransmit_timeout()
        );
    }

    #[test]
    fn the_timeout_is_bounded() {
        let mut fast = RttEstimator::new();
        for _ in 0..20 {
            fast.sample(Duration::from_micros(50));
        }
        assert_eq!(
            fast.retransmit_timeout(),
            MIN_RETRANSMIT_TIMEOUT,
            "a very fast link must not produce a hair-trigger timeout"
        );

        let mut slow = RttEstimator::new();
        for _ in 0..20 {
            slow.sample(Duration::from_secs(120));
        }
        assert_eq!(slow.retransmit_timeout(), MAX_RETRANSMIT_TIMEOUT);
    }

    #[test]
    fn the_minimum_rtt_is_remembered() {
        let mut estimator = RttEstimator::new();
        estimator.sample(Duration::from_millis(100));
        estimator.sample(Duration::from_millis(30));
        estimator.sample(Duration::from_millis(200));

        assert_eq!(estimator.min_rtt(), Some(Duration::from_millis(30)));
    }
}
