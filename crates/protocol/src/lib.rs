//! # nexusnet-protocol
//!
//! The NexusNet wire format: frame layout, framing, and version negotiation.
//!
//! This crate is deliberately transport-agnostic and allocation-conscious. It
//! knows how to turn frames into bytes and bytes back into frames; it does not
//! open sockets, compress, or encrypt. Those concerns live in
//! `nexusnet-transport`, `nexusnet-compression`, and `nexusnet-encryption`,
//! which describe their work using the flags defined here.
//!
//! ## Layers
//!
//! * [`Frame`] and [`FrameHeader`] — the fixed 16-byte header and its payload.
//! * [`Encoder`] and [`Decoder`] — incremental codecs for byte streams, where
//!   frames may be split across reads or batched into one.
//! * [`ProtocolVersion`] and [`negotiate`] — capability agreement at handshake.
//!
//! ## Example
//!
//! ```
//! use bytes::Bytes;
//! use nexusnet_protocol::{Decoder, Encoder, Frame, FrameFlags, FrameType};
//!
//! // Build and encode two frames.
//! let mut encoder = Encoder::new();
//! encoder.encode(&Frame::new(FrameType::Data, 1, Bytes::from_static(b"hello"))?);
//! encoder.encode(
//!     &Frame::new(FrameType::Data, 1, Bytes::from_static(b"world"))?
//!         .with_flags(FrameFlags::END_OF_STREAM),
//! );
//! let wire = encoder.take();
//!
//! // Decode them back out of the byte stream.
//! let mut decoder = Decoder::new();
//! decoder.push(&wire);
//!
//! let first = decoder.next_frame()?.expect("first frame is complete");
//! assert_eq!(first.payload().as_ref(), b"hello");
//!
//! let second = decoder.next_frame()?.expect("second frame is complete");
//! assert!(second.header().flags.contains(FrameFlags::END_OF_STREAM));
//!
//! assert!(decoder.next_frame()?.is_none());
//! # Ok::<(), nexusnet_protocol::Error>(())
//! ```
//!
//! ## Robustness
//!
//! Decoding is defensive by default. Unknown frame types and undefined flag
//! bits are rejected rather than ignored, reserved header bits must be zero,
//! and payload lengths are bounds-checked against
//! [`DEFAULT_MAX_PAYLOAD_LEN`] (configurable per decoder) *before* any payload
//! memory is committed.
#![cfg_attr(docsrs, feature(doc_cfg))]

use bytes::BytesMut;

mod codec;
mod error;
mod frame;
mod version;

pub use crate::codec::{is_fatal, Decoder};
pub use crate::error::{Error, Result};
pub use crate::frame::{Frame, FrameFlags, FrameHeader, FrameType, HEADER_LEN};
pub use crate::version::{negotiate, ProtocolVersion};

/// The magic number beginning every frame: ASCII `"NX"`.
pub const MAGIC: u16 = 0x4E58;

/// The wire-format version implemented by this build.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(1, 0);

/// Every protocol version this build can speak, for use with [`negotiate`].
pub const SUPPORTED_VERSIONS: &[ProtocolVersion] = &[PROTOCOL_VERSION];

/// The default maximum payload length: 16 MiB.
///
/// Frames declaring a longer payload are rejected before any memory is
/// allocated for them. Override per connection with
/// [`Decoder::with_max_payload_len`].
pub const DEFAULT_MAX_PAYLOAD_LEN: u32 = 16 * 1024 * 1024;

/// Buffers outbound frames as a contiguous byte stream.
///
/// Batching several frames into one buffer lets a transport issue a single
/// write syscall instead of one per frame.
#[derive(Debug)]
pub struct Encoder {
    pub(crate) buffer: BytesMut,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_is_ascii_nx() {
        assert_eq!(MAGIC.to_be_bytes(), *b"NX");
    }

    #[test]
    fn current_version_is_supported() {
        assert!(SUPPORTED_VERSIONS.contains(&PROTOCOL_VERSION));
    }

    #[test]
    fn negotiating_with_ourselves_yields_our_version() {
        assert_eq!(
            negotiate(SUPPORTED_VERSIONS, SUPPORTED_VERSIONS),
            Ok(PROTOCOL_VERSION)
        );
    }
}
