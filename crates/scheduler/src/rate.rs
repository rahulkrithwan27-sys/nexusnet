//! Rate limiting and traffic shaping.
//!
//! [`TokenBucket`] is the standard shaping primitive: tokens accrue at a steady
//! rate up to a burst ceiling, and sending spends them. The burst allowance is
//! the point — a strict rate limit with no burst behaves badly on real traffic,
//! which arrives in clumps rather than smoothly.
//!
//! ## Testing time-dependent behavior
//!
//! Every method has an `_at` variant taking an explicit [`Instant`]. Tests and
//! simulations drive the clock directly instead of sleeping, so rate-limit
//! behavior is verified deterministically rather than approximately.

use std::time::{Duration, Instant};

/// A token bucket rate limiter.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, Instant};
/// use nexusnet_scheduler::TokenBucket;
///
/// // 1000 bytes per second, allowing bursts up to 2000 bytes.
/// // `new_at` pins the start instant, so the example is exact rather than
/// // subject to the microseconds between reading the clock twice.
/// let start = Instant::now();
/// let mut bucket = TokenBucket::new_at(1000.0, 2000, start);
///
/// // The bucket starts full, so a burst succeeds immediately.
/// assert!(bucket.try_consume_at(2000, start));
/// assert!(!bucket.try_consume_at(1, start));
///
/// // After a second, another 1000 bytes have accrued.
/// assert!(bucket.try_consume_at(1000, start + Duration::from_secs(1)));
/// ```
#[derive(Debug, Clone)]
pub struct TokenBucket {
    /// Tokens added per second.
    rate: f64,
    /// The maximum number of tokens that can accumulate.
    capacity: u64,
    /// Currently available tokens, fractional so slow rates still accrue.
    available: f64,
    last_refill: Instant,
    consumed: u64,
    rejected: u64,
}

impl TokenBucket {
    /// Creates a bucket refilling at `rate` tokens per second, holding at most
    /// `capacity`.
    ///
    /// The bucket starts full so a caller is not rate limited before it has
    /// sent anything. A non-positive rate is raised to a very small positive
    /// value, since a zero rate would block forever rather than limit.
    #[must_use]
    pub fn new(rate: f64, capacity: u64) -> Self {
        Self::new_at(rate, capacity, Instant::now())
    }

    /// Creates a bucket with an explicit starting instant.
    #[must_use]
    pub fn new_at(rate: f64, capacity: u64, now: Instant) -> Self {
        let capacity = capacity.max(1);

        Self {
            rate: if rate > 0.0 { rate } else { f64::MIN_POSITIVE },
            capacity,
            available: capacity as f64,
            last_refill: now,
            consumed: 0,
            rejected: 0,
        }
    }

    /// Returns the refill rate in tokens per second.
    #[must_use]
    pub const fn rate(&self) -> f64 {
        self.rate
    }

    /// Returns the burst capacity.
    #[must_use]
    pub const fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Returns the tokens consumed so far.
    #[must_use]
    pub const fn consumed(&self) -> u64 {
        self.consumed
    }

    /// Returns how many requests were refused.
    #[must_use]
    pub const fn rejected(&self) -> u64 {
        self.rejected
    }

    /// Changes the refill rate, keeping tokens already accrued.
    ///
    /// Used by adaptive senders that revise their estimate of available
    /// bandwidth.
    pub fn set_rate(&mut self, rate: f64) {
        self.rate = if rate > 0.0 { rate } else { f64::MIN_POSITIVE };
    }

    /// Returns the tokens currently available.
    #[must_use]
    pub fn available(&mut self) -> u64 {
        self.available_at(Instant::now())
    }

    /// Returns the tokens available at `now`.
    #[must_use]
    pub fn available_at(&mut self, now: Instant) -> u64 {
        self.refill(now);
        self.available as u64
    }

    /// Attempts to consume `tokens`, returning `false` if too few are available.
    pub fn try_consume(&mut self, tokens: u64) -> bool {
        self.try_consume_at(tokens, Instant::now())
    }

    /// Attempts to consume `tokens` as of `now`.
    ///
    /// A request larger than the bucket's whole capacity can never succeed, so
    /// it is refused immediately rather than waited on forever.
    pub fn try_consume_at(&mut self, tokens: u64, now: Instant) -> bool {
        self.refill(now);

        if tokens as f64 <= self.available {
            self.available -= tokens as f64;
            self.consumed += tokens;
            true
        } else {
            self.rejected += 1;
            false
        }
    }

    /// Returns how long until `tokens` would be available.
    ///
    /// Returns [`Duration::ZERO`] when they already are, and `None` when the
    /// request exceeds the bucket's capacity and therefore never will be.
    #[must_use]
    pub fn time_until(&mut self, tokens: u64) -> Option<Duration> {
        self.time_until_at(tokens, Instant::now())
    }

    /// Returns how long after `now` until `tokens` would be available.
    #[must_use]
    pub fn time_until_at(&mut self, tokens: u64, now: Instant) -> Option<Duration> {
        if tokens > self.capacity {
            return None;
        }

        self.refill(now);

        if tokens as f64 <= self.available {
            return Some(Duration::ZERO);
        }

        let shortfall = tokens as f64 - self.available;
        Some(Duration::from_secs_f64(shortfall / self.rate))
    }

