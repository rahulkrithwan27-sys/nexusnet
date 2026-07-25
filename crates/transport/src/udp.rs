//! UDP transport: one frame per datagram.
//!
//! Datagrams differ from streams in ways that matter here. There is no
//! reassembly, so a frame must fit in a single datagram; delivery is unordered
//! and unreliable, so a missing frame is not detectable at this layer; and an
//! oversized datagram is truncated by the kernel rather than deferred, so it is
//! reported as an error instead of silently mis-parsed.

use std::net::SocketAddr;

use nexusnet_protocol::Frame;
use tokio::net::{ToSocketAddrs, UdpSocket};

use crate::config::{Error, Result, TransportConfig};

/// A UDP endpoint that sends and receives one frame per datagram.
#[derive(Debug)]
pub struct UdpEndpoint {
    socket: UdpSocket,
    config: TransportConfig,
    buffer: Vec<u8>,
}

impl UdpEndpoint {
    /// Binds an endpoint to `address`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the address is in use or cannot be bound.
    pub async fn bind<A>(address: A, config: TransportConfig) -> Result<Self>
    where
        A: ToSocketAddrs,
    {
        let socket = UdpSocket::bind(address).await?;

        Ok(Self {
            socket,
            config,
            // One extra byte lets an oversized datagram be detected rather than
            // silently truncated to exactly the limit.
            buffer: vec![0_u8; config.max_datagram + 1],
        })
    }

    /// Returns the address the endpoint is bound to.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the address cannot be retrieved.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    /// Returns the endpoint configuration.
    #[must_use]
    pub const fn config(&self) -> &TransportConfig {
        &self.config
    }

    /// Sends a frame as a single datagram to `target`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DatagramTooLarge`] if the encoded frame exceeds the
    /// configured datagram limit, or [`Error::Io`] if the send fails.
    pub async fn send_to<A>(&self, frame: &Frame, target: A) -> Result<usize>
    where
        A: ToSocketAddrs,
    {
        let encoded = frame.encode();
        if encoded.len() > self.config.max_datagram {
            return Err(Error::DatagramTooLarge {
                len: encoded.len(),
                max: self.config.max_datagram,
            });
        }

        Ok(self.socket.send_to(&encoded, target).await?)
    }

    /// Receives a single frame from the next datagram.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DatagramTooLarge`] if the datagram exceeds the limit,
    /// [`Error::Protocol`] if the datagram is not a well-formed frame, or
    /// [`Error::Io`] if the receive fails.
    pub async fn recv_from(&mut self) -> Result<(Frame, SocketAddr)> {
        let (len, peer) = self.socket.recv_from(&mut self.buffer).await?;

        if len > self.config.max_datagram {
            return Err(Error::DatagramTooLarge {
                len,
                max: self.config.max_datagram,
            });
        }

        let (frame, _consumed) =
            Frame::decode_with_limit(&self.buffer[..len], self.config.max_payload_len)?;

        Ok((frame, peer))
    }

    /// Consumes the endpoint, returning the underlying socket.
    #[must_use]
    pub fn into_inner(self) -> UdpSocket {
        self.socket
    }
}
