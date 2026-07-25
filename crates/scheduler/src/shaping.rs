//! Traffic shaping.
//!
//! [`TrafficShaper`] sits between a queue and the wire and answers one
//! question: may this packet go now, and if not, how long until it can?
//!
//! It layers two limits over [`TokenBucket`]:
//!
//! * An **aggregate** limit, which caps total throughput.
//! * An optional **reservation** per priority class, guaranteeing a share of
//!   that aggregate to traffic that must not be squeezed out by bulk transfers.
//!
//! The reservation matters because an aggregate limit alone is first-come,
//! first-served: a large background upload can consume the entire budget and
//! leave a heartbeat waiting behind it. A class with a reservation draws on its
//! own bucket first and only then competes for the shared one.

use std::time::{Duration, Instant};

use crate::priority::Priority;
use crate::rate::TokenBucket;

/// The default burst allowance, expressed as seconds of the configured rate.
///
/// One second of burst absorbs ordinary clumping without letting a long idle
/// period bank an unbounded credit.
pub const DEFAULT_BURST_SECONDS: f64 = 1.0;

/// What the shaper decided about a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShapeDecision {
    /// The packet may be sent now; its cost has been charged.
    Send,
    /// The packet must wait. No cost has been charged.
    Wait {
        /// How long until enough credit will have accrued.
        delay: Duration,
    },
    /// The packet can never be sent under this configuration.
    ///
    /// Returned when a packet is larger than the entire burst capacity, so no
    /// amount of waiting would admit it. Raise the burst allowance or split the
    /// payload; waiting would block forever.
    Oversized {
        /// The packet length that could not be admitted.
        len: usize,
        /// The largest length this shaper can ever admit.
        capacity: u64,
    },
}

impl ShapeDecision {
    /// Returns `true` if the packet may be sent immediately.
    #[must_use]
    pub const fn is_send(&self) -> bool {
        matches!(self, Self::Send)
    }

    /// Returns the delay to wait, if the packet was held back.
    #[must_use]
    pub const fn delay(&self) -> Option<Duration> {
        match self {
            Self::Wait { delay } => Some(*delay),
            Self::Send | Self::Oversized { .. } => None,
        }
    }
}

/// Shapes outbound traffic to a configured rate.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, Instant};
/// use nexusnet_scheduler::{Priority, ShapeDecision, TrafficShaper};
///
/// let now = Instant::now();
/// // 10 KiB/s aggregate, with one second of burst.
/// let mut shaper = TrafficShaper::new_at(10_240.0, now);
///
/// // The burst allowance admits an initial clump.
/// assert!(shaper.admit_at(Priority::Normal, 10_240, now).is_send());
///
/// // The next packet must wait for credit to accrue.
/// let decision = shaper.admit_at(Priority::Normal, 1024, now);
/// assert!(decision.delay().is_some());
/// ```
#[derive(Debug)]
pub struct TrafficShaper {
    aggregate: TokenBucket,
    /// Per-class reserved buckets. `None` means the class has no reservation
    /// and draws only on the aggregate.
    reserved: [Option<TokenBucket>; 5],
    rate: f64,
    burst_seconds: f64,
    admitted: u64,
    delayed: u64,
    bytes_admitted: u64,
}

impl TrafficShaper {
    /// Creates a shaper limiting throughput to `bytes_per_second`.
    #[must_use]
    pub fn new(bytes_per_second: f64) -> Self {
        Self::new_at(bytes_per_second, Instant::now())
    }

    /// Creates a shaper with an explicit starting instant.
    ///
    /// Prefer this in tests and simulations, where driving the clock directly
    /// makes behavior deterministic instead of approximate.
    #[must_use]
    pub fn new_at(bytes_per_second: f64, now: Instant) -> Self {
        let rate = if bytes_per_second > 0.0 {
            bytes_per_second
        } else {
            f64::MIN_POSITIVE
        };
        let capacity = burst_capacity(rate, DEFAULT_BURST_SECONDS);

        Self {
            aggregate: TokenBucket::new_at(rate, capacity, now),
            reserved: [None, None, None, None, None],
            rate,
            burst_seconds: DEFAULT_BURST_SECONDS,
            admitted: 0,
            delayed: 0,
            bytes_admitted: 0,
        }
    }

