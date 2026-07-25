//! # nexusnet-transport
//!
//! Transport-layer connectivity for NexusNet: carrying protocol frames over
//! real sockets.
//!
//! This crate is where [`nexusnet_protocol`]'s incremental decoder finally does
//! its job. A TCP read returns whatever bytes happen to have arrived — half a
//! frame, or three frames and a fragment — and [`Connection`] turns that stream
//! back into whole frames.
//!
//! ## What's here
//!
//! * [`Connection`] — a framed connection generic over any async stream. TCP
//!   today; TLS and QUIC attach to the same type later.
//! * [`TcpListener`] and [`tcp::connect`] — the stream transport.
//! * [`UdpEndpoint`] — a datagram transport, one frame per datagram.
//! * [`ConnectionPool`] — reusable connections with idle expiry and automatic
//!   removal of connections left desynchronized by a failure.
//! * [`ReconnectPolicy`] — exponential backoff with jitter for dialing.
//! * [`Session`] — stream multiplexing, carrying many logical streams over one
//!   connection.
//! * [`TransportConfig`] — payload limits, buffer sizes, timeouts, and socket
//!   options.
//!
//! ## Example
//!
//! ```
//! # use bytes::Bytes;
//! # use nexusnet_protocol::{Frame, FrameType};
//! # use nexusnet_transport::{tcp, TcpListener, TransportConfig};
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), nexusnet_transport::Error> {
//! let config = TransportConfig::default();
//! let listener = TcpListener::bind("127.0.0.1:0", config).await?;
//! let address = listener.local_addr()?;
//!
//! let server = tokio::spawn(async move {
//!     let (mut connection, _peer) = listener.accept().await?;
//!     while let Some(frame) = connection.recv().await? {
//!         connection.send(&frame).await?;
//!     }
//!     Ok::<_, nexusnet_transport::Error>(())
//! });
//!
//! let mut client = tcp::connect(address, config).await?;
//! client.send(&Frame::new(FrameType::Data, 1, Bytes::from_static(b"hello"))?).await?;
//!
//! let echoed = client.recv().await?.expect("the echo arrives");
//! assert_eq!(echoed.payload().as_ref(), b"hello");
//!
//! client.shutdown().await?;
//! server.await.expect("the server task completes")?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Closing semantics
//!
//! A clean close at a frame boundary is `Ok(None)` from [`Connection::recv`];
//! a close *mid-frame* is [`Error::UnexpectedEof`]. Conflating the two would
//! turn silent data loss into an ordinary end-of-stream, so they stay distinct.
//! [`Error::is_fatal`] reports whether a connection can be reused after an
//! error.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod config;
mod connection;
mod mux;
mod pool;
mod reconnect;
pub mod tcp;
mod udp;

pub use crate::config::{
    Error, Result, TransportConfig, DEFAULT_CONNECT_TIMEOUT, DEFAULT_MAX_DATAGRAM,
    DEFAULT_READ_BUFFER,
};
pub use crate::connection::{Connection, ConnectionReader, ConnectionWriter};
pub use crate::mux::{
    Role, Session, SessionConfig, SessionDriver, SessionHandle, SessionStats, Stream,
    CONTROL_STREAM_ID, DEFAULT_MAX_STREAMS, DEFAULT_OUTBOUND_BUFFER, DEFAULT_STREAM_BUFFER,
};
pub use crate::pool::{
    ConnectionPool, PoolConfig, PoolStats, PooledConnection, DEFAULT_MAX_IDLE, DEFAULT_POOL_SIZE,
};
pub use crate::reconnect::{
    connect_with_retry, is_retryable, ReconnectPolicy, DEFAULT_INITIAL_DELAY, DEFAULT_MAX_DELAY,
    DEFAULT_MULTIPLIER,
};
pub use crate::tcp::{TcpConnection, TcpListener};
pub use crate::udp::UdpEndpoint;
