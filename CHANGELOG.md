# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`nexusnet-protocol` (Phase 2).** The NexusNet wire format.
  - Fixed 16-byte big-endian frame header with magic, protocol version, frame
    type, flags, reserved bits, stream id, and payload length.
  - `FrameType` (data, control, ping, pong, handshake, close) and `FrameFlags`
    (compressed, encrypted, end-of-stream); unknown types and undefined flag
    bits are rejected rather than ignored.
  - `Encoder` for batching outbound frames into a single write, and `Decoder`,
    an incremental codec that handles frames split across reads or batched into
    one read.
  - Payload lengths are bounds-checked against a configurable maximum
    (16 MiB by default) before any payload memory is allocated.
  - `ProtocolVersion` and `negotiate` for major/minor capability agreement.
  - 39 unit and integration tests, 5 doc tests, and Criterion benchmarks for
    encode, decode, and chunked stream decoding.

- **`nexusnet-serializer` (Phase 2).** Payload serialization over `serde`.
  - `Format` (MessagePack, JSON) with stable wire discriminants, media types,
    `FromStr`, and `negotiate` for content agreement.
  - `encode`/`decode`, plus `decode_with_limit`, which rejects oversized input
    before parsing so a deserializer cannot be induced to allocate against it.
  - Formats are independent cargo features; `Format::is_available` keeps
    negotiation from selecting a compiled-out format, and building with no
    format feature is a compile error rather than a runtime failure.
  - 17 tests across the full feature matrix, plus Criterion benchmarks that
    report encoded size alongside throughput.

- **`nexusnet-compression` (Phase 2).** Adaptive payload compression.
  - `Algorithm` (none, gzip, deflate, brotli, zstd) with stable wire
    discriminants; gzip/deflate/brotli are pure Rust and enabled by default,
    zstd is opt-in because it requires a C toolchain.
  - `Level`, an abstract 0-100 scale mapped onto each backend's native range
    (gzip 0-9, brotli 0-11, zstd 1-22).
  - `Compressor` applies an adaptive policy: payloads below a size threshold are
    skipped, and compressed results are kept only if they actually shrank past a
    configurable ratio, which correctly declines already-compressed data.
  - `Outcome` reports whether compression was applied, mapping onto the
    protocol's compressed frame flag, plus achieved ratio and bytes saved.
  - Decompression enforces a maximum output size during decoding, rejecting
    decompression bombs before their output is materialized.
  - 24 tests passing across all five feature combinations, plus Criterion
    benchmarks reporting ratio alongside throughput.

- **`nexusnet-transport` (Phase 3).** TCP and UDP transports over Tokio.
  - `Connection<S>`, a framed connection generic over any async stream, pairing
    the protocol codec with real I/O; `send`, `send_all` for batched writes,
    `recv`, and `shutdown`.
  - `TcpListener` and `tcp::connect`, the latter bounded by a configurable
    connect timeout; `TcpStream` nodelay is enabled by default since framed
    protocols gain nothing from Nagle's algorithm.
  - `UdpEndpoint` carrying one frame per datagram, rejecting oversized
    datagrams rather than silently truncating them.
  - `TransportConfig` for payload limits, read buffer size, datagram limits,
    connect timeout, and socket options.
  - A clean close at a frame boundary reports end-of-stream; a close mid-frame
    is `UnexpectedEof`, so truncation is never mistaken for a normal close.
    `Error::is_fatal` distinguishes connection-invalidating errors.
  - 25 tests: framing over in-memory pipes and over real loopback sockets,
    including 500 variable-length frames arriving in order, concurrent clients,
    payload-limit enforcement, connect timeout, and malformed datagrams.

- **Connection pooling and reconnection (Phase 3).**
  - `ConnectionPool` reuses connections to a peer, bounded by an idle-connection
    limit and an idle-time window; connections idle past that window are
    discarded rather than handed out, since peers and middleboxes drop idle
    connections silently.
  - `PooledConnection` returns itself to the pool on drop, but detects fatal
    errors and clean peer closes automatically and drops those connections
    instead, so a desynchronized stream is never handed to another caller.
  - `PoolStats` reports connections created, reused, discarded, and expired,
    plus a reuse ratio.
  - `ReconnectPolicy` and `connect_with_retry` implement exponential backoff
    with equal jitter, capped delays, and optional attempt limits; jitter
    decorrelates clients so a recovering service is not hammered in lockstep.
    Only connection establishment is retried, never a mid-session failure.
  - 22 further tests covering reuse, capacity, expiry, broken-connection
    rejection, backoff bounds, jitter distribution, and recovery once a server
    becomes reachable.

