//! Scheduler metrics.
//!
//! Counters are kept on the scheduler itself and exposed as an immutable
//! [`SchedulerMetrics`] snapshot. Handing out a snapshot rather than a live
//! reference matters: a caller computing several derived figures — say the
//! shaping rate and the retry rate — sees one consistent moment rather than
//! numbers that shift between reads.

use std::time::Duration;

use crate::priority::Priority;

/// A point-in-time view of scheduler activity.
///
/// All counters are cumulative since the scheduler was created, except
/// [`pending`](Self::pending) and [`in_flight`](Self::in_flight), which are
/// instantaneous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SchedulerMetrics {
    /// Packets accepted into the scheduler.
    pub enqueued: u64,
    /// Packets released for sending.
    pub dispatched: u64,
    /// Packets refused because a priority class was full.
    pub rejected: u64,
    /// Packets dropped after exhausting their retry budget.
    pub dropped: u64,
    /// Payload bytes released for sending, excluding retransmissions.
    pub bytes_dispatched: u64,
    /// Payload bytes released as retransmissions.
    pub bytes_retransmitted: u64,
    /// Times a packet was held back by the rate limiter.
    pub shaped: u64,
    /// Total time packets spent waiting on the rate limiter.
    pub shaped_delay: Duration,
    /// Retransmissions scheduled.
    pub retries_scheduled: u64,
    /// Retransmissions actually re-queued once their delay elapsed.
    pub retries_dispatched: u64,
    /// Packets acknowledged by the peer.
    pub acknowledged: u64,
    /// Packets currently waiting in the queue.
    pub pending: usize,
    /// Packets sent but neither acknowledged nor abandoned.
    pub in_flight: usize,
    /// Packets waiting for a retry delay to elapse.
    pub awaiting_retry: usize,
    /// Packets currently queued in each priority class, lowest class first.
    pub pending_by_priority: [usize; 5],
}

impl SchedulerMetrics {
    /// Returns the total bytes released, including retransmissions.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.bytes_dispatched + self.bytes_retransmitted
    }

    /// Returns the fraction of dispatched bytes that were retransmissions.
    ///
    /// A useful health signal: a rising value means the link is losing packets
    /// or the retry timeout is too aggressive. Returns `0.0` before anything
    /// has been sent.
    #[must_use]
    pub fn retransmission_ratio(&self) -> f64 {
        let total = self.total_bytes();
        if total == 0 {
            0.0
        } else {
            self.bytes_retransmitted as f64 / total as f64
        }
    }

    /// Returns the fraction of enqueue attempts that were refused.
    ///
    /// Returns `0.0` before any attempt.
    #[must_use]
    pub fn rejection_ratio(&self) -> f64 {
        let attempts = self.enqueued + self.rejected;
        if attempts == 0 {
            0.0
        } else {
            self.rejected as f64 / attempts as f64
        }
    }

    /// Returns the mean delay imposed by the rate limiter per shaped packet.
    ///
    /// Returns [`Duration::ZERO`] when nothing has been shaped.
    #[must_use]
    pub fn mean_shaped_delay(&self) -> Duration {
        if self.shaped == 0 {
            Duration::ZERO
        } else {
            self.shaped_delay / u32::try_from(self.shaped).unwrap_or(u32::MAX)
        }
    }

    /// Returns how many packets are queued in `priority`.
    #[must_use]
    pub const fn pending_in(&self, priority: Priority) -> usize {
        self.pending_by_priority[priority.index()]
    }

    /// Returns `true` when nothing is queued, in flight, or awaiting retry.
    ///
    /// Useful as a drain condition during shutdown.
    #[must_use]
    pub const fn is_idle(&self) -> bool {
        self.pending == 0 && self.in_flight == 0 && self.awaiting_retry == 0
    }
}

