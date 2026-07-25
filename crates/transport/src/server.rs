//! A server that ties listeners to the engine lifecycle.
//!
//! The pieces built so far — framing, pooling, multiplexing — are all
//! *mechanism*. This module is the policy that makes them a framework: accept
//! connections while the engine runs, bound how many run at once, and stop
//! accepting on shutdown while giving in-flight work a bounded chance to
//! finish.
//!
//! Note the dependency direction: this crate depends on `nexusnet-core`, never
//! the reverse. The core engine stays free of networking, so it remains cheap
//! to depend on and fast to compile.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use nexusnet_core::Engine;
use tokio::net::ToSocketAddrs;
use tokio::sync::Notify;

use crate::config::{Result, TransportConfig};
use crate::tcp::{TcpConnection, TcpListener};

/// A boxed, owned future, as returned by [`Handler::handle`].
///
/// Handlers are stored behind a trait object so a server can be configured at
/// runtime, and trait objects cannot use `async fn` directly.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// The default maximum number of simultaneous connections.
pub const DEFAULT_MAX_CONNECTIONS: usize = 1024;

/// Handles accepted connections.
///
/// Implemented automatically for closures returning a future, so the common
/// case needs no explicit `impl`:
///
/// ```
/// # use nexusnet_transport::{ServerConfig, TcpConnection};
/// # use std::net::SocketAddr;
/// let handler = |mut connection: TcpConnection, _peer: SocketAddr| async move {
///     while let Ok(Some(frame)) = connection.recv().await {
///         if connection.send(&frame).await.is_err() {
///             break;
///         }
///     }
/// };
/// # let _ = handler;
/// ```
pub trait Handler: Send + Sync + 'static {
    /// Handles one accepted connection to completion.
    ///
    /// Errors are the handler's to deal with: one failing connection must not
    /// take down the server, so nothing is propagated here.
    fn handle(&self, connection: TcpConnection, peer: SocketAddr) -> BoxFuture<()>;
}

impl<F, Fut> Handler for F
where
    F: Fn(TcpConnection, SocketAddr) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn handle(&self, connection: TcpConnection, peer: SocketAddr) -> BoxFuture<()> {
        Box::pin(self(connection, peer))
    }
}

/// Configuration for a [`Server`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServerConfig {
    /// The maximum number of connections served simultaneously.
    ///
    /// Connections beyond this are accepted and immediately dropped, so the
    /// peer learns promptly rather than waiting in a backlog that may never
    /// drain.
    pub max_connections: usize,
    /// Configuration applied to accepted connections.
    pub transport: TransportConfig,
    /// How long to wait for in-flight connections during shutdown.
    ///
    /// `None` takes the value from the engine's own configuration, which keeps
    /// one shutdown budget for the whole process.
    pub shutdown_timeout: Option<Duration>,
}

impl ServerConfig {
    /// Creates a configuration with default values.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_connections: DEFAULT_MAX_CONNECTIONS,
            transport: TransportConfig::new(),
            shutdown_timeout: None,
        }
    }

    /// Sets the maximum number of simultaneous connections.
    ///
    /// Zero is raised to one; a server that can serve nothing is never what a
    /// caller means.
    #[must_use]
    pub const fn with_max_connections(mut self, max_connections: usize) -> Self {
        self.max_connections = if max_connections == 0 {
            1
        } else {
            max_connections
        };
        self
    }

    /// Sets the transport configuration for accepted connections.
    #[must_use]
    pub const fn with_transport(mut self, transport: TransportConfig) -> Self {
        self.transport = transport;
        self
    }

    /// Overrides the engine's shutdown timeout for this server.
    #[must_use]
    pub const fn with_shutdown_timeout(mut self, shutdown_timeout: Duration) -> Self {
        self.shutdown_timeout = Some(shutdown_timeout);
        self
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// A snapshot of server activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ServerStats {
    /// Connections accepted and handed to the handler.
    pub accepted: u64,
    /// Connections refused because the connection limit was reached.
    pub rejected: u64,
    /// Connections currently being served.
    pub active: usize,
    /// The highest number of simultaneous connections observed.
    pub peak_active: usize,
    /// Connections still running when the shutdown grace period expired.
    pub abandoned: usize,
}

