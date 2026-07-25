# Examples

Workspace-level, cross-crate examples live here as the framework grows.

Today, runnable examples ship inside the crate they exercise so they can be run
with Cargo's example runner, e.g.:

```bash
cargo run -p nexusnet-core --example engine_lifecycle
```

See [`crates/core/examples/`](../crates/core/examples/).
