//! Congestion detection and window control.
//!
//! ## Predicting congestion rather than reacting to it
//!
//! Loss-based congestion control waits for a packet to be dropped. By the time
//! that happens the bottleneck queue is already full, which means every packet
//! behind it has been sitting in that queue — latency has already suffered, and
//! the loss is the *symptom*, not the warning.
//!
//! Queues fill before they overflow, and a filling queue shows up as round-trip
//! time rising above the path's minimum. [`CongestionDetector`] watches that
//! ratio, so it reports [`CongestionSignal::Queueing`] while there is still
//! time to slow down. This is the insight behind TCP Vegas and, later, BBR.
//!
//! Loss is still handled, because loss still happens for reasons unrelated to
//! congestion. But it is the fallback, not the primary signal.

use std::time::Duration;

/// How much round-trip time may rise above the path minimum before it counts as
/// queueing.
///
/// A quarter above the floor is a deliberate compromise: tight enough to notice
/// a queue forming, loose enough that ordinary scheduling noise on a fast path
/// does not constantly trip it.
pub const DEFAULT_QUEUEING_THRESHOLD: f64 = 1.25;

/// The smoothing factor applied to observed round-trip times.
const RTT_SMOOTHING: f64 = 0.2;

/// What the detector concluded from the latest measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CongestionSignal {
    /// No sign of congestion.
    None,
    /// Round-trip time is rising above the path minimum: a queue is building.
    ///
    /// This is the predictive signal. Reducing the send rate now avoids the
    /// loss that would otherwise follow.
    Queueing,
    /// A packet was lost. Congestion is no longer a prediction.
    Loss,
}

impl CongestionSignal {
    /// Returns `true` if the sender should reduce its rate.
    #[must_use]
    pub const fn should_back_off(self) -> bool {
        matches!(self, Self::Queueing | Self::Loss)
    }
}

impl std::fmt::Display for CongestionSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::None => "none",
            Self::Queueing => "queueing",
            Self::Loss => "loss",
        };
        f.write_str(name)
    }
}

/// Detects congestion from round-trip time inflation.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use nexusnet_optimizer::{CongestionDetector, CongestionSignal};
///
/// let mut detector = CongestionDetector::new();
///
/// // Establish the path's baseline latency.
/// for _ in 0..20 {
///     detector.observe(Duration::from_millis(20));
/// }
/// assert_eq!(detector.signal(), CongestionSignal::None);
///
/// // Latency climbs well above the floor: a queue is filling.
/// for _ in 0..20 {
///     detector.observe(Duration::from_millis(90));
/// }
/// assert_eq!(detector.signal(), CongestionSignal::Queueing);
/// ```
#[derive(Debug, Clone)]
pub struct CongestionDetector {
    /// The lowest round-trip time seen, approximating the path with no queue.
    min_rtt: Option<f64>,
    /// The smoothed current round-trip time.
    smoothed_rtt: Option<f64>,
    threshold: f64,
    samples: u64,
    queueing_events: u64,
    loss_events: u64,
    last_signal: CongestionSignal,
}

