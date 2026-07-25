//! Retransmission scheduling.
//!
//! A retry manager answers two questions: *should* this be retried, and *when*.
//! Getting the second wrong is what turns a brief outage into an outage that
//! stays: clients that all retry on the same fixed interval synchronize and
//! hammer the service exactly as it tries to recover.
//!
//! [`RetryPolicy`] therefore applies exponential backoff with jitter, and
//! [`RetryManager`] holds pending retries in a due-time-ordered heap so
//! releasing them costs `O(log n)` rather than a scan.

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::time::{Duration, Instant};

/// The default delay before a first retry.
pub const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(200);

/// The default ceiling on retry delay.
pub const DEFAULT_MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// The default number of attempts, counting the original send.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 4;

/// How long to wait between attempts, and how many to make.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct RetryPolicy {
    /// The delay before the first retry.
    pub initial_delay: Duration,
    /// The ceiling on any single delay.
    pub max_delay: Duration,
    /// The factor each successive delay is multiplied by.
    pub multiplier: f64,
    /// The total attempts permitted, including the original send.
    ///
    /// `None` retries indefinitely, which is appropriate only when some other
    /// mechanism bounds the work.
    pub max_attempts: Option<u32>,
    /// Whether to randomize delays so clients do not retry in lockstep.
    pub jitter: bool,
}

impl RetryPolicy {
    /// Creates a policy with default values.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            initial_delay: DEFAULT_RETRY_DELAY,
            max_delay: DEFAULT_MAX_RETRY_DELAY,
            multiplier: 2.0,
            max_attempts: Some(DEFAULT_MAX_ATTEMPTS),
            jitter: true,
        }
    }

    /// A policy that never retries.
    #[must_use]
    pub const fn never() -> Self {
        Self {
            max_attempts: Some(1),
            ..Self::new()
        }
    }

    /// Sets the first retry delay.
    #[must_use]
    pub const fn with_initial_delay(mut self, initial_delay: Duration) -> Self {
        self.initial_delay = initial_delay;
        self
    }

    /// Sets the maximum delay.
    #[must_use]
    pub const fn with_max_delay(mut self, max_delay: Duration) -> Self {
        self.max_delay = max_delay;
        self
    }

    /// Sets the backoff multiplier.
    ///
    /// Values below 1.0 are raised to 1.0: a shrinking backoff would retry ever
    /// faster against a struggling peer, which is precisely backwards.
    #[must_use]
    pub fn with_multiplier(mut self, multiplier: f64) -> Self {
        self.multiplier = if multiplier < 1.0 { 1.0 } else { multiplier };
        self
    }

    /// Sets the attempt limit, or `None` for unlimited.
    #[must_use]
    pub const fn with_max_attempts(mut self, max_attempts: Option<u32>) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Sets whether delays are randomized.
    #[must_use]
    pub const fn with_jitter(mut self, jitter: bool) -> Self {
        self.jitter = jitter;
        self
    }

    /// Returns `true` if a packet that has been sent `attempts` times may be
    /// retried again.
    #[must_use]
    pub const fn permits(&self, attempts: u32) -> bool {
        match self.max_attempts {
            Some(max) => attempts < max,
            None => true,
        }
    }

    /// Returns the delay before the retry following `attempts` sends.
    ///
    /// `attempts` is the number already made, so `1` yields the first retry
    /// delay. With jitter the result is uniform in `[delay / 2, delay]` —
    /// still backing off, but decorrelated across clients.
    #[must_use]
    pub fn delay_after(&self, attempts: u32) -> Duration {
        let exponent = attempts.saturating_sub(1);
        let base = self.initial_delay.as_secs_f64() * self.multiplier.powi(exponent as i32);
        let capped = base.min(self.max_delay.as_secs_f64()).max(0.0);

        let seconds = if self.jitter {
            let half = capped / 2.0;
            half + half * jitter_unit()
        } else {
            capped
        };

        Duration::from_secs_f64(seconds)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns a pseudo-random value in `[0, 1)`.
///
/// Jitter exists to decorrelate clients, not to resist an adversary, so this is
/// a cheap clock-seeded xorshift rather than a random-number dependency.
fn jitter_unit() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| u64::from(elapsed.subsec_nanos()))
        .wrapping_add(1);

    let mut state = nanos ^ 0x2545_F491_4F6C_DD1D;
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;

    // The top 53 bits are the mantissa width of an f64.
    ((state >> 11) as f64) / ((1_u64 << 53) as f64)
}

