//! # NexusNet Core
//!
//! Foundational building blocks for the NexusNet networking framework: the
//! [`Engine`] and its lifecycle, a validated [`EngineConfig`], a shared
//! [`struct@Error`] type, and structured [`logging`] setup.
//!
//! This crate is the base of the workspace. It intentionally contains **no
//! networking code**; transport, compression, scheduling, and the other
//! subsystems live in sibling crates and attach to the lifecycle established
//! here in later phases.
//!
//! ## Quick start
//!
//! ```
//! use nexusnet_core::{Engine, EngineState, LogLevel};
//!
//! // Build an engine with a fluent, validated builder.
//! let engine = Engine::builder()
//!     .name("gateway")
//!     .log_level(LogLevel::Info)
//!     .build()?;
//!
//! assert_eq!(engine.state(), EngineState::Created);
//!
//! // Drive it through its lifecycle.
//! engine.start()?;
//! assert!(engine.is_running());
//! engine.shutdown()?;
//! assert_eq!(engine.state(), EngineState::Stopped);
//! # Ok::<(), nexusnet_core::Error>(())
//! ```
//!
//! ## Configuration
//!
//! [`EngineConfig`] can be built fluently, deserialized, or layered with
//! `NEXUSNET_*` environment overrides. See the [`config`] module for details.
//!
//! ## Error handling
//!
//! All fallible operations return [`Result`], the crate's [`std::result::Result`]
//! alias. The crate never panics or unwraps as part of normal control flow.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc(html_root_url = "https://docs.rs/nexusnet-core/0.1.0")]

mod config;
mod engine;
mod error;
pub mod logging;
mod version;

pub use crate::config::{EngineConfig, EngineConfigBuilder, LogFormat, LogLevel};
pub use crate::engine::{Engine, EngineBuilder, EngineState};
pub use crate::error::{Error, Result};
pub use crate::version::{
    version_string, NAME, VERSION, VERSION_MAJOR, VERSION_MINOR, VERSION_PATCH,
};
