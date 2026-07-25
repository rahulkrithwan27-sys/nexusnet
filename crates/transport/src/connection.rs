//! A framed connection over any asynchronous byte stream.
//!
//! [`Connection`] pairs the protocol codec with an async stream. It is generic
//! over the stream type, so the same logic serves TCP today, TLS and QUIC
//! later, and an in-memory pipe in tests — no sockets required to test framing
//! behavior.

use bytes::BytesMut;
use nexusnet_protocol::{Decoder, Encoder, Frame};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::config::{Error, Result, TransportConfig};

/// A framed connection over an asynchronous byte stream.
///
/// # Examples
///
/// Framing can be exercised over an in-memory duplex pipe:
///
/// ```
/// # use bytes::Bytes;
/// # use nexusnet_protocol::{Frame, FrameType};
/// # use nexusnet_transport::{Connection, TransportConfig};
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), nexusnet_transport::Error> {
/// let (client, server) = tokio::io::duplex(4096);
/// let mut client = Connection::new(client, TransportConfig::default());
/// let mut server = Connection::new(server, TransportConfig::default());
///
/// let frame = Frame::new(FrameType::Data, 1, Bytes::from_static(b"ping"))?;
/// client.send(&frame).await?;
///
/// let received = server.recv().await?.expect("a frame arrives");
/// assert_eq!(received.payload().as_ref(), b"ping");
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Connection<S> {
    stream: S,
    decoder: Decoder,
    encoder: Encoder,
    read_buffer: BytesMut,
    config: TransportConfig,
    frames_sent: u64,
    frames_received: u64,
}

impl<S> Connection<S> {
    /// Wraps `stream` in a framed connection.
    #[must_use]
    pub fn new(stream: S, config: TransportConfig) -> Self {
        Self {
            stream,
            decoder: Decoder::with_max_payload_len(config.max_payload_len),
            encoder: Encoder::new(),
            read_buffer: BytesMut::zeroed(config.read_buffer),
            config,
            frames_sent: 0,
            frames_received: 0,
        }
    }

    /// Returns the configuration this connection was created with.
    #[must_use]
    pub const fn config(&self) -> &TransportConfig {
        &self.config
    }

    /// Returns how many frames have been sent.
    #[must_use]
    pub const fn frames_sent(&self) -> u64 {
        self.frames_sent
    }

    /// Returns how many frames have been received.
    #[must_use]
    pub const fn frames_received(&self) -> u64 {
        self.frames_received
    }

    /// Returns a reference to the underlying stream.
    #[must_use]
    pub const fn get_ref(&self) -> &S {
        &self.stream
    }

    /// Consumes the connection, returning the underlying stream.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.stream
    }
}

impl<S> Connection<S>
where
    S: AsyncWrite + Unpin,
{
    /// Sends a single frame, flushing it to the stream.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the write fails.
    pub async fn send(&mut self, frame: &Frame) -> Result<()> {
        self.encoder.encode(frame);
        let bytes = self.encoder.take();

        self.stream.write_all(&bytes).await?;
        self.stream.flush().await?;
        self.frames_sent += 1;

        Ok(())
    }

    /// Sends several frames in a single write.
    ///
    /// Batching amortizes the syscall across frames, which matters when
    /// flushing a queue. An empty slice is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the write fails.
    pub async fn send_all(&mut self, frames: &[Frame]) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }

        for frame in frames {
            self.encoder.encode(frame);
        }
        let bytes = self.encoder.take();

        self.stream.write_all(&bytes).await?;
        self.stream.flush().await?;
        self.frames_sent += frames.len() as u64;

        Ok(())
    }

    /// Shuts down the write half of the stream.
    ///
    /// Signals a clean end-of-stream to the peer, whose next
    /// [`recv`](Connection::recv) returns `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the shutdown fails.
    pub async fn shutdown(&mut self) -> Result<()> {
        self.stream.shutdown().await?;
        Ok(())
    }
}