    /// Sets the burst allowance, in seconds of the configured rate.
    ///
    /// Rebuilds the buckets, so any credit currently accrued is reset.
    #[must_use]
    pub fn with_burst_seconds(mut self, burst_seconds: f64) -> Self {
        self.burst_seconds = burst_seconds.clamp(0.01, 60.0);
        self.aggregate = TokenBucket::new(self.rate, burst_capacity(self.rate, self.burst_seconds));
        self
    }

    /// Reserves a fraction of the aggregate rate for one priority class.
    ///
    /// The fraction is clamped to `0.0..=1.0`. A reserved class draws on its own
    /// bucket first, so bulk traffic cannot starve it of the shared budget.
    #[must_use]
    pub fn with_reservation(mut self, priority: Priority, fraction: f64) -> Self {
        let fraction = fraction.clamp(0.0, 1.0);

        if fraction <= 0.0 {
            self.reserved[priority.index()] = None;
            return self;
        }

        let rate = self.rate * fraction;
        let capacity = burst_capacity(rate, self.burst_seconds);
        self.reserved[priority.index()] = Some(TokenBucket::new(rate, capacity));

        self
    }

    /// Returns the aggregate rate in bytes per second.
    #[must_use]
    pub const fn rate(&self) -> f64 {
        self.rate
    }

    /// Returns the largest packet this shaper can ever admit.
    #[must_use]
    pub const fn capacity(&self) -> u64 {
        self.aggregate.capacity()
    }

    /// Returns how many packets were admitted immediately.
    #[must_use]
    pub const fn admitted(&self) -> u64 {
        self.admitted
    }

    /// Returns how many packets were held back at least once.
    #[must_use]
    pub const fn delayed(&self) -> u64 {
        self.delayed
    }

    /// Returns the total bytes admitted.
    #[must_use]
    pub const fn bytes_admitted(&self) -> u64 {
        self.bytes_admitted
    }

    /// Revises the aggregate rate, keeping accrued credit.
    ///
    /// Reserved buckets are rescaled in proportion, so a reservation stays the
    /// same share of a changed total. Used by adaptive senders acting on a
    /// revised bandwidth estimate.
    pub fn set_rate(&mut self, bytes_per_second: f64) {
        let new_rate = if bytes_per_second > 0.0 {
            bytes_per_second
        } else {
            f64::MIN_POSITIVE
        };
        let scale = new_rate / self.rate;

        self.rate = new_rate;
        self.aggregate.set_rate(new_rate);

        for bucket in self.reserved.iter_mut().flatten() {
            let rescaled = bucket.rate() * scale;
            bucket.set_rate(rescaled);
        }
    }

    /// Decides whether a packet of `len` bytes in `priority` may be sent now.
    pub fn admit(&mut self, priority: Priority, len: usize) -> ShapeDecision {
        self.admit_at(priority, len, Instant::now())
    }

    /// Decides admission as of `now`.
    ///
    /// A reserved class spends its own credit first and falls back to the
    /// aggregate; an unreserved class uses the aggregate alone. Charging is
    /// all-or-nothing: a packet that must wait is charged nothing, so partial
    /// deductions cannot accumulate and stall a stream indefinitely.
    pub fn admit_at(&mut self, priority: Priority, len: usize, now: Instant) -> ShapeDecision {
        let tokens = len as u64;

        if tokens > self.aggregate.capacity() {
            return ShapeDecision::Oversized {
                len,
                capacity: self.aggregate.capacity(),
            };
        }

        // Try the class's own reservation first.
        if let Some(bucket) = self.reserved[priority.index()].as_mut() {
            if bucket.try_consume_at(tokens, now) {
                self.admitted += 1;
                self.bytes_admitted += tokens;
                return ShapeDecision::Send;
            }
        }

        if self.aggregate.try_consume_at(tokens, now) {
            self.admitted += 1;
            self.bytes_admitted += tokens;
            return ShapeDecision::Send;
        }

        // Wait for whichever source will be ready sooner.
        let aggregate_wait = self
            .aggregate
            .time_until_at(tokens, now)
            .unwrap_or(Duration::MAX);

        let reserved_wait = self.reserved[priority.index()]
            .as_mut()
            .and_then(|bucket| bucket.time_until_at(tokens, now))
            .unwrap_or(Duration::MAX);

        self.delayed += 1;

        ShapeDecision::Wait {
            delay: aggregate_wait.min(reserved_wait),
        }
    }

