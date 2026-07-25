//! Per-stream flow control using credit windows.
//!
//! This is the mechanism that removes head-of-line blocking from a multiplexed
//! session. Without it, a session either has no backpressure at all (a slow
//! consumer makes the sender buffer without bound) or applies backpressure to
//! the whole connection (a slow consumer stalls every other stream too). Both
//! are wrong.
//!
//! A credit window fixes it by making backpressure *per stream*. A receiver
//! advertises how many bytes it is prepared to accept; the sender may send only
//! that much before waiting. Because the accounting is per stream, one stalled
//! consumer exhausts only its own window and every other stream keeps flowing.
//! This is what HTTP/2 and QUIC do, for exactly this reason.
//!
//! ## The two halves
//!
//! [`SendWindow`] is the sender's view: how much it may still transmit.
//! [`ReceiveWindow`] is the receiver's: how much it has consumed and when to
//! tell the peer that space has freed up. They are deliberately separate types,
//! because confusing the two is the classic flow-control bug.

use std::collections::HashMap;

/// The default initial window: 256 KiB per stream.
///
/// Large enough that a healthy stream is never throttled by the window on a
/// typical network, small enough that many streams do not commit unbounded
/// memory.
pub const DEFAULT_WINDOW: u32 = 256 * 1024;

/// The fraction of a window that must be consumed before an update is sent.
///
/// Updating after every byte would flood the connection with bookkeeping;
/// waiting until the window is empty would stall the sender. Half is the usual
/// compromise.
pub const DEFAULT_UPDATE_THRESHOLD: f64 = 0.5;

/// An error arising from flow-control accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FlowError {
    /// The peer sent more than its window permitted.
    ///
    /// This is a protocol violation rather than congestion: a correct peer
    /// tracks its own window and stops. The connection should be closed.
    #[error("peer exceeded its flow-control window by {excess} bytes")]
    WindowExceeded {
        /// How many bytes beyond the window were received.
        excess: u32,
    },

    /// A window update would overflow the permitted maximum.
    #[error("window update of {increment} would exceed the maximum window size")]
    WindowOverflow {
        /// The increment that was refused.
        increment: u32,
    },
}

/// The sender's view of a stream's flow-control window.
///
/// # Examples
///
/// ```
/// use nexusnet_scheduler::SendWindow;
///
/// let mut window = SendWindow::new(1000);
///
/// // Send within the window.
/// assert_eq!(window.reserve(600), 600);
/// assert_eq!(window.available(), 400);
///
/// // A larger request is clamped to what the window allows.
/// assert_eq!(window.reserve(900), 400);
/// assert!(window.is_blocked());
///
/// // The peer reports it has consumed data, freeing space.
/// window.grant(500).unwrap();
/// assert_eq!(window.available(), 500);
/// ```
#[derive(Debug, Clone)]
pub struct SendWindow {
    available: u32,
    sent: u64,
    blocked_count: u64,
}

impl SendWindow {
    /// Creates a window with `initial` bytes of credit.
    #[must_use]
    pub const fn new(initial: u32) -> Self {
        Self {
            available: initial,
            sent: 0,
            blocked_count: 0,
        }
    }

    /// Returns the bytes that may still be sent.
    #[must_use]
    pub const fn available(&self) -> u32 {
        self.available
    }

    /// Returns `true` when no credit remains.
    #[must_use]
    pub const fn is_blocked(&self) -> bool {
        self.available == 0
    }

    /// Returns the total bytes sent through this window.
    #[must_use]
    pub const fn sent(&self) -> u64 {
        self.sent
    }

    /// Returns how many times this window ran out of credit.
    ///
    /// A rising count means the window is too small for the traffic, or the
    /// peer is slow to consume.
    #[must_use]
    pub const fn blocked_count(&self) -> u64 {
        self.blocked_count
    }

