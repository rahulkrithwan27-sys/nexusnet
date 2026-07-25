//! Reconnection policy: exponential backoff with jitter.
//!
//! A client that retries a downed server on a fixed interval creates a
//! thundering herd — every client wakes at the same moment and hammers the
//! service just as it is trying to recover. Exponential backoff spreads retries
//! out over time; jitter spreads them out across clients. Both matter, and
//! jitter is the one people forget.

use std::time::Duration;

use tokio::net::ToSocketAddrs;

use crate::config::{Error, Result, TransportConfig};
use crate::tcp::{self, TcpConnection};

/// The default delay before the first retry.
pub const DEFAULT_INITIAL_DELAY: Duration = Duration::from_millis(100);

/// The default ceiling on retry delay.
pub const DEFAULT_MAX_DELAY: Duration = Duration::from_secs(30);

/// The default growth factor applied to each successive delay.
pub const DEFAULT_MULTIPLIER: f64 = 2.0;

/// How long to wait between reconnection attempts.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct ReconnectPolicy {
    /// The delay before the first retry.
    pub initial_delay: Duration,
    /// The maximum delay between retries.
    pub max_delay: Duration,
    /// The factor each delay is multiplied by.
    pub multiplier: f64,
    /// The maximum number of attempts, or `None` to retry indefinitely.
    ///
    /// This counts *total* attempts, so `Some(1)` means "try once, never
    /// retry".
    pub max_attempts: Option<u32>,
    /// Whether to randomize each delay.
    pub jitter: bool,
}

impl ReconnectPolicy {
    /// Creates a policy with default values.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            initial_delay: DEFAULT_INITIAL_DELAY,
            max_delay: DEFAULT_MAX_DELAY,
            multiplier: DEFAULT_MULTIPLIER,
            max_attempts: Some(5),
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

    /// Sets the initial delay.
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
    /// Values below 1.0 are raised to 1.0; a shrinking backoff would retry ever
    /// more aggressively against a struggling server, which is the opposite of
    /// the intent.
    #[must_use]
    pub fn with_multiplier(mut self, multiplier: f64) -> Self {
        self.multiplier = if multiplier < 1.0 { 1.0 } else { multiplier };
        self
    }

    /// Sets the maximum number of attempts, or `None` for unlimited.
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

    /// Returns `true` if another attempt is permitted after `attempts` tries.
    #[must_use]
    pub const fn should_retry(&self, attempts: u32) -> bool {
        match self.max_attempts {
            Some(max) => attempts < max,
            None => true,
        }
    }

    /// Returns the delay before the retry following `attempt`.
    ///
    /// `attempt` is zero-based: `0` is the wait after the first failure. The
    /// delay grows geometrically and is capped at
    /// [`max_delay`](Self::max_delay). With jitter enabled the result is
    /// uniformly distributed in `[delay / 2, delay]`, which is the "equal
    /// jitter" strategy: still backing off, but no longer synchronized across
    /// clients.
    #[must_use]
    pub fn delay_for(&self, attempt: u32) -> Duration {
        let base = self.initial_delay.as_secs_f64() * self.multiplier.powi(attempt as i32);
        let capped = base.min(self.max_delay.as_secs_f64()).max(0.0);

        let seconds = if self.jitter {
            let half = capped / 2.0;
            half + half * random_unit()
        } else {
            capped
        };

        Duration::from_secs_f64(seconds)
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns a pseudo-random value in `[0, 1)`.
///
/// Jitter needs to decorrelate clients, not to be cryptographically random, so
/// this uses a cheap xorshift seeded from the clock rather than pulling in a
/// random-number dependency.
fn random_unit() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as u64)
        .wrapping_add(1);

    let mut x = nanos ^ 0x2545_F491_4F6C_DD1D;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;

    // Use the top 53 bits, the mantissa width of an f64.
    ((x >> 11) as f64) / ((1_u64 << 53) as f64)
}

