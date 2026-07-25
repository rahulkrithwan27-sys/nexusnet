# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

_Nothing yet._

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
