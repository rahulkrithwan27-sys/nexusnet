//! Network quality detection.
//!
//! A single number cannot describe a link. A connection with 100 MiB/s of
//! bandwidth and 20% packet loss is not a good connection, and averaging those
//! two facts produces a figure that flatters it. [`NetworkQuality`] therefore
//! grades each dimension separately and reports the **worst** of them.
//!
//! That choice is deliberate and conservative: the dimension that is failing is
//! the one that determines what the application actually experiences, so it is
//! the one worth adapting to.

use std::time::Duration;

use crate::estimate::DEFAULT_SMOOTHING;

/// A coarse grade for the current network conditions.
///
/// Discriminants ascend with quality, so ordering comparisons read naturally
/// and `min` selects the worst of several grades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
#[repr(u8)]
pub enum NetworkQuality {
    /// Barely usable: severe loss, very high latency, or almost no bandwidth.
    Critical = 0,
    /// Degraded enough that the application should notice and adapt.
    Poor = 1,
    /// Workable, but with headroom worth conserving.
    Fair = 2,
    /// Comfortable for ordinary traffic.
    Good = 3,
    /// Fast, low latency, and essentially lossless.
    Excellent = 4,
}

impl NetworkQuality {
    /// Every grade, worst first.
    pub const ALL: [Self; 5] = [
        Self::Critical,
        Self::Poor,
        Self::Fair,
        Self::Good,
        Self::Excellent,
    ];

    /// Returns a 0–4 score, higher being better.
    #[must_use]
    pub const fn score(self) -> u8 {
        self as u8
    }

    /// Returns `true` when conditions warrant conserving bandwidth.
    ///
    /// True for [`Fair`](Self::Fair) and below: `Fair` is the point at which
    /// spending CPU to save bytes starts paying off.
    #[must_use]
    pub const fn is_degraded(self) -> bool {
        matches!(self, Self::Critical | Self::Poor | Self::Fair)
    }

    /// Returns `true` when the link is in trouble rather than merely busy.
    #[must_use]
    pub const fn is_critical(self) -> bool {
        matches!(self, Self::Critical)
    }

    /// Returns a short human-readable description.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Critical => "critical: severe loss, latency, or bandwidth starvation",
            Self::Poor => "poor: degraded enough to require adaptation",
            Self::Fair => "fair: workable, with headroom worth conserving",
            Self::Good => "good: comfortable for ordinary traffic",
            Self::Excellent => "excellent: fast, low latency, essentially lossless",
        }
    }

    /// Grades a bandwidth estimate in bytes per second.
    #[must_use]
    pub fn from_bandwidth(bytes_per_second: f64) -> Self {
        if bytes_per_second >= 8.0 * 1024.0 * 1024.0 {
            Self::Excellent
        } else if bytes_per_second >= 1024.0 * 1024.0 {
            Self::Good
        } else if bytes_per_second >= 256.0 * 1024.0 {
            Self::Fair
        } else if bytes_per_second >= 32.0 * 1024.0 {
            Self::Poor
        } else {
            Self::Critical
        }
    }

    /// Grades a round-trip time.
    #[must_use]
    pub fn from_rtt(rtt: Duration) -> Self {
        let millis = rtt.as_millis();

        if millis <= 30 {
            Self::Excellent
        } else if millis <= 100 {
            Self::Good
        } else if millis <= 250 {
            Self::Fair
        } else if millis <= 600 {
            Self::Poor
        } else {
            Self::Critical
        }
    }

    /// Grades a packet loss ratio in `0.0..=1.0`.
    ///
    /// Loss is graded harshly because its effect compounds: every lost packet
    /// costs a retransmission *and* a round trip of delay, so a few percent
    /// hurts far more than the raw number suggests.
    #[must_use]
    pub fn from_loss(ratio: f64) -> Self {
        if ratio <= 0.001 {
            Self::Excellent
        } else if ratio <= 0.01 {
            Self::Good
        } else if ratio <= 0.03 {
            Self::Fair
        } else if ratio <= 0.10 {
            Self::Poor
        } else {
            Self::Critical
        }
    }

    /// Combines several grades, returning the worst.
    ///
    /// Averaging would let a fast link disguise heavy loss. The failing
    /// dimension is what the application experiences, so it wins.
    #[must_use]
    pub fn worst_of(grades: &[Self]) -> Option<Self> {
        grades.iter().copied().min()
    }
}

impl Default for NetworkQuality {
    fn default() -> Self {
        Self::Good
    }
}

impl std::fmt::Display for NetworkQuality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Critical => "critical",
            Self::Poor => "poor",
            Self::Fair => "fair",
            Self::Good => "good",
            Self::Excellent => "excellent",
        };
        f.write_str(name)
    }
}

