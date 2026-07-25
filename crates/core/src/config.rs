//! Engine configuration.
//!
//! [`EngineConfig`] is the single source of truth for how an [`Engine`] behaves.
//! It can be constructed in three ways, which compose freely:
//!
//! * [`EngineConfig::default`] — sensible production defaults.
//! * [`EngineConfig::builder`] — a fluent [`EngineConfigBuilder`] with
//!   per-field setters and validation on [`build`](EngineConfigBuilder::build).
//! * [`EngineConfig::with_env_overrides`] — apply `NEXUSNET_*` environment
//!   variables on top of an existing configuration.
//!
//! The type derives [`serde::Serialize`] and [`serde::Deserialize`], so loading
//! from TOML or YAML files is a thin layer that later phases can add without
//! changing this module.
//!
//! [`Engine`]: crate::Engine

use std::env;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Prefix applied to every environment variable read by
/// [`EngineConfig::with_env_overrides`].
const ENV_PREFIX: &str = "NEXUSNET_";

/// The verbosity of engine logging.
///
/// This maps onto [`tracing`] levels and is also used to build the default
/// [`tracing_subscriber`] filter directive when the `RUST_LOG` environment
/// variable is not set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum LogLevel {
    /// Extremely verbose, per-operation tracing.
    Trace,
    /// Detailed diagnostic information useful while debugging.
    Debug,
    /// High-level operational messages. This is the default.
    #[default]
    Info,
    /// Recoverable problems that deserve attention.
    Warn,
    /// Errors that abort the operation in progress.
    Error,
    /// Logging disabled entirely.
    Off,
}

impl LogLevel {
    /// Returns the level as a lowercase [`tracing`] filter directive, such as
    /// `"info"` or `"off"`.
    ///
    /// The returned string is suitable for
    /// [`EnvFilter::new`](tracing_subscriber::EnvFilter::new).
    #[must_use]
    pub const fn as_directive(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Off => "off",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_directive())
    }
}

impl FromStr for LogLevel {
    type Err = Error;

    /// Parses a case-insensitive level name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] if `s` is not one of `trace`, `debug`,
    /// `info`, `warn`, `error`, or `off`.
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            "off" | "none" => Ok(Self::Off),
            other => Err(Error::invalid_config(
                "log_level",
                format!("unknown log level `{other}`"),
            )),
        }
    }
}

/// The on-screen format used by the logging subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum LogFormat {
    /// Human-readable, single-line-per-event output. This is the default.
    #[default]
    Full,
    /// Condensed human-readable output with less metadata.
    Compact,
    /// Multi-line, indented output aimed at local development.
    Pretty,
    /// Machine-readable newline-delimited JSON, aimed at log aggregators.
    Json,
}

impl fmt::Display for LogFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Full => "full",
            Self::Compact => "compact",
            Self::Pretty => "pretty",
            Self::Json => "json",
        };
        f.write_str(name)
    }
}

impl FromStr for LogFormat {
    type Err = Error;

    /// Parses a case-insensitive format name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] if `s` is not one of `full`, `compact`,
    /// `pretty`, or `json`.
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "full" => Ok(Self::Full),
            "compact" => Ok(Self::Compact),
            "pretty" => Ok(Self::Pretty),
            "json" => Ok(Self::Json),
            other => Err(Error::invalid_config(
                "log_format",
                format!("unknown log format `{other}`"),
            )),
        }
    }
}

/// Complete, validated configuration for an [`Engine`](crate::Engine).
///
/// Construct one via [`EngineConfig::default`], [`EngineConfig::builder`], or by
/// deserializing it, then optionally layer environment overrides on top with
/// [`with_env_overrides`](EngineConfig::with_env_overrides).
///
/// # Examples
///
/// ```
/// use nexusnet_core::{EngineConfig, LogLevel};
///
/// let config = EngineConfig::builder()
///     .name("edge-node-1")
///     .log_level(LogLevel::Debug)
///     .build()
///     .expect("valid configuration");
///
/// assert_eq!(config.name, "edge-node-1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct EngineConfig {
    /// Human-readable name for this engine instance. Must be non-empty.
    pub name: String,

    /// Minimum severity of log events emitted by the engine.
    pub log_level: LogLevel,

    /// The rendering format used when the engine installs logging.
    pub log_format: LogFormat,

    /// Number of worker threads the async runtime should use.
    ///
    /// `None` means "let the runtime decide" (typically one thread per
    /// available CPU core). When `Some`, the value must be at least `1`.
    pub worker_threads: Option<usize>,

    /// Maximum time a graceful shutdown is allowed to take before callers may
    /// consider it failed. Must be non-zero.
    pub shutdown_timeout: Duration,

    /// Whether building the engine should install a global logging subscriber.
    ///
    /// Defaults to `false` so that the engine never hijacks the host
    /// application's logging setup. Enable it for standalone binaries that want
    /// the engine to own logging.
    pub install_logging: bool,

    /// Whether runtime metrics collection is enabled.
    ///
    /// This is a foundation-level toggle; metric backends are wired up in a
    /// later phase.
    pub metrics_enabled: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            name: "nexusnet".to_owned(),
            log_level: LogLevel::Info,
            log_format: LogFormat::Full,
            worker_threads: None,
            shutdown_timeout: Duration::from_secs(30),
            install_logging: false,
            metrics_enabled: false,
        }
    }
}

