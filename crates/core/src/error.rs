//! Error handling for the NexusNet core engine.
//!
//! Every fallible operation in this crate returns [`Result`], which is a thin
//! alias over [`std::result::Result`] with the error type fixed to
//! [`struct@Error`]. The error enum is marked `#[non_exhaustive]` so new
//! variants can be added in future releases without breaking downstream
//! `match` expressions.
//!
//! # Design
//!
//! The core crate never panics as part of its normal control flow and never
//! calls [`Result::unwrap`] on a value that could realistically be `Err`.
//! Instead, failures are surfaced as typed [`Error`] variants that callers can
//! inspect and handle programmatically.

use std::result::Result as StdResult;

/// A specialized [`Result`](std::result::Result) type for NexusNet operations.
///
/// This alias is used throughout the crate so that the error type never needs
/// to be spelled out explicitly at call sites.
pub type Result<T> = StdResult<T, Error>;

/// The error type returned by every fallible operation in `nexusnet-core`.
///
/// The enum is `#[non_exhaustive]`; downstream code must include a wildcard arm
/// when matching on it to remain forward-compatible.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The engine was asked to start, but it is already running.
    #[error("engine is already running")]
    AlreadyRunning,

    /// An operation required a running engine, but the engine was not running.
    #[error("engine is not running")]
    NotRunning,

    /// The engine has been shut down and can no longer transition state.
    ///
    /// A shut-down engine is terminal: build a new [`Engine`] to continue.
    ///
    /// [`Engine`]: crate::Engine
    #[error("engine has already been shut down")]
    AlreadyShutDown,

    /// A configuration value failed validation.
    ///
    /// The `field` identifies which configuration key was rejected and
    /// `reason` explains why in human-readable form.
    #[error("invalid configuration for `{field}`: {reason}")]
    InvalidConfig {
        /// The configuration field that failed validation.
        field: &'static str,
        /// A human-readable explanation of the validation failure.
        reason: String,
    },

    /// An environment variable held a value that could not be parsed into the
    /// expected type.
    #[error("environment variable `{variable}` is invalid: {reason}")]
    InvalidEnvVar {
        /// The name of the environment variable that could not be parsed.
        variable: String,
        /// A human-readable explanation of the parse failure.
        reason: String,
    },

    /// The global [`tracing`] subscriber could not be installed.
    ///
    /// This most commonly happens when a subscriber has already been installed
    /// by the host application or by a previous call to
    /// [`logging::init`](crate::logging::init).
    #[error("failed to initialize logging: {0}")]
    LoggingInit(String),
}

impl Error {
    /// Constructs an [`Error::InvalidConfig`] for the given `field` and
    /// `reason`.
    ///
    /// This is a convenience constructor used internally by configuration
    /// validation; it accepts anything convertible into a [`String`] for the
    /// reason so call sites can pass either a literal or an owned message.
    #[must_use]
    pub fn invalid_config(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidConfig {
            field,
            reason: reason.into(),
        }
    }

    /// Constructs an [`Error::InvalidEnvVar`] for the given `variable` and
    /// `reason`.
    #[must_use]
    pub fn invalid_env_var(variable: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidEnvVar {
            variable: variable.into(),
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_config_constructor_sets_fields() {
        let err = Error::invalid_config("name", "must not be empty");
        match err {
            Error::InvalidConfig { field, reason } => {
                assert_eq!(field, "name");
                assert_eq!(reason, "must not be empty");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn invalid_env_var_constructor_sets_fields() {
        let err = Error::invalid_env_var("NEXUSNET_NAME", "bad value");
        match err {
            Error::InvalidEnvVar { variable, reason } => {
                assert_eq!(variable, "NEXUSNET_NAME");
                assert_eq!(reason, "bad value");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn display_messages_are_descriptive() {
        assert_eq!(
            Error::AlreadyRunning.to_string(),
            "engine is already running"
        );
        assert_eq!(Error::NotRunning.to_string(), "engine is not running");
        assert_eq!(
            Error::invalid_config("name", "empty").to_string(),
            "invalid configuration for `name`: empty"
        );
    }
}
