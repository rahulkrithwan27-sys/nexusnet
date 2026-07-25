//! Priority classes and weighted fair queueing.
//!
//! Strict priority — always drain the highest class first — is the obvious
//! design and the wrong one. A steady stream of high-priority traffic starves
//! everything below it indefinitely, and background work that never runs is a
//! bug that shows up in production as "the metrics stopped arriving".
//!
//! [`PriorityQueue`] uses **deficit round robin** instead. Each class earns
//! credit in proportion to its weight and spends it to dequeue. Higher classes
//! get more bandwidth, but every class makes progress, and the ratio is a
//! configured number rather than an emergent accident.

use std::collections::VecDeque;

/// How urgent a queued item is.
///
/// Discriminants ascend with urgency so ordering comparisons read naturally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
#[repr(u8)]
pub enum Priority {
    /// Bulk transfers and telemetry. Yields to everything else.
    Background = 0,
    /// Ordinary work with no particular deadline.
    Low = 1,
    /// The default for application traffic.
    Normal = 2,
    /// Interactive traffic that a user is waiting on.
    High = 3,
    /// Control and liveness traffic that must not be delayed.
    Critical = 4,
}

impl Priority {
    /// Every priority, lowest first.
    pub const ALL: [Self; 5] = [
        Self::Background,
        Self::Low,
        Self::Normal,
        Self::High,
        Self::Critical,
    ];

    /// Returns the scheduling weight of this class.
    ///
    /// Weights are the share of service each class receives per round. The
    /// spread is deliberately wide enough to matter but bounded so the lowest
    /// class still progresses: critical traffic gets 16 times background's
    /// share, not unlimited priority over it.
    #[must_use]
    pub const fn weight(self) -> u32 {
        match self {
            Self::Background => 1,
            Self::Low => 2,
            Self::Normal => 4,
            Self::High => 8,
            Self::Critical => 16,
        }
    }

    /// Returns the class as an index into a five-element array.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

impl Default for Priority {
    fn default() -> Self {
        Self::Normal
    }
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Background => "background",
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
            Self::Critical => "critical",
        };
        f.write_str(name)
    }
}

/// A snapshot of queue activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct QueueStats {
    /// Items accepted into the queue.
    pub enqueued: u64,
    /// Items handed out by the scheduler.
    pub dequeued: u64,
    /// Items refused because a class was at capacity.
    pub rejected: u64,
    /// Items currently waiting.
    pub pending: usize,
}

/// A weighted fair queue across [`Priority`] classes.
///
/// # Examples
///
/// ```
/// use nexusnet_scheduler::{Priority, PriorityQueue};
///
/// let mut queue: PriorityQueue<&str> = PriorityQueue::new(16);
///
/// queue.enqueue(Priority::Background, "bulk upload").unwrap();
/// queue.enqueue(Priority::Critical, "heartbeat").unwrap();
///
/// // Critical traffic is served first, but background traffic still runs.
/// assert_eq!(queue.dequeue(), Some("heartbeat"));
/// assert_eq!(queue.dequeue(), Some("bulk upload"));
/// ```
#[derive(Debug)]
pub struct PriorityQueue<T> {
    lanes: [VecDeque<T>; 5],
    /// Accumulated service credit per class, in items.
    deficit: [u32; 5],
    capacity_per_class: usize,
    enqueued: u64,
    dequeued: u64,
    rejected: u64,
}

/// The reason an item could not be enqueued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EnqueueError {
    /// The priority class is at capacity.
    ///
    /// Capacity is per class, so a flood of background work cannot consume the
    /// space that critical traffic will need.
    #[error("the {priority} queue is full at {capacity} items")]
    Full {
        /// The class that was full.
        priority: Priority,
        /// The configured per-class capacity.
        capacity: usize,
    },
}

impl<T> PriorityQueue<T> {
    /// Creates a queue holding at most `capacity_per_class` items per class.
    ///
    /// Zero is raised to one.
    #[must_use]
    pub fn new(capacity_per_class: usize) -> Self {
        Self {
            lanes: Default::default(),
            deficit: [0; 5],
            capacity_per_class: capacity_per_class.max(1),
            enqueued: 0,
            dequeued: 0,
            rejected: 0,
        }
    }

