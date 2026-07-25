//! # nexusnet-analytics
//!
//! Measurement and statistics for NexusNet: what the network actually did.
//!
//! ## What's here
//!
//! * [`Histogram`] — distributions with percentiles, in bounded memory.
//! * [`RateMeter`] — throughput, smoothed and lifetime-averaged.
//! * [`ConnectionStats`] — per-connection bytes, frames, errors, loss, latency,
//!   and jitter, summarized by [`ConnectionSnapshot`].
//!
//! ## Percentiles, not averages
//!
//! A mean round-trip time of 40 ms is consistent with every request taking
//! 40 ms, and equally consistent with 95% taking 5 ms while the rest take
//! 700 ms. Those are very different links, and only the second generates
//! complaints. Everything here that measures a distribution reports percentiles
//! for that reason.
//!
//! ```
//! use std::time::Duration;
//! use nexusnet_analytics::Histogram;
//!
//! let mut latency = Histogram::new();
//! for _ in 0..95 {
//!     latency.record(Duration::from_millis(5));
//! }
//! for _ in 0..5 {
//!     latency.record(Duration::from_millis(700));
//! }
//!
//! // The median is fast, but the tail is not — and the tail is what users feel.
//! assert!(latency.median().expect("samples exist") < Duration::from_millis(20));
//! assert!(latency.summary().has_heavy_tail());
//! ```
//!
//! ## Bounded memory
//!
//! [`Histogram`] uses fixed logarithmic buckets rather than retaining samples,
//! so a process that runs for months uses the same memory as one that just
//! started. The cost is that percentiles are accurate to a bucket width rather
//! than exact, which is the right trade when the question is whether the tail
//! is 10 ms or 500 ms.
//!
//! ## An explicit clock
//!
//! Every time-dependent type has an `_at` constructor and `_at` methods taking
//! an [`Instant`](std::time::Instant). Rates computed from a supplied clock are
//! exact in tests, rather than depending on how long the test happened to take.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod histogram;
mod stats;

pub use crate::histogram::{DistributionSummary, Histogram, BUCKET_COUNT};
pub use crate::stats::{ConnectionSnapshot, ConnectionStats, RateMeter, DEFAULT_SMOOTHING};
