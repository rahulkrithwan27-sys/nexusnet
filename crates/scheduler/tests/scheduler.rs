//! Integration tests: the scheduler's pieces working together.
//!
//! Each module's unit tests exercise one component in isolation. These tests
//! run the whole pipeline — priority queue, shaper, and retry manager under one
//! [`PacketScheduler`] — over simulated time, which is where integration bugs
//! (a retry starving a class, shaping delays compounding with backoff) would
//! actually appear.

use std::time::{Duration, Instant};

use nexusnet_scheduler::{
    Dispatch, PacketScheduler, Priority, RetryPolicy, SchedulerConfig, TrafficShaper,
};

/// Drives the scheduler with simulated time until it goes idle.
///
/// `deliver` decides each packet's fate; returns `(delivered, clock)`.
fn run_to_idle<F>(
    scheduler: &mut PacketScheduler<u64>,
    start: Instant,
    mut deliver: F,
) -> (Vec<u64>, Instant)
where
    F: FnMut(u64, u32) -> bool,
{
    let mut clock = start;
    let mut delivered = Vec::new();

    // A generous bound so a livelock fails the test instead of hanging it.
    for _ in 0..100_000 {
        match scheduler.poll_at(clock) {
            Dispatch::Send(packet) => {
                let id = *packet.payload();
                if deliver(id, packet.attempts()) {
                    scheduler.acknowledge(packet.id());
                    delivered.push(id);
                } else {
                    scheduler.fail(packet, clock);
                }
            }
            Dispatch::Wait { delay } => {
                clock += delay.max(Duration::from_micros(100));
            }
            Dispatch::Idle => return (delivered, clock),
            _ => {}
        }
    }

    panic!("the scheduler never went idle");
}

#[test]
fn everything_is_delivered_on_a_clean_link() {
    let start = Instant::now();
    let mut scheduler: PacketScheduler<u64> = PacketScheduler::new_at(
        SchedulerConfig::new()
            .with_rate(100_000.0)
            .with_retry(RetryPolicy::new().with_jitter(false)),
        start,
    );

    for id in 0..200_u64 {
        let priority = match id % 4 {
            0 => Priority::Critical,
            1 => Priority::Normal,
            2 => Priority::Low,
            _ => Priority::Background,
        };
        scheduler.enqueue(priority, 256, id).expect("accepted");
    }

    let (delivered, _) = run_to_idle(&mut scheduler, start, |_, _| true);

    assert_eq!(delivered.len(), 200, "nothing may be lost on a clean link");
    let metrics = scheduler.metrics();
    assert_eq!(metrics.acknowledged, 200);
    assert_eq!(metrics.dropped, 0);
    assert!((metrics.retransmission_ratio() - 0.0).abs() < f64::EPSILON);
    assert!(metrics.is_idle());
}

#[test]
fn a_lossy_link_recovers_through_retries() {
    let start = Instant::now();
    let mut scheduler: PacketScheduler<u64> = PacketScheduler::new_at(
        SchedulerConfig::new().with_rate(1_000_000.0).with_retry(
            RetryPolicy::new()
                .with_jitter(false)
                .with_initial_delay(Duration::from_millis(5))
                .with_max_attempts(Some(6)),
        ),
        start,
    );

    for id in 0..100_u64 {
        scheduler
            .enqueue(Priority::Normal, 128, id)
            .expect("accepted");
    }

    // Every packet fails its first two attempts and succeeds on the third:
    // deterministic heavy loss, so success depends entirely on the retry path.
    let (mut delivered, _) = run_to_idle(&mut scheduler, start, |_, attempts| attempts >= 3);
    delivered.sort_unstable();

    assert_eq!(
        delivered,
        (0..100).collect::<Vec<_>>(),
        "every packet must eventually arrive"
    );

    let metrics = scheduler.metrics();
    assert_eq!(metrics.dropped, 0);
    assert_eq!(metrics.retries_dispatched, 200, "two retries per packet");
    // 300 sends of 128 bytes, 200 of them retransmissions.
    assert!(
        (metrics.retransmission_ratio() - 2.0 / 3.0).abs() < 0.01,
        "got {}",
        metrics.retransmission_ratio()
    );
}

#[test]
fn a_dead_destination_exhausts_budgets_rather_than_looping() {
    let start = Instant::now();
    let mut scheduler: PacketScheduler<u64> = PacketScheduler::new_at(
        SchedulerConfig::new().with_rate(1_000_000.0).with_retry(
            RetryPolicy::new()
                .with_jitter(false)
                .with_initial_delay(Duration::from_millis(1))
                .with_max_attempts(Some(4)),
        ),
        start,
    );

    for id in 0..50_u64 {
        scheduler
            .enqueue(Priority::Normal, 64, id)
            .expect("accepted");
    }

    // Nothing ever arrives.
    let (delivered, _) = run_to_idle(&mut scheduler, start, |_, _| false);

    assert!(delivered.is_empty());
    let metrics = scheduler.metrics();
    assert_eq!(
        metrics.dropped, 50,
        "every packet is abandoned, none lingers"
    );
    assert!(
        metrics.is_idle(),
        "an undeliverable backlog must still drain"
    );
}

