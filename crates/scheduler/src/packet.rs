//! The packet scheduler: the component that ties the rest together.
//!
//! [`PacketScheduler`] owns a [`PriorityQueue`], a [`TrafficShaper`], and a
//! [`RetryManager`], and drives them from a single [`poll_at`] call. A caller
//! loops: poll, send whatever comes back, report the outcome.
//!
//! [`poll_at`]: PacketScheduler::poll_at
//!
//! ## Why polling rather than callbacks
//!
//! The scheduler never sends anything itself and owns no I/O. It is a pure
//! state machine driven by an explicit clock, which means the whole of its
//! behavior — rate limiting, backoff, priority inversion under load — is
//! testable without sockets, timers, or sleeping. The caller keeps control of
//! when work happens, which is what makes the same scheduler usable from a
//! Tokio task, a thread, or a simulation.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::metrics::{Counters, SchedulerMetrics};
use crate::priority::{EnqueueError, Priority, PriorityQueue};
use crate::retry::{RetryDecision, RetryManager, RetryPolicy};
use crate::shaping::{ShapeDecision, TrafficShaper};

/// Identifies a packet within one scheduler.
///
/// Identifiers are unique per scheduler instance and never reused, so a late
/// acknowledgement for an abandoned packet cannot be mistaken for a live one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PacketId(u64);

impl PacketId {
    /// Returns the identifier as an integer, for logging or wire encoding.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for PacketId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "packet#{}", self.0)
    }
}

/// A unit of work held by the scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet<T> {
    id: PacketId,
    priority: Priority,
    len: usize,
    attempts: u32,
    payload: T,
}

impl<T> Packet<T> {
    /// Returns this packet's identifier.
    #[must_use]
    pub const fn id(&self) -> PacketId {
        self.id
    }

    /// Returns the priority class it was queued in.
    #[must_use]
    pub const fn priority(&self) -> Priority {
        self.priority
    }

    /// Returns the declared payload length in bytes.
    ///
    /// This is what the shaper charges against the rate limit; it is supplied
    /// by the caller rather than measured, so a caller may include framing
    /// overhead if it wants the limit to account for it.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the declared length is zero.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns how many times this packet has been dispatched.
    ///
    /// A value above one means it is a retransmission.
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Returns `true` if this dispatch is a retransmission.
    #[must_use]
    pub const fn is_retransmission(&self) -> bool {
        self.attempts > 1
    }

    /// Returns a reference to the payload.
    #[must_use]
    pub const fn payload(&self) -> &T {
        &self.payload
    }

    /// Consumes the packet, returning its payload.
    #[must_use]
    pub fn into_payload(self) -> T {
        self.payload
    }
}

/// The result of polling the scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Dispatch<T> {
    /// Send this packet now.
    ///
    /// It is recorded as in flight; report the outcome with
    /// [`acknowledge`](PacketScheduler::acknowledge) or
    /// [`fail`](PacketScheduler::fail).
    Send(Packet<T>),
    /// Nothing may be sent yet. Poll again after `delay`.
    ///
    /// The delay accounts for both the rate limiter and the earliest pending
    /// retry, so sleeping for it wastes no work and misses nothing.
    Wait {
        /// How long until something will be ready.
        delay: Duration,
    },
    /// There is nothing queued and nothing pending.
    Idle,
}

impl<T> Dispatch<T> {
    /// Returns the packet, if one is ready to send.
    #[must_use]
    pub fn packet(self) -> Option<Packet<T>> {
        match self {
            Self::Send(packet) => Some(packet),
            Self::Wait { .. } | Self::Idle => None,
        }
    }

    /// Returns `true` if a packet is ready.
    #[must_use]
    pub const fn is_send(&self) -> bool {
        matches!(self, Self::Send(_))
    }