/// Estimates the packet loss ratio.
///
/// Loss is reported as deliveries and losses rather than a ratio, because the
/// caller knows counts and the estimator should own the smoothing. Like the
/// other estimators here it uses an exponentially weighted average, so a brief
/// burst of loss does not permanently condemn a healthy link.
#[derive(Debug, Clone)]
pub struct LossEstimator {
    smoothed: Option<f64>,
    smoothing: f64,
    delivered: u64,
    lost: u64,
}

impl LossEstimator {
    /// Creates an estimator with the default smoothing factor.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            smoothed: None,
            smoothing: DEFAULT_SMOOTHING,
            delivered: 0,
            lost: 0,
        }
    }

    /// Creates an estimator with a specific smoothing factor.
    ///
    /// Clamped to `0.01..=1.0`.
    #[must_use]
    pub fn with_smoothing(smoothing: f64) -> Self {
        Self {
            smoothing: smoothing.clamp(0.01, 1.0),
            ..Self::new()
        }
    }

    /// Records the outcome of a batch: `delivered` arrived, `lost` did not.
    ///
    /// An empty batch is ignored rather than treated as zero loss, which would
    /// let idle periods wash out a genuine problem.
    pub fn sample(&mut self, delivered: u64, lost: u64) {
        let total = delivered + lost;
        if total == 0 {
            return;
        }

        self.delivered += delivered;
        self.lost += lost;

        let ratio = lost as f64 / total as f64;
        self.smoothed = Some(match self.smoothed {
            Some(current) => current + self.smoothing * (ratio - current),
            None => ratio,
        });
    }

    /// Records a single delivered packet.
    pub fn record_delivered(&mut self) {
        self.sample(1, 0);
    }

    /// Records a single lost packet.
    pub fn record_lost(&mut self) {
        self.sample(0, 1);
    }

    /// Returns the smoothed loss ratio, if any sample exists.
    #[must_use]
    pub fn ratio(&self) -> Option<f64> {
        self.smoothed
    }

    /// Returns the total packets delivered.
    #[must_use]
    pub const fn delivered(&self) -> u64 {
        self.delivered
    }

    /// Returns the total packets lost.
    #[must_use]
    pub const fn lost(&self) -> u64 {
        self.lost
    }

    /// Returns the lifetime loss ratio across every sample.
    ///
    /// Unlike [`ratio`](Self::ratio) this does not decay, so it describes the
    /// whole connection rather than its current state.
    #[must_use]
    pub fn lifetime_ratio(&self) -> f64 {
        let total = self.delivered + self.lost;
        if total == 0 {
            0.0
        } else {
            self.lost as f64 / total as f64
        }
    }

    /// Returns `true` once enough packets have been seen to trust the estimate.
    ///
    /// A single loss out of two packets is a 50% ratio and means nothing.
    #[must_use]
    pub const fn is_confident(&self) -> bool {
        self.delivered + self.lost >= 20
    }

    /// Discards all history.
    pub fn reset(&mut self) {
        self.smoothed = None;
        self.delivered = 0;
        self.lost = 0;
    }
}

