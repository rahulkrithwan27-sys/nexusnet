//! The [`Engine`] and its lifecycle.
//!
//! In this foundation phase the engine owns configuration and a lifecycle state
//! machine; it deliberately performs no networking. Later phases attach the
//! transport, scheduler, and optimizer subsystems to the same lifecycle hooks
//! established here.
//!
//! # Lifecycle
//!
//! An engine moves through a small, strictly ordered set of states:
//!
//! ```text
//!  Created ──start()──▶ Running ──shutdown()──▶ ShuttingDown ─▶ Stopped
//! ```
//!
//! Illegal transitions (starting twice, shutting down before starting, or using
//! an already-stopped engine) return a typed [`Error`] instead of panicking.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::config::EngineConfig;
use crate::error::{Error, Result};
use crate::logging;

/// The lifecycle state of an [`Engine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EngineState {
    /// The engine has been built but not yet started.
    Created,
    /// The engine is running and ready to serve work.
    Running,
    /// A graceful shutdown is in progress.
    ShuttingDown,
    /// The engine has stopped and is terminal.
    Stopped,
}

impl fmt::Display for EngineState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::ShuttingDown => "shutting-down",
            Self::Stopped => "stopped",
        };
        f.write_str(name)
    }
}

/// Shared inner state behind an [`Engine`] handle.
///
/// Kept behind an [`Arc`] so cloning an [`Engine`] is cheap and every clone
/// observes the same lifecycle state.
#[derive(Debug)]
struct EngineInner {
    config: EngineConfig,
    state: Mutex<EngineState>,
}

/// The central NexusNet engine handle.
///
/// [`Engine`] is a cheap-to-clone handle over shared state: cloning yields
/// another handle to the *same* underlying engine, not a new engine. Build one
/// with [`Engine::builder`] or [`Engine::new`].
///
/// # Examples
///
/// ```
/// use nexusnet_core::{Engine, EngineState};
///
/// let engine = Engine::builder()
///     .name("example")
///     .build()
///     .expect("valid configuration");
///
/// assert_eq!(engine.state(), EngineState::Created);
/// engine.start().expect("engine starts");
/// assert_eq!(engine.state(), EngineState::Running);
/// engine.shutdown().expect("engine shuts down");
/// assert_eq!(engine.state(), EngineState::Stopped);
/// ```
#[derive(Debug, Clone)]
pub struct Engine {
    inner: Arc<EngineInner>,
}

impl Engine {
    /// Starts building an engine with an [`EngineBuilder`].
    pub fn builder() -> EngineBuilder {
        EngineBuilder::new()
    }

    /// Builds an engine from the given configuration.
    ///
    /// This is a convenience for `Engine::builder().config(config).build()`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] if `config` fails validation, or
    /// [`Error::LoggingInit`] if the configuration requests logging
    /// installation and a subscriber cannot be installed.
    pub fn with_config(config: EngineConfig) -> Result<Self> {
        Self::builder().config(config).build()
    }

    /// Builds an engine using [`EngineConfig::default`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::LoggingInit`] only if the default configuration is
    /// changed to install logging and a subscriber cannot be installed; the
    /// default configuration itself always validates successfully.
    pub fn new() -> Result<Self> {
        Self::builder().build()
    }

    /// Constructs the engine from an already-validated configuration, applying
    /// side effects (such as logging installation) that a builder requested.
    fn assemble(config: EngineConfig) -> Result<Self> {
        if config.install_logging {
            logging::init(&config)?;
        }

        let engine = Self {
            inner: Arc::new(EngineInner {
                config,
                state: Mutex::new(EngineState::Created),
            }),
        };

        tracing::debug!(
            engine.name = %engine.inner.config.name,
            "engine assembled"
        );

        Ok(engine)
    }

    /// Returns a reference to the engine's configuration.
    #[must_use]
    pub fn config(&self) -> &EngineConfig {
        &self.inner.config
    }

    /// Returns the current lifecycle [`EngineState`].
    #[must_use]
    pub fn state(&self) -> EngineState {
        *self.state_guard()
    }