- **Stream multiplexing (Phase 3).**
  - `Session::start` splits a connection into a clonable `SessionHandle` and a
    `SessionDriver`; only the driver touches the socket, routing inbound frames
    to their stream and serializing outbound frames, so there is no locking on
    the hot path.
  - `Stream` carries one logical conversation, with `send`, `recv`, and `close`;
    closing signals end-of-stream to the peer, and dropping a stream frees its
    slot.
  - Stream identifiers use role parity, as in HTTP/2 and QUIC: clients allocate
    odd identifiers, servers even, and `0` is reserved for control frames, so
    the two peers can allocate concurrently without negotiation or collision.
  - `Connection::split` yields `ConnectionReader` and `ConnectionWriter`, so
    reads and writes can proceed from separate tasks; both share one
    implementation with `Connection` rather than duplicating the codec logic.
  - Configurable stream limits, per-stream and session-wide buffers, and
    automatic ping response; `SessionStats` reports streams opened, accepted,
    closed, and frames dropped.
  - 13 further tests, including three interleaved streams each seeing only
    their own payloads in order, and eight concurrent streams over one real
    socket.
  - Known limitation: per-stream buffers bound memory but a stalled consumer
    blocks the whole session (head-of-line blocking). Credit-window flow control
    is deferred to the scheduler phase.

- **Server and engine lifecycle integration (Phase 3).**
  - `Server` accepts connections while its engine runs, starting the engine if
    needed and shutting it down before returning, so the engine's lifecycle
    brackets the server's exactly.
  - `Handler` is implemented automatically for closures returning a future;
    handler errors stay with the handler and a failed `accept` is logged rather
    than terminating the server.
  - `ServerConfig` caps simultaneous connections, with excess connections closed
    immediately rather than queued in a backlog; shutdown grace defaults to the
    engine's own `shutdown_timeout`.
  - `ServerHandle` requests shutdown; `ServerStats` reports accepted, rejected,
    active, peak, and abandoned connections.
  - `nexusnet-transport` now depends on `nexusnet-core`, never the reverse, so
    the core engine stays free of networking dependencies.
  - A runnable `echo_server` example exercises the whole stack end to end.
  - 12 further tests covering lifecycle bracketing, connection limits, graceful
    drain, and the bounded grace period.

- **`nexusnet-cache` (Phase 5).** Caching and content deduplication.
  - `LruCache` bounds both entry count and total bytes, since neither limit
    alone is sufficient for payloads whose sizes vary by orders of magnitude;
    supports per-entry and default expiry, with lazy collection.
  - Eviction uses an ordered index rather than scanning for the least recently
    used entry, and expiry sweeps are skipped when no entry carries a TTL;
    together these improved the eviction path by 11.7x.
  - `Deduplicator` and `DedupStore` provide content-addressed deduplication,
    replacing repeat payloads with a 16-byte digest reference; payloads below
    64 bytes are never deduplicated, and an unresolvable reference reports a
    miss so the sender can resend.
  - `Digest` is a fast 128-bit non-cryptographic hash, documented as unsuitable
    for deduplication across a trust boundary.
  - 31 tests including order-index and TTL-counter consistency under churn,
    plus Criterion benchmarks.

- **`nexusnet-scheduler` (Phase 5).** Traffic scheduling.
  - `PriorityQueue` uses deficit round robin across five classes rather than
    strict priority, so urgent traffic wins without starving anything else; a
    test asserts background traffic still progresses at roughly the configured
    16:1 ratio under saturation. Capacity is per class, so low-priority floods
    cannot crowd out critical traffic.
  - `TokenBucket` provides rate limiting with a burst allowance; every method
    has an `_at` variant taking an explicit instant, so behavior is tested by
    driving the clock rather than sleeping.
  - `FlowController`, `SendWindow`, and `ReceiveWindow` implement per-stream
    credit windows, the mechanism that removes head-of-line blocking from a
    multiplexed session. Send and receive are distinct types since conflating
    the directions is the classic flow-control bug, and a peer overrunning its
    window is reported as a protocol violation rather than congestion.
- **`nexusnet-optimizer` (Phase 7).** Adaptive optimization.
  - `BandwidthEstimator` and `RttEstimator` provide exponentially weighted
    estimates; the retransmission timeout follows Jacobson/Karels, using RTT
    variation rather than the average alone so a jittery link does not produce
    a hair-trigger timeout.
  - `Optimizer` returns payload size from the bandwidth-delay product,
    compression level from link speed, retry timeout from RTT, and bytes in
    flight, along with a confidence flag so callers can ignore advice drawn from
    too few samples.
  - The crate produces advice only; it sends nothing and depends on no other
    NexusNet crate, keeping policy separable from mechanism.
  - 61 further tests across both crates.

