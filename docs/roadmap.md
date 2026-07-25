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

## Phase 2 — Encoding foundations ✅

- ✅ `nexusnet-protocol`: wire format, framing, version negotiation.
- ✅ `nexusnet-serializer`: MessagePack and JSON behind independent cargo
  features, with format negotiation and size-limited decoding. Protocol Buffers
  deferred: it needs a `.proto`/codegen workflow that is premature before the
  message shapes settle, and `prost` can be added later without `protoc`.
- ✅ `nexusnet-compression`: gzip, deflate, and brotli in a pure-Rust default
  feature set, with zstd opt-in behind its C dependency. Adaptive policy skips
  small and incompressible payloads, and decompression enforces an output limit
  to reject decompression bombs.

## Phase 3 — Transport 🚧

- ✅ `nexusnet-transport`: TCP and UDP, with a `Connection` type generic over
  any async stream so TLS and QUIC attach to the same framing logic. Bounded
  connect timeouts, configurable payload and datagram limits, and clean-close
  versus truncation distinguished.
- ✅ Connection pooling with idle expiry and broken-connection detection, and
  reconnection with exponentially backed-off, jittered retries.
- ✅ Stream multiplexing with role-based identifier parity, per-stream
  backpressure, and automatic ping response.
- ✅ Async runtime integration: `Server` binds listeners to the engine
  lifecycle, with a connection cap and bounded graceful shutdown.
- QUIC, WebSocket, HTTP/2, and HTTP/3 transports.
- ✅ Per-stream flow control (credit windows) implemented in `nexusnet-scheduler`; wiring it into the multiplexer remains.

## Phase 4 — Security

- `nexusnet-encryption`: ChaCha20-Poly1305 and AES-GCM; TLS via `rustls`; key
  rotation, nonce management, and secure handshake orchestration.

## Phase 5 — Data movement & flow control

- ✅ `nexusnet-cache`: LRU caching with expiry and byte-aware capacity, plus
  content-addressed deduplication. Delta synchronization and disk tiering
  remain.
- ✅ `nexusnet-scheduler`: weighted fair priority queueing, token-bucket rate
  limiting, and per-stream credit-window flow control. Retry management and
  adaptive sending policy remain.

## Phase 6 — Observability & routing

- `nexusnet-analytics`: bandwidth, compression ratio, packet loss, latency, RTT,
  jitter, CPU/RAM, and connection statistics.
- `nexusnet-router`: route resolution, path selection, load balancing, and
  health-aware failover.
- `nexusnet-telemetry`: metrics export, distributed tracing integration, and
  dashboard feeds.

## Phase 7 — Adaptive optimization

- ✅ `nexusnet-optimizer`: bandwidth and round-trip-time estimation with
  adaptive payload sizing, compression level, and retry timing. Congestion
  prediction and predictive scheduling remain.

## Phase 8 — Extensibility & SDKs

- `nexusnet-plugin-api`: stable extension traits, registration, and discovery.
- SDKs under `sdk/`: Python (via `PyO3`/FFI), C++ (via a C ABI), and Flutter;
  plus a REST API surface.
- `dashboard/`: web dashboard backend and frontend.

## Cross-cutting, ongoing

- Benchmarks (Criterion) for every performance-sensitive path.
- Documentation: per-crate READMEs, architecture notes, and examples.
- Security review as each networking and cryptographic component lands.
