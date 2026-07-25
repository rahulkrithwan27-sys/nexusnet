# nexusnet-core

Core engine, configuration, error types, and lifecycle primitives for the
NexusNet networking framework. This crate is the base of the workspace and
contains **no networking code**; sibling crates attach to the lifecycle it
defines.

## What's here

- **`Engine` / `EngineBuilder`** — a cheap-to-clone handle with a strict
  lifecycle state machine (`Created → Running → ShuttingDown → Stopped`).
- **`EngineConfig` / `EngineConfigBuilder`** — validated configuration with
  defaults, a fluent builder, `serde` support, and `NEXUSNET_*` environment
  overrides.
- **`Error` / `Result`** — a `thiserror`-based error type; the crate never
  panics or unwraps in normal control flow.
- **`logging`** — structured logging setup over `tracing` / `tracing-subscriber`
  with `full`, `compact`, `pretty`, and `json` formats.

## Example

```rust
use nexusnet_core::{Engine, EngineState, LogLevel};

let engine = Engine::builder()
    .name("gateway")
    .log_level(LogLevel::Info)
    .build()?;

engine.start()?;
assert!(engine.is_running());
engine.shutdown()?;
assert_eq!(engine.state(), EngineState::Stopped);
# Ok::<(), nexusnet_core::Error>(())
```

Run the bundled example:

```bash
cargo run -p nexusnet-core --example engine_lifecycle
```

## Testing & benchmarks

```bash
cargo test  -p nexusnet-core
cargo bench -p nexusnet-core
```

## License

Licensed under the MIT license. See [`LICENSE`](../../LICENSE).
