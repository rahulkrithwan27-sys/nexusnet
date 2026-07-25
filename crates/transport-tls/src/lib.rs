//! # nexusnet-transport-tls
//!
//! TLS-secured framed connections: [`nexusnet_transport`] running over
//! [`nexusnet_tls`].
//!
//! This crate exists to keep the two lower crates independent. If
//! `nexusnet-transport` depended on `nexusnet-tls` for an optional TLS feature
//! while `nexusnet-tls` depended on `nexusnet-transport`, the two would form a
//! publish cycle that crates.io cannot resolve. Putting the integration here —
//! depending on both, depended on by neither — breaks that cycle.
//!
//! Because [`Connection`] is generic over any `AsyncRead + AsyncWrite` stream
//! and the TLS streams are exactly that, this is a thin convenience layer, not
//! new protocol machinery.
//!
//! ## Minimum supported Rust version
//!
//! Requires **Rust 1.85** via `nexusnet-tls`. The rest of the workspace builds
//! on 1.75.
#![cfg_attr(docsrs, feature(doc_cfg))]

use std::net::SocketAddr;
use std::sync::Arc;

use nexusnet_tls::{ClientStream, ServerStream, TlsAcceptor, TlsConfigBuilder, TlsConnector};
use nexusnet_transport::{Connection, TransportConfig};
use tokio::net::TcpStream;

/// The result type used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Something went wrong establishing a TLS-secured connection.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A TLS handshake or configuration failed.
    ///
    /// The most security-relevant failure: it includes certificate
    /// verification, so a handshake error is what an interception attempt looks
    /// like from inside the transport.
    #[error("TLS error: {reason}")]
    Tls {
        /// A description of what failed.
        reason: String,
    },

    /// The TCP connection could not be established in time.
    #[error("connection to {address} timed out")]
    ConnectTimeout {
        /// The address attempted.
        address: String,
    },

    /// An I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// A framed connection secured by TLS, client side.
pub type TlsClientConnection = Connection<ClientStream<TcpStream>>;

/// A framed connection secured by TLS, server side.
pub type TlsServerConnection = Connection<ServerStream<TcpStream>>;

/// Connects to `address`, performs a TLS handshake against `domain`, and
/// returns a framed connection.
///
/// `domain` is the name the server's certificate is verified against. It must
/// be the name you intend to reach, not one derived from the connection, since
/// an attacker controls the latter.
///
/// # Errors
///
/// Returns [`Error::Tls`] if the handshake fails — including certificate
/// verification, which is what stops interception — or [`Error::ConnectTimeout`]
/// if the TCP connection cannot be established in time.
pub async fn connect_tls(
    address: SocketAddr,
    domain: &str,
    client_config: Arc<rustls::ClientConfig>,
    config: TransportConfig,
) -> Result<TlsClientConnection> {
    let stream =
        match tokio::time::timeout(config.connect_timeout, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(source)) => return Err(Error::Io(source)),
            Err(_) => {
                return Err(Error::ConnectTimeout {
                    address: address.to_string(),
                })
            }
        };

    let connector = TlsConnector::new(client_config);
    let tls_stream = connector
        .connect(domain, stream)
        .await
        .map_err(|source| Error::Tls {
            reason: source.to_string(),
        })?;

    Ok(Connection::new(tls_stream, config))
}

/// Connects using a client configuration that trusts the system roots.
///
/// # Errors
///
/// Returns [`Error::Tls`] if the configuration cannot be built or the handshake
/// fails.
pub async fn connect_tls_default(
    address: SocketAddr,
    domain: &str,
    config: TransportConfig,
) -> Result<TlsClientConnection> {
    let client_config = TlsConfigBuilder::new()
        .build_client()
        .map_err(|source| Error::Tls {
            reason: source.to_string(),
        })?;

    connect_tls(address, domain, client_config, config).await
}

/// Accepts framed connections secured by TLS.
#[derive(Clone)]
pub struct TlsListener {
    acceptor: TlsAcceptor,
    config: TransportConfig,
}

impl TlsListener {
    /// Creates a listener from a server configuration.
    #[must_use]
    pub fn new(server_config: Arc<rustls::ServerConfig>, config: TransportConfig) -> Self {
        Self {
            acceptor: TlsAcceptor::new(server_config),
            config,
        }
    }

    /// Performs the TLS handshake on an already-accepted TCP stream and returns
    /// a framed connection.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Tls`] if the handshake fails, which includes a client
    /// presenting an unacceptable certificate or a refused protocol version.
    pub async fn accept(&self, stream: TcpStream) -> Result<TlsServerConnection> {
        let tls_stream = self
            .acceptor
            .accept(stream)
            .await
            .map_err(|source| Error::Tls {
                reason: source.to_string(),
            })?;

        Ok(Connection::new(tls_stream, self.config))
    }
}

impl std::fmt::Debug for TlsListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsListener").finish_non_exhaustive()
    }
}
