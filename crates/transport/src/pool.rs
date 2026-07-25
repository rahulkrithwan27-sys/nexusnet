//! A pool of reusable TCP connections.
//!
//! Establishing a TCP connection costs a round trip before any data moves, and
//! more if TLS is layered on later. Pooling amortizes that across requests.
//!
//! Two details make a pool correct rather than merely fast:
//!
//! * **Idle connections go stale.** A peer, load balancer, or NAT device will
//!   drop an idle connection without telling us. Reusing one that has silently
//!   died turns a fresh request into a mysterious failure, so connections older
//!   than a configured idle window are discarded rather than handed out.
//! * **A failed connection must not go back.** After a protocol error or a
//!   truncated read the stream is desynchronized; returning it to the pool
//!   would spread that failure to an unrelated caller. [`PooledConnection`]
//!   watches for fatal errors and drops such connections automatically.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use nexusnet_protocol::Frame;

use crate::config::{Result, TransportConfig};
use crate::reconnect::{connect_with_retry, ReconnectPolicy};
use crate::tcp::TcpConnection;

/// The default maximum number of idle connections retained.
pub const DEFAULT_POOL_SIZE: usize = 16;

/// The default idle window after which a pooled connection is discarded.
pub const DEFAULT_MAX_IDLE: Duration = Duration::from_secs(60);

/// Configuration for a [`ConnectionPool`].
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct PoolConfig {
    /// The maximum number of idle connections retained.
    ///
    /// This bounds *idle* connections, not connections in use; checked-out
    /// connections beyond this are simply not returned to the pool.
    pub max_idle_connections: usize,
    /// How long a connection may sit idle before it is discarded.
    pub max_idle_time: Duration,
    /// Configuration applied to newly created connections.
    pub transport: TransportConfig,
    /// The retry policy used when establishing new connections.
    pub reconnect: ReconnectPolicy,
}

impl PoolConfig {
    /// Creates a configuration with default values.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_idle_connections: DEFAULT_POOL_SIZE,
            max_idle_time: DEFAULT_MAX_IDLE,
            transport: TransportConfig::new(),
            reconnect: ReconnectPolicy::new(),
        }
    }

    /// Sets the maximum number of idle connections.
    #[must_use]
    pub const fn with_max_idle_connections(mut self, max_idle_connections: usize) -> Self {
        self.max_idle_connections = max_idle_connections;
        self
    }

    /// Sets how long a connection may sit idle.
    #[must_use]
    pub const fn with_max_idle_time(mut self, max_idle_time: Duration) -> Self {
        self.max_idle_time = max_idle_time;
        self
    }

    /// Sets the transport configuration for new connections.
    #[must_use]
    pub const fn with_transport(mut self, transport: TransportConfig) -> Self {
        self.transport = transport;
        self
    }

    /// Sets the reconnection policy for new connections.
    #[must_use]
    pub const fn with_reconnect(mut self, reconnect: ReconnectPolicy) -> Self {
        self.reconnect = reconnect;
        self
    }
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// A snapshot of pool activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct PoolStats {
    /// Connections established because none could be reused.
    pub created: u64,
    /// Checkouts satisfied from the idle set.
    pub reused: u64,
    /// Connections dropped after a fatal error.
    pub discarded: u64,
    /// Connections dropped because they had been idle too long.
    pub expired: u64,
    /// Connections currently idle in the pool.
    pub idle: usize,
}

impl PoolStats {
    /// Returns the fraction of checkouts served without a new connection.
    ///
    /// Returns `0.0` before any checkout has happened.
    #[must_use]
    pub fn reuse_ratio(&self) -> f64 {
        let total = self.created + self.reused;
        if total == 0 {
            0.0
        } else {
            self.reused as f64 / total as f64
        }
    }
}

/// A connection stored in the pool alongside when it was last used.
#[derive(Debug)]
struct Idle {
    connection: TcpConnection,
    returned_at: Instant,
}