impl EngineConfig {
    /// Starts building a configuration with [`EngineConfigBuilder`].
    ///
    /// The builder begins from [`EngineConfig::default`]; every field left
    /// untouched keeps its default value.
    pub fn builder() -> EngineConfigBuilder {
        EngineConfigBuilder::new()
    }

    /// Converts this configuration back into a builder, seeded with the current
    /// field values.
    ///
    /// This is useful for taking an existing configuration and adjusting a
    /// handful of fields without restating the rest.
    pub fn into_builder(self) -> EngineConfigBuilder {
        EngineConfigBuilder { config: self }
    }

    /// Validates the configuration, returning an error describing the first
    /// problem found.
    ///
    /// Validation is intentionally cheap and side-effect free, so it is safe to
    /// call repeatedly.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] when:
    ///
    /// * [`name`](Self::name) is empty or contains only whitespace, or
    /// * [`worker_threads`](Self::worker_threads) is `Some(0)`, or
    /// * [`shutdown_timeout`](Self::shutdown_timeout) is zero.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::invalid_config(
                "name",
                "engine name must not be empty",
            ));
        }

        if self.worker_threads == Some(0) {
            return Err(Error::invalid_config(
                "worker_threads",
                "worker thread count must be at least 1 when set",
            ));
        }

        if self.shutdown_timeout.is_zero() {
            return Err(Error::invalid_config(
                "shutdown_timeout",
                "shutdown timeout must be greater than zero",
            ));
        }

        Ok(())
    }

    /// Returns a copy of this configuration with `NEXUSNET_*` environment
    /// variables applied on top of the current values.
    ///
    /// Recognized variables (all optional):
    ///
    /// * `NEXUSNET_NAME` — [`name`](Self::name).
    /// * `NEXUSNET_LOG_LEVEL` — [`log_level`](Self::log_level).
    /// * `NEXUSNET_LOG_FORMAT` — [`log_format`](Self::log_format).
    /// * `NEXUSNET_WORKER_THREADS` — [`worker_threads`](Self::worker_threads);
    ///   the literal `auto` clears it to `None`.
    /// * `NEXUSNET_SHUTDOWN_TIMEOUT_SECS` —
    ///   [`shutdown_timeout`](Self::shutdown_timeout), in whole seconds.
    /// * `NEXUSNET_INSTALL_LOGGING` — [`install_logging`](Self::install_logging).
    /// * `NEXUSNET_METRICS_ENABLED` — [`metrics_enabled`](Self::metrics_enabled).
    ///
    /// The returned configuration is validated before being handed back.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidEnvVar`] if any recognized variable holds a value
    /// that cannot be parsed, or [`Error::InvalidConfig`] if the resulting
    /// configuration fails [`validate`](Self::validate).
    pub fn with_env_overrides(self) -> Result<Self> {
        let mut config = self;

        if let Some(value) = read_env("NAME") {
            config.name = value;
        }

        if let Some(value) = read_env("LOG_LEVEL") {
            config.log_level = value
                .parse()
                .map_err(|e: Error| Error::invalid_env_var(env_key("LOG_LEVEL"), e.to_string()))?;
        }

        if let Some(value) = read_env("LOG_FORMAT") {
            config.log_format = value
                .parse()
                .map_err(|e: Error| Error::invalid_env_var(env_key("LOG_FORMAT"), e.to_string()))?;
        }

        if let Some(value) = read_env("WORKER_THREADS") {
            config.worker_threads = parse_worker_threads(&value)?;
        }

        if let Some(value) = read_env("SHUTDOWN_TIMEOUT_SECS") {
            let secs: u64 = value.trim().parse().map_err(|_| {
                Error::invalid_env_var(
                    env_key("SHUTDOWN_TIMEOUT_SECS"),
                    "expected a non-negative integer number of seconds",
                )
            })?;
            config.shutdown_timeout = Duration::from_secs(secs);
        }

        if let Some(value) = read_env("INSTALL_LOGGING") {
            config.install_logging = parse_bool(&value, "INSTALL_LOGGING")?;
        }

        if let Some(value) = read_env("METRICS_ENABLED") {
            config.metrics_enabled = parse_bool(&value, "METRICS_ENABLED")?;
        }

        config.validate()?;
        Ok(config)
    }
}

