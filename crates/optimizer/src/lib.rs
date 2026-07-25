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
//! * [`Optimizer`] — converts those estimates into a [`Recommendation`]:
//!   payload size, compression level, retry timeout, and bytes in flight.
//!
//! ## Example
//!
//! ```
//! use std::time::Duration;
//! use nexusnet_optimizer::{CompressionAdvice, Optimizer};
//!
//! let mut optimizer = Optimizer::new();
//! for _ in 0..10 {
//!     optimizer.record_delivery(32 * 1024, Duration::from_secs(1));
//!     optimizer.record_rtt(Duration::from_millis(300));
//! }
//!
//! let advice = optimizer.recommend();
//! // A slow link: compressing hard repays the CPU cost many times over.
//! assert_eq!(advice.compression, CompressionAdvice::Maximum);
//! ```
//!
//! ## Advice, not action
//!
//! Nothing here sends data or reaches into another subsystem. [`Optimizer`]
//! returns recommendations that a caller may apply or ignore. That keeps the
//! policy testable on its own and leaves the mechanism crates — transport,
//! compression, scheduler — free of any dependency on it.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod advisor;
mod estimate;

pub use crate::advisor::{
    CompressionAdvice, Optimizer, Recommendation, DEFAULT_PAYLOAD, FAST_LINK_BYTES_PER_SECOND,
    MAX_PAYLOAD, MIN_PAYLOAD, SLOW_LINK_BYTES_PER_SECOND,
};
pub use crate::estimate::{
    BandwidthEstimator, RttEstimator, DEFAULT_SMOOTHING, MAX_RETRANSMIT_TIMEOUT,
    MIN_RETRANSMIT_TIMEOUT,
};
