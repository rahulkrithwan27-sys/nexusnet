//! A registry of named metrics.
//!
//! Three instrument kinds cover what a networking framework needs to report:
//!
//! * A **counter** only ever increases — bytes sent, errors seen. Exporters
//!   rely on that monotonicity to compute rates across scrapes, so a counter
//!   that could decrease would silently produce wrong graphs.
//! * A **gauge** moves in both directions — connections open, queue depth.
//! * A **histogram** records a distribution — latency, payload size.
//!
//! The registry is deliberately simple and synchronous. It is a reporting
//! surface, not a hot path: values are updated by the components that own them
//! and read when something exports.

use std::collections::BTreeMap;
use std::time::Duration;

use nexusnet_analytics::{DistributionSummary, Histogram};

/// The kind of an instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MetricKind {
    /// A monotonically increasing total.
    Counter,
    /// A value that moves in both directions.
    Gauge,
    /// A distribution of measurements.
    Histogram,
}

impl std::fmt::Display for MetricKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
            Self::Histogram => "histogram",
        };
        f.write_str(name)
    }
}

/// The current value of one instrument.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MetricValue {
    /// A counter total.
    Counter(u64),
    /// A gauge reading.
    Gauge(f64),
    /// A distribution summary.
    Histogram(DistributionSummary),
}

impl MetricValue {
    /// Returns the kind of instrument this value came from.
    #[must_use]
    pub const fn kind(&self) -> MetricKind {
        match self {
            Self::Counter(_) => MetricKind::Counter,
            Self::Gauge(_) => MetricKind::Gauge,
            Self::Histogram(_) => MetricKind::Histogram,
        }
    }

    /// Returns the value as a float, for exporters that emit a single number.
    ///
    /// A histogram reports its count, since that is the one figure every
    /// exporter can represent without inventing a convention.
    #[must_use]
    pub fn as_f64(&self) -> f64 {
        match self {
            Self::Counter(value) => *value as f64,
            Self::Gauge(value) => *value,
            Self::Histogram(summary) => summary.count as f64,
        }
    }
}

/// One named metric.
#[derive(Debug, Clone)]
struct Metric {
    description: String,
    value: Instrument,
}

/// The mutable state behind a metric.
#[derive(Debug, Clone)]
enum Instrument {
    Counter(u64),
    Gauge(f64),
    Histogram(Box<Histogram>),
}

/// A named metric and its current value.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricSample {
    /// The metric name.
    pub name: String,
    /// A human-readable description, for exporters that carry help text.
    pub description: String,
    /// The current value.
    pub value: MetricValue,
}

/// A collection of named metrics.
///
/// Names are stored in sorted order, so exported output is stable between
/// scrapes. Unstable ordering makes diffs between two exports unreadable and
/// breaks naive change detection.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use nexusnet_telemetry::MetricsRegistry;
///
/// let mut registry = MetricsRegistry::new();
///
/// registry.counter("frames_sent", "Frames written to the wire").increment(3);
/// registry.gauge("connections_open", "Currently open connections").set(12.0);
/// registry
///     .histogram("request_latency", "End-to-end request latency")
///     .record(Duration::from_millis(40));
///
/// assert_eq!(registry.len(), 3);
/// ```
#[derive(Debug, Clone, Default)]
pub struct MetricsRegistry {
    metrics: BTreeMap<String, Metric>,
}

