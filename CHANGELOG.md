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
