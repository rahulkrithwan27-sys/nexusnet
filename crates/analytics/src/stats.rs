//! Throughput metering and per-connection statistics.

use std::time::{Duration, Instant};

use crate::histogram::{DistributionSummary, Histogram};

/// The default smoothing factor for throughput samples.
pub const DEFAULT_SMOOTHING: f64 = 0.2;

/// Measures throughput in bytes per second.
///
/// Reports both a smoothed rate, which is what to act on, and a lifetime
/// average, which describes the connection as a whole. The two differ during a
/// transition, and that difference is the signal worth watching.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, Instant};
/// use nexusnet_analytics::RateMeter;
///
/// let start = Instant::now();
/// let mut meter = RateMeter::new_at(start);
///
/// meter.record_at(1000, start + Duration::from_secs(1));
/// meter.record_at(1000, start + Duration::from_secs(2));
///
/// assert!(meter.bytes_per_second().expect("samples exist") > 0.0);
/// assert_eq!(meter.total_bytes(), 2000);
/// ```
#[derive(Debug, Clone)]
pub struct RateMeter {
    smoothed: Option<f64>,
    smoothing: f64,
    peak: f64,
    total_bytes: u64,
    started_at: Instant,
    last_sample_at: Instant,
    samples: u64,
}

impl RateMeter {
    /// Creates a meter starting now.
    #[must_use]
    pub fn new() -> Self {
        Self::new_at(Instant::now())
    }

    /// Creates a meter with an explicit starting instant.
    ///
    /// Prefer this in tests, where supplying the clock makes rate calculations
    /// exact rather than dependent on how long the test took to run.
    #[must_use]
    pub fn new_at(now: Instant) -> Self {
        Self {
            smoothed: None,
            smoothing: DEFAULT_SMOOTHING,
            peak: 0.0,
            total_bytes: 0,
            started_at: now,
            last_sample_at: now,
            samples: 0,
        }
    }

    /// Sets the smoothing factor, clamped to `0.01..=1.0`.
    #[must_use]
    pub fn with_smoothing(mut self, smoothing: f64) -> Self {
        self.smoothing = smoothing.clamp(0.01, 1.0);
        self
    }

    /// Records `bytes` transferred as of now.
    pub fn record(&mut self, bytes: u64) {
        self.record_at(bytes, Instant::now());
    }

    /// Records `bytes` transferred as of `now`.
    ///
    /// The interval is measured from the previous sample. A zero interval is
    /// counted toward the total but contributes no rate, since dividing by it
    /// would imply infinite throughput.
    pub fn record_at(&mut self, bytes: u64, now: Instant) {
        self.total_bytes += bytes;
        self.samples += 1;

        let elapsed = now.saturating_duration_since(self.last_sample_at);
        self.last_sample_at = now;

        let seconds = elapsed.as_secs_f64();
        if seconds <= 0.0 || bytes == 0 {
            return;
        }

        let rate = bytes as f64 / seconds;
        self.peak = self.peak.max(rate);

        self.smoothed = Some(match self.smoothed {
            Some(current) => current + self.smoothing * (rate - current),
            None => rate,
        });
    }

    /// Returns the smoothed throughput, if any sample produced a rate.
    #[must_use]
    pub fn bytes_per_second(&self) -> Option<f64> {
        self.smoothed
    }

    /// Returns the highest instantaneous rate observed.
    #[must_use]
    pub const fn peak_bytes_per_second(&self) -> f64 {
        self.peak
    }

    /// Returns the total bytes recorded.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Returns how many samples were recorded.
    #[must_use]
    pub const fn samples(&self) -> u64 {
        self.samples
    }

    /// Returns the average throughput over the meter's whole lifetime.
    #[must_use]
    pub fn average_bytes_per_second(&self, now: Instant) -> f64 {
        let seconds = now.saturating_duration_since(self.started_at).as_secs_f64();

        if seconds <= 0.0 {
            0.0
        } else {
            self.total_bytes as f64 / seconds
        }
    }

    /// Returns how long the meter has been running.
    #[must_use]
    pub fn uptime(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started_at)
    }

    /// Discards all history, restarting from `now`.
    pub fn reset(&mut self, now: Instant) {
        self.smoothed = None;
        self.peak = 0.0;
        self.total_bytes = 0;
        self.started_at = now;
        self.last_sample_at = now;
        self.samples = 0;
    }
}