impl CongestionDetector {
    /// Creates a detector with the default threshold.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            min_rtt: None,
            smoothed_rtt: None,
            threshold: DEFAULT_QUEUEING_THRESHOLD,
            samples: 0,
            queueing_events: 0,
            loss_events: 0,
            last_signal: CongestionSignal::None,
        }
    }

    /// Creates a detector with an explicit inflation threshold.
    ///
    /// Clamped to `1.05..=4.0`. A threshold at or below 1.0 would report
    /// queueing constantly, since the smoothed time is rarely exactly the
    /// minimum.
    #[must_use]
    pub fn with_threshold(threshold: f64) -> Self {
        Self {
            threshold: threshold.clamp(1.05, 4.0),
            ..Self::new()
        }
    }

    /// Records a round-trip time measurement and returns the resulting signal.
    pub fn observe(&mut self, rtt: Duration) -> CongestionSignal {
        let seconds = rtt.as_secs_f64();
        if seconds <= 0.0 {
            return self.last_signal;
        }

        self.samples += 1;
        self.min_rtt = Some(self.min_rtt.map_or(seconds, |min| min.min(seconds)));

        self.smoothed_rtt = Some(match self.smoothed_rtt {
            Some(current) => current + RTT_SMOOTHING * (seconds - current),
            None => seconds,
        });

        // Do not cry congestion on a handful of samples; the minimum has not
        // settled yet and almost anything looks inflated against it.
        let signal = if self.samples >= 8 && self.inflation().is_some_and(|r| r >= self.threshold) {
            self.queueing_events += 1;
            CongestionSignal::Queueing
        } else {
            CongestionSignal::None
        };

        self.last_signal = signal;
        signal
    }

    /// Records a lost packet.
    ///
    /// Loss overrides the delay-based signal: it is unambiguous evidence, where
    /// inflation is an inference.
    pub fn observe_loss(&mut self) -> CongestionSignal {
        self.loss_events += 1;
        self.last_signal = CongestionSignal::Loss;

        CongestionSignal::Loss
    }

    /// Returns the most recent signal.
    #[must_use]
    pub const fn signal(&self) -> CongestionSignal {
        self.last_signal
    }

    /// Returns how far the current round-trip time exceeds the path minimum.
    ///
    /// `1.0` means no queueing; `2.0` means latency has doubled. Returns `None`
    /// before any measurement.
    #[must_use]
    pub fn inflation(&self) -> Option<f64> {
        match (self.smoothed_rtt, self.min_rtt) {
            (Some(current), Some(min)) if min > 0.0 => Some(current / min),
            _ => None,
        }
    }

    /// Returns the lowest round-trip time observed.
    #[must_use]
    pub fn min_rtt(&self) -> Option<Duration> {
        self.min_rtt.map(Duration::from_secs_f64)
    }

    /// Returns the estimated time packets are spending queued.
    ///
    /// This is the latency a sender would recover by slowing down, which makes
    /// it the figure worth acting on rather than the raw round-trip time.
    #[must_use]
    pub fn queueing_delay(&self) -> Option<Duration> {
        match (self.smoothed_rtt, self.min_rtt) {
            (Some(current), Some(min)) if current > min => {
                Some(Duration::from_secs_f64(current - min))
            }
            (Some(_), Some(_)) => Some(Duration::ZERO),
            _ => None,
        }
    }

    /// Returns how many times queueing has been detected.
    #[must_use]
    pub const fn queueing_events(&self) -> u64 {
        self.queueing_events
    }

    /// Returns how many losses have been reported.
    #[must_use]
    pub const fn loss_events(&self) -> u64 {
        self.loss_events
    }

    /// Discards all history, as after a route change.
    ///
    /// The minimum is cleared deliberately: a new path has its own floor, and
    /// keeping the old one would make a slower path look permanently congested.
    pub fn reset(&mut self) {
        self.min_rtt = None;
        self.smoothed_rtt = None;
        self.samples = 0;
        self.last_signal = CongestionSignal::None;
    }
}

impl Default for CongestionDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Which phase of congestion control a sender is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CongestionState {
    /// Probing for capacity by growing the window quickly.
    SlowStart,
    /// Near the estimated capacity, growing cautiously.
    CongestionAvoidance,
    /// Backing off after a congestion signal.
    Recovery,
}

impl std::fmt::Display for CongestionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::SlowStart => "slow-start",
            Self::CongestionAvoidance => "congestion-avoidance",
            Self::Recovery => "recovery",
        };
        f.write_str(name)
    }
}

/// The default segment size used for window arithmetic, in bytes.
pub const DEFAULT_SEGMENT: u64 = 1400;

/// Controls how many bytes may be outstanding.
///
/// Follows the additive-increase, multiplicative-decrease shape that TCP uses:
/// grow steadily while things are fine, halve on a congestion signal. The
/// asymmetry is the point — probing upward should be gradual, but backing off
/// must be immediate, because the queue is already filling.
///
/// # Examples
///
/// ```
/// use nexusnet_optimizer::{CongestionState, CongestionWindow};
///
/// let mut window = CongestionWindow::new(1400);
/// assert_eq!(window.state(), CongestionState::SlowStart);
///
/// // Acknowledgements grow the window quickly at first.
/// let initial = window.bytes();
/// window.on_ack(1400);
/// assert!(window.bytes() > initial);
///
/// // A congestion signal halves it and leaves slow start behind.
/// window.on_congestion();
/// assert!(window.bytes() < initial * 2);
/// assert_eq!(window.state(), CongestionState::Recovery);
/// ```
#[derive(Debug, Clone)]
pub struct CongestionWindow {
    window: f64,
    /// The window size at which slow start gives way to cautious growth.
    threshold: f64,
    segment: f64,
    state: CongestionState,
    min_window: f64,
    max_window: f64,
    reductions: u64,
}