/// A pool of reusable connections to a single peer.
///
/// # Examples
///
/// ```
/// # use bytes::Bytes;
/// # use nexusnet_protocol::{Frame, FrameType};
/// # use nexusnet_transport::{ConnectionPool, PoolConfig, TcpListener, TransportConfig};
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), nexusnet_transport::Error> {
/// let listener = TcpListener::bind("127.0.0.1:0", TransportConfig::default()).await?;
/// let address = listener.local_addr()?;
///
/// tokio::spawn(async move {
///     loop {
///         let (mut connection, _peer) = listener.accept().await?;
///         tokio::spawn(async move {
///             while let Ok(Some(frame)) = connection.recv().await {
///                 if connection.send(&frame).await.is_err() {
///                     break;
///                 }
///             }
///         });
///     }
///     #[allow(unreachable_code)]
///     Ok::<_, nexusnet_transport::Error>(())
/// });
///
/// let pool = ConnectionPool::new(address, PoolConfig::default());
///
/// // The first checkout dials; the second reuses the same connection.
/// for _ in 0..2 {
///     let mut connection = pool.get().await?;
///     connection.send(&Frame::new(FrameType::Data, 1, Bytes::from_static(b"hi"))?).await?;
///     connection.recv().await?;
/// }
///
/// assert_eq!(pool.stats().created, 1);
/// assert_eq!(pool.stats().reused, 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct ConnectionPool {
    address: SocketAddr,
    config: PoolConfig,
    idle: Mutex<VecDeque<Idle>>,
    created: AtomicU64,
    reused: AtomicU64,
    discarded: AtomicU64,
    expired: AtomicU64,
}

impl ConnectionPool {
    /// Creates an empty pool targeting `address`.
    ///
    /// No connection is established until the first [`get`](Self::get).
    #[must_use]
    pub fn new(address: SocketAddr, config: PoolConfig) -> Arc<Self> {
        Arc::new(Self {
            address,
            config,
            idle: Mutex::new(VecDeque::new()),
            created: AtomicU64::new(0),
            reused: AtomicU64::new(0),
            discarded: AtomicU64::new(0),
            expired: AtomicU64::new(0),
        })
    }

    /// Returns the address this pool connects to.
    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Returns the pool configuration.
    #[must_use]
    pub const fn config(&self) -> &PoolConfig {
        &self.config
    }

    /// Returns a snapshot of pool activity.
    #[must_use]
    pub fn stats(&self) -> PoolStats {
        PoolStats {
            created: self.created.load(Ordering::Relaxed),
            reused: self.reused.load(Ordering::Relaxed),
            discarded: self.discarded.load(Ordering::Relaxed),
            expired: self.expired.load(Ordering::Relaxed),
            idle: self.lock_idle().len(),
        }
    }

    /// Locks the idle set, recovering from a poisoned mutex.
    ///
    /// A panic elsewhere should not render the pool permanently unusable; the
    /// queue is a plain collection with no invariant a panic could break.
    fn lock_idle(&self) -> std::sync::MutexGuard<'_, VecDeque<Idle>> {
        self.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes a live connection from the idle set, discarding expired ones.
    fn take_idle(&self) -> Option<TcpConnection> {
        let mut idle = self.lock_idle();
        let mut expired = 0_u64;

        // Newest connections are at the front, so the first live one wins.
        let found = loop {
            let Some(entry) = idle.pop_front() else {
                break None;
            };

            if entry.returned_at.elapsed() <= self.config.max_idle_time {
                break Some(entry.connection);
            }
            expired += 1;
        };

        if expired > 0 {
            self.expired.fetch_add(expired, Ordering::Relaxed);
        }

        found
    }

    /// Checks out a connection, reusing an idle one when possible.
    ///
    /// # Errors
    ///
    /// Returns an error if no idle connection is available and a new one cannot
    /// be established within the configured retry policy.
    pub async fn get(self: &Arc<Self>) -> Result<PooledConnection> {
        if let Some(connection) = self.take_idle() {
            self.reused.fetch_add(1, Ordering::Relaxed);
            return Ok(PooledConnection::new(connection, Arc::clone(self)));
        }

        let connection =
            connect_with_retry(self.address, self.config.transport, self.config.reconnect).await?;
        self.created.fetch_add(1, Ordering::Relaxed);

        Ok(PooledConnection::new(connection, Arc::clone(self)))
    }

    /// Returns a healthy connection to the idle set.
    fn recycle(&self, connection: TcpConnection) {
        let mut idle = self.lock_idle();

        if idle.len() >= self.config.max_idle_connections {
            // The pool is full; let this connection close.
            self.discarded.fetch_add(1, Ordering::Relaxed);
            return;
        }

        idle.push_front(Idle {
            connection,
            returned_at: Instant::now(),
        });
    }

