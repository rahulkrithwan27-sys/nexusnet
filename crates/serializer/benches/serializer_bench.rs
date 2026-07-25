//! Criterion benchmarks comparing serialization formats.
//!
//! These measure encode and decode cost, and report encoded size, so the
//! compactness/speed trade-off between MessagePack and JSON is visible rather
//! than assumed. Run with `cargo bench -p nexusnet-serializer`.
//!
//! The `criterion_group!` macro expands to a public function without a doc
//! comment, so `missing_docs` is allowed for this benchmark-only target.
#![allow(missing_docs)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use nexusnet_serializer::{decode, encode, Format};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Metrics {
    node: String,
    region: String,
    rtt_micros: u32,
    jitter_micros: u32,
    packets: u64,
    loss_ratio: f64,
    tags: Vec<String>,
}

fn payload(entries: usize) -> Vec<Metrics> {
    (0..entries)
        .map(|i| Metrics {
            node: format!("edge-{i}"),
            region: "ap-south-1".to_owned(),
            rtt_micros: 8_400 + u32::try_from(i % 1000).unwrap_or(0),
            jitter_micros: 120,
            packets: 1_000_000 + i as u64,
            loss_ratio: 0.0012,
            tags: vec!["primary".to_owned(), "tls".to_owned()],
        })
        .collect()
}

fn bench_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("serializer_encode");

    for entries in [1_usize, 64, 512] {
        let value = payload(entries);
        for format in Format::available() {
            group.bench_with_input(
                BenchmarkId::new(format.to_string(), entries),
                &value,
                |b, value| {
                    b.iter(|| {
                        let bytes =
                            encode(black_box(format), black_box(value)).expect("value serializes");
                        black_box(bytes)
                    });
                },
            );
        }
    }

    group.finish();
}

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("serializer_decode");

    for entries in [1_usize, 64, 512] {
        let value = payload(entries);
        for format in Format::available() {
            let bytes = encode(format, &value).expect("value serializes");
            group.bench_with_input(
                BenchmarkId::new(format.to_string(), entries),
                &bytes,
                |b, bytes| {
                    b.iter(|| {
                        let decoded: Vec<Metrics> =
                            decode(black_box(format), black_box(bytes)).expect("bytes deserialize");
                        black_box(decoded)
                    });
                },
            );
        }
    }

    group.finish();
}

/// Not a timing benchmark: reports encoded size per format so the compactness
/// difference is recorded alongside the speed numbers.
fn report_sizes(_c: &mut Criterion) {
    for entries in [1_usize, 64, 512] {
        let value = payload(entries);
        for format in Format::available() {
            let len = encode(format, &value).expect("value serializes").len();
            println!("encoded size: {format} x{entries} = {len} bytes");
        }
    }
}

criterion_group!(benches, bench_encode, bench_decode, report_sizes);
criterion_main!(benches);