    /// Returns `true` if the scheduler has no work at all.
    #[must_use]
    pub const fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    /// Returns how long to wait, if the scheduler asked for a delay.
    #[must_use]
    pub const fn delay(&self) -> Option<Duration> {
        match self {
            Self::Wait { delay } => Some(*delay),
            Self::Send(_) | Self::Idle => None,
        }
    }
}

/// Configuration for a [`PacketScheduler`].
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct SchedulerConfig {
    /// How many packets each priority class may hold.
    pub queue_capacity: usize,
    /// The aggregate send rate in bytes per second.
    pub rate_bytes_per_second: f64,
    /// The retry policy applied to failed sends.
    pub retry: RetryPolicy,
}

impl SchedulerConfig {
    /// Creates a configuration with default values.
    ///
    /// Defaults to 1 MiB/s, which is deliberately conservative: an unmeasured
    /// link should not be flooded, and
    /// [`set_rate`](PacketScheduler::set_rate) raises it once bandwidth is
    /// known.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            queue_capacity: 1024,
            rate_bytes_per_second: 1_048_576.0, // 1 MiB/s
            retry: RetryPolicy::new(),
        }
    }

    /// Sets the per-class queue capacity.
    #[must_use]
    pub const fn with_queue_capacity(mut self, queue_capacity: usize) -> Self {
        self.queue_capacity = queue_capacity;
        self
    }

    /// Sets the aggregate send rate.
    #[must_use]
    pub fn with_rate(mut self, rate_bytes_per_second: f64) -> Self {
        self.rate_bytes_per_second = rate_bytes_per_second;
        self
    }

    /// Sets the retry policy.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Schedules packets by priority, subject to a rate limit, with retries.
///
/// # Examples
///
/// ```
/// use std::time::Instant;
/// use nexusnet_scheduler::{PacketScheduler, Priority, SchedulerConfig};
///
/// let now = Instant::now();
/// let config = SchedulerConfig::new().with_rate(1_000_000.0);
/// let mut scheduler: PacketScheduler<&str> = PacketScheduler::new_at(config, now);
///
/// scheduler.enqueue(Priority::Background, 100, "bulk")?;
/// scheduler.enqueue(Priority::Critical, 100, "heartbeat")?;
///
/// // Urgent traffic is released first.
/// let packet = scheduler.poll_at(now).packet().expect("a packet is ready");
/// assert_eq!(*packet.payload(), "heartbeat");
///
/// scheduler.acknowledge(packet.id());
/// # Ok::<(), nexusnet_scheduler::EnqueueError>(())
/// ```
#[derive(Debug)]
pub struct PacketScheduler<T> {
    queue: PriorityQueue<Packet<T>>,
    shaper: TrafficShaper,
    retries: RetryManager<Packet<T>>,
    /// A packet taken from the queue but held back by the rate limiter.
    ///
    /// Holding it here rather than pushing it back preserves the scheduler's
    /// decision exactly: the same packet is reconsidered next poll, instead of
    /// re-entering the queue and possibly losing its place.
    deferred: Option<Packet<T>>,
    /// Packets dispatched but not yet acknowledged or failed.
    in_flight: HashMap<PacketId, ()>,
    counters: Counters,
    next_id: u64,
}

impl<T> PacketScheduler<T> {
    /// Creates a scheduler from `config`.
    #[must_use]
    pub fn new(config: SchedulerConfig) -> Self {
        Self::new_at(config, Instant::now())
    }

    /// Creates a scheduler with an explicit starting instant.
    ///
    /// Prefer this in tests and simulations: with the clock supplied by the
    /// caller, every timing-dependent behavior is deterministic.
    #[must_use]
    pub fn new_at(config: SchedulerConfig, now: Instant) -> Self {
        Self {
            queue: PriorityQueue::new(config.queue_capacity),
            shaper: TrafficShaper::new_at(config.rate_bytes_per_second, now),
            retries: RetryManager::new(config.retry),
            deferred: None,
            in_flight: HashMap::new(),
            counters: Counters::default(),
            next_id: 0,
        }
    }

