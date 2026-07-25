//! Criterion benchmarks for the scheduler's hot paths.
#![allow(missing_docs)]

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use nexusnet_scheduler::{
    Dispatch, PacketScheduler, Priority, PriorityQueue, RetryManager, RetryPolicy, SchedulerConfig,
    TokenBucket,
};

fn bench_priority_queue(c: &mut Criterion) {
    let mut group = c.benchmark_group("priority_queue");
    group.throughput(Throughput::Elements(1));

    group.bench_function("enqueue_dequeue_cycle", |b| {
        let mut queue: PriorityQueue<u64> = PriorityQueue::new(1024);
        let mut counter = 0_u64;
        b.iter(|| {
            let priority = Priority::ALL[(counter % 5) as usize];
            counter += 1;
            queue.enqueue(priority, black_box(counter)).expect("space");
            black_box(queue.dequeue())
        });
    });

    group.finish();
}

fn bench_token_bucket(c: &mut Criterion) {
    let mut group = c.benchmark_group("token_bucket");
    group.throughput(Throughput::Elements(1));

    group.bench_function("try_consume", |b| {
        let start = Instant::now();
        let mut bucket = TokenBucket::new_at(1e12, u64::MAX / 2, start);
        let mut tick = 0_u64;
        b.iter(|| {
            tick += 1;
            let now = start + Duration::from_nanos(tick);
            black_box(bucket.try_consume_at(black_box(64), now))
        });
    });

    group.finish();
}

fn bench_retry_manager(c: &mut Criterion) {
    let mut group = c.benchmark_group("retry_manager");
    group.throughput(Throughput::Elements(1));

    group.bench_function("schedule_and_release", |b| {
        let policy = RetryPolicy::new()
            .with_jitter(false)
            .with_initial_delay(Duration::from_millis(1));
        let mut retries: RetryManager<u64> = RetryManager::new(policy);
        let start = Instant::now();
        let mut tick = 0_u64;
        b.iter(|| {
            tick += 1;
            let now = start + Duration::from_millis(tick);
            retries.record_failure(black_box(tick), 1, now);
            black_box(retries.take_due(now + Duration::from_millis(2)).len())
        });
    });

    group.finish();
}

fn bench_packet_scheduler(c: &mut Criterion) {
    let mut group = c.benchmark_group("packet_scheduler");
    group.throughput(Throughput::Elements(1));

    // The full cycle a real sender pays per packet: enqueue, poll, acknowledge.
    group.bench_function("enqueue_poll_ack_cycle", |b| {
        let start = Instant::now();
        let mut scheduler: PacketScheduler<u64> =
            PacketScheduler::new_at(SchedulerConfig::new().with_rate(1e12), start);
        let mut tick = 0_u64;
        b.iter(|| {
            tick += 1;
            let now = start + Duration::from_nanos(tick);
            scheduler
                .enqueue(Priority::Normal, 256, black_box(tick))
                .expect("space");
            match scheduler.poll_at(now) {
                Dispatch::Send(packet) => {
                    scheduler.acknowledge(packet.id());
                }
                _ => unreachable!("an unshaped scheduler always dispatches"),
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_priority_queue,
    bench_token_bucket,
    bench_retry_manager,
    bench_packet_scheduler
);
criterion_main!(benches);
