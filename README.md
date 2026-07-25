<!-- markdownlint-disable MD013 -->
# NexusNet

**NexusNet** is an open-source, high-performance networking framework written in
Rust. Its long-term goal is an adaptive network-optimization engine that reduces
bandwidth, improves latency, and performs intelligent packet optimization across
many transports and protocols, with SDKs for multiple languages.

> **Status: Phase 1 — Workspace & Foundation.**
> This phase establishes the workspace, the core engine lifecycle, configuration,
> error handling, and logging. It deliberately contains **no networking code
> yet**; each subsystem is scaffolded as its own crate and implemented in a later
> phase. See [`docs/roadmap.md`](docs/roadmap.md).

## Highlights (this phase)

- A clean Cargo **workspace** with one crate per subsystem.
- A fully implemented, tested **core**: `Engine` lifecycle, validated
  `EngineConfig` (builder + `serde` + `NEXUSNET_*` env overrides), a
  `thiserror`-based error type, and structured `tracing` logging.
- **Strict quality gates**: `rustfmt`, Clippy with `-D warnings`, unit +
  integration + doc tests, and a Criterion benchmark harness.
- A dependency-light **`nexusnet` CLI** for inspecting build and config state.

## Workspace layout

```text
nexusnet/
├── crates/
│   ├── core/          # Engine, config, errors, logging  (implemented)
│   ├── transport/     # TCP/UDP/QUIC/WebSocket/HTTP2/HTTP3 (scaffold)
│   ├── compression/   # Zstd/Gzip/Brotli, adaptive/streaming (scaffold)
│   ├── serializer/    # MessagePack/Protobuf/JSON/binary (scaffold)
│   ├── encryption/    # ChaCha20-Poly1305/AES-GCM/TLS (scaffold)
│   ├── cache/         # LRU/TTL, delta sync, dedup (scaffold)
│   ├── scheduler/     # priority, shaping, rate limiting (scaffold)
│   ├── analytics/     # bandwidth/latency/RTT/jitter stats (scaffold)
│   ├── optimizer/     # adaptive, model-driven optimization (scaffold)
│   ├── protocol/      # wire format & framing (scaffold)
│   ├── router/        # routing & load balancing (scaffold)
│   ├── telemetry/     # metrics/trace export (scaffold)
│   ├── plugin_api/    # extension traits (scaffold)
│   └── cli/           # `nexusnet` binary (implemented)
├── sdk/               # flutter / python / cpp bindings (later phases)
├── dashboard/         # web dashboard backend/frontend (later phases)
├── docs/              # architecture & roadmap
├── examples/          # workspace-level examples (per-crate examples live in each crate)
├── tests/             # cross-crate end-to-end tests (later phases)
└── benches/           # workspace-level benchmarks (per-crate benches live in each crate)
```

Each subsystem crate is published under the `nexusnet-*` name (e.g.
`nexusnet-core`) and its library is imported as `nexusnet_core`.

## Quick start

```rust
use nexusnet_core::{Engine, EngineState, LogLevel};

fn main() -> Result<(), nexusnet_core::Error> {
    let engine = Engine::builder()
        .name("gateway")
        .log_level(LogLevel::Info)
        .build()?;

    engine.start()?;
    assert!(engine.is_running());
    engine.shutdown()?;
    assert_eq!(engine.state(), EngineState::Stopped);
    Ok(())
}
```

Run the bundled example and the CLI:

```bash
cargo run -p nexusnet-core --example engine_lifecycle
cargo run -p nexusnet-cli -- info      # honors NEXUSNET_* env vars
```

## Building, testing, and linting

```bash
# Build everything
cargo build --workspace

# Run all tests (unit + integration + doc tests)
cargo test --workspace

# Formatting (check only)
cargo fmt --all -- --check

# Lint with warnings treated as errors (the CI gate)
cargo clippy --workspace --all-targets -- -D warnings

# Benchmarks
cargo bench -p nexusnet-core
```

The toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml); the
crate's minimum supported Rust version (MSRV) is **1.75**.

## Configuration

`EngineConfig` can be built fluently, deserialized (`serde`), or layered with
environment overrides:

| Variable                        | Field              | Notes                              |
| ------------------------------- | ------------------ | ---------------------------------- |
| `NEXUSNET_NAME`                 | `name`             | non-empty                          |
| `NEXUSNET_LOG_LEVEL`            | `log_level`        | `trace`…`error`, `off`             |
| `NEXUSNET_LOG_FORMAT`           | `log_format`       | `full`/`compact`/`pretty`/`json`   |
| `NEXUSNET_WORKER_THREADS`       | `worker_threads`   | positive integer, or `auto`        |
| `NEXUSNET_SHUTDOWN_TIMEOUT_SECS`| `shutdown_timeout` | whole seconds                      |
| `NEXUSNET_INSTALL_LOGGING`      | `install_logging`  | boolean                            |
| `NEXUSNET_METRICS_ENABLED`      | `metrics_enabled`  | boolean                            |

## Documentation

- [Architecture overview](docs/architecture.md)
- [Roadmap](docs/roadmap.md)
- [Contributing](CONTRIBUTING.md) · [Code of Conduct](CODE_OF_CONDUCT.md) · [Security policy](SECURITY.md)

## License

Licensed under the [MIT License](LICENSE).