    /// Returns `true` if the engine is currently [`Running`](EngineState::Running).
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.state() == EngineState::Running
    }

    /// Transitions the engine from [`Created`](EngineState::Created) to
    /// [`Running`](EngineState::Running).
    ///
    /// # Errors
    ///
    /// * [`Error::AlreadyRunning`] if the engine is already running or in the
    ///   middle of shutting down.
    /// * [`Error::AlreadyShutDown`] if the engine has already stopped.
    pub fn start(&self) -> Result<()> {
        let mut state = self.state_guard();
        match *state {
            EngineState::Created => {
                *state = EngineState::Running;
                drop(state);
                tracing::info!(engine.name = %self.inner.config.name, "engine started");
                Ok(())
            }
            EngineState::Running | EngineState::ShuttingDown => Err(Error::AlreadyRunning),
            EngineState::Stopped => Err(Error::AlreadyShutDown),
        }
    }

    /// Gracefully shuts the engine down, moving it to
    /// [`Stopped`](EngineState::Stopped).
    ///
    /// The state advances through [`ShuttingDown`](EngineState::ShuttingDown)
    /// for the duration of the call so concurrent observers can see that a
    /// shutdown is underway.
    ///
    /// # Errors
    ///
    /// * [`Error::NotRunning`] if the engine has not been started.
    /// * [`Error::AlreadyShutDown`] if the engine has already stopped.
    pub fn shutdown(&self) -> Result<()> {
        let mut state = self.state_guard();
        match *state {
            EngineState::Running => {
                *state = EngineState::ShuttingDown;
                // The shutdown work of later subsystems will run here while the
                // state reflects `ShuttingDown`; the lock is intentionally held
                // so no other transition can interleave.
                *state = EngineState::Stopped;
                drop(state);
                tracing::info!(engine.name = %self.inner.config.name, "engine stopped");
                Ok(())
            }
            EngineState::Created => Err(Error::NotRunning),
            EngineState::ShuttingDown | EngineState::Stopped => Err(Error::AlreadyShutDown),
        }
    }

    /// Acquires the state lock, recovering the guard if a previous holder
    /// panicked.
    ///
    /// The critical sections guarded by this lock cannot panic, so poisoning is
    /// not expected in practice; recovering the guard keeps this method
    /// panic-free regardless.
    fn state_guard(&self) -> MutexGuard<'_, EngineState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// A fluent builder for [`Engine`].
///
/// The builder wraps an [`EngineConfigBuilder`](crate::EngineConfigBuilder), so
/// per-field setters and validation live in exactly one place. Obtain a builder
/// with [`Engine::builder`].
///
/// # Examples
///
/// ```
/// use nexusnet_core::{Engine, LogLevel};
///
/// let engine = Engine::builder()
///     .name("router-a")
///     .log_level(LogLevel::Warn)
///     .apply_env_overrides(true)
///     .build()
///     .expect("valid configuration");
/// # let _ = engine;
/// ```
#[derive(Debug, Clone)]
#[must_use = "an EngineBuilder does nothing until `.build()` is called"]
pub struct EngineBuilder {
    inner: crate::config::EngineConfigBuilder,
    apply_env: bool,
}

impl EngineBuilder {
    /// Creates a builder seeded with [`EngineConfig::default`].
    fn new() -> Self {
        Self {
            inner: EngineConfig::builder(),
            apply_env: false,
        }
    }

    /// Replaces the in-progress configuration with `config`.
    ///
    /// Any setters called before this are discarded; setters called afterwards
    /// override individual fields of `config`.
    pub fn config(mut self, config: EngineConfig) -> Self {
        self.inner = config.into_builder();
        self
    }

