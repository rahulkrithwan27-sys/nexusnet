//! # nexusnet-telemetry
//!
//! Metrics collection and export for NexusNet.
//!
//! ## What's here
//!
//! * [`MetricsRegistry`] — named counters, gauges, and histograms.
//! * [`prometheus`] and [`json`] — rendering a registry for external systems.
//!
//! ## Example
//!
//! ```
//! use std::time::Duration;
//! use nexusnet_telemetry::{prometheus, MetricsRegistry};
//!
//! let mut registry = MetricsRegistry::new();
//! registry.counter("frames_sent", "Frames written to the wire").increment(120);
//! registry.gauge("connections_open", "Currently open connections").set(4.0);
//! registry
//!     .histogram("request_latency", "End-to-end request latency")
//!     .record(Duration::from_millis(35));
//!
//! let exposition = prometheus(&registry);
//! assert!(exposition.contains("# TYPE frames_sent counter"));
//! ```
//!
//! ## Three instruments, chosen deliberately
//!
//! A **counter** only increases; exporters rely on that to compute rates
//! between scrapes, so a counter that could decrease would silently produce
//! wrong graphs. A **gauge** moves both ways. A **histogram** records a
//! distribution, because an average latency hides the tail that users actually
//! notice.
//!
//! ## Stable output
//!
//! Metrics are stored in sorted order, so two exports of unchanged state are
//! byte-identical. Unstable ordering makes exports impossible to diff and
//! breaks naive change detection.
//!
//! ## No I/O
//!
//! Exporters return a `String`. Where those bytes go — an HTTP response, a
//! file, a log line — is the caller's decision, which keeps this crate free of
//! any server or runtime dependency.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod export;
mod registry;

pub use crate::export::{json, prometheus};
pub use crate::registry::{
    CounterHandle, GaugeHandle, HistogramHandle, MetricKind, MetricSample, MetricValue,
    MetricsRegistry,
};