    /// Replaces the traffic shaper, discarding accrued credit.
    ///
    /// Use this to apply a reservation, which
    /// [`SchedulerConfig`] does not express:
    ///
    /// ```
    /// # use nexusnet_scheduler::{PacketScheduler, Priority, SchedulerConfig, TrafficShaper};
    /// # let mut scheduler: PacketScheduler<()> = PacketScheduler::new(SchedulerConfig::new());
    /// scheduler.set_shaper(
    ///     TrafficShaper::new(1_000_000.0).with_reservation(Priority::Critical, 0.2),
    /// );
    /// ```
    pub fn set_shaper(&mut self, shaper: TrafficShaper) {
        self.shaper = shaper;
    }

    /// Revises the aggregate send rate, keeping accrued credit.
    ///
    /// This is the hook an adaptive sender uses when its bandwidth estimate
    /// changes.
    pub fn set_rate(&mut self, bytes_per_second: f64) {
        self.shaper.set_rate(bytes_per_second);
    }

    /// Replaces the retry policy for future failures.
    pub fn set_retry_policy(&mut self, policy: RetryPolicy) {
        self.retries.set_policy(policy);
    }

    /// Returns the traffic shaper.
    #[must_use]
    pub const fn shaper(&self) -> &TrafficShaper {
        &self.shaper
    }

