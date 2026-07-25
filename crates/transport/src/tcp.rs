//! TCP transport: listening, accepting, and dialing.

use std::net::SocketAddr;

use tokio::net::{TcpStream, ToSocketAddrs};

use crate::config::{Error, Result, TransportConfig};
use crate::connection::Connection;

/// A framed connection over TCP.
pub type TcpConnection = Connection<TcpStream>;

/// Applies socket options that the configuration requests.
fn configure(stream: &TcpStream, config: &TransportConfig) -> Result<()> {
    stream.set_nodelay(config.nodelay)?;
    Ok(())
}

/// Connects to `address`, returning a framed connection.
///
/// The attempt is bounded by [`TransportConfig::connect_timeout`], so a
/// blackholed address fails promptly rather than hanging on the OS default.
///
/// # Errors
///
/// Returns [`Error::ConnectTimeout`] if the timeout elapses, or [`Error::Io`]
/// if the connection is refused or the address cannot be resolved.
pub async fn connect<A>(address: A, config: TransportConfig) -> Result<TcpConnection>
where
    A: ToSocketAddrs + std::fmt::Debug,
{
    let label = format!("{address:?}");

    let stream =
        match tokio::time::timeout(config.connect_timeout, TcpStream::connect(address)).await {
            Ok(result) => result?,
            Err(_elapsed) => {
                return Err(Error::ConnectTimeout {
                    address: label,
                    timeout: config.connect_timeout,
                })
            }
        };

    configure(&stream, &config)?;
    Ok(Connection::new(stream, config))
}

/// A TCP listener producing framed connections.
///
/// # Examples
///
/// ```
/// # use bytes::Bytes;
/// # use nexusnet_protocol::{Frame, FrameType};
/// # use nexusnet_transport::{tcp, TcpListener, TransportConfig};
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), nexusnet_transport::Error> {
/// let config = TransportConfig::default();
///
/// // Port 0 asks the OS for any free port.
/// let listener = TcpListener::bind("127.0.0.1:0", config).await?;
/// let address = listener.local_addr()?;
///
/// let server = tokio::spawn(async move {
///     let (mut connection, _peer) = listener.accept().await?;
///     let frame = connection.recv().await?.expect("a frame arrives");
///     connection.send(&frame).await?; // echo it back
///     Ok::<_, nexusnet_transport::Error>(())
/// });
///
/// let mut client = tcp::connect(address, config).await?;
/// client.send(&Frame::new(FrameType::Data, 1, Bytes::from_static(b"ping"))?).await?;
///
/// let echoed = client.recv().await?.expect("the echo arrives");
/// assert_eq!(echoed.payload().as_ref(), b"ping");
///
/// server.await.expect("the server task completes")?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct TcpListener {
    inner: tokio::net::TcpListener,
    config: TransportConfig,
}

impl TcpListener {
    /// Binds a listener to `address`.
    ///
    /// Binding to port `0` asks the operating system for an unused port, which
    /// [`local_addr`](TcpListener::local_addr) then reports.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the address is in use or cannot be bound.
    pub async fn bind<A>(address: A, config: TransportConfig) -> Result<Self>
    where
        A: ToSocketAddrs,
    {
        let inner = tokio::net::TcpListener::bind(address).await?;
        Ok(Self { inner, config })
    }

    /// Returns the address the listener is bound to.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the address cannot be retrieved.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.inner.local_addr()?)
    }

    /// Returns the configuration applied to accepted connections.
    #[must_use]
    pub const fn config(&self) -> &TransportConfig {
        &self.config
    }

    /// Accepts the next inbound connection.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the accept fails or socket options cannot be
    /// applied to the accepted stream.
    pub async fn accept(&self) -> Result<(TcpConnection, SocketAddr)> {
        let (stream, peer) = self.inner.accept().await?;
        configure(&stream, &self.config)?;

        Ok((Connection::new(stream, self.config), peer))
    }

    /// Consumes the listener, returning the underlying Tokio listener.
    #[must_use]
    pub fn into_inner(self) -> tokio::net::TcpListener {
        self.inner
    }
}