/// Counters shared between the accept loop and connection tasks.
#[derive(Debug, Default)]
struct Tracker {
    accepted: AtomicU64,
    rejected: AtomicU64,
    active: AtomicUsize,
    peak_active: AtomicUsize,
    idle: Notify,
}

impl Tracker {
    fn enter(&self) {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.accepted.fetch_add(1, Ordering::Relaxed);
        self.peak_active.fetch_max(active, Ordering::AcqRel);
    }

    fn leave(&self) {
        if self.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            // The last connection finished; anyone awaiting quiescence can wake.
            self.idle.notify_waiters();
        }
    }

    fn stats(&self, abandoned: usize) -> ServerStats {
        ServerStats {
            accepted: self.accepted.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            active: self.active.load(Ordering::Acquire),
            peak_active: self.peak_active.load(Ordering::Acquire),
            abandoned,
        }
    }
}

/// A handle for stopping a running server.
#[derive(Debug, Clone)]
pub struct ServerHandle {
    shutdown: Arc<Notify>,
    tracker: Arc<Tracker>,
}

impl ServerHandle {
    /// Asks the server to stop accepting new connections and shut down.
    ///
    /// In-flight connections are given the configured grace period to finish.
    pub fn shutdown(&self) {
        self.shutdown.notify_waiters();
    }

    /// Returns a snapshot of server activity.
    #[must_use]
    pub fn stats(&self) -> ServerStats {
        self.tracker.stats(0)
    }
}

/// A server that accepts connections for as long as its engine is running.
///
/// # Examples
///
/// ```
/// # use bytes::Bytes;
/// # use nexusnet_core::Engine;
/// # use nexusnet_protocol::{Frame, FrameType};
/// # use nexusnet_transport::{tcp, Server, ServerConfig, TcpConnection, TransportConfig};
/// # use std::net::SocketAddr;
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let engine = Engine::builder().name("echo").build()?;
/// let server = Server::bind("127.0.0.1:0", ServerConfig::default()).await?;
/// let address = server.local_addr()?;
/// let handle = server.handle();
///
/// let task = tokio::spawn(server.serve(engine, |mut connection: TcpConnection, _peer: SocketAddr| async move {
///     while let Ok(Some(frame)) = connection.recv().await {
///         if connection.send(&frame).await.is_err() {
///             break;
///         }
///     }
/// }));
///
/// let mut client = tcp::connect(address, TransportConfig::default()).await?;
/// client.send(&Frame::new(FrameType::Data, 1, Bytes::from_static(b"hi"))?).await?;
/// assert_eq!(client.recv().await?.expect("echo").payload().as_ref(), b"hi");
/// drop(client);
///
/// handle.shutdown();
/// let stats = task.await??;
/// assert_eq!(stats.accepted, 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Server {
    listener: TcpListener,
    config: ServerConfig,
    shutdown: Arc<Notify>,
    tracker: Arc<Tracker>,
}

impl Server {
    /// Binds a server to `address`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) if the address cannot be bound.
    pub async fn bind<A>(address: A, config: ServerConfig) -> Result<Self>
    where
        A: ToSocketAddrs,
    {
        let listener = TcpListener::bind(address, config.transport).await?;

        Ok(Self {
            listener,
            config,
            shutdown: Arc::new(Notify::new()),
            tracker: Arc::new(Tracker::default()),
        })
    }

    /// Returns the address the server is bound to.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) if the address cannot be read.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Returns the server configuration.
    #[must_use]
    pub const fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Returns a handle for stopping this server.
    ///
    /// Take the handle before calling [`serve`](Self::serve), which consumes
    /// the server.
    #[must_use]
    pub fn handle(&self) -> ServerHandle {
        ServerHandle {
            shutdown: Arc::clone(&self.shutdown),
            tracker: Arc::clone(&self.tracker),
        }
    }

