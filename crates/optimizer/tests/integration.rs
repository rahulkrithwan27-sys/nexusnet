//! Integration tests: the optimizer's components tracking one link together.
//!
//! Unit tests verify each estimator in isolation. These follow a single
//! connection through a whole lifecycle — healthy, congested, degraded,
//! recovered — and assert the components agree with each other at each stage,
//! which is where inconsistencies between them would surface.

use std::time::{Duration, Instant};

use nexusnet_optimizer::{
    advise_send_under_congestion, CongestionDetector, CongestionSignal, CongestionState,
    CongestionWindow, NetworkOptimizer, NetworkQuality, SendAdvice, Trend, TrendPredictor,
};

/// One tick of a simulated link fed into every component at once.
struct Link {
    optimizer: NetworkOptimizer,
    detector: CongestionDetector,
    window: CongestionWindow,
    predictor: TrendPredictor,
    clock: Instant,
}

impl Link {
    fn new() -> Self {
        Self {
            optimizer: NetworkOptimizer::new(),
            detector: CongestionDetector::new(),
            window: CongestionWindow::new(1400),
            predictor: TrendPredictor::new(),
            clock: Instant::now(),
        }
    }

    /// Advances one second with the given conditions.
    fn tick(&mut self, bytes_per_second: u64, rtt: Duration, delivered: u64, lost: u64) {
        self.clock += Duration::from_secs(1);

        self.optimizer
            .record_delivery(bytes_per_second, Duration::from_secs(1));
        self.optimizer.record_rtt(rtt);
        self.optimizer.record_loss(delivered, lost);

        let signal = self.detector.observe(rtt);
        self.window.apply(signal);
        if lost > 0 {
            self.detector.observe_loss();
            self.window.on_congestion();
        } else {
            self.window.on_ack(1400);
        }

        self.predictor.record(bytes_per_second as f64, self.clock);
    }
}

#[test]
fn a_healthy_link_reads_healthy_everywhere() {
    let mut link = Link::new();

    for _ in 0..30 {
        link.tick(16 * 1024 * 1024, Duration::from_millis(15), 100, 0);
    }

    // Every component should agree: nothing is wrong.
    assert_eq!(link.optimizer.quality(), NetworkQuality::Excellent);
    assert!(!link.optimizer.plan().compression.enabled);
    assert_eq!(link.detector.signal(), CongestionSignal::None);
    assert_eq!(link.predictor.trend(link.clock), Trend::Stable);
    assert!(
        link.window.bytes() > 14_000,
        "an uncongested window should have grown past its initial size"
    );
}

#[test]
fn queueing_is_flagged_before_quality_degrades() {
    let mut link = Link::new();

    // Establish a fast baseline.
    for _ in 0..15 {
        link.tick(8 * 1024 * 1024, Duration::from_millis(20), 100, 0);
    }

    // Latency triples — a queue building — but bandwidth and loss are still
    // fine, and 60ms is still a "Good" latency in absolute terms.
    for _ in 0..20 {
        link.tick(8 * 1024 * 1024, Duration::from_millis(60), 100, 0);
    }

    assert_eq!(
        link.detector.signal(),
        CongestionSignal::Queueing,
        "relative inflation must fire before absolute thresholds notice"
    );
    assert!(
        link.optimizer.quality() >= NetworkQuality::Good,
        "graded quality still looks fine — which is exactly why the \
         detector's relative signal is needed, got {}",
        link.optimizer.quality()
    );
    assert!(
        link.detector.queueing_delay().expect("measured") >= Duration::from_millis(25),
        "the recoverable delay should be visible"
    );
}

#[test]
fn a_degrading_link_moves_every_component_in_the_same_direction() {
    let mut link = Link::new();

    for _ in 0..15 {
        link.tick(8 * 1024 * 1024, Duration::from_millis(20), 100, 0);
    }

    // Bandwidth collapses steadily while loss appears.
    let mut rate = 8 * 1024 * 1024_u64;
    for _ in 0..25 {
        rate = (rate as f64 * 0.75) as u64;
        link.tick(rate.max(16 * 1024), Duration::from_millis(300), 92, 8);
    }

    let plan = link.optimizer.plan();
    assert!(plan.quality.is_degraded());
    assert!(plan.compression.enabled, "a scarce link should compress");
    assert!(plan.delta_sync.enabled);

    assert!(
        link.window.reductions() > 0,
        "loss must have shrunk the congestion window"
    );

    let forecast = link.predictor.forecast(link.clock).expect("samples exist");
    assert_eq!(forecast.trend, Trend::Degrading);

    // The scheduling advice under congestion: bulk traffic throttles.
    let advice = advise_send_under_congestion(
        Some(forecast),
        false,
        link.detector.signal().should_back_off(),
    );
    assert_eq!(advice, SendAdvice::Throttle);

    // But urgent traffic still goes out.
    let urgent = advise_send_under_congestion(
        Some(forecast),
        true,
        link.detector.signal().should_back_off(),
    );
    assert!(urgent.should_send());
}

#[test]
fn recovery_is_visible_but_the_history_is_not_erased() {
    let mut link = Link::new();

    // Bad start.
    for _ in 0..25 {
        link.tick(24 * 1024, Duration::from_millis(700), 85, 15);
    }
    assert_eq!(link.optimizer.quality(), NetworkQuality::Critical);
    let shrunken = link.window.bytes();

    // Sustained recovery.
    link.detector.reset(); // The path changed; its floor changed with it.
    for _ in 0..200 {
        link.tick(16 * 1024 * 1024, Duration::from_millis(15), 100, 0);
    }

    let metrics = link.optimizer.metrics();
    assert!(
        metrics.quality > NetworkQuality::Critical,
        "recovery must show, got {}",
        metrics.quality
    );
    assert!(metrics.has_recovered());
    assert_eq!(
        metrics.worst_quality,
        NetworkQuality::Critical,
        "the low-water mark survives"
    );

    assert!(
        link.window.bytes() > shrunken,
        "the window should regrow: {shrunken} -> {}",
        link.window.bytes()
    );
    assert_eq!(link.detector.signal(), CongestionSignal::None);
    assert_eq!(link.window.state(), CongestionState::CongestionAvoidance);
}

#[test]
fn the_plan_and_the_window_give_consistent_send_budgets() {
    let mut link = Link::new();

    for _ in 0..40 {
        link.tick(2 * 1024 * 1024, Duration::from_millis(100), 100, 0);
    }

    let plan = link.optimizer.plan();
    assert!(plan.confident);

    // Bandwidth-delay product: 2 MiB/s * 100ms = ~200 KiB.
    let bdp = 2.0 * 1024.0 * 1024.0 * 0.1;
    let in_flight = plan.in_flight_bytes as f64;
    assert!(
        (in_flight - bdp).abs() < bdp * 0.3,
        "the plan's in-flight budget should approximate the BDP: {in_flight} vs {bdp}"
    );

    // The payload recommendation must fit within that budget many times over,
    // or a single payload would occupy the whole pipe.
    assert!(
        plan.payload_size as u64 * 4 <= plan.in_flight_bytes,
        "payload {} should be a fraction of the in-flight budget {}",
        plan.payload_size,
        plan.in_flight_bytes
    );
}