- **`nexusnet-scheduler` completion (Phase 5).**
  - `PacketScheduler` unifies priority queueing, traffic shaping, and retry
    management behind a single `poll_at` call; it performs no I/O and takes its
    clock from the caller, so all timing behaviour is deterministically tested.
  - `TrafficShaper` adds an aggregate rate with optional per-class reservations,
    so bulk traffic cannot consume the budget a heartbeat needs; admission is
    all-or-nothing so refusals never deduct credit.
  - `RetryManager` and `RetryPolicy` schedule retransmissions with backed-off,
    jittered delays, held in a due-time heap so release is O(log n) and ties
    break by scheduling order.
  - `SchedulerMetrics` reports an immutable snapshot: packet and byte counters
    split between first sends and retransmissions, shaping delay, and pending,
    in-flight, and awaiting-retry depths.
  - A packet deferred by the rate limiter keeps its place; a packet larger than
    the burst capacity is dropped rather than blocking the queue; and `Wait`
    returns the sooner of the shaping and retry deadlines.
  - 98 tests in the crate, up from 38.

- **Phase 6: observability and routing.**
  - `nexusnet-analytics`: `Histogram` records distributions in 64 fixed
    logarithmic buckets, so memory is constant regardless of runtime, with
    percentiles, exact min/max/mean, merging, and heavy-tail detection.
    `RateMeter` reports smoothed and lifetime throughput; `ConnectionStats`
    tracks bytes, frames, errors, loss, latency, and jitter derived from
    consecutive round-trip measurements.
  - `nexusnet-router`: `Router` selects endpoints by round robin, weighted round
    robin, or least connections (normalized by weight), withdrawing endpoints
    that fail repeatedly. `HealthTracker` implements a circuit breaker where a
    withdrawn endpoint is restored only after a cooldown and a successful probe,
    and a failed probe restarts the cooldown. `select` returns `None` on total
    exhaustion rather than routing to a known-dead backend.
  - `nexusnet-telemetry`: `MetricsRegistry` holds counters, gauges, and
    histograms in stable name order, with Prometheus text and JSON exporters
    that escape their output and perform no I/O.
  - 97 further tests across the three crates.

- **Phase 7 completion: congestion prediction and predictive scheduling.**
  - `CongestionDetector` infers congestion from round-trip time rising above the
    path minimum, reporting `Queueing` before any packet is dropped; loss
    remains handled as an unambiguous fallback. `queueing_delay` reports the
    latency recoverable by slowing down.
  - `CongestionWindow` applies additive increase and multiplicative decrease
    with slow start, cautious recovery, and a timeout path that collapses the
    window; it never reaches zero, which would stall a connection permanently.
  - `TrendPredictor` fits a least-squares line over a bounded sample window and
    reports direction, extrapolated value, and a coefficient-of-determination
    confidence, so a trend drawn through noise is not acted on.
  - `advise_send` turns a forecast into scheduling advice: degrading conditions
    move work earlier, improving conditions defer bulk work, urgent work is
    never deferred, and observed congestion overrides the forecast.
  - 46 further tests in the crate.

- **Flow control wired into the multiplexer.** `Session` now enforces
  per-stream credit windows: `Stream::send` waits when its window is exhausted,
  `Stream::recv` returns credit to the peer via `Control` frames, and a stalled
  consumer blocks only its own stream — the previously documented head-of-line
  limitation is resolved and covered by four tests, including a sustained
  64-payload transfer through a 4 KiB window. A window overrun by the peer is a
  protocol violation that tears the session down; a payload larger than the
  window is rejected immediately rather than deadlocking.
- **Integration tests and benchmarks for `nexusnet-scheduler` and
  `nexusnet-optimizer`.** Six scheduler scenarios drive the full pipeline over
  simulated time (clean link, deterministic heavy loss, dead destination,
  end-to-end rate bounding, fairness under load with retries interleaved, and a
  reservation surviving a bulk flood); five optimizer scenarios follow one link
  through healthy, queueing, degraded, and recovered states asserting every
  component agrees. Criterion benchmarks cover both crates' hot paths — all
  under 120 ns per operation.

- **Enum defaults now derived.** `NetworkQuality`, `Priority`, and `Strategy`
  replace manual `Default` implementations with `#[derive(Default)]` and a
  `#[default]` variant attribute, satisfying `clippy::derivable_impls` on newer
  toolchains. Tests pin each default variant, since a misplaced attribute would
  silently change behaviour rather than fail to compile.

- **`scripts/check-lints.py`.** Catches the lints that newer clippy releases
  reject but the MSRV toolchain misses — orphaned doc comments, byte-char
  arrays, and derivable enum `Default` implementations — using only Python, and
  reports every occurrence rather than stopping at the first. Wired into CI
  alongside `cargo clippy`, not instead of it.