/// Reads the environment variable named `{ENV_PREFIX}{suffix}`, returning `None`
/// if it is unset or empty after trimming.
fn read_env(suffix: &str) -> Option<String> {
    let value = env::var(env_key(suffix)).ok()?;
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Builds the fully qualified environment variable name for a suffix.
fn env_key(suffix: &str) -> String {
    format!("{ENV_PREFIX}{suffix}")
}

/// Parses the worker-thread override, treating `auto` as `None`.
fn parse_worker_threads(value: &str) -> Result<Option<usize>> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    let count: usize = trimmed.parse().map_err(|_| {
        Error::invalid_env_var(
            env_key("WORKER_THREADS"),
            "expected a positive integer or `auto`",
        )
    })?;
    if count == 0 {
        return Err(Error::invalid_env_var(
            env_key("WORKER_THREADS"),
            "worker thread count must be at least 1",
        ));
    }
    Ok(Some(count))
}

/// Parses a permissive boolean from an environment variable value.
fn parse_bool(value: &str, suffix: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(Error::invalid_env_var(
            env_key(suffix),
            "expected a boolean (`true`/`false`, `1`/`0`, `yes`/`no`, `on`/`off`)",
        )),
    }
}

/// A fluent builder for [`EngineConfig`].
///
/// Obtain one with [`EngineConfig::builder`]. Each setter consumes and returns
/// the builder so calls can be chained; [`build`](Self::build) validates the
/// accumulated configuration.
#[derive(Debug, Clone)]
#[must_use = "an EngineConfigBuilder does nothing until `.build()` is called"]
pub struct EngineConfigBuilder {
    config: EngineConfig,
}

impl EngineConfigBuilder {
    /// Creates a builder seeded with [`EngineConfig::default`].
    fn new() -> Self {
        Self {
            config: EngineConfig::default(),
        }
    }

    /// Sets the human-readable [`name`](EngineConfig::name) of the engine.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.config.name = name.into();
        self
    }

    /// Sets the minimum [`log_level`](EngineConfig::log_level).
    pub fn log_level(mut self, level: LogLevel) -> Self {
        self.config.log_level = level;
        self
    }

    /// Sets the [`log_format`](EngineConfig::log_format).
    pub fn log_format(mut self, format: LogFormat) -> Self {
        self.config.log_format = format;
        self
    }

    /// Sets an explicit [`worker_threads`](EngineConfig::worker_threads) count.
    pub fn worker_threads(mut self, count: usize) -> Self {
        self.config.worker_threads = Some(count);
        self
    }

    /// Clears the worker-thread override so the runtime chooses automatically.
    pub fn auto_worker_threads(mut self) -> Self {
        self.config.worker_threads = None;
        self
    }

    /// Sets the graceful [`shutdown_timeout`](EngineConfig::shutdown_timeout).
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.config.shutdown_timeout = timeout;
        self
    }

    /// Sets whether the engine installs a global logging subscriber on build.
    pub fn install_logging(mut self, enabled: bool) -> Self {
        self.config.install_logging = enabled;
        self
    }

    /// Sets whether runtime metrics collection is enabled.
    pub fn metrics_enabled(mut self, enabled: bool) -> Self {
        self.config.metrics_enabled = enabled;
        self
    }

    /// Validates and returns the accumulated [`EngineConfig`].
    ///
    /// # Errors
    ///
    /// Returns any error produced by [`EngineConfig::validate`].
    pub fn build(self) -> Result<EngineConfig> {
        self.config.validate()?;
        Ok(self.config)
    }
}

