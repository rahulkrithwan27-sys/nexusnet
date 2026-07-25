# NexusNet Roadmap

This roadmap maps the planned phases to the crates they deliver. It is a plan of
intent, not a commitment to dates; phases may be reordered as the design firms
up. The trade-offs of any architectural change will be documented in the pull
request that makes it.

## Phase 1 — Workspace & Foundation ✅

- Cargo workspace, shared metadata, and strict lint policy.
- `nexusnet-core`: engine lifecycle, configuration, errors, logging.
- `nexusnet-cli`: `nexusnet` binary (`version`, `info`).
- Scaffolding for every subsystem crate.
- CI: formatting, Clippy (`-D warnings`), tests, build.

## Phase 2 — Encoding foundations

- `nexusnet-protocol`: wire format, framing, version negotiation.
- `nexusnet-serializer`: MessagePack, Protocol Buffers, JSON, binary; schema
  validation.
- `nexusnet-compression`: Zstd, Gzip, Brotli; adaptive and streaming modes;
  compression benchmarking.

## Phase 3 — Transport

- `nexusnet-transport`: TCP and UDP first, then QUIC, WebSocket, HTTP/2, and
  HTTP/3; connection pooling, automatic reconnect, and multiplexing.
- Async runtime integration wired into the core engine lifecycle.

## Phase 4 — Security

- `nexusnet-encryption`: ChaCha20-Poly1305 and AES-GCM; TLS via `rustls`; key
  rotation, nonce management, and secure handshake orchestration.

## Phase 5 — Data movement & flow control

- `nexusnet-cache`: LRU/TTL caches, delta synchronization, deduplication, and a
  memory/disk tiering strategy.
- `nexusnet-scheduler`: priority queueing, traffic shaping, rate limiting,
  bandwidth estimation, adaptive sending, and retry management.

## Phase 6 — Observability & routing

- `nexusnet-analytics`: bandwidth, compression ratio, packet loss, latency, RTT,
  jitter, CPU/RAM, and connection statistics.
- `nexusnet-router`: route resolution, path selection, load balancing, and
  health-aware failover.
- `nexusnet-telemetry`: metrics export, distributed tracing integration, and
  dashboard feeds.

## Phase 7 — Adaptive optimization

- `nexusnet-optimizer`: bandwidth and congestion prediction; adaptive packet
  sizing, compression level, retry timing, and quality; predictive scheduling.

## Phase 8 — Extensibility & SDKs

- `nexusnet-plugin-api`: stable extension traits, registration, and discovery.
- SDKs under `sdk/`: Python (via `PyO3`/FFI), C++ (via a C ABI), and Flutter;
  plus a REST API surface.
- `dashboard/`: web dashboard backend and frontend.

## Cross-cutting, ongoing

- Benchmarks (Criterion) for every performance-sensitive path.
- Documentation: per-crate READMEs, architecture notes, and examples.
- Security review as each networking and cryptographic component lands.