impl Default for RateMeter {
    fn default() -> Self {
        Self::new()
    }
}

/// A point-in-time view of one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ConnectionSnapshot {
    /// Bytes sent.
    pub bytes_sent: u64,
    /// Bytes received.
    pub bytes_received: u64,
    /// Frames sent.
    pub frames_sent: u64,
    /// Frames received.
    pub frames_received: u64,
    /// Errors observed.
    pub errors: u64,
    /// Packets known to have been lost.
    pub packets_lost: u64,
    /// How long the connection has been open.
    pub uptime: Duration,
    /// The round-trip time distribution.
    pub rtt: DistributionSummary,
    /// The inter-arrival jitter distribution.
    pub jitter: DistributionSummary,
}

impl ConnectionSnapshot {
    /// Returns the total bytes moved in both directions.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.bytes_sent + self.bytes_received
    }

    /// Returns the fraction of packets lost.
    ///
    /// Returns `0.0` when nothing has been sent.
    #[must_use]
    pub fn loss_ratio(&self) -> f64 {
        let attempted = self.frames_sent + self.packets_lost;
        if attempted == 0 {
            0.0
        } else {
            self.packets_lost as f64 / attempted as f64
        }
    }

    /// Returns the mean payload size of sent frames.
    ///
    /// Returns `0.0` when nothing has been sent. A value far below the
    /// configured payload target suggests traffic is being fragmented
    /// needlessly.
    #[must_use]
    pub fn mean_frame_size(&self) -> f64 {
        if self.frames_sent == 0 {
            0.0
        } else {
            self.bytes_sent as f64 / self.frames_sent as f64
        }
    }

    /// Returns `true` if any error has been recorded.
    #[must_use]
    pub const fn has_errors(&self) -> bool {
        self.errors > 0
    }
}

/// Collects statistics for a single connection.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, Instant};
/// use nexusnet_analytics::ConnectionStats;
///
/// let start = Instant::now();
/// let mut stats = ConnectionStats::new_at(start);
///
/// stats.record_sent(1024);
/// stats.record_received(512);
/// stats.record_rtt(Duration::from_millis(40));
///
/// let snapshot = stats.snapshot(start + Duration::from_secs(1));
/// assert_eq!(snapshot.total_bytes(), 1536);
/// assert_eq!(snapshot.frames_sent, 1);
/// ```
#[derive(Debug, Clone)]
pub struct ConnectionStats {
    bytes_sent: u64,
    bytes_received: u64,
    frames_sent: u64,
    frames_received: u64,
    errors: u64,
    packets_lost: u64,
    rtt: Histogram,
    jitter: Histogram,
    /// The previous round-trip time, for computing inter-arrival jitter.
    last_rtt: Option<Duration>,
    send_rate: RateMeter,
    receive_rate: RateMeter,
    started_at: Instant,
}

impl ConnectionStats {
    /// Creates statistics starting now.
    #[must_use]
    pub fn new() -> Self {
        Self::new_at(Instant::now())
    }

    /// Creates statistics with an explicit starting instant.
    #[must_use]
    pub fn new_at(now: Instant) -> Self {
        Self {
            bytes_sent: 0,
            bytes_received: 0,
            frames_sent: 0,
            frames_received: 0,
            errors: 0,
            packets_lost: 0,
            rtt: Histogram::new(),
            jitter: Histogram::new(),
            last_rtt: None,
            send_rate: RateMeter::new_at(now),
            receive_rate: RateMeter::new_at(now),
            started_at: now,
        }
    }

    /// Records a frame of `bytes` sent.
    pub fn record_sent(&mut self, bytes: u64) {
        self.record_sent_at(bytes, Instant::now());
    }

    /// Records a frame of `bytes` sent as of `now`.
    pub fn record_sent_at(&mut self, bytes: u64, now: Instant) {
        self.bytes_sent += bytes;
        self.frames_sent += 1;
        self.send_rate.record_at(bytes, now);
    }

    /// Records a frame of `bytes` received.
    pub fn record_received(&mut self, bytes: u64) {
        self.record_received_at(bytes, Instant::now());
    }

    /// Records a frame of `bytes` received as of `now`.
    pub fn record_received_at(&mut self, bytes: u64, now: Instant) {
        self.bytes_received += bytes;
        self.frames_received += 1;
        self.receive_rate.record_at(bytes, now);
    }

