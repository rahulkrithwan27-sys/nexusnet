//! TLS-secured framed connections.
//!
//! This module exists only when the `tls` feature is enabled. It ties
//! [`nexusnet_tls`] to [`Connection`], so a caller gets a framed connection
//! that runs over an authenticated TLS 1.3 session in one step rather than
//! assembling the two layers by hand.
//!
//! Because [`Connection`] is generic over any `AsyncRead + AsyncWrite` stream,
//! and the TLS streams are exactly that, the integration is a thin convenience
//! layer rather than new protocol machinery.
//!
//! ## Feature and MSRV
//!
//! The TLS stack requires Rust 1.85, above the crate's own 1.75 baseline. That
//! requirement is confined to this feature: default builds of
//! `nexusnet-transport` do not pull it in and remain buildable on 1.75.

use std::net::SocketAddr;
use std::sync::Arc;

use nexusnet_tls::{ClientStream, ServerStream, TlsAcceptor, TlsConfigBuilder, TlsConnector};
use tokio::net::TcpStream;

use crate::config::{Error, Result, TransportConfig};
use crate::connection::Connection;

pub use nexusnet_tls::{
    export_key_client, export_key_server, load_certificates, load_private_key, SessionInfo,
};

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
                    timeout: config.connect_timeout,
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
/// A convenience over [`connect_tls`] for the common case of a
/// publicly-trusted server certificate.
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
///
/// Wraps a [`tokio::net::TcpListener`] and a [`TlsAcceptor`], performing the
/// handshake before handing back a framed connection.
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

    /// Accepts one connection from an already-accepted TCP stream.
    ///
    /// Separating the TCP accept from the TLS handshake lets a caller bound the
    /// handshake, log the peer, or shed load before paying for the handshake.
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