    /// Serves connections until the engine shuts down or the handle is
    /// signalled.
    ///
    /// The engine is started if it has not been already, and is shut down
    /// before this returns, so the engine's lifecycle brackets the server's
    /// exactly. In-flight connections are given the grace period from
    /// [`ServerConfig::shutdown_timeout`], defaulting to the engine's own
    /// shutdown timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if the engine cannot be started, or if accepting fails
    /// in a way that is not recoverable.
    pub async fn serve<H>(self, engine: Engine, handler: H) -> Result<ServerStats>
    where
        H: Handler,
    {
        if !engine.is_running() {
            engine.start()?;
        }

        let grace = self
            .config
            .shutdown_timeout
            .unwrap_or(engine.config().shutdown_timeout);
        let handler = Arc::new(handler);

        tracing::info!(
            address = ?self.listener.local_addr().ok(),
            max_connections = self.config.max_connections,
            "server accepting connections"
        );

        loop {
            tokio::select! {
                accepted = self.listener.accept() => {
                    match accepted {
                        Ok((connection, peer)) => self.dispatch(connection, peer, &handler),
                        Err(error) => {
                            // A single failed accept (a peer that vanished, a
                            // transient descriptor limit) must not kill the
                            // server; log it and keep serving.
                            tracing::warn!(%error, "failed to accept a connection");
                        }
                    }
                }
                () = self.shutdown.notified() => {
                    tracing::info!("shutdown requested; no longer accepting connections");
                    break;
                }
            }
        }

        let abandoned = self.drain(grace).await;

        // Shutting down an already-stopped engine is not an error worth
        // failing the server over.
        let _ = engine.shutdown();

        let stats = self.tracker.stats(abandoned);
        tracing::info!(
            accepted = stats.accepted,
            rejected = stats.rejected,
            peak_active = stats.peak_active,
            abandoned,
            "server stopped"
        );

        Ok(stats)
    }

    /// Spawns a task for one accepted connection, honoring the connection cap.
    fn dispatch<H>(&self, connection: TcpConnection, peer: SocketAddr, handler: &Arc<H>)
    where
        H: Handler,
    {
        if self.tracker.active.load(Ordering::Acquire) >= self.config.max_connections {
            self.tracker.rejected.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(%peer, "connection limit reached; refusing connection");
            // Dropping the connection closes it, so the peer finds out now.
            return;
        }

        self.tracker.enter();

        let handler = Arc::clone(handler);
        let tracker = Arc::clone(&self.tracker);

        tokio::spawn(async move {
            handler.handle(connection, peer).await;
            tracker.leave();
        });
    }

    /// Waits for in-flight connections, up to `grace`.
    ///
    /// Returns how many were still running when the budget expired.
    async fn drain(&self, grace: Duration) -> usize {
        if self.tracker.active.load(Ordering::Acquire) == 0 {
            return 0;
        }

        tracing::info!(
            grace_ms = grace.as_millis() as u64,
            "waiting for in-flight connections"
        );

        let waiter = async {
            while self.tracker.active.load(Ordering::Acquire) > 0 {
                self.tracker.idle.notified().await;
            }
        };

        if tokio::time::timeout(grace, waiter).await.is_err() {
            let remaining = self.tracker.active.load(Ordering::Acquire);
            tracing::warn!(
                remaining,
                "grace period expired with connections still open"
            );
            remaining
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_builders_apply() {
        let config = ServerConfig::new()
            .with_max_connections(8)
            .with_shutdown_timeout(Duration::from_secs(2));

        assert_eq!(config.max_connections, 8);
        assert_eq!(config.shutdown_timeout, Some(Duration::from_secs(2)));
    }

    #[test]
    fn zero_connection_limit_is_corrected() {
        assert_eq!(
            ServerConfig::new().with_max_connections(0).max_connections,
            1
        );
    }

    #[test]
    fn tracker_records_peak_usage() {
        let tracker = Tracker::default();

        tracker.enter();
        tracker.enter();
        tracker.enter();
        assert_eq!(tracker.stats(0).active, 3);
        assert_eq!(tracker.stats(0).peak_active, 3);

        tracker.leave();
        tracker.leave();

        let stats = tracker.stats(0);
        assert_eq!(stats.active, 1);
        assert_eq!(stats.peak_active, 3, "the peak is remembered");
        assert_eq!(stats.accepted, 3);
    }
}