    /// Returns how long until a packet of `len` bytes could be admitted.
    ///
    /// Returns `None` when it never could, matching
    /// [`ShapeDecision::Oversized`].
    #[must_use]
    pub fn time_until(&mut self, priority: Priority, len: usize, now: Instant) -> Option<Duration> {
        let tokens = len as u64;

        if tokens > self.aggregate.capacity() {
            return None;
        }

        let aggregate_wait = self.aggregate.time_until_at(tokens, now)?;
        let reserved_wait = self.reserved[priority.index()]
            .as_mut()
            .and_then(|bucket| bucket.time_until_at(tokens, now))
            .unwrap_or(Duration::MAX);

        Some(aggregate_wait.min(reserved_wait))
    }
}

/// Returns a burst capacity of at least one byte for a rate and window.
fn burst_capacity(rate: f64, seconds: f64) -> u64 {
    let capacity = rate * seconds;

    if capacity.is_finite() && capacity >= 1.0 {
        capacity as u64
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start() -> Instant {
        Instant::now()
    }

    #[test]
    fn the_burst_allowance_admits_an_initial_clump() {
        let now = start();
        let mut shaper = TrafficShaper::new_at(1000.0, now);

        // One second of burst at 1000 bytes per second.
        assert!(shaper.admit_at(Priority::Normal, 1000, now).is_send());
        assert_eq!(shaper.admitted(), 1);
        assert_eq!(shaper.bytes_admitted(), 1000);
    }

    #[test]
    fn exceeding_the_rate_produces_a_wait() {
        let now = start();
        let mut shaper = TrafficShaper::new_at(1000.0, now);
        assert!(shaper.admit_at(Priority::Normal, 1000, now).is_send());

        let decision = shaper.admit_at(Priority::Normal, 500, now);
        let delay = decision.delay().expect("the packet must wait");
        assert!(
            (delay.as_secs_f64() - 0.5).abs() < 0.01,
            "500 bytes at 1000/s is half a second, got {delay:?}"
        );
        assert_eq!(shaper.delayed(), 1);
    }

    #[test]
    fn waiting_charges_nothing() {
        let now = start();
        let mut shaper = TrafficShaper::new_at(1000.0, now);
        assert!(shaper.admit_at(Priority::Normal, 1000, now).is_send());

        // Several refusals must not deduct anything.
        for _ in 0..5 {
            assert!(!shaper.admit_at(Priority::Normal, 500, now).is_send());
        }

        // After half a second exactly 500 bytes have accrued.
        let later = now + Duration::from_millis(500);
        assert!(
            shaper.admit_at(Priority::Normal, 500, later).is_send(),
            "refusals must not have consumed credit"
        );
    }

    #[test]
    fn credit_accrues_over_time() {
        let now = start();
        let mut shaper = TrafficShaper::new_at(1000.0, now);
        assert!(shaper.admit_at(Priority::Normal, 1000, now).is_send());

        assert!(shaper
            .admit_at(Priority::Normal, 250, now + Duration::from_millis(250))
            .is_send());
        assert!(shaper
            .admit_at(Priority::Normal, 750, now + Duration::from_secs(1))
            .is_send());
    }

    #[test]
    fn a_packet_larger_than_the_burst_is_rejected_outright() {
        let now = start();
        let mut shaper = TrafficShaper::new_at(1000.0, now);

        let decision = shaper.admit_at(Priority::Normal, 5000, now);
        assert!(
            matches!(decision, ShapeDecision::Oversized { len: 5000, .. }),
            "got {decision:?}"
        );
        assert!(
            decision.delay().is_none(),
            "waiting for an impossible packet would block forever"
        );
        assert!(shaper.time_until(Priority::Normal, 5000, now).is_none());
    }

    #[test]
    fn the_average_rate_is_enforced_over_time() {
        let now = start();
        let mut shaper = TrafficShaper::new_at(1000.0, now);

        let mut sent = 0_u64;
        // Offer 100 bytes every 10ms for one simulated second.
        for step in 0..100_u64 {
            let at = now + Duration::from_millis(step * 10);
            if shaper.admit_at(Priority::Normal, 100, at).is_send() {
                sent += 100;
            }
        }

        // One second of rate plus the initial full burst.
        assert!(
            (1000..=2000).contains(&sent),
            "sent {sent} bytes, expected the rate to bound it"
        );
    }

    #[test]
    fn a_reservation_survives_aggregate_exhaustion() {
        let now = start();
        // A quarter of the budget is reserved for critical traffic.
        let mut shaper =
            TrafficShaper::new_at(1000.0, now).with_reservation(Priority::Critical, 0.25);

        // Bulk traffic drains the whole aggregate bucket.
        assert!(shaper.admit_at(Priority::Background, 1000, now).is_send());
        assert!(!shaper.admit_at(Priority::Background, 100, now).is_send());

        // Critical traffic still has its own reserved credit.
        assert!(
            shaper.admit_at(Priority::Critical, 200, now).is_send(),
            "a reservation exists precisely so bulk traffic cannot starve it"
        );
    }

    #[test]
    fn an_unreserved_class_uses_only_the_aggregate() {
        let now = start();
        let mut shaper =
            TrafficShaper::new_at(1000.0, now).with_reservation(Priority::Critical, 0.5);

        assert!(shaper.admit_at(Priority::Normal, 1000, now).is_send());
        assert!(
            !shaper.admit_at(Priority::Normal, 100, now).is_send(),
            "normal traffic has no reservation to fall back on"
        );
    }

    #[test]
    fn a_zero_reservation_removes_it() {
        let now = start();
        let mut shaper = TrafficShaper::new_at(1000.0, now)
            .with_reservation(Priority::Critical, 0.5)
            .with_reservation(Priority::Critical, 0.0);

        assert!(shaper.admit_at(Priority::Background, 1000, now).is_send());
        assert!(!shaper.admit_at(Priority::Critical, 100, now).is_send());
    }

    #[test]
    fn the_rate_can_be_revised() {
        let now = start();
        let mut shaper = TrafficShaper::new_at(1000.0, now);
        assert!(shaper.admit_at(Priority::Normal, 1000, now).is_send());

        shaper.set_rate(4000.0);

        // At the new rate, 1000 bytes accrue in a quarter second.
        assert!(shaper
            .admit_at(Priority::Normal, 1000, now + Duration::from_millis(250))
            .is_send());
    }

    #[test]
    fn revising_the_rate_rescales_reservations() {
        let now = start();
        let mut shaper =
            TrafficShaper::new_at(1000.0, now).with_reservation(Priority::Critical, 0.5);

        shaper.set_rate(2000.0);

        // Drain both buckets, then check the reservation refills at its share
        // of the new total rather than the old one.
        while shaper.admit_at(Priority::Critical, 100, now).is_send() {}

        let later = now + Duration::from_secs(1);
        // The reservation is now 1000 bytes per second.
        assert!(shaper.admit_at(Priority::Critical, 900, later).is_send());
    }

    #[test]
    fn a_zero_rate_is_corrected() {
        let shaper = TrafficShaper::new_at(0.0, start());
        assert!(shaper.rate() > 0.0, "a zero rate would block forever");
        assert!(shaper.capacity() >= 1);
    }

    #[test]
    fn the_burst_window_is_configurable() {
        let generous = TrafficShaper::new(1000.0).with_burst_seconds(5.0);
        assert_eq!(generous.capacity(), 5000);

        // Clamped rather than accepted, since a zero burst admits nothing.
        let tight = TrafficShaper::new(1000.0).with_burst_seconds(0.0);
        assert!(tight.capacity() >= 1);
    }

    #[test]
    fn time_until_reports_the_wait() {
        let now = start();
        let mut shaper = TrafficShaper::new_at(1000.0, now);
        assert!(shaper.admit_at(Priority::Normal, 1000, now).is_send());

        let wait = shaper
            .time_until(Priority::Normal, 250, now)
            .expect("within capacity");
        assert!(
            (wait.as_secs_f64() - 0.25).abs() < 0.01,
            "expected ~250ms, got {wait:?}"
        );
    }
}
