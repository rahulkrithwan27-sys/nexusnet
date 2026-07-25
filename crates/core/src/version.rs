//! Compile-time version and build metadata.
//!
//! These constants are resolved from Cargo's environment variables at build
//! time, so they always reflect the version declared in `Cargo.toml` without
//! any manual synchronization.

/// The name of this crate (`nexusnet-core`).
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// The semantic version of this crate, e.g. `"0.1.0"`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The major component of [`VERSION`].
pub const VERSION_MAJOR: &str = env!("CARGO_PKG_VERSION_MAJOR");

/// The minor component of [`VERSION`].
pub const VERSION_MINOR: &str = env!("CARGO_PKG_VERSION_MINOR");

/// The patch component of [`VERSION`].
pub const VERSION_PATCH: &str = env!("CARGO_PKG_VERSION_PATCH");

/// Returns a human-readable identifier of the form `"nexusnet-core x.y.z"`.
///
/// This is the string that command-line tools and diagnostics should print to
/// identify the running engine build.
///
/// # Examples
///
/// ```
/// let banner = nexusnet_core::version_string();
/// assert!(banner.starts_with("nexusnet-core "));
/// ```
#[must_use]
pub fn version_string() -> String {
    format!("{NAME} {VERSION}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_constants_are_populated() {
        assert_eq!(NAME, "nexusnet-core");
        assert!(!VERSION.is_empty());
        assert_eq!(
            VERSION,
            format!("{VERSION_MAJOR}.{VERSION_MINOR}.{VERSION_PATCH}")
        );
    }

    #[test]
    fn version_string_has_expected_shape() {
        let s = version_string();
        assert!(s.starts_with("nexusnet-core "));
        assert!(s.contains(VERSION));
    }
}
