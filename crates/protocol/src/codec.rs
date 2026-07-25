//! Incremental encoding and decoding for byte streams.
//!
//! A transport delivers arbitrary byte chunks: one read may contain half a
//! frame, or three frames and a fragment. [`Decoder`] absorbs those chunks and
//! yields whole frames as they become available, buffering the remainder.

use bytes::{Buf, BytesMut};

use crate::error::{Error, Result};
use crate::frame::{Frame, FrameHeader, HEADER_LEN};
use crate::{Encoder, DEFAULT_MAX_PAYLOAD_LEN};

/// An incremental frame decoder.
///
/// Feed bytes with [`push`](Decoder::push), then drain whole frames with
/// [`next_frame`](Decoder::next_frame) until it returns `Ok(None)`.
///
/// # Examples
///
/// ```
/// use bytes::Bytes;
/// use nexusnet_protocol::{Decoder, Frame, FrameType};
///
/// let frame = Frame::new(FrameType::Data, 1, Bytes::from_static(b"payload"))?;
/// let encoded = frame.encode();
///
/// // Deliver the frame one byte at a time, as a slow network might.
/// let mut decoder = Decoder::new();
/// let mut decoded = None;
/// for byte in encoded.iter() {
///     decoder.push(&[*byte]);
///     if let Some(f) = decoder.next_frame()? {
///         decoded = Some(f);
///     }
/// }
///
/// assert_eq!(decoded.as_ref(), Some(&frame));
/// # Ok::<(), nexusnet_protocol::Error>(())
/// ```
#[derive(Debug)]
pub struct Decoder {
    buffer: BytesMut,
    max_payload_len: u32,
    /// Header of a frame whose payload has not fully arrived yet. Caching it
    /// avoids re-parsing and re-validating the header on every subsequent read.
    pending: Option<FrameHeader>,
}

impl Decoder {
    /// Creates a decoder with the default payload limit.
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_payload_len(DEFAULT_MAX_PAYLOAD_LEN)
    }

    /// Creates a decoder that rejects payloads larger than `max_payload_len`.
    ///
    /// Choose the smallest value your application can tolerate: the limit is
    /// checked against the declared length *before* any payload is buffered,
    /// so it bounds memory a peer can cause you to commit.
    #[must_use]
    pub fn with_max_payload_len(max_payload_len: u32) -> Self {
        Self {
            buffer: BytesMut::new(),
            max_payload_len,
            pending: None,
        }
    }

    /// Returns the configured maximum payload length.
    #[must_use]
    pub const fn max_payload_len(&self) -> u32 {
        self.max_payload_len
    }

    /// Returns the number of buffered bytes not yet formed into a frame.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Returns `true` when no partial data is buffered.
    ///
    /// A connection closing while this is `false` ended mid-frame.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Appends freshly read bytes to the internal buffer.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Discards all buffered data and any partially decoded frame.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.pending = None;
    }

    /// Attempts to take the next complete frame from the buffer.
    ///
    /// Returns `Ok(None)` when more bytes are needed; this is the normal
    /// "read again" signal, not an error.
    ///
    /// # Errors
    ///
    /// Returns a decoding error if the buffered bytes are not a valid frame,
    /// for example [`Error::InvalidMagic`] or [`Error::PayloadTooLarge`]. Such
    /// an error is not recoverable in place: the stream is desynchronized, so
    /// close the connection or [`reset`](Decoder::reset) and resynchronize.
    pub fn next_frame(&mut self) -> Result<Option<Frame>> {
        let header = match self.pending {
            Some(header) => header,
            None => {
                if self.buffer.len() < HEADER_LEN {
                    return Ok(None);
                }

                let header = FrameHeader::decode(&self.buffer, self.max_payload_len)?;
                self.pending = Some(header);
                header
            }
        };

        let payload_len = header.payload_len as usize;
        if self.buffer.len() < HEADER_LEN + payload_len {
            return Ok(None);
        }

        self.buffer.advance(HEADER_LEN);
        let payload = self.buffer.split_to(payload_len).freeze();
        self.pending = None;

        Frame::from_parts(header, payload).map(Some)
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    /// Creates an encoder with an empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::new(),
        }
    }

    /// Creates an encoder that preallocates `capacity` bytes.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: BytesMut::with_capacity(capacity),
        }
    }

    /// Appends an encoded frame to the internal buffer.
    pub fn encode(&mut self, frame: &Frame) {
        frame.encode_into(&mut self.buffer);
    }

    /// Returns the bytes accumulated so far without clearing them.
    #[must_use]
    pub fn buffer(&self) -> &[u8] {
        &self.buffer
    }

    /// Takes all accumulated bytes, leaving the encoder empty.
    #[must_use]
    pub fn take(&mut self) -> bytes::Bytes {
        self.buffer.split().freeze()
    }

    /// Returns `true` when nothing is pending transmission.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Reports whether `error` leaves the stream unusable.
