//! Endpoint health tracking.
//!
//! Removing a failing endpoint is easy. Deciding when to put it *back* is the
//! part that goes wrong, and it goes wrong in two directions: return it too
//! eagerly and traffic keeps hitting a broken server; never return it and a
//! transient blip permanently shrinks the pool.
//!
//! [`HealthTracker`] uses the circuit-breaker pattern. An endpoint that fails
//! repeatedly opens its circuit and is withdrawn. After a cooldown it moves to
//! half-open, where a *single* probe decides: success closes the circuit,
//! failure reopens it for another cooldown. One request is risked, not all of
//! them.

use std::time::{Duration, Instant};

/// The default consecutive failures before an endpoint is withdrawn.
pub const DEFAULT_FAILURE_THRESHOLD: u32 = 3;

/// The default successes required in half-open state before full recovery.
pub const DEFAULT_SUCCESS_THRESHOLD: u32 = 2;

/// The default cooldown before a withdrawn endpoint is probed again.
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);

/// Whether an endpoint is currently usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Health {
    /// Accepting traffic normally.
    Healthy,
    /// Withdrawn after repeated failures.
    Unhealthy,
    /// Withdrawn, but due for a probe.
    ///
    /// One request is allowed through to test recovery. Everything else keeps
    /// avoiding this endpoint until that probe resolves.
    Recovering,
}

impl Health {
    /// Returns `true` if the endpoint may serve ordinary traffic.
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Healthy)
    }

    /// Returns `true` if the endpoint is withdrawn.
    #[must_use]
    pub const fn is_withdrawn(self) -> bool {
        matches!(self, Self::Unhealthy | Self::Recovering)
    }
}

impl std::fmt::Display for Health {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
            Self::Recovering => "recovering",
        };
        f.write_str(name)
    }
}

/// How aggressively to withdraw and restore endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HealthPolicy {
    /// Consecutive failures that withdraw an endpoint.
    pub failure_threshold: u32,
    /// Consecutive successes in half-open state that restore it.
    pub success_threshold: u32,
    /// How long to wait before probing a withdrawn endpoint.
    pub cooldown: Duration,
}

impl HealthPolicy {
    /// Creates a policy with default values.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            failure_threshold: DEFAULT_FAILURE_THRESHOLD,
            success_threshold: DEFAULT_SUCCESS_THRESHOLD,
            cooldown: DEFAULT_COOLDOWN,
        }
    }

    /// Sets the failure threshold.
    ///
    /// Zero is raised to one; withdrawing on zero failures would remove every
    /// endpoint immediately.
    #[must_use]
    pub const fn with_failure_threshold(mut self, failure_threshold: u32) -> Self {
        self.failure_threshold = if failure_threshold == 0 {
            1
        } else {
            failure_threshold
        };
        self
    }

    /// Sets the success threshold for recovery.
    #[must_use]
    pub const fn with_success_threshold(mut self, success_threshold: u32) -> Self {
        self.success_threshold = if success_threshold == 0 {
            1
        } else {
            success_threshold
        };
        self
    }

    /// Sets the cooldown before probing.
    #[must_use]
    pub const fn with_cooldown(mut self, cooldown: Duration) -> Self {
        self.cooldown = cooldown;
        self
    }
}

impl Default for HealthPolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Tracks one endpoint's health.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, Instant};
/// use nexusnet_router::{Health, HealthPolicy, HealthTracker};
///
/// let now = Instant::now();
/// let policy = HealthPolicy::new()
///     .with_failure_threshold(2)
///     .with_cooldown(Duration::from_secs(5));
/// let mut tracker = HealthTracker::new(policy);
///
/// // Repeated failures withdraw the endpoint.
/// tracker.record_failure(now);
/// tracker.record_failure(now);
/// assert_eq!(tracker.health(now), Health::Unhealthy);
///
/// // After the cooldown it is probed rather than trusted outright.
/// let later = now + Duration::from_secs(6);
/// assert_eq!(tracker.health(later), Health::Recovering);
/// ```
#[derive(Debug, Clone)]
pub struct HealthTracker {
    policy: HealthPolicy,
    consecutive_failures: u32,
    consecutive_successes: u32,
    /// When the circuit opened, if it is open.
    opened_at: Option<Instant>,
    total_successes: u64,
    total_failures: u64,
    withdrawals: u64,
}

