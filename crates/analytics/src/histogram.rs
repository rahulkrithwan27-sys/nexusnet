//! Distribution tracking for latency and other measurements.
//!
//! Averages hide exactly what matters on a network. A mean round-trip time of
//! 40 ms is consistent with every request taking 40 ms, and equally consistent
//! with 95% taking 5 ms while the rest take 700 ms. The second link is the one
//! users complain about, and only percentiles distinguish them.
//!
//! [`Histogram`] uses fixed logarithmic buckets rather than storing samples.
//! That bounds memory regardless of how long it runs, which a network process
//! needs — the alternative grows without limit or discards history arbitrarily.
//! The cost is that percentiles are accurate to the bucket width, which widens
//! for larger values; that is the right trade when the question is "is the tail
//! 10 ms or 500 ms", not "is it 412 ms or 415 ms".

use std::time::Duration;

/// The number of buckets spanning the tracked range.
///
/// Each bucket covers roughly a 26% increase over the one below it, giving
/// about 5% relative accuracy in the middle of a bucket across seven orders of
/// magnitude.
pub const BUCKET_COUNT: usize = 64;

/// The smallest value distinguished, in microseconds.
const MIN_MICROS: f64 = 1.0;

/// The growth factor between adjacent buckets.
///
/// Chosen so [`BUCKET_COUNT`] buckets span 1 microsecond to roughly 100
/// seconds, which covers everything from a local round trip to a badly stalled
/// one.
const GROWTH: f64 = 1.26;

/// A fixed-memory distribution of measurements.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use nexusnet_analytics::Histogram;
///
/// let mut histogram = Histogram::new();
/// for millis in [5, 6, 5, 7, 6, 200] {
///     histogram.record(Duration::from_millis(millis));
/// }
///
/// // The mean is dragged upward by the outlier; the median is not.
/// let median = histogram.percentile(50.0).expect("samples exist");
/// assert!(median < Duration::from_millis(20));
///
/// // The tail is visible, which is the point.
/// let tail = histogram.percentile(99.0).expect("samples exist");
/// assert!(tail >= Duration::from_millis(100));
/// ```
#[derive(Debug, Clone)]
pub struct Histogram {
    buckets: [u64; BUCKET_COUNT],
    count: u64,
    sum_micros: f64,
    min_micros: Option<f64>,
    max_micros: Option<f64>,
}