    /// Reserves up to `wanted` bytes, returning how many were granted.
    ///
    /// Returns less than requested when the window is nearly exhausted, and
    /// zero when it is empty. Partial grants are deliberate: sending what fits
    /// makes progress, whereas refusing outright would stall a stream that
    /// could still move data.
    pub fn reserve(&mut self, wanted: u32) -> u32 {
        let granted = wanted.min(self.available);

        self.available -= granted;
        self.sent += u64::from(granted);

        if self.available == 0 && granted < wanted {
            self.blocked_count += 1;
        }

        granted
    }

    /// Adds `increment` bytes of credit, as advertised by the peer.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::WindowOverflow`] if the credit would exceed
    /// [`u32::MAX`], which indicates a buggy or hostile peer rather than a
    /// legitimate update.
    pub fn grant(&mut self, increment: u32) -> Result<(), FlowError> {
        self.available = self
            .available
            .checked_add(increment)
            .ok_or(FlowError::WindowOverflow { increment })?;

        Ok(())
    }
}

/// The receiver's view of a stream's flow-control window.
#[derive(Debug, Clone)]
pub struct ReceiveWindow {
    capacity: u32,
    /// Bytes received but not yet consumed by the application.
    outstanding: u32,
    /// Bytes consumed since the last window update was emitted.
    unreported: u32,
    threshold: u32,
    received: u64,
}

impl ReceiveWindow {
    /// Creates a window of `capacity` bytes using the default update threshold.
    #[must_use]
    pub fn new(capacity: u32) -> Self {
        Self::with_threshold(capacity, DEFAULT_UPDATE_THRESHOLD)
    }

    /// Creates a window that emits an update once `threshold` of it has been
    /// consumed.
    ///
    /// The threshold is clamped to `0.05..=1.0`; below that, updates would
    /// dominate the connection.
    #[must_use]
    pub fn with_threshold(capacity: u32, threshold: f64) -> Self {
        let capacity = capacity.max(1);
        let clamped = threshold.clamp(0.05, 1.0);

        Self {
            capacity,
            outstanding: 0,
            unreported: 0,
            threshold: ((capacity as f64) * clamped) as u32,
            received: 0,
        }
    }

    /// Returns the window capacity.
    #[must_use]
    pub const fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Returns the space currently free.
    #[must_use]
    pub const fn available(&self) -> u32 {
        self.capacity - self.outstanding
    }

    /// Returns bytes received but not yet consumed.
    #[must_use]
    pub const fn outstanding(&self) -> u32 {
        self.outstanding
    }

    /// Returns the total bytes received.
    #[must_use]
    pub const fn received(&self) -> u64 {
        self.received
    }

    /// Records `len` bytes arriving from the peer.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::WindowExceeded`] if the peer sent more than its
    /// window allowed. That is a protocol violation, not congestion: the
    /// connection should be closed rather than the data dropped.
    pub fn receive(&mut self, len: u32) -> Result<(), FlowError> {
        let used = self.outstanding.saturating_add(len);

        if used > self.capacity {
            return Err(FlowError::WindowExceeded {
                excess: used - self.capacity,
            });
        }

        self.outstanding = used;
        self.received += u64::from(len);

        Ok(())
    }

    /// Records that the application consumed `len` bytes.
    ///
    /// Returns `Some(increment)` when enough has been consumed to be worth
    /// telling the peer, and `None` otherwise. Sending that increment is what
    /// re-opens the sender's window.
    pub fn consume(&mut self, len: u32) -> Option<u32> {
        let freed = len.min(self.outstanding);
        self.outstanding -= freed;
        self.unreported = self.unreported.saturating_add(freed);

        if self.unreported >= self.threshold && self.unreported > 0 {
            let increment = self.unreported;
            self.unreported = 0;
            Some(increment)
        } else {
            None
        }
    }

    /// Forces a window update for whatever has been consumed.
    ///
    /// Useful when a stream goes idle holding un-reported credit that the peer
    /// would otherwise wait on.
    pub fn flush_update(&mut self) -> Option<u32> {
        if self.unreported == 0 {
            return None;
        }

        let increment = self.unreported;
        self.unreported = 0;
        Some(increment)
    }
}