- **`nexusnet-plugin-api` (Phase 8).** Extension points for third-party code.
  - `Plugin` provides the lifecycle trait; `PluginRegistry` registers, loads,
    and unloads, isolating failures so one misconfigured plugin cannot prevent
    the others from loading, and removing a plugin even when its own cleanup
    fails.
  - `ApiVersion` refuses plugins built against an incompatible API: majors must
    match and a plugin may target an older minor but never a newer one, since a
    newer one may use extension points this host lacks.
  - `Interceptor` and `InterceptorChain` provide the data-path extension point,
    running in priority order outbound and reverse order inbound so transforming
    pairs unwind correctly; an interceptor may also drop a payload, and errors
    name the interceptor responsible.
  - Runtime loading of shared libraries is deliberately not supported and the
    reasoning is documented: Rust has no stable ABI, so the API version check
    cannot protect against an ABI mismatch.
  - 35 tests including a three-deep nested transform asserting a lossless round
    trip.

- **`nexusnet-encryption` (Phase 4).** Authenticated encryption.
  - `SessionCrypto` derives a per-direction key with HKDF-SHA256, seals and
    opens messages with `ChaCha20-Poly1305` (default) or `AES-256-GCM`, and
    supports rotation.
  - Nonces come only from `NonceSequence`, which counts and refuses to wrap:
    reusing a nonce under one key leaks the authentication key, so sending stops
    instead.
  - Separate keys per direction prevent a message being reflected back at its
    sender and authenticating as the peer's.
  - `ReplayFilter` rejects duplicates over a 64-message window while tolerating
    reordering; authentication is verified before the filter is updated, so a
    forgery cannot poison it into dropping genuine traffic.
  - Decryption failures are deliberately indistinguishable from one another, to
    avoid providing an attacker an oracle.
  - `Key` zeroes itself on drop, compares in constant time, and redacts itself
    from `Debug` output.
  - The crate builds on the 1.75 MSRV; `zeroize` is pinned to 1.8.2 and its
    derive macro avoided, keeping the dependency surface small.
  - 43 tests covering tamper detection, associated-data binding, cross-cipher
    rejection, replay, reordering, and rotation.

- **TLS wired into the transport.** A new optional `tls` feature on
  `nexusnet-transport` adds `connect_tls`, `connect_tls_default`, and
  `TlsListener`, which return the ordinary framed `Connection` running over an
  authenticated TLS 1.3 session. The feature is off by default, so plain builds
  remain on the 1.75 MSRV; enabling it requires 1.85. Because `Connection<S>` is
  generic over any `AsyncRead + AsyncWrite` stream, the integration is a thin
  convenience layer rather than new protocol code. Includes an end-to-end
  example and integration tests (hosted in `nexusnet-tls`, since they exercise
  both crates and need the 1.85 toolchain), one asserting that an untrusted
  server certificate prevents a framed connection from forming at all.

- **Removed the global MSRV override in `clippy.toml`.** It hard-coded `1.75`,
  which conflicted with the 1.85 `nexusnet-tls` crate and produced a
  "MSRV differs" warning on every TLS compilation unit. Clippy reads each
  crate's `rust-version` from its `Cargo.toml` automatically, so per-crate
  values are now authoritative.

- **Mutual TLS (client-certificate authentication).** `TlsConfigBuilder::build_server_with_client_auth` requires each connecting client to present a certificate chaining to a supplied trust store, and `build_client_with_cert` presents one. A client with no certificate or an untrusted one is rejected at the handshake; an empty client-trust store is refused at build time. Four integration tests cover the success path and each rejection.

### Changed

- Toolchain pin moved from `1.83.0` to `1.97.1` and `rust-src` added to the
  pinned components; the older pin predated current `rust-analyzer` proc-macro
  ABI expectations and crashed the language server. MSRV is unchanged at 1.75
  and is still enforced by a dedicated CI job.

## [0.1.0] - 2026-07-23

### Added

- **Workspace foundation (Phase 1).**
  - Cargo workspace with shared package metadata, dependency versions, and a
    strict lint policy (`[workspace.lints]`).
  - `nexusnet-core`: `Engine` and `EngineBuilder` with a strict lifecycle state
    machine (`Created → Running → ShuttingDown → Stopped`); validated
    `EngineConfig` and `EngineConfigBuilder` with defaults, `serde` support, and
    `NEXUSNET_*` environment overrides; a `thiserror`-based `Error` type and
    `Result` alias; structured logging over `tracing`/`tracing-subscriber`.
  - `nexusnet-cli`: a `nexusnet` binary exposing `version` and `info` commands.
  - Scaffolding crates for transport, compression, serializer, encryption,
    cache, scheduler, analytics, optimizer, protocol, router, telemetry, and
    plugin API.
  - Unit, integration, and documentation tests, plus a Criterion benchmark
    harness for `nexusnet-core`.
  - Project documentation, GitHub issue/PR templates, and a CI workflow running
    formatting, Clippy, tests, and build.

[Unreleased]: https://github.com/nexusnet/nexusnet/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/nexusnet/nexusnet/releases/tag/v0.1.0