impl CongestionWindow {
    /// Creates a window sized for `segment`-byte payloads.
    ///
    /// Starts at ten segments, matching the initial window most modern stacks
    /// use: large enough that a short transfer completes in one round trip,
    /// small enough not to overwhelm a thin path.
    #[must_use]
    pub fn new(segment: u64) -> Self {
        let segment = segment.max(1) as f64;

        Self {
            window: segment * 10.0,
            threshold: f64::MAX,
            segment,
            state: CongestionState::SlowStart,
            min_window: segment * 2.0,
            max_window: segment * 10_000.0,
            reductions: 0,
        }
    }

    /// Returns the current window in bytes.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.window as u64
    }

    /// Returns the current phase.
    #[must_use]
    pub const fn state(&self) -> CongestionState {
        self.state
    }

    /// Returns how many times the window has been reduced.
    #[must_use]
    pub const fn reductions(&self) -> u64 {
        self.reductions
    }

    /// Returns `true` if `len` more bytes may be sent with `in_flight`
    /// outstanding.
    #[must_use]
    pub fn can_send(&self, in_flight: u64, len: u64) -> bool {
        in_flight.saturating_add(len) <= self.bytes()
    }

    /// Returns how many more bytes may be sent right now.
    #[must_use]
    pub fn available(&self, in_flight: u64) -> u64 {
        self.bytes().saturating_sub(in_flight)
    }

    /// Records `bytes` acknowledged, growing the window.
    ///
    /// In slow start the window grows by the amount acknowledged, doubling each
    /// round trip. In congestion avoidance it grows by roughly one segment per
    /// round trip, which is the "additive increase" half of the algorithm.
    pub fn on_ack(&mut self, bytes: u64) {
        if bytes == 0 {
            return;
        }

        let acked = bytes as f64;

        match self.state {
            CongestionState::SlowStart => {
                self.window += acked;

                if self.window >= self.threshold {
                    self.state = CongestionState::CongestionAvoidance;
                }
            }
            CongestionState::CongestionAvoidance | CongestionState::Recovery => {
                // One segment per window's worth of data acknowledged.
                self.window += (self.segment * acked) / self.window;
                self.state = CongestionState::CongestionAvoidance;
            }
        }

        self.window = self.window.clamp(self.min_window, self.max_window);
    }

    /// Reduces the window in response to congestion.
    ///
    /// Halving is the "multiplicative decrease" half. It is deliberately
    /// aggressive: the queue is already building, and a gentle reduction would
    /// keep it building.
    pub fn on_congestion(&mut self) {
        self.threshold = (self.window / 2.0).max(self.min_window);
        self.window = self.threshold;
        self.state = CongestionState::Recovery;
        self.reductions += 1;
    }

    /// Collapses the window after a timeout.
    ///
    /// A timeout is worse evidence than a single loss — it suggests the path
    /// stopped delivering entirely — so the window returns to its minimum and
    /// probing restarts from scratch.
    pub fn on_timeout(&mut self) {
        self.threshold = (self.window / 2.0).max(self.min_window);
        self.window = self.min_window;
        self.state = CongestionState::SlowStart;
        self.reductions += 1;
    }

    /// Applies a congestion signal, choosing the appropriate response.
    pub fn apply(&mut self, signal: CongestionSignal) {
        match signal {
            CongestionSignal::None => {}
            // Queueing is an early warning, so back off before loss occurs.
            CongestionSignal::Queueing | CongestionSignal::Loss => self.on_congestion(),
        }
    }

    /// Resets to the initial window and slow start.
    pub fn reset(&mut self) {
        self.window = self.segment * 10.0;
        self.threshold = f64::MAX;
        self.state = CongestionState::SlowStart;
    }
}

