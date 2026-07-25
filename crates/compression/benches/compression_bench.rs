//! Criterion benchmarks comparing compression codecs.
//!
//! These measure compress and decompress throughput on realistic text-like
//! data, and print achieved ratios so the speed/size trade-off between codecs
//! is recorded rather than assumed. Only codecs compiled into the current build
//! are measured. Run with `cargo bench -p nexusnet-compression`.
//!
//! The `criterion_group!` macro expands to a public function without a doc
//! comment, so `missing_docs` is allowed for this benchmark-only target.
#![allow(missing_docs)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nexusnet_compression::{compress, decompress, Algorithm, Compressor, Level};

/// Text-like data with realistic redundancy.
fn corpus(size: usize) -> Vec<u8> {
    let sentence =
        b"GET /api/v1/metrics HTTP/1.1\r\nHost: edge.nexusnet.dev\r\nAccept: application/json\r\n\r\n";
    sentence.iter().copied().cycle().take(size).collect()
}

fn real_algorithms() -> Vec<Algorithm> {
    Algorithm::available()
        .into_iter()
        .filter(|a| *a != Algorithm::None)
        .collect()
}

fn bench_compress(c: &mut Criterion) {
    let mut group = c.benchmark_group("compress");
    let data = corpus(64 * 1024);
    group.throughput(Throughput::Bytes(data.len() as u64));

    for algorithm in real_algorithms() {
        for level in [Level::FAST, Level::BALANCED, Level::BEST] {
            group.bench_with_input(
                BenchmarkId::new(format!("{algorithm}"), level.get()),
                &data,
                |b, data| {
                    b.iter(|| {
                        let out = compress(black_box(algorithm), black_box(level), black_box(data))
                            .expect("data compresses");
                        black_box(out)
                    });
                },
            );
        }
    }

    group.finish();
}

fn bench_decompress(c: &mut Criterion) {
    let mut group = c.benchmark_group("decompress");
    let data = corpus(64 * 1024);
    group.throughput(Throughput::Bytes(data.len() as u64));

    for algorithm in real_algorithms() {
        let packed = compress(algorithm, Level::BALANCED, &data).expect("data compresses");
        group.bench_with_input(
            BenchmarkId::from_parameter(algorithm.to_string()),
            &packed,
            |b, packed| {
                b.iter(|| {
                    let out = decompress(black_box(algorithm), black_box(packed), 1 << 20)
                        .expect("data decompresses");
                    black_box(out)
                });
            },
        );
    }

    group.finish();
}

/// The adaptive path on data that is not worth compressing: this is the cost of
/// the policy deciding to skip, which should stay far below a real compression.
fn bench_adaptive_skip(c: &mut Criterion) {
    let mut group = c.benchmark_group("adaptive");

    let tiny = b"ack".to_vec();
    for algorithm in real_algorithms() {
        let compressor = Compressor::new(algorithm);
        group.bench_with_input(
            BenchmarkId::new("skip_small", algorithm.to_string()),
            &tiny,
            |b, data| {
                b.iter(|| {
                    let outcome = compressor.compress(black_box(data)).expect("no failure");
                    black_box(outcome)
                });
            },
        );
    }

    group.finish();
}

/// Not a timing benchmark: reports the ratio each codec achieves.
fn report_ratios(_c: &mut Criterion) {
    let data = corpus(64 * 1024);

    for algorithm in real_algorithms() {
        for level in [Level::FAST, Level::BALANCED, Level::BEST] {
            let packed = compress(algorithm, level, &data).expect("data compresses");
            let ratio = packed.len() as f64 / data.len() as f64;
            println!(
                "ratio: {algorithm} level {} -> {} bytes from {} ({:.2}%)",
                level.get(),
                packed.len(),
                data.len(),
                ratio * 100.0
            );
        }
    }
}

criterion_group!(
    benches,
    bench_compress,
    bench_decompress,
    bench_adaptive_skip,
    report_ratios
);
criterion_main!(benches);