#[test]
fn the_shaper_bounds_throughput_end_to_end() {
    let start = Instant::now();
    // 10 KiB/s with the default one-second burst.
    let mut scheduler: PacketScheduler<u64> =
        PacketScheduler::new_at(SchedulerConfig::new().with_rate(10_240.0), start);

    for id in 0..30_u64 {
        scheduler
            .enqueue(Priority::Normal, 1024, id)
            .expect("accepted");
    }

    let (delivered, finish) = run_to_idle(&mut scheduler, start, |_, _| true);
    assert_eq!(delivered.len(), 30);

    // 30 KiB minus the 10 KiB burst leaves 20 KiB to pay for at 10 KiB/s.
    let elapsed = finish.saturating_duration_since(start);
    assert!(
        elapsed >= Duration::from_millis(1900),
        "30 KiB through a 10 KiB/s shaper cannot finish in {elapsed:?}"
    );
}

#[test]
fn priorities_hold_under_load_with_retries_in_the_mix() {
    let start = Instant::now();
    let mut scheduler: PacketScheduler<u64> = PacketScheduler::new_at(
        SchedulerConfig::new()
            .with_rate(1_000_000.0)
            .with_queue_capacity(600)
            .with_retry(
                RetryPolicy::new()
                    .with_jitter(false)
                    .with_initial_delay(Duration::from_millis(1)),
            ),
        start,
    );

    // Critical ids 0..500, background ids 1000..1500.
    for id in 0..500_u64 {
        scheduler
            .enqueue(Priority::Critical, 64, id)
            .expect("accepted");
        scheduler
            .enqueue(Priority::Background, 64, 1000 + id)
            .expect("accepted");
    }

    // Every fifth packet fails once, so retries interleave with fresh traffic.
    let mut clock = start;
    let mut order = Vec::new();

    while order.len() < 300 {
        match scheduler.poll_at(clock) {
            Dispatch::Send(packet) => {
                let id = *packet.payload();
                if id % 5 == 0 && packet.attempts() == 1 {
                    scheduler.fail(packet, clock);
                } else {
                    scheduler.acknowledge(packet.id());
                    order.push(id);
                }
            }
            Dispatch::Wait { delay } => clock += delay.max(Duration::from_micros(100)),
            Dispatch::Idle => break,
            _ => {}
        }
    }

    let critical = order.iter().filter(|&&id| id < 1000).count();
    let background = order.len() - critical;

    assert!(background > 0, "background must not be starved");
    let ratio = critical as f64 / background as f64;
    assert!(
        ratio > 4.0,
        "critical should dominate under saturation, got {ratio:.1}:1"
    );
}

#[test]
fn a_reserved_class_survives_a_bulk_flood() {
    let start = Instant::now();
    let mut scheduler: PacketScheduler<u64> =
        PacketScheduler::new_at(SchedulerConfig::new().with_rate(10_000.0), start);

    // A fifth of the rate is reserved for critical traffic.
    scheduler.set_shaper(
        TrafficShaper::new_at(10_000.0, start).with_reservation(Priority::Critical, 0.2),
    );

    // Bulk traffic large enough to drain the aggregate burst entirely.
    for id in 0..10_u64 {
        scheduler
            .enqueue(Priority::Background, 1000, id)
            .expect("accepted");
    }
    scheduler
        .enqueue(Priority::Critical, 500, 999)
        .expect("accepted");

    let mut clock = start;
    let mut heartbeat_at = None;

    for _ in 0..10_000 {
        match scheduler.poll_at(clock) {
            Dispatch::Send(packet) => {
                if *packet.payload() == 999 {
                    heartbeat_at = Some(clock);
                }
                scheduler.acknowledge(packet.id());
            }
            Dispatch::Wait { delay } => clock += delay.max(Duration::from_micros(100)),
            Dispatch::Idle => break,
            _ => {}
        }
        if heartbeat_at.is_some() {
            break;
        }
    }

    let sent_at = heartbeat_at.expect("the heartbeat must go out");
    assert!(
        sent_at.saturating_duration_since(start) < Duration::from_millis(500),
        "the reservation should carry the heartbeat past the flood promptly, took {:?}",
        sent_at.saturating_duration_since(start)
    );
}