impl Default for CongestionWindow {
    fn default() -> Self {
        Self::new(DEFAULT_SEGMENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsampled_detector_reports_nothing() {
        let detector = CongestionDetector::new();

        assert_eq!(detector.signal(), CongestionSignal::None);
        assert!(detector.inflation().is_none());
        assert!(detector.min_rtt().is_none());
        assert!(detector.queueing_delay().is_none());
    }

    #[test]
    fn a_steady_path_shows_no_congestion() {
        let mut detector = CongestionDetector::new();

        for _ in 0..50 {
            detector.observe(Duration::from_millis(25));
        }

        assert_eq!(detector.signal(), CongestionSignal::None);
        assert_eq!(detector.queueing_events(), 0);

        let inflation = detector.inflation().expect("samples exist");
        assert!(
            (inflation - 1.0).abs() < 0.05,
            "a steady path should show no inflation, got {inflation}"
        );
    }

    #[test]
    fn rising_latency_predicts_congestion_before_loss() {
        let mut detector = CongestionDetector::new();

        for _ in 0..20 {
            detector.observe(Duration::from_millis(20));
        }
        assert_eq!(detector.signal(), CongestionSignal::None);

        // The queue fills; latency climbs well above the floor.
        for _ in 0..30 {
            detector.observe(Duration::from_millis(100));
        }

        assert_eq!(
            detector.signal(),
            CongestionSignal::Queueing,
            "a filling queue must be detected without waiting for a drop"
        );
        assert_eq!(
            detector.loss_events(),
            0,
            "the whole point is predicting before loss occurs"
        );
        assert!(detector.signal().should_back_off());
    }

    #[test]
    fn a_few_samples_do_not_trigger_a_verdict() {
        let mut detector = CongestionDetector::new();

        detector.observe(Duration::from_millis(10));
        let signal = detector.observe(Duration::from_millis(500));

        assert_eq!(
            signal,
            CongestionSignal::None,
            "the minimum has not settled, so almost anything looks inflated"
        );
    }

    #[test]
    fn loss_is_reported_unambiguously() {
        let mut detector = CongestionDetector::new();
        for _ in 0..20 {
            detector.observe(Duration::from_millis(20));
        }

        assert_eq!(detector.observe_loss(), CongestionSignal::Loss);
        assert_eq!(detector.loss_events(), 1);
        assert!(detector.signal().should_back_off());
    }

    #[test]
    fn queueing_delay_is_the_recoverable_latency() {
        let mut detector = CongestionDetector::new();

        for _ in 0..20 {
            detector.observe(Duration::from_millis(20));
        }
        for _ in 0..50 {
            detector.observe(Duration::from_millis(120));
        }

        let delay = detector.queueing_delay().expect("samples exist");
        assert!(
            delay >= Duration::from_millis(70),
            "roughly 100ms of the latency is queue, got {delay:?}"
        );
    }

    #[test]
    fn the_threshold_is_clamped_to_something_usable() {
        // A threshold of 1.0 would report queueing on essentially every sample.
        let detector = CongestionDetector::with_threshold(0.5);
        assert!(detector.threshold >= 1.05);

        let generous = CongestionDetector::with_threshold(100.0);
        assert!(generous.threshold <= 4.0);
    }

    #[test]
    fn resetting_forgets_the_old_path() {
        let mut detector = CongestionDetector::new();
        for _ in 0..20 {
            detector.observe(Duration::from_millis(10));
        }

        detector.reset();

        // A genuinely slower path must not look congested just because the old
        // one was faster.
        for _ in 0..20 {
            detector.observe(Duration::from_millis(200));
        }
        assert_eq!(detector.signal(), CongestionSignal::None);
    }

    #[test]
    fn a_new_window_starts_in_slow_start() {
        let window = CongestionWindow::new(1400);

        assert_eq!(window.state(), CongestionState::SlowStart);
        assert_eq!(window.bytes(), 14_000, "ten segments");
        assert_eq!(window.reductions(), 0);
    }

    #[test]
    fn slow_start_grows_quickly() {
        let mut window = CongestionWindow::new(1000);
        let initial = window.bytes();

        // A full window acknowledged should roughly double it.
        for _ in 0..10 {
            window.on_ack(1000);
        }

        assert!(
            window.bytes() >= initial * 2,
            "slow start should double per round trip, got {} from {initial}",
            window.bytes()
        );
    }

    #[test]
    fn congestion_halves_the_window() {
        let mut window = CongestionWindow::new(1000);
        for _ in 0..20 {
            window.on_ack(1000);
        }

        let before = window.bytes();
        window.on_congestion();

        assert!(
            window.bytes() <= before / 2 + 1,
            "expected roughly half of {before}, got {}",
            window.bytes()
        );
        assert_eq!(window.state(), CongestionState::Recovery);
        assert_eq!(window.reductions(), 1);
    }

    #[test]
    fn growth_is_cautious_after_a_reduction() {
        let mut window = CongestionWindow::new(1000);
        for _ in 0..20 {
            window.on_ack(1000);
        }
        window.on_congestion();

        let after_backoff = window.bytes();
        for _ in 0..10 {
            window.on_ack(1000);
        }
        let grown = window.bytes();

        assert_eq!(window.state(), CongestionState::CongestionAvoidance);
        assert!(grown > after_backoff, "the window should still recover");
        assert!(
            grown < after_backoff * 2,
            "but far more slowly than slow start: {after_backoff} -> {grown}"
        );
    }

    #[test]
    fn a_timeout_collapses_the_window() {
        let mut window = CongestionWindow::new(1000);
        for _ in 0..30 {
            window.on_ack(1000);
        }

        window.on_timeout();

        assert_eq!(
            window.bytes(),
            2000,
            "a timeout suggests the path stopped delivering entirely"
        );
        assert_eq!(window.state(), CongestionState::SlowStart);
    }

    #[test]
    fn the_window_never_collapses_to_nothing() {
        let mut window = CongestionWindow::new(1000);

        for _ in 0..50 {
            window.on_congestion();
        }

        assert!(
            window.bytes() >= 2000,
            "a zero window would stall the connection permanently"
        );
    }

    #[test]
    fn sending_is_gated_by_the_window() {
        let window = CongestionWindow::new(1000);

        // Ten 1000-byte segments, so the window is 10,000 bytes.
        assert_eq!(window.bytes(), 10_000);
        assert!(window.can_send(0, 10_000));
        assert!(!window.can_send(10_000, 10_000));
        assert_eq!(window.available(4000), 6000);
        assert_eq!(
            window.available(20_000),
            0,
            "saturating rather than wrapping"
        );
    }

    #[test]
    fn a_queueing_signal_backs_off_like_a_loss() {
        let mut queueing = CongestionWindow::new(1000);
        let mut lost = CongestionWindow::new(1000);

        for _ in 0..20 {
            queueing.on_ack(1000);
            lost.on_ack(1000);
        }

        queueing.apply(CongestionSignal::Queueing);
        lost.apply(CongestionSignal::Loss);

        assert_eq!(
            queueing.bytes(),
            lost.bytes(),
            "an early warning deserves the same response as the event itself"
        );
    }

    #[test]
    fn no_signal_leaves_the_window_alone() {
        let mut window = CongestionWindow::new(1000);
        let before = window.bytes();

        window.apply(CongestionSignal::None);
        assert_eq!(window.bytes(), before);
    }

    #[test]
    fn zero_length_acknowledgements_are_ignored() {
        let mut window = CongestionWindow::new(1000);
        let before = window.bytes();

        window.on_ack(0);
        assert_eq!(window.bytes(), before);
    }

    #[test]
    fn resetting_restores_the_initial_window() {
        let mut window = CongestionWindow::new(1000);
        window.on_congestion();
        window.reset();

        assert_eq!(window.bytes(), 10_000);
        assert_eq!(window.state(), CongestionState::SlowStart);
    }

    #[test]
    fn signals_and_states_display() {
        assert_eq!(CongestionSignal::Queueing.to_string(), "queueing");
        assert_eq!(CongestionState::SlowStart.to_string(), "slow-start");
        assert!(!CongestionSignal::None.should_back_off());
    }
}