/// Flow-control state for every stream in a session.
///
/// Keeping the per-stream windows together makes the central claim testable:
/// exhausting one stream's window must leave the others untouched.
#[derive(Debug)]
pub struct FlowController {
    send: HashMap<u32, SendWindow>,
    receive: HashMap<u32, ReceiveWindow>,
    initial_window: u32,
}

impl FlowController {
    /// Creates a controller issuing `initial_window` bytes to each new stream.
    #[must_use]
    pub fn new(initial_window: u32) -> Self {
        Self {
            send: HashMap::new(),
            receive: HashMap::new(),
            initial_window: initial_window.max(1),
        }
    }

    /// Returns the window size given to new streams.
    #[must_use]
    pub const fn initial_window(&self) -> u32 {
        self.initial_window
    }

    /// Returns the number of streams being tracked.
    #[must_use]
    pub fn stream_count(&self) -> usize {
        self.send.len().max(self.receive.len())
    }

    /// Registers a stream, creating both windows.
    pub fn open(&mut self, stream_id: u32) {
        self.send
            .entry(stream_id)
            .or_insert_with(|| SendWindow::new(self.initial_window));
        self.receive
            .entry(stream_id)
            .or_insert_with(|| ReceiveWindow::new(self.initial_window));
    }

    /// Forgets a stream's windows.
    pub fn close(&mut self, stream_id: u32) {
        self.send.remove(&stream_id);
        self.receive.remove(&stream_id);
    }

    /// Reserves send credit for a stream, registering it if unseen.
    pub fn reserve(&mut self, stream_id: u32, wanted: u32) -> u32 {
        self.open(stream_id);
        self.send
            .get_mut(&stream_id)
            .map_or(0, |window| window.reserve(wanted))
    }

    /// Applies a window update received from the peer.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::WindowOverflow`] if the update would overflow.
    pub fn grant(&mut self, stream_id: u32, increment: u32) -> Result<(), FlowError> {
        self.open(stream_id);
        match self.send.get_mut(&stream_id) {
            Some(window) => window.grant(increment),
            None => Ok(()),
        }
    }

    /// Records data arriving on a stream.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::WindowExceeded`] if the peer overran its window.
    pub fn receive(&mut self, stream_id: u32, len: u32) -> Result<(), FlowError> {
        self.open(stream_id);
        match self.receive.get_mut(&stream_id) {
            Some(window) => window.receive(len),
            None => Ok(()),
        }
    }

    /// Records consumption, returning any update to send to the peer.
    pub fn consume(&mut self, stream_id: u32, len: u32) -> Option<u32> {
        self.receive
            .get_mut(&stream_id)
            .and_then(|window| window.consume(len))
    }

    /// Returns the send credit remaining for a stream.
    #[must_use]
    pub fn send_available(&self, stream_id: u32) -> u32 {
        self.send.get(&stream_id).map_or(0, SendWindow::available)
    }

    /// Returns `true` if a stream cannot currently send.
    #[must_use]
    pub fn is_blocked(&self, stream_id: u32) -> bool {
        self.send
            .get(&stream_id)
            .is_some_and(SendWindow::is_blocked)
    }
}