    /// Returns the number of packets queued, including any deferred one.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.queue.len() + usize::from(self.deferred.is_some())
    }

    /// Returns the number of packets sent but not yet resolved.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// Returns `true` when nothing is queued, in flight, or awaiting retry.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.pending() == 0 && self.in_flight.is_empty() && self.retries.is_empty()
    }

    /// Returns a snapshot of scheduler activity.
    #[must_use]
    pub fn metrics(&self) -> SchedulerMetrics {
        let mut pending_by_priority = [0_usize; 5];
        for priority in Priority::ALL {
            pending_by_priority[priority.index()] = self.queue.len_of(priority);
        }
        if let Some(deferred) = self.deferred.as_ref() {
            pending_by_priority[deferred.priority.index()] += 1;
        }

        SchedulerMetrics {
            enqueued: self.counters.enqueued,
            dispatched: self.counters.dispatched,
            rejected: self.counters.rejected,
            dropped: self.counters.dropped,
            bytes_dispatched: self.counters.bytes_dispatched,
            bytes_retransmitted: self.counters.bytes_retransmitted,
            shaped: self.counters.shaped,
            shaped_delay: self.counters.shaped_delay,
            retries_scheduled: self.counters.retries_scheduled,
            retries_dispatched: self.counters.retries_dispatched,
            acknowledged: self.counters.acknowledged,
            pending: self.pending(),
            in_flight: self.in_flight.len(),
            awaiting_retry: self.retries.pending(),
            pending_by_priority,
        }
    }

    /// Queues a payload of `len` bytes at `priority`.
    ///
    /// `len` is what the rate limiter charges; supply the on-wire size if you
    /// want framing overhead counted.
    ///
    /// # Errors
    ///
    /// Returns [`EnqueueError::Full`] if that priority class is at capacity.
    /// Capacity is per class, so a flood of bulk traffic cannot deny space to
    /// critical packets.
    pub fn enqueue(
        &mut self,
        priority: Priority,
        len: usize,
        payload: T,
    ) -> Result<PacketId, EnqueueError> {
        let id = PacketId(self.next_id);

        let packet = Packet {
            id,
            priority,
            len,
            attempts: 0,
            payload,
        };

        match self.queue.enqueue(priority, packet) {
            Ok(()) => {
                self.next_id += 1;
                self.counters.enqueued += 1;
                Ok(id)
            }
            Err(error) => {
                self.counters.rejected += 1;
                Err(error)
            }
        }
    }

    /// Advances the scheduler and returns what to do next.
    pub fn poll(&mut self) -> Dispatch<T> {
        self.poll_at(Instant::now())
    }

    /// Advances the scheduler as of `now`.
    ///
    /// Order of business: release any retries whose delay has elapsed, then
    /// take the next packet by priority, then ask the shaper whether it may go.
    /// Retries are re-queued rather than jumping the queue, so a retransmission
    /// competes fairly with fresh traffic of the same class.
    pub fn poll_at(&mut self, now: Instant) -> Dispatch<T> {
        self.release_due_retries(now);

        let Some(mut packet) = self.deferred.take().or_else(|| self.queue.dequeue()) else {
            return self.idle_or_wait(now);
        };

        match self.shaper.admit_at(packet.priority, packet.len, now) {
            ShapeDecision::Send => {
                packet.attempts += 1;
                let is_retransmission = packet.is_retransmission();

                self.counters.record_dispatch(packet.len, is_retransmission);
                if is_retransmission {
                    self.counters.retries_dispatched += 1;
                }
                self.in_flight.insert(packet.id, ());

                Dispatch::Send(packet)
            }
            ShapeDecision::Wait { delay } => {
                self.counters.record_shaping(delay);
                self.deferred = Some(packet);

                // A pending retry may come due sooner than the rate limit
                // clears, so wait for whichever is nearer.
                let retry_delay = self.retries.time_until_due(now).unwrap_or(Duration::MAX);
                Dispatch::Wait {
                    delay: delay.min(retry_delay),
                }
            }
            ShapeDecision::Oversized { .. } => {
                // No amount of waiting admits this packet, so drop it rather
                // than stall the queue behind it forever.
                self.counters.dropped += 1;
                self.poll_at(now)
            }
        }
    }

    /// Returns how long until the scheduler will have work, if it is waiting.
    #[must_use]
    pub fn time_until_ready(&mut self, now: Instant) -> Option<Duration> {
        if let Some(packet) = self.deferred.as_ref() {
            let (priority, len) = (packet.priority, packet.len);
            return self.shaper.time_until(priority, len, now);
        }

        self.retries.time_until_due(now)
    }

    /// Records that a packet was delivered successfully.
    ///
    /// Returns `true` if the packet was in flight; `false` means the
    /// acknowledgement was duplicate or late, which is worth ignoring rather
    /// than treating as an error.
    pub fn acknowledge(&mut self, id: PacketId) -> bool {
        if self.in_flight.remove(&id).is_some() {
            self.counters.acknowledged += 1;
            true
        } else {
            false
        }
    }

    /// Records that a dispatched packet failed.
    ///
    /// The packet is scheduled for another attempt if its budget allows,
    /// otherwise it is dropped. Returns the decision so a caller can log or
    /// surface an abandoned packet.
    pub fn fail(&mut self, packet: Packet<T>, now: Instant) -> RetryDecision {
        self.in_flight.remove(&packet.id);

        let attempts = packet.attempts;
        let decision = self.retries.record_failure(packet, attempts, now);

        match decision {
            RetryDecision::Scheduled { .. } => self.counters.retries_scheduled += 1,
            RetryDecision::Exhausted { .. } => self.counters.dropped += 1,
        }

        decision
    }

    /// Moves retries whose delay has elapsed back into the queue.
    fn release_due_retries(&mut self, now: Instant) {
        for packet in self.retries.take_due(now) {
            let priority = packet.priority;

            if self.queue.enqueue(priority, packet).is_err() {
                // The class filled while the retry was waiting. Dropping is the
                // honest outcome: the alternative is unbounded growth in a
                // queue the caller asked to bound.
                self.counters.dropped += 1;
            }
        }
    }

    /// Decides what to report when no packet could be taken.
    fn idle_or_wait(&mut self, now: Instant) -> Dispatch<T> {
        match self.retries.time_until_due(now) {
            Some(delay) => Dispatch::Wait { delay },
            None => Dispatch::Idle,
        }
    }

    /// Discards every queued and pending packet, returning their payloads.
    ///
    /// In-flight packets are not returned, since the caller already holds them.
    pub fn drain(&mut self) -> Vec<T> {
        let mut drained: Vec<T> = self
            .retries
            .drain()
            .into_iter()
            .map(Packet::into_payload)
            .collect();

        if let Some(packet) = self.deferred.take() {
            drained.push(packet.into_payload());
        }

        while let Some(packet) = self.queue.dequeue() {
            drained.push(packet.into_payload());
        }

        drained
    }
}