    /// Returns the total number of waiting items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lanes.iter().map(VecDeque::len).sum()
    }

    /// Returns `true` when nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lanes.iter().all(VecDeque::is_empty)
    }

    /// Returns how many items wait in one class.
    #[must_use]
    pub fn len_of(&self, priority: Priority) -> usize {
        self.lanes[priority.index()].len()
    }

    /// Returns a snapshot of queue activity.
    #[must_use]
    pub fn stats(&self) -> QueueStats {
        QueueStats {
            enqueued: self.enqueued,
            dequeued: self.dequeued,
            rejected: self.rejected,
            pending: self.len(),
        }
    }

    /// Adds an item to a priority class.
    ///
    /// # Errors
    ///
    /// Returns [`EnqueueError::Full`] if that class is at capacity. Capacity is
    /// per class deliberately: a flood of low-priority work must not be able to
    /// crowd out the critical traffic that keeps a connection alive.
    pub fn enqueue(&mut self, priority: Priority, item: T) -> Result<(), EnqueueError> {
        let lane = &mut self.lanes[priority.index()];

        if lane.len() >= self.capacity_per_class {
            self.rejected += 1;
            return Err(EnqueueError::Full {
                priority,
                capacity: self.capacity_per_class,
            });
        }

        let was_idle = lane.is_empty();
        lane.push_back(item);

        if was_idle {
            // A class that has just become active starts with its full share.
            // Without this, urgent traffic arriving into an idle class would
            // queue behind a busy class still holding credit from this round.
            self.deficit[priority.index()] = self.deficit[priority.index()].max(priority.weight());
        }

        self.enqueued += 1;

        Ok(())
    }

    /// Removes the next item to service, or `None` if the queue is empty.
    ///
    /// Classes are served in proportion to their weight, and the round-robin
    /// cursor advances so no class can monopolize service.
    pub fn dequeue(&mut self) -> Option<T> {
        if self.is_empty() {
            return None;
        }

        // Two passes at most: spend existing credit, then replenish and retry.
        // The replenish gives every non-empty class credit, so the second pass
        // always finds something.
        for _ in 0..2 {
            // Highest priority first. Fairness comes from the size of each
            // class's credit, not from the order they are examined, so scanning
            // downwards gives urgent traffic its natural precedence while the
            // deficits still bound how much it may take.
            for priority in Priority::ALL.iter().rev() {
                let index = priority.index();

                if self.lanes[index].is_empty() {
                    // An idle class does not bank credit; otherwise it would
                    // burst unfairly the moment it becomes active again.
                    self.deficit[index] = 0;
                    continue;
                }

                if self.deficit[index] == 0 {
                    continue;
                }

                self.deficit[index] -= 1;

                if let Some(item) = self.lanes[index].pop_front() {
                    self.dequeued += 1;
                    return Some(item);
                }
            }

            self.replenish();
        }

        None
    }

    /// Grants each non-empty class its weight in credit.
    fn replenish(&mut self) {
        for priority in Priority::ALL {
            let index = priority.index();
            if !self.lanes[index].is_empty() {
                self.deficit[index] = self.deficit[index].saturating_add(priority.weight());
            }
        }
    }

    /// Removes every waiting item.
    pub fn clear(&mut self) {
        for lane in &mut self.lanes {
            lane.clear();
        }
        self.deficit = [0; 5];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priorities_order_by_urgency() {
        assert!(Priority::Critical > Priority::High);
        assert!(Priority::High > Priority::Normal);
        assert!(Priority::Normal > Priority::Low);
        assert!(Priority::Low > Priority::Background);
        assert_eq!(Priority::default(), Priority::Normal);
    }

    #[test]
    fn weights_ascend_with_priority() {
        let mut previous = 0;
        for priority in Priority::ALL {
            assert!(
                priority.weight() > previous,
                "{priority} should outweigh the class below it"
            );
            previous = priority.weight();
        }
    }

    #[test]
    fn higher_priority_is_served_first() {
        let mut queue: PriorityQueue<&str> = PriorityQueue::new(8);
        queue
            .enqueue(Priority::Background, "bulk")
            .expect("accepted");
        queue.enqueue(Priority::Critical, "ping").expect("accepted");

        assert_eq!(queue.dequeue(), Some("ping"));
        assert_eq!(queue.dequeue(), Some("bulk"));
        assert!(queue.dequeue().is_none());
    }

    #[test]
    fn low_priority_is_never_starved() {
        let mut queue: PriorityQueue<Priority> = PriorityQueue::new(1000);

        // Saturate every class so the scheduler must choose continuously.
        for _ in 0..200 {
            queue
                .enqueue(Priority::Critical, Priority::Critical)
                .expect("accepted");
            queue
                .enqueue(Priority::Background, Priority::Background)
                .expect("accepted");
        }

        let mut background_served = 0;
        for _ in 0..100 {
            if queue.dequeue() == Some(Priority::Background) {
                background_served += 1;
            }
        }

        assert!(
            background_served > 0,
            "strict priority would starve background traffic entirely"
        );
    }

    #[test]
    fn service_is_roughly_proportional_to_weight() {
        let mut queue: PriorityQueue<Priority> = PriorityQueue::new(10_000);

        for _ in 0..2000 {
            queue
                .enqueue(Priority::Critical, Priority::Critical)
                .expect("accepted");
            queue
                .enqueue(Priority::Background, Priority::Background)
                .expect("accepted");
        }

        let mut critical = 0;
        let mut background = 0;
        for _ in 0..1700 {
            match queue.dequeue() {
                Some(Priority::Critical) => critical += 1,
                Some(Priority::Background) => background += 1,
                _ => {}
            }
        }

        // Weights are 16 and 1, so critical should dominate but not exclude.
        assert!(background > 0, "background must make progress");
        let ratio = critical as f64 / background as f64;
        assert!(
            (8.0..=32.0).contains(&ratio),
            "expected roughly a 16:1 split, got {ratio:.1}:1 ({critical} vs {background})"
        );
    }

    #[test]
    fn per_class_capacity_protects_critical_traffic() {
        let mut queue: PriorityQueue<u32> = PriorityQueue::new(2);

        // Flood the background class to its limit.
        queue.enqueue(Priority::Background, 1).expect("accepted");
        queue.enqueue(Priority::Background, 2).expect("accepted");
        let err = queue
            .enqueue(Priority::Background, 3)
            .expect_err("the background class is full");
        assert!(matches!(
            err,
            EnqueueError::Full {
                priority: Priority::Background,
                capacity: 2
            }
        ));

        // Critical traffic still has its own space.
        queue.enqueue(Priority::Critical, 99).expect("accepted");
        assert_eq!(queue.stats().rejected, 1);
    }

    #[test]
    fn fifo_order_holds_within_a_class() {
        let mut queue: PriorityQueue<u32> = PriorityQueue::new(16);
        for i in 0..5 {
            queue.enqueue(Priority::Normal, i).expect("accepted");
        }

        for expected in 0..5 {
            assert_eq!(queue.dequeue(), Some(expected));
        }
    }

    #[test]
    fn an_idle_class_does_not_bank_credit() {
        let mut queue: PriorityQueue<&str> = PriorityQueue::new(64);

        // Drain plenty of normal traffic while critical stays idle.
        for _ in 0..50 {
            queue.enqueue(Priority::Normal, "n").expect("accepted");
        }
        for _ in 0..50 {
            let _ = queue.dequeue();
        }

        // Critical arrives now; it should be served promptly, not burst.
        queue.enqueue(Priority::Critical, "c").expect("accepted");
        queue.enqueue(Priority::Normal, "n").expect("accepted");
        assert_eq!(queue.dequeue(), Some("c"));
    }

    #[test]
    fn statistics_track_activity() {
        let mut queue: PriorityQueue<u32> = PriorityQueue::new(2);
        queue.enqueue(Priority::Normal, 1).expect("accepted");
        queue.enqueue(Priority::Normal, 2).expect("accepted");
        let _ = queue.enqueue(Priority::Normal, 3);
        let _ = queue.dequeue();

        let stats = queue.stats();
        assert_eq!(stats.enqueued, 2);
        assert_eq!(stats.dequeued, 1);
        assert_eq!(stats.rejected, 1);
        assert_eq!(stats.pending, 1);
    }

    #[test]
    fn clearing_empties_every_class() {
        let mut queue: PriorityQueue<u32> = PriorityQueue::new(8);
        queue.enqueue(Priority::High, 1).expect("accepted");
        queue.enqueue(Priority::Low, 2).expect("accepted");

        queue.clear();
        assert!(queue.is_empty());
        assert!(queue.dequeue().is_none());
    }
}
