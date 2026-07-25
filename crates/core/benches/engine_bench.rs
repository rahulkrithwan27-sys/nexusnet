//! Criterion micro-benchmarks for the core foundation.
//!
//! These establish the benchmarking harness and give a baseline for two hot,
//! allocation-sensitive paths: configuration validation and the engine
//! lifecycle. Run with `cargo bench -p nexusnet-core`.
//!
//! The `criterion_group!` macro expands to a public function without a doc
//! comment, so `missing_docs` is allowed for this benchmark-only target.
#![allow(missing_docs)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use nexusnet_core::{Engine, EngineConfig, LogLevel};

fn bench_config_build(c: &mut Criterion) {
    c.bench_function("config_builder_build", |b| {
        b.iter(|| {
            let config = EngineConfig::builder()
                .name(black_box("bench-node"))
                .log_level(black_box(LogLevel::Debug))
                .worker_threads(black_box(4))
                .build()
                .expect("configuration is valid");
            black_box(config)
        });
    });
}

fn bench_engine_lifecycle(c: &mut Criterion) {
    c.bench_function("engine_start_shutdown", |b| {
        b.iter(|| {
            let engine = Engine::builder()
                .name(black_box("bench-node"))
                .build()
                .expect("configuration is valid");
            engine.start().expect("start");
            engine.shutdown().expect("shutdown");
            black_box(engine)
        });
    });
}

criterion_group!(benches, bench_config_build, bench_engine_lifecycle);
criterion_main!(benches);
