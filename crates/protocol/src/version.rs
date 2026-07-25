//! Protocol versioning and negotiation.
//!
//! NexusNet uses a two-part `major.minor` protocol version carried in every
//! frame header. Compatibility follows one rule: **peers are compatible when
//! their major versions match**. A newer minor version may add frame types or
//! flags, but must not change the meaning of anything already defined, so an
//! older peer can safely talk to a newer one at the older minor level.

use std::cmp::Ordering;
use std::fmt;

use crate::error::{Error, Result};

/// A `major.minor` protocol version.
///
/// Ordering is lexicographic: major first, then minor. This makes
/// [`negotiate`] able to pick the highest mutually supported version with a
/// simple maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion {
    /// The major version. Peers must agree on this exactly.
    pub major: u8,
    /// The minor version. Higher minors are backward compatible.
    pub minor: u8,
}

impl ProtocolVersion {
    /// Creates a version from its parts.
    ///
    /// # Examples
    ///
    /// ```
    /// use nexusnet_protocol::ProtocolVersion;
    ///
    /// let v = ProtocolVersion::new(1, 2);
    /// assert_eq!(v.major, 1);
    /// assert_eq!(v.minor, 2);
    /// ```
    #[must_use]
    pub const fn new(major: u8, minor: u8) -> Self {
        Self { major, minor }
    }

    /// Returns `true` when `self` can communicate with `other`.
    ///
    /// Compatibility requires identical major versions; the minor versions may
    /// differ freely.
    ///
    /// # Examples
    ///
    /// ```
    /// use nexusnet_protocol::ProtocolVersion;
    ///
    /// let ours = ProtocolVersion::new(1, 3);
    /// assert!(ours.is_compatible_with(ProtocolVersion::new(1, 0)));
    /// assert!(!ours.is_compatible_with(ProtocolVersion::new(2, 0)));
    /// ```
    #[must_use]
    pub const fn is_compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }

    /// Returns the lower of two compatible versions.
    ///
    /// The effective version of a connection is the minimum of both peers, so
    /// neither side uses a feature the other cannot parse.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoCommonVersion`] when the major versions differ.
    pub fn common_with(self, other: Self) -> Result<Self> {
        if self.is_compatible_with(other) {
            Ok(match self.cmp(&other) {
                Ordering::Less | Ordering::Equal => self,
                Ordering::Greater => other,
            })
        } else {
            Err(Error::NoCommonVersion)
        }
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Selects the highest version supported by both peers.
///
/// Both slices are treated as unordered sets of supported versions. The result
/// is the greatest version that appears (compatibly) in both.
///
/// # Errors
///
/// Returns [`Error::NoCommonVersion`] if the two sets share no compatible
/// version, including when either slice is empty.
///
/// # Examples
///
/// ```
/// use nexusnet_protocol::{negotiate, ProtocolVersion};
///
/// let local = [ProtocolVersion::new(1, 0), ProtocolVersion::new(1, 2)];
/// let remote = [ProtocolVersion::new(1, 1), ProtocolVersion::new(2, 0)];
///
/// // Highest common major-1 version, capped at what each side supports.
/// assert_eq!(negotiate(&local, &remote)?, ProtocolVersion::new(1, 1));
/// # Ok::<(), nexusnet_protocol::Error>(())
/// ```
pub fn negotiate(local: &[ProtocolVersion], remote: &[ProtocolVersion]) -> Result<ProtocolVersion> {
    local
        .iter()
        .filter_map(|&ours| {
            remote
                .iter()
                .filter(|&&theirs| ours.is_compatible_with(theirs))
                .map(|&theirs| ours.min(theirs))
                .max()
        })
        .max()
        .ok_or(Error::NoCommonVersion)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_dotted() {
        assert_eq!(ProtocolVersion::new(1, 4).to_string(), "1.4");
    }

    #[test]
    fn compatibility_depends_only_on_major() {
        let a = ProtocolVersion::new(1, 0);
        assert!(a.is_compatible_with(ProtocolVersion::new(1, 9)));
        assert!(!a.is_compatible_with(ProtocolVersion::new(0, 9)));
    }

    #[test]
    fn ordering_is_major_then_minor() {
        assert!(ProtocolVersion::new(1, 0) < ProtocolVersion::new(1, 1));
        assert!(ProtocolVersion::new(1, 9) < ProtocolVersion::new(2, 0));
    }

    #[test]
    fn common_with_picks_the_lower_version() {
        let ours = ProtocolVersion::new(1, 5);
        let theirs = ProtocolVersion::new(1, 2);
        assert_eq!(ours.common_with(theirs), Ok(theirs));
        assert_eq!(theirs.common_with(ours), Ok(theirs));
    }

    #[test]
    fn common_with_rejects_major_mismatch() {
        let ours = ProtocolVersion::new(1, 0);
        assert_eq!(
            ours.common_with(ProtocolVersion::new(2, 0)),
            Err(Error::NoCommonVersion)
        );
    }

    #[test]
    fn negotiate_selects_highest_common() {
        let local = [ProtocolVersion::new(1, 0), ProtocolVersion::new(1, 3)];
        let remote = [ProtocolVersion::new(1, 1), ProtocolVersion::new(1, 2)];
        assert_eq!(negotiate(&local, &remote), Ok(ProtocolVersion::new(1, 2)));
    }

    #[test]
    fn negotiate_fails_without_shared_major() {
        let local = [ProtocolVersion::new(1, 0)];
        let remote = [ProtocolVersion::new(2, 0)];
        assert_eq!(negotiate(&local, &remote), Err(Error::NoCommonVersion));
    }

    #[test]
    fn negotiate_fails_on_empty_input() {
        let local = [ProtocolVersion::new(1, 0)];
        assert_eq!(negotiate(&local, &[]), Err(Error::NoCommonVersion));
        assert_eq!(negotiate(&[], &local), Err(Error::NoCommonVersion));
    }
}
