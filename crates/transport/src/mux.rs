//! Stream multiplexing: many logical streams over one connection.
//!
//! A single TCP connection can carry many independent conversations if each
//! frame names the stream it belongs to. That is what the protocol header's
//! `stream_id` is for, and this module turns it into an API.
//!
//! ## Stream identifiers
//!
//! Both peers open streams, so both allocate identifiers, and they must not
//! collide. The convention here is the one HTTP/2 and QUIC use: the initiator's
//! side determines parity. A client allocates odd identifiers, a server
//! allocates even ones, and identifier `0` is reserved for connection-level
//! control frames such as ping. No negotiation is needed and no collision is
//! possible.
//!
//! ## Architecture
//!
//! [`Session::start`] splits the connection and returns a cheap-to-clone
//! [`SessionHandle`] plus a [`SessionDriver`]. The driver owns the I/O: it
//! reads frames and routes them to the right stream, and it serializes
//! outbound frames from every stream onto the single connection. Nothing else
//! touches the socket, so no locking is needed on the hot path.
//!
//! ## Flow control
//!
//! Per-stream inbound channels are bounded, which bounds memory. A consumer
//! that stops reading will therefore eventually stall the driver, which stalls
//! *all* streams — classic head-of-line blocking. Real per-stream flow control
//! (a credit window, as HTTP/2 and QUIC use) belongs with the scheduler work in
//! a later phase; until then, consume promptly or raise
//! [`SessionConfig::stream_buffer`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use nexusnet_protocol::{Frame, FrameFlags, FrameType};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::config::{Error, Result};
use crate::connection::{Connection, ConnectionReader, ConnectionWriter};

/// The stream identifier reserved for connection-level control frames.
pub const CONTROL_STREAM_ID: u32 = 0;

/// The default number of inbound payloads buffered per stream.
pub const DEFAULT_STREAM_BUFFER: usize = 32;

/// The default number of outbound frames buffered across the session.
pub const DEFAULT_OUTBOUND_BUFFER: usize = 256;

/// The default maximum number of concurrently open streams.
pub const DEFAULT_MAX_STREAMS: usize = 256;

/// Which side of a connection this session is.
///
/// Determines the parity of locally allocated stream identifiers, which is what
/// keeps the two peers from choosing the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// The dialing side. Allocates odd stream identifiers.
    Client,
    /// The listening side. Allocates even stream identifiers.
    Server,
}

impl Role {
    /// Returns the first stream identifier this role allocates.
    #[must_use]
    pub const fn first_stream_id(self) -> u32 {
        match self {
            Self::Client => 1,
            Self::Server => 2,
        }
    }

    /// Returns `true` if `stream_id` was allocated by this role.
    #[must_use]
    pub const fn owns(self, stream_id: u32) -> bool {
        if stream_id == CONTROL_STREAM_ID {
            return false;
        }

        match self {
            Self::Client => stream_id % 2 == 1,
            Self::Server => stream_id % 2 == 0,
        }
    }

    /// Returns the opposite role.
    #[must_use]
    pub const fn peer(self) -> Self {
        match self {
            Self::Client => Self::Server,
            Self::Server => Self::Client,
        }
    }
}

/// Configuration for a multiplexed session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionConfig {
    /// Inbound payloads buffered per stream before backpressure applies.
    pub stream_buffer: usize,
    /// Outbound frames buffered across the whole session.
    pub outbound_buffer: usize,
    /// The maximum number of concurrently open streams.
    pub max_streams: usize,
    /// Whether to answer inbound pings automatically.
    pub auto_pong: bool,
}

impl SessionConfig {
    /// Creates a configuration with default values.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            stream_buffer: DEFAULT_STREAM_BUFFER,
            outbound_buffer: DEFAULT_OUTBOUND_BUFFER,
            max_streams: DEFAULT_MAX_STREAMS,
            auto_pong: true,
        }
    }

    /// Sets the per-stream inbound buffer.
    ///
    /// Zero is raised to one, since a zero-capacity channel cannot buffer.
    #[must_use]
    pub const fn with_stream_buffer(mut self, stream_buffer: usize) -> Self {
        self.stream_buffer = if stream_buffer == 0 { 1 } else { stream_buffer };
        self
    }

    /// Sets the session-wide outbound buffer.
    ///
    /// Zero is raised to one.
    #[must_use]
    pub const fn with_outbound_buffer(mut self, outbound_buffer: usize) -> Self {
        self.outbound_buffer = if outbound_buffer == 0 {
            1
        } else {
            outbound_buffer
        };
        self
    }

    /// Sets the maximum number of concurrent streams.
    #[must_use]
    pub const fn with_max_streams(mut self, max_streams: usize) -> Self {
        self.max_streams = max_streams;
        self
    }

    /// Sets whether inbound pings are answered automatically.
    #[must_use]
    pub const fn with_auto_pong(mut self, auto_pong: bool) -> Self {
        self.auto_pong = auto_pong;
        self
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// A snapshot of session activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SessionStats {
    /// Streams opened locally.
    pub streams_opened: u64,
    /// Streams opened by the peer.
    pub streams_accepted: u64,
    /// Streams that have closed.
    pub streams_closed: u64,
    /// Frames dropped because they named an unknown, already-closed stream.
    pub frames_dropped: u64,
    /// Streams currently open.
    pub streams_active: usize,
}

