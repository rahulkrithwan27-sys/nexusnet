//! Criterion benchmarks for the cache and deduplicator.
//!
//! The `criterion_group!` macro expands to a public function without a doc
//! comment, so `missing_docs` is allowed for this benchmark-only target.
#![allow(missing_docs)]

use std::hint::black_box;

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nexusnet_cache::{Deduplicator, Digest, LruCache};

fn bench_cache_ops(c: &mut Criterion) {
    let mut group = c.benchmark_group("lru_cache");

    for size in [128_usize, 4096] {
        group.bench_with_input(BenchmarkId::new("insert", size), &size, |b, &size| {
            b.iter(|| {
                let mut cache: LruCache<u64, Vec<u8>> = LruCache::new(size);
                for i in 0..size as u64 {
                    cache.insert(black_box(i), vec![0_u8; 64]);
                }
                black_box(cache.len())
            });
        });

        let mut warm: LruCache<u64, Vec<u8>> = LruCache::new(size);
        for i in 0..size as u64 {
            warm.insert(i, vec![0_u8; 64]);
        }

        group.bench_with_input(BenchmarkId::new("get_hit", size), &size, |b, &size| {
            let mut key = 0_u64;
            b.iter(|| {
                key = (key + 1) % size as u64;
                black_box(warm.get(&key).is_some())
            });
        });
    }

    // Eviction is the path that used to be O(n): insert well past capacity.
    group.bench_function("insert_with_eviction", |b| {
        b.iter(|| {
            let mut cache: LruCache<u64, Vec<u8>> = LruCache::new(1024);
            for i in 0..8192_u64 {
                cache.insert(black_box(i), vec![0_u8; 32]);
            }
            black_box(cache.len())
        });
    });

    group.finish();
}

fn bench_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("digest");

    for size in [64_usize, 1024, 65536] {
        let data = vec![0xA5_u8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, data| {
            b.iter(|| black_box(Digest::of(black_box(data))));
        });
    }

    group.finish();
}

fn bench_dedup(c: &mut Criterion) {
    let mut group = c.benchmark_group("dedup");
    let payload = Bytes::from(vec![b'x'; 4096]);

    group.throughput(Throughput::Bytes(payload.len() as u64));
    group.bench_function("offer_repeated", |b| {
        let mut dedup = Deduplicator::new(1024);
        dedup.offer(payload.clone());

        b.iter(|| black_box(dedup.offer(black_box(payload.clone())).is_cached()));
    });

    group.finish();
}

criterion_group!(benches, bench_cache_ops, bench_digest, bench_dedup);
criterion_main!(benches);
