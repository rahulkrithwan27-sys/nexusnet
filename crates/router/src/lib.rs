//! # nexusnet-router
//!
//! Route selection, load balancing, and health-aware failover.
//!
//! ## What's here
//!
//! * [`Router`] — holds endpoints, picks one per request, and withdraws those
//!   that fail.
//! * [`Strategy`] — round robin, weighted round robin, or least connections.
//! * [`HealthTracker`] — the circuit breaker behind automatic failover.
//!
//! ## Example
//!
//! ```
//! use std::time::Instant;
//! use nexusnet_router::{Router, Strategy};
//!
//! let now = Instant::now();
//! let mut router: Router<&str> = Router::new(Strategy::RoundRobin);
//!
//! let primary = router.add("10.0.0.1:9000");
//! let secondary = router.add("10.0.0.2:9000");
//!
//! assert_eq!(router.select(now), Some(primary));
//! assert_eq!(router.select(now), Some(secondary));
//!
//! // Repeated failures withdraw an endpoint automatically.
//! for _ in 0..3 {
//!     router.record_failure(primary, now);
//! }
//! assert_eq!(router.select(now), Some(secondary));
//! ```
//!
//! ## Recovery is the hard part
//!
//! Removing a failing endpoint is easy. Deciding when to put it back is what
//! goes wrong — return it too eagerly and traffic keeps hitting a broken
//! server; never return it and a transient blip permanently shrinks the pool.
//!
//! The router uses a circuit breaker. Consecutive failures withdraw an
//! endpoint; after a cooldown it becomes *recovering*, where a single probe
//! decides its fate. Success restores it, failure restarts the cooldown. One
//! request is risked rather than all of them, and a recovering endpoint is
//! probed ahead of ordinary selection because that probe is what restores
//! capacity.
//!
//! ## Reporting exhaustion
//!
//! [`Router::select`] returns `None` when every endpoint is withdrawn and none
//! is due a probe. That is deliberate: routing to a known-dead backend produces
//! a slow failure and wasted work, whereas an honest `None` lets the caller
//! apply backpressure or fail fast.
//!
//! ## An explicit clock
//!
//! Health transitions depend on time, so every method that needs it takes an
//! [`Instant`](std::time::Instant) from the caller. Cooldowns and probe timing
//! are therefore tested deterministically rather than by sleeping.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod health;
mod routing;

pub use crate::health::{
    Health, HealthPolicy, HealthTracker, DEFAULT_COOLDOWN, DEFAULT_FAILURE_THRESHOLD,
    DEFAULT_SUCCESS_THRESHOLD,
};
pub use crate::routing::{Endpoint, EndpointId, Router, RouterStats, Strategy};