///
/// All decoding errors desynchronize the stream, so this always returns `true`
/// today; it exists so callers can write intent-revealing code that keeps
/// working if recoverable errors are introduced later.
#[must_use]
pub const fn is_fatal(error: &Error) -> bool {
    let _ = error;
    true
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;
    use crate::frame::FrameType;

    fn data_frame(stream_id: u32, payload: &'static [u8]) -> Frame {
        Frame::new(FrameType::Data, stream_id, Bytes::from_static(payload))
            .expect("payload fits in u32")
    }

    #[test]
    fn decodes_a_single_whole_frame() {
        let frame = data_frame(1, b"hello");
        let mut decoder = Decoder::new();
        decoder.push(&frame.encode());

        assert_eq!(decoder.next_frame(), Ok(Some(frame)));
        assert_eq!(decoder.next_frame(), Ok(None));
        assert!(decoder.is_empty());
    }

    #[test]
    fn decodes_several_frames_from_one_chunk() {
        let first = data_frame(1, b"one");
        let second = data_frame(2, b"two");

        let mut encoder = Encoder::new();
        encoder.encode(&first);
        encoder.encode(&second);

        let mut decoder = Decoder::new();
        decoder.push(&encoder.take());

        assert_eq!(decoder.next_frame(), Ok(Some(first)));
        assert_eq!(decoder.next_frame(), Ok(Some(second)));
        assert_eq!(decoder.next_frame(), Ok(None));
    }

    #[test]
    fn waits_for_the_rest_of_a_split_header() {
        let frame = data_frame(1, b"payload");
        let encoded = frame.encode();

        let mut decoder = Decoder::new();
        decoder.push(&encoded[..4]);
        assert_eq!(decoder.next_frame(), Ok(None));

        decoder.push(&encoded[4..]);
        assert_eq!(decoder.next_frame(), Ok(Some(frame)));
    }

    #[test]
    fn waits_for_the_rest_of_a_split_payload() {
        let frame = data_frame(1, b"a longer payload body");
        let encoded = frame.encode();
        let split = HEADER_LEN + 4;

        let mut decoder = Decoder::new();
        decoder.push(&encoded[..split]);
        assert_eq!(decoder.next_frame(), Ok(None));
        assert!(!decoder.is_empty());

        decoder.push(&encoded[split..]);
        assert_eq!(decoder.next_frame(), Ok(Some(frame)));
        assert!(decoder.is_empty());
    }

    #[test]
    fn handles_a_frame_arriving_one_byte_at_a_time() {
        let frame = data_frame(3, b"trickle");
        let encoded = frame.encode();

        let mut decoder = Decoder::new();
        let mut decoded = None;
        for byte in encoded.iter() {
            decoder.push(&[*byte]);
            if let Some(f) = decoder.next_frame().expect("stream stays valid") {
                decoded = Some(f);
            }
        }

        assert_eq!(decoded, Some(frame));
    }

    #[test]
    fn keeps_trailing_bytes_of_the_next_frame() {
        let first = data_frame(1, b"first");
        let second = data_frame(2, b"second");

        let mut buf = BytesMut::new();
        first.encode_into(&mut buf);
        second.encode_into(&mut buf);

        // Deliver the first frame plus a fragment of the second.
        let cut = first.encoded_len() + 5;
        let mut decoder = Decoder::new();
        decoder.push(&buf[..cut]);

        assert_eq!(decoder.next_frame(), Ok(Some(first)));
        assert_eq!(decoder.next_frame(), Ok(None));
        assert_eq!(decoder.buffered(), 5);

        decoder.push(&buf[cut..]);
        assert_eq!(decoder.next_frame(), Ok(Some(second)));
    }

    #[test]
    fn rejects_an_oversized_payload_before_buffering_it() {
        let mut header_bytes = BytesMut::new();
        FrameHeader::new(FrameType::Data, 1, 8192).encode_into(&mut header_bytes);

        let mut decoder = Decoder::with_max_payload_len(1024);
        decoder.push(&header_bytes);

        assert_eq!(
            decoder.next_frame(),
            Err(Error::PayloadTooLarge {
                len: 8192,
                max: 1024,
            })
        );
    }

    #[test]
    fn rejects_a_corrupt_header() {
        let mut encoded = BytesMut::from(&data_frame(1, b"x").encode()[..]);
        encoded[0] = 0x00;

        let mut decoder = Decoder::new();
        decoder.push(&encoded);

        let err = decoder.next_frame().expect_err("corrupt magic is an error");
        assert!(matches!(err, Error::InvalidMagic { .. }));
        assert!(is_fatal(&err));
    }

    #[test]
    fn reset_clears_partial_state() {
        let frame = data_frame(1, b"partial");
        let encoded = frame.encode();

        let mut decoder = Decoder::new();
        decoder.push(&encoded[..HEADER_LEN + 2]);
        assert_eq!(decoder.next_frame(), Ok(None));
        assert!(!decoder.is_empty());

        decoder.reset();
        assert!(decoder.is_empty());
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn encoder_accumulates_and_drains() {
        let mut encoder = Encoder::with_capacity(128);
        assert!(encoder.is_empty());

        encoder.encode(&data_frame(1, b"a"));
        encoder.encode(&data_frame(2, b"bb"));
        assert!(!encoder.is_empty());
        assert_eq!(encoder.buffer().len(), 2 * HEADER_LEN + 3);

        let taken = encoder.take();
        assert_eq!(taken.len(), 2 * HEADER_LEN + 3);
        assert!(encoder.is_empty());
    }
}