    /// Adds tokens accrued since the last refill.
    fn refill(&mut self, now: Instant) {
        // A clock that appears to move backwards (which `Instant` forbids, but
        // arithmetic on injected values could produce) must not remove tokens.
        let elapsed = now.saturating_duration_since(self.last_refill);
        if elapsed.is_zero() {
            return;
        }

        self.available =
            (self.available + elapsed.as_secs_f64() * self.rate).min(self.capacity as f64);
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_new_bucket_starts_full() {
        let now = start();
        let mut bucket = TokenBucket::new_at(100.0, 500, now);
        assert_eq!(bucket.available_at(now), 500);
    }

    #[test]
    fn consuming_reduces_available_tokens() {
        let now = start();
        let mut bucket = TokenBucket::new_at(100.0, 500, now);

        assert!(bucket.try_consume_at(200, now));
        assert_eq!(bucket.available_at(now), 300);
        assert_eq!(bucket.consumed(), 200);
    }

    #[test]
    fn exhausting_the_bucket_refuses_further_requests() {
        let now = start();
        let mut bucket = TokenBucket::new_at(100.0, 500, now);

        assert!(bucket.try_consume_at(500, now));
        assert!(!bucket.try_consume_at(1, now));
        assert_eq!(bucket.rejected(), 1);
    }

    #[test]
    fn tokens_accrue_over_time() {
        let now = start();
        let mut bucket = TokenBucket::new_at(100.0, 1000, now);

        assert!(bucket.try_consume_at(1000, now));
        assert_eq!(bucket.available_at(now), 0);

        // 100 tokens per second, so half a second yields 50.
        assert_eq!(bucket.available_at(now + Duration::from_millis(500)), 50);
        assert_eq!(bucket.available_at(now + Duration::from_secs(2)), 200);
    }

    #[test]
    fn accrual_is_capped_at_capacity() {
        let now = start();
        let mut bucket = TokenBucket::new_at(100.0, 300, now);

        assert!(bucket.try_consume_at(300, now));
        // An hour of accrual still cannot exceed the burst ceiling.
        assert_eq!(bucket.available_at(now + Duration::from_secs(3600)), 300);
    }

    #[test]
    fn the_average_rate_is_enforced_over_time() {
        let now = start();
        // 1000 tokens per second, minimal burst.
        let mut bucket = TokenBucket::new_at(1000.0, 1000, now);

        let mut granted = 0_u64;
        // Ask for 100 tokens every 10ms for one simulated second.
        for step in 0..100_u32 {
            let at = now + Duration::from_millis(u64::from(step) * 10);
            if bucket.try_consume_at(100, at) {
                granted += 100;
            }
        }

        // One second at 1000/s, plus the initial full bucket of 1000.
        assert!(
            (1000..=2000).contains(&granted),
            "granted {granted} tokens, expected the rate to bound it"
        );
    }

    #[test]
    fn time_until_reports_the_wait() {
        let now = start();
        let mut bucket = TokenBucket::new_at(100.0, 500, now);
        assert!(bucket.try_consume_at(500, now));

        // 250 tokens at 100/s is 2.5 seconds.
        let wait = bucket.time_until_at(250, now).expect("within capacity");
        assert!(
            (wait.as_secs_f64() - 2.5).abs() < 0.01,
            "expected ~2.5s, got {wait:?}"
        );
    }

    #[test]
    fn time_until_is_zero_when_tokens_are_ready() {
        let now = start();
        let mut bucket = TokenBucket::new_at(100.0, 500, now);
        assert_eq!(bucket.time_until_at(100, now), Some(Duration::ZERO));
    }

    #[test]
    fn a_request_larger_than_capacity_is_impossible() {
        let now = start();
        let mut bucket = TokenBucket::new_at(100.0, 500, now);

        assert_eq!(
            bucket.time_until_at(501, now),
            None,
            "a request beyond capacity can never be satisfied"
        );
        assert!(!bucket.try_consume_at(501, now));
    }

    #[test]
    fn the_rate_can_be_revised() {
        let now = start();
        let mut bucket = TokenBucket::new_at(100.0, 1000, now);
        assert!(bucket.try_consume_at(1000, now));

        bucket.set_rate(500.0);
        assert_eq!(bucket.available_at(now + Duration::from_secs(1)), 500);
    }

    #[test]
    fn a_zero_rate_is_corrected() {
        let now = start();
        let bucket = TokenBucket::new_at(0.0, 100, now);
        assert!(bucket.rate() > 0.0, "a zero rate would block forever");
    }

    #[test]
    fn a_backwards_clock_does_not_remove_tokens() {
        let now = start() + Duration::from_secs(10);
        let mut bucket = TokenBucket::new_at(100.0, 500, now);
        assert!(bucket.try_consume_at(400, now));

        let before = bucket.available_at(now);
        let earlier = bucket.available_at(now - Duration::from_secs(5));
        assert_eq!(earlier, before, "time moving backwards must be inert");
    }
}
