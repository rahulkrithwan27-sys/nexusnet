//! Demonstrates building an engine, installing logging, and driving it through
//! its lifecycle.
//!
//! Run with:
//!
//! ```text
//! cargo run -p nexusnet-core --example engine_lifecycle
//! ```
//!
//! Set `NEXUSNET_LOG_LEVEL=debug` (or `RUST_LOG=debug`) to see more detail.

use nexusnet_core::{Engine, LogFormat, LogLevel};

fn main() -> Result<(), nexusnet_core::Error> {
    // Build an engine that owns its logging setup. `install_logging(true)` makes
    // the engine install a global subscriber during `build`.
    let engine = Engine::builder()
        .name("example-node")
        .log_level(LogLevel::Debug)
        .log_format(LogFormat::Full)
        .install_logging(true)
        .apply_env_overrides(true)
        .build()?;

    println!("engine name  : {}", engine.config().name);
    println!("initial state: {}", engine.state());

    engine.start()?;
    println!("after start  : {}", engine.state());

    // In later phases this is where request serving would happen. For the
    // foundation, we immediately begin a graceful shutdown.
    engine.shutdown()?;
    println!("after stop   : {}", engine.state());

    Ok(())
}