impl MetricsRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns how many metrics are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.metrics.len()
    }

    /// Returns `true` if nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.metrics.is_empty()
    }

    /// Returns a handle to a counter, creating it if needed.
    ///
    /// Registering an existing name with a different kind replaces it, since
    /// silently ignoring the second registration would leave a caller updating
    /// an instrument that never appears.
    pub fn counter<'a>(&'a mut self, name: &str, description: &str) -> CounterHandle<'a> {
        let metric = self.entry(name, description, || Instrument::Counter(0));

        if !matches!(metric.value, Instrument::Counter(_)) {
            metric.value = Instrument::Counter(0);
        }

        CounterHandle { metric }
    }

    /// Returns a handle to a gauge, creating it if needed.
    pub fn gauge<'a>(&'a mut self, name: &str, description: &str) -> GaugeHandle<'a> {
        let metric = self.entry(name, description, || Instrument::Gauge(0.0));

        if !matches!(metric.value, Instrument::Gauge(_)) {
            metric.value = Instrument::Gauge(0.0);
        }

        GaugeHandle { metric }
    }

    /// Returns a handle to a histogram, creating it if needed.
    pub fn histogram<'a>(&'a mut self, name: &str, description: &str) -> HistogramHandle<'a> {
        let metric = self.entry(name, description, || {
            Instrument::Histogram(Box::new(Histogram::new()))
        });

        if !matches!(metric.value, Instrument::Histogram(_)) {
            metric.value = Instrument::Histogram(Box::new(Histogram::new()));
        }

        HistogramHandle { metric }
    }

    /// Returns the current value of a metric.
    #[must_use]
    pub fn value(&self, name: &str) -> Option<MetricValue> {
        self.metrics.get(name).map(|metric| match &metric.value {
            Instrument::Counter(value) => MetricValue::Counter(*value),
            Instrument::Gauge(value) => MetricValue::Gauge(*value),
            Instrument::Histogram(histogram) => MetricValue::Histogram(histogram.summary()),
        })
    }

    /// Returns every metric, in stable name order.
    #[must_use]
    pub fn samples(&self) -> Vec<MetricSample> {
        self.metrics
            .iter()
            .map(|(name, metric)| MetricSample {
                name: name.clone(),
                description: metric.description.clone(),
                value: match &metric.value {
                    Instrument::Counter(value) => MetricValue::Counter(*value),
                    Instrument::Gauge(value) => MetricValue::Gauge(*value),
                    Instrument::Histogram(histogram) => MetricValue::Histogram(histogram.summary()),
                },
            })
            .collect()
    }

    /// Removes a metric, returning whether it was present.
    pub fn remove(&mut self, name: &str) -> bool {
        self.metrics.remove(name).is_some()
    }

    /// Removes every metric.
    pub fn clear(&mut self) {
        self.metrics.clear();
    }

    /// Returns the entry for `name`, creating it with `default` if absent.
    fn entry(
        &mut self,
        name: &str,
        description: &str,
        default: impl FnOnce() -> Instrument,
    ) -> &mut Metric {
        self.metrics
            .entry(name.to_owned())
            .or_insert_with(|| Metric {
                description: description.to_owned(),
                value: default(),
            })
    }
}

/// A handle for updating a counter.
#[derive(Debug)]
pub struct CounterHandle<'a> {
    metric: &'a mut Metric,
}

impl CounterHandle<'_> {
    /// Adds `amount` to the counter.
    pub fn increment(&mut self, amount: u64) {
        if let Instrument::Counter(value) = &mut self.metric.value {
            *value = value.saturating_add(amount);
        }
    }

    /// Returns the current total.
    #[must_use]
    pub fn get(&self) -> u64 {
        match self.metric.value {
            Instrument::Counter(value) => value,
            _ => 0,
        }
    }
}

/// A handle for updating a gauge.
#[derive(Debug)]
pub struct GaugeHandle<'a> {
    metric: &'a mut Metric,
}

impl GaugeHandle<'_> {
    /// Sets the gauge to `value`.
    ///
    /// Non-finite values are ignored; a `NaN` gauge poisons every downstream
    /// aggregate it touches.
    pub fn set(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }

        if let Instrument::Gauge(slot) = &mut self.metric.value {
            *slot = value;
        }
    }

    /// Adds `delta` to the gauge, which may be negative.
    pub fn add(&mut self, delta: f64) {
        if !delta.is_finite() {
            return;
        }

        if let Instrument::Gauge(slot) = &mut self.metric.value {
            *slot += delta;
        }
    }

    /// Returns the current reading.
    #[must_use]
    pub fn get(&self) -> f64 {
        match self.metric.value {
            Instrument::Gauge(value) => value,
            _ => 0.0,
        }
    }
}

/// A handle for updating a histogram.
#[derive(Debug)]
pub struct HistogramHandle<'a> {
    metric: &'a mut Metric,
}

