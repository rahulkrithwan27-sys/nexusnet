//! # nexusnet-scheduler
//!
//! Traffic scheduling for NexusNet: deciding what to send, how fast, and in
//! what order.
//!
//! ## What's here
//!
//! * [`PriorityQueue`] — weighted fair queueing across [`Priority`] classes,
//!   so urgent traffic wins without starving anything else.
//! * [`TokenBucket`] — rate limiting and traffic shaping with a burst
//!   allowance.
//! * [`FlowController`] — per-stream credit windows, the mechanism that removes
//!   head-of-line blocking from a multiplexed session.
//!
//! ## Example
//!
//! ```
//! use nexusnet_scheduler::{Priority, PriorityQueue};
//!
//! let mut queue: PriorityQueue<&str> = PriorityQueue::new(64);
//! queue.enqueue(Priority::Background, "telemetry batch")?;
//! queue.enqueue(Priority::Critical, "keepalive")?;
//!
//! assert_eq!(queue.dequeue(), Some("keepalive"));
//! # Ok::<(), nexusnet_scheduler::EnqueueError>(())
//! ```
//!
//! ## Why weighted, not strict
//!
//! Strict priority is the obvious scheduling design and the wrong one: a steady
//! stream of urgent traffic starves everything beneath it, and background work
//! that never runs becomes an outage that looks like a mystery. Deficit round
//! robin gives each class a configured share, so the ratio between classes is a
//! decision rather than an accident.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod flow;
mod priority;
mod rate;

pub use crate::flow::{
    FlowController, FlowError, ReceiveWindow, SendWindow, DEFAULT_UPDATE_THRESHOLD, DEFAULT_WINDOW,
};
pub use crate::priority::{EnqueueError, Priority, PriorityQueue, QueueStats};
pub use crate::rate::TokenBucket;