/// Shared state between the handle and the driver.
#[derive(Debug)]
struct Shared {
    role: Role,
    config: SessionConfig,
    next_id: AtomicU32,
    streams: Mutex<HashMap<u32, mpsc::Sender<Bytes>>>,
    streams_opened: AtomicU64,
    streams_accepted: AtomicU64,
    streams_closed: AtomicU64,
    frames_dropped: AtomicU64,
    shutdown: tokio::sync::Notify,
}

impl Shared {
    fn lock_streams(&self) -> std::sync::MutexGuard<'_, HashMap<u32, mpsc::Sender<Bytes>>> {
        self.streams.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Allocates the next identifier of this role's parity.
    ///
    /// Identifiers advance by two, so they never collide with the peer's.
    fn allocate_id(&self) -> u32 {
        self.next_id.fetch_add(2, Ordering::Relaxed)
    }

    fn close_stream(&self, stream_id: u32) {
        if self.lock_streams().remove(&stream_id).is_some() {
            self.streams_closed.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A multiplexed session over a single connection.
///
/// # Examples
///
/// ```
/// # use bytes::Bytes;
/// # use nexusnet_transport::{Connection, Role, Session, SessionConfig, TransportConfig};
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), nexusnet_transport::Error> {
/// let (client_io, server_io) = tokio::io::duplex(64 * 1024);
/// let config = TransportConfig::default();
///
/// let (client, client_driver) = Session::start(
///     Connection::new(client_io, config),
///     Role::Client,
///     SessionConfig::default(),
/// );
/// let (server, server_driver) = Session::start(
///     Connection::new(server_io, config),
///     Role::Server,
///     SessionConfig::default(),
/// );
///
/// tokio::spawn(client_driver.run());
/// tokio::spawn(server_driver.run());
///
/// // Two independent streams over one connection.
/// let mut first = client.open_stream()?;
/// let mut second = client.open_stream()?;
/// assert_eq!(first.id(), 1);
/// assert_eq!(second.id(), 3); // Odd identifiers: this side is the client.
///
/// first.send(Bytes::from_static(b"one")).await?;
/// second.send(Bytes::from_static(b"two")).await?;
///
/// let mut accepted = server.accept_stream().await.expect("a stream arrives");
/// assert_eq!(accepted.recv().await, Some(Bytes::from_static(b"one")));
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Session;

impl Session {
    /// Creates a session over `connection`.
    ///
    /// Returns a handle for opening and accepting streams, and a driver that
    /// must be run — usually with `tokio::spawn(driver.run())` — for any I/O to
    /// happen.
    #[must_use]
    pub fn start<S>(
        connection: Connection<S>,
        role: Role,
        config: SessionConfig,
    ) -> (
        SessionHandle,
        SessionDriver<tokio::io::ReadHalf<S>, tokio::io::WriteHalf<S>>,
    )
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (reader, writer) = connection.split();

        let shared = Arc::new(Shared {
            role,
            config,
            next_id: AtomicU32::new(role.first_stream_id()),
            streams: Mutex::new(HashMap::new()),
            streams_opened: AtomicU64::new(0),
            streams_accepted: AtomicU64::new(0),
            streams_closed: AtomicU64::new(0),
            frames_dropped: AtomicU64::new(0),
            shutdown: tokio::sync::Notify::new(),
        });

        let (outbound_tx, outbound_rx) = mpsc::channel(config.outbound_buffer);
        let (accept_tx, accept_rx) = mpsc::channel(config.max_streams.max(1));

        let handle = SessionHandle {
            shared: Arc::clone(&shared),
            outbound: outbound_tx.clone(),
            incoming: Arc::new(tokio::sync::Mutex::new(accept_rx)),
        };

        let driver = SessionDriver {
            shared,
            reader,
            writer,
            outbound_rx,
            outbound_tx,
            accept_tx,
        };

        (handle, driver)
    }
}

/// A cheap-to-clone handle for opening and accepting streams.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    shared: Arc<Shared>,
    outbound: mpsc::Sender<Frame>,
    incoming: Arc<tokio::sync::Mutex<mpsc::Receiver<Stream>>>,
}

impl SessionHandle {
    /// Returns this session's role.
    #[must_use]
    pub fn role(&self) -> Role {
        self.shared.role
    }

