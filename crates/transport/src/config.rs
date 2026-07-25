//! Transport errors and configuration.

use std::result::Result as StdResult;
use std::time::Duration;

/// A specialized [`Result`](std::result::Result) for transport operations.
pub type Result<T> = StdResult<T, Error>;

/// An error produced while establishing or using a transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An underlying I/O operation failed.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// A frame could not be encoded or decoded.
    ///
    /// On a stream transport this desynchronizes the connection, so the
    /// connection should be closed rather than reused.
    #[error("protocol error: {0}")]
    Protocol(#[from] nexusnet_protocol::Error),

    /// The peer closed the connection in the middle of a frame.
    ///
    /// Distinguished from a clean shutdown, which is reported as `Ok(None)`
    /// from [`Connection::recv`](crate::Connection::recv).
    #[error("connection closed mid-frame with {buffered} bytes buffered")]
    UnexpectedEof {
        /// How many bytes of an incomplete frame were buffered.
        buffered: usize,
    },

    /// Establishing a connection took longer than the configured timeout.
    #[error("timed out connecting to {address} after {}ms", timeout.as_millis())]
    ConnectTimeout {
        /// The address that was being dialed.
        address: String,
        /// The timeout that elapsed.
        timeout: Duration,
    },

    /// The engine refused a lifecycle transition.
    ///
    /// For example, starting a server whose engine has already been shut down.
    #[error("engine error: {0}")]
    Engine(#[from] nexusnet_core::Error),

    /// The session already has as many open streams as it permits.
    #[error("cannot open another stream: the limit of {max} is reached")]
    TooManyStreams {
        /// The configured maximum number of concurrent streams.
        max: usize,
    },

    /// The session driver has stopped, so no frame can be sent.
    #[error("the session has closed")]
    SessionClosed,

    /// The stream was closed locally and cannot be written to.
    #[error("stream {stream_id} is closed")]
    StreamClosed {
        /// The identifier of the closed stream.
        stream_id: u32,
    },

    /// A datagram did not fit the receive buffer and was truncated.
    ///
    /// Unlike a stream, a datagram cannot be reassembled from parts, so an
    /// oversized datagram is unrecoverable rather than merely deferred.
    #[error("datagram of at least {len} bytes exceeds the {max} byte limit")]
    DatagramTooLarge {
        /// The observed datagram length.
        len: usize,
        /// The configured maximum datagram size.
        max: usize,
    },
}

impl Error {
    /// Returns `true` when the error leaves the connection unusable.
    ///
    /// Protocol errors and unexpected end-of-file desynchronize a stream, so
    /// the connection must be closed. Timeouts and oversized datagrams do not
    /// invalidate the endpoint itself.
    #[must_use]
    pub const fn is_fatal(&self) -> bool {
        matches!(self, Self::Protocol(_) | Self::UnexpectedEof { .. })
    }
}

/// The default read buffer size: 64 KiB.
///
/// Large enough to absorb several frames per syscall without committing much
/// memory per idle connection.
pub const DEFAULT_READ_BUFFER: usize = 64 * 1024;

/// The default maximum datagram size: 64 KiB, the practical UDP payload ceiling.
pub const DEFAULT_MAX_DATAGRAM: usize = 64 * 1024;

/// The default connect timeout.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Configuration shared by the transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TransportConfig {
    /// The largest frame payload accepted from a peer.
    ///
    /// Enforced by the frame decoder before any payload is buffered.
    pub max_payload_len: u32,
    /// The size of the buffer used for each read syscall.
    pub read_buffer: usize,
    /// The largest datagram accepted on a datagram transport.
    pub max_datagram: usize,
    /// How long to wait for a connection to be established.
    pub connect_timeout: Duration,
    /// Whether to disable Nagle's algorithm on TCP connections.
    ///
    /// Defaults to `true`: NexusNet sends discrete frames, and Nagle delays
    /// small writes hoping to coalesce them, which adds latency for no benefit.
    pub nodelay: bool,
}

impl TransportConfig {
    /// Creates a configuration with default values.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_payload_len: nexusnet_protocol::DEFAULT_MAX_PAYLOAD_LEN,
            read_buffer: DEFAULT_READ_BUFFER,
            max_datagram: DEFAULT_MAX_DATAGRAM,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            nodelay: true,
        }
    }

    /// Sets the maximum accepted frame payload length.
    #[must_use]
    pub const fn with_max_payload_len(mut self, max_payload_len: u32) -> Self {
        self.max_payload_len = max_payload_len;
        self
    }

    /// Sets the per-read buffer size.
    ///
    /// A zero value is replaced with 1, since a zero-length read would spin
    /// without making progress.
    #[must_use]
    pub const fn with_read_buffer(mut self, read_buffer: usize) -> Self {
        self.read_buffer = if read_buffer == 0 { 1 } else { read_buffer };
        self
    }

    /// Sets the maximum accepted datagram size.
    #[must_use]
    pub const fn with_max_datagram(mut self, max_datagram: usize) -> Self {
        self.max_datagram = max_datagram;
        self
    }

    /// Sets the connect timeout.
    #[must_use]
    pub const fn with_connect_timeout(mut self, connect_timeout: Duration) -> Self {
        self.connect_timeout = connect_timeout;
        self
    }

    /// Sets whether Nagle's algorithm is disabled.
    #[must_use]
    pub const fn with_nodelay(mut self, nodelay: bool) -> Self {
        self.nodelay = nodelay;
        self
    }
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let config = TransportConfig::default();
        assert_eq!(config.read_buffer, DEFAULT_READ_BUFFER);
        assert_eq!(config.connect_timeout, DEFAULT_CONNECT_TIMEOUT);
        assert!(config.nodelay, "framed protocols should not wait on Nagle");
    }

    #[test]
    fn builder_methods_override_defaults() {
        let config = TransportConfig::new()
            .with_max_payload_len(1024)
            .with_read_buffer(4096)
            .with_max_datagram(2048)
            .with_connect_timeout(Duration::from_millis(250))
            .with_nodelay(false);

        assert_eq!(config.max_payload_len, 1024);
        assert_eq!(config.read_buffer, 4096);
        assert_eq!(config.max_datagram, 2048);
        assert_eq!(config.connect_timeout, Duration::from_millis(250));
        assert!(!config.nodelay);
    }

    #[test]
    fn zero_read_buffer_is_corrected() {
        assert_eq!(TransportConfig::new().with_read_buffer(0).read_buffer, 1);
    }

    #[test]
    fn fatality_is_classified() {
        let protocol = Error::Protocol(nexusnet_protocol::Error::NoCommonVersion);
        assert!(protocol.is_fatal());
        assert!(Error::UnexpectedEof { buffered: 4 }.is_fatal());

        assert!(!Error::DatagramTooLarge { len: 100, max: 50 }.is_fatal());
    }
}
