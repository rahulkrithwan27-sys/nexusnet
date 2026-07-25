//! Criterion benchmarks for the optimizer's hot paths.
//!
//! These run on every delivery or acknowledgement in a busy sender, so their
//! cost is paid millions of times an hour.
#![allow(missing_docs)]

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use nexusnet_optimizer::{
    BandwidthEstimator, CongestionDetector, CongestionWindow, NetworkOptimizer, RttEstimator,
    TrendPredictor,
};

fn bench_estimators(c: &mut Criterion) {
    let mut group = c.benchmark_group("estimators");
    group.throughput(Throughput::Elements(1));

    group.bench_function("bandwidth_sample", |b| {
        let mut estimator = BandwidthEstimator::new();
        b.iter(|| estimator.sample(black_box(4096), Duration::from_millis(10)));
    });

    group.bench_function("rtt_sample", |b| {
        let mut estimator = RttEstimator::new();
        b.iter(|| estimator.sample(black_box(Duration::from_millis(40))));
    });

    group.finish();
}

fn bench_network_optimizer(c: &mut Criterion) {
    let mut group = c.benchmark_group("network_optimizer");
    group.throughput(Throughput::Elements(1));

    group.bench_function("record_delivery", |b| {
        let mut optimizer = NetworkOptimizer::new();
        b.iter(|| optimizer.record_delivery(black_box(4096), Duration::from_millis(10)));
    });

    group.bench_function("plan", |b| {
        let mut optimizer = NetworkOptimizer::new();
        for _ in 0..50 {
            optimizer.record_delivery(1024 * 1024, Duration::from_secs(1));
            optimizer.record_rtt(Duration::from_millis(50));
            optimizer.record_loss(99, 1);
        }
        b.iter(|| black_box(optimizer.plan()));
    });

    group.finish();
}

fn bench_congestion(c: &mut Criterion) {
    let mut group = c.benchmark_group("congestion");
    group.throughput(Throughput::Elements(1));

    group.bench_function("detector_observe", |b| {
        let mut detector = CongestionDetector::new();
        b.iter(|| black_box(detector.observe(black_box(Duration::from_millis(30)))));
    });

    group.bench_function("window_on_ack", |b| {
        let mut window = CongestionWindow::new(1400);
        b.iter(|| window.on_ack(black_box(1400)));
    });

    group.finish();
}

fn bench_predictor(c: &mut Criterion) {
    let mut group = c.benchmark_group("predictor");
    group.throughput(Throughput::Elements(1));

    group.bench_function("record", |b| {
        let start = Instant::now();
        let mut predictor = TrendPredictor::new();
        let mut tick = 0_u64;
        b.iter(|| {
            tick += 1;
            predictor.record(black_box(tick as f64), start + Duration::from_millis(tick));
        });
    });

    group.bench_function("forecast_over_full_window", |b| {
        let start = Instant::now();
        let mut predictor = TrendPredictor::new();
        for tick in 0..16_u64 {
            predictor.record(1000.0 + tick as f64, start + Duration::from_secs(tick));
        }
        let now = start + Duration::from_secs(16);
        b.iter(|| black_box(predictor.forecast(black_box(now))));
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_estimators,
    bench_network_optimizer,
    bench_congestion,
    bench_predictor
);
criterion_main!(benches);