impl Default for EngineConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = EngineConfig::default();
        assert!(config.validate().is_ok());
        assert_eq!(config.name, "nexusnet");
        assert_eq!(config.log_level, LogLevel::Info);
        assert_eq!(config.log_format, LogFormat::Full);
        assert_eq!(config.worker_threads, None);
        assert_eq!(config.shutdown_timeout, Duration::from_secs(30));
        assert!(!config.install_logging);
        assert!(!config.metrics_enabled);
    }

    #[test]
    fn builder_overrides_fields() {
        let config = EngineConfig::builder()
            .name("edge")
            .log_level(LogLevel::Debug)
            .log_format(LogFormat::Json)
            .worker_threads(4)
            .shutdown_timeout(Duration::from_secs(5))
            .install_logging(true)
            .metrics_enabled(true)
            .build()
            .expect("configuration is valid");

        assert_eq!(config.name, "edge");
        assert_eq!(config.log_level, LogLevel::Debug);
        assert_eq!(config.log_format, LogFormat::Json);
        assert_eq!(config.worker_threads, Some(4));
        assert_eq!(config.shutdown_timeout, Duration::from_secs(5));
        assert!(config.install_logging);
        assert!(config.metrics_enabled);
    }

    #[test]
    fn empty_name_is_rejected() {
        let err = EngineConfig::builder()
            .name("   ")
            .build()
            .expect_err("empty name must be rejected");
        assert!(matches!(err, Error::InvalidConfig { field: "name", .. }));
    }

    #[test]
    fn zero_worker_threads_is_rejected() {
        let err = EngineConfig::builder()
            .worker_threads(0)
            .build()
            .expect_err("zero worker threads must be rejected");
        assert!(matches!(
            err,
            Error::InvalidConfig {
                field: "worker_threads",
                ..
            }
        ));
    }

    #[test]
    fn zero_shutdown_timeout_is_rejected() {
        let err = EngineConfig::builder()
            .shutdown_timeout(Duration::ZERO)
            .build()
            .expect_err("zero timeout must be rejected");
        assert!(matches!(
            err,
            Error::InvalidConfig {
                field: "shutdown_timeout",
                ..
            }
        ));
    }

    #[test]
    fn into_builder_roundtrips() {
        let original = EngineConfig::builder()
            .name("roundtrip")
            .worker_threads(2)
            .build()
            .unwrap();
        let rebuilt = original.clone().into_builder().build().unwrap();
        assert_eq!(original, rebuilt);
    }

    #[test]
    fn log_level_parses_case_insensitively() {
        assert_eq!("TRACE".parse::<LogLevel>().unwrap(), LogLevel::Trace);
        assert_eq!("Warning".parse::<LogLevel>().unwrap(), LogLevel::Warn);
        assert_eq!("off".parse::<LogLevel>().unwrap(), LogLevel::Off);
        assert!("verbose".parse::<LogLevel>().is_err());
    }

    #[test]
    fn log_format_parses_case_insensitively() {
        assert_eq!("JSON".parse::<LogFormat>().unwrap(), LogFormat::Json);
        assert_eq!("pretty".parse::<LogFormat>().unwrap(), LogFormat::Pretty);
        assert!("xml".parse::<LogFormat>().is_err());
    }

    #[test]
    fn log_level_directives_are_lowercase() {
        assert_eq!(LogLevel::Info.as_directive(), "info");
        assert_eq!(LogLevel::Off.as_directive(), "off");
        assert_eq!(LogLevel::Warn.to_string(), "warn");
    }

    #[test]
    fn parse_worker_threads_handles_auto_and_numbers() {
        assert_eq!(parse_worker_threads("auto").unwrap(), None);
        assert_eq!(parse_worker_threads("AUTO").unwrap(), None);
        assert_eq!(parse_worker_threads("8").unwrap(), Some(8));
        assert!(parse_worker_threads("0").is_err());
        assert!(parse_worker_threads("-1").is_err());
        assert!(parse_worker_threads("many").is_err());
    }

    #[test]
    fn parse_bool_accepts_common_spellings() {
        for truthy in ["1", "true", "TRUE", "yes", "on"] {
            assert!(parse_bool(truthy, "INSTALL_LOGGING").unwrap());
        }
        for falsy in ["0", "false", "No", "off"] {
            assert!(!parse_bool(falsy, "INSTALL_LOGGING").unwrap());
        }
        assert!(parse_bool("maybe", "INSTALL_LOGGING").is_err());
    }
}