/// Connects to `address`, retrying according to `policy`.
///
/// Only connection *establishment* is retried. Once a connection is returned it
/// is an ordinary [`TcpConnection`]; a mid-session failure is surfaced to the
/// caller rather than silently reconnected, because a transparent reconnect
/// would lose stream state the caller may care about.
///
/// # Errors
///
/// Returns the last error encountered once the policy stops permitting
/// retries — typically [`Error::Io`] or [`Error::ConnectTimeout`].
pub async fn connect_with_retry<A>(
    address: A,
    config: TransportConfig,
    policy: ReconnectPolicy,
) -> Result<TcpConnection>
where
    A: ToSocketAddrs + Clone + std::fmt::Debug,
{
    let mut attempts = 0_u32;

    loop {
        match tcp::connect(address.clone(), config).await {
            Ok(connection) => return Ok(connection),
            Err(error) => {
                attempts += 1;

                if !policy.should_retry(attempts) {
                    return Err(error);
                }

                let delay = policy.delay_for(attempts - 1);
                tracing::debug!(
                    attempt = attempts,
                    delay_ms = delay.as_millis() as u64,
                    "connection attempt failed, retrying"
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Reports whether an error is worth retrying.
///
/// Protocol errors are not: they indicate the peer is speaking something this
/// build cannot parse, and retrying will reproduce the same result.
#[must_use]
pub const fn is_retryable(error: &Error) -> bool {
    matches!(error, Error::Io(_) | Error::ConnectTimeout { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_geometrically() {
        let policy = ReconnectPolicy::new()
            .with_jitter(false)
            .with_initial_delay(Duration::from_millis(100))
            .with_multiplier(2.0)
            .with_max_delay(Duration::from_secs(60));

        assert_eq!(policy.delay_for(0), Duration::from_millis(100));
        assert_eq!(policy.delay_for(1), Duration::from_millis(200));
        assert_eq!(policy.delay_for(2), Duration::from_millis(400));
        assert_eq!(policy.delay_for(3), Duration::from_millis(800));
    }

    #[test]
    fn backoff_is_capped() {
        let policy = ReconnectPolicy::new()
            .with_jitter(false)
            .with_initial_delay(Duration::from_millis(100))
            .with_max_delay(Duration::from_secs(1));

        // Would be ~102 seconds uncapped.
        assert_eq!(policy.delay_for(10), Duration::from_secs(1));
        assert_eq!(policy.delay_for(100), Duration::from_secs(1));
    }

    #[test]
    fn jitter_stays_within_half_the_delay() {
        let policy = ReconnectPolicy::new()
            .with_jitter(true)
            .with_initial_delay(Duration::from_millis(1000))
            .with_max_delay(Duration::from_secs(60));

        for _ in 0..200 {
            let delay = policy.delay_for(0);
            assert!(
                delay >= Duration::from_millis(500) && delay <= Duration::from_millis(1000),
                "jittered delay {delay:?} outside [500ms, 1000ms]"
            );
        }
    }

    #[test]
    fn jitter_actually_varies() {
        let policy = ReconnectPolicy::new()
            .with_jitter(true)
            .with_initial_delay(Duration::from_secs(10));

        let mut seen = std::collections::HashSet::new();
        for _ in 0..50 {
            seen.insert(policy.delay_for(0).as_nanos());
            std::thread::yield_now();
        }

        assert!(
            seen.len() > 1,
            "jitter should produce varying delays, got {seen:?}"
        );
    }

    #[test]
    fn attempt_limits_are_respected() {
        let limited = ReconnectPolicy::new().with_max_attempts(Some(3));
        assert!(limited.should_retry(1));
        assert!(limited.should_retry(2));
        assert!(!limited.should_retry(3));

        let unlimited = ReconnectPolicy::new().with_max_attempts(None);
        assert!(unlimited.should_retry(1_000_000));

        assert!(!ReconnectPolicy::never().should_retry(1));
    }

    #[test]
    fn shrinking_multipliers_are_corrected() {
        let policy = ReconnectPolicy::new().with_multiplier(0.5);
        assert!((policy.multiplier - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn protocol_errors_are_not_retried() {
        assert!(!is_retryable(&Error::Protocol(
            nexusnet_protocol::Error::NoCommonVersion
        )));
        assert!(is_retryable(&Error::ConnectTimeout {
            address: "example:1".to_owned(),
            timeout: Duration::from_secs(1),
        }));
    }

    #[test]
    fn random_unit_stays_in_range() {
        for _ in 0..1000 {
            let value = random_unit();
            assert!((0.0..1.0).contains(&value), "{value} out of range");
        }
    }
}