impl Default for LossEstimator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grades_order_by_quality() {
        assert!(NetworkQuality::Excellent > NetworkQuality::Good);
        assert!(NetworkQuality::Good > NetworkQuality::Fair);
        assert!(NetworkQuality::Fair > NetworkQuality::Poor);
        assert!(NetworkQuality::Poor > NetworkQuality::Critical);
        assert_eq!(NetworkQuality::Excellent.score(), 4);
    }

    #[test]
    fn bandwidth_is_graded_across_its_range() {
        assert_eq!(
            NetworkQuality::from_bandwidth(50.0 * 1024.0 * 1024.0),
            NetworkQuality::Excellent
        );
        assert_eq!(
            NetworkQuality::from_bandwidth(2.0 * 1024.0 * 1024.0),
            NetworkQuality::Good
        );
        assert_eq!(
            NetworkQuality::from_bandwidth(400.0 * 1024.0),
            NetworkQuality::Fair
        );
        assert_eq!(
            NetworkQuality::from_bandwidth(64.0 * 1024.0),
            NetworkQuality::Poor
        );
        assert_eq!(
            NetworkQuality::from_bandwidth(1024.0),
            NetworkQuality::Critical
        );
    }

    #[test]
    fn latency_is_graded_across_its_range() {
        assert_eq!(
            NetworkQuality::from_rtt(Duration::from_millis(10)),
            NetworkQuality::Excellent
        );
        assert_eq!(
            NetworkQuality::from_rtt(Duration::from_millis(80)),
            NetworkQuality::Good
        );
        assert_eq!(
            NetworkQuality::from_rtt(Duration::from_millis(200)),
            NetworkQuality::Fair
        );
        assert_eq!(
            NetworkQuality::from_rtt(Duration::from_millis(500)),
            NetworkQuality::Poor
        );
        assert_eq!(
            NetworkQuality::from_rtt(Duration::from_secs(2)),
            NetworkQuality::Critical
        );
    }

    #[test]
    fn loss_is_graded_harshly() {
        assert_eq!(NetworkQuality::from_loss(0.0), NetworkQuality::Excellent);
        assert_eq!(NetworkQuality::from_loss(0.005), NetworkQuality::Good);
        assert_eq!(NetworkQuality::from_loss(0.02), NetworkQuality::Fair);
        assert_eq!(NetworkQuality::from_loss(0.07), NetworkQuality::Poor);
        assert_eq!(
            NetworkQuality::from_loss(0.25),
            NetworkQuality::Critical,
            "a quarter of packets lost is not a usable link"
        );
    }

    #[test]
    fn the_worst_dimension_decides() {
        // Excellent bandwidth cannot disguise catastrophic loss.
        let combined = NetworkQuality::worst_of(&[
            NetworkQuality::Excellent,
            NetworkQuality::Excellent,
            NetworkQuality::Critical,
        ]);

        assert_eq!(
            combined,
            Some(NetworkQuality::Critical),
            "averaging would flatter a link that is actually failing"
        );
    }

    #[test]
    fn combining_nothing_yields_nothing() {
        assert_eq!(NetworkQuality::worst_of(&[]), None);
    }

    #[test]
    fn degradation_starts_at_fair() {
        assert!(!NetworkQuality::Excellent.is_degraded());
        assert!(!NetworkQuality::Good.is_degraded());
        assert!(NetworkQuality::Fair.is_degraded());
        assert!(NetworkQuality::Poor.is_degraded());
        assert!(NetworkQuality::Critical.is_degraded());

        assert!(NetworkQuality::Critical.is_critical());
        assert!(!NetworkQuality::Poor.is_critical());
    }

    #[test]
    fn grades_display_and_describe() {
        assert_eq!(NetworkQuality::Fair.to_string(), "fair");
        assert!(NetworkQuality::Poor.description().contains("poor"));
    }

    #[test]
    fn an_unsampled_loss_estimator_reports_nothing() {
        let estimator = LossEstimator::new();
        assert!(estimator.ratio().is_none());
        assert!(!estimator.is_confident());
        assert!((estimator.lifetime_ratio() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_lossless_link_reports_no_loss() {
        let mut estimator = LossEstimator::new();
        for _ in 0..50 {
            estimator.record_delivered();
        }

        assert!((estimator.ratio().expect("samples exist") - 0.0).abs() < 1e-9);
        assert!(estimator.is_confident());
        assert_eq!(estimator.delivered(), 50);
        assert_eq!(estimator.lost(), 0);
    }

    #[test]
    fn a_lossy_link_converges_on_its_rate() {
        let mut estimator = LossEstimator::new();
        // A steady one-in-ten loss rate.
        for _ in 0..200 {
            estimator.sample(9, 1);
        }

        let ratio = estimator.ratio().expect("samples exist");
        assert!(
            (ratio - 0.1).abs() < 0.02,
            "expected roughly 10% loss, got {ratio}"
        );
    }

    #[test]
    fn a_burst_of_loss_decays_rather_than_condemning_the_link() {
        let mut estimator = LossEstimator::new();
        for _ in 0..50 {
            estimator.sample(100, 0);
        }

        // One bad moment.
        estimator.sample(0, 50);
        let spiked = estimator.ratio().expect("samples exist");

        // Recovery.
        for _ in 0..50 {
            estimator.sample(100, 0);
        }
        let recovered = estimator.ratio().expect("samples exist");

        assert!(
            recovered < spiked,
            "a transient burst must not permanently condemn a healthy link"
        );
        assert!(recovered < 0.01);
    }

    #[test]
    fn the_lifetime_ratio_does_not_decay() {
        let mut estimator = LossEstimator::new();
        estimator.sample(90, 10);

        assert!((estimator.lifetime_ratio() - 0.1).abs() < 1e-9);

        // Later perfection does not erase the historical record.
        for _ in 0..100 {
            estimator.sample(10, 0);
        }
        assert!(estimator.lifetime_ratio() > 0.0);
        assert!(estimator.ratio().expect("samples exist") < 0.01);
    }

    #[test]
    fn empty_batches_are_ignored() {
        let mut estimator = LossEstimator::new();
        estimator.sample(0, 0);

        assert!(
            estimator.ratio().is_none(),
            "an idle period is not evidence of a lossless link"
        );
    }

    #[test]
    fn resetting_clears_history() {
        let mut estimator = LossEstimator::new();
        estimator.sample(50, 50);
        estimator.reset();

        assert!(estimator.ratio().is_none());
        assert_eq!(estimator.delivered(), 0);
    }
}