    /// Returns a snapshot of session activity.
    #[must_use]
    pub fn stats(&self) -> SessionStats {
        SessionStats {
            streams_opened: self.shared.streams_opened.load(Ordering::Relaxed),
            streams_accepted: self.shared.streams_accepted.load(Ordering::Relaxed),
            streams_closed: self.shared.streams_closed.load(Ordering::Relaxed),
            frames_dropped: self.shared.frames_dropped.load(Ordering::Relaxed),
            streams_active: self.shared.lock_streams().len(),
        }
    }

    /// Opens a new outbound stream.
    ///
    /// The peer learns of the stream when its first frame arrives, so opening
    /// costs no round trip.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TooManyStreams`] if the configured stream limit is
    /// already reached.
    pub fn open_stream(&self) -> Result<Stream> {
        let (tx, rx) = mpsc::channel(self.shared.config.stream_buffer);

        let stream_id = {
            let mut streams = self.shared.lock_streams();
            if streams.len() >= self.shared.config.max_streams {
                return Err(Error::TooManyStreams {
                    max: self.shared.config.max_streams,
                });
            }

            let stream_id = self.shared.allocate_id();
            streams.insert(stream_id, tx);
            stream_id
        };

        self.shared.streams_opened.fetch_add(1, Ordering::Relaxed);

        Ok(Stream {
            id: stream_id,
            shared: Arc::clone(&self.shared),
            outbound: self.outbound.clone(),
            inbound: rx,
            closed: false,
        })
    }

    /// Waits for the peer to open a stream.
    ///
    /// Returns `None` once the session has ended.
    pub async fn accept_stream(&self) -> Option<Stream> {
        self.incoming.lock().await.recv().await
    }

    /// Sends a ping on the control stream.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SessionClosed`] if the driver has stopped.
    pub async fn ping(&self, payload: Bytes) -> Result<()> {
        let frame = Frame::new(FrameType::Ping, CONTROL_STREAM_ID, payload)?;
        self.outbound
            .send(frame)
            .await
            .map_err(|_| Error::SessionClosed)
    }

    /// Returns `true` if the session driver is still running.
    #[must_use]
    pub fn is_open(&self) -> bool {
        !self.outbound.is_closed()
    }

    /// Asks the session driver to stop.
    ///
    /// In-flight frames already queued may not be written. For an orderly
    /// finish, close each stream first and let the peer hang up.
    pub fn close(&self) {
        self.shared.shutdown.notify_waiters();
    }
}

/// Drives the session's I/O.
///
/// Nothing happens on the connection until [`run`](SessionDriver::run) is
/// polled, normally via `tokio::spawn`.
#[derive(Debug)]
pub struct SessionDriver<R, W> {
    shared: Arc<Shared>,
    reader: ConnectionReader<R>,
    writer: ConnectionWriter<W>,
    outbound_rx: mpsc::Receiver<Frame>,
    outbound_tx: mpsc::Sender<Frame>,
    accept_tx: mpsc::Sender<Stream>,
}

impl<R, W> SessionDriver<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    /// Runs the session until the connection closes or fails.
    ///
    /// Returns `Ok(())` on a clean shutdown by either side.
    ///
    /// # Errors
    ///
    /// Returns a transport error if the connection fails in a way that is not a
    /// clean close, such as a protocol violation by the peer.
    pub async fn run(mut self) -> Result<()> {
        loop {
            tokio::select! {
                inbound = self.reader.recv() => {
                    match inbound? {
                        Some(frame) => self.route(frame).await?,
                        // The peer closed cleanly.
                        None => break,
                    }
                }
                outbound = self.outbound_rx.recv() => {
                    match outbound {
                        Some(frame) => self.writer.send(&frame).await?,
                        // The driver holds a sender, so this cannot happen;
                        // treat it as a shutdown rather than looping hot.
                        None => break,
                    }
                }
                () = self.shared.shutdown.notified() => break,
            }
        }

        // Closing the registry drops every per-stream sender, which ends each
        // stream's `recv` cleanly rather than leaving it hanging.
        self.shared.lock_streams().clear();
        let _ = self.writer.shutdown().await;

        Ok(())
    }