impl<T> Default for PacketScheduler<T> {
    fn default() -> Self {
        Self::new(SchedulerConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start() -> Instant {
        Instant::now()
    }

    /// A scheduler with a rate high enough that shaping never interferes.
    fn unshaped(now: Instant) -> PacketScheduler<&'static str> {
        PacketScheduler::new_at(
            SchedulerConfig::new()
                .with_rate(1e9)
                .with_retry(RetryPolicy::new().with_jitter(false)),
            now,
        )
    }

    #[test]
    fn an_empty_scheduler_is_idle() {
        let now = start();
        let mut scheduler = unshaped(now);

        assert!(scheduler.poll_at(now).is_idle());
        assert!(scheduler.is_idle());
        assert!(scheduler.metrics().is_idle());
    }

    #[test]
    fn a_queued_packet_is_dispatched() {
        let now = start();
        let mut scheduler = unshaped(now);

        let id = scheduler
            .enqueue(Priority::Normal, 100, "payload")
            .expect("accepted");

        let packet = scheduler.poll_at(now).packet().expect("a packet is ready");

        assert_eq!(packet.id(), id);
        assert_eq!(*packet.payload(), "payload");
        assert_eq!(packet.attempts(), 1);
        assert!(!packet.is_retransmission());
        assert_eq!(scheduler.in_flight(), 1);
    }

    #[test]
    fn urgent_traffic_is_dispatched_first() {
        let now = start();
        let mut scheduler = unshaped(now);

        scheduler
            .enqueue(Priority::Background, 10, "bulk")
            .expect("accepted");
        scheduler
            .enqueue(Priority::Critical, 10, "heartbeat")
            .expect("accepted");

        let first = scheduler.poll_at(now).packet().expect("ready");
        assert_eq!(*first.payload(), "heartbeat");
    }

    #[test]
    fn bulk_traffic_is_not_starved() {
        let now = start();
        let mut scheduler = PacketScheduler::new_at(
            SchedulerConfig::new()
                .with_rate(1e9)
                .with_queue_capacity(500),
            now,
        );

        for _ in 0..200 {
            scheduler
                .enqueue(Priority::Critical, 10, Priority::Critical)
                .expect("accepted");
            scheduler
                .enqueue(Priority::Background, 10, Priority::Background)
                .expect("accepted");
        }

        let mut background = 0;
        for _ in 0..100 {
            if let Some(packet) = scheduler.poll_at(now).packet() {
                if *packet.payload() == Priority::Background {
                    background += 1;
                }
                scheduler.acknowledge(packet.id());
            }
        }

        assert!(
            background > 0,
            "weighted fair queueing must let bulk traffic through"
        );
    }

    #[test]
    fn the_rate_limit_defers_a_packet() {
        let now = start();
        // 1000 bytes per second, so one second of burst.
        let mut scheduler = PacketScheduler::new_at(SchedulerConfig::new().with_rate(1000.0), now);

        scheduler
            .enqueue(Priority::Normal, 1000, "first")
            .expect("accepted");
        scheduler
            .enqueue(Priority::Normal, 500, "second")
            .expect("accepted");

        assert!(
            scheduler.poll_at(now).is_send(),
            "the burst admits the first"
        );

        let dispatch = scheduler.poll_at(now);
        let delay = dispatch.delay().expect("the second must wait");
        assert!(
            (delay.as_secs_f64() - 0.5).abs() < 0.05,
            "500 bytes at 1000/s is half a second, got {delay:?}"
        );

        // The deferred packet is still counted as pending, not lost.
        assert_eq!(scheduler.pending(), 1);
        assert_eq!(scheduler.metrics().shaped, 1);
    }

    #[test]
    fn a_deferred_packet_is_dispatched_once_credit_accrues() {
        let now = start();
        let mut scheduler = PacketScheduler::new_at(SchedulerConfig::new().with_rate(1000.0), now);

        scheduler
            .enqueue(Priority::Normal, 1000, "first")
            .expect("accepted");
        scheduler
            .enqueue(Priority::Normal, 500, "second")
            .expect("accepted");

        assert!(scheduler.poll_at(now).is_send());
        assert!(!scheduler.poll_at(now).is_send());

        let later = now + Duration::from_millis(500);
        let packet = scheduler.poll_at(later).packet().expect("credit accrued");
        assert_eq!(*packet.payload(), "second");
    }

    #[test]
    fn a_deferred_packet_keeps_its_place() {
        let now = start();
        let mut scheduler = PacketScheduler::new_at(SchedulerConfig::new().with_rate(1000.0), now);

        scheduler
            .enqueue(Priority::Normal, 1000, "first")
            .expect("accepted");
        scheduler
            .enqueue(Priority::Normal, 400, "deferred")
            .expect("accepted");

        assert!(scheduler.poll_at(now).is_send());
        assert!(!scheduler.poll_at(now).is_send(), "the second is deferred");

        // Higher-priority traffic arrives while the second waits.
        scheduler
            .enqueue(Priority::Critical, 100, "urgent")
            .expect("accepted");

        let later = now + Duration::from_secs(1);
        let packet = scheduler.poll_at(later).packet().expect("ready");
        assert_eq!(
            *packet.payload(),
            "deferred",
            "the packet already taken from the queue is reconsidered first"
        );
    }

    #[test]
    fn an_oversized_packet_is_dropped_rather_than_stalling_the_queue() {
        let now = start();
        // Burst capacity is 1000 bytes; a 5000-byte packet can never be sent.
        let mut scheduler = PacketScheduler::new_at(SchedulerConfig::new().with_rate(1000.0), now);

        scheduler
            .enqueue(Priority::Normal, 5000, "impossible")
            .expect("accepted");
        scheduler
            .enqueue(Priority::Normal, 100, "fine")
            .expect("accepted");

        let packet = scheduler.poll_at(now).packet().expect("ready");
        assert_eq!(
            *packet.payload(),
            "fine",
            "an unsendable packet must not block the queue behind it"
        );
        assert_eq!(scheduler.metrics().dropped, 1);
    }

    #[test]
    fn acknowledging_clears_a_packet() {
        let now = start();
        let mut scheduler = unshaped(now);
        scheduler
            .enqueue(Priority::Normal, 10, "payload")
            .expect("accepted");

        let packet = scheduler.poll_at(now).packet().expect("ready");
        assert!(scheduler.acknowledge(packet.id()));
        assert_eq!(scheduler.in_flight(), 0);
        assert_eq!(scheduler.metrics().acknowledged, 1);

        assert!(
            !scheduler.acknowledge(packet.id()),
            "a duplicate acknowledgement is ignored, not an error"
        );
    }

    #[test]
    fn a_failed_packet_is_retried_after_its_delay() {
        let now = start();
        let mut scheduler = PacketScheduler::new_at(
            SchedulerConfig::new().with_rate(1e9).with_retry(
                RetryPolicy::new()
                    .with_jitter(false)
                    .with_initial_delay(Duration::from_millis(100)),
            ),
            now,
        );

        scheduler
            .enqueue(Priority::Normal, 10, "payload")
            .expect("accepted");

        let packet = scheduler.poll_at(now).packet().expect("ready");
        let decision = scheduler.fail(packet, now);
        assert!(decision.will_retry());
        assert_eq!(scheduler.in_flight(), 0);

        // Not yet due.
        let dispatch = scheduler.poll_at(now);
        assert!(!dispatch.is_send());
        assert_eq!(dispatch.delay(), Some(Duration::from_millis(100)));

        // Due now, and marked as a retransmission.
        let retried = scheduler
            .poll_at(now + Duration::from_millis(100))
            .packet()
            .expect("the retry is due");
        assert_eq!(*retried.payload(), "payload");
        assert_eq!(retried.attempts(), 2);
        assert!(retried.is_retransmission());
    }

    #[test]
    fn retransmitted_bytes_are_counted_separately() {
        let now = start();
        let mut scheduler = PacketScheduler::new_at(
            SchedulerConfig::new().with_rate(1e9).with_retry(
                RetryPolicy::new()
                    .with_jitter(false)
                    .with_initial_delay(Duration::from_millis(10)),
            ),
            now,
        );

        scheduler
            .enqueue(Priority::Normal, 100, "payload")
            .expect("accepted");

        let packet = scheduler.poll_at(now).packet().expect("ready");
        scheduler.fail(packet, now);

        let retried = scheduler
            .poll_at(now + Duration::from_millis(20))
            .packet()
            .expect("the retry is due");
        scheduler.acknowledge(retried.id());

        let metrics = scheduler.metrics();
        assert_eq!(metrics.bytes_dispatched, 100);
        assert_eq!(metrics.bytes_retransmitted, 100);
        assert!((metrics.retransmission_ratio() - 0.5).abs() < 1e-9);
        assert_eq!(metrics.retries_scheduled, 1);
        assert_eq!(metrics.retries_dispatched, 1);
    }

    #[test]
    fn a_packet_is_dropped_once_its_budget_is_exhausted() {
        let now = start();
        let mut scheduler = PacketScheduler::new_at(
            SchedulerConfig::new().with_rate(1e9).with_retry(
                RetryPolicy::new()
                    .with_jitter(false)
                    .with_initial_delay(Duration::from_millis(1))
                    .with_max_attempts(Some(3)),
            ),
            now,
        );

        scheduler
            .enqueue(Priority::Normal, 10, "doomed")
            .expect("accepted");

        let mut clock = now;
        let mut last = None;

        for _ in 0..3 {
            clock += Duration::from_millis(50);
            let packet = scheduler.poll_at(clock).packet().expect("ready");
            last = Some(scheduler.fail(packet, clock));
        }

        assert_eq!(
            last,
            Some(RetryDecision::Exhausted { attempts: 3 }),
            "the third failure exhausts a three-attempt budget"
        );
        assert!(scheduler.poll_at(clock + Duration::from_secs(1)).is_idle());
        assert_eq!(scheduler.metrics().dropped, 1);
    }

    #[test]
    fn a_full_class_rejects_further_packets() {
        let now = start();
        let mut scheduler = PacketScheduler::new_at(
            SchedulerConfig::new().with_rate(1e9).with_queue_capacity(2),
            now,
        );

        scheduler
            .enqueue(Priority::Background, 10, "a")
            .expect("accepted");
        scheduler
            .enqueue(Priority::Background, 10, "b")
            .expect("accepted");

        let error = scheduler
            .enqueue(Priority::Background, 10, "c")
            .expect_err("the class is full");
        assert!(matches!(error, EnqueueError::Full { .. }));

        // A different class still has room, which is the point of per-class
        // capacity.
        scheduler
            .enqueue(Priority::Critical, 10, "urgent")
            .expect("accepted");

        let metrics = scheduler.metrics();
        assert_eq!(metrics.rejected, 1);
        assert!((metrics.rejection_ratio() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn the_wait_accounts_for_both_shaping_and_retries() {
        let now = start();
        let mut scheduler = PacketScheduler::new_at(
            SchedulerConfig::new().with_rate(1000.0).with_retry(
                RetryPolicy::new()
                    .with_jitter(false)
                    .with_initial_delay(Duration::from_millis(50)),
            ),
            now,
        );

        // Drain the burst so the next packet must wait a full second.
        scheduler
            .enqueue(Priority::Normal, 1000, "first")
            .expect("accepted");
        let first = scheduler.poll_at(now).packet().expect("ready");

        scheduler
            .enqueue(Priority::Normal, 1000, "second")
            .expect("accepted");
        scheduler.fail(first, now); // Due in 50ms.

        let dispatch = scheduler.poll_at(now);
        let delay = dispatch.delay().expect("waiting");
        assert!(
            delay <= Duration::from_millis(50),
            "the sooner of the two deadlines should win, got {delay:?}"
        );
    }

    #[test]
    fn the_rate_can_be_revised_at_runtime() {
        let now = start();
        let mut scheduler = PacketScheduler::new_at(SchedulerConfig::new().with_rate(1000.0), now);

        scheduler
            .enqueue(Priority::Normal, 1000, "first")
            .expect("accepted");
        assert!(scheduler.poll_at(now).is_send());

        // A better bandwidth estimate arrives.
        scheduler.set_rate(10_000.0);

        scheduler
            .enqueue(Priority::Normal, 1000, "second")
            .expect("accepted");
        let later = now + Duration::from_millis(150);
        assert!(
            scheduler.poll_at(later).is_send(),
            "the raised rate should admit the packet sooner"
        );
    }

    #[test]
    fn metrics_report_pending_work_per_class() {
        let now = start();
        let mut scheduler = unshaped(now);

        scheduler
            .enqueue(Priority::High, 10, "a")
            .expect("accepted");
        scheduler
            .enqueue(Priority::High, 10, "b")
            .expect("accepted");
        scheduler.enqueue(Priority::Low, 10, "c").expect("accepted");

        let metrics = scheduler.metrics();
        assert_eq!(metrics.pending, 3);
        assert_eq!(metrics.pending_in(Priority::High), 2);
        assert_eq!(metrics.pending_in(Priority::Low), 1);
        assert_eq!(metrics.pending_in(Priority::Critical), 0);
        assert!(!metrics.is_idle());
    }

    #[test]
    fn draining_returns_queued_and_pending_payloads() {
        let now = start();
        let mut scheduler = PacketScheduler::new_at(
            SchedulerConfig::new()
                .with_rate(1e9)
                .with_retry(RetryPolicy::new().with_jitter(false)),
            now,
        );

        scheduler
            .enqueue(Priority::Normal, 10, "queued")
            .expect("accepted");
        scheduler
            .enqueue(Priority::Normal, 10, "failed")
            .expect("accepted");

        // Send one and fail it so it sits in the retry manager.
        let packet = scheduler.poll_at(now).packet().expect("ready");
        scheduler.fail(packet, now);

        let mut drained = scheduler.drain();
        drained.sort_unstable();

        assert_eq!(drained, vec!["failed", "queued"]);
        assert_eq!(scheduler.pending(), 0);
    }

    #[test]
    fn identifiers_are_unique_and_not_reused() {
        let now = start();
        let mut scheduler = unshaped(now);

        let first = scheduler
            .enqueue(Priority::Normal, 10, "a")
            .expect("accepted");
        let second = scheduler
            .enqueue(Priority::Normal, 10, "b")
            .expect("accepted");

        assert_ne!(first, second);
        assert_eq!(first.get() + 1, second.get());
        assert_eq!(first.to_string(), "packet#0");
    }

    #[test]
    fn a_rejected_packet_does_not_consume_an_identifier() {
        let now = start();
        let mut scheduler = PacketScheduler::new_at(
            SchedulerConfig::new().with_rate(1e9).with_queue_capacity(1),
            now,
        );

        let first = scheduler
            .enqueue(Priority::Normal, 10, "a")
            .expect("accepted");
        assert!(scheduler.enqueue(Priority::Normal, 10, "b").is_err());

        // The identifier sequence must not skip over a packet that was never
        // accepted, or gaps would look like lost packets to an observer.
        let next = scheduler
            .enqueue(Priority::High, 10, "c")
            .expect("accepted");
        assert_eq!(next.get(), first.get() + 1);
    }
}