impl HealthTracker {
    /// Creates a tracker applying `policy`, starting healthy.
    #[must_use]
    pub const fn new(policy: HealthPolicy) -> Self {
        Self {
            policy,
            consecutive_failures: 0,
            consecutive_successes: 0,
            opened_at: None,
            total_successes: 0,
            total_failures: 0,
            withdrawals: 0,
        }
    }

    /// Returns the health as of `now`.
    ///
    /// A withdrawn endpoint becomes [`Health::Recovering`] once its cooldown
    /// has elapsed, without any explicit tick.
    #[must_use]
    pub fn health(&self, now: Instant) -> Health {
        match self.opened_at {
            None => Health::Healthy,
            Some(opened) => {
                if now.saturating_duration_since(opened) >= self.policy.cooldown {
                    Health::Recovering
                } else {
                    Health::Unhealthy
                }
            }
        }
    }

    /// Returns `true` if this endpoint may serve ordinary traffic.
    #[must_use]
    pub fn is_available(&self, now: Instant) -> bool {
        self.health(now).is_available()
    }

    /// Returns `true` if this endpoint should receive a recovery probe.
    #[must_use]
    pub fn accepts_probe(&self, now: Instant) -> bool {
        matches!(self.health(now), Health::Recovering)
    }

    /// Records a successful request.
    pub fn record_success(&mut self, now: Instant) {
        let _ = now;

        self.total_successes += 1;
        self.consecutive_failures = 0;

        if self.opened_at.is_some() {
            self.consecutive_successes += 1;

            // Require several consecutive successes before trusting it again;
            // one lucky request is not evidence of recovery.
            if self.consecutive_successes >= self.policy.success_threshold {
                self.opened_at = None;
                self.consecutive_successes = 0;
            }
        }
    }

    /// Records a failed request.
    pub fn record_failure(&mut self, now: Instant) {
        self.total_failures += 1;
        self.consecutive_successes = 0;
        self.consecutive_failures += 1;

        if self.opened_at.is_some() {
            // A failed probe restarts the cooldown rather than retrying at once.
            self.opened_at = Some(now);
            return;
        }

        if self.consecutive_failures >= self.policy.failure_threshold {
            self.opened_at = Some(now);
            self.withdrawals += 1;
        }
    }

    /// Returns the total successful requests observed.
    #[must_use]
    pub const fn total_successes(&self) -> u64 {
        self.total_successes
    }

    /// Returns the total failed requests observed.
    #[must_use]
    pub const fn total_failures(&self) -> u64 {
        self.total_failures
    }

    /// Returns how many times this endpoint has been withdrawn.
    #[must_use]
    pub const fn withdrawals(&self) -> u64 {
        self.withdrawals
    }

    /// Returns the fraction of requests that succeeded.
    ///
    /// Returns `1.0` before any request, so an untried endpoint is not
    /// penalized against tried ones.
    #[must_use]
    pub fn success_ratio(&self) -> f64 {
        let total = self.total_successes + self.total_failures;
        if total == 0 {
            1.0
        } else {
            self.total_successes as f64 / total as f64
        }
    }