    /// Records a round-trip time measurement.
    ///
    /// Jitter is derived from consecutive measurements, so it becomes available
    /// from the second sample onward.
    pub fn record_rtt(&mut self, rtt: Duration) {
        self.rtt.record(rtt);

        if let Some(previous) = self.last_rtt {
            let delta = if rtt > previous {
                rtt - previous
            } else {
                previous - rtt
            };
            self.jitter.record(delta);
        }

        self.last_rtt = Some(rtt);
    }

    /// Records an error.
    pub fn record_error(&mut self) {
        self.errors += 1;
    }

    /// Records `count` lost packets.
    pub fn record_lost(&mut self, count: u64) {
        self.packets_lost += count;
    }

    /// Returns the send-side throughput meter.
    #[must_use]
    pub const fn send_rate(&self) -> &RateMeter {
        &self.send_rate
    }

    /// Returns the receive-side throughput meter.
    #[must_use]
    pub const fn receive_rate(&self) -> &RateMeter {
        &self.receive_rate
    }

    /// Returns the round-trip time distribution.
    #[must_use]
    pub const fn rtt(&self) -> &Histogram {
        &self.rtt
    }

    /// Returns the jitter distribution.
    #[must_use]
    pub const fn jitter(&self) -> &Histogram {
        &self.jitter
    }

    /// Returns a snapshot as of `now`.
    #[must_use]
    pub fn snapshot(&self, now: Instant) -> ConnectionSnapshot {
        ConnectionSnapshot {
            bytes_sent: self.bytes_sent,
            bytes_received: self.bytes_received,
            frames_sent: self.frames_sent,
            frames_received: self.frames_received,
            errors: self.errors,
            packets_lost: self.packets_lost,
            uptime: now.saturating_duration_since(self.started_at),
            rtt: self.rtt.summary(),
            jitter: self.jitter.summary(),
        }
    }
}

impl Default for ConnectionStats {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start() -> Instant {
        Instant::now()
    }

    #[test]
    fn an_unsampled_meter_reports_no_rate() {
        let meter = RateMeter::new_at(start());

        assert!(meter.bytes_per_second().is_none());
        assert_eq!(meter.total_bytes(), 0);
    }

    #[test]
    fn throughput_is_measured() {
        let now = start();
        let mut meter = RateMeter::new_at(now);

        // 1000 bytes per second, sustained.
        for step in 1..=20_u64 {
            meter.record_at(1000, now + Duration::from_secs(step));
        }

        let rate = meter.bytes_per_second().expect("samples exist");
        assert!(
            (rate - 1000.0).abs() < 50.0,
            "expected ~1000 B/s, got {rate}"
        );
        assert_eq!(meter.total_bytes(), 20_000);
    }

    #[test]
    fn the_peak_rate_is_remembered() {
        let now = start();
        let mut meter = RateMeter::new_at(now);

        meter.record_at(1000, now + Duration::from_secs(1));
        // A brief 10x burst.
        meter.record_at(10_000, now + Duration::from_secs(2));
        meter.record_at(1000, now + Duration::from_secs(3));

        assert!(
            (meter.peak_bytes_per_second() - 10_000.0).abs() < 1.0,
            "the burst should be recorded as the peak"
        );
        assert!(
            meter.bytes_per_second().expect("samples exist") < 5000.0,
            "but smoothing should keep the current rate lower"
        );
    }

    #[test]
    fn a_zero_interval_does_not_imply_infinite_throughput() {
        let now = start();
        let mut meter = RateMeter::new_at(now);

        meter.record_at(1000, now);
        assert!(
            meter.bytes_per_second().is_none(),
            "no time has passed, so no rate can be computed"
        );
        assert_eq!(meter.total_bytes(), 1000, "but the bytes still count");
    }

    #[test]
    fn the_lifetime_average_differs_from_the_smoothed_rate() {
        let now = start();
        let mut meter = RateMeter::new_at(now);

        // Idle for most of the window, then a burst at the end.
        meter.record_at(1000, now + Duration::from_secs(10));

        let average = meter.average_bytes_per_second(now + Duration::from_secs(10));
        assert!(
            (average - 100.0).abs() < 1.0,
            "1000 bytes over 10 seconds is 100 B/s, got {average}"
        );
    }

    #[test]
    fn resetting_restarts_the_meter() {
        let now = start();
        let mut meter = RateMeter::new_at(now);
        meter.record_at(5000, now + Duration::from_secs(1));

        meter.reset(now + Duration::from_secs(2));
        assert_eq!(meter.total_bytes(), 0);
        assert!(meter.bytes_per_second().is_none());
    }