    /// Closes every idle connection.
    pub fn clear(&self) {
        let mut idle = self.lock_idle();
        let dropped = idle.len() as u64;
        idle.clear();

        if dropped > 0 {
            self.discarded.fetch_add(dropped, Ordering::Relaxed);
        }
    }
}

/// A connection checked out of a [`ConnectionPool`].
///
/// Returns itself to the pool when dropped, unless a fatal error occurred or
/// it was explicitly discarded. Fatal errors are detected automatically: the
/// forwarding methods here mark the connection broken when
/// [`Error::is_fatal`](crate::Error::is_fatal) reports the stream is
/// desynchronized.
#[derive(Debug)]
pub struct PooledConnection {
    connection: Option<TcpConnection>,
    pool: Arc<ConnectionPool>,
    broken: bool,
}

impl PooledConnection {
    fn new(connection: TcpConnection, pool: Arc<ConnectionPool>) -> Self {
        Self {
            connection: Some(connection),
            pool,
            broken: false,
        }
    }

    /// Returns a reference to the underlying connection.
    ///
    /// # Panics
    ///
    /// Panics only if called after the connection has been taken, which the
    /// public API does not permit.
    #[must_use]
    pub fn connection(&self) -> &TcpConnection {
        self.connection
            .as_ref()
            .expect("a checked-out connection is always present")
    }

    /// Returns a mutable reference to the underlying connection.
    ///
    /// Prefer the forwarding methods, which also track connection health.
    ///
    /// # Panics
    ///
    /// Panics only if called after the connection has been taken, which the
    /// public API does not permit.
    pub fn connection_mut(&mut self) -> &mut TcpConnection {
        self.connection
            .as_mut()
            .expect("a checked-out connection is always present")
    }

    /// Marks this connection as unfit for reuse.
    ///
    /// Call this after any application-level problem the transport cannot see,
    /// such as a peer signalling that it is shutting down.
    pub fn mark_broken(&mut self) {
        self.broken = true;
    }

    /// Returns `true` if this connection will not be returned to the pool.
    #[must_use]
    pub const fn is_broken(&self) -> bool {
        self.broken
    }

    /// Sends a frame, marking the connection broken on a fatal error.
    ///
    /// # Errors
    ///
    /// Returns any error from the underlying connection.
    pub async fn send(&mut self, frame: &Frame) -> Result<()> {
        let result = self.connection_mut().send(frame).await;
        self.note(&result);
        result
    }

    /// Sends several frames in one write, marking the connection broken on a
    /// fatal error.
    ///
    /// # Errors
    ///
    /// Returns any error from the underlying connection.
    pub async fn send_all(&mut self, frames: &[Frame]) -> Result<()> {
        let result = self.connection_mut().send_all(frames).await;
        self.note(&result);
        result
    }

    /// Receives a frame, marking the connection broken on a fatal error.
    ///
    /// A clean end-of-stream also marks the connection broken: the peer has
    /// closed it, so it cannot serve another caller.
    ///
    /// # Errors
    ///
    /// Returns any error from the underlying connection.
    pub async fn recv(&mut self) -> Result<Option<Frame>> {
        let result = self.connection_mut().recv().await;

        match &result {
            Ok(None) => self.broken = true,
            Ok(Some(_)) => {}
            Err(error) => {
                if error.is_fatal() {
                    self.broken = true;
                }
            }
        }

        result
    }

    /// Marks the connection broken if `result` holds a fatal error.
    fn note<T>(&mut self, result: &Result<T>) {
        if let Err(error) = result {
            if error.is_fatal() {
                self.broken = true;
            }
        }
    }

    /// Discards this connection instead of returning it to the pool.
    pub fn discard(mut self) {
        self.broken = true;
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };

        if self.broken {
            self.pool.discarded.fetch_add(1, Ordering::Relaxed);
        } else {
            self.pool.recycle(connection);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuse_ratio_handles_an_empty_pool() {
        let stats = PoolStats::default();
        assert!((stats.reuse_ratio() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn reuse_ratio_is_computed() {
        let stats = PoolStats {
            created: 1,
            reused: 3,
            ..PoolStats::default()
        };
        assert!((stats.reuse_ratio() - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn pool_config_builders_apply() {
        let config = PoolConfig::new()
            .with_max_idle_connections(4)
            .with_max_idle_time(Duration::from_secs(5));

        assert_eq!(config.max_idle_connections, 4);
        assert_eq!(config.max_idle_time, Duration::from_secs(5));
    }
}