/// What the retry manager decided about a failed send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetryDecision {
    /// The item was scheduled for another attempt after the given delay.
    Scheduled {
        /// How long until the item becomes due.
        delay: Duration,
        /// How many attempts have now been made.
        attempts: u32,
    },
    /// The item exhausted its budget and was abandoned.
    Exhausted {
        /// How many attempts were made in total.
        attempts: u32,
    },
}

impl RetryDecision {
    /// Returns `true` if another attempt will be made.
    #[must_use]
    pub const fn will_retry(&self) -> bool {
        matches!(self, Self::Scheduled { .. })
    }

    /// Returns the delay before the next attempt, if there is one.
    #[must_use]
    pub const fn delay(&self) -> Option<Duration> {
        match self {
            Self::Scheduled { delay, .. } => Some(*delay),
            Self::Exhausted { .. } => None,
        }
    }
}

/// An item waiting for its retry delay to elapse.
#[derive(Debug)]
struct Pending<T> {
    due_at: Instant,
    /// Distinguishes items scheduled for the same instant so ordering is total
    /// and releases stay first-scheduled-first.
    sequence: u64,
    item: T,
}

impl<T> PartialEq for Pending<T> {
    fn eq(&self, other: &Self) -> bool {
        self.due_at == other.due_at && self.sequence == other.sequence
    }
}

impl<T> Eq for Pending<T> {}

impl<T> PartialOrd for Pending<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Pending<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.due_at
            .cmp(&other.due_at)
            .then_with(|| self.sequence.cmp(&other.sequence))
    }
}

/// Schedules and releases retransmissions.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, Instant};
/// use nexusnet_scheduler::{RetryManager, RetryPolicy};
///
/// let now = Instant::now();
/// let policy = RetryPolicy::new()
///     .with_jitter(false)
///     .with_initial_delay(Duration::from_millis(100));
/// let mut retries: RetryManager<&str> = RetryManager::new(policy);
///
/// // A first failure schedules a retry rather than giving up.
/// let decision = retries.record_failure("packet", 1, now);
/// assert!(decision.will_retry());
///
/// // It is not due immediately, but is after the delay.
/// assert!(retries.take_due(now).is_empty());
/// assert_eq!(retries.take_due(now + Duration::from_millis(150)), vec!["packet"]);
/// ```
#[derive(Debug)]
pub struct RetryManager<T> {
    policy: RetryPolicy,
    pending: BinaryHeap<Reverse<Pending<T>>>,
    sequence: u64,
    scheduled: u64,
    released: u64,
    exhausted: u64,
}

impl<T> RetryManager<T> {
    /// Creates a manager applying `policy`.
    #[must_use]
    pub fn new(policy: RetryPolicy) -> Self {
        Self {
            policy,
            pending: BinaryHeap::new(),
            sequence: 0,
            scheduled: 0,
            released: 0,
            exhausted: 0,
        }
    }

    /// Returns the policy in force.
    #[must_use]
    pub const fn policy(&self) -> &RetryPolicy {
        &self.policy
    }

    /// Replaces the policy.
    ///
    /// Items already scheduled keep the due time they were given; only future
    /// decisions use the new policy.
    pub fn set_policy(&mut self, policy: RetryPolicy) {
        self.policy = policy;
    }

    /// Returns how many items are waiting for their delay to elapse.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Returns `true` when nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Returns how many retries have been scheduled in total.
    #[must_use]
    pub const fn scheduled(&self) -> u64 {
        self.scheduled
    }

    /// Returns how many retries have become due and been released.
    #[must_use]
    pub const fn released(&self) -> u64 {
        self.released
    }

    /// Returns how many items were abandoned after exhausting their budget.
    #[must_use]
    pub const fn exhausted(&self) -> u64 {
        self.exhausted
    }

    /// Returns when the earliest pending retry becomes due.
    ///
    /// A caller with nothing else to do can sleep until this instant rather
    /// than polling.
    #[must_use]
    pub fn next_due(&self) -> Option<Instant> {
        self.pending.peek().map(|Reverse(entry)| entry.due_at)
    }

