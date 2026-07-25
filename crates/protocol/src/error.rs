//! Error types for wire-format encoding and decoding.

use std::result::Result as StdResult;

use crate::{ProtocolVersion, MAGIC};

/// A specialized [`Result`](std::result::Result) for protocol operations.
pub type Result<T> = StdResult<T, Error>;

/// An error produced while encoding or decoding the NexusNet wire format.
///
/// Every variant carries the observed value so callers can log precisely what
/// went wrong on the wire without re-parsing the buffer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The frame did not begin with the protocol magic number.
    ///
    /// This usually means the stream is misaligned or is not a NexusNet stream
    /// at all.
    #[error("invalid frame magic: expected {expected:#06x}, found {found:#06x}")]
    InvalidMagic {
        /// The magic number the protocol requires.
        expected: u16,
        /// The magic number actually present in the buffer.
        found: u16,
    },

    /// The peer advertised a protocol version this build cannot speak.
    #[error("unsupported protocol version {found}, this build speaks {supported}")]
    UnsupportedVersion {
        /// The version received from the peer.
        found: ProtocolVersion,
        /// The version this build implements.
        supported: ProtocolVersion,
    },

    /// The frame-type byte did not correspond to a known [`FrameType`].
    ///
    /// [`FrameType`]: crate::FrameType
    #[error("unknown frame type: {value:#04x}")]
    UnknownFrameType {
        /// The unrecognized discriminant.
        value: u8,
    },

    /// The flags byte contained bits that are not defined by this version.
    ///
    /// Unknown flags are rejected rather than ignored so that a future
    /// flag-bearing frame is never silently misinterpreted.
    #[error("unknown frame flags set: {bits:#010b}")]
    UnknownFlags {
        /// The full flags byte as received.
        bits: u8,
    },

    /// A reserved header field was non-zero.
    ///
    /// Reserved bits are required to be zero so they can be assigned meaning in
    /// a later revision without ambiguity.
    #[error("reserved header field must be zero, found {value:#06x}")]
    ReservedNotZero {
        /// The non-zero reserved value.
        value: u16,
    },

    /// The declared payload length exceeded the configured maximum.
    ///
    /// This is the primary defense against a malicious peer inducing an
    /// unbounded allocation.
    #[error("payload length {len} exceeds maximum {max}")]
    PayloadTooLarge {
        /// The length declared in the frame header.
        len: u32,
        /// The configured maximum payload length.
        max: u32,
    },

    /// A destination buffer was too small to hold the encoded output.
    #[error("buffer too small: need {needed} bytes, have {available}")]
    BufferTooSmall {
        /// The number of bytes required.
        needed: usize,
        /// The number of bytes available.
        available: usize,
    },

    /// A payload was too large to be described by the 32-bit length field.
    #[error("payload of {len} bytes cannot be encoded in a 32-bit length field")]
    PayloadLengthOverflow {
        /// The oversized payload length.
        len: usize,
    },

    /// Version negotiation found no version supported by both peers.
    #[error("no mutually supported protocol version")]
    NoCommonVersion,
}

impl Error {
    /// Builds an [`Error::InvalidMagic`] from the observed magic number.
    #[must_use]
    pub const fn invalid_magic(found: u16) -> Self {
        Self::InvalidMagic {
            expected: MAGIC,
            found,
        }
    }
}