impl<S> Connection<S>
where
    S: AsyncRead + Unpin,
{
    /// Receives the next frame.
    ///
    /// Returns `Ok(None)` when the peer closes the connection cleanly at a
    /// frame boundary. A close *mid-frame* is [`Error::UnexpectedEof`] instead,
    /// because silently treating truncated data as a clean end would hide data
    /// loss.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if a read fails, [`Error::Protocol`] if the peer
    /// sent something unparseable, or [`Error::UnexpectedEof`] as described
    /// above. Protocol errors are fatal: the stream is desynchronized and the
    /// connection should be dropped.
    pub async fn recv(&mut self) -> Result<Option<Frame>> {
        loop {
            // Drain anything already buffered before issuing another read.
            if let Some(frame) = self.decoder.next_frame()? {
                self.frames_received += 1;
                return Ok(Some(frame));
            }

            let read = self.stream.read(&mut self.read_buffer).await?;
            if read == 0 {
                let buffered = self.decoder.buffered();
                return if buffered == 0 {
                    Ok(None)
                } else {
                    Err(Error::UnexpectedEof { buffered })
                };
            }

            self.decoder.push(&self.read_buffer[..read]);
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use nexusnet_protocol::{FrameType, HEADER_LEN};
    use tokio::io::AsyncWriteExt;

    use super::*;

    fn frame(stream_id: u32, payload: &'static [u8]) -> Frame {
        Frame::new(FrameType::Data, stream_id, Bytes::from_static(payload))
            .expect("payload fits in u32")
    }

    fn pair() -> (
        Connection<tokio::io::DuplexStream>,
        Connection<tokio::io::DuplexStream>,
    ) {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let config = TransportConfig::default();
        (Connection::new(a, config), Connection::new(b, config))
    }

    #[tokio::test]
    async fn a_frame_round_trips() {
        let (mut client, mut server) = pair();
        let sent = frame(1, b"hello");

        client.send(&sent).await.expect("send succeeds");
        let received = server
            .recv()
            .await
            .expect("no transport error")
            .expect("a frame arrives");

        assert_eq!(received, sent);
        assert_eq!(client.frames_sent(), 1);
        assert_eq!(server.frames_received(), 1);
    }

    #[tokio::test]
    async fn many_frames_preserve_order() {
        let (mut client, mut server) = pair();
        let frames: Vec<Frame> = (0..32).map(|i| frame(i, b"payload")).collect();

        client.send_all(&frames).await.expect("batch send succeeds");

        for expected in &frames {
            let received = server
                .recv()
                .await
                .expect("no transport error")
                .expect("a frame arrives");
            assert_eq!(&received, expected);
        }

        assert_eq!(client.frames_sent(), 32);
    }

    #[tokio::test]
    async fn clean_shutdown_reports_end_of_stream() {
        let (mut client, mut server) = pair();

        client
            .send(&frame(1, b"last"))
            .await
            .expect("send succeeds");
        client.shutdown().await.expect("shutdown succeeds");

        assert!(server.recv().await.expect("no error").is_some());
        assert!(
            server
                .recv()
                .await
                .expect("clean eof is not an error")
                .is_none(),
            "a clean close should report end-of-stream"
        );
    }

    #[tokio::test]
    async fn truncated_frame_is_an_error_not_a_clean_close() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let mut server = Connection::new(server, TransportConfig::default());

        // Write a header plus a partial payload, then close.
        let encoded = frame(1, b"incomplete payload").encode();
        client
            .write_all(&encoded[..HEADER_LEN + 4])
            .await
            .expect("partial write succeeds");
        client.shutdown().await.expect("shutdown succeeds");
        drop(client);

        let err = server
            .recv()
            .await
            .expect_err("truncation must be an error");
        assert!(matches!(err, Error::UnexpectedEof { .. }));
        assert!(err.is_fatal());
    }

    #[tokio::test]
    async fn empty_batch_is_a_no_op() {
        let (mut client, _server) = pair();
        client.send_all(&[]).await.expect("empty batch succeeds");
        assert_eq!(client.frames_sent(), 0);
    }

    #[tokio::test]
    async fn oversized_frames_are_rejected_by_the_decoder() {
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        let config = TransportConfig::default().with_max_payload_len(64);
        let mut reader = Connection::new(reader, config);

        let encoded = frame(
            1,
            b"this payload is longer than the sixty-four byte limit imposed above",
        )
        .encode();
        writer.write_all(&encoded).await.expect("write succeeds");

        let err = reader
            .recv()
            .await
            .expect_err("oversized frame is rejected");
        assert!(matches!(err, Error::Protocol(_)));
        assert!(err.is_fatal());
    }

    #[tokio::test]
    async fn a_tiny_read_buffer_still_reassembles_frames() {
        // Force many reads per frame to exercise the buffering path.
        let (client, server) = tokio::io::duplex(64 * 1024);
        let config = TransportConfig::default().with_read_buffer(7);
        let mut client = Connection::new(client, TransportConfig::default());
        let mut server = Connection::new(server, config);

        let sent = frame(9, b"a payload considerably longer than the read buffer");
        client.send(&sent).await.expect("send succeeds");

        let received = server
            .recv()
            .await
            .expect("no transport error")
            .expect("a frame arrives");
        assert_eq!(received, sent);
    }
}
