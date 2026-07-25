//! Errors raised by the plugin system.

use crate::metadata::ApiVersion;

/// The result type used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Something went wrong loading or running a plugin.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The plugin targets an API version this build cannot honour.
    ///
    /// Refusing is the only safe response: a plugin built against a different
    /// API would misbehave at runtime, in whatever way the mismatch happened to
    /// manifest.
    #[error("plugin '{name}' targets API {required}, but this build provides {provided}")]
    IncompatibleApi {
        /// The plugin's name.
        name: String,
        /// The API version the plugin was built against.
        required: ApiVersion,
        /// The API version this build provides.
        provided: ApiVersion,
    },

    /// A plugin with this name is already registered.
    ///
    /// Names are registry keys, so silently replacing one would leave the
    /// operator with no way to tell which is running.
    #[error("a plugin named '{name}' is already registered")]
    DuplicateName {
        /// The conflicting name.
        name: String,
    },

    /// The plugin's declared name cannot be used as a key.
    #[error("the plugin name '{name}' is not usable: it is empty or whitespace")]
    InvalidName {
        /// The offending name, quoted for visibility.
        name: String,
    },

    /// No plugin is registered under this name.
    #[error("no plugin named '{name}' is registered")]
    NotFound {
        /// The name that was looked up.
        name: String,
    },

    /// The plugin refused to load.
    #[error("plugin '{name}' failed to load: {reason}")]
    LoadFailed {
        /// The plugin's name.
        name: String,
        /// What the plugin reported.
        reason: String,
    },

    /// A setting was missing or malformed.
    #[error("setting '{key}' is unusable: {reason}")]
    Configuration {
        /// The setting key.
        key: String,
        /// Why it could not be used.
        reason: String,
    },

    /// An interceptor failed while processing a payload.
    #[error("interceptor '{name}' failed: {reason}")]
    Interceptor {
        /// The interceptor's name.
        name: String,
        /// What it reported.
        reason: String,
    },
}

impl Error {
    /// Returns `true` if the error means the plugin can never work here.
    ///
    /// Distinguishes a permanent mismatch from a transient failure, so a caller
    /// knows whether retrying could possibly help.
    #[must_use]
    pub const fn is_permanent(&self) -> bool {
        matches!(
            self,
            Self::IncompatibleApi { .. } | Self::InvalidName { .. } | Self::DuplicateName { .. }
        )
    }
}
