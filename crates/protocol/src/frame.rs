//! Frame types, header layout, and frame encoding.
//!
//! # Wire layout
//!
//! Every frame begins with a fixed 16-byte header in network byte order
//! (big-endian), followed by `payload_len` bytes of payload:
//!
//! ```text
//!  0       1       2       3       4       5       6       7
//! +-------+-------+-------+-------+-------+-------+-------+-------+
//! |     magic     | major | minor |  type | flags |   reserved    |
//! +-------+-------+-------+-------+-------+-------+-------+-------+
//! |           stream_id           |          payload_len          |
//! +-------+-------+-------+-------+-------+-------+-------+-------+
//! |                          payload ...                          |
//! +---------------------------------------------------------------+
//! ```
//!
//! The header is deliberately fixed-width: a reader always knows exactly how
//! many bytes it needs before it can learn the payload length, which keeps the
//! streaming decoder a simple two-state machine.

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::error::{Error, Result};
use crate::version::ProtocolVersion;
use crate::{DEFAULT_MAX_PAYLOAD_LEN, MAGIC, PROTOCOL_VERSION};

/// The size in bytes of an encoded [`FrameHeader`].
pub const HEADER_LEN: usize = 16;

/// The kind of a frame, carried in the header's `type` byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(u8)]
pub enum FrameType {
    /// Application payload belonging to a stream.
    Data = 0x01,
    /// Connection or stream control metadata.
    Control = 0x02,
    /// Liveness probe; the peer replies with [`FrameType::Pong`].
    Ping = 0x03,
    /// Reply to a [`FrameType::Ping`].
    Pong = 0x04,
    /// Opens a connection and carries version negotiation.
    Handshake = 0x05,
    /// Signals orderly shutdown of a stream or connection.
    Close = 0x06,
}

impl FrameType {
    /// Returns the on-the-wire discriminant.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parses a frame type from its wire discriminant.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownFrameType`] if the byte is not a defined type.
    pub const fn from_u8(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(Self::Data),
            0x02 => Ok(Self::Control),
            0x03 => Ok(Self::Ping),
            0x04 => Ok(Self::Pong),
            0x05 => Ok(Self::Handshake),
            0x06 => Ok(Self::Close),
            other => Err(Error::UnknownFrameType { value: other }),
        }
    }
}

/// Bit flags describing how a payload has been transformed.
///
/// Flags are order-independent and describe the payload as it appears on the
/// wire. A receiver applies the inverse transformations in the reverse of the
/// order given here: decrypt, then decompress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct FrameFlags(u8);

impl FrameFlags {
    /// No flags set.
    pub const NONE: Self = Self(0x00);
    /// The payload is compressed.
    pub const COMPRESSED: Self = Self(0x01);
    /// The payload is encrypted.
    pub const ENCRYPTED: Self = Self(0x02);
    /// This is the final frame of its stream.
    pub const END_OF_STREAM: Self = Self(0x04);

    /// Every bit currently defined; any other bit is rejected on decode.
    const DEFINED: u8 = 0x01 | 0x02 | 0x04;

    /// Returns the raw bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Parses flags from a raw byte.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownFlags`] if any undefined bit is set. Rejecting
    /// unknown bits keeps a future flag from being silently ignored by an old
    /// peer that would then misread the payload.
    pub const fn from_bits(bits: u8) -> Result<Self> {
        if bits & !Self::DEFINED == 0 {
            Ok(Self(bits))
        } else {
            Err(Error::UnknownFlags { bits })
        }
    }

    /// Returns `true` if every flag in `other` is set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns the union of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns `true` when no flags are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for FrameFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// The fixed-size header that prefixes every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// The protocol version this frame was produced by.
    pub version: ProtocolVersion,
    /// The kind of frame.
    pub frame_type: FrameType,
    /// Payload transformation flags.
    pub flags: FrameFlags,
    /// The stream this frame belongs to; `0` denotes connection scope.
    pub stream_id: u32,
    /// The number of payload bytes following the header.
    pub payload_len: u32,
}