impl Default for FlowController {
    fn default() -> Self {
        Self::new(DEFAULT_WINDOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_window_grants_within_its_credit() {
        let mut window = SendWindow::new(1000);

        assert_eq!(window.reserve(400), 400);
        assert_eq!(window.available(), 600);
        assert_eq!(window.sent(), 400);
        assert!(!window.is_blocked());
    }

    #[test]
    fn a_send_window_grants_partially_rather_than_refusing() {
        let mut window = SendWindow::new(100);

        // Asking for more than remains yields what is left, not zero.
        assert_eq!(window.reserve(250), 100);
        assert!(window.is_blocked());
        assert_eq!(window.blocked_count(), 1);
        assert_eq!(window.reserve(50), 0);
    }

    #[test]
    fn granting_credit_reopens_a_send_window() {
        let mut window = SendWindow::new(100);
        assert_eq!(window.reserve(100), 100);
        assert!(window.is_blocked());

        window.grant(500).expect("a normal update");
        assert_eq!(window.available(), 500);
        assert!(!window.is_blocked());
    }

    #[test]
    fn an_overflowing_grant_is_rejected() {
        let mut window = SendWindow::new(u32::MAX - 10);
        let err = window.grant(100).expect_err("this would overflow");
        assert!(matches!(err, FlowError::WindowOverflow { increment: 100 }));
    }

    #[test]
    fn a_receive_window_tracks_outstanding_data() {
        let mut window = ReceiveWindow::new(1000);

        window.receive(400).expect("within the window");
        assert_eq!(window.outstanding(), 400);
        assert_eq!(window.available(), 600);
        assert_eq!(window.received(), 400);
    }

    #[test]
    fn overrunning_the_receive_window_is_a_protocol_error() {
        let mut window = ReceiveWindow::new(100);
        window.receive(80).expect("within the window");

        let err = window.receive(50).expect_err("this overruns the window");
        assert!(matches!(err, FlowError::WindowExceeded { excess: 30 }));
    }

    #[test]
    fn consuming_emits_an_update_past_the_threshold() {
        let mut window = ReceiveWindow::with_threshold(1000, 0.5);
        window.receive(1000).expect("fills the window");

        // Below the threshold: no update, to avoid flooding the peer.
        assert_eq!(window.consume(400), None);

        // Crossing it releases the accumulated credit in one update.
        assert_eq!(window.consume(200), Some(600));
        assert_eq!(window.outstanding(), 400);
    }

    #[test]
    fn a_flush_releases_unreported_credit() {
        let mut window = ReceiveWindow::with_threshold(1000, 0.9);
        window.receive(500).expect("within the window");

        assert_eq!(window.consume(100), None, "below the threshold");
        assert_eq!(window.flush_update(), Some(100), "flushed on demand");
        assert_eq!(window.flush_update(), None, "nothing left to report");
    }

    #[test]
    fn one_blocked_stream_does_not_block_the_others() {
        // This is the whole point of per-stream flow control.
        let mut control = FlowController::new(1000);

        for stream_id in [1_u32, 3, 5] {
            control.open(stream_id);
        }

        // Stream 1 exhausts its window entirely.
        assert_eq!(control.reserve(1, 1000), 1000);
        assert!(control.is_blocked(1));
        assert_eq!(control.reserve(1, 100), 0);

        // The others are completely unaffected.
        assert!(!control.is_blocked(3));
        assert!(!control.is_blocked(5));
        assert_eq!(control.reserve(3, 500), 500);
        assert_eq!(control.reserve(5, 1000), 1000);
    }

    #[test]
    fn the_two_halves_form_a_working_loop() {
        let mut sender = FlowController::new(1000);
        let mut receiver = FlowController::new(1000);
        let stream = 1_u32;

        // The sender fills its window.
        let granted = sender.reserve(stream, 1000);
        assert_eq!(granted, 1000);
        assert!(sender.is_blocked(stream));

        // The receiver takes the data in.
        receiver
            .receive(stream, granted)
            .expect("within the window");

        // The application consumes it, producing an update.
        let update = receiver
            .consume(stream, 1000)
            .expect("consuming a full window must report");

        // The update re-opens the sender.
        sender.grant(stream, update).expect("a normal update");
        assert!(!sender.is_blocked(stream));
        assert_eq!(sender.send_available(stream), 1000);
    }

    #[test]
    fn closing_a_stream_releases_its_windows() {
        let mut control = FlowController::new(1000);
        control.open(1);
        control.open(3);
        assert_eq!(control.stream_count(), 2);

        control.close(1);
        assert_eq!(control.stream_count(), 1);
        assert_eq!(
            control.send_available(1),
            0,
            "a closed stream has no credit"
        );
    }

    #[test]
    fn thresholds_are_clamped_to_something_sensible() {
        // A threshold of zero would emit an update per byte.
        let mut window = ReceiveWindow::with_threshold(1000, 0.0);
        window.receive(1000).expect("fills the window");
        assert_eq!(window.consume(1), None, "an update per byte would flood");
    }
}
