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
//! * [`TrafficShaper`] — an aggregate send rate with optional per-class
//!   reservations, so bulk transfers cannot squeeze out urgent traffic.
//! * [`RetryManager`] — retransmission scheduling with backed-off, jittered
//!   delays.
//! * [`PacketScheduler`] — the component that ties those together, and
//!   [`SchedulerMetrics`] reporting what it did.
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
//! ## Putting it together
//!
//! [`PacketScheduler`] is the intended entry point. A caller loops: poll, send
//! whatever comes back, report the outcome.
//!
//! ```
//! use std::time::Instant;
//! use nexusnet_scheduler::{Dispatch, PacketScheduler, Priority, SchedulerConfig};
//!
//! let now = Instant::now();
//! let mut scheduler: PacketScheduler<&str> =
//!     PacketScheduler::new_at(SchedulerConfig::new().with_rate(1_000_000.0), now);
//!
//! scheduler.enqueue(Priority::Background, 512, "bulk upload")?;
//! scheduler.enqueue(Priority::Critical, 64, "keepalive")?;
//!
//! match scheduler.poll_at(now) {
//!     Dispatch::Send(packet) => {
//!         // Urgent traffic goes first.
//!         assert_eq!(*packet.payload(), "keepalive");
//!         scheduler.acknowledge(packet.id());
//!     }
//!     Dispatch::Wait { delay } => { /* sleep for `delay`, then poll again */ }
//!     Dispatch::Idle => { /* nothing to do */ }
//!     // `Dispatch` is `#[non_exhaustive]`, so a catch-all is required and
//!     // future variants cannot silently break this match.
//!     _ => {}
//! }
//! # Ok::<(), nexusnet_scheduler::EnqueueError>(())
//! ```
//!
//! ## No I/O, and an explicit clock
//!
//! Nothing here sends data or owns a timer. The scheduler is a state machine
//! driven by an [`Instant`](std::time::Instant) the caller supplies, so every
//! timing-dependent behavior — rate limiting, backoff, priority under load — is
//! tested deterministically rather than by sleeping and hoping. The same type
//! therefore works from a Tokio task, a plain thread, or a simulation.
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
mod metrics;
mod packet;
mod priority;
mod rate;
mod retry;
mod shaping;

pub use crate::flow::{
    FlowController, FlowError, ReceiveWindow, SendWindow, DEFAULT_UPDATE_THRESHOLD, DEFAULT_WINDOW,
};
pub use crate::metrics::SchedulerMetrics;
pub use crate::packet::{Dispatch, Packet, PacketId, PacketScheduler, SchedulerConfig};
pub use crate::priority::{EnqueueError, Priority, PriorityQueue, QueueStats};
pub use crate::rate::TokenBucket;
pub use crate::retry::{
    RetryDecision, RetryManager, RetryPolicy, DEFAULT_MAX_ATTEMPTS, DEFAULT_MAX_RETRY_DELAY,
    DEFAULT_RETRY_DELAY,
};
pub use crate::shaping::{ShapeDecision, TrafficShaper, DEFAULT_BURST_SECONDS};