    #[test]
    fn connection_traffic_is_counted_in_both_directions() {
        let now = start();
        let mut stats = ConnectionStats::new_at(now);

        stats.record_sent_at(1000, now + Duration::from_millis(100));
        stats.record_sent_at(500, now + Duration::from_millis(200));
        stats.record_received_at(2000, now + Duration::from_millis(300));

        let snapshot = stats.snapshot(now + Duration::from_secs(1));
        assert_eq!(snapshot.bytes_sent, 1500);
        assert_eq!(snapshot.bytes_received, 2000);
        assert_eq!(snapshot.frames_sent, 2);
        assert_eq!(snapshot.frames_received, 1);
        assert_eq!(snapshot.total_bytes(), 3500);
    }

    #[test]
    fn jitter_is_derived_from_consecutive_measurements() {
        let now = start();
        let mut stats = ConnectionStats::new_at(now);

        stats.record_rtt(Duration::from_millis(50));
        // Jitter needs a pair, so nothing yet.
        assert!(stats.jitter().is_empty());

        stats.record_rtt(Duration::from_millis(70));
        assert_eq!(stats.jitter().count(), 1);

        let summary = stats.snapshot(now).jitter;
        assert!(summary.max.expect("a sample exists") >= Duration::from_millis(15));
    }

    #[test]
    fn a_steady_link_shows_little_jitter() {
        let now = start();
        let mut stats = ConnectionStats::new_at(now);

        for _ in 0..20 {
            stats.record_rtt(Duration::from_millis(50));
        }

        let summary = stats.snapshot(now).jitter;
        assert!(
            summary.max.expect("samples exist") < Duration::from_millis(5),
            "identical measurements should show essentially no jitter"
        );
    }

    #[test]
    fn an_erratic_link_shows_jitter() {
        let now = start();
        let mut stats = ConnectionStats::new_at(now);

        for index in 0..20 {
            let millis = if index % 2 == 0 { 20 } else { 180 };
            stats.record_rtt(Duration::from_millis(millis));
        }

        let summary = stats.snapshot(now).jitter;
        assert!(
            summary.p50.expect("samples exist") >= Duration::from_millis(100),
            "alternating latency should register as large jitter"
        );
    }

    #[test]
    fn errors_and_losses_are_tracked() {
        let now = start();
        let mut stats = ConnectionStats::new_at(now);

        for _ in 0..90 {
            stats.record_sent_at(100, now);
        }
        stats.record_lost(10);
        stats.record_error();

        let snapshot = stats.snapshot(now);
        assert!(snapshot.has_errors());
        assert_eq!(snapshot.packets_lost, 10);
        assert!(
            (snapshot.loss_ratio() - 0.1).abs() < 1e-9,
            "got {}",
            snapshot.loss_ratio()
        );
    }

    #[test]
    fn a_clean_connection_reports_no_loss() {
        let now = start();
        let mut stats = ConnectionStats::new_at(now);
        stats.record_sent_at(100, now);

        let snapshot = stats.snapshot(now);
        assert!((snapshot.loss_ratio() - 0.0).abs() < f64::EPSILON);
        assert!(!snapshot.has_errors());
    }

    #[test]
    fn mean_frame_size_reveals_fragmentation() {
        let now = start();
        let mut stats = ConnectionStats::new_at(now);

        for _ in 0..10 {
            stats.record_sent_at(64, now);
        }

        let snapshot = stats.snapshot(now);
        assert!(
            (snapshot.mean_frame_size() - 64.0).abs() < 1e-9,
            "got {}",
            snapshot.mean_frame_size()
        );
    }

    #[test]
    fn an_idle_connection_divides_safely() {
        let now = start();
        let stats = ConnectionStats::new_at(now);
        let snapshot = stats.snapshot(now);

        assert!((snapshot.mean_frame_size() - 0.0).abs() < f64::EPSILON);
        assert!((snapshot.loss_ratio() - 0.0).abs() < f64::EPSILON);
        assert_eq!(snapshot.uptime, Duration::ZERO);
    }

    #[test]
    fn uptime_is_reported() {
        let now = start();
        let stats = ConnectionStats::new_at(now);

        let snapshot = stats.snapshot(now + Duration::from_secs(42));
        assert_eq!(snapshot.uptime, Duration::from_secs(42));
    }
}