impl FrameHeader {
    /// Creates a header for the current [`PROTOCOL_VERSION`] with no flags.
    #[must_use]
    pub const fn new(frame_type: FrameType, stream_id: u32, payload_len: u32) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            frame_type,
            flags: FrameFlags::NONE,
            stream_id,
            payload_len,
        }
    }

    /// Returns a copy with `flags` applied.
    #[must_use]
    pub const fn with_flags(mut self, flags: FrameFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Writes the header to `dst`, appending exactly [`HEADER_LEN`] bytes.
    pub fn encode_into(&self, dst: &mut BytesMut) {
        dst.reserve(HEADER_LEN);
        dst.put_u16(MAGIC);
        dst.put_u8(self.version.major);
        dst.put_u8(self.version.minor);
        dst.put_u8(self.frame_type.as_u8());
        dst.put_u8(self.flags.bits());
        dst.put_u16(0); // reserved
        dst.put_u32(self.stream_id);
        dst.put_u32(self.payload_len);
    }

    /// Decodes a header from exactly the first [`HEADER_LEN`] bytes of `src`.
    ///
    /// `src` is not consumed; callers that need to advance the buffer should do
    /// so themselves. `max_payload_len` bounds the declared payload length so a
    /// hostile peer cannot induce a large allocation.
    ///
    /// # Errors
    ///
    /// Returns [`Error::BufferTooSmall`] if fewer than [`HEADER_LEN`] bytes are
    /// available, [`Error::InvalidMagic`] on a bad magic number,
    /// [`Error::ReservedNotZero`] if reserved bits are set,
    /// [`Error::UnknownFrameType`] or [`Error::UnknownFlags`] on unrecognized
    /// header fields, and [`Error::PayloadTooLarge`] if the declared length
    /// exceeds `max_payload_len`.
    pub fn decode(src: &[u8], max_payload_len: u32) -> Result<Self> {
        if src.len() < HEADER_LEN {
            return Err(Error::BufferTooSmall {
                needed: HEADER_LEN,
                available: src.len(),
            });
        }

        let mut cursor = &src[..HEADER_LEN];

        let magic = cursor.get_u16();
        if magic != MAGIC {
            return Err(Error::invalid_magic(magic));
        }

        let version = ProtocolVersion::new(cursor.get_u8(), cursor.get_u8());
        let frame_type = FrameType::from_u8(cursor.get_u8())?;
        let flags = FrameFlags::from_bits(cursor.get_u8())?;

        let reserved = cursor.get_u16();
        if reserved != 0 {
            return Err(Error::ReservedNotZero { value: reserved });
        }

        let stream_id = cursor.get_u32();
        let payload_len = cursor.get_u32();
        if payload_len > max_payload_len {
            return Err(Error::PayloadTooLarge {
                len: payload_len,
                max: max_payload_len,
            });
        }

        Ok(Self {
            version,
            frame_type,
            flags,
            stream_id,
            payload_len,
        })
    }
}

/// A complete frame: a [`FrameHeader`] and its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    header: FrameHeader,
    payload: Bytes,
}

impl Frame {
    /// Builds a frame from a type, stream, and payload.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PayloadLengthOverflow`] if the payload is longer than
    /// [`u32::MAX`] bytes.
    pub fn new(frame_type: FrameType, stream_id: u32, payload: impl Into<Bytes>) -> Result<Self> {
        let payload = payload.into();
        let payload_len = u32::try_from(payload.len())
            .map_err(|_| Error::PayloadLengthOverflow { len: payload.len() })?;

        Ok(Self {
            header: FrameHeader::new(frame_type, stream_id, payload_len),
            payload,
        })
    }