    /// Returns how long until the earliest pending retry is due, as of `now`.
    ///
    /// Returns [`Duration::ZERO`] when something is already due.
    #[must_use]
    pub fn time_until_due(&self, now: Instant) -> Option<Duration> {
        self.next_due()
            .map(|due| due.saturating_duration_since(now))
    }

    /// Records a failed send of `item`, which has now been attempted
    /// `attempts` times.
    ///
    /// Returns [`RetryDecision::Scheduled`] if another attempt is permitted, in
    /// which case the item is retained until due. Otherwise the item is dropped
    /// and [`RetryDecision::Exhausted`] is returned.
    pub fn record_failure(&mut self, item: T, attempts: u32, now: Instant) -> RetryDecision {
        if !self.policy.permits(attempts) {
            self.exhausted += 1;
            return RetryDecision::Exhausted { attempts };
        }

        let delay = self.policy.delay_after(attempts);
        self.sequence += 1;
        self.scheduled += 1;

        self.pending.push(Reverse(Pending {
            due_at: now + delay,
            sequence: self.sequence,
            item,
        }));

        RetryDecision::Scheduled { delay, attempts }
    }

    /// Removes and returns every item due at or before `now`.
    ///
    /// Items come back in due-time order, and ties break by scheduling order so
    /// a burst of failures retries in the sequence it failed.
    pub fn take_due(&mut self, now: Instant) -> Vec<T> {
        let mut due = Vec::new();

        while let Some(Reverse(entry)) = self.pending.peek() {
            if entry.due_at > now {
                break;
            }

            let Some(Reverse(entry)) = self.pending.pop() else {
                break;
            };
            due.push(entry.item);
        }

        self.released += due.len() as u64;

        due
    }

    /// Discards every pending retry, returning the abandoned items.
    pub fn drain(&mut self) -> Vec<T> {
        let drained: Vec<T> = self
            .pending
            .drain()
            .map(|Reverse(entry)| entry.item)
            .collect();

        drained
    }
}