impl HistogramHandle<'_> {
    /// Records a measurement.
    pub fn record(&mut self, value: Duration) {
        if let Instrument::Histogram(histogram) = &mut self.metric.value {
            histogram.record(value);
        }
    }

    /// Returns a summary of the distribution.
    #[must_use]
    pub fn summary(&self) -> DistributionSummary {
        match &self.metric.value {
            Instrument::Histogram(histogram) => histogram.summary(),
            _ => DistributionSummary::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_registry_has_nothing() {
        let registry = MetricsRegistry::new();

        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.value("absent").is_none());
        assert!(registry.samples().is_empty());
    }

    #[test]
    fn counters_accumulate() {
        let mut registry = MetricsRegistry::new();

        registry.counter("bytes", "Bytes sent").increment(100);
        registry.counter("bytes", "Bytes sent").increment(50);

        assert_eq!(registry.value("bytes"), Some(MetricValue::Counter(150)));
        assert_eq!(registry.len(), 1, "the same name is one metric");
    }

    #[test]
    fn counters_saturate_rather_than_wrapping() {
        let mut registry = MetricsRegistry::new();

        registry
            .counter("huge", "A large total")
            .increment(u64::MAX);
        registry.counter("huge", "A large total").increment(10);

        assert_eq!(
            registry.value("huge"),
            Some(MetricValue::Counter(u64::MAX)),
            "a counter that wrapped would break rate calculations"
        );
    }

    #[test]
    fn gauges_move_in_both_directions() {
        let mut registry = MetricsRegistry::new();

        registry.gauge("connections", "Open connections").set(10.0);
        registry.gauge("connections", "Open connections").add(5.0);
        registry.gauge("connections", "Open connections").add(-3.0);

        assert_eq!(
            registry.value("connections"),
            Some(MetricValue::Gauge(12.0))
        );
    }

    #[test]
    fn non_finite_gauge_values_are_ignored() {
        let mut registry = MetricsRegistry::new();

        registry.gauge("depth", "Queue depth").set(5.0);
        registry.gauge("depth", "Queue depth").set(f64::NAN);
        registry.gauge("depth", "Queue depth").add(f64::INFINITY);

        assert_eq!(
            registry.value("depth"),
            Some(MetricValue::Gauge(5.0)),
            "a NaN gauge would poison every aggregate downstream"
        );
    }

    #[test]
    fn histograms_record_distributions() {
        let mut registry = MetricsRegistry::new();

        for millis in [10, 20, 30, 40] {
            registry
                .histogram("latency", "Request latency")
                .record(Duration::from_millis(millis));
        }

        let Some(MetricValue::Histogram(summary)) = registry.value("latency") else {
            panic!("expected a histogram");
        };

        assert_eq!(summary.count, 4);
        assert!(summary.p50.is_some());
    }

    #[test]
    fn samples_are_returned_in_stable_order() {
        let mut registry = MetricsRegistry::new();

        registry.counter("zebra", "Last").increment(1);
        registry.counter("alpha", "First").increment(1);
        registry.counter("middle", "Middle").increment(1);

        let names: Vec<String> = registry.samples().into_iter().map(|s| s.name).collect();

        assert_eq!(
            names,
            vec!["alpha", "middle", "zebra"],
            "unstable ordering makes exports impossible to diff"
        );
    }

    #[test]
    fn descriptions_are_retained() {
        let mut registry = MetricsRegistry::new();
        registry
            .counter("frames", "Frames written to the wire")
            .increment(1);

        let sample = registry.samples().into_iter().next().expect("one metric");
        assert_eq!(sample.description, "Frames written to the wire");
    }

    #[test]
    fn re_registering_with_a_different_kind_replaces_the_metric() {
        let mut registry = MetricsRegistry::new();

        registry.counter("thing", "As a counter").increment(5);
        registry.gauge("thing", "Now a gauge").set(2.0);

        assert_eq!(
            registry.value("thing"),
            Some(MetricValue::Gauge(2.0)),
            "silently ignoring this would leave the caller updating nothing"
        );
    }

    #[test]
    fn metrics_can_be_removed() {
        let mut registry = MetricsRegistry::new();
        registry.counter("temp", "Temporary").increment(1);

        assert!(registry.remove("temp"));
        assert!(!registry.remove("temp"), "removing twice is not an error");
        assert!(registry.is_empty());
    }

    #[test]
    fn clearing_empties_the_registry() {
        let mut registry = MetricsRegistry::new();
        registry.counter("a", "A").increment(1);
        registry.gauge("b", "B").set(1.0);

        registry.clear();
        assert!(registry.is_empty());
    }

    #[test]
    fn values_convert_to_a_single_number() {
        assert!((MetricValue::Counter(42).as_f64() - 42.0).abs() < f64::EPSILON);
        assert!((MetricValue::Gauge(1.5).as_f64() - 1.5).abs() < f64::EPSILON);

        // `DistributionSummary` is non-exhaustive, so build a real one.
        let mut registry = MetricsRegistry::new();
        for _ in 0..7 {
            registry
                .histogram("h", "Histogram")
                .record(Duration::from_millis(1));
        }
        let value = registry.value("h").expect("registered");
        assert!((value.as_f64() - 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn kinds_are_reported() {
        assert_eq!(MetricValue::Counter(0).kind(), MetricKind::Counter);
        assert_eq!(MetricValue::Gauge(0.0).kind(), MetricKind::Gauge);
        assert_eq!(MetricKind::Histogram.to_string(), "histogram");
    }

    #[test]
    fn handles_read_back_their_values() {
        let mut registry = MetricsRegistry::new();

        let mut counter = registry.counter("c", "Counter");
        counter.increment(7);
        assert_eq!(counter.get(), 7);

        let mut gauge = registry.gauge("g", "Gauge");
        gauge.set(3.5);
        assert!((gauge.get() - 3.5).abs() < f64::EPSILON);

        let mut histogram = registry.histogram("h", "Histogram");
        histogram.record(Duration::from_millis(5));
        assert_eq!(histogram.summary().count, 1);
    }
}