/// Mutable counters backing a [`SchedulerMetrics`] snapshot.
///
/// Kept separate from the snapshot so the public type stays a plain value with
/// no mutating methods a caller could reach for by accident.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub(crate) enqueued: u64,
    pub(crate) dispatched: u64,
    pub(crate) rejected: u64,
    pub(crate) dropped: u64,
    pub(crate) bytes_dispatched: u64,
    pub(crate) bytes_retransmitted: u64,
    pub(crate) shaped: u64,
    pub(crate) shaped_delay: Duration,
    pub(crate) retries_scheduled: u64,
    pub(crate) retries_dispatched: u64,
    pub(crate) acknowledged: u64,
}

impl Counters {
    /// Records that a packet was released, distinguishing first sends from
    /// retransmissions so the retransmission ratio stays meaningful.
    pub(crate) fn record_dispatch(&mut self, len: usize, is_retransmission: bool) {
        self.dispatched += 1;

        if is_retransmission {
            self.bytes_retransmitted += len as u64;
        } else {
            self.bytes_dispatched += len as u64;
        }
    }

    /// Records that the rate limiter held a packet back for `delay`.
    pub(crate) fn record_shaping(&mut self, delay: Duration) {
        self.shaped += 1;
        self.shaped_delay = self.shaped_delay.saturating_add(delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_snapshot_is_idle() {
        let metrics = SchedulerMetrics::default();
        assert!(metrics.is_idle());
        assert!((metrics.retransmission_ratio() - 0.0).abs() < f64::EPSILON);
        assert!((metrics.rejection_ratio() - 0.0).abs() < f64::EPSILON);
        assert_eq!(metrics.mean_shaped_delay(), Duration::ZERO);
    }

    #[test]
    fn totals_include_retransmissions() {
        let metrics = SchedulerMetrics {
            bytes_dispatched: 750,
            bytes_retransmitted: 250,
            ..SchedulerMetrics::default()
        };

        assert_eq!(metrics.total_bytes(), 1000);
        assert!((metrics.retransmission_ratio() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn the_rejection_ratio_counts_all_attempts() {
        let metrics = SchedulerMetrics {
            enqueued: 90,
            rejected: 10,
            ..SchedulerMetrics::default()
        };

        assert!((metrics.rejection_ratio() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn the_mean_shaped_delay_is_averaged() {
        let metrics = SchedulerMetrics {
            shaped: 4,
            shaped_delay: Duration::from_millis(400),
            ..SchedulerMetrics::default()
        };

        assert_eq!(metrics.mean_shaped_delay(), Duration::from_millis(100));
    }

    #[test]
    fn pending_is_reported_per_class() {
        let mut metrics = SchedulerMetrics::default();
        metrics.pending_by_priority[Priority::High.index()] = 7;

        assert_eq!(metrics.pending_in(Priority::High), 7);
        assert_eq!(metrics.pending_in(Priority::Low), 0);
    }

    #[test]
    fn work_in_any_stage_means_not_idle() {
        for metrics in [
            SchedulerMetrics {
                pending: 1,
                ..SchedulerMetrics::default()
            },
            SchedulerMetrics {
                in_flight: 1,
                ..SchedulerMetrics::default()
            },
            SchedulerMetrics {
                awaiting_retry: 1,
                ..SchedulerMetrics::default()
            },
        ] {
            assert!(!metrics.is_idle(), "outstanding work must not read as idle");
        }
    }

    #[test]
    fn dispatch_counters_separate_first_sends_from_retries() {
        let mut counters = Counters::default();
        counters.record_dispatch(100, false);
        counters.record_dispatch(40, true);

        assert_eq!(counters.dispatched, 2);
        assert_eq!(counters.bytes_dispatched, 100);
        assert_eq!(counters.bytes_retransmitted, 40);
    }

    #[test]
    fn shaping_delay_accumulates() {
        let mut counters = Counters::default();
        counters.record_shaping(Duration::from_millis(30));
        counters.record_shaping(Duration::from_millis(70));

        assert_eq!(counters.shaped, 2);
        assert_eq!(counters.shaped_delay, Duration::from_millis(100));
    }
}
