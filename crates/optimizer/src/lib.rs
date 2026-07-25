//! # nexusnet-optimizer
//!
//! Adaptive optimization for NexusNet: measuring the network and turning those
//! measurements into sending decisions.
//!
//! ## What's here
//!
//! * [`BandwidthEstimator`] and [`RttEstimator`] — smoothed estimates of link
//!   capacity and latency, plus a retransmission timeout derived the way TCP
//!   derives it.
//! * [`LossEstimator`] and [`NetworkQuality`] — packet loss tracking and a
//!   coarse grade for the link as a whole.
//! * [`NetworkOptimizer`] — the entry point: observes conditions and returns an
//!   [`OptimizationPlan`] covering payload size, compression, caching, delta
//!   synchronization, retry timing, and bytes in flight, with
//!   [`OptimizationMetrics`] reporting what it saw.
//! * [`CompressionStrategy`], [`CacheStrategy`], and [`DeltaSyncStrategy`] —
//!   the individual decisions, usable on their own.
//! * [`CongestionDetector`] and [`CongestionWindow`] — congestion predicted
//!   from latency inflation, and the window that responds to it.
//! * [`TrendPredictor`] and [`advise_send`] — where conditions are heading, and
//!   what to do about it.
//! * [`Optimizer`] — the narrower predecessor of [`NetworkOptimizer`],
//!   producing a [`Recommendation`] from bandwidth and latency alone. Retained
//!   for callers that do not measure loss.
//!
//! ## Example
//!
//! ```
//! use std::time::Duration;
//! use nexusnet_optimizer::NetworkOptimizer;
//!
//! let mut optimizer = NetworkOptimizer::new();
//!
//! // Feed it what the transport observes.
//! for _ in 0..20 {
//!     optimizer.record_delivery(32 * 1024, Duration::from_secs(1));
//!     optimizer.record_rtt(Duration::from_millis(300));
//!     optimizer.record_loss(95, 5);
//! }
//!
//! let plan = optimizer.plan();
//!
//! // A scarce link: trade CPU and memory for bytes.
//! assert!(plan.quality.is_degraded());
//! assert!(plan.compression.enabled);
//! assert!(plan.delta_sync.enabled);
//! assert!(plan.cache.capacity_bytes > 0);
//! ```
//!
//! ## Predicting congestion, not reacting to it
//!
//! Loss-based congestion control waits for a packet to be dropped, by which
//! point the bottleneck queue is already full and every packet behind it has
//! been delayed. Queues fill before they overflow, and a filling queue shows up
//! as latency rising above the path's minimum — so
//! [`CongestionDetector`] reports [`CongestionSignal::Queueing`] while there is
//! still time to slow down.
//!
//! ```
//! use std::time::Duration;
//! use nexusnet_optimizer::{CongestionDetector, CongestionSignal};
//!
//! let mut detector = CongestionDetector::new();
//! for _ in 0..20 {
//!     detector.observe(Duration::from_millis(20));
//! }
//! for _ in 0..30 {
//!     detector.observe(Duration::from_millis(100));
//! }
//!
//! assert_eq!(detector.signal(), CongestionSignal::Queueing);
//! assert_eq!(detector.loss_events(), 0, "detected before anything was dropped");
//! ```
//!
//! ## Grading the link
//!
//! [`NetworkQuality`] grades bandwidth, latency, and loss separately and reports
//! the **worst** of them. Averaging would let a fast link disguise heavy packet
//! loss, and the failing dimension is what the application actually
//! experiences. Dimensions that have never been measured do not count against a
//! link, so a new connection does not look broken before anything is known
//! about it.
//!
//! ## Advice, not action
//!
//! Nothing here sends data or reaches into another subsystem. [`Optimizer`]
//! returns recommendations that a caller may apply or ignore. That keeps the
//! policy testable on its own and leaves the mechanism crates — transport,
//! compression, scheduler — free of any dependency on it.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod advisor;
mod congestion;
mod estimate;
mod network;
mod predict;
mod quality;
mod strategy;

pub use crate::advisor::{
    CompressionAdvice, Optimizer, Recommendation, DEFAULT_PAYLOAD, FAST_LINK_BYTES_PER_SECOND,
    MAX_PAYLOAD, MIN_PAYLOAD, SLOW_LINK_BYTES_PER_SECOND,
};
pub use crate::congestion::{
    CongestionDetector, CongestionSignal, CongestionState, CongestionWindow,
    DEFAULT_QUEUEING_THRESHOLD, DEFAULT_SEGMENT,
};
pub use crate::estimate::{
    BandwidthEstimator, RttEstimator, DEFAULT_SMOOTHING, MAX_RETRANSMIT_TIMEOUT,
    MIN_RETRANSMIT_TIMEOUT,
};
pub use crate::network::{NetworkOptimizer, OptimizationMetrics, OptimizationPlan};
pub use crate::predict::{
    advise_send, advise_send_under_congestion, Forecast, SendAdvice, Trend, TrendPredictor,
    DEFAULT_HORIZON, DEFAULT_WINDOW,
};
pub use crate::quality::{LossEstimator, NetworkQuality};
pub use crate::strategy::{
    CacheStrategy, CompressionStrategy, DeltaSyncStrategy, MIN_COMPRESSIBLE, MIN_DELTA_PAYLOAD,
};