    /// Restores the endpoint to full health, discarding failure history.
    pub fn reset(&mut self) {
        self.consecutive_failures = 0;
        self.consecutive_successes = 0;
        self.opened_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> HealthPolicy {
        HealthPolicy::new()
            .with_failure_threshold(3)
            .with_success_threshold(2)
            .with_cooldown(Duration::from_secs(30))
    }

    fn start() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_new_endpoint_is_healthy() {
        let now = start();
        let tracker = HealthTracker::new(policy());

        assert_eq!(tracker.health(now), Health::Healthy);
        assert!(tracker.is_available(now));
        assert!((tracker.success_ratio() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn isolated_failures_do_not_withdraw_an_endpoint() {
        let now = start();
        let mut tracker = HealthTracker::new(policy());

        // Failures interrupted by successes never reach the threshold.
        for _ in 0..10 {
            tracker.record_failure(now);
            tracker.record_failure(now);
            tracker.record_success(now);
        }

        assert!(
            tracker.is_available(now),
            "a flaky-but-working endpoint should stay in the pool"
        );
        assert_eq!(tracker.withdrawals(), 0);
    }

    #[test]
    fn consecutive_failures_withdraw_an_endpoint() {
        let now = start();
        let mut tracker = HealthTracker::new(policy());

        tracker.record_failure(now);
        tracker.record_failure(now);
        assert!(tracker.is_available(now), "still below the threshold");

        tracker.record_failure(now);
        assert_eq!(tracker.health(now), Health::Unhealthy);
        assert!(!tracker.is_available(now));
        assert_eq!(tracker.withdrawals(), 1);
    }

    #[test]
    fn a_withdrawn_endpoint_is_probed_after_its_cooldown() {
        let now = start();
        let mut tracker = HealthTracker::new(policy());

        for _ in 0..3 {
            tracker.record_failure(now);
        }

        assert_eq!(
            tracker.health(now + Duration::from_secs(10)),
            Health::Unhealthy
        );
        assert!(!tracker.accepts_probe(now + Duration::from_secs(10)));

        let after = now + Duration::from_secs(31);
        assert_eq!(tracker.health(after), Health::Recovering);
        assert!(tracker.accepts_probe(after));
        assert!(
            !tracker.is_available(after),
            "recovering endpoints take a probe, not ordinary traffic"
        );
    }

    #[test]
    fn a_failed_probe_restarts_the_cooldown() {
        let now = start();
        let mut tracker = HealthTracker::new(policy());

        for _ in 0..3 {
            tracker.record_failure(now);
        }

        let probe_time = now + Duration::from_secs(31);
        assert!(tracker.accepts_probe(probe_time));

        // The probe fails.
        tracker.record_failure(probe_time);

        assert_eq!(
            tracker.health(probe_time + Duration::from_secs(1)),
            Health::Unhealthy,
            "a failed probe must not be retried immediately"
        );
        assert_eq!(
            tracker.health(probe_time + Duration::from_secs(31)),
            Health::Recovering
        );
    }

    #[test]
    fn recovery_requires_several_successes() {
        let now = start();
        let mut tracker = HealthTracker::new(policy());

        for _ in 0..3 {
            tracker.record_failure(now);
        }

        let probe_time = now + Duration::from_secs(31);

        // One success is not enough to trust it again.
        tracker.record_success(probe_time);
        assert!(
            !tracker.is_available(probe_time),
            "one lucky request is not evidence of recovery"
        );

        tracker.record_success(probe_time);
        assert_eq!(tracker.health(probe_time), Health::Healthy);
        assert!(tracker.is_available(probe_time));
    }

    #[test]
    fn a_failure_during_recovery_reopens_the_circuit() {
        let now = start();
        let mut tracker = HealthTracker::new(policy());

        for _ in 0..3 {
            tracker.record_failure(now);
        }

        let probe_time = now + Duration::from_secs(31);
        tracker.record_success(probe_time);
        tracker.record_failure(probe_time);

        assert_eq!(tracker.health(probe_time), Health::Unhealthy);
    }

    #[test]
    fn success_resets_the_failure_streak() {
        let now = start();
        let mut tracker = HealthTracker::new(policy());

        tracker.record_failure(now);
        tracker.record_failure(now);
        tracker.record_success(now);
        tracker.record_failure(now);
        tracker.record_failure(now);

        assert!(
            tracker.is_available(now),
            "the streak restarted, so the threshold was never reached"
        );
    }

    #[test]
    fn statistics_are_tracked() {
        let now = start();
        let mut tracker = HealthTracker::new(policy());

        for _ in 0..8 {
            tracker.record_success(now);
        }
        for _ in 0..2 {
            tracker.record_failure(now);
        }

        assert_eq!(tracker.total_successes(), 8);
        assert_eq!(tracker.total_failures(), 2);
        assert!((tracker.success_ratio() - 0.8).abs() < 1e-9);
    }

    #[test]
    fn resetting_restores_health() {
        let now = start();
        let mut tracker = HealthTracker::new(policy());

        for _ in 0..3 {
            tracker.record_failure(now);
        }
        assert!(!tracker.is_available(now));

        tracker.reset();
        assert!(tracker.is_available(now));
        assert_eq!(
            tracker.total_failures(),
            3,
            "resetting health should not erase the historical record"
        );
    }

    #[test]
    fn degenerate_thresholds_are_corrected() {
        let corrected = HealthPolicy::new()
            .with_failure_threshold(0)
            .with_success_threshold(0);

        assert_eq!(corrected.failure_threshold, 1);
        assert_eq!(corrected.success_threshold, 1);
    }

    #[test]
    fn health_states_classify_correctly() {
        assert!(Health::Healthy.is_available());
        assert!(!Health::Healthy.is_withdrawn());

        assert!(!Health::Unhealthy.is_available());
        assert!(Health::Unhealthy.is_withdrawn());
        assert!(Health::Recovering.is_withdrawn());

        assert_eq!(Health::Recovering.to_string(), "recovering");
    }
}