    /// Delivers one inbound frame to its destination.
    async fn route(&mut self, frame: Frame) -> Result<()> {
        let stream_id = frame.header().stream_id;
        let end_of_stream = frame.header().flags.contains(FrameFlags::END_OF_STREAM);

        match frame.header().frame_type {
            FrameType::Ping => {
                if self.shared.config.auto_pong {
                    let pong =
                        Frame::new(FrameType::Pong, CONTROL_STREAM_ID, frame.into_payload())?;
                    self.writer.send(&pong).await?;
                }
                return Ok(());
            }
            FrameType::Pong | FrameType::Handshake | FrameType::Control => return Ok(()),
            FrameType::Close => {
                self.shared.close_stream(stream_id);
                return Ok(());
            }
            FrameType::Data => {}
            // `FrameType` is non-exhaustive: a future frame type this build
            // does not understand is ignored rather than misrouted.
            _ => {
                self.shared.frames_dropped.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        }

        if stream_id == CONTROL_STREAM_ID {
            return Ok(());
        }

        let known = self.shared.lock_streams().contains_key(&stream_id);
        if !known {
            // A frame on an unknown identifier opens a stream, but only if the
            // peer owns that identifier's parity. Otherwise it names a stream
            // we closed, and the frame is stale.
            if self.shared.role.peer().owns(stream_id) {
                self.open_inbound(stream_id).await?;
            } else {
                self.shared.frames_dropped.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        }

        let sender = self.shared.lock_streams().get(&stream_id).cloned();
        if let Some(sender) = sender {
            let payload = frame.into_payload();
            if !payload.is_empty() && sender.send(payload).await.is_err() {
                // The application dropped its Stream; forget the registration.
                self.shared.close_stream(stream_id);
            }
        } else {
            self.shared.frames_dropped.fetch_add(1, Ordering::Relaxed);
        }

        if end_of_stream {
            self.shared.close_stream(stream_id);
        }

        Ok(())
    }

    /// Registers a peer-initiated stream and hands it to `accept_stream`.
    async fn open_inbound(&mut self, stream_id: u32) -> Result<()> {
        let (tx, rx) = mpsc::channel(self.shared.config.stream_buffer);

        {
            let mut streams = self.shared.lock_streams();
            if streams.len() >= self.shared.config.max_streams {
                self.shared.frames_dropped.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
            streams.insert(stream_id, tx);
        }

        self.shared.streams_accepted.fetch_add(1, Ordering::Relaxed);

        let stream = Stream {
            id: stream_id,
            shared: Arc::clone(&self.shared),
            outbound: self.outbound_tx.clone(),
            inbound: rx,
            closed: false,
        };

        if self.accept_tx.send(stream).await.is_err() {
            // Nobody is accepting; keep the registration so data is not lost
            // for a listener that appears later.
            tracing::debug!(stream_id, "no acceptor for inbound stream");
        }

        Ok(())
    }
}

/// One logical stream within a session.
#[derive(Debug)]
pub struct Stream {
    id: u32,
    shared: Arc<Shared>,
    outbound: mpsc::Sender<Frame>,
    inbound: mpsc::Receiver<Bytes>,
    closed: bool,
}

impl Stream {
    /// Returns this stream's identifier.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.id
    }

    /// Returns `true` once the stream has been closed locally.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Sends a payload on this stream.
    ///
    /// # Errors
    ///
    /// Returns [`Error::StreamClosed`] if the stream was closed locally, or
    /// [`Error::SessionClosed`] if the session driver has stopped.
    pub async fn send(&mut self, payload: Bytes) -> Result<()> {
        if self.closed {
            return Err(Error::StreamClosed { stream_id: self.id });
        }

        let frame = Frame::new(FrameType::Data, self.id, payload)?;
        self.outbound
            .send(frame)
            .await
            .map_err(|_| Error::SessionClosed)
    }

    /// Receives the next payload, or `None` once the stream ends.
    pub async fn recv(&mut self) -> Option<Bytes> {
        self.inbound.recv().await
    }

    /// Closes the stream, signalling end-of-stream to the peer.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SessionClosed`] if the session driver has stopped.
    pub async fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;

        let frame = Frame::new(FrameType::Data, self.id, Bytes::new())?
            .with_flags(FrameFlags::END_OF_STREAM);

        let result = self
            .outbound
            .send(frame)
            .await
            .map_err(|_| Error::SessionClosed);

        self.shared.close_stream(self.id);
        self.inbound.close();

        result
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // Deregister so the driver stops routing to a stream nobody holds.
        self.shared.close_stream(self.id);
    }
}
