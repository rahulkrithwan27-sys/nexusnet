//! Structured logging setup built on [`tracing`] and [`tracing_subscriber`].
//!
//! [`init`] installs a process-global subscriber that renders events in the
//! format requested by the [`EngineConfig`]. Filtering honors the standard
//! `RUST_LOG` environment variable when present, falling back to the
//! configured [`log_level`](EngineConfig::log_level) otherwise.
//!
//! Because a process may only have one global subscriber, [`init`] uses
//! `try_init` internally and reports a typed [`Error::LoggingInit`] rather than
//! panicking if a subscriber is already installed.
//!
//! [`EngineConfig`]: crate::EngineConfig
//! [`log_level`]: crate::EngineConfig::log_level

use tracing_subscriber::fmt;
use tracing_subscriber::EnvFilter;

use crate::config::{EngineConfig, LogFormat};
use crate::error::{Error, Result};

/// Builds the [`EnvFilter`] for the subscriber.
///
/// Precedence: an explicit `RUST_LOG` value wins; otherwise the configured
/// [`LogLevel`](crate::LogLevel) is used as the default directive.
fn build_filter(config: &EngineConfig) -> Result<EnvFilter> {
    EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(config.log_level.as_directive()))
        .map_err(|e| Error::LoggingInit(e.to_string()))
}

/// Installs a global [`tracing`] subscriber configured from `config`.
///
/// This should be called at most once per process. It is idempotent only in the
/// sense that a second call returns an error instead of panicking.
///
/// # Errors
///
/// Returns [`Error::LoggingInit`] if the filter directive is invalid or if a
/// global subscriber has already been installed (for example by the host
/// application or a previous call to this function).
///
/// # Examples
///
/// ```
/// use nexusnet_core::{logging, EngineConfig, LogFormat};
///
/// let config = EngineConfig::builder()
///     .log_format(LogFormat::Json)
///     .build()
///     .unwrap();
///
/// // The first call in a fresh process succeeds; here we simply ignore the
/// // result because the test harness may have installed a subscriber already.
/// let _ = logging::init(&config);
/// ```
pub fn init(config: &EngineConfig) -> Result<()> {
    let filter = build_filter(config)?;

    // The concrete subscriber type differs per format, so each arm installs its
    // own fully built subscriber. `try_init` returns `Err` if a global
    // subscriber is already present, which we translate into a typed error.
    let outcome = match config.log_format {
        LogFormat::Full => fmt().with_env_filter(filter).try_init(),
        LogFormat::Compact => fmt().compact().with_env_filter(filter).try_init(),
        LogFormat::Pretty => fmt().pretty().with_env_filter(filter).try_init(),
        LogFormat::Json => fmt().json().with_env_filter(filter).try_init(),
    };

    outcome.map_err(|e| Error::LoggingInit(e.to_string()))
}