impl Histogram {
    /// Creates an empty histogram.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buckets: [0; BUCKET_COUNT],
            count: 0,
            sum_micros: 0.0,
            min_micros: None,
            max_micros: None,
        }
    }

    /// Records a measurement.
    pub fn record(&mut self, value: Duration) {
        self.record_micros(value.as_secs_f64() * 1_000_000.0);
    }

    /// Records a measurement given directly in microseconds.
    ///
    /// Negative and non-finite values are ignored rather than corrupting the
    /// distribution.
    pub fn record_micros(&mut self, micros: f64) {
        if !micros.is_finite() || micros < 0.0 {
            return;
        }

        self.buckets[Self::bucket_for(micros)] += 1;
        self.count += 1;
        self.sum_micros += micros;

        self.min_micros = Some(self.min_micros.map_or(micros, |min| min.min(micros)));
        self.max_micros = Some(self.max_micros.map_or(micros, |max| max.max(micros)));
    }

    /// Returns how many measurements were recorded.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// Returns `true` if nothing has been recorded.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns the smallest measurement.
    #[must_use]
    pub fn min(&self) -> Option<Duration> {
        self.min_micros.map(micros_to_duration)
    }

    /// Returns the largest measurement.
    #[must_use]
    pub fn max(&self) -> Option<Duration> {
        self.max_micros.map(micros_to_duration)
    }

    /// Returns the arithmetic mean.
    ///
    /// Kept alongside the percentiles because the gap between mean and median
    /// is itself informative: a mean far above the median means a heavy tail.
    #[must_use]
    pub fn mean(&self) -> Option<Duration> {
        if self.count == 0 {
            return None;
        }

        Some(micros_to_duration(self.sum_micros / self.count as f64))
    }

    /// Returns the value below which `percentile` percent of measurements fall.
    ///
    /// `percentile` is clamped to `0.0..=100.0`. Returns `None` if nothing has
    /// been recorded. The result is accurate to the width of the containing
    /// bucket.
    #[must_use]
    pub fn percentile(&self, percentile: f64) -> Option<Duration> {
        if self.count == 0 {
            return None;
        }

        let clamped = percentile.clamp(0.0, 100.0);
        // `ceil` so that p100 lands on the final sample rather than one short.
        let target = ((clamped / 100.0) * self.count as f64).ceil().max(1.0) as u64;

        let mut seen = 0_u64;
        for (index, &count) in self.buckets.iter().enumerate() {
            seen += count;
            if seen >= target {
                // Report the bucket's upper bound, so a percentile is never an
                // underestimate of the latency actually observed.
                return Some(micros_to_duration(Self::bucket_upper_bound(index)));
            }
        }

        self.max()
    }

    /// Returns the median.
    #[must_use]
    pub fn median(&self) -> Option<Duration> {
        self.percentile(50.0)
    }

    /// Returns a summary of the distribution.
    #[must_use]
    pub fn summary(&self) -> DistributionSummary {
        DistributionSummary {
            count: self.count,
            min: self.min(),
            max: self.max(),
            mean: self.mean(),
            p50: self.percentile(50.0),
            p90: self.percentile(90.0),
            p99: self.percentile(99.0),
        }
    }

    /// Merges another histogram into this one.
    ///
    /// Used to combine per-connection distributions into a per-process view
    /// without keeping every sample.
    pub fn merge(&mut self, other: &Self) {
        for (slot, count) in self.buckets.iter_mut().zip(other.buckets.iter()) {
            *slot += count;
        }

        self.count += other.count;
        self.sum_micros += other.sum_micros;

        if let Some(other_min) = other.min_micros {
            self.min_micros = Some(self.min_micros.map_or(other_min, |min| min.min(other_min)));
        }
        if let Some(other_max) = other.max_micros {
            self.max_micros = Some(self.max_micros.map_or(other_max, |max| max.max(other_max)));
        }
    }

    /// Discards every measurement.
    pub fn clear(&mut self) {
        self.buckets = [0; BUCKET_COUNT];
        self.count = 0;
        self.sum_micros = 0.0;
        self.min_micros = None;
        self.max_micros = None;
    }

    /// Returns the bucket index a value falls in.
    fn bucket_for(micros: f64) -> usize {
        if micros < MIN_MICROS {
            return 0;
        }

        let index = (micros / MIN_MICROS).ln() / GROWTH.ln();

        if index.is_finite() && index >= 0.0 {
            (index as usize).min(BUCKET_COUNT - 1)
        } else {
            0
        }
    }

    /// Returns the upper bound of a bucket, in microseconds.
    fn bucket_upper_bound(index: usize) -> f64 {
        MIN_MICROS * GROWTH.powi(index as i32 + 1)
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

/// A point-in-time summary of a distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DistributionSummary {
    /// How many measurements were recorded.
    pub count: u64,
    /// The smallest measurement.
    pub min: Option<Duration>,
    /// The largest measurement.
    pub max: Option<Duration>,
    /// The arithmetic mean.
    pub mean: Option<Duration>,
    /// The median.
    pub p50: Option<Duration>,
    /// The 90th percentile.
    pub p90: Option<Duration>,
    /// The 99th percentile.
    pub p99: Option<Duration>,
}

impl DistributionSummary {
    /// Returns `true` if the distribution has a heavy tail.
    ///
    /// True when the 99th percentile is at least four times the median, which
    /// indicates a minority of requests behaving very differently from the
    /// rest — the situation an average would conceal entirely.
    #[must_use]
    pub fn has_heavy_tail(&self) -> bool {
        match (self.p50, self.p99) {
            (Some(median), Some(tail)) if !median.is_zero() => {
                tail.as_secs_f64() >= median.as_secs_f64() * 4.0
            }
            _ => false,
        }
    }
}