    /// Sets the engine [`name`](EngineConfig::name).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.inner = self.inner.name(name);
        self
    }

    /// Sets the [`log_level`](EngineConfig::log_level).
    pub fn log_level(mut self, level: crate::LogLevel) -> Self {
        self.inner = self.inner.log_level(level);
        self
    }

    /// Sets the [`log_format`](EngineConfig::log_format).
    pub fn log_format(mut self, format: crate::LogFormat) -> Self {
        self.inner = self.inner.log_format(format);
        self
    }

    /// Sets an explicit [`worker_threads`](EngineConfig::worker_threads) count.
    pub fn worker_threads(mut self, count: usize) -> Self {
        self.inner = self.inner.worker_threads(count);
        self
    }

    /// Sets the graceful [`shutdown_timeout`](EngineConfig::shutdown_timeout).
    pub fn shutdown_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.inner = self.inner.shutdown_timeout(timeout);
        self
    }

    /// Sets whether the engine installs a global logging subscriber on build.
    pub fn install_logging(mut self, enabled: bool) -> Self {
        self.inner = self.inner.install_logging(enabled);
        self
    }

    /// Sets whether runtime metrics collection is enabled.
    pub fn metrics_enabled(mut self, enabled: bool) -> Self {
        self.inner = self.inner.metrics_enabled(enabled);
        self
    }

    /// Controls whether `NEXUSNET_*` environment variables are applied on top of
    /// the configured values during [`build`](Self::build).
    ///
    /// Defaults to `false`.
    pub fn apply_env_overrides(mut self, enabled: bool) -> Self {
        self.apply_env = enabled;
        self
    }

    /// Validates the configuration, optionally applies environment overrides,
    /// and constructs the [`Engine`].
    ///
    /// # Errors
    ///
    /// * [`Error::InvalidConfig`] if the configuration fails validation.
    /// * [`Error::InvalidEnvVar`] if environment overrides are enabled and a
    ///   recognized variable holds an unparseable value.
    /// * [`Error::LoggingInit`] if logging installation was requested and a
    ///   subscriber cannot be installed.
    pub fn build(self) -> Result<Engine> {
        let config = self.inner.build()?;
        let config = if self.apply_env {
            config.with_env_overrides()?
        } else {
            config
        };
        Engine::assemble(config)
    }
}

impl Default for EngineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_engine() -> Engine {
        Engine::builder()
            .name("test")
            .build()
            .expect("configuration is valid")
    }

    #[test]
    fn new_engine_starts_in_created_state() {
        let engine = test_engine();
        assert_eq!(engine.state(), EngineState::Created);
        assert!(!engine.is_running());
    }

    #[test]
    fn full_lifecycle_transitions_are_ordered() {
        let engine = test_engine();
        engine.start().expect("start succeeds from Created");
        assert_eq!(engine.state(), EngineState::Running);
        assert!(engine.is_running());
        engine.shutdown().expect("shutdown succeeds from Running");
        assert_eq!(engine.state(), EngineState::Stopped);
        assert!(!engine.is_running());
    }

    #[test]
    fn starting_twice_is_rejected() {
        let engine = test_engine();
        engine.start().unwrap();
        assert!(matches!(engine.start(), Err(Error::AlreadyRunning)));
    }

    #[test]
    fn shutdown_before_start_is_rejected() {
        let engine = test_engine();
        assert!(matches!(engine.shutdown(), Err(Error::NotRunning)));
    }

    #[test]
    fn using_a_stopped_engine_is_rejected() {
        let engine = test_engine();
        engine.start().unwrap();
        engine.shutdown().unwrap();
        assert!(matches!(engine.start(), Err(Error::AlreadyShutDown)));
        assert!(matches!(engine.shutdown(), Err(Error::AlreadyShutDown)));
    }

    #[test]
    fn clones_share_lifecycle_state() {
        let engine = test_engine();
        let clone = engine.clone();
        engine.start().unwrap();
        // The clone observes the same underlying state.
        assert_eq!(clone.state(), EngineState::Running);
    }

    #[test]
    fn config_is_accessible() {
        let engine = Engine::builder().name("named").build().unwrap();
        assert_eq!(engine.config().name, "named");
    }

    #[test]
    fn engine_state_display_is_stable() {
        assert_eq!(EngineState::Created.to_string(), "created");
        assert_eq!(EngineState::Running.to_string(), "running");
        assert_eq!(EngineState::ShuttingDown.to_string(), "shutting-down");
        assert_eq!(EngineState::Stopped.to_string(), "stopped");
    }
}
