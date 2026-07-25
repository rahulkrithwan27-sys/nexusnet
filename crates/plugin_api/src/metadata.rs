//! Plugin identity and API compatibility.
//!
//! ## Why version the API at all
//!
//! A plugin is written against a particular set of extension points. When those
//! change, a plugin built against the old shape will misbehave — subtly, at
//! runtime, in whatever way the mismatch happens to manifest. Refusing to load
//! it is the only safe response, and refusing requires knowing which version it
//! was built against.
//!
//! [`ApiVersion`] makes that check explicit. A plugin declares the version it
//! targets; the host compares and rejects what it cannot honour.

use std::fmt;

/// The plugin API version this build of NexusNet provides.
///
/// Bump the major component when an extension point changes shape or is
/// removed, and the minor component when one is added. Additions are
/// backwards-compatible; changes are not.
pub const CURRENT_API_VERSION: ApiVersion = ApiVersion::new(1, 0);

/// A plugin API version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ApiVersion {
    /// Incremented when an extension point changes incompatibly.
    pub major: u16,
    /// Incremented when an extension point is added.
    pub minor: u16,
}

impl ApiVersion {
    /// Creates a version.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Returns `true` if a plugin targeting this version can run against
    /// `host`.
    ///
    /// The rule is the usual one for additive interfaces: majors must match
    /// exactly, and the plugin's minor may not exceed the host's. A plugin
    /// built against a *newer* minor may use extension points this host does
    /// not have, so it is refused; a plugin built against an older minor uses
    /// only what has always existed, so it is accepted.
    #[must_use]
    pub const fn is_compatible_with(self, host: Self) -> bool {
        self.major == host.major && self.minor <= host.minor
    }

    /// Returns `true` if this version works with the current build.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        self.is_compatible_with(CURRENT_API_VERSION)
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// What a plugin declares about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PluginMetadata {
    /// A unique identifier, used as the registry key.
    pub name: String,
    /// The plugin's own version, for the operator's benefit.
    pub version: String,
    /// The plugin API version this plugin was built against.
    pub api_version: ApiVersion,
    /// A one-line description.
    pub description: String,
}

impl PluginMetadata {
    /// Creates metadata targeting the current API version.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            api_version: CURRENT_API_VERSION,
            description: String::new(),
        }
    }

    /// Sets the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Sets the API version this plugin targets.
    ///
    /// Only useful for a plugin that deliberately targets an older API than the
    /// crate it is compiled against; otherwise the default is correct.
    #[must_use]
    pub const fn with_api_version(mut self, api_version: ApiVersion) -> Self {
        self.api_version = api_version;
        self
    }

    /// Returns `true` if the name is usable as a registry key.
    ///
    /// Empty or whitespace-only names are rejected: they cannot be looked up or
    /// meaningfully reported in a log.
    #[must_use]
    pub fn has_valid_name(&self) -> bool {
        !self.name.trim().is_empty()
    }
}

impl fmt::Display for PluginMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} v{} (API {})",
            self.name, self.version, self.api_version
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_display_dotted() {
        assert_eq!(ApiVersion::new(2, 7).to_string(), "2.7");
    }

    #[test]
    fn an_identical_version_is_compatible() {
        let host = ApiVersion::new(1, 3);
        assert!(ApiVersion::new(1, 3).is_compatible_with(host));
    }

    #[test]
    fn an_older_minor_is_accepted() {
        let host = ApiVersion::new(1, 5);
        assert!(
            ApiVersion::new(1, 2).is_compatible_with(host),
            "a plugin using only long-standing extension points is safe"
        );
    }

    #[test]
    fn a_newer_minor_is_refused() {
        let host = ApiVersion::new(1, 2);
        assert!(
            !ApiVersion::new(1, 5).is_compatible_with(host),
            "the plugin may use extension points this host does not have"
        );
    }

    #[test]
    fn a_different_major_is_refused_in_both_directions() {
        let host = ApiVersion::new(2, 0);

        assert!(!ApiVersion::new(1, 9).is_compatible_with(host));
        assert!(!ApiVersion::new(3, 0).is_compatible_with(host));
    }

    #[test]
    fn the_current_version_is_supported() {
        assert!(CURRENT_API_VERSION.is_supported());
    }

    #[test]
    fn versions_order_major_then_minor() {
        assert!(ApiVersion::new(1, 9) < ApiVersion::new(2, 0));
        assert!(ApiVersion::new(1, 2) < ApiVersion::new(1, 10));
    }

    #[test]
    fn metadata_defaults_to_the_current_api() {
        let metadata = PluginMetadata::new("compressor", "0.1.0");

        assert_eq!(metadata.api_version, CURRENT_API_VERSION);
        assert!(metadata.has_valid_name());
        assert!(metadata.description.is_empty());
    }

    #[test]
    fn metadata_builders_apply() {
        let metadata = PluginMetadata::new("audit", "2.1.0")
            .with_description("Logs every frame")
            .with_api_version(ApiVersion::new(1, 0));

        assert_eq!(metadata.description, "Logs every frame");
        assert_eq!(metadata.api_version, ApiVersion::new(1, 0));
        assert_eq!(metadata.to_string(), "audit v2.1.0 (API 1.0)");
    }

    #[test]
    fn an_unusable_name_is_detected() {
        assert!(!PluginMetadata::new("", "1.0").has_valid_name());
        assert!(
            !PluginMetadata::new("   ", "1.0").has_valid_name(),
            "whitespace cannot be looked up or logged usefully"
        );
    }
}