    /// Builds a frame from an already-constructed header and payload.
    ///
    /// The header's `payload_len` is corrected to match `payload`, so the two
    /// can never disagree.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PayloadLengthOverflow`] if the payload is longer than
    /// [`u32::MAX`] bytes.
    pub fn from_parts(mut header: FrameHeader, payload: impl Into<Bytes>) -> Result<Self> {
        let payload = payload.into();
        header.payload_len = u32::try_from(payload.len())
            .map_err(|_| Error::PayloadLengthOverflow { len: payload.len() })?;

        Ok(Self { header, payload })
    }

    /// Returns the frame header.
    #[must_use]
    pub const fn header(&self) -> &FrameHeader {
        &self.header
    }

    /// Returns the payload bytes.
    #[must_use]
    pub const fn payload(&self) -> &Bytes {
        &self.payload
    }

    /// Consumes the frame, returning its payload.
    #[must_use]
    pub fn into_payload(self) -> Bytes {
        self.payload
    }

    /// Applies `flags` to this frame's header.
    #[must_use]
    pub const fn with_flags(mut self, flags: FrameFlags) -> Self {
        self.header.flags = flags;
        self
    }

    /// Returns the total encoded size: header plus payload.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        HEADER_LEN + self.payload.len()
    }

    /// Appends the encoded frame to `dst`.
    pub fn encode_into(&self, dst: &mut BytesMut) {
        dst.reserve(self.encoded_len());
        self.header.encode_into(dst);
        dst.put_slice(&self.payload);
    }

    /// Encodes the frame into a freshly allocated buffer.
    #[must_use]
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(self.encoded_len());
        self.encode_into(&mut buf);
        buf.freeze()
    }

    /// Decodes a single complete frame from the front of `src`.
    ///
    /// Returns the frame and the number of bytes consumed. Use
    /// [`Decoder`](crate::Decoder) instead when reading from a stream where
    /// frames may arrive split across reads.
    ///
    /// # Errors
    ///
    /// Returns [`Error::BufferTooSmall`] if `src` does not yet hold the whole
    /// frame, plus any error from [`FrameHeader::decode`].
    pub fn decode(src: &[u8]) -> Result<(Self, usize)> {
        Self::decode_with_limit(src, DEFAULT_MAX_PAYLOAD_LEN)
    }

    /// Decodes a single frame, bounding the payload by `max_payload_len`.
    ///
    /// # Errors
    ///
    /// See [`Frame::decode`].
    pub fn decode_with_limit(src: &[u8], max_payload_len: u32) -> Result<(Self, usize)> {
        let header = FrameHeader::decode(src, max_payload_len)?;
        let payload_len = header.payload_len as usize;
        let total = HEADER_LEN + payload_len;

        if src.len() < total {
            return Err(Error::BufferTooSmall {
                needed: total,
                available: src.len(),
            });
        }

        let payload = Bytes::copy_from_slice(&src[HEADER_LEN..total]);
        Ok((Self { header, payload }, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_type_roundtrips() {
        for ty in [
            FrameType::Data,
            FrameType::Control,
            FrameType::Ping,
            FrameType::Pong,
            FrameType::Handshake,
            FrameType::Close,
        ] {
            assert_eq!(FrameType::from_u8(ty.as_u8()), Ok(ty));
        }
    }

    #[test]
    fn unknown_frame_type_is_rejected() {
        assert_eq!(
            FrameType::from_u8(0xAB),
            Err(Error::UnknownFrameType { value: 0xAB })
        );
    }

    #[test]
    fn flags_combine_and_test() {
        let flags = FrameFlags::COMPRESSED | FrameFlags::ENCRYPTED;
        assert!(flags.contains(FrameFlags::COMPRESSED));
        assert!(flags.contains(FrameFlags::ENCRYPTED));
        assert!(!flags.contains(FrameFlags::END_OF_STREAM));
        assert!(!flags.is_empty());
        assert!(FrameFlags::NONE.is_empty());
    }

    #[test]
    fn undefined_flag_bits_are_rejected() {
        assert_eq!(
            FrameFlags::from_bits(0b1000_0000),
            Err(Error::UnknownFlags { bits: 0b1000_0000 })
        );
        assert_eq!(FrameFlags::from_bits(0x07), Ok(FrameFlags(0x07)));
    }

    #[test]
    fn header_encodes_to_fixed_width() {
        let mut buf = BytesMut::new();
        FrameHeader::new(FrameType::Data, 7, 3).encode_into(&mut buf);
        assert_eq!(buf.len(), HEADER_LEN);
    }

    #[test]
    fn header_roundtrips() {
        let original = FrameHeader::new(FrameType::Control, 42, 128)
            .with_flags(FrameFlags::COMPRESSED | FrameFlags::END_OF_STREAM);

        let mut buf = BytesMut::new();
        original.encode_into(&mut buf);

        assert_eq!(
            FrameHeader::decode(&buf, DEFAULT_MAX_PAYLOAD_LEN),
            Ok(original)
        );
    }

    #[test]
    fn frame_roundtrips() {
        let frame = Frame::new(FrameType::Data, 9, Bytes::from_static(b"hello wire"))
            .expect("payload fits in u32");
        let encoded = frame.encode();

        let (decoded, consumed) = Frame::decode(&encoded).expect("valid frame decodes");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, frame);
        assert_eq!(decoded.payload().as_ref(), b"hello wire");
    }

    #[test]
    fn empty_payload_is_valid() {
        let frame = Frame::new(FrameType::Ping, 0, Bytes::new()).expect("empty payload is fine");
        let encoded = frame.encode();
        assert_eq!(encoded.len(), HEADER_LEN);

        let (decoded, consumed) = Frame::decode(&encoded).expect("ping decodes");
        assert_eq!(consumed, HEADER_LEN);
        assert!(decoded.payload().is_empty());
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut encoded = BytesMut::from(
            &Frame::new(FrameType::Data, 1, Bytes::from_static(b"x"))
                .expect("payload fits")
                .encode()[..],
        );
        encoded[0] = 0xFF;

        assert!(matches!(
            Frame::decode(&encoded),
            Err(Error::InvalidMagic { .. })
        ));
    }

    #[test]
    fn nonzero_reserved_is_rejected() {
        let mut encoded = BytesMut::from(
            &Frame::new(FrameType::Data, 1, Bytes::from_static(b"x"))
                .expect("payload fits")
                .encode()[..],
        );
        encoded[6] = 0x01;

        assert_eq!(
            Frame::decode(&encoded),
            Err(Error::ReservedNotZero { value: 0x0100 })
        );
    }

    #[test]
    fn truncated_frame_reports_how_much_is_needed() {
        let frame = Frame::new(FrameType::Data, 1, Bytes::from_static(b"0123456789"))
            .expect("payload fits");
        let encoded = frame.encode();

        let err = Frame::decode(&encoded[..encoded.len() - 4]).expect_err("truncation is an error");
        assert_eq!(
            err,
            Error::BufferTooSmall {
                needed: encoded.len(),
                available: encoded.len() - 4,
            }
        );
    }

    #[test]
    fn oversized_payload_is_rejected_before_allocation() {
        let mut buf = BytesMut::new();
        FrameHeader::new(FrameType::Data, 1, 4096).encode_into(&mut buf);

        assert_eq!(
            FrameHeader::decode(&buf, 1024),
            Err(Error::PayloadTooLarge {
                len: 4096,
                max: 1024,
            })
        );
    }

    #[test]
    fn from_parts_corrects_payload_len() {
        let header = FrameHeader::new(FrameType::Data, 1, 999);
        let frame = Frame::from_parts(header, Bytes::from_static(b"abc")).expect("payload fits");
        assert_eq!(frame.header().payload_len, 3);
    }
}