/// Converts microseconds to a [`Duration`], guarding against bad input.
fn micros_to_duration(micros: f64) -> Duration {
    if micros.is_finite() && micros > 0.0 {
        Duration::from_secs_f64(micros / 1_000_000.0)
    } else {
        Duration::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_histogram_reports_nothing() {
        let histogram = Histogram::new();

        assert!(histogram.is_empty());
        assert_eq!(histogram.count(), 0);
        assert!(histogram.mean().is_none());
        assert!(histogram.percentile(50.0).is_none());
        assert!(histogram.min().is_none());
    }

    #[test]
    fn measurements_are_counted() {
        let mut histogram = Histogram::new();
        for _ in 0..10 {
            histogram.record(Duration::from_millis(5));
        }

        assert_eq!(histogram.count(), 10);
        assert!(!histogram.is_empty());
    }

    #[test]
    fn extremes_are_tracked_exactly() {
        let mut histogram = Histogram::new();
        histogram.record(Duration::from_millis(3));
        histogram.record(Duration::from_millis(900));
        histogram.record(Duration::from_millis(50));

        // Min and max are stored directly, so they are exact rather than
        // bucketed.
        assert_eq!(histogram.min(), Some(Duration::from_millis(3)));
        assert_eq!(histogram.max(), Some(Duration::from_millis(900)));
    }

    #[test]
    fn the_mean_is_computed() {
        let mut histogram = Histogram::new();
        for millis in [10, 20, 30] {
            histogram.record(Duration::from_millis(millis));
        }

        let mean = histogram.mean().expect("samples exist");
        assert!(
            (mean.as_millis() as i64 - 20).abs() <= 1,
            "expected ~20ms, got {mean:?}"
        );
    }

    #[test]
    fn percentiles_are_ordered() {
        let mut histogram = Histogram::new();
        for millis in 1..=100 {
            histogram.record(Duration::from_millis(millis));
        }

        let p50 = histogram.percentile(50.0).expect("samples exist");
        let p90 = histogram.percentile(90.0).expect("samples exist");
        let p99 = histogram.percentile(99.0).expect("samples exist");

        assert!(p50 <= p90, "p50 {p50:?} exceeded p90 {p90:?}");
        assert!(p90 <= p99, "p90 {p90:?} exceeded p99 {p99:?}");
    }

    #[test]
    fn percentiles_land_near_the_true_value() {
        let mut histogram = Histogram::new();
        for millis in 1..=1000 {
            histogram.record(Duration::from_millis(millis));
        }

        let p50 = histogram.percentile(50.0).expect("samples exist");
        // Buckets are ~26% wide, so allow generous but meaningful tolerance.
        assert!(
            p50 >= Duration::from_millis(450) && p50 <= Duration::from_millis(700),
            "median of 1..=1000ms should be near 500ms, got {p50:?}"
        );
    }

    #[test]
    fn the_tail_is_visible_where_an_average_would_hide_it() {
        let mut histogram = Histogram::new();

        // 95 fast requests and 5 very slow ones.
        for _ in 0..95 {
            histogram.record(Duration::from_millis(5));
        }
        for _ in 0..5 {
            histogram.record(Duration::from_millis(700));
        }

        let median = histogram.median().expect("samples exist");
        let p99 = histogram.percentile(99.0).expect("samples exist");

        assert!(
            median < Duration::from_millis(20),
            "most requests were fast, got {median:?}"
        );
        assert!(
            p99 >= Duration::from_millis(400),
            "the slow minority must be visible, got {p99:?}"
        );
        assert!(histogram.summary().has_heavy_tail());
    }

    #[test]
    fn a_uniform_distribution_has_no_heavy_tail() {
        let mut histogram = Histogram::new();
        for _ in 0..100 {
            histogram.record(Duration::from_millis(50));
        }

        assert!(!histogram.summary().has_heavy_tail());
    }

    #[test]
    fn percentile_arguments_are_clamped() {
        let mut histogram = Histogram::new();
        histogram.record(Duration::from_millis(10));

        assert!(histogram.percentile(-50.0).is_some());
        assert!(histogram.percentile(500.0).is_some());
    }

    #[test]
    fn degenerate_measurements_are_ignored() {
        let mut histogram = Histogram::new();
        histogram.record_micros(f64::NAN);
        histogram.record_micros(f64::INFINITY);
        histogram.record_micros(-5.0);

        assert!(
            histogram.is_empty(),
            "invalid measurements must not enter the distribution"
        );
    }

    #[test]
    fn zero_is_a_valid_measurement() {
        let mut histogram = Histogram::new();
        histogram.record(Duration::ZERO);

        assert_eq!(histogram.count(), 1);
        assert_eq!(histogram.min(), Some(Duration::ZERO));
    }

    #[test]
    fn a_very_large_measurement_lands_in_the_last_bucket() {
        let mut histogram = Histogram::new();
        histogram.record(Duration::from_secs(3600));

        assert_eq!(histogram.count(), 1);
        assert_eq!(histogram.max(), Some(Duration::from_secs(3600)));
        assert!(histogram.percentile(100.0).is_some());
    }

    #[test]
    fn histograms_merge() {
        let mut first = Histogram::new();
        for _ in 0..50 {
            first.record(Duration::from_millis(10));
        }

        let mut second = Histogram::new();
        for _ in 0..50 {
            second.record(Duration::from_millis(200));
        }

        first.merge(&second);

        assert_eq!(first.count(), 100);
        assert_eq!(first.min(), Some(Duration::from_millis(10)));
        assert_eq!(first.max(), Some(Duration::from_millis(200)));
    }

    #[test]
    fn merging_an_empty_histogram_changes_nothing() {
        let mut histogram = Histogram::new();
        histogram.record(Duration::from_millis(10));
        let before = histogram.summary();

        histogram.merge(&Histogram::new());
        assert_eq!(histogram.summary(), before);
    }

    #[test]
    fn clearing_empties_the_histogram() {
        let mut histogram = Histogram::new();
        for _ in 0..10 {
            histogram.record(Duration::from_millis(10));
        }

        histogram.clear();
        assert!(histogram.is_empty());
        assert!(histogram.max().is_none());
    }

    #[test]
    fn memory_use_is_bounded_regardless_of_sample_count() {
        let mut histogram = Histogram::new();
        for i in 0..100_000_u64 {
            histogram.record(Duration::from_micros(i % 10_000));
        }

        // The whole point of bucketing: a hundred thousand samples occupy the
        // same fixed array as one.
        assert_eq!(histogram.count(), 100_000);
        assert_eq!(histogram.buckets.len(), BUCKET_COUNT);
    }
}