impl<T> Default for RetryManager<T> {
    fn default() -> Self {
        Self::new(RetryPolicy::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_policy() -> RetryPolicy {
        RetryPolicy::new()
            .with_jitter(false)
            .with_initial_delay(Duration::from_millis(100))
            .with_multiplier(2.0)
            .with_max_attempts(Some(4))
    }

    #[test]
    fn backoff_grows_geometrically() {
        let policy = fixed_policy();

        assert_eq!(policy.delay_after(1), Duration::from_millis(100));
        assert_eq!(policy.delay_after(2), Duration::from_millis(200));
        assert_eq!(policy.delay_after(3), Duration::from_millis(400));
    }

    #[test]
    fn backoff_is_capped() {
        let policy = fixed_policy().with_max_delay(Duration::from_millis(250));

        assert_eq!(policy.delay_after(5), Duration::from_millis(250));
        assert_eq!(policy.delay_after(50), Duration::from_millis(250));
    }

    #[test]
    fn jitter_stays_within_half_the_delay() {
        let policy = RetryPolicy::new()
            .with_jitter(true)
            .with_initial_delay(Duration::from_millis(1000))
            .with_max_delay(Duration::from_secs(60));

        for _ in 0..200 {
            let delay = policy.delay_after(1);
            assert!(
                delay >= Duration::from_millis(500) && delay <= Duration::from_millis(1000),
                "jittered delay {delay:?} outside [500ms, 1000ms]"
            );
        }
    }

    #[test]
    fn a_shrinking_multiplier_is_corrected() {
        let policy = RetryPolicy::new().with_multiplier(0.25);
        assert!((policy.multiplier - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn attempt_limits_are_respected() {
        let policy = fixed_policy();
        assert!(policy.permits(1));
        assert!(policy.permits(3));
        assert!(!policy.permits(4), "the fourth attempt exhausts the budget");

        assert!(!RetryPolicy::never().permits(1));
        assert!(RetryPolicy::new()
            .with_max_attempts(None)
            .permits(1_000_000));
    }

    #[test]
    fn a_failure_schedules_a_retry() {
        let now = Instant::now();
        let mut retries: RetryManager<&str> = RetryManager::new(fixed_policy());

        let decision = retries.record_failure("a", 1, now);
        assert!(decision.will_retry());
        assert_eq!(decision.delay(), Some(Duration::from_millis(100)));
        assert_eq!(retries.pending(), 1);
        assert_eq!(retries.scheduled(), 1);
    }

    #[test]
    fn an_item_is_not_due_before_its_delay() {
        let now = Instant::now();
        let mut retries: RetryManager<&str> = RetryManager::new(fixed_policy());
        retries.record_failure("a", 1, now);

        assert!(retries.take_due(now).is_empty());
        assert!(retries.take_due(now + Duration::from_millis(99)).is_empty());
        assert_eq!(
            retries.take_due(now + Duration::from_millis(100)),
            vec!["a"]
        );
        assert!(retries.is_empty());
    }

    #[test]
    fn exhausting_the_budget_abandons_the_item() {
        let now = Instant::now();
        let mut retries: RetryManager<&str> = RetryManager::new(fixed_policy());

        let decision = retries.record_failure("a", 4, now);
        assert!(!decision.will_retry());
        assert_eq!(decision, RetryDecision::Exhausted { attempts: 4 });
        assert!(retries.is_empty(), "an abandoned item must not be retained");
        assert_eq!(retries.exhausted(), 1);
    }

    #[test]
    fn items_are_released_in_due_order() {
        let now = Instant::now();
        let mut retries: RetryManager<&str> = RetryManager::new(fixed_policy());

        // Later attempts have longer delays, so scheduling order differs from
        // due order.
        retries.record_failure("third", 3, now); // 400ms
        retries.record_failure("first", 1, now); // 100ms
        retries.record_failure("second", 2, now); // 200ms

        let due = retries.take_due(now + Duration::from_secs(1));
        assert_eq!(due, vec!["first", "second", "third"]);
    }

    #[test]
    fn ties_break_by_scheduling_order() {
        let now = Instant::now();
        let mut retries: RetryManager<u32> = RetryManager::new(fixed_policy());

        // Identical attempt counts give identical delays.
        for id in 0..5 {
            retries.record_failure(id, 1, now);
        }

        let due = retries.take_due(now + Duration::from_millis(200));
        assert_eq!(due, vec![0, 1, 2, 3, 4], "a burst must retry in order");
    }

    #[test]
    fn only_due_items_are_released() {
        let now = Instant::now();
        let mut retries: RetryManager<&str> = RetryManager::new(fixed_policy());

        retries.record_failure("soon", 1, now); // 100ms
        retries.record_failure("later", 3, now); // 400ms

        assert_eq!(
            retries.take_due(now + Duration::from_millis(150)),
            vec!["soon"]
        );
        assert_eq!(retries.pending(), 1, "the later item stays queued");
        assert_eq!(retries.released(), 1);
    }

    #[test]
    fn the_next_due_instant_is_reported() {
        let now = Instant::now();
        let mut retries: RetryManager<&str> = RetryManager::new(fixed_policy());
        assert!(retries.next_due().is_none());

        retries.record_failure("later", 3, now);
        retries.record_failure("soon", 1, now);

        // The earliest deadline wins, regardless of insertion order.
        assert_eq!(retries.next_due(), Some(now + Duration::from_millis(100)));
        assert_eq!(
            retries.time_until_due(now),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            retries.time_until_due(now + Duration::from_secs(1)),
            Some(Duration::ZERO),
            "an overdue item reports no remaining wait"
        );
    }

    #[test]
    fn draining_abandons_everything_pending() {
        let now = Instant::now();
        let mut retries: RetryManager<u32> = RetryManager::new(fixed_policy());
        for id in 0..3 {
            retries.record_failure(id, 1, now);
        }

        let mut drained = retries.drain();
        drained.sort_unstable();

        assert_eq!(drained, vec![0, 1, 2]);
        assert!(retries.is_empty());
    }

    #[test]
    fn the_policy_can_be_replaced() {
        let mut retries: RetryManager<&str> = RetryManager::new(fixed_policy());
        retries.set_policy(RetryPolicy::never());

        assert!(!retries.record_failure("a", 1, Instant::now()).will_retry());
    }
}
